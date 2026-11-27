// Multi-source TV & Anime Intro / Recap / End-Credits Skip Resolver & Persistent Cache.
//
// Combines three free, zero-API-key crowd-sourced timestamp databases (requiring ZERO IPTV
// provider connections!) with local MKV chapter markers and local audio fingerprinting:
//   1. MKV embedded chapters (exact to the file being played)
//   2. Local audio fingerprint matches (intro.rs)
//   3. AniSkip (for Anime: OP / ED / Recap via ARM TMDB -> MyAnimeList mapping)
//   4. IntroDB (api.introdb.app/segments via IMDb ID — intro, recap, outro/credits, post_credits)
//   5. TheIntroDB v2 (api.theintrodb.org/v2/media via TMDB ID — intro, recap, credits, preview)
//
// Segments are merged per type (e.g., MKV chapter intro + IntroDB credits) and sanity-checked
// against the actual episode duration (including PAL 25fps / trimmed-end credit re-anchoring).

use crate::app::App;
use crate::util::now_ms;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HIT_TTL_MS: i64 = 30 * 86_400_000; // 30 days for found skip timestamps
const MISS_TTL_MS: i64 = 3 * 86_400_000; // 3 days for misses (new episodes get community timestamps quickly)
const MAL_TTL_MS: i64 = 30 * 86_400_000; // 30 days for finished MAL TV season chains
const MAL_AIRING_TTL_MS: i64 = 7 * 86_400_000; // 7 days when latest cour is still airing (eps == 0)
const CURRENT_SKIP_VER: u8 = 2; // v2: AniList-filtered MAL TV chain + wildcard episodeLength + abs_ep mapping
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkipSeg {
    pub s: f64,
    pub e: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MalEntry {
    pub id: i64,
    #[serde(default)]
    pub eps: i64, // 0 = currently airing / unknown total
    #[serde(default)]
    pub start: String, // YYYY-MM-DD for chronological sorting
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct MalChain {
    #[serde(default)]
    entries: Vec<MalEntry>,
    #[serde(default)]
    t: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EpisodeSkips {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recap: Option<SkipSeg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<SkipSeg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_end: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_credits: Option<SkipSeg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ep_len: Option<f64>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub t: i64,
    #[serde(default)]
    pub v: u8,
}

impl EpisodeSkips {
    pub fn has_any(&self) -> bool {
        self.recap.is_some() || self.intro.is_some() || self.credits.is_some()
    }
    pub fn is_fresh(&self, now: i64) -> bool {
        if self.t <= 0 { return false; }
        let ttl = if self.has_any() { HIT_TTL_MS } else { MISS_TTL_MS };
        now - self.t < ttl
    }
    pub fn is_fresh_for(&self, now: i64, is_anime: bool) -> bool {
        if is_anime && self.v < CURRENT_SKIP_VER { return false; }
        self.is_fresh(now)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DiskCache {
    #[serde(default)]
    eps: HashMap<String, EpisodeSkips>,
    #[serde(default)]
    mal: HashMap<String, Vec<i64>>,
    #[serde(default)]
    mal_tv: HashMap<String, MalChain>,
}

pub struct SkipDb {
    file: PathBuf,
    cache: Mutex<DiskCache>,
    dirty: std::sync::atomic::AtomicBool,
    last_save: std::sync::atomic::AtomicI64,
}

impl SkipDb {
    pub fn new(data: &Path) -> Self {
        let file = data.join("skipdb-cache.json");
        let cache = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<DiskCache>(&b).ok())
            .unwrap_or_default();
        SkipDb {
            file,
            cache: Mutex::new(cache),
            dirty: Default::default(),
            last_save: Default::default(),
        }
    }

    pub fn save_if_dirty(&self, force: bool) {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.dirty.load(SeqCst) { return; }
        let now = now_ms();
        if !force && now - self.last_save.load(SeqCst) < 5000 { return; }
        self.dirty.store(false, SeqCst);
        self.last_save.store(now, SeqCst);
        let bytes = match serde_json::to_vec(&*self.cache.lock().unwrap()) {
            Ok(b) => b,
            Err(_) => return,
        };
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    pub fn cache_key(tmdb_id: &str, imdb_id: &str, season: i64, ep: i64) -> Option<String> {
        if season < 0 || ep <= 0 { return None; }
        let tm = tmdb_id.trim();
        let im = imdb_id.trim();
        if !tm.is_empty() && tm != "0" {
            Some(format!("tmdb:{tm}:{season}:{ep}"))
        } else if im.starts_with("tt") {
            Some(format!("imdb:{im}:{season}:{ep}"))
        } else {
            None
        }
    }

    pub fn get_cached(&self, key: &str) -> Option<EpisodeSkips> {
        self.get_cached_for(key, false)
    }

    pub fn get_cached_for(&self, key: &str, is_anime: bool) -> Option<EpisodeSkips> {
        let now = now_ms();
        self.cache.lock().unwrap().eps.get(key).filter(|e| e.is_fresh_for(now, is_anime)).cloned()
    }

    pub fn put_cached(&self, key: String, val: EpisodeSkips) {
        self.cache.lock().unwrap().eps.insert(key, val);
        self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
        self.save_if_dirty(false);
    }

    pub fn stats(&self) -> Value {
        let g = self.cache.lock().unwrap();
        let mut with_any = 0usize;
        let mut with_intro = 0usize;
        let mut with_credits = 0usize;
        let mut with_recap = 0usize;
        let mut shows: HashSet<&str> = HashSet::new();
        for (k, v) in &g.eps {
            if v.has_any() {
                with_any += 1;
                if let Some((prefix, _)) = k.rsplit_once(':') {
                    if let Some((show_k, _)) = prefix.rsplit_once(':') {
                        shows.insert(show_k);
                    }
                }
            }
            if v.intro.is_some() { with_intro += 1; }
            if v.credits.is_some() { with_credits += 1; }
            if v.recap.is_some() { with_recap += 1; }
        }
        json!({
            "cached": g.eps.len(),
            "matched": with_any,
            "withIntro": with_intro,
            "withCredits": with_credits,
            "withRecap": with_recap,
            "shows": shows.len(),
        })
    }
}

// ------------------------------------------------------------------ Parsers (pure & unit-tested)

fn clean_seg(s: f64, e: f64, min_len: f64, max_len: f64) -> Option<SkipSeg> {
    if !s.is_finite() || !e.is_finite() || s < 0.0 || e <= s { return None; }
    let len = e - s;
    if len < min_len || len > max_len { return None; }
    Some(SkipSeg { s: (s * 10.0).round() / 10.0, e: (e * 10.0).round() / 10.0 })
}

/// Parse IntroDB (`https://api.introdb.app/segments?imdb_id=...&season=...&episode=...`).
pub fn parse_introdb(v: &Value) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    let seg_from = |obj: &Value, min_l: f64, max_l: f64| -> Option<SkipSeg> {
        let s = obj["start_sec"].as_f64().or_else(|| obj["start_ms"].as_f64().map(|m| m / 1000.0))?;
        let e = obj["end_sec"].as_f64().or_else(|| obj["end_ms"].as_f64().map(|m| m / 1000.0))?;
        clean_seg(s, e, min_l, max_l)
    };
    if let Some(rc) = seg_from(&v["recap"], 5.0, 240.0) { out.recap = Some(rc); }
    if let Some(intro) = seg_from(&v["intro"], 5.0, 210.0) { out.intro = Some(intro); }
    if let Some(o) = v.get("outro").filter(|x| x.is_object()) {
        if let Some(st) = o["start_sec"].as_f64().or_else(|| o["start_ms"].as_f64().map(|m| m / 1000.0)) {
            if st.is_finite() && st > 60.0 {
                out.credits = Some((st * 10.0).round() / 10.0);
                if let Some(en) = o["end_sec"].as_f64().or_else(|| o["end_ms"].as_f64().map(|m| m / 1000.0)) {
                    if en.is_finite() && en > st + 5.0 {
                        out.credits_end = Some((en * 10.0).round() / 10.0);
                    }
                }
            }
        }
    }
    if let Some(pc) = seg_from(&v["post_credits"], 5.0, 300.0) { out.post_credits = Some(pc); }
    if out.has_any() { out.source = "introdb".into(); }
    out
}

/// Parse TheIntroDB v2 (`https://api.theintrodb.org/v2/media?tmdb_id=...&season=...&episode=...`).
pub fn parse_theintrodb(v: &Value) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    let first_seg = |arr: &Value, min_l: f64, max_l: f64| -> Option<SkipSeg> {
        for item in arr.as_array()? {
            let s = item["start_ms"].as_f64().map(|m| m / 1000.0).unwrap_or(0.0);
            let Some(e) = item["end_ms"].as_f64().map(|m| m / 1000.0) else { continue };
            if let Some(seg) = clean_seg(s, e, min_l, max_l) {
                return Some(seg);
            }
        }
        None
    };
    if let Some(rc) = first_seg(&v["recap"], 5.0, 240.0) { out.recap = Some(rc); }
    if let Some(intro) = first_seg(&v["intro"], 5.0, 210.0) { out.intro = Some(intro); }
    if let Some(arr) = v["credits"].as_array() {
        for item in arr {
            if let Some(st) = item["start_ms"].as_f64().map(|m| m / 1000.0) {
                if st.is_finite() && st > 60.0 {
                    out.credits = Some((st * 10.0).round() / 10.0);
                    if let Some(en) = item["end_ms"].as_f64().map(|m| m / 1000.0) {
                        if en.is_finite() && en > st + 5.0 {
                            out.credits_end = Some((en * 10.0).round() / 10.0);
                        }
                    }
                    break;
                }
            }
        }
    }
    if out.has_any() { out.source = "theintrodb".into(); }
    out
}

/// Parse AniSkip (`https://api.aniskip.com/v2/skip-times/{mal_id}/{ep}?types[]=...`).
/// Prefers pure `op`/`ed` over `mixed-op`/`mixed-ed` when both are present.
pub fn parse_aniskip(v: &Value) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    if v["found"].as_bool() != Some(true) { return out; }
    let Some(results) = v["results"].as_array() else { return out };
    let mut mixed_op: Option<SkipSeg> = None;
    let mut mixed_ed: Option<(f64, Option<f64>)> = None;
    for r in results {
        let stype = r["skipType"].as_str().unwrap_or("");
        let iv = &r["interval"];
        let (Some(s), Some(e)) = (iv["startTime"].as_f64(), iv["endTime"].as_f64()) else { continue };
        if out.ep_len.is_none() {
            if let Some(el) = r["episodeLength"].as_f64().filter(|x| *x > 120.0) {
                out.ep_len = Some(el);
            }
        }
        match stype {
            "op" if out.intro.is_none() => {
                out.intro = clean_seg(s, e, 10.0, 210.0);
            }
            "mixed-op" if mixed_op.is_none() => {
                mixed_op = clean_seg(s, e, 10.0, 210.0);
            }
            "recap" if out.recap.is_none() => {
                out.recap = clean_seg(s, e, 5.0, 260.0);
            }
            "ed" if out.credits.is_none() => {
                if s.is_finite() && s > 60.0 {
                    out.credits = Some((s * 10.0).round() / 10.0);
                    if e.is_finite() && e > s + 5.0 {
                        out.credits_end = Some((e * 10.0).round() / 10.0);
                    }
                }
            }
            "mixed-ed" if mixed_ed.is_none() => {
                if s.is_finite() && s > 60.0 {
                    let st = (s * 10.0).round() / 10.0;
                    let en = (e.is_finite() && e > s + 5.0).then(|| (e * 10.0).round() / 10.0);
                    mixed_ed = Some((st, en));
                }
            }
            _ => {}
        }
    }
    if out.intro.is_none() { out.intro = mixed_op; }
    if out.credits.is_none() {
        if let Some((st, en)) = mixed_ed {
            out.credits = Some(st);
            out.credits_end = en;
        }
    }
    if out.has_any() { out.source = "aniskip".into(); }
    out
}

/// Merge two remote EpisodeSkips (`primary` wins per field; `secondary` fills missing fields).
pub fn combine_remote(mut primary: EpisodeSkips, secondary: EpisodeSkips) -> EpisodeSkips {
    let mut used_sec = false;
    if primary.recap.is_none() && secondary.recap.is_some() { primary.recap = secondary.recap; used_sec = true; }
    if primary.intro.is_none() && secondary.intro.is_some() { primary.intro = secondary.intro; used_sec = true; }
    if primary.credits.is_none() && secondary.credits.is_some() {
        primary.credits = secondary.credits;
        primary.credits_end = secondary.credits_end;
        used_sec = true;
    } else if primary.credits.is_some() && primary.credits_end.is_none() && secondary.credits_end.is_some() {
        primary.credits_end = secondary.credits_end;
    }
    if primary.post_credits.is_none() && secondary.post_credits.is_some() { primary.post_credits = secondary.post_credits; used_sec = true; }
    if primary.ep_len.is_none() { primary.ep_len = secondary.ep_len; }
    if used_sec && !secondary.source.is_empty() {
        if primary.source.is_empty() {
            primary.source = secondary.source;
        } else if !primary.source.contains(&secondary.source) {
            primary.source = format!("{}+{}", primary.source, secondary.source);
        }
    }
    primary
}

/// Final merged output attached to `/api/probe` and `/api/skips/resolve`.
#[derive(Clone, Debug, Default)]
pub struct ResolvedSkips {
    pub skips: Vec<Value>,         // [{ s, e, title }, ...]
    pub intro: Option<(f64, f64)>, // primary intro segment
    pub recap: Option<(f64, f64)>, // primary recap segment
    pub credits: Option<f64>,      // validated credits/ED start time
    pub credits_end: Option<f64>,  // validated credits/ED end time (for skipping ED -> post-credits/preview)
    pub post_credits: Option<(f64, f64)>,
    pub has_post_credits: bool,
    pub source: String,
}

fn validate_head_seg(seg: &SkipSeg, dur: f64, max_frac: f64, max_len: f64) -> Option<(f64, f64)> {
    let c = clean_seg(seg.s, seg.e, 4.0, max_len)?;
    if dur > 120.0 && c.e > dur * max_frac { return None; }
    Some((c.s, c.e))
}

fn round1(x: f64) -> f64 { (x * 10.0).round() / 10.0 }

fn validate_credits(start: f64, end: Option<f64>, ep_len: Option<f64>, dur: f64) -> Option<(f64, Option<f64>)> {
    if !start.is_finite() || start < 60.0 { return None; }
    let clean_end = end.filter(|&x| x.is_finite() && x > start + 5.0);
    if dur <= 120.0 {
        return Some((round1(start), clean_end.map(round1)));
    }
    // Check ep_len first so anime post-credits/preview tails (where credits_end < ep_len by 30-60s)
    // aren't misread as truncated files, while PAL 25fps speedups & trimmed encodes re-anchor accurately!
    if let Some(el) = ep_len.filter(|&x| x > start + 8.0 && x > dur + 12.0) {
        let ratio = dur / el;
        let (adj_s, adj_e) = if (0.93..=0.985).contains(&ratio) {
            // PAL 25fps (~4% speedup) scales proportionally across the episode
            (start * ratio, clean_end.map(|e| (e * ratio).min(dur)))
        } else {
            // Trimmed outro / shorter broadcast cut: anchor relative to episode end
            let delta = dur - el;
            (start + delta, clean_end.map(|e| (e + delta).min(dur)))
        };
        if adj_s >= dur * 0.55 && adj_s <= dur - 6.0 {
            let valid_e = adj_e.filter(|&e| e > adj_s + 5.0 && e <= dur + 2.0).map(round1);
            return Some((round1(adj_s), valid_e));
        }
    }
    if let Some(re) = clean_end.filter(|&x| x > dur + 12.0) {
        let cred_len = re - start;
        if (12.0..=360.0).contains(&cred_len) {
            let anchored = dur - cred_len;
            if anchored >= dur * 0.55 && anchored <= dur - 6.0 {
                return Some((round1(anchored), Some(round1(dur))));
            }
        }
    }
    if start >= dur * 0.55 && start <= dur - 6.0 {
        let valid_e = clean_end.filter(|&e| e <= dur + 5.0).map(|e| round1(e.min(dur)));
        return Some((round1(start), valid_e));
    }
    None
}

/// Merge all 5 layers with strict priority:
///   1. MKV embedded chapters (100% file-accurate)
///   2. Local audio fingerprint (exact to provider stream)
///   3. Remote crowd-sourced DBs (AniSkip / IntroDB / TheIntroDB v2)
///
/// Anime refinements:
///   - Drops bogus overlapping community recaps inside the OP (e.g. Black Clover Ep 52, JJK S2E2)
///   - Chains adjacent/overlapping Recap + OP (gap <= 6s in either order) into ONE seamless
///     "Recap + Intro" jump so neither Web MSE nor mpv double-seeks
///   - Exposes `credits_end` & `has_post_credits` so viewers can skip ED straight to post-credits/preview
pub fn merge_all(
    chapter_skips: &[Value],
    chapter_credits: Option<f64>,
    audio_intro: Option<(f64, f64)>,
    audio_credits: Option<f64>,
    remote: Option<&EpisodeSkips>,
    dur: f64,
) -> ResolvedSkips {
    let mut out = ResolvedSkips::default();
    let mut sources: Vec<String> = vec![];
    let mut add_src = |s: &str| {
        if !s.is_empty() && !sources.iter().any(|x| x == s) {
            sources.push(s.to_string());
        }
    };

    // 1. MKV embedded chapters
    let mut ch_recap: Option<(f64, f64)> = None;
    let mut ch_intro: Option<(f64, f64)> = None;
    for c in chapter_skips {
        let (Some(s), Some(e)) = (c["s"].as_f64(), c["e"].as_f64()) else { continue };
        let title = c["title"].as_str().unwrap_or("");
        let low = title.to_lowercase();
        let is_recap = low.contains("recap") || low.contains("previously");
        if is_recap {
            if ch_recap.is_none() {
                if let Some(v) = validate_head_seg(&SkipSeg { s, e }, dur, 0.45, 260.0) {
                    ch_recap = Some(v);
                    add_src("chapters");
                }
            }
        } else if ch_intro.is_none() {
            if let Some(v) = validate_head_seg(&SkipSeg { s, e }, dur, 0.50, 210.0) {
                ch_intro = Some(v);
                add_src("chapters");
            }
        }
    }

    // Recap: Chapters > Remote
    out.recap = ch_recap.or_else(|| {
        let r = remote?;
        let rc = r.recap.as_ref()?;
        let v = validate_head_seg(rc, dur, 0.45, 260.0)?;
        add_src(&r.source);
        Some(v)
    });

    // Intro: Chapters > Local Audio Fingerprint > Remote
    out.intro = ch_intro
        .or_else(|| {
            let (s, e) = audio_intro?;
            let v = validate_head_seg(&SkipSeg { s, e }, dur, 0.50, 210.0)?;
            add_src("audio");
            Some(v)
        })
        .or_else(|| {
            let r = remote?;
            let iv = r.intro.as_ref()?;
            let v = validate_head_seg(iv, dur, 0.50, 210.0)?;
            add_src(&r.source);
            Some(v)
        });

    // Dedupe bogus overlapping community recap that sits >= 65% inside the intro (e.g. Black Clover Ep 52, JJK S2E2)
    if let (Some((rs, re)), Some((is, ie))) = (out.recap, out.intro) {
        let ov = (re.min(ie) - rs.max(is)).max(0.0);
        let r_len = (re - rs).max(1.0);
        if ov / r_len >= 0.65 {
            out.recap = None;
        }
    }

    // Credits: Chapters > Local Audio Fingerprint > Remote
    let cred_pair = chapter_credits
        .and_then(|c| validate_credits(c, None, None, dur))
        .map(|c| { add_src("chapters"); c })
        .or_else(|| {
            let c = audio_credits.and_then(|c| validate_credits(c, None, None, dur))?;
            add_src("audio");
            Some(c)
        })
        .or_else(|| {
            let r = remote?;
            let c = validate_credits(r.credits?, r.credits_end, r.ep_len, dur)?;
            add_src(&r.source);
            Some(c)
        });
    if let Some((cs, ce)) = cred_pair {
        out.credits = Some(cs);
        out.credits_end = ce;
    }

    if let Some(r) = remote {
        if let Some(pc) = &r.post_credits {
            if dur <= 120.0 || (pc.s >= dur * 0.65 && pc.e <= dur + 5.0) {
                out.post_credits = Some((pc.s, pc.e));
            }
        }
    }
    out.has_post_credits = out.post_credits.is_some()
        || out.credits_end.map_or(false, |ce| dur > 120.0 && dur - ce >= 20.0 && dur - ce <= 210.0);

    // Build flat skips[] array (chronological). When Recap and Intro overlap or sit back-to-back
    // (gap <= 6s in either order: Recap->OP or OP->Recap), merge into one seamless "Recap + Intro"
    // jump so auto-skip performs a single zero-stutter seek!
    let mut flat: Vec<(f64, f64, &str)> = vec![];
    match (out.recap, out.intro) {
        (Some((rs, re)), Some((is, ie))) => {
            let (first_s, first_e, second_s, second_e) = if rs <= is { (rs, re, is, ie) } else { (is, ie, rs, re) };
            let gap = second_s - first_e;
            if gap <= 6.0 {
                let merged_e = first_e.max(second_e);
                let title = if rs <= is { "Recap + Intro" } else { "Intro + Recap" };
                flat.push((first_s, merged_e, title));
            } else {
                flat.push((rs, re, "Recap"));
                flat.push((is, ie, "Intro"));
            }
        }
        (Some((rs, re)), None) => flat.push((rs, re, "Recap")),
        (None, Some((is, ie))) => flat.push((is, ie, "Intro")),
        (None, None) => {}
    }
    flat.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.skips = flat.into_iter().map(|(s, e, title)| json!({ "s": s, "e": e, "title": title })).collect();
    out.source = sources.join("+");
    out
}

// ------------------------------------------------------------------ Network Fetchers

async fn fetch_introdb(http: &reqwest::Client, imdb_id: &str, season: i64, ep: i64) -> Option<EpisodeSkips> {
    if !imdb_id.starts_with("tt") { return None; }
    let url = format!("https://api.introdb.app/segments?imdb_id={imdb_id}&season={season}&episode={ep}");
    let resp = http.get(&url)
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_millis(3500))
        .send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let v: Value = resp.json().await.ok()?;
    let parsed = parse_introdb(&v);
    if parsed.has_any() { Some(parsed) } else { None }
}

async fn fetch_theintrodb(http: &reqwest::Client, tmdb_id: &str, season: i64, ep: i64) -> Option<EpisodeSkips> {
    if tmdb_id.is_empty() || tmdb_id == "0" { return None; }
    let url = format!("https://api.theintrodb.org/v2/media?tmdb_id={tmdb_id}&season={season}&episode={ep}");
    let resp = http.get(&url)
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_millis(3500))
        .send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let v: Value = resp.json().await.ok()?;
    let parsed = parse_theintrodb(&v);
    if parsed.has_any() { Some(parsed) } else { None }
}

