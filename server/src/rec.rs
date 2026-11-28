// Rows: real-world trends (TMDB) ∩ catalogue, personal rows, offline Taste Vector scorer, catalogue fallbacks.
use crate::app::App;
use crate::catalog::Item;
use crate::meta::Meta;
use crate::tmdb::{TTL_D, TTL_H};
use crate::util::{now_ms, s};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

const MOVIE_GENRE_ROWS: &[(i64, &str)] = &[(28, "Action"), (53, "Thrillers"), (35, "Comedy"), (878, "Sci-Fi"), (27, "Horror"), (80, "Crime"), (18, "Drama"), (16, "Animation"), (10751, "Family"), (99, "Documentaries"), (10749, "Romance"), (12, "Adventure")];
const TV_GENRE_ROWS: &[(i64, &str)] = &[(18, "Drama"), (80, "Crime"), (35, "Comedy"), (10765, "Sci-Fi & Fantasy"), (10759, "Action & Adventure"), (99, "Docuseries"), (10764, "Reality"), (16, "Animation"), (9648, "Mystery"), (10762, "Kids")];

fn western(o: &str) -> bool { matches!(o, "" | "US" | "UK" | "GB" | "AU" | "CA" | "IE" | "NZ") }
fn row(id: &str, title: &str, items: Vec<Value>) -> Value { json!({ "id": id, "title": title, "items": items }) }
fn rows_nonempty(v: Vec<Value>) -> Value { Value::Array(v.into_iter().filter(|r| r["items"].as_array().map_or(false, |a| !a.is_empty())).collect()) }
pub fn sp(k: &'static str, v: &str) -> (&'static str, String) { (k, v.to_string()) }

pub fn hidden_ratings(app: &App) -> HashSet<String> {
    app.state.read(|st| st["ratings"].as_object().map(|m| m.iter().filter(|(_, v)| v["v"].as_i64().unwrap_or(0) < 0).map(|(k, _)| k.clone()).collect()).unwrap_or_default())
}

/// Cross-media genre projection: maps any movie OR TV genre ID into equivalent TMDB movie genre IDs with weights.
fn movie_genre_equivalents(g: i64) -> &'static [(i64, f64)] {
    match g {
        10759 => &[(28, 1.0), (12, 0.9)],     // TV Action & Adventure -> Movie Action, Adventure
        10765 => &[(878, 1.0), (14, 0.9)],    // TV Sci-Fi & Fantasy -> Movie Sci-Fi, Fantasy
        10768 => &[(10752, 1.0)],             // TV War & Politics -> Movie War
        10762 => &[(10751, 0.8), (16, 0.6)],  // TV Kids -> Movie Family, Animation
        10763 | 10764 | 10766 | 10767 => &[], // TV-only genres (News, Reality, Soap, Talk)
        28 => &[(28, 1.0)],
        12 => &[(12, 1.0)],
        16 => &[(16, 1.0)],
        35 => &[(35, 1.0)],
        80 => &[(80, 1.0)],
        99 => &[(99, 1.0)],
        18 => &[(18, 1.0)],
        10751 => &[(10751, 1.0)],
        14 => &[(14, 1.0)],
        36 => &[(36, 1.0)],
        27 => &[(27, 1.0)],
        10402 => &[(10402, 1.0)],
        9648 => &[(9648, 1.0)],
        10749 => &[(10749, 1.0)],
        878 => &[(878, 1.0)],
        10770 => &[(10770, 1.0)],
        53 => &[(53, 1.0)],
        10752 => &[(10752, 1.0)],
        37 => &[(37, 1.0)],
        _ => &[],
    }
}

/// Cross-media genre projection: maps any movie OR TV genre ID into equivalent TMDB TV genre IDs with weights.
fn tv_genre_equivalents(g: i64) -> &'static [(i64, f64)] {
    match g {
        28 | 12 => &[(10759, 1.0)],             // Movie Action / Adventure -> TV Action & Adventure
        878 | 14 => &[(10765, 1.0)],            // Movie Sci-Fi / Fantasy -> TV Sci-Fi & Fantasy
        10752 => &[(10768, 1.0)],               // Movie War -> TV War & Politics
        53 => &[(80, 0.65), (9648, 0.65)],      // Movie Thriller -> TV Crime, Mystery
        27 => &[(9648, 0.6), (10765, 0.55)],    // Movie Horror -> TV Mystery, Sci-Fi & Fantasy
        10749 | 36 | 10402 | 10770 => &[],      // Movie-only genres unsupported by /discover/tv
        10759 => &[(10759, 1.0)],
        16 => &[(16, 1.0)],
        35 => &[(35, 1.0)],
        80 => &[(80, 1.0)],
        99 => &[(99, 1.0)],
        18 => &[(18, 1.0)],
        10751 => &[(10751, 1.0)],
        10762 => &[(10762, 1.0)],
        9648 => &[(9648, 1.0)],
        10763 => &[(10763, 1.0)],
        10764 => &[(10764, 1.0)],
        10765 => &[(10765, 1.0)],
        10766 => &[(10766, 1.0)],
        10767 => &[(10767, 1.0)],
        10768 => &[(10768, 1.0)],
        37 => &[(37, 1.0)],
        _ => &[],
    }
}

fn add_cross_genre_weight(gw_movie: &mut HashMap<i64, f64>, gw_tv: &mut HashMap<i64, f64>, genre_id: i64, weight: f64) {
    for &(mg, f) in movie_genre_equivalents(genre_id) {
        *gw_movie.entry(mg).or_default() += weight * f;
    }
    for &(tg, f) in tv_genre_equivalents(genre_id) {
        *gw_tv.entry(tg).or_default() += weight * f;
    }
}

fn is_rewatch_allowed(watched_ts: Option<i64>, progress_ts: i64) -> bool {
    match watched_ts {
        Some(wt) => progress_ts > wt,
        None => true,
    }
}

