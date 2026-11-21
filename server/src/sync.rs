// Import / sync your viewing history from elsewhere.
// - Letterboxd: no public API, so we read the data export (.zip, or the CSVs in it): watched films,
//   ratings (3.5+ = like, 2 or less = dislike) and the watchlist (-> Wanted, which alerts on arrival).
// - Trakt: OAuth device flow with the viewer's own (free) Trakt API app. Pull: watched films and
//   episodes, ratings, watchlist. Push: what you finish, rate and add here.
use crate::app::App;
use crate::rec::sp;
use crate::util::{now_ms, s};
use axum::{body::Bytes, extract::State, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type A = State<Arc<App>>;
const TTL_W: i64 = 7 * 86_400_000;

#[derive(Default)]
pub struct Jobs { pub import: Mutex<Value>, pub trakt: Mutex<Value> }

fn err(code: StatusCode, m: &str) -> Response { (code, Json(json!({ "error": m }))).into_response() }

// ---------------------------------------------------------------- CSV
fn csv(text: &str) -> Vec<HashMap<String, String>> {
    let mut rows: Vec<Vec<String>> = vec![]; let mut row = vec![]; let mut f = String::new(); let mut q = false;
    let mut it = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = it.next() {
        match (c, q) {
            ('"', true) if it.peek() == Some(&'"') => { f.push('"'); it.next(); }
            ('"', _) => q = !q,
            (',', false) => { row.push(std::mem::take(&mut f)); }
            ('\n', false) => { row.push(std::mem::take(&mut f)); rows.push(std::mem::take(&mut row)); }
            ('\r', false) => {}
            _ => f.push(c),
        }
    }
    if !f.is_empty() || !row.is_empty() { row.push(f); rows.push(row); }
    let Some(head) = rows.first().cloned() else { return vec![] };
    rows.into_iter().skip(1).filter(|r| r.len() > 1).map(|r| head.iter().cloned().zip(r).collect()).collect()
}

fn files_from(body: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if body.starts_with(b"PK") {
        if let Ok(mut z) = zip::ZipArchive::new(std::io::Cursor::new(body)) {
            for i in 0..z.len() { if let Ok(mut f) = z.by_index(i) {
                let name = f.name().rsplit('/').next().unwrap_or("").to_lowercase();
                if !name.ends_with(".csv") || f.name().contains("deleted/") || f.name().contains("orphaned/") { continue; }
                let mut t = String::new(); use std::io::Read; if f.read_to_string(&mut t).is_ok() { out.insert(name, t); }
            } }
        }
    } else if let Ok(v) = serde_json::from_slice::<Value>(body) {
        // {"files": {"watched.csv": "..."}}
        for (k, t) in v["files"].as_object().into_iter().flatten() { out.insert(k.to_lowercase(), s(t)); }
    }
    out
}

async fn tmdb_movie(app: &App, title: &str, year: &str) -> Option<(String, String)> {
    let r = app.tmdb.get("/search/movie", [sp("query", title), sp("year", year)], TTL_W).await?;
    let hit = r["results"].as_array()?.first()?.clone();
    Some((hit["id"].as_i64()?.to_string(), s(&hit["poster_path"])))
}

pub async fn letterboxd(State(app): A, body: Bytes) -> Response {
    if !app.tmdb.enabled() { return err(StatusCode::BAD_REQUEST, "Add a TMDB key first (needed to match films)."); }
    let files = files_from(&body);
    let watched = files.get("watched.csv").map(|t| csv(t)).unwrap_or_default();
    let ratings = files.get("ratings.csv").map(|t| csv(t)).unwrap_or_default();
    let watchlist = files.get("watchlist.csv").map(|t| csv(t)).unwrap_or_default();
    if watched.is_empty() && ratings.is_empty() && watchlist.is_empty() { return err(StatusCode::BAD_REQUEST, "That doesn't look like a Letterboxd export. Use the .zip from letterboxd.com/settings/data (Export your data)."); }
    if app.jobs.import.lock().unwrap()["running"].as_bool() == Some(true) { return err(StatusCode::CONFLICT, "An import is already running."); }
    let total = watched.len() + ratings.len() + watchlist.len();
    *app.jobs.import.lock().unwrap() = json!({ "running": true, "source": "Letterboxd", "done": 0, "total": total });
    let a = app.clone();
    tokio::spawn(async move {
        let mut todo: Vec<(u8, HashMap<String, String>)> = vec![];
        for r in watched { todo.push((b'w', r)); } for r in ratings { todo.push((b'r', r)); } for r in watchlist { todo.push((b'l', r)); }
        let mut results: Vec<(u8, HashMap<String, String>, Option<(String, String)>)> = vec![];
        for chunk in todo.chunks(6) {
            let found = futures_util::future::join_all(chunk.iter().map(|(_, r)| { let a = a.clone(); let (n, y) = (r.get("Name").cloned().unwrap_or_default(), r.get("Year").cloned().unwrap_or_default()); async move { tmdb_movie(&a, &n, &y).await } })).await;
            for ((k, r), m) in chunk.iter().cloned().zip(found) { results.push((k, r, m)); }
            { let mut j = a.jobs.import.lock().unwrap(); j["done"] = json!(results.len()); }
        }
        let (mut nw, mut nl, mut nd, mut nq, mut miss) = (0, 0, 0, 0, vec![]);
        let date_ms = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(12, 0, 0)).map(|t| t.and_utc().timestamp_millis()).unwrap_or_else(now_ms);
        a.state.with(|st| for (k, r, m) in &results {
            let title = r.get("Name").cloned().unwrap_or_default();
            let Some((id, poster)) = m else { if miss.len() < 40 { miss.push(title); } continue };
            let key = format!("movie:{id}");
            match *k {
                b'w' => { if st["watched"].get(&key).is_none() { st["watched"][&key] = json!({ "t": date_ms(r.get("Date").map(|x| x.as_str()).unwrap_or("")), "title": title, "src": "letterboxd" }); nw += 1; } }
                b'r' => { let v: f64 = r.get("Rating").and_then(|x| x.parse().ok()).unwrap_or(0.0);
                         if v >= 3.5 || (v > 0.0 && v <= 2.0) { st["ratings"][&key] = json!({ "v": if v >= 3.5 { 1 } else { -1 }, "title": title, "t": now_ms(), "src": "letterboxd" }); if v >= 3.5 { nl += 1 } else { nd += 1 } }
                         if st["watched"].get(&key).is_none() { st["watched"][&key] = json!({ "t": now_ms(), "title": title, "src": "letterboxd" }); nw += 1; } }
                _ => { if st["wanted"].get(&key).is_none() { st["wanted"][&key] = json!({ "title": title, "year": r.get("Year").and_then(|y| y.parse::<i64>().ok()), "poster": if poster.is_empty() { Value::Null } else { json!(format!("https://image.tmdb.org/t/p/w342{poster}")) }, "t": now_ms() }); nq += 1; } }
            }
        });
        a.state.save(); a.memo.clear(); tokio::spawn(crate::prewarm_pub(a.clone()));
        *a.jobs.import.lock().unwrap() = json!({ "running": false, "source": "Letterboxd", "done": total, "total": total,
            "result": { "watched": nw, "liked": nl, "disliked": nd, "watchlist": nq, "unmatched": miss } });
        println!("[import] letterboxd: {nw} watched, {nl} liked, {nd} disliked, {nq} watchlist");
    });
    Json(json!({ "ok": true, "total": total })).into_response()
}
pub async fn import_status(State(app): A) -> Response { Json(app.jobs.import.lock().unwrap().clone()).into_response() }

