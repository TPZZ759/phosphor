// TMDB client with a persistent response cache (same file format as before),
// bounded TTL/size eviction, retry/Retry-After handling, and race-free in-flight de-duplication.
use crate::store::Config;
use crate::util::now_ms;
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.themoviedb.org/3";
const MAX_CACHE_ENTRIES: usize = 10_000;
const MAX_CACHE_AGE_MS: i64 = 30 * TTL_D;

#[derive(Debug)]
pub enum FetchRes {
    Ok(Value),
    NotFound,
    Transient,
}

pub struct Tmdb {
    cfg: Arc<Config>,
    http: reqwest::Client,
    file: PathBuf,
    cache: Mutex<HashMap<String, (i64, Arc<Value>)>>,
    dirty_gen: AtomicU64,
    flushed_gen: AtomicU64,
    pub last_error: Mutex<Option<String>>,
    inflight: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Option<Arc<Value>>>>>>,
}

fn redact_api_key(msg: &str) -> String {
    crate::re!(r"(?i)(api_key)=[^&\s\)\]]+").replace_all(msg, "$1=***").into_owned()
}

fn parse_retry_after(r: &reqwest::Response) -> Option<Duration> {
    let raw = r.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    let secs = raw.parse::<u64>().ok()?;
    Some(Duration::from_secs(secs.clamp(1, 30)))
}

fn prune_cache(map: &mut HashMap<String, (i64, Arc<Value>)>, now: i64) -> bool {
    let before = map.len();
    let cutoff = now - MAX_CACHE_AGE_MS;
    map.retain(|_, (t, _)| *t >= cutoff);
    if map.len() > MAX_CACHE_ENTRIES {
        let mut by_age: Vec<(i64, String)> = map.iter().map(|(k, (t, _))| (*t, k.clone())).collect();
        by_age.sort_unstable_by_key(|x| x.0);
        let drop_n = map.len() - MAX_CACHE_ENTRIES;
        for (_, k) in by_age.into_iter().take(drop_n) {
            map.remove(&k);
        }
    }
    map.len() != before
}

struct CacheEntryRef<'a> {
    t: i64,
    d: &'a Value,
}
impl Serialize for CacheEntryRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut m = serializer.serialize_map(Some(2))?;
        m.serialize_entry("t", &self.t)?;
        m.serialize_entry("d", self.d)?;
        m.end()
    }
}

struct CacheSnapRef<'a>(&'a [(String, i64, Arc<Value>)]);
impl Serialize for CacheSnapRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut m = serializer.serialize_map(Some(self.0.len()))?;
        for (k, t, d) in self.0 {
            m.serialize_entry(k, &CacheEntryRef { t: *t, d: &**d })?;
        }
        m.end()
    }
}

fn atomic_replace(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    if std::fs::rename(tmp, dest).is_ok() {
        return Ok(());
    }
    for attempt in 0..5u64 {
        let _ = std::fs::remove_file(dest);
        if std::fs::rename(tmp, dest).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(30 * (attempt + 1)));
    }
    let res = std::fs::rename(tmp, dest);
    if res.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    res
}

impl Tmdb {
    pub fn new(cfg: Arc<Config>, http: reqwest::Client, file: PathBuf) -> Self {
        let mut cache = HashMap::new();
        if let Some(Value::Object(m)) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            for (k, v) in m {
                if let (Some(t), Some(d)) = (v["t"].as_i64(), v.get("d")) {
                    cache.insert(k, (t, Arc::new(d.clone())));
                }
            }
        }
        let pruned = prune_cache(&mut cache, now_ms());
        Tmdb {
            cfg,
            http,
            file,
            cache: Mutex::new(cache),
            dirty_gen: AtomicU64::new(if pruned { 1 } else { 0 }),
            flushed_gen: AtomicU64::new(0),
            last_error: Mutex::new(None),
            inflight: Default::default(),
        }
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