/// Map TMDB results to catalogue items (TMDB order), skipping hidden ones.
pub fn match_results(app: &App, results: &[Value], kind: Option<&str>, limit: usize) -> Vec<Value> {
    let cat = app.catalog(); let hide = hidden_ratings(app); let prefs = crate::discover::hide_prefs(app);
    let hide_cams = crate::discover::hide_cams_enabled(app);
    let mut out = vec![]; let mut seen = HashSet::new();
    for r in results {
        let mt = r["media_type"].as_str().or(kind).unwrap_or("");
        let id = r["id"].as_i64().map(|x| x.to_string()).unwrap_or_else(|| s(&r["id"]));
        if hide.contains(&format!("{mt}:{id}")) { continue; }
        if !prefs.is_empty() {
            let g: Vec<i64> = r["genre_ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default();
            if crate::discover::hidden_by(&prefs, &g, r["original_language"].as_str().unwrap_or(""), "", &[], &HashSet::new()) { continue; }
        }
        let it = match mt { "tv" => cat.by_tmdb("tv", &id), "movie" => cat.by_tmdb("movie", &id), _ => None };
        let Some(it) = it else { continue };
        if hide_cams && crate::showcase::is_cam_only(app, it) { continue; }
        if !seen.insert(format!("{}{}", it.k, it.id)) { continue; }
        out.push(app.card(it, Some(r)));
        if out.len() >= limit { break; }
    }
    out
}

fn year() -> i32 { chrono::Datelike::year(&chrono::Utc::now()) }

pub fn recent(app: &App, tv: bool, n: usize) -> Vec<Value> {
    let cat = app.catalog(); let y = year(); let list = if tv { &cat.series } else { &cat.movies };
    let mut v: Vec<&Item> = list.iter().filter(|x| x.en && !x.sport && x.year.map_or(false, |yy| yy >= y - 1) && !x.poster.is_empty() && western(&x.origin) && !crate::discover::hidden_item(app, x)).collect();
    v.sort_by(|a, b| b.added.cmp(&a.added));
    v.into_iter().take(n).map(|x| app.card(x, None)).collect()
}
pub fn buzz(app: &App, tv: bool, n: usize) -> Vec<Value> {
    let cat = app.catalog(); let y = year(); let now = now_ms() as f64 / 1000.0; let list = if tv { &cat.series } else { &cat.movies };
    let score = |x: &Item| x.rating + match x.year { Some(yy) if yy >= y => 3.0, Some(yy) if yy == y - 1 => 1.5, _ => 0.0 } + (3.0 - (now - x.added as f64) / (86400.0 * 10.0)).max(0.0);
    let mut v: Vec<&Item> = list.iter().filter(|x| x.en && !x.sport && !x.poster.is_empty() && x.rating >= 6.0 && x.year.map_or(false, |yy| yy >= y - 1) && western(&x.origin) && !crate::discover::hidden_item(app, x)).collect();
    v.sort_by(|a, b| score(b).total_cmp(&score(a)));
    v.into_iter().take(n).map(|x| app.card(x, None)).collect()
}
pub fn from_category(app: &App, tv: bool, re: &regex::Regex, n: usize, by_rating: bool) -> Vec<Value> {
    let cat = app.catalog(); let kind = if tv { "series" } else { "movie" };
    let ids: HashSet<&str> = cat.cats.values().filter(|c| c.kind == kind && c.count > 0 && (c.en || c.sport) && re.is_match(&c.name)).map(|c| c.id.as_str()).collect();
    let list = if tv { &cat.series } else { &cat.movies };
    let mut v: Vec<&Item> = list.iter().filter(|x| ids.contains(x.cat.as_str()) && !x.poster.is_empty()).collect();
    if by_rating { v.sort_by(|a, b| b.rating.total_cmp(&a.rating)); } else { v.sort_by(|a, b| b.added.cmp(&a.added)); }
    v.into_iter().take(n).map(|x| app.card(x, None)).collect()
}

/// Re-hydrate `My List` entries against the live catalog so cards always show fresh posters,
/// ratings, 4K/DV badges, genres, and canonical IDs instead of stale snapshots.
pub fn my_list_cards(app: &App) -> Vec<Value> {
    let cat = app.catalog();
    let raw: Vec<(String, Value)> = app.state.read(|st| {
        let mut v: Vec<(&String, &Value)> = st["list"].as_object().map(|m| m.iter().collect()).unwrap_or_default();
        v.sort_by(|a, b| b.1["t"].as_i64().cmp(&a.1["t"].as_i64()));
        v.into_iter().map(|(k, x)| (k.clone(), x["item"].clone())).collect()
    });
    raw.into_iter().map(|(list_key, saved)| {
        let is_tv = list_key.starts_with("s:") || saved["k"].as_str() == Some("s");
        let mt = if is_tv { "tv" } else { "movie" };
        let tmdb = s(&saved["tmdb"]);
        let id = saved["id"].as_i64()
            .or_else(|| list_key.split_once(':').and_then(|(_, rest)| rest.parse::<i64>().ok()))
            .unwrap_or(0);
        let live = (!tmdb.is_empty()).then(|| cat.by_tmdb(mt, &tmdb)).flatten()
            .or_else(|| if id > 0 { if is_tv { cat.series_item(id) } else { cat.movie(id) } } else { None });
        match live {
            Some(it) => app.card_meta(it),
            None => saved,
        }
    }).collect()
}

/// Super-hardened Continue Watching:
/// - Deduplicates by canonical show (handling both `item.sid` and legacy `k=="s"` `item.id`) AND by canonical movie/TMDB id.
/// - Merges episode lists across primary + variants so watching on a 4K / Nordic variant never drops a show from Next Up.
/// - Allows rewatches when `progress.t > watched.t`.
/// - Excludes Season 0 specials from Next Up unless actively watching Season 0.
/// - Always populates `latest_e["key"]` for credits-based completion detection.
pub async fn continue_watching(app: &Arc<App>) -> Vec<Value> {
    let now = now_ms();
    let (entries, dismissed, pinned, watched_ts): (Vec<(String, Value)>, HashMap<String, i64>, HashMap<String, i64>, HashMap<String, i64>) = app.state.read(|st| {
        let entries = st["progress"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        let dis = st["cwDismissed"].as_object().map(|m| m.iter().filter_map(|(k, v)| v.as_i64().map(|t| (k.clone(), t))).collect()).unwrap_or_default();
        let pin = st["cwPinned"].as_object().map(|m| m.iter().filter_map(|(k, v)| v.as_i64().map(|t| (k.clone(), t))).collect()).unwrap_or_default();
        let wat = st["watched"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v["t"].as_i64().unwrap_or(0))).collect()).unwrap_or_default();
        (entries, dis, pin, wat)
    });
    let cat = app.catalog();
    let is_finished = |pos: f64, dur: f64, eid: Option<i64>| -> bool {
        if dur <= 0.0 { return false; }
        let p = pos.clamp(0.0, dur);
        let pct = p / dur;
        if pct >= 0.92 { return true; }
        // Long movies (>90 min): within the final 9 minutes is almost always credits
        if eid.is_none() && dur >= 5400.0 && (dur - p) <= 540.0 && pct >= 0.87 { return true; }
        if let Some(id) = eid {
            if id > 0 {
                if let Some(cr) = app.skips.entry(id)["credits"].as_f64() {
                    if cr > dur * 0.6 && p >= cr - 15.0 { return true; }
                }
            }
        }
        false
    };

    let mut watched_eps: HashSet<i64> = HashSet::new();
    // Canonical series id -> (latest_t, latest_entry, latest_in_progress_entry_if_any)
    let mut shows: HashMap<i64, (i64, Value, Option<(i64, Value)>)> = HashMap::new();
    // Canonical movie key ("tmdb:..." or "m:<canonical_id>") -> (t, card)
    let mut movie_cards: HashMap<String, (i64, Value)> = HashMap::new();
    let mut cards: Vec<(bool, i64, Value)> = vec![];

    for (key, raw_e) in &entries {
        let mut e = raw_e.clone();
        // Always ensure `e["key"]` is populated for credits-based completion detection and card wiring.
        if e["key"].as_str().map_or(true, |k| k.is_empty()) {
            e["key"] = json!(key);
        }
        let d = e["dur"].as_f64().unwrap_or(0.0).max(0.0);
        let p = if d > 0.0 { e["pos"].as_f64().unwrap_or(0.0).clamp(0.0, d) } else { 0.0 };
        e["pos"] = json!(p);
        let t = e["t"].as_i64().unwrap_or(0);
        let pct = if d > 0.0 { p / d } else { 0.0 };

        if key.starts_with("e:") {
            let eid = key[2..].parse::<i64>().unwrap_or(0);
            let fin = is_finished(p, d, Some(eid));
            if fin { watched_eps.insert(eid); }
            let raw_sid = e["item"]["sid"].as_i64()
                .or_else(|| e["item"]["sid"].as_str().and_then(|x| x.parse().ok()))
                .or_else(|| if e["item"]["k"] == "s" { e["item"]["id"].as_i64() } else { None })
                .unwrap_or(0);
            let sid = if raw_sid > 0 { cat.series_item(raw_sid).map(|it| it.id).unwrap_or(raw_sid) } else { 0 };
            if sid > 0 {
                let in_prog = !fin && d > 1.0 && (p >= 20.0 || pct >= 0.015);
                let entry = shows.entry(sid).or_insert((0, Value::Null, None));
                if t >= entry.0 { entry.0 = t; entry.1 = e.clone(); }
                if in_prog && entry.2.as_ref().map_or(true, |(pt, _)| t >= *pt) {
                    entry.2 = Some((t, e.clone()));
                }
            }
            continue;
        }

        if key.starts_with("cu:") {
            if !(d > 0.0 && p >= 20.0 && pct < 0.92) { continue; }
            if dismissed.get(key).map_or(false, |&dt| dt >= t) { continue; }
            let mut it = e["item"].clone();
            if !it.is_object() { it = json!({}); }
            let exp = it["exp"].as_i64().unwrap_or(0);
            if exp > 0 && now > exp { continue; }
            let is_pin = pinned.contains_key(key);
            it["progress"] = json!(pct);
            it["resume"] = json!(p);
            it["dur"] = json!(d);
            it["remainingSecs"] = json!((d - p).max(0.0).round());
            it["key"] = json!(key);
            it["cwKey"] = json!(key);
            it["pinned"] = json!(is_pin);
            cards.push((is_pin, t, it));
            continue;
        }

        // Movies ("m:<id>")
        let raw_mid = key.strip_prefix("m:").and_then(|x| x.parse::<i64>().ok())
            .or_else(|| e["item"]["id"].as_i64()).unwrap_or(0);
        let canon = if raw_mid > 0 { cat.movie(raw_mid) } else { None };
        let mid = canon.map(|c| c.id).unwrap_or(raw_mid);
        let tmdb = canon.and_then(|c| c.tmdb.clone()).unwrap_or_default();
        // Allow rewatches: only hide behind `state["watched"]` if watched timestamp >= progress timestamp.
        if !tmdb.is_empty() && !is_rewatch_allowed(watched_ts.get(&format!("movie:{tmdb}")).copied(), t) {
            continue;
        }
        let cw_key = format!("m:{mid}");
        if dismissed.get(key).map_or(false, |&dt| dt >= t) || dismissed.get(&cw_key).map_or(false, |&dt| dt >= t) { continue; }
        if is_finished(p, d, None) || !(d > 0.0 && (p >= 30.0 || pct >= 0.02)) { continue; }
        let mut it = e["item"].clone();
        if !it.is_object() { it = json!({}); }
        if let Some(c) = canon {
            if s(&it["title"]).is_empty() { it["title"] = json!(c.title); }
            if s(&it["poster"]).is_empty() { it["poster"] = json!(c.poster); }
            if s(&it["backdrop"]).is_empty() { it["backdrop"] = json!(c.backdrop); }
            it["id"] = json!(c.id);
            it["tmdb"] = json!(c.tmdb);
            it["isAnime"] = json!(crate::anime::is_anime_item(app, c));
        }
        let is_pin = pinned.contains_key(&cw_key) || pinned.contains_key(key);
        it["k"] = json!("m");
        it["progress"] = json!(pct);
        it["resume"] = json!(p);
        it["dur"] = json!(d);
        it["remainingSecs"] = json!((d - p).max(0.0).round());
        it["key"] = json!(key);
        it["cwKey"] = json!(cw_key);
        it["pinned"] = json!(is_pin);
        let dedupe_key = if !tmdb.is_empty() { format!("tmdb:{tmdb}") } else { cw_key };
        if movie_cards.get(&dedupe_key).map_or(true, |(pt, _)| t > *pt) {
            movie_cards.insert(dedupe_key, (t, it));
        }
    }

    for (_, (t, it)) in movie_cards {
        let is_pin = it["pinned"].as_bool().unwrap_or(false);
        cards.push((is_pin, t, it));
    }

    // Group series: one card per show, preferring active in-progress episode or next unwatched episode
    let mut need_next: Vec<(i64, i64, Value)> = vec![];
    for (sid, (max_t, latest_e, in_prog)) in shows {
        let cw_key = format!("s:{sid}");
        if dismissed.get(&cw_key).map_or(false, |&dt| dt >= max_t) { continue; }
        let series_it = cat.series_item(sid);
        if let Some(sit) = series_it {
            if let Some(tm) = &sit.tmdb {
                if !is_rewatch_allowed(watched_ts.get(&format!("tv:{tm}")).copied(), max_t) {
                    continue;
                }
            }
        }
        let is_pin = pinned.contains_key(&cw_key);
        let ld = latest_e["dur"].as_f64().unwrap_or(0.0);
        let lp = latest_e["pos"].as_f64().unwrap_or(0.0);
        let l_eid = latest_e["key"].as_str().and_then(|k| k.strip_prefix("e:")).and_then(|x| x.parse::<i64>().ok())
            .or_else(|| latest_e["item"]["id"].as_i64()).unwrap_or(0);
        let latest_fin = is_finished(lp, ld, Some(l_eid));
        let latest_in_prog = !latest_fin && ld > 1.0 && (lp >= 20.0 || lp / ld >= 0.015);

        if latest_in_prog {
            let mut it = latest_e["item"].clone();
            if !it.is_object() { it = json!({}); }
            if let Some(sit) = series_it {
                if s(&it["title"]).is_empty() { it["title"] = json!(sit.title); }
                if s(&it["poster"]).is_empty() { it["poster"] = json!(sit.poster); }
                if s(&it["backdrop"]).is_empty() { it["backdrop"] = json!(sit.backdrop); }
                it["tmdb"] = json!(sit.tmdb);
                it["isAnime"] = json!(crate::anime::is_anime_item(app, sit));
            }
            it["k"] = json!("e");
            it["sid"] = json!(sid);
            it["progress"] = json!(lp / ld);
            it["resume"] = json!(lp);
            it["dur"] = json!(ld);
            it["remainingSecs"] = json!((ld - lp).max(0.0).round());
            it["key"] = latest_e["key"].clone();
            it["cwKey"] = json!(cw_key);
            it["pinned"] = json!(is_pin);
            cards.push((is_pin, max_t, it));
        } else if latest_fin && (is_pin || now - max_t < 120 * 86_400_000) {
            need_next.push((max_t, sid, latest_e));
        } else if let Some((pt, pe)) = in_prog {
            let pd = pe["dur"].as_f64().unwrap_or(1.0);
            let pp = pe["pos"].as_f64().unwrap_or(0.0);
            let mut it = pe["item"].clone();
            if !it.is_object() { it = json!({}); }
            if let Some(sit) = series_it {
                it["tmdb"] = json!(sit.tmdb);
                it["isAnime"] = json!(crate::anime::is_anime_item(app, sit));
            }
            it["k"] = json!("e");
            it["sid"] = json!(sid);
            it["progress"] = json!(pp / pd);
            it["resume"] = json!(pp);
            it["dur"] = json!(pd);
            it["remainingSecs"] = json!((pd - pp).max(0.0).round());
            it["key"] = pe["key"].clone();
            it["cwKey"] = json!(cw_key);
            it["pinned"] = json!(is_pin);
            cards.push((is_pin, pt, it));
        } else if !latest_fin && ld > 1.0 && lp >= 5.0 {
            need_next.push((max_t, sid, latest_e));
        }
    }

    // Resolve next unwatched episode for up to 15 most recent shows, merging episodes across primary + variants!
    need_next.sort_by_key(|x| -x.0);
    need_next.truncate(15);
    let show_specs: Vec<Vec<(i64, bool)>> = need_next.iter().map(|(_, sid, _)| {
        let mut specs = vec![(*sid, false)];
        if let Some(sit) = cat.series_item(*sid) {
            if sit.id != *sid { specs.insert(0, (sit.id, false)); }
            let mut extras: Vec<(i64, bool, bool, bool, bool)> = sit.variants.iter()
                .filter(|v| !v.cam && !specs.iter().any(|x| x.0 == v.id))
                .map(|v| (v.id, v.sub_market, v.dv, v.uhd, v.dolby_audio))
                .collect();
            extras.sort_by_key(|x| (x.1, !x.2, !x.3, !x.4));
            extras.truncate(4);
            for (vid, sub, ..) in extras { specs.push((vid, sub)); }
        }
        specs
    }).collect();

    let all_infos = futures_util::future::join_all(show_specs.iter().map(|specs| {
        let app = app.clone();
        async move {
            futures_util::future::join_all(
                specs.iter().map(|(vsid, _)| app.xc_info("get_series_info", "series_id", *vsid, 2 * 3_600_000))
            ).await
        }
    })).await;

    for (((t, sid, e), specs), infos) in need_next.into_iter().zip(show_specs).zip(all_infos) {
        // Merge canonical episodes across native EN variants; also map every variant's eid -> (sn, en) so
        // watching on a 4K/Nordic variant resolves cleanly to the next episode!
        let mut merged_en: BTreeMap<(i64, i64), (i64, String, String, i64)> = BTreeMap::new();
        let mut merged_sub: BTreeMap<(i64, i64), (i64, String, String, i64)> = BTreeMap::new();
        let mut eid_to_se: HashMap<i64, (i64, i64)> = HashMap::new();
        let mut watched_se: HashSet<(i64, i64)> = HashSet::new();
        for ((_, is_sub), info) in specs.iter().zip(infos.iter()) {
            let Some(m) = info["episodes"].as_object() else { continue };
            for (sk, list) in m {
                let Ok(sn) = sk.parse::<i64>() else { continue };
                for ep in list.as_array().into_iter().flatten() {
                    let eid = crate::util::i(&ep["id"]);
                    if eid <= 0 { continue; }
                    let en = crate::util::i(&ep["episode_num"]);
                    let etitle = crate::re!(r"(?i)^.*?S\d+E\d+\s*-?\s*").replace(&s(&ep["title"]), "").to_string();
                    let still = s(&ep["info"]["movie_image"]);
                    let dur_s = crate::util::i(&ep["info"]["duration_secs"]);
                    eid_to_se.insert(eid, (sn, en));
                    if watched_eps.contains(&eid) { watched_se.insert((sn, en)); }
                    let target = if *is_sub { &mut merged_sub } else { &mut merged_en };
                    target.entry((sn, en)).or_insert((eid, etitle, still, dur_s));
                }
            }
        }
        for (se, val) in merged_sub {
            merged_en.entry(se).or_insert(val);
        }
        if merged_en.is_empty() { continue; }

        let last_eid = e["key"].as_str().and_then(|k| k.strip_prefix("e:")).and_then(|x| x.parse::<i64>().ok())
            .or_else(|| e["item"]["id"].as_i64()).unwrap_or(0);
        let last_se = eid_to_se.get(&last_eid).copied().or_else(|| {
            let sn = e["item"]["season"].as_i64();
            let en = e["item"]["epNum"].as_i64();
            match (sn, en) { (Some(a), Some(b)) => Some((a, b)), _ => None }
        });
        let watching_specials = last_se.map_or(false, |(sn, _)| sn == 0);

        // Exclude Season 0 specials unless the user is actively watching Season 0!
        let flat: Vec<(i64, i64, i64, String, String, i64)> = merged_en
            .into_iter()
            .filter(|((sn, _), _)| watching_specials || *sn > 0)
            .map(|((sn, en), (eid, etitle, still, dur_s))| (sn, en, eid, etitle, still, dur_s))
            .collect();
        if flat.is_empty() { continue; }

        let total_eps = flat.len();
        let watched_count = flat.iter().filter(|x| watched_eps.contains(&x.2) || watched_se.contains(&(x.0, x.1))).count();
        let series_it = cat.series_item(sid);
        let is_an = series_it.map_or(false, |it| crate::anime::is_anime_item(app, it));
        let skip_filler = is_an && app.state.read(|st| st["prefs"]["skipFiller"].as_bool().unwrap_or(false));
        let show_filler = if skip_filler { series_it.and_then(|it| crate::filler::builtin_lookup(&it.title)) } else { None };

        let last_pos = flat.iter().position(|x| x.2 == last_eid)
            .or_else(|| last_se.and_then(|se| flat.iter().position(|x| (x.0, x.1) == se)));
        let last_done = watched_eps.contains(&last_eid) || last_se.map_or(false, |se| watched_se.contains(&se));
        let start_ix = last_pos.map(|ix| if last_done { ix + 1 } else { ix }).unwrap_or(0);
        if watched_count == 0 && start_ix == 0 { continue; }

        let mut abs_nums = vec![0i64; flat.len()];
        let (mut prev_sn, mut season_off, mut season_max) = (-1i64, 0i64, 0i64);
        for (i, x) in flat.iter().enumerate() {
            if x.0 <= 0 { continue; }
            if x.0 != prev_sn {
                let max_abs = season_max;
                season_off = if max_abs > 0 && x.1 > 0 && x.1 <= max_abs { max_abs } else { 0 };
                prev_sn = x.0;
            }
            let a = if x.1 > 0 { season_off + x.1 } else { season_max + 1 };
            if a > season_max { season_max = a; }
            abs_nums[i] = a;
        }
        let Some((_, nx)) = flat.iter().enumerate().skip(start_ix).find(|(i, x)| {
            !watched_eps.contains(&x.2)
                && !watched_se.contains(&(x.0, x.1))
                && (!is_an || crate::anime::ep_dub_status(app, x.2, None, "") != "no")
                && show_filler.as_ref().map_or(true, |sf| abs_nums[*i] == 0 || !sf.is_filler(abs_nums[*i]))
        }) else { continue };
        let cw_key = format!("s:{sid}");
        let is_pin = pinned.contains_key(&cw_key);
        let title = { let x = s(&e["item"]["title"]); if !x.is_empty() { x } else { series_it.map(|i| i.title.clone()).unwrap_or_default() } };
        let poster = { let x = s(&e["item"]["poster"]); if !x.is_empty() { x } else { series_it.map(|i| i.poster.clone()).unwrap_or_default() } };
        let backdrop = { if !nx.4.is_empty() { nx.4.clone() } else { let x = s(&e["item"]["backdrop"]); if !x.is_empty() { x } else { series_it.map(|i| i.backdrop.clone()).unwrap_or_default() } } };
        let sub = if nx.3.is_empty() { format!("S{}:E{}", nx.0, nx.1) } else { format!("S{}:E{} · {}", nx.0, nx.1, nx.3) };
        cards.push((is_pin, t, json!({
            "k": "e", "id": nx.2, "sid": sid, "title": title, "sub": sub,
            "season": nx.0, "epNum": nx.1, "epTitle": nx.3,
            "watchedEps": watched_count, "totalEps": total_eps,
            "poster": poster, "backdrop": backdrop, "tmdb": series_it.and_then(|i| i.tmdb.clone()),
            "isAnime": is_an, "progress": 0.0, "resume": 0.0, "dur": nx.5, "nextUp": true,
            "key": format!("e:{}", nx.2), "cwKey": cw_key, "pinned": is_pin
        })));
    }

    cards.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    cards.into_iter().take(24).map(|x| x.2).collect()
}

