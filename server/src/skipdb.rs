// Multi-source TV & Anime Intro / Recap / Ending / Credits Skip Resolver & Persistent Cache.
//
// Combines three free, zero-API-key crowd-sourced timestamp databases (requiring ZERO IPTV
// provider connections!) with local MKV chapter markers and local audio fingerprinting:
//   1. MKV embedded chapters (100% file-accurate)
//   2. Local audio fingerprint matches (intro.rs — exact to the provider's actual stream file)
//   3. AniSkip (for Anime: OP / ED / Recap / Mixed-OP / Mixed-ED via ARM TMDB -> MAL TV chain)
//   4. IntroDB (api.introdb.app/segments via IMDb ID — intro, recap, outro, post_credits + confidence/subs)
//   5. TheIntroDB v2 (api.theintrodb.org/v2/media via TMDB ID — intro, recap, credits, preview)
//
// v3 Anime Accuracy Guarantees:
//   - 3-way consensus across AniSkip + IntroDB + TheIntroDB (never mixes timestamps from conflicting cuts)
//   - Drops uncorroborated single-submission IntroDB/TheIntroDB "recap" entries & all S1E1 remote recaps
//   - Marks mixed-op / mixed-ed and >25s conflicting remote intros `auto: false` (manual button only)
//   - Separates in-episode Anime EDs followed by substantial C-part story (>= 35s left) from true final
//     credits: skips the 90s ED song straight to the C-part story instead of cutting the episode off early!
//   - Exact-duration AniSkip matching first (falling back to wildcard episodeLength=0 on 404)

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
const CURRENT_SKIP_VER: u8 = 3; // v3: 3-way consensus, mixed/uncertain gating, C-part ED vs final_outro separation
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

fn is_false(b: &bool) -> bool { !*b }
fn is_zero_u32(n: &u32) -> bool { *n == 0 }

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
    #[serde(default, skip_serializing_if = "is_false")]
    pub recap_auto: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<SkipSeg>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub intro_mixed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub intro_uncertain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_end: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub credits_mixed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_outro: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_credits: Option<SkipSeg>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ep_len: Option<f64>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub intro_subs: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub recap_subs: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub outro_subs: u32,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub t: i64,
    #[serde(default)]
    pub v: u8,
}

impl EpisodeSkips {
    pub fn has_any(&self) -> bool {
        self.recap.is_some() || self.intro.is_some() || self.credits.is_some() || self.final_outro.is_some()
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
            if v.credits.is_some() || v.final_outro.is_some() { with_credits += 1; }
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

fn round1(x: f64) -> f64 { (x * 10.0).round() / 10.0 }

fn clean_seg(s: f64, e: f64, min_len: f64, max_len: f64) -> Option<SkipSeg> {
    if !s.is_finite() || !e.is_finite() || s < 0.0 || e <= s { return None; }
    let len = e - s;
    if len < min_len || len > max_len { return None; }
    Some(SkipSeg { s: round1(s), e: round1(e) })
}

/// Parse IntroDB (`https://api.introdb.app/segments?imdb_id=...&season=...&episode=...`).
pub fn parse_introdb(v: &Value) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    let seg_from = |obj: &Value, min_l: f64, max_l: f64| -> Option<(SkipSeg, u32)> {
        if !obj.is_object() { return None; }
        if let Some(conf) = obj["confidence"].as_f64() {
            if conf < 0.5 { return None; }
        }
        let s = obj["start_sec"].as_f64().or_else(|| obj["start_ms"].as_f64().map(|m| m / 1000.0))?;
        let e = obj["end_sec"].as_f64().or_else(|| obj["end_ms"].as_f64().map(|m| m / 1000.0))?;
        let subs = obj["submission_count"].as_u64().unwrap_or(0) as u32;
        Some((clean_seg(s, e, min_l, max_l)?, subs))
    };
    if let Some((rc, subs)) = seg_from(&v["recap"], 5.0, 240.0) {
        out.recap = Some(rc);
        out.recap_subs = subs;
        out.recap_auto = subs >= 2;
    }
    if let Some((intro, subs)) = seg_from(&v["intro"], 5.0, 210.0) {
        out.intro = Some(intro);
        out.intro_subs = subs;
    }
    if let Some(o) = v.get("outro").filter(|x| x.is_object()) {
        let conf_ok = o["confidence"].as_f64().map_or(true, |c| c >= 0.5);
        if conf_ok {
            if let Some(st) = o["start_sec"].as_f64().or_else(|| o["start_ms"].as_f64().map(|m| m / 1000.0)) {
                if st.is_finite() && st > 60.0 {
                    out.credits = Some(round1(st));
                    out.outro_subs = o["submission_count"].as_u64().unwrap_or(0) as u32;
                    if let Some(en) = o["end_sec"].as_f64().or_else(|| o["end_ms"].as_f64().map(|m| m / 1000.0)) {
                        if en.is_finite() && en > st + 5.0 {
                            out.credits_end = Some(round1(en));
                        }
                    }
                }
            }
        }
    }
    if let Some((pc, _)) = seg_from(&v["post_credits"], 5.0, 300.0) { out.post_credits = Some(pc); }
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
                    out.credits = Some(round1(st));
                    if let Some(en) = item["end_ms"].as_f64().map(|m| m / 1000.0) {
                        if en.is_finite() && en > st + 5.0 {
                            out.credits_end = Some(round1(en));
                        }
                    }
                    break;
                }
            }
        }
    }
    if let Some(arr) = v["preview"].as_array() {
        for item in arr {
            if let Some(st) = item["start_ms"].as_f64().map(|m| m / 1000.0) {
                if st.is_finite() && st > 120.0 {
                    let en = item["end_ms"].as_f64().map(|m| m / 1000.0).filter(|&x| x > st + 4.0).unwrap_or(st + 25.0);
                    if let Some(seg) = clean_seg(st, en, 4.0, 180.0) {
                        out.post_credits = Some(seg);
                        break;
                    }
                }
            }
        }
    }
    if out.has_any() { out.source = "theintrodb".into(); }
    out
}

