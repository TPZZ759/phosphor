// Live TV TS ring buffer: owns the single upstream connection for live channels (via `crate::slot`),
// absorbs provider hiccups/reconnects transparently, enables live pause/rewind with PCR-accurate
// timing & Random-Access keyframe joins, and supports zero-connection retroactive DVR recording.
use crate::slot::{Owner, SlotLease};
use crate::util::now_ms;
use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufWriter, Write};
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const TS_PKT: usize = 188;
const CHUNK_TARGET: usize = 128 * 1024;
const MEM_CAP: usize = 384 << 20; // 384 MB ring cap
const MAX_AGE_MS: i64 = 25 * 60_000; // up to 25 minutes of live rewind
const IDLE_TIMEOUT_MS: i64 = 35_000; // release upstream 35 s after last reader / keepalive
const NON_TS_FAIL_BYTES: usize = 32 * 1024; // fast-fail non-TS HTML/JSON/m3u8 responses
const MAX_PMT_BYTES: usize = 4096;

static PORT: AtomicU16 = AtomicU16::new(7878);
static GEN: AtomicU64 = AtomicU64::new(0);
static LAST_CLOSE: AtomicI64 = AtomicI64::new(0);
static CUR: Mutex<Option<Arc<LiveBuf>>> = Mutex::new(None);
static REC: Mutex<Option<ActiveRec>> = Mutex::new(None);
static LAST_REC: Mutex<Option<Value>> = Mutex::new(None);

pub fn set_port(p: u16) {
    PORT.store(p, SeqCst);
}
pub fn last_close() -> i64 {
    LAST_CLOSE.load(SeqCst)
}

struct ActiveRec {
    id: String,
    title: String,
    mkv_file: String,
    ts_file: String,
    started_ms: i64,
    retroactive_secs: f64,
    bytes: Arc<AtomicU64>,
    lb_gen: u64,
    lb: Arc<LiveBuf>,
}

struct Chunk {
    seq: u64,
    ts_ms: i64,    // wall-clock ms (for ring age cap)
    media_ms: i64, // PCR-driven media timeline ms (falls back to relative wall-clock)
    bytes: Bytes,
    has_pat: bool,
    has_rai: bool, // MPEG-TS Random Access Indicator (0x40 in adaptation flags)
}

#[derive(Default)]
struct St {
    chunks: VecDeque<Chunk>,
    next_seq: u64,
    mem: usize,
    pat: Option<Vec<u8>>,
    pmt_pid: Option<u16>,
    pcr_pid: Option<u16>,
    pmt: Option<Vec<u8>>, // complete assembled PMT (1..N raw 188-byte TS packets, <= 4096 bytes)
    pmt_acc_pkts: Vec<u8>,
    pmt_section_need: usize,
    pmt_section_have: usize,
    pmt_cc: Option<u8>,
    pmt_assembling: bool,
    last_pcr_base: Option<u64>,
    last_pcr_wall_ms: i64,
    pcr_timeline_ms: f64,
    has_pcr: bool,
    readers: HashMap<u64, i64>, // reader id -> current chunk media_ms
    next_reader: u64,
    connected: bool,
    reconnects: u32,
    failing_since: Option<i64>,
    last_touch: i64,
    first_chunk_ms: i64,
    rec_tx: Option<std::sync::mpsc::Sender<Bytes>>,
}

impl St {
    fn oldest_wall_ms(&self) -> i64 {
        self.chunks.front().map(|c| c.ts_ms).unwrap_or(0)
    }
    fn oldest_media_ms(&self) -> i64 {
        self.chunks.front().map(|c| c.media_ms).unwrap_or(0)
    }
    fn newest_media_ms(&self) -> i64 {
        self.chunks.back().map(|c| c.media_ms).unwrap_or(0)
    }
    fn buffered_secs(&self) -> f64 {
        if self.chunks.len() < 2 {
            return 0.0;
        }
        ((self.newest_media_ms() - self.oldest_media_ms()).max(0) as f64) / 1000.0
    }

    fn current_media_ms(&self, wall_ms: i64) -> i64 {
        if self.has_pcr {
            self.pcr_timeline_ms.round() as i64
        } else if self.first_chunk_ms > 0 {
            (wall_ms - self.first_chunk_ms).max(0)
        } else {
            0
        }
    }

