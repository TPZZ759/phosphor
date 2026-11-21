// Notifications, OMDb scores, new-episode watcher.
use crate::app::App;
use crate::util::{now_ms, s};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------- Windows toast via PowerShell (no installs). Clicking opens the app.
pub mod notify {
    pub fn notify(title: &str, body: &str, link: &str) {
        let x = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;");
        let xml = format!(r#"<toast activationType="protocol" launch="{}"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#, x(link), x(title), x(body));
        let ps = format!(r#"[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null
$x = New-Object Windows.Data.Xml.Dom.XmlDocument
$x.LoadXml('{}')
$app = '{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($x))"#, xml.replace('\'', "''"));
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &ps]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)] { use std::os::windows::process::CommandExt; cmd.creation_flags(0x08000000); }
        let _ = cmd.spawn();
        println!("[notify] {title} — {body}");
    }
}

// ---------- OMDb (IMDb / RT / Metacritic), cached 14 days
pub struct Omdb { file: std::path::PathBuf, cache: Mutex<HashMap<String, (i64, Value)>> }
impl Omdb {
    pub fn new(file: std::path::PathBuf) -> Self {
        let mut cache = HashMap::new();
        if let Some(Value::Object(m)) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()) { for (k, v) in m { cache.insert(k, (v["t"].as_i64().unwrap_or(0), v["v"].clone())); } }
        Omdb { file, cache: Mutex::new(cache) }
    }
    pub fn get(&self, imdb: &str) -> Option<Value> {
        if imdb.is_empty() { return None; }
        self.cache.lock().unwrap().get(imdb).map(|(_, v)| v.clone())
    }
    pub async fn test(app: &App, key: &str) -> bool {
        let url = format!("https://www.omdbapi.com/?i=tt0111161&apikey={}", crate::util::enc(key));
        match app.http.get(&url).timeout(Duration::from_secs(10)).send().await { Ok(r) => r.json::<Value>().await.map(|j| j["Response"] == "True").unwrap_or(false), Err(_) => false }
    }
    pub async fn ratings(app: &App, imdb: &str) -> Value {
        let key = app.cfg.str("omdbKey"); if key.trim().is_empty() || imdb.is_empty() { return Value::Null; }
        let hit = app.omdb.cache.lock().unwrap().get(imdb).cloned();
        if let Some((t, v)) = &hit { if now_ms() - t < 14 * 86_400_000 { return v.clone(); } }
        let url = format!("https://www.omdbapi.com/?i={}&apikey={}", crate::util::enc(imdb), crate::util::enc(key.trim()));
        let Ok(r) = (async { app.http.get(&url).timeout(Duration::from_secs(8)).send().await?.json::<Value>().await }).await else { return hit.map(|h| h.1).unwrap_or(Value::Null) };
        if r["Response"] != "True" { return hit.map(|h| h.1).unwrap_or(Value::Null); }
        let find = |src: &str| r["Ratings"].as_array().into_iter().flatten().find(|x| x["Source"] == src).map(|x| s(&x["Value"])).unwrap_or_default();
        let na = |v: &Value| { let x = s(v); if x == "N/A" { String::new() } else { x } };
        let v = json!({ "imdb": na(&r["imdbRating"]), "votes": na(&r["imdbVotes"]), "rt": find("Rotten Tomatoes"), "mc": find("Metacritic").replace("/100", ""), "awards": na(&r["Awards"]) });
        app.omdb.cache.lock().unwrap().insert(imdb.to_string(), (now_ms(), v.clone()));
        let obj: serde_json::Map<String, Value> = app.omdb.cache.lock().unwrap().iter().map(|(k, (t, v))| (k.clone(), json!({ "t": t, "v": v }))).collect();
        let _ = std::fs::write(&app.omdb.file, serde_json::to_vec(&obj).unwrap());
        v
    }
}