/// Parse AniSkip (`https://api.aniskip.com/v2/skip-times/{mal_id}/{ep}?types[]=...`).
/// Prefers pure `op`/`ed` over `mixed-op`/`mixed-ed` and explicitly tracks `intro_mixed`/`credits_mixed`
/// so dialogue-over-music sequences are NEVER destructively auto-skipped.
pub fn parse_aniskip(v: &Value) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    if v["found"].as_bool() != Some(true) { return out; }
    let Some(results) = v["results"].as_array() else { return out };
    let mut mixed_op: Option<SkipSeg> = None;
    let mut mixed_ed: Option<(f64, Option<f64>)> = None;
    let mut op_len: Option<f64> = None;
    let mut ed_len: Option<f64> = None;
    for r in results {
        let stype = r["skipType"].as_str().unwrap_or("");
        let iv = &r["interval"];
        let (Some(s), Some(e)) = (iv["startTime"].as_f64(), iv["endTime"].as_f64()) else { continue };
        let el = r["episodeLength"].as_f64().filter(|x| *x > 120.0);
        match stype {
            "op" if out.intro.is_none() => {
                if let Some(seg) = clean_seg(s, e, 10.0, 210.0) {
                    out.intro = Some(seg);
                    if op_len.is_none() { op_len = el; }
                }
            }
            "mixed-op" if mixed_op.is_none() => {
                if let Some(seg) = clean_seg(s, e, 10.0, 210.0) {
                    mixed_op = Some(seg);
                    if op_len.is_none() { op_len = el; }
                }
            }
            "recap" if out.recap.is_none() => {
                out.recap = clean_seg(s, e, 5.0, 260.0);
            }
            "ed" if out.credits.is_none() => {
                if s.is_finite() && s > 60.0 {
                    out.credits = Some(round1(s));
                    if e.is_finite() && e > s + 5.0 {
                        out.credits_end = Some(round1(e));
                    }
                    if ed_len.is_none() { ed_len = el; }
                }
            }
            "mixed-ed" if mixed_ed.is_none() => {
                if s.is_finite() && s > 60.0 {
                    let st = round1(s);
                    let en = (e.is_finite() && e > s + 5.0).then(|| round1(e));
                    mixed_ed = Some((st, en));
                    if ed_len.is_none() { ed_len = el; }
                }
            }
            _ => {}
        }
        if out.ep_len.is_none() { out.ep_len = el; }
    }
    if out.intro.is_none() {
        if let Some(m) = mixed_op {
            out.intro = Some(m);
            out.intro_mixed = true;
        }
    }
    if out.credits.is_none() {
        if let Some((st, en)) = mixed_ed {
            out.credits = Some(st);
            out.credits_end = en;
            out.credits_mixed = true;
        }
    }
    if let Some(el) = op_len.or(ed_len) { out.ep_len = Some(el); }
    if out.has_any() { out.source = "aniskip".into(); }
    out
}

