// Discovery: Browse filters, curated/mood rows, franchises, people, fuzzy + plain-English search,
// wanted list, surprise me, content-type hiding.
use crate::app::App;
use crate::catalog::{Catalog, Item};
use crate::meta::Meta;
use crate::rec::{match_results, sp};
use crate::tmdb::{TTL_D, TTL_H};
use crate::util::{norm, now_ms, s, tmdb_img};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn year() -> i32 { chrono::Datelike::year(&chrono::Utc::now()) }

fn app_port(app: &App) -> u16 {
    std::env::var("TV_PORT").ok().and_then(|p| p.parse().ok())
        .unwrap_or_else(|| app.cfg.get("port").as_u64().unwrap_or(7878) as u16)
}

// ---------- content hiding
pub fn hide_prefs(app: &App) -> Vec<String> { app.state.read(|st| st["prefs"]["hide"].as_array().map(|a| a.iter().map(s).collect()).unwrap_or_default()) }
pub fn hidden_by(hide: &[String], g: &[i64], l: &str, cat: &str, kw: &[i64], standup: &HashSet<i64>) -> bool {
    hide.iter().any(|h| match h.as_str() {
        "kids" => g.contains(&10762) || g.contains(&10751),
        "anime" => g.contains(&16) && matches!(l, "ja" | "zh" | "ko"),
        "reality" => g.contains(&10764),
        "talk" => g.contains(&10767) || g.contains(&10763),
        "docs" => g.contains(&99),
        "horror" => g.contains(&27),
        "standup" => cat.to_uppercase().contains("STAND-UP") || cat.to_uppercase().contains("STAND UP") || kw.iter().any(|k| standup.contains(k)),
        _ => false,
    })
}
pub(crate) fn standup_kw(app: &App) -> HashSet<i64> {
    let mut set: HashSet<i64> = app.kw_cache.lock().unwrap().iter()
        .filter(|(k, _)| k.starts_with("stand"))
        .filter_map(|(_, v)| *v)
        .collect();
    set.insert(9716); // TMDB "stand-up comedy"
    set
}
pub fn hide_cams_enabled(app: &App) -> bool { app.state.read(|st| st["prefs"]["hideCams"].as_bool().unwrap_or(true)) }
pub fn hidden_item(app: &App, it: &Item) -> bool {
    if hide_cams_enabled(app) && crate::showcase::is_cam_only(app, it) { return true; }
    let hide = hide_prefs(app); if hide.is_empty() { return false; }
    let m = app.meta_of(it).unwrap_or_default();
    let cat = app.catalog().cat_name(if it.k == "s" { "series" } else { "movie" }, &it.cat);
    hidden_by(&hide, &m.g, &m.l, &cat, &m.kw, &standup_kw(app))
}

pub fn watched_set(app: &App) -> HashSet<String> {
    let cat = app.catalog();
    app.state.read(|st| {
        let mut set: HashSet<String> = st["watched"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        for h in st["history"].as_array().into_iter().flatten() { let id = s(&h["tmdb"]); if !id.is_empty() { set.insert(format!("{}:{id}", if h["kind"] == "s" { "tv" } else { "movie" })); } }
        for p in st["progress"].as_object().into_iter().flat_map(|m| m.values()) {
            if p["item"]["k"] == "m" { let d = p["dur"].as_f64().unwrap_or(0.0); if d > 0.0 && p["pos"].as_f64().unwrap_or(0.0) / d > 0.9 {
                if let Some(t) = p["item"]["id"].as_i64().and_then(|id| cat.movie(id)).and_then(|it| it.tmdb.clone()) { set.insert(format!("movie:{t}")); } } }
        }
        set
    })
}

pub async fn keyword_ids(app: &Arc<App>, names: &[&str]) -> Vec<i64> {
    let mut out = vec![];
    for n in names {
        let cached = app.kw_cache.lock().unwrap().get(*n).cloned();
        let id = match cached {
            Some(v) => v,
            None => {
                let r = app.tmdb.get("/search/keyword", [sp("query", n)], 30 * TTL_D).await;
                let id = r.as_ref().and_then(|r| r["results"].as_array()).and_then(|a| a.iter().find(|k| s(&k["name"]).eq_ignore_ascii_case(n))).and_then(|k| k["id"].as_i64());
                app.kw_cache.lock().unwrap().insert(n.to_string(), id); id
            }
        };
        if let Some(i) = id { out.push(i); }
    }
    out
}

fn cmp_ci(a: &str, b: &str) -> std::cmp::Ordering {
    a.bytes().map(|c| c.to_ascii_lowercase()).cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

fn genre_matches(m_genres: &[i64], g: i64) -> bool {
    if m_genres.contains(&g) { return true; }
    match g {
        28 | 12 => m_genres.contains(&10759),
        10759 => m_genres.contains(&28) || m_genres.contains(&12),
        878 | 14 => m_genres.contains(&10765),
        10765 => m_genres.contains(&878) || m_genres.contains(&14),
        10752 => m_genres.contains(&10768),
        10768 => m_genres.contains(&10752),
        _ => false,
    }
}

fn contains_ci(hay: &str, needle_lc: &str, whole_word: bool) -> bool {
    if needle_lc.is_empty() || hay.is_empty() { return false; }
    if !needle_lc.is_ascii() {
        return hay.to_lowercase().contains(needle_lc);
    }
    let hb = hay.as_bytes();
    let nb = needle_lc.as_bytes();
    let nl = nb.len();
    if nl > hb.len() { return false; }
    let first = nb[0];
    let end = hb.len() - nl;
    let mut i = 0;
    while i <= end {
        if hb[i].to_ascii_lowercase() == first
            && hb[i..i + nl].iter().zip(nb).all(|(a, b)| a.to_ascii_lowercase() == *b)
        {
            if !whole_word { return true; }
            let left_ok = i == 0 || !hb[i - 1].is_ascii_alphanumeric();
            let r = i + nl;
            let right_ok = r == hb.len()
                || !hb[r].is_ascii_alphanumeric()
                || (hb[r].to_ascii_lowercase() == b's'
                    && (r + 1 == hb.len() || !hb[r + 1].is_ascii_alphanumeric()));
            if left_ok && right_ok { return true; }
        }
        i += 1;
    }
    false
}

fn matches_folded(hay: &str, term_norm: &str) -> bool {
    if hay.is_empty() || term_norm.is_empty() { return false; }
    if contains_ci(hay, term_norm, false) { return true; }
    !hay.is_ascii() && norm(hay).contains(term_norm)
}

fn item_matches_text(cat: &Catalog, it: &Item, m: Option<&Meta>, terms: &[String], word_short: bool) -> bool {
    let is_tv = it.k == "s";
    let mut plats_opt: Option<Vec<&'static str>> = None;
    for w in terms {
        let ws = word_short && w.len() <= 3;
        let ws_lbl = word_short && w.len() <= 2;
        if contains_ci(&it.title, w, ws) { continue; }
        if let Some(m) = m {
            if !m.ot.is_empty() && matches_folded(&m.ot, w) { continue; }
            if m.dir.iter().any(|d| matches_folded(d, w)) { continue; }
            if m.cast.iter().any(|c| matches_folded(c, w)) { continue; }
            if contains_ci(&m.ov, w, ws) { continue; }
            if m.g.iter().filter_map(|g| crate::meta::genre_name(is_tv, *g)).any(|gn| contains_ci(gn, w, ws_lbl)) { continue; }
            if !word_short && m.oc.iter().any(|co| co.eq_ignore_ascii_case(w)) { continue; }
        }
        if contains_ci(&it.plot, w, ws) { continue; }
        if contains_ci(&it.genre, w, ws_lbl) { continue; }
        if !word_short && !it.origin.is_empty() && it.origin.eq_ignore_ascii_case(w) { continue; }
        let plats = plats_opt.get_or_insert_with(|| cat.item_platforms(it));
        if plats.iter().any(|p| contains_ci(p, w, ws_lbl)) { continue; }
        return false;
    }
    true
}

// ---------- 1. Browse
pub fn browse(app: &App, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let kind_raw = get("kind").trim().to_lowercase();
    if kind_raw == "docs" { return crate::docs::browse(app, q); }
    if kind_raw == "anime" { return crate::anime::browse(app, q); }
    let kind = match kind_raw.as_str() { "tv" | "series" => "tv", "all" | "any" => "all", _ => "movie" };

    let genres: Vec<i64> = get("g").split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let gmode = get("gmode").trim().to_lowercase();
    let terms: Vec<String> = norm(&get("q")).split_whitespace().map(String::from).collect();
    let num = |k: &str| get(k).parse::<f64>().unwrap_or(0.0);
    let y1 = num("y1") as i32;
    let y2 = { let v = num("y2") as i32; if v == 0 { 9999 } else { v } };
    let rmin = num("rmin");
    let vmin_opt: Option<i64> = q.get("vmin").map(|v| v.trim()).filter(|v| !v.is_empty()).and_then(|v| v.parse().ok());
    let vmax = get("vmax").trim().parse::<i64>().unwrap_or(0);
    let (rtmax, rtmin) = (num("rtmax") as i64, num("rtmin") as i64);
    let lang = get("lang").trim().to_lowercase();
    let country = { let c = get("country").trim().to_uppercase(); if c == "UK" { "GB".to_string() } else { c } };
    let platform = get("platform").trim().to_string();
    let status = get("status").trim().to_lowercase();
    let uhd_only = get("uhd") == "1";
    let kws: Vec<i64> = get("kw").split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let langs: Vec<&str> = if lang == "nordic" { vec!["sv", "da", "no", "nb", "fi", "is"] } else { vec![lang.as_str()] };
    let seen = if get("unwatched") == "1" { Some(watched_set(app)) } else { None };
    let hide_cams = hide_cams_enabled(app);
    let disliked = crate::rec::hidden_ratings(app);

    let mut hide = hide_prefs(app);
    if genres.contains(&99) { hide.retain(|h| h != "docs"); }
    if genres.contains(&27) { hide.retain(|h| h != "horror"); }
    if genres.contains(&16) { hide.retain(|h| h != "anime"); }
    if genres.contains(&10762) { hide.retain(|h| h != "kids"); }
    if genres.contains(&10764) { hide.retain(|h| h != "reality"); }
    if genres.contains(&10767) || genres.contains(&10763) { hide.retain(|h| h != "talk"); }
    let su = standup_kw(app);

    let cat = app.catalog();
    let db = app.meta.read().unwrap();
    let mut list: Vec<(&Item, &Meta)> = vec![];
    let pools: Vec<&[Item]> = match kind {
        "tv" => vec![cat.series.as_slice()],
        "all" => vec![cat.movies.as_slice(), cat.series.as_slice()],
        _ => vec![cat.movies.as_slice()],
    };
    for pool in pools {
        for it in pool.iter().filter(|x| x.tmdb.is_some() && x.en) {
            if hide_cams && crate::showcase::is_cam_only(app, it) { continue; }
            let is_tv = it.k == "s";
            let mt = if is_tv { "tv" } else { "movie" };
            let tmdb_id = it.tmdb.as_ref().unwrap();
            let tkey = format!("{mt}:{tmdb_id}");
            if disliked.contains(&tkey) { continue; }
            let Some(m) = db.get(&tkey) else { continue };
            if m.missing { continue; }
            let yr = if m.year() > 0 { m.year() } else { it.year.unwrap_or(0) };
            if y1 > 0 && yr < y1 { continue; }
            if y2 < 9999 && (yr == 0 || yr > y2) { continue; }
            if !genres.is_empty() {
                let ok = if gmode == "all" {
                    genres.iter().all(|&g| genre_matches(&m.g, g))
                } else {
                    genres.iter().any(|&g| genre_matches(&m.g, g))
                };
                if !ok { continue; }
            }
            if let Some(vmin) = vmin_opt { if m.vc < vmin { continue; } }
            if vmax > 0 && m.vc > vmax { continue; }
            if rmin > 0.0 {
                let floor = vmin_opt.unwrap_or(if is_tv || m.g.contains(&99) { 25 } else { 50 });
                if m.va < rmin || m.vc < floor { continue; }
            }
            if rtmax > 0 && (m.rt == 0 || m.rt > rtmax) { continue; }
            if rtmin > 0 && m.rt < rtmin { continue; }
            match lang.as_str() {
                "" => {},
                "en" => if m.l != "en" { continue },
                "nonen" => if m.l == "en" { continue },
                _ => if !langs.contains(&m.l.as_str()) { continue },
            }
            if !country.is_empty() && !m.oc.iter().any(|c| c.eq_ignore_ascii_case(&country)) { continue; }
            if !platform.is_empty() && !cat.item_has_platform(it, &platform) { continue; }
            if !status.is_empty() {
                match status.as_str() {
                    "ended" => if !is_tv || !m.st.eq_ignore_ascii_case("Ended") { continue; },
                    "returning" => if !is_tv || !m.st.eq_ignore_ascii_case("Returning Series") { continue; },
                    "miniseries" => if !is_tv || m.ns != 1 || !m.st.eq_ignore_ascii_case("Ended") { continue; },
                    _ => {}
                }
            }
            let has_uhd = (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd);
            if uhd_only && !has_uhd { continue; }
            if !kws.is_empty() && !kws.iter().any(|k| m.kw.contains(k)) { continue; }
            if let Some(seen) = &seen { if seen.contains(&tkey) { continue; } }
            if !hide.is_empty() && hidden_by(&hide, &m.g, &m.l, &cat.cat_name(if is_tv { "series" } else { "movie" }, &it.cat), &m.kw, &su) { continue; }
            if !terms.is_empty() && !item_matches_text(&cat, it, Some(m), &terms, false) { continue; }
            list.push((it, m));
        }
    }

    // Dynamic facets across all matched items (before pagination)
    let mut g_counts: HashMap<i64, usize> = HashMap::new();
    let mut p_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut c_counts: HashMap<&str, usize> = HashMap::new();
    let (mut mv_count, mut tv_count, mut uhd_count) = (0usize, 0usize, 0usize);
    for (it, m) in &list {
        if it.k == "s" { tv_count += 1; } else { mv_count += 1; }
        if (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd) { uhd_count += 1; }
        for &g in &m.g { *g_counts.entry(g).or_default() += 1; }
        for p in cat.item_platforms(it) { *p_counts.entry(p).or_default() += 1; }
        for co in &m.oc { if !co.is_empty() { *c_counts.entry(co.as_str()).or_default() += 1; } }
    }
    let mut plat_vec: Vec<(&str, usize)> = p_counts.into_iter().collect();
    plat_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    plat_vec.truncate(12);
    let mut country_vec: Vec<(&str, usize)> = c_counts.into_iter().collect();
    country_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    country_vec.truncate(12);
    let genres_json: serde_json::Map<String, Value> = g_counts.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let facets = json!({
        "genres": Value::Object(genres_json),
        "platforms": plat_vec.into_iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>(),
        "countries": country_vec.into_iter().map(|(c, n)| json!({ "code": c, "count": n })).collect::<Vec<_>>(),
        "movies": mv_count,
        "series": tv_count,
        "uhd": uhd_count,
    });

    match get("sort").as_str() {
        "rating" => list.sort_by(|a, b| b.1.bayes().total_cmp(&a.1.bayes())),
        "votes" => list.sort_by(|a, b| b.1.vc.cmp(&a.1.vc).then_with(|| b.1.bayes().total_cmp(&a.1.bayes()))),
        "newest" => list.sort_by(|a, b| b.1.d.cmp(&a.1.d).then(b.0.year.cmp(&a.0.year))),
        "oldest" => list.sort_by(|a, b| {
            let da = if a.1.d.is_empty() { "9999" } else { a.1.d.as_str() };
            let db = if b.1.d.is_empty() { "9999" } else { b.1.d.as_str() };
            da.cmp(db).then(a.0.year.unwrap_or(9999).cmp(&b.0.year.unwrap_or(9999)))
        }),
        "runtime_asc" => list.sort_by(|a, b| {
            let ra = if a.1.rt > 0 { a.1.rt } else { i64::MAX };
            let rb = if b.1.rt > 0 { b.1.rt } else { i64::MAX };
            ra.cmp(&rb)
        }),
        "runtime_desc" => list.sort_by(|a, b| b.1.rt.cmp(&a.1.rt)),
        "added" => list.sort_by(|a, b| b.0.added.cmp(&a.0.added)),
        "az" => list.sort_by(|a, b| cmp_ci(&a.0.title, &b.0.title)),
        _ => list.sort_by(|a, b| b.1.p.total_cmp(&a.1.p)),
    }
    let per = get("per").parse::<usize>().unwrap_or(60).max(1);
    let page = get("page").parse::<usize>().unwrap_or(0);
    let total = list.len();
    let items: Vec<&Item> = list.into_iter().skip(page * per).take(per).map(|x| x.0).collect();
    drop(db);
    let running = app.meta_running.load(std::sync::atomic::Ordering::Relaxed);
    let indexing = if running { json!({ "done": app.meta.read().unwrap().len(), "total": cat.movie_by_tmdb.len() + cat.series_by_tmdb.len() }) } else { Value::Null };
    json!({ "total": total, "page": page, "per": per, "facets": facets, "items": items.into_iter().map(|it| app.card_meta(it)).collect::<Vec<_>>(), "indexing": indexing })
}

// ---------- 3. Curated & moods
struct Mood { id: &'static str, title: &'static str, kws: &'static [&'static str], genres: &'static [i64] }
const MOODS: &[Mood] = &[
    Mood { id: "mind", title: "Mind-Bending", kws: &["mind-bending", "plot twist", "time loop", "alternate reality", "nonlinear timeline", "surrealism", "parallel world", "dream"], genres: &[] },
    Mood { id: "feel", title: "Feel-Good", kws: &["feel-good", "heartwarming", "uplifting", "wholesome", "friendship"], genres: &[35, 10751, 10749] },
    Mood { id: "edge", title: "Edge of Your Seat", kws: &["suspense", "tense", "survival", "heist", "manhunt", "hostage", "cat and mouse"], genres: &[53, 28, 80] },
    Mood { id: "true", title: "Based on a True Story", kws: &["based on true story", "biography", "based on real events", "true crime"], genres: &[] },
    Mood { id: "dark", title: "Dark & Gritty", kws: &["neo-noir", "serial killer", "gritty", "organized crime", "revenge", "corruption"], genres: &[] },
    Mood { id: "epic", title: "Big & Epic", kws: &["epic", "space opera", "sword and sorcery", "kaiju", "dystopia", "post-apocalyptic future"], genres: &[] },
];

fn seeded_shuffle<T>(v: &mut Vec<T>, mut seed: u64) {
    for i in (1..v.len()).rev() { seed = (seed * 9301 + 49297) % 233280; let j = ((seed as f64 / 233280.0) * (i + 1) as f64) as usize; v.swap(i, j); }
}
fn rank(app: &App, tv: bool, filter: impl Fn(&Meta) -> bool, score: impl Fn(&Meta) -> f64, n: usize, rotate: bool) -> Vec<Value> {
    let mt = if tv { "tv" } else { "movie" };
    let cat = app.catalog(); let seen = watched_set(app); let hide = hide_prefs(app); let su = standup_kw(app);
    let hide_cams = hide_cams_enabled(app);
    let disliked = crate::rec::hidden_ratings(app);
    let db = app.meta.read().unwrap();
    let pool = if tv { &cat.series } else { &cat.movies };
    let mut list: Vec<(&Item, f64)> = pool.iter().filter(|x| x.en && x.tmdb.is_some()).filter_map(|it| {
        if hide_cams && crate::showcase::is_cam_only(app, it) { return None; }
        let t = it.tmdb.as_ref().unwrap();
        let tkey = format!("{mt}:{t}");
        if disliked.contains(&tkey) || seen.contains(&tkey) { return None; }
        let m = db.get(&tkey)?;
        if m.missing || !filter(m) { return None; }
        if !hide.is_empty() && hidden_by(&hide, &m.g, &m.l, &cat.cat_name(if tv { "series" } else { "movie" }, &it.cat), &m.kw, &su) { return None; }
        Some((it, score(m)))
    }).collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1));
    if rotate { list.truncate(n * 3); seeded_shuffle(&mut list, (now_ms() / 86_400_000) as u64); }
    let items: Vec<&Item> = list.into_iter().take(n).map(|x| x.0).collect();
    drop(db);
    items.into_iter().map(|it| app.card_meta(it)).collect()
}