    fn push_chunk(&mut self, bytes: Vec<u8>, has_pat: bool, has_rai: bool, ts_ms: i64) {
        if bytes.is_empty() {
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.first_chunk_ms == 0 {
            self.first_chunk_ms = ts_ms;
        }
        let media_ms = self.current_media_ms(ts_ms).max(self.newest_media_ms());
        let data = Bytes::from(bytes);
        if let Some(ref tx) = self.rec_tx {
            if tx.send(data.clone()).is_err() {
                self.rec_tx = None;
            }
        }
        self.mem += data.len();
        self.chunks.push_back(Chunk {
            seq,
            ts_ms,
            media_ms,
            bytes: data,
            has_pat,
            has_rai,
        });
        while self.mem > MEM_CAP
            || (self.chunks.len() > 2
                && (ts_ms - self.oldest_wall_ms() > MAX_AGE_MS
                    || media_ms - self.oldest_media_ms() > MAX_AGE_MS))
        {
            if let Some(old) = self.chunks.pop_front() {
                self.mem = self.mem.saturating_sub(old.bytes.len());
            } else {
                break;
            }
        }
    }

    /// Inspect a single 188-byte MPEG-TS packet.
    /// Returns `(has_pat, has_rai)` while assembling PAT, multi-packet PMT, and PCR timestamps.
    fn inspect_pkt(&mut self, pkt: &[u8], now: i64) -> (bool, bool) {
        if pkt.len() != TS_PKT || pkt[0] != 0x47 {
            return (false, false);
        }
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | (pkt[2] as u16);
        let pusi = (pkt[1] & 0x40) != 0;
        let afc = (pkt[3] >> 4) & 0x03;
        let cc = pkt[3] & 0x0F;
        let mut has_rai = false;

        // Parse adaptation field for Random Access Indicator (0x40) and PCR (0x10)
        if afc == 2 || afc == 3 {
            let af_len = pkt[4] as usize;
            if af_len >= 1 && 5 + af_len <= TS_PKT {
                let flags = pkt[5];
                if (flags & 0x40) != 0 && pid != 0 && Some(pid) != self.pmt_pid {
                    has_rai = true;
                }
                if (flags & 0x10) != 0
                    && af_len >= 7
                    && self.pcr_pid.map_or(true, |pp| pp == pid)
                {
                    if self.pcr_pid.is_none() {
                        self.pcr_pid = Some(pid);
                    }
                    let pcr_base: u64 = ((pkt[6] as u64) << 25)
                        | ((pkt[7] as u64) << 17)
                        | ((pkt[8] as u64) << 9)
                        | ((pkt[9] as u64) << 1)
                        | ((pkt[10] as u64) >> 7);
                    match self.last_pcr_base {
                        None => {
                            self.last_pcr_base = Some(pcr_base);
                            self.last_pcr_wall_ms = now;
                            if self.first_chunk_ms > 0 && !self.has_pcr {
                                self.pcr_timeline_ms = (now - self.first_chunk_ms).max(0) as f64;
                            }
                            self.has_pcr = true;
                        }
                        Some(prev) => {
                            let raw_delta = pcr_base.wrapping_sub(prev) & ((1u64 << 33) - 1);
                            // Normal forward PCR progress (< 30 s)
                            if raw_delta > 0 && raw_delta < 90_000 * 30 {
                                self.pcr_timeline_ms += (raw_delta as f64) / 90.0;
                            } else if raw_delta != 0 {
                                // Discontinuity (reconnect / ad splice): bridge cleanly with wall-clock delta
                                let wall_dt = (now - self.last_pcr_wall_ms).clamp(0, 5_000) as f64;
                                self.pcr_timeline_ms += wall_dt;
                            }
                            self.last_pcr_base = Some(pcr_base);
                            self.last_pcr_wall_ms = now;
                            self.has_pcr = true;
                        }
                    }
                }
            }
        }

        // PAT (PID 0)
        if pid == 0 {
            self.pat = Some(pkt.to_vec());
            if pusi && (afc == 1 || afc == 3) {
                let mut off = 4usize;
                if afc == 3 {
                    off += 1 + pkt.get(4).copied().unwrap_or(0) as usize;
                }
                if off < TS_PKT {
                    let ptr = pkt[off] as usize;
                    let sec = off + 1 + ptr;
                    if sec + 12 <= TS_PKT && pkt[sec] == 0x00 {
                        let sec_len = ((((pkt[sec + 1] & 0x0F) as usize) << 8)
                            | pkt[sec + 2] as usize)
                            .min(TS_PKT - sec - 3);
                        let end = (sec + 3 + sec_len).saturating_sub(4).min(TS_PKT);
                        let mut p = sec + 8;
                        while p + 4 <= end {
                            let prog_num = ((pkt[p] as u16) << 8) | pkt[p + 1] as u16;
                            let pmt_pid = (((pkt[p + 2] & 0x1F) as u16) << 8) | pkt[p + 3] as u16;
                            if prog_num != 0 && pmt_pid != 0 {
                                if self.pmt_pid != Some(pmt_pid) {
                                    self.pmt_pid = Some(pmt_pid);
                                    self.pmt_assembling = false;
                                    self.pmt_acc_pkts.clear();
                                }
                                break;
                            }
                            p += 4;
                        }
                    }
                }
            }
            return (true, has_rai);
        }

        // Multi-packet PMT assembly using PUSI + section_length (up to 4096 bytes)
        if Some(pid) == self.pmt_pid && (afc == 1 || afc == 3) {
            let mut off = 4usize;
            if afc == 3 {
                off += 1 + pkt.get(4).copied().unwrap_or(0) as usize;
            }
            if off < TS_PKT {
                if pusi {
                    let ptr = pkt[off] as usize;
                    let sec_start = off + 1 + ptr;
                    if sec_start + 3 <= TS_PKT && pkt[sec_start] == 0x02 {
                        let sec_len = ((((pkt[sec_start + 1] & 0x0F) as usize) << 8)
                            | pkt[sec_start + 2] as usize)
                            .min(MAX_PMT_BYTES.saturating_sub(3));
                        if sec_start + 10 <= TS_PKT {
                            let pp = (((pkt[sec_start + 8] & 0x1F) as u16) << 8)
                                | pkt[sec_start + 9] as u16;
                            if pp != 0 && pp != 0x1FFF {
                                self.pcr_pid = Some(pp);
                            }
                        }
                        let need = 3 + sec_len;
                        let have = TS_PKT - sec_start;
                        self.pmt_acc_pkts.clear();
                        self.pmt_acc_pkts.extend_from_slice(pkt);
                        self.pmt_section_need = need;
                        self.pmt_section_have = have;
                        self.pmt_cc = Some(cc);
                        if have >= need {
                            self.pmt = Some(self.pmt_acc_pkts.clone());
                            self.pmt_assembling = false;
                        } else {
                            self.pmt_assembling = true;
                        }
                    } else {
                        self.pmt_assembling = false;
                    }
                } else if self.pmt_assembling {
                    let cc_ok = self
                        .pmt_cc
                        .map_or(false, |prev| ((prev + 1) & 0x0F) == cc);
                    if cc_ok && self.pmt_acc_pkts.len() + TS_PKT <= MAX_PMT_BYTES {
                        self.pmt_cc = Some(cc);
                        self.pmt_acc_pkts.extend_from_slice(pkt);
                        self.pmt_section_have += TS_PKT - off;
                        if self.pmt_section_have >= self.pmt_section_need {
                            self.pmt = Some(self.pmt_acc_pkts.clone());
                            self.pmt_assembling = false;
                        }
                    } else {
                        self.pmt_assembling = false;
                        self.pmt_acc_pkts.clear();
                    }
                }
            }
        }

        (false, has_rai)
    }

    fn header_bytes(&self) -> Vec<u8> {
        let mut hdr = Vec::with_capacity(
            self.pat.as_ref().map_or(0, |v| v.len()) + self.pmt.as_ref().map_or(0, |v| v.len()),
        );
        if let Some(pat) = &self.pat {
            hdr.extend_from_slice(pat);
        }
        if let Some(pmt) = &self.pmt {
            hdr.extend_from_slice(pmt);
        }
        hdr
    }

    fn chunk_for_back(&self, back_secs: f64) -> Option<u64> {
        if self.chunks.is_empty() {
            return None;
        }
        let newest = self.newest_media_ms();
        let target = if back_secs <= 0.5 {
            // Live edge: start ~1.5 s back so ffmpeg gets immediate video+audio frames
            newest - 1500
        } else {
            newest - (back_secs * 1000.0) as i64
        };
        let pos = self.chunks.partition_point(|c| c.media_ms < target);
        let idx = pos.min(self.chunks.len() - 1);
        if back_secs <= 0.5 {
            // Prefer Random Access Indicator (clean IDR keyframe) within up to 6 chunks back, then PAT
            for back in 0..=6usize {
                if let Some(i) = idx.checked_sub(back) {
                    if self.chunks[i].has_rai {
                        return Some(self.chunks[i].seq);
                    }
                }
            }
            for back in 0..=4usize {
                if let Some(i) = idx.checked_sub(back) {
                    if self.chunks[i].has_pat {
                        return Some(self.chunks[i].seq);
                    }
                }
            }
        } else {
            // Rewind/DVR: prefer a nearby RAI keyframe within 3 chunks back for clean decoder lock
            for back in 0..=3usize {
                if let Some(i) = idx.checked_sub(back) {
                    if self.chunks[i].has_rai {
                        return Some(self.chunks[i].seq);
                    }
                }
            }
        }
        Some(self.chunks[idx].seq)
    }

    fn get_chunk(&self, seq: u64) -> Option<(u64, i64, Bytes)> {
        let front = self.chunks.front()?;
        if seq < front.seq {
            return Some((front.seq, front.media_ms, front.bytes.clone()));
        }
        let idx = (seq - front.seq) as usize;
        self.chunks
            .get(idx)
            .map(|c| (c.seq, c.media_ms, c.bytes.clone()))
    }
}

/// Find the first valid MPEG-TS sync pair (`0x47` followed by `0x47` exactly 188 bytes later).
fn find_ts_sync_pair(buf: &[u8]) -> Option<usize> {
    if buf.len() <= TS_PKT {
        return None;
    }
    let limit = buf.len() - TS_PKT;
    let mut i = 0usize;
    while i < limit {
        if let Some(rel) = buf[i..limit].iter().position(|&b| b == 0x47) {
            let pos = i + rel;
            if buf[pos + TS_PKT] == 0x47 {
                return Some(pos);
            }
            i = pos + 1;
        } else {
            break;
        }
    }
    None
}

fn looks_like_non_ts_text(buf: &[u8]) -> bool {
    let trimmed = match buf.iter().position(|b| !b.is_ascii_whitespace()) {
        Some(i) => &buf[i..],
        None => return false,
    };
    trimmed.starts_with(b"#EXTM3U")
        || trimmed.starts_with(b"<!DOCTYPE")
        || trimmed.starts_with(b"<!doctype")
        || trimmed.starts_with(b"<html")
        || trimmed.starts_with(b"<HTML")
        || trimmed.starts_with(b"<?xml")
        || trimmed.starts_with(b"{\"")
}

pub struct LiveBuf {
    pub gen: u64,
    pub url: String,
    ua: String,
    st: Mutex<St>,
    data: Notify,
    close_notify: Notify,
    closed: AtomicBool,
    http: reqwest::Client,
}

impl LiveBuf {
    fn local_url(&self, back_secs: f64) -> String {
        if back_secs > 0.5 {
            format!(
                "http://127.0.0.1:{}/livebuf/{}?back={:.1}",
                PORT.load(SeqCst),
                self.gen,
                back_secs
            )
        } else {
            format!("http://127.0.0.1:{}/livebuf/{}", PORT.load(SeqCst), self.gen)
        }
    }

