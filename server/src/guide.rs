// TV guide grid, catch-up and "Tonight".
// - Duplicate channels (the same channel as HD / FHD / 4K / backup feeds) merge into one row with feeds.
// - Catch-up: channels with tv_archive play past programmes through the provider's timeshift URLs
//   (start time in the provider's own time zone).
use crate::app::App;
use crate::catalog::Item;
use crate::util::now_ms;
use axum::{extract::{Query, State}, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type A = State<Arc<App>>;
type Q = Query<HashMap<String, String>>;

/// (merge key, quality label) for a channel name: country prefix and quality/feed tags removed.
pub fn base_name(title: &str) -> (String, String) {
    let t = crate::re!(r"^\s*[A-Z0-9]{2,4}\s*[|:]\s*").replace(title, "").to_string();
    let up = t.to_uppercase();
    let is_4k = crate::re!(r"\b(4K|UHD|3840P?|2160P?)\b|ᵁᴴᴰ|⁴ᴷ").is_match(&up);
    let is_raw = crate::re!(r"\bRAW\b|ᴿᴬᵂ").is_match(&up);
    let fps = if crate::re!(r"60\s?FPS|⁶⁰ᶠᵖˢ").is_match(&up) { " 60fps" } else if crate::re!(r"50\s?FPS|⁵⁰ᶠᵖˢ").is_match(&up) { " 50fps" } else { "" };
    let label = if is_4k { format!("4K{fps}") }
        else if is_raw { format!("RAW{fps}") }
        else if crate::re!(r"\bFHD\b|ᶠᴴᴰ").is_match(&up) { format!("FHD{fps}") }
        else if crate::re!(r"\bHD\b|ᴴᴰ").is_match(&up) { format!("HD{fps}") }
        else if crate::re!(r"\bSD\b").is_match(&up) { "SD".into() }
        else { fps.trim().to_string() };
    let k = crate::re!(r"(?i)\b(4K|UHD|3840P?|2160P?|FHD|HD|SD|HEVC|H\.?265|RAW|\d{2}\s?FPS|VIP|BACKUP|ALT|DOLBY AUDIO)\b|ᵁᴴᴰ|⁴ᴷ|ᶠᴴᴰ|ᴴᴰ|ᴿᴬᵂ|ⱽᴵᴾ|◉|[()\[\]*•]").replace_all(&t, " ").to_string();
    (crate::re!(r"\s+").replace_all(k.trim(), " ").to_lowercase(), label)
}

fn live_label_rank(l: &str, title: &str) -> i32 {
    let up = format!("{l} {title}").to_uppercase();
    if up.contains("4K") || up.contains("UHD") || up.contains("3840") { 0 }
    else if up.contains("RAW") && (up.contains("60FPS") || up.contains("50FPS")) { 1 }
    else if up.contains("RAW") { 2 }
    else if up.contains("FHD") { 3 }
    else if up.contains("HD") { 4 }
    else if up.contains("SD") { 6 }
    else { 5 }
}

pub struct Row<'a> { pub key: String, pub title: String, pub feeds: Vec<(&'a Item, String)> }

/// Merge duplicate channels (same base name, or same EPG id + similar name). Order is kept.
pub fn merge<'a>(list: &[&'a Item]) -> Vec<Row<'a>> {
    let mut rows: Vec<Row> = vec![]; let mut idx: HashMap<String, usize> = HashMap::new();
    for ch in list {
        let (k, label) = base_name(&ch.title);
        let key = if k.is_empty() { ch.id.to_string() } else { k };
        let title = crate::re!(r"(?i)\s*\b(4K|UHD|FHD|HD|SD|HEVC|RAW)\b|ᵁᴴᴰ|ᶠᴴᴰ|ᴴᴰ|◉").replace_all(&crate::re!(r"^\s*[A-Z0-9]{2,4}\s*[|:]\s*").replace(&ch.title, ""), "").trim().to_string();
        if let Some(&i) = idx.get(&key) { rows[i].feeds.push((ch, label)); continue; }
        idx.insert(key.clone(), rows.len());
        rows.push(Row { key, title: if title.is_empty() { ch.title.clone() } else { title }, feeds: vec![(ch, label)] });
    }
    // primary feed: best picture first (4K > RAW 50/60fps > RAW > FHD > HD > SD), preferring EPG & catch-up
    for r in rows.iter_mut() {
        r.feeds.sort_by_key(|(c, l)| (c.epg.is_empty() as i32, live_label_rank(l, &c.title), (c.archive == 0) as i32));
        let mut n = 0; for f in r.feeds.iter_mut() { if f.1.is_empty() { n += 1; f.1 = if n == 1 { "Main".into() } else { format!("Feed {n}") }; } }
    }
    rows
}

