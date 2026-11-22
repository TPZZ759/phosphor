// Live TV TS ring buffer: owns the single upstream connection for live channels,
// absorbs provider hiccups/reconnects transparently, and enables live pause & rewind.
use crate::util::now_ms;
use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const TS_PKT: usize = 188;
const CHUNK_TARGET: usize = 128 * 1024;
const MEM_CAP: usize = 384 << 20; // 384 MB ring cap
const MAX_AGE_MS: i64 = 25 * 60_000; // up to 25 minutes of live rewind

static PORT: AtomicU16 = AtomicU16::new(7878);
static GEN: AtomicU64 = AtomicU64::new(0);
static LAST_CLOSE: AtomicI64 = AtomicI64::new(0);
static CUR: Mutex<Option<Arc<LiveBuf>>> = Mutex::new(None);

pub fn set_port(p: u16) { PORT.store(p, SeqCst); }
pub fn last_close() -> i64 { LAST_CLOSE.load(SeqCst) }

struct Chunk {
    seq: u64,
    ts_ms: i64,
    bytes: Vec<u8>,
    has_pat: bool,
}

#[derive(Default)]
struct St {
    chunks: VecDeque<Chunk>,
    next_seq: u64,
    mem: usize,
    pat: Option<Vec<u8>>,
    pmt_pid: Option<u16>,
    pmt: Option<Vec<u8>>,
    readers: HashMap<u64, i64>, // reader id -> current chunk ts_ms
    next_reader: u64,
    connected: bool,
    reconnects: u32,
    failing_since: Option<i64>,
    no_readers_since: i64,
    last_touch: i64,
    first_chunk_ms: i64,
}

impl St {
    fn oldest_ms(&self) -> i64 { self.chunks.front().map(|c| c.ts_ms).unwrap_or(0) }
    fn newest_ms(&self) -> i64 { self.chunks.back().map(|c| c.ts_ms).unwrap_or(0) }
    fn buffered_secs(&self) -> f64 {
        if self.chunks.len() < 2 { return 0.0; }
        ((self.newest_ms() - self.oldest_ms()).max(0) as f64) / 1000.0
    }
    fn push_chunk(&mut self, bytes: Vec<u8>, has_pat: bool, ts_ms: i64) {
        if bytes.is_empty() { return; }
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.first_chunk_ms == 0 { self.first_chunk_ms = ts_ms; }
        self.mem += bytes.len();
        self.chunks.push_back(Chunk { seq, ts_ms, bytes, has_pat });
        while self.mem > MEM_CAP || (self.chunks.len() > 2 && ts_ms - self.oldest_ms() > MAX_AGE_MS) {
            if let Some(old) = self.chunks.pop_front() {
                self.mem = self.mem.saturating_sub(old.bytes.len());
            } else { break; }
        }
    }
    fn inspect_pkt(&mut self, pkt: &[u8]) -> bool {
        if pkt.len() != TS_PKT || pkt[0] != 0x47 { return false; }
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | (pkt[2] as u16);
        let pusi = (pkt[1] & 0x40) != 0;
        let afc = (pkt[3] >> 4) & 0x03;
        if pid == 0 {
            self.pat = Some(pkt.to_vec());
            if pusi && (afc == 1 || afc == 3) {
                let mut off = 4usize;
                if afc == 3 { off += 1 + pkt.get(4).copied().unwrap_or(0) as usize; }
                if off < TS_PKT {
                    let ptr = pkt[off] as usize;
                    let sec = off + 1 + ptr;
                    if sec + 12 <= TS_PKT && pkt[sec] == 0x00 {
                        let sec_len = ((((pkt[sec + 1] & 0x0F) as usize) << 8) | pkt[sec + 2] as usize).min(TS_PKT - sec - 3);
                        let end = (sec + 3 + sec_len).saturating_sub(4).min(TS_PKT);
                        let mut p = sec + 8;
                        while p + 4 <= end {
                            let prog_num = ((pkt[p] as u16) << 8) | pkt[p + 1] as u16;
                            let pmt_pid = (((pkt[p + 2] & 0x1F) as u16) << 8) | pkt[p + 3] as u16;
                            if prog_num != 0 && pmt_pid != 0 {
                                self.pmt_pid = Some(pmt_pid);
                                break;
                            }
                            p += 4;
                        }
                    }
                }
            }
            return true;
        }
        if Some(pid) == self.pmt_pid {
            self.pmt = Some(pkt.to_vec());
        }
        false
    }
    fn chunk_for_back(&self, back_secs: f64) -> Option<u64> {
        if self.chunks.is_empty() { return None; }
        let newest = self.newest_ms();
        let target = if back_secs <= 0.5 {
            // Live edge: start ~1.5s back so ffmpeg gets immediate video+audio frames
            newest - 1500
        } else {
            newest - (back_secs * 1000.0) as i64
        };
        // Find first chunk at or after target, preferring one with PAT if nearby
        let pos = self.chunks.partition_point(|c| c.ts_ms < target);
        let idx = pos.min(self.chunks.len() - 1);
        if back_secs <= 0.5 {
            // Look back up to 3 chunks for a PAT boundary for fastest decoder lock
            for back in 0..=3usize {
                if let Some(i) = idx.checked_sub(back) {
                    if self.chunks[i].has_pat { return Some(self.chunks[i].seq); }
                }
            }
        }
        Some(self.chunks[idx].seq)
    }
    fn get_chunk(&self, seq: u64) -> Option<(u64, i64, Bytes)> {
        let front = self.chunks.front()?;
        if seq < front.seq {
            return Some((front.seq, front.ts_ms, Bytes::copy_from_slice(&front.bytes)));
        }
        let idx = (seq - front.seq) as usize;
        self.chunks.get(idx).map(|c| (c.seq, c.ts_ms, Bytes::copy_from_slice(&c.bytes)))
    }
}