    fn close(&self) {
        if !self.closed.swap(true, SeqCst) {
            let was_connected = {
                let mut st = self.st.lock().unwrap();
                // Drop DVR tap sender so any active recording flushes & remuxes cleanly
                st.rec_tx = None;
                st.connected
            };
            if was_connected {
                LAST_CLOSE.store(now_ms(), SeqCst);
                crate::slot::mark_released();
            }
            // Clear REC handle if it belonged to this livebuf generation
            let mut rec_g = REC.lock().unwrap();
            if rec_g.as_ref().map_or(false, |r| r.lb_gen == self.gen) {
                rec_g.take();
            }
        }
        self.close_notify.notify_waiters();
        self.data.notify_waiters();
    }

    async fn wait_closed(&self) {
        loop {
            if self.closed.load(SeqCst) {
                return;
            }
            let n = self.close_notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.closed.load(SeqCst) {
                return;
            }
            n.await;
        }
    }

    async fn sleep_cancellable(&self, ms: u64, lease: Option<&SlotLease>) -> bool {
        if self.closed.load(SeqCst) || lease.map_or(false, |l| l.is_cancelled()) {
            return false;
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => {
                !self.closed.load(SeqCst) && !lease.map_or(false, |l| l.is_cancelled())
            }
            _ = self.wait_closed() => false,
            _ = async {
                if let Some(l) = lease { l.cancelled().await; }
                else { std::future::pending::<()>().await; }
            } => false,
        }
    }

    fn is_idle_expired(&self) -> bool {
        let mut st = self.st.lock().unwrap();
        let now = now_ms();
        if !st.readers.is_empty() || st.rec_tx.is_some() {
            st.last_touch = now;
            return false;
        }
        now.saturating_sub(st.last_touch) >= IDLE_TIMEOUT_MS
    }

