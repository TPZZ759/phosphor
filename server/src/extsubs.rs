// Automatic external English subtitles: searches free no-key providers (Stremio OpenSubtitles v3
// + OpenSubtitles legacy REST) plus optional API-key upgrades (OpenSubtitles.com REST v1, Wyzie),
// downloads & unpacks plain/gzip/zip payloads with UTF-8/UTF-16/CP1252 decoding, converts SRT<->VTT,
// caches under data/ext-subs/, and auto-syncs cached tracks against ffmpeg's live audio tap.
use crate::app::App;
use crate::tmdb::{NO, TTL_D};
use crate::util::{enc, norm, now_ms, s};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const SEARCH_TTL_MS: i64 = 15 * 60_000;
const MAX_SUB_BYTES: usize = 10 * 1024 * 1024;
const OS_LEGACY_UA: &str = "VLSub 0.11.1";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) tv-rs/1.0";

static SEARCH_CACHE: OnceLock<Mutex<HashMap<String, (i64, Value)>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct SubCand {
    id: String,
    file_id: String,
    lang: String,
    label: String,
    hi: bool,
    hi_known: bool,
    url: String,
    source: String,
    format: String,
    downloads: i64,
    rating: f64,
    fps: f64,
    url_tier: u8, // higher = preferred download URL when merging duplicate file_ids
}

/// Sanitize a cache key (e.g. `"m:12345"` -> `"m_12345"`) into a safe single-segment filename stem.
pub fn sanitize_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len().min(96));
    let mut last_us = false;
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
            last_us = ch == '_';
        } else if !last_us && !out.is_empty() {
            out.push('_');
            last_us = true;
        }
        if out.len() >= 96 {
            break;
        }
    }
    out.trim_matches('_').to_string()
}

fn sub_dir(app: &App) -> PathBuf {
    app.data.join("ext-subs")
}

/// Path to cached `.vtt` file on disk, if previously fetched and non-empty.
pub fn cached_vtt_path(app: &App, key: &str) -> Option<PathBuf> {
    let safe = sanitize_key(key);
    if safe.is_empty() {
        return None;
    }
    let p = sub_dir(app).join(format!("{safe}.vtt"));
    std::fs::metadata(&p).ok().filter(|m| m.is_file() && m.len() > 16).map(|_| p)
}

/// Browser-ready URL (`/api/extsubs/file/<safe>.vtt`) if an external subtitle is already cached.
pub fn cached_vtt(app: &App, key: &str) -> Option<String> {
    let safe = sanitize_key(key);
    cached_vtt_path(app, key)?;
    Some(format!("/api/extsubs/file/{safe}.vtt"))
}

/// Local `.srt` path for mpv (`--sub-file=...`), if an external subtitle is already cached.
pub fn cached_srt_path(app: &App, key: &str) -> Option<PathBuf> {
    let safe = sanitize_key(key);
    if safe.is_empty() {
        return None;
    }
    let p = sub_dir(app).join(format!("{safe}.srt"));
    std::fs::metadata(&p).ok().filter(|m| m.is_file() && m.len() > 16).map(|_| p)
}

/// Safely read a cached `.vtt` file by route filename (e.g. `"m_12345.vtt"`), rejecting path traversal.
pub fn read_vtt_file(app: &App, name: &str) -> Option<String> {
    let stem = name.strip_suffix(".vtt")?;
    if stem.is_empty() || stem.contains('/') || stem.contains('\\') || stem.contains("..") {
        return None;
    }
    if !stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    std::fs::read_to_string(sub_dir(app).join(format!("{stem}.vtt"))).ok()
}

fn norm_imdb(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    if let Some(rest) = t.strip_prefix("tt").or_else(|| t.strip_prefix("TT")) {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return format!("tt{rest}");
        }
    } else if t.chars().all(|c| c.is_ascii_digit()) {
        return format!("tt{t}");
    }
    String::new()
}

fn is_english(lang: &str) -> bool {
    let l = lang.trim().to_ascii_lowercase();
    matches!(l.as_str(), "en" | "eng" | "english" | "en-us" | "en-gb" | "en-au" | "en-ca")
        || l.starts_with("en-")
        || l.starts_with("en_")
}

fn detect_hi(name: &str) -> bool {
    if crate::re!(r"(?i)(^|[\W_])non[\s._-]*hi([\W_]|$)").is_match(name) {
        return false;
    }
    crate::re!(r"(?i)(^|[\W_])(hi|sdh|hearing[\s._-]*impaired)([\W_]|$)").is_match(name)
        || crate::re!(r"(?i)\((hi|sdh|cc|hearing\s+impaired)\)").is_match(name)
        || crate::re!(r"(?i)\[(hi|sdh|cc)\]").is_match(name)
}

fn extract_os_file_id(url: &str) -> String {
    crate::re!(r"/file(?:ad)?/(\d+)")
        .captures(url)
        .map(|c| c[1].to_string())
        .unwrap_or_default()
}