/// Filter & sort raw ARM MyAnimeList IDs into a clean chronological TV cour chain using AniList's
/// batch GraphQL API (1 fast POST per series, cached permanently on disk). Eliminates OVAs, Movies,
/// and Jump Festa specials that otherwise hijack `mal_ids[season - 1]` (e.g. Black Clover 33950 OVA,
/// Attack on Titan 18397 OVA / 36702 Movie, Demon Slayer 47398 ONA short!).
async fn fetch_anilist_tv_chain(http: &reqwest::Client, raw_ids: &[i64]) -> Option<Vec<MalEntry>> {
    if raw_ids.is_empty() { return Some(vec![]); }
    let query = "query ($ids: [Int]) { Page(perPage: 50) { media(idMal_in: $ids, type: ANIME) { idMal format episodes startDate { year month day } } } }";
    let mut tv: Vec<MalEntry> = vec![];
    let mut tv_short: Vec<MalEntry> = vec![];
    let mut ona: Vec<MalEntry> = vec![];
    let mut saw_any = false;
    for chunk in raw_ids.chunks(50) {
        let body = json!({ "query": query, "variables": { "ids": chunk } });
        let resp = http.post("https://graphql.anilist.co")
            .header("User-Agent", BROWSER_UA)
            .header("Accept", "application/json")
            .timeout(Duration::from_millis(4000))
            .json(&body)
            .send().await.ok()?;
        if !resp.status().is_success() { return None; }
        let v: Value = resp.json().await.ok()?;
        let Some(arr) = v["data"]["Page"]["media"].as_array() else { return None };
        saw_any = true;
        for m in arr {
            let Some(id) = m["idMal"].as_i64().filter(|&x| x > 0) else { continue };
            let fmt = m["format"].as_str().unwrap_or("");
            let eps = m["episodes"].as_i64().unwrap_or(0).max(0);
            let sd = &m["startDate"];
            let yr = sd["year"].as_i64().unwrap_or(9999);
            let mo = sd["month"].as_i64().unwrap_or(12);
            let dy = sd["day"].as_i64().unwrap_or(31);
            let start = format!("{yr:04}-{mo:02}-{dy:02}");
            let entry = MalEntry { id, eps, start };
            match fmt {
                "TV" => tv.push(entry),
                "TV_SHORT" => tv_short.push(entry),
                "ONA" => ona.push(entry),
                _ => {} // drop MOVIE, OVA, SPECIAL, MUSIC
            }
        }
    }
    if !saw_any { return None; }
    let mut chosen = if !tv.is_empty() { tv } else if !tv_short.is_empty() { tv_short } else { ona };
    chosen.sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
    chosen.dedup_by_key(|x| x.id);
    Some(chosen)
}