/// Merge two remote EpisodeSkips for Western/general TV (`primary` wins per field; `secondary` fills missing).
pub fn combine_remote(mut primary: EpisodeSkips, secondary: EpisodeSkips) -> EpisodeSkips {
    let mut used_sec = false;
    if primary.recap.is_none() && secondary.recap.is_some() {
        primary.recap = secondary.recap;
        primary.recap_auto = true; // standard Western TV recaps ("Previously on...")
        primary.recap_subs = secondary.recap_subs;
        used_sec = true;
    } else if primary.recap.is_some() {
        primary.recap_auto = true;
    }
    if primary.intro.is_none() && secondary.intro.is_some() {
        primary.intro = secondary.intro;
        primary.intro_mixed = secondary.intro_mixed;
        primary.intro_uncertain = secondary.intro_uncertain;
        primary.intro_subs = secondary.intro_subs;
        used_sec = true;
    }
    if primary.credits.is_none() && secondary.credits.is_some() {
        primary.credits = secondary.credits;
        primary.credits_end = secondary.credits_end;
        primary.credits_mixed = secondary.credits_mixed;
        primary.outro_subs = secondary.outro_subs;
        used_sec = true;
    } else if primary.credits.is_some() && primary.credits_end.is_none() && secondary.credits_end.is_some() {
        primary.credits_end = secondary.credits_end;
    }
    if primary.final_outro.is_none() && secondary.final_outro.is_some() {
        primary.final_outro = secondary.final_outro;
        used_sec = true;
    }
    if primary.post_credits.is_none() && secondary.post_credits.is_some() {
        primary.post_credits = secondary.post_credits;
        used_sec = true;
    }
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

fn segs_close(a: &SkipSeg, b: &SkipSeg, tol: f64) -> bool {
    (a.s - b.s).abs() <= tol && (a.e - b.e).abs() <= tol
}

fn seg_overlap_ratio(inner: &SkipSeg, other: &SkipSeg) -> f64 {
    let ov = (inner.e.min(other.e) - inner.s.max(other.s)).max(0.0);
    ov / (inner.e - inner.s).max(1.0)
}

/// IntroDB and TheIntroDB occasionally mirror single-submission entries to the exact 0.1s.
/// Two entries count as *independent* corroboration if their timestamps aren't bit-identical
/// OR if IntroDB has >= 2 human submissions.
fn independent_pair(i: &SkipSeg, t: &SkipSeg, introdb_subs: u32, tol: f64) -> bool {
    if !segs_close(i, t, tol) { return false; }
    if introdb_subs >= 2 { return true; }
    (i.s - t.s).abs() > 0.15 || (i.e - t.e).abs() > 0.15
}

fn is_valid_anime_head(seg: &SkipSeg, dur: f64) -> bool {
    let len = seg.e - seg.s;
    if len < 10.0 || len > 200.0 { return false; }
    if dur > 120.0 {
        seg.e <= dur * 0.48
    } else {
        // Even before duration is probed, an anime OP never starts after 11 minutes
        seg.s < 660.0
    }
}

fn op_len_score(seg: &SkipSeg) -> f64 {
    // Classic anime TV OP is ~89-91s; smaller score = closer to canonical anime OP
    ((seg.e - seg.s) - 90.0).abs()
}

/// 3-way consensus resolver specifically tuned for Anime (AniSkip + IntroDB + TheIntroDB v2).
/// Eliminates the exact failure modes that ruin anime auto-skip:
///   1. Cross-cut Frankenskips (never imports a recap from a source whose OP disagrees by >15s)
///   2. Bogus 1-submission IntroDB/TheIntroDB cold-open "recaps" & Episode 1 recaps
///   3. Wildly conflicting cuts (marks `intro_uncertain = true` -> manual skip button instead of blind jump)
///   4. Lone AniSkip outlier when IntroDB + TheIntroDB independently agree (e.g. Black Clover E103)
///   5. Preserves `final_outro` separately from an earlier in-episode ED song (e.g. 7DS S1E3/E5)
pub fn combine_anime_sources(
    aniskip: Option<EpisodeSkips>,
    introdb: Option<EpisodeSkips>,
    theintrodb: Option<EpisodeSkips>,
    season: i64,
    ep: i64,
    dur: f64,
) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    let mut srcs: Vec<&str> = vec![];
    let mut add_src = |name: &'static str| {
        if !srcs.contains(&name) { srcs.push(name); }
    };

    let a_i = aniskip.as_ref().and_then(|x| x.intro.as_ref()).filter(|s| is_valid_anime_head(s, dur));
    let i_i = introdb.as_ref().and_then(|x| x.intro.as_ref()).filter(|s| is_valid_anime_head(s, dur));
    let t_i = theintrodb.as_ref().and_then(|x| x.intro.as_ref()).filter(|s| is_valid_anime_head(s, dur));
    let i_intro_subs = introdb.as_ref().map_or(0, |x| x.intro_subs);
    let a_intro_mixed = aniskip.as_ref().map_or(false, |x| x.intro_mixed);

    // --- 1. Anime Opening (OP) 3-way consensus ---
    let ai_agree = matches!((a_i, i_i), (Some(a), Some(i)) if segs_close(a, i, 12.0));
    let at_agree = matches!((a_i, t_i), (Some(a), Some(t)) if segs_close(a, t, 12.0));
    let it_indep = matches!((i_i, t_i), (Some(i), Some(t)) if independent_pair(i, t, i_intro_subs, 10.0));
    let it_close = matches!((i_i, t_i), (Some(i), Some(t)) if segs_close(i, t, 10.0));

    if let Some(a) = a_i {
        if ai_agree || at_agree {
            // High-confidence consensus anchored on AniSkip's crisp 90s OP timestamps
            out.intro = Some(a.clone());
            out.intro_mixed = a_intro_mixed;
            add_src("aniskip");
            if ai_agree { add_src("introdb"); }
            if at_agree { add_src("theintrodb"); }
        } else if it_indep && i_i.map_or(false, |i| (a.s - i.s).abs() > 25.0) {
            // Two independent human DBs agree strongly while AniSkip is a >25s lone outlier (Black Clover E103!)
            let i_seg = i_i.unwrap();
            let t_seg = t_i.unwrap();
            let chosen = if op_len_score(i_seg) <= op_len_score(t_seg) + 2.0 { i_seg } else { t_seg };
            out.intro = Some(chosen.clone());
            out.intro_subs = i_intro_subs;
            add_src("introdb");
            add_src("theintrodb");
        } else {
            // Check whether other DBs wildly conflict (>25s away = multiple incompatible cuts like 7DS S1E4!)
            let conflict_i = i_i.map_or(false, |i| (a.s - i.s).abs() > 25.0);
            let conflict_t = t_i.map_or(false, |t| (a.s - t.s).abs() > 25.0);
            out.intro = Some(a.clone());
            out.intro_mixed = a_intro_mixed;
            out.intro_uncertain = conflict_i || conflict_t;
            add_src("aniskip");
        }
    } else if let (Some(i), Some(t)) = (i_i, t_i) {
        if it_close {
            let chosen = if op_len_score(i) <= op_len_score(t) + 2.0 { i } else { t };
            out.intro = Some(chosen.clone());
            out.intro_subs = i_intro_subs;
            // If both are mirrored 1-sub and bloated (>115s), mark uncertain
            out.intro_uncertain = !it_indep && (chosen.e - chosen.s) > 115.0;
            add_src("introdb");
            if it_indep { add_src("theintrodb"); }
        } else {
            // IntroDB & TheIntroDB disagree with no AniSkip tiebreaker
            let chosen = if op_len_score(i) <= op_len_score(t) { i } else { t };
            out.intro = Some(chosen.clone());
            out.intro_subs = i_intro_subs;
            out.intro_uncertain = (i.s - t.s).abs() > 25.0;
            add_src(if std::ptr::eq(chosen, i) { "introdb" } else { "theintrodb" });
        }
    } else if let Some(i) = i_i {
        out.intro = Some(i.clone());
        out.intro_subs = i_intro_subs;
        if i_intro_subs <= 1 && (i.e - i.s) > 115.0 { out.intro_uncertain = true; }
        add_src("introdb");
    } else if let Some(t) = t_i {
        out.intro = Some(t.clone());
        // Single uncorroborated TheIntroDB anime intro over 115s (e.g. 7DS S1E1 0..155.4s) -> manual button only
        if (t.e - t.s) > 115.0 { out.intro_uncertain = true; }
        add_src("theintrodb");
    }

    // --- 2. Strict Anime Recap Hygiene ---
    // Episode 1 of a series never has a recap.
    if !(season == 1 && ep == 1) {
        let same_cut_as = |src_intro: Option<&SkipSeg>| -> bool {
            match (&out.intro, src_intro) {
                (Some(chosen), Some(si)) => (chosen.s - si.s).abs() <= 15.0,
                (Some(_), None) => true,
                (None, _) => true,
            }
        };
        let recap_ok = |rc: &SkipSeg| -> bool {
            let len = rc.e - rc.s;
            if len < 8.0 || len > 180.0 { return false; }
            if dur > 120.0 && rc.e > dur * 0.45 { return false; }
            if let Some(ref intro) = out.intro {
                if seg_overlap_ratio(rc, intro) >= 0.35 { return false; }
            }
            true
        };

        let a_rc = aniskip.as_ref().and_then(|x| x.recap.as_ref()).filter(|r| recap_ok(r) && same_cut_as(a_i));
        let i_rc = introdb.as_ref().and_then(|x| x.recap.as_ref()).filter(|r| recap_ok(r) && same_cut_as(i_i));
        let t_rc = theintrodb.as_ref().and_then(|x| x.recap.as_ref()).filter(|r| recap_ok(r) && same_cut_as(t_i));
        let i_rc_subs = introdb.as_ref().map_or(0, |x| x.recap_subs);

        if let Some(arc) = a_rc {
            let corroborated = i_rc.map_or(false, |ir| segs_close(arc, ir, 10.0))
                || t_rc.map_or(false, |tr| segs_close(arc, tr, 10.0));
            out.recap = Some(arc.clone());
            // Single-source AniSkip recap shows manual "Skip Recap" button; corroborated recap auto-skips!
            out.recap_auto = corroborated;
            add_src("aniskip");
        } else if let Some(irc) = i_rc {
            // Require >= 2 human submissions OR independent (non-mirrored) agreement with TheIntroDB!
            // Completely eliminates 1-submission bogus cold-open recaps (7DS S1E2-E6, Solo Leveling S1E2, Black Clover E103)!
            let indep_t = t_rc.map_or(false, |tr| independent_pair(irc, tr, i_rc_subs, 10.0));
            if i_rc_subs >= 2 || indep_t {
                out.recap = Some(irc.clone());
                out.recap_subs = i_rc_subs;
                out.recap_auto = true;
                add_src("introdb");
                if indep_t { add_src("theintrodb"); }
            }
        }
        // Single uncorroborated TheIntroDB recap with no AniSkip/IntroDB confirmation is intentionally skipped.
    }

    // --- 3. Anime Ending (ED) & Final Outro Separation ---
    let cred_ok = |st: f64| -> bool {
        if !st.is_finite() || st < 180.0 { return false; }
        if dur > 120.0 && (st < dur * 0.68 || st > dur - 5.0) { return false; }
        if let Some(ref intro) = out.intro {
            if st < intro.e + 30.0 { return false; }
        }
        true
    };

    let a_ed = aniskip.as_ref().and_then(|x| x.credits.filter(|&c| cred_ok(c)).map(|c| (c, x.credits_end, x.credits_mixed)));
    let i_ed = introdb.as_ref().and_then(|x| x.credits.filter(|&c| cred_ok(c)).map(|c| (c, x.credits_end, x.outro_subs)));
    let t_ed = theintrodb.as_ref().and_then(|x| x.credits.filter(|&c| cred_ok(c)).map(|c| (c, x.credits_end)));

    if let Some((cs, ce, mixed)) = a_ed {
        out.credits = Some(cs);
        out.credits_end = ce;
        out.credits_mixed = mixed;
        add_src("aniskip");
        // If IntroDB or TheIntroDB has a *later* true end-of-episode credits card after AniSkip's ED
        // (e.g. 7DS S1E3/E5: AniSkip ED 1278..1367, IntroDB final Netflix outro 1452..1471), preserve it!
        if let Some(ed_end) = ce {
            for cand in [i_ed.map(|x| x.0), t_ed.map(|x| x.0)].into_iter().flatten() {
                if cand >= ed_end + 15.0 {
                    out.final_outro = Some(out.final_outro.map_or(cand, |prev| prev.max(cand)));
                }
            }
        }
    } else if let Some((cs, ce)) = t_ed {
        out.credits = Some(cs);
        out.credits_end = ce;
        add_src("theintrodb");
        if let Some(ed_end) = ce {
            if let Some((is, _, _)) = i_ed {
                if is >= ed_end + 15.0 { out.final_outro = Some(is); }
            }
        }
    } else if let Some((cs, ce, subs)) = i_ed {
        out.credits = Some(cs);
        out.credits_end = ce;
        out.outro_subs = subs;
        add_src("introdb");
    }

    // Preserve post-credits / next-episode preview markers
    out.post_credits = introdb.as_ref().and_then(|x| x.post_credits.clone())
        .or_else(|| theintrodb.as_ref().and_then(|x| x.post_credits.clone()));

    out.ep_len = aniskip.as_ref().and_then(|x| x.ep_len)
        .or_else(|| introdb.as_ref().and_then(|x| x.ep_len))
        .or_else(|| theintrodb.as_ref().and_then(|x| x.ep_len));

    out.source = srcs.join("+");
    out
}

