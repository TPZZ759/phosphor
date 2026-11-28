// Playback: ffmpeg remuxes any upstream stream into fragmented MP4 for the browser and, in the same
// pass, writes text subtitles to WebVTT into per-session directories (`subs/s_{seq}`).
// The account allows ONE connection: one upstream read at a time (coordinated via `crate::slot`).
use crate::slot::{Owner, SlotLease};
use crate::util::now_ms;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::{Child, Command};

#[derive(Default)]
struct ProbeStore {
    by_key: HashMap<String, Value>,
    /// O(1) secondary index: `(normalized_kind, numeric_id) -> probe_key`
    by_id: HashMap<(String, i64), String>,
}

impl ProbeStore {
    fn from_map(by_key: HashMap<String, Value>) -> Self {
        let mut store = ProbeStore {
            by_key: HashMap::with_capacity(by_key.len()),
            by_id: HashMap::with_capacity(by_key.len()),
        };
        for (k, v) in by_key {
            store.insert(k, v);
        }
        store
    }

    fn insert(&mut self, k: String, v: Value) {
        if let Some((kind, id)) = parse_probe_key(&k) {
            let new_v2 = v["v"].as_i64() == Some(2);
            let replace = match self.by_id.get(&(kind.clone(), id)) {
                None => true,
                Some(old_k) => {
                    new_v2
                        || self
                            .by_key
                            .get(old_k)
                            .and_then(|ov| ov["v"].as_i64())
                            != Some(2)
                }
            };
            if replace {
                self.by_id.insert((kind, id), k.clone());
            }
        }
        self.by_key.insert(k, v);
    }

    fn remove(&mut self, k: &str) {
        self.by_key.remove(k);
        if let Some((kind, id)) = parse_probe_key(k) {
            if self.by_id.get(&(kind.clone(), id)).map(|s| s.as_str()) == Some(k) {
                self.by_id.remove(&(kind, id));
            }
        }
    }

    fn get_by_kind_id(&self, kind: &str, id: i64) -> Option<&Value> {
        let nk = norm_kind(kind);
        let k = self.by_id.get(&(nk.to_string(), id))?;
        self.by_key.get(k)
    }

    fn find_by_id_any<T, F: Fn(&Value) -> Option<T>>(&self, id: i64, f: F) -> Option<T> {
        for kind in ["movie", "series", "live", ""] {
            if let Some(k) = self.by_id.get(&(kind.to_string(), id)) {
                if let Some(v) = self.by_key.get(k) {
                    if let Some(res) = f(v) {
                        return Some(res);
                    }
                }
            }
        }
        None
    }
}

pub struct Streamer {
    active: Mutex<Option<(u64, String, Child, Option<SlotLease>)>>,
    seq: AtomicU64,
    cur_seq: AtomicU64,
    last_stop: AtomicI64,
    sub_dir: PathBuf,
    probe_file: PathBuf,
    probe_save_lock: Arc<Mutex<()>>,
    probes: Mutex<ProbeStore>,
    /// `(seq, video marker expected, audio marker expected, video transcoded, requested start)`
    markers: Mutex<(u64, bool, bool, bool, f64)>,
    /// called with the session's audio tap when it ends (intro detection)
    on_stop: Mutex<Option<(u64, Box<dyn FnOnce(PathBuf) + Send>)>>,
    bytes: AtomicU64,
    /// recently cut sessions (stream key, time): stopped by us, not by the stream failing
    cut: Mutex<Vec<(String, i64)>>,
    /// page that owns the current session (several app windows share one connection)
    pub owner: Mutex<String>,
    /// (seq, probe-cache key, start time) of the current session
    began: Mutex<(u64, String, i64)>,
}

const TEXT_SUBS: &[&str] = &["subrip", "ass", "ssa", "mov_text", "webvtt", "text", "srt"];

fn pkey(url: &str) -> String {
    crate::re!(r"^https?://[^/]+").replace(url, "").to_string()
}

fn norm_kind(kind: &str) -> &'static str {
    match kind {
        "movie" | "m" | "vod" => "movie",
        "series" | "s" | "e" | "episode" | "tv" => "series",
        "live" | "l" => "live",
        _ => "",
    }
}

/// Extract `(kind, id)` from a probe-cache key like `"/movie/user/pass/12345.mkv"`.
fn parse_probe_key(k: &str) -> Option<(String, i64)> {
    let kind = if k.starts_with("/movie/") {
        "movie"
    } else if k.starts_with("/series/") {
        "series"
    } else if k.starts_with("/live/") {
        "live"
    } else {
        ""
    };
    let tail = k.rsplit('/').next()?;
    let id_part = tail.split('.').next()?;
    let id: i64 = id_part.parse().ok()?;
    Some((kind.to_string(), id))
}

