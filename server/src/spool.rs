// Read-ahead cache ("spool") for films and episodes.
//
// One task owns the single provider connection (via `crate::slot`). It downloads ahead of playback
// into memory (1 MB blocks, about 2.5 minutes of video ahead) and serves the bytes to ffmpeg/ffprobe/mpv
// over a local URL with Range support. So:
//   - provider hiccups and dropped connections are absorbed by the buffer: the spool reconnects with
//     `Range: bytes=N-` in the background while playback carries on from memory;
//   - seeks and restarts within what's downloaded never touch the provider (no connection churn, no
//     release wait), and the probe + the stream share one connection;
//   - initial priming fills up to 24 MB (or EOF) and holds a 5-second reader-gap grace window so
//     probe -> mpv HDR display switches reuse the exact same warm upstream TCP socket;
//   - when the buffer is full the provider connection is closed politely; it reopens when needed;
//   - once the current episode reaches EOF in memory, `pre_spool_next` can warm the next episode's
//     first 24 MB in a secondary slot without disturbing the currently playing episode.
// Live TV / catch-up don't use it (no ranges; they can't run ahead of the broadcast).
use crate::slot::{Owner, SlotLease};
use crate::util::now_ms;
use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const BLOCK: u64 = 1 << 20;
const MEM_CAP: u64 = 900 << 20; // most memory the cache may hold
const BEHIND: u64 = 96 << 20; // kept behind the playhead (quick back-seeks)
const PRIME_BYTES: u64 = 24 << 20; // initial warm-up before readers attach (or EOF)
const READER_GAP_GRACE_MS: i64 = 5_000; // keep warm TCP socket across ffprobe -> mpv HDR settle
const AHEAD_SECS: f64 = 150.0;
#[cfg_attr(not(test), allow(dead_code))]
const REOPEN_WAIT_MS: i64 = crate::slot::CROSS_URL_COOLDOWN_MS; // polite gap when switching titles or recovering from a slot error

enum OpenErr {
    Unsupported,     // provider ignores Range or sent mismatched Content-Range -> fall back
    RateOrSlot(u16), // 401/403/404/410/429/460 (slot still releasing, max connections, or auth)
    Transient,       // network drop or 5xx
}

fn is_slot_or_auth_status(code: u16) -> bool {
    matches!(code, 401 | 403 | 404 | 410 | 429 | 460)
}

fn should_retry_portal_url(code: u16) -> bool {
    // Only clear cached 302 CDN URL and retry portal on expired/missing token errors — NEVER on 460/429!
    matches!(code, 401 | 403 | 404 | 410)
}

fn slot_backoff_ms(consecutive: u32) -> u64 {
    match consecutive {
        0 | 1 => 3000,
        2 => 6000,
        _ => 10_000,
    }
}

/// Parse HTTP `Content-Range: bytes <first>-<last>/<total|*>` -> `(first, last, total)`.
fn parse_content_range(val: &str) -> Option<(u64, u64, Option<u64>)> {
    let trimmed = val.trim();
    let rest = trimmed
        .strip_prefix("bytes ")
        .or_else(|| trimmed.strip_prefix("BYTES "))
        .or_else(|| {
            if trimmed.len() > 6 && trimmed[..6].eq_ignore_ascii_case("bytes ") {
                Some(&trimmed[6..])
            } else {
                None
            }
        })?;
    let (range_part, total_part) = rest.trim().split_once('/')?;
    let (first_s, last_s) = range_part.trim().split_once('-')?;
    let first = first_s.trim().parse::<u64>().ok()?;
    let last = last_s.trim().parse::<u64>().ok()?;
    if last < first {
        return None;
    }
    let tp = total_part.trim();
    let total = if tp == "*" {
        None
    } else {
        Some(tp.parse::<u64>().ok()?)
    };
    Some((first, last, total))
}

/// Parse client HTTP `Range: bytes=...` against known `total` size.
/// Supports `bytes=start-end`, `bytes=start-`, and suffix ranges `bytes=-suffix`, rejecting
/// `end < start`, zero suffix, or out-of-bounds ranges cleanly with `Err(())` (416).
fn parse_http_range(val: &str, total: u64) -> Result<(u64, u64), ()> {
    if total == 0 {
        return Err(());
    }
    let trimmed = val.trim();
    if trimmed.len() < 6 || !trimmed[..6].eq_ignore_ascii_case("bytes=") {
        return Err(());
    }
    let spec = trimmed[6..].split(',').next().unwrap_or("").trim();
    let (s_str, e_str) = spec.split_once('-').ok_or(())?;
    let s_str = s_str.trim();
    let e_str = e_str.trim();
    if s_str.is_empty() {
        // Suffix range: bytes=-500 -> last 500 bytes of file
        let suffix: u64 = e_str.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let start = total.saturating_sub(suffix);
        return Ok((start, total - 1));
    }
    let start: u64 = s_str.parse().map_err(|_| ())?;
    if start >= total {
        return Err(());
    }
    if e_str.is_empty() {
        return Ok((start, total - 1));
    }
    let end_raw: u64 = e_str.parse().map_err(|_| ())?;
    if end_raw < start {
        return Err(());
    }
    Ok((start, end_raw.min(total - 1)))
}

