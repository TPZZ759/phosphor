// Shared application state + the stale-while-revalidate memo that makes pages instant.
use crate::catalog::{Catalog, Item};
use crate::meta::Meta;
use crate::store::{Config, State};
use crate::tmdb::Tmdb;
use crate::util::{now_ms, tmdb_img};
use crate::xc::{Account, Xc};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

#[derive(Clone, Debug)]
pub struct Prog { pub s: i64, pub e: i64, pub title: String, pub desc: String }

pub struct App {
    pub data: PathBuf,
    pub public: PathBuf,
    pub cfg: Arc<Config>,
    pub state: State,
    pub http: reqwest::Client,
    pub xc: Xc,
    pub tmdb: Tmdb,
    pub cat: RwLock<Arc<Catalog>>,
    pub meta: RwLock<HashMap<String, Meta>>,
    pub meta_running: AtomicBool,
    pub meta_progress: Mutex<(usize, usize)>,
    pub memo: Memo,
    pub epg: RwLock<Arc<HashMap<String, Vec<Prog>>>>,
    pub syncing: AtomicBool,
    pub sync_error: Mutex<Option<String>>,
    pub stream: crate::stream::Streamer,
    pub fixtures: crate::fixtures::Fixtures,
    pub omdb: crate::extras::Omdb,
    pub kw_cache: Mutex<HashMap<String, Option<i64>>>,
    pub acct: Mutex<Option<(i64, Account)>>,
    pub events_raw: Mutex<Option<(i64, Arc<Vec<crate::sports::Event>>)>>,
    pub net_idx: Mutex<HashMap<String, Vec<i64>>>,
    pub xinfo: Mutex<HashMap<String, (i64, Arc<Value>)>>,
    pub tri: RwLock<Option<Arc<Vec<(String, bool, usize, HashSet<[u8; 3]>)>>>>,
    pub skips: crate::intro::Skips,
    pub skipdb: crate::skipdb::SkipDb,
    pub remote: crate::remote::Remote,
    pub jobs: crate::sync::Jobs,
    pub cast: crate::cast::Cast,
    pub display: crate::display::DisplayCtl,
    pub mpv: crate::mpv::Mpv,
    pub dubs: crate::anime::DubCache,
    /// stream request generation (newest wins)
    pub stream_gen: std::sync::atomic::AtomicU64,
}

impl App {
    pub fn catalog(&self) -> Arc<Catalog> { self.cat.read().unwrap().clone() }
    pub fn ready(&self) -> bool { self.catalog().loaded_at > 0 }
    pub fn meta_get(&self, mt: &str, id: &str) -> Option<Meta> { self.meta.read().unwrap().get(&format!("{mt}:{id}")).cloned() }
    pub fn meta_of(&self, it: &Item) -> Option<Meta> { it.tmdb.as_ref().and_then(|t| self.meta_get(if it.k == "s" { "tv" } else { "movie" }, t)).filter(|m| !m.missing) }
    pub fn save_meta(&self) {
        let body = { serde_json::to_vec(&*self.meta.read().unwrap()).unwrap() };
        let f = self.data.join("meta.json"); let tmp = f.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() { let _ = std::fs::rename(&tmp, &f); }
    }

    /// Card JSON for a catalogue item, optionally enriched with a TMDB result object.
    pub fn card(&self, it: &Item, tm: Option<&Value>) -> Value {
        let cam_only = it.cam && it.variants.iter().all(|v| v.cam);
        let has_uhd = (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd);
        let has_dv = (!it.cam && it.dv) || it.variants.iter().any(|v| !v.cam && v.dv);
        let has_bluray = (!it.cam && it.bluray) || it.variants.iter().any(|v| !v.cam && v.bluray);
        let mut c = json!({ "k": it.k, "id": it.id, "tmdb": it.tmdb, "title": it.title, "year": it.year,
            "poster": it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/"), "backdrop": it.backdrop, "rating": it.rating,
            "uhd": has_uhd });
        if has_dv { c["dv"] = json!(true); }
        if has_bluray { c["bluray"] = json!(true); }
        if cam_only { c["cam"] = json!(true); }
        if let Some(tm) = tm {
            if let Some(b) = tm["backdrop_path"].as_str() { if !b.is_empty() { c["backdrop"] = json!(tmdb_img(b, "w1280")); } }
            if let Some(p) = tm["poster_path"].as_str() { if !p.is_empty() { c["poster"] = json!(tmdb_img(p, "w342")); } }
            c["overview"] = json!(tm["overview"].as_str().unwrap_or(""));
            if let Some(v) = tm["vote_average"].as_f64() { if v > 0.0 { c["rating"] = json!((v * 10.0).round() / 10.0); } }
            if let Some(t) = tm["title"].as_str().or(tm["name"].as_str()) { if !t.is_empty() { c["title"] = json!(t); } }
            let d = tm["release_date"].as_str().filter(|x| !x.is_empty()).or(tm["first_air_date"].as_str()).unwrap_or("");
            if let Some(y) = d.get(0..4).and_then(|y| y.parse::<i32>().ok()) { c["year"] = json!(y); }
        }
        c
    }
    /// Card enriched from the local metadata index (zero network calls).
    pub fn card_meta(&self, it: &Item) -> Value {
        let m = self.meta_of(it);
        let tm = m.as_ref().map(|m| json!({ "backdrop_path": m.bd, "poster_path": m.pp, "overview": m.ov, "vote_average": m.va, "release_date": m.d }));
        let mut c = self.card(it, tm.as_ref());
        let fallback_genres = || -> Vec<String> {
            it.genre.split(|ch| ch == ',' || ch == '/' || ch == '|')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .take(6)
                .collect()
        };
        if let Some(m) = m {
            let g: Vec<&str> = m.g.iter().filter_map(|g| crate::meta::genre_name(it.k == "s", *g)).take(6).collect();
            if !g.is_empty() {
                c["genres"] = json!(g);
            } else if !it.genre.is_empty() {
                c["genres"] = json!(fallback_genres());
            } else {
                c["genres"] = json!([]);
            }
            if m.rt > 0 { c["runtime"] = json!(m.rt); }
            if m.vc > 0 { c["votes"] = json!(m.vc); }
            if let Some(co) = m.oc.first().filter(|x| !x.is_empty()) { c["country"] = json!(co); }
            if !m.l.is_empty() { c["lang"] = json!(m.l); }
            if it.k == "s" {
                if m.ns > 0 { c["seasons"] = json!(m.ns); }
                if !m.st.is_empty() { c["status"] = json!(m.st); }
            }
            if !m.im.is_empty() {
                c["imdb"] = json!(m.im);
                if let Some(sc) = self.omdb.get(&m.im) { c["scores"] = sc; }
            }
            if m.g.contains(&99) { c["isDoc"] = json!(true); }
        } else if !it.genre.is_empty() {
            c["genres"] = json!(fallback_genres());
        }
        if c["overview"].as_str().unwrap_or("").is_empty() && !it.plot.is_empty() {
            c["overview"] = json!(it.plot);
        }
        let plats = self.catalog().item_platforms(it);
        if !plats.is_empty() { c["platforms"] = json!(plats); }
        let versions = 1 + it.variants.len();
        if versions > 1 { c["versions"] = json!(versions); }
        c
    }

