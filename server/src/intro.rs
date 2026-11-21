// Intro / end-credits detection by audio fingerprinting.
// Episodes of a show share the same theme music. We fingerprint the first and last few minutes of
// each episode's audio (8 kHz mono, the same tap ffmpeg already writes during playback, or a short
// background fetch while the account is idle) and look for a long stretch two episodes have in
// common. Fingerprint: per 16 ms frame, 16 bits = sign of the change in energy difference between
// adjacent log-spaced bands (robust to re-encoding, volume and small timing shifts).
use rustfft::{num_complex::Complex, FftPlanner};

pub const RATE: usize = 8000;
const WIN: usize = 1024; // 128 ms
const HOP: usize = 128; // 16 ms
pub const FPS: f64 = RATE as f64 / HOP as f64; // 62.5 frames/s
const BANDS: usize = 17;
const SILENT: u32 = 0x1_0000; // marker bit (outside the 16 fingerprint bits)

/// One u32 per frame: low 16 bits = fingerprint, SILENT bit set for near-silent frames.
pub fn fingerprint(pcm: &[i16]) -> Vec<u32> {
    if pcm.len() < WIN * 2 { return vec![]; }
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WIN);
    let hann: Vec<f32> = (0..WIN).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / WIN as f32).cos()).collect();
    // band edges (FFT bins), log spaced 250..3500 Hz
    let edges: Vec<usize> = (0..=BANDS).map(|b| {
        let f = 250.0 * (3500.0f64 / 250.0).powf(b as f64 / BANDS as f64);
        ((f / RATE as f64) * WIN as f64).round() as usize
    }).collect();
    let mut out = vec![]; let mut prev: Option<Vec<f32>> = None;
    let mut buf = vec![Complex::new(0.0f32, 0.0); WIN];
    let mut i = 0;
    while i + WIN <= pcm.len() {
        let mut total = 0.0f32;
        for k in 0..WIN { let v = pcm[i + k] as f32 / 32768.0; total += v * v; buf[k] = Complex::new(v * hann[k], 0.0); }
        fft.process(&mut buf);
        let e: Vec<f32> = (0..BANDS).map(|b| (edges[b]..edges[b + 1].max(edges[b] + 1)).map(|k| buf[k].norm_sqr()).sum::<f32>().max(1e-12).ln()).collect();
        let rms = (total / WIN as f32).sqrt();
        let mut bits = 0u32;
        if let Some(p) = &prev {
            for b in 0..16 { if (e[b] - e[b + 1]) - (p[b] - p[b + 1]) > 0.0 { bits |= 1 << b; } }
        }
        if rms < 0.003 { bits |= SILENT; }
        out.push(bits);
        prev = Some(e);
        i += HOP;
    }
    out
}

pub fn to_bytes(fp: &[u32]) -> Vec<u8> { fp.iter().flat_map(|x| x.to_le_bytes()).collect() }
pub fn from_bytes(b: &[u8]) -> Vec<u32> { b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect() }