pub async fn curated(app: Arc<App>, tv: bool) -> Value {
    let y = year(); let mut rows = vec![]; let mt = if tv { "tv" } else { "movie" };
    rows.push(json!({ "id": format!("{mt}-gems"), "title": "Hidden Gems", "sub": "Highly rated, under the radar", "items": rank(&app, tv, |m| m.va >= 7.2 && m.vc >= 150 && m.vc <= 2500, |m| m.bayes(), 40, true) }));
    let min_votes = if tv { 300 } else { 800 };
    rows.push(json!({ "id": format!("{mt}-critics"), "title": "Critics' Picks", "sub": format!("Best reviewed since {}", y - 5), "items": rank(&app, tv, |m| m.year() >= y - 5 && m.vc >= min_votes, |m| m.bayes(), 40, false) }));
    if !tv { rows.push(json!({ "id": "short", "title": "Short on Time", "sub": "Great films under 100 minutes", "items": rank(&app, tv, |m| m.rt >= 70 && m.rt <= 100 && m.bayes() >= 6.7, |m| m.bayes() + (m.p + 1.0).log10(), 40, true) })); }
    else { rows.push(json!({ "id": "binge", "title": "Bingeable Miniseries", "sub": "One season, done", "items": rank(&app, tv, |m| m.ns == 1 && m.st == "Ended" && m.bayes() >= 7.0, |m| m.bayes() + (m.p + 1.0).log10(), 40, true) })); }
    for mood in MOODS {
        let ids = keyword_ids(&app, mood.kws).await;
        let gs = mood.genres;
        let items = rank(&app, tv, |m| m.kw.iter().any(|k| ids.contains(k)) && (gs.is_empty() || m.g.iter().any(|g| gs.contains(g))) && m.bayes() >= 6.6,
            |m| m.bayes() * 1.4 + (m.p + 1.0).log10() + m.kw.iter().filter(|k| ids.contains(k)).count() as f64 * 0.4, 40, true);
        rows.push(json!({ "id": format!("{mt}-mood-{}", mood.id), "title": mood.title, "items": items }));
    }
    if !tv {
        let bp = keyword_ids(&app, &["oscar (best picture)", "best picture winner", "academy award for best picture"]).await;
        if !bp.is_empty() { rows.push(json!({ "id": "oscars", "title": "Best Picture Winners", "items": rank(&app, tv, |m| m.kw.iter().any(|k| bp.contains(k)), |m| m.year() as f64, 40, false) })); }
    }
    Value::Array(rows.into_iter().filter(|r| r["items"].as_array().map_or(0, |a| a.len()) >= 6).collect())
}

pub fn franchises(app: &App) -> Vec<Value> {
    let cat = app.catalog(); let db = app.meta.read().unwrap();
    let mut groups: HashMap<i64, (crate::meta::Col, usize, f64)> = HashMap::new();
    for it in cat.movies.iter().filter(|x| x.en && x.tmdb.is_some()) {
        let Some(m) = db.get(&format!("movie:{}", it.tmdb.as_ref().unwrap())) else { continue };
        let Some(col) = &m.col else { continue };
        let g = groups.entry(col.id).or_insert((col.clone(), 0, 0.0)); g.1 += 1; g.2 += m.p;
    }
    let mut v: Vec<_> = groups.into_values().filter(|g| g.1 >= 3).collect();
    v.sort_by(|a, b| b.2.total_cmp(&a.2));
    v.into_iter().take(40).map(|(c, n, _)| json!({ "k": "c", "id": c.id, "title": c.n.trim_end_matches(" Collection"), "poster": tmdb_img(c.pp.as_deref().unwrap_or(""), "w342"), "backdrop": tmdb_img(c.bd.as_deref().unwrap_or(""), "w1280"), "sub": format!("{n} films") })).collect()
}

