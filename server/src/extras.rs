// Notifications, OMDb scores (with 7d negative cache & atomic writes), and variant-aware new-episode watcher.
use crate::app::App;
use crate::util::{now_ms, s};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OMDB_POS_TTL_MS: i64 = 14 * 86_400_000;
const OMDB_NEG_TTL_MS: i64 = 7 * 86_400_000;

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

// ---------- OMDb (IMDb / RT / Metacritic), cached 14 days (misses cached as "none" for 7 days)
pub struct Omdb { file: std::path::PathBuf, cache: Mutex<HashMap<String, (i64, Value)>> }
impl Omdb {
    pub fn new(file: std::path::PathBuf) -> Self {
        let mut cache = HashMap::new();
        if let Some(Value::Object(m)) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            for (k, v) in m { cache.insert(k, (v["t"].as_i64().unwrap_or(0), v["v"].clone())); }
        }
        Omdb { file, cache: Mutex::new(cache) }
    }
    pub fn get(&self, imdb: &str) -> Option<Value> {
        if imdb.is_empty() { return None; }
        self.cache.lock().unwrap().get(imdb).and_then(|(_, v)| v.is_object().then(|| v.clone()))
    }
    fn persist(&self) {
        let bytes = {
            let obj: serde_json::Map<String, Value> = self.cache.lock().unwrap()
                .iter()
                .map(|(k, (t, v))| (k.clone(), json!({ "t": t, "v": v })))
                .collect();
            serde_json::to_vec(&obj).ok()
        };
        if let Some(bytes) = bytes {
            let _ = crate::store::atomic_write(&self.file, &bytes);
        }
    }
    pub async fn test(app: &App, key: &str) -> bool {
        let url = format!("https://www.omdbapi.com/?i=tt0111161&apikey={}", crate::util::enc(key));
        match app.http.get(&url).timeout(Duration::from_secs(10)).send().await {
            Ok(r) => r.json::<Value>().await.map(|j| j["Response"] == "True").unwrap_or(false),
            Err(_) => false,
        }
    }
    pub async fn ratings(app: &App, imdb: &str) -> Value {
        let key = app.cfg.str("omdbKey");
        if key.trim().is_empty() || imdb.is_empty() { return Value::Null; }
        let hit = app.omdb.cache.lock().unwrap().get(imdb).cloned();
        let now = now_ms();
        if let Some((t, v)) = &hit {
            let is_neg = v.as_str() == Some("none") || v.is_null();
            let ttl = if is_neg { OMDB_NEG_TTL_MS } else { OMDB_POS_TTL_MS };
            if now - t < ttl {
                return if is_neg { Value::Null } else { v.clone() };
            }
        }
        let url = format!("https://www.omdbapi.com/?i={}&apikey={}", crate::util::enc(imdb), crate::util::enc(key.trim()));
        let Ok(r) = (async { app.http.get(&url).timeout(Duration::from_secs(8)).send().await?.json::<Value>().await }).await else {
            return hit.and_then(|(_, v)| v.is_object().then_some(v)).unwrap_or(Value::Null);
        };
        if r["Response"] != "True" {
            // Cache genuine misses ("Movie not found!") for 7 days so we don't burn OMDb quota repeatedly,
            // while never caching transient quota-limit or invalid-key failures.
            let err_low = s(&r["Error"]).to_lowercase();
            let transient = err_low.contains("limit") || err_low.contains("invalid api key") || err_low.contains("api key");
            if !transient {
                app.omdb.cache.lock().unwrap().insert(imdb.to_string(), (now_ms(), json!("none")));
                app.omdb.persist();
            }
            return hit.and_then(|(_, v)| v.is_object().then_some(v)).unwrap_or(Value::Null);
        }
        let find = |src: &str| r["Ratings"].as_array().into_iter().flatten().find(|x| x["Source"] == src).map(|x| s(&x["Value"])).unwrap_or_default();
        let na = |v: &Value| { let x = s(v); if x == "N/A" { String::new() } else { x } };
        let v = json!({
            "imdb": na(&r["imdbRating"]),
            "votes": na(&r["imdbVotes"]),
            "rt": find("Rotten Tomatoes"),
            "mc": find("Metacritic").replace("/100", ""),
            "awards": na(&r["Awards"])
        });
        app.omdb.cache.lock().unwrap().insert(imdb.to_string(), (now_ms(), v.clone()));
        app.omdb.persist();
        v
    }
}

// ---------- New-episode watcher (metadata calls only, no stream connection)

fn se_key(sn: i64, en: i64) -> String {
    format!("S{sn}:E{en}")
}

