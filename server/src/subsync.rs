// Automatic subtitle synchronisation.
// Speech detection: WebRTC VAD (same detector ffsubsync/alass use) on a low-rate mono copy of the
// audio that ffmpeg writes during playback. Alignment: alass-core (handles constant offsets AND
// splits, e.g. recaps or different cuts), plus frame-rate correction (23.976 <-> 25 fps).
// Self-check: a correction is only reported as "improves" if the subtitles overlap detected speech
// better after it than before; the player re-runs this as more audio arrives.
use alass_core::{align, align_nosplit, standard_scoring, NoProgressHandler, TimeDelta, TimePoint, TimeSpan};
use serde_json::{json, Value};
use std::path::Path;

const RATE: usize = 8000;
const FRAME: usize = 240; // 30 ms at 8 kHz

pub fn speech_spans(pcm: &[i16]) -> Vec<(i64, i64)> {
    let mut vad = webrtc_vad::Vad::new_with_rate_and_mode(webrtc_vad::SampleRate::Rate8kHz, webrtc_vad::VadMode::Aggressive);
    let mut spans: Vec<(i64, i64)> = vec![];
    let mut cur: Option<i64> = None;
    for (i, f) in pcm.chunks_exact(FRAME).enumerate() {
        let t = (i * FRAME * 1000 / RATE) as i64;
        let voiced = vad.is_voice_segment(f).unwrap_or(false);
        match (voiced, cur) {
            (true, None) => cur = Some(t),
            (false, Some(s)) => { spans.push((s, t)); cur = None; }
            _ => {}
        }
    }
    if let Some(s) = cur { spans.push((s, (pcm.len() * 1000 / RATE) as i64)); }
    // merge short gaps, drop blips
    let mut merged: Vec<(i64, i64)> = vec![];
    for sp in spans {
        if let Some(last) = merged.last_mut() { if sp.0 - last.1 < 250 { last.1 = sp.1; continue; } }
        merged.push(sp);
    }
    merged.retain(|s| s.1 - s.0 >= 150);
    merged
}

pub fn parse_vtt(text: &str) -> Vec<(i64, i64)> {
    let ts = |s: &str| -> Option<i64> {
        let s = s.trim().split_whitespace().next()?;
        let p: Vec<&str> = s.split(':').collect();
        let (h, m, sec) = match p.len() { 3 => (p[0].parse::<f64>().ok()?, p[1].parse::<f64>().ok()?, p[2].parse::<f64>().ok()?), 2 => (0.0, p[0].parse::<f64>().ok()?, p[1].parse::<f64>().ok()?), _ => return None };
        Some(((h * 3600.0 + m * 60.0 + sec) * 1000.0).round() as i64)
    };
    text.lines().filter_map(|l| { let (a, b) = l.split_once("-->")?; Some((ts(a)?, ts(b)?)) }).filter(|(a, b)| b > a).collect()
}

/// Fraction of subtitle time that overlaps detected speech.
fn overlap(cues: &[(i64, i64)], speech: &[(i64, i64)]) -> f64 {
    let total: i64 = cues.iter().map(|c| c.1 - c.0).sum();
    if total == 0 { return 0.0; }
    let mut hit = 0i64; let mut j0 = 0usize;
    let mut sorted: Vec<(i64, i64)> = cues.to_vec(); sorted.sort();
    for c in &sorted {
        while j0 < speech.len() && speech[j0].1 <= c.0 { j0 += 1; }
        let mut j = j0;
        while j < speech.len() && speech[j].0 < c.1 { hit += (c.1.min(speech[j].1) - c.0.max(speech[j].0)).max(0); j += 1; }
    }
    hit as f64 / total as f64
}

fn scale(v: &[(i64, i64)], r: f64) -> Vec<(i64, i64)> { v.iter().map(|&(a, b)| ((a as f64 * r) as i64, (b as f64 * r) as i64)).collect() }
fn shift(v: &[(i64, i64)], d: i64) -> Vec<(i64, i64)> { v.iter().map(|&(a, b)| (a + d, b + d)).collect() }

/// Subtitles often appear early and linger after speech; viewers notice the START. After alass,
/// nudge everything (±1.5 s) so cue starts land on speech onsets.
fn refine_onsets(cues: &[(i64, i64)], deltas: &[i64], speech: &[(i64, i64)]) -> i64 {
    let onsets: Vec<i64> = speech.iter().map(|s| s.0).collect();
    if onsets.is_empty() { return 0; }
    let nearest = |t: i64| -> i64 { let i = onsets.partition_point(|&o| o < t); let mut d = i64::MAX; if i < onsets.len() { d = d.min((onsets[i] - t).abs()); } if i > 0 { d = d.min((t - onsets[i - 1]).abs()); } d };
    let score = |extra: i64| -> f64 { cues.iter().zip(deltas).map(|(c, d)| { let x = nearest(c.0 + d + extra) as f64 / 250.0; (-x * x).exp() }).sum() };
    let base = score(0);
    let mut best = (0i64, base);
    let mut e = -1500;
    while e <= 1500 { let sc = score(e); if sc > best.1 { best = (e, sc); } e += 20; }
    // require a real gain so we don't jitter on already-good subtitles
    if best.1 > base * 1.05 { best.0 } else { 0 }
}