pub async fn collection(app: &Arc<App>, id: &str) -> Value {
    let d = app.tmdb.get(format!("/collection/{id}"), crate::tmdb::NO, 7 * TTL_D).await.unwrap_or_default();
    let mut parts: Vec<Value> = d["parts"].as_array().cloned().unwrap_or_default();
    parts.sort_by(|a, b| { let x = s(&a["release_date"]); let y = s(&b["release_date"]); (if x.is_empty() { "9".into() } else { x }).cmp(&(if y.is_empty() { "9".into() } else { y })) });
    let seen = watched_set(app); let cat = app.catalog();
    let items: Vec<Value> = parts.iter().map(|p| {
        let pid = p["id"].to_string();
        match cat.by_tmdb("movie", &pid) {
            Some(it) => { let mut c = app.card(it, Some(p)); c["watched"] = json!(seen.contains(&format!("movie:{pid}"))); c }
            None => json!({ "k": "x", "mt": "movie", "tmdb": pid, "title": s(&p["title"]), "year": s(&p["release_date"]).get(0..4).and_then(|y| y.parse::<i32>().ok()), "poster": tmdb_img(p["poster_path"].as_str().unwrap_or(""), "w342"), "missing": true }),
        }
    }).collect();
    json!({ "id": id, "title": s(&d["name"]).trim_end_matches(" Collection"), "overview": s(&d["overview"]), "backdrop": tmdb_img(d["backdrop_path"].as_str().unwrap_or(""), "w1280"), "items": items })
}

// ---------- 2. Search
fn trigrams(s: &str) -> HashSet<[u8; 3]> {
    let t: Vec<u8> = format!(" {s} ").into_bytes(); let mut out = HashSet::new();
    for w in t.windows(3) { out.insert([w[0], w[1], w[2]]); }
    out
}
fn fuzzy_items<'a>(app: &App, cat: &'a Catalog, q: &str, limit: usize) -> Vec<&'a Item> {
    let idx = { app.tri.read().unwrap().clone() };
    let idx = match idx { Some(i) => i, None => {
        let mut v = vec![];
        for (ix, it) in cat.movies.iter().enumerate() { if it.en && it.tmdb.is_some() { v.push((it.title.clone(), false, ix, trigrams(&norm(&it.title)))); } }
        for (ix, it) in cat.series.iter().enumerate() { if it.en && it.tmdb.is_some() { v.push((it.title.clone(), true, ix, trigrams(&norm(&it.title)))); } }
        let a = Arc::new(v); *app.tri.write().unwrap() = Some(a.clone()); a
    } };
    let qg = trigrams(&norm(q)); if qg.len() < 3 { return vec![]; }
    let db = app.meta.read().unwrap();
    let mut out: Vec<(f64, bool, usize)> = idx.iter().filter_map(|(_, tv, ix, g)| {
        let inter = qg.iter().filter(|x| g.contains(*x)).count() as f64;
        let cover = inter / qg.len() as f64; let dice = 2.0 * inter / (qg.len() + g.len()) as f64;
        if cover < 0.55 { return None; }
        let it = if *tv { &cat.series[*ix] } else { &cat.movies[*ix] };
        let pop = it.tmdb.as_ref().and_then(|t| db.get(&format!("{}:{t}", if *tv { "tv" } else { "movie" }))).map(|m| m.p).unwrap_or(1.0);
        Some((cover * 0.7 + dice * 0.3 + (pop + 1.0).log10() * 0.04, *tv, *ix))
    }).collect();
    drop(db);
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().take(limit).map(|(_, tv, ix)| if tv { &cat.series[ix] } else { &cat.movies[ix] }).collect()
}

pub fn fuzzy(app: &App, q: &str, limit: usize) -> Vec<Value> {
    let cat = app.catalog();
    fuzzy_items(app, &cat, q, limit).into_iter().map(|it| app.card_meta(it)).collect()
}

pub async fn people(app: &Arc<App>, q: &str) -> Vec<Value> {
    if !app.tmdb.enabled() || q.len() < 3 { return vec![]; }
    let r = app.tmdb.get("/search/person", [sp("query", q)], TTL_D).await;
    r.as_ref().and_then(|r| r["results"].as_array()).into_iter().flatten()
        .filter(|p| p["profile_path"].is_string() && p["popularity"].as_f64().unwrap_or(0.0) > 1.0).take(6)
        .map(|p| json!({ "id": p["id"], "name": p["name"], "photo": tmdb_img(p["profile_path"].as_str().unwrap_or(""), "w185"), "dept": p["known_for_department"] })).collect()
}

pub async fn person(app: &Arc<App>, id: &str) -> Option<Value> {
    let d = app.tmdb.get(format!("/person/{id}"), [sp("append_to_response", "combined_credits")], 3 * TTL_D).await?;
    let cat = app.catalog();
    let pick = |list: Vec<Value>| -> Vec<Value> {
        let mut list = list; list.sort_by(|a, b| b["popularity"].as_f64().unwrap_or(0.0).total_cmp(&a["popularity"].as_f64().unwrap_or(0.0)));
        let mut seen = HashSet::new(); let mut out = vec![];
        for c in list {
            let mt = s(&c["media_type"]); let Some(it) = cat.by_tmdb(&mt, &c["id"].to_string()) else { continue };
            if !seen.insert(format!("{}{}", it.k, it.id)) { continue; }
            if mt == "tv" {
                let g: Vec<i64> = c["genre_ids"].as_array().into_iter().flatten().filter_map(|x| x.as_i64()).collect();
                if g.iter().any(|x| matches!(x, 10767 | 10763 | 10764)) { continue; }
                if c.get("character").is_some() && c["episode_count"].as_i64().unwrap_or(0) < 3 { continue; }
            }
            let mut card = app.card(it, Some(&c)); card["role"] = json!(c["character"].as_str().or(c["job"].as_str()).unwrap_or("")); out.push(card);
        }
        out
    };
    let cast = d["combined_credits"]["cast"].as_array().cloned().unwrap_or_default();
    let crew: Vec<Value> = d["combined_credits"]["crew"].as_array().cloned().unwrap_or_default().into_iter()
        .filter(|c| { let j = s(&c["job"]); ["Director", "Writer", "Screenplay", "Creator"].contains(&j.as_str()) || (j == "Executive Producer" && c["media_type"] == "tv") }).collect();
    Some(json!({ "id": id, "name": d["name"], "photo": tmdb_img(d["profile_path"].as_str().unwrap_or(""), "w342"), "bio": s(&d["biography"]).chars().take(700).collect::<String>(),
        "born": s(&d["birthday"]), "dept": d["known_for_department"], "acting": pick(cast), "crew": pick(crew) }))
}

pub fn external(mt: &str, x: &Value, wanted: bool) -> Value {
    let d = x["release_date"].as_str().filter(|v| !v.is_empty()).or(x["first_air_date"].as_str()).unwrap_or("");
    json!({ "k": "x", "id": format!("{mt}-{}", x["id"]), "mt": mt, "tmdb": x["id"].to_string(), "title": x["title"].as_str().or(x["name"].as_str()).unwrap_or(""),
        "year": d.get(0..4).and_then(|y| y.parse::<i32>().ok()), "poster": tmdb_img(x["poster_path"].as_str().unwrap_or(""), "w342"),
        "backdrop": tmdb_img(x["backdrop_path"].as_str().unwrap_or(""), "w1280"), "overview": s(&x["overview"]), "release": d, "rating": x["vote_average"].as_f64().unwrap_or(0.0), "wanted": wanted })
}
fn is_wanted(app: &App, mt: &str, id: &str) -> bool { app.state.read(|st| st["wanted"].get(format!("{mt}:{id}")).is_some()) }

pub async fn missing(app: &Arc<App>, q: &str) -> Vec<Value> {
    if !app.tmdb.enabled() || q.len() < 3 { return vec![]; }
    let r = app.tmdb.get("/search/multi", [sp("query", q), sp("include_adult", "false")], TTL_D).await;
    let cat = app.catalog();
    r.as_ref().and_then(|r| r["results"].as_array()).into_iter().flatten().filter(|x| {
        let mt = s(&x["media_type"]);
        (mt == "movie" || mt == "tv") && x["poster_path"].is_string() && (x["popularity"].as_f64().unwrap_or(0.0) > 2.0 || x["vote_count"].as_i64().unwrap_or(0) > 50) && cat.by_tmdb(&mt, &x["id"].to_string()).is_none()
    }).take(8).map(|x| { let mt = s(&x["media_type"]); external(&mt, x, is_wanted(app, &mt, &x["id"].to_string())) }).collect()
}

/// Parse decade phrases accurately:
/// - `1920s` -> `(1920, 1929)`
/// - `2020s` / `20s` / `the 20s` / `'20s` -> `(2020, 2029)`
/// - `00s` -> `(2000, 2009)`, `10s` -> `(2010, 2019)`
/// - `30s`..`90s` -> `(1930..1990)`
fn parse_decade(q: &str) -> Option<(i32, i32)> {
    if let Some(c) = crate::re!(r"\b(?:the\s+)?((?:19|20)\d0)'?s\b").captures(q) {
        let d: i32 = c[1].parse().ok()?;
        return Some((d, d + 9));
    }
    if let Some(c) = crate::re!(r"\b(?:the\s+)?'?([0-9]0)'?s\b").captures(q) {
        let two: i32 = c[1].parse().ok()?;
        let d = if two <= 20 { 2000 + two } else { 1900 + two };
        return Some((d, d + 9));
    }
    None
}

/// Interleave two lists (movies + series) so mixed results alternate evenly instead of burying series at index 30+.
fn interleave_items(a: Vec<Value>, b: Vec<Value>, limit: usize) -> Vec<Value> {
    let mut out = Vec::with_capacity((a.len() + b.len()).min(limit));
    let mut ia = a.into_iter();
    let mut ib = b.into_iter();
    loop {
        if out.len() >= limit { break; }
        let xa = ia.next();
        let xb = ib.next();
        if xa.is_none() && xb.is_none() { break; }
        if let Some(x) = xa { out.push(x); if out.len() >= limit { break; } }
        if let Some(x) = xb { out.push(x); }
    }
    out
}

/// True when the query contains unexplained title words outside known NL filter/connector tokens
/// (e.g. `"War of the Worlds"`, `"Space Jam"`, `"Doc Martin"`, `"Adventure Time"`).
fn has_leftover_title_words(q: &str) -> bool {
    let stripped = crate::re!(r"\b(?:the|a|an|of|in|on|for|from|to|with|and|or|at|by|about|me|show|find|some|good|great|best|top|acclaimed|highly|rated|new|recent|latest|classic|old|short|quick|movies?|films?|shows?|series|tv|docuseries|documentar(?:y|ies)|docu\w*|docs?|4k|uhd|ultra|hd|mini|limited|netflix|hbo|max|apple|disney|plus|amazon|prime|video|discovery|paramount|peacock|showtime|crunchyroll|action|comed(?:y|ies)|funny|hilarious|laugh|horror|scary|terrifying|creepy|sci|fi|science|fiction|space|futuristic|thrillers?|suspense|romance|romantic|love|stor(?:y|ies)|dramas?|crime|true|gangsters?|heists?|mafia|animated|animation|cartoons?|family|kids|fantasy|magic|war|westerns?|cowboys?|myster(?:y|ies)|whodunn?it|detective|musicals?|history|historical|period|adventure|wildlife|nature|sports?|music|bio\w*|portrait|profile|food|cooking|culinary|society|culture|politics|korean|japanese|french|spanish|indian|bollywood|hindi|german|italian|scandi\w*|nordic|swedish|danish|norwegian|chinese|british|uk|english|australian|aussie|american|canadian|irish|after|since|before|twist|mind|bending|dark|gritty|feel|uplifting|wholesome|edge|tense|gripping|epic|(?:19|20)?\d0'?s|(?:19|20)\d\d)\b")
        .replace_all(q, " ");
    stripped.split(|c: char| !c.is_alphanumeric()).any(|w| w.len() >= 2)
}

fn exact_title_cards(app: &App, cat: &Catalog, q0: &str) -> Vec<Value> {
    let qn = norm(q0);
    if qn.is_empty() { return vec![]; }
    let mut hits: Vec<(&Item, f64)> = vec![];
    let db = app.meta.read().unwrap();
    for it in cat.movies.iter().chain(cat.series.iter()).filter(|x| x.en) {
        let tn = norm(&it.title);
        let exact = tn == qn || (qn.contains(' ') && tn.starts_with(&format!("{qn} ")));
        if !exact { continue; }
        let pop = db_meta(&db, it).map(|m| m.p).unwrap_or(it.rating);
        let sc = (if tn == qn { 100.0 } else { 40.0 }) + pop;
        hits.push((it, sc));
    }
    drop(db);
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    hits.into_iter().take(6).map(|(it, _)| app.card_meta(it)).collect()
}

