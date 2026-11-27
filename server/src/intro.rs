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

pub const HEAD_SECS: f64 = 420.0; // 7 min: catches late anime cold-open OPs (e.g. JJK, 7DS)
pub const TAIL_SECS: f64 = 360.0; // 6 min: catches anime EDs followed by 2-3.5 min C-part epilogues

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
    pub fn count_matched(&self) -> usize {
        self.db.lock().unwrap()["eps"].as_object().map_or(0, |m| {
            m.values().filter(|v| v["intro"].is_array() || v["credits"].is_number()).count()
        })
    }
    pub fn has(&self, ep: i64, which: &str) -> bool { self.dir.join(format!("{ep}_{which}.fp")).exists() }
    pub fn fp_secs(&self, ep: i64, which: &str) -> f64 {
        std::fs::metadata(self.dir.join(format!("{ep}_{which}.fp")))
            .map(|m| (m.len() / 4) as f64 / FPS)
            .unwrap_or(0.0)
    }
    /// Store a fingerprint ("h" = head starting at `start`, "t" = tail starting at `start` seconds).
    pub fn put(&self, ep: i64, which: &str, fp: &[u32], start: f64) {
        if fp.len() < (30.0 * FPS) as usize { return; }
        let _ = std::fs::write(self.dir.join(format!("{ep}_{which}.fp")), to_bytes(fp));
        let mut g = self.db.lock().unwrap();
        let e = &mut g["eps"][ep.to_string()];
        if e.is_null() { *e = json!({}); }
        if which == "h" { e["headStart"] = json!(start); }
        if which == "t" { e["tailStart"] = json!(start); }
        drop(g);
        self.save();
    }
    fn load(&self, ep: i64, which: &str) -> Option<Vec<u32>> { std::fs::read(self.dir.join(format!("{ep}_{which}.fp"))).ok().map(|b| from_bytes(&b)) }

    /// Re-run detection for every episode of a series that has fingerprints.
    /// Supports both arbitrary `headStart` offsets AND mid-tail anime EDs with C-part epilogues,
    /// storing exact `[intro_s, intro_e]`, `credits`, and `creditsEnd`!
    pub fn analyze(&self, sid: i64) {
        let eps: Vec<(i64, i64, i64, f64, f64, f64)> = {
            let g = self.db.lock().unwrap();
            g["eps"].as_object().map(|m| m.iter().filter(|(_, v)| v["sid"].as_i64() == Some(sid))
                .filter_map(|(k, v)| Some((
                    k.parse().ok()?,
                    v["season"].as_i64().unwrap_or(0),
                    v["n"].as_i64().unwrap_or(0),
                    v["headStart"].as_f64().unwrap_or(0.0),
                    v["tailStart"].as_f64().unwrap_or(0.0),
                    v["dur"].as_f64().unwrap_or(0.0),
                ))).collect()).unwrap_or_default()
        };
        let mut found = 0;
        for &(ep, season, n, head_start, tail_start, dur) in &eps {
            // compare with the nearest episodes (same season first)
            let mut others: Vec<&(i64, i64, i64, f64, f64, f64)> = eps.iter().filter(|o| o.0 != ep).collect();
            others.sort_by_key(|o| ((o.1 != season) as i64, (o.2 - n).abs()));
            let mut intro: Option<(f64, f64)> = None;
            let mut credits: Option<(f64, f64)> = None;
            if let Some(h) = self.load(ep, "h") {
                let mut votes: Vec<(f64, f64)> = vec![];
                for o in others.iter().take(5) {
                    if let Some(oh) = self.load(o.0, "h") {
                        if let Some(m) = best_match(&h, &oh, 12.0) {
                            let len = m.1 - m.0;
                            if len <= 150.0 {
                                votes.push((head_start + m.0, head_start + m.1));
                            }
                        }
                    }
                }
                votes.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
                intro = votes.first().copied();
            }
            if let Some(t) = self.load(ep, "t") {
                let tail_len = t.len() as f64 / FPS;
                let mut tail_votes: Vec<(f64, f64)> = vec![];
                for o in others.iter().take(5) {
                    if let Some(ot) = self.load(o.0, "t") {
                        if let Some(m) = best_match(&t, &ot, 10.0) {
                            let len = m.1 - m.0;
                            if len > 165.0 { continue; }
                            let st = tail_start + m.0;
                            let en = tail_start + m.1;
                            let near_end = m.1 > tail_len - 30.0 || (dur > 120.0 && dur - en < 30.0);
                            // Accept EITHER a full theme song (>= 45s, e.g. 90s anime ED before a 2-3 min C-part!)
                            // OR a shorter >= 10s end-credits sting running to near EOF.
                            if len >= 45.0 || near_end {
                                tail_votes.push((st, en));
                            }
                        }
                    }
                }
                tail_votes.sort_by(|a, b| (b.1 - b.0).total_cmp(&(a.1 - a.0)));
                credits = tail_votes.first().copied();
            }
            let mut g = self.db.lock().unwrap(); let e = &mut g["eps"][ep.to_string()];
            e["intro"] = intro.map(|(s, x)| json!([(s * 10.0).round() / 10.0, (x * 10.0).round() / 10.0])).unwrap_or(Value::Null);
            e["credits"] = credits.map(|(s, _)| json!((s * 10.0).round() / 10.0)).unwrap_or(Value::Null);
            e["creditsEnd"] = credits.map(|(_, x)| json!((x * 10.0).round() / 10.0)).unwrap_or(Value::Null);
            if intro.is_some() { found += 1; }
        }
        self.save();
        println!("[intro] series {sid}: {} episodes fingerprinted, intro found in {found}", eps.len());
    }
}