// ---------------------------------------------------------------- Trakt
const API: &str = "https://api.trakt.tv";
fn tcfg(app: &App) -> (String, String) { (app.cfg.str("traktClientId"), app.cfg.str("traktClientSecret")) }
fn token(app: &App) -> Option<String> { let t = app.cfg.get("traktAuth"); t["access_token"].as_str().map(String::from) }

async fn call(app: &App, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
    refresh_if_needed(app).await;
    let (cid, _) = tcfg(app);
    let mut rq = app.http.request(method, format!("{API}{path}")).header("trakt-api-version", "2").header("trakt-api-key", &cid).header("Content-Type", "application/json").header("User-Agent", "tv-app/1.0").timeout(std::time::Duration::from_secs(30));
    if let Some(t) = token(app) { rq = rq.bearer_auth(t); }
    if let Some(b) = body { rq = rq.json(&b); }
    let r = rq.send().await.map_err(|e| e.to_string())?;
    let st = r.status(); let txt = r.text().await.unwrap_or_default();
    if !st.is_success() { return Err(format!("Trakt {st}")); }
    Ok(serde_json::from_str(&txt).unwrap_or(Value::Null))
}
async fn refresh_if_needed(app: &App) {
    let a = app.cfg.get("traktAuth"); let (cid, sec) = tcfg(app);
    let exp = a["created_at"].as_i64().unwrap_or(0) + a["expires_in"].as_i64().unwrap_or(0);
    if a["refresh_token"].is_null() || exp - 86_400 > now_ms() / 1000 { return; }
    let r = app.http.post(format!("{API}/oauth/token")).json(&json!({ "refresh_token": a["refresh_token"], "client_id": cid, "client_secret": sec, "redirect_uri": "urn:ietf:wg:oauth:2.0:oob", "grant_type": "refresh_token" })).send().await;
    if let Ok(r) = r { if r.status().is_success() { if let Ok(v) = r.json::<Value>().await { app.cfg.set("traktAuth", v); } } }
}