/// Longest shared stretch between two fingerprints: (a_start, a_end, b_start, b_end) in seconds.
pub fn best_match(a: &[u32], b: &[u32], min_secs: f64) -> Option<(f64, f64, f64, f64)> {
    if a.len() < 100 || b.len() < 100 { return None; }
    // 1) candidate alignments: exact 16-bit hits vote for an offset (b_index - a_index)
    let mut index: std::collections::HashMap<u16, Vec<u32>> = Default::default();
    for (i, &x) in a.iter().enumerate() { if x & SILENT == 0 { index.entry(x as u16).or_default().push(i as u32); } }
    let mut votes: std::collections::HashMap<i64, u32> = Default::default();
    for (j, &y) in b.iter().enumerate() {
        if y & SILENT != 0 { continue; }
        if let Some(list) = index.get(&(y as u16)) { if list.len() > 50 { continue; } for &i in list { *votes.entry(j as i64 - i as i64).or_default() += 1; } }
    }
    let mut cand: Vec<(i64, u32)> = votes.iter().map(|(&o, &n)| (o, n + votes.get(&(o - 1)).copied().unwrap_or(0) + votes.get(&(o + 1)).copied().unwrap_or(0))).collect();
    cand.sort_by(|x, y| y.1.cmp(&x.1));
    let mut tried: Vec<i64> = vec![]; let mut best: Option<(usize, usize, i64)> = None;
    for (off0, n) in cand.into_iter().take(40) {
        if n < 8 || tried.iter().any(|t| (t - off0).abs() <= 3) { continue; }
        tried.push(off0); if tried.len() > 6 { break; }
        for off in [off0 - 1, off0, off0 + 1] {
            // 2) verify: smoothed bit-error rate along this exact alignment (random audio ~0.5)
            let lo = 0i64.max(-off) as usize; let hi = (a.len() as i64).min(b.len() as i64 - off) as usize;
            if hi <= lo + 100 { continue; }
            let err: Vec<f32> = (lo..hi).map(|i| {
                let (x, y) = (a[i], b[(i as i64 + off) as usize]);
                let e = if (x | y) & SILENT != 0 { if x & y & SILENT != 0 { 6 } else { 12 } } else { ((x ^ y) & 0xFFFF).count_ones() };
                e as f32 / 16.0
            }).collect();
            let w = (2.0 * FPS) as usize; let mut acc = 0.0f32; let mut good = vec![false; err.len()];
            for k in 0..err.len() { acc += err[k]; if k >= w { acc -= err[k - w]; } let m = acc / (k.min(w - 1) + 1) as f32; if k >= w / 2 { good[k - w / 2] = m < 0.36; } }
            // bridge short dropouts (a loud sound effect over the theme, a codec glitch)
            let gap = (3.0 * FPS) as usize; let mut last_good: Option<usize> = None;
            for k in 0..good.len() { if good[k] { if let Some(l) = last_good { if k - l > 1 && k - l <= gap { for x in l..k { good[x] = true; } } } last_good = Some(k); } }
            let (mut run_s, mut bs, mut bl) = (None, 0usize, 0usize);
            for k in 0..=good.len() {
                let g = k < good.len() && good[k];
                match (g, run_s) { (true, None) => run_s = Some(k), (false, Some(s)) => { if k - s > bl { bl = k - s; bs = s; } run_s = None; } _ => {} }
            }
            if bl > best.map_or(0, |b| b.1 - b.0) { best = Some((lo + bs, lo + bs + bl, off)); }
        }
    }
    let (s, e, off) = best?;
    let secs = (e - s) as f64 / FPS;
    if secs < min_secs { return None; }
    Some((s as f64 / FPS, e as f64 / FPS, (s as i64 + off) as f64 / FPS, (e as i64 + off) as f64 / FPS))
}

// ---------------------------------------------------------------- per-episode store + analysis
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub const HEAD_SECS: f64 = 330.0; // intros almost always finish within the first ~5 minutes
pub const TAIL_SECS: f64 = 300.0;

pub struct Skips { dir: PathBuf, file: PathBuf, db: Mutex<Value>, pub cancel: AtomicBool, pub busy: AtomicBool }