/// Final merged output attached to `/api/probe` and `/api/skips/resolve`.
#[derive(Clone, Debug, Default)]
pub struct ResolvedSkips {
    pub skips: Vec<Value>,          // [{ s, e, title, kind, auto }, ...]
    pub intro: Option<(f64, f64)>,  // primary intro/OP segment
    pub recap: Option<(f64, f64)>,  // primary recap segment
    pub ending: Option<(f64, f64)>, // in-episode anime ED skipped -> C-part story
    pub credits: Option<f64>,       // validated Up-Next / end-credits start time
    pub credits_end: Option<f64>,   // validated credits end time (for short post-credits preview)
    pub credits_mixed: bool,        // true when ED plays over active story/dialogue (gentle countdown only)
    pub post_credits: Option<(f64, f64)>,
    pub has_post_credits: bool,
    pub source: String,
}

fn validate_head_seg(seg: &SkipSeg, dur: f64, max_frac: f64, max_len: f64) -> Option<(f64, f64)> {
    let c = clean_seg(seg.s, seg.e, 4.0, max_len)?;
    if dur > 120.0 && c.e > dur * max_frac { return None; }
    Some((c.s, c.e))
}

fn validate_credits(start: f64, end: Option<f64>, ep_len: Option<f64>, dur: f64, min_frac: f64) -> Option<(f64, Option<f64>)> {
    if !start.is_finite() || start < 60.0 { return None; }
    let clean_end = end.filter(|&x| x.is_finite() && x > start + 5.0);
    if dur <= 120.0 {
        return Some((round1(start), clean_end.map(round1)));
    }
    // Check ep_len first so anime post-credits/C-part tails (where credits_end < ep_len by 40-200s)
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
        if adj_s >= dur * min_frac && adj_s <= dur - 6.0 {
            let valid_e = adj_e.filter(|&e| e > adj_s + 5.0 && e <= dur + 2.0).map(round1);
            return Some((round1(adj_s), valid_e));
        }
    }
    if let Some(re) = clean_end.filter(|&x| x > dur + 12.0) {
        let cred_len = re - start;
        if (12.0..=360.0).contains(&cred_len) {
            let anchored = dur - cred_len;
            if anchored >= dur * min_frac && anchored <= dur - 6.0 {
                return Some((round1(anchored), Some(round1(dur))));
            }
        }
    }
    if start >= dur * min_frac && start <= dur - 6.0 {
        let valid_e = clean_end.filter(|&e| e <= dur + 5.0).map(|e| round1(e.min(dur)));
        return Some((round1(start), valid_e));
    }
    None
}

/// Merge all 5 layers with strict priority:
///   1. MKV embedded chapters (100% file-accurate)
///   2. Local audio fingerprint (exact to provider stream; overrides remote uncertainty)
///   3. Remote crowd-sourced DBs (AniSkip / IntroDB / TheIntroDB v2 consensus)
///
/// Anime v3 intelligence:
///   - Tags every segment with `kind` (`"recap" | "intro" | "ending"`) and `auto: bool`
///   - Never auto-skips `mixed-op`, `mixed-ed`, unverified anime recaps, or conflicting remote cuts
///   - Separates in-episode Anime EDs followed by >= 35s of C-part story into an in-episode `"Ending"`
///     skip that lands cleanly at the C-part instead of yanking the viewer into the next episode!
///   - Only chains adjacent Recap + Intro when both are `auto: true`
pub fn merge_all(
    chapter_skips: &[Value],
    chapter_credits: Option<f64>,
    chapter_credits_end: Option<f64>,
    audio_intro: Option<(f64, f64)>,
    audio_credits: Option<f64>,
    audio_credits_end: Option<f64>,
    remote: Option<&EpisodeSkips>,
    dur: f64,
    is_anime: bool,
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

    // Recap: Chapters (auto=true) > Remote (auto=r.recap_auto on anime, true on general TV)
    let mut recap_auto = false;
    out.recap = ch_recap.map(|v| { recap_auto = true; v }).or_else(|| {
        let r = remote?;
        let rc = r.recap.as_ref()?;
        let v = validate_head_seg(rc, dur, 0.45, 260.0)?;
        recap_auto = if is_anime { r.recap_auto } else { true };
        add_src(&r.source);
        Some(v)
    });

    // Intro: Chapters (auto=true) > Local Audio Fingerprint (auto=true) > Remote (auto=!mixed && !uncertain)
    let mut intro_auto = true;
    let mut intro_mixed = false;
    let mut intro_uncertain = false;
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
            let v = validate_head_seg(iv, dur, if is_anime { 0.48 } else { 0.50 }, 210.0)?;
            intro_mixed = r.intro_mixed;
            intro_uncertain = r.intro_uncertain;
            intro_auto = !intro_mixed && !intro_uncertain;
            add_src(&r.source);
            Some(v)
        });

    // Dedupe bogus overlapping community recap that sits >= 45% inside the intro
    if let (Some((rs, re)), Some((is, ie))) = (out.recap, out.intro) {
        let ov = (re.min(ie) - rs.max(is)).max(0.0);
        let r_len = (re - rs).max(1.0);
        if ov / r_len >= 0.45 {
            out.recap = None;
            recap_auto = false;
        }
    }

    // Credits / Ending: Chapters > Local Audio Fingerprint > Remote
    let min_cred_frac = if is_anime { 0.68 } else { 0.55 };
    let mut cred_mixed = false;
    let cred_pair = chapter_credits
        .and_then(|c| validate_credits(c, chapter_credits_end, None, dur, min_cred_frac))
        .map(|c| { add_src("chapters"); c })
        .or_else(|| {
            let c = audio_credits.and_then(|c| validate_credits(c, audio_credits_end, None, dur, min_cred_frac))?;
            add_src("audio");
            Some(c)
        })
        .or_else(|| {
            let r = remote?;
            let c = validate_credits(r.credits?, r.credits_end, r.ep_len, dur, min_cred_frac)?;
            cred_mixed = r.credits_mixed;
            add_src(&r.source);
            Some(c)
        });

    if let Some(r) = remote {
        if let Some(pc) = &r.post_credits {
            if dur <= 120.0 || (pc.s >= dur * 0.65 && pc.e <= dur + 5.0) {
                out.post_credits = Some((pc.s, pc.e));
            }
        }
    }

    let mut ending_seg: Option<(f64, f64, bool)> = None;
    if let Some((cs, ce)) = cred_pair {
        let tail_after = ce.map(|e| dur - e).unwrap_or(0.0);
        // Substantial C-part / epilogue after anime ED (>= 35s left):
        // Emit the ED song as an in-episode skip segment -> resume episode at `ce`,
        // and anchor Up Next ONLY at `final_outro` / post_credits end (or let player wait till true EOF)!
        if is_anime && dur > 120.0 && ce.is_some() && (35.0..=320.0).contains(&tail_after) {
            let ee = ce.unwrap();
            ending_seg = Some((cs, ee, !cred_mixed));
            out.ending = Some((cs, ee));
            out.has_post_credits = true;
            // Use true final outro if one exists after the C-part (e.g. 7DS S1E3/E5 final Netflix card at 1452s)
            let final_cr = remote
                .and_then(|r| r.final_outro)
                .filter(|&fo| fo.is_finite() && fo >= ee + 10.0 && fo <= dur - 5.0)
                .map(round1);
            out.credits = final_cr;
            out.credits_end = None;
            out.credits_mixed = false;
        } else {
            out.credits = Some(cs);
            out.credits_end = ce;
            out.credits_mixed = cred_mixed;
            out.has_post_credits = out.post_credits.is_some()
                || ce.map_or(false, |e| dur > 120.0 && dur - e >= 15.0 && dur - e < 35.0);
        }
    } else if let Some(fo) = remote.and_then(|r| r.final_outro).and_then(|c| validate_credits(c, None, None, dur, min_cred_frac)) {
        out.credits = Some(fo.0);
        if let Some(r) = remote { add_src(&r.source); }
    }

    // Build chronological skips[] array with explicit `kind` and `auto` flags.
    // Only chain adjacent Recap + Intro (gap <= 6s) when BOTH are verified `auto: true`!
    let mut flat: Vec<(f64, f64, &str, &str, bool, bool)> = vec![]; // s, e, title, kind, auto, mixed
    match (out.recap, out.intro) {
        (Some((rs, re)), Some((is, ie))) => {
            let (first_s, first_e, second_s, second_e) = if rs <= is { (rs, re, is, ie) } else { (is, ie, rs, re) };
            let gap = second_s - first_e;
            if gap <= 6.0 && recap_auto && intro_auto {
                let merged_e = first_e.max(second_e);
                let title = if rs <= is { "Recap + Intro" } else { "Intro + Recap" };
                flat.push((first_s, merged_e, title, "intro", true, false));
            } else {
                flat.push((rs, re, "Recap", "recap", recap_auto, false));
                flat.push((is, ie, "Intro", "intro", intro_auto, intro_mixed));
            }
        }
        (Some((rs, re)), None) => flat.push((rs, re, "Recap", "recap", recap_auto, false)),
        (None, Some((is, ie))) => flat.push((is, ie, "Intro", "intro", intro_auto, intro_mixed)),
        (None, None) => {}
    }
    if let Some((es, ee, ed_auto)) = ending_seg {
        flat.push((es, ee, "Ending", "ending", ed_auto, !ed_auto));
    }
    flat.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.skips = flat.into_iter().map(|(s, e, title, kind, auto, mixed)| {
        let mut obj = json!({ "s": s, "e": e, "title": title, "kind": kind, "auto": auto });
        if mixed { obj["mixed"] = json!(true); }
        if kind == "intro" && intro_uncertain { obj["uncertain"] = json!(true); }
        obj
    }).collect();
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
/// and Jump Festa specials that otherwise hijack `mal_ids[season - 1]`.
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

    // 1. Cumulative absolute-episode walk across chronologically sorted TV cours
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
                push(entry.id, (abs_ep - cum).max(1));
                break;
            }
        }
    }

    // 2. Long-running primary MAL heuristic when abs_ep wasn't passed (e.g. Black Clover S2 E52)
    if abs_ep <= 0 && chain[0].eps >= 40 && ep > 1 && ep <= chain[0].eps {
        push(chain[0].id, ep);
    }

    // 3. Season-indexed TV cour fallback
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