static PORT: AtomicU16 = AtomicU16::new(7878);
static GEN: AtomicU64 = AtomicU64::new(0);
static LAST_CLOSE: AtomicI64 = AtomicI64::new(0);
static CUR: Mutex<Option<Arc<Spool>>> = Mutex::new(None);
/// Secondary pre-spooled next episode (primed only after `CUR` reaches EOF & releases upstream).
static PRE: Mutex<Option<Arc<Spool>>> = Mutex::new(None);

pub fn set_port(p: u16) {
    PORT.store(p, SeqCst);
}

struct Reader {
    pos: u64,
    served: u64,
    waiting: Option<u64>,
}

#[derive(Default)]
struct St {
    total: Option<u64>,
    blocks: BTreeMap<u64, Vec<u8>>, // block index -> bytes, always filled from the block's start
    mem: u64,
    readers: HashMap<u64, Reader>,
    next_reader: u64,
    bitrate: f64, // bytes per second (file size / duration), once known
    prime: bool,  // fetch the start of the file (up to PRIME_BYTES or EOF) even before readers join
    connected: bool,
    upstream_pos: u64,
    reconnects: u32,
    failing_since: Option<i64>,
    unsupported: bool,
    no_readers_since: i64,
    resolved_url: Option<String>, // cached final 302 CDN URL so MKV Cues / seeks skip portal redirect
}

