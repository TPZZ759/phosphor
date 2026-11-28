// Automatic subtitle synchronisation.
// Speech detection: WebRTC VAD (same detector ffsubsync/alass use) on a low-rate mono copy of the
// audio that ffmpeg writes during playback. Alignment: alass-core (handles constant offsets AND
// splits, e.g. recaps or different cuts), plus frame-rate correction (23.976 <-> 25 fps).
// Self-check: a correction is only reported as "improves" if the subtitles overlap detected speech
// better after it than before; the player re-runs this as more audio arrives.
use alass_core::{align, align_nosplit, standard_scoring, NoProgressHandler, TimeDelta, TimePoint, TimeSpan};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const RATE: usize = 8000;
const FRAME: usize = 240; // 30 ms at 8 kHz
const WINDOW_PAD_MS: i64 = 180_000; // ±180 s window so cues shifted by a missing recap/intro are never pruned

struct VadCacheEntry {
    pcm_len: usize, // number of i16 samples already processed (multiple of FRAME)
    prefix_hash: u64,
    raw_spans: Vec<(i64, i64)>,
    cur_start: Option<i64>,
}

static VAD_CACHE: OnceLock<Mutex<HashMap<PathBuf, VadCacheEntry>>> = OnceLock::new();

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn finalize_vad_spans(raw: &[(i64, i64)], cur: Option<i64>, total_samples: usize) -> Vec<(i64, i64)> {
    let mut spans = raw.to_vec();
    if let Some(s) = cur {
        spans.push((s, (total_samples * 1000 / RATE) as i64));
    }
    // merge short gaps, drop blips
    let mut merged: Vec<(i64, i64)> = vec![];
    for sp in spans {
        if let Some(last) = merged.last_mut() {
            if sp.0 - last.1 < 250 {
                last.1 = sp.1;
                continue;
            }
        }
        merged.push(sp);
    }
    merged.retain(|s| s.1 - s.0 >= 150);
    merged
}

fn run_vad_slice(
    pcm: &[i16],
    sample_offset: usize,
    raw_spans: &mut Vec<(i64, i64)>,
    cur: &mut Option<i64>,
) {
    let mut vad = webrtc_vad::Vad::new_with_rate_and_mode(
        webrtc_vad::SampleRate::Rate8kHz,
        webrtc_vad::VadMode::Aggressive,
    );
    for (i, f) in pcm.chunks_exact(FRAME).enumerate() {
        let t = ((sample_offset + i * FRAME) * 1000 / RATE) as i64;
        let voiced = vad.is_voice_segment(f).unwrap_or(false);
        match (voiced, *cur) {
            (true, None) => *cur = Some(t),
            (false, Some(s)) => {
                raw_spans.push((s, t));
                *cur = None;
            }
            _ => {}
        }
    }
}

pub fn speech_spans(pcm: &[i16]) -> Vec<(i64, i64)> {
    let mut raw: Vec<(i64, i64)> = vec![];
    let mut cur: Option<i64> = None;
    run_vad_slice(pcm, 0, &mut raw, &mut cur);
    finalize_vad_spans(&raw, cur, pcm.len())
}

/// Incremental cached VAD per `(pcm_path, pcm_len)` so 5-minute re-checks only run WebRTC VAD on
/// newly appended audio bytes.
fn speech_spans_cached(pcm_path: &Path, bytes: &[u8]) -> (Vec<(i64, i64)>, i64) {
    let total_samples = bytes.len() / 2;
    let aligned_samples = (total_samples / FRAME) * FRAME;
    let dur = (total_samples * 1000 / RATE) as i64;
    let prefix_hash = fnv1a_64(&bytes[..bytes.len().min(4096)]);
    let key = pcm_path.to_path_buf();
    let cache = VAD_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    let (start_sample, mut raw_spans, mut cur_start) = {
        let g = cache.lock().unwrap();
        if let Some(e) = g.get(&key) {
            if e.prefix_hash == prefix_hash && e.pcm_len <= aligned_samples {
                (e.pcm_len, e.raw_spans.clone(), e.cur_start)
            } else {
                (0, vec![], None)
            }
        } else {
            (0, vec![], None)
        }
    };

    if aligned_samples > start_sample {
        let new_bytes = &bytes[start_sample * 2..aligned_samples * 2];
        let pcm_delta: Vec<i16> = new_bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        run_vad_slice(&pcm_delta, start_sample, &mut raw_spans, &mut cur_start);
        let mut g = cache.lock().unwrap();
        if g.len() >= 8 && !g.contains_key(&key) {
            g.clear();
        }
        g.insert(
            key,
            VadCacheEntry {
                pcm_len: aligned_samples,
                prefix_hash,
                raw_spans: raw_spans.clone(),
                cur_start,
            },
        );
    }

    (finalize_vad_spans(&raw_spans, cur_start, total_samples), dur)
}