/// Seeds for personal rows: liked first, then history, then recent watched. (mt, tmdb, title, weight, liked)
pub fn seeds(app: &App) -> Vec<(String, String, String, f64, bool)> {
    app.state.read(|st| {
        let mut out = vec![]; let mut seen = HashSet::new();
        let mut likes: Vec<(&String, &Value)> = st["ratings"].as_object().map(|m| m.iter().filter(|(_, v)| v["v"].as_i64().unwrap_or(0) > 0).collect()).unwrap_or_default();
        likes.sort_by(|a, b| b.1["t"].as_i64().cmp(&a.1["t"].as_i64()));
        for (k, v) in likes { if let Some((mt, id)) = k.split_once(':') { if seen.insert(k.clone()) { out.push((mt.into(), id.into(), s(&v["title"]), 2.0, true)); } } }
        for h in st["history"].as_array().into_iter().flatten() {
            let id = s(&h["tmdb"]); if id.is_empty() { continue; }
            let mt = if h["kind"] == "s" { "tv" } else { "movie" }; let k = format!("{mt}:{id}");
            if seen.insert(k) { out.push((mt.into(), id, s(&h["title"]), 1.0, false)); }
        }
        let mut wat: Vec<(&String, &Value)> = st["watched"].as_object().map(|m| m.iter().collect()).unwrap_or_default();
        wat.sort_by(|a, b| b.1["t"].as_i64().cmp(&a.1["t"].as_i64()));
        for (k, v) in wat.into_iter().take(12) {
            if let Some((mt, id)) = k.split_once(':') {
                let title = s(&v["title"]);
                if !title.is_empty() && seen.insert(k.clone()) {
                    out.push((mt.into(), id.into(), title, 0.8, false));
                }
            }
        }
        out
    })
}