impl Skips {
    pub fn new(data: &Path) -> Self {
        let dir = data.join("fp"); let _ = std::fs::create_dir_all(&dir);
        let file = data.join("skips.json");
        let db = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({ "eps": {} }));
        Skips { dir, file, db: Mutex::new(db), cancel: Default::default(), busy: Default::default() }
    }
    fn save(&self) { let b = serde_json::to_vec(&*self.db.lock().unwrap()).unwrap(); let t = self.file.with_extension("tmp"); if std::fs::write(&t, b).is_ok() { let _ = std::fs::rename(&t, &self.file); } }
    pub fn register(&self, ep: i64, sid: i64, season: i64, n: i64, dur: f64) {
        let mut g = self.db.lock().unwrap(); let e = &mut g["eps"][ep.to_string()];
        if e.is_null() { *e = json!({}); }
        e["sid"] = json!(sid); e["season"] = json!(season); e["n"] = json!(n); if dur > 0.0 { e["dur"] = json!(dur); }
    }
    pub fn entry(&self, ep: i64) -> Value { self.db.lock().unwrap()["eps"][ep.to_string()].clone() }
    pub fn has(&self, ep: i64, which: &str) -> bool { self.dir.join(format!("{ep}_{which}.fp")).exists() }
    /// Store a fingerprint ("h" = head starting at 0, "t" = tail starting at `start` seconds).
    pub fn put(&self, ep: i64, which: &str, fp: &[u32], start: f64) {
        if fp.len() < (30.0 * FPS) as usize { return; }
        let _ = std::fs::write(self.dir.join(format!("{ep}_{which}.fp")), to_bytes(fp));
        if which == "t" { self.db.lock().unwrap()["eps"][ep.to_string()]["tailStart"] = json!(start); }
        self.save();
    }
    fn load(&self, ep: i64, which: &str) -> Option<Vec<u32>> { std::fs::read(self.dir.join(format!("{ep}_{which}.fp"))).ok().map(|b| from_bytes(&b)) }

    /// Re-run detection for every episode of a series that has fingerprints.
    pub fn analyze(&self, sid: i64) {
        let eps: Vec<(i64, i64, i64, f64)> = {
            let g = self.db.lock().unwrap();
            g["eps"].as_object().map(|m| m.iter().filter(|(_, v)| v["sid"].as_i64() == Some(sid))
                .filter_map(|(k, v)| Some((k.parse().ok()?, v["season"].as_i64().unwrap_or(0), v["n"].as_i64().unwrap_or(0), v["tailStart"].as_f64().unwrap_or(0.0)))).collect()).unwrap_or_default()
        };
        let mut found = 0;
        for &(ep, season, n, tail_start) in &eps {
            // compare with the nearest episodes (same season first)
            let mut others: Vec<&(i64, i64, i64, f64)> = eps.iter().filter(|o| o.0 != ep).collect();
            others.sort_by_key(|o| ((o.1 != season) as i64, (o.2 - n).abs()));
            let mut intro: Option<(f64, f64)> = None; let mut credits: Option<f64> = None;
            if let Some(h) = self.load(ep, "h") {
                let mut votes: Vec<(f64, f64)> = vec![];
                for o in others.iter().take(4) { if let Some(oh) = self.load(o.0, "h") {
                    if let Some(m) = best_match(&h, &oh, 12.0) { if m.1 - m.0 <= 150.0 { votes.push((m.0, m.1)); } }
                } }
                // the longest stretch seen at a consistent position
                votes.sort_by(|a, b| (b.1 - b.0).partial_cmp(&(a.1 - a.0)).unwrap());
                intro = votes.first().copied();
            }
            if let Some(t) = self.load(ep, "t") {
                let tail_len = t.len() as f64 / FPS;
                let mut best: Option<f64> = None;
                for o in others.iter().take(4) { if let Some(ot) = self.load(o.0, "t") {
                    // shared end-credits music running to (near) the end of what we captured
                    if let Some(m) = best_match(&t, &ot, 10.0) { if m.1 > tail_len - 25.0 { let st = tail_start + m.0; best = Some(best.map_or(st, |b: f64| b.min(st))); } }
                } }
                credits = best;
            }
            let mut g = self.db.lock().unwrap(); let e = &mut g["eps"][ep.to_string()];
            e["intro"] = intro.map(|(s, x)| json!([(s * 10.0).round() / 10.0, (x * 10.0).round() / 10.0])).unwrap_or(Value::Null);
            e["credits"] = credits.map(|c| json!((c * 10.0).round() / 10.0)).unwrap_or(Value::Null);
            if intro.is_some() { found += 1; }
        }
        self.save();
        println!("[intro] series {sid}: {} episodes fingerprinted, intro found in {found}", eps.len());
    }
}