impl St {
    /// Bytes available at `pos` (a slice of the block holding it), if downloaded.
    fn have(&self, pos: u64) -> Option<(&Vec<u8>, usize)> {
        let b = self.blocks.get(&(pos / BLOCK))?;
        let off = (pos % BLOCK) as usize;
        if b.len() > off {
            Some((b, off))
        } else {
            None
        }
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

    fn primary(&self) -> Option<u64> {
        self.readers
            .values()
            .max_by_key(|r| r.served)
            .map(|r| r.pos)
    }

    fn is_eof(&self) -> bool {
        let Some(total) = self.total.filter(|&t| t > 0) else {
            return false;
        };
        let p = self.primary().unwrap_or(0);
        self.frontier(p) >= total
    }

    fn cap(&self) -> u64 {
        if self.bitrate > 0.0 {
            ((self.bitrate * AHEAD_SECS) as u64).clamp(48 << 20, 700 << 20)
        } else {
            128 << 20
        }
    }

    /// Where the download should be: a reader waiting on missing bytes first, else read-ahead,
    /// else initial 24 MB prime (or EOF).
    fn target(&self) -> Option<u64> {
        let total = self.total.unwrap_or(u64::MAX);
        let waiting = self
            .readers
            .values()
            .filter_map(|r| r.waiting.map(|w| (r.served, w)))
            .filter(|(_, w)| *w < total && self.have(*w).is_none())
            .max_by_key(|x| x.0);
        if let Some((_, w)) = waiting {
            return Some(self.frontier(w));
        }
        if let Some(p) = self.primary() {
            let f = self.frontier(p);
            if f < total && f - p < self.cap() {
                return Some(f);
            }
        }
        if self.prime {
            let prime_goal = PRIME_BYTES.min(total);
            let f = self.frontier(0);
            if f < prime_goal {
                return Some(f);
            }
        }
        None
    }

    fn store(&mut self, mut pos: u64, data: &[u8]) {
        let mut i = 0;
        while i < data.len() {
            let idx = pos / BLOCK;
            let off = (pos % BLOCK) as usize;
            let room = (BLOCK as usize - off).min(data.len() - i);
            let b = self.blocks.entry(idx).or_default();
            if b.len() == off {
                b.extend_from_slice(&data[i..i + room]);
                self.mem += room as u64;
            }
            // (already have these bytes, or a gap before them: skip; blocks always fill from their start)
            i += room;
            pos += room as u64;
        }
        self.evict();
    }

    fn evict(&mut self) {
        if self.mem <= MEM_CAP {
            return;
        }
        let p = self.primary().unwrap_or(0);
        let keep_from = p.saturating_sub(BEHIND) / BLOCK;
        let keep_to = (p + self.cap()) / BLOCK + 1;
        // Protect blocks within ±4 blocks of ANY active reader's cursor or pending seek offset
        // so secondary readers (e.g. MKV Cues at EOF) never livelock against eviction when mem > 900 MB.
        let mut reader_windows: Vec<(u64, u64)> = Vec::with_capacity(self.readers.len() * 2);
        for r in self.readers.values() {
            let rb = r.pos / BLOCK;
            reader_windows.push((rb.saturating_sub(4), rb.saturating_add(4)));
            if let Some(w) = r.waiting {
                let wb = w / BLOCK;
                reader_windows.push((wb.saturating_sub(4), wb.saturating_add(4)));
            }
        }
        let is_protected = |k: u64| -> bool {
            (k >= keep_from && k <= keep_to)
                || reader_windows.iter().any(|&(lo, hi)| k >= lo && k <= hi)
        };
        let mut idx: Vec<u64> = self
            .blocks
            .keys()
            .copied()
            .filter(|&k| !is_protected(k))
            .collect();
        // Farthest from the primary playhead first
        idx.sort_by_key(|&k| {
            std::cmp::Reverse(if k < keep_from {
                keep_from - k
            } else {
                k.saturating_sub(keep_to)
            })
        });
        for k in idx {
            if self.mem <= MEM_CAP * 9 / 10 {
                break;
            }
            if let Some(b) = self.blocks.remove(&k) {
                self.mem -= b.len() as u64;
            }
        }
    }
}

pub struct Spool {
    pub gen: u64,
    pub url: String,
    ua: String,
    st: Mutex<St>,
    data: Notify,
    demand: Notify,
    close_notify: Notify,
    closed: AtomicBool,
    is_pre: AtomicBool,
    http: reqwest::Client,
}

impl Spool {
    fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}/spool/{}", PORT.load(SeqCst), self.gen)
    }

    fn close(&self) {
        if !self.closed.swap(true, SeqCst) {
            let was_connected = self.st.lock().unwrap().connected;
            if was_connected {
                LAST_CLOSE.store(now_ms(), SeqCst);
                crate::slot::mark_released();
            }
        }
        self.close_notify.notify_waiters();
        self.data.notify_waiters();
        self.demand.notify_waiters();
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

    /// Sleep for `ms`, aborting immediately if this spool closes or its slot lease is cancelled.
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

    /// The download loop: keeps one provider connection at the right place via `crate::slot`,
    /// reconnecting with backoff after errors, holding a 5 s reader-gap grace window, and closing
    /// immediately (<20 ms) when cancelled or detached.
    async fn run(self: Arc<Self>) {
        let mut body: Option<
            std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>,
        > = None;
        let mut lease: Option<SlotLease> = None;
        let mut backoff = 1000u64;
        let mut slot_errs = 0u32;
        let mut grace_since: Option<i64> = None;

        loop {
            if self.closed.load(SeqCst) {
                break;
            }
            if lease.as_ref().map_or(false, |l| l.is_cancelled()) {
                if body.take().is_some() {
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                }
                lease = None;
                self.close();
                break;
            }

            let (target, idle, at_end, no_readers, is_priming) = {
                let mut st = self.st.lock().unwrap();
                let now = now_ms();
                if st.readers.is_empty() {
                    if st.no_readers_since == 0 {
                        st.no_readers_since = now;
                    }
                } else {
                    st.no_readers_since = 0;
                }
                let idle_limit = if st.prime { 45_000 } else { 15_000 };
                let idle = st.no_readers_since > 0 && now - st.no_readers_since > idle_limit;
                let at_end = st.total.map_or(false, |tot| st.upstream_pos >= tot);
                (
                    st.target(),
                    idle,
                    at_end,
                    st.readers.is_empty(),
                    st.prime,
                )
            };

            let Some(t) = target.filter(|_| !idle) else {
                // Keep warm TCP socket alive across short reader gaps (e.g. ffprobe -> mpv HDR switch)
                // unless we hit EOF, timed out, are in pre-spool background mode, or a reader already filled cap().
                let can_grace = body.is_some()
                    && !idle
                    && !at_end
                    && no_readers
                    && !is_priming
                    && !self.is_pre.load(SeqCst);
                if can_grace {
                    let now = now_ms();
                    let started = *grace_since.get_or_insert(now);
                    let elapsed = now.saturating_sub(started);
                    if elapsed < READER_GAP_GRACE_MS {
                        let rem = (READER_GAP_GRACE_MS - elapsed) as u64;
                        let cancelled = tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_millis(rem.min(500))) => false,
                            _ = self.demand.notified() => false,
                            _ = self.wait_closed() => true,
                            _ = async {
                                if let Some(ref l) = lease { l.cancelled().await; }
                                else { std::future::pending::<()>().await; }
                            } => true,
                        };
                        if cancelled
                            || self.closed.load(SeqCst)
                            || lease.as_ref().map_or(false, |l| l.is_cancelled())
                        {
                            if body.take().is_some() {
                                crate::slot::mark_released();
                                LAST_CLOSE.store(now_ms(), SeqCst);
                                self.st.lock().unwrap().connected = false;
                            }
                            lease = None;
                            self.close();
                            break;
                        }
                        continue;
                    }
                }
                grace_since = None;
                if body.take().is_some() {
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                }
                if let Some(l) = lease.take() {
                    l.release();
                }
                // Nothing to do: free cache after 10 min unused
                let expired = {
                    let st = self.st.lock().unwrap();
                    st.readers.is_empty()
                        && st.no_readers_since > 0
                        && now_ms() - st.no_readers_since > 600_000
                };
                if expired {
                    self.close();
                    break;
                }
                tokio::select! {
                    _ = tokio::time::timeout(std::time::Duration::from_secs(1), self.demand.notified()) => {}
                    _ = self.wait_closed() => break,
                }
                continue;
            };
            grace_since = None;

            let at = self.st.lock().unwrap().upstream_pos;
            // Keep reading through a small gap (already-cached bytes) rather than reconnecting
            let reuse = body.is_some() && t >= at && t - at < (8 << 20);
            if !reuse {
                if body.take().is_some() {
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                }
                if let Some(l) = lease.take() {
                    l.release();
                }
                let start = t / BLOCK * BLOCK
                    + {
                        let st = self.st.lock().unwrap();
                        st.blocks.get(&(t / BLOCK)).map_or(0, |b| b.len() as u64)
                    };
                let new_lease = crate::slot::acquire(Owner::Spool, &self.url);
                let ready = tokio::select! {
                    ok = new_lease.wait_cooldown() => ok,
                    _ = self.wait_closed() => false,
                };
                if !ready || self.closed.load(SeqCst) {
                    new_lease.release();
                    if self.closed.load(SeqCst) || new_lease.is_cancelled() {
                        self.close();
                    }
                    break;
                }

                let open_res = tokio::select! {
                    r = self.open(start) => Some(r),
                    _ = new_lease.cancelled() => None,
                    _ = self.wait_closed() => None,
                };
                match open_res {
                    None => {
                        // Cancelled mid-connect: TCP future dropped immediately
                        crate::slot::mark_released();
                        LAST_CLOSE.store(now_ms(), SeqCst);
                        new_lease.release();
                        self.close();
                        break;
                    }
                    Some(Ok(s)) => {
                        body = Some(s);
                        lease = Some(new_lease);
                        backoff = 1000;
                        slot_errs = 0;
                        {
                            let mut st = self.st.lock().unwrap();
                            st.connected = true;
                            st.upstream_pos = start;
                            st.failing_since = None;
                        }
                        // Wake attach() immediately as soon as headers & total size are ready
                        self.data.notify_waiters();
                    }
                    Some(Err(OpenErr::Unsupported)) => {
                        crate::slot::mark_released();
                        new_lease.release();
                        {
                            let mut st = self.st.lock().unwrap();
                            st.connected = false;
                            st.reconnects += 1;
                            st.unsupported = true;
                        }
                        self.close();
                        break;
                    }
                    Some(Err(OpenErr::RateOrSlot(code))) => {
                        slot_errs += 1;
                        crate::slot::mark_released();
                        LAST_CLOSE.store(now_ms(), SeqCst);
                        new_lease.release();
                        let have_total = {
                            let mut st = self.st.lock().unwrap();
                            st.connected = false;
                            st.reconnects += 1;
                            if st.failing_since.is_none() {
                                st.failing_since = Some(now_ms());
                            }
                            st.total.is_some()
                        };
                        self.data.notify_waiters();
                        let limit = if code == 404 || code == 410 {
                            2
                        } else if have_total {
                            5
                        } else {
                            3
                        };
                        if slot_errs >= limit {
                            println!(
                                "[spool] giving up after {slot_errs} consecutive {code} rejections (total={have_total})"
                            );
                            self.close();
                            break;
                        }
                        let wait = if code == 404 || code == 410 {
                            1500
                        } else {
                            slot_backoff_ms(slot_errs)
                        };
                        if !self.sleep_cancellable(wait, None).await {
                            break;
                        }
                        continue;
                    }
                    Some(Err(OpenErr::Transient)) => {
                        slot_errs = 0;
                        crate::slot::mark_released();
                        new_lease.release();
                        {
                            let mut st = self.st.lock().unwrap();
                            st.connected = false;
                            st.reconnects += 1;
                            if st.failing_since.is_none() {
                                st.failing_since = Some(now_ms());
                            }
                        }
                        self.data.notify_waiters();
                        let wait = backoff;
                        backoff = (backoff * 2).min(8000);
                        if !self.sleep_cancellable(wait, None).await {
                            break;
                        }
                        continue;
                    }
                }
            }

            let next = tokio::select! {
                r = tokio::time::timeout(std::time::Duration::from_secs(12), body.as_mut().unwrap().next()) => Some(r),
                _ = async {
                    if let Some(ref l) = lease { l.cancelled().await; }
                    else { std::future::pending::<()>().await; }
                } => None,
                _ = self.wait_closed() => None,
            };
            match next {
                None => {
                    // Cancelled or closed mid-read: drop HTTP stream immediately (<20 ms)
                    body = None;
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                    if let Some(l) = lease.take() {
                        l.release();
                    }
                    self.close();
                    break;
                }
                Some(Ok(Some(Ok(chunk)))) => {
                    {
                        let mut st = self.st.lock().unwrap();
                        let p = st.upstream_pos;
                        st.store(p, &chunk);
                        st.upstream_pos += chunk.len() as u64;
                        if st.prime {
                            let goal = st.total.map_or(PRIME_BYTES, |tot| PRIME_BYTES.min(tot));
                            if st.frontier(0) >= goal {
                                st.prime = false;
                            }
                        }
                    }
                    self.data.notify_waiters();
                }
                Some(other) => {
                    // End of file, error, or silence for 12 s: drop the connection; loop reconnects where needed
                    let at_end = {
                        let st = self.st.lock().unwrap();
                        st.total.map_or(false, |t| st.upstream_pos >= t)
                    };
                    if !at_end {
                        println!(
                            "[spool] provider connection {}; reconnecting at {} MB",
                            match other {
                                Err(_) => "went silent".to_string(),
                                Ok(Some(Err(e))) => format!("failed ({e})"),
                                _ => "ended early".into(),
                            },
                            self.st.lock().unwrap().upstream_pos >> 20
                        );
                    }
                    body = None;
                    crate::slot::mark_released();
                    LAST_CLOSE.store(now_ms(), SeqCst);
                    self.st.lock().unwrap().connected = false;
                    if let Some(l) = lease.take() {
                        l.release();
                    }
                }
            }
        }

        if body.take().is_some() {
            crate::slot::mark_released();
            LAST_CLOSE.store(now_ms(), SeqCst);
            self.st.lock().unwrap().connected = false;
        }
        if let Some(l) = lease.take() {
            l.release();
        }
    }

    /// Open the provider at `start`, reusing the cached 302 CDN URL for fast intra-file Range seeks,
    /// validating `Content-Range` start byte, and only retrying portal URL on 401/403/404/410.
    async fn open(
        &self,
        start: u64,
    ) -> Result<
        std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<Bytes>> + Send>>,
        OpenErr,
    > {
        let cached = self.st.lock().unwrap().resolved_url.clone();
        let req_url = cached.as_deref().unwrap_or(&self.url);
        let mut r = match self
            .http
            .get(req_url)
            .header(header::USER_AGENT, &self.ua)
            .header(header::RANGE, format!("bytes={start}-"))
            .timeout(std::time::Duration::from_secs(3600 * 6))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => {
                if cached.is_some() {
                    self.st.lock().unwrap().resolved_url = None;
                }
                return Err(OpenErr::Transient);
            }
        };

        // Only clear a cached 302 CDN URL and retry via the portal URL on 401/403/404/410.
        // Never retry immediately on 460/429 (avoids double-hitting a busy 1/1 provider slot).
        if should_retry_portal_url(r.status().as_u16()) && cached.is_some() {
            self.st.lock().unwrap().resolved_url = None;
            r = self
                .http
                .get(&self.url)
                .header(header::USER_AGENT, &self.ua)
                .header(header::RANGE, format!("bytes={start}-"))
                .timeout(std::time::Duration::from_secs(3600 * 6))
                .send()
                .await
                .map_err(|_| OpenErr::Transient)?;
        }

        let status = r.status();
        if status == reqwest::StatusCode::OK {
            if start > 0 {
                return Err(OpenErr::Unsupported);
            }
            let accepts_bytes = r
                .headers()
                .get(header::ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .map_or(false, |v| v.trim().eq_ignore_ascii_case("bytes"));
            if !accepts_bytes {
                println!("[spool] provider ignores ranges: playing directly");
                return Err(OpenErr::Unsupported);
            }
        }
        if !status.is_success() {
            println!("[spool] provider said {status} at {} MB", start >> 20);
            let code = status.as_u16();
            return Err(if is_slot_or_auth_status(code) {
                OpenErr::RateOrSlot(code)
            } else {
                OpenErr::Transient
            });
        }

        // Validate Content-Range start byte matches requested `start` before storing any blocks
        let cr_parsed = r
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            match cr_parsed {
                Some((cr_start, _, _)) if cr_start != start => {
                    println!(
                        "[spool] Content-Range start {cr_start} != requested {start}; refusing corrupt range"
                    );
                    return Err(OpenErr::Unsupported);
                }
                None if start > 0 => {
                    println!("[spool] missing/invalid Content-Range on 206 at {start}; refusing");
                    return Err(OpenErr::Unsupported);
                }
                _ => {}
            }
        } else if let Some((cr_start, _, _)) = cr_parsed {
            if cr_start != start {
                println!(
                    "[spool] unexpected Content-Range start {cr_start} != requested {start}"
                );
                return Err(OpenErr::Unsupported);
            }
        }

        let final_url = r.url().as_str().to_string();
        let total = cr_parsed
            .and_then(|(_, _, t)| t)
            .or_else(|| r.content_length().map(|l| l + start));
        {
            let mut st = self.st.lock().unwrap();
            if let Some(t) = total {
                st.total = Some(t);
            }
            if !final_url.is_empty() && final_url != self.url {
                st.resolved_url = Some(final_url);
            }
        }
        Ok(Box::pin(r.bytes_stream()))
    }
}