fn atomic_write_file(path: &Path, bytes: &[u8], lock: &Mutex<()>) {
    let _g = lock.lock().unwrap();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        for _ in 0..3 {
            if std::fs::rename(&tmp, path).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let _ = std::fs::copy(&tmp, path);
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn pick_subs(subs: &Value) -> Value {
    let mut v: Vec<Value> = subs.as_array().cloned().unwrap_or_default();
    v.sort_by_key(|s| {
        (
            !s["lang"].as_str().unwrap_or("").starts_with("en"),
            s["i"].as_i64().unwrap_or(0),
        )
    });
    v.truncate(8);
    Value::Array(v)
}

fn quality_json_from_probe(v: &Value) -> Option<Value> {
    if v["v"].as_i64() != Some(2) {
        return None;
    }
    let mut q = v["pic"].clone();
    fix_fps(&mut q);
    q["ch"] = json!(v["audio"]
        .as_array()
        .and_then(|a| a.iter().filter_map(|x| x["ch"].as_i64()).max())
        .unwrap_or(0));
    Some(q)
}

impl Streamer {
    pub fn new(data: &Path) -> Self {
        let sub_dir = data.join("subs");
        let _ = std::fs::create_dir_all(&sub_dir);
        // Clean up stale session subdirectories and legacy loose files from previous server runs
        let clean_dir = sub_dir.clone();
        std::thread::spawn(move || {
            if let Ok(rd) = std::fs::read_dir(&clean_dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let _ = std::fs::remove_dir_all(&p);
                    } else {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        });
        let probe_file = data.join("probe-cache.json");
        let raw_probes: HashMap<String, Value> = std::fs::read(&probe_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Streamer {
            active: Mutex::new(None),
            seq: AtomicU64::new(0),
            cur_seq: AtomicU64::new(0),
            last_stop: AtomicI64::new(0),
            sub_dir,
            probe_file,
            probe_save_lock: Arc::new(Mutex::new(())),
            probes: Mutex::new(ProbeStore::from_map(raw_probes)),
            markers: Mutex::new((0, false, false, false, 0.0)),
            on_stop: Mutex::new(None),
            bytes: AtomicU64::new(0),
            cut: Mutex::new(vec![]),
            owner: Mutex::new(String::new()),
            began: Mutex::new((0, String::new(), 0)),
        }
    }

    fn session_dir(&self, seq: u64) -> PathBuf {
        self.sub_dir.join(format!("s_{seq}"))
    }

    fn active_dir(&self) -> PathBuf {
        let s = self.cur_seq.load(Ordering::SeqCst);
        if s > 0 {
            self.session_dir(s)
        } else {
            self.sub_dir.clone()
        }
    }

    pub fn probe_count(&self) -> usize {
        self.probes.lock().unwrap().by_key.len()
    }

    pub fn cached_duration(&self, url: &str) -> f64 {
        self.probes
            .lock()
            .unwrap()
            .by_key
            .get(&pkey(url))
            .and_then(|v| v["duration"].as_f64())
            .filter(|&d| d.is_finite() && d > 0.0)
            .unwrap_or(0.0)
    }

    pub fn evict_probe(&self, url: &str) {
        self.probes.lock().unwrap().remove(&pkey(url));
    }

    /// Was this stream id cut by us (another session, a probe, a stop) in the last `ms`? Then a player
    /// error for it is not the stream's fault and mustn't count against its health.
    pub fn was_cut(&self, id: i64, ms: i64) -> bool {
        let suf = format!(":{id}");
        self.cut
            .lock()
            .unwrap()
            .iter()
            .any(|(k, t)| k.ends_with(&suf) && now_ms() - t < ms)
    }

    pub fn count(&self, n: usize) {
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }

    pub fn sent(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    pub fn is_active(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    pub fn last_stop(&self) -> i64 {
        self.last_stop
            .load(Ordering::SeqCst)
            .max(crate::slot::last_released_ms())
    }

    pub fn stop(&self, reason: &str) {
        let taken = self.active.lock().unwrap().take();
        if let Some((seq, key, mut child, lease)) = taken {
            let _ = child.start_kill();
            drop(lease);
            println!("[stream] stopped {key} ({reason})");
            self.last_stop.store(now_ms(), Ordering::SeqCst);
            {
                let mut c = self.cut.lock().unwrap();
                c.retain(|x| now_ms() - x.1 < 60_000);
                c.push((key, now_ms()));
            }
            self.ended(seq);
        }
    }

    fn stop_if(&self, seq: u64, reason: &str) {
        let taken = {
            let mut g = self.active.lock().unwrap();
            if g.as_ref().map_or(false, |a| a.0 == seq) {
                g.take()
            } else {
                None
            }
        };
        if let Some((_, key, mut c, lease)) = taken {
            let _ = c.start_kill();
            drop(lease);
            println!("[stream] stopped {key} ({reason})");
            self.last_stop.store(now_ms(), Ordering::SeqCst);
            self.ended(seq);
        }
    }

    /// Session over: hand its audio tap to the registered callback and clean up the session dir
    /// once ffmpeg releases Windows file locks.
    fn ended(&self, seq: u64) {
        let cb = {
            let mut g = self.on_stop.lock().unwrap();
            if g.as_ref().map_or(false, |x| x.0 == seq) {
                g.take()
            } else {
                None
            }
        };
        let sdir = self.session_dir(seq);
        if let Some((_, f)) = cb {
            let src = sdir.join("audio.pcm");
            let fp_dir = self.sub_dir.parent().unwrap_or(&self.sub_dir).join("fp");
            let dst = fp_dir.join(format!("session_{seq}.pcm"));
            // ffmpeg may hold the file for a moment after the kill: retry the move briefly
            std::thread::spawn(move || {
                let _ = std::fs::create_dir_all(&fp_dir);
                let mut handed = false;
                for _ in 0..20 {
                    if std::fs::rename(&src, &dst).is_ok() || std::fs::copy(&src, &dst).is_ok() {
                        handed = true;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if handed {
                    f(dst);
                }
                for _ in 0..10 {
                    if std::fs::remove_dir_all(&sdir).is_ok() || !sdir.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            });
        } else {
            // No audio callback: safely clean up the session dir in background after ffmpeg exits
            std::thread::spawn(move || {
                for _ in 0..15 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if std::fs::remove_dir_all(&sdir).is_ok() || !sdir.exists() {
                        break;
                    }
                }
            });
        }
    }

    pub fn on_stop(&self, seq: u64, f: Box<dyn FnOnce(PathBuf) + Send>) {
        *self.on_stop.lock().unwrap() = Some((seq, f));
    }

    pub fn mark_stop(&self) {
        self.last_stop.store(now_ms(), Ordering::SeqCst);
        crate::slot::mark_released();
    }

    pub async fn probe(&self, url: &str, ua: &str) -> Option<Value> {
        let k = pkey(url);
        // v2 entries carry picture details (resolution, HDR, frame rate); older ones are re-probed
        if let Some(mut v) = self.probes.lock().unwrap().by_key.get(&k).cloned() {
            if v["v"].as_i64() == Some(2) {
                fix_fps(&mut v["pic"]);
                return Some(v);
            }
        }
        self.stop("probe");
        // Through the read-ahead cache when this title is spooled (no new provider connection);
        // otherwise coordinate via SlotArbiter for exact single cooldown.
        let local = crate::spool::local_for(url);
        let via_spool = local.is_some();
        let lease = if via_spool {
            None
        } else {
            let l = crate::slot::acquire(Owner::Probe, url);
            if !l.wait_cooldown().await {
                l.release();
                return None;
            }
            Some(l)
        };
        let src = local.unwrap_or_else(|| url.to_string());
        let out = tokio::select! {
            r = tokio::time::timeout(std::time::Duration::from_secs(25), {
                let mut c = Command::new("ffprobe");
                c.args([
                    "-v",
                    "error",
                    "-user_agent",
                    ua,
                    "-rw_timeout",
                    "15000000",
                    "-probesize",
                    "2000000",
                    "-analyzeduration",
                    "1500000",
                    "-show_entries",
                    "format=duration,bit_rate:stream=index,codec_type,codec_name,codec_tag_string,profile,channels,width,height,pix_fmt,color_transfer,color_primaries,avg_frame_rate,r_frame_rate,field_order,bit_rate:stream_side_data=side_data_type,dv_profile:stream_tags=language,title:stream_disposition=forced,default",
                    "-show_chapters",
                    "-of",
                    "json",
                    &src,
                ]);
                #[cfg(windows)]
                c.creation_flags(0x08000000);
                c.kill_on_drop(true).output()
            }) => r.ok().and_then(|x| x.ok()),
            _ = async {
                if let Some(ref l) = lease { l.cancelled().await; }
                else { std::future::pending::<()>().await; }
            } => None,
        };
        // Only update upstream release timestamps when we actually touched the provider (not local spool!)
        if let Some(l) = lease {
            l.release();
            self.last_stop.store(now_ms(), Ordering::SeqCst);
        }
        let out = out?;
        if !out.status.success() {
            return None;
        }
        let j: Value = serde_json::from_slice(&out.stdout).ok()?;
        let streams = j["streams"].as_array().cloned().unwrap_or_default();
        let video = streams
            .iter()
            .find(|s| s["codec_type"] == "video")
            .map(|s| s["codec_name"].clone())
            .unwrap_or(json!(""));
        let audio: Vec<Value> = streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "i": i,
                    "codec": s["codec_name"],
                    "ch": s["channels"],
                    "lang": s["tags"]["language"].as_str().unwrap_or(""),
                    "title": s["tags"]["title"].as_str().unwrap_or(""),
                })
            })
            .collect();
        let subs: Vec<Value> = streams
            .iter()
            .filter(|s| s["codec_type"] == "subtitle")
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "i": i,
                    "codec": s["codec_name"],
                    "lang": s["tags"]["language"].as_str().unwrap_or(""),
                    "title": s["tags"]["title"].as_str().unwrap_or(""),
                    "forced": s["disposition"]["forced"].as_i64().unwrap_or(0) == 1,
                })
            })
            .filter(|s| TEXT_SUBS.contains(&s["codec"].as_str().unwrap_or("")))
            .collect();
        let dur = j["format"]["duration"]
            .as_str()
            .and_then(|x| x.parse::<f64>().ok())
            .unwrap_or(0.0);
        let raw_chapters: Vec<(f64, f64, String)> = j["chapters"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                (
                    c["start_time"]
                        .as_str()
                        .and_then(|x| x.parse::<f64>().ok())
                        .unwrap_or(0.0),
                    c["end_time"]
                        .as_str()
                        .and_then(|x| x.parse::<f64>().ok())
                        .unwrap_or(0.0),
                    c["tags"]["title"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let skips: Vec<Value> = raw_chapters
            .iter()
            .filter(|(s, e, t)| {
                crate::re!(r"(?i)\b(intro|opening|op|recap|previously|title sequence|main title)\b")
                    .is_match(t)
                    && *e > *s + 4.0
                    && *e - *s < 300.0
            })
            .map(|(s, e, t)| json!({ "s": s, "e": e, "title": t }))
            .collect();
        let cred_ch: Option<(f64, f64)> = raw_chapters
            .iter()
            .filter(|(s, _, t)| {
                crate::re!(r"(?i)\b(end credits|credits|outro|ending|ed|end titles|closing)\b")
                    .is_match(t)
                    && *s > 60.0
                    && (dur <= 120.0 || (*s >= dur * 0.55 && *s < dur - 5.0))
            })
            .map(|(s, e, _)| (*s, *e))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let chapter_credits = cred_ch.map(|x| x.0);
        let chapter_credits_end = cred_ch.and_then(|(s, e)| (e > s + 5.0).then_some(e));
        let pic = streams
            .iter()
            .find(|s| s["codec_type"] == "video")
            .map(picture)
            .unwrap_or(json!({}));
        let mut r = json!({
            "v": 2,
            "duration": dur,
            "video": video,
            "audio": audio,
            "subs": pick_subs(&Value::Array(subs)),
            "skips": skips,
            "chapterCredits": chapter_credits,
            "chapterCreditsEnd": chapter_credits_end,
            "pic": pic,
        });
        r["pic"]["kbps"] = json!(j["format"]["bit_rate"]
            .as_str()
            .and_then(|x| x.parse::<f64>().ok())
            .map(|b| (b / 1000.0).round())
            .unwrap_or(0.0));
        let bytes = {
            let mut p = self.probes.lock().unwrap();
            p.insert(k, r.clone());
            serde_json::to_vec(&p.by_key).ok()
        };
        if let Some(bytes) = bytes {
            let path = self.probe_file.clone();
            let lock = self.probe_save_lock.clone();
            let _ = tokio::task::spawn_blocking(move || atomic_write_file(&path, &bytes, &lock))
                .await;
        }
        Some(r)
    }

    /// Picture details of an already-probed stream id for a specific kind (`"movie"` or `"series"`), O(1).
    pub fn quality_of_kind(&self, kind: &str, id: i64) -> Option<Value> {
        let p = self.probes.lock().unwrap();
        p.get_by_kind_id(kind, id).and_then(quality_json_from_probe)
    }

    /// Audio stream list of an already-probed stream id for a specific kind (`"movie"` or `"series"`), O(1).
    pub fn audio_of_kind(&self, kind: &str, id: i64) -> Option<Vec<Value>> {
        let p = self.probes.lock().unwrap();
        p.get_by_kind_id(kind, id)
            .and_then(|v| v["audio"].as_array().cloned())
    }

    /// Picture details of an already-probed stream id (any kind), O(1) via secondary index.
    pub fn quality_of(&self, id: i64) -> Option<Value> {
        let p = self.probes.lock().unwrap();
        p.find_by_id_any(id, quality_json_from_probe)
    }

    /// Audio stream list of an already-probed stream id (any kind), O(1) via secondary index.
    pub fn audio_of(&self, id: i64) -> Option<Vec<Value>> {
        let p = self.probes.lock().unwrap();
        p.find_by_id_any(id, |v| v["audio"].as_array().cloned())
    }

    fn sub_path_in(&self, dir: &Path, i: i64) -> PathBuf {
        dir.join(format!("sub_{i}.vtt"))
    }

    pub fn read_sub(&self, i: i64) -> String {
        std::fs::read_to_string(self.sub_path_in(&self.active_dir(), i)).unwrap_or_default()
    }

    /// First packet of a framecrc marker: (dts, pts) in seconds, plus the audio sample rate if any.
    fn marker_in(&self, dir: &Path, name: &str) -> Option<(f64, f64, f64)> {
        let t = std::fs::read_to_string(dir.join(name)).ok()?;
        let mut tb = 0.001;
        let mut sr = 0.0;
        for l in t.lines() {
            if let Some(x) = l.strip_prefix("#tb 0:") {
                if let Some((n, d)) = x.trim().split_once('/') {
                    tb = n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok()?;
                }
            }
            if let Some(x) = l.strip_prefix("#sample_rate 0:") {
                sr = x.trim().parse().unwrap_or(0.0);
            }
        }
        let l = t.lines().find(|l| l.starts_with("0,"))?;
        let c: Vec<&str> = l.split(',').map(|x| x.trim()).collect();
        let dts: i64 = c.get(1)?.parse().ok()?;
        let pts: i64 = c.get(2)?.parse().unwrap_or(dts);
        if dts.abs() > 1_000_000_000_000_000 {
            return None; // NOPTS
        }
        Some((dts as f64 * tb, pts as f64 * tb, sr))
    }

    fn starts_from_dir(
        &self,
        dir: &Path,
        ev: bool,
        ea: bool,
        tx: bool,
        t0: f64,
    ) -> Option<(f64, f64, f64)> {
        // Re-encoded video uses an exact seek: both tracks are cut at t0, unless a track only begins later.
        if tx {
            let v = if ev {
                let m = self.marker_in(dir, "start_v.txt")?;
                Some(m.1.max(t0))
            } else {
                None
            };
            let a = if ea {
                let m = self.marker_in(dir, "start_a.txt")?;
                Some(m.0.max(t0))
            } else {
                None
            };
            return match (v, a) {
                (Some(v), Some(a)) => Some((v, v, a)),
                (Some(v), None) => Some((v, v, v)),
                (None, Some(a)) => Some((a, a, a)),
                _ => None,
            };
        }
        let v = if ev {
            let m = self.marker_in(dir, "start_v.txt")?;
            Some((m.0, m.1))
        } else {
            None
        };
        let a = if ea {
            let m = self.marker_in(dir, "start_a.txt")?;
            Some(m.0 - 1024.0 / if m.2 > 0.0 { m.2 } else { 48000.0 })
        } else {
            None
        };
        match (v, a) {
            (Some(v), Some(a)) => Some((v.0, v.1, a)),
            (Some(v), None) => Some((v.0, v.1, v.0)),
            (None, Some(a)) => Some((a, a, a)),
            _ => None,
        }
    }

    /// Each track's true first timestamp (film time, seconds) for a specific session `seq`.
    pub fn starts_for(&self, seq: u64) -> Option<(f64, f64, f64)> {
        let (mseq, ev, ea, tx, t0) = *self.markers.lock().unwrap();
        if mseq != seq {
            return None;
        }
        let dir = self.session_dir(seq);
        self.starts_from_dir(&dir, ev, ea, tx, t0)
    }

    /// Each track's true first timestamp (film time, seconds) for the active session.
    pub fn starts(&self) -> Option<(f64, f64, f64)> {
        let (mseq, ev, ea, tx, t0) = *self.markers.lock().unwrap();
        let dir = if mseq > 0 {
            self.session_dir(mseq)
        } else {
            self.active_dir()
        };
        self.starts_from_dir(&dir, ev, ea, tx, t0)
    }

    /// Session start in film time = the streamed MP4's time zero.
    pub fn session_start(&self) -> Option<f64> {
        self.starts().map(|(v, _, a)| v.min(a))
    }

    pub fn sync_paths(&self, i: i64) -> (PathBuf, PathBuf) {
        let dir = self.active_dir();
        (dir.join("audio.pcm"), self.sub_path_in(&dir, i))
    }

    /// Start ffmpeg and return (sequence id, stdout). The caller streams stdout to the client.
    pub fn start(
        &self,
        url: &str,
        ua: &str,
        key: &str,
        live: bool,
        start: f64,
        audio: i64,
        transcode: bool,
        info: Option<&Value>,
        audio_delay_ms: i64,
        ts: bool,
        surround_out: bool,
        local_override: Option<&str>,
        night: bool,
        deint: bool,
    ) -> Option<(u64, tokio::process::ChildStdout)> {
        self.stop("replaced");
        self.bytes.store(0, Ordering::Relaxed);

        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let sdir = self.session_dir(seq);
        let _ = std::fs::create_dir_all(&sdir);
        self.cur_seq.store(seq, Ordering::SeqCst);

        // Background sweep of stale old `s_*` dirs (leave seq and seq-1 alone for ended() callback)
        let sweep_root = self.sub_dir.clone();
        std::thread::spawn(move || {
            if let Ok(rd) = std::fs::read_dir(&sweep_root) {
                for entry in rd.flatten() {
                    let name = entry.file_name();
                    let name_s = name.to_string_lossy();
                    if let Some(old_seq) = name_s
                        .strip_prefix("s_")
                        .and_then(|n| n.parse::<u64>().ok())
                    {
                        if old_seq + 1 < seq {
                            let _ = std::fs::remove_dir_all(entry.path());
                        }
                    }
                }
            }
        });

        let vcodec = info
            .and_then(|i| i["video"].as_str())
            .unwrap_or("")
            .to_string();
        let pic = info.map(|i| i["pic"].clone()).unwrap_or(json!({}));
        let ten_bit_avc = vcodec == "h264" && pic["bits"].as_i64().unwrap_or(8) > 8; // Hi10P: no browser decodes it
        let do_deint = live && deint;
        let must_tx = transcode
            || do_deint
            || ten_bit_avc
            || (!vcodec.is_empty() && !["h264", "hevc", "av1", "vp9"].contains(&vcodec.as_str()));
        let hdr = pic["hdr"].as_str().map_or(false, |h| !h.is_empty());

        let mut a: Vec<String> = vec![
            "-y".into(),
            "-nostdin".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-user_agent".into(),
            ua.into(),
        ];

        let local_src = local_override
            .map(String::from)
            .or_else(|| crate::spool::local_for(url));
        let via_local = local_src.is_some();
        let src = local_src.unwrap_or_else(|| url.to_string());

        // Live: short probe so channels start fast; drop +igndts on live fMP4 so B-frame DTS stays intact
        if live {
            for x in [
                "-probesize",
                "1500000",
                "-analyzeduration",
                "1500000",
                "-rw_timeout",
                "10000000",
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_at_eof",
                "1",
                "-reconnect_on_network_error",
                "1",
                "-reconnect_delay_max",
                "6",
                "-fflags",
                "+genpts+discardcorrupt",
            ] {
                a.push(x.into());
            }
        } else if via_local && info.is_some() {
            // Reading from localhost gigabit RAM spool with streams already probed: fast analysis & fastseek!
            for x in [
                "-analyzeduration",
                "1000000",
                "-probesize",
                "1000000",
                "-rw_timeout",
                "15000000",
                "-fflags",
                "+fastseek",
            ] {
                a.push(x.into());
            }
        } else {
            for x in [
                "-analyzeduration",
                "3000000",
                "-probesize",
                "3000000",
                "-rw_timeout",
                "15000000",
                "-fflags",
                "+fastseek",
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_on_network_error",
                "1",
                "-reconnect_on_http_error",
                "5xx",
                "-reconnect_delay_max",
                "10",
            ] {
                a.push(x.into());
            }
        }

        // A/V sync on seek/resume: -noaccurate_seek starts copied video & audio at the same keyframe;
        // fmp4.rs restores each track's exact start via framecrc markers.
        if start > 0.0 {
            if !live && !must_tx {
                a.push("-noaccurate_seek".into());
            }
            a.push("-ss".into());
            a.push(format!("{start}"));
        }
        if !live {
            a.push("-copyts".into());
        }

        for x in ["-i", src.as_str(), "-map", "0:v:0?", "-map"] {
            a.push(x.into());
        }
        a.push(format!("0:a:{audio}?"));

        if do_deint {
            a.push("-vf".into());
            a.push("bwdif=mode=1:parity=-1:deint=1".into());
        }

        // NVENC VBR CQ mode: -rc vbr -b:v 0 -maxrate 30M -bufsize 60M avoids ffmpeg's 2 Mbps default cap!
        if must_tx && hdr {
            for x in [
                "-c:v",
                "hevc_nvenc",
                "-preset",
                "p5",
                "-rc",
                "vbr",
                "-cq",
                "20",
                "-b:v",
                "0",
                "-maxrate",
                "30M",
                "-bufsize",
                "60M",
                "-bf",
                "0",
                "-profile:v",
                "main10",
                "-pix_fmt",
                "p010le",
                "-tag:v",
                "hvc1",
                "-color_primaries",
                "bt2020",
                "-colorspace",
                "bt2020nc",
                "-color_trc",
                if pic["hdr"] == "HLG" {
                    "arib-std-b67"
                } else {
                    "smpte2084"
                },
            ] {
                a.push(x.into());
            }
        } else if must_tx {
            for x in [
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p5",
                "-rc",
                "vbr",
                "-cq",
                "20",
                "-b:v",
                "0",
                "-maxrate",
                "30M",
                "-bufsize",
                "60M",
                "-bf",
                "0",
                "-pix_fmt",
                "yuv420p",
            ] {
                a.push(x.into());
            }
        } else {
            a.push("-c:v".into());
            a.push("copy".into());
            if vcodec == "hevc" {
                a.push("-tag:v".into());
                a.push("hvc1".into());
            }
        }

        let ch = info
            .and_then(|i| {
                i["audio"]
                    .as_array()
                    .and_then(|a| a.get(audio as usize))
                    .and_then(|x| x["ch"].as_i64())
            })
            .unwrap_or(2);
        let surround = !ts && surround_out && ch >= 6;
        let mut af_parts: Vec<String> = vec![];
        // Live TV always keeps async timestamp resampling active even when audio_delay_ms != 0
        if live {
            af_parts.push("aresample=async=1000:first_pts=0".into());
        }
        if audio_delay_ms > 0 {
            af_parts.push(format!("adelay=delays={audio_delay_ms}:all=1"));
        } else if audio_delay_ms < 0 {
            let shift = (-audio_delay_ms) as f64 / 1000.0;
            if live {
                af_parts.push(format!("atrim=start={shift:.3},asetpts=PTS-STARTPTS"));
            } else {
                // On VOD with -copyts, NEVER reset PTS to 0 (asetpts=PTS-STARTPTS breaks interleaving!);
                // trim exact samples at 48 kHz and shift PTS back by `shift` preserving -copyts base.
                let samples = ((-audio_delay_ms) as u64).saturating_mul(48).max(1);
                af_parts.push(format!(
                    "aresample=48000,atrim=start_sample={samples},asetpts=PTS-{shift:.3}/TB"
                ));
            }
        }
        if !ts && !surround_out && ch >= 6 {
            // Unity-gain 5.1/7.1 -> stereo downmix (weights sum to 1.00: no +7 dB clipping, keeps SL/SR!)
            af_parts.push(
                "pan=stereo|FL=0.32*FL+0.23*FC+0.20*BL+0.20*SL+0.05*LFE|FR=0.32*FR+0.23*FC+0.20*BR+0.20*SR+0.05*LFE"
                    .into(),
            );
        }
        if night {
            af_parts.push("dynaudnorm=f=150:g=15:p=0.9".into());
        }
        let af = if af_parts.is_empty() {
            None
        } else {
            Some(af_parts.join(","))
        };
        if let Some(f) = &af {
            a.push("-af".into());
            a.push(f.clone());
        }

        if surround {
            for x in ["-c:a", "aac", "-ac", "6", "-b:a", "448k"] {
                a.push(x.into());
            }
        } else {
            for x in ["-c:a", "aac", "-ac", "2", "-b:a", "256k"] {
                a.push(x.into());
            }
        }
        if ts {
            for x in ["-f", "mpegts", "-mpegts_flags", "+resend_headers", "pipe:1"] {
                a.push(x.into());
            }
        } else {
            for x in [
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof",
                "-frag_duration",
                "500000",
                "-f",
                "mp4",
                "pipe:1",
            ] {
                a.push(x.into());
            }
        }

        let subs = info.map(|i| pick_subs(&i["subs"])).unwrap_or(json!([]));
        let has_v = info.map_or(false, |i| !i["video"].as_str().unwrap_or("").is_empty());
        let has_a = info.map_or(false, |i| {
            i["audio"]
                .as_array()
                .map_or(false, |x| (audio as usize) < x.len())
        });
        *self.markers.lock().unwrap() = (seq, !live && !ts && has_v, !live && !ts && has_a, must_tx, start);

        // Skip VTT, framecrc start markers, and audio.pcm side outputs on DLNA Cast (MPEG-TS)
        if !ts {
            for s in subs.as_array().into_iter().flatten() {
                let i = s["i"].as_i64().unwrap_or(0);
                for x in [
                    "-map".to_string(),
                    format!("0:s:{i}?"),
                    "-c:s".into(),
                    "webvtt".into(),
                    "-flush_packets".into(),
                    "1".into(),
                    "-f".into(),
                    "webvtt".into(),
                    self.sub_path_in(&sdir, i).to_string_lossy().to_string(),
                ] {
                    a.push(x);
                }
            }
            if !live && has_v {
                for x in [
                    "-map".to_string(),
                    "0:v:0?".into(),
                    "-c".into(),
                    "copy".into(),
                    "-frames:v".into(),
                    "1".into(),
                    "-flush_packets".into(),
                    "1".into(),
                    "-f".into(),
                    "framecrc".into(),
                    sdir.join("start_v.txt").to_string_lossy().to_string(),
                ] {
                    a.push(x);
                }
            }
            if !live && has_a {
                for x in [
                    "-map".to_string(),
                    format!("0:a:{audio}?"),
                    "-c".into(),
                    "copy".into(),
                    "-frames:a".into(),
                    "1".into(),
                    "-flush_packets".into(),
                    "1".into(),
                    "-f".into(),
                    "framecrc".into(),
                    sdir.join("start_a.txt").to_string_lossy().to_string(),
                ] {
                    a.push(x);
                }
            }
            // Low-rate mono copy of the audio: subtitle sync + intro/credits fingerprints.
            // Keep lightweight (skip heavy dynaudnorm/5.1 pan filters on 8 kHz VAD tap).
            if !live && has_a {
                for x in ["-map".to_string(), format!("0:a:{audio}?")] {
                    a.push(x);
                }
                if audio_delay_ms > 0 {
                    a.push("-af".into());
                    a.push(format!("adelay=delays={audio_delay_ms}:all=1"));
                } else if audio_delay_ms < 0 {
                    let shift = (-audio_delay_ms) as f64 / 1000.0;
                    let samples = ((-audio_delay_ms) as u64).saturating_mul(8).max(1);
                    a.push("-af".into());
                    a.push(format!("aresample=8000,atrim=start_sample={samples},asetpts=PTS-{shift:.3}/TB"));
                }
                for x in ["-ac", "1", "-ar", "8000", "-c:a", "pcm_s16le", "-f", "s16le"] {
                    a.push(x.into());
                }
                a.push(sdir.join("audio.pcm").to_string_lossy().to_string());
            }
        }

        let lease = if via_local {
            None
        } else {
            Some(crate::slot::acquire(Owner::Ffmpeg, url))
        };

        let mut cmd = Command::new("ffmpeg");
        cmd.args(&a)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        // TV_FFLOG=1: ffmpeg's errors go to data/ffmpeg.log (diagnostics)
        match std::env::var("TV_FFLOG").ok().and_then(|_| {
            std::fs::File::create(
                self.sub_dir
                    .parent()
                    .unwrap_or(&self.sub_dir)
                    .join("ffmpeg.log"),
            )
            .ok()
        }) {
            Some(f) => {
                cmd.stderr(f);
            }
            None => {
                cmd.stderr(Stdio::null());
            }
        }
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                drop(lease);
                println!("[stream] ffmpeg failed to start: {e}");
                return None;
            }
        };
        let out = child.stdout.take()?;
        *self.active.lock().unwrap() = Some((seq, key.to_string(), child, lease));
        *self.began.lock().unwrap() = (seq, pkey(url), now_ms());
        println!(
            "[stream] {key} seq={seq} start={start} audio={audio} subs={} {}{}",
            if ts {
                0
            } else {
                subs.as_array().map_or(0, |a| a.len())
            },
            if must_tx { "transcode" } else { "copy" },
            if vcodec.is_empty() {
                String::new()
            } else {
                format!(" {vcodec}")
            }
        );
        Some((seq, out))
    }

    /// Output ended: was it the real end? False when the session was cut (by us or another window)
    /// or ffmpeg failed, so the player gets an error (and resumes) instead of a clean "film over".
    /// Waits up to 3.0 s cleanly for ffmpeg to finalize side outputs; returns false only if killed/errored.
    pub async fn ended_ok(&self, seq: u64) -> bool {
        for _ in 0..30 {
            let st = {
                let mut g = self.active.lock().unwrap();
                match g.as_mut() {
                    Some((s, _, c, _)) if *s == seq => Some(c.try_wait()),
                    _ => None,
                }
            };
            match st {
                None => return false,
                Some(Ok(Some(code))) => {
                    if !code.success() {
                        let (s, k, t) = self.began.lock().unwrap().clone();
                        if s == seq && now_ms() - t < 8000 {
                            self.probes.lock().unwrap().remove(&k);
                        }
                    }
                    return code.success();
                }
                Some(Err(_)) => return false,
                Some(Ok(None)) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
        // Still running cleanly after 3 s (not killed/replaced, no error exit)
        let still_ours = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .map_or(false, |(s, _, _, _)| *s == seq);
        still_ours
    }

    pub fn finished(&self, seq: u64) {
        self.stop_if(seq, "client closed");
    }
}

/// Picture details from ffprobe's video stream: size, bit depth, HDR format, frame rate, interlacing.
fn picture(s: &Value) -> Value {
    let rate = |k: &str| {
        s[k].as_str()
            .and_then(|r| r.split_once('/'))
            .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?.max(1e-9)))
            .filter(|x| x.is_finite() && *x > 0.0 && *x < 300.0)
    };
    // MKV's average rate is often off-grid (e.g. 24.417 for 23.976 video): snap to a real rate, else trust r_frame_rate
    let fps = {
        let a = rate("avg_frame_rate").map(snap_fps);
        let r = rate("r_frame_rate").map(snap_fps);
        match (a, r) {
            (Some(a), _) if is_std(a) => a,
            (_, Some(r)) if is_std(r) => r,
            (Some(a), _) => a,
            (None, Some(r)) => r,
            _ => 0.0,
        }
    };
    let pix = s["pix_fmt"].as_str().unwrap_or("");
    let bits = if crate::re!(r"p1[02]").is_match(pix) || pix.contains("10le") {
        10
    } else {
        8
    };
    let side = s["side_data_list"].as_array().cloned().unwrap_or_default();
    let dv = side
        .iter()
        .find(|x| {
            x["side_data_type"]
                .as_str()
                .map_or(false, |t| t.contains("DOVI"))
        })
        .and_then(|x| x["dv_profile"].as_i64())
        .or_else(|| {
            if ["dvh1", "dvhe"].contains(&s["codec_tag_string"].as_str().unwrap_or("")) {
                Some(5)
            } else {
                None
            }
        });
    let trc = s["color_transfer"].as_str().unwrap_or("");
    let hdr = if dv.is_some() && trc != "smpte2084" && trc != "arib-std-b67" {
        "DV"
    } else if trc == "smpte2084" {
        "HDR10"
    } else if trc == "arib-std-b67" {
        "HLG"
    } else if dv.is_some() {
        "DV"
    } else {
        ""
    };
    let fo = s["field_order"].as_str().unwrap_or("progressive");
    json!({
        "w": s["width"],
        "h": s["height"],
        "bits": bits,
        "hdr": hdr,
        "dv": dv,
        "fps": (fps * 1000.0).round() / 1000.0,
        "interlaced": !fo.is_empty() && fo != "progressive" && fo != "unknown",
        "codec": s["codec_name"],
    })
}

const STD_FPS: [f64; 9] = [23.976, 24.0, 25.0, 29.97, 30.0, 47.952, 50.0, 59.94, 60.0];
fn is_std(f: f64) -> bool {
    STD_FPS.iter().any(|s| (s - f).abs() < 1e-6)
}
/// Nearest standard frame rate within 1.5 %, else unchanged.
pub fn snap_fps(f: f64) -> f64 {
    STD_FPS
        .iter()
        .copied()
        .min_by(|a, b| (a - f).abs().total_cmp(&(b - f).abs()))
        .filter(|s| (s - f).abs() / s < 0.015)
        .unwrap_or(f)
}
/// Cached entries from before snapping: an off-grid rate (24.417) is treated as 23.976 film.
fn fix_fps(pic: &mut Value) {
    if let Some(f) = pic["fps"].as_f64() {
        if f > 0.0 && !is_std(f) {
            let s = snap_fps(f);
            pic["fps"] = json!(if is_std(s) {
                s
            } else if (f - 24.0).abs() < 1.0 {
                23.976
            } else {
                f
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_secondary_index_separates_movie_and_series_ids() {
        let mut store = ProbeStore::default();
        store.insert(
            "/movie/u/p/42.mkv".into(),
            json!({ "v": 2, "pic": { "w": 3840, "h": 2160 }, "audio": [{ "ch": 6, "lang": "eng" }] }),
        );
        store.insert(
            "/series/u/p/42.mkv".into(),
            json!({ "v": 2, "pic": { "w": 1920, "h": 1080 }, "audio": [{ "ch": 2, "lang": "jpn" }] }),
        );
        assert_eq!(store.get_by_kind_id("movie", 42).unwrap()["pic"]["h"], 2160);
        assert_eq!(store.get_by_kind_id("series", 42).unwrap()["pic"]["h"], 1080);
        assert_eq!(store.get_by_kind_id("m", 42).unwrap()["pic"]["h"], 2160);
        assert_eq!(store.get_by_kind_id("e", 42).unwrap()["pic"]["h"], 1080);
        assert!(store.find_by_id_any(42, quality_json_from_probe).is_some());

        store.remove("/movie/u/p/42.mkv");
        assert!(store.get_by_kind_id("movie", 42).is_none());
        assert_eq!(store.get_by_kind_id("series", 42).unwrap()["pic"]["h"], 1080);
    }
}