fn is_show_ended(app: &App, sid: i64) -> bool {
    let cat = app.catalog();
    let Some(it) = cat.series_item(sid) else { return false };
    let Some(m) = app.meta_of(it) else { return false };
    matches!(m.st.trim().to_ascii_lowercase().as_str(), "ended" | "canceled" | "cancelled")
}

/// Fetch fresh `get_series_info` across the primary series ID and its non-CAM EN variants,
/// updating `app.xinfo` immediately so clicking a notification shows the new episodes right away.
async fn fetch_series_episodes(app: &App, sid: i64) -> Option<Vec<(i64, i64, i64, String)>> {
    let variant_ids: Vec<i64> = {
        let cat = app.catalog();
        let mut ids = vec![sid];
        if let Some(it) = cat.series_item(sid) {
            if !ids.contains(&it.id) { ids.push(it.id); }
            for v in &it.variants {
                if !v.cam && !v.sub_market && !ids.contains(&v.id) {
                    ids.push(v.id);
                    if ids.len() >= 4 { break; }
                }
            }
        }
        ids
    };

    // Deduplicate episodes by logical (season, episode_num); primary variant wins on collisions.
    let mut by_se: BTreeMap<(i64, i64), (i64, String)> = BTreeMap::new();
    let mut any_ok = false;

    for vsid in variant_ids {
        let Ok(info) = app.xc.api("get_series_info", format!("&series_id={vsid}"), 20_000).await else {
            continue;
        };
        any_ok = true;
        // Keep app.xinfo fresh so opening the show immediately includes newly discovered episodes!
        {
            let mut m = app.xinfo.lock().unwrap();
            if m.len() > 3000 { m.clear(); }
            m.insert(format!("get_series_info:{vsid}"), (now_ms(), Arc::new(info.clone())));
        }
        for (sn_s, list) in info["episodes"].as_object().into_iter().flatten() {
            let sn: i64 = sn_s.parse().unwrap_or(0);
            for e in list.as_array().into_iter().flatten() {
                let eid = crate::util::i(&e["id"]);
                let en = crate::util::i(&e["episode_num"]);
                if eid <= 0 { continue; }
                by_se.entry((sn, en)).or_insert_with(|| (eid, s(&e["title"])));
            }
        }
    }

    if !any_ok || by_se.is_empty() {
        return None;
    }
    Some(by_se.into_iter().map(|((sn, en), (eid, title))| (eid, sn, en, title)).collect())
}

fn auto_follow(app: &App) {
    app.state.with(|st| {
        let mut add: Vec<(String, Value)> = vec![];
        for (k, v) in st["list"].as_object().into_iter().flatten() {
            if let Some(sid) = k.strip_prefix("s:") { add.push((sid.to_string(), v["item"].clone())); }
        }
        for p in st["progress"].as_object().into_iter().flat_map(|m| m.values()) {
            if p["item"]["k"] == "e" && !p["item"]["sid"].is_null() {
                add.push((s(&p["item"]["sid"]), p["item"].clone()));
            }
        }
        for (sid, it) in add {
            if st["follows"].get(&sid).is_none() {
                st["follows"][&sid] = json!({
                    "title": s(&it["title"]), "poster": s(&it["poster"]), "backdrop": s(&it["backdrop"]),
                    "known": null, "knownSe": null, "t": now_ms()
                });
            }
        }
    });
}