#[derive(Clone, Debug)]
struct ParsedCue {
    orig_idx: usize,
    start: i64,
    end: i64,
    is_sdh: bool,
}

fn parse_ts_ms(s: &str) -> Option<i64> {
    let s = s.trim().split_whitespace().next()?.replace(',', ".");
    let p: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match p.len() {
        3 => (
            p[0].parse::<f64>().ok()?,
            p[1].parse::<f64>().ok()?,
            p[2].parse::<f64>().ok()?,
        ),
        2 => (0.0, p[0].parse::<f64>().ok()?, p[1].parse::<f64>().ok()?),
        _ => return None,
    };
    Some(((h * 3600.0 + m * 60.0 + sec) * 1000.0).round() as i64)
}

/// Detect SDH non-speech cues (`[MUSIC]`, `(LAUGHING)`, `♪...♪`, etc.) so they don't pollute
/// speech-overlap alignment.
pub fn is_sdh_nonspeech(text: &str) -> bool {
    let no_html = crate::re!(r"<[^>]+>").replace_all(text, "");
    let no_ass = crate::re!(r"\{[^}]*\}").replace_all(&no_html, "");
    let trimmed = no_ass.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Musical cues / lyrics marked with ♪ or ♫
    let non_empty_lines: Vec<&str> = trimmed.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if !non_empty_lines.is_empty()
        && non_empty_lines
            .iter()
            .all(|l| l.contains('♪') || l.contains('♫'))
    {
        return true;
    }
    // Strip bracketed & parenthesized sound descriptions: [MUSIC], (LAUGHING), etc.
    let stripped = crate::re!(r"\[[^\]]*\]|\([^)]*\)").replace_all(trimmed, " ");
    !stripped.chars().any(|c| c.is_alphanumeric())
}

pub fn parse_vtt(text: &str) -> Vec<(i64, i64)> {
    text.lines()
        .filter_map(|l| {
            let (a, b) = l.split_once("-->")?;
            Some((parse_ts_ms(a)?, parse_ts_ms(b)?))
        })
        .filter(|(a, b)| b > a)
        .collect()
}

/// Parse VTT cues matching `app.js` cue indexing (`0..orig_len`) while tagging SDH non-speech cues.
fn parse_vtt_cues(text: &str) -> Vec<ParsedCue> {
    let clean = text.replace('\r', "");
    let mut out = vec![];
    for block in clean.split("\n\n") {
        let lines: Vec<&str> = block.split('\n').collect();
        let Some(ti) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let Some((a_s, b_s)) = lines[ti].split_once("-->") else {
            continue;
        };
        let (Some(a), Some(b)) = (parse_ts_ms(a_s), parse_ts_ms(b_s)) else {
            continue;
        };
        if b <= a {
            continue;
        }
        let raw_txt = lines[ti + 1..].join("\n");
        let no_tags = crate::re!(r"\{[^}]*\}")
            .replace_all(&crate::re!(r"<[^>]+>").replace_all(&raw_txt, ""), "")
            .trim()
            .to_string();
        if no_tags.is_empty() {
            continue;
        }
        let orig_idx = out.len();
        out.push(ParsedCue {
            orig_idx,
            start: a,
            end: b,
            is_sdh: is_sdh_nonspeech(&no_tags),
        });
    }
    out
}

