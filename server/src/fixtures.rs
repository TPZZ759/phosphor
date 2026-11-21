// Real fixtures + live scores from ESPN's public scoreboards, matched to channels.
use crate::app::App;
use crate::sports::Event;
use crate::util::{norm, now_ms, s};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct League { pub id: &'static str, pub path: &'static str, pub name: &'static str, pub sport: Option<&'static str>, pub solo: bool }
pub const LEAGUES: &[League] = &[
    League { id: "nfl", path: "football/nfl", name: "NFL", sport: None, solo: false },
    League { id: "nba", path: "basketball/nba", name: "NBA", sport: None, solo: false },
    League { id: "nhl", path: "hockey/nhl", name: "NHL", sport: None, solo: false },
    League { id: "mlb", path: "baseball/mlb", name: "MLB", sport: None, solo: false },
    League { id: "epl", path: "soccer/eng.1", name: "Premier League", sport: Some("Soccer"), solo: false },
    League { id: "ucl", path: "soccer/uefa.champions", name: "Champions League", sport: Some("Soccer"), solo: false },
    League { id: "laliga", path: "soccer/esp.1", name: "La Liga", sport: Some("Soccer"), solo: false },
    League { id: "bund", path: "soccer/ger.1", name: "Bundesliga", sport: Some("Soccer"), solo: false },
    League { id: "seriea", path: "soccer/ita.1", name: "Serie A", sport: Some("Soccer"), solo: false },
    League { id: "mls", path: "soccer/usa.1", name: "MLS", sport: Some("Soccer"), solo: false },
    League { id: "aleague", path: "soccer/aus.1", name: "A-League", sport: Some("Soccer"), solo: false },
    League { id: "afl", path: "australian-football/afl", name: "AFL", sport: None, solo: false },
    League { id: "nrl", path: "rugby-league/3", name: "NRL", sport: None, solo: false },
    League { id: "ncaaf", path: "football/college-football", name: "College Football", sport: Some("NCAAF"), solo: false },
    League { id: "wnba", path: "basketball/wnba", name: "WNBA", sport: None, solo: false },
    League { id: "ufc", path: "mma/ufc", name: "UFC", sport: None, solo: true },
    League { id: "f1", path: "racing/f1", name: "F1", sport: None, solo: true },
];
const BASE: &str = "https://site.api.espn.com/apis/site/v2/sports/";

#[derive(Default)]
pub struct Fixtures { cache: Mutex<HashMap<String, (i64, Arc<Vec<Value>>)>>, teams: Mutex<HashMap<String, (i64, Arc<Vec<Value>>)>> }

fn et_date(ts: i64) -> String { chrono::DateTime::from_timestamp_millis(ts).unwrap().with_timezone(&chrono_tz::America::New_York).format("%Y%m%d").to_string() }

fn normalize(lg: &'static League, e: &Value) -> Option<Value> {
    let comp = e["competitions"].get(0)?; let st = &e["status"]["type"];
    let team = |c: &Value| json!({ "id": c["team"]["id"].as_str().or(c["athlete"]["id"].as_str()).or(c["id"].as_str()).unwrap_or(""),
        "name": c["team"]["displayName"].as_str().or(c["athlete"]["displayName"].as_str()).unwrap_or(""),
        "short": c["team"]["shortDisplayName"].as_str().or(c["athlete"]["shortName"].as_str()).unwrap_or(""), "abbr": s(&c["team"]["abbreviation"]),
        "location": s(&c["team"]["location"]), "nick": s(&c["team"]["name"]), "logo": c["team"]["logo"].as_str().or(c["athlete"]["flag"]["href"].as_str()).unwrap_or(""),
        "score": c["score"].as_str().unwrap_or(""), "winner": c["winner"].as_bool().unwrap_or(false) });
    let comps = comp["competitors"].as_array()?;
    let home = comps.iter().find(|c| c["homeAway"] == "home").map(team).unwrap_or(json!({}));
    let away = comps.iter().find(|c| c["homeAway"] == "away").map(team).unwrap_or(json!({}));
    let pick = |t: &Value| { let a = s(&t["short"]); if a.is_empty() { s(&t["name"]) } else { a } };
    let ds = s(&e["date"]);
    let start = chrono::NaiveDateTime::parse_from_str(&ds, "%Y-%m-%dT%H:%MZ").or_else(|_| chrono::NaiveDateTime::parse_from_str(&ds, "%Y-%m-%dT%H:%M:%SZ")).map(|d| d.and_utc().timestamp_millis()).unwrap_or(0);
    Some(json!({ "id": format!("{}:{}", lg.id, s(&e["id"])), "league": lg.id, "leagueName": lg.name, "sport": lg.sport.unwrap_or(lg.name), "solo": lg.solo,
        "title": if lg.solo { s(&e["name"]) } else { format!("{} vs {}", pick(&away), pick(&home)) }, "name": e["name"], "start": start,
        "state": st["state"], "completed": st["completed"].as_bool().unwrap_or(false), "detail": st["shortDetail"].as_str().or(st["detail"].as_str()).unwrap_or(""),
        "home": if lg.solo { Value::Null } else { home }, "away": if lg.solo { Value::Null } else { away },
        "broadcasts": comp["broadcasts"].as_array().into_iter().flatten().flat_map(|b| b["names"].as_array().cloned().unwrap_or_default()).collect::<Vec<_>>(),
        "venue": s(&comp["venue"]["fullName"]) }))
}