fn build_http_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .pool_max_idle_per_host(0)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()
}

fn find_active_by_url(url: &str) -> Option<Arc<Spool>> {
    if let Some(s) = CUR
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.url == url && !s.closed.load(SeqCst))
        .cloned()
    {
        return Some(s);
    }
    PRE.lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.url == url && !s.closed.load(SeqCst))
        .cloned()
}

fn find_active_by_gen(gen: u64) -> Option<Arc<Spool>> {
    if let Some(s) = CUR
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.gen == gen && !s.closed.load(SeqCst))
        .cloned()
    {
        return Some(s);
    }
    PRE.lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.gen == gen && !s.closed.load(SeqCst))
        .cloned()
}

async fn wait_for_total(sp: &Arc<Spool>, timeout_secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let notified = sp.data.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if sp.closed.load(SeqCst) {
            return false;
        }
        if sp.st.lock().unwrap().total.is_some() {
            return true;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return false;
        }
    }
}

/// Use the spool for this provider URL: returns the local URL ffmpeg/mpv should open, or None to
/// play directly (provider without range support, or it didn't answer). Reuses the current spool
/// (or promotes a pre-spooled next episode!) when it's the same title.
pub async fn attach(url: &str, ua: &str) -> Option<String> {
    crate::livebuf::detach();

    // Fast path: already active in CUR (drop MutexGuard before awaiting!)
    let cur_existing = CUR
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.url == url && !s.closed.load(SeqCst))
        .cloned();
    if let Some(s) = cur_existing {
        if wait_for_total(&s, 20).await {
            return Some(s.local_url());
        }
        return None;
    }

    // Fast path: already pre-spooled in PRE -> promote atomically to CUR (<1 ms binge start!)
    let promoted = {
        let mut pre_g = PRE.lock().unwrap();
        if pre_g
            .as_ref()
            .map_or(false, |s| s.url == url && !s.closed.load(SeqCst))
        {
            pre_g.take()
        } else {
            None
        }
    };
    if let Some(sp) = promoted {
        sp.is_pre.store(false, SeqCst);
        let old_cur = CUR.lock().unwrap().replace(sp.clone());
        if let Some(old) = old_cur {
            old.close();
        }
        sp.demand.notify_one();
        if wait_for_total(&sp, 20).await {
            return Some(sp.local_url());
        }
        sp.close();
        return None;
    }

    let http = build_http_client()?;
    let sp = Arc::new(Spool {
        gen: GEN.fetch_add(1, SeqCst) + 1,
        url: url.to_string(),
        ua: ua.to_string(),
        st: Mutex::new(St {
            prime: true,
            ..Default::default()
        }),
        data: Notify::new(),
        demand: Notify::new(),
        close_notify: Notify::new(),
        closed: AtomicBool::new(false),
        is_pre: AtomicBool::new(false),
        http,
    });

    // Atomic replacement under CUR lock prevents TOCTOU leak of superseded spools
    let (reuse, old_cur) = {
        let mut cur_g = CUR.lock().unwrap();
        if let Some(existing) = cur_g
            .as_ref()
            .filter(|s| s.url == url && !s.closed.load(SeqCst))
            .cloned()
        {
            (Some(existing), None)
        } else {
            (None, cur_g.replace(sp.clone()))
        }
    };
    if let Some(existing) = reuse {
        if wait_for_total(&existing, 20).await {
            return Some(existing.local_url());
        }
        return None;
    }
    let old_pre = PRE.lock().unwrap().take();
    if let Some(old) = old_cur {
        old.close();
    }
    if let Some(old) = old_pre {
        old.close();
    }

    tokio::spawn(sp.clone().run());
    if wait_for_total(&sp, 20).await {
        return Some(sp.local_url());
    }
    sp.close();
    None
}

