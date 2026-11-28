// Personal Cinema DNA (taste profile, watch stats, deterministic persona, director/actor
// insights & personalized recs) + Specialty & Prestige Hubs (Stand-Up Comedy, Live Concerts
// & Music Films, World Cinema, Apple TV+, HBO, IMDb Top 250 & Under-95m Tight Bangers).
use crate::app::App;
use crate::catalog::{Catalog, Item};
use crate::discover::{hidden_item, watched_set};
use crate::meta::Meta;
use crate::rec::{hidden_ratings, sp};
use crate::tmdb::TTL_D;
use crate::util::{norm, s, tmdb_img};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const DEFAULT_MOVIE_MINS: f64 = 105.0;
const DEFAULT_EPISODE_MINS: f64 = 45.0;
const COMPLETE_PCT: f64 = 0.88;
const MAX_CREDIT_FETCH: usize = 12;
const CREDIT_CONCURRENCY: usize = 5;
const TMDB_STANDUP_KW: i64 = 9716;

// ---------------------------------------------------------------------------
// 1. Shared helpers (genres, countries, languages, decades, runtime, UHD)
// ---------------------------------------------------------------------------

fn canonical_genre_id(g: i64) -> i64 {
    match g {
        10759 => 28,    // Action & Adventure -> Action
        10765 => 878,   // Sci-Fi & Fantasy -> Sci-Fi
        10768 => 10752, // War & Politics -> War
        other => other,
    }
}

fn canonical_genre_name(id: i64) -> Option<&'static str> {
    crate::meta::genre_name(false, id).or_else(|| crate::meta::genre_name(true, id))
}

fn norm_country(c: &str) -> String {
    let up = c.trim().to_uppercase();
    if up == "UK" {
        "GB".to_string()
    } else {
        up
    }
}

fn item_has_uhd(it: &Item) -> bool {
    (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd)
}

fn item_year(it: &Item, meta: Option<&Meta>) -> i32 {
    let my = meta.map(|m| m.year()).unwrap_or(0);
    if my > 0 {
        my
    } else {
        it.year.unwrap_or(0)
    }
}

fn item_sort_date(it: &Item, meta: Option<&Meta>) -> String {
    if let Some(m) = meta {
        if !m.d.is_empty() {
            return m.d.clone();
        }
    }
    let y = item_year(it, meta);
    if y > 0 {
        format!("{y:04}-01-01")
    } else {
        String::new()
    }
}

fn item_bayes(it: &Item, meta: Option<&Meta>) -> f64 {
    match meta {
        Some(m) if m.va > 0.0 => m.bayes(),
        _ => (it.rating / 10.0).clamp(0.0, 1.0) * 6.4,
    }
}

fn item_pop(it: &Item, meta: Option<&Meta>) -> f64 {
    meta.map(|m| m.p).unwrap_or(it.rating).max(0.0)
}

fn item_votes(meta: Option<&Meta>) -> i64 {
    meta.map(|m| m.vc).unwrap_or(0)
}

fn has_poster(it: &Item, meta: Option<&Meta>) -> bool {
    !it.poster.is_empty() || meta.map_or(false, |m| !m.pp.is_empty())
}

fn item_countries(it: &Item, meta: Option<&Meta>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = meta {
        for c in &m.oc {
            let code = norm_country(c);
            if !code.is_empty() && !out.contains(&code) {
                out.push(code);
            }
        }
    }
    if out.is_empty() && !it.origin.is_empty() {
        let code = norm_country(&it.origin);
        if !code.is_empty() {
            out.push(code);
        }
    }
    out
}

/// Collect cleaned, uppercased category names across the item's primary category and all variants.
fn item_cat_uppers(cat: &Catalog, it: &Item) -> Vec<String> {
    let kind = if it.k == "s" { "series" } else { "movie" };
    let mut out = Vec::with_capacity(1 + it.variants.len());
    let mut push_cid = |cid: &str| {
        if cid.is_empty() {
            return;
        }
        let name = cat.cat_name(kind, cid).to_uppercase();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    };
    push_cid(&it.cat);
    let raw = if it.k == "s" {
        &cat.series_raw
    } else {
        &cat.movie_raw
    };
    for v in &it.variants {
        if let Some(vit) = raw.get(&v.id) {
            push_cid(&vit.cat);
        }
    }
    out
}

fn lang_aliases(code: &str) -> &'static str {
    match code {
        "ko" => "korean korea",
        "ja" => "japanese japan",
        "fr" => "french france",
        "es" => "spanish spain mexico argentina",
        "de" => "german germany austria",
        "it" => "italian italy",
        "zh" | "cn" | "yue" => "chinese mandarin cantonese hong kong taiwan",
        "sv" => "swedish sweden nordic scandinavian",
        "da" => "danish denmark nordic scandinavian",
        "no" | "nb" => "norwegian norway nordic scandinavian",
        "fi" => "finnish finland nordic",
        "is" => "icelandic iceland nordic",
        "hi" | "ta" | "te" | "ml" => "indian hindi bollywood tamil telugu",
        "pt" => "portuguese brazil portugal",
        "pl" => "polish poland",
        "tr" => "turkish turkey",
        "fa" => "persian iranian iran",
        "th" => "thai thailand",
        "ru" => "russian russia",
        "nl" => "dutch netherlands",
        _ => "",
    }
}

fn country_aliases(code: &str) -> &'static str {
    match code {
        "US" => "usa united states american",
        "GB" => "uk united kingdom british england",
        "AU" => "australia australian",
        "CA" => "canada canadian",
        "KR" => "korea korean south korea",
        "JP" => "japan japanese",
        "FR" => "france french",
        "IT" => "italy italian",
        "ES" => "spain spanish",
        "DE" => "germany german",
        "SE" => "sweden swedish nordic",
        "DK" => "denmark danish nordic",
        "NO" => "norway norwegian nordic",
        "MX" => "mexico mexican",
        "IN" => "india indian",
        "HK" => "hong kong",
        "TW" => "taiwan taiwanese",
        _ => "",
    }
}

fn standup_keywords(app: &App) -> HashSet<i64> {
    let mut set: HashSet<i64> = app
        .kw_cache
        .lock()
        .unwrap()
        .iter()
        .filter(|(k, _)| k.starts_with("stand"))
        .filter_map(|(_, v)| *v)
        .collect();
    set.insert(TMDB_STANDUP_KW);
    set
}

fn decade_bucket(year: i32) -> Option<&'static str> {
    if year < 1880 {
        None
    } else if year >= 2020 {
        Some("2020s")
    } else if year >= 2010 {
        Some("2010s")
    } else if year >= 2000 {
        Some("2000s")
    } else if year >= 1990 {
        Some("1990s")
    } else {
        Some("Classic")
    }
}

fn decade_rank(dec: &str) -> u8 {
    match dec {
        "2020s" => 5,
        "2010s" => 4,
        "2000s" => 3,
        "1990s" => 2,
        "Classic" => 1,
        _ => 0,
    }
}

fn percentile_sorted(sorted: &[i64], pct: f64) -> i64 {
    let n = sorted.len();
    if n == 0 {
        return 0;
    }
    if n == 1 {
        return sorted[0];
    }
    let pos = pct.clamp(0.0, 1.0) * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = pos - lo as f64;
        (sorted[lo] as f64 * (1.0 - frac) + sorted[hi] as f64 * frac).round() as i64
    }
}

/// Compute runtime sweet spot (IQR / median window) from watched/liked movie runtimes in minutes.
fn compute_runtime_sweet_spot(raw_runtimes: &[i64]) -> String {
    let mut rts: Vec<i64> = raw_runtimes
        .iter()
        .copied()
        .filter(|&r| (30..=300).contains(&r))
        .collect();
    if rts.is_empty() {
        return String::new();
    }
    rts.sort_unstable();
    if rts.len() == 1 {
        let r = rts[0];
        return format!("{}–{} min", (r - 10).max(30), r + 10);
    }
    let q1 = percentile_sorted(&rts, 0.25);
    let med = percentile_sorted(&rts, 0.50);
    let q3 = percentile_sorted(&rts, 0.75);
    let (lo, hi) = if q3 - q1 < 10 {
        ((med - 10).max(30), med + 10)
    } else {
        (q1, q3)
    };
    format!("{lo}–{hi} min")
}

// ---------------------------------------------------------------------------
// 2. Deterministic Cinephile Persona Classifier
// ---------------------------------------------------------------------------

struct PersonaInput<'a> {
    top_genres: &'a [(i64, &'static str, usize)],
    top_decade: Option<&'a str>,
    uhd_share_pct: u64,
    avg_rating: f64,
    movies_watched: usize,
    episodes_watched: usize,
    non_en_share_pct: u64,
    doc_share_pct: u64,
    top_director: Option<&'a str>,
    top_platform: Option<&'a str>,
}

fn compute_persona(inp: &PersonaInput<'_>) -> (String, String) {
    if inp.movies_watched == 0 && inp.episodes_watched == 0 && inp.top_genres.is_empty() {
        return (
            "Fresh Screen Explorer".to_string(),
            "Start watching or rating titles to unlock your personal cinema DNA.".to_string(),
        );
    }

    let g1 = inp.top_genres.first().map(|x| x.0);
    let g2 = inp.top_genres.get(1).map(|x| x.0);
    let g1_name = inp.top_genres.first().map(|x| x.1);
    let g2_name = inp.top_genres.get(1).map(|x| x.1);
    let has_g = |id: i64| g2 == Some(id) || inp.top_genres.get(2).map(|x| x.0) == Some(id);
    let high_4k = inp.uhd_share_pct >= 50;
    let acclaim = inp.avg_rating >= 7.5;
    let classic_era = matches!(inp.top_decade, Some("Classic" | "1990s"));
    let world_lover = inp.non_en_share_pct >= 30;
    let tv_binger = inp.episodes_watched >= 8 && inp.episodes_watched >= inp.movies_watched.saturating_mul(4);

    let tag: &str = if g1 == Some(99) || inp.doc_share_pct >= 35 {
        if has_g(80) {
            "True-Crime Doc Explorer"
        } else if has_g(36) || has_g(10752) {
            "History Doc Devotee"
        } else {
            "Real-World Doc Explorer"
        }
    } else {
        match g1 {
            Some(878) => {
                if high_4k && has_g(28) {
                    "4K Sci-Fi Spectacle Fan"
                } else if acclaim || has_g(18) {
                    "Prestige Sci-Fi Cinephile"
                } else if has_g(53) || has_g(9648) {
                    "Mind-Bending Sci-Fi Sleuth"
                } else {
                    "Sci-Fi Visionary"
                }
            }
            Some(53) | Some(9648) => {
                if has_g(80) || has_g(27) {
                    "Dark Thriller Purist"
                } else if acclaim || has_g(18) {
                    "Prestige Suspense Fan"
                } else {
                    "Edge-of-Seat Thriller Fan"
                }
            }
            Some(80) => {
                if high_4k && has_g(28) {
                    "4K Action & Crime Fan"
                } else if has_g(18) || acclaim {
                    "Prestige Crime Connoisseur"
                } else if has_g(53) || has_g(9648) {
                    "Dark Thriller Purist"
                } else {
                    "Noir & Crime Devotee"
                }
            }
            Some(28) | Some(12) => {
                if high_4k && has_g(80) {
                    "4K Action & Crime Fan"
                } else if high_4k {
                    "4K Blockbuster Purist"
                } else if has_g(878) || has_g(14) {
                    "Epic Spectacle Fan"
                } else {
                    "High-Octane Action Fan"
                }
            }
            Some(27) => {
                if has_g(53) || has_g(9648) {
                    "Midnight Horror Purist"
                } else {
                    "Cult Horror Aficionado"
                }
            }
            Some(35) => {
                if has_g(18) || has_g(10749) {
                    "Smart Dramedy Connoisseur"
                } else {
                    "Sharp Comedy Devotee"
                }
            }
            Some(16) => {
                if world_lover {
                    "Anime & Animation Fan"
                } else {
                    "Animated Storyteller Fan"
                }
            }
            Some(18) => {
                if world_lover {
                    "World Cinema Auteur Fan"
                } else if classic_era {
                    "Classic Cinema Purist"
                } else if tv_binger {
                    "Prestige TV Binger"
                } else if acclaim {
                    "Acclaimed Drama Cinephile"
                } else {
                    "Character Drama Devotee"
                }
            }
            Some(14) => "Myth & Fantasy Explorer",
            Some(10749) => "Modern Romance Cinephile",
            Some(36) | Some(10752) => "Historical Epic Devotee",
            Some(10402) => "Music & Stage Cinephile",
            Some(37) => "Frontier Western Purist",
            _ => {
                if world_lover {
                    "World Cinema Explorer"
                } else if classic_era {
                    "Retro Cinema Purist"
                } else if high_4k {
                    "4K Home Cinema Purist"
                } else if tv_binger {
                    "Prestige TV Binger"
                } else {
                    "Eclectic Cinema Lover"
                }
            }
        }
    };

    // Build a concise 1-line summary grounded in the user's real stats.
    let mut parts: Vec<String> = Vec::new();
    match (g1_name, g2_name) {
        (Some(a), Some(b)) => parts.push(format!("Gravitates toward {a} and {b}")),
        (Some(a), None) => parts.push(format!("Gravitates toward {a}")),
        _ => parts.push("Builds a varied watchlist".to_string()),
    }
    if let Some(dec) = inp.top_decade {
        let era = if dec == "Classic" {
            "from the Classic era".to_string()
        } else {
            format!("from the {dec}")
        };
        if let Some(first) = parts.first_mut() {
            first.push(' ');
            first.push_str(&era);
        }
    }
    if inp.avg_rating >= 6.8 {
        parts.push(format!("averaging {:.1}/10", inp.avg_rating));
    }
    if inp.uhd_share_pct >= 35 {
        parts.push(format!("with {}% in 4K", inp.uhd_share_pct));
    }
    let mut summary = parts.join(", ");
    if let Some(dir) = inp.top_director {
        summary.push_str(&format!(" — led by {dir}."));
    } else if let Some(plat) = inp.top_platform {
        summary.push_str(&format!(" — most often on {plat}."));
    } else {
        summary.push('.');
    }

    (tag.to_string(), summary)
}