fn clean_label(file_name: &str, release_name: &str, fallback: &str) -> String {
    let pick = if !release_name.trim().is_empty() {
        release_name.trim()
    } else if !file_name.trim().is_empty() {
        file_name.trim()
    } else {
        fallback.trim()
    };
    let stripped = crate::re!(r"(?i)(\.(srt|vtt|sub|ass|ssa|txt))+$")
        .replace(pick, "")
        .trim()
        .to_string();
    if stripped.is_empty() {
        "English".to_string()
    } else {
        stripped.chars().take(120).collect()
    }
}

fn clean_query_title(title: &str) -> String {
    let t = crate::re!(r"(?i)^(?:[A-Z]{2,4}\s*[-|:]\s*)").replace(title.trim(), "");
    let t = crate::re!(r"\s*[\(\[]\s*(?:19|20)\d\d\s*[\)\]]\s*$").replace(&t, "");
    let t = crate::re!(r"(?i)\s*[\(\[]\s*(?:4k|uhd|hd|fhd|sd|dub|sub|eng|multi|cam|ts)\s*[\)\]]\s*$").replace(&t, "");
    t.trim().to_string()
}

fn fmt_from_name(name: &str) -> &'static str {
    let lower = name.trim().to_ascii_lowercase();
    if lower.ends_with(".vtt") { "vtt" } else { "srt" }
}

async fn resolve_imdb(app: &App, norm_mt: &str, tmdb: &str, imdb: &str) -> String {
    let direct = norm_imdb(imdb);
    if !direct.is_empty() {
        return direct;
    }
    let tm = tmdb.trim();
    if tm.is_empty() {
        return String::new();
    }
    if let Some(m) = app.meta_get(norm_mt, tm) {
        let cached = norm_imdb(&m.im);
        if !cached.is_empty() {
            return cached;
        }
    }
    if app.tmdb.enabled() {
        if let Some(d) = app.tmdb.get(format!("/{norm_mt}/{tm}/external_ids"), NO, 30 * TTL_D).await {
            let resolved = norm_imdb(&s(&d["imdb_id"]));
            if !resolved.is_empty() {
                return resolved;
            }
        }
    }
    String::new()
}

/// Primary no-key source: Stremio OpenSubtitles v3 addon (direct UTF-8 SRT downloads).
async fn query_stremio(app: &App, is_tv: bool, imdb: &str, season: Option<i64>, ep: Option<i64>) -> Vec<SubCand> {
    if imdb.is_empty() {
        return vec![];
    }
    let url = if is_tv {
        let (Some(sn), Some(en)) = (season, ep) else { return vec![] };
        if sn <= 0 || en <= 0 { return vec![]; }
        format!("https://opensubtitles-v3.strem.io/subtitles/series/{imdb}:{sn}:{en}.json")
    } else {
        format!("https://opensubtitles-v3.strem.io/subtitles/movie/{imdb}.json")
    };
    let resp = match app.http.get(&url).header("User-Agent", BROWSER_UA).timeout(Duration::from_secs(10)).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let Ok(j) = resp.json::<Value>().await else { return vec![] };
    let mut out = vec![];
    for item in j["subtitles"].as_array().into_iter().flatten() {
        let lang = s(&item["lang"]);
        if !is_english(&lang) {
            continue;
        }
        let dl = s(&item["url"]);
        if !dl.starts_with("http://") && !dl.starts_with("https://") {
            continue;
        }
        let fname = s(&item["subtitleFileName"]);
        let rname = s(&item["movieReleaseName"]);
        let id_raw = s(&item["id"]);
        let file_id = extract_os_file_id(&dl);
        let hi = detect_hi(&fname) || detect_hi(&rname);
        let fps = item["fpsMilli"].as_f64().map(|m| m / 1000.0).unwrap_or(0.0);
        let fmt = fmt_from_name(&fname);
        let id = if !file_id.is_empty() {
            format!("os:{file_id}")
        } else if !id_raw.is_empty() {
            format!("stremio:{id_raw}")
        } else {
            format!("stremio:{}", out.len())
        };
        out.push(SubCand {
            id,
            file_id,
            lang: "en".into(),
            label: clean_label(&fname, &rname, "English (OpenSubtitles)"),
            hi,
            hi_known: false,
            url: dl,
            source: "stremio".into(),
            format: fmt.into(),
            downloads: 0,
            rating: 0.0,
            fps,
            url_tier: 2,
        });
    }
    out
}