async fn fetch_day(app: &Arc<App>, lg: &'static League, date: &str, fresh: bool) -> Arc<Vec<Value>> {
    let key = format!("{}|{date}", lg.id);
    let hit = app.fixtures.cache.lock().unwrap().get(&key).cloned();
    let ttl = if fresh { 60_000 } else { 30 * 60_000 };
    if let Some((t, v)) = &hit { if now_ms() - t < ttl { return v.clone(); } }
    let url = format!("{BASE}{}/scoreboard?dates={date}&limit=200", lg.path);
    match app.http.get(&url).timeout(Duration::from_secs(12)).send().await.ok().and_then(|r| r.status().is_success().then_some(r)) {
        Some(r) => match r.json::<Value>().await {
            Ok(j) => { let v = Arc::new(j["events"].as_array().into_iter().flatten().filter_map(|e| normalize(lg, e)).collect::<Vec<_>>()); app.fixtures.cache.lock().unwrap().insert(key, (now_ms(), v.clone())); v }
            Err(_) => hit.map(|h| h.1).unwrap_or_default(),
        },
        None => hit.map(|h| h.1).unwrap_or_default(),
    }
}

pub async fn all(app: &Arc<App>) -> Vec<Value> {
    let now = now_ms(); let today = et_date(now); let yday = et_date(now - 86_400_000);
    let mut jobs: Vec<(usize, String)> = vec![];
    for li in 0..LEAGUES.len() { for k in -1..=6i64 { jobs.push((li, et_date(now + k * 86_400_000))); } }
    let futs: Vec<_> = jobs.into_iter().map(|(li, d)| { let app = app.clone(); let fresh = d == today || d == yday; tokio::spawn(async move { fetch_day(&app, &LEAGUES[li], &d, fresh).await }) }).collect();
    let res: Vec<Arc<Vec<Value>>> = futures_util::future::join_all(futs).await.into_iter().map(|r| r.unwrap_or_default()).collect();
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let state_rank = |s: &str| match s { "in" => 2, "post" => 1, _ => 0 };
    for f in res.into_iter().flat_map(|v| (*v).clone()).filter(|f| f["state"] != "post" || now - f["start"].as_i64().unwrap_or(0) < 12 * 3_600_000) {
        let id = s(&f["id"]);
        let replace = by_id.get(&id).map_or(true, |prev| state_rank(f["state"].as_str().unwrap_or("")) > state_rank(prev["state"].as_str().unwrap_or("")));
        if replace { by_id.insert(id, f); }
    }
    let mut out: Vec<Value> = by_id.into_values().collect();
    out.sort_by_key(|f| f["start"].as_i64().unwrap_or(0));
    out
}
use futures_util::StreamExt;