/// Set of all TMDB keys (`movie:<id>` / `tv:<id>`) the user has rated, watched (incl. Trakt/Letterboxd),
/// has in history, or completed via playback progress.
pub fn seen_set(app: &App) -> HashSet<String> {
    let cat = app.catalog();
    app.state.read(|st| {
        let mut set: HashSet<String> = st["ratings"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        if let Some(w) = st["watched"].as_object() {
            for k in w.keys() { set.insert(k.clone()); }
        }
        for h in st["history"].as_array().into_iter().flatten() {
            let id = s(&h["tmdb"]);
            if !id.is_empty() { set.insert(format!("{}:{id}", if h["kind"] == "s" { "tv" } else { "movie" })); }
        }
        for (key, p) in st["progress"].as_object().into_iter().flatten() {
            let is_movie = key.starts_with("m:") || p["item"]["k"] == "m";
            if !is_movie { continue; }
            let d = p["dur"].as_f64().unwrap_or(0.0);
            let pos = p["pos"].as_f64().unwrap_or(0.0);
            let completed = d > 0.0 && (pos / d >= 0.90 || (d >= 5400.0 && (d - pos) <= 540.0 && pos / d >= 0.87));
            if !completed { continue; }
            let direct_tm = {
                let a = s(&p["tmdb"]);
                if !a.is_empty() { a } else { s(&p["item"]["tmdb"]) }
            };
            if !direct_tm.is_empty() {
                set.insert(format!("movie:{direct_tm}"));
            } else if let Some(mid) = key.strip_prefix("m:").and_then(|x| x.parse::<i64>().ok()).or_else(|| p["item"]["id"].as_i64()) {
                if let Some(t) = cat.movie(mid).and_then(|it| it.tmdb.as_deref()) {
                    set.insert(format!("movie:{t}"));
                }
            }
        }
        set
    })
}

/// Offline local Taste Vector built purely from local `meta.json` (canonical cross-media genres,
/// keywords, directors/creators, cast, release-era preference, and dislike penalties).
#[derive(Default)]
struct TasteVector {
    gw_movie: HashMap<i64, f64>,
    gw_tv: HashMap<i64, f64>,
    kw_w: HashMap<i64, f64>,
    dir_w: HashMap<String, f64>,
    cast_w: HashMap<String, f64>,
    era_sum: f64,
    era_w: f64,
}

impl TasteVector {
    fn ingest_meta(&mut self, m: &Meta, w: f64) {
        for &g in &m.g {
            add_cross_genre_weight(&mut self.gw_movie, &mut self.gw_tv, g, w);
        }
        for &kw in &m.kw {
            *self.kw_w.entry(kw).or_default() += w;
        }
        for d in &m.dir {
            let k = d.to_lowercase();
            if !k.is_empty() { *self.dir_w.entry(k).or_default() += w; }
        }
        for c in &m.cast {
            let k = c.to_lowercase();
            if !k.is_empty() { *self.cast_w.entry(k).or_default() += w; }
        }
        if w > 0.0 {
            let yr = m.year();
            if yr >= 1950 {
                self.era_sum += (yr as f64) * w;
                self.era_w += w;
            }
        }
    }

    fn score_meta(&self, is_tv: bool, m: &Meta, cur_year: i32) -> f64 {
        let gw = if is_tv { &self.gw_tv } else { &self.gw_movie };
        let g_tot = gw.values().map(|x| x.abs()).sum::<f64>().max(1.0);
        let g_sc: f64 = m.g.iter().map(|id| gw.get(id).copied().unwrap_or(0.0)).sum::<f64>() / g_tot;

        let kw_tot = self.kw_w.values().map(|x| x.abs()).sum::<f64>().max(4.0);
        let kw_raw: f64 = m.kw.iter().map(|k| self.kw_w.get(k).copied().unwrap_or(0.0)).sum::<f64>();
        let kw_sc = (kw_raw / kw_tot).clamp(-1.5, 2.0);

        let dir_sc: f64 = m.dir.iter()
            .map(|d| self.dir_w.get(&d.to_lowercase()).copied().unwrap_or(0.0))
            .sum::<f64>()
            .clamp(-2.5, 3.0);

        let cast_sc: f64 = m.cast.iter()
            .map(|c| self.cast_w.get(&c.to_lowercase()).copied().unwrap_or(0.0))
            .sum::<f64>()
            .clamp(-1.5, 2.0);

        let yr = m.year();
        let era_sc = if self.era_w > 0.0 && yr >= 1950 {
            let pref_yr = self.era_sum / self.era_w;
            let diff = ((yr as f64) - pref_yr).abs();
            (1.0 - diff / 25.0).clamp(-0.3, 0.6)
        } else if yr >= cur_year - 2 {
            0.35
        } else {
            0.0
        };

        let qual = (m.bayes() - 6.4) * 0.9 + (m.p.max(0.0) + 1.0).log10() * 0.55;
        g_sc * 4.2 + kw_sc * 2.2 + dir_sc * 0.9 + cast_sc * 0.45 + era_sc + qual
    }
}

fn build_local_taste(app: &App) -> (TasteVector, Vec<(String, String, f64)>) {
    let pos_seeds: Vec<_> = seeds(app).into_iter().take(18).collect();
    let dislikes: Vec<(String, String)> = app.state.read(|st| {
        st["ratings"].as_object()
            .map(|m| m.iter().filter(|(_, v)| v["v"].as_i64().unwrap_or(0) < 0).take(16)
                .filter_map(|(k, _)| k.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())))
                .collect())
            .unwrap_or_default()
    });
    let mut tv = TasteVector::default();
    let mut missing_local: Vec<(String, String, f64)> = vec![];
    let db = app.meta.read().unwrap();
    for (mt, id, _, w, _) in &pos_seeds {
        match db.get(&format!("{mt}:{id}")).filter(|m| !m.missing) {
            Some(m) => tv.ingest_meta(m, *w),
            None => missing_local.push((mt.clone(), id.clone(), *w)),
        }
    }
    for (mt, id) in &dislikes {
        match db.get(&format!("{mt}:{id}")).filter(|m| !m.missing) {
            Some(m) => tv.ingest_meta(m, -2.2),
            None => missing_local.push((mt.clone(), id.clone(), -2.2)),
        }
    }
    (tv, missing_local)
}

