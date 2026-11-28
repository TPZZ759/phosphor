// Xtream Codes API client with server failover.
use crate::store::Config;
use crate::util::{enc, i, now_ms, s};
use futures_util::StreamExt;
use serde::de::IgnoredAny;
use serde_json::Value;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

pub struct Xc {
    pub cfg: Arc<Config>,
    pub http: reqwest::Client,
    active: AtomicUsize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Account { pub status: String, pub active: i64, pub max: i64, pub exp: i64, pub tz: String }

fn redact_credentials(msg: &str, user: &str, pass: &str) -> String {
    let mut out = msg.to_string();
    let enc_u = enc(user);
    let enc_p = enc(pass);
    for secret in [pass, enc_p.as_str(), user, enc_u.as_str()] {
        if secret.len() >= 2 {
            out = out.replace(secret, "***");
        }
    }
    crate::re!(r"(?i)(username|password)=[^&\s\)\]]+").replace_all(&out, "$1=***").into_owned()
}

fn looks_like_html(prefix: &[u8]) -> bool {
    let s = String::from_utf8_lossy(prefix).to_lowercase();
    let t = s.trim_start();
    t.starts_with("<!doctype html")
        || t.starts_with("<html")
        || t.starts_with("<head")
        || s.contains("<title>just a moment")
        || s.contains("attention required! | cloudflare")
        || s.contains("502 bad gateway")
        || s.contains("503 service temporarily unavailable")
}

fn validate_payload(dest: &Path, tmp: &Path, first: Option<u8>, last: Option<u8>, prefix: &[u8]) -> Result<(), String> {
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if looks_like_html(prefix) {
        return Err("provider returned an HTML error/Cloudflare page instead of data".into());
    }
    if ext == "json" {
        let valid_bounds = matches!((first, last), (Some(b'['), Some(b']')) | (Some(b'{'), Some(b'}')));
        if !valid_bounds {
            return Err("provider response is not valid JSON".into());
        }
        // Stream-validate JSON syntax without allocating a DOM tree so truncated responses never overwrite cache.
        let file = File::open(tmp).map_err(|e| format!("open tmp for validation: {e}"))?;
        let reader = BufReader::with_capacity(256 * 1024, file);
        let mut de = serde_json::Deserializer::from_reader(reader);
        serde::Deserialize::deserialize(&mut de)
            .map(|_: IgnoredAny| ())
            .map_err(|e| format!("corrupt/truncated JSON payload: {e}"))?;
    } else if ext == "xml" {
        if first != Some(b'<') {
            return Err("provider response is not valid XML".into());
        }
    }
    Ok(())
}

fn unique_tmp_path(dest: &Path) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let fname = dest.file_name().and_then(|n| n.to_str()).unwrap_or("xc");
    let name = format!("{fname}.{}.{}.{seq}.tmp", std::process::id(), now_ms());
    match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join(name),
        _ => PathBuf::from(name),
    }
}

async fn replace_file_retry(tmp: &Path, dest: &Path) -> Result<(), String> {
    if tokio::fs::rename(tmp, dest).await.is_ok() {
        return Ok(());
    }
    for attempt in 0..5u64 {
        let _ = tokio::fs::remove_file(dest).await;
        if tokio::fs::rename(tmp, dest).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(40 * (attempt + 1))).await;
    }
    let err = tokio::fs::rename(tmp, dest).await.err().map(|e| e.to_string()).unwrap_or_else(|| "rename failed".into());
    let _ = tokio::fs::remove_file(tmp).await;
    Err(err)
}

impl Xc {
    pub fn new(cfg: Arc<Config>, http: reqwest::Client) -> Self { Xc { cfg, http, active: AtomicUsize::new(0) } }
    fn servers(&self) -> Vec<String> { self.cfg.servers().into_iter().filter(|s| !s.trim().is_empty()).collect() }
    pub fn base(&self) -> String {
        let s = self.servers();
        if s.is_empty() { return String::new(); }
        s[self.active.load(Ordering::Relaxed) % s.len()].clone()
    }
    fn ua(&self) -> String { self.cfg.str("userAgent") }
    fn creds(&self) -> (String, String) { (self.cfg.str("username"), self.cfg.str("password")) }
    fn redact(&self, e: impl std::fmt::Display) -> String {
        let (u, p) = self.creds();
        redact_credentials(&e.to_string(), &u, &p)
    }