    async fn run(self: Arc<Self>) {
        let mut backoff = 1000u64;
        let mut slot_errs = 0u32;

        'outer: loop {
            if self.closed.load(SeqCst) {
                break;
            }
            if self.is_idle_expired() {
                println!("[livebuf] idle timeout (35s); releasing upstream");
                self.close();
                break;
            }

            let lease = crate::slot::acquire(Owner::LiveBuf, &self.url);
            let ready = tokio::select! {
                ok = lease.wait_cooldown() => ok,
                _ = self.wait_closed() => false,
            };
            if !ready || self.closed.load(SeqCst) {
                lease.release();
                self.close();
                break;
            }

            let resp = tokio::select! {
                r = self.http.get(&self.url)
                    .header(header::USER_AGENT, &self.ua)
                    .timeout(std::time::Duration::from_secs(3600 * 12))
                    .send() => Some(r),
                _ = lease.cancelled() => None,
                _ = self.wait_closed() => None,
            };

            let mut stream = match resp {
                None => {
                    // Cancelled or closed mid-connect
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    lease.release();
                    self.close();
                    break;
                }
                Some(Ok(r)) if r.status().is_success() => {
                    backoff = 1000;
                    slot_errs = 0;
                    {
                        let mut st = self.st.lock().unwrap();
                        st.connected = true;
                        st.failing_since = None;
                    }
                    Box::pin(r.bytes_stream())
                }
                Some(Ok(r)) => {
                    let code = r.status().as_u16();
                    println!("[livebuf] provider said {}", r.status());
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    lease.release();
                    let have_data = {
                        let mut st = self.st.lock().unwrap();
                        st.connected = false;
                        st.reconnects += 1;
                        if st.failing_since.is_none() {
                            st.failing_since = Some(now_ms());
                        }
                        !st.chunks.is_empty()
                    };
                    // HTTP 404 / 410 are terminal: stop infinite retry loops immediately
                    if code == 404 || code == 410 {
                        println!("[livebuf] terminal HTTP {code}; closing");
                        self.close();
                        break;
                    }
                    if matches!(code, 401 | 403 | 429 | 460) {
                        slot_errs += 1;
                        let limit = if have_data { 5 } else { 3 };
                        if slot_errs >= limit {
                            println!(
                                "[livebuf] giving up after {slot_errs} consecutive {code} rejections"
                            );
                            self.close();
                            break;
                        }
                        let wait = match slot_errs {
                            1 => 3000,
                            2 => 6000,
                            _ => 10_000,
                        };
                        if !self.sleep_cancellable(wait, None).await {
                            break;
                        }
                        continue;
                    }
                    slot_errs = 0;
                    let wait = backoff;
                    backoff = (backoff * 2).min(6000);
                    if !self.sleep_cancellable(wait, None).await {
                        break;
                    }
                    continue;
                }
                Some(Err(e)) => {
                    slot_errs = 0;
                    crate::slot::mark_released();
                    lease.release();
                    println!("[livebuf] connect failed ({e})");
                    {
                        let mut st = self.st.lock().unwrap();
                        st.connected = false;
                        st.reconnects += 1;
                        if st.failing_since.is_none() {
                            st.failing_since = Some(now_ms());
                        }
                    }
                    let wait = backoff;
                    backoff = (backoff * 2).min(6000);
                    if !self.sleep_cancellable(wait, None).await {
                        break;
                    }
                    continue;
                }
            };

            let mut carry: Vec<u8> = Vec::with_capacity(TS_PKT * 8);
            let mut acc: Vec<u8> = Vec::with_capacity(CHUNK_TARGET + TS_PKT * 4);
            let mut acc_pat = false;
            let mut acc_rai = false;
            let mut synced = false;
            let mut last_flush = now_ms();

            loop {
                if self.closed.load(SeqCst) || lease.is_cancelled() {
                    drop(stream);
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                    lease.release();
                    self.close();
                    break 'outer;
                }
                if self.is_idle_expired() {
                    drop(stream);
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                    lease.release();
                    println!("[livebuf] idle timeout during stream (35s); releasing upstream");
                    self.close();
                    break 'outer;
                }

                let next = tokio::select! {
                    r = tokio::time::timeout(std::time::Duration::from_secs(12), stream.next()) => Some(r),
                    _ = lease.cancelled() => None,
                    _ = self.wait_closed() => None,
                };
                match next {
                    None => {
                        drop(stream);
                        crate::slot::mark_released();
                        LAST_CLOSE.store(now_ms(), SeqCst);
                        self.st.lock().unwrap().connected = false;
                        lease.release();
                        self.close();
                        break 'outer;
                    }
                    Some(Ok(Some(Ok(chunk)))) => {
                        carry.extend_from_slice(&chunk);
                        if !synced {
                            if looks_like_non_ts_text(&carry) {
                                println!("[livebuf] non-TS text/playlist response detected; failing fast");
                                drop(stream);
                                crate::slot::mark_released();
                                LAST_CLOSE.store(now_ms(), SeqCst);
                                self.st.lock().unwrap().connected = false;
                                lease.release();
                                self.close();
                                break 'outer;
                            }
                            match find_ts_sync_pair(&carry) {
                                Some(pos) => {
                                    if pos > 0 {
                                        carry.drain(..pos);
                                    }
                                    synced = true;
                                }
                                None => {
                                    if carry.len() >= NON_TS_FAIL_BYTES {
                                        println!(
                                            "[livebuf] non-TS response (no 0x47 sync pair in first {} KB); failing fast",
                                            NON_TS_FAIL_BYTES >> 10
                                        );
                                        drop(stream);
                                        crate::slot::mark_released();
                                        LAST_CLOSE.store(now_ms(), SeqCst);
                                        self.st.lock().unwrap().connected = false;
                                        lease.release();
                                        self.close();
                                        break 'outer;
                                    }
                                    continue;
                                }
                            }
                        }

                        // Align to 188-byte MPEG-TS packet sync byte 0x47
                        let mut i = 0usize;
                        let mut st = self.st.lock().unwrap();
                        while carry.len().saturating_sub(i) >= TS_PKT {
                            if carry[i] != 0x47 {
                                if let Some(rel) = carry[i..].iter().position(|&b| b == 0x47) {
                                    let pos = i + rel;
                                    if pos + TS_PKT < carry.len() {
                                        if carry[pos + TS_PKT] == 0x47 {
                                            i = pos;
                                            continue;
                                        }
                                        i = pos + 1;
                                        continue;
                                    } else {
                                        i = pos;
                                        break;
                                    }
                                } else {
                                    i = carry.len();
                                    break;
                                }
                            }
                            let pkt = &carry[i..i + TS_PKT];
                            let now = now_ms();
                            let (hp, hr) = st.inspect_pkt(pkt, now);
                            if hp {
                                acc_pat = true;
                            }
                            if hr {
                                acc_rai = true;
                            }
                            acc.extend_from_slice(pkt);
                            i += TS_PKT;
                            if acc.len() >= CHUNK_TARGET
                                || (acc.len() >= TS_PKT * 20 && now - last_flush >= 250)
                            {
                                let out = std::mem::replace(
                                    &mut acc,
                                    Vec::with_capacity(CHUNK_TARGET + TS_PKT * 4),
                                );
                                let pat_f = std::mem::replace(&mut acc_pat, false);
                                let rai_f = std::mem::replace(&mut acc_rai, false);
                                st.push_chunk(out, pat_f, rai_f, now);
                                last_flush = now;
                            }
                        }
                        if i > 0 {
                            carry.drain(..i);
                        }
                        let now = now_ms();
                        if !acc.is_empty() && now - last_flush >= 250 {
                            let out = std::mem::replace(
                                &mut acc,
                                Vec::with_capacity(CHUNK_TARGET + TS_PKT * 4),
                            );
                            let pat_f = std::mem::replace(&mut acc_pat, false);
                            let rai_f = std::mem::replace(&mut acc_rai, false);
                            st.push_chunk(out, pat_f, rai_f, now);
                            last_flush = now;
                        }
                        drop(st);
                        self.data.notify_waiters();
                    }
                    Some(other) => {
                        if !acc.is_empty() {
                            let mut st = self.st.lock().unwrap();
                            let out = std::mem::take(&mut acc);
                            let pat_f = std::mem::take(&mut acc_pat);
                            let rai_f = std::mem::take(&mut acc_rai);
                            st.push_chunk(out, pat_f, rai_f, now_ms());
                        }
                        let non_ts_eof = !synced
                            && matches!(other, Ok(None))
                            && self.st.lock().unwrap().chunks.is_empty();
                        drop(stream);
                        crate::slot::mark_released();
                        LAST_CLOSE.store(now_ms(), SeqCst);
                        lease.release();
                        if non_ts_eof {
                            println!(
                                "[livebuf] upstream ended before any MPEG-TS sync ({} bytes); failing fast",
                                carry.len()
                            );
                            self.st.lock().unwrap().connected = false;
                            self.close();
                            break 'outer;
                        }
                        println!(
                            "[livebuf] upstream {}; reconnecting",
                            match other {
                                Err(_) => "went silent".to_string(),
                                Ok(Some(Err(e))) => format!("failed ({e})"),
                                _ => "ended".into(),
                            }
                        );
                        {
                            let mut st = self.st.lock().unwrap();
                            st.connected = false;
                            st.reconnects += 1;
                            if st.failing_since.is_none() {
                                st.failing_since = Some(now_ms());
                            }
                        }
                        self.data.notify_waiters();
                        break;
                    }
                }
            }
        }
    }
}