    async fn fetch_status(&self, url: &str, bearer: bool) -> FetchRes {
        for attempt in 0..3u64 {
            let mut rq = self.http.get(url).timeout(Duration::from_secs(15));
            if bearer { rq = rq.bearer_auth(self.key()); }
            match rq.send().await {
                Ok(r) if r.status().as_u16() == 429 => {
                    let wait = parse_retry_after(&r).unwrap_or_else(|| Duration::from_millis(1500 * (1u64 << attempt)));
                    if attempt < 2 {
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    *self.last_error.lock().unwrap() = Some("TMDB HTTP 429 Too Many Requests".into());
                    return FetchRes::Transient;
                }
                Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                    Ok(v) => {
                        *self.last_error.lock().unwrap() = None;
                        return FetchRes::Ok(v);
                    }
                    Err(e) => {
                        *self.last_error.lock().unwrap() = Some(redact_api_key(&e.to_string()));
                        if attempt < 2 {
                            tokio::time::sleep(Duration::from_millis(500 * (1u64 << attempt))).await;
                            continue;
                        }
                        return FetchRes::Transient;
                    }
                },
                Ok(r) if r.status().as_u16() == 404 => {
                    return FetchRes::NotFound;
                }
                Ok(r) if r.status().is_server_error() => {
                    *self.last_error.lock().unwrap() = Some(format!("TMDB HTTP {}", r.status()));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(600 * (1u64 << attempt))).await;
                        continue;
                    }
                    return FetchRes::Transient;
                }
                Ok(r) => {
                    *self.last_error.lock().unwrap() = Some(format!("TMDB HTTP {}", r.status()));
                    return FetchRes::Transient;
                }
                Err(e) => {
                    *self.last_error.lock().unwrap() = Some(redact_api_key(&e.to_string()));
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_millis(600 * (1u64 << attempt))).await;
                        continue;
                    }
                    return FetchRes::Transient;
                }
            }
        }
        FetchRes::Transient
    }

    async fn fetch(&self, url: &str, bearer: bool) -> Option<Value> {
        match self.fetch_status(url, bearer).await {
            FetchRes::Ok(v) => Some(v),
            FetchRes::NotFound | FetchRes::Transient => None,
        }
    }

    /// Cached GET; serves stale data if TMDB is unreachable.
    pub async fn get<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, params: P, ttl_ms: i64) -> Option<Arc<Value>> {
        if !self.enabled() { return None; }
        let (url, ck, bearer) = self.url(endpoint.as_ref(), params.as_ref());
        let hit = self.cache.lock().unwrap().get(&ck).cloned();
        if let Some((t, d)) = &hit { if now_ms() - t < ttl_ms { return Some(d.clone()); } }
        let cell = {
            let mut m = self.inflight.lock().await;
            m.entry(ck.clone()).or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())).clone()
        };
        let res = cell.get_or_init(|| async {
            match self.fetch(&url, bearer).await {
                Some(v) => {
                    let v = Arc::new(v);
                    self.cache.lock().unwrap().insert(ck.clone(), (now_ms(), v.clone()));
                    self.dirty_gen.fetch_add(1, Ordering::SeqCst);
                    Some(v)
                }
                None => None,
            }
        }).await.clone();
        // Race-free cleanup: only remove `ck` if it still points to our exact `OnceCell`.
        {
            let mut m = self.inflight.lock().await;
            if m.get(&ck).map_or(false, |cur| Arc::ptr_eq(cur, &cell)) {
                m.remove(&ck);
            }
        }
        res.or(hit.map(|h| h.1))
    }
    /// Several pages of a list endpoint, concatenated `results`.
    pub async fn pages<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, n: usize, params: P, ttl_ms: i64) -> Vec<Value> {
        let endpoint = endpoint.as_ref().to_string(); let params = params.as_ref().to_vec();
        let futs = (1..=n).map(|p| { let mut ps: Vec<(&'static str, String)> = params.clone(); ps.push(("page", p.to_string())); let ep = endpoint.clone(); async move { self.get(ep, ps, ttl_ms).await } });
        let res = futures_util::future::join_all(futs).await;
        res.into_iter().flatten().flat_map(|v| v["results"].as_array().cloned().unwrap_or_default()).collect()
    }
    /// Uncached with explicit 404 vs transient-error distinction (used by bulk metadata indexer).
    pub async fn raw_status<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, params: P) -> FetchRes {
        if !self.enabled() { return FetchRes::Transient; }
        let (url, _, bearer) = self.url(endpoint.as_ref(), params.as_ref());
        self.fetch_status(&url, bearer).await
    }
    /// Uncached (bulk indexing keeps its own store).
    pub async fn raw<E: AsRef<str>, P: AsRef<[(&'static str, String)]>>(&self, endpoint: E, params: P) -> Option<Value> {
        match self.raw_status(endpoint, params).await {
            FetchRes::Ok(v) => Some(v),
            FetchRes::NotFound | FetchRes::Transient => None,
        }
    }
    pub async fn test(&self) -> bool { self.raw("/configuration", NO).await.is_some() }

    pub fn flush(&self) {
        let gen = self.dirty_gen.load(Ordering::SeqCst);
        if gen == self.flushed_gen.load(Ordering::SeqCst) {
            return;
        }
        // Snapshot only Arc handles under lock (zero Value deep clones!) and enforce TTL + 10,000-entry cap.
        let snap: Vec<(String, i64, Arc<Value>)> = {
            let mut g = self.cache.lock().unwrap();
            prune_cache(&mut g, now_ms());
            g.iter().map(|(k, (t, d))| (k.clone(), *t, d.clone())).collect()
        };
        let tmp = self.file.with_extension(format!("json.{}.{gen}.tmp", std::process::id()));
        let write_ok = (|| -> std::io::Result<()> {
            let file = File::create(&tmp)?;
            let mut writer = BufWriter::with_capacity(256 * 1024, file);
            serde_json::to_writer(&mut writer, &CacheSnapRef(&snap))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            writer.flush()
        })();
        if write_ok.is_ok() && atomic_replace(&tmp, &self.file).is_ok() {
            self.flushed_gen.fetch_max(gen, Ordering::SeqCst);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

pub const NO: [(&str, String); 0] = [];
pub const TTL_H: i64 = 3600 * 1000;
pub const TTL_D: i64 = 24 * TTL_H;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_cache_enforces_ttl_and_max_entries() {
        let now = now_ms();
        let mut m: HashMap<String, (i64, Arc<Value>)> = HashMap::new();
        // Expired entry
        m.insert("old".into(), (now - MAX_CACHE_AGE_MS - 10_000, Arc::new(Value::Null)));
        // 10,005 fresh entries with increasing timestamps
        for idx in 0..(MAX_CACHE_ENTRIES + 5) {
            m.insert(format!("k{idx}"), (now - (MAX_CACHE_ENTRIES as i64) + idx as i64, Arc::new(Value::Null)));
        }
        assert!(prune_cache(&mut m, now));
        assert!(!m.contains_key("old"));
        assert_eq!(m.len(), MAX_CACHE_ENTRIES);
        assert!(!m.contains_key("k0"));
        assert!(m.contains_key(&format!("k{}", MAX_CACHE_ENTRIES + 4)));
    }
}