// ---------------------------------------------------------------------------
// 3. Personal Cinema DNA (`profile`)
// ---------------------------------------------------------------------------

struct TitleRecord {
    mt: &'static str, // "movie" | "tv"
    tmdb: Option<String>,
    title: String,
    item: Option<Item>,
    meta: Option<Meta>,
    completed: bool,
    started: bool,
    liked: bool,
    disliked: bool,
    has_real_progress: bool,
    real_progress_secs: f64,
    completed_eps: usize,
    max_progress_secs: f64,
    last_t: i64,
}

impl TitleRecord {
    fn taste_weight(&self) -> f64 {
        if self.disliked {
            return 0.0;
        }
        let mut w = 0.0;
        if self.liked {
            w += 3.0;
        }
        if self.mt == "movie" {
            if self.completed {
                w += 2.0;
            } else if self.started {
                if self.max_progress_secs >= 300.0 {
                    w += 1.0;
                } else {
                    w += 0.4;
                }
            }
        } else if self.completed || self.started {
            w += 1.5 + (self.completed_eps.min(10) as f64) * 0.25;
        }
        w
    }

    fn engaged(&self) -> bool {
        !self.disliked && (self.completed || self.started || self.liked)
    }

    fn year(&self) -> i32 {
        let my = self.meta.as_ref().map(|m| m.year()).unwrap_or(0);
        if my > 0 {
            my
        } else {
            self.item.as_ref().and_then(|it| it.year).unwrap_or(0)
        }
    }

    fn rating(&self) -> f64 {
        if let Some(m) = &self.meta {
            if m.va > 0.0 {
                return m.va;
            }
        }
        self.item.as_ref().map(|it| it.rating).unwrap_or(0.0)
    }

    fn runtime_mins(&self) -> i64 {
        self.meta.as_ref().map(|m| m.rt).unwrap_or(0)
    }
}

struct CreditTarget {
    mt: &'static str,
    tmdb: String,
    title: String,
    weight: f64,
    liked: bool,
    last_t: i64,
}

struct PersonAgg {
    id: i64,
    name: String,
    photo: String,
    count: usize,
    weight: f64,
    titles: Vec<String>,
}

