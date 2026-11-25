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
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkipSeg {
    pub s: f64,
    pub e: f64,
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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DiskCache {
    #[serde(default)]
    eps: HashMap<String, EpisodeSkips>,
    #[serde(default)]
    mal: HashMap<String, Vec<i64>>,
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
        let now = now_ms();
        self.cache.lock().unwrap().eps.get(key).filter(|e| e.is_fresh(now)).cloned()
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

/// Parse AniSkip (`https://api.aniskip.com/v2/skip-times/{mal_id}/{ep}?types=...`).
pub fn parse_aniskip(v: &Value) -> EpisodeSkips {
    let mut out = EpisodeSkips::default();
    if v["found"].as_bool() != Some(true) { return out; }
    let Some(results) = v["results"].as_array() else { return out };
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
            "op" | "mixed-op" if out.intro.is_none() => {
                out.intro = clean_seg(s, e, 10.0, 210.0);
            }
            "recap" if out.recap.is_none() => {
                out.recap = clean_seg(s, e, 5.0, 240.0);
            }
            "ed" | "mixed-ed" if out.credits.is_none() => {
                if s.is_finite() && s > 60.0 {
                    out.credits = Some((s * 10.0).round() / 10.0);
                    if e.is_finite() && e > s + 5.0 {
                        out.credits_end = Some((e * 10.0).round() / 10.0);
                    }
                }
            }
            _ => {}
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
    pub skips: Vec<Value>,         // [{ s, e, title, source }, ...]
    pub intro: Option<(f64, f64)>, // primary intro segment
    pub recap: Option<(f64, f64)>, // primary recap segment
    pub credits: Option<f64>,      // validated credits start time
    pub post_credits: Option<(f64, f64)>,
    pub source: String,
}

fn validate_head_seg(seg: &SkipSeg, dur: f64, max_frac: f64, max_len: f64) -> Option<(f64, f64)> {
    let c = clean_seg(seg.s, seg.e, 4.0, max_len)?;
    if dur > 120.0 && c.e > dur * max_frac { return None; }
    Some((c.s, c.e))
}

fn validate_credits(start: f64, end: Option<f64>, ep_len: Option<f64>, dur: f64) -> Option<f64> {
    if !start.is_finite() || start < 60.0 { return None; }
    if dur <= 120.0 {
        return Some((start * 10.0).round() / 10.0);
    }
    // If source reported an end timestamp or episode length notably longer than our encode
    // (e.g., PAL 25fps 4% speedup or trimmed outro), re-anchor credits relative to the end!
    let ref_end = end.or(ep_len).filter(|&x| x > start + 8.0);
    if let Some(re) = ref_end {
        let cred_len = re - start;
        if re > dur + 12.0 && cred_len >= 12.0 && cred_len <= 360.0 {
            let anchored = dur - cred_len;
            if anchored >= dur * 0.55 && anchored <= dur - 6.0 {
                return Some((anchored * 10.0).round() / 10.0);
            }
        }
    }
    if start >= dur * 0.55 && start <= dur - 6.0 {
        return Some((start * 10.0).round() / 10.0);
    }
    None
}

/// Merge all 5 layers with strict priority:
///   1. MKV embedded chapters (100% file-accurate)
///   2. Local audio fingerprint (exact to provider stream)
///   3. Remote crowd-sourced DBs (AniSkip / IntroDB / TheIntroDB v2)
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
                if let Some(v) = validate_head_seg(&SkipSeg { s, e }, dur, 0.42, 240.0) {
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
        let v = validate_head_seg(rc, dur, 0.42, 240.0)?;
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

    // Credits: Chapters > Local Audio Fingerprint > Remote
    out.credits = chapter_credits
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

    if let Some(r) = remote {
        if let Some(pc) = &r.post_credits {
            if dur <= 120.0 || (pc.s >= dur * 0.65 && pc.e <= dur + 5.0) {
                out.post_credits = Some((pc.s, pc.e));
            }
        }
    }

    // Build flat skips[] array (Recap + Intro, sorted by start time, no overlapping dupes)
    let mut flat: Vec<(f64, f64, &str)> = vec![];
    if let Some((s, e)) = out.recap {
        flat.push((s, e, "Recap"));
    }
    if let Some((s, e)) = out.intro {
        // Avoid pushing an intro that completely overlaps the recap
        let dup = flat.iter().any(|&(rs, re, _)| (rs - s).abs() < 4.0 && (re - e).abs() < 4.0);
        if !dup {
            flat.push((s, e, "Intro"));
        }
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

async fn resolve_mal_ids(app: &App, tmdb_id: &str) -> Vec<i64> {
    if tmdb_id.is_empty() || tmdb_id == "0" { return vec![]; }
    if let Some(v) = app.skipdb.cache.lock().unwrap().mal.get(tmdb_id).cloned() {
        return v;
    }
    let url = format!("https://arm.haglund.dev/api/v2/themoviedb?id={tmdb_id}&include=myanimelist");
    let ids: Vec<i64> = match app.http.get(&url)
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_millis(3000))
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
    app.skipdb.cache.lock().unwrap().mal.insert(tmdb_id.to_string(), ids.clone());
    app.skipdb.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    ids
}

async fn fetch_aniskip(app: &App, tmdb_id: &str, season: i64, ep: i64, dur: f64) -> Option<EpisodeSkips> {
    let mal_ids = resolve_mal_ids(app, tmdb_id).await;
    if mal_ids.is_empty() { return None; }
    let idx = if season > 0 { (season - 1) as usize } else { 0 };
    let mal_id = mal_ids.get(idx).copied().or_else(|| mal_ids.first().copied())?;
    let ep_len = if dur > 120.0 { dur.round() as i64 } else { 0 };
    let url = format!(
        "https://api.aniskip.com/v2/skip-times/{mal_id}/{ep}?types=op&types=ed&types=recap&types=mixed-op&types=mixed-ed&episodeLength={ep_len}"
    );
    let resp = app.http.get(&url)
        .header("User-Agent", BROWSER_UA)
        .timeout(Duration::from_millis(3500))
        .send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let v: Value = resp.json().await.ok()?;
    let parsed = parse_aniskip(&v);
    if parsed.has_any() { Some(parsed) } else { None }
}

/// Resolve an episode's crowd-sourced skip timestamps (cached 30 days on hit, 3 days on miss).
/// Uses ZERO IPTV provider connections!
pub async fn resolve_episode(
    app: &App,
    tmdb_id: &str,
    imdb_id: &str,
    season: i64,
    ep: i64,
    dur: f64,
    is_anime: bool,
) -> Option<EpisodeSkips> {
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) {
        // Never hit external skip APIs during offline test runs unless already cached.
        let key = SkipDb::cache_key(tmdb_id, imdb_id, season, ep)?;
        return app.skipdb.get_cached(&key);
    }

    // Try to fill imdb_id from local meta.json (97.4% hit rate, zero network calls!)
    let mut im = imdb_id.trim().to_string();
    let mut anime = is_anime;
    if !tmdb_id.is_empty() && tmdb_id != "0" {
        if let Some(m) = app.meta_get("tv", tmdb_id) {
            if im.is_empty() && m.im.starts_with("tt") { im = m.im.clone(); }
            if !anime && m.g.contains(&16) && m.l == "ja" { anime = true; }
        }
    }
    let key = SkipDb::cache_key(tmdb_id, &im, season, ep)?;
    if let Some(cached) = app.skipdb.get_cached(&key) {
        return if cached.has_any() { Some(cached) } else { None };
    }

    // Query IntroDB + TheIntroDB v2 (+ AniSkip for anime) concurrently!
    let (idb, tidb, aniskip) = tokio::join!(
        fetch_introdb(&app.http, &im, season, ep),
        fetch_theintrodb(&app.http, tmdb_id, season, ep),
        async {
            if anime { fetch_aniskip(app, tmdb_id, season, ep, dur).await } else { None }
        }
    );

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
    if !merged.has_any() { merged.source = "none".into(); }
    app.skipdb.put_cached(key, merged.clone());
    if merged.has_any() { Some(merged) } else { None }
}

/// Fire-and-forget lookahead: whenever the user plays S:E, pre-cache E+1..E+3 from public skip DBs
/// (uses ZERO IPTV provider connections!) so auto-next has instant 0ms skip timestamps ready.
pub fn spawn_prefetch_ahead(app: Arc<App>, tmdb_id: String, imdb_id: String, season: i64, ep: i64, is_anime: bool) {
    if (tmdb_id.is_empty() && !imdb_id.starts_with("tt")) || season < 0 || ep <= 0 { return; }
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) { return; }
    tokio::spawn(async move {
        for next_ep in (ep + 1)..=(ep + 3) {
            let Some(key) = SkipDb::cache_key(&tmdb_id, &imdb_id, season, next_ep) else { break };
            if app.skipdb.get_cached(&key).is_some() { continue; }
            let _ = resolve_episode(&app, &tmdb_id, &imdb_id, season, next_ep, 0.0, is_anime).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        app.skipdb.save_if_dirty(true);
    });
}

/// Gentle background collection pre-warmer: populates skip timestamps for shows in Continue Watching,
/// Follows, My List, and top English TV shows without EVER touching the IPTV provider connection.
pub async fn prefetch_worker(app: Arc<App>) {
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) { return; }
    let cat = app.catalog();
    if cat.series.is_empty() { return; }

    // 1. Priority: shows in progress / follows / my list / wanted
    let mut todo_eps: Vec<(String, String, i64, i64, bool)> = vec![];
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

    let push_show_eps = |todo: &mut Vec<(String, String, i64, i64, bool)>, tmdb: &str, im: &str, is_anime: bool, season: i64, from_ep: i64, count: i64| {
        for e in from_ep..(from_ep + count) {
            if e <= 0 { continue; }
            if let Some(k) = SkipDb::cache_key(tmdb, im, season, e) {
                if app.skipdb.get_cached(&k).is_none() {
                    todo.push((tmdb.to_string(), im.to_string(), season, e, is_anime));
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
        let anime = meta.as_ref().map_or(false, |m| m.g.contains(&16) && m.l == "ja");
        push_show_eps(&mut todo_eps, tmdb, &im, anime, sn, en, 5);
    }
    for sid in list_sids.into_iter().take(20) {
        let Some(it) = cat.series_item(sid) else { continue };
        let Some(ref tmdb) = it.tmdb else { continue };
        if !seen_show.insert(tmdb.clone()) { continue; }
        let meta = app.meta_get("tv", tmdb);
        let im = meta.as_ref().map(|m| m.im.clone()).unwrap_or_default();
        let anime = meta.as_ref().map_or(false, |m| m.g.contains(&16) && m.l == "ja");
        push_show_eps(&mut todo_eps, tmdb, &im, anime, 1, 1, 3);
    }

    // 2. Top popular English TV shows in the catalog (so browsing & clicking Play is instant!)
    if todo_eps.len() < 45 {
        let mut pop_shows: Vec<(f64, String, String, bool)> = cat.series.iter()
            .filter(|s| s.en && !s.sport)
            .filter_map(|s| {
                let tmdb = s.tmdb.as_ref()?;
                if seen_show.contains(tmdb) { return None; }
                let m = app.meta_get("tv", tmdb)?;
                if m.im.is_empty() && m.p < 20.0 { return None; }
                let anime = m.g.contains(&16) && m.l == "ja";
                Some((m.p + m.va * 5.0, tmdb.clone(), m.im.clone(), anime))
            })
            .collect();
        pop_shows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, tmdb, im, anime) in pop_shows.into_iter().take(40) {
            if !seen_show.insert(tmdb.clone()) { continue; }
            push_show_eps(&mut todo_eps, &tmdb, &im, anime, 1, 1, 2);
            if todo_eps.len() >= 50 { break; }
        }
    }

    let mut fetched = 0usize;
    let mut hits = 0usize;
    for (tmdb, im, sn, en, anime) in todo_eps.into_iter().take(50) {
        if let Some(res) = resolve_episode(&app, &tmdb, &im, sn, en, 0.0, anime).await {
            if res.has_any() { hits += 1; }
        }
        fetched += 1;
        tokio::time::sleep(Duration::from_millis(350)).await;
    }
    if fetched > 0 {
        app.skipdb.save_if_dirty(true);
        println!("[skipdb] prefetched {fetched} TV episodes ({hits} matched from IntroDB/TheIntroDB/AniSkip)");
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
            intro: Some(SkipSeg { s: 40.0, e: 120.0 }),
            credits: Some(3252.0),
            credits_end: Some(3320.0), // 68s credits on 3320s NTSC encode
            post_credits: None,
            ep_len: Some(3320.0),
            source: "introdb".into(),
            t: 1,
        };
        // PAL 25fps encode is 3185s (4% shorter): credits should re-anchor to 3185 - 68 = 3117s!
        let ch = vec![json!({ "s": 42.0, "e": 118.0, "title": "Opening Credits" })];
        let m = merge_all(&ch, None, Some((45.0, 125.0)), None, Some(&remote), 3185.0);
        // Chapter intro wins over audio & remote
        assert_eq!(m.intro, Some((42.0, 118.0)));
        // Remote recap fills missing chapter recap
        assert_eq!(m.recap, Some((0.0, 35.0)));
        // PAL-reanchored credits from remote!
        assert_eq!(m.credits, Some(3117.0));
        assert_eq!(m.skips.len(), 2);
        assert_eq!(m.skips[0]["title"], "Recap");
        assert_eq!(m.skips[1]["title"], "Intro");
    }
}