/// Secondary / fallback no-key source: OpenSubtitles legacy REST (returns rich HI/download/rating
/// metadata and supports title search fallback when IMDb ID is unavailable; downloads are `.srt.gz`).
async fn query_os_legacy(app: &App, is_tv: bool, imdb: &str, season: Option<i64>, ep: Option<i64>, title: &str) -> Vec<SubCand> {
    let url = if !imdb.is_empty() {
        let num = imdb.trim_start_matches("tt");
        if is_tv {
            let (Some(sn), Some(en)) = (season, ep) else { return vec![] };
            if sn <= 0 || en <= 0 { return vec![]; }
            format!("https://rest.opensubtitles.org/search/episode-{en}/imdbid-{num}/season-{sn}/sublanguageid-eng")
        } else {
            format!("https://rest.opensubtitles.org/search/imdbid-{num}/sublanguageid-eng")
        }
    } else {
        let q = clean_query_title(title);
        if q.len() < 2 {
            return vec![];
        }
        let q_enc = enc(&q.to_lowercase());
        if is_tv {
            let (Some(sn), Some(en)) = (season, ep) else { return vec![] };
            if sn <= 0 || en <= 0 { return vec![]; }
            format!("https://rest.opensubtitles.org/search/episode-{en}/query-{q_enc}/season-{sn}/sublanguageid-eng")
        } else {
            format!("https://rest.opensubtitles.org/search/query-{q_enc}/sublanguageid-eng")
        }
    };
    let resp = match app
        .http
        .get(&url)
        .header("User-Agent", OS_LEGACY_UA)
        .header("X-User-Agent", OS_LEGACY_UA)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let Ok(arr) = resp.json::<Value>().await else { return vec![] };
    let mut out = vec![];
    for item in arr.as_array().into_iter().flatten() {
        if s(&item["SubBad"]) == "1" {
            continue;
        }
        let lang = {
            let a = s(&item["SubLanguageID"]);
            if a.is_empty() { s(&item["ISO639"]) } else { a }
        };
        if !lang.is_empty() && !is_english(&lang) {
            continue;
        }
        let fmt_raw = s(&item["SubFormat"]).to_ascii_lowercase();
        let fname = s(&item["SubFileName"]);
        let fmt = if fmt_raw.is_empty() { fmt_from_name(&fname).to_string() } else { fmt_raw };
        if fmt != "srt" && fmt != "vtt" {
            continue;
        }
        if is_tv && imdb.is_empty() {
            if let (Some(sn), Some(en)) = (season, ep) {
                let rs = crate::util::i(&item["SeriesSeason"]);
                let re = crate::util::i(&item["SeriesEpisode"]);
                if (rs > 0 && rs != sn) || (re > 0 && re != en) {
                    continue;
                }
            }
        }
        let dl = s(&item["SubDownloadLink"]);
        if !dl.starts_with("http://") && !dl.starts_with("https://") {
            continue;
        }
        let file_id = s(&item["IDSubtitleFile"]);
        let rname = s(&item["MovieReleaseName"]);
        let hi_flag = s(&item["SubHearingImpaired"]);
        let hi_known = hi_flag == "0" || hi_flag == "1";
        let hi = if hi_known {
            hi_flag == "1" || detect_hi(&fname)
        } else {
            detect_hi(&fname) || detect_hi(&rname)
        };
        let downloads = crate::util::i(&item["SubDownloadsCnt"]);
        let rating = crate::util::num(&item["SubRating"]);
        let fps = crate::util::num(&item["MovieFPS"]);
        let id = if !file_id.is_empty() {
            format!("os:{file_id}")
        } else {
            format!("osleg:{}", out.len())
        };
        out.push(SubCand {
            id,
            file_id,
            lang: "en".into(),
            label: clean_label(&fname, &rname, "English (OpenSubtitles)"),
            hi,
            hi_known,
            url: dl,
            source: "opensubtitles".into(),
            format: fmt,
            downloads,
            rating,
            fps,
            url_tier: 0,
        });
    }
    out
}

/// Optional upgrade: OpenSubtitles.com REST API v1 (enabled only when `cfg.str("opensubtitlesKey")` is set).
async fn query_os_com(app: &App, is_tv: bool, tmdb: &str, imdb: &str, season: Option<i64>, ep: Option<i64>, title: &str) -> Vec<SubCand> {
    let key = app.cfg.str("opensubtitlesKey").trim().to_string();
    if key.is_empty() {
        return vec![];
    }
    let mut params: Vec<(&str, String)> = vec![("languages", "en".into()), ("order_by", "download_count".into())];
    if !imdb.is_empty() {
        let num = imdb.trim_start_matches("tt").trim_start_matches('0');
        if !num.is_empty() {
            if is_tv {
                params.push(("parent_imdb_id", num.into()));
            } else {
                params.push(("imdb_id", num.into()));
            }
        }
    } else if !tmdb.trim().is_empty() {
        if is_tv {
            params.push(("parent_tmdb_id", tmdb.trim().into()));
        } else {
            params.push(("tmdb_id", tmdb.trim().into()));
        }
    } else {
        let q = clean_query_title(title);
        if q.len() < 2 {
            return vec![];
        }
        params.push(("query", q));
    }
    if is_tv {
        if let (Some(sn), Some(en)) = (season, ep) {
            if sn > 0 && en > 0 {
                params.push(("season_number", sn.to_string()));
                params.push(("episode_number", en.to_string()));
            }
        }
    }
    let qs: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("https://api.opensubtitles.com/api/v1/subtitles?{qs}");
    let resp = match app
        .http
        .get(&url)
        .header("Api-Key", &key)
        .header("User-Agent", "tv-rs v1.0")
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let Ok(j) = resp.json::<Value>().await else { return vec![] };
    let mut out = vec![];
    for item in j["data"].as_array().into_iter().flatten() {
        let a = &item["attributes"];
        let Some(fid) = a["files"].get(0).and_then(|f| f["file_id"].as_i64()) else { continue };
        let fname = s(&a["files"][0]["file_name"]);
        let release = s(&a["release"]);
        let hi = a["hearing_impaired"].as_bool().unwrap_or_else(|| detect_hi(&fname) || detect_hi(&release));
        let downloads = a["download_count"].as_i64().unwrap_or(0);
        let rating = a["ratings"].as_f64().unwrap_or(0.0);
        let fps = a["fps"].as_f64().unwrap_or(0.0);
        let fmt = fmt_from_name(&fname);
        out.push(SubCand {
            id: format!("oscom:{fid}"),
            file_id: fid.to_string(),
            lang: "en".into(),
            label: clean_label(&fname, &release, "English (OpenSubtitles.com)"),
            hi,
            hi_known: true,
            url: format!("oscom://{fid}"),
            source: "opensubtitles.com".into(),
            format: fmt.into(),
            downloads,
            rating,
            fps,
            url_tier: 3,
        });
    }
    out
}

