// Local metadata index for every English title (same meta.json format as before, plus optional ot/dd/dir/cast fields).
use crate::app::App;
use crate::tmdb::FetchRes;
use crate::util::{now_ms, s};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Col { pub id: i64, pub n: String, #[serde(default)] pub pp: Option<String>, #[serde(default)] pub bd: Option<String> }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)] pub t: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub missing: bool,
    #[serde(default)] pub g: Vec<i64>,
    #[serde(default)] pub rt: i64,
    #[serde(default)] pub l: String,
    #[serde(default)] pub oc: Vec<String>,
    #[serde(default)] pub p: f64,
    #[serde(default)] pub vc: i64,
    #[serde(default)] pub va: f64,
    #[serde(default)] pub d: String,
    #[serde(default)] pub ov: String,
    #[serde(default)] pub bd: String,
    #[serde(default)] pub pp: String,
    #[serde(default)] pub kw: Vec<i64>,
    #[serde(default)] pub im: String,
    #[serde(default)] pub col: Option<Col>,
    #[serde(default)] pub st: String,
    #[serde(default)] pub ns: i64,
    /// Original title / name (romaji / foreign / native title for multilingual search)
    #[serde(default, skip_serializing_if = "String::is_empty")] pub ot: String,
    /// Earliest TMDB Digital (type 4) or Physical (type 5) release date (`YYYY-MM-DD`)
    #[serde(default, skip_serializing_if = "String::is_empty")] pub dd: String,
    /// Top 2 directors (movies) or creators/showrunners (TV)
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub dir: Vec<String>,
    /// Top 4 billed cast names
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub cast: Vec<String>,
}
impl Meta {
    pub fn year(&self) -> i32 { self.d.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(0) }
    pub fn bayes(&self) -> f64 { let v = self.vc as f64; (v / (v + 300.0)) * self.va + (300.0 / (v + 300.0)) * 6.4 }
}

pub const MOVIE_GENRES: &[(i64, &str)] = &[(28, "Action"), (12, "Adventure"), (16, "Animation"), (35, "Comedy"), (80, "Crime"), (99, "Documentary"), (18, "Drama"), (10751, "Family"), (14, "Fantasy"), (36, "History"), (27, "Horror"), (10402, "Music"), (9648, "Mystery"), (10749, "Romance"), (878, "Sci-Fi"), (10770, "TV Movie"), (53, "Thriller"), (10752, "War"), (37, "Western")];
pub const TV_GENRES: &[(i64, &str)] = &[(10759, "Action & Adventure"), (16, "Animation"), (35, "Comedy"), (80, "Crime"), (99, "Documentary"), (18, "Drama"), (10751, "Family"), (10762, "Kids"), (9648, "Mystery"), (10763, "News"), (10764, "Reality"), (10765, "Sci-Fi & Fantasy"), (10766, "Soap"), (10767, "Talk"), (10768, "War & Politics"), (37, "Western")];
pub fn genre_name(tv: bool, id: i64) -> Option<&'static str> { (if tv { TV_GENRES } else { MOVIE_GENRES }).iter().find(|g| g.0 == id).map(|g| g.1) }

/// Extract earliest TMDB Digital (type 4) or Physical (type 5) release date (`YYYY-MM-DD`).
fn extract_digital_date(d: &Value) -> String {
    let mut best_major: Option<String> = None;
    let mut best_any: Option<String> = None;
    for region in d["release_dates"]["results"].as_array().into_iter().flatten() {
        let cc = region["iso_3166_1"].as_str().unwrap_or("");
        let major = matches!(cc, "US" | "AU" | "GB" | "CA");
        for rd in region["release_dates"].as_array().into_iter().flatten() {
            let ty = rd["type"].as_i64().unwrap_or(0);
            if ty != 4 && ty != 5 { continue; }
            let raw = rd["release_date"].as_str().unwrap_or("");
            let Some(day) = raw.get(0..10) else { continue };
            if day.len() == 10 && day.as_bytes()[4] == b'-' && day.as_bytes()[7] == b'-' {
                if major && best_major.as_deref().map_or(true, |cur| day < cur) {
                    best_major = Some(day.to_string());
                }
                if best_any.as_deref().map_or(true, |cur| day < cur) {
                    best_any = Some(day.to_string());
                }
            }
        }
    }
    best_major.or(best_any).unwrap_or_default()
}

fn push_unique_name(out: &mut Vec<String>, name: String, max: usize) {
    let n = name.trim().to_string();
    if n.is_empty() || out.len() >= max { return; }
    if !out.iter().any(|x| x.eq_ignore_ascii_case(&n)) {
        out.push(n);
    }
}