// Plain-English search.
pub async fn smart(app: &Arc<App>, q0: &str) -> Option<Value> {
    let q = q0.to_lowercase();
    let is_doc_word = crate::re!(r"\b(documentar(y|ies)|docu\w*|docs?)\b").is_match(&q);
    let mut kind = if crate::re!(r"\b(shows?|series|tv|docuseries)\b").is_match(&q) { Some("tv") } else if crate::re!(r"\b(movies?|films?)\b").is_match(&q) { Some("movie") } else { None };
    let (mut gm, mut gt, mut labels): (Vec<i64>, Vec<i64>, Vec<String>) = (vec![], vec![], vec![]);
    let mut chips: Vec<Value> = vec![];
    let mut f: HashMap<String, String> = HashMap::new();

    // Platforms
    let mut strong_platform = false;
    let nl_platform: &[(&regex::Regex, &str, &str, bool)] = &[
        (crate::re!(r"\bnetflix\b"), "netflix", "Netflix", true),
        (crate::re!(r"\bhbo(\s+max)?\b"), "hbo", "HBO", true),
        (crate::re!(r"\b(apple\s*tv\+?|apple\+)\b"), "apple", "Apple TV+", true),
        (crate::re!(r"\bapple\b"), "apple", "Apple TV+", false),
        (crate::re!(r"\bdisney(\+| plus)?\b"), "disney", "Disney+", true),
        (crate::re!(r"\b(amazon|prime\s+video)\b"), "amazon", "Amazon", true),
        (crate::re!(r"\bdiscovery(\+| plus)\b"), "discovery", "Discovery+", true),
        (crate::re!(r"\bdiscovery\b"), "discovery", "Discovery+", false),
        (crate::re!(r"\bparamount(\+| plus)?\b"), "paramount", "Paramount+", true),
        (crate::re!(r"\bpeacock\b"), "peacock", "Peacock", true),
        (crate::re!(r"\bshowtime\b"), "showtime", "Showtime", true),
        (crate::re!(r"\bcrunchyroll\b"), "crunchyroll", "Crunchyroll", true),
    ];
    for &(re, slug, disp, strong) in nl_platform {
        if !f.contains_key("platform") && re.is_match(&q) {
            f.insert("platform".into(), slug.into());
            labels.push(disp.into());
            chips.push(json!({ "key": "platform", "value": slug, "label": disp }));
            if strong { strong_platform = true; }
        }
    }

    // 4K / UHD & Miniseries / Limited Series
    let mut is_uhd = false;
    if crate::re!(r"\b(4k|uhd|ultra\s*hd)\b").is_match(&q) {
        f.insert("uhd".into(), "1".into());
        labels.push("4K".into());
        chips.push(json!({ "key": "uhd", "value": "1", "label": "4K" }));
        is_uhd = true;
    }
    let mut is_mini = false;
    if crate::re!(r"\b(mini[- ]?series|limited\s+series)\b").is_match(&q) {
        f.insert("status".into(), "miniseries".into());
        labels.push("Miniseries".into());
        chips.push(json!({ "key": "status", "value": "miniseries", "label": "Miniseries" }));
        is_mini = true;
        if kind.is_none() { kind = Some("tv"); }
    }

    // Documentary sub-topics
    let mut doc_topic: Option<&'static str> = None;
    let mut space_hint = false;
    if crate::re!(r"\btrue\s+crime\b").is_match(&q) || (is_doc_word && crate::re!(r"\bcrime\b").is_match(&q)) {
        doc_topic = Some("crime");
        labels.push("True Crime".into());
    } else if crate::re!(r"\b(wildlife)\b").is_match(&q)
        || (is_doc_word && crate::re!(r"\bnature\b").is_match(&q))
        || (crate::re!(r"\bnature\b").is_match(&q) && (kind.is_some() || !labels.is_empty() || crate::re!(r"\b(best|top|great|new|recent)\b").is_match(&q)))
    {
        doc_topic = Some("nature");
        labels.push("Nature & Wildlife".into());
    } else if is_doc_word && crate::re!(r"\bspace\b").is_match(&q) {
        doc_topic = Some("science");
        space_hint = true;
        labels.push("Space & Science".into());
    } else if (is_doc_word && crate::re!(r"\bscience\b").is_match(&q))
        || (crate::re!(r"\bscience\b").is_match(&q) && !crate::re!(r"\bscience\s+fiction\b").is_match(&q) && (kind.is_some() || !labels.is_empty()))
    {
        doc_topic = Some("science");
        labels.push("Science Docs".into());
    } else if is_doc_word && crate::re!(r"\bsports?\b").is_match(&q) {
        doc_topic = Some("sports");
        labels.push("Sports Docs".into());
    } else if is_doc_word && crate::re!(r"\bmusic\b").is_match(&q) {
        doc_topic = Some("music");
        labels.push("Music Docs".into());
    } else if is_doc_word && crate::re!(r"\b(history|historical|war|ww2|wwii)\b").is_match(&q) {
        doc_topic = Some("history");
        labels.push("History Docs".into());
    } else if is_doc_word && crate::re!(r"\b(bio\w*|portrait|profile)\b").is_match(&q) {
        doc_topic = Some("bio");
        labels.push("Biography Docs".into());
    } else if is_doc_word && crate::re!(r"\b(food|cooking|culinary)\b").is_match(&q) {
        doc_topic = Some("food");
        labels.push("Food Docs".into());
    } else if is_doc_word && crate::re!(r"\b(society|culture|politics)\b").is_match(&q) {
        doc_topic = Some("society");
        labels.push("Society Docs".into());
    }
    if let Some(topic) = doc_topic {
        chips.push(json!({ "key": "docTopic", "value": topic, "label": labels.last().cloned().unwrap_or_default() }));
    }

    let nl_genres: &[(&regex::Regex, &[i64], &[i64])] = &[
        (crate::re!(r"\b(action)\b"), &[28], &[10759]), (crate::re!(r"\b(comed(y|ies)|funny|hilarious|laugh)\b"), &[35], &[35]), (crate::re!(r"\b(horror|scary|terrifying|creepy)\b"), &[27], &[9648]),
        (crate::re!(r"\b(sci[- ]?fi|science fiction|space|futuristic)\b"), &[878], &[10765]), (crate::re!(r"\b(thrillers?|suspense)\b"), &[53], &[9648, 80]), (crate::re!(r"\b(romance|romantic|love stor(y|ies))\b"), &[10749], &[18]),
        (crate::re!(r"\b(dramas?)\b"), &[18], &[18]), (crate::re!(r"\b(crime|gangsters?|heists?|mafia)\b"), &[80], &[80]), (crate::re!(r"\b(documentar(y|ies)|docu\w*|docs?)\b"), &[99], &[99]),
        (crate::re!(r"\b(animated|animation|cartoons?)\b"), &[16], &[16]), (crate::re!(r"\b(family|kids)\b"), &[10751], &[10751, 10762]), (crate::re!(r"\b(fantasy|magic)\b"), &[14], &[10765]),
        (crate::re!(r"\b(war)\b"), &[10752], &[10768]), (crate::re!(r"\b(westerns?|cowboys?)\b"), &[37], &[37]), (crate::re!(r"\b(myster(y|ies)|whodunn?it|detective)\b"), &[9648], &[9648]),
        (crate::re!(r"\b(musicals?)\b"), &[10402], &[10402]), (crate::re!(r"\b(history|historical|period)\b"), &[36], &[10768]), (crate::re!(r"\b(adventure)\b"), &[12], &[10759]),
    ];
    let nl_lang: &[(&regex::Regex, &str)] = &[(crate::re!(r"\bkorean\b"), "ko"), (crate::re!(r"\bjapanese\b"), "ja"), (crate::re!(r"\bfrench\b"), "fr"), (crate::re!(r"\bspanish\b"), "es"), (crate::re!(r"\b(indian|bollywood|hindi)\b"), "hi"), (crate::re!(r"\bgerman\b"), "de"), (crate::re!(r"\bitalian\b"), "it"), (crate::re!(r"\b(scandi\w*|nordic|swedish|danish|norwegian)\b"), "nordic"), (crate::re!(r"\bchinese\b"), "zh")];
    let nl_country: &[(&regex::Regex, &str)] = &[(crate::re!(r"\b(british|uk|english)\b"), "GB"), (crate::re!(r"\b(australian|aussie)\b"), "AU"), (crate::re!(r"\bamerican\b"), "US"), (crate::re!(r"\bcanadian\b"), "CA"), (crate::re!(r"\birish\b"), "IE")];
    if doc_topic.is_none() {
        for (re, m, t) in nl_genres {
            if let Some(c) = re.find(&q) {
                gm.extend_from_slice(m);
                gt.extend_from_slice(t);
                labels.push(c.as_str().to_string());
                chips.push(json!({ "key": "g", "value": m.first().copied().unwrap_or(0).to_string(), "label": c.as_str() }));
            }
        }
    }
    if let Some((y1, y2)) = parse_decade(&q) {
        f.insert("y1".into(), y1.to_string());
        f.insert("y2".into(), y2.to_string());
        let lbl = format!("{y1}s");
        labels.push(lbl.clone());
        chips.push(json!({ "key": "decade", "value": lbl, "label": lbl }));
    }
    if let Some(c) = crate::re!(r"\b(?:after|since|from)\s+((?:19|20)\d\d)\b").captures(&q) { f.insert("y1".into(), c[1].to_string()); labels.push(format!("since {}", &c[1])); chips.push(json!({ "key": "y1", "value": &c[1], "label": format!("Since {}", &c[1]) })); }
    if let Some(c) = crate::re!(r"\bbefore\s+((?:19|20)\d\d)\b").captures(&q) { f.insert("y2".into(), c[1].to_string()); labels.push(format!("before {}", &c[1])); chips.push(json!({ "key": "y2", "value": &c[1], "label": format!("Before {}", &c[1]) })); }
    if crate::re!(r"\b(new|recent|latest)\b").is_match(&q) { f.insert("y1".into(), (year() - 2).to_string()); labels.push("recent".into()); chips.push(json!({ "key": "y1", "value": (year() - 2).to_string(), "label": "Recent" })); }
    if crate::re!(r"\b(classic|old)\b").is_match(&q) { f.insert("y2".into(), "1990".into()); labels.push("classic".into()); chips.push(json!({ "key": "y2", "value": "1990", "label": "Classic" })); }
    if crate::re!(r"\b(short|quick)\b").is_match(&q) { f.insert("rtmax".into(), "100".into()); labels.push("short".into()); chips.push(json!({ "key": "rtmax", "value": "100", "label": "Under 100m" })); }
    if crate::re!(r"\b(best|top|great|acclaimed|good)\b").is_match(&q) { f.insert("rmin".into(), "7.2".into()); labels.push("highly rated".into()); chips.push(json!({ "key": "rmin", "value": "7.2", "label": "7.2+" })); }
    for (re, l) in nl_lang { if let Some(c) = re.find(&q) { f.insert("lang".into(), l.to_string()); labels.push(c.as_str().into()); chips.push(json!({ "key": "lang", "value": l, "label": c.as_str() })); } }
    for (re, c2) in nl_country { if let Some(c) = re.find(&q) { f.insert("country".into(), c2.to_string()); labels.push(c.as_str().into()); chips.push(json!({ "key": "country", "value": c2, "label": c.as_str() })); } }
    let mood_hit = |id: &str| match id { "mind" => crate::re!(r"twist|mind").is_match(&q), "true" => q.contains("true stor"), "dark" => crate::re!(r"\b(dark|gritty)\b").is_match(&q), "feel" => crate::re!(r"feel[- ]good|uplifting|wholesome").is_match(&q), "edge" => crate::re!(r"edge of|tense|gripping").is_match(&q), "epic" => crate::re!(r"\bepic\b").is_match(&q), _ => false };
    let hits: Vec<&Mood> = MOODS.iter().filter(|m| mood_hit(m.id)).collect();
    if !hits.is_empty() {
        let names: Vec<&str> = hits.iter().flat_map(|m| m.kws.iter().copied()).collect();
        let kw_s = keyword_ids(app, &names).await.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
        f.insert("kw".into(), kw_s.clone());
        for m in &hits {
            labels.push(m.title.to_string());
            chips.push(json!({ "key": "mood", "value": m.id, "label": m.title }));
        }
    }
    // "like X"
    if let Some(c) = crate::re!(r"\b(?:like|similar to)\s+(.+)$").captures(&q) {
        let name = crate::re!(r"\b(but|and|with)\b.*$").replace(&c[1], "").trim().to_string();
        let cat = app.catalog(); let (mv, sr, _) = cat.search(&name, false, 3);
        let src = if sr.first().is_some() && (mv.first().is_none() || kind == Some("tv")) { sr.first().copied() } else { mv.first().copied() };
        if let Some(src) = src.filter(|x| x.tmdb.is_some()) {
            let mt = if src.k == "s" { "tv" } else { "movie" }; let t = src.tmdb.clone().unwrap();
            let (a, b) = tokio::join!(app.tmdb.get(format!("/{mt}/{t}/recommendations"), crate::tmdb::NO, TTL_D), app.tmdb.get(format!("/{mt}/{t}/similar"), crate::tmdb::NO, TTL_D));
            let mut all: Vec<Value> = a.as_ref().and_then(|x| x["results"].as_array()).cloned().unwrap_or_default();
            all.extend(b.as_ref().and_then(|x| x["results"].as_array()).cloned().unwrap_or_default());
            let mut items = match_results(app, &all, Some(mt), 40);
            let want: HashSet<i64> = (if mt == "tv" { &gt } else { &gm }).iter().copied().collect();
            if !want.is_empty() { items.retain(|x| app.meta_get(mt, &s(&x["tmdb"])).map_or(false, |m| m.g.iter().any(|g| want.contains(g)))); }
            let extra = if labels.is_empty() { String::new() } else { format!(" · {}", labels.join(" · ")) };
            return Some(json!({ "label": format!("Like {}{extra}", src.title), "items": items, "chips": chips }));
        }
    }
    // "with <actor>"
    if let Some(c) = crate::re!(r"\b(?:with|starring|featuring|by|directed by)\s+([a-z .'-]{4,})$").captures(&q) {
        let ppl = people(app, c[1].trim()).await;
        if let Some(p0) = ppl.first() {
            if let Some(p) = person(app, &p0["id"].to_string()).await {
                let mut items: Vec<Value> = p["acting"].as_array().cloned().unwrap_or_default(); items.extend(p["crew"].as_array().cloned().unwrap_or_default());
                if let Some(k) = kind { items.retain(|x| (k == "tv") == (x["k"] == "s")); }
                items.truncate(40);
                return Some(json!({ "label": format!("With {}", s(&p["name"])), "items": items, "person": p0, "chips": chips }));
            }
        }
    }

    let strong_single = !gm.is_empty() || f.contains_key("kw") || doc_topic.is_some() || is_mini || is_uhd || strong_platform;
    if labels.is_empty() || (labels.len() == 1 && !strong_single) { return None; }

    let cat = app.catalog();
    let exact_hits = exact_title_cards(app, &cat, q0);
    let leftover = has_leftover_title_words(&q);
    // Avoid hijacking literal non-filter titles like `"Doc Holliday"` or `"Space Cowboys"` when only 1 incidental keyword matched!
    if exact_hits.is_empty() && leftover && labels.len() == 1 {
        return None;
    }

    let label = labels.iter().map(|l| {
        let mut c = l.chars();
        c.next().map(|ch| ch.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" · ");

    let nl_items: Vec<Value> = if let Some(topic) = doc_topic {
        let want_tv = kind != Some("movie");
        let want_mv = kind != Some("tv");
        let y1 = f.get("y1").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let y2 = f.get("y2").and_then(|v| v.parse::<i32>().ok()).unwrap_or(9999);
        let rmin = f.get("rmin").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let rtmax = f.get("rtmax").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let uhd_req = f.get("uhd").map(|v| v == "1").unwrap_or(false);
        let plat_req = f.get("platform").cloned().unwrap_or_default();
        let lang_req = f.get("lang").cloned().unwrap_or_default();
        let country_req = f.get("country").cloned().unwrap_or_default();
        let status_req = f.get("status").cloned().unwrap_or_default();
        let langs: Vec<&str> = if lang_req == "nordic" { vec!["sv", "da", "no", "nb", "fi", "is"] } else { vec![lang_req.as_str()] };
        let hide_cams = hide_cams_enabled(app);
        let disliked = crate::rec::hidden_ratings(app);
        let mut scored: Vec<(&Item, f64)> = vec![];
        for it in cat.movies.iter().filter(|_| want_mv).chain(cat.series.iter().filter(|_| want_tv)) {
            if hide_cams && crate::showcase::is_cam_only(app, it) { continue; }
            if let Some(t) = &it.tmdb {
                let mt = if it.k == "s" { "tv" } else { "movie" };
                if disliked.contains(&format!("{mt}:{t}")) { continue; }
            }
            if !crate::docs::is_doc_item(app, it) { continue; }
            if !crate::docs::doc_topics(app, it).contains(&topic) { continue; }
            if uhd_req && !((!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd)) { continue; }
            if !plat_req.is_empty() && !cat.item_has_platform(it, &plat_req) { continue; }
            let m = app.meta_of(it);
            let yr = m.as_ref().map(|m| m.year()).filter(|&y| y > 0).or(it.year).unwrap_or(0);
            if y1 > 0 && yr < y1 { continue; }
            if y2 < 9999 && (yr == 0 || yr > y2) { continue; }
            if rmin > 0.0 && m.as_ref().map_or(it.rating < rmin, |m| m.va < rmin || m.vc < 25) { continue; }
            if rtmax > 0 && it.k == "m" && m.as_ref().map_or(true, |m| m.rt == 0 || m.rt > rtmax) { continue; }
            if !lang_req.is_empty() {
                let ml = m.as_ref().map(|m| m.l.as_str()).unwrap_or("");
                match lang_req.as_str() {
                    "en" => if ml != "en" { continue; },
                    "nonen" => if ml == "en" { continue; },
                    _ => if !langs.contains(&ml) { continue; },
                }
            }
            if !country_req.is_empty() && !m.as_ref().map_or(false, |m| m.oc.iter().any(|c| c.eq_ignore_ascii_case(&country_req))) { continue; }
            if status_req == "miniseries" && !(it.k == "s" && m.as_ref().map_or(false, |m| m.ns == 1 && m.st.eq_ignore_ascii_case("Ended"))) { continue; }
            let mut sc = m.as_ref().map(|m| m.bayes() + (m.p.max(0.0) + 1.0).log10() * 0.4).unwrap_or(it.rating);
            if space_hint {
                let ov = m.as_ref().map(|m| m.ov.as_str()).unwrap_or(it.plot.as_str());
                let sp_re = crate::re!(r"(?i)\b(space|cosmos|universe|nasa|planet|astronaut|mars|moon|galaxy|star|apollo|rocket|orbit|solar)\b");
                if sp_re.is_match(&it.title) || sp_re.is_match(ov) { sc += 3.0; }
            }
            scored.push((it, sc));
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.into_iter().take(40).map(|(it, _)| {
            let mut c = app.card_meta(it);
            c["isDoc"] = json!(true);
            let slugs = crate::docs::doc_topics(app, it);
            let dt = crate::docs::doc_topic_labels(app, it);
            if !dt.is_empty() { c["docTopics"] = json!(dt); }
            if !slugs.is_empty() { c["docTopicSlugs"] = json!(slugs); }
            c
        }).collect()
    } else if let Some(k) = kind {
        let mut p = f.clone();
        p.insert("kind".into(), k.into());
        p.insert("sort".into(), "rating".into());
        p.insert("per".into(), "40".into());
        let g = if k == "tv" { &gt } else { &gm };
        if !g.is_empty() { p.insert("g".into(), g.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")); }
        if k == "tv" { p.remove("rtmax"); }
        browse(app, &p)["items"].as_array().cloned().unwrap_or_default()
    } else {
        // Interleave movies + series when kind == None!
        let mut pm = f.clone();
        pm.insert("kind".into(), "movie".into());
        pm.insert("sort".into(), "rating".into());
        pm.insert("per".into(), "24".into());
        if !gm.is_empty() { pm.insert("g".into(), gm.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")); }
        let mv_out = browse(app, &pm)["items"].as_array().cloned().unwrap_or_default();

        let mut pt = f.clone();
        pt.insert("kind".into(), "tv".into());
        pt.insert("sort".into(), "rating".into());
        pt.insert("per".into(), "24".into());
        if !gt.is_empty() { pt.insert("g".into(), gt.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")); }
        pt.remove("rtmax");
        let tv_out = browse(app, &pt)["items"].as_array().cloned().unwrap_or_default();

        interleave_items(mv_out, tv_out, 40)
    };

    // If literal exact title matches exist (e.g. "True Crime", "War of the Worlds", "Love Story", "Space Jam",
    // "Doc Martin", "Adventure Time"), NEVER let NL filters hijack them: place exact title matches FIRST and
    // expose the parsed NL filters as optional `chips` + `filteredItems`!
    if !exact_hits.is_empty() {
        let mut combined = Vec::with_capacity(exact_hits.len() + nl_items.len());
        let mut seen_keys: HashSet<String> = HashSet::new();
        for c in &exact_hits {
            let ck = format!("{}:{}", s(&c["k"]), s(&c["id"]));
            if seen_keys.insert(ck) { combined.push(c.clone()); }
        }
        for c in &nl_items {
            if combined.len() >= 40 { break; }
            let ck = format!("{}:{}", s(&c["k"]), s(&c["id"]));
            if seen_keys.insert(ck) { combined.push(c.clone()); }
        }
        return Some(json!({
            "label": label,
            "items": combined,
            "exactFirst": true,
            "optional": true,
            "exactMatches": exact_hits,
            "filteredItems": nl_items,
            "chips": chips,
            "filters": f
        }));
    }

    Some(json!({ "label": label, "items": nl_items, "chips": chips, "filters": f }))
}

fn db_meta<'a>(db: &'a HashMap<String, Meta>, it: &Item) -> Option<&'a Meta> {
    it.tmdb
        .as_ref()
        .and_then(|t| db.get(&format!("{}:{t}", if it.k == "s" { "tv" } else { "movie" })))
        .filter(|m| !m.missing)
}

fn sort_search_by<T>(app: &App, list: &mut [T], sort: &str, item_of: impl Fn(&T) -> &Item) {
    if sort.is_empty() || sort == "relevance" { return; }
    let db = app.meta.read().unwrap();
    match sort {
        "rating" => list.sort_by(|a, b| {
            let (ia, ib) = (item_of(a), item_of(b));
            let sa = db_meta(&db, ia).map(|m| m.bayes()).unwrap_or(ia.rating);
            let sb = db_meta(&db, ib).map(|m| m.bayes()).unwrap_or(ib.rating);
            sb.total_cmp(&sa)
        }),
        "newest" => list.sort_by(|a, b| {
            let (ia, ib) = (item_of(a), item_of(b));
            let da = db_meta(&db, ia).map(|m| m.d.as_str()).unwrap_or("");
            let db_d = db_meta(&db, ib).map(|m| m.d.as_str()).unwrap_or("");
            db_d.cmp(da).then(ib.year.cmp(&ia.year))
        }),
        "popular" => list.sort_by(|a, b| {
            let (ia, ib) = (item_of(a), item_of(b));
            let pa = db_meta(&db, ia).map(|m| m.p).unwrap_or(ia.rating);
            let pb = db_meta(&db, ib).map(|m| m.p).unwrap_or(ib.rating);
            pb.total_cmp(&pa)
        }),
        _ => {}
    }
}

/// Deep multi-bucket search across Movies, Series, Documentaries, Anime, Synopsis/Cast/Director/Topic hits,
/// Live TV (channel + full 48h & catch-up EPG), People, Plain-English Smart search, and Missing/Wanted.
pub async fn deep_search(app: Arc<App>, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let qs = get("q").trim().to_string();
    if qs.is_empty() {
        return json!({
            "movies": [], "series": [], "docs": [], "anime": [], "topicHits": [],
            "fuzzy": false, "people": [], "smart": Value::Null, "missing": [], "live": [],
            "counts": { "movies": 0, "series": 0, "docs": 0, "anime": 0, "topicHits": 0, "people": 0, "live": 0, "missing": 0, "total": 0 }
        });
    }
    let all = get("all") == "1";
    let uhd = get("uhd") == "1";
    let rmin = get("rmin").parse::<f64>().unwrap_or(0.0);
    let y1 = get("y1").parse::<i32>().unwrap_or(0);
    let y2 = { let v = get("y2").parse::<i32>().unwrap_or(0); if v == 0 { 9999 } else { v } };
    let platform = get("platform").trim().to_string();
    let sort = get("sort").trim().to_lowercase();

    let (movies, series, docs, anime, topic_hits, live, fuzzy, unique_cat) = {
        let cat = app.catalog();
        let q_norm = norm(&qs);
        let terms: Vec<String> = if !q_norm.is_empty() {
            q_norm.split_whitespace().map(String::from).collect()
        } else {
            qs.to_lowercase().split_whitespace().map(String::from).collect()
        };
        let qq = terms.join(" ");

        let check_filters = |it: &Item, m: Option<&Meta>| -> bool {
            if uhd && !((!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd)) { return false; }
            if !platform.is_empty() && !cat.item_has_platform(it, &platform) { return false; }
            if y1 > 0 || y2 < 9999 {
                let yr = m.map(|m| m.year()).filter(|&y| y > 0).or(it.year).unwrap_or(0);
                if y1 > 0 && yr < y1 { return false; }
                if y2 < 9999 && (yr == 0 || yr > y2) { return false; }
            }
            if rmin > 0.0 {
                let r = m.map(|m| if m.va > 0.0 { m.va } else { it.rating }).unwrap_or(it.rating);
                if r < rmin { return false; }
            }
            true
        };

        // Unified rich search: diacritics, punctuation folding, acronyms, short-word OSA typos, and meta.ot (romaji/orig_title)!
        let (mut mv_items, mut sr_items, lv_raw, mut title_keys) = {
            let db = app.meta.read().unwrap();
            let (mv_raw, sr_raw, lv_raw) = cat.search_rich(&qs, all, 100, |it| {
                db_meta(&db, it).map(|m| m.ot.as_str()).filter(|s| !s.is_empty())
            });
            let mut tk: HashSet<(&'static str, i64)> = HashSet::with_capacity(mv_raw.len() + sr_raw.len());
            for it in mv_raw.iter().chain(sr_raw.iter()) { tk.insert((it.k, it.id)); }
            let mv: Vec<&Item> = mv_raw.into_iter().filter(|it| check_filters(it, db_meta(&db, it))).collect();
            let sr: Vec<&Item> = sr_raw.into_iter().filter(|it| check_filters(it, db_meta(&db, it))).collect();
            (mv, sr, lv_raw, tk)
        };

        let mut fuzzy = !mv_items.is_empty() || !sr_items.is_empty();
        if fuzzy && !terms.is_empty() {
            // Only mark `fuzzy = true` if the top hit wasn't a plain folded substring match
            let top_exact = mv_items.first().or(sr_items.first()).map_or(false, |it| {
                let tn = norm(&it.title);
                terms.iter().all(|w| tn.contains(w.as_str()))
            });
            if top_exact { fuzzy = false; }
        }
        if mv_items.len() + sr_items.len() < 4 && qs.len() >= 4 {
            let fz = fuzzy_items(&app, &cat, &qs, 24);
            let db = app.meta.read().unwrap();
            for it in fz {
                if !title_keys.contains(&(it.k, it.id)) && check_filters(it, db_meta(&db, it)) {
                    title_keys.insert((it.k, it.id));
                    fuzzy = true;
                    if it.k == "s" { sr_items.push(it); } else { mv_items.push(it); }
                }
            }
        }

        // Synopsis / Director / Cast / Keyword / Subject matches from meta.json (+ docTopics)
        let mut topic_cands: Vec<(&Item, f64)> = vec![];
        if !terms.is_empty() && qs.len() >= 2 {
            let doc_hint_cats: HashSet<(&str, &str)> = cat.cats.values().filter(|c| {
                let n = &c.name;
                contains_ci(n, "doc", false) || contains_ci(n, "discovery", false)
                    || contains_ci(n, "nat geo", false) || contains_ci(n, "history", false)
                    || contains_ci(n, "science", false) || contains_ci(n, "crime", false)
                    || contains_ci(n, "bbc", false) || contains_ci(n, "animal", false)
            }).map(|c| (if c.kind == "series" { "s" } else { "m" }, c.id.as_str())).collect();

            let mut doc_extra: Vec<&Item> = vec![];
            {
                let db = app.meta.read().unwrap();
                for it in cat.movies.iter().chain(cat.series.iter()).filter(|it| all || it.en) {
                    if title_keys.contains(&(it.k, it.id)) { continue; }
                    let mt = if it.k == "s" { "tv" } else { "movie" };
                    let m = it.tmdb.as_ref().and_then(|t| db.get(&format!("{mt}:{t}"))).filter(|m| !m.missing);
                    if !check_filters(it, m) { continue; }
                    if item_matches_text(&cat, it, m, &terms, true) {
                        let ov_txt = m.map(|m| m.ov.as_str()).filter(|s| !s.is_empty()).unwrap_or(it.plot.as_str());
                        let phrase_bonus = if terms.len() > 1 && contains_ci(ov_txt, &qq, false) { 1.2 } else { 0.0 };
                        // Strong local director / cast credit boost so searching a director/actor works offline too!
                        let credit_bonus = m.map(|m| {
                            let dir_hit = m.dir.iter().any(|d| { let dn = norm(d); terms.iter().all(|w| dn.contains(w.as_str())) });
                            let cast_hit = m.cast.iter().any(|c| { let cn = norm(c); terms.iter().all(|w| cn.contains(w.as_str())) });
                            (if dir_hit { 2.5 } else { 0.0 }) + (if cast_hit { 1.8 } else { 0.0 })
                        }).unwrap_or(0.0);
                        let sc = m.map(|m| m.bayes() * 1.3 + (m.p.max(0.0) + 1.0).log10() * 0.9).unwrap_or(it.rating) + phrase_bonus + credit_bonus;
                        topic_cands.push((it, sc));
                    } else if m.map_or(false, |m| m.g.contains(&99)) || doc_hint_cats.contains(&(it.k, it.cat.as_str())) {
                        doc_extra.push(it);
                    }
                }
            }
            for it in doc_extra {
                if crate::docs::is_doc_item(&app, it) {
                    let d_slugs = crate::docs::doc_topics(&app, it);
                    let d_labels = crate::docs::doc_topic_labels(&app, it);
                    if !d_slugs.is_empty() || !d_labels.is_empty() {
                        let m = app.meta_of(it);
                        let all_ok = terms.iter().all(|w| {
                            let ws_lbl = w.len() <= 2;
                            d_slugs.iter().any(|t| contains_ci(t, w, ws_lbl))
                                || d_labels.iter().any(|l| contains_ci(l, w, ws_lbl))
                                || item_matches_text(&cat, it, m.as_ref(), std::slice::from_ref(w), true)
                        });
                        if all_ok {
                            let sc = m.as_ref().map(|m| m.bayes() * 1.3 + (m.p.max(0.0) + 1.0).log10() * 0.9).unwrap_or(it.rating) + 0.4;
                            topic_cands.push((it, sc));
                        }
                    }
                }
            }
            topic_cands.sort_by(|a, b| b.1.total_cmp(&a.1));
        }

        // Dedicated Documentaries & Anime buckets from title + synopsis matches
        let mut docs_items: Vec<&Item> = vec![];
        let mut anime_items: Vec<(&Item, &'static str, usize)> = vec![];
        let mut seen_docs: HashSet<(&'static str, i64)> = HashSet::new();
        let mut seen_anime: HashSet<(&'static str, i64)> = HashSet::new();
        for it in mv_items.iter().copied().chain(sr_items.iter().copied()).chain(topic_cands.iter().take(80).map(|(it, _)| *it)) {
            if seen_docs.insert((it.k, it.id)) && crate::docs::is_doc_item(&app, it) {
                docs_items.push(it);
            }
            if seen_anime.insert((it.k, it.id)) && crate::anime::is_anime_item(&app, it) {
                let (dub, dc) = if it.k == "s" {
                    let (st, y, _) = crate::anime::series_dub_summary(&app, it.id);
                    (st, y)
                } else {
                    (crate::anime::movie_dub_status(&app, it), 0)
                };
                if dub != "no" {
                    anime_items.push((it, dub, dc));
                }
            }
        }

        let mut topic_items: Vec<&Item> = topic_cands.into_iter().take(36).map(|(it, _)| it).collect();

        sort_search_by(&app, &mut mv_items, &sort, |x| *x);
        sort_search_by(&app, &mut sr_items, &sort, |x| *x);
        sort_search_by(&app, &mut docs_items, &sort, |x| *x);
        sort_search_by(&app, &mut anime_items, &sort, |x| x.0);
        sort_search_by(&app, &mut topic_items, &sort, |x| *x);

        mv_items.truncate(60);
        sr_items.truncate(60);
        docs_items.truncate(60);
        anime_items.truncate(60);

        let mut uniq: HashSet<(&'static str, i64)> = HashSet::new();
        for it in mv_items.iter().chain(sr_items.iter()).chain(docs_items.iter()).chain(topic_items.iter()) {
            uniq.insert((it.k, it.id));
        }
        for (it, _, _) in &anime_items { uniq.insert((it.k, it.id)); }
        let unique_cat = uniq.len();

        let movies: Vec<Value> = mv_items.into_iter().map(|x| app.card_meta(x)).collect();
        let series: Vec<Value> = sr_items.into_iter().map(|x| app.card_meta(x)).collect();
        let docs: Vec<Value> = docs_items.into_iter().map(|it| {
            let mut c = app.card_meta(it);
            c["isDoc"] = json!(true);
            let slugs = crate::docs::doc_topics(&app, it);
            let dt = crate::docs::doc_topic_labels(&app, it);
            if !dt.is_empty() { c["docTopics"] = json!(dt); }
            if !slugs.is_empty() { c["docTopicSlugs"] = json!(slugs); }
            c
        }).collect();
        let anime: Vec<Value> = anime_items.into_iter().map(|(it, dub, dc)| {
            let mut c = app.card_meta(it);
            c["isAnime"] = json!(true);
            c["dubStatus"] = json!(dub);
            if dc > 0 { c["dubbedCount"] = json!(dc); }
            c
        }).collect();
        let topic_hits: Vec<Value> = topic_items.into_iter().map(|x| app.card_meta(x)).collect();

        // Live TV search: channel name OR full 48h upcoming + live + catch-up EPG programme match!
        let mut live_cards: Vec<Value> = vec![];
        let mut seen_live: HashSet<i64> = HashSet::new();
        for ch in lv_raw.into_iter().take(60) {
            if seen_live.insert(ch.id) {
                live_cards.push(crate::sports::channel_json(&app, ch));
            }
        }
        if live_cards.len() < 60 && !terms.is_empty() {
            let epg_map = app.epg.read().unwrap().clone();
            let now = now_ms();
            let horizon = now + 48 * 3_600_000;
            let matches_prog = |t: &str| terms.iter().all(|w| contains_ci(t, w, w.len() <= 2));
            let mut epg_hits: Vec<(u8, bool, i64, &Item, Value)> = vec![];
            for ch in cat.live.iter().filter(|ch| (all || ch.en || ch.sport) && !ch.epg.is_empty() && !seen_live.contains(&ch.id)) {
                let Some(progs) = epg_map.get(&ch.epg) else { continue };
                let mut best_hit: Option<(u8, i64, Value)> = None;
                let cu_cutoff = if ch.archive > 0 { now - ch.archive * 86_400_000 } else { i64::MAX };
                for p in progs {
                    if !matches_prog(&p.title) { continue; }
                    let (prio, rank_t, status) = if p.s <= now && p.e > now {
                        (3u8, 0i64, "live")
                    } else if p.s > now && p.s <= horizon {
                        (2u8, -(p.s - now), "upcoming")
                    } else if p.e <= now && ch.archive > 0 && p.s >= cu_cutoff {
                        (1u8, p.s, "catchup")
                    } else {
                        continue;
                    };
                    let better = best_hit.as_ref().map_or(true, |(bp, bt, _)| prio > *bp || (prio == *bp && rank_t > *bt));
                    if better {
                        best_hit = Some((prio, rank_t, json!({ "title": p.title, "s": p.s, "e": p.e, "status": status })));
                    }
                }
                if let Some((prio, rank_t, mval)) = best_hit {
                    epg_hits.push((prio, ch.en, rank_t, ch, mval));
                }
            }
            epg_hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
            for (_, _, _, ch, mval) in epg_hits {
                if live_cards.len() >= 60 { break; }
                if seen_live.insert(ch.id) {
                    let mut card = crate::sports::channel_json(&app, ch);
                    card["epgMatch"] = mval;
                    live_cards.push(card);
                }
            }
        }
        (movies, series, docs, anime, topic_hits, live_cards, fuzzy, unique_cat)
    };

    let (people, smart, missing) = tokio::join!(people(&app, &qs), smart(&app, &qs), missing(&app, &qs));
    let total = unique_cat + live.len() + people.len();
    json!({
        "movies": movies,
        "series": series,
        "docs": docs,
        "anime": anime,
        "topicHits": topic_hits,
        "fuzzy": fuzzy,
        "people": people,
        "smart": smart,
        "missing": missing,
        "live": live,
        "counts": {
            "movies": movies.len(),
            "series": series.len(),
            "docs": docs.len(),
            "anime": anime.len(),
            "topicHits": topic_hits.len(),
            "people": people.len(),
            "live": live.len(),
            "missing": missing.len(),
            "total": total,
        }
    })
}

// ---------- wanted list
fn wanted_satisfied(target: &str, cam_only: bool, has_4k: bool, has_dub: bool) -> bool {
    if cam_only { return false; }
    match target {
        "4k" => has_4k,
        "dub" => has_dub,
        _ => true, // "clean" (default): any non-CAM release satisfies
    }
}

pub fn check_wanted(app: &App) {
    let cat = app.catalog();
    let mut alerts: Vec<(&'static str, i64, String, String)> = vec![];
    let mut dirty = false;
    app.state.with(|st| {
        let keys: Vec<String> = st["wanted"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        for k in keys {
            let Some((mt, id)) = k.split_once(':') else { continue };
            let Some(it) = cat.by_tmdb(mt, id) else { continue };
            let target = {
                let raw = s(&st["wanted"][&k]["target"]).trim().to_lowercase();
                if raw.is_empty() { "clean".to_string() } else { raw }
            };
            let cam_only = crate::showcase::is_cam_only(app, it);
            if cam_only {
                // Keep in Wanted! Flag `camOnly: true` instead of firing a bogus "Now available" toast.
                if st["wanted"][&k]["camOnly"].as_bool() != Some(true) {
                    st["wanted"][&k]["camOnly"] = json!(true);
                    dirty = true;
                }
                continue;
            }
            let has_4k = crate::showcase::best_tier(app, it) >= crate::showcase::QualityTier::Uhd4k;
            let has_dub = if it.k == "s" {
                crate::anime::series_dub_summary(app, it.id).0 == "yes"
            } else {
                crate::anime::movie_dub_status(app, it) == "yes"
            };
            if !wanted_satisfied(&target, false, has_4k, has_dub) {
                if st["wanted"][&k]["camOnly"].as_bool() == Some(true) {
                    st["wanted"][&k]["camOnly"] = json!(false);
                    dirty = true;
                }
                if target == "4k" && st["wanted"][&k]["hdAvailable"].as_bool() != Some(true) {
                    st["wanted"][&k]["hdAvailable"] = json!(true);
                    dirty = true;
                }
                continue;
            }
            let poster = { let p = s(&st["wanted"][&k]["poster"]); if p.is_empty() { it.poster.clone() } else { p } };
            st["arrived"][&k] = json!({ "k": it.k, "id": it.id, "title": it.title, "poster": poster, "target": target, "t": now_ms() });
            st["wanted"].as_object_mut().unwrap().remove(&k);
            dirty = true;
            alerts.push((it.k, it.id, it.title.clone(), target));
        }
        let now = now_ms();
        if let Some(m) = st["arrived"].as_object_mut() {
            let before = m.len();
            m.retain(|_, a| now - a["t"].as_i64().unwrap_or(0) < 21 * 86_400_000);
            if m.len() != before { dirty = true; }
        }
    });
    if dirty { app.state.save(); }
    let link = format!("http://localhost:{}/", app_port(app));
    for (k, id, t, target) in alerts {
        let heading = match target.as_str() {
            "4k" => format!("Now in 4K: {t}"),
            "dub" => format!("English dub ready: {t}"),
            _ => format!("Now available: {t}"),
        };
        crate::notify_all(app, "wanted", &heading, "From your Wanted list", &link,
            json!({ "k": k, "id": id, "title": t, "label": "View title" }));
    }
}

pub async fn coming_soon(app: &Arc<App>) -> Vec<Value> {
    let r = app.tmdb.pages("/movie/upcoming", 3, [sp("region", "AU")], 12 * TTL_H).await;
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string(); let cat = app.catalog();
    let mut v: Vec<&Value> = r.iter().filter(|x| x["poster_path"].is_string() && s(&x["release_date"]) >= today && cat.by_tmdb("movie", &x["id"].to_string()).is_none()).collect();
    v.sort_by(|a, b| b["popularity"].as_f64().unwrap_or(0.0).total_cmp(&a["popularity"].as_f64().unwrap_or(0.0)));
    v.into_iter().take(30).map(|x| external("movie", x, is_wanted(app, "movie", &x["id"].to_string()))).collect()
}

pub async fn surprise(app: &Arc<App>, kind: &str) -> Value {
    let fy = app.memo("foryou", 20 * 60_000, crate::rec::for_you).await;
    let mut pool: Vec<Value> = fy.as_array().cloned().unwrap_or_default().into_iter().filter(|x| kind.is_empty() || (kind == "tv") == (x["k"] == "s")).collect();
    if pool.len() < 10 {
        let y = year();
        for tv in if kind.is_empty() { vec![false, true] } else { vec![kind == "tv"] } {
            pool.extend(rank(app, tv, |m| m.bayes() >= 7.0 && m.year() >= y - 15, |m| m.bayes() + (m.p + 1.0).log10(), 80, false));
        }
    }
    if pool.is_empty() { return json!({}); }
    let i = (now_ms() as usize ^ (now_ms() as usize >> 7)) % pool.len();
    pool.swap_remove(i)
}

pub fn peek(app: &App, k: &str, id: i64) -> Value {
    let cat = app.catalog();
    let it = if k == "s" { cat.series_item(id) } else { cat.movie(id) };
    let Some(it) = it else { return json!({}) };
    let mut c = app.card_meta(it); let m = app.meta_of(it);
    c["overview"] = json!(m.as_ref().map(|m| m.ov.clone()).filter(|x| !x.is_empty()).unwrap_or_else(|| it.plot.clone()));
    c["seasons"] = json!(m.as_ref().map(|m| m.ns).unwrap_or(0));
    c["watched"] = json!(it.tmdb.as_ref().map_or(false, |t| watched_set(app).contains(&format!("{}:{t}", if k == "s" { "tv" } else { "movie" }))));
    c
}

/// Watch-Night Concierge ("Pick For Me"): filters strictly from the user's real catalogue,
/// scores candidates by Bayesian rating + popularity + 4K bonus + slight randomness,
/// and returns 3 distinct, metadata-enriched picks with zero slow network blocking.
pub async fn concierge(app: Arc<App>, b: Value) -> Value {
    use rand::Rng;

    let kind_raw = b["kind"].as_str().unwrap_or("movie").trim().to_lowercase();
    let kind = match kind_raw.as_str() {
        "tv" | "series" => "tv",
        "anime" => "anime",
        "docs" | "doc" | "documentary" | "documentaries" => "docs",
        "any" | "all" => "any",
        _ => "movie",
    };
    let mood_raw = b["mood"].as_str().unwrap_or("any").trim().to_lowercase();
    let mood = match mood_raw.as_str() {
        "" | "any" => "any",
        "action" => "action",
        "scifi" | "sci-fi" => "scifi",
        "thriller" => "thriller",
        "comedy" => "comedy",
        "crime" | "true-crime" | "truecrime" => "crime",
        "horror" => "horror",
        "drama" => "drama",
        "feelgood" | "feel-good" | "feel" => "feelgood",
        "mindbender" | "mind-bender" | "mind-bending" | "mind" => "mindbender",
        "nature" | "wildlife" => "nature",
        "history" => "history",
        "science" | "space" => "science",
        "sports" | "sport" => "sports",
        "music" => "music",
        "bio" | "biography" => "bio",
        "society" | "culture" => "society",
        "food" | "cooking" => "food",
        _ => "any",
    };
    let max_min = b.get("maxMin").map(crate::util::i).unwrap_or(0);
    let min_rating = match b.get("minRating") {
        Some(v) if !v.is_null() => crate::util::num(v),
        _ => 7.0,
    };
    let uhd_only = b["uhd"].as_bool().unwrap_or(false);
    let unwatched = b["unwatched"].as_bool().unwrap_or(true);
    let exclude: HashSet<String> = b["exclude"]
        .as_array()
        .into_iter()
        .flatten()
        .map(s)
        .filter(|x| !x.is_empty())
        .collect();

    // Resolve cached TMDB keyword IDs for keyword-driven moods before sync locks / ThreadRng.
    let mood_kws: HashSet<i64> = match mood {
        "feelgood" => keyword_ids(&app, &["feel-good", "heartwarming", "uplifting", "wholesome", "friendship"]).await.into_iter().collect(),
        "mindbender" => keyword_ids(&app, &["mind-bending", "plot twist", "time loop", "alternate reality", "nonlinear timeline", "surrealism", "parallel world", "dream"]).await.into_iter().collect(),
        "thriller" => keyword_ids(&app, &["suspense", "tense", "survival", "heist", "manhunt", "hostage", "cat and mouse"]).await.into_iter().collect(),
        _ => HashSet::new(),
    };

    let cat = app.catalog();
    if cat.loaded_at == 0 {
        return json!({ "totalMatches": 0, "picks": [] });
    }

    // Build watched + in-progress exclusion sets when `unwatched` is true.
    let (seen_tmdb, in_progress_keys): (HashSet<String>, HashSet<String>) = if unwatched {
        let mut tmdb_set = watched_set(&app);
        let mut prog_keys = HashSet::new();
        app.state.read(|st| {
            for (key, p) in st["progress"].as_object().into_iter().flatten() {
                let pos = p["pos"].as_f64().unwrap_or(0.0);
                let dur = p["dur"].as_f64().unwrap_or(0.0);
                let active_or_done = pos >= 20.0 || (dur > 0.0 && pos / dur >= 0.02);
                if !active_or_done { continue; }
                let ik = p["item"]["k"].as_str().unwrap_or("");
                if key.starts_with("m:") || ik == "m" {
                    let raw_id = key.strip_prefix("m:").and_then(|x| x.parse::<i64>().ok())
                        .or_else(|| p["item"]["id"].as_i64())
                        .unwrap_or(0);
                    if raw_id > 0 {
                        prog_keys.insert(format!("m:{raw_id}"));
                        if let Some(m_it) = cat.movie(raw_id) {
                            prog_keys.insert(format!("m:{}", m_it.id));
                            if let Some(t) = &m_it.tmdb { tmdb_set.insert(format!("movie:{t}")); }
                        }
                    }
                    let pt = s(&p["tmdb"]);
                    if !pt.is_empty() { tmdb_set.insert(format!("movie:{pt}")); }
                } else if key.starts_with("e:") || key.starts_with("s:") || ik == "e" || ik == "s" {
                    let raw_sid = p["item"]["sid"].as_i64()
                        .or_else(|| p["item"]["sid"].as_str().and_then(|x| x.parse().ok()))
                        .or_else(|| if ik == "s" { p["item"]["id"].as_i64() } else { None })
                        .or_else(|| key.strip_prefix("s:").and_then(|x| x.parse().ok()))
                        .unwrap_or(0);
                    if raw_sid > 0 {
                        prog_keys.insert(format!("s:{raw_sid}"));
                        if let Some(s_it) = cat.series_item(raw_sid) {
                            prog_keys.insert(format!("s:{}", s_it.id));
                            if let Some(t) = &s_it.tmdb { tmdb_set.insert(format!("tv:{t}")); }
                        }
                    }
                    let pt = s(&p["tmdb"]);
                    if !pt.is_empty() { tmdb_set.insert(format!("tv:{pt}")); }
                }
            }
        });
        (tmdb_set, prog_keys)
    } else {
        (HashSet::new(), HashSet::new())
    };

    let hide_cams = hide_cams_enabled(&app);
    let disliked = crate::rec::hidden_ratings(&app);
    let mut hide = hide_prefs(&app);
    if kind == "anime" { hide.retain(|h| h != "anime"); }
    if kind == "docs" { hide.retain(|h| h != "docs"); }
    if mood == "horror" { hide.retain(|h| h != "horror"); }
    let su = standup_kw(&app);

    let pool: Vec<(&Item, bool, &'static str)> = match kind {
        "movie" => cat.movies.iter().filter(|it| it.en && !it.sport).map(|it| (it, false, "")).collect(),
        "tv" => cat.series.iter().filter(|it| it.en && !it.sport).map(|it| (it, false, "")).collect(),
        "docs" => cat.movies.iter().chain(cat.series.iter()).filter(|it| crate::docs::is_doc_item(&app, it)).map(|it| (it, false, "")).collect(),
        "anime" => {
            let mut v = vec![];
            for it in cat.series.iter().filter(|it| crate::anime::is_anime_item(&app, it)) {
                let (dub, _, _) = crate::anime::series_dub_summary(&app, it.id);
                if dub != "no" { v.push((it, true, dub)); }
            }
            for it in cat.movies.iter().filter(|it| crate::anime::is_anime_item(&app, it)) {
                let dub = crate::anime::movie_dub_status(&app, it);
                if dub != "no" { v.push((it, true, dub)); }
            }
            v
        }
        _ => cat.movies.iter().chain(cat.series.iter()).filter(|it| it.en && !it.sport).map(|it| (it, false, "")).collect(),
    };

    struct Cand<'a> {
        it: &'a Item,
        score: f64,
        col_id: Option<i64>,
        is_anime: bool,
        dub: &'static str,
    }

    let (total_matches, chosen): (usize, Vec<(&Item, bool, &'static str)>) = {
        let mut rng = rand::thread_rng();
        let mut total = 0usize;
        let mut cands: Vec<Cand<'_>> = vec![];

        for (it, is_an, dub) in pool {
            if hide_cams && crate::showcase::is_cam_only(&app, it) { continue; }
            let Some(m) = app.meta_of(it) else { continue };
            let tv = it.k == "s";
            let mt = if tv { "tv" } else { "movie" };
            let tmdb_key = it.tmdb.as_ref().map(|t| format!("{mt}:{t}")).unwrap_or_default();
            let item_key = format!("{}:{}", it.k, it.id);

            if !tmdb_key.is_empty() && disliked.contains(&tmdb_key) { continue; }
            if unwatched {
                if in_progress_keys.contains(&item_key) { continue; }
                if !tmdb_key.is_empty() && seen_tmdb.contains(&tmdb_key) { continue; }
            }

            let is_uhd = (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd);
            if uhd_only && !is_uhd { continue; }

            if max_min > 0 && max_min < 900 {
                if tv {
                    if m.rt > max_min { continue; }
                } else if m.rt == 0 || m.rt > max_min {
                    continue;
                }
            }

            if min_rating > 0.0 {
                let min_vc = if kind == "docs" { 25 } else if is_an || tv { 30 } else { 50 };
                if m.va < min_rating || m.vc < min_vc { continue; }
            }

            if !hide.is_empty() {
                let cat_name = cat.cat_name(if tv { "series" } else { "movie" }, &it.cat);
                if hidden_by(&hide, &m.g, &m.l, &cat_name, &m.kw, &su) { continue; }
            }

            let mood_bonus: f64 = match mood {
                "any" => 0.0,
                "action" => {
                    if !(m.g.contains(&28) || m.g.contains(&10759)) { continue; }
                    0.0
                }
                "scifi" => {
                    if !(m.g.contains(&878) || m.g.contains(&10765)) { continue; }
                    0.0
                }
                "thriller" => {
                    let kw_hits = m.kw.iter().filter(|k| mood_kws.contains(k)).count();
                    let g_ok = if tv { m.g.contains(&53) || m.g.contains(&9648) || m.g.contains(&80) } else { m.g.contains(&53) };
                    if !g_ok && kw_hits == 0 { continue; }
                    (if m.g.contains(&53) { 0.35 } else { 0.0 }) + (kw_hits.min(3) as f64) * 0.25
                }
                "comedy" => {
                    if !m.g.contains(&35) { continue; }
                    0.0
                }
                "crime" => {
                    if kind == "docs" {
                        if !m.g.contains(&80) && !crate::docs::doc_topics(&app, it).contains(&"crime") { continue; }
                    } else if !m.g.contains(&80) {
                        continue;
                    }
                    0.0
                }
                "horror" => {
                    let ok = if tv {
                        m.g.contains(&27) || ((m.g.contains(&9648) || m.g.contains(&10765))
                            && crate::re!(r"(?i)\b(horror|haunt|ghost|demon|vampire|zombie|supernatural|curse|killer|terror|nightmare|monster|blood|occult|macabre|creepy|evil|sinister|witch|slasher)\b").is_match(&m.ov))
                    } else {
                        m.g.contains(&27)
                    };
                    if !ok { continue; }
                    0.0
                }
                "drama" => {
                    if !m.g.contains(&18) { continue; }
                    0.0
                }
                "feelgood" => {
                    let kw_hits = m.kw.iter().filter(|k| mood_kws.contains(k)).count();
                    let dark = m.g.contains(&27) || m.g.contains(&53) || m.g.contains(&80) || m.g.contains(&10752);
                    let g_ok = !dark && (m.g.contains(&35) || m.g.contains(&10751) || m.g.contains(&10749));
                    if kw_hits == 0 && !g_ok { continue; }
                    if kw_hits > 0 { 1.1 + ((kw_hits - 1).min(3) as f64) * 0.3 } else { 0.0 }
                }
                "mindbender" => {
                    let kw_hits = m.kw.iter().filter(|k| mood_kws.contains(k)).count();
                    let cue_ok = (m.g.contains(&9648) || m.g.contains(&878) || m.g.contains(&10765) || m.g.contains(&53))
                        && crate::re!(r"(?i)\b(twist|reality|dream|memory|memories|time loop|time travel|parallel|dimension|psycho|illusion|simulation|conspiracy|mind[- ]?bend|surreal|paradox|labyrinth|alternate|dystopi|identity|amnesia|hallucinat)\b").is_match(&m.ov);
                    if kw_hits == 0 && !cue_ok { continue; }
                    if kw_hits > 0 { 1.4 + ((kw_hits - 1).min(3) as f64) * 0.3 } else { 0.5 }
                }
                "nature" | "history" | "science" | "sports" | "music" | "bio" | "society" | "food" => {
                    if !crate::docs::doc_topics(&app, it).contains(&mood) { continue; }
                    0.0
                }
                _ => 0.0,
            };

            total += 1;

            if exclude.contains(&item_key) || (!tmdb_key.is_empty() && exclude.contains(&tmdb_key)) {
                continue;
            }

            let bayes = m.bayes();
            let pop = (m.p.max(0.0) + 1.0).log10();
            let uhd_bonus = if is_uhd { 0.45 } else { 0.0 };
            let bd_bonus = if !m.bd.is_empty() || !it.backdrop.is_empty() { 0.3 } else { 0.0 };
            let dub_bonus = if is_an && dub == "yes" { 0.4 } else { 0.0 };
            let jitter: f64 = rng.gen_range(0.0..1.5);
            let score = bayes * 1.35 + pop * 0.65 + uhd_bonus + bd_bonus + dub_bonus + mood_bonus + jitter;

            cands.push(Cand {
                it,
                score,
                col_id: m.col.as_ref().map(|c| c.id),
                is_anime: is_an,
                dub,
            });
        }

        cands.sort_by(|a, b| b.score.total_cmp(&a.score));

        let mut out: Vec<(&Item, bool, &'static str)> = vec![];
        let mut seen_keys: HashSet<String> = HashSet::new();
        let mut seen_cols: HashSet<i64> = HashSet::new();

        for pass in 0..2 {
            for c in &cands {
                if out.len() >= 3 { break; }
                let k = format!("{}:{}", c.it.k, c.it.id);
                if seen_keys.contains(&k) { continue; }
                if pass == 0 {
                    if let Some(cid) = c.col_id {
                        if seen_cols.contains(&cid) { continue; }
                    }
                }
                seen_keys.insert(k);
                if let Some(cid) = c.col_id { seen_cols.insert(cid); }
                out.push((c.it, c.is_anime, c.dub));
            }
            if out.len() >= 3 { break; }
        }

        (total, out)
    };

    let picks: Vec<Value> = chosen.into_iter().map(|(it, is_an, dub)| {
        let mut c = app.card_meta(it);
        let m = app.meta_of(it);
        c["key"] = json!(format!("{}:{}", it.k, it.id));
        if c.get("runtime").is_none() {
            c["runtime"] = json!(m.as_ref().map(|m| m.rt).unwrap_or(0));
        }
        if s(&c["overview"]).is_empty() && !it.plot.is_empty() {
            c["overview"] = json!(it.plot);
        }
        if c["genres"].as_array().map_or(true, |a| a.is_empty()) && !it.genre.is_empty() {
            let gs: Vec<String> = it.genre.split(|ch| ch == ',' || ch == '/').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).take(3).collect();
            c["genres"] = json!(gs);
        }
        c["uhd"] = json!((!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd));
        c["trailer"] = json!(it.trailer);
        if it.k == "s" {
            c["seasons"] = json!(m.as_ref().map(|m| m.ns).unwrap_or(0));
        }
        c["scores"] = m.as_ref().and_then(|m| app.omdb.get(&m.im)).unwrap_or(Value::Null);
        if is_an {
            c["isAnime"] = json!(true);
            c["dubStatus"] = json!(dub);
        }
        if kind == "docs" {
            c["isDoc"] = json!(true);
            let slugs = crate::docs::doc_topics(&app, it);
            let dt = crate::docs::doc_topic_labels(&app, it);
            if !dt.is_empty() { c["docTopics"] = json!(dt); }
            if !slugs.is_empty() { c["docTopicSlugs"] = json!(slugs); }
        }
        c
    }).collect();

    json!({ "totalMatches": total_matches, "picks": picks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decade_distinguishes_2020s_from_1920s() {
        assert_eq!(parse_decade("best 20s sci-fi movies"), Some((2020, 2029)));
        assert_eq!(parse_decade("the 20s"), Some((2020, 2029)));
        assert_eq!(parse_decade("2020s thriller"), Some((2020, 2029)));
        assert_eq!(parse_decade("1920s silent films"), Some((1920, 1929)));
        assert_eq!(parse_decade("90s action movies"), Some((1990, 1999)));
        assert_eq!(parse_decade("00s comedies"), Some((2000, 2009)));
        assert_eq!(parse_decade("10s sci-fi"), Some((2010, 2019)));
        assert_eq!(parse_decade("1980s horror"), Some((1980, 1989)));
    }

    #[test]
    fn wanted_target_blocks_cam_and_honors_4k_and_dub() {
        // CAM-only NEVER satisfies any target!
        assert!(!wanted_satisfied("clean", true, false, false));
        assert!(!wanted_satisfied("4k", true, true, false));
        assert!(!wanted_satisfied("dub", true, false, true));

        // Clean HD release satisfies "clean", not "4k" or "dub"
        assert!(wanted_satisfied("clean", false, false, false));
        assert!(!wanted_satisfied("4k", false, false, false));
        assert!(!wanted_satisfied("dub", false, false, false));

        // Clean 4K satisfies "clean" and "4k"
        assert!(wanted_satisfied("4k", false, true, false));
        // Clean English dub satisfies "dub"
        assert!(wanted_satisfied("dub", false, false, true));
    }

    #[test]
    fn literal_titles_detected_as_leftover_words_and_interleave_alternates() {
        assert!(has_leftover_title_words("war of the worlds"));
        assert!(has_leftover_title_words("space jam"));
        assert!(has_leftover_title_words("doc martin"));
        assert!(has_leftover_title_words("adventure time"));
        assert!(!has_leftover_title_words("best 90s sci-fi movies on netflix"));
        assert!(!has_leftover_title_words("true crime documentaries"));

        let mv = vec![json!("m1"), json!("m2"), json!("m3")];
        let tv = vec![json!("s1"), json!("s2")];
        assert_eq!(interleave_items(mv, tv, 10), vec![json!("m1"), json!("s1"), json!("m2"), json!("s2"), json!("m3")]);
    }

    #[test]
    fn hidden_by_catches_standup_keyword_9716() {
        let mut su = HashSet::new();
        su.insert(9716);
        assert!(hidden_by(&["standup".into()], &[35], "en", "EN - COMEDY", &[9716], &su));
        assert!(!hidden_by(&["standup".into()], &[35], "en", "EN - COMEDY", &[100], &su));
    }
}