/// Pre-spool the next episode's first 24 MB ONLY if the current spool has already reached EOF
/// and released the upstream connection (0 risk of stealing the active episode's connection).
pub async fn pre_spool_next(url: &str, ua: &str) -> bool {
    if url.is_empty() || crate::livebuf::is_active() || crate::slot::is_busy() {
        return false;
    }
    // Verify CUR (if any) is EOF and disconnected
    {
        let cur_g = CUR.lock().unwrap();
        if let Some(cur) = cur_g.as_ref().filter(|s| !s.closed.load(SeqCst)) {
            if cur.url == url {
                return cur.st.lock().unwrap().total.is_some();
            }
            let st = cur.st.lock().unwrap();
            if st.connected || !st.is_eof() {
                return false;
            }
        }
    }
    // Already pre-spooled for this URL? (drop MutexGuard before awaiting!)
    let pre_existing = PRE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.url == url && !s.closed.load(SeqCst))
        .cloned();
    if let Some(existing) = pre_existing {
        return wait_for_total(&existing, 15).await;
    }

    let Some(http) = build_http_client() else {
        return false;
    };
    let sp = Arc::new(Spool {
        gen: GEN.fetch_add(1, SeqCst) + 1,
        url: url.to_string(),
        ua: ua.to_string(),
        st: Mutex::new(St {
            prime: true,
            ..Default::default()
        }),
        data: Notify::new(),
        demand: Notify::new(),
        close_notify: Notify::new(),
        closed: AtomicBool::new(false),
        is_pre: AtomicBool::new(true),
        http,
    });
    let old_pre = {
        let mut pre_g = PRE.lock().unwrap();
        if pre_g
            .as_ref()
            .map_or(false, |s| s.url == url && !s.closed.load(SeqCst))
        {
            return true;
        }
        pre_g.replace(sp.clone())
    };
    if let Some(old) = old_pre {
        old.close();
    }
    println!("[spool] pre-spooling next episode (first 24 MB)");
    tokio::spawn(sp.clone().run());
    if wait_for_total(&sp, 15).await {
        true
    } else {
        sp.close();
        false
    }
}