/// Resolve canonical key + optional catalog item + meta for a title, merging title-only
/// duplicates (e.g. a history row with `tmdb: null`) into an existing TMDB entry when possible.
fn resolve_record_key(
    mt: &'static str,
    tmdb: Option<&str>,
    raw_id: i64,
    title: &str,
    title_index: &HashMap<(&'static str, String), String>,
) -> String {
    if let Some(t) = tmdb.filter(|x| !x.is_empty() && *x != "0") {
        return format!("{mt}:{t}");
    }
    let nt = norm(title);
    if !nt.is_empty() {
        if let Some(existing) = title_index.get(&(mt, nt.clone())) {
            return existing.clone();
        }
    }
    if raw_id > 0 {
        let prefix = if mt == "tv" { "s" } else { "m" };
        return format!("{prefix}:{raw_id}");
    }
    if !nt.is_empty() {
        format!("title:{mt}:{nt}")
    } else {
        format!("anon:{mt}:{raw_id}")
    }
}

struct DnaSnapshot {
    records: HashMap<String, TitleRecord>,
    episodes_watched: usize,
    total_secs: f64,
    liked_count: usize,
    disliked_count: usize,
    list_count: usize,
    watched_keys: HashSet<String>,
    disliked_keys: HashSet<String>,
}

fn collect_dna_snapshot(app: &App) -> DnaSnapshot {
    let cat = app.catalog();
    let db = app.meta.read().unwrap();

    let (progress_entries, watched_entries, history_entries, rating_entries, list_count) =
        app.state.read(|st| {
            let prog: Vec<(String, Value)> = st["progress"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let wat: Vec<(String, Value)> = st["watched"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let hist: Vec<Value> = st["history"].as_array().cloned().unwrap_or_default();
            let rats: Vec<(String, Value)> = st["ratings"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let lcnt = st["list"].as_object().map_or(0, |m| m.len());
            (prog, wat, hist, rats, lcnt)
        });

    let mut records: HashMap<String, TitleRecord> = HashMap::new();
    let mut title_index: HashMap<(&'static str, String), String> = HashMap::new();
    let mut completed_ep_ids: HashSet<i64> = HashSet::new();
    let mut watched_keys: HashSet<String> = HashSet::new();
    let mut disliked_keys: HashSet<String> = HashSet::new();
    let mut total_secs = 0.0f64;
    let mut liked_count = 0usize;
    let mut disliked_count = 0usize;

    let lookup_meta = |mt: &str, tmdb: Option<&str>| -> Option<Meta> {
        tmdb.filter(|t| !t.is_empty() && *t != "0")
            .and_then(|t| db.get(&format!("{mt}:{t}")))
            .filter(|m| !m.missing)
            .cloned()
    };

    let upsert = |records: &mut HashMap<String, TitleRecord>,
                  title_index: &mut HashMap<(&'static str, String), String>,
                  mt: &'static str,
                  tmdb: Option<String>,
                  raw_id: i64,
                  title_hint: &str,
                  item_opt: Option<&Item>|
     -> String {
        let canon_tmdb = tmdb
            .filter(|t| !t.is_empty() && t != "0")
            .or_else(|| item_opt.and_then(|it| it.tmdb.clone()));
        let resolved_title = if !title_hint.trim().is_empty() {
            title_hint.trim().to_string()
        } else {
            item_opt.map(|it| it.title.clone()).unwrap_or_default()
        };
        let key = resolve_record_key(mt, canon_tmdb.as_deref(), raw_id, &resolved_title, title_index);
        let nt = norm(&resolved_title);
        if !nt.is_empty() && canon_tmdb.is_some() {
            title_index.entry((mt, nt)).or_insert_with(|| key.clone());
        }
        let rec = records.entry(key.clone()).or_insert_with(|| {
            let item = item_opt.cloned().or_else(|| {
                canon_tmdb
                    .as_deref()
                    .and_then(|t| cat.by_tmdb(mt, t))
                    .cloned()
            });
            let meta = lookup_meta(mt, canon_tmdb.as_deref());
            TitleRecord {
                mt,
                tmdb: canon_tmdb.clone(),
                title: if !resolved_title.is_empty() {
                    resolved_title.clone()
                } else {
                    item.as_ref().map(|i| i.title.clone()).unwrap_or_default()
                },
                item,
                meta,
                completed: false,
                started: false,
                liked: false,
                disliked: false,
                has_real_progress: false,
                real_progress_secs: 0.0,
                completed_eps: 0,
                max_progress_secs: 0.0,
                last_t: 0,
            }
        });
        if rec.tmdb.is_none() && canon_tmdb.is_some() {
            rec.tmdb = canon_tmdb.clone();
        }
        if rec.item.is_none() {
            rec.item = item_opt.cloned().or_else(|| {
                rec.tmdb
                    .as_deref()
                    .and_then(|t| cat.by_tmdb(mt, t))
                    .cloned()
            });
        }
        if rec.meta.is_none() {
            rec.meta = lookup_meta(mt, rec.tmdb.as_deref());
        }
        if rec.title.is_empty() && !resolved_title.is_empty() {
            rec.title = resolved_title;
        }
        key
    };

    // 1) Ratings (seed TMDB keys first so title-only history merges into them)
    for (rk, rv) in &rating_entries {
        let Some((mt_raw, tid)) = rk.split_once(':') else {
            continue;
        };
        let mt = if mt_raw == "tv" { "tv" } else { "movie" };
        let v = rv["v"].as_i64().unwrap_or(0);
        let t = rv["t"].as_i64().unwrap_or(0);
        let title = s(&rv["title"]);
        let it = cat.by_tmdb(mt, tid);
        let key = upsert(&mut records, &mut title_index, mt, Some(tid.to_string()), 0, &title, it);
        if let Some(rec) = records.get_mut(&key) {
            rec.last_t = rec.last_t.max(t);
            if v > 0 {
                rec.liked = true;
                liked_count += 1;
            } else if v < 0 {
                rec.disliked = true;
                disliked_count += 1;
                disliked_keys.insert(format!("{mt}:{tid}"));
            }
        }
    }

    // 2) Explicit `watched` entries
    let mut watched_tv_keys: Vec<String> = Vec::new();
    for (wk, wv) in &watched_entries {
        let t = wv["t"].as_i64().unwrap_or(0);
        let title = s(&wv["title"]);
        if let Some(eid_str) = wk.strip_prefix("e:") {
            if let Ok(eid) = eid_str.parse::<i64>() {
                completed_ep_ids.insert(eid);
            }
            continue;
        }
        let Some((mt_raw, tid)) = wk.split_once(':') else {
            continue;
        };
        if mt_raw != "movie" && mt_raw != "tv" {
            continue;
        }
        let mt = if mt_raw == "tv" { "tv" } else { "movie" };
        watched_keys.insert(format!("{mt}:{tid}"));
        let it = cat.by_tmdb(mt, tid);
        let key = upsert(&mut records, &mut title_index, mt, Some(tid.to_string()), 0, &title, it);
        if let Some(rec) = records.get_mut(&key) {
            rec.completed = true;
            rec.started = true;
            rec.last_t = rec.last_t.max(t);
        }
        if mt == "tv" {
            watched_tv_keys.push(key);
        }
    }

    // 3) Progress entries (`m:<id>` and `e:<id>`)
    let mut ep_progress_ids: HashSet<i64> = HashSet::new();
    let mut series_with_ep_progress: HashSet<String> = HashSet::new();
    for (pk, pv) in &progress_entries {
        let dur = pv["dur"].as_f64().unwrap_or(0.0).max(0.0);
        let pos = if dur > 0.0 {
            pv["pos"].as_f64().unwrap_or(0.0).clamp(0.0, dur)
        } else {
            0.0
        };
        let pct = if dur > 0.0 { pos / dur } else { 0.0 };
        let t = pv["t"].as_i64().unwrap_or(0);
        let item_v = &pv["item"];
        let title_hint = s(&item_v["title"]);

        if let Some(eid_str) = pk.strip_prefix("e:") {
            let eid = eid_str
                .parse::<i64>()
                .ok()
                .or_else(|| item_v["id"].as_i64())
                .unwrap_or(0);
            if eid > 0 {
                ep_progress_ids.insert(eid);
            }
            let raw_sid = item_v["sid"]
                .as_i64()
                .or_else(|| item_v["sid"].as_str().and_then(|x| x.parse().ok()))
                .or_else(|| {
                    if item_v["k"] == "s" {
                        item_v["id"].as_i64()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let sit = if raw_sid > 0 {
                cat.series_item(raw_sid)
            } else {
                None
            };
            let tmdb_hint = {
                let x = s(&item_v["tmdb"]);
                if x.is_empty() || x == "0" {
                    None
                } else {
                    Some(x)
                }
            };
            let skey = upsert(
                &mut records,
                &mut title_index,
                "tv",
                tmdb_hint,
                sit.map(|i| i.id).unwrap_or(raw_sid),
                &title_hint,
                sit,
            );
            series_with_ep_progress.insert(skey.clone());

            let finished = dur > 0.0 && pct >= COMPLETE_PCT;
            let ep_rt_mins = records
                .get(&skey)
                .map(|r| r.runtime_mins())
                .filter(|&rt| rt > 0)
                .map(|rt| rt as f64)
                .unwrap_or(DEFAULT_EPISODE_MINS);

            if dur > 60.0 {
                total_secs += pos.min(18_000.0);
            } else if finished {
                // Synthetic mark-watched marker (e.g. pos=1, dur=1 from episodes-watched)
                total_secs += ep_rt_mins * 60.0;
            }

            if finished && eid > 0 {
                completed_ep_ids.insert(eid);
            }
            if let Some(rec) = records.get_mut(&skey) {
                rec.started = true;
                rec.last_t = rec.last_t.max(t);
                if dur > 60.0 {
                    rec.has_real_progress = true;
                    rec.max_progress_secs = rec.max_progress_secs.max(pos);
                }
                if finished {
                    rec.completed_eps += 1;
                    rec.completed = true;
                }
            }
        } else if let Some(mid_str) = pk.strip_prefix("m:") {
            let raw_mid = mid_str
                .parse::<i64>()
                .ok()
                .or_else(|| item_v["id"].as_i64())
                .unwrap_or(0);
            let mit = if raw_mid > 0 { cat.movie(raw_mid) } else { None };
            let tmdb_hint = {
                let x = s(&item_v["tmdb"]);
                if x.is_empty() || x == "0" {
                    None
                } else {
                    Some(x)
                }
            };
            let mkey = upsert(
                &mut records,
                &mut title_index,
                "movie",
                tmdb_hint,
                mit.map(|i| i.id).unwrap_or(raw_mid),
                &title_hint,
                mit,
            );

            let finished = dur > 0.0 && pct >= COMPLETE_PCT;

            if let Some(rec) = records.get_mut(&mkey) {
                rec.started = true;
                rec.last_t = rec.last_t.max(t);
                if dur > 60.0 {
                    rec.has_real_progress = true;
                    rec.real_progress_secs = rec.real_progress_secs.max(pos.min(36_000.0));
                    rec.max_progress_secs = rec.max_progress_secs.max(pos);
                }
                if finished {
                    rec.completed = true;
                    if let Some(tid) = &rec.tmdb {
                        watched_keys.insert(format!("movie:{tid}"));
                    }
                }
            }
        }
    }

    // 4) History entries (run after progress/watched/ratings so title-only rows merge cleanly!)
    // History records when a title was opened/played, NOT completion — never force 5-second clicks
    // to `completed = true` without explicit `watched` or >=88% playback progress.
    let mut hist_ordered = history_entries;
    hist_ordered.sort_by_key(|h| s(&h["tmdb"]).is_empty());
    for h in &hist_ordered {
        let kind = s(&h["kind"]);
        let mt = if kind == "s" || kind == "tv" { "tv" } else { "movie" };
        let tid = {
            let x = s(&h["tmdb"]);
            if x.is_empty() || x == "0" {
                None
            } else {
                Some(x)
            }
        };
        let title = s(&h["title"]);
        if tid.is_none() && title.trim().is_empty() {
            continue;
        }
        let t = h["t"].as_i64().unwrap_or(0);
        let it = tid.as_deref().and_then(|x| cat.by_tmdb(mt, x));
        let key = upsert(&mut records, &mut title_index, mt, tid.clone(), 0, &title, it);
        if let Some(rec) = records.get_mut(&key) {
            rec.started = true;
            rec.last_t = rec.last_t.max(t);
            if rec.completed {
                if let Some(x) = &rec.tmdb {
                    watched_keys.insert(format!("{mt}:{x}"));
                }
            }
        }
    }

    // 5) Sum movie runtime ONCE per deduplicated movie record (real progress or estimated runtime
    //    for watched / `continue_done` movies with dur <= 60 — eliminates double-counting!).
    for rec in records.values() {
        if rec.mt == "movie" {
            if rec.has_real_progress {
                total_secs += rec.real_progress_secs;
            } else if rec.completed {
                let mins = if rec.runtime_mins() > 0 {
                    rec.runtime_mins() as f64
                } else {
                    DEFAULT_MOVIE_MINS
                };
                total_secs += mins * 60.0;
            }
        }
    }
    for tv_key in watched_tv_keys {
        if !series_with_ep_progress.contains(&tv_key) {
            let mins = records
                .get(&tv_key)
                .map(|r| r.runtime_mins())
                .filter(|&rt| rt > 0)
                .map(|rt| rt as f64)
                .unwrap_or(DEFAULT_EPISODE_MINS);
            total_secs += mins * 60.0;
        }
    }
    for &eid in &completed_ep_ids {
        if !ep_progress_ids.contains(&eid) {
            total_secs += DEFAULT_EPISODE_MINS * 60.0;
        }
    }

    drop(db);

    DnaSnapshot {
        records,
        episodes_watched: completed_ep_ids.len(),
        total_secs,
        liked_count,
        disliked_count,
        list_count,
        watched_keys,
        disliked_keys,
    }
}

/// Personal Cinema DNA: watch stats, genres, decades, platforms, countries, directors/actors,
/// deterministic persona, and personalized CAM-free unwatched recommendation rows.
pub async fn profile(app: Arc<App>) -> Value {
    let snap = collect_dna_snapshot(&app);
    let cat = app.catalog();

    let mut movies_watched = 0usize;
    let mut shows_started = 0usize;
    let mut uhd_yes = 0usize;
    let mut uhd_total = 0usize;
    let mut rating_sum = 0.0f64;
    let mut rating_cnt = 0usize;
    let mut movie_runtimes: Vec<i64> = Vec::new();
    let mut non_en_cnt = 0usize;
    let mut lang_known_cnt = 0usize;
    let mut doc_cnt = 0usize;
    let mut engaged_cnt = 0usize;

    // Genre aggregation: canonical_id -> (weighted_score, count, liked_count)
    let mut genre_agg: HashMap<i64, (f64, usize, usize)> = HashMap::new();
    let mut decade_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut platform_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut country_counts: HashMap<String, usize> = HashMap::new();

    let mut credit_targets: Vec<CreditTarget> = Vec::new();
    let mut love_seeds: Vec<CreditTarget> = Vec::new();

    for rec in snap.records.values() {
        if rec.mt == "movie" && rec.completed {
            movies_watched += 1;
        }
        if rec.mt == "tv" && (rec.started || rec.completed) {
            shows_started += 1;
        }
        if rec.started || rec.completed {
            if let Some(it) = &rec.item {
                uhd_total += 1;
                if item_has_uhd(it) {
                    uhd_yes += 1;
                }
            }
        }
        if !rec.engaged() {
            continue;
        }
        engaged_cnt += 1;

        let w = rec.taste_weight().max(0.5);
        let r = rec.rating();
        if r > 0.0 {
            rating_sum += r;
            rating_cnt += 1;
        }
        if rec.mt == "movie" {
            let rt = rec.runtime_mins();
            if rt > 0 {
                movie_runtimes.push(rt);
            }
        }
        if let Some(dec) = decade_bucket(rec.year()) {
            *decade_counts.entry(dec).or_default() += 1;
        }
        if let Some(m) = &rec.meta {
            if !m.l.is_empty() {
                lang_known_cnt += 1;
                if m.l != "en" {
                    non_en_cnt += 1;
                }
            }
            if m.g.contains(&99) {
                doc_cnt += 1;
            }
            let mut seen_g = HashSet::new();
            for &g in &m.g {
                let cg = canonical_genre_id(g);
                if seen_g.insert(cg) {
                    let e = genre_agg.entry(cg).or_insert((0.0, 0, 0));
                    e.0 += w;
                    e.1 += 1;
                    if rec.liked {
                        e.2 += 1;
                    }
                }
            }
        }
        if let Some(it) = &rec.item {
            for p in cat.item_platforms(it) {
                *platform_counts.entry(p).or_default() += 1;
            }
            for co in item_countries(it, rec.meta.as_ref()) {
                *country_counts.entry(co).or_default() += 1;
            }
        } else if let Some(m) = &rec.meta {
            for c in &m.oc {
                let code = norm_country(c);
                if !code.is_empty() {
                    *country_counts.entry(code).or_default() += 1;
                }
            }
        }

        if let Some(tid) = &rec.tmdb {
            let target = CreditTarget {
                mt: rec.mt,
                tmdb: tid.clone(),
                title: rec.title.clone(),
                weight: w,
                liked: rec.liked,
                last_t: rec.last_t,
            };
            if rec.liked || rec.completed {
                love_seeds.push(CreditTarget {
                    mt: target.mt,
                    tmdb: target.tmdb.clone(),
                    title: target.title.clone(),
                    weight: target.weight + if rec.rating() >= 7.5 { 0.5 } else { 0.0 },
                    liked: target.liked,
                    last_t: target.last_t,
                });
            }
            credit_targets.push(target);
        }
    }

    let total_hours = ((snap.total_secs / 3600.0) * 10.0).round() / 10.0;
    let uhd_share_pct = if uhd_total > 0 {
        ((uhd_yes as f64 / uhd_total as f64) * 100.0).round() as u64
    } else {
        0
    };
    let avg_rating = if rating_cnt > 0 {
        ((rating_sum / rating_cnt as f64) * 10.0).round() / 10.0
    } else {
        0.0
    };
    let runtime_sweet_spot = compute_runtime_sweet_spot(&movie_runtimes);

    // Top 8 genres by weighted watch + like score
    let total_genre_weight: f64 = genre_agg.values().map(|x| x.0).sum();
    let mut genre_rows: Vec<(i64, &'static str, f64, usize, usize)> = genre_agg
        .into_iter()
        .filter_map(|(id, (w, cnt, liked))| {
            let name = canonical_genre_name(id)?;
            Some((id, name, w, cnt, liked))
        })
        .collect();
    genre_rows.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then(b.3.cmp(&a.3))
            .then(a.1.cmp(b.1))
    });
    let top_genres_json: Vec<Value> = genre_rows
        .iter()
        .take(8)
        .map(|&(id, name, w, count, liked)| {
            let pct = if total_genre_weight > 0.0 {
                ((w / total_genre_weight) * 100.0).round() as u64
            } else {
                0
            };
            json!({
                "id": id,
                "name": name,
                "count": count,
                "pct": pct,
                "liked": liked
            })
        })
        .collect();

    // Top decades
    let total_decades: usize = decade_counts.values().sum();
    let mut dec_rows: Vec<(&'static str, usize)> = decade_counts.into_iter().collect();
    dec_rows.sort_by(|a, b| b.1.cmp(&a.1).then(decade_rank(b.0).cmp(&decade_rank(a.0))));
    let top_decades_json: Vec<Value> = dec_rows
        .iter()
        .map(|&(decade, count)| {
            let pct = if total_decades > 0 {
                ((count as f64 / total_decades as f64) * 100.0).round() as u64
            } else {
                0
            };
            json!({ "decade": decade, "count": count, "pct": pct })
        })
        .collect();

    // Top platforms
    let mut plat_rows: Vec<(&'static str, usize)> = platform_counts.into_iter().collect();
    plat_rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let top_platforms_json: Vec<Value> = plat_rows
        .iter()
        .take(8)
        .map(|&(name, count)| json!({ "name": name, "count": count }))
        .collect();

    // Top countries
    let mut country_rows: Vec<(String, usize)> = country_counts.into_iter().collect();
    country_rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top_countries_json: Vec<Value> = country_rows
        .iter()
        .take(8)
        .map(|(code, count)| json!({ "code": code, "count": count }))
        .collect();

    // Fetch TMDB credits for up to MAX_CREDIT_FETCH top watched/liked titles, balancing Movies + TV
    // (reserving up to 4 slots for TV shows when present so watching 12 movies never starves TV
    // showrunners/actors; 30-day persistent cache, bounded concurrency = 5).
    let (mut movie_targets, mut tv_targets): (Vec<CreditTarget>, Vec<CreditTarget>) =
        credit_targets.into_iter().partition(|c| c.mt == "movie");
    let cmp_target = |a: &CreditTarget, b: &CreditTarget| {
        b.liked
            .cmp(&a.liked)
            .then(b.weight.total_cmp(&a.weight))
            .then(b.last_t.cmp(&a.last_t))
    };
    movie_targets.sort_by(cmp_target);
    tv_targets.sort_by(cmp_target);

    let tv_reserved = tv_targets.len().min(4);
    let movie_take = movie_targets.len().min(MAX_CREDIT_FETCH - tv_reserved);
    let tv_take = tv_targets.len().min(MAX_CREDIT_FETCH - movie_take);
    let mut credit_targets: Vec<CreditTarget> = Vec::with_capacity(MAX_CREDIT_FETCH);
    credit_targets.extend(movie_targets.into_iter().take(movie_take));
    credit_targets.extend(tv_targets.into_iter().take(tv_take));

    let mut dir_map: HashMap<i64, PersonAgg> = HashMap::new();
    let mut act_map: HashMap<i64, PersonAgg> = HashMap::new();

    if app.tmdb.enabled() && !credit_targets.is_empty() {
        let sem = Arc::new(tokio::sync::Semaphore::new(CREDIT_CONCURRENCY));
        let futs = credit_targets.iter().map(|ct| {
            let app = app.clone();
            let sem = sem.clone();
            let mt = ct.mt;
            let tid = ct.tmdb.clone();
            async move {
                let _permit = sem.acquire_owned().await.ok()?;
                let ep = format!("/{mt}/{tid}");
                app.tmdb
                    .get(&ep, [sp("append_to_response", "credits")], 30 * TTL_D)
                    .await
            }
        });
        let results = futures_util::future::join_all(futs).await;

        for (ct, resp) in credit_targets.iter().zip(results) {
            let Some(d) = resp else { continue };
            let display_title = if !ct.title.is_empty() {
                ct.title.clone()
            } else {
                d["title"]
                    .as_str()
                    .or_else(|| d["name"].as_str())
                    .unwrap_or("")
                    .to_string()
            };

            // Directors (movies: crew job == "Director"; TV: created_by or Director)
            let mut seen_dir = HashSet::new();
            if ct.mt == "tv" {
                for cr in d["created_by"].as_array().into_iter().flatten() {
                    let pid = cr["id"].as_i64().unwrap_or(0);
                    let name = s(&cr["name"]);
                    if pid <= 0 || name.is_empty() || !seen_dir.insert(pid) {
                        continue;
                    }
                    let photo = tmdb_img(cr["profile_path"].as_str().unwrap_or(""), "w185");
                    let e = dir_map.entry(pid).or_insert_with(|| PersonAgg {
                        id: pid,
                        name,
                        photo,
                        count: 0,
                        weight: 0.0,
                        titles: Vec::new(),
                    });
                    e.count += 1;
                    e.weight += ct.weight;
                    if !display_title.is_empty() && !e.titles.contains(&display_title) {
                        e.titles.push(display_title.clone());
                    }
                }
            }
            for cr in d["credits"]["crew"].as_array().into_iter().flatten() {
                if cr["job"].as_str() != Some("Director") {
                    continue;
                }
                let pid = cr["id"].as_i64().unwrap_or(0);
                let name = s(&cr["name"]);
                if pid <= 0 || name.is_empty() || !seen_dir.insert(pid) {
                    continue;
                }
                let photo = tmdb_img(cr["profile_path"].as_str().unwrap_or(""), "w185");
                let e = dir_map.entry(pid).or_insert_with(|| PersonAgg {
                    id: pid,
                    name,
                    photo,
                    count: 0,
                    weight: 0.0,
                    titles: Vec::new(),
                });
                e.count += 1;
                e.weight += ct.weight;
                if !display_title.is_empty() && !e.titles.contains(&display_title) {
                    e.titles.push(display_title.clone());
                }
            }

            // Top-billed actors (first 6)
            let mut seen_act = HashSet::new();
            for ca in d["credits"]["cast"].as_array().into_iter().flatten().take(6) {
                let pid = ca["id"].as_i64().unwrap_or(0);
                let name = s(&ca["name"]);
                if pid <= 0 || name.is_empty() || !seen_act.insert(pid) {
                    continue;
                }
                let photo = tmdb_img(ca["profile_path"].as_str().unwrap_or(""), "w185");
                let e = act_map.entry(pid).or_insert_with(|| PersonAgg {
                    id: pid,
                    name,
                    photo,
                    count: 0,
                    weight: 0.0,
                    titles: Vec::new(),
                });
                e.count += 1;
                e.weight += ct.weight;
                if !display_title.is_empty() && !e.titles.contains(&display_title) {
                    e.titles.push(display_title.clone());
                }
            }
        }
    }

    let mut dirs: Vec<PersonAgg> = dir_map.into_values().collect();
    dirs.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.weight.total_cmp(&a.weight))
            .then(a.name.cmp(&b.name))
    });
    let top_directors_json: Vec<Value> = dirs
        .iter()
        .take(8)
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "photo": p.photo,
                "count": p.count,
                "titles": p.titles
            })
        })
        .collect();

    let mut acts: Vec<PersonAgg> = act_map.into_values().collect();
    acts.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.weight.total_cmp(&a.weight))
            .then(a.name.cmp(&b.name))
    });
    let top_actors_json: Vec<Value> = acts
        .iter()
        .take(10)
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "photo": p.photo,
                "count": p.count,
                "titles": p.titles
            })
        })
        .collect();

    // Deterministic persona tag + 1-line summary
    let non_en_share_pct = if lang_known_cnt > 0 {
        ((non_en_cnt as f64 / lang_known_cnt as f64) * 100.0).round() as u64
    } else {
        0
    };
    let doc_share_pct = if engaged_cnt > 0 {
        ((doc_cnt as f64 / engaged_cnt as f64) * 100.0).round() as u64
    } else {
        0
    };
    let persona_genres: Vec<(i64, &'static str, usize)> = genre_rows
        .iter()
        .take(5)
        .map(|&(id, name, _, cnt, _)| (id, name, cnt))
        .collect();
    let (persona_tag, persona_summary) = compute_persona(&PersonaInput {
        top_genres: &persona_genres,
        top_decade: dec_rows.first().map(|x| x.0),
        uhd_share_pct,
        avg_rating,
        movies_watched,
        episodes_watched: snap.episodes_watched,
        non_en_share_pct,
        doc_share_pct,
        top_director: dirs.first().map(|d| d.name.as_str()),
        top_platform: plat_rows.first().map(|p| p.0),
    });

    // Build up to 3 curated CAM-free & unwatched recommendation rows (`becauseYouLoved`)
    let mut exclude = watched_set(&app);
    exclude.extend(snap.watched_keys.iter().cloned());
    exclude.extend(snap.disliked_keys.iter().cloned());
    for rec in snap.records.values() {
        if let Some(tid) = &rec.tmdb {
            if rec.started || rec.completed || rec.disliked {
                exclude.insert(format!("{}:{tid}", rec.mt));
            }
        }
    }

    love_seeds.sort_by(|a, b| {
        b.liked
            .cmp(&a.liked)
            .then(b.weight.total_cmp(&a.weight))
            .then(b.last_t.cmp(&a.last_t))
    });

    let top_genre_pair: Vec<(i64, &'static str)> = genre_rows
        .iter()
        .take(3)
        .map(|&(id, name, _, _, _)| (id, name))
        .collect();

    let because_you_loved = build_because_you_loved(
        &app,
        &dirs,
        &love_seeds,
        &top_genre_pair,
        &exclude,
    )
    .await;

    json!({
        "moviesWatched": movies_watched,
        "episodesWatched": snap.episodes_watched,
        "showsStarted": shows_started,
        "totalHours": total_hours,
        "likedCount": snap.liked_count,
        "dislikedCount": snap.disliked_count,
        "listCount": snap.list_count,
        "uhdSharePct": uhd_share_pct,
        "avgRating": avg_rating,
        "runtimeSweetSpot": runtime_sweet_spot,
        "topGenres": top_genres_json,
        "topDecades": top_decades_json,
        "topPlatforms": top_platforms_json,
        "topCountries": top_countries_json,
        "topDirectors": top_directors_json,
        "topActors": top_actors_json,
        "persona": {
            "tag": persona_tag,
            "summary": persona_summary
        },
        "becauseYouLoved": because_you_loved
    })
}

async fn build_because_you_loved(
    app: &Arc<App>,
    dirs: &[PersonAgg],
    love_seeds: &[CreditTarget],
    top_genres: &[(i64, &'static str)],
    exclude: &HashSet<String>,
) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut used_cards: HashSet<String> = HashSet::new();

    // Helper: pick clean, unwatched, CAM-free cards from a TMDB results array
    let match_unwatched = |results: &[Value], default_mt: Option<&str>, used: &mut HashSet<String>| -> Vec<Value> {
        let cat = app.catalog();
        let mut items = Vec::new();
        for r in results {
            let mt = r["media_type"].as_str().or(default_mt).unwrap_or("");
            if mt != "movie" && mt != "tv" {
                continue;
            }
            let tid = r["id"]
                .as_i64()
                .map(|x| x.to_string())
                .unwrap_or_else(|| s(&r["id"]));
            if tid.is_empty() {
                continue;
            }
            let tkey = format!("{mt}:{tid}");
            if exclude.contains(&tkey) || used.contains(&tkey) {
                continue;
            }
            let Some(it) = cat.by_tmdb(mt, &tid) else {
                continue;
            };
            if !it.en || it.sport || crate::showcase::is_cam_only(app, it) || hidden_item(app, it) {
                continue;
            }
            used.insert(tkey);
            items.push(app.card_meta(it));
            if items.len() >= 24 {
                break;
            }
        }
        items
    };

    // 1. Top director filmography row (try top 2 directors until one yields >= 3 unwatched titles)
    if app.tmdb.enabled() {
        for dir in dirs.iter().take(2) {
            if rows.len() >= 1 {
                break;
            }
            let Some(p) = app
                .tmdb
                .get(
                    format!("/person/{}", dir.id),
                    [sp("append_to_response", "combined_credits")],
                    7 * TTL_D,
                )
                .await
            else {
                continue;
            };
            let mut crew: Vec<Value> = p["combined_credits"]["crew"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| {
                    let j = c["job"].as_str().unwrap_or("");
                    j == "Director" || (j == "Creator" && c["media_type"] == "tv")
                })
                .cloned()
                .collect();
            crew.sort_by(|a, b| {
                let sa = b["vote_average"].as_f64().unwrap_or(0.0)
                    + (b["popularity"].as_f64().unwrap_or(0.0) + 1.0).log10();
                let sb = a["vote_average"].as_f64().unwrap_or(0.0)
                    + (a["popularity"].as_f64().unwrap_or(0.0) + 1.0).log10();
                sa.total_cmp(&sb)
            });
            let mut trial_used = used_cards.clone();
            let items = match_unwatched(&crew, None, &mut trial_used);
            if items.len() >= 3 {
                used_cards = trial_used;
                rows.push(json!({
                    "id": format!("dna-dir-{}", dir.id),
                    "title": format!("More Directed by {}", dir.name),
                    "sub": "Unwatched films from your top filmmaker",
                    "items": items
                }));
            }
        }
    }

    // 2. Top liked / watched seed recommendation rows (up to 2, mixing movies & TV when both exist)
    let mut picked_seeds: Vec<&CreditTarget> = Vec::new();
    let mut picked_ids: HashSet<String> = HashSet::new();
    if let Some(first) = love_seeds.first() {
        picked_ids.insert(format!("{}:{}", first.mt, first.tmdb));
        picked_seeds.push(first);
        // Prefer a complementary media type for slot #2 if one is available among top seeds
        if let Some(alt) = love_seeds.iter().find(|s| s.mt != first.mt) {
            picked_ids.insert(format!("{}:{}", alt.mt, alt.tmdb));
            picked_seeds.push(alt);
        }
    }
    for s in love_seeds {
        if picked_seeds.len() >= 4 {
            break;
        }
        let k = format!("{}:{}", s.mt, s.tmdb);
        if picked_ids.insert(k) {
            picked_seeds.push(s);
        }
    }

    for seed in picked_seeds {
        if rows.len() >= 3 {
            break;
        }
        let mut items = Vec::new();
        let mut trial_used = used_cards.clone();

        if app.tmdb.enabled() {
            if let Some(r) = app
                .tmdb
                .get(
                    format!("/{}/{}/recommendations", seed.mt, seed.tmdb),
                    crate::tmdb::NO,
                    7 * TTL_D,
                )
                .await
            {
                let res = r["results"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
                items = match_unwatched(res, Some(seed.mt), &mut trial_used);
            }
        }

        // Local meta similarity fallback when TMDB is unavailable or sparse
        if items.len() < 3 {
            trial_used = used_cards.clone();
            items = local_similar_row(app, seed.mt, &seed.tmdb, exclude, &mut trial_used, 24);
        }

        if items.len() >= 3 {
            used_cards = trial_used;
            let verb = if seed.liked { "Loved" } else { "Watched" };
            rows.push(json!({
                "id": format!("dna-seed-{}-{}", seed.mt, seed.tmdb),
                "title": format!("Because You {verb} {}", seed.title),
                "items": items
            }));
        }
    }

    // 3. Local acclaimed genre-affinity fallback rows to reach up to 3 rows
    if rows.len() < 3 && !top_genres.is_empty() {
        let g1 = top_genres[0];
        let g2 = top_genres.get(1).copied();
        let mut trial_used = used_cards.clone();
        let items = local_genre_picks(app, g1.0, g2.map(|x| x.0), exclude, &mut trial_used, 24);
        if items.len() >= 3 {
            used_cards = trial_used;
            let title = match g2 {
                Some((_, n2)) => format!("Acclaimed {} & {} Picks", g1.1, n2),
                None => format!("Acclaimed {} Picks", g1.1),
            };
            rows.push(json!({
                "id": format!("dna-genre-{}", g1.0),
                "title": title,
                "sub": "Tailored to your taste profile · unwatched & CAM-free",
                "items": items
            }));
        }
    }

    // If still < 3 rows and we have a second distinct top genre, add one more clean genre shelf
    if rows.len() < 3 && top_genres.len() >= 2 {
        let g2 = top_genres[1];
        let mut trial_used = used_cards.clone();
        let items = local_genre_picks(app, g2.0, None, exclude, &mut trial_used, 24);
        if items.len() >= 3 {
            rows.push(json!({
                "id": format!("dna-genre-single-{}", g2.0),
                "title": format!("Essential {} to Discover", g2.1),
                "sub": "Unwatched highlights from your catalog",
                "items": items
            }));
        }
    }

    rows.truncate(3);
    rows
}

fn local_similar_row(
    app: &App,
    mt: &str,
    tmdb: &str,
    exclude: &HashSet<String>,
    used: &mut HashSet<String>,
    limit: usize,
) -> Vec<Value> {
    let seed_meta = match app.meta_get(mt, tmdb) {
        Some(m) if !m.missing && !m.g.is_empty() => m,
        _ => return Vec::new(),
    };
    let seed_genres: HashSet<i64> = seed_meta.g.iter().map(|&g| canonical_genre_id(g)).collect();
    let seed_kws: HashSet<i64> = seed_meta.kw.iter().copied().collect();

    let cat = app.catalog();
    let db = app.meta.read().unwrap();
    let pool = if mt == "tv" { &cat.series } else { &cat.movies };
    let mut scored: Vec<(Item, String, f64)> = Vec::new();

    for it in pool {
        if !it.en || it.sport || crate::showcase::is_cam_only(app, it) {
            continue;
        }
        let Some(tid) = &it.tmdb else { continue };
        let tkey = format!("{mt}:{tid}");
        if exclude.contains(&tkey) || used.contains(&tkey) {
            continue;
        }
        let Some(m) = db.get(&tkey).filter(|m| !m.missing) else {
            continue;
        };
        if m.va < 6.8 || m.vc < 80 {
            continue;
        }
        let g_hits = m
            .g
            .iter()
            .map(|&g| canonical_genre_id(g))
            .filter(|g| seed_genres.contains(g))
            .count();
        if g_hits == 0 {
            continue;
        }
        let kw_hits = m.kw.iter().filter(|k| seed_kws.contains(k)).count();
        let sc = (g_hits as f64) * 1.6
            + (kw_hits.min(4) as f64) * 0.9
            + m.bayes() * 0.8
            + (m.p.max(0.0) + 1.0).log10() * 0.35
            + if item_has_uhd(it) { 0.25 } else { 0.0 };
        scored.push((it.clone(), tkey, sc));
    }
    drop(db);

    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut out = Vec::new();
    for (it, tkey, _) in scored {
        if hidden_item(app, &it) {
            continue;
        }
        used.insert(tkey);
        out.push(app.card_meta(&it));
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn local_genre_picks(
    app: &App,
    g1: i64,
    g2: Option<i64>,
    exclude: &HashSet<String>,
    used: &mut HashSet<String>,
    limit: usize,
) -> Vec<Value> {
    let cat = app.catalog();
    let db = app.meta.read().unwrap();
    let mut scored: Vec<(Item, String, f64)> = Vec::new();

    for (mt, pool) in [("movie", &cat.movies), ("tv", &cat.series)] {
        for it in pool {
            if !it.en || it.sport || crate::showcase::is_cam_only(app, it) {
                continue;
            }
            let Some(tid) = &it.tmdb else { continue };
            let tkey = format!("{mt}:{tid}");
            if exclude.contains(&tkey) || used.contains(&tkey) {
                continue;
            }
            let Some(m) = db.get(&tkey).filter(|m| !m.missing) else {
                continue;
            };
            if m.va < 7.0 || m.vc < 150 {
                continue;
            }
            let has1 = m.g.iter().any(|&g| canonical_genre_id(g) == g1);
            let has2 = g2.map_or(false, |x| m.g.iter().any(|&g| canonical_genre_id(g) == x));
            if !has1 && !has2 {
                continue;
            }
            let sc = m.bayes() * 1.15
                + (m.p.max(0.0) + 1.0).log10() * 0.5
                + if has1 && has2 { 1.4 } else if has1 { 0.4 } else { 0.0 }
                + if item_has_uhd(it) { 0.3 } else { 0.0 };
            scored.push((it.clone(), tkey, sc));
        }
    }
    drop(db);

    scored.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut out = Vec::new();
    for (it, tkey, _) in scored {
        if g1 != 99 && hidden_item(app, &it) {
            continue;
        }
        used.insert(tkey);
        out.push(app.card_meta(&it));
        if out.len() >= limit {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 4. Specialty & Prestige Hubs Classifiers (Zero Network Calls)
// ---------------------------------------------------------------------------

fn is_standup_match(
    it: &Item,
    cat_uppers: &[String],
    meta: Option<&Meta>,
    su_kws: &HashSet<i64>,
) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") {
        return false;
    }
    if cat_uppers
        .iter()
        .any(|c| c.contains("STAND-UP") || c.contains("STAND UP") || c.contains("STANDUP"))
    {
        return true;
    }
    // For series, require an explicit Stand-Up category or explicit stand-up title phrase
    // so scripted sitcoms about comedians (e.g. Seinfeld, Louie) do not falsely match via TMDB keyword 9716.
    if it.k == "m" {
        if let Some(m) = meta {
            if !m.g.contains(&18) && m.kw.iter().any(|k| su_kws.contains(k)) {
                return true;
            }
        }
    }
    let is_comedy = meta.map_or(false, |m| m.g.contains(&35))
        || it.genre.to_lowercase().contains("comedy");
    if is_comedy {
        let re = crate::re!(
            r"(?i)\b(?:stand[- ]?up\s+(?:comedy|special|set|tour)|comedy\s+special|live\s+at\s+the\s+apollo)\b"
        );
        if re.is_match(&it.title)
            || (it.k == "m" && (meta.map_or(false, |m| re.is_match(&m.ov)) || re.is_match(&it.plot)))
        {
            return true;
        }
    }
    false
}

fn is_concert_match(it: &Item, cat_uppers: &[String], meta: Option<&Meta>) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") {
        return false;
    }
    if cat_uppers.iter().any(|c| {
        c.contains("CONCERT") || c.contains("NETFLIX MUSIC") || c.contains("MUSICAL")
    }) {
        return true;
    }
    if let Some(m) = meta {
        if m.g.contains(&10402) {
            if m.g.contains(&99) {
                return true;
            }
            let title_re = crate::re!(
                r"(?i)(?:\bconcert\b|\blive\s+(?:at|in|from)\b|\bunplugged\b|\bworld\s+tour\b|\bsymphony\b)"
            );
            let ov_re = crate::re!(
                r"(?i)\b(?:concert(?:\s+film|\s+tour)?|live\s+concert|live\s+performance|world\s+tour|mtv\s+unplugged|recorded\s+live\s+(?:at|in))\b"
            );
            if title_re.is_match(&it.title) || ov_re.is_match(&m.ov) {
                return true;
            }
        }
    }
    false
}

fn is_world_cinema_match(it: &Item, cat_uppers: &[String], meta: Option<&Meta>) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") {
        return false;
    }
    let in_anime_cat = cat_uppers
        .iter()
        .any(|c| c.contains("CRUNCHYROLL") || c.contains("ANIME") || c.contains("ANIMI"));
    let in_world_cat = cat_uppers.iter().any(|c| {
        c.contains("NETFLIX ASIA")
            || c.contains("ITALIAN SUB ENG")
            || c.contains("ASIA MOVIES")
            || c.contains("ASIA SERIES")
            || c.contains("DISNEY+ ASIA")
    });

    if let Some(m) = meta {
        if m.l.is_empty() || m.l == "en" {
            return false;
        }
        // Keep dedicated seasonal anime out of World Cinema, while allowing landmark animated films
        // like Studio Ghibli masterpieces (vc >= 1500, va >= 7.8).
        let is_east_asian_anim = m.g.contains(&16) && matches!(m.l.as_str(), "ja" | "zh");
        if (in_anime_cat || (it.k == "s" && is_east_asian_anim))
            && !(it.k == "m" && m.va >= 7.8 && m.vc >= 1500)
        {
            return false;
        }
        if m.va >= 7.1 && m.vc >= 150 {
            return true;
        }
        if in_world_cat && m.va >= 7.1 && m.vc >= 40 {
            return true;
        }
        return false;
    }

    in_world_cat && !in_anime_cat && it.rating >= 7.1
}

fn is_apple_prestige(it: &Item, platforms: &[&'static str]) -> bool {
    it.en && !it.sport && (it.k == "m" || it.k == "s") && platforms.contains(&"Apple TV+")
}

fn is_hbo_prestige(it: &Item, platforms: &[&'static str], cat_uppers: &[String]) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") || !platforms.contains(&"HBO") {
        return false;
    }
    // Exclude podcast-only HBO entries from prestige TV/cinema
    !(!cat_uppers.is_empty() && cat_uppers.iter().all(|c| c.contains("PODCAST")))
}

fn is_imdb_top_match(it: &Item, cat_uppers: &[String], meta: Option<&Meta>) -> bool {
    if !it.en || it.sport || it.k != "m" {
        return false;
    }
    if cat_uppers
        .iter()
        .any(|c| c.contains("IMDB TOP 250") || c.contains("IMDB TOP"))
    {
        return true;
    }
    meta.map_or(false, |m| m.va >= 8.1 && m.vc >= 2500)
}

fn is_short_gem_match(
    it: &Item,
    meta: Option<&Meta>,
    is_standup: bool,
    is_concert: bool,
) -> bool {
    if !it.en || it.sport || it.k != "m" || is_standup || is_concert {
        return false;
    }
    let Some(m) = meta else { return false };
    // Exclude documentaries (covered in the Docs hub's Short Documentaries shelf)
    if m.g.contains(&99) {
        return false;
    }
    (65..=95).contains(&m.rt) && m.va >= 7.0 && m.vc >= 200
}

#[derive(Clone, Copy, Default)]
struct BucketFlags {
    standup: bool,
    concerts: bool,
    world: bool,
    apple: bool,
    hbo: bool,
    imdb: bool,
    short: bool,
    in_imdb_cat: bool,
    in_concert_cat: bool,
}

impl BucketFlags {
    fn any(&self) -> bool {
        self.standup
            || self.concerts
            || self.world
            || self.apple
            || self.hbo
            || self.imdb
            || self.short
    }

    fn matches(&self, bucket: &str) -> bool {
        match bucket {
            "standup" | "stand-up" | "comedy-specials" => self.standup,
            "concerts" | "concert" | "music" => self.concerts,
            "world" | "world-cinema" | "worldcinema" => self.world,
            "apple" | "prestige-apple" | "appletv" | "apple+" => self.apple,
            "hbo" | "prestige-hbo" | "hbomax" => self.hbo,
            "imdb" | "imdb-top" | "imdb250" | "classics" => self.imdb,
            "short" | "short-gems" | "shortgems" | "tight" => self.short,
            _ => self.any(),
        }
    }
}

struct SpecialEntry {
    item: Item,
    meta: Option<Meta>,
    flags: BucketFlags,
    platforms: Vec<&'static str>,
    cat_uppers: Vec<String>,
    countries: Vec<String>,
    is_uhd: bool,
}

impl SpecialEntry {
    fn bayes(&self) -> f64 {
        item_bayes(&self.item, self.meta.as_ref())
    }
    fn pop(&self) -> f64 {
        item_pop(&self.item, self.meta.as_ref())
    }
    fn votes(&self) -> i64 {
        item_votes(self.meta.as_ref())
    }
    fn year(&self) -> i32 {
        item_year(&self.item, self.meta.as_ref())
    }
    fn sort_date(&self) -> String {
        item_sort_date(&self.item, self.meta.as_ref())
    }
    fn has_poster(&self) -> bool {
        has_poster(&self.item, self.meta.as_ref())
    }
    fn tmdb_key(&self) -> Option<String> {
        self.item.tmdb.as_ref().map(|t| {
            let mt = if self.item.k == "s" { "tv" } else { "movie" };
            format!("{mt}:{t}")
        })
    }
    fn is_in_set(&self, set: &HashSet<String>) -> bool {
        self.tmdb_key().map_or(false, |k| set.contains(&k))
    }
}

/// Build the full CAM-free specialty pool in one linear pass (zero network calls).
fn build_specials_pool(app: &App) -> Vec<SpecialEntry> {
    let cat = app.catalog();
    let su_kws = standup_keywords(app);
    let db = app.meta.read().unwrap();
    let mut out: Vec<SpecialEntry> = Vec::new();

    for (mt, list) in [("movie", &cat.movies), ("tv", &cat.series)] {
        for it in list {
            if !it.en || it.sport || crate::showcase::is_cam_only(app, it) {
                continue;
            }
            let meta = it
                .tmdb
                .as_ref()
                .and_then(|t| db.get(&format!("{mt}:{t}")))
                .filter(|m| !m.missing);
            let cat_uppers = item_cat_uppers(&cat, it);
            let platforms = cat.item_platforms(it);

            let standup = is_standup_match(it, &cat_uppers, meta, &su_kws);
            let concerts = is_concert_match(it, &cat_uppers, meta);
            let world = is_world_cinema_match(it, &cat_uppers, meta);
            let apple = is_apple_prestige(it, &platforms);
            let hbo = is_hbo_prestige(it, &platforms, &cat_uppers);
            let imdb = is_imdb_top_match(it, &cat_uppers, meta);
            let short = is_short_gem_match(it, meta, standup, concerts);

            let flags = BucketFlags {
                standup,
                concerts,
                world,
                apple,
                hbo,
                imdb,
                short,
                in_imdb_cat: cat_uppers
                    .iter()
                    .any(|c| c.contains("IMDB TOP 250") || c.contains("IMDB TOP")),
                in_concert_cat: cat_uppers
                    .iter()
                    .any(|c| c.contains("CONCERT") || c.contains("NETFLIX MUSIC")),
            };
            if !flags.any() {
                continue;
            }
            let countries = item_countries(it, meta);
            let is_uhd = item_has_uhd(it);
            out.push(SpecialEntry {
                item: it.clone(),
                meta: meta.cloned(),
                flags,
                platforms,
                cat_uppers,
                countries,
                is_uhd,
            });
        }
    }
    drop(db);
    out
}

fn push_hub_row(rows: &mut Vec<Value>, id: &str, title: &str, sub: &str, items: Vec<Value>) {
    if items.is_empty() {
        return;
    }
    rows.push(json!({
        "id": id,
        "title": title,
        "sub": sub,
        "items": items
    }));
}

// ---------------------------------------------------------------------------
// 5. Specialty & Prestige Hub (`specials_hub`)
// ---------------------------------------------------------------------------

/// Instant (zero network calls), strictly CAM-free Specialty & Prestige Hub.
pub fn specials_hub(app: &App) -> Value {
    let pool = build_specials_pool(app);
    let hide = hidden_ratings(app);
    let seen = watched_set(app);
    let cy = chrono::Datelike::year(&chrono::Utc::now());

    let (
        mut standup_cnt,
        mut concerts_cnt,
        mut world_cnt,
        mut apple_cnt,
        mut hbo_cnt,
        mut imdb_cnt,
        mut short_cnt,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);

    for e in &pool {
        if e.flags.standup {
            standup_cnt += 1;
        }
        if e.flags.concerts {
            concerts_cnt += 1;
        }
        if e.flags.world {
            world_cnt += 1;
        }
        if e.flags.apple {
            apple_cnt += 1;
        }
        if e.flags.hbo {
            hbo_cnt += 1;
        }
        if e.flags.imdb {
            imdb_cnt += 1;
        }
        if e.flags.short && !e.is_in_set(&seen) {
            short_cnt += 1;
        }
    }

    let visible: Vec<&SpecialEntry> = pool.iter().filter(|e| !e.is_in_set(&hide)).collect();
    let mut rows: Vec<Value> = Vec::new();

    // 1. Stand-Up Comedy Specials (sorted by rating/newest, up to 48)
    let mut standup_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| e.flags.standup)
        .collect();
    standup_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            let yr = e.year();
            let recency = if yr >= cy - 1 {
                0.75
            } else if yr >= cy - 3 {
                0.45
            } else if yr >= cy - 6 {
                0.2
            } else {
                0.0
            };
            e.bayes() * 1.15
                + recency
                + (e.pop() + 1.0).log10() * 0.45
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b)
            .total_cmp(&score(a))
            .then(b.sort_date().cmp(&a.sort_date()))
    });
    let standup_cards: Vec<Value> = standup_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "standup",
        "Stand-Up Comedy Specials",
        "Netflix, Disney+ & acclaimed stand-up specials",
        standup_cards,
    );

    // 2. Live Concerts & Music Films (up to 48; prioritize live concerts & concert docs ahead of musicals)
    let mut concerts_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| e.flags.concerts)
        .collect();
    concerts_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            let live_bonus = if e.flags.in_concert_cat {
                1.6
            } else if e.meta.as_ref().map_or(false, |m| m.g.contains(&99)) {
                0.7
            } else {
                0.0
            };
            e.bayes() * 1.15
                + (e.pop() + 1.0).log10().min(2.2) * 0.45
                + live_bonus
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b)
            .total_cmp(&score(a))
            .then(b.votes().cmp(&a.votes()))
    });
    let concerts_cards: Vec<Value> = concerts_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "concerts",
        "Live Concerts & Music Films",
        "Concerts, tour films, music docs & musicals",
        concerts_cards,
    );

    // 3. Acclaimed World Cinema (Korean, Japanese, European)
    let mut world_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| {
            e.flags.world
                && e.meta
                    .as_ref()
                    .map_or(true, |m| m.va >= 7.1 && m.vc >= 150)
        })
        .collect();
    if world_v.len() < 12 {
        world_v = visible.iter().copied().filter(|e| e.flags.world).collect();
    }
    world_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            e.bayes() * 1.45
                + (e.votes().max(0) as f64 + 1.0).log10() * 0.25
                + (e.pop() + 1.0).log10().min(2.2) * 0.3
                + if e.is_uhd { 0.2 } else { 0.0 }
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b).total_cmp(&score(a)).then(b.votes().cmp(&a.votes()))
    });
    let world_cards: Vec<Value> = world_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "world-cinema",
        "Acclaimed World Cinema",
        "Korean, Japanese & European cinema · great with English subtitles",
        world_cards,
    );

    // 4. Apple TV+ Prestige
    let mut apple_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| e.flags.apple)
        .collect();
    apple_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            let kids_pen = if e.meta.as_ref().map_or(false, |m| m.g.contains(&10762)) {
                -1.2
            } else {
                0.0
            };
            e.bayes() * 1.2
                + (e.pop() + 1.0).log10() * 0.7
                + if e.is_uhd { 0.35 } else { 0.0 }
                + kids_pen
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b).total_cmp(&score(a)).then(b.votes().cmp(&a.votes()))
    });
    let apple_cards: Vec<Value> = apple_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "prestige-apple",
        "Apple TV+ Prestige",
        "Original series & films · often in 4K Dolby Vision",
        apple_cards,
    );

    // 5. HBO Prestige
    let mut hbo_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| e.flags.hbo)
        .collect();
    hbo_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            e.bayes() * 1.2
                + (e.pop() + 1.0).log10() * 0.7
                + if e.is_uhd { 0.35 } else { 0.0 }
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b).total_cmp(&score(a)).then(b.votes().cmp(&a.votes()))
    });
    let hbo_cards: Vec<Value> = hbo_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "prestige-hbo",
        "HBO Prestige",
        "Acclaimed HBO series & originals",
        hbo_cards,
    );

    // 6. IMDb Top 250 Classics
    let mut imdb_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| e.flags.imdb)
        .collect();
    imdb_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            e.bayes() * 1.35
                + (e.votes().max(0) as f64 + 1.0).log10() * 0.35
                + if e.flags.in_imdb_cat { 0.2 } else { 0.0 }
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b).total_cmp(&score(a)).then(b.votes().cmp(&a.votes()))
    });
    let imdb_cards: Vec<Value> = imdb_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "imdb-top",
        "IMDb Top 250 Classics",
        "All-time highest-rated cinema",
        imdb_cards,
    );

    // 7. Under 95 Minutes (Tight Bangers) — unwatched movies, 65..=95m, va >= 7.0, vc >= 200
    let mut short_v: Vec<&SpecialEntry> = visible
        .iter()
        .copied()
        .filter(|e| e.flags.short && !e.is_in_set(&seen))
        .collect();
    short_v.sort_by(|a, b| {
        let score = |e: &SpecialEntry| {
            e.bayes() * 1.4
                + (e.votes().max(0) as f64 + 1.0).log10() * 0.2
                + (e.pop() + 1.0).log10().min(2.2) * 0.3
                + if e.is_uhd { 0.2 } else { 0.0 }
                + if e.has_poster() { 0.0 } else { -2.5 }
        };
        score(b).total_cmp(&score(a)).then(b.votes().cmp(&a.votes()))
    });
    let short_cards: Vec<Value> = short_v
        .into_iter()
        .take(48)
        .map(|e| app.card_meta(&e.item))
        .collect();
    push_hub_row(
        &mut rows,
        "short-gems",
        "Under 95 Minutes (Tight Bangers)",
        "Unwatched acclaimed films · 65–95 min",
        short_cards,
    );

    json!({
        "rows": rows,
        "counts": {
            "standup": standup_cnt,
            "concerts": concerts_cnt,
            "worldCinema": world_cnt,
            "apple": apple_cnt,
            "hbo": hbo_cnt,
            "imdbTop": imdb_cnt,
            "shortGems": short_cnt
        }
    })
}