/// Pure offline local Taste Vector recommendations (<5ms, zero TMDB network latency).
pub fn taste_for_you(app: &App, limit: usize) -> Vec<Value> {
    let (tv, _) = build_local_taste(app);
    if tv.gw_movie.is_empty() && tv.gw_tv.is_empty() && tv.kw_w.is_empty() && tv.dir_w.is_empty() && tv.cast_w.is_empty() {
        return vec![];
    }
    score_local_catalog(app, &tv, limit)
}

fn score_local_catalog(app: &App, tv: &TasteVector, limit: usize) -> Vec<Value> {
    let cat = app.catalog();
    let seen = seen_set(app);
    let hide = hidden_ratings(app);
    let prefs = crate::discover::hide_prefs(app);
    let hide_cams = crate::discover::hide_cams_enabled(app);
    let su = crate::discover::standup_kw(app);
    let cy = year();
    let db = app.meta.read().unwrap();

    let mut scored: Vec<(f64, &Item)> = vec![];
    for (is_tv, mt, pool) in [(false, "movie", cat.movies.as_slice()), (true, "tv", cat.series.as_slice())] {
        let min_vc = if is_tv { 35 } else { 80 };
        for it in pool.iter().filter(|x| x.en && !x.sport && x.tmdb.is_some()) {
            let t = it.tmdb.as_ref().unwrap();
            let key = format!("{mt}:{t}");
            if seen.contains(&key) || hide.contains(&key) { continue; }
            if hide_cams && crate::showcase::is_cam_only(app, it) { continue; }
            let Some(m) = db.get(&key).filter(|m| !m.missing) else { continue };
            if m.vc < min_vc || m.va < 6.0 { continue; }
            if !prefs.is_empty() && crate::discover::hidden_by(&prefs, &m.g, &m.l, &cat.cat_name(if is_tv { "series" } else { "movie" }, &it.cat), &m.kw, &su) {
                continue;
            }
            let sc = tv.score_meta(is_tv, m, cy);
            scored.push((sc, it));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let top: Vec<&Item> = scored.into_iter().take(limit).map(|x| x.1).collect();
    drop(db);
    top.into_iter().map(|it| app.card_meta(it)).collect()
}

pub async fn because_you_watched(app: &Arc<App>) -> Vec<Value> {
    if !app.tmdb.enabled() { return vec![]; }
    let top_seeds: Vec<_> = seeds(app).into_iter().filter(|x| !x.2.is_empty()).take(3).collect();
    let futs = top_seeds.iter().map(|(mt, id, ..)| {
        let ep = format!("/{mt}/{id}/recommendations");
        let app = app.clone();
        async move { app.tmdb.get(&ep, crate::tmdb::NO, TTL_D).await }
    });
    let results = futures_util::future::join_all(futs).await;
    let mut rows = vec![];
    for ((mt, id, title, _, liked), r) in top_seeds.into_iter().zip(results) {
        let items = match_results(app, r.as_ref().and_then(|r| r["results"].as_array()).map(|a| a.as_slice()).unwrap_or(&[]), Some(&mt), 30);
        if items.len() >= 4 {
            rows.push(json!({ "id": format!("byw-{mt}{id}"), "title": format!("Because you {} {title}", if liked { "liked" } else { "watched" }), "items": items }));
        }
    }
    rows
}

/// Taste profile: cross-media genre affinity + local Taste Vector (keywords, directors, cast, era, dislikes)
/// + parallelized TMDB recommendations/trends/discover + quality + popularity.
pub async fn for_you(app: Arc<App>) -> Value {
    let seed_list: Vec<_> = seeds(&app).into_iter().take(12).collect();
    if seed_list.is_empty() { return json!([]); }

    let (mut tv, missing_local) = build_local_taste(&app);
    // Offline fallback: works instantaneously even with no TMDB key / offline!
    if !app.tmdb.enabled() {
        return Value::Array(score_local_catalog(&app, &tv, 40));
    }

    // Only fetch TMDB details for seeds not already indexed in local meta.json
    if !missing_local.is_empty() {
        let details = futures_util::future::join_all(missing_local.iter().map(|(mt, id, _)| {
            let ep = format!("/{mt}/{id}");
            let app = app.clone();
            async move { app.tmdb.get(&ep, crate::tmdb::NO, 7 * TTL_D).await }
        })).await;
        for ((_, _, w), d) in missing_local.iter().zip(details) {
            if let Some(d) = d {
                for g in d["genres"].as_array().into_iter().flatten() {
                    if let Some(id) = g["id"].as_i64() {
                        add_cross_genre_weight(&mut tv.gw_movie, &mut tv.gw_tv, id, *w);
                    }
                }
            }
        }
    }

    let mut top_movie_g: Vec<(i64, f64)> = tv.gw_movie.iter().filter(|x| *x.1 > 0.0).map(|(&a, &b)| (a, b)).collect();
    top_movie_g.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut top_tv_g: Vec<(i64, f64)> = tv.gw_tv.iter().filter(|x| *x.1 > 0.0).map(|(&a, &b)| (a, b)).collect();
    top_tv_g.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Parallelize ALL independent TMDB calls (recommendations + trending + cross-media movie/TV discover)
    let rec_futs = futures_util::future::join_all(seed_list.iter().take(8).map(|(mt, id, _, w, _)| {
        let ep = format!("/{mt}/{id}/recommendations");
        let mt = mt.clone();
        let w = *w;
        let app = app.clone();
        async move { (mt, w, app.tmdb.get(&ep, crate::tmdb::NO, TTL_D).await) }
    }));
    let disc_mv_futs = futures_util::future::join_all(top_movie_g.iter().take(3).map(|(g, _)| {
        let gs = g.to_string();
        let app = app.clone();
        async move { app.tmdb.pages("/discover/movie", 2, [sp("with_genres", &gs), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "200")], 12 * TTL_H).await }
    }));
    let disc_tv_futs = futures_util::future::join_all(top_tv_g.iter().take(3).map(|(g, _)| {
        let gs = g.to_string();
        let app = app.clone();
        async move { app.tmdb.pages("/discover/tv", 2, [sp("with_genres", &gs), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "100")], 12 * TTL_H).await }
    }));
    let (rec_res, trend_mv, trend_tv, disc_mv, disc_tv) = tokio::join!(
        rec_futs,
        app.tmdb.pages("/trending/movie/week", 3, crate::tmdb::NO, 3 * TTL_H),
        app.tmdb.pages("/trending/tv/week", 3, crate::tmdb::NO, 3 * TTL_H),
        disc_mv_futs,
        disc_tv_futs
    );

    let mut cand: HashMap<String, (Value, String, f64)> = HashMap::new();
    let add = |cand: &mut HashMap<String, (Value, String, f64)>, list: &[Value], mt: &str, boost: f64| {
        for r in list {
            let m = r["media_type"].as_str().unwrap_or(mt).to_string();
            if m != "movie" && m != "tv" { continue; }
            let k = format!("{m}:{}", r["id"]);
            let e = cand.entry(k).or_insert((r.clone(), m, 0.0));
            e.2 += boost;
        }
    };
    for (mt, w, r) in rec_res {
        add(&mut cand, r.as_ref().and_then(|r| r["results"].as_array()).map(|a| a.as_slice()).unwrap_or(&[]), &mt, w);
    }
    add(&mut cand, &trend_mv, "movie", 0.0);
    add(&mut cand, &trend_tv, "tv", 0.0);
    for list in disc_mv { add(&mut cand, &list, "movie", 0.0); }
    for list in disc_tv { add(&mut cand, &list, "tv", 0.0); }

    let seen = seen_set(&app);
    let mv_tot: f64 = tv.gw_movie.values().map(|x| x.abs()).sum::<f64>().max(1.0);
    let tv_tot: f64 = tv.gw_tv.values().map(|x| x.abs()).sum::<f64>().max(1.0);
    let y = year();
    let db = app.meta.read().unwrap();
    let mut scored: Vec<(f64, Value)> = cand.into_iter().filter(|(k, _)| !seen.contains(k)).map(|(k, (r, mt, recs))| {
        let is_tv = mt == "tv";
        let (gw, tot) = if is_tv { (&tv.gw_tv, tv_tot) } else { (&tv.gw_movie, mv_tot) };
        let g: f64 = r["genre_ids"].as_array().into_iter().flatten().filter_map(|x| x.as_i64())
            .map(|id| gw.get(&id).copied().unwrap_or(0.0)).sum::<f64>() / tot;
        let d = r["release_date"].as_str().filter(|x| !x.is_empty()).or(r["first_air_date"].as_str()).unwrap_or("0");
        let yr: i32 = d.get(0..4).and_then(|x| x.parse().ok()).unwrap_or(0);
        // Enrich TMDB candidate with local Taste Vector signals (keywords, directors, cast, era) when indexed!
        let local_boost = db.get(&k).filter(|m| !m.missing).map(|m| {
            let kw_tot = tv.kw_w.values().map(|x| x.abs()).sum::<f64>().max(4.0);
            let kw_s = (m.kw.iter().map(|id| tv.kw_w.get(id).copied().unwrap_or(0.0)).sum::<f64>() / kw_tot).clamp(-1.2, 1.8);
            let dir_s = m.dir.iter().map(|n| tv.dir_w.get(&n.to_lowercase()).copied().unwrap_or(0.0)).sum::<f64>().clamp(-2.0, 2.5);
            let cast_s = m.cast.iter().map(|n| tv.cast_w.get(&n.to_lowercase()).copied().unwrap_or(0.0)).sum::<f64>().clamp(-1.0, 1.5);
            kw_s * 1.6 + dir_s * 0.8 + cast_s * 0.4
        }).unwrap_or(0.0);
        let sc = g * 4.0 + recs * 1.5 + local_boost + (r["popularity"].as_f64().unwrap_or(1.0).max(0.0) + 1.0).log10() * 0.8 + (r["vote_average"].as_f64().unwrap_or(6.0) - 6.5) * 0.6 + if yr >= y - 1 { 0.8 } else { 0.0 };
        let mut r = r; r["media_type"] = json!(mt); (sc, r)
    }).collect();
    drop(db);
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let list: Vec<Value> = scored.into_iter().map(|x| x.1).collect();
    let mut out = match_results(&app, &list, None, 34);

    // Blend in top local Taste Vector catalog discoveries not already returned by TMDB
    let local_extra = score_local_catalog(&app, &tv, 16);
    let mut have: HashSet<String> = out.iter().map(|c| format!("{}{}", s(&c["k"]), s(&c["id"]))).collect();
    for c in local_extra {
        if out.len() >= 40 { break; }
        let ck = format!("{}{}", s(&c["k"]), s(&c["id"]));
        if have.insert(ck) { out.push(c); }
    }
    Value::Array(out)
}