fn team_tokens(t: &Value) -> Vec<String> {
    let stop: HashSet<&str> = ["fc", "cf", "sc", "afc", "the", "city", "united", "state", "university", "de", "club"].into_iter().collect();
    let mut out: Vec<String> = vec![];
    for k in ["short", "name", "location", "nick"] { let n = norm(&s(&t[k])); if n.len() >= 3 && !stop.contains(n.as_str()) && !out.contains(&n) { out.push(n); } }
    let a = s(&t["abbr"]); if a.len() >= 3 { let n = norm(&a); if !out.contains(&n) { out.push(n); } }
    out
}
fn has_tok(hay: &str, toks: &[String]) -> bool { let h = format!(" {hay} "); toks.iter().any(|t| h.contains(&format!(" {t} "))) }

fn network(app: &App, name: &str) -> Vec<(i64, String)> {
    let n = crate::re!(r"\bnet\b").replace(&norm(name), "network").to_string();
    if n.len() < 2 || crate::re!(r"local|app|\+|plus|stream|radio").is_match(&n) { return vec![]; }
    let cat = app.catalog();
    let ids = { app.net_idx.lock().unwrap().get(&n).cloned() };
    let ids = ids.unwrap_or_else(|| {
        let re = regex::Regex::new(&format!(r"(?i)^(us|uk|ca|au)?\s*{}\s*(hd|fhd|east)?$", regex::escape(&n).replace(' ', r"\s*"))).unwrap();
        let hits: Vec<i64> = cat.live.iter().filter(|ch| ch.en && re.is_match(&norm(&crate::re!(r"^[A-Z]{2}\s*[:|]\s*").replace(&ch.title, "")))).take(2).map(|c| c.id).collect();
        app.net_idx.lock().unwrap().insert(n.clone(), hits.clone()); hits
    });
    ids.into_iter().filter_map(|id| cat.live_item(id).map(|c| (id, c.title.clone()))).collect()
}

fn feeds(app: &App, f: &Value, evs: &[Event]) -> Vec<Value> {
    let start = f["start"].as_i64().unwrap_or(0); let post = f["state"] == "post";
    let mut list: Vec<(i64, String, bool, bool)> = vec![]; // id, channel, en, network
    if f["solo"].as_bool().unwrap_or(false) {
        let lid = s(&f["league"]);
        for ev in evs {
            if !((lid == "ufc" && ev.league == "UFC") || (lid == "f1" && ev.league == "F1")) { continue; }
            if ev.start.map_or(false, |s| (s - start).abs() > 8 * 3_600_000) || (ev.ended && !post) { continue; }
            let t = norm(&ev.title);
            if lid == "ufc" && !crate::re!(r"ufc|fight night").is_match(&t) { continue; }
            if lid == "f1" && !crate::re!(r"prix|gp|f1|formula|qualifying|race|practice").is_match(&t) { continue; }
            list.push((ev.id, ev.channel.clone(), ev.en, false));
        }
    } else {
        let ht = team_tokens(&f["home"]); let at = team_tokens(&f["away"]);
        for ev in evs {
            if ev.start.map_or(false, |s| (s - start).abs() > 6 * 3_600_000) || (ev.ended && !post) { continue; }
            let t = norm(&format!("{} {}", ev.title, ev.channel));
            if has_tok(&t, &ht) && has_tok(&t, &at) { list.push((ev.id, ev.channel.clone(), ev.en, false)); }
        }
    }
    for b in f["broadcasts"].as_array().into_iter().flatten() { for (id, title) in network(app, &s(b)) { list.push((id, title, true, true)); } }
    let pref4k = app.state.read(|st| st["prefs"]["prefer4k"].as_bool().unwrap_or(true));
    let rank = |x: &(i64, String, bool, bool)| -> i32 {
        let up = x.1.to_uppercase();
        let health_pen = (crate::source_health(app, x.0) * 2.0).round() as i32;
        let q = if up.contains("4K") || up.contains("UHD") || up.contains("3840") { if pref4k { -3 } else { 1 } }
            else if up.contains("RAW") && (up.contains("60FPS") || up.contains("50FPS")) { -2 }
            else if up.contains("RAW") || up.contains("FHD") { -1 }
            else if up.contains("SD") { 2 }
            else { 0 };
        health_pen + q + (if x.2 { 0 } else { 6 }) + (if x.3 { 1 } else { 0 })
            + if crate::re!(r"(?i)multi ?view").is_match(&x.1) { 4 } else { 0 }
            + if crate::re!(r"(?i)espa|french|\bfr\b|deportes").is_match(&x.1) { 4 } else { 0 }
    };
    let mut seen = HashSet::new(); list.retain(|x| seen.insert(x.0));
    list.sort_by_key(|x| rank(x));
    list.into_iter().take(8).map(|x| json!({ "id": x.0, "channel": x.1, "en": x.2, "kind": if x.3 { "network" } else { "event" } })).collect()
}