/// Fingerprint the head/tail of a finished playback session's audio tap (no extra connection).
pub fn from_session(sk: &Skips, pcm: &Path, ep: i64, sid: i64, start: f64, dur: f64) {
    let Ok(meta) = std::fs::metadata(pcm) else { return };
    let secs = meta.len() as f64 / (2.0 * RATE as f64);
    let read = |from: f64, len: f64| -> Vec<i16> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(pcm) else { return vec![] };
        let off = ((from * RATE as f64) as u64) * 2; let n = ((len * RATE as f64) as usize) * 2;
        let mut b = vec![0u8; n]; if f.seek(SeekFrom::Start(off)).is_err() { return vec![]; }
        let got = f.read(&mut b).unwrap_or(0); let mut total = got;
        while total < n { match f.read(&mut b[total..]) { Ok(0) | Err(_) => break, Ok(k) => total += k } }
        b.truncate(total); b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    };
    let mut changed = false;
    if start < 2.0 && secs >= HEAD_SECS - 5.0 && !sk.has(ep, "h") { sk.put(ep, "h", &fingerprint(&read(0.0, HEAD_SECS)), 0.0); changed = true; }
    if dur > 600.0 && start + secs >= dur - 20.0 && !sk.has(ep, "t") {
        let from = (secs - TAIL_SECS).max(0.0);
        sk.put(ep, "t", &fingerprint(&read(from, TAIL_SECS)), start + from); changed = true;
    }
    let _ = std::fs::remove_file(pcm);
    if changed { sk.analyze(sid); }
}

/// Run ffmpeg to grab `len` seconds of audio from `url` at `from` (8 kHz mono). Aborts on cancel.
pub async fn grab(sk: &Skips, url: &str, ua: &str, from: f64, len: f64) -> Option<Vec<i16>> {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-user_agent", ua, "-reconnect", "1", "-reconnect_delay_max", "4"]);
    if from > 0.0 { cmd.args(["-ss", &format!("{from}")]); }
    cmd.args(["-t", &format!("{len}"), "-i", url, "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "pipe:1"]);
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)] { cmd.creation_flags(0x08000000); }
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = async { use tokio::io::AsyncReadExt; let mut v = vec![]; let _ = out.read_to_end(&mut v).await; v };
    let watch = async { loop { tokio::time::sleep(std::time::Duration::from_millis(200)).await; if sk.cancel.load(Ordering::SeqCst) { break; } } };
    let bytes = tokio::select! { v = reader => v, _ = watch => { let _ = child.start_kill(); return None; } };
    let _ = child.wait().await;
    if bytes.len() < (20.0 * RATE as f64) as usize * 2 { return None; }
    Some(bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect())
}

// ---------------------------------------------------------------- online skip segments (IntroDB.app + AniSkip)
use crate::app::App;
use crate::util::{enc, norm, now_ms};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const ONLINE_HIT_TTL_MS: i64 = 30 * 86_400_000;
const ONLINE_MISS_TTL_MS: i64 = 3 * 86_400_000;
const ONLINE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2500);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OnlineSkipEntry {
    pub t: i64,
    #[serde(default)]
    pub recap: Option<(f64, f64)>,
    #[serde(default)]
    pub intro: Option<(f64, f64)>,
    #[serde(default)]
    pub outro: Option<(f64, f64)>,
    #[serde(default)]
    pub src: String, // "introdb" | "aniskip" | "none"
}

