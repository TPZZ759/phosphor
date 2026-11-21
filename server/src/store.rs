// Config + viewer state, both JSON files shared with the original app (same formats).
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Config {
    pub path: PathBuf,
    pub v: Mutex<Value>,
}
impl Config {
    pub fn load(path: PathBuf) -> Self {
        let v = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
        Config { path, v: Mutex::new(v) }
    }
    pub fn get(&self, k: &str) -> Value { self.v.lock().unwrap().get(k).cloned().unwrap_or(Value::Null) }
    pub fn str(&self, k: &str) -> String { crate::util::s(&self.get(k)) }
    pub fn set(&self, k: &str, val: Value) {
        let mut g = self.v.lock().unwrap();
        g[k] = val;
        let _ = std::fs::write(&self.path, serde_json::to_string_pretty(&*g).unwrap());
    }
    pub fn servers(&self) -> Vec<String> {
        self.get("servers").as_array().map(|a| a.iter().map(crate::util::s).collect()).unwrap_or_default()
    }
}

/// Viewer state (progress, lists, ratings...). Saved with a short debounce.
pub struct State {
    pub path: PathBuf,
    pub v: Mutex<Value>,
    dirty: std::sync::atomic::AtomicBool,
}
impl State {
    pub fn load(path: PathBuf) -> Self {
        let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
        for (k, d) in [("progress", json!({})), ("list", json!({})), ("history", json!([])), ("favs", json!({})), ("ratings", json!({})),
            ("follows", json!({})), ("newEps", json!({})), ("teams", json!([])), ("notified", json!({})), ("prefs", json!({"hide": []})),
            ("watched", json!({})), ("wanted", json!({})), ("arrived", json!({})), ("savedRows", json!([])),
            ("recentLive", json!([])), ("reminders", json!([])), ("cwDismissed", json!({})), ("cwPinned", json!({})), ("dubWatch", json!({}))] {
            if v.get(k).is_none() { v[k] = d; }
        }
        if v["prefs"].get("hide").is_none() { v["prefs"]["hide"] = json!([]); }
        State { path, v: Mutex::new(v), dirty: Default::default() }
    }
    pub fn with<R>(&self, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut g = self.v.lock().unwrap();
        f(&mut g)
    }
    pub fn read<R>(&self, f: impl FnOnce(&Value) -> R) -> R { let g = self.v.lock().unwrap(); f(&g) }
    pub fn save(&self) { self.dirty.store(true, std::sync::atomic::Ordering::SeqCst); }
    pub fn flush(&self) {
        if self.dirty.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let body = { serde_json::to_string(&*self.v.lock().unwrap()).unwrap() };
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, body).is_ok() { let _ = std::fs::rename(&tmp, &self.path); }
        }
    }
}