pub async fn with_feeds(app: &Arc<App>) -> Vec<Value> {
    let list = all(app).await;
    let evs = crate::sports::events(app, true);
    let favs: HashSet<String> = app.state.read(|st| st["teams"].as_array().into_iter().flatten().map(|t| format!("{}:{}", s(&t["league"]), s(&t["id"]))).collect());
    list.into_iter().map(|mut f| {
        f["feeds"] = Value::Array(feeds(app, &f, &evs));
        let lg = s(&f["league"]);
        f["fav"] = json!(!f["home"].is_null() && (favs.contains(&format!("{lg}:{}", s(&f["home"]["id"]))) || favs.contains(&format!("{lg}:{}", s(&f["away"]["id"])))));
        f
    }).collect()
}

pub async fn teams(app: &Arc<App>, league: &str) -> Arc<Vec<Value>> {
    let Some(lg) = LEAGUES.iter().find(|l| l.id == league && !l.solo) else { return Arc::new(vec![]) };
    let hit = app.fixtures.teams.lock().unwrap().get(league).cloned();
    if let Some((t, v)) = &hit { if now_ms() - t < 86_400_000 { return v.clone(); } }
    let url = format!("{BASE}{}/teams?limit=500", lg.path);
    let Ok(j) = (async { app.http.get(&url).timeout(Duration::from_secs(12)).send().await?.json::<Value>().await }).await else { return hit.map(|h| h.1).unwrap_or_default() };
    let mut v: Vec<Value> = j["sports"][0]["leagues"][0]["teams"].as_array().into_iter().flatten().map(|x| &x["team"])
        .map(|t| json!({ "league": league, "id": s(&t["id"]), "name": t["displayName"], "short": t["shortDisplayName"], "logo": t["logos"][0]["href"].as_str().unwrap_or("") })).collect();
    v.sort_by(|a, b| s(&a["name"]).cmp(&s(&b["name"])));
    let v = Arc::new(v); app.fixtures.teams.lock().unwrap().insert(league.into(), (now_ms(), v.clone())); v
}

/// Kick-off alerts for favourite teams (10 minutes before).
pub async fn alerts(app: &Arc<App>) {
    let has = app.state.read(|st| st["teams"].as_array().map_or(false, |a| !a.is_empty()));
    if !has { return; }
    let list = with_feeds(app).await; let now = now_ms();
    for f in list {
        if !f["fav"].as_bool().unwrap_or(false) || f["state"] != "pre" { continue; }
        let id = s(&f["id"]);
        if app.state.read(|st| st["notified"].get(&id).is_some()) { continue; }
        let mins = (f["start"].as_i64().unwrap_or(0) - now) as f64 / 60_000.0;
        if mins <= 10.0 && mins > -5.0 {
            app.state.with(|st| st["notified"][&id] = json!(now)); app.state.save();
            let feed = f["feeds"].get(0);
            let feed_name = feed.map(|x| s(&x["channel"])).unwrap_or_else(|| "no stream found yet".into());
            let action = match feed.and_then(|x| x["id"].as_i64()) {
                Some(fid) => json!({ "k": "l", "id": fid, "title": s(&f["title"]), "feeds": f["feeds"], "label": "Watch now" }),
                None => json!({ "route": "#/sports", "label": "Sports" }),
            };
            crate::notify_all(app, "kickoff", &format!("{} starts {}", s(&f["title"]), if mins > 1.0 { format!("in {} min", mins.round()) } else { "now".into() }),
                &format!("{} · {feed_name}", s(&f["leagueName"])), "http://localhost:7878/#/sports", action);
        }
    }
    app.state.with(|st| if let Some(m) = st["notified"].as_object_mut() { m.retain(|_, t| now - t.as_i64().unwrap_or(0) < 3 * 86_400_000); });
}