// ---------------------------------------------------------------------------
// 6. Specialty & Prestige Browse (`specials_browse`)
// ---------------------------------------------------------------------------

fn matches_special_search(e: &SpecialEntry, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut hay = String::with_capacity(256);
    hay.push_str(&e.item.title);
    hay.push(' ');
    let yr = e.year();
    if yr > 0 {
        hay.push_str(&yr.to_string());
        hay.push(' ');
    }
    if let Some(m) = &e.meta {
        hay.push_str(&m.ov);
        hay.push(' ');
        if !m.l.is_empty() {
            hay.push_str(&m.l);
            hay.push(' ');
            let la = lang_aliases(&m.l);
            if !la.is_empty() {
                hay.push_str(la);
                hay.push(' ');
            }
        }
        for &g in &m.g {
            if let Some(gn) = crate::meta::genre_name(e.item.k == "s", g) {
                hay.push_str(gn);
                hay.push(' ');
            }
        }
    }
    if !e.item.plot.is_empty() {
        hay.push_str(&e.item.plot);
        hay.push(' ');
    }
    if !e.item.genre.is_empty() {
        hay.push_str(&e.item.genre);
        hay.push(' ');
    }
    for p in &e.platforms {
        hay.push_str(p);
        hay.push(' ');
    }
    for c in &e.countries {
        hay.push_str(c);
        hay.push(' ');
        let ca = country_aliases(c);
        if !ca.is_empty() {
            hay.push_str(ca);
            hay.push(' ');
        }
    }
    for cn in &e.cat_uppers {
        hay.push_str(cn);
        hay.push(' ');
    }
    let lo = hay.to_lowercase();
    terms.iter().all(|w| lo.contains(w.as_str()))
}