/// Optional upgrade: Wyzie Subs (enabled only when `cfg.str("wyzieKey")` is set; public endpoint requires key).
async fn query_wyzie(app: &App, is_tv: bool, tmdb: &str, imdb: &str, season: Option<i64>, ep: Option<i64>) -> Vec<SubCand> {
    let key = app.cfg.str("wyzieKey").trim().to_string();
    if key.is_empty() {
        return vec![];
    }
    let id = if !imdb.is_empty() { imdb } else { tmdb.trim() };
    if id.is_empty() {
        return vec![];
    }
    let mut url = format!("https://sub.wyzie.io/search?id={}&language=en&key={}", enc(id), enc(&key));
    if is_tv {
        if let (Some(sn), Some(en)) = (season, ep) {
            if sn > 0 && en > 0 {
                url.push_str(&format!("&season={sn}&episode={en}"));
            }
        }
    }
    let resp = match app.http.get(&url).header("User-Agent", BROWSER_UA).timeout(Duration::from_secs(10)).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let Ok(arr) = resp.json::<Value>().await else { return vec![] };
    let mut out = vec![];
    for item in arr.as_array().into_iter().flatten() {
        let dl = s(&item["url"]);
        if !dl.starts_with("http://") && !dl.starts_with("https://") {
            continue;
        }
        let fmt = {
            let f = s(&item["format"]).to_ascii_lowercase();
            if f == "vtt" { "vtt" } else { "srt" }
        };
        let label_raw = {
            let r = s(&item["release"]);
            if r.is_empty() { s(&item["display"]) } else { r }
        };
        let hi = item["isHearingImpaired"].as_bool().unwrap_or_else(|| detect_hi(&label_raw));
        let wid = s(&item["id"]);
        out.push(SubCand {
            id: if wid.is_empty() { format!("wyzie:{}", out.len()) } else { format!("wyzie:{wid}") },
            file_id: String::new(),
            lang: "en".into(),
            label: clean_label(&label_raw, "", "English (Wyzie)"),
            hi,
            hi_known: item.get("isHearingImpaired").and_then(|v| v.as_bool()).is_some(),
            url: dl,
            source: "wyzie".into(),
            format: fmt.into(),
            downloads: 0,
            rating: 0.0,
            fps: 0.0,
            url_tier: 1,
        });
    }
    out
}

fn score_cand(c: &SubCand) -> f64 {
    let mut sc = 0.0;
    let l = &c.label;
    // Heavily penalize partial-disc rips and cam/screener timings
    if crate::re!(r"(?i)(^|[\W_])(cd\s*[123]|disc\s*[123]|part\s*[123]|dvdscr|cam|hdcam|telesync|workprint)([\W_]|$)").is_match(l) {
        sc -= 500.0;
    }
    // Reward clean modern digital / disc sources
    if crate::re!(r"(?i)(web[\s._-]?dl|webrip|bluray|blu-ray|bdrip|remux)").is_match(l) {
        sc += 60.0;
    }
    if crate::re!(r"(?i)(2160p|1080p|4k|uhd|amzn|nf|netflix|dsnp|atvp|hmax|ma)").is_match(l) {
        sc += 25.0;
    }
    if c.downloads > 0 {
        sc += ((c.downloads as f64) + 1.0).log10() * 25.0;
    }
    if c.rating > 0.0 {
        sc += c.rating.clamp(0.0, 10.0) * 4.0;
    }
    if (c.fps - 23.976).abs() < 0.05 || (c.fps - 24.0).abs() < 0.05 {
        sc += 10.0;
    }
    sc += (c.url_tier as f64) * 8.0;
    sc
}

