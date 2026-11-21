// TMDB client with a persistent response cache (same file format as before) and
// in-flight de-duplication, so concurrent requests for the same list share one fetch.
use crate::store::Config;
use crate::util::now_ms;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.themoviedb.org/3";

pub struct Tmdb {
    cfg: Arc<Config>,
    http: reqwest::Client,
    file: PathBuf,
    cache: Mutex<HashMap<String, (i64, Arc<Value>)>>,
    dirty: std::sync::atomic::AtomicBool,
    pub last_error: Mutex<Option<String>>,
    inflight: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Option<Arc<Value>>>>>>,
}

impl Tmdb {
    pub fn new(cfg: Arc<Config>, http: reqwest::Client, file: PathBuf) -> Self {
        let mut cache = HashMap::new();
        if let Some(Value::Object(m)) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            for (k, v) in m { if let (Some(t), Some(d)) = (v["t"].as_i64(), v.get("d")) { cache.insert(k, (t, Arc::new(d.clone()))); } }
        }
        Tmdb { cfg, http, file, cache: Mutex::new(cache), dirty: Default::default(), last_error: Mutex::new(None), inflight: Default::default() }
    }
    pub fn key(&self) -> String { self.cfg.str("tmdbKey").trim().to_string() }
    pub fn enabled(&self) -> bool { !self.key().is_empty() }

    fn url(&self, endpoint: &str, params: &[(&str, String)]) -> (String, String, bool) {
        let key = self.key(); let bearer = key.len() > 40;
        let mut q: Vec<(String, String)> = vec![("language".into(), "en-US".into())];
        for (k, v) in params { q.push((k.to_string(), v.clone())); }
        let qs = |q: &Vec<(String, String)>| q.iter().map(|(k, v)| format!("{}={}", crate::util::enc(k), crate::util::enc(v))).collect::<Vec<_>>().join("&");
        let cache_key = format!("{endpoint}?{}", qs(&q));
        if !bearer { q.push(("api_key".into(), key)); }
        (format!("{API}{endpoint}?{}", qs(&q)), cache_key, bearer)
    }

    async fn fetch(&self, url: &str, bearer: bool) -> Option<Value> {
        for attempt in 0..3u64 {
            let mut rq = self.http.get(url).timeout(Duration::from_secs(15));
            if bearer { rq = rq.bearer_auth(self.key()); }
            match rq.send().await {
                Ok(r) if r.status().as_u16() == 429 => { tokio::time::sleep(Duration::from_millis(1500 * (attempt + 1))).await; }
                Ok(r) if r.status().is_success() => { *self.last_error.lock().unwrap() = None; return r.json().await.ok(); }
                Ok(r) => { *self.last_error.lock().unwrap() = Some(format!("TMDB HTTP {}", r.status())); return None; }
                Err(e) => { *self.last_error.lock().unwrap() = Some(e.to_string()); return None; }
            }
        }
        None
    }

    /// Cached GET; serves stale data if TMDB is unreachable.
    pub async fn get<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, params: P, ttl_ms: i64) -> Option<Arc<Value>> {
        if !self.enabled() { return None; }
        let (url, ck, bearer) = self.url(endpoint.as_ref(), params.as_ref());
        let hit = self.cache.lock().unwrap().get(&ck).cloned();
        if let Some((t, d)) = &hit { if now_ms() - t < ttl_ms { return Some(d.clone()); } }
        let cell = { let mut m = self.inflight.lock().await; m.entry(ck.clone()).or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())).clone() };
        let res = cell.get_or_init(|| async {
            match self.fetch(&url, bearer).await {
                Some(v) => { let v = Arc::new(v); self.cache.lock().unwrap().insert(ck.clone(), (now_ms(), v.clone())); self.dirty.store(true, std::sync::atomic::Ordering::SeqCst); Some(v) }
                None => None,
            }
        }).await.clone();
        self.inflight.lock().await.remove(&ck);
        res.or(hit.map(|h| h.1))
    }
    /// Several pages of a list endpoint, concatenated `results`.
    pub async fn pages<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, n: usize, params: P, ttl_ms: i64) -> Vec<Value> {
        let endpoint = endpoint.as_ref().to_string(); let params = params.as_ref().to_vec();
        let futs = (1..=n).map(|p| { let mut ps: Vec<(&'static str, String)> = params.clone(); ps.push(("page", p.to_string())); let ep = endpoint.clone(); async move { self.get(ep, ps, ttl_ms).await } });
        let res = futures_util::future::join_all(futs).await;
        res.into_iter().flatten().flat_map(|v| v["results"].as_array().cloned().unwrap_or_default()).collect()
    }
    /// Uncached (bulk indexing keeps its own store).
    pub async fn raw<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, params: P) -> Option<Value> {
        if !self.enabled() { return None; }
        let (url, _, bearer) = self.url(endpoint.as_ref(), params.as_ref());
        self.fetch(&url, bearer).await
    }
    pub async fn test(&self) -> bool { self.raw("/configuration", NO).await.is_some() }

    pub fn flush(&self) {
        if self.dirty.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let obj: serde_json::Map<String, Value> = self.cache.lock().unwrap().iter().map(|(k, (t, d))| (k.clone(), json!({ "t": t, "d": &**d }))).collect();
            let tmp = self.file.with_extension("json.tmp");
            if std::fs::write(&tmp, serde_json::to_vec(&obj).unwrap()).is_ok() { let _ = std::fs::rename(&tmp, &self.file); }
        }
    }
}

pub const NO: [(&str, String); 0] = [];
pub const TTL_H: i64 = 3600 * 1000;
pub const TTL_D: i64 = 24 * TTL_H;