async fn fetch_aniskip_query(http: &reqwest::Client, mal_id: i64, local_ep: i64, ep_len_param: i64) -> Option<EpisodeSkips> {
    let url = format!(
        "https://api.aniskip.com/v2/skip-times/{mal_id}/{local_ep}?types[]=op&types[]=ed&types[]=recap&types[]=mixed-op&types[]=mixed-ed&episodeLength={ep_len_param}"
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

async fn fetch_aniskip_one(http: &reqwest::Client, mal_id: i64, local_ep: i64, dur: f64) -> Option<EpisodeSkips> {
    // Try exact probed file duration FIRST so when AniSkip has multiple submissions (e.g. Netflix 1472s
    // vs Broadcast 1420s), we get the exact length-matched cut! Fall back to episodeLength=0 on 404.
    if dur > 120.0 {
        let len_i = dur.round() as i64;
        if let Some(exact) = fetch_aniskip_query(http, mal_id, local_ep, len_i).await {
            return Some(exact);
        }
    }
    fetch_aniskip_query(http, mal_id, local_ep, 0).await
}

async fn fetch_aniskip(app: &App, tmdb_id: &str, season: i64, ep: i64, abs_ep: i64, dur: f64) -> Option<EpisodeSkips> {
    let chain = resolve_mal_tv_chain(app, tmdb_id).await;
    if chain.is_empty() { return None; }
    for (mal_id, local_ep) in pick_mal_targets(&chain, season, ep, abs_ep) {
        if let Some(hit) = fetch_aniskip_one(&app.http, mal_id, local_ep, dur).await {
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
    dur: f64,
    is_anime: bool,
) -> Option<EpisodeSkips> {
    resolve_episode_inner(app, tmdb_id, imdb_id, season, ep, abs_ep, dur, is_anime, false).await
}

/// Re-query with exact probed duration when a cached/cold lookup had a mismatched `ep_len`.
pub async fn refine_with_duration(
    app: &App,
    tmdb_id: &str,
    imdb_id: &str,
    season: i64,
    ep: i64,
    abs_ep: i64,
    dur: f64,
    is_anime: bool,
    current: Option<EpisodeSkips>,
) -> Option<EpisodeSkips> {
    if !is_anime || dur <= 120.0 { return current; }
    let needs_refine = current.as_ref().map_or(false, |c| {
        c.ep_len.map_or(false, |el| (el - dur).abs() > 14.0)
    });
    if !needs_refine { return current; }
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) { return current; }
    resolve_episode_inner(app, tmdb_id, imdb_id, season, ep, abs_ep, dur, true, true).await.or(current)
}

async fn resolve_episode_inner(
    app: &App,
    tmdb_id: &str,
    imdb_id: &str,
    season: i64,
    ep: i64,
    abs_ep: i64,
    dur: f64,
    is_anime: bool,
    force_refresh: bool,
) -> Option<EpisodeSkips> {
    let mut im = imdb_id.trim().to_string();
    let mut anime = is_anime;
    if !tmdb_id.is_empty() && tmdb_id != "0" {
        if let Some(m) = app.meta_get("tv", tmdb_id) {
            if im.is_empty() && m.im.starts_with("tt") { im = m.im.clone(); }
            if !anime && m.g.contains(&16) && (m.l == "ja" || m.oc.iter().any(|c| c == "JP")) { anime = true; }
        }
    }

    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) {
        let key = SkipDb::cache_key(tmdb_id, &im, season, ep)?;
        return app.skipdb.get_cached_for(&key, anime);
    }

    let key = SkipDb::cache_key(tmdb_id, &im, season, ep)?;
    if !force_refresh {
        if let Some(cached) = app.skipdb.get_cached_for(&key, anime) {
            return if cached.has_any() { Some(cached) } else { None };
        }
    }

    // Query IntroDB + TheIntroDB v2 (+ AniSkip for anime) concurrently!
    let (mut idb, mut tidb, aniskip) = tokio::join!(
        fetch_introdb(&app.http, &im, season, ep),
        fetch_theintrodb(&app.http, tmdb_id, season, ep),
        async {
            if anime { fetch_aniskip(app, tmdb_id, season, ep, abs_ep, dur).await } else { None }
        }
    );

    // Anime TMDB-Season-1 fallback: many long-running anime (e.g. Black Clover S2..S4) are split into
    // multi-seasons by IPTV providers while TMDB/IMDb index them all under Season 1 (S1 E52..E170).
    let effective_abs = if abs_ep > 0 { abs_ep } else { ep };
    if anime && season > 1 && effective_abs > 0 && (idb.is_none() || tidb.is_none()) {
        let (idb_abs, tidb_abs) = tokio::join!(
            async { if idb.is_none() { fetch_introdb(&app.http, &im, 1, effective_abs).await } else { None } },
            async { if tidb.is_none() { fetch_theintrodb(&app.http, tmdb_id, 1, effective_abs).await } else { None } }
        );
        if idb.is_none() { idb = idb_abs; }
        if tidb.is_none() { tidb = tidb_abs; }
    }

    let mut merged = if anime {
        combine_anime_sources(aniskip, idb, tidb, season, ep, dur)
    } else {
        let mut m = EpisodeSkips::default();
        if let Some(i) = idb { m = combine_remote(m, i); }
        if let Some(t) = tidb { m = combine_remote(m, t); }
        m
    };
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
    fn parses_introdb_segments_and_subs() {
        let j = json!({
            "imdb_id": "tt0944947",
            "season": 1,
            "episode": 2,
            "intro": { "start_sec": 6, "end_sec": 105, "confidence": 1, "submission_count": 4 },
            "recap": { "start_sec": 0, "end_sec": 45, "confidence": 1, "submission_count": 2 },
            "outro": { "start_sec": 3252, "end_sec": 3320, "confidence": 1, "submission_count": 3 },
            "post_credits": null
        });
        let p = parse_introdb(&j);
        assert_eq!(p.intro, Some(SkipSeg { s: 6.0, e: 105.0 }));
        assert_eq!(p.intro_subs, 4);
        assert_eq!(p.recap, Some(SkipSeg { s: 0.0, e: 45.0 }));
        assert_eq!(p.recap_subs, 2);
        assert!(p.recap_auto);
        assert_eq!(p.credits, Some(3252.0));
        assert_eq!(p.credits_end, Some(3320.0));
        assert_eq!(p.source, "introdb");
    }

    #[test]
    fn parses_theintrodb_v2_and_preview() {
        let j = json!({
            "tmdb_id": 127532,
            "intro": [{ "start_ms": 130507, "end_ms": 220039 }],
            "recap": [{ "start_ms": 0, "end_ms": 0 }],
            "credits": [{ "start_ms": 1325507, "end_ms": 1415260 }],
            "preview": [{ "start_ms": 1415042, "end_ms": null }]
        });
        let p = parse_theintrodb(&j);
        assert_eq!(p.intro, Some(SkipSeg { s: 130.5, e: 220.0 }));
        assert_eq!(p.recap, None);
        assert_eq!(p.credits, Some(1325.5));
        assert_eq!(p.credits_end, Some(1415.3));
        assert_eq!(p.post_credits, Some(SkipSeg { s: 1415.0, e: 1440.0 }));

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
    fn parses_aniskip_mixed_flags() {
        let j = json!({
            "found": true,
            "results": [
                { "skipType": "mixed-op", "interval": { "startTime": 54.3, "endTime": 144.3 }, "episodeLength": 1450.0 },
                { "skipType": "mixed-ed", "interval": { "startTime": 1350.0, "endTime": 1440.0 }, "episodeLength": 1450.0 }
            ]
        });
        let p = parse_aniskip(&j);
        assert_eq!(p.intro, Some(SkipSeg { s: 54.3, e: 144.3 }));
        assert!(p.intro_mixed, "mixed-op must set intro_mixed=true");
        assert!(p.credits_mixed, "mixed-ed must set credits_mixed=true");

        let m = merge_all(&[], None, None, None, None, None, Some(&p), 1450.0, true);
        assert_eq!(m.skips[0]["auto"], json!(false), "mixed-op must NEVER auto-skip");
        assert_eq!(m.skips[0]["mixed"], json!(true));
        assert!(m.credits_mixed);
    }

    #[test]
    fn merge_respects_priority_and_pal_credit_reanchoring() {
        let remote = EpisodeSkips {
            recap: Some(SkipSeg { s: 0.0, e: 35.0 }),
            recap_auto: true,
            intro: Some(SkipSeg { s: 45.0, e: 120.0 }),
            credits: Some(3252.0),
            credits_end: Some(3320.0),
            ep_len: Some(3320.0),
            source: "introdb".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
            ..Default::default()
        };
        let ch = vec![json!({ "s": 42.0, "e": 118.0, "title": "Opening Credits" })];
        let m = merge_all(&ch, None, None, Some((45.0, 125.0)), None, None, Some(&remote), 3185.0, false);
        assert_eq!(m.intro, Some((42.0, 118.0)));
        assert_eq!(m.recap, Some((0.0, 35.0)));
        let cr = m.credits.expect("credits");
        assert!((cr - 3119.8).abs() < 4.0, "expected PAL credits ~3119.8, got {cr}");
        assert_eq!(m.skips.len(), 2);
        assert_eq!(m.skips[0]["title"], "Recap");
        assert_eq!(m.skips[1]["title"], "Intro");
    }

    #[test]
    fn seven_deadly_sins_real_cases_fixed() {
        // S1E2: AniSkip has pure OP 102.4..192.4 & true-end ED 1330..1470;
        // IntroDB has bogus 1-submission recap 0..84 (cold open!) -> MUST BE DROPPED!
        let a2 = parse_aniskip(&json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 102.4, "endTime": 192.4 }, "episodeLength": 1470.2 },
                { "skipType": "ed", "interval": { "startTime": 1330.0, "endTime": 1470.0 }, "episodeLength": 1470.2 }
            ]
        }));
        let i2 = parse_introdb(&json!({
            "intro": { "start_sec": 94.3, "end_sec": 198.7, "confidence": 1, "submission_count": 1 },
            "recap": { "start_sec": 0.02, "end_sec": 83.98, "confidence": 1, "submission_count": 1 },
            "outro": { "start_sec": 1452.0, "end_sec": 1471.0, "confidence": 1, "submission_count": 1 }
        }));
        let t2 = parse_theintrodb(&json!({
            "intro": [{ "start_ms": 86200, "end_ms": 205000 }],
            "recap": [{ "start_ms": 0, "end_ms": 84000 }],
            "credits": [{ "start_ms": 1334000, "end_ms": 1424000 }]
        }));
        let c2 = combine_anime_sources(Some(a2), Some(i2), Some(t2), 1, 2, 1471.6);
        assert_eq!(c2.recap, None, "1-sub mirrored bogus cold-open recap must be dropped");
        assert_eq!(c2.intro, Some(SkipSeg { s: 102.4, e: 192.4 }));
        assert!(!c2.intro_uncertain);
        let m2 = merge_all(&[], None, None, None, None, None, Some(&c2), 1471.6, true);
        assert_eq!(m2.skips.len(), 1);
        assert_eq!(m2.skips[0]["kind"], "intro");
        assert_eq!(m2.skips[0]["auto"], json!(true));
        assert_eq!(m2.credits, Some(1330.0));

        // S1E3: AniSkip ED 1278..1367 leaves 104.6s of C-part story before IntroDB final_outro 1452!
        // Must emit Ending as in-episode skip (1278->1367) and set Up Next credits = 1452.0!
        let a3 = parse_aniskip(&json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 115.4, "endTime": 205.4 }, "episodeLength": 1470.2 },
                { "skipType": "ed", "interval": { "startTime": 1278.0, "endTime": 1367.0 }, "episodeLength": 1470.0 }
            ]
        }));
        let i3 = parse_introdb(&json!({
            "intro": { "start_sec": 107.3, "end_sec": 211.8, "confidence": 1, "submission_count": 1 },
            "recap": { "start_sec": 67.234, "end_sec": 97.244, "confidence": 1, "submission_count": 1 },
            "outro": { "start_sec": 1452.0, "end_sec": 1471.0, "confidence": 1, "submission_count": 1 }
        }));
        let t3 = parse_theintrodb(&json!({
            "intro": [{ "start_ms": 111016, "end_ms": 208711 }],
            "recap": [{ "start_ms": 67234, "end_ms": 97244 }],
            "credits": [{ "start_ms": 1277964, "end_ms": 1367991 }]
        }));
        let c3 = combine_anime_sources(Some(a3), Some(i3), Some(t3), 1, 3, 1471.6);
        assert_eq!(c3.recap, None, "mirrored 1-sub recap must be dropped");
        assert_eq!(c3.final_outro, Some(1452.0), "final Netflix outro after C-part must be preserved");
        let m3 = merge_all(&[], None, None, None, None, None, Some(&c3), 1471.6, true);
        assert_eq!(m3.skips.len(), 2, "must have Opening AND in-episode Ending skip");
        assert_eq!(m3.skips[0]["kind"], "intro");
        assert_eq!(m3.skips[1]["kind"], "ending");
        assert_eq!(m3.skips[1]["s"], json!(1278.0));
        assert_eq!(m3.skips[1]["e"], json!(1367.0));
        assert_eq!(m3.skips[1]["auto"], json!(true));
        assert_eq!(m3.credits, Some(1452.0), "Up Next must wait until true final outro 1452s!");

        // S1E4: All 3 sources disagree on OP by >45s (62 vs 108 vs 134) AND ED is 1188.5..1278.5 (195s C-part!)
        // Must mark OP manual-only (auto: false), drop mirrored recap, skip ED in-episode, credits = None!
        let a4 = parse_aniskip(&json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 62.1, "endTime": 152.1 }, "episodeLength": 1473.5 },
                { "skipType": "ed", "interval": { "startTime": 1188.5, "endTime": 1278.5 }, "episodeLength": 1469.2 }
            ]
        }));
        let i4 = parse_introdb(&json!({
            "intro": { "start_sec": 107.7, "end_sec": 207.4, "confidence": 1, "submission_count": 1 },
            "recap": { "start_sec": 15.035, "end_sec": 54.722, "confidence": 1, "submission_count": 1 },
            "outro": { "start_sec": 1281.1, "end_sec": 1347.4, "confidence": 1, "submission_count": 1 }
        }));
        let t4 = parse_theintrodb(&json!({
            "intro": [{ "start_ms": 133742, "end_ms": 231478 }],
            "recap": [{ "start_ms": 15035, "end_ms": 54722 }]
        }));
        let c4 = combine_anime_sources(Some(a4), Some(i4), Some(t4), 1, 4, 1473.6);
        assert!(c4.intro_uncertain, "conflicting >45s OP cuts must be flagged uncertain");
        assert_eq!(c4.recap, None);
        let m4 = merge_all(&[], None, None, None, None, None, Some(&c4), 1473.6, true);
        assert_eq!(m4.skips[0]["kind"], "intro");
        assert_eq!(m4.skips[0]["auto"], json!(false), "conflicting OP must NOT blindly auto-skip");
        assert_eq!(m4.skips[1]["kind"], "ending");
        assert_eq!(m4.skips[1]["e"], json!(1278.5));
        assert_eq!(m4.credits, None, "must NOT yank user to next ep 3m15s before EOF");

        // Once local audio fingerprint finds the true OP on S1E4, audio overrides uncertainty -> auto: true!
        let m4_fp = merge_all(&[], None, None, Some((107.5, 197.5)), None, None, Some(&c4), 1473.6, true);
        assert_eq!(m4_fp.intro, Some((107.5, 197.5)));
        assert_eq!(m4_fp.skips[0]["auto"], json!(true), "local audio FP overrides remote uncertainty");
    }

    #[test]
    fn black_clover_e103_and_solo_leveling_s1e2_consensus() {
        // Black Clover E103: IntroDB (150.8..240.8) & TheIntroDB (151.1..241.2) independently agree
        // while AniSkip is a 140s outlier (11.1..101.1) -> consensus wins, bogus 1-sub recap dropped!
        let abc = parse_aniskip(&json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 11.146, "endTime": 101.146 }, "episodeLength": 1434.2 },
                { "skipType": "ed", "interval": { "startTime": 1294.7, "endTime": 1384.7 }, "episodeLength": 1441.0 }
            ]
        }));
        let ibc = parse_introdb(&json!({
            "intro": { "start_sec": 150.82, "end_sec": 240.79, "confidence": 1, "submission_count": 1 },
            "recap": { "start_sec": 2.0, "end_sec": 148.0, "confidence": 1, "submission_count": 1 }
        }));
        let tbc = parse_theintrodb(&json!({
            "intro": [{ "start_ms": 151145, "end_ms": 241151 }],
            "credits": [{ "start_ms": 1287077, "end_ms": 1377042 }]
        }));
        let cbc = combine_anime_sources(Some(abc), Some(ibc), Some(tbc), 3, 103, 1434.2);
        assert_eq!(cbc.intro, Some(SkipSeg { s: 150.8, e: 240.8 }));
        assert!(!cbc.intro_uncertain);
        assert_eq!(cbc.recap, None);
        let mbc = merge_all(&[], None, None, None, None, None, Some(&cbc), 1434.2, true);
        assert_eq!(mbc.skips[0]["s"], json!(150.8));
        assert_eq!(mbc.skips[1]["kind"], "ending", "49.5s Petit Clover after ED -> in-episode Ending skip!");
        assert_eq!(mbc.skips[1]["e"], json!(1384.7));

        // Solo Leveling S1E2: All 3 agree on OP 130.5..220.5; IntroDB 1-sub recap 0..132 MUST be dropped!
        let asl = parse_aniskip(&json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 130.531, "endTime": 220.531 }, "episodeLength": 1417.4 },
                { "skipType": "ed", "interval": { "startTime": 1325.0, "endTime": 1414.0 }, "episodeLength": 1422.0 }
            ]
        }));
        let isl = parse_introdb(&json!({
            "intro": { "start_sec": 130.531, "end_sec": 220.531, "confidence": 1, "submission_count": 3 },
            "recap": { "start_sec": 0.0, "end_sec": 132.0, "confidence": 1, "submission_count": 1 }
        }));
        let tsl = parse_theintrodb(&json!({
            "intro": [{ "start_ms": 130507, "end_ms": 220039 }],
            "credits": [{ "start_ms": 1325507, "end_ms": 1415260 }],
            "preview": [{ "start_ms": 1415042, "end_ms": null }]
        }));
        let csl = combine_anime_sources(Some(asl), Some(isl), Some(tsl), 1, 2, 1418.0);
        assert_eq!(csl.recap, None, "must NEVER delete Solo Leveling S1E2 2m12s cold open");
        assert_eq!(csl.intro, Some(SkipSeg { s: 130.5, e: 220.5 }));
        let msl = merge_all(&[], None, None, None, None, None, Some(&csl), 1418.0, true);
        assert_eq!(msl.skips.len(), 1);
        assert_eq!(msl.skips[0]["s"], json!(130.5));
        assert_eq!(msl.credits, Some(1325.0));
    }

    #[test]
    fn ep1_drops_bogus_recap_and_late_op_rejected() {
        let a = parse_aniskip(&json!({
            "found": true,
            "results": [
                { "skipType": "op", "interval": { "startTime": 28.8, "endTime": 118.8 }, "episodeLength": 1500.0 },
                { "skipType": "recap", "interval": { "startTime": 132.8, "endTime": 201.3 }, "episodeLength": 1500.0 }
            ]
        }));
        let c = combine_anime_sources(Some(a), None, None, 1, 1, 1500.0);
        assert_eq!(c.recap, None, "Episode 1 of a series must never accept remote recaps");
        assert_eq!(c.intro, Some(SkipSeg { s: 28.8, e: 118.8 }));

        // 7DS S1E1: IntroDB intro at minute 20 (1214..1304) must be rejected; bloated 155s TheIntroDB intro -> manual only
        let i1 = parse_introdb(&json!({
            "intro": { "start_sec": 1214.8, "end_sec": 1304.8, "confidence": 1, "submission_count": 1 },
            "outro": { "start_sec": 1452.0, "end_sec": 1471.0, "confidence": 1, "submission_count": 1 }
        }));
        let t1 = parse_theintrodb(&json!({
            "intro": [{ "start_ms": 0, "end_ms": 155400 }]
        }));
        let c1 = combine_anime_sources(None, Some(i1), Some(t1), 1, 1, 1471.6);
        assert_eq!(c1.intro, Some(SkipSeg { s: 0.0, e: 155.4 }));
        assert!(c1.intro_uncertain, "bloated 155s single-source Ep1 intro must be manual-only");
        assert_eq!(c1.credits, Some(1452.0));
    }

    #[test]
    fn anime_mal_chain_maps_long_runners_and_split_cours() {
        let bc = vec![
            MalEntry { id: 34572, eps: 170, start: "2017-10-03".into() },
            MalEntry { id: 61967, eps: 0,   start: "2026-10-03".into() },
        ];
        assert_eq!(pick_mal_targets(&bc, 1, 1, 1)[0], (34572, 1));
        assert_eq!(pick_mal_targets(&bc, 2, 52, 52)[0], (34572, 52));
        assert_eq!(pick_mal_targets(&bc, 3, 103, 103)[0], (34572, 103));
        assert_eq!(pick_mal_targets(&bc, 4, 170, 170)[0], (34572, 170));
        assert_eq!(pick_mal_targets(&bc, 2, 52, 0)[0], (34572, 52));
        assert_eq!(pick_mal_targets(&bc, 2, 1, 171)[0], (61967, 1));
        assert_eq!(pick_mal_targets(&bc, 5, 9, 179)[0], (61967, 9));

        let ds = vec![
            MalEntry { id: 38000, eps: 26, start: "2019-04-06".into() },
            MalEntry { id: 49926, eps: 7,  start: "2021-10-10".into() },
            MalEntry { id: 47778, eps: 11, start: "2021-12-05".into() },
            MalEntry { id: 51019, eps: 11, start: "2023-04-09".into() },
            MalEntry { id: 55701, eps: 8,  start: "2024-05-12".into() },
        ];
        assert_eq!(pick_mal_targets(&ds, 1, 19, 19)[0], (38000, 19));
        assert_eq!(pick_mal_targets(&ds, 2, 2, 28)[0], (49926, 2));
        assert_eq!(pick_mal_targets(&ds, 2, 8, 34)[0], (47778, 1));
        assert_eq!(pick_mal_targets(&ds, 3, 1, 45)[0], (51019, 1));
        assert_eq!(pick_mal_targets(&ds, 4, 1, 56)[0], (55701, 1));

        let aot = vec![
            MalEntry { id: 16498, eps: 25, start: "2013-04-07".into() },
            MalEntry { id: 25777, eps: 12, start: "2017-04-01".into() },
            MalEntry { id: 35760, eps: 12, start: "2018-07-23".into() },
            MalEntry { id: 38524, eps: 10, start: "2019-04-29".into() },
            MalEntry { id: 40028, eps: 16, start: "2020-12-07".into() },
            MalEntry { id: 48583, eps: 12, start: "2022-01-10".into() },
        ];
        assert_eq!(pick_mal_targets(&aot, 2, 1, 26)[0], (25777, 1));
        assert_eq!(pick_mal_targets(&aot, 3, 13, 50)[0], (38524, 1));
        assert_eq!(pick_mal_targets(&aot, 4, 1, 60)[0], (40028, 1));
    }

    #[test]
    fn anime_verified_recap_op_chaining_and_manual_recap_separation() {
        // 1. Both verified auto=true & overlapping -> chains into ONE seamless "Recap + Intro" skip
        let r1 = EpisodeSkips {
            recap: Some(SkipSeg { s: 0.0, e: 90.3 }),
            recap_auto: true,
            intro: Some(SkipSeg { s: 87.5, e: 177.5 }),
            credits: Some(1294.7),
            credits_end: Some(1384.7),
            ep_len: Some(1441.0),
            source: "aniskip+introdb".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
            ..Default::default()
        };
        let m1 = merge_all(&[], None, None, None, None, None, Some(&r1), 1440.0, true);
        assert_eq!(m1.skips.len(), 2); // Recap + Intro AND Ending
        assert_eq!(m1.skips[0]["title"], "Recap + Intro");
        assert_eq!(m1.skips[0]["s"], 0.0);
        assert_eq!(m1.skips[0]["e"], 177.5);
        assert_eq!(m1.skips[0]["auto"], json!(true));
        assert_eq!(m1.skips[1]["kind"], "ending");
        assert!(m1.has_post_credits);

        // 2. Single-source AniSkip recap (recap_auto=false) stays separate from auto OP so cold open is safe
        let r2 = EpisodeSkips {
            recap: Some(SkipSeg { s: 0.0, e: 85.0 }),
            recap_auto: false,
            intro: Some(SkipSeg { s: 85.0, e: 175.0 }),
            source: "aniskip".into(),
            t: 1,
            v: CURRENT_SKIP_VER,
            ..Default::default()
        };
        let m2 = merge_all(&[], None, None, None, None, None, Some(&r2), 1440.0, true);
        assert_eq!(m2.skips.len(), 2);
        assert_eq!(m2.skips[0]["kind"], "recap");
        assert_eq!(m2.skips[0]["auto"], json!(false));
        assert_eq!(m2.skips[1]["kind"], "intro");
        assert_eq!(m2.skips[1]["auto"], json!(true));
    }
}