/// Local URL if this provider URL is being spooled (or pre-spooled) right now.
pub fn local_for(url: &str) -> Option<String> {
    find_active_by_url(url).map(|s| s.local_url())
}

pub fn attached(url: &str) -> bool {
    local_for(url).is_some()
}

/// True if either the active spool or a pre-spool currently holds an open upstream TCP socket.
pub fn is_connected() -> bool {
    let cur = CUR.lock().unwrap().clone();
    if cur
        .as_ref()
        .map_or(false, |s| !s.closed.load(SeqCst) && s.st.lock().unwrap().connected)
    {
        return true;
    }
    let pre = PRE.lock().unwrap().clone();
    pre.as_ref()
        .map_or(false, |s| !s.closed.load(SeqCst) && s.st.lock().unwrap().connected)
}

/// True when `url` is spooled and every byte from the active playhead through EOF is in memory.
pub fn is_eof(url: &str) -> bool {
    let Some(s) = find_active_by_url(url) else {
        return false;
    };
    let eof = s.st.lock().unwrap().is_eof();
    eof
}

/// Give the provider connection back (player closed, live TV, explicit stop).
pub fn detach() {
    let old_cur = CUR.lock().unwrap().take();
    let old_pre = PRE.lock().unwrap().take();
    if let Some(s) = old_cur {
        s.close();
    }
    if let Some(s) = old_pre {
        s.close();
    }
}