pub struct LiveBuf {
    pub gen: u64,
    pub url: String,
    ua: String,
    st: Mutex<St>,
    data: Notify,
    closed: AtomicBool,
    http: reqwest::Client,
}

impl LiveBuf {
    fn local_url(&self, back_secs: f64) -> String {
        if back_secs > 0.5 {
            format!("http://127.0.0.1:{}/livebuf/{}?back={:.1}", PORT.load(SeqCst), self.gen, back_secs)
        } else {
            format!("http://127.0.0.1:{}/livebuf/{}", PORT.load(SeqCst), self.gen)
        }
    }
    fn close(&self) {
        if !self.closed.swap(true, SeqCst) {
            LAST_CLOSE.store(now_ms(), SeqCst);
        }
        self.data.notify_waiters();
    }

    async fn run(self: Arc<Self>) {
        let mut backoff = 1000u64;
        let mut slot_errs = 0u32;
        loop {
            if self.closed.load(SeqCst) { break; }
            {
                let mut st = self.st.lock().unwrap();
                let now = now_ms();
                let active = !st.readers.is_empty() || (now - st.last_touch < 35_000);
                if active {
                    st.no_readers_since = 0;
                } else {
                    if st.no_readers_since == 0 { st.no_readers_since = now; }
                    if now - st.no_readers_since > 45_000 {
                        drop(st);
                        println!("[livebuf] idle timeout; releasing upstream");
                        self.close();
                        break;
                    }
                }
            }
            let since = now_ms() - LAST_CLOSE.load(SeqCst);
            if since < 1500 { tokio::time::sleep(std::time::Duration::from_millis((1500 - since) as u64)).await; }
            if self.closed.load(SeqCst) { break; }
            let resp = self.http.get(&self.url)
                .header(header::USER_AGENT, &self.ua)
                .timeout(std::time::Duration::from_secs(3600 * 12))
                .send().await;
            let mut stream = match resp {
                Ok(r) if r.status().is_success() => {
                    backoff = 1000;
                    slot_errs = 0;
                    { let mut st = self.st.lock().unwrap(); st.connected = true; st.failing_since = None; }
                    Box::pin(r.bytes_stream())
                }
                Ok(r) => {
                    let code = r.status().as_u16();
                    println!("[livebuf] provider said {}", r.status());
                    let have_data = {
                        let mut st = self.st.lock().unwrap();
                        st.connected = false;
                        st.reconnects += 1;
                        if st.failing_since.is_none() { st.failing_since = Some(now_ms()); }
                        !st.chunks.is_empty()
                    };
                    if matches!(code, 401 | 403 | 429 | 460) {
                        slot_errs += 1;
                        LAST_CLOSE.store(now_ms(), SeqCst);
                        let limit = if have_data { 5 } else { 3 };
                        if slot_errs >= limit {
                            println!("[livebuf] giving up after {slot_errs} consecutive {code} rejections");
                            self.close();
                            break;
                        }
                        let wait = match slot_errs { 1 => 3000, 2 => 6000, _ => 10_000 };
                        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                        continue;
                    }
                    slot_errs = 0;
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                    backoff = (backoff * 2).min(6000);
                    continue;
                }
                Err(e) => {
                    slot_errs = 0;
                    println!("[livebuf] connect failed ({e})");
                    { let mut st = self.st.lock().unwrap(); st.connected = false; st.reconnects += 1; if st.failing_since.is_none() { st.failing_since = Some(now_ms()); } }
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                    backoff = (backoff * 2).min(6000);
                    continue;
                }
            };

            let mut carry: Vec<u8> = Vec::with_capacity(TS_PKT * 4);
            let mut acc: Vec<u8> = Vec::with_capacity(CHUNK_TARGET + TS_PKT * 4);
            let mut acc_pat = false;
            let mut last_flush = now_ms();

            loop {
                if self.closed.load(SeqCst) { break; }
                // Idle check during read loop too
                {
                    let mut st = self.st.lock().unwrap();
                    let now = now_ms();
                    let active = !st.readers.is_empty() || (now - st.last_touch < 35_000);
                    if active { st.no_readers_since = 0; }
                    else {
                        if st.no_readers_since == 0 { st.no_readers_since = now; }
                        if now - st.no_readers_since > 45_000 {
                            drop(st);
                            println!("[livebuf] idle timeout during stream; releasing upstream");
                            self.close();
                            break;
                        }
                    }
                }
                let next = tokio::time::timeout(std::time::Duration::from_secs(12), stream.next()).await;
                match next {
                    Ok(Some(Ok(chunk))) => {
                        carry.extend_from_slice(&chunk);
                        // Align to 188-byte MPEG-TS packet sync byte 0x47
                        let mut i = 0usize;
                        let mut st = self.st.lock().unwrap();
                        while carry.len().saturating_sub(i) >= TS_PKT {
                            if carry[i] != 0x47 {
                                // Resync: look for 0x47 followed by 0x47 188 bytes later
                                if let Some(rel) = carry[i..].iter().position(|&b| b == 0x47) {
                                    let pos = i + rel;
                                    if pos + TS_PKT < carry.len() {
                                        if carry[pos + TS_PKT] == 0x47 { i = pos; continue; }
                                        i = pos + 1; continue;
                                    } else {
                                        // Need more bytes to verify sync
                                        i = pos;
                                        break;
                                    }
                                } else {
                                    i = carry.len();
                                    break;
                                }
                            }
                            let pkt = &carry[i..i + TS_PKT];
                            if st.inspect_pkt(pkt) { acc_pat = true; }
                            acc.extend_from_slice(pkt);
                            i += TS_PKT;
                            let now = now_ms();
                            if acc.len() >= CHUNK_TARGET || (acc.len() >= TS_PKT * 20 && now - last_flush >= 250) {
                                let out = std::mem::replace(&mut acc, Vec::with_capacity(CHUNK_TARGET + TS_PKT * 4));
                                let hp = std::mem::replace(&mut acc_pat, false);
                                st.push_chunk(out, hp, now);
                                last_flush = now;
                            }
                        }
                        if i > 0 { carry.drain(..i); }
                        let now = now_ms();
                        if !acc.is_empty() && now - last_flush >= 250 {
                            let out = std::mem::replace(&mut acc, Vec::with_capacity(CHUNK_TARGET + TS_PKT * 4));
                            let hp = std::mem::replace(&mut acc_pat, false);
                            st.push_chunk(out, hp, now);
                            last_flush = now;
                        }
                        drop(st);
                        self.data.notify_waiters();
                    }
                    other => {
                        if !acc.is_empty() {
                            let mut st = self.st.lock().unwrap();
                            let out = std::mem::take(&mut acc);
                            let hp = std::mem::take(&mut acc_pat);
                            st.push_chunk(out, hp, now_ms());
                        }
                        println!("[livebuf] upstream {}; reconnecting", match other {
                            Err(_) => "went silent".to_string(),
                            Ok(Some(Err(e))) => format!("failed ({e})"),
                            _ => "ended".into(),
                        });
                        LAST_CLOSE.store(now_ms(), SeqCst);
                        { let mut st = self.st.lock().unwrap(); st.connected = false; st.reconnects += 1; if st.failing_since.is_none() { st.failing_since = Some(now_ms()); } }
                        self.data.notify_waiters();
                        break;
                    }
                }
            }
        }
        LAST_CLOSE.store(now_ms(), SeqCst);
    }
}

