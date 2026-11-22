// Xtream Codes API client with server failover.
use crate::store::Config;
use crate::util::{enc, i, s};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

pub struct Xc {
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    active: AtomicUsize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Account { pub status: String, pub active: i64, pub max: i64, pub exp: i64, pub tz: String }

impl Xc {
    pub fn new(cfg: Arc<Config>, http: reqwest::Client) -> Self { Xc { cfg, http, active: AtomicUsize::new(0) } }
    fn servers(&self) -> Vec<String> { self.cfg.servers() }
    pub fn base(&self) -> String { let s = self.servers(); s.get(self.active.load(Ordering::Relaxed) % s.len().max(1)).cloned().unwrap_or_default() }
    fn ua(&self) -> String { self.cfg.str("userAgent") }
    fn creds(&self) -> (String, String) { (self.cfg.str("username"), self.cfg.str("password")) }
    pub fn api_url(&self, base: &str, action: &str, extra: &str) -> String {
        let (u, p) = self.creds();
        let mut url = format!("{base}/player_api.php?username={}&password={}", enc(&u), enc(&p));
        if !action.is_empty() { url.push_str(&format!("&action={action}")); }
        url.push_str(extra);
        url
    }

    pub async fn api<S: AsRef<str>>(&self, action: &str, extra: S, timeout: u64) -> anyhow_lite::Result<Value> {
        let extra = extra.as_ref();
        let servers = self.servers(); let n = servers.len().max(1);
        let start = self.active.load(Ordering::Relaxed);
        let mut last = String::from("no servers");
        for k in 0..n {
            let idx = (start + k) % n;
            let url = self.api_url(&servers[idx], action, extra);
            match self.http.get(&url).header("User-Agent", self.ua()).timeout(Duration::from_millis(timeout)).send().await {
                Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                    Ok(v) => { self.active.store(idx, Ordering::Relaxed); return Ok(v); }
                    Err(e) => last = e.to_string(),
                },
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last.into())
    }

    /// Stream a large response to disk (catalogue lists are 20–75 MB).
    pub async fn download(&self, path_or_action: &str, dest: &std::path::Path) -> anyhow_lite::Result<()> {
        let servers = self.servers(); let n = servers.len().max(1);
        let start = self.active.load(Ordering::Relaxed);
        let mut last = String::new();
        for k in 0..n {
            let idx = (start + k) % n; let base = &servers[idx];
            let url = if path_or_action.starts_with('/') { format!("{base}{path_or_action}") } else { self.api_url(base, path_or_action, "") };
            let res = match self.http.get(&url).header("User-Agent", self.ua()).timeout(Duration::from_secs(300)).send().await {
                Ok(r) if r.status().is_success() => r, Ok(r) => { last = format!("HTTP {}", r.status()); continue; } Err(e) => { last = e.to_string(); continue; }
            };
            let tmp = dest.with_extension("part");
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
            let mut stream = res.bytes_stream(); let mut ok = true;
            while let Some(chunk) = stream.next().await {
                match chunk { Ok(c) => { if f.write_all(&c).await.is_err() { ok = false; break; } } Err(e) => { last = e.to_string(); ok = false; break; } }
            }
            f.flush().await.ok(); drop(f);
            // Self-protection: providers sometimes answer "[]" or a truncated list for a moment.
            // Never let a response far smaller than the copy we already have replace it.
            if ok {
                let new_len = tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0);
                let old_len = tokio::fs::metadata(dest).await.map(|m| m.len()).unwrap_or(0);
                if old_len > 100_000 && new_len < old_len / 2 {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    eprintln!("[sync] {}: got {new_len} bytes vs {old_len} cached, keeping cached copy", dest.display());
                    last = format!("provider sent an incomplete list ({new_len} bytes)"); ok = false;
                }
            }
            if ok { tokio::fs::rename(&tmp, dest).await.map_err(|e| e.to_string())?; self.active.store(idx, Ordering::Relaxed); return Ok(()); }
        }
        Err(last.into())
    }

    pub fn epg_path(&self) -> String { let (u, p) = self.creds(); format!("/xmltv.php?username={}&password={}", enc(&u), enc(&p)) }

    /// `alt` = use the next server in the list (playback fallback).
    pub fn stream_url(&self, kind: &str, id: &str, ext: &str, alt: bool) -> String {
        let (u, p) = self.creds();
        let seg = match kind { "live" => "live", "movie" => "movie", _ => "series" };
        let servers = self.servers(); let n = servers.len().max(1);
        let idx = (self.active.load(Ordering::Relaxed) + if alt { 1 } else { 0 }) % n;
        format!("{}/{seg}/{u}/{p}/{id}.{ext}", servers.get(idx).cloned().unwrap_or_default())
    }

    /// Rotate away from a server whose stream URL returned 403/404 even if player_api.php answered 200.
    pub fn mark_stream_failed(&self, failed_url: &str) {
        let servers = self.servers();
        if servers.len() < 2 { return; }
        let cur = self.active.load(Ordering::Relaxed) % servers.len();
        if failed_url.starts_with(&servers[cur]) {
            let next = (cur + 1) % servers.len();
            self.active.store(next, Ordering::Relaxed);
            println!("[xc] rotating stream server {} -> {}", servers[cur], servers[next]);
        }
    }

    pub async fn account(&self) -> anyhow_lite::Result<Account> {
        let j = self.api("", "", 8000).await?;
        let u = &j["user_info"];
        Ok(Account { status: s(&u["status"]), active: i(&u["active_cons"]), max: i(&u["max_connections"]).max(1), exp: i(&u["exp_date"]) * 1000, tz: s(&j["server_info"]["timezone"]) })
    }
}

/// Tiny error type so we don't need extra crates.
pub mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, String>;
}