pub async fn au_providers(app: &Arc<App>, mt: &str) -> Vec<Value> {
    let prov = app.tmdb.get(format!("/watch/providers/{mt}"), [sp("watch_region", "AU")], 7 * TTL_D).await;
    let want: [(&str, &regex::Regex); 7] = [("Netflix", crate::re!(r"(?i)^netflix$")), ("Stan", crate::re!(r"(?i)^stan$")), ("Binge", crate::re!(r"(?i)^binge$")), ("Prime Video", crate::re!(r"(?i)^amazon prime video$")), ("Disney+", crate::re!(r"(?i)^disney plus$")), ("Apple TV+", crate::re!(r"(?i)^apple tv(\+| plus)?$")), ("Paramount+", crate::re!(r"(?i)^paramount plus$"))];
    let matched: Vec<(&str, String)> = want.iter().filter_map(|(label, re)| {
        let p = prov.as_ref().and_then(|p| p["results"].as_array()).and_then(|a| a.iter().find(|x| re.is_match(x["provider_name"].as_str().unwrap_or(""))))?;
        Some((*label, p["provider_id"].to_string()))
    }).collect();
    let futs = matched.iter().map(|(_, pid)| {
        let ep = format!("/discover/{mt}");
        let pid = pid.clone();
        let app = app.clone();
        async move {
            app.tmdb.pages(&ep, 3, [sp("watch_region", "AU"), sp("with_watch_providers", &pid), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "20")], 12 * TTL_H).await
        }
    });
    let results = futures_util::future::join_all(futs).await;
    let mut rows = vec![];
    for ((label, pid), res) in matched.into_iter().zip(results) {
        let items = match_results(app, &res, Some(mt), 40);
        if items.len() >= 5 { rows.push(json!({ "id": format!("au-{mt}-{pid}"), "title": format!("Popular on {label}"), "sub": "Australia", "items": items })); }
    }
    rows
}