const MIN_SEG: usize = 8;
/// Merge runs of equal offset shorter than MIN_SEG lines into the neighbouring longer run.
fn smooth_segments(mut d: Vec<i64>) -> Vec<i64> {
    for _ in 0..10 {
        let mut segs: Vec<(usize, usize)> = vec![]; // [start, end)
        let mut i = 0;
        while i < d.len() { let mut j = i + 1; while j < d.len() && d[j] == d[i] { j += 1; } segs.push((i, j)); i = j; }
        if segs.len() <= 1 { break; }
        let Some(k) = (0..segs.len()).filter(|&k| segs[k].1 - segs[k].0 < MIN_SEG).min_by_key(|&k| segs[k].1 - segs[k].0) else { break };
        let len = |k: usize| segs[k].1 - segs[k].0;
        let src = match (k.checked_sub(1), (k + 1 < segs.len()).then_some(k + 1)) {
            (Some(p), Some(n)) => if len(p) >= len(n) { p } else { n }, (Some(p), None) => p, (None, Some(n)) => n, _ => break };
        let v = d[segs[src].0];
        for x in segs[k].0..segs[k].1 { d[x] = v; }
    }
    d
}

fn spans(v: &[(i64, i64)]) -> Vec<TimeSpan> { v.iter().map(|&(a, b)| TimeSpan::new(TimePoint::from(a), TimePoint::from(b.max(a + 1)))).collect() }

pub fn sync(pcm_path: &Path, vtt_path: &Path) -> Value { sync_at(pcm_path, vtt_path, 0) }
/// `base_ms`: session start in film time (subtitles carry film time; the audio tap starts at the session start).
pub fn sync_at(pcm_path: &Path, vtt_path: &Path, base_ms: i64) -> Value {
    let Ok(bytes) = std::fs::read(pcm_path) else { return json!({ "ready": false, "reason": "no audio yet" }) };
    let pcm: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let dur = (pcm.len() * 1000 / RATE) as i64;
    let speech = speech_spans(&pcm);
    let speech_ms: i64 = speech.iter().map(|s| s.1 - s.0).sum();
    let all_cues: Vec<(i64, i64)> = parse_vtt(&std::fs::read_to_string(vtt_path).unwrap_or_default()).into_iter().map(|(a, b)| (a - base_ms, b - base_ms)).filter(|c| c.1 > 0).collect();
    // only cues inside the audio we've heard so far (minus a margin)
    let cues: Vec<(i64, i64)> = all_cues.iter().copied().filter(|c| c.1 < dur - 3000).collect();
    if speech_ms < 15_000 || cues.len() < 8 {
        return json!({ "ready": false, "reason": "need more dialogue", "speechSecs": speech_ms / 1000, "cues": cues.len(), "heardSecs": dur / 1000 });
    }
    let reference = spans(&speech);
    // Candidate frame rates: 1.0 plus the best alternative by constant-shift fit.
    let ratios = [25.0 / 23.976, 23.976 / 25.0, 25.0 / 24.0, 24.0 / 25.0];
    let mut alt = (1.0f64, -1.0f64);
    for r in ratios {
        let sc = scale(&cues, r);
        let (d, _) = align_nosplit(&reference, &spans(&sc), standard_scoring, NoProgressHandler);
        let ov = overlap(&shift(&sc, d.as_i64()), &speech);
        if ov > alt.1 { alt = (r, ov); }
    }
    // Full split-aware alignment (alass recommended: split penalty 7, speed optimisation 3) + onset refinement.
    let solve = |r: f64| {
        let sc = scale(&cues, r);
        // Few lines heard: one constant offset (splits would overfit). Otherwise split-aware, then
        // merge any split shorter than MIN_SEG lines (real splits = recaps/different cuts, long runs).
        let (mut deltas, score): (Vec<i64>, f64) = if sc.len() < 30 {
            let (d, score) = align_nosplit(&reference, &spans(&sc), standard_scoring, NoProgressHandler);
            (vec![d.as_i64(); sc.len()], score)
        } else {
            let (d, score) = align(&reference, &spans(&sc), 7.0, Some(3.0), standard_scoring, NoProgressHandler);
            (smooth_segments(d.iter().map(TimeDelta::as_i64).collect()), score)
        };
        let extra = refine_onsets(&sc, &deltas, &speech);
        for x in deltas.iter_mut() { *x += extra; }
        let corrected: Vec<(i64, i64)> = sc.iter().zip(&deltas).map(|(&(a, b), d)| (a + d, b + d)).collect();
        (r, deltas, score, overlap(&corrected, &speech))
    };
    let base = solve(1.0);
    // Only change frame rate when it clearly wins after full alignment.
    let (ratio, deltas, score, after) = if alt.0 != 1.0 { let o = solve(alt.0); if o.3 > base.3 + 0.03 { o } else { base } } else { base };
    let before = overlap(&cues, &speech);
    let max_shift = deltas.iter().map(|d| d.abs()).max().unwrap_or(0);
    let mut sorted = deltas.clone(); sorted.sort();
    let median = sorted[sorted.len() / 2];
    let tail: Vec<i64> = { let mut t: Vec<i64> = deltas.iter().rev().take(10).copied().collect(); t.sort(); t };
    let plausible = deltas.iter().all(|d| d.abs() < 120_000);
    json!({ "ready": true, "ratio": ratio, "deltas": deltas, "analyzed": cues.len(), "offset": median, "tailOffset": tail[tail.len() / 2],
        "score": score, "overlapBefore": before, "overlapAfter": after,
        // self-check: only apply when it measurably improves speech overlap
        "improves": plausible && after >= before + 0.03 && (max_shift >= 150 || ratio != 1.0), "heardSecs": dur / 1000, "speechSecs": speech_ms / 1000 })
}