impl OnlineSkipEntry {
    fn is_hit(&self) -> bool {
        self.src != "none" && (self.recap.is_some() || self.intro.is_some() || self.outro.is_some())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct MalIdEntry {
    pub t: i64,
    pub id: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct OnlineSkipDb {
    #[serde(default)]
    entries: HashMap<String, OnlineSkipEntry>,
    #[serde(default)]
    mal_ids: HashMap<String, MalIdEntry>,
}

/// Persistent disk-backed cache for IntroDB.app + AniSkip lookups (`data/skip-online-cache.json`).
pub struct OnlineSkips {
    file: PathBuf,
    db: Mutex<OnlineSkipDb>,
}

impl OnlineSkips {
    pub fn new(data: &Path) -> Self {
        let file = data.join("skip-online-cache.json");
        let db: OnlineSkipDb = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        OnlineSkips { file, db: Mutex::new(db) }
    }

    fn write_bytes(&self, bytes: Vec<u8>) {
        let tmp = self.file.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    fn get_entry(&self, key: &str, now: i64) -> Option<OnlineSkipEntry> {
        let g = self.db.lock().unwrap();
        let e = g.entries.get(key)?;
        let ttl = if e.is_hit() { ONLINE_HIT_TTL_MS } else { ONLINE_MISS_TTL_MS };
        if now - e.t < ttl { Some(e.clone()) } else { None }
    }

    fn put_entry(&self, key: String, entry: OnlineSkipEntry) {
        let bytes = {
            let mut g = self.db.lock().unwrap();
            g.entries.insert(key, entry);
            serde_json::to_vec(&*g).ok()
        };
        if let Some(b) = bytes { self.write_bytes(b); }
    }

    fn get_mal_id(&self, norm_title: &str, now: i64) -> Option<Option<i64>> {
        let g = self.db.lock().unwrap();
        let e = g.mal_ids.get(norm_title)?;
        let ttl = if e.id.is_some() { ONLINE_HIT_TTL_MS } else { ONLINE_MISS_TTL_MS };
        if now - e.t < ttl { Some(e.id) } else { None }
    }

    fn put_mal_id(&self, norm_title: String, id: Option<i64>, now: i64) {
        let bytes = {
            let mut g = self.db.lock().unwrap();
            g.mal_ids.insert(norm_title, MalIdEntry { t: now, id });
            serde_json::to_vec(&*g).ok()
        };
        if let Some(b) = bytes { self.write_bytes(b); }
    }
}

fn online_cache_key(kind: &str, imdb: &str, norm_title: &str, season: Option<i64>, ep: Option<i64>, is_anime: bool) -> Option<String> {
    match kind {
        "series" => {
            let e = ep.filter(|&x| x > 0)?;
            if !imdb.is_empty() {
                if let Some(s) = season.filter(|&x| x >= 0) {
                    return Some(format!("tv:{imdb}:s{s}e{e}"));
                }
            }
            if is_anime && !norm_title.is_empty() {
                let s = season.filter(|&x| x >= 0).unwrap_or(1);
                return Some(format!("an:{norm_title}:s{s}e{e}"));
            }
            None
        }
        "movie" if !imdb.is_empty() => Some(format!("movie:{imdb}")),
        _ => None,
    }
}

fn parse_introdb_seg(seg: &Value, allow_open_end: bool) -> Option<(f64, f64)> {
    if seg.is_null() { return None; }
    let obj = if let Some(arr) = seg.as_array() { arr.first()? } else { seg };
    if !obj.is_object() { return None; }
    let start = obj["start_sec"].as_f64()
        .or_else(|| obj["start_ms"].as_f64().map(|ms| ms / 1000.0))?;
    let end = obj["end_sec"].as_f64()
        .or_else(|| obj["end_ms"].as_f64().map(|ms| ms / 1000.0));
    if !start.is_finite() || start < 0.0 { return None; }
    match end {
        Some(e) if e.is_finite() && e > start => Some((start, e)),
        None if allow_open_end => Some((start, start)),
        _ => None,
    }
}

/// Parse IntroDB.app `/segments` payload into `(recap, intro, outro)`.
pub fn parse_introdb(v: &Value) -> (Option<(f64, f64)>, Option<(f64, f64)>, Option<(f64, f64)>) {
    (
        parse_introdb_seg(&v["recap"], false),
        parse_introdb_seg(&v["intro"], false),
        parse_introdb_seg(&v["outro"], true),
    )
}

/// Parse AniSkip v2 `/skip-times` payload into `(recap, intro, outro)`.
pub fn parse_aniskip(v: &Value) -> (Option<(f64, f64)>, Option<(f64, f64)>, Option<(f64, f64)>) {
    let mut recap: Option<(f64, f64)> = None;
    let mut intro: Option<(f64, f64)> = None;
    let mut outro: Option<(f64, f64)> = None;
    let mut mixed_intro: Option<(f64, f64)> = None;
    let mut mixed_outro: Option<(f64, f64)> = None;
    for r in v["results"].as_array().into_iter().flatten() {
        let iv = &r["interval"];
        let Some(s) = iv["startTime"].as_f64().or_else(|| iv["start_time"].as_f64()) else { continue };
        let Some(e) = iv["endTime"].as_f64().or_else(|| iv["end_time"].as_f64()) else { continue };
        if !s.is_finite() || !e.is_finite() || s < 0.0 || e <= s { continue; }
        match r["skipType"].as_str().unwrap_or("") {
            "op" if intro.is_none() => intro = Some((s, e)),
            "ed" if outro.is_none() => outro = Some((s, e)),
            "recap" if recap.is_none() => recap = Some((s, e)),
            "mixed-op" if mixed_intro.is_none() => mixed_intro = Some((s, e)),
            "mixed-ed" if mixed_outro.is_none() => mixed_outro = Some((s, e)),
            _ => {}
        }
    }
    (recap, intro.or(mixed_intro), outro.or(mixed_outro))
}

/// Pick the best-matching MAL ID from a Jikan `/v4/anime?q=...` search response.
pub fn pick_jikan_mal_id(v: &Value, title: &str) -> Option<i64> {
    let want = norm(title);
    if want.is_empty() { return None; }
    let want_words: Vec<&str> = want.split_whitespace().collect();
    let mut best: Option<(i32, i64)> = None;
    for d in v["data"].as_array().into_iter().flatten() {
        let Some(mal_id) = d["mal_id"].as_i64().filter(|&id| id > 0) else { continue };
        let mut cands: Vec<&str> = vec![];
        if let Some(t) = d["title"].as_str() { cands.push(t); }
        if let Some(t) = d["title_english"].as_str() { cands.push(t); }
        for t in d["titles"].as_array().into_iter().flatten() {
            if let Some(s) = t["title"].as_str() { cands.push(s); }
        }
        for s in d["title_synonyms"].as_array().into_iter().flatten() {
            if let Some(s) = s.as_str() { cands.push(s); }
        }
        let mut text_score = 0i32;
        for raw in cands {
            let c = norm(raw);
            if c.is_empty() { continue; }
            let sc = if c == want {
                100
            } else if c.starts_with(&want) || want.starts_with(&c) {
                80
            } else {
                let cw: Vec<&str> = c.split_whitespace().collect();
                if cw.is_empty() || want_words.is_empty() {
                    0
                } else {
                    let common = want_words.iter().filter(|w| cw.contains(w)).count();
                    ((2.0 * common as f64 / (want_words.len() + cw.len()) as f64) * 70.0).round() as i32
                }
            };
            if sc > text_score { text_score = sc; }
        }
        if text_score < 40 { continue; }
        let bonus = match d["type"].as_str().unwrap_or("") { "TV" => 5, "ONA" => 3, _ => 0 };
        let total = text_score + bonus;
        if best.map_or(true, |(bs, _)| total > bs) {
            best = Some((total, mal_id));
        }
    }
    best.map(|(_, id)| id)
}

/// Drop out-of-bounds or implausible segments against `dur`.
pub fn clamp_online_entry(entry: &mut OnlineSkipEntry, dur: f64) {
    let check_head = |seg: Option<(f64, f64)>| -> Option<(f64, f64)> {
        let (s, e) = seg?;
        if !s.is_finite() || !e.is_finite() || s < 0.0 || e <= s { return None; }
        let e = if dur > 30.0 { e.min(dur) } else { e };
        if e - s < 4.0 || e - s > 300.0 { return None; }
        if dur > 30.0 && s >= dur * 0.5 { return None; }
        Some((s, e))
    };
    entry.recap = check_head(entry.recap);
    entry.intro = check_head(entry.intro);
    entry.outro = entry.outro.and_then(|(s, e)| {
        if !s.is_finite() || s < 0.0 { return None; }
        if dur > 30.0 && (s < dur * 0.5 || s >= dur - 3.0) { return None; }
        let e = if e.is_finite() && e >= s {
            if dur > 30.0 { e.min(dur) } else { e }
        } else if dur > 30.0 {
            dur
        } else {
            s
        };
        Some((s, e))
    });
    if entry.recap.is_none() && entry.intro.is_none() && entry.outro.is_none() {
        entry.src = "none".into();
    }
}

/// Look up online skip segments (IntroDB.app, then AniSkip fallback for anime) with disk caching.
pub async fn lookup_online(
    app: &App,
    kind: &str,
    imdb: &str,
    title: &str,
    season: Option<i64>,
    ep: Option<i64>,
    dur: f64,
    is_anime: bool,
) -> Option<OnlineSkipEntry> {
    let imdb = imdb.trim();
    let norm_title = norm(title);
    let key = online_cache_key(kind, imdb, &norm_title, season, ep, is_anime)?;
    let now = now_ms();

    if let Some(mut cached) = app.online_skips.get_entry(&key, now) {
        clamp_online_entry(&mut cached, dur);
        return Some(cached);
    }

    let mut entry = OnlineSkipEntry {
        t: now,
        recap: None,
        intro: None,
        outro: None,
        src: "none".into(),
    };

    // Source 1: IntroDB.app (TV & Movies with IMDb ID)
    if !imdb.is_empty() {
        let url = if kind == "series" {
            match (season, ep) {
                (Some(s), Some(e)) => Some(format!(
                    "https://api.introdb.app/segments?imdb_id={}&season={s}&episode={e}",
                    enc(imdb)
                )),
                _ => None,
            }
        } else if kind == "movie" {
            Some(format!("https://api.introdb.app/segments?imdb_id={}", enc(imdb)))
        } else {
            None
        };
        if let Some(url) = url {
            if let Ok(resp) = app.http.get(&url)
                .header(reqwest::header::USER_AGENT, "TV/1.0")
                .timeout(ONLINE_TIMEOUT)
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(v) = resp.json::<Value>().await {
                        let (recap, intro, outro) = parse_introdb(&v);
                        entry.recap = recap;
                        entry.intro = intro;
                        entry.outro = outro;
                        if entry.recap.is_some() || entry.intro.is_some() || entry.outro.is_some() {
                            entry.src = "introdb".into();
                        }
                        clamp_online_entry(&mut entry, dur);
                    }
                }
            }
        }
    }

    // Source 2: AniSkip (Anime fallback when intro & outro are still missing)
    if kind == "series" && is_anime && entry.intro.is_none() && entry.outro.is_none() {
        if let (Some(ep_num), false) = (ep.filter(|&e| e > 0), norm_title.is_empty()) {
            let mal_id = match app.online_skips.get_mal_id(&norm_title, now) {
                Some(cached_id) => cached_id,
                None => {
                    let jikan_url = format!("https://api.jikan.moe/v4/anime?q={}&limit=3", enc(title.trim()));
                    let mut resolved: Option<Option<i64>> = None;
                    if let Ok(resp) = app.http.get(&jikan_url)
                        .header(reqwest::header::USER_AGENT, "TV/1.0")
                        .timeout(ONLINE_TIMEOUT)
                        .send()
                        .await
                    {
                        if resp.status().is_success() {
                            if let Ok(v) = resp.json::<Value>().await {
                                resolved = Some(pick_jikan_mal_id(&v, title));
                            }
                        }
                    }
                    if let Some(id_opt) = resolved {
                        app.online_skips.put_mal_id(norm_title.clone(), id_opt, now_ms());
                        id_opt
                    } else {
                        None
                    }
                }
            };

            if let Some(mal_id) = mal_id {
                let ep_len = if dur > 0.0 { dur.round() as i64 } else { 0 };
                let aniskip_url = format!(
                    "https://api.aniskip.com/v2/skip-times/{mal_id}/{ep_num}?types[]=op&types[]=ed&types[]=recap&episodeLength={ep_len}"
                );
                if let Ok(resp) = app.http.get(&aniskip_url)
                    .header(reqwest::header::USER_AGENT, "TV/1.0")
                    .timeout(ONLINE_TIMEOUT)
                    .send()
                    .await
                {
                    if resp.status().is_success() {
                        if let Ok(v) = resp.json::<Value>().await {
                            let (recap, intro, outro) = parse_aniskip(&v);
                            if entry.recap.is_none() { entry.recap = recap; }
                            entry.intro = intro;
                            entry.outro = outro;
                            if entry.recap.is_some() || entry.intro.is_some() || entry.outro.is_some() {
                                entry.src = "aniskip".into();
                            }
                        }
                    }
                }
            }
        }
    }

    clamp_online_entry(&mut entry, dur);
    entry.t = now_ms();
    app.online_skips.put_entry(key, entry.clone());
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_introdb_sec_and_ms_fallback() {
        let v_sec = json!({
            "recap": { "start_sec": 0.0, "end_sec": 38.5 },
            "intro": { "start_sec": 120.2, "end_sec": 205.8 },
            "outro": { "start_sec": 2510.0, "end_sec": 2640.0 }
        });
        let (recap, intro, outro) = parse_introdb(&v_sec);
        assert_eq!(recap, Some((0.0, 38.5)));
        assert_eq!(intro, Some((120.2, 205.8)));
        assert_eq!(outro, Some((2510.0, 2640.0)));

        let v_ms = json!({
            "recap": null,
            "intro": { "start_ms": 15500, "end_ms": 105500 },
            "outro": { "start_ms": 1320000, "end_ms": null }
        });
        let (recap2, intro2, outro2) = parse_introdb(&v_ms);
        assert_eq!(recap2, None);
        assert_eq!(intro2, Some((15.5, 105.5)));
        assert_eq!(outro2, Some((1320.0, 1320.0)));
    }

    #[test]
    fn parse_aniskip_op_ed_recap() {
        let v = json!({
            "found": true,
            "results": [
                { "skipType": "recap", "interval": { "startTime": 0.0, "endTime": 52.0 } },
                { "skipType": "op", "interval": { "startTime": 85.5, "endTime": 175.5 } },
                { "skipType": "ed", "interval": { "startTime": 1310.0, "endTime": 1400.0 } }
            ]
        });
        let (recap, intro, outro) = parse_aniskip(&v);
        assert_eq!(recap, Some((0.0, 52.0)));
        assert_eq!(intro, Some((85.5, 175.5)));
        assert_eq!(outro, Some((1310.0, 1400.0)));
    }

    #[test]
    fn clamp_online_entry_drops_implausible_segments() {
        let mut e = OnlineSkipEntry {
            t: 1,
            recap: Some((0.0, 3.0)),      // too short (< 4s) -> dropped
            intro: Some((800.0, 890.0)),  // past dur*0.5 -> dropped
            outro: Some((600.0, 700.0)),  // before dur*0.5 -> dropped
            src: "introdb".into(),
        };
        clamp_online_entry(&mut e, 1440.0);
        assert_eq!(e.recap, None);
        assert_eq!(e.intro, None);
        assert_eq!(e.outro, None);
        assert_eq!(e.src, "none");

        let mut e2 = OnlineSkipEntry {
            t: 1,
            recap: Some((0.0, 350.0)),      // too long (> 300s) -> dropped
            intro: Some((85.0, 175.0)),     // valid -> kept
            outro: Some((1438.0, 1440.0)),  // too close to end (>= dur-3) -> dropped
            src: "aniskip".into(),
        };
        clamp_online_entry(&mut e2, 1440.0);
        assert_eq!(e2.recap, None);
        assert_eq!(e2.intro, Some((85.0, 175.0)));
        assert_eq!(e2.outro, None);
        assert_eq!(e2.src, "aniskip");

        let mut e3 = OnlineSkipEntry {
            t: 1,
            recap: Some((0.0, 45.0)),
            intro: Some((90.0, 180.0)),
            outro: Some((1315.0, 1405.0)),
            src: "introdb".into(),
        };
        clamp_online_entry(&mut e3, 1440.0);
        assert_eq!(e3.recap, Some((0.0, 45.0)));
        assert_eq!(e3.intro, Some((90.0, 180.0)));
        assert_eq!(e3.outro, Some((1315.0, 1405.0)));
        assert_eq!(e3.src, "introdb");
    }

    #[test]
    fn jikan_matching_and_cache_keys() {
        let jikan = json!({
            "data": [
                { "mal_id": 999, "type": "Movie", "title": "Jujutsu Kaisen 0", "title_english": "Jujutsu Kaisen 0" },
                { "mal_id": 40748, "type": "TV", "title": "Jujutsu Kaisen", "title_english": "Jujutsu Kaisen" }
            ]
        });
        assert_eq!(pick_jikan_mal_id(&jikan, "Jujutsu Kaisen"), Some(40748));

        assert_eq!(
            online_cache_key("series", "tt0903747", "breaking bad", Some(1), Some(2), false),
            Some("tv:tt0903747:s1e2".into())
        );
        assert_eq!(
            online_cache_key("movie", "tt0133093", "the matrix", None, None, false),
            Some("movie:tt0133093".into())
        );
        assert_eq!(
            online_cache_key("series", "", "jujutsu kaisen", Some(1), Some(3), true),
            Some("an:jujutsu kaisen:s1e3".into())
        );
        assert_eq!(online_cache_key("series", "", "unknown", Some(1), Some(1), false), None);
    }
}