/// Filterable, searchable Specialty & Prestige explorer across all 7 buckets.
/// Params: `bucket=standup|concerts|world|apple|hbo|imdb|short`, `q`, `unwatched=1`,
/// `uhd=1`, `sort=rating|newest|added|az`, `page`, `limit`.
pub fn specials_browse(app: &App, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let bucket = get("bucket").trim().to_lowercase();
    let terms: Vec<String> = get("q")
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let unwatched = matches!(get("unwatched").trim(), "1" | "true");
    let uhd = matches!(get("uhd").trim(), "1" | "true");
    let sort = get("sort").trim().to_lowercase();
    let page = get("page").trim().parse::<usize>().unwrap_or(0);
    let limit = q
        .get("limit")
        .or_else(|| q.get("per"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(60)
        .clamp(1, 120);

    let seen = if unwatched {
        Some(watched_set(app))
    } else {
        None
    };
    let pool = build_specials_pool(app);

    let (
        mut c_all,
        mut c_standup,
        mut c_concerts,
        mut c_world,
        mut c_apple,
        mut c_hbo,
        mut c_imdb,
        mut c_short,
    ) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);

    let mut matched: Vec<&SpecialEntry> = Vec::new();

    for e in &pool {
        if uhd && !e.is_uhd {
            continue;
        }
        if let Some(seen) = &seen {
            if e.is_in_set(seen) {
                continue;
            }
        }
        if !matches_special_search(e, &terms) {
            continue;
        }

        c_all += 1;
        if e.flags.standup {
            c_standup += 1;
        }
        if e.flags.concerts {
            c_concerts += 1;
        }
        if e.flags.world {
            c_world += 1;
        }
        if e.flags.apple {
            c_apple += 1;
        }
        if e.flags.hbo {
            c_hbo += 1;
        }
        if e.flags.imdb {
            c_imdb += 1;
        }
        if e.flags.short {
            c_short += 1;
        }

        if e.flags.matches(&bucket) {
            matched.push(e);
        }
    }

    match sort.as_str() {
        "rating" => matched.sort_by(|a, b| {
            b.bayes()
                .total_cmp(&a.bayes())
                .then(b.votes().cmp(&a.votes()))
                .then(b.pop().total_cmp(&a.pop()))
        }),
        "newest" => matched.sort_by(|a, b| {
            b.sort_date()
                .cmp(&a.sort_date())
                .then(b.item.added.cmp(&a.item.added))
                .then(b.bayes().total_cmp(&a.bayes()))
        }),
        "added" => matched.sort_by(|a, b| {
            b.item
                .added
                .cmp(&a.item.added)
                .then(b.bayes().total_cmp(&a.bayes()))
        }),
        "az" => matched.sort_by(|a, b| {
            a.item
                .title
                .to_lowercase()
                .cmp(&b.item.title.to_lowercase())
        }),
        _ => matched.sort_by(|a, b| {
            let sa = a.bayes() * 1.2 + (a.pop() + 1.0).log10() * 0.7;
            let sb = b.bayes() * 1.2 + (b.pop() + 1.0).log10() * 0.7;
            sb.total_cmp(&sa).then(b.votes().cmp(&a.votes()))
        }),
    }

    let total = matched.len();
    let more = (page + 1).saturating_mul(limit) < total;
    let items: Vec<Value> = matched
        .into_iter()
        .skip(page.saturating_mul(limit))
        .take(limit)
        .map(|e| app.card_meta(&e.item))
        .collect();

    json!({
        "items": items,
        "total": total,
        "page": page,
        "limit": limit,
        "more": more,
        "bucketCounts": {
            "all": c_all,
            "standup": c_standup,
            "concerts": c_concerts,
            "world": c_world,
            "apple": c_apple,
            "hbo": c_hbo,
            "imdb": c_imdb,
            "short": c_short
        }
    })
}

// ---------------------------------------------------------------------------
// 7. Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_item(k: &'static str, title: &str, genre: &str) -> Item {
        Item {
            k,
            id: 1,
            title: title.into(),
            en: true,
            sport: false,
            genre: genre.into(),
            ..Default::default()
        }
    }

    #[test]
    fn persona_classification_is_deterministic_and_accurate() {
        // Dark Thriller Purist
        let g_thrill = [(53, "Thriller", 12), (80, "Crime", 8)];
        let (tag, sum) = compute_persona(&PersonaInput {
            top_genres: &g_thrill,
            top_decade: Some("2010s"),
            uhd_share_pct: 40,
            avg_rating: 7.7,
            movies_watched: 20,
            episodes_watched: 5,
            non_en_share_pct: 10,
            doc_share_pct: 5,
            top_director: Some("David Fincher"),
            top_platform: Some("Netflix"),
        });
        assert_eq!(tag, "Dark Thriller Purist");
        assert!(sum.contains("Thriller") && sum.contains("Crime"));
        assert!(sum.contains("2010s"));
        assert!(sum.contains("David Fincher"));

        // Prestige Sci-Fi Cinephile
        let g_scifi = [(878, "Sci-Fi", 15), (18, "Drama", 10)];
        let (tag, sum) = compute_persona(&PersonaInput {
            top_genres: &g_scifi,
            top_decade: Some("2020s"),
            uhd_share_pct: 65,
            avg_rating: 7.9,
            movies_watched: 18,
            episodes_watched: 12,
            non_en_share_pct: 15,
            doc_share_pct: 0,
            top_director: Some("Denis Villeneuve"),
            top_platform: Some("Apple TV+"),
        });
        assert_eq!(tag, "Prestige Sci-Fi Cinephile");
        assert!(sum.contains("65% in 4K"));
        assert!(sum.contains("Denis Villeneuve"));

        // 4K Action & Crime Fan
        let g_act = [(28, "Action", 14), (80, "Crime", 9)];
        let (tag, _) = compute_persona(&PersonaInput {
            top_genres: &g_act,
            top_decade: Some("2020s"),
            uhd_share_pct: 72,
            avg_rating: 7.1,
            movies_watched: 22,
            episodes_watched: 4,
            non_en_share_pct: 5,
            doc_share_pct: 0,
            top_director: None,
            top_platform: Some("Amazon"),
        });
        assert_eq!(tag, "4K Action & Crime Fan");

        // True-Crime Doc Explorer
        let g_doc = [(99, "Documentary", 10), (80, "Crime", 7)];
        let (tag, _) = compute_persona(&PersonaInput {
            top_genres: &g_doc,
            top_decade: Some("2020s"),
            uhd_share_pct: 30,
            avg_rating: 7.4,
            movies_watched: 8,
            episodes_watched: 10,
            non_en_share_pct: 5,
            doc_share_pct: 55,
            top_director: None,
            top_platform: Some("Netflix"),
        });
        assert_eq!(tag, "True-Crime Doc Explorer");

        // Fresh / empty user
        let (tag_empty, sum_empty) = compute_persona(&PersonaInput {
            top_genres: &[],
            top_decade: None,
            uhd_share_pct: 0,
            avg_rating: 0.0,
            movies_watched: 0,
            episodes_watched: 0,
            non_en_share_pct: 0,
            doc_share_pct: 0,
            top_director: None,
            top_platform: None,
        });
        assert_eq!(tag_empty, "Fresh Screen Explorer");
        assert!(!sum_empty.is_empty());
    }

    #[test]
    fn decade_bucketing_handles_all_eras_and_invalid_years() {
        assert_eq!(decade_bucket(2026), Some("2020s"));
        assert_eq!(decade_bucket(2020), Some("2020s"));
        assert_eq!(decade_bucket(2019), Some("2010s"));
        assert_eq!(decade_bucket(2010), Some("2010s"));
        assert_eq!(decade_bucket(2005), Some("2000s"));
        assert_eq!(decade_bucket(1999), Some("1990s"));
        assert_eq!(decade_bucket(1990), Some("1990s"));
        assert_eq!(decade_bucket(1989), Some("Classic"));
        assert_eq!(decade_bucket(1957), Some("Classic"));
        assert_eq!(decade_bucket(0), None);
        assert_eq!(decade_bucket(-5), None);
    }

    #[test]
    fn runtime_sweet_spot_computes_iqr_and_edge_cases() {
        assert_eq!(compute_runtime_sweet_spot(&[]), "");
        assert_eq!(compute_runtime_sweet_spot(&[0, 12, 999]), "");
        assert_eq!(compute_runtime_sweet_spot(&[110]), "100–120 min");
        assert_eq!(compute_runtime_sweet_spot(&[95, 95, 110, 125, 125]), "95–125 min");
        // Tight cluster (< 10m IQR) widens smoothly around median
        assert_eq!(compute_runtime_sweet_spot(&[100, 102, 104, 105]), "93–113 min");
        // Outliers below 30m or above 300m are ignored
        assert_eq!(compute_runtime_sweet_spot(&[5, 90, 95, 110, 125, 130, 500]), "95–125 min");
    }

    #[test]
    fn standup_and_concert_category_and_meta_detection() {
        let su_kws: HashSet<i64> = [TMDB_STANDUP_KW].into_iter().collect();

        // Provider Stand-Up categories (movies + series)
        let chappelle = mk_item("m", "Dave Chappelle: The Closer", "");
        assert!(is_standup_match(
            &chappelle,
            &["NETFLIX STAND-UP COMEDY".to_string()],
            None,
            &su_kws
        ));
        let disney_su = mk_item("m", "Jim Gaffigan: The Skinny", "");
        assert!(is_standup_match(
            &disney_su,
            &["DISNEY+ STAND-UP COMEDY".to_string()],
            None,
            &su_kws
        ));
        let kw_su = mk_item("m", "Bo Burnham: Inside", "");
        let kw_meta = Meta { g: vec![35, 10402], kw: vec![9716], ..Default::default() };
        assert!(is_standup_match(&kw_su, &["NETFLIX MOVIES".to_string()], Some(&kw_meta), &su_kws));

        // Scripted sitcom about a comedian (e.g. Louie) must NOT falsely trigger Stand-Up via keyword 9716
        let louie = mk_item("s", "Louie", "Comedy, Drama");
        let louie_meta = Meta { g: vec![35, 18], kw: vec![9716], ..Default::default() };
        assert!(!is_standup_match(&louie, &["ENGLISH SERIES".to_string()], Some(&louie_meta), &su_kws));

        // Non-stand-up comedies & "Stand by Me" must NOT falsely trigger
        let stand_by_me = mk_item("m", "Stand by Me", "Drama, Adventure");
        let drama_meta = Meta { g: vec![18, 12], ov: "Four boys stand up for each other.".into(), ..Default::default() };
        assert!(!is_standup_match(&stand_by_me, &["EN - DRAMA".to_string()], Some(&drama_meta), &su_kws));
        let superbad = mk_item("m", "Superbad", "Comedy");
        let com_meta = Meta { g: vec![35], ov: "Two high school friends throw a party.".into(), ..Default::default() };
        assert!(!is_standup_match(&superbad, &["EN - COMEDY".to_string()], Some(&com_meta), &su_kws));

        // Concerts & Music Films categories
        let zimmer = mk_item("m", "Hans Zimmer: Live in Prague", "");
        assert!(is_concert_match(&zimmer, &["EN - CONCERTS".to_string()], None));
        let nf_music = mk_item("m", "Homecoming: A Film by Beyonce", "");
        assert!(is_concert_match(&nf_music, &["NETFLIX MUSIC".to_string()], None));
        let musical = mk_item("m", "Hamilton", "");
        assert!(is_concert_match(&musical, &["EN - MUSICAL".to_string()], None));

        // Meta-based concert film outside concert category
        let stop_making = mk_item("m", "Stop Making Sense", "");
        let concert_meta = Meta {
            g: vec![10402, 99],
            ov: "Talking Heads live performance filmed in Hollywood.".into(),
            ..Default::default()
        };
        assert!(is_concert_match(&stop_making, &["EN - CLASSICS".to_string()], Some(&concert_meta)));

        // Ordinary drama mentioning "live in Paris" must NOT match concerts
        let paris = mk_item("m", "Midnight in Paris", "");
        let paris_meta = Meta {
            g: vec![35, 14, 10749],
            ov: "A writer travels to live in Paris during the 1920s.".into(),
            ..Default::default()
        };
        assert!(!is_concert_match(&paris, &["EN - ROMANCE".to_string()], Some(&paris_meta)));
    }

    #[test]
    fn world_cinema_and_short_gems_filtering() {
        // Acclaimed Korean film -> included
        let parasite = mk_item("m", "Parasite", "");
        let parasite_meta = Meta {
            l: "ko".into(),
            va: 8.5,
            vc: 18000,
            g: vec![35, 53, 18],
            rt: 132,
            ..Default::default()
        };
        assert!(is_world_cinema_match(&parasite, &["EN - THRILLER".to_string()], Some(&parasite_meta)));

        // Acclaimed Italian film in EN - ITALIAN SUB ENG -> included
        let cinema_p = mk_item("m", "Cinema Paradiso", "");
        let cp_meta = Meta {
            l: "it".into(),
            va: 8.4,
            vc: 4200,
            g: vec![18, 10749],
            rt: 124,
            ..Default::default()
        };
        assert!(is_world_cinema_match(&cinema_p, &["EN - ITALIAN SUB ENG".to_string()], Some(&cp_meta)));

        // English-language film -> excluded from World Cinema
        let dune = mk_item("m", "Dune", "");
        let dune_meta = Meta {
            l: "en".into(),
            va: 7.9,
            vc: 12000,
            g: vec![878, 12],
            rt: 155,
            ..Default::default()
        };
        assert!(!is_world_cinema_match(&dune, &["EN - SCI-FI".to_string()], Some(&dune_meta)));

        // Low-rated or low-vote non-English outside curated world cats -> excluded
        let low_meta = Meta { l: "fr".into(), va: 6.4, vc: 500, g: vec![18], ..Default::default() };
        assert!(!is_world_cinema_match(&parasite, &["EN - DRAMA".to_string()], Some(&low_meta)));
        let tiny_votes = Meta { l: "es".into(), va: 7.8, vc: 25, g: vec![18], ..Default::default() };
        assert!(!is_world_cinema_match(&parasite, &["EN - DRAMA".to_string()], Some(&tiny_votes)));

        // Seasonal Japanese TV anime -> excluded (lives in Anime hub), but landmark Ghibli film -> included!
        let tv_anime = mk_item("s", "Solo Leveling", "");
        let tv_anime_meta = Meta { l: "ja".into(), va: 8.4, vc: 1200, g: vec![16, 10759], ..Default::default() };
        assert!(!is_world_cinema_match(&tv_anime, &["CRUNCHYROLL".to_string()], Some(&tv_anime_meta)));
        let spirited = mk_item("m", "Spirited Away", "");
        let spirited_meta = Meta { l: "ja".into(), va: 8.5, vc: 16500, g: vec![16, 14, 10751], ..Default::default() };
        assert!(is_world_cinema_match(&spirited, &["NETFLIX ASIA".to_string()], Some(&spirited_meta)));

        // Under-95m Tight Bangers
        let run_lola = mk_item("m", "Run Lola Run", "");
        let lola_meta = Meta { rt: 81, va: 7.4, vc: 2100, g: vec![28, 18, 53], ..Default::default() };
        assert!(is_short_gem_match(&run_lola, Some(&lola_meta), false, false));
        // Too short (<65), too long (>95), documentary, or stand-up special -> excluded
        let too_short = Meta { rt: 45, va: 7.8, vc: 500, ..Default::default() };
        let too_long = Meta { rt: 112, va: 8.1, vc: 5000, ..Default::default() };
        let short_doc = Meta { rt: 80, va: 7.9, vc: 500, g: vec![99], ..Default::default() };
        assert!(!is_short_gem_match(&run_lola, Some(&too_short), false, false));
        assert!(!is_short_gem_match(&run_lola, Some(&too_long), false, false));
        assert!(!is_short_gem_match(&run_lola, Some(&short_doc), false, false));
        assert!(!is_short_gem_match(&run_lola, Some(&lola_meta), true, false));
    }

    #[test]
    fn prestige_and_imdb_and_record_dedup_helpers() {
        let apple_show = mk_item("s", "Severance", "");
        assert!(is_apple_prestige(&apple_show, &["Apple TV+"]));
        assert!(!is_apple_prestige(&apple_show, &["Netflix"]));

        let hbo_show = mk_item("s", "The Sopranos", "");
        assert!(is_hbo_prestige(&hbo_show, &["HBO"], &["HBO MAX SERIES".to_string()]));
        assert!(!is_hbo_prestige(&hbo_show, &["HBO"], &["HBO MAX PODCAST".to_string()]));

        let classic = mk_item("m", "The Godfather", "");
        assert!(is_imdb_top_match(&classic, &["EN - IMDB TOP 250".to_string()], None));
        let top_meta = Meta { va: 8.5, vc: 12000, ..Default::default() };
        assert!(is_imdb_top_match(&classic, &["EN - CRIME".to_string()], Some(&top_meta)));
        let mid_meta = Meta { va: 7.4, vc: 4000, ..Default::default() };
        assert!(!is_imdb_top_match(&classic, &["EN - CRIME".to_string()], Some(&mid_meta)));

        // Title-only history record merges into existing TMDB record
        let mut idx: HashMap<(&'static str, String), String> = HashMap::new();
        idx.insert(("movie", norm("Tenzing")), "movie:1288069".to_string());
        assert_eq!(
            resolve_record_key("movie", None, 0, "Tenzing", &idx),
            "movie:1288069"
        );
        assert_eq!(
            resolve_record_key("movie", Some("1288069"), 99, "Tenzing", &idx),
            "movie:1288069"
        );

        // Bucket alias matching
        let flags = BucketFlags {
            world: true,
            imdb: true,
            ..Default::default()
        };
        assert!(flags.matches("world"));
        assert!(flags.matches("world-cinema"));
        assert!(flags.matches("imdb"));
        assert!(flags.matches("imdb-top"));
        assert!(flags.matches("all"));
        assert!(!flags.matches("standup"));
    }
}