    pub fn api_url(&self, base: &str, action: &str, extra: &str) -> String {
        let (u, p) = self.creds();
        let mut url = format!("{base}/player_api.php?username={}&password={}", enc(&u), enc(&p));
        if !action.is_empty() { url.push_str(&format!("&action={action}")); }
        url.push_str(extra);
        url
    }

    pub async fn api<S: AsRef<str>>(&self, action: &str, extra: S, timeout: u64) -> anyhow_lite::Result<Value> {
        let extra = extra.as_ref();
        let servers = self.servers();
        if servers.is_empty() { return Err("no servers configured".into()); }
        let n = servers.len();
        let start = self.active.load(Ordering::Relaxed);
        let mut last = String::from("no servers");
        for k in 0..n {
            let idx = (start + k) % n;
            let url = self.api_url(&servers[idx], action, extra);
            match self.http.get(&url).header("User-Agent", self.ua()).timeout(Duration::from_millis(timeout)).send().await {
                Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                    Ok(v) => { self.active.store(idx, Ordering::Relaxed); return Ok(v); }
                    Err(e) => last = self.redact(e),
                },
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = self.redact(e),
            }
        }
        Err(last)
    }

    /// Stream a large response to disk (catalogue lists are 20–75 MB) with content validation & atomic Windows-safe replace.
    pub async fn download(&self, path_or_action: &str, dest: &Path) -> anyhow_lite::Result<()> {
        let servers = self.servers();
        if servers.is_empty() { return Err("no servers configured".into()); }
        let n = servers.len();
        let start = self.active.load(Ordering::Relaxed);
        let mut last = String::from("download failed");
        for k in 0..n {
            let idx = (start + k) % n;
            let base = &servers[idx];
            let url = if path_or_action.starts_with('/') { format!("{base}{path_or_action}") } else { self.api_url(base, path_or_action, "") };
            let res = match self.http.get(&url).header("User-Agent", self.ua()).timeout(Duration::from_secs(300)).send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => { last = format!("HTTP {}", r.status()); continue; }
                Err(e) => { last = self.redact(e); continue; }
            };
            let tmp = unique_tmp_path(dest);
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| self.redact(e))?;
            let mut stream = res.bytes_stream();
            let mut ok = true;
            let mut first_non_ws: Option<u8> = None;
            let mut last_non_ws: Option<u8> = None;
            let mut prefix: Vec<u8> = Vec::with_capacity(512);
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(c) => {
                        if prefix.len() < 512 {
                            let take = (512 - prefix.len()).min(c.len());
                            prefix.extend_from_slice(&c[..take]);
                        }
                        if first_non_ws.is_none() {
                            if let Some(&b) = c.iter().find(|b| !b.is_ascii_whitespace()) {
                                first_non_ws = Some(b);
                            }
                        }
                        if let Some(&b) = c.iter().rev().find(|b| !b.is_ascii_whitespace()) {
                            last_non_ws = Some(b);
                        }
                        if let Err(e) = f.write_all(&c).await {
                            last = self.redact(e);
                            ok = false;
                            break;
                        }
                    }
                    Err(e) => {
                        last = self.redact(e);
                        ok = false;
                        break;
                    }
                }
            }
            f.flush().await.ok();
            drop(f);

            if !ok {
                let _ = tokio::fs::remove_file(&tmp).await;
                continue;
            }

            // Validate payload format (reject HTML Cloudflare error pages / truncated JSON before touching cached file)
            let dest_owned = dest.to_path_buf();
            let tmp_owned = tmp.clone();
            let valid = tokio::task::spawn_blocking(move || validate_payload(&dest_owned, &tmp_owned, first_non_ws, last_non_ws, &prefix))
                .await
                .unwrap_or_else(|e| Err(format!("validation task failed: {e}")));
            if let Err(reason) = valid {
                let _ = tokio::fs::remove_file(&tmp).await;
                eprintln!("[sync] {}: rejected invalid payload ({reason})", dest.display());
                last = reason;
                continue;
            }

            // Self-protection: providers sometimes answer "[]" or a truncated list for a moment.
            // Never let a response far smaller than the copy we already have replace it.
            let new_len = tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0);
            let old_len = tokio::fs::metadata(dest).await.map(|m| m.len()).unwrap_or(0);
            if old_len > 100_000 && new_len < old_len / 2 {
                let _ = tokio::fs::remove_file(&tmp).await;
                eprintln!("[sync] {}: got {new_len} bytes vs {old_len} cached, keeping cached copy", dest.display());
                last = format!("provider sent an incomplete list ({new_len} bytes)");
                continue;
            }

            match replace_file_retry(&tmp, dest).await {
                Ok(()) => {
                    self.active.store(idx, Ordering::Relaxed);
                    return Ok(());
                }
                Err(e) => {
                    last = self.redact(e);
                }
            }
        }
        Err(last)
    }

    pub fn epg_path(&self) -> String { let (u, p) = self.creds(); format!("/xmltv.php?username={}&password={}", enc(&u), enc(&p)) }

    /// `alt` = use the next server in the list (playback fallback).
    pub fn stream_url(&self, kind: &str, id: &str, ext: &str, alt: bool) -> String {
        let (u, p) = self.creds();
        let seg = match kind { "live" => "live", "movie" => "movie", _ => "series" };
        let servers = self.servers();
        if servers.is_empty() { return String::new(); }
        let idx = (self.active.load(Ordering::Relaxed) + if alt { 1 } else { 0 }) % servers.len();
        format!("{}/{seg}/{}/{}/{id}.{ext}", servers[idx], enc(&u), enc(&p))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_plaintext_and_encoded_credentials() {
        let msg = "error sending request for url (http://host.example/player_api.php?username=my%20user&password=p%40ss%21&action=get_vod_streams)";
        let clean = redact_credentials(msg, "my user", "p@ss!");
        assert!(!clean.contains("my user"));
        assert!(!clean.contains("p@ss!"));
        assert!(!clean.contains("my%20user"));
        assert!(!clean.contains("p%40ss%21"));
        assert!(clean.contains("username=***"));
        assert!(clean.contains("password=***"));
    }

    #[test]
    fn validates_json_and_rejects_cloudflare_html() {
        let tmp = std::env::temp_dir().join(format!("tv_xc_test_{}.tmp", now_ms()));
        std::fs::write(&tmp, br#"[{"id":1}]"#).unwrap();
        assert!(validate_payload(Path::new("get_vod_streams.json"), &tmp, Some(b'['), Some(b']'), br#"[{"id":1}]"#).is_ok());

        let cf = b"<!DOCTYPE html><html><head><title>Just a moment...</title></head></html>";
        std::fs::write(&tmp, cf).unwrap();
        assert!(validate_payload(Path::new("get_vod_streams.json"), &tmp, Some(b'<'), Some(b'>'), cf).is_err());
        assert!(validate_payload(Path::new("epg.xml"), &tmp, Some(b'<'), Some(b'>'), cf).is_err());

        std::fs::write(&tmp, b"<tv><channel id=\"1\"/></tv>").unwrap();
        assert!(validate_payload(Path::new("epg.xml"), &tmp, Some(b'<'), Some(b'>'), b"<tv><channel id=\"1\"/></tv>").is_ok());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn stream_url_encodes_creds_and_handles_empty_servers() {
        let tmp_cfg = std::env::temp_dir().join(format!("tv_xc_cfg_{}.json", now_ms()));
        std::fs::write(&tmp_cfg, r#"{"servers":[],"username":"a/b","password":"p@s&s"}"#).unwrap();
        let cfg = Arc::new(Config::load(tmp_cfg.clone()));
        let xc = Xc::new(cfg.clone(), reqwest::Client::new());
        assert_eq!(xc.stream_url("movie", "42", "mkv", false), "");
        assert_eq!(xc.base(), "");

        cfg.set("servers", serde_json::json!(["http://example.test:8080"]));
        let u = xc.stream_url("movie", "42", "mkv", false);
        assert_eq!(u, "http://example.test:8080/movie/a%2Fb/p%40s%26s/42.mkv");
        let _ = std::fs::remove_file(&tmp_cfg);
    }
}