pub async fn trakt_info(State(app): A) -> Response {
    let (cid, _) = tcfg(&app);
    let mut j = app.jobs.trakt.lock().unwrap().clone(); if j.is_null() { j = json!({}); }
    j["hasApp"] = json!(!cid.is_empty()); j["connected"] = json!(token(&app).is_some()); j["user"] = app.cfg.get("traktUser"); j["lastSync"] = app.cfg.get("traktLastSync");
    Json(j).into_response()
}
/// Start the device flow: the viewer opens trakt.tv/activate and types the code.
pub async fn trakt_connect(State(app): A, Json(b): Json<Value>) -> Response {
    if let (Some(c), Some(sec)) = (b["clientId"].as_str(), b["clientSecret"].as_str()) { if !c.trim().is_empty() { app.cfg.set("traktClientId", json!(c.trim())); app.cfg.set("traktClientSecret", json!(sec.trim())); } }
    let (cid, sec) = tcfg(&app);
    if cid.is_empty() || sec.is_empty() { return err(StatusCode::BAD_REQUEST, "Paste your Trakt app's Client ID and Client Secret first."); }
    let r = match app.http.post(format!("{API}/oauth/device/code")).json(&json!({ "client_id": cid })).send().await { Ok(r) => r, Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()) };
    if !r.status().is_success() { return err(StatusCode::BAD_REQUEST, "Trakt didn't accept that Client ID. Check it on trakt.tv/oauth/applications."); }
    let d: Value = r.json().await.unwrap_or(Value::Null);
    *app.jobs.trakt.lock().unwrap() = json!({ "pending": { "code": d["user_code"], "url": d["verification_url"], "until": now_ms() + d["expires_in"].as_i64().unwrap_or(600) * 1000 } });
    let a = app.clone(); let dc = s(&d["device_code"]); let every = d["interval"].as_u64().unwrap_or(5); let until = now_ms() + d["expires_in"].as_i64().unwrap_or(600) * 1000;
    tokio::spawn(async move {
        while now_ms() < until {
            tokio::time::sleep(std::time::Duration::from_secs(every)).await;
            let r = a.http.post(format!("{API}/oauth/device/token")).json(&json!({ "code": dc, "client_id": cid, "client_secret": sec })).send().await;
            let Ok(r) = r else { continue };
            if r.status().as_u16() == 200 {
                if let Ok(v) = r.json::<Value>().await { a.cfg.set("traktAuth", v); }
                if let Ok(me) = call(&a, reqwest::Method::GET, "/users/me", None).await { a.cfg.set("traktUser", json!(s(&me["username"]))); }
                *a.jobs.trakt.lock().unwrap() = json!({ "connected": true });
                pull(a.clone()).await; return;
            }
            if [404u16, 409, 410, 418].contains(&r.status().as_u16()) { break; } // invalid / used / expired / denied
        }
        *a.jobs.trakt.lock().unwrap() = json!({ "error": "The code expired or was declined. Try connecting again." });
    });
    Json(json!({ "code": d["user_code"], "url": d["verification_url"] })).into_response()
}
pub async fn trakt_disconnect(State(app): A) -> Response {
    for k in ["traktAuth", "traktUser"] { app.cfg.set(k, Value::Null); }
    *app.jobs.trakt.lock().unwrap() = json!({}); Json(json!({ "ok": true })).into_response()
}
pub async fn trakt_sync(State(app): A) -> Response {
    if token(&app).is_none() { return err(StatusCode::BAD_REQUEST, "Connect Trakt first."); }
    tokio::spawn(pull(app.clone())); Json(json!({ "ok": true })).into_response()
}