/// Fraction of subtitle time inside `[0, dur]` that overlaps detected speech.
fn overlap_in_window(cues: &[(i64, i64)], speech: &[(i64, i64)], dur: i64) -> f64 {
    let mut clipped: Vec<(i64, i64)> = cues
        .iter()
        .filter_map(|&(a, b)| {
            let ca = a.max(0);
            let cb = b.min(dur);
            (cb > ca).then_some((ca, cb))
        })
        .collect();
    if clipped.is_empty() {
        return 0.0;
    }
    let total: i64 = clipped.iter().map(|c| c.1 - c.0).sum();
    if total < 1000 {
        return 0.0;
    }
    clipped.sort();
    let mut hit = 0i64;
    let mut j0 = 0usize;
    for c in &clipped {
        while j0 < speech.len() && speech[j0].1 <= c.0 {
            j0 += 1;
        }
        let mut j = j0;
        while j < speech.len() && speech[j].0 < c.1 {
            hit += (c.1.min(speech[j].1) - c.0.max(speech[j].0)).max(0);
            j += 1;
        }
    }
    hit as f64 / total as f64
}

fn shift(v: &[(i64, i64)], d: i64) -> Vec<(i64, i64)> {
    v.iter().map(|&(a, b)| (a + d, b + d)).collect()
}

/// Subtitles often appear early and linger after speech; viewers notice the START. After alass,
/// nudge everything (±1.5 s) so cue starts land on speech onsets.
fn refine_onsets(cues: &[(i64, i64)], deltas: &[i64], speech: &[(i64, i64)], dur: i64) -> i64 {
    let onsets: Vec<i64> = speech.iter().map(|s| s.0).collect();
    if onsets.is_empty() {
        return 0;
    }
    let nearest = |t: i64| -> i64 {
        let i = onsets.partition_point(|&o| o < t);
        let mut d = i64::MAX;
        if i < onsets.len() {
            d = d.min((onsets[i] - t).abs());
        }
        if i > 0 {
            d = d.min((t - onsets[i - 1]).abs());
        }
        d
    };
    let score = |extra: i64| -> f64 {
        cues.iter()
            .zip(deltas)
            .filter_map(|(c, d)| {
                let s = c.0 + d + extra;
                (s >= -500 && s <= dur).then_some(s)
            })
            .map(|s| {
                let x = nearest(s) as f64 / 250.0;
                (-x * x).exp()
            })
            .sum()
    };
    let base = score(0);
    let mut best = (0i64, base);
    let mut e = -1500;
    while e <= 1500 {
        let sc = score(e);
        if sc > best.1 {
            best = (e, sc);
        }
        e += 20;
    }
    // require a real gain so we don't jitter on already-good subtitles
    if best.1 > base * 1.05 {
        best.0
    } else {
        0
    }
}

const MIN_SEG: usize = 8;
/// Merge runs of equal offset shorter than MIN_SEG lines into the neighbouring longer run.
fn smooth_segments(mut d: Vec<i64>) -> Vec<i64> {
    for _ in 0..10 {
        let mut segs: Vec<(usize, usize)> = vec![]; // [start, end)
        let mut i = 0;
        while i < d.len() {
            let mut j = i + 1;
            while j < d.len() && d[j] == d[i] {
                j += 1;
            }
            segs.push((i, j));
            i = j;
        }
        if segs.len() <= 1 {
            break;
        }
        let Some(k) = (0..segs.len())
            .filter(|&k| segs[k].1 - segs[k].0 < MIN_SEG)
            .min_by_key(|&k| segs[k].1 - segs[k].0)
        else {
            break;
        };
        let len = |k: usize| segs[k].1 - segs[k].0;
        let src = match (k.checked_sub(1), (k + 1 < segs.len()).then_some(k + 1)) {
            (Some(p), Some(n)) => {
                if len(p) >= len(n) {
                    p
                } else {
                    n
                }
            }
            (Some(p), None) => p,
            (None, Some(n)) => n,
            _ => break,
        };
        let v = d[segs[src].0];
        for x in segs[k].0..segs[k].1 {
            d[x] = v;
        }
    }
    d
}

fn spans(v: &[(i64, i64)]) -> Vec<TimeSpan> {
    v.iter()
        .map(|&(a, b)| TimeSpan::new(TimePoint::from(a), TimePoint::from(b.max(a + 1))))
        .collect()
}

/// Exact film-time offset caused by scaling `base_ms` by `ratio` when translating session-relative
/// deltas to absolute film-time deltas (`t_film = s * ratio + d_film`, MSE `base = 0`).
pub fn base_ratio_offset_ms(base_ms: i64, ratio: f64) -> i64 {
    (base_ms as f64 * (1.0 - ratio)).round() as i64
}