fn merge_and_rank(raw: Vec<SubCand>) -> Vec<SubCand> {
    let mut by_fid: HashMap<String, SubCand> = HashMap::new();
    let mut rest: Vec<SubCand> = vec![];
    for c in raw {
        if !c.file_id.is_empty() {
            if let Some(prev) = by_fid.get_mut(&c.file_id) {
                // Keep authoritative HI flag when known; prefer higher download/rating; prefer cleaner URL tier
                if c.hi_known && !prev.hi_known {
                    prev.hi = c.hi;
                    prev.hi_known = true;
                }
                if c.downloads > prev.downloads {
                    prev.downloads = c.downloads;
                }
                if c.rating > prev.rating {
                    prev.rating = c.rating;
                }
                if prev.fps <= 0.0 && c.fps > 0.0 {
                    prev.fps = c.fps;
                }
                if c.url_tier > prev.url_tier {
                    prev.url = c.url;
                    prev.source = c.source;
                    prev.id = c.id;
                    prev.url_tier = c.url_tier;
                }
                if prev.label == "English" && c.label != "English" {
                    prev.label = c.label;
                }
                continue;
            }
            by_fid.insert(c.file_id.clone(), c);
        } else {
            rest.push(c);
        }
    }
    let mut all: Vec<SubCand> = by_fid.into_values().chain(rest).collect();
    // Clean non-SDH English always ranked ahead of SDH/HI; then highest quality score
    all.sort_by(|a, b| a.hi.cmp(&b.hi).then_with(|| score_cand(b).total_cmp(&score_cand(a))));
    let mut seen_url: HashSet<String> = HashSet::new();
    let mut seen_lbl: HashSet<(String, bool)> = HashSet::new();
    let mut out: Vec<SubCand> = vec![];
    for c in all {
        if !seen_url.insert(c.url.clone()) {
            continue;
        }
        let lk = (norm(&c.label), c.hi);
        if !lk.0.is_empty() && !seen_lbl.insert(lk) {
            continue;
        }
        out.push(c);
        if out.len() >= 25 {
            break;
        }
    }
    out
}

/// Search external English subtitles for a movie or episode.
/// Resolves `imdb` from local metadata or TMDB `/{mt}/{tmdb}/external_ids` (cached 30d) when missing.
pub async fn search(app: &Arc<App>, mt: &str, tmdb: &str, imdb: &str, season: Option<i64>, ep: Option<i64>, title: &str) -> Value {
    let is_tv = matches!(mt.trim().to_ascii_lowercase().as_str(), "tv" | "series" | "s" | "e" | "episode")
        || (season.is_some() && ep.is_some());
    let norm_mt = if is_tv { "tv" } else { "movie" };
    let imdb = resolve_imdb(app, norm_mt, tmdb, imdb).await;

    let cache_key = format!(
        "{norm_mt}:{imdb}:{}:{:?}:{:?}:{}:{}:{}",
        tmdb.trim(),
        season,
        ep,
        norm(title),
        !app.cfg.str("opensubtitlesKey").trim().is_empty(),
        !app.cfg.str("wyzieKey").trim().is_empty(),
    );
    let cache = SEARCH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((t, v)) = cache.lock().unwrap().get(&cache_key).cloned() {
        if now_ms() - t < SEARCH_TTL_MS {
            return v;
        }
    }

    let (stremio, os_leg, os_com, wyzie) = tokio::join!(
        query_stremio(app, is_tv, &imdb, season, ep),
        query_os_legacy(app, is_tv, &imdb, season, ep, title),
        query_os_com(app, is_tv, tmdb, &imdb, season, ep, title),
        query_wyzie(app, is_tv, tmdb, &imdb, season, ep),
    );
    let mut combined = Vec::with_capacity(stremio.len() + os_leg.len() + os_com.len() + wyzie.len());
    combined.extend(os_com);
    combined.extend(stremio);
    combined.extend(wyzie);
    combined.extend(os_leg);

    let ranked = merge_and_rank(combined);
    let results: Vec<Value> = ranked
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "lang": c.lang,
                "label": c.label,
                "hi": c.hi,
                "url": c.url,
                "source": c.source,
                "format": c.format,
            })
        })
        .collect();
    let out = json!({ "imdb": imdb, "results": results });
    if out["results"].as_array().map_or(false, |a| !a.is_empty()) {
        let mut g = cache.lock().unwrap();
        if g.len() >= 200 {
            let cutoff = now_ms() - SEARCH_TTL_MS;
            g.retain(|_, (t, _)| *t > cutoff);
            if g.len() >= 200 {
                g.clear();
            }
        }
        g.insert(cache_key, (now_ms(), out.clone()));
    }
    out
}

// ---------------------------------------------------------------- decoding & SRT <-> WebVTT

const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