pub fn compact(mt: &str, d: &Value) -> Meta {
    let kw = if mt == "movie" { &d["keywords"]["keywords"] } else { &d["keywords"]["results"] };
    let oc: Vec<String> = d["origin_country"].as_array().map(|a| a.iter().map(s).collect())
        .or_else(|| d["production_countries"].as_array().map(|a| a.iter().map(|c| s(&c["iso_3166_1"])).collect())).unwrap_or_default();
    let col = if d["belongs_to_collection"].is_object() { let c = &d["belongs_to_collection"]; Some(Col { id: c["id"].as_i64().unwrap_or(0), n: s(&c["name"]), pp: c["poster_path"].as_str().map(String::from), bd: c["backdrop_path"].as_str().map(String::from) }) } else { None };
    let ot = if mt == "movie" { s(&d["original_title"]) } else { s(&d["original_name"]) };
    let dd = if mt == "movie" { extract_digital_date(d) } else { String::new() };
    let mut dir: Vec<String> = vec![];
    if mt == "movie" {
        for c in d["credits"]["crew"].as_array().into_iter().flatten() {
            if c["job"].as_str() == Some("Director") {
                push_unique_name(&mut dir, s(&c["name"]), 2);
            }
        }
    } else {
        for c in d["created_by"].as_array().into_iter().flatten() {
            push_unique_name(&mut dir, s(&c["name"]), 2);
        }
        if dir.len() < 2 {
            for want in ["Creator", "Showrunner", "Director", "Executive Producer"] {
                if dir.len() >= 2 { break; }
                for c in d["credits"]["crew"].as_array().into_iter().flatten() {
                    if c["job"].as_str() == Some(want) {
                        push_unique_name(&mut dir, s(&c["name"]), 2);
                    }
                }
            }
        }
    }
    let mut cast: Vec<String> = vec![];
    for c in d["credits"]["cast"].as_array().into_iter().flatten() {
        push_unique_name(&mut cast, s(&c["name"]), 4);
    }
    Meta {
        t: now_ms(), missing: false,
        g: d["genres"].as_array().map(|a| a.iter().filter_map(|g| g["id"].as_i64()).collect()).unwrap_or_default(),
        rt: if mt == "movie" { d["runtime"].as_i64().unwrap_or(0) } else { d["episode_run_time"].as_array().and_then(|a| a.first()).and_then(|x| x.as_i64()).unwrap_or(0) },
        l: s(&d["original_language"]), oc: oc.into_iter().take(3).collect(),
        p: (d["popularity"].as_f64().unwrap_or(0.0) * 10.0).round() / 10.0, vc: d["vote_count"].as_i64().unwrap_or(0), va: d["vote_average"].as_f64().unwrap_or(0.0),
        d: { let r = s(&d["release_date"]); if r.is_empty() { s(&d["first_air_date"]) } else { r } },
        ov: s(&d["overview"]).chars().take(260).collect(), bd: s(&d["backdrop_path"]), pp: s(&d["poster_path"]),
        kw: kw.as_array().map(|a| a.iter().take(20).filter_map(|k| k["id"].as_i64()).collect()).unwrap_or_default(),
        im: { let a = s(&d["imdb_id"]); if a.is_empty() { s(&d["external_ids"]["imdb_id"]) } else { a } },
        col, st: s(&d["status"]), ns: d["number_of_seasons"].as_i64().unwrap_or(0),
        ot, dd, dir, cast,
    }
}

pub fn load(path: &std::path::Path) -> HashMap<String, Meta> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

async fn save_meta_async(app: &Arc<App>) {
    let a = app.clone();
    let _ = tokio::task::spawn_blocking(move || a.save_meta()).await;
}