async fn resolve_mal_tv_chain(app: &App, tmdb_id: &str) -> Vec<MalEntry> {
    if tmdb_id.is_empty() || tmdb_id == "0" { return vec![]; }
    let now = now_ms();
    if let Some(ch) = app.skipdb.cache.lock().unwrap().mal_tv.get(tmdb_id).cloned() {
        let airing = ch.entries.iter().any(|e| e.eps == 0);
        let ttl = if airing { MAL_AIRING_TTL_MS } else { MAL_TTL_MS };
        if ch.t > 0 && now - ch.t < ttl {
            return ch.entries;
        }
    }
    let url = format!("https://arm.haglund.dev/api/v2/themoviedb?id={tmdb_id}&include=myanimelist");
    let raw_ids: Vec<i64> = match app.http.get(&url)
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_millis(3500))
        .send().await
    {
        Ok(r) if r.status().is_success() => {
            r.json::<Value>().await.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
                .into_iter()
                .filter_map(|x| x["myanimelist"].as_i64().filter(|&id| id > 0))
                .collect()
        }
        _ => vec![],
    };
    let entries = if raw_ids.is_empty() {
        vec![]
    } else if let Some(filtered) = fetch_anilist_tv_chain(&app.http, &raw_ids).await {
        if !filtered.is_empty() {
            filtered
        } else {
            raw_ids.iter().map(|&id| MalEntry { id, eps: 0, start: String::new() }).collect()
        }
    } else {
        raw_ids.iter().map(|&id| MalEntry { id, eps: 0, start: String::new() }).collect()
    };
    {
        let mut g = app.skipdb.cache.lock().unwrap();
        g.mal.insert(tmdb_id.to_string(), raw_ids);
        g.mal_tv.insert(tmdb_id.to_string(), MalChain { entries: entries.clone(), t: now });
    }
    app.skipdb.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    entries
}