/// Decode raw subtitle bytes safely: handles UTF-8 (with or without BOM), UTF-16 LE/BE, and Windows-1252.
fn decode_sub_bytes(raw: &[u8]) -> String {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        if let Ok(s) = std::str::from_utf8(&raw[3..]) {
            return s.to_string();
        }
    }
    if raw.starts_with(&[0xFF, 0xFE]) && raw.len() >= 2 {
        let units: Vec<u16> = raw[2..].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if raw.starts_with(&[0xFE, 0xFF]) && raw.len() >= 2 {
        let units: Vec<u16> = raw[2..].chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    // Windows-1252 (superset of ISO-8859-1) fallback — preserves smart quotes, dashes, accents cleanly.
    let mut out = String::with_capacity(raw.len());
    for &b in raw {
        if b < 0x80 {
            out.push(b as char);
        } else if b < 0xA0 {
            out.push(CP1252_HIGH[(b - 0x80) as usize]);
        } else {
            out.push(b as char);
        }
    }
    out
}

fn parse_ts_ms(tok: &str) -> Option<i64> {
    let tok = tok.trim();
    if tok.is_empty() || tok.starts_with('-') {
        return None;
    }
    let (hms, frac) = match tok.split_once(',').or_else(|| tok.split_once('.')) {
        Some((a, b)) => (a, b),
        None => (tok, "0"),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec): (i64, i64, i64) = match parts.len() {
        3 => (parts[0].parse().ok()?, parts[1].parse().ok()?, parts[2].parse().ok()?),
        2 => (0, parts[0].parse().ok()?, parts[1].parse().ok()?),
        _ => return None,
    };
    if h < 0 || m < 0 || m > 59 || sec < 0 || sec > 59 {
        return None;
    }
    let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
    let ms: i64 = match digits.len() {
        0 => 0,
        1 => digits.parse::<i64>().ok()? * 100,
        2 => digits.parse::<i64>().ok()? * 10,
        _ => digits.parse::<i64>().ok()?,
    };
    Some((h * 3600 + m * 60 + sec) * 1000 + ms)
}

fn parse_timing_line(line: &str) -> Option<(i64, i64, String)> {
    let (left, right) = line.split_once("-->")?;
    let a = parse_ts_ms(left.trim().split_whitespace().last()?)?;
    let mut r_iter = right.trim().splitn(2, |c: char| c.is_whitespace());
    let b_tok = r_iter.next()?;
    let b = parse_ts_ms(b_tok)?;
    let settings = r_iter.next().unwrap_or("").trim().to_string();
    if b < a {
        return None;
    }
    let b = if b == a { a + 500 } else { b };
    Some((a, b, settings))
}

fn fmt_ts(ms: i64, sep: char) -> String {
    let ms = ms.max(0);
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let sec = (ms % 60_000) / 1000;
    let rem = ms % 1000;
    format!("{h:02}:{m:02}:{sec:02}{sep}{rem:03}")
}

fn clean_cue_line(line: &str) -> String {
    // Strip ASS/SSA override blocks like {\an8} and HTML <font ...> tags while keeping <i>/<b>/<u>.
    let s1 = crate::re!(r"\{\\[^}]*\}").replace_all(line, "");
    let s2 = crate::re!(r"(?i)</?font[^>]*>").replace_all(&s1, "");
    s2.trim_end().to_string()
}

fn sanitize_text(raw: &str) -> String {
    let s = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            '\0'..='\u{001F}' | '\u{007F}' => {}
            _ => out.push(c),
        }
    }
    out
}

fn parse_cues(clean: &str) -> Vec<(i64, i64, String, Vec<String>)> {
    let lines: Vec<&str> = clean.lines().collect();
    let mut cues: Vec<(i64, i64, String, Vec<String>)> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // Skip WebVTT header / NOTE / STYLE / REGION blocks cleanly
        if trimmed.starts_with("WEBVTT") || trimmed.starts_with("NOTE") || trimmed == "STYLE" || trimmed == "REGION" {
            i += 1;
            while i < lines.len() && !lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        }
        if let Some((a, b, settings)) = parse_timing_line(trimmed) {
            i += 1;
            let mut body: Vec<String> = vec![];
            while i < lines.len() {
                let l = lines[i];
                if parse_timing_line(l.trim()).is_some() {
                    // If the preceding line was an SRT sequence number directly before the next timing line, drop it.
                    if let Some(last) = body.last() {
                        let lt = last.trim();
                        if !lt.is_empty() && lt.chars().all(|c| c.is_ascii_digit()) {
                            body.pop();
                        }
                    }
                    break;
                }
                if l.trim().is_empty() {
                    i += 1;
                    // End of cue block (skip consecutive blank lines)
                    while i < lines.len() && lines[i].trim().is_empty() {
                        i += 1;
                    }
                    break;
                }
                let cleaned = clean_cue_line(l);
                if !cleaned.trim().is_empty() {
                    body.push(cleaned);
                }
                i += 1;
            }
            while body.last().map_or(false, |l| l.trim().is_empty()) {
                body.pop();
            }
            if !body.is_empty() {
                cues.push((a, b, settings, body));
            }
        } else {
            i += 1;
        }
    }
    cues
}

