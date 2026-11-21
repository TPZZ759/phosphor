// Read-ahead cache ("spool") for films and episodes.
//
// One task owns the single provider connection. It downloads ahead of playback into memory (1 MB
// blocks, about 2.5 minutes of video ahead) and serves the bytes to ffmpeg/ffprobe over a local URL
// with Range support. So:
//   - provider hiccups and dropped connections are absorbed by the buffer: the spool reconnects with
//     `Range: bytes=N-` in the background while playback carries on from memory;
//   - seeks and restarts within what's downloaded never touch the provider (no connection churn, no
//     release wait), and the probe + the stream share one connection;
//   - when the buffer is full the provider connection is closed (polite, and nothing idles on the
//     one-connection account); it reopens when the buffer drains to half.
// Live TV / catch-up don't use it (no ranges; they can't run ahead of the broadcast).
use crate::util::now_ms;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const BLOCK: u64 = 1 << 20;
const MEM_CAP: u64 = 900 << 20; // most memory the cache may hold
const BEHIND: u64 = 96 << 20; // kept behind the playhead (quick back-seeks)
const AHEAD_SECS: f64 = 150.0;

static PORT: AtomicU16 = AtomicU16::new(7878);
static GEN: AtomicU64 = AtomicU64::new(0);
static LAST_CLOSE: AtomicI64 = AtomicI64::new(0);
static CUR: Mutex<Option<Arc<Spool>>> = Mutex::new(None);

pub fn set_port(p: u16) { PORT.store(p, SeqCst); }

struct Reader { pos: u64, served: u64, waiting: Option<u64> }

#[derive(Default)]
struct St {
    total: Option<u64>,
    blocks: BTreeMap<u64, Vec<u8>>, // block index -> bytes, always filled from the block's start
    mem: u64,
    readers: HashMap<u64, Reader>,
    next_reader: u64,
    bitrate: f64, // bytes per second (file size / duration), once known
    prime: bool,  // fetch the start of the file even before anyone asks (header + first seconds)
    connected: bool,
    upstream_pos: u64,
    reconnects: u32,
    failing_since: Option<i64>,
    unsupported: bool,
    no_readers_since: i64,
}
impl St {
    /// Bytes available at `pos` (a slice of the block holding it), if downloaded.
    fn have(&self, pos: u64) -> Option<(&Vec<u8>, usize)> {
        let b = self.blocks.get(&(pos / BLOCK))?; let off = (pos % BLOCK) as usize;
        if b.len() > off { Some((b, off)) } else { None }
    }
    /// First byte at or after `pos` that isn't downloaded (contiguously).
    fn frontier(&self, pos: u64) -> u64 {
        let mut idx = pos / BLOCK;
        loop {
            match self.blocks.get(&idx) {
                Some(b) if b.len() as u64 == BLOCK => idx += 1,
                Some(b) => return (idx * BLOCK + b.len() as u64).max(pos),
                None => return (idx * BLOCK).max(pos),
            }
        }
    }
    fn primary(&self) -> Option<u64> { self.readers.values().max_by_key(|r| r.served).map(|r| r.pos) }
    fn cap(&self) -> u64 { if self.bitrate > 0.0 { ((self.bitrate * AHEAD_SECS) as u64).clamp(48 << 20, 700 << 20) } else { 128 << 20 } }
    /// Where the download should be: a reader waiting on missing bytes first, else read-ahead.
    fn target(&self) -> Option<u64> {
        let total = self.total.unwrap_or(u64::MAX);
        let waiting = self.readers.values().filter_map(|r| r.waiting.map(|w| (r.served, w))).filter(|(_, w)| *w < total && self.have(*w).is_none()).max_by_key(|x| x.0);
        if let Some((_, w)) = waiting { return Some(self.frontier(w)); }
        if let Some(p) = self.primary() { let f = self.frontier(p); if f < total && f - p < self.cap() { return Some(f); } }
        if self.prime && self.have(0).is_none() { return Some(0); }
        None
    }
    fn store(&mut self, mut pos: u64, data: &[u8]) {
        let mut i = 0;
        while i < data.len() {
            let idx = pos / BLOCK; let off = (pos % BLOCK) as usize;
            let room = (BLOCK as usize - off).min(data.len() - i);
            let b = self.blocks.entry(idx).or_default();
            if b.len() == off { b.extend_from_slice(&data[i..i + room]); self.mem += room as u64; }
            // (already have these bytes, or a gap before them: skip; blocks always fill from their start)
            i += room; pos += room as u64;
        }
        self.evict();
    }
    fn evict(&mut self) {
        if self.mem <= MEM_CAP { return; }
        let p = self.primary().unwrap_or(0); let keep_from = p.saturating_sub(BEHIND) / BLOCK; let keep_to = (p + self.cap()) / BLOCK + 1;
        // farthest from the playhead first
        let mut idx: Vec<u64> = self.blocks.keys().copied().filter(|&k| k < keep_from || k > keep_to).collect();
        idx.sort_by_key(|&k| std::cmp::Reverse(if k < keep_from { keep_from - k } else { k - keep_to }));
        for k in idx { if self.mem <= MEM_CAP * 9 / 10 { break; } if let Some(b) = self.blocks.remove(&k) { self.mem -= b.len() as u64; } }
    }
}