/// Attach the live ring buffer to this upstream URL. Reuses an already-attached ring for the same
/// channel (so pause/resume and rewind never drop the provider connection).
pub async fn attach(url: &str, ua: &str, back_secs: f64) -> Option<String> {
    crate::spool::detach();
    let cur = CUR.lock().unwrap().clone();
    if let Some(s) = cur.as_ref().filter(|s| s.url == url && !s.closed.load(SeqCst)) {
        s.st.lock().unwrap().last_touch = now_ms();
        return Some(s.local_url(back_secs));
    }
    if let Some(s) = cur { s.close(); }
    let http = reqwest::Client::builder()
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .pool_max_idle_per_host(0)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build().ok()?;
    let lb = Arc::new(LiveBuf {
        gen: GEN.fetch_add(1, SeqCst) + 1,
        url: url.to_string(),
        ua: ua.to_string(),
        st: Mutex::new(St { last_touch: now_ms(), ..Default::default() }),
        data: Notify::new(),
        closed: AtomicBool::new(false),
        http,
    });
    *CUR.lock().unwrap() = Some(lb.clone());
    tokio::spawn(lb.clone().run());
    // Wait up to 10s for first TS chunk so ffmpeg gets data right away
    for _ in 0..100 {
        if lb.closed.load(SeqCst) { return None; }
        if !lb.st.lock().unwrap().chunks.is_empty() { return Some(lb.local_url(back_secs)); }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Even if slow to start, keep attached unless closed
    if lb.closed.load(SeqCst) { None } else { Some(lb.local_url(back_secs)) }
}

pub fn local_for(url: &str, back_secs: f64) -> Option<String> {
    CUR.lock().unwrap().as_ref().filter(|s| s.url == url && !s.closed.load(SeqCst)).map(|s| s.local_url(back_secs))
}
pub fn attached(url: &str) -> bool { local_for(url, 0.0).is_some() }
pub fn is_active() -> bool { CUR.lock().unwrap().as_ref().map_or(false, |s| !s.closed.load(SeqCst)) }
pub fn touch() {
    if let Some(s) = CUR.lock().unwrap().as_ref() {
        if !s.closed.load(SeqCst) { s.st.lock().unwrap().last_touch = now_ms(); }
    }
}
pub fn detach() { if let Some(s) = CUR.lock().unwrap().take() { s.close(); } }

pub fn stats() -> serde_json::Value {
    let Some(s) = CUR.lock().unwrap().clone().filter(|s| !s.closed.load(SeqCst)) else { return serde_json::json!(null) };
    let st = s.st.lock().unwrap();
    let newest = st.newest_ms();
    let reader_ms = st.readers.values().copied().max().unwrap_or(newest);
    let behind = if newest > 0 && reader_ms > 0 { ((newest - reader_ms).max(0) as f64) / 1000.0 } else { 0.0 };
    serde_json::json!({
        "bufferedSecs": st.buffered_secs().round(),
        "behindSecs": behind.round(),
        "memMB": st.mem >> 20,
        "connected": st.connected,
        "reconnects": st.reconnects,
    })
}

/// GET /livebuf/{gen}?back={secs}
pub async fn serve(Path(gen): Path<u64>, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(lb) = CUR.lock().unwrap().clone().filter(|s| s.gen == gen && !s.closed.load(SeqCst)) else {
        return (StatusCode::NOT_FOUND, "gone").into_response();
    };
    let back: f64 = q.get("back").and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0).max(0.0);
    let (id, start_seq, header_bytes) = {
        let mut st = lb.st.lock().unwrap();
        st.last_touch = now_ms();
        st.next_reader += 1;
        let id = st.next_reader;
        let seq = st.chunk_for_back(back).unwrap_or(st.next_seq);
        let ts = st.get_chunk(seq).map(|x| x.1).unwrap_or_else(|| st.newest_ms());
        st.readers.insert(id, ts);
        // Prepend latest PAT + PMT so ffmpeg immediately detects streams at any rewind point
        let mut hdr = Vec::with_capacity(TS_PKT * 2);
        if let Some(pat) = &st.pat { hdr.extend_from_slice(pat); }
        if let Some(pmt) = &st.pmt { hdr.extend_from_slice(pmt); }
        (id, seq, if hdr.is_empty() { None } else { Some(Bytes::from(hdr)) })
    };
    struct Guard(Arc<LiveBuf>, u64);
    impl Drop for Guard {
        fn drop(&mut self) {
            let mut st = self.0.st.lock().unwrap();
            st.readers.remove(&self.1);
            st.last_touch = now_ms();
        }
    }
    let g = Guard(lb.clone(), id);
    let stream = futures_util::stream::unfold((g, start_seq, header_bytes), move |(g, mut seq, mut hdr)| async move {
        if let Some(h) = hdr.take() {
            return Some((Ok::<Bytes, std::io::Error>(h), (g, seq, None)));
        }
        let lb = g.0.clone();
        loop {
            if lb.closed.load(SeqCst) {
                return Some((Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "livebuf closed")), (g, seq, None)));
            }
            let notified = lb.data.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut st = lb.st.lock().unwrap();
                st.last_touch = now_ms();
                if let Some((got_seq, ts, bytes)) = st.get_chunk(seq) {
                    if let Some(r) = st.readers.get_mut(&g.1) { *r = ts; }
                    seq = got_seq + 1;
                    return Some((Ok(bytes), (g, seq, None)));
                }
                if st.failing_since.map_or(false, |t| now_ms() - t > 35_000) {
                    return Some((Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "live upstream unreachable")), (g, seq, None)));
                }
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/MP2T")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .unwrap()
}