// ---------- New-episode watcher (metadata calls only, no stream connection)
fn auto_follow(app: &App) {
    app.state.with(|st| {
        let mut add: Vec<(String, Value)> = vec![];
        for (k, v) in st["list"].as_object().into_iter().flatten() { if let Some(sid) = k.strip_prefix("s:") { add.push((sid.to_string(), v["item"].clone())); } }
        for p in st["progress"].as_object().into_iter().flat_map(|m| m.values()) { if p["item"]["k"] == "e" && !p["item"]["sid"].is_null() { add.push((s(&p["item"]["sid"]), p["item"].clone())); } }
        for (sid, it) in add { if st["follows"].get(&sid).is_none() { st["follows"][&sid] = json!({ "title": s(&it["title"]), "poster": s(&it["poster"]), "backdrop": s(&it["backdrop"]), "known": null, "t": now_ms() }); } }
    });
}
pub async fn check_episodes(app: Arc<App>) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
    auto_follow(&app);
    let follows: Vec<(String, Value)> = app.state.read(|st| st["follows"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default());
    for (sid, f) in follows {
        let Ok(info) = app.xc.api("get_series_info", format!("&series_id={sid}"), 20000).await else { continue };
        let mut eps: Vec<(i64, i64, i64, String)> = vec![];
        for (sn, list) in info["episodes"].as_object().into_iter().flatten() { for e in list.as_array().into_iter().flatten() { eps.push((crate::util::i(&e["id"]), sn.parse().unwrap_or(0), crate::util::i(&e["episode_num"]), s(&e["title"]))); } }
        if eps.is_empty() { continue; }
        let ids: Vec<i64> = eps.iter().map(|e| e.0).collect();
        if f["known"].is_null() { app.state.with(|st| st["follows"][&sid]["known"] = json!(ids)); app.state.save(); continue; }
        let known: HashSet<i64> = f["known"].as_array().into_iter().flatten().filter_map(|x| x.as_i64()).collect();
        let mut fresh: Vec<&(i64, i64, i64, String)> = eps.iter().filter(|e| !known.contains(&e.0)).collect();
        if !fresh.is_empty() {
            fresh.sort_by_key(|e| (e.1, e.2));
            let (first, last) = (fresh[0], fresh[fresh.len() - 1]);
            let title = s(&f["title"]);
            app.state.with(|st| {
                let mut all: Vec<i64> = st["newEps"][&sid]["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_i64()).collect();
                for e in &fresh { if !all.contains(&e.0) { all.push(e.0); } }
                st["newEps"][&sid] = json!({ "ids": all, "latest": { "id": last.0, "s": last.1, "e": last.2, "title": last.3 }, "first": { "id": first.0, "s": first.1, "e": first.2 }, "t": now_ms(), "title": title, "poster": f["poster"], "backdrop": f["backdrop"] });
                st["follows"][&sid]["known"] = json!(ids);
            });
            app.state.save();
            let clean = crate::re!(r"(?i)^.*?S\d+E\d+\s*-?\s*").replace(&last.3, "").to_string();
            crate::notify_all(&app, "episode", &format!("New episode{}: {title}", if fresh.len() > 1 { "s" } else { "" }),
                &if fresh.len() > 1 { format!("{} new episodes, up to S{}:E{}", fresh.len(), last.1, last.2) } else { format!("S{}:E{}{}", last.1, last.2, if clean.is_empty() { String::new() } else { format!(" · {clean}") }) },
                "http://localhost:7878/", json!({ "k": "s", "id": sid.parse::<i64>().unwrap_or(0), "title": title, "label": "Open show" }));
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
}
pub fn follow(app: &Arc<App>, sid: &str, on: bool, item: &Value) {
    app.state.with(|st| {
        if on { if st["follows"].get(sid).is_none() { st["follows"][sid] = json!({ "title": s(&item["title"]), "poster": s(&item["poster"]), "backdrop": s(&item["backdrop"]), "known": null, "t": now_ms() }); } }
        else { if let Some(m) = st["follows"].as_object_mut() { m.remove(sid); } if let Some(m) = st["newEps"].as_object_mut() { m.remove(sid); } }
    });
    app.state.save();
    if on { tokio::spawn(check_episodes(app.clone())); }
}