async fn wait_first_chunk(lb: &Arc<LiveBuf>, timeout_secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let notified = lb.data.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if lb.closed.load(SeqCst) {
            return false;
        }
        if !lb.st.lock().unwrap().chunks.is_empty() {
            return true;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return !lb.closed.load(SeqCst);
        }
    }
}

/// Attach the live ring buffer to this upstream URL. Reuses an already-attached ring for the same
/// channel (so pause/resume and rewind never drop the provider connection).
pub async fn attach(url: &str, ua: &str, back_secs: f64) -> Option<String> {
    crate::spool::detach();
    // Fast-path reuse
    if let Some(s) = CUR
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.url == url && !s.closed.load(SeqCst))
        .cloned()
    {
        s.st.lock().unwrap().last_touch = now_ms();
        return Some(s.local_url(back_secs));
    }

    let http = reqwest::Client::builder()
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .pool_max_idle_per_host(0)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let lb = Arc::new(LiveBuf {
        gen: GEN.fetch_add(1, SeqCst) + 1,
        url: url.to_string(),
        ua: ua.to_string(),
        st: Mutex::new(St {
            last_touch: now_ms(),
            ..Default::default()
        }),
        data: Notify::new(),
        close_notify: Notify::new(),
        closed: AtomicBool::new(false),
        http,
    });

    // Atomic replacement under CUR lock eliminates TOCTOU connection leaks
    let (reuse, old) = {
        let mut g = CUR.lock().unwrap();
        if let Some(existing) = g
            .as_ref()
            .filter(|s| s.url == url && !s.closed.load(SeqCst))
            .cloned()
        {
            (Some(existing), None)
        } else {
            (None, g.replace(lb.clone()))
        }
    };
    if let Some(existing) = reuse {
        existing.st.lock().unwrap().last_touch = now_ms();
        return Some(existing.local_url(back_secs));
    }
    if let Some(prev) = old {
        prev.close();
    }

    tokio::spawn(lb.clone().run());
    if wait_first_chunk(&lb, 10).await {
        Some(lb.local_url(back_secs))
    } else {
        None
    }
}