struct RunningGuard(&'static std::sync::atomic::AtomicBool);
impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub async fn check_episodes(app: Arc<App>) {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
    let _guard = RunningGuard(&RUNNING);

    auto_follow(&app);
    let follows: Vec<(String, Value)> = app.state.read(|st| {
        st["follows"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
    });
    let home_url = format!("http://localhost:{}/", app.port());

    for (sid, f) in follows {
        let Ok(sid_num) = sid.parse::<i64>() else { continue };
        // Automatically skip Ended / Canceled shows to save provider API calls.
        if is_show_ended(&app, sid_num) { continue; }

        let Some(eps) = fetch_series_episodes(&app, sid_num).await else { continue };
        let ids: Vec<i64> = eps.iter().map(|e| e.0).collect();
        let ses: Vec<String> = eps.iter().map(|e| se_key(e.1, e.2)).collect();

        let has_known_se = f["knownSe"].is_array();
        let has_known_ids = f["known"].is_array();

        // Brand-new follow: seed both `known` and `knownSe` silently.
        if !has_known_se && !has_known_ids {
            app.state.with(|st| {
                if st["follows"].get(&sid).is_some() {
                    st["follows"][&sid]["known"] = json!(ids);
                    st["follows"][&sid]["knownSe"] = json!(ses);
                }
            });
            app.state.save();
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        }

        let known_ids: HashSet<i64> = f["known"].as_array().into_iter().flatten().filter_map(|x| x.as_i64()).collect();
        let mut fresh: Vec<&(i64, i64, i64, String)> = if has_known_se {
            let known_se: HashSet<String> = f["knownSe"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
            // Track by (season, episode_num) across all variants so provider re-imports with new IDs NEVER spam!
            eps.iter().filter(|e| !known_se.contains(&se_key(e.1, e.2)) && !known_ids.contains(&e.0)).collect()
        } else {
            // One-time migration from legacy ID-only `known`:
            // If zero old IDs match (provider re-imported the whole series), silently migrate `knownSe` without spamming!
            let overlap = eps.iter().filter(|e| known_ids.contains(&e.0)).count();
            if overlap == 0 {
                app.state.with(|st| {
                    if st["follows"].get(&sid).is_some() {
                        st["follows"][&sid]["known"] = json!(ids);
                        st["follows"][&sid]["knownSe"] = json!(ses);
                    }
                });
                app.state.save();
                tokio::time::sleep(Duration::from_millis(800)).await;
                continue;
            }
            let max_known_se = eps.iter().filter(|e| known_ids.contains(&e.0)).map(|e| (e.1, e.2)).max().unwrap_or((0, 0));
            let migrated_fresh: Vec<&(i64, i64, i64, String)> = eps.iter()
                .filter(|e| !known_ids.contains(&e.0) && (e.1, e.2) > max_known_se)
                .collect();
            if migrated_fresh.is_empty() {
                app.state.with(|st| {
                    if st["follows"].get(&sid).is_some() {
                        st["follows"][&sid]["known"] = json!(ids);
                        st["follows"][&sid]["knownSe"] = json!(ses);
                    }
                });
                app.state.save();
            }
            migrated_fresh
        };

        if !fresh.is_empty() {
            fresh.sort_by_key(|e| (e.1, e.2));
            let (first, last) = (fresh[0], fresh[fresh.len() - 1]);
            let title = s(&f["title"]);
            app.state.with(|st| {
                let mut all: Vec<i64> = st["newEps"][&sid]["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_i64()).collect();
                for e in &fresh { if !all.contains(&e.0) { all.push(e.0); } }
                st["newEps"][&sid] = json!({
                    "ids": all,
                    "latest": { "id": last.0, "s": last.1, "e": last.2, "title": last.3 },
                    "first": { "id": first.0, "s": first.1, "e": first.2 },
                    "t": now_ms(), "title": title, "poster": f["poster"], "backdrop": f["backdrop"]
                });
                if st["follows"].get(&sid).is_some() {
                    st["follows"][&sid]["known"] = json!(ids);
                    st["follows"][&sid]["knownSe"] = json!(ses);
                }
            });
            app.state.save();
            let clean = crate::re!(r"(?i)^.*?S\d+E\d+\s*-?\s*").replace(&last.3, "").to_string();
            crate::notify_all(
                &app,
                "episode",
                &format!("New episode{}: {title}", if fresh.len() > 1 { "s" } else { "" }),
                &if fresh.len() > 1 {
                    format!("{} new episodes, up to S{}:E{}", fresh.len(), last.1, last.2)
                } else {
                    format!("S{}:E{}{}", last.1, last.2, if clean.is_empty() { String::new() } else { format!(" · {clean}") })
                },
                &home_url,
                json!({ "k": "s", "id": sid_num, "title": title, "label": "Open show" }),
            );
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
}

/// Immediately seed a newly followed show's `known` + `knownSe` even if `check_episodes` is already running.
async fn seed_follow(app: Arc<App>, sid: String) {
    let Ok(sid_num) = sid.parse::<i64>() else { return };
    let Some(eps) = fetch_series_episodes(&app, sid_num).await else { return };
    let ids: Vec<i64> = eps.iter().map(|e| e.0).collect();
    let ses: Vec<String> = eps.iter().map(|e| se_key(e.1, e.2)).collect();
    app.state.with(|st| {
        if let Some(entry) = st["follows"].get_mut(&sid) {
            if entry["knownSe"].is_null() {
                entry["known"] = json!(ids);
                entry["knownSe"] = json!(ses);
            }
        }
    });
    app.state.save();
}

pub fn follow(app: &Arc<App>, sid: &str, on: bool, item: &Value) {
    app.state.with(|st| {
        if on {
            if st["follows"].get(sid).is_none() {
                st["follows"][sid] = json!({
                    "title": s(&item["title"]), "poster": s(&item["poster"]), "backdrop": s(&item["backdrop"]),
                    "known": null, "knownSe": null, "t": now_ms()
                });
            }
        } else {
            if let Some(m) = st["follows"].as_object_mut() { m.remove(sid); }
            if let Some(m) = st["newEps"].as_object_mut() { m.remove(sid); }
        }
    });
    app.state.save();
    if on {
        tokio::spawn(seed_follow(app.clone(), sid.to_string()));
    }
}