fn channels_for(app: &App, cat: &str) -> Vec<Item> {
    let c = app.catalog();
    if cat == "favs" {
        let favs: Vec<i64> = app.state.read(|st| st["favs"].as_object().map(|m| m.keys().filter_map(|k| k.strip_prefix("l:").and_then(|x| x.parse().ok())).collect()).unwrap_or_default());
        favs.into_iter().filter_map(|id| c.live_item(id).cloned()).collect()
    } else if cat == "catchup" {
        // every English channel the provider offers catch-up on, guide data first
        let ep = app.epg.read().unwrap().clone();
        let mut v: Vec<Item> = c.live.iter().filter(|x| x.archive > 0 && x.en && ep.get(&x.epg).map_or(false, |l| !l.is_empty())).cloned().collect();
        v.sort_by_key(|x| x.title.clone()); v
    } else { c.live.iter().filter(|x| x.cat == cat).cloned().collect() }
}

pub async fn guide(State(app): A, Query(q): Q) -> Response {
    let cat = q.get("cat").cloned().unwrap_or_else(|| "favs".into());
    let from = q.get("from").and_then(|x| x.parse::<i64>().ok()).unwrap_or_else(|| now_ms() - 3_600_000);
    let hours = q.get("hours").and_then(|x| x.parse::<i64>().ok()).unwrap_or(6).clamp(1, 24);
    let to = from + hours * 3_600_000;
    let list = channels_for(&app, &cat); let refs: Vec<&Item> = list.iter().collect();
    let epg = app.epg.read().unwrap().clone();
    let (favs, progress, reminders): (std::collections::HashSet<String>, Value, std::collections::HashSet<String>) = app.state.read(|st| {
        let f = st["favs"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let r = st["reminders"].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str().map(String::from)).collect();
        (f, st["progress"].clone(), r)
    });
    let rows: Vec<Value> = merge(&refs).into_iter().map(|r| {
        let main = r.feeds[0].0;
        let progs: Vec<Value> = r.feeds.iter().find_map(|(c, _)| epg.get(&c.epg)).map(|l| l.iter().filter(|p| p.e > from && p.s < to).map(|p| {
            let cu_key = format!("cu:{}:{}", main.id, p.s);
            let pr = progress.get(&cu_key).or_else(|| r.feeds.iter().find_map(|(c, _)| progress.get(&format!("cu:{}:{}", c.id, p.s))));
            let (prog_pct, resume) = pr.map(|x| {
                let d = x["dur"].as_f64().unwrap_or(0.0); let pos = x["pos"].as_f64().unwrap_or(0.0);
                (if d > 0.0 { pos / d } else { 0.0 }, pos)
            }).unwrap_or((0.0, 0.0));
            let rem_id = format!("rem:{}:{}", main.id, p.s);
            json!({ "s": p.s, "e": p.e, "title": p.title, "desc": p.desc, "progress": prog_pct, "resume": resume, "reminded": reminders.contains(&rem_id) })
        }).collect()).unwrap_or_default();
        let archive = r.feeds.iter().map(|(c, _)| c.archive).max().unwrap_or(0);
        json!({ "key": r.key, "title": r.title, "logo": r.feeds.iter().map(|f| f.0.poster.clone()).find(|p| !p.is_empty()).unwrap_or_default(), "id": main.id, "epg": main.epg, "archive": archive,
            "fav": r.feeds.iter().any(|(c, _)| favs.contains(&format!("l:{}", c.id))),
            "feeds": r.feeds.iter().map(|(c, l)| json!({ "id": c.id, "label": l, "archive": c.archive, "channel": c.title })).collect::<Vec<_>>(), "progs": progs })
    }).collect();
    Json(json!({ "from": from, "to": to, "now": now_ms(), "rows": rows })).into_response()
}

/// Alternate feeds for a single live channel (same base name in its category, or across English/sport channels).
pub fn feeds_for(app: &App, ch: &Item) -> Vec<Value> {
    let cat = app.catalog();
    let (key, _) = base_name(&ch.title);
    if key.is_empty() { return vec![]; }
    let mut matches: Vec<(&Item, String)> = cat.live.iter()
        .filter(|x| (x.cat == ch.cat || x.en || x.sport) && base_name(&x.title).0 == key)
        .map(|x| { let (_, l) = base_name(&x.title); (x, l) }).collect();
    if matches.len() <= 1 { return vec![]; }
    matches.sort_by_key(|(c, l)| ((c.id != ch.id) as i32, live_label_rank(l, &c.title), c.cat != ch.cat));
    let mut seen = std::collections::HashSet::new();
    matches.retain(|(c, _)| seen.insert(c.id));
    matches.truncate(8);
    matches.into_iter().map(|(c, l)| json!({ "id": c.id, "label": if l.is_empty() { c.title.clone() } else { format!("{l} · {}", c.title) }, "channel": c.title, "archive": c.archive })).collect()
}