pub fn local_for(url: &str, back_secs: f64) -> Option<String> {
    CUR.lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.url == url && !s.closed.load(SeqCst))
        .map(|s| s.local_url(back_secs))
}
pub fn attached(url: &str) -> bool {
    local_for(url, 0.0).is_some()
}
pub fn is_active() -> bool {
    CUR.lock()
        .unwrap()
        .as_ref()
        .map_or(false, |s| !s.closed.load(SeqCst))
}
pub fn is_recording() -> bool {
    REC.lock().unwrap().is_some()
}
pub fn touch() {
    if let Some(s) = CUR.lock().unwrap().as_ref() {
        if !s.closed.load(SeqCst) {
            s.st.lock().unwrap().last_touch = now_ms();
        }
    }
}
pub fn detach() {
    if let Some(s) = CUR.lock().unwrap().take() {
        s.close();
    }
}

pub fn stats() -> Value {
    let Some(s) = CUR.lock().unwrap().clone().filter(|s| !s.closed.load(SeqCst)) else {
        return json!(null);
    };
    let st = s.st.lock().unwrap();
    let newest = st.newest_media_ms();
    let reader_ms = st.readers.values().copied().max().unwrap_or(newest);
    let behind = if newest > 0 && reader_ms > 0 {
        ((newest - reader_ms).max(0) as f64) / 1000.0
    } else {
        0.0
    };
    json!({
        "bufferedSecs": st.buffered_secs().round(),
        "behindSecs": behind.round(),
        "memMB": st.mem >> 20,
        "connected": st.connected,
        "reconnects": st.reconnects,
        "recording": st.rec_tx.is_some(),
        "hasPcr": st.has_pcr,
    })
}

fn sanitize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len().min(64));
    let mut prev_us = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
            prev_us = false;
        } else if !prev_us && !out.is_empty() {
            out.push('_');
            prev_us = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    let clean = out.trim_matches('_').to_string();
    if clean.is_empty() {
        "live".to_string()
    } else {
        clean
    }
}

