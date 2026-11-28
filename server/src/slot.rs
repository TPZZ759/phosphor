// Unified single-connection upstream slot arbiter.
//
// The IPTV account permits ONE active upstream connection at a time. Every upstream consumer
// (VOD read-ahead spool, live TV ring buffer, direct ffmpeg, direct mpv, direct ffprobe, and
// background jobs) coordinates through this arbiter so:
//   - switching owners immediately signals cancellation to the previous upstream task via Notify
//     (<20 ms TCP drop instead of waiting for a 12 s read timeout);
//   - a single global release timestamp (`last_released_ms`) eliminates stacked double/triple
//     cooldown sleeps across modules;
//   - cooldown math is exact: 1500 ms cross-URL, 250 ms same-URL, 0 ms if already released
//     more than 1500 ms ago (or on cold start).
use crate::util::now_ms;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Notify;

pub const CROSS_URL_COOLDOWN_MS: i64 = 1500;
pub const SAME_URL_COOLDOWN_MS: i64 = 750;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    None,
    Spool,
    LiveBuf,
    Ffmpeg,
    Mpv,
    Probe,
    Background,
}

impl Owner {
    pub fn as_str(self) -> &'static str {
        match self {
            Owner::None => "none",
            Owner::Spool => "spool",
            Owner::LiveBuf => "livebuf",
            Owner::Ffmpeg => "ffmpeg",
            Owner::Mpv => "mpv",
            Owner::Probe => "probe",
            Owner::Background => "background",
        }
    }
}

struct ActiveState {
    gen: u64,
    owner: Owner,
    url: Option<String>,
    last_url: Option<String>,
    cancel: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
}

struct Inner {
    gen: AtomicU64,
    last_released_ms: AtomicI64,
    state: Mutex<ActiveState>,
}