/// Background indexer: missing entries first, refresh after 21 days; ~25 req/s.
/// Distinguishes HTTP 404 (`missing: true`) from transient 5xx/429/network errors (never poisons meta.json),
/// and pauses politely while active playback is running.
pub async fn build(app: Arc<App>) {
    if !app.tmdb.enabled() || app.meta_running.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
    let todo: Vec<(String, String, bool)> = {
        let cat = app.catalog(); let db = app.meta.read().unwrap(); let stale = now_ms() - 21 * 86_400_000;
        let mut v = vec![];
        for id in cat.movie_by_tmdb.keys() { let e = db.get(&format!("movie:{id}")); if e.map_or(true, |e| e.t < stale) { v.push(("movie".to_string(), id.clone(), e.is_none())); } }
        for id in cat.series_by_tmdb.keys() { let e = db.get(&format!("tv:{id}")); if e.map_or(true, |e| e.t < stale) { v.push(("tv".to_string(), id.clone(), e.is_none())); } }
        v.sort_by_key(|x| !x.2); v
    };
    *app.meta_progress.lock().unwrap() = (0, todo.len());
    if !todo.is_empty() { println!("[meta] indexing {} titles", todo.len()); }
    let todo = Arc::new(todo); let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut handles = vec![];
    for _ in 0..8 {
        let (app, todo, next) = (app.clone(), todo.clone(), next.clone());
        handles.push(tokio::spawn(async move {
            loop {
                // Yield politely during active streaming / playback to keep bandwidth & CPU silky smooth.
                while app.stream.is_active() || crate::mpv::running(&app) || crate::livebuf::is_active() {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((mt, id, _)) = todo.get(i) else { break };
                let append = if mt == "movie" { "keywords,release_dates,credits" } else { "keywords,external_ids,credits" };
                let res = app.tmdb.raw_status(format!("/{mt}/{id}"), &[("append_to_response", append.to_string())]).await;
                {
                    let mut db = app.meta.write().unwrap();
                    let k = format!("{mt}:{id}");
                    match res {
                        FetchRes::Ok(d) if d["id"].is_number() => {
                            db.insert(k, compact(mt, &d));
                        }
                        FetchRes::NotFound => {
                            match db.get_mut(&k) {
                                Some(existing) if existing.missing => { existing.t = now_ms(); }
                                Some(_) => {} // keep prior valid metadata if TMDB temporarily removed it
                                None => { db.insert(k, Meta { t: now_ms(), missing: true, ..Default::default() }); }
                            }
                        }
                        FetchRes::Transient | FetchRes::Ok(_) => {
                            // Never poison meta.json as `missing: true` on transient 5xx/429/network errors!
                        }
                    }
                }
                let done = { let mut p = app.meta_progress.lock().unwrap(); p.0 += 1; p.0 };
                if done % 400 == 0 {
                    save_meta_async(&app).await;
                    println!("[meta] {done}/{}", todo.len());
                }
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
        }));
    }
    for h in handles { let _ = h.await; }
    save_meta_async(&app).await;
    app.meta_running.store(false, std::sync::atomic::Ordering::SeqCst);
    app.memo.clear();
    println!("[meta] done");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_extracts_orig_title_digital_date_directors_and_cast() {
        let movie = json!({
            "id": 693134,
            "title": "Dune: Part Two",
            "original_title": "Dune: Part Two",
            "original_language": "en",
            "release_date": "2024-02-27",
            "runtime": 166,
            "vote_average": 8.2,
            "vote_count": 5200,
            "popularity": 120.4,
            "genres": [{"id": 878}, {"id": 12}],
            "keywords": {"keywords": [{"id": 100}]},
            "release_dates": {
                "results": [
                    {"iso_3166_1": "US", "release_dates": [
                        {"type": 3, "release_date": "2024-03-01T00:00:00.000Z"},
                        {"type": 4, "release_date": "2024-04-16T00:00:00.000Z"},
                        {"type": 5, "release_date": "2024-05-14T00:00:00.000Z"}
                    ]}
                ]
            },
            "credits": {
                "crew": [
                    {"job": "Producer", "name": "Mary Parent"},
                    {"job": "Director", "name": "Denis Villeneuve"}
                ],
                "cast": [
                    {"name": "Timothée Chalamet"},
                    {"name": "Zendaya"},
                    {"name": "Rebecca Ferguson"},
                    {"name": "Javier Bardem"},
                    {"name": "Josh Brolin"}
                ]
            }
        });
        let m = compact("movie", &movie);
        assert_eq!(m.ot, "Dune: Part Two");
        assert_eq!(m.dd, "2024-04-16");
        assert_eq!(m.dir, vec!["Denis Villeneuve"]);
        assert_eq!(m.cast, vec!["Timothée Chalamet", "Zendaya", "Rebecca Ferguson", "Javier Bardem"]);

        // Backward compat: legacy JSON without ot/dd/dir/cast deserializes cleanly
        let legacy: Meta = serde_json::from_str(r#"{"t":1,"g":[28],"rt":120}"#).unwrap();
        assert!(legacy.ot.is_empty());
        assert!(legacy.dd.is_empty());
        assert!(legacy.dir.is_empty());
        assert!(legacy.cast.is_empty());
    }
}