pub struct Spool { pub gen: u64, pub url: String, ua: String, st: Mutex<St>, data: Notify, demand: Notify, closed: AtomicBool, http: reqwest::Client }

impl Spool {
    fn local_url(&self) -> String { format!("http://127.0.0.1:{}/spool/{}", PORT.load(SeqCst), self.gen) }
    fn close(&self) { self.closed.store(true, SeqCst); self.data.notify_waiters(); self.demand.notify_one(); }

    /// The download loop: keeps one provider connection at the right place, reconnecting with
    /// backoff after errors, closing it when the buffer is full or nobody is reading.
    async fn run(self: Arc<Self>) {
        let mut body: Option<std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>> = None;
        let mut backoff = 500u64;
        loop {
            if self.closed.load(SeqCst) { break; }
            let (target, idle) = { let mut st = self.st.lock().unwrap();
                if st.readers.is_empty() { if st.no_readers_since == 0 { st.no_readers_since = now_ms(); } } else { st.no_readers_since = 0; }
                (st.target(), st.no_readers_since > 0 && now_ms() - st.no_readers_since > 15_000) };
            let Some(t) = target.filter(|_| !idle) else {
                if body.take().is_some() { self.st.lock().unwrap().connected = false; LAST_CLOSE.store(now_ms(), SeqCst); }
                // nothing to do: freed after 10 min unused
                if idle && now_ms() - self.st.lock().unwrap().no_readers_since > 600_000 { self.close(); break; }
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), self.demand.notified()).await;
                continue;
            };
            let at = self.st.lock().unwrap().upstream_pos;
            // keep reading through a small gap (already-cached bytes) rather than reconnecting
            let reuse = body.is_some() && t >= at && t - at < (8 << 20);
            if !reuse {
                if body.take().is_some() { LAST_CLOSE.store(now_ms(), SeqCst); }
                let start = t / BLOCK * BLOCK + { let st = self.st.lock().unwrap(); st.blocks.get(&(t / BLOCK)).map_or(0, |b| b.len() as u64) };
                let since = now_ms() - LAST_CLOSE.load(SeqCst); if since < 400 { tokio::time::sleep(std::time::Duration::from_millis((400 - since) as u64)).await; }
                match self.open(start).await {
                    Ok(s) => { body = Some(s); backoff = 500; let mut st = self.st.lock().unwrap(); st.connected = true; st.upstream_pos = start; st.failing_since = None; }
                    Err(unsupported) => {
                        {
                            let mut st = self.st.lock().unwrap(); st.connected = false; st.reconnects += 1;
                            if unsupported { st.unsupported = true; } else if st.failing_since.is_none() { st.failing_since = Some(now_ms()); }
                        }
                        if unsupported { self.close(); break; }
                        self.data.notify_waiters();
                        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await; backoff = (backoff * 2).min(8000);
                        continue;
                    }
                }
            }
            let next = tokio::time::timeout(std::time::Duration::from_secs(12), body.as_mut().unwrap().next()).await;
            match next {
                Ok(Some(Ok(chunk))) => { { let mut st = self.st.lock().unwrap(); let p = st.upstream_pos; st.store(p, &chunk); st.upstream_pos += chunk.len() as u64; if p == 0 { st.prime = false; } } self.data.notify_waiters(); }
                other => {
                    // end of file, error, or silence for 12 s: drop the connection; the loop reconnects where needed
                    let at_end = { let st = self.st.lock().unwrap(); st.total.map_or(false, |t| st.upstream_pos >= t) };
                    if !at_end { println!("[spool] provider connection {}; reconnecting at {} MB", match other { Err(_) => "went silent".to_string(), Ok(Some(Err(e))) => format!("failed ({e})"), _ => "ended early".into() }, self.st.lock().unwrap().upstream_pos >> 20); }
                    body = None; LAST_CLOSE.store(now_ms(), SeqCst); self.st.lock().unwrap().connected = false;
                }
            }
        }
        LAST_CLOSE.store(now_ms(), SeqCst);
    }

    /// Open the provider at `start`. Err(true) = the provider doesn't support ranges (use direct playback).
    async fn open(&self, start: u64) -> Result<std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>, bool> {
        let r = self.http.get(&self.url).header(header::USER_AGENT, &self.ua).header(header::RANGE, format!("bytes={start}-"))
            .timeout(std::time::Duration::from_secs(3600 * 6)).send().await.map_err(|_| false)?;
        let status = r.status();
        if status == reqwest::StatusCode::OK && start == 0 && r.headers().get(header::ACCEPT_RANGES).and_then(|v| v.to_str().ok()) != Some("bytes") { println!("[spool] provider ignores ranges: playing directly"); return Err(true); }
        if status == reqwest::StatusCode::OK && start > 0 { return Err(true); }
        if !status.is_success() { println!("[spool] provider said {status} at {} MB", start >> 20); return Err(false); }
        let total = r.headers().get(header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit('/').next()).and_then(|t| t.parse::<u64>().ok())
            .or_else(|| r.content_length().map(|l| l + start));
        if let Some(t) = total { self.st.lock().unwrap().total = Some(t); }
        Ok(Box::pin(r.bytes_stream()))
    }
}