fn sync_from_spans(speech: &[(i64, i64)], dur: i64, all_parsed: &[ParsedCue], base_ms: i64) -> Value {
    let speech_ms: i64 = speech.iter().map(|s| s.1 - s.0).sum();
    let orig_len = all_parsed.len();

    // Candidate non-SDH cues for a given ratio `r`, mapped into audio-relative time via:
    //   rel = round(orig * r) - base_ms
    // Because `orig * r - base_ms == (orig - base_ms) * r - base_ms * (1 - r)`, the shift `d_film`
    // that aligns `rel + d_film ≈ t_audio_rel` directly satisfies `t_film = orig * r + d_film`
    // (zero -153.8s drift on 1h MSE resumes!).
    let candidates_for = |r: f64| -> Vec<(usize, (i64, i64))> {
        all_parsed
            .iter()
            .filter(|c| !c.is_sdh)
            .filter_map(|c| {
                let sa = (c.start as f64 * r).round() as i64 - base_ms;
                let sb = (c.end as f64 * r).round() as i64 - base_ms;
                if sb > -WINDOW_PAD_MS && sa < dur + WINDOW_PAD_MS {
                    Some((c.orig_idx, (sa, sb.max(sa + 1))))
                } else {
                    None
                }
            })
            .collect()
    };

    let ratios = [25.0 / 23.976, 23.976 / 25.0, 25.0 / 24.0, 24.0 / 25.0];
    let cands_1 = candidates_for(1.0);
    let any_cands = cands_1.len() >= 8 || ratios.iter().any(|&r| candidates_for(r).len() >= 8);
    if speech_ms < 15_000 || !any_cands {
        return json!({
            "ready": false,
            "reason": "need more dialogue",
            "speechSecs": speech_ms / 1000,
            "cues": cands_1.len(),
            "heardSecs": dur / 1000,
            "baseMs": base_ms
        });
    }

    let reference = spans(speech);
    let mut alt = (1.0f64, -1.0f64);
    for r in ratios {
        let rc = candidates_for(r);
        if rc.len() < 8 {
            continue;
        }
        let sc: Vec<(i64, i64)> = rc.iter().map(|&(_, t)| t).collect();
        let (d, _) = align_nosplit(&reference, &spans(&sc), standard_scoring, NoProgressHandler);
        let ov = overlap_in_window(&shift(&sc, d.as_i64()), speech, dur);
        if ov > alt.1 {
            alt = (r, ov);
        }
    }

    struct Solved {
        ratio: f64,
        indices: Vec<usize>,
        deltas: Vec<i64>,
        score: f64,
        after: f64,
    }

    let solve = |r: f64| -> Option<Solved> {
        let rc = candidates_for(r);
        if rc.len() < 8 {
            return None;
        }
        let sc_all: Vec<(i64, i64)> = rc.iter().map(|&(_, t)| t).collect();
        let (d0_td, score0) = align_nosplit(&reference, &spans(&sc_all), standard_scoring, NoProgressHandler);
        let d0 = d0_td.as_i64();

        // Narrow to cues whose coarsely-shifted timestamps land inside the heard audio window
        // (with ±25s slack for mid-window splits) so out-of-window cues at ±180s cannot collapse inward.
        let mut active: Vec<(usize, (i64, i64))> = rc
            .iter()
            .copied()
            .filter(|&(_, (a, b))| b + d0 > -25_000 && a + d0 < dur + 25_000)
            .collect();
        if active.len() < 8 {
            active = rc;
        }
        let sc: Vec<(i64, i64)> = active.iter().map(|&(_, t)| t).collect();
        let indices: Vec<usize> = active.iter().map(|&(idx, _)| idx).collect();

        // Baseline constant-shift solution + onset refinement
        let mut ns_deltas = vec![d0; sc.len()];
        let ns_extra = refine_onsets(&sc, &ns_deltas, speech, dur);
        for x in &mut ns_deltas {
            *x += ns_extra;
        }
        let ns_corr: Vec<(i64, i64)> = sc.iter().zip(&ns_deltas).map(|(&(a, b), d)| (a + d, b + d)).collect();
        let ns_ov = overlap_in_window(&ns_corr, speech, dur);

        let (best_deltas, best_score, best_ov) = if sc.len() >= 30 {
            let (d, sp_score) = align(&reference, &spans(&sc), 7.0, Some(3.0), standard_scoring, NoProgressHandler);
            let mut sp_deltas = smooth_segments(d.iter().map(TimeDelta::as_i64).collect());
            let sp_extra = refine_onsets(&sc, &sp_deltas, speech, dur);
            for x in &mut sp_deltas {
                *x += sp_extra;
            }
            let sp_corr: Vec<(i64, i64)> = sc.iter().zip(&sp_deltas).map(|(&(a, b), d)| (a + d, b + d)).collect();
            let sp_ov = overlap_in_window(&sp_corr, speech, dur);
            let sp_sane = sp_deltas.iter().all(|x| x.abs() <= WINDOW_PAD_MS + 5_000);
            if sp_sane && sp_ov > ns_ov + 0.015 {
                (sp_deltas, sp_score, sp_ov)
            } else {
                (ns_deltas, score0, ns_ov)
            }
        } else {
            (ns_deltas, score0, ns_ov)
        };

        Some(Solved {
            ratio: r,
            indices,
            deltas: best_deltas,
            score: best_score,
            after: best_ov,
        })
    };

    let base = solve(1.0);
    // Only change frame rate when it clearly wins after full alignment (or base had no candidates).
    let chosen = match (base, if alt.0 != 1.0 { solve(alt.0) } else { None }) {
        (Some(b), Some(o)) if o.after > b.after + 0.03 => o,
        (Some(b), _) => b,
        (None, Some(o)) => o,
        (None, None) => {
            return json!({
                "ready": false,
                "reason": "need more dialogue",
                "speechSecs": speech_ms / 1000,
                "cues": cands_1.len(),
                "heardSecs": dur / 1000,
                "baseMs": base_ms
            });
        }
    };

    // Measure `before` overlap on original unscaled non-SDH cues inside [0, dur].
    let orig_window: Vec<(i64, i64)> = all_parsed
        .iter()
        .filter(|c| !c.is_sdh)
        .map(|c| (c.start - base_ms, c.end - base_ms))
        .collect();
    let before = overlap_in_window(&orig_window, speech, dur);

    let max_shift = chosen.deltas.iter().map(|d| d.abs()).max().unwrap_or(0);
    let mut sorted = chosen.deltas.clone();
    sorted.sort();
    let median = sorted[sorted.len() / 2];
    let tail_slice: Vec<i64> = {
        let mut t: Vec<i64> = chosen.deltas.iter().rev().take(10).copied().collect();
        t.sort();
        t
    };
    let tail_offset = tail_slice[tail_slice.len() / 2];
    let first_delta = chosen.deltas.first().copied().unwrap_or(median);
    let first_idx = chosen.indices.first().copied().unwrap_or(0);
    let last_idx = chosen.indices.last().copied().unwrap_or(0);

    // Map solved deltas back to original full cue indices [0..orig_len] so `app.js` `deltas[i]`
    // never misaligns when resuming mid-file or skipping SDH cues!
    let mut full_deltas = vec![tail_offset; orig_len];
    for i in 0..first_idx.min(orig_len) {
        if base_ms > 0 && all_parsed[i].end < base_ms - WINDOW_PAD_MS {
            full_deltas[i] = 0;
        } else {
            full_deltas[i] = first_delta;
        }
    }
    let mut solved_map = vec![None; orig_len];
    for (&idx, &d) in chosen.indices.iter().zip(&chosen.deltas) {
        if idx < orig_len {
            solved_map[idx] = Some(d);
        }
    }
    let mut cur_d = first_delta;
    for i in first_idx..=last_idx.min(orig_len.saturating_sub(1)) {
        if let Some(d) = solved_map[i] {
            cur_d = d;
        }
        full_deltas[i] = cur_d;
    }

    let base_offset_ms = base_ratio_offset_ms(base_ms, chosen.ratio);
    let plausible = chosen.deltas.iter().all(|d| d.abs() <= WINDOW_PAD_MS + 5_000);
    let ratio_changed = (chosen.ratio - 1.0).abs() > 1e-6;
    json!({
        "ready": true,
        "ratio": chosen.ratio,
        "baseMs": base_ms,
        "baseOffsetMs": base_offset_ms,
        "deltas": full_deltas,
        "analyzed": chosen.deltas.len(),
        "totalCues": orig_len,
        "offset": median,
        "tailOffset": tail_offset,
        "score": chosen.score,
        "overlapBefore": before,
        "overlapAfter": chosen.after,
        // self-check: only apply when it measurably improves speech overlap
        "improves": plausible && chosen.after >= before + 0.03 && (max_shift >= 150 || ratio_changed),
        "heardSecs": dur / 1000,
        "speechSecs": speech_ms / 1000
    })
}