/// Fingerprint the head/tail of a finished playback session's audio tap (no extra connection).
/// Captures partial head/tail clips even when the viewer skipped an OP/ED or sought mid-episode,
/// upgrading stored fingerprints whenever a longer span becomes available.
pub fn from_session(sk: &Skips, pcm: &Path, ep: i64, sid: i64, start: f64, dur: f64) {
    let Ok(meta) = std::fs::metadata(pcm) else { return };
    let secs = meta.len() as f64 / (2.0 * RATE as f64);
    let read = |from: f64, len: f64| -> Vec<i16> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(pcm) else { return vec![] };
        let off = ((from.max(0.0) * RATE as f64) as u64) * 2;
        let n = ((len.max(0.0) * RATE as f64) as usize) * 2;
        if n == 0 { return vec![]; }
        let mut b = vec![0u8; n]; if f.seek(SeekFrom::Start(off)).is_err() { return vec![]; }
        let got = f.read(&mut b).unwrap_or(0); let mut total = got;
        while total < n { match f.read(&mut b[total..]) { Ok(0) | Err(_) => break, Ok(k) => total += k } }
        b.truncate(total); b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    };
    let mut changed = false;
    // Head window: capture any span overlapping [0, HEAD_SECS] that is >= 95s (and longer than any existing clip)
    if start < HEAD_SECS - 95.0 {
        let head_end = (start + secs).min(HEAD_SECS);
        let head_len = head_end - start;
        if head_len >= 95.0 && head_len > sk.fp_secs(ep, "h") + 15.0 {
            sk.put(ep, "h", &fingerprint(&read(0.0, head_len)), start);
            changed = true;
        }
    }
    // Tail window: capture any span overlapping [dur - TAIL_SECS, dur] that is >= 95s and reaches within 240s of EOF
    if dur > 600.0 {
        let tail_win_s = (dur - TAIL_SECS).max(0.0);
        let span_s = start.max(tail_win_s);
        let span_e = (start + secs).min(dur);
        let span_len = span_e - span_s;
        if span_len >= 95.0 && span_e >= dur - 240.0 && span_len > sk.fp_secs(ep, "t") + 15.0 {
            let from_pcm = (span_s - start).max(0.0);
            sk.put(ep, "t", &fingerprint(&read(from_pcm, span_len)), span_s);
            changed = true;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_theme(secs: f64, seed: u32) -> Vec<i16> {
        let n = (secs * RATE as f64) as usize;
        let mut out = Vec::with_capacity(n);
        let mut s = seed.wrapping_add(1);
        for i in 0..n {
            let t = i as f64 / RATE as f64;
            // Changing melodic chord progression with rich harmonic content across 300-3000 Hz
            let beat = (t * 2.5).floor();
            let f1 = 330.0 + ((beat * 73.0) % 440.0);
            let f2 = f1 * 1.498;
            let f3 = 880.0 + ((beat * 131.0) % 1200.0);
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let nse = (((s >> 16) as i16) as f64 / 32768.0) * 0.12;
            let v = 0.35 * (2.0 * std::f64::consts::PI * f1 * t).sin()
                + 0.28 * (2.0 * std::f64::consts::PI * f2 * t).sin()
                + 0.22 * (2.0 * std::f64::consts::PI * f3 * t).sin()
                + nse;
            out.push((v.clamp(-0.95, 0.95) * 28000.0) as i16);
        }
        out
    }

    fn synth_noise(secs: f64, seed: u32) -> Vec<i16> {
        let n = (secs * RATE as f64) as usize;
        let mut out = Vec::with_capacity(n);
        let mut s = seed.wrapping_add(7);
        for _ in 0..n {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            out.push((((s >> 16) as i16) / 3) as i16);
        }
        out
    }

    #[test]
    fn detects_cpart_anime_ed_and_credits_end_with_head_offset() {
        let tmp = std::env::temp_dir().join(format!("tv_intro_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let sk = Skips::new(&tmp);
        let ed = synth_theme(52.0, 42);
        // Ep 1 tail [1110..1470]: 100s dialogue + 52s ED [1210..1262] + 108s C-part story!
        let mut t1 = synth_noise(100.0, 1);
        t1.extend_from_slice(&ed);
        t1.extend_from_slice(&synth_noise(108.0, 2));
        // Ep 2 tail [1110..1470]: 70s dialogue + 52s ED [1180..1232] + 138s C-part story!
        let mut t2 = synth_noise(70.0, 3);
        t2.extend_from_slice(&ed);
        t2.extend_from_slice(&synth_noise(138.0, 4));

        sk.register(101, 999, 1, 1, 1470.0);
        sk.register(102, 999, 1, 2, 1470.0);
        sk.put(101, "t", &fingerprint(&t1), 1110.0);
        sk.put(102, "t", &fingerprint(&t2), 1110.0);
        sk.analyze(999);

        let e1 = sk.entry(101);
        let c1 = e1["credits"].as_f64().expect("ep1 credits");
        let ce1 = e1["creditsEnd"].as_f64().expect("ep1 creditsEnd");
        assert!((c1 - 1210.0).abs() < 3.0, "expected ep1 ED start ~1210, got {c1}");
        assert!((ce1 - 1262.0).abs() < 3.0, "expected ep1 ED end ~1262, got {ce1}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