/// Use the spool for this provider URL: returns the local URL ffmpeg should open, or None to play
/// directly (provider without range support, or it didn't answer). Reuses the current spool when
/// it's the same title (seeks, restarts, probe then play).
pub async fn attach(url: &str, ua: &str) -> Option<String> {
    crate::livebuf::detach();
    let cur = CUR.lock().unwrap().clone();
    if let Some(s) = cur.as_ref().filter(|s| s.url == url && !s.closed.load(SeqCst)) { return Some(s.local_url()); }
    if let Some(s) = cur { s.close(); }
    let http = reqwest::Client::builder().pool_max_idle_per_host(0).connect_timeout(std::time::Duration::from_secs(10)).build().ok()?;
    let sp = Arc::new(Spool { gen: GEN.fetch_add(1, SeqCst) + 1, url: url.to_string(), ua: ua.to_string(), st: Mutex::new(St { prime: true, ..Default::default() }), data: Notify::new(), demand: Notify::new(), closed: AtomicBool::new(false), http });
    *CUR.lock().unwrap() = Some(sp.clone());
    tokio::spawn(sp.clone().run());
    // wait for the first connection: we need the file size, and to know ranges work
    for _ in 0..200 {
        if sp.closed.load(SeqCst) { return None; }
        if sp.st.lock().unwrap().total.is_some() { return Some(sp.local_url()); }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    sp.close(); None
}
/// Local URL if this provider URL is being spooled right now.
pub fn local_for(url: &str) -> Option<String> { CUR.lock().unwrap().as_ref().filter(|s| s.url == url && !s.closed.load(SeqCst)).map(|s| s.local_url()) }
pub fn attached(url: &str) -> bool { local_for(url).is_some() }
/// Give the provider connection back (player closed, live TV, pro player).
pub fn detach() { if let Some(s) = CUR.lock().unwrap().take() { s.close(); } }
pub fn set_duration(url: &str, secs: f64) {
    if let Some(s) = CUR.lock().unwrap().as_ref().filter(|s| s.url == url) { let mut st = s.st.lock().unwrap(); if let Some(t) = st.total { if secs > 60.0 { st.bitrate = t as f64 / secs; } } }
}
/// For the playback info overlay: seconds and MB buffered ahead of playback, connection state.
pub fn stats() -> serde_json::Value {
    let Some(s) = CUR.lock().unwrap().clone() else { return serde_json::json!(null) };
    let st = s.st.lock().unwrap(); let p = st.primary().unwrap_or(0); let ahead = st.frontier(p) - p;
    serde_json::json!({ "aheadMB": (ahead >> 20), "aheadSecs": if st.bitrate > 0.0 { (ahead as f64 / st.bitrate).round() } else { -1.0 }, "connected": st.connected, "reconnects": st.reconnects, "memMB": st.mem >> 20 })
}

/// GET /spool/{gen}: the cached file with Range support (127.0.0.1 only, like the rest of the API).
pub async fn serve(Path(gen): Path<u64>, headers: HeaderMap) -> Response {
    let Some(sp) = CUR.lock().unwrap().clone().filter(|s| s.gen == gen && !s.closed.load(SeqCst)) else { return (StatusCode::NOT_FOUND, "gone").into_response() };
    let (start, end) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|r| r.strip_prefix("bytes="))
        .map(|r| { let mut it = r.split('-'); (it.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0), it.next().and_then(|x| x.parse::<u64>().ok())) }).unwrap_or((0, None));
    let Some(total) = sp.st.lock().unwrap().total else { return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response() };
    if start >= total { return (StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, format!("bytes */{total}"))]).into_response(); }
    let end = end.unwrap_or(total - 1).min(total - 1);
    let id = { let mut st = sp.st.lock().unwrap(); st.next_reader += 1; let id = st.next_reader; st.readers.insert(id, Reader { pos: start, served: 0, waiting: None }); id };
    sp.demand.notify_one();
    struct Guard(Arc<Spool>, u64);
    impl Drop for Guard { fn drop(&mut self) { self.0.st.lock().unwrap().readers.remove(&self.1); self.0.demand.notify_one(); } }
    let g = Guard(sp.clone(), id);
    let stream = futures_util::stream::unfold((g, start), move |(g, pos)| async move {
        if pos > end { return None; }
        let sp = g.0.clone();
        loop {
            if sp.closed.load(SeqCst) { return Some((Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "spool closed")), (g, end + 1))); }
            let notified = sp.data.notified(); tokio::pin!(notified); notified.as_mut().enable();
            {
                let mut st = sp.st.lock().unwrap();
                if let Some((b, off)) = st.have(pos) {
                    let n = (b.len() - off).min((end - pos + 1) as usize).min(512 * 1024);
                    let chunk = Bytes::copy_from_slice(&b[off..off + n]);
                    if let Some(r) = st.readers.get_mut(&g.1) { r.pos = pos + n as u64; r.served += n as u64; r.waiting = None; }
                    drop(st); sp.demand.notify_one();
                    return Some((Ok(chunk), (g, pos + n as u64)));
                }
                if let Some(r) = st.readers.get_mut(&g.1) { r.pos = pos; r.waiting = Some(pos); }
                // the provider has been unreachable for a long time: give up so the player can report it
                if st.failing_since.map_or(false, |t| now_ms() - t > 45_000) { drop(st); return Some((Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "provider unreachable")), (g, end + 1))); }
            }
            sp.demand.notify_one();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
        }
    });
    Response::builder().status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, "application/octet-stream").header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}")).header(header::CONTENT_LENGTH, end - start + 1)
        .body(Body::from_stream(stream)).unwrap()
}