/// Detach ONLY the spool matching `url`, leaving any pre-spooled next episode intact.
pub fn detach_if_url(url: &str) {
    let old_cur = {
        let mut g = CUR.lock().unwrap();
        if g.as_ref().map_or(false, |s| s.url == url) {
            g.take()
        } else {
            None
        }
    };
    let old_pre = {
        let mut g = PRE.lock().unwrap();
        if g.as_ref().map_or(false, |s| s.url == url) {
            g.take()
        } else {
            None
        }
    };
    if let Some(s) = old_cur {
        s.close();
    }
    if let Some(s) = old_pre {
        s.close();
    }
}

pub fn set_duration(url: &str, secs: f64) {
    if let Some(s) = find_active_by_url(url) {
        let mut st = s.st.lock().unwrap();
        if let Some(t) = st.total {
            if secs > 60.0 {
                st.bitrate = t as f64 / secs;
            }
        }
    }
}

/// For the playback info overlay: seconds and MB buffered ahead of playback, connection state.
pub fn stats() -> serde_json::Value {
    let Some(s) = CUR.lock().unwrap().clone().filter(|s| !s.closed.load(SeqCst)) else {
        return serde_json::json!(null);
    };
    let (ahead, bitrate, connected, reconnects, mem, eof) = {
        let st = s.st.lock().unwrap();
        let p = st.primary().unwrap_or(0);
        (
            st.frontier(p).saturating_sub(p),
            st.bitrate,
            st.connected,
            st.reconnects,
            st.mem,
            st.is_eof(),
        )
    };
    let pre = PRE.lock().unwrap().clone();
    let pre_ready = pre
        .as_ref()
        .map_or(false, |ps| !ps.closed.load(SeqCst) && ps.st.lock().unwrap().total.is_some());
    serde_json::json!({
        "aheadMB": (ahead >> 20),
        "aheadSecs": if bitrate > 0.0 { (ahead as f64 / bitrate).round() } else { -1.0 },
        "connected": connected,
        "reconnects": reconnects,
        "memMB": mem >> 20,
        "eof": eof,
        "preSpooled": pre_ready,
    })
}