impl Inner {
    fn new() -> Self {
        Inner {
            gen: AtomicU64::new(0),
            last_released_ms: AtomicI64::new(0),
            state: Mutex::new(ActiveState {
                gen: 0,
                owner: Owner::None,
                url: None,
                last_url: None,
                cancel: Arc::new(Notify::new()),
                cancelled: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    fn bump_released(&self, now: i64) {
        let mut cur = self.last_released_ms.load(SeqCst);
        while now > cur {
            match self
                .last_released_ms
                .compare_exchange_weak(cur, now, SeqCst, SeqCst)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }
}

/// Pure single-cooldown computation shared by `acquire_at` and `cooldown_remaining_ms`.
fn compute_cooldown_ms(
    now: i64,
    last_released_ms: i64,
    had_active_owner: bool,
    prev_url: Option<&str>,
    new_url: &str,
) -> u64 {
    let same_url = !new_url.is_empty() && prev_url == Some(new_url);
    let required = if same_url {
        SAME_URL_COOLDOWN_MS
    } else {
        CROSS_URL_COOLDOWN_MS
    };
    if had_active_owner {
        return required as u64;
    }
    if last_released_ms <= 0 {
        return 0;
    }
    let elapsed = now.saturating_sub(last_released_ms);
    if elapsed >= CROSS_URL_COOLDOWN_MS || elapsed >= required {
        0
    } else {
        (required - elapsed).max(0) as u64
    }
}

pub struct SlotArbiter {
    inner: Arc<Inner>,
}

impl Default for SlotArbiter {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotArbiter {
    pub fn new() -> Self {
        SlotArbiter {
            inner: Arc::new(Inner::new()),
        }
    }

    /// Acquire exclusive ownership of the upstream connection slot.
    /// Immediately cancels any previous owner and returns a `SlotLease` with the exact
    /// single-cooldown wait (1500 ms cross-URL, 250 ms same-URL, 0 ms if already idle >1500 ms).
    pub fn acquire(&self, owner: Owner, url: &str) -> SlotLease {
        self.acquire_at(owner, url, now_ms())
    }

    /// Deterministic timestamp variant of `acquire` (primarily for unit tests).
    pub fn acquire_at(&self, owner: Owner, url: &str, now: i64) -> SlotLease {
        let gen = self.inner.gen.fetch_add(1, SeqCst) + 1;
        let cancel = Arc::new(Notify::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (prev_cancel, prev_cancelled, wait_ms) = {
            let mut st = self.inner.state.lock().unwrap();
            let had_active = st.owner != Owner::None;
            let prev_url = st.url.as_deref().or(st.last_url.as_deref());
            let last_rel = self.inner.last_released_ms.load(SeqCst);
            let wait = compute_cooldown_ms(now, last_rel, had_active, prev_url, url);
            let prev_c = if had_active {
                Some((st.cancel.clone(), st.cancelled.clone()))
            } else {
                None
            };
            if had_active {
                self.inner.bump_released(now);
            }
            st.gen = gen;
            st.owner = owner;
            st.url = if owner == Owner::None || url.is_empty() {
                None
            } else {
                Some(url.to_string())
            };
            if !url.is_empty() {
                st.last_url = Some(url.to_string());
            }
            st.cancel = cancel.clone();
            st.cancelled = cancelled.clone();
            (
                prev_c.as_ref().map(|x| x.0.clone()),
                prev_c.as_ref().map(|x| x.1.clone()),
                wait,
            )
        };
        if let (Some(n), Some(flag)) = (prev_cancel, prev_cancelled) {
            flag.store(true, SeqCst);
            n.notify_waiters();
        }
        SlotLease {
            inner: self.inner.clone(),
            gen,
            owner,
            url: url.to_string(),
            cancel,
            cancelled,
            released: AtomicBool::new(false),
            wait_ms,
        }
    }

    /// Cancel the current owner (if any) without installing a new owner.
    pub fn cancel_current(&self) {
        self.cancel_current_at(now_ms());
    }

    pub fn cancel_current_at(&self, now: i64) {
        let prev = {
            let mut st = self.inner.state.lock().unwrap();
            if st.owner == Owner::None {
                None
            } else {
                self.inner.bump_released(now);
                let out = (st.cancel.clone(), st.cancelled.clone());
                st.owner = Owner::None;
                st.url = None;
                st.cancel = Arc::new(Notify::new());
                st.cancelled = Arc::new(AtomicBool::new(false));
                Some(out)
            }
        };
        if let Some((n, flag)) = prev {
            flag.store(true, SeqCst);
            n.notify_waiters();
        }
    }

    /// Record that an upstream TCP connection has actually closed right now.
    pub fn mark_released(&self) {
        self.inner.bump_released(now_ms());
    }

    pub fn mark_released_at(&self, now: i64) {
        self.inner.bump_released(now);
    }

    pub fn last_released_ms(&self) -> i64 {
        self.inner.last_released_ms.load(SeqCst)
    }

    pub fn is_busy(&self) -> bool {
        self.inner.state.lock().unwrap().owner != Owner::None
    }

    pub fn owner(&self) -> Owner {
        self.inner.state.lock().unwrap().owner
    }

    pub fn current_url(&self) -> Option<String> {
        self.inner.state.lock().unwrap().url.clone()
    }

    /// Clone the current owner's cancellation `Notify` for use in `tokio::select!`.
    pub fn cancel_notify(&self) -> Arc<Notify> {
        self.inner.state.lock().unwrap().cancel.clone()
    }

    /// Compute how many milliseconds a caller for `url` would need to wait right now.
    pub fn cooldown_remaining_ms(&self, url: Option<&str>) -> u64 {
        self.cooldown_remaining_ms_at(url, now_ms())
    }

    pub fn cooldown_remaining_ms_at(&self, url: Option<&str>, now: i64) -> u64 {
        let st = self.inner.state.lock().unwrap();
        let had_active = st.owner != Owner::None;
        let prev_url = st.url.as_deref().or(st.last_url.as_deref());
        let last_rel = self.inner.last_released_ms.load(SeqCst);
        compute_cooldown_ms(now, last_rel, had_active, prev_url, url.unwrap_or(""))
    }

    pub fn status(&self) -> Value {
        let st = self.inner.state.lock().unwrap();
        let last_rel = self.inner.last_released_ms.load(SeqCst);
        let since_ms = if last_rel > 0 {
            Some(now_ms().saturating_sub(last_rel))
        } else {
            None
        };
        json!({
            "busy": st.owner != Owner::None,
            "owner": st.owner.as_str(),
            "gen": st.gen,
            "lastReleasedMs": last_rel,
            "idleMs": since_ms,
        })
    }
}

pub struct SlotLease {
    inner: Arc<Inner>,
    gen: u64,
    owner: Owner,
    url: String,
    cancel: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
    released: AtomicBool,
    wait_ms: u64,
}

impl SlotLease {
    pub fn gen(&self) -> u64 {
        self.gen
    }

    pub fn owner(&self) -> Owner {
        self.owner
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn wait_ms(&self) -> u64 {
        self.wait_ms
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(SeqCst)
    }

    pub fn cancel_notify(&self) -> Arc<Notify> {
        self.cancel.clone()
    }

    /// Completes immediately if this lease has already been cancelled, or as soon as another
    /// owner supersedes it. Race-free against concurrent cancellation.
    pub async fn cancelled(&self) {
        loop {
            if self.cancelled.load(SeqCst) {
                return;
            }
            let n = self.cancel.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.cancelled.load(SeqCst) {
                return;
            }
            n.await;
        }
    }

    /// Sleep for the lease's computed single cooldown, aborting immediately if cancelled.
    /// Returns `true` if the lease is still active and ready to open upstream, `false` if cancelled.
    pub async fn wait_cooldown(&self) -> bool {
        if self.is_cancelled() {
            return false;
        }
        if self.wait_ms == 0 {
            return !self.is_cancelled();
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(self.wait_ms)) => !self.is_cancelled(),
            _ = self.cancelled() => false,
        }
    }

    /// Stamp a real TCP socket release while keeping or winding down this lease.
    pub fn mark_released(&self) {
        self.inner.bump_released(now_ms());
    }

    /// Release ownership of the slot if this lease is still the active generation.
    pub fn release(&self) {
        self.release_at(now_ms());
    }

    pub fn release_at(&self, now: i64) {
        if self.released.swap(true, SeqCst) {
            return;
        }
        let mut st = self.inner.state.lock().unwrap();
        if st.gen == self.gen && st.owner != Owner::None {
            st.owner = Owner::None;
            st.url = None;
            st.cancel = Arc::new(Notify::new());
            st.cancelled = Arc::new(AtomicBool::new(false));
            self.inner.bump_released(now);
        }
    }
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        self.release();
    }
}

static GLOBAL: LazyLock<SlotArbiter> = LazyLock::new(SlotArbiter::new);

pub fn global() -> &'static SlotArbiter {
    &GLOBAL
}

pub fn acquire(owner: Owner, url: &str) -> SlotLease {
    GLOBAL.acquire(owner, url)
}

pub fn cancel_current() {
    GLOBAL.cancel_current();
}

pub fn mark_released() {
    GLOBAL.mark_released();
}

pub fn last_released_ms() -> i64 {
    GLOBAL.last_released_ms()
}

pub fn is_busy() -> bool {
    GLOBAL.is_busy()
}

pub fn owner() -> Owner {
    GLOBAL.owner()
}

pub fn current_url() -> Option<String> {
    GLOBAL.current_url()
}

pub fn cancel_notify() -> Arc<Notify> {
    GLOBAL.cancel_notify()
}

pub fn cooldown_remaining_ms(url: Option<&str>) -> u64 {
    GLOBAL.cooldown_remaining_ms(url)
}

pub fn status() -> Value {
    GLOBAL.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_cooldown_math_cold_same_cross_and_idle() {
        let a = SlotArbiter::new();
        assert!(!a.is_busy());
        assert_eq!(a.owner(), Owner::None);
        assert_eq!(a.last_released_ms(), 0);

        // Cold start: 0 ms cooldown
        let l1 = a.acquire_at(Owner::Spool, "http://p/movie/1.mkv", 10_000);
        assert_eq!(l1.wait_ms(), 0);
        assert!(a.is_busy());
        assert_eq!(a.owner(), Owner::Spool);
        assert_eq!(a.current_url().as_deref(), Some("http://p/movie/1.mkv"));
        assert!(!l1.is_cancelled());

        // Same-URL re-acquire while active: cancels l1 immediately, 750 ms cooldown (prevents 509 storms)
        let l2 = a.acquire_at(Owner::Spool, "http://p/movie/1.mkv", 10_500);
        assert!(l1.is_cancelled());
        assert_eq!(l2.wait_ms(), 750);
        assert_eq!(a.last_released_ms(), 10_500);

        // Dropping superseded l1 must NOT clear l2's active ownership
        drop(l1);
        assert!(a.is_busy());
        assert_eq!(a.owner(), Owner::Spool);

        // Cross-URL acquire while active: cancels l2, 1500 ms cooldown
        let l3 = a.acquire_at(Owner::LiveBuf, "http://p/live/99.ts", 11_000);
        assert!(l2.is_cancelled());
        assert_eq!(l3.wait_ms(), 1500);
        assert_eq!(a.owner(), Owner::LiveBuf);
        assert_eq!(a.current_url().as_deref(), Some("http://p/live/99.ts"));

        // Release l3 cleanly at t=12_000
        l3.release_at(12_000);
        assert!(!a.is_busy());
        assert_eq!(a.owner(), Owner::None);
        assert_eq!(a.last_released_ms(), 12_000);

        // Same-URL after 100 ms idle -> 650 ms remaining
        assert_eq!(
            a.cooldown_remaining_ms_at(Some("http://p/live/99.ts"), 12_100),
            650
        );
        // Same-URL after 750 ms idle -> 0 ms remaining
        assert_eq!(
            a.cooldown_remaining_ms_at(Some("http://p/live/99.ts"), 12_750),
            0
        );
        // Cross-URL after 500 ms idle -> 1000 ms remaining
        assert_eq!(
            a.cooldown_remaining_ms_at(Some("http://p/movie/2.mkv"), 12_500),
            1000
        );
        // Cross-URL after >1500 ms idle -> 0 ms remaining
        let l4 = a.acquire_at(Owner::Probe, "http://p/movie/2.mkv", 13_501);
        assert_eq!(l4.wait_ms(), 0);
        assert_eq!(a.owner(), Owner::Probe);
        drop(l4);
        assert!(!a.is_busy());
    }

    #[tokio::test]
    async fn cancellation_signaling_wakes_select_immediately() {
        let a = Arc::new(SlotArbiter::new());
        let l1 = a.acquire(Owner::Spool, "http://p/movie/1.mkv");
        let done = Arc::new(AtomicBool::new(false));
        let done2 = done.clone();
        let h = tokio::spawn(async move {
            tokio::select! {
                _ = l1.cancelled() => {
                    done2.store(true, SeqCst);
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
            }
        });
        tokio::task::yield_now().await;
        let _l2 = a.acquire(Owner::LiveBuf, "http://p/live/10.ts");
        tokio::time::timeout(std::time::Duration::from_millis(250), h)
            .await
            .expect("cancelled task did not wake in time")
            .unwrap();
        assert!(done.load(SeqCst));
    }

    #[tokio::test]
    async fn cancel_current_aborts_wait_cooldown() {
        let a = Arc::new(SlotArbiter::new());
        let _l1 = a.acquire(Owner::Spool, "http://p/movie/1.mkv");
        let l2 = a.acquire(Owner::LiveBuf, "http://p/live/2.ts");
        assert_eq!(l2.wait_ms(), 1500);
        let a2 = a.clone();
        let h = tokio::spawn(async move { l2.wait_cooldown().await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        a2.cancel_current();
        let completed = tokio::time::timeout(std::time::Duration::from_millis(250), h)
            .await
            .expect("wait_cooldown did not abort on cancel")
            .unwrap();
        assert!(!completed);
        assert!(!a.is_busy());
    }
}