/// Map `(season, ep, abs_ep)` onto ordered `(mal_id, local_ep)` candidates across a chronological TV MAL chain.
/// Works seamlessly for BOTH:
///   - Continuous single-MAL long-runners (Black Clover 1..170, One Piece, Naruto, Bleach, HxH)
///   - Multi-season & split-cour anime (Demon Slayer Mugen Train + Entertainment District, AoT S3 P1/P2 & S4 P1/P2, JJK, Solo Leveling)
pub fn pick_mal_targets(chain: &[MalEntry], season: i64, ep: i64, abs_ep: i64) -> Vec<(i64, i64)> {
    if chain.is_empty() || ep <= 0 { return vec![]; }
    let mut out: Vec<(i64, i64)> = vec![];
    let mut push = |mid: i64, local_ep: i64| {
        if mid > 0 && local_ep > 0 && !out.contains(&(mid, local_ep)) {
            out.push((mid, local_ep));
        }
    };

    if chain.len() == 1 {
        let target_ep = if abs_ep > 0 { abs_ep } else { ep };
        push(chain[0].id, target_ep);
        if ep != target_ep { push(chain[0].id, ep); }
        return out;
    }

    // 1. Cumulative absolute-episode walk across chronologically sorted TV cours (handles split-cours & long-runners!)
    if abs_ep > 0 {
        let mut cum = 0i64;
        for entry in chain {
            if entry.eps > 0 {
                if abs_ep <= cum + entry.eps {
                    push(entry.id, abs_ep - cum);
                    break;
                }
                cum += entry.eps;
            } else {
                // Currently airing / open-ended cour (e.g., Black Clover S2 2026)
                push(entry.id, (abs_ep - cum).max(1));
                break;
            }
        }
    }

    // 2. Long-running primary MAL heuristic when abs_ep wasn't passed (e.g. Black Clover S2 E52)
    if abs_ep <= 0 && chain[0].eps >= 40 && ep > 1 && ep <= chain[0].eps {
        push(chain[0].id, ep);
    }

    // 3. Season-indexed TV cour fallback (e.g. Solo Leveling S2 E1 without abs_ep)
    if season >= 1 {
        if let Some(entry) = chain.get((season - 1) as usize) {
            if entry.eps == 0 || ep <= entry.eps {
                push(entry.id, ep);
            }
        }
    }

    // 4. Final fallback to first TV entry
    drop(push);
    if out.is_empty() {
        let fallback_ep = if abs_ep > 0 { abs_ep } else { ep };
        if chain[0].id > 0 && fallback_ep > 0 {
            out.push((chain[0].id, fallback_ep));
        }
    }
    out.truncate(2);
    out
}