/// Pull history, ratings and watchlist from Trakt into this app.
pub async fn pull(app: Arc<App>) {
    if token(&app).is_none() { return; }
    { let mut j = app.jobs.trakt.lock().unwrap(); if !j.is_object() { *j = json!({}); } j["syncing"] = json!(true); }
    let get = |p: &'static str| { let a = app.clone(); async move { call(&a, reqwest::Method::GET, p, None).await.unwrap_or(Value::Null) } };
    let (wm, ws, rm, rs, lm, ls) = tokio::join!(get("/sync/watched/movies"), get("/sync/watched/shows"), get("/sync/ratings/movies"), get("/sync/ratings/shows"), get("/sync/watchlist/movies"), get("/sync/watchlist/shows"));
    let ms = |t: &Value| chrono::DateTime::parse_from_rfc3339(&s(t)).map(|d| d.timestamp_millis()).unwrap_or_else(|_| now_ms());
    let (mut n_m, mut n_r, mut n_l, mut n_e) = (0, 0, 0, 0);
    app.state.with(|st| {
        for x in wm.as_array().into_iter().flatten() { if let Some(id) = x["movie"]["ids"]["tmdb"].as_i64() { let k = format!("movie:{id}"); if st["watched"].get(&k).is_none() { st["watched"][&k] = json!({ "t": ms(&x["last_watched_at"]), "title": x["movie"]["title"], "src": "trakt" }); n_m += 1; } } }
        for (arr, mt, kk) in [(&rm, "movie", "movie"), (&rs, "tv", "show")] { for x in arr.as_array().into_iter().flatten() {
            let Some(id) = x[kk]["ids"]["tmdb"].as_i64() else { continue }; let v = x["rating"].as_i64().unwrap_or(0);
            if v >= 7 || (v > 0 && v <= 4) { st["ratings"][format!("{mt}:{id}")] = json!({ "v": if v >= 7 { 1 } else { -1 }, "title": x[kk]["title"], "t": ms(&x["rated_at"]), "src": "trakt" }); n_r += 1; }
        } }
        for (arr, mt, kk) in [(&lm, "movie", "movie"), (&ls, "tv", "show")] { for x in arr.as_array().into_iter().flatten() {
            let Some(id) = x[kk]["ids"]["tmdb"].as_i64() else { continue }; let k = format!("{mt}:{id}");
            if st["wanted"].get(&k).is_none() { st["wanted"][&k] = json!({ "title": x[kk]["title"], "year": x[kk]["year"], "t": ms(&x["listed_at"]) }); n_l += 1; }
        } }
    });
    // watched episodes of shows that exist here -> mark those episodes watched (Next up, progress)
    let cat = app.catalog();
    for x in ws.as_array().into_iter().flatten().take(150) {
        let Some(tm) = x["show"]["ids"]["tmdb"].as_i64() else { continue };
        let Some(it) = cat.series_by_tmdb.get(&tm.to_string()).map(|&i| cat.series[i].clone()) else { continue };
        let info = app.xc_info("get_series_info", "series_id", it.id, 6 * 3_600_000).await;
        let mut marks = vec![];
        for season in x["seasons"].as_array().into_iter().flatten() { let sn = season["number"].as_i64().unwrap_or(-1);
            for e in season["episodes"].as_array().into_iter().flatten() { let en = e["number"].as_i64().unwrap_or(-1);
                if let Some(ep) = info["episodes"][sn.to_string()].as_array().and_then(|a| a.iter().find(|z| crate::util::i(&z["episode_num"]) == en)) { marks.push((crate::util::i(&ep["id"]), sn, en, ms(&e["last_watched_at"]))); }
            } }
        app.state.with(|st| for (eid, sn, en, t) in &marks { let k = format!("e:{eid}");
            if st["progress"].get(&k).is_none() { st["progress"][&k] = json!({ "key": k, "pos": 1, "dur": 1, "t": t, "item": { "k": "e", "id": eid, "sid": it.id, "title": it.title, "sub": format!("S{sn}:E{en}"), "poster": it.poster, "backdrop": it.backdrop } }); n_e += 1; } });
    }
    app.state.save(); app.memo.clear(); tokio::spawn(crate::prewarm_pub(app.clone()));
    app.cfg.set("traktLastSync", json!(now_ms()));
    *app.jobs.trakt.lock().unwrap() = json!({ "connected": true, "result": { "watched": n_m, "episodes": n_e, "ratings": n_r, "watchlist": n_l } });
    println!("[trakt] pulled {n_m} films, {n_e} episodes, {n_r} ratings, {n_l} watchlist");
}