pub fn new_episodes(app: &App) -> Vec<Value> {
    let cat = app.catalog();
    app.state.read(|st| {
        let mut v: Vec<(&String, &Value)> = st["newEps"].as_object().map(|m| m.iter().collect()).unwrap_or_default();
        v.sort_by(|a, b| b.1["t"].as_i64().cmp(&a.1["t"].as_i64()));
        v.into_iter().map(|(sid, n)| {
            let parsed_id = sid.parse::<i64>().unwrap_or(0);
            let it = (parsed_id > 0).then(|| cat.series_item(parsed_id)).flatten();
            let ids = n["ids"].as_array().map(|a| a.len()).unwrap_or(0);
            json!({
                "k": "s",
                "id": it.map(|i| i.id).unwrap_or(parsed_id),
                "title": if s(&n["title"]).is_empty() { it.map(|i| i.title.clone()).unwrap_or_default() } else { s(&n["title"]) },
                "poster": it.map(|i| i.poster.clone()).filter(|x| !x.is_empty()).unwrap_or_else(|| s(&n["poster"])),
                "backdrop": it.map(|i| i.backdrop.clone()).filter(|x| !x.is_empty()).unwrap_or_else(|| s(&n["backdrop"])),
                "sub": if ids > 1 { format!("{ids} new episodes") } else { format!("New: S{}:E{}", n["latest"]["s"], n["latest"]["e"]) },
                "badge": "NEW"
            })
        }).collect()
    })
}

pub async fn hero(app: Arc<App>) -> Value {
    let mut items: Vec<Value> = vec![];
    if app.tmdb.enabled() {
        items = match_results(&app, &app.tmdb.pages("/trending/all/day", 2, crate::tmdb::NO, 3 * TTL_H).await, None, 12).into_iter()
            .filter(|x| !s(&x["backdrop"]).is_empty() && !s(&x["overview"]).is_empty()).collect();
    }
    if items.len() < 3 { items = buzz(&app, true, 40).into_iter().filter(|x| !s(&x["backdrop"]).is_empty()).take(8).collect(); }
    Value::Array(items.into_iter().take(6).collect())
}

/// The shared (non-personal) part of Home. Pre-warmed in the background.
pub async fn home_shared(app: Arc<App>) -> Value {
    let mut r = vec![];
    if app.tmdb.enabled() {
        let t = &app.tmdb;
        let (top, tm, tt, np, ptv, pm, tr) = tokio::join!(t.pages("/trending/all/day", 3, crate::tmdb::NO, 3 * TTL_H), t.pages("/trending/movie/week", 5, crate::tmdb::NO, 3 * TTL_H), t.pages("/trending/tv/week", 5, crate::tmdb::NO, 3 * TTL_H),
            t.pages("/movie/now_playing", 4, [sp("region", "AU")], 3 * TTL_H), t.pages("/tv/popular", 5, crate::tmdb::NO, 3 * TTL_H), t.pages("/movie/popular", 5, [sp("region", "AU")], 3 * TTL_H), t.pages("/movie/top_rated", 3, crate::tmdb::NO, 3 * TTL_H));
        let top10 = match_results(&app, &top, None, 10);
        if top10.len() >= 5 { r.push(json!({ "id": "top10", "title": "Top 10 Today", "items": top10, "style": "top10" })); }
        r.push(row("trend-movies", "Trending Movies", match_results(&app, &tm, Some("movie"), 40)));
        r.push(row("trend-tv", "Trending Series", match_results(&app, &tt, Some("tv"), 40)));
        r.push(json!({ "id": "sports-slot" }));
        r.push(row("in-cinemas", "Fresh From Cinemas", match_results(&app, &np, Some("movie"), 40)));
        r.push(row("popular-tv", "Popular Series", match_results(&app, &ptv, Some("tv"), 40)));
        r.push(row("popular-movies", "Popular Movies", match_results(&app, &pm, Some("movie"), 40)));
        r.push(row("acclaimed", "Critically Acclaimed", match_results(&app, &tr, Some("movie"), 40)));
    } else {
        r.push(row("buzz-movies", "New & Highly Rated Movies", buzz(&app, false, 40)));
        r.push(row("buzz-tv", "New & Highly Rated Series", buzz(&app, true, 40)));
        r.push(json!({ "id": "sports-slot" }));
        r.push(row("imdb", "IMDb Top 250", from_category(&app, false, crate::re!(r"(?i)IMDB TOP 250"), 40, true)));
    }
    r.push(row("new-movies", "Recently Added Movies", recent(&app, false, 40)));
    r.push(row("new-series", "Recently Updated Series", recent(&app, true, 40)));
    Value::Array(r)
}

pub async fn home(app: &Arc<App>) -> Value {
    let mut out: Vec<Value> = vec![];
    let cw = continue_watching(app).await;
    if !cw.is_empty() { out.push(json!({ "id": "continue", "title": "Continue Watching", "items": cw, "style": "continue" })); }
    let ne = new_episodes(app);
    if !ne.is_empty() { out.push(json!({ "id": "new-eps", "title": "New Episodes", "sub": "From shows you follow", "items": ne, "style": "wide" })); }
    let arrived: Vec<Value> = app.state.read(|st| { let mut v: Vec<Value> = st["arrived"].as_object().map(|m| m.values().cloned().collect()).unwrap_or_default(); v.sort_by(|a, b| b["t"].as_i64().cmp(&a["t"].as_i64())); v });
    if !arrived.is_empty() { out.push(json!({ "id": "arrived", "title": "Now Available", "sub": "From your Wanted list", "items": arrived })); }
    let my_list = my_list_cards(app);
    let (shared, byw, fy, au) = tokio::join!(
        app.memo("home-shared", 30 * 60_000, home_shared),
        because_you_watched(app),
        app.memo("foryou", 20 * 60_000, for_you),
        async { if app.tmdb.enabled() { app.memo("au-tv", 3 * 3_600_000, |a| async move { Value::Array(au_providers(&a, "tv").await) }).await } else { Arc::new(json!([])) } });
    let fy = fy.as_array().cloned().unwrap_or_default(); let au = au.as_array().cloned().unwrap_or_default();
    let mut fy_added = false;
    for r in shared.as_array().into_iter().flatten() {
        out.push(r.clone());
        let id = r["id"].as_str().unwrap_or("");
        if (id == "top10" || id == "buzz-movies") && !fy_added && fy.len() >= 6 { out.push(json!({ "id": "foryou", "title": "For You", "sub": "Based on what you watch and like", "items": fy })); fy_added = true; }
        if id == "trend-tv" || id == "buzz-tv" {
            if !my_list.is_empty() { out.push(row("mylist", "My List", my_list.clone())); }
            out.extend(byw.iter().take(1).cloned());
        }
        if id == "sports-slot" { out.extend(au.iter().take(3).cloned()); }
        if id == "popular-tv" { out.extend(byw.iter().skip(1).cloned()); }
    }
    Value::Array(out.into_iter().filter(|r| r["id"] == "sports-slot" || r["items"].as_array().map_or(false, |a| !a.is_empty())).collect())
}