async fn fetch_aniskip_one(http: &reqwest::Client, mal_id: i64, local_ep: i64) -> Option<EpisodeSkips> {
    // Always query episodeLength=0 (wildcard) because AniSkip enforces a strict ±20s length window when
    // episodeLength > 0, returning 404 on slightly longer/shorter encodes (e.g. AoT 1450s, One Piece 1500s).
    // We validate & re-anchor credits against the actual probed file duration in merge_all().
    let url = format!(
        "https://api.aniskip.com/v2/skip-times/{mal_id}/{local_ep}?types[]=op&types[]=ed&types[]=recap&types[]=mixed-op&types[]=mixed-ed&episodeLength=0"
    );
    let resp = http.get(&url)
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_millis(3500))
        .send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let v: Value = resp.json().await.ok()?;
    let parsed = parse_aniskip(&v);
    if parsed.has_any() { Some(parsed) } else { None }
}

async fn fetch_aniskip(app: &App, tmdb_id: &str, season: i64, ep: i64, abs_ep: i64) -> Option<EpisodeSkips> {
    let chain = resolve_mal_tv_chain(app, tmdb_id).await;
    if chain.is_empty() { return None; }
    for (mal_id, local_ep) in pick_mal_targets(&chain, season, ep, abs_ep) {
        if let Some(hit) = fetch_aniskip_one(&app.http, mal_id, local_ep).await {
            return Some(hit);
        }
    }
    None
}