/// Provider timeshift URL for a past programme. `start_ms` UTC; `minutes` length.
pub fn timeshift_url(app: &App, id: i64, start_ms: i64, minutes: i64) -> String {
    let tz: chrono_tz::Tz = app.cfg.str("serverTimezone").parse().unwrap_or(chrono_tz::Europe::Amsterdam);
    use chrono::TimeZone;
    let t = chrono::Utc.timestamp_millis_opt(start_ms).single().unwrap_or_default().with_timezone(&tz);
    let live = app.xc.stream_url("live", &id.to_string(), "ts", false);
    // .../live/u/p/id.ts -> .../timeshift/u/p/minutes/YYYY-MM-DD:HH-MM/id.ts
    let base = live.rsplit_once("/live/").map(|x| x.0.to_string()).unwrap_or_default();
    let creds = live.rsplit_once("/live/").map(|x| x.1.rsplitn(2, '/').nth(1).unwrap_or("").to_string()).unwrap_or_default();
    format!("{base}/timeshift/{creds}/{}/{}/{id}.ts", minutes.max(1), t.format("%Y-%m-%d:%H-%M"))
}

/// Tonight: new episodes of followed shows + what's on the favourite channels this evening.
pub async fn tonight(State(app): A) -> Response {
    let now = now_ms();
    let cat = app.catalog();
    let new_eps: Vec<Value> = app.state.read(|st| st["newEps"].as_object().map(|m| m.iter().map(|(sid, v)| (sid.clone(), v.clone())).collect::<Vec<_>>()).unwrap_or_default())
        .into_iter().filter_map(|(sid, v)| { let it = cat.series_item(sid.parse().ok()?)?; let mut c = app.card_meta(it); c["newEp"] = v["latest"].clone(); c["newCount"] = json!(v["ids"].as_array().map(|a| a.len()).unwrap_or(0)); Some(c) }).collect();
    // evening window in local time: now .. 01:00 tomorrow
    let local = chrono::Local::now();
    let end = (local.date_naive() + chrono::Duration::days(1)).and_hms_opt(1, 0, 0).unwrap();
    let end_ms = end.and_local_timezone(chrono::Local).single().map(|t| t.timestamp_millis()).unwrap_or(now + 6 * 3_600_000);
    let mut list = channels_for(&app, "favs"); let mine = !list.is_empty();
    if !mine { list = default_channels(&app); }
    let refs: Vec<&Item> = list.iter().collect();
    let epg = app.epg.read().unwrap().clone();
    let favs: Vec<Value> = merge(&refs).into_iter().filter_map(|r| {
        let progs: Vec<Value> = r.feeds.iter().find_map(|(c, _)| epg.get(&c.epg))?.iter().filter(|p| p.e > now && p.s < end_ms).take(8).map(|p| json!({ "s": p.s, "e": p.e, "title": p.title, "desc": p.desc })).collect();
        if progs.is_empty() { return None; }
        Some(json!({ "title": r.title, "id": r.feeds[0].0.id, "logo": r.feeds[0].0.poster, "epg": r.feeds[0].0.epg, "progs": progs }))
    }).collect();
    let city = { let c = app.cfg.str("city"); if c.is_empty() { "Sydney".to_string() } else { c } };
    Json(json!({ "now": now, "until": end_ms, "newEps": new_eps, "favs": favs, "mine": mine, "city": city })).into_response()
}

/// No favourites yet: the main local channels (free-to-air for the viewer's city first, then Foxtel sport).
fn default_channels(app: &App) -> Vec<Item> {
    let city = { let c = app.cfg.str("city"); if c.is_empty() { "sydney".to_string() } else { c.to_lowercase() } };
    let cat = app.catalog(); let epg = app.epg.read().unwrap().clone();
    let order = ["7", "9", "10", "abc", "sbs", "7two", "7mate", "9go", "9gem", "10 bold", "10 peach", "abc news", "sbs viceland", "fox footy", "fox league", "fox cricket", "espn"];
    let mut best: Vec<(usize, i32, &Item)> = vec![];
    for ch in cat.live.iter() {
        if !crate::re!(r"^AU\s*\|").is_match(&cat.cat_name("live", &ch.cat)) || !epg.get(&ch.epg).map_or(false, |l| !l.is_empty()) { continue; }
        let n = crate::epgx::norm(&ch.title); let local = n.contains(&city) || n.contains("national");
        let base = n.replace(&city, "").replace("national", "").trim().to_string();
        let base = match base.as_str() { "fox sports 504" => "fox footy".to_string(), "fox sports 502" => "fox league".to_string(), "fox sports 501" => "fox cricket".to_string(), "espn 1" => "espn".to_string(), _ => base };
        let Some(rank) = order.iter().position(|o| base == *o) else { continue };
        best.push((rank, if local { 0 } else { 1 }, ch));
    }
    best.sort_by_key(|x| (x.0, x.1));
    let mut seen = std::collections::HashSet::new();
    best.into_iter().filter(|x| seen.insert(x.0)).take(12).map(|x| x.2.clone()).collect()
}