/// Convert SRT (or pass through already-valid WebVTT) into clean, canonical UTF-8 WebVTT.
/// Strips BOM and invalid control sequences, normalizes `,` -> `.` timestamps, and cleans SSA/font tags.
pub fn srt_to_vtt(raw: &str) -> String {
    let clean = sanitize_text(raw);
    let trimmed = clean.trim_start();
    // Already-valid WebVTT: preserve header/styles/positioning while normalizing comma timestamps.
    if trimmed.starts_with("WEBVTT")
        && trimmed.as_bytes().get(6).map_or(true, |b| matches!(b, b'\n' | b' ' | b'\t'))
    {
        let mut out = String::with_capacity(trimmed.len() + 8);
        let mut lines = trimmed.lines();
        if let Some(hdr) = lines.next() {
            out.push_str(hdr.trim_end());
            out.push('\n');
        }
        let mut need_sep = true;
        for line in lines {
            if need_sep {
                if !line.trim().is_empty() && !line.contains(':') {
                    out.push('\n');
                }
                need_sep = false;
            }
            if let Some((a, b, settings)) = parse_timing_line(line.trim()) {
                out.push_str(&fmt_ts(a, '.'));
                out.push_str(" --> ");
                out.push_str(&fmt_ts(b, '.'));
                if !settings.is_empty() {
                    out.push(' ');
                    out.push_str(&settings);
                }
                out.push('\n');
            } else {
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        return out;
    }

    let cues = parse_cues(&clean);
    let mut out = String::with_capacity(clean.len() + 16);
    out.push_str("WEBVTT\n\n");
    for (a, b, settings, body) in cues {
        out.push_str(&fmt_ts(a, '.'));
        out.push_str(" --> ");
        out.push_str(&fmt_ts(b, '.'));
        if !settings.is_empty() {
            out.push(' ');
            out.push_str(&settings);
        }
        out.push('\n');
        for l in body {
            out.push_str(&l);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn vtt_to_srt(vtt: &str) -> String {
    let cues = parse_cues(vtt);
    let mut out = String::with_capacity(vtt.len());
    for (idx, (a, b, _, body)) in cues.into_iter().enumerate() {
        out.push_str(&(idx + 1).to_string());
        out.push('\n');
        out.push_str(&fmt_ts(a, ','));
        out.push_str(" --> ");
        out.push_str(&fmt_ts(b, ','));
        out.push('\n');
        for l in body {
            out.push_str(&l);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn unpack_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&[0x1F, 0x8B]) {
        let mut dec = flate2::read::GzDecoder::new(bytes).take(MAX_SUB_BYTES as u64);
        let mut out = Vec::new();
        dec.read_to_end(&mut out).map_err(|e| format!("gzip decode failed: {e}"))?;
        return Ok(out);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("zip open failed: {e}"))?;
        let mut best: Option<(i32, Vec<u8>)> = None;
        for i in 0..archive.len() {
            let Ok(file) = archive.by_index(i) else { continue };
            if file.is_dir() {
                continue;
            }
            let name = file.name().to_string();
            if name.contains("__MACOSX/") || name.starts_with('.') {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if !lower.ends_with(".srt") && !lower.ends_with(".vtt") {
                continue;
            }
            let mut buf = Vec::new();
            if file.take(MAX_SUB_BYTES as u64).read_to_end(&mut buf).is_err() || buf.is_empty() {
                continue;
            }
            let mut rank = (buf.len().min(500_000) / 1000) as i32;
            if lower.ends_with(".srt") { rank += 50; }
            if lower.contains("eng") || lower.contains(".en.") { rank += 100; }
            if best.as_ref().map_or(true, |(br, _)| rank > *br) {
                best = Some((rank, buf));
            }
        }
        return best.map(|(_, b)| b).ok_or_else(|| "zip contained no .srt or .vtt file".into());
    }
    Ok(bytes.to_vec())
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content.as_bytes()).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename failed: {e}")
    })
}

async fn resolve_oscom_download(app: &App, file_id_str: &str) -> Result<String, String> {
    let key = app.cfg.str("opensubtitlesKey").trim().to_string();
    if key.is_empty() {
        return Err("OpenSubtitles API key not configured".into());
    }
    let fid: i64 = file_id_str.trim().parse().map_err(|_| "invalid OpenSubtitles file_id".to_string())?;
    let resp = app
        .http
        .post("https://api.opensubtitles.com/api/v1/download")
        .header("Api-Key", &key)
        .header("User-Agent", "tv-rs v1.0")
        .header("Accept", "application/json")
        .json(&json!({ "file_id": fid }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("OpenSubtitles download request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("OpenSubtitles download HTTP {}", resp.status()));
    }
    let j: Value = resp.json().await.map_err(|e| format!("bad OpenSubtitles JSON: {e}"))?;
    let link = s(&j["link"]);
    if link.starts_with("http://") || link.starts_with("https://") {
        Ok(link)
    } else {
        Err("OpenSubtitles did not return a download link".into())
    }
}

/// Download an external subtitle (plain/gzip/zip), validate real cues, write sanitized
/// `<key>.srt` and `<key>.vtt` under `data/ext-subs/`, and return metadata for browser & mpv.
pub async fn fetch(app: &Arc<App>, key: &str, url: &str) -> Result<Value, String> {
    let safe = sanitize_key(key);
    if safe.is_empty() {
        return Err("invalid subtitle key".into());
    }
    let dl_url = if let Some(fid) = url.trim().strip_prefix("oscom://") {
        resolve_oscom_download(app, fid).await?
    } else {
        url.trim().to_string()
    };
    if !dl_url.starts_with("http://") && !dl_url.starts_with("https://") {
        return Err("invalid subtitle URL".into());
    }
    let ua = if dl_url.contains("opensubtitles.org") { OS_LEGACY_UA } else { BROWSER_UA };
    let resp = app
        .http
        .get(&dl_url)
        .header("User-Agent", ua)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("subtitle download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("subtitle download HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("subtitle read failed: {e}"))?;
    if bytes.is_empty() {
        return Err("downloaded subtitle is empty".into());
    }
    if bytes.len() > MAX_SUB_BYTES {
        return Err("downloaded subtitle exceeds 10 MB limit".into());
    }
    let unpacked = unpack_bytes(&bytes)?;
    let text = decode_sub_bytes(&unpacked);
    let vtt = srt_to_vtt(&text);
    let cues = crate::subsync::parse_vtt(&vtt).len();
    if cues == 0 {
        return Err("downloaded file contains no valid subtitle cues".into());
    }
    let srt = vtt_to_srt(&vtt);
    let dir = sub_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir ext-subs failed: {e}"))?;
    let srt_path = dir.join(format!("{safe}.srt"));
    let vtt_path = dir.join(format!("{safe}.vtt"));
    atomic_write(&srt_path, &srt)?;
    atomic_write(&vtt_path, &vtt)?;
    Ok(json!({
        "ok": true,
        "key": key,
        "vttUrl": format!("/api/extsubs/file/{safe}.vtt"),
        "srtPath": srt_path.to_string_lossy().to_string(),
        "cues": cues,
    }))
}

/// Auto-sync a cached external subtitle against the active ffmpeg session's audio tap.
pub async fn sync_cached(app: &Arc<App>, key: &str) -> Value {
    let Some(vtt_path) = cached_vtt_path(app, key) else {
        return json!({ "ready": false, "reason": "no cached external subtitle" });
    };
    let pcm = app.stream.sync_paths(0).0;
    let base = (app.stream.session_start().unwrap_or(0.0) * 1000.0) as i64;
    let mut r = tokio::task::spawn_blocking(move || crate::subsync::sync_at(&pcm, &vtt_path, base))
        .await
        .unwrap_or_else(|_| json!({ "ready": false, "reason": "sync task failed" }));
    r["base"] = json!(base);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sanitize_key_blocks_traversal() {
        assert_eq!(sanitize_key("m:12345"), "m_12345");
        assert_eq!(sanitize_key("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_key("e:999-a_b"), "e_999-a_b");
        assert_eq!(sanitize_key("///..."), "");
    }

    #[test]
    fn srt_to_vtt_basic_and_bom_and_tags() {
        let srt = "\u{FEFF}1\r\n00:00:01,500 --> 00:00:04,250\r\n{\\an8}<font color=\"#fff\">Hello, <i>world</i>!</font>\r\n\r\n2\r\n0:01:02,5 --> 0:01:05,000\r\nSecond line\r\n";
        let vtt = srt_to_vtt(srt);
        assert!(vtt.starts_with("WEBVTT\n\n"));
        assert!(vtt.contains("00:00:01.500 --> 00:00:04.250\nHello, <i>world</i>!\n"));
        assert!(vtt.contains("00:01:02.500 --> 00:01:05.000\nSecond line\n"));
        assert_eq!(crate::subsync::parse_vtt(&vtt).len(), 2);
        let back = vtt_to_srt(&vtt);
        assert!(back.contains("1\n00:00:01,500 --> 00:00:04,250\nHello, <i>world</i>!\n"));
    }

    #[test]
    fn srt_to_vtt_passthrough_vtt() {
        let vtt = "\u{FEFF}WEBVTT\r\n\r\n00:00:02.000 --> 00:00:05.000 align:start\r\nAlready VTT\r\n";
        let out = srt_to_vtt(vtt);
        assert!(out.starts_with("WEBVTT\n\n"));
        assert!(out.contains("00:00:02.000 --> 00:00:05.000 align:start\nAlready VTT\n"));
        assert_eq!(crate::subsync::parse_vtt(&out).len(), 1);
    }

    #[test]
    fn decode_cp1252_and_utf16() {
        // CP1252: 0x93 = “, 0x94 = ”, 0x92 = ’, 0x97 = —, 0xE9 = é
        let cp = b"\x93It\x92s caf\xe9 \x97 good!\x94";
        assert_eq!(decode_sub_bytes(cp), "\u{201C}It\u{2019}s caf\u{00E9} \u{2014} good!\u{201D}");
        let mut u16le = vec![0xFF, 0xFE];
        for c in "Hello".encode_utf16() {
            u16le.extend_from_slice(&c.to_le_bytes());
        }
        assert_eq!(decode_sub_bytes(&u16le), "Hello");
    }

    #[test]
    fn unpack_gzip_and_zip() {
        let sample = b"1\n00:00:01,000 --> 00:00:03,000\nHi\n";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(sample).unwrap();
        let gz = enc.finish().unwrap();
        assert_eq!(unpack_bytes(&gz).unwrap(), sample);

        let mut zbuf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut zbuf);
            zw.start_file("movie.en.srt", zip::write::SimpleFileOptions::default()).unwrap();
            zw.write_all(sample).unwrap();
            zw.finish().unwrap();
        }
        assert_eq!(unpack_bytes(&zbuf.into_inner()).unwrap(), sample);
    }

    #[test]
    fn hi_detection_and_ranking() {
        assert!(detect_hi("The.Dark.Knight.2008.1080p.BluRay.HI.srt"));
        assert!(detect_hi("Show.S01E01.SDH.en.srt"));
        assert!(!detect_hi("Breaking.Bad.S01E01.NonHI.cc.en.srt"));
        assert!(!detect_hi("Dune.Part.Two.2024.1080p.AMZN.WEB-DL.srt"));
    }
}