/// GET /spool/{gen}: the cached file with Range support (127.0.0.1 only, like the rest of the API).
pub async fn serve(Path(gen): Path<u64>, headers: HeaderMap) -> Response {
    let Some(sp) = find_active_by_gen(gen) else {
        return (StatusCode::NOT_FOUND, "gone").into_response();
    };
    let Some(total) = sp.st.lock().unwrap().total else {
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
    };
    let range_hdr = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let has_range = range_hdr.is_some();
    let (start, end) = match range_hdr {
        Some(raw) => match parse_http_range(raw, total) {
            Ok(bounds) => bounds,
            Err(()) => {
                return (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{total}"))],
                )
                    .into_response();
            }
        },
        None => {
            if total == 0 {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, 0)
                    .body(Body::empty())
                    .unwrap();
            }
            (0, total - 1)
        }
    };
    let id = {
        let mut st = sp.st.lock().unwrap();
        st.next_reader += 1;
        let id = st.next_reader;
        st.readers.insert(
            id,
            Reader {
                pos: start,
                served: 0,
                waiting: None,
            },
        );
        id
    };
    sp.demand.notify_one();
    struct Guard(Arc<Spool>, u64);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.st.lock().unwrap().readers.remove(&self.1);
            self.0.demand.notify_one();
        }
    }
    let g = Guard(sp.clone(), id);
    let stream = futures_util::stream::unfold((g, start), move |(g, pos)| async move {
        if pos > end {
            return None;
        }
        let sp = g.0.clone();
        loop {
            if sp.closed.load(SeqCst) {
                return Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "spool closed",
                    )),
                    (g, end + 1),
                ));
            }
            let notified = sp.data.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut st = sp.st.lock().unwrap();
                if let Some((b, off)) = st.have(pos) {
                    let n = (b.len() - off)
                        .min((end - pos + 1) as usize)
                        .min(512 * 1024);
                    let chunk = Bytes::copy_from_slice(&b[off..off + n]);
                    if let Some(r) = st.readers.get_mut(&g.1) {
                        r.pos = pos + n as u64;
                        r.served += n as u64;
                        r.waiting = None;
                    }
                    drop(st);
                    sp.demand.notify_one();
                    return Some((Ok(chunk), (g, pos + n as u64)));
                }
                if let Some(r) = st.readers.get_mut(&g.1) {
                    r.pos = pos;
                    r.waiting = Some(pos);
                }
                // The provider has been unreachable for a long time: give up so the player can report it
                if st.failing_since.map_or(false, |t| now_ms() - t > 45_000) {
                    drop(st);
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "provider unreachable",
                        )),
                        (g, end + 1),
                    ));
                }
            }
            sp.demand.notify_one();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
        }
    });
    let mut rb = Response::builder()
        .status(if has_range {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, end - start + 1);
    if has_range {
        rb = rb.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"));
    }
    rb.body(Body::from_stream(stream)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_and_auth_status_classification_and_backoff() {
        for code in [401, 403, 404, 410, 429, 460] {
            assert!(
                is_slot_or_auth_status(code),
                "expected {code} to be slot/auth"
            );
        }
        for code in [200, 206, 500, 502, 503, 504] {
            assert!(
                !is_slot_or_auth_status(code),
                "expected {code} not to be slot/auth"
            );
        }
        for code in [401, 403, 404, 410] {
            assert!(should_retry_portal_url(code));
        }
        for code in [429, 460, 500, 503] {
            assert!(!should_retry_portal_url(code));
        }
        assert_eq!(slot_backoff_ms(1), 3000);
        assert_eq!(slot_backoff_ms(2), 6000);
        assert_eq!(slot_backoff_ms(3), 10_000);
        assert_eq!(slot_backoff_ms(9), 10_000);
        assert_eq!(REOPEN_WAIT_MS, 1500);
    }

    #[test]
    fn http_range_and_content_range_parsing() {
        // Standard ranges
        assert_eq!(parse_http_range("bytes=0-99", 1000), Ok((0, 99)));
        assert_eq!(parse_http_range("bytes=100-", 1000), Ok((100, 999)));
        assert_eq!(parse_http_range("bytes=500-5000", 1000), Ok((500, 999)));
        // Suffix ranges
        assert_eq!(parse_http_range("bytes=-500", 1000), Ok((500, 999)));
        assert_eq!(parse_http_range("bytes=-5000", 1000), Ok((0, 999)));
        assert!(parse_http_range("bytes=-0", 1000).is_err());
        // Invalid / underflow ranges
        assert!(parse_http_range("bytes=500-400", 1000).is_err());
        assert!(parse_http_range("bytes=1000-", 1000).is_err());
        assert!(parse_http_range("bytes=0-0", 0).is_err());

        // Content-Range parsing
        assert_eq!(
            parse_content_range("bytes 1048576-2097151/10000000"),
            Some((1_048_576, 2_097_151, Some(10_000_000)))
        );
        assert_eq!(parse_content_range("bytes 0-99/*"), Some((0, 99, None)));
        assert_eq!(parse_content_range("bytes 200-100/500"), None);
    }

    #[test]
    fn evict_protects_secondary_eof_reader_blocks() {
        let mut st = St::default();
        st.total = Some(2000 * BLOCK);
        // Primary reader near start (served 100 MB)
        st.readers.insert(
            1,
            Reader {
                pos: 10 * BLOCK,
                served: 100 * BLOCK,
                waiting: None,
            },
        );
        // Secondary reader reading MKV Cues near EOF
        st.readers.insert(
            2,
            Reader {
                pos: 1998 * BLOCK,
                served: 1024,
                waiting: Some(1999 * BLOCK),
            },
        );
        // Populate 910 MB across distant middle blocks + EOF blocks
        for k in 300..1205u64 {
            st.blocks.insert(k, vec![0u8; BLOCK as usize]);
            st.mem += BLOCK;
        }
        for k in 1995..=2000u64 {
            st.blocks.insert(k, vec![0u8; BLOCK as usize]);
            st.mem += BLOCK;
        }
        assert!(st.mem > MEM_CAP);
        st.evict();
        assert!(st.mem <= MEM_CAP * 9 / 10);
        // Every block within ±4 of reader 2's cursor/wait MUST survive eviction
        for k in 1995..=2000u64 {
            assert!(st.blocks.contains_key(&k), "EOF reader block {k} was evicted");
        }
    }
}