pub async fn movies(app: Arc<App>) -> Value {
    let mut r = vec![]; let t = &app.tmdb;
    if t.enabled() {
        let (a, b, c) = tokio::join!(t.pages("/trending/movie/week", 5, crate::tmdb::NO, 3 * TTL_H), t.pages("/movie/now_playing", 4, [sp("region", "AU")], 3 * TTL_H), t.pages("/movie/popular", 5, [sp("region", "AU")], 3 * TTL_H));
        r.push(row("m-trend", "Trending This Week", match_results(&app, &a, Some("movie"), 40)));
        r.push(row("m-cinema", "Fresh From Cinemas", match_results(&app, &b, Some("movie"), 40)));
        r.push(row("m-pop", "Popular Right Now", match_results(&app, &c, Some("movie"), 40)));
    } else { r.push(row("m-buzz", "New & Highly Rated", buzz(&app, false, 40))); }
    r.push(row("m-new", "Recently Added", recent(&app, false, 40)));
    if t.enabled() { r.extend(au_providers(&app, "movie").await); }
    // Movies in 4K: include any movie with a non-CAM 4K primary OR 4K variant (even if primary is HD!).
    let cat = app.catalog();
    let mut uhd_movies: Vec<&Item> = cat.movies.iter()
        .filter(|x| x.en && !x.sport && !x.poster.is_empty()
            && ((!x.cam && x.uhd) || x.variants.iter().any(|v| !v.cam && v.uhd))
            && !crate::discover::hidden_item(&app, x))
        .collect();
    uhd_movies.sort_by(|a, b| b.added.cmp(&a.added).then_with(|| b.rating.total_cmp(&a.rating)));
    r.push(row("m-4k", "In 4K", uhd_movies.into_iter().take(40).map(|x| app.card_meta(x)).collect()));
    if t.enabled() {
        let y = year();
        let futs = MOVIE_GENRE_ROWS.iter().map(|(g, _)| t.pages("/discover/movie", 3, [sp("with_genres", &g.to_string()), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "150"), sp("primary_release_date.gte", &format!("{}-01-01", y - 6))], 12 * TTL_H));
        let res = futures_util::future::join_all(futs).await;
        for ((g, name), list) in MOVIE_GENRE_ROWS.iter().zip(res) { r.push(row(&format!("m-g{g}"), name, match_results(&app, &list, Some("movie"), 40))); }
        r.push(row("m-top", "All-Time Greats", match_results(&app, &t.pages("/movie/top_rated", 4, crate::tmdb::NO, 3 * TTL_H).await, Some("movie"), 40)));
    } else {
        for g in ["ACTION", "THRILLER", "COMEDY", "SCIENCE FICTION", "HORROR", "DRAMA", "FAMILY", "DOCUMENTARIES"] {
            let re = regex::Regex::new(&format!("(?i)^EN - {g}$")).unwrap();
            r.push(row(&format!("m-c{g}"), &crate::util::title_case(&g.to_lowercase()), from_category(&app, false, &re, 40, false)));
        }
    }
    let mut sp_items: Vec<&Item> = cat.movies.iter().filter(|x| x.sport && !x.poster.is_empty()).collect();
    sp_items.sort_by(|a, b| b.added.cmp(&a.added));
    r.push(row("m-sport", "Sports Replays & Fights", sp_items.into_iter().take(40).map(|x| app.card(x, None)).collect()));
    rows_nonempty(r)
}

pub async fn series(app: Arc<App>) -> Value {
    let mut r = vec![]; let t = &app.tmdb;
    if t.enabled() {
        let (a, b, c) = tokio::join!(t.pages("/trending/tv/week", 5, crate::tmdb::NO, 3 * TTL_H), t.pages("/tv/on_the_air", 4, crate::tmdb::NO, 3 * TTL_H), t.pages("/tv/popular", 5, crate::tmdb::NO, 3 * TTL_H));
        r.push(row("s-trend", "Trending This Week", match_results(&app, &a, Some("tv"), 40)));
        r.push(row("s-air", "New Episodes This Week", match_results(&app, &b, Some("tv"), 40)));
        r.push(row("s-pop", "Popular Right Now", match_results(&app, &c, Some("tv"), 40)));
    } else { r.push(row("s-buzz", "New & Highly Rated", buzz(&app, true, 40))); }
    r.push(row("s-new", "Recently Updated", recent(&app, true, 40)));
    if t.enabled() { r.extend(au_providers(&app, "tv").await); }
    if t.enabled() {
        let futs = TV_GENRE_ROWS.iter().map(|(g, _)| t.pages("/discover/tv", 3, [sp("with_genres", &g.to_string()), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "80"), sp("with_original_language", "en")], 12 * TTL_H));
        let res = futures_util::future::join_all(futs).await;
        for ((g, name), list) in TV_GENRE_ROWS.iter().zip(res) { r.push(row(&format!("s-g{g}"), name, match_results(&app, &list, Some("tv"), 40))); }
        r.push(row("s-top", "All-Time Greats", match_results(&app, &t.pages("/tv/top_rated", 4, crate::tmdb::NO, 3 * TTL_H).await, Some("tv"), 40)));
    }
    rows_nonempty(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_media_genre_projection_bridges_movies_and_tv() {
        let mut gm = HashMap::new();
        let mut gt = HashMap::new();
        // TV fan watching Action & Adventure (10759) + Sci-Fi & Fantasy (10765) + War & Politics (10768)
        add_cross_genre_weight(&mut gm, &mut gt, 10759, 2.0);
        add_cross_genre_weight(&mut gm, &mut gt, 10765, 2.0);
        add_cross_genre_weight(&mut gm, &mut gt, 10768, 1.0);
        assert!(gm.get(&28).copied().unwrap_or(0.0) > 0.0, "10759 -> movie Action 28");
        assert!(gm.get(&12).copied().unwrap_or(0.0) > 0.0, "10759 -> movie Adventure 12");
        assert!(gm.get(&878).copied().unwrap_or(0.0) > 0.0, "10765 -> movie Sci-Fi 878");
        assert!(gm.get(&14).copied().unwrap_or(0.0) > 0.0, "10765 -> movie Fantasy 14");
        assert!(gm.get(&10752).copied().unwrap_or(0.0) > 0.0, "10768 -> movie War 10752");

        // Movie fan watching Thriller (53) + Horror (27) + Action (28)
        let mut gm2 = HashMap::new();
        let mut gt2 = HashMap::new();
        add_cross_genre_weight(&mut gm2, &mut gt2, 53, 2.0);
        add_cross_genre_weight(&mut gm2, &mut gt2, 27, 2.0);
        add_cross_genre_weight(&mut gm2, &mut gt2, 28, 2.0);
        assert!(gt2.get(&10759).copied().unwrap_or(0.0) > 0.0, "28 -> TV Action & Adventure 10759");
        assert!(gt2.get(&80).copied().unwrap_or(0.0) > 0.0, "53 -> TV Crime 80");
        assert!(gt2.get(&9648).copied().unwrap_or(0.0) > 0.0, "53/27 -> TV Mystery 9648");
        assert!(gt2.get(&10765).copied().unwrap_or(0.0) > 0.0, "27 -> TV Sci-Fi & Fantasy 10765");
    }

    #[test]
    fn rewatch_check_allows_newer_progress_after_watched() {
        assert!(is_rewatch_allowed(None, 1000));
        assert!(!is_rewatch_allowed(Some(2000), 1000));
        assert!(!is_rewatch_allowed(Some(2000), 2000));
        assert!(is_rewatch_allowed(Some(2000), 2500));
    }

    #[test]
    fn taste_vector_rewards_matching_director_keywords_and_penalizes_dislikes() {
        let mut tv = TasteVector::default();
        let liked = Meta {
            g: vec![878, 12],
            kw: vec![100, 200],
            dir: vec!["Denis Villeneuve".into()],
            cast: vec!["Timothée Chalamet".into()],
            d: "2024-03-01".into(),
            ..Default::default()
        };
        let disliked = Meta {
            g: vec![27],
            kw: vec![999],
            dir: vec!["Bad Dir".into()],
            d: "2020-01-01".into(),
            ..Default::default()
        };
        tv.ingest_meta(&liked, 2.0);
        tv.ingest_meta(&disliked, -2.5);

        let match_film = Meta {
            g: vec![878],
            kw: vec![100],
            dir: vec!["Denis Villeneuve".into()],
            va: 8.1,
            vc: 4000,
            p: 50.0,
            d: "2021-10-01".into(),
            ..Default::default()
        };
        let horror_film = Meta {
            g: vec![27],
            kw: vec![999],
            dir: vec!["Bad Dir".into()],
            va: 8.1,
            vc: 4000,
            p: 50.0,
            d: "2021-10-01".into(),
            ..Default::default()
        };
        assert!(tv.score_meta(false, &match_film, 2026) > tv.score_meta(false, &horror_film, 2026) + 5.0);
    }
}