/// Resolve an episode's crowd-sourced skip timestamps (cached 30 days on hit, 3 days on miss).
/// Uses ZERO IPTV provider connections!
pub async fn resolve_episode(
    app: &App,
    tmdb_id: &str,
    imdb_id: &str,
    season: i64,
    ep: i64,
    abs_ep: i64,
    _dur: f64,
    is_anime: bool,
) -> Option<EpisodeSkips> {
    // Try to fill imdb_id from local meta.json (97.4% hit rate, zero network calls!)
    let mut im = imdb_id.trim().to_string();
    let mut anime = is_anime;
    if !tmdb_id.is_empty() && tmdb_id != "0" {
        if let Some(m) = app.meta_get("tv", tmdb_id) {
            if im.is_empty() && m.im.starts_with("tt") { im = m.im.clone(); }
            if !anime && m.g.contains(&16) && (m.l == "ja" || m.oc.iter().any(|c| c == "JP")) { anime = true; }
        }
    }

    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) {
        // Never hit external skip APIs during offline test runs unless already cached.
        let key = SkipDb::cache_key(tmdb_id, &im, season, ep)?;
        return app.skipdb.get_cached_for(&key, anime);
    }

    let key = SkipDb::cache_key(tmdb_id, &im, season, ep)?;
    if let Some(cached) = app.skipdb.get_cached_for(&key, anime) {
        return if cached.has_any() { Some(cached) } else { None };
    }

    // Query IntroDB + TheIntroDB v2 (+ AniSkip for anime) concurrently!
    let (mut idb, mut tidb, aniskip) = tokio::join!(
        fetch_introdb(&app.http, &im, season, ep),
        fetch_theintrodb(&app.http, tmdb_id, season, ep),
        async {
            if anime { fetch_aniskip(app, tmdb_id, season, ep, abs_ep).await } else { None }
        }
    );

    // Anime TMDB-Season-1 fallback: many long-running anime (e.g. Black Clover S2..S4) are split into
    // multi-seasons by IPTV providers while TMDB/IMDb index them all under Season 1 (S1 E52..E170).
    // If S{season}E{ep} missed on IntroDB/TheIntroDB, retry (S=1, E=abs_ep) for any missing fields!
    let effective_abs = if abs_ep > 0 { abs_ep } else { ep };
    if anime && season > 1 && effective_abs > 0 && (idb.is_none() || tidb.is_none()) {
        let (idb_abs, tidb_abs) = tokio::join!(
            async { if idb.is_none() { fetch_introdb(&app.http, &im, 1, effective_abs).await } else { None } },
            async { if tidb.is_none() { fetch_theintrodb(&app.http, tmdb_id, 1, effective_abs).await } else { None } }
        );
        if idb.is_none() { idb = idb_abs; }
        if tidb.is_none() { tidb = tidb_abs; }
    }

    let mut merged = EpisodeSkips::default();
    if anime {
        if let Some(a) = aniskip { merged = combine_remote(merged, a); }
        if let Some(i) = idb { merged = combine_remote(merged, i); }
        if let Some(t) = tidb { merged = combine_remote(merged, t); }
    } else {
        if let Some(i) = idb { merged = combine_remote(merged, i); }
        if let Some(t) = tidb { merged = combine_remote(merged, t); }
    }
    merged.t = now_ms();
    merged.v = CURRENT_SKIP_VER;
    if !merged.has_any() { merged.source = "none".into(); }
    app.skipdb.put_cached(key, merged.clone());
    if merged.has_any() { Some(merged) } else { None }
}

/// Fire-and-forget lookahead: whenever the user plays S:E, pre-cache E+1..E+3 from public skip DBs
/// (uses ZERO IPTV provider connections!) so auto-next has instant 0ms skip timestamps ready.
pub fn spawn_prefetch_ahead(app: Arc<App>, tmdb_id: String, imdb_id: String, season: i64, ep: i64, abs_ep: i64, is_anime: bool) {
    if (tmdb_id.is_empty() && !imdb_id.starts_with("tt")) || season < 0 || ep <= 0 { return; }
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) { return; }
    tokio::spawn(async move {
        for delta in 1..=3 {
            let next_ep = ep + delta;
            let next_abs = if abs_ep > 0 { abs_ep + delta } else { 0 };
            let Some(key) = SkipDb::cache_key(&tmdb_id, &imdb_id, season, next_ep) else { break };
            if app.skipdb.get_cached_for(&key, is_anime).is_some() { continue; }
            let _ = resolve_episode(&app, &tmdb_id, &imdb_id, season, next_ep, next_abs, 0.0, is_anime).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        app.skipdb.save_if_dirty(true);
    });
}

/// Gentle background collection pre-warmer: populates skip timestamps for shows in Continue Watching,
/// Follows, My List, top Anime, and top English TV shows without EVER touching the IPTV provider connection.
pub async fn prefetch_worker(app: Arc<App>) {
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) { return; }
    let cat = app.catalog();
    if cat.series.is_empty() { return; }

    // 1. Priority: shows in progress / follows / my list / wanted
    let mut todo_eps: Vec<(String, String, i64, i64, i64, bool)> = vec![];
    let mut seen_show: HashSet<String> = HashSet::new();

    let (prog_sids, list_sids): (Vec<(i64, i64, i64)>, Vec<i64>) = app.state.read(|st| {
        let mut p_list: Vec<(i64, i64, i64, i64)> = vec![];
        if let Some(m) = st["progress"].as_object() {
            for (k, v) in m {
                if k.starts_with("e:") {
                    if let Some(sid) = v["item"]["sid"].as_i64() {
                        let sub = v["item"]["sub"].as_str().unwrap_or("");
                        let (sn, en) = parse_se_from_sub(sub).unwrap_or((1, 1));
                        p_list.push((sid, sn, en, v["t"].as_i64().unwrap_or(0)));
                    }
                }
            }
        }
        p_list.sort_by_key(|x| -x.3);
        let prog: Vec<(i64, i64, i64)> = p_list.into_iter().map(|(s, sn, en, _)| (s, sn, en)).collect();
        let mut l_list: Vec<i64> = vec![];
        for key in ["follows", "list", "wanted"] {
            if let Some(m) = st[key].as_object() {
                for (mk, v) in m {
                    if key == "follows" {
                        if let Ok(sid) = mk.parse::<i64>() { if !l_list.contains(&sid) { l_list.push(sid); } }
                    } else if v["item"]["k"].as_str() == Some("s") {
                        if let Some(sid) = v["item"]["id"].as_i64() { if !l_list.contains(&sid) { l_list.push(sid); } }
                    }
                }
            }
        }
        (prog, l_list)
    });

    let push_show_eps = |todo: &mut Vec<(String, String, i64, i64, i64, bool)>, tmdb: &str, im: &str, is_anime: bool, season: i64, from_ep: i64, count: i64| {
        for e in from_ep..(from_ep + count) {
            if e <= 0 { continue; }
            if let Some(k) = SkipDb::cache_key(tmdb, im, season, e) {
                if app.skipdb.get_cached_for(&k, is_anime).is_none() {
                    let abs = if season == 1 { e } else { 0 };
                    todo.push((tmdb.to_string(), im.to_string(), season, e, abs, is_anime));
                }
            }
        }
    };

    for (sid, sn, en) in prog_sids.into_iter().take(12) {
        let Some(it) = cat.series_item(sid) else { continue };
        let Some(ref tmdb) = it.tmdb else { continue };
        if !seen_show.insert(tmdb.clone()) { continue; }
        let meta = app.meta_get("tv", tmdb);
        let im = meta.as_ref().map(|m| m.im.clone()).unwrap_or_default();
        let anime = crate::anime::is_anime_item(&app, it);
        push_show_eps(&mut todo_eps, tmdb, &im, anime, sn, en, 5);
    }
    for sid in list_sids.into_iter().take(20) {
        let Some(it) = cat.series_item(sid) else { continue };
        let Some(ref tmdb) = it.tmdb else { continue };
        if !seen_show.insert(tmdb.clone()) { continue; }
        let meta = app.meta_get("tv", tmdb);
        let im = meta.as_ref().map(|m| m.im.clone()).unwrap_or_default();
        let anime = crate::anime::is_anime_item(&app, it);
        push_show_eps(&mut todo_eps, tmdb, &im, anime, 1, 1, 3);
    }

    // 2. Top popular Anime series (so clicking Play on any flagship anime has 0ms AniSkip OP/ED ready!)
    if todo_eps.len() < 45 {
        let mut pop_anime: Vec<(f64, String, String)> = cat.series.iter()
            .filter(|s| crate::anime::is_anime_item(&app, s))
            .filter_map(|s| {
                let tmdb = s.tmdb.as_ref()?;
                if seen_show.contains(tmdb) { return None; }
                let m = app.meta_get("tv", tmdb)?;
                Some((m.p + m.va * 6.0, tmdb.clone(), m.im.clone()))
            })
            .collect();
        pop_anime.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, tmdb, im) in pop_anime.into_iter().take(18) {
            if !seen_show.insert(tmdb.clone()) { continue; }
            push_show_eps(&mut todo_eps, &tmdb, &im, true, 1, 1, 2);
            if todo_eps.len() >= 45 { break; }
        }
    }

    // 3. Top popular English TV shows in the catalog
    if todo_eps.len() < 55 {
        let mut pop_shows: Vec<(f64, String, String, bool)> = cat.series.iter()
            .filter(|s| s.en && !s.sport)
            .filter_map(|s| {
                let tmdb = s.tmdb.as_ref()?;
                if seen_show.contains(tmdb) { return None; }
                let m = app.meta_get("tv", tmdb)?;
                if m.im.is_empty() && m.p < 20.0 { return None; }
                let anime = crate::anime::is_anime_item(&app, s);
                Some((m.p + m.va * 5.0, tmdb.clone(), m.im.clone(), anime))
            })
            .collect();
        pop_shows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, tmdb, im, anime) in pop_shows.into_iter().take(40) {
            if !seen_show.insert(tmdb.clone()) { continue; }
            push_show_eps(&mut todo_eps, &tmdb, &im, anime, 1, 1, 2);
            if todo_eps.len() >= 60 { break; }
        }
    }

    let mut fetched = 0usize;
    let mut hits = 0usize;
    for (tmdb, im, sn, en, abs, anime) in todo_eps.into_iter().take(60) {
        if let Some(res) = resolve_episode(&app, &tmdb, &im, sn, en, abs, 0.0, anime).await {
            if res.has_any() { hits += 1; }
        }
        fetched += 1;
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    if fetched > 0 {
        app.skipdb.save_if_dirty(true);
        println!("[skipdb] prefetched {fetched} TV/Anime episodes ({hits} matched from AniSkip/IntroDB/TheIntroDB)");
    }
}