fn write_rec_meta(path: &FsPath, meta: &Value) {
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(meta) {
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Zero-connection Live DVR Recording Tap: attaches directly to the active `livebuf::CUR` ring
/// (0 extra upstream connections!), dumps up to `retroactive_secs` already buffered in RAM plus
/// live incoming MPEG-TS packets to `data/recordings/<safe_title>_<ts>.ts`, and on stop remuxes
/// cleanly to `.mkv` in a background thread with a `.json` metadata sidecar.
pub fn start_record(data_dir: &FsPath, title: &str, retroactive_secs: f64) -> Result<Value, String> {
    if REC.lock().unwrap().is_some() {
        return Err("A live recording is already in progress".into());
    }
    let lb = CUR
        .lock()
        .unwrap()
        .clone()
        .filter(|s| !s.closed.load(SeqCst))
        .ok_or_else(|| "No active live channel is buffering".to_string())?;

    let rec_dir = data_dir.join("recordings");
    std::fs::create_dir_all(&rec_dir).map_err(|e| format!("Could not create recordings dir: {e}"))?;

    let safe = sanitize_title(title);
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let base = format!("{safe}_{stamp}");
    let ts_name = format!("{base}.ts");
    let mkv_name = format!("{base}.mkv");
    let ts_path: PathBuf = rec_dir.join(&ts_name);
    let mkv_path: PathBuf = rec_dir.join(&mkv_name);
    let meta_path: PathBuf = rec_dir.join(format!("{base}.json"));

    let file = std::fs::File::create(&ts_path)
        .map_err(|e| format!("Could not create recording file: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel::<Bytes>();

    // Snapshot PAT/PMT + retroactive ring chunks + attach live channel atomically under St lock
    let (hdr, retro_chunks, retro_actual_secs) = {
        let mut st = lb.st.lock().unwrap();
        if st.rec_tx.is_some() {
            drop(st);
            drop(file);
            let _ = std::fs::remove_file(&ts_path);
            return Err("A live recording is already attached".into());
        }
        let hdr = st.header_bytes();
        let retro_req = retroactive_secs.max(0.0);
        let mut chunks: Vec<Bytes> = Vec::new();
        let mut covered_secs = 0.0;
        if retro_req > 0.0 && !st.chunks.is_empty() {
            let start_seq = if retro_req >= st.buffered_secs() {
                st.chunks.front().map(|c| c.seq)
            } else {
                st.chunk_for_back(retro_req)
            };
            if let Some(sseq) = start_seq {
                let newest_m = st.newest_media_ms();
                let mut first_m: Option<i64> = None;
                for c in &st.chunks {
                    if c.seq >= sseq {
                        if first_m.is_none() {
                            first_m = Some(c.media_ms);
                        }
                        chunks.push(c.bytes.clone());
                    }
                }
                if let Some(fm) = first_m {
                    covered_secs = ((newest_m - fm).max(0) as f64) / 1000.0;
                }
            }
        }
        st.rec_tx = Some(tx);
        st.last_touch = now_ms();
        (hdr, chunks, (covered_secs * 10.0).round() / 10.0)
    };

    let started_ms = now_ms();
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let clean_title = if title.trim().is_empty() {
        safe.clone()
    } else {
        title.trim().to_string()
    };

    let init_meta = json!({
        "id": base,
        "title": clean_title,
        "status": "recording",
        "file": mkv_name,
        "tsFile": ts_name,
        "startedAt": started_ms,
        "retroactiveSecs": retro_actual_secs,
        "bytes": 0,
    });
    write_rec_meta(&meta_path, &init_meta);

    {
        let mut rec_g = REC.lock().unwrap();
        *rec_g = Some(ActiveRec {
            id: base.clone(),
            title: clean_title.clone(),
            mkv_file: mkv_name.clone(),
            ts_file: ts_name.clone(),
            started_ms,
            retroactive_secs: retro_actual_secs,
            bytes: bytes_counter.clone(),
            lb_gen: lb.gen,
            lb: lb.clone(),
        });
    }

    let bc = bytes_counter.clone();
    let t_id = base.clone();
    let t_title = clean_title.clone();
    let t_mkv_name = mkv_name.clone();
    let t_ts_name = ts_name.clone();

    std::thread::spawn(move || {
        let mut w = BufWriter::with_capacity(256 * 1024, file);
        if !hdr.is_empty() && w.write_all(&hdr).is_ok() {
            bc.fetch_add(hdr.len() as u64, SeqCst);
        }
        for c in retro_chunks {
            if w.write_all(&c).is_ok() {
                bc.fetch_add(c.len() as u64, SeqCst);
            }
        }
        while let Ok(chunk) = rx.recv() {
            if w.write_all(&chunk).is_err() {
                break;
            }
            bc.fetch_add(chunk.len() as u64, SeqCst);
        }
        let _ = w.flush();
        drop(w);

        let ended_ms = now_ms();
        let total_bytes = bc.load(SeqCst);
        let dur_secs =
            (((ended_ms - started_ms).max(0) as f64) / 1000.0 + retro_actual_secs).round();

        let remux_meta = json!({
            "id": t_id,
            "title": t_title,
            "status": "remuxing",
            "file": t_mkv_name,
            "tsFile": t_ts_name,
            "startedAt": started_ms,
            "endedAt": ended_ms,
            "retroactiveSecs": retro_actual_secs,
            "durationSecs": dur_secs,
            "bytes": total_bytes,
        });
        write_rec_meta(&meta_path, &remux_meta);
        *LAST_REC.lock().unwrap() = Some(remux_meta);

        // Clean background remux from raw MPEG-TS -> Matroska (.mkv) with -c copy
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-fflags",
            "+genpts+discardcorrupt",
            "-i",
        ])
        .arg(&ts_path)
        .args(["-map", "0:v?", "-map", "0:a?", "-map", "0:s?", "-c", "copy"])
        .arg(&mkv_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }

        let remux_ok = match cmd.output() {
            Ok(out) if out.status.success() => {
                std::fs::metadata(&mkv_path).map_or(false, |m| m.len() > 0)
            }
            Ok(out) => {
                let err_txt = String::from_utf8_lossy(&out.stderr);
                println!("[livebuf/dvr] ffmpeg remux failed: {}", err_txt.trim());
                false
            }
            Err(e) => {
                println!("[livebuf/dvr] could not launch ffmpeg for remux: {e}");
                false
            }
        };

        let final_meta = if remux_ok {
            let mkv_size = std::fs::metadata(&mkv_path).map(|m| m.len()).unwrap_or(total_bytes);
            let _ = std::fs::remove_file(&ts_path);
            println!(
                "[livebuf/dvr] saved {} ({} MB, {:.0}s)",
                t_mkv_name,
                mkv_size >> 20,
                dur_secs
            );
            json!({
                "id": t_id,
                "title": t_title,
                "status": "ready",
                "file": t_mkv_name,
                "startedAt": started_ms,
                "endedAt": ended_ms,
                "retroactiveSecs": retro_actual_secs,
                "durationSecs": dur_secs,
                "sizeBytes": mkv_size,
            })
        } else {
            let ts_size = std::fs::metadata(&ts_path).map(|m| m.len()).unwrap_or(total_bytes);
            json!({
                "id": t_id,
                "title": t_title,
                "status": "ts_only",
                "file": t_ts_name,
                "startedAt": started_ms,
                "endedAt": ended_ms,
                "retroactiveSecs": retro_actual_secs,
                "durationSecs": dur_secs,
                "sizeBytes": ts_size,
            })
        };
        write_rec_meta(&meta_path, &final_meta);
        *LAST_REC.lock().unwrap() = Some(final_meta);
    });

    Ok(json!({
        "ok": true,
        "recording": true,
        "id": base,
        "title": clean_title,
        "file": mkv_name,
        "tsFile": ts_name,
        "startedAt": started_ms,
        "retroactiveSecs": retro_actual_secs,
    }))
}

/// Stop the active live DVR recording tap and trigger background `.ts -> .mkv` remux.
pub fn stop_record() -> Value {
    let Some(rec) = REC.lock().unwrap().take() else {
        return json!({ "ok": false, "recording": false, "error": "Not recording" });
    };
    {
        let mut st = rec.lb.st.lock().unwrap();
        st.rec_tx = None;
        st.last_touch = now_ms();
    }
    let bytes = rec.bytes.load(SeqCst);
    let dur_secs =
        (((now_ms() - rec.started_ms).max(0) as f64) / 1000.0 + rec.retroactive_secs).round();
    json!({
        "ok": true,
        "recording": false,
        "remuxing": true,
        "id": rec.id,
        "title": rec.title,
        "file": rec.mkv_file,
        "tsFile": rec.ts_file,
        "bytes": bytes,
        "mb": bytes >> 20,
        "durationSecs": dur_secs,
    })
}

/// Live DVR recording status (active recording telemetry or most recent finished/remuxing clip).
pub fn record_status() -> Value {
    if let Some(rec) = REC.lock().unwrap().as_ref() {
        let bytes = rec.bytes.load(SeqCst);
        let dur_secs =
            (((now_ms() - rec.started_ms).max(0) as f64) / 1000.0 + rec.retroactive_secs).round();
        return json!({
            "recording": true,
            "id": rec.id,
            "title": rec.title,
            "file": rec.mkv_file,
            "tsFile": rec.ts_file,
            "startedAt": rec.started_ms,
            "retroactiveSecs": rec.retroactive_secs,
            "durationSecs": dur_secs,
            "bytes": bytes,
            "mb": bytes >> 20,
        });
    }
    json!({
        "recording": false,
        "last": LAST_REC.lock().unwrap().clone(),
    })
}

/// GET /livebuf/{gen}?back={secs}
pub async fn serve(Path(gen): Path<u64>, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(lb) = CUR
        .lock()
        .unwrap()
        .clone()
        .filter(|s| s.gen == gen && !s.closed.load(SeqCst))
    else {
        return (StatusCode::NOT_FOUND, "gone").into_response();
    };
    let back: f64 = q
        .get("back")
        .and_then(|x| x.parse::<f64>().ok())
        .unwrap_or(0.0)
        .max(0.0);
    let (id, start_seq, header_bytes) = {
        let mut st = lb.st.lock().unwrap();
        st.last_touch = now_ms();
        st.next_reader += 1;
        let id = st.next_reader;
        let seq = st.chunk_for_back(back).unwrap_or(st.next_seq);
        let ts = st
            .get_chunk(seq)
            .map(|x| x.1)
            .unwrap_or_else(|| st.newest_media_ms());
        st.readers.insert(id, ts);
        // Prepend latest PAT + complete assembled PMT so ffmpeg immediately detects streams at any rewind point
        let hdr = st.header_bytes();
        (
            id,
            seq,
            if hdr.is_empty() {
                None
            } else {
                Some(Bytes::from(hdr))
            },
        )
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
    let stream = futures_util::stream::unfold(
        (g, start_seq, header_bytes),
        move |(g, mut seq, mut hdr)| async move {
            if let Some(h) = hdr.take() {
                return Some((Ok::<Bytes, std::io::Error>(h), (g, seq, None)));
            }
            let lb = g.0.clone();
            loop {
                if lb.closed.load(SeqCst) {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionAborted,
                            "livebuf closed",
                        )),
                        (g, seq, None),
                    ));
                }
                let notified = lb.data.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let mut st = lb.st.lock().unwrap();
                    st.last_touch = now_ms();
                    if let Some((got_seq, ts, bytes)) = st.get_chunk(seq) {
                        if let Some(r) = st.readers.get_mut(&g.1) {
                            *r = ts;
                        }
                        seq = got_seq + 1;
                        return Some((Ok(bytes), (g, seq, None)));
                    }
                    if st.failing_since.map_or(false, |t| now_ms() - t > 35_000) {
                        return Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "live upstream unreachable",
                            )),
                            (g, seq, None),
                        ));
                    }
                }
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
            }
        },
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/MP2T")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pkt(pid: u16, pusi: bool, afc: u8, cc: u8) -> [u8; TS_PKT] {
        let mut p = [0xFFu8; TS_PKT];
        p[0] = 0x47;
        p[1] = (if pusi { 0x40 } else { 0 }) | ((pid >> 8) as u8 & 0x1F);
        p[2] = (pid & 0xFF) as u8;
        p[3] = ((afc & 0x03) << 4) | (cc & 0x0F);
        p
    }

    #[test]
    fn multi_packet_pmt_rai_and_pcr_parsing() {
        let mut st = St::default();
        // Build PAT announcing PMT PID 0x100
        let mut pat = make_pkt(0, true, 1, 0);
        pat[4] = 0; // pointer_field
        pat[5] = 0x00; // table_id
        pat[6] = 0xB0;
        pat[7] = 13; // section_length = 13
        pat[13] = 0x00;
        pat[14] = 0x01; // program_number = 1
        pat[15] = 0xE1;
        pat[16] = 0x00; // PMT PID = 0x0100
        let (hp, _) = st.inspect_pkt(&pat, 1000);
        assert!(hp);
        assert_eq!(st.pmt_pid, Some(0x0100));

        // Build 2-packet PMT (section_length = 220 -> needs 223 bytes > 183 payload bytes in pkt 1)
        let mut pmt1 = make_pkt(0x0100, true, 1, 3);
        pmt1[4] = 0; // pointer
        pmt1[5] = 0x02; // PMT table_id
        pmt1[6] = 0xB0;
        pmt1[7] = 220; // section_length = 220
        pmt1[13] = 0xE1;
        pmt1[14] = 0x01; // PCR PID = 0x0101
        st.inspect_pkt(&pmt1, 1000);
        assert!(st.pmt.is_none(), "incomplete multi-packet PMT must not overwrite yet");
        assert_eq!(st.pcr_pid, Some(0x0101));

        let pmt2 = make_pkt(0x0100, false, 1, 4);
        st.inspect_pkt(&pmt2, 1000);
        assert_eq!(st.pmt.as_ref().map(|v| v.len()), Some(TS_PKT * 2));

        // Video packet with RAI (0x40) + PCR (0x10) on PCR PID 0x0101
        let mut vp1 = make_pkt(0x0101, true, 3, 0);
        vp1[4] = 7; // adaptation_field_length
        vp1[5] = 0x50; // RAI (0x40) | PCR (0x10)
        // PCR base = 90_000 (1.0 s)
        let pcr1: u64 = 90_000;
        vp1[6] = (pcr1 >> 25) as u8;
        vp1[7] = (pcr1 >> 17) as u8;
        vp1[8] = (pcr1 >> 9) as u8;
        vp1[9] = (pcr1 >> 1) as u8;
        vp1[10] = ((pcr1 & 1) << 7) as u8;
        vp1[11] = 0;
        let (_, rai) = st.inspect_pkt(&vp1, 1000);
        assert!(rai);
        assert!(st.has_pcr);

        // Second PCR 2.5 s later (90_000 + 225_000 = 315_000)
        let mut vp2 = make_pkt(0x0101, false, 3, 1);
        vp2[4] = 7;
        vp2[5] = 0x10;
        let pcr2: u64 = 315_000;
        vp2[6] = (pcr2 >> 25) as u8;
        vp2[7] = (pcr2 >> 17) as u8;
        vp2[8] = (pcr2 >> 9) as u8;
        vp2[9] = (pcr2 >> 1) as u8;
        vp2[10] = ((pcr2 & 1) << 7) as u8;
        vp2[11] = 0;
        st.inspect_pkt(&vp2, 1050);
        assert!((st.pcr_timeline_ms - 2500.0).abs() < 1.0);
    }

    #[test]
    fn non_ts_detection() {
        assert!(looks_like_non_ts_text(b"#EXTM3U\n#EXT-X-VERSION:3"));
        assert!(looks_like_non_ts_text(b"  <html><body>403</body></html>"));
        assert!(!looks_like_non_ts_text(&[0x47, 0x40, 0x00, 0x10]));
        assert_eq!(find_ts_sync_pair(&vec![0xAA; 1024]), None);
        let mut ts = vec![0x00u8; 400];
        ts[10] = 0x47;
        ts[10 + TS_PKT] = 0x47;
        assert_eq!(find_ts_sync_pair(&ts), Some(10));
    }
}