/// Push one event to Trakt (no-op unless connected). Called from progress / rate / wanted / watched.
pub fn push(app: &Arc<App>, what: &'static str, body: Value) {
    if token(app).is_none() { return; }
    let a = app.clone();
    tokio::spawn(async move { let path = match what { "history" => "/sync/history", "unhistory" => "/sync/history/remove", "rate" => "/sync/ratings", "unrate" => "/sync/ratings/remove", "watchlist" => "/sync/watchlist", _ => "/sync/watchlist/remove" };
        if let Err(e) = call(&a, reqwest::Method::POST, path, Some(body)).await { println!("[trakt] push {what} failed: {e}"); } });
}
/// Finished something here: scrobble it to Trakt once.
pub fn finished(app: &Arc<App>, key: &str, item: &Value, tmdb: &Value) {
    if token(app).is_none() { return; }
    let done = app.state.read(|st| st["traktPushed"].get(key).is_some()); if done { return; }
    let tm = tmdb.as_str().and_then(|x| x.parse::<i64>().ok()).or_else(|| tmdb.as_i64());
    let body = if key.starts_with("m:") { tm.map(|t| json!({ "movies": [{ "ids": { "tmdb": t } }] })) } else {
        let sub = s(&item["sub"]); let c = crate::re!(r"S(\d+):E(\d+)").captures(&sub).map(|c| (c[1].parse::<i64>().unwrap_or(0), c[2].parse::<i64>().unwrap_or(0)));
        match (tm, c) { (Some(t), Some((sn, en))) => Some(json!({ "shows": [{ "ids": { "tmdb": t }, "seasons": [{ "number": sn, "episodes": [{ "number": en }] }] }] })), _ => None }
    };
    if let Some(b) = body { app.state.with(|st| { if st["traktPushed"].is_null() { st["traktPushed"] = json!({}); } st["traktPushed"][key] = json!(now_ms()); }); push(app, "history", b); }
}
pub fn ids_body(mt: &str, tmdb: &str) -> Value { let t: i64 = tmdb.parse().unwrap_or(0); if mt == "tv" { json!({ "shows": [{ "ids": { "tmdb": t } }] }) } else { json!({ "movies": [{ "ids": { "tmdb": t } }] }) } }
pub fn season_episodes_body(tmdb: &str, season: i64, episodes: &[i64]) -> Option<Value> {
    let t: i64 = tmdb.parse().ok()?;
    if t <= 0 || episodes.is_empty() { return None; }
    let eps: Vec<Value> = episodes.iter().map(|n| json!({ "number": n })).collect();
    Some(json!({ "shows": [{ "ids": { "tmdb": t }, "seasons": [{ "number": season, "episodes": eps }] }] }))
}
