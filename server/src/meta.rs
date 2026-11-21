// Local metadata index for every English title (same meta.json format as before).
use crate::app::App;
use crate::util::{now_ms, s};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

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
}
impl Meta {
    pub fn year(&self) -> i32 { self.d.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(0) }
    pub fn bayes(&self) -> f64 { let v = self.vc as f64; (v / (v + 300.0)) * self.va + (300.0 / (v + 300.0)) * 6.4 }
}

pub const MOVIE_GENRES: &[(i64, &str)] = &[(28, "Action"), (12, "Adventure"), (16, "Animation"), (35, "Comedy"), (80, "Crime"), (99, "Documentary"), (18, "Drama"), (10751, "Family"), (14, "Fantasy"), (36, "History"), (27, "Horror"), (10402, "Music"), (9648, "Mystery"), (10749, "Romance"), (878, "Sci-Fi"), (10770, "TV Movie"), (53, "Thriller"), (10752, "War"), (37, "Western")];
pub const TV_GENRES: &[(i64, &str)] = &[(10759, "Action & Adventure"), (16, "Animation"), (35, "Comedy"), (80, "Crime"), (99, "Documentary"), (18, "Drama"), (10751, "Family"), (10762, "Kids"), (9648, "Mystery"), (10763, "News"), (10764, "Reality"), (10765, "Sci-Fi & Fantasy"), (10766, "Soap"), (10767, "Talk"), (10768, "War & Politics"), (37, "Western")];
pub fn genre_name(tv: bool, id: i64) -> Option<&'static str> { (if tv { TV_GENRES } else { MOVIE_GENRES }).iter().find(|g| g.0 == id).map(|g| g.1) }

pub fn compact(mt: &str, d: &Value) -> Meta {
    let kw = if mt == "movie" { &d["keywords"]["keywords"] } else { &d["keywords"]["results"] };
    let oc: Vec<String> = d["origin_country"].as_array().map(|a| a.iter().map(s).collect())
        .or_else(|| d["production_countries"].as_array().map(|a| a.iter().map(|c| s(&c["iso_3166_1"])).collect())).unwrap_or_default();
    let col = if d["belongs_to_collection"].is_object() { let c = &d["belongs_to_collection"]; Some(Col { id: c["id"].as_i64().unwrap_or(0), n: s(&c["name"]), pp: c["poster_path"].as_str().map(String::from), bd: c["backdrop_path"].as_str().map(String::from) }) } else { None };
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
    }
}

pub fn load(path: &std::path::Path) -> HashMap<String, Meta> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Background indexer: missing entries first, refresh after 21 days; ~25 req/s.
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
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((mt, id, _)) = todo.get(i) else { break };
                let append = if mt == "movie" { "keywords" } else { "keywords,external_ids" };
                let d = app.tmdb.raw(format!("/{mt}/{id}"), &[("append_to_response", append.to_string())]).await;
                {
                    let mut db = app.meta.write().unwrap(); let k = format!("{mt}:{id}");
                    match d { Some(d) if d["id"].is_number() => { db.insert(k, compact(mt, &d)); } _ => { db.entry(k).or_insert(Meta { t: now_ms(), missing: true, ..Default::default() }); } }
                }
                let done = { let mut p = app.meta_progress.lock().unwrap(); p.0 += 1; p.0 };
                if done % 400 == 0 { app.save_meta(); println!("[meta] {done}/{}", todo.len()); }
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
        }));
    }
    for h in handles { let _ = h.await; }
    app.save_meta();
    app.meta_running.store(false, std::sync::atomic::Ordering::SeqCst);
    app.memo.clear();
    println!("[meta] done");
}