pub fn sync(pcm_path: &Path, vtt_path: &Path) -> Value {
    sync_at(pcm_path, vtt_path, 0)
}

/// `base_ms`: session start in film time (subtitles carry film time; the audio tap starts at the session start).
pub fn sync_at(pcm_path: &Path, vtt_path: &Path, base_ms: i64) -> Value {
    let Ok(bytes) = std::fs::read(pcm_path) else {
        return json!({ "ready": false, "reason": "no audio yet" });
    };
    let (speech, dur) = speech_spans_cached(pcm_path, &bytes);
    let vtt_text = std::fs::read_to_string(vtt_path).unwrap_or_default();
    let parsed = parse_vtt_cues(&vtt_text);
    sync_from_spans(&speech, dur, &parsed, base_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_vtt_ms(ms: i64) -> String {
        let ms = ms.max(0);
        let h = ms / 3_600_000;
        let m = (ms % 3_600_000) / 60_000;
        let s = (ms % 60_000) / 1000;
        let r = ms % 1000;
        format!("{h:02}:{m:02}:{s:02}.{r:03}")
    }

    #[test]
    fn fps_ratio_resume_1h_math_has_zero_drift() {
        // Simulate a 1-hour resume (base_ms = 3_600_000) of a 23.976 fps film with a 25 fps subtitle.
        // True relationship: t_film = t_sub * (25.0 / 23.976) + 2000 ms (2s constant offset).
        let base_ms: i64 = 3_600_000;
        let true_ratio = 25.0 / 23.976;
        let true_const_ms: i64 = 2_000;
        let dur: i64 = 180_000; // 3 minutes of heard audio starting at 1h00m00s

        assert_eq!(base_ratio_offset_ms(base_ms, true_ratio), -153_754);

        // Build 30 spoken spans in [5s .. 175s] relative to base_ms (each 2.5s long, every 5.5s)
        let mut speech: Vec<(i64, i64)> = vec![];
        for k in 0..30i64 {
            let s = 5_000 + k * 5_500;
            speech.push((s, s + 2_500));
        }

        // Build 80 total cues: 30 pre-resume cues, 30 in-window cues, 20 post-window cues.
        let mut vtt = String::from("WEBVTT\n\n");
        let mut in_window_orig_starts: Vec<(usize, i64, i64)> = vec![];
        // 30 pre-resume cues (film times 10m .. 20m)
        for k in 0..30i64 {
            let t = 600_000 + k * 5_500;
            let sa = ((t - true_const_ms) as f64 / true_ratio).round() as i64;
            let sb = ((t + 2_500 - true_const_ms) as f64 / true_ratio).round() as i64;
            vtt.push_str(&format!("{} --> {}\nEarlier line {k}\n\n", fmt_vtt_ms(sa), fmt_vtt_ms(sb)));
        }
        // 30 in-window cues corresponding directly to `speech`
        for (k, &(rel_s, rel_e)) in speech.iter().enumerate() {
            let film_s = base_ms + rel_s;
            let film_e = base_ms + rel_e;
            let sa = ((film_s - true_const_ms) as f64 / true_ratio).round() as i64;
            let sb = ((film_e - true_const_ms) as f64 / true_ratio).round() as i64;
            in_window_orig_starts.push((30 + k, sa, film_s));
            vtt.push_str(&format!("{} --> {}\nDialogue line {k}\n\n", fmt_vtt_ms(sa), fmt_vtt_ms(sb)));
        }
        // 20 post-window cues
        for k in 0..20i64 {
            let film_s = base_ms + 200_000 + k * 5_500;
            let sa = ((film_s - true_const_ms) as f64 / true_ratio).round() as i64;
            let sb = ((film_s + 2_500 - true_const_ms) as f64 / true_ratio).round() as i64;
            vtt.push_str(&format!("{} --> {}\nLater line {k}\n\n", fmt_vtt_ms(sa), fmt_vtt_ms(sb)));
        }

        let parsed = parse_vtt_cues(&vtt);
        assert_eq!(parsed.len(), 80);

        let res = sync_from_spans(&speech, dur, &parsed, base_ms);
        assert_eq!(res["ready"].as_bool(), Some(true));
        assert_eq!(res["improves"].as_bool(), Some(true), "expected improves=true: {res}");
        assert_eq!(res["baseMs"].as_i64(), Some(base_ms));
        assert_eq!(res["baseOffsetMs"].as_i64(), Some(-153_754));

        let ratio = res["ratio"].as_f64().unwrap();
        assert!((ratio - true_ratio).abs() < 1e-4, "ratio={ratio}");

        let deltas = res["deltas"].as_array().unwrap();
        assert_eq!(deltas.len(), 80, "deltas must map back to full original cue indices [0..80]");
        // Out-of-window pre-resume cues should be 0
        assert_eq!(deltas[0].as_i64(), Some(0));

        // Check MSE formula (`base = 0`): `orig_start * ratio + deltas[i]` must match true film time (NO -153.8s drift!)
        for (idx, orig_sa, true_film_s) in in_window_orig_starts {
            let d = deltas[idx].as_i64().unwrap();
            let rendered_ms = (orig_sa as f64 * ratio).round() as i64 + d;
            assert!(
                (rendered_ms - true_film_s).abs() <= 250,
                "cue {idx}: rendered={rendered_ms} expected={true_film_s} delta={d}"
            );
        }
    }

    #[test]
    fn full_index_alignment_sdh_strip_and_wide_recap_window() {
        assert!(is_sdh_nonspeech("[DRAMATIC MUSIC PLAYING]"));
        assert!(is_sdh_nonspeech("(LAUGHING)"));
        assert!(is_sdh_nonspeech("♪ Singing a song ♪"));
        assert!(!is_sdh_nonspeech("(whispering) Watch out!"));
        assert!(!is_sdh_nonspeech("[John] Hello there."));

        // Missing 95-second recap at start of episode: subtitle cues are 95s LATE (need -95_000 ms shift).
        let shift_ms: i64 = 95_000;
        let dur: i64 = 160_000;
        let mut speech: Vec<(i64, i64)> = vec![];
        let mut vtt = String::from("WEBVTT\n\n");
        // Cue 0: SDH non-speech cue
        vtt.push_str("00:00:01.000 --> 00:00:04.000\n[UPBEAT THEME MUSIC]\n\n");
        // Cues 1..=25: real dialogue shifted late by +95s (beyond old [0, dur] cutoff for later cues!)
        for k in 0..25i64 {
            let s = 5_000 + k * 6_000;
            let e = s + 2_500;
            speech.push((s, e));
            vtt.push_str(&format!(
                "{} --> {}\nSpoken line {k}\n\n",
                fmt_vtt_ms(s + shift_ms),
                fmt_vtt_ms(e + shift_ms)
            ));
        }
        // Cue 26: trailing SDH cue
        vtt.push_str("00:04:30.000 --> 00:04:34.000\n♪ Closing melody ♪\n\n");

        let parsed = parse_vtt_cues(&vtt);
        assert_eq!(parsed.len(), 27);
        assert!(parsed[0].is_sdh);
        assert!(!parsed[1].is_sdh);
        assert!(parsed[26].is_sdh);

        let res = sync_from_spans(&speech, dur, &parsed, 0);
        assert_eq!(res["ready"].as_bool(), Some(true));
        assert_eq!(res["improves"].as_bool(), Some(true), "res={res}");
        let deltas = res["deltas"].as_array().unwrap();
        assert_eq!(deltas.len(), 27);
        let d_mid = deltas[10].as_i64().unwrap();
        assert!(
            (d_mid + shift_ms).abs() <= 250,
            "expected ~-{shift_ms}ms shift, got {d_mid}"
        );
    }
}