fn parse_se_from_sub(sub: &str) -> Option<(i64, i64)> {
    let caps = crate::re!(r"(?i)S(\d+)\s*[:xE]\s*E?(\d+)").captures(sub)?;
    let s: i64 = caps.get(1)?.as_str().parse().ok()?;
    let e: i64 = caps.get(2)?.as_str().parse().ok()?;
    Some((s, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_introdb_segments() {
        let j = json!({
            "imdb_id": "tt0944947",
            "season": 1,
            "episode": 2,
            "intro": { "start_sec": 6, "end_sec": 105, "confidence": 1 },
            "recap": { "start_sec": 0, "end_sec": 45 },
            "outro": { "start_sec": 3252, "end_sec": 3320 },
            "post_credits": null
        });
        let p = parse_introdb(&j);
        assert_eq!(p.intro, Some(SkipSeg { s: 6.0, e: 105.0 }));
        assert_eq!(p.recap, Some(SkipSeg { s: 0.0, e: 45.0 }));
        assert_eq!(p.credits, Some(3252.0));
        assert_eq!(p.credits_end, Some(3320.0));
        assert_eq!(p.source, "introdb");
    }

    #[test]
    fn parses_theintrodb_v2_and_rejects_zero_len() {
        let j = json!({
            "tmdb_id": 85937,
            "intro": [{ "start_ms": 57663, "end_ms": 147091 }],
            "recap": [{ "start_ms": 0, "end_ms": 0 }],
            "credits": [{ "start_ms": 1290172, "end_ms": 1380067 }]
        });
        let p = parse_theintrodb(&j);
        assert_eq!(p.intro, Some(SkipSeg { s: 57.7, e: 147.1 }));
        assert_eq!(p.recap, None);
        assert_eq!(p.credits, Some(1290.2));
        assert_eq!(p.credits_end, Some(1380.1));

        // start_ms: null means start at 0s (Fallout S1E2 pattern)
        let j2 = json!({
            "intro": [{ "start_ms": null, "end_ms": 16000 }],
            "credits": [{ "start_ms": 3591482, "end_ms": null }]
        });
        let p2 = parse_theintrodb(&j2);
        assert_eq!(p2.intro, Some(SkipSeg { s: 0.0, e: 16.0 }));
        assert_eq!(p2.credits, Some(3591.5));
        assert_eq!(p2.credits_end, None);
    }

    #[test]
    fn parses_aniskip_op_ed_recap() {
        let j = json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 54.312, "endTime": 144.312 }, "episodeLength": 1450.032 },
                { "skipType": "ed", "interval": { "startTime": 1372.218, "endTime": 1451.0 }, "episodeLength": 1450.032 },
                { "skipType": "recap", "interval": { "startTime": 0.0, "endTime": 42.5 }, "episodeLength": 1450.032 }
            ]
        });
        let p = parse_aniskip(&j);
        assert_eq!(p.intro, Some(SkipSeg { s: 54.3, e: 144.3 }));
        assert_eq!(p.recap, Some(SkipSeg { s: 0.0, e: 42.5 }));
        assert_eq!(p.credits, Some(1372.2));
        assert_eq!(p.credits_end, Some(1451.0));
        assert_eq!(p.source, "aniskip");
    }

    #[test]
    fn merge_respects_priority_and_pal_credit_reanchoring() {
        let remote = EpisodeSkips {
            recap: Some(SkipSeg { s: 0.0, e: 35.0 }),
            intro: Some(SkipSeg { s: 45.0, e: 120.0 }),
            credits: Some(3252.0),
            credits_end: Some(3320.0), // 68s credits on 3320s NTSC encode
            post_credits: None,
            ep_len: Some(3320.0),
            source: "introdb".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
        };
        // PAL 25fps encode is 3185s (4% shorter): credits scale proportionally (~3119.8s)
        let ch = vec![json!({ "s": 42.0, "e": 118.0, "title": "Opening Credits" })];
        let m = merge_all(&ch, None, Some((45.0, 125.0)), None, Some(&remote), 3185.0);
        // Chapter intro wins over audio & remote
        assert_eq!(m.intro, Some((42.0, 118.0)));
        // Remote recap fills missing chapter recap (gap 42-35=7s > 6s, kept separate)
        assert_eq!(m.recap, Some((0.0, 35.0)));
        // PAL-scaled credits from remote!
        let cr = m.credits.expect("credits");
        assert!((cr - 3119.8).abs() < 4.0, "expected PAL credits ~3119.8, got {cr}");
        assert_eq!(m.skips.len(), 2);
        assert_eq!(m.skips[0]["title"], "Recap");
        assert_eq!(m.skips[1]["title"], "Intro");
    }

    #[test]
    fn anime_mal_chain_maps_long_runners_and_split_cours() {
        // Black Clover TV chain after filtering out Jump Festa 2016 Special (33950)
        let bc = vec![
            MalEntry { id: 34572, eps: 170, start: "2017-10-03".into() },
            MalEntry { id: 61967, eps: 0,   start: "2026-10-03".into() },
        ];
        assert_eq!(pick_mal_targets(&bc, 1, 1, 1)[0], (34572, 1));
        assert_eq!(pick_mal_targets(&bc, 2, 52, 52)[0], (34572, 52));
        assert_eq!(pick_mal_targets(&bc, 3, 103, 103)[0], (34572, 103));
        assert_eq!(pick_mal_targets(&bc, 4, 170, 170)[0], (34572, 170));
        // Even without abs_ep, S2E52 maps to main 170-ep MAL 34572!
        assert_eq!(pick_mal_targets(&bc, 2, 52, 0)[0], (34572, 52));
        // Black Clover 2026 Season 2 (abs 171..179 -> MAL 61967 ep 1..9)
        assert_eq!(pick_mal_targets(&bc, 2, 1, 171)[0], (61967, 1));
        assert_eq!(pick_mal_targets(&bc, 5, 9, 179)[0], (61967, 9));

        // Demon Slayer split-cour TV chain (Mugen Train 7 eps + Entertainment District 11 eps)
        let ds = vec![
            MalEntry { id: 38000, eps: 26, start: "2019-04-06".into() },
            MalEntry { id: 49926, eps: 7,  start: "2021-10-10".into() },
            MalEntry { id: 47778, eps: 11, start: "2021-12-05".into() },
            MalEntry { id: 51019, eps: 11, start: "2023-04-09".into() },
            MalEntry { id: 55701, eps: 8,  start: "2024-05-12".into() },
        ];
        assert_eq!(pick_mal_targets(&ds, 1, 19, 19)[0], (38000, 19));
        assert_eq!(pick_mal_targets(&ds, 2, 2, 28)[0], (49926, 2));   // Mugen Train Ep 2
        assert_eq!(pick_mal_targets(&ds, 2, 8, 34)[0], (47778, 1));   // Entertainment District Ep 1!
        assert_eq!(pick_mal_targets(&ds, 3, 1, 45)[0], (51019, 1));   // Swordsmith Village Ep 1!
        assert_eq!(pick_mal_targets(&ds, 4, 1, 56)[0], (55701, 1));   // Hashira Training Ep 1!

        // Attack on Titan split-cour TV chain (OVAs/Movies filtered out)
        let aot = vec![
            MalEntry { id: 16498, eps: 25, start: "2013-04-07".into() },
            MalEntry { id: 25777, eps: 12, start: "2017-04-01".into() },
            MalEntry { id: 35760, eps: 12, start: "2018-07-23".into() },
            MalEntry { id: 38524, eps: 10, start: "2019-04-29".into() },
            MalEntry { id: 40028, eps: 16, start: "2020-12-07".into() },
            MalEntry { id: 48583, eps: 12, start: "2022-01-10".into() },
        ];
        assert_eq!(pick_mal_targets(&aot, 2, 1, 26)[0], (25777, 1));
        assert_eq!(pick_mal_targets(&aot, 3, 13, 50)[0], (38524, 1)); // S3 Part 2 Ep 1!
        assert_eq!(pick_mal_targets(&aot, 4, 1, 60)[0], (40028, 1));
    }

    #[test]
    fn anime_recap_op_chaining_dedupe_and_post_credits() {
        // 1. Solo Leveling S1E2 / Black Clover Ep 156: overlapping Recap + OP merges into ONE jump
        let r1 = EpisodeSkips {
            recap: Some(SkipSeg { s: 0.0, e: 90.3 }),
            intro: Some(SkipSeg { s: 87.5, e: 177.5 }),
            credits: Some(1294.7),
            credits_end: Some(1384.7), // 56s left for Petit Clover + Next Episode Preview!
            ep_len: Some(1441.0),
            source: "aniskip".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
            ..Default::default()
        };
        let m1 = merge_all(&[], None, None, None, Some(&r1), 1440.0);
        assert_eq!(m1.skips.len(), 1);
        assert_eq!(m1.skips[0]["title"], "Recap + Intro");
        assert_eq!(m1.skips[0]["s"], 0.0);
        assert_eq!(m1.skips[0]["e"], 177.5);
        assert_eq!(m1.credits, Some(1294.7));
        assert_eq!(m1.credits_end, Some(1384.7));
        assert!(m1.has_post_credits, "55s tail after ED should flag post-credits/preview");

        // 2. Black Clover Ep 52 / JJK S2E2: bogus recap sitting inside OP gets dropped
        let r2 = EpisodeSkips {
            recap: Some(SkipSeg { s: 2.5, e: 92.5 }),
            intro: Some(SkipSeg { s: 5.2, e: 95.2 }),
            source: "aniskip".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
            ..Default::default()
        };
        let m2 = merge_all(&[], None, None, None, Some(&r2), 1435.0);
        assert_eq!(m2.recap, None);
        assert_eq!(m2.skips.len(), 1);
        assert_eq!(m2.skips[0]["title"], "Intro");

        // 3. One Piece S1E1: Recap AFTER Intro with real scene in between (>6s gap) stays 2 chronological skips
        let r3 = EpisodeSkips {
            intro: Some(SkipSeg { s: 28.8, e: 118.8 }),
            recap: Some(SkipSeg { s: 132.8, e: 201.3 }),
            source: "aniskip".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
            ..Default::default()
        };
        let m3 = merge_all(&[], None, None, None, Some(&r3), 1500.0);
        assert_eq!(m3.skips.len(), 2);
        assert_eq!(m3.skips[0]["title"], "Intro");
        assert_eq!(m3.skips[1]["title"], "Recap");
    }
}