    /// Provider title info (get_vod_info / get_series_info), cached: it's the slowest call we make.
    pub async fn xc_info(&self, action: &str, param: &str, id: i64, ttl_ms: i64) -> Value {
        let key = format!("{action}:{id}");
        let hit = self.xinfo.lock().unwrap().get(&key).cloned();
        if let Some((t, v)) = &hit { if now_ms() - t < ttl_ms { return (**v).clone(); } }
        match self.xc.api(action, format!("&{param}={id}"), 30000).await {
            Ok(v) => { let v = Arc::new(v); let mut m = self.xinfo.lock().unwrap(); if m.len() > 3000 { m.clear(); } m.insert(key, (now_ms(), v.clone())); (*v).clone() }
            Err(_) => hit.map(|h| (*h.1).clone()).unwrap_or(Value::Null),
        }
    }
    pub async fn account(&self, fresh: bool) -> Result<Account, String> {
        if !fresh { if let Some((t, a)) = self.acct.lock().unwrap().clone() { if now_ms() - t < 60_000 { return Ok(a); } } }
        let a = self.xc.account().await?;
        if !a.tz.is_empty() && self.cfg.str("serverTimezone") != a.tz { self.cfg.set("serverTimezone", serde_json::json!(a.tz)); }
        *self.acct.lock().unwrap() = Some((now_ms(), a.clone()));
        Ok(a)
    }
}

/// Stale-while-revalidate cache: fresh -> return; stale -> return now + refresh in background; missing -> compute.
#[derive(Default)]
pub struct Memo {
    map: Mutex<HashMap<String, (i64, Arc<Value>)>>,
    refreshing: Mutex<HashSet<String>>,
    lock: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}
impl Memo {
    pub fn clear(&self) { self.map.lock().unwrap().clear(); }
    pub fn expire(&self) { for v in self.map.lock().unwrap().values_mut() { v.0 = 0; } }
    pub fn remove_prefix(&self, p: &str) { self.map.lock().unwrap().retain(|k, _| !k.starts_with(p)); }
}

impl App {
    pub async fn memo<F, Fut>(self: &Arc<Self>, key: &str, ttl_ms: i64, f: F) -> Arc<Value>
    where F: Fn(Arc<App>) -> Fut + Send + Sync + 'static, Fut: Future<Output = Value> + Send + 'static {
        let hit = self.memo.map.lock().unwrap().get(key).cloned();
        if let Some((t, v)) = &hit {
            if now_ms() - t < ttl_ms { return v.clone(); }
            // stale: serve now, refresh once in the background
            if self.memo.refreshing.lock().unwrap().insert(key.to_string()) {
                let (app, key) = (self.clone(), key.to_string());
                tokio::spawn(async move {
                    let v = f(app.clone()).await;
                    app.memo.map.lock().unwrap().insert(key.clone(), (now_ms(), Arc::new(v)));
                    app.memo.refreshing.lock().unwrap().remove(&key);
                });
            }
            return v.clone();
        }
        // missing: compute (one at a time per key)
        let l = { let mut m = self.memo.lock.lock().await; m.entry(key.to_string()).or_default().clone() };
        let _g = l.lock().await;
        if let Some((_, v)) = self.memo.map.lock().unwrap().get(key).cloned() { return v; }
        let v = Arc::new(f(self.clone()).await);
        self.memo.map.lock().unwrap().insert(key.to_string(), (now_ms(), v.clone()));
        v
    }
}
