// TV guide grid, catch-up and "Tonight".
// - Duplicate channels (the same channel as HD / FHD / 4K / backup feeds) merge into one row with feeds,
//   keyed by (country_tag, base_name) so US | ESPN and AU | ESPN never collapse into one row.
// - Catch-up: channels with tv_archive play past programmes through the provider's timeshift URLs
//   (start time in the provider's own time zone, with safe ±3-minute padding and URL-encoded credentials).
use crate::app::App;
use crate::catalog::Item;
use crate::util::now_ms;
use axum::{extract::{Query, State}, response::{IntoResponse, Response}, Json};
use chrono::Timelike;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type A = State<Arc<App>>;
type Q = Query<HashMap<String, String>>;

/// Extract normalized 2-letter country prefix from a live channel title (e.g. `US | ESPN` -> `"US"`, `AU | ESPN` -> `"AU"`).
/// Non-country quality/brand prefixes (`4K |`, `VIP |`, `PRIME |`, `RAW |`, `FHD |`, `HD |`) return `""`.
pub fn country_tag(title: &str) -> String {
    if let Some(c) = crate::re!(r"(?i)^\s*(US|UK|GB|CA|AU|NZ|IE|DE|FR|ES|IT|NL|BE|PT|SE|NO|DK|FI|PL|CH|AT|IN|ZA|AE|AR|BR|MX|JP|KR|GR|TR|RO|CZ|HU|HR|RS|BG|UA|IL)(?:\s+(?:EN|FR|4K|HD|UHD))?\s*[|:]").captures(title) {
        let code = c[1].to_uppercase();
        if code == "GB" { "UK".to_string() } else { code }
    } else {
        String::new()
    }
}

/// (merge key, quality label) for a channel name: country/network prefix and quality/feed tags removed.
pub fn base_name(title: &str) -> (String, String) {
    let t = crate::re!(r"^\s*(?:AT&T|PRIME|CA\s+EN|CA\s+FR|AR\s+4K|[A-Z0-9]{2,4})\s*[|:]\s*").replace(title, "").to_string();
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

pub struct Row<'a> {
    pub key: String,
    pub country: String,
    pub title: String,
    pub feeds: Vec<(&'a Item, String)>,
}

/// Merge duplicate channels keyed by `(country_tag, base_name)` (plus matching EPG ID when countries
/// are compatible). Channels from different countries (e.g. `US | ESPN` vs `AU | ESPN`) NEVER collapse.
pub fn merge<'a>(list: &[&'a Item]) -> Vec<Row<'a>> {
    let mut rows: Vec<Row> = vec![];
    let mut idx: HashMap<(String, String), usize> = HashMap::new();
    let mut epg_idx: HashMap<(String, String), usize> = HashMap::new(); // (epg, base) -> row idx

    for ch in list {
        let cc = country_tag(&ch.title);
        let (k, label) = base_name(&ch.title);
        let base = if k.is_empty() { ch.id.to_string() } else { k };
        let title = crate::re!(r"(?i)\s*\b(4K|UHD|FHD|HD|SD|HEVC|RAW)\b|ᵁᴴᴰ|ᶠᴴᴰ|ᴴᴰ|◉")
            .replace_all(&crate::re!(r"^\s*(?:AT&T|PRIME|CA\s+EN|CA\s+FR|AR\s+4K|[A-Z0-9]{2,4})\s*[|:]\s*").replace(&ch.title, ""), "")
            .trim()
            .to_string();

        // 1. Exact (country_tag, base_name) match
        if let Some(&i) = idx.get(&(cc.clone(), base.clone())) {
            rows[i].feeds.push((ch, label));
            if !ch.epg.is_empty() {
                epg_idx.entry((ch.epg.clone(), base.clone())).or_insert(i);
            }
            continue;
        }
        // 2. Same non-empty EPG ID + same base_name when country tags do not conflict (one is untagged)
        if !ch.epg.is_empty() {
            if let Some(&i) = epg_idx.get(&(ch.epg.clone(), base.clone())) {
                if rows[i].country.is_empty() || cc.is_empty() || rows[i].country == cc {
                    if rows[i].country.is_empty() && !cc.is_empty() {
                        rows[i].country = cc.clone();
                        idx.insert((cc.clone(), base.clone()), i);
                    }
                    rows[i].feeds.push((ch, label));
                    continue;
                }
            }
        }

        let row_i = rows.len();
        let key = if cc.is_empty() { base.clone() } else { format!("{}:{base}", cc.to_lowercase()) };
        idx.insert((cc.clone(), base.clone()), row_i);
        if !ch.epg.is_empty() {
            epg_idx.entry((ch.epg.clone(), base.clone())).or_insert(row_i);
        }
        rows.push(Row {
            key,
            country: cc,
            title: if title.is_empty() { ch.title.clone() } else { title },
            feeds: vec![(ch, label)],
        });
    }

    // Disambiguate display titles when multiple countries share the same channel name in one view
    let mut title_countries: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for r in &rows {
        if !r.country.is_empty() {
            title_countries.entry(r.title.to_lowercase()).or_default().insert(r.country.clone());
        }
    }
    for r in rows.iter_mut() {
        if !r.country.is_empty() {
            if title_countries.get(&r.title.to_lowercase()).map_or(false, |s| s.len() > 1) {
                r.title = format!("{} ({})", r.title, r.country);
            }
        }
        // primary feed: best picture first (4K > RAW 50/60fps > RAW > FHD > HD > SD); EPG is shared across all feeds in the row
        r.feeds.sort_by_key(|(c, l)| (live_label_rank(l, &c.title), c.epg.is_empty() as i32, (c.archive == 0) as i32));
        let mut n = 0;
        for f in r.feeds.iter_mut() {
            if f.1.is_empty() {
                n += 1;
                f.1 = if n == 1 { "Main".into() } else { format!("Feed {n}") };
            }
        }
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
        let archive_feed = r.feeds.iter()
            .filter(|(c, _)| c.archive > 0)
            .min_by_key(|(c, l)| (live_label_rank(l, &c.title), -c.archive))
            .map(|(c, _)| c.id);
        let progs: Vec<Value> = r.feeds.iter().find_map(|(c, _)| epg.get(&c.epg)).map(|l| l.iter().filter(|p| p.e > from && p.s < to).map(|p| {
            let cu_key = format!("cu:{}:{}", archive_feed.unwrap_or(main.id), p.s);
            let pr = progress.get(&cu_key).or_else(|| r.feeds.iter().find_map(|(c, _)| progress.get(&format!("cu:{}:{}", c.id, p.s))));
            let (prog_pct, resume) = pr.map(|x| {
                let d = x["dur"].as_f64().unwrap_or(0.0); let pos = x["pos"].as_f64().unwrap_or(0.0);
                (if d > 0.0 { pos / d } else { 0.0 }, pos)
            }).unwrap_or((0.0, 0.0));
            let rem_id = format!("rem:{}:{}", main.id, p.s);
            json!({ "s": p.s, "e": p.e, "title": p.title, "desc": p.desc, "progress": prog_pct, "resume": resume, "reminded": reminders.contains(&rem_id) })
        }).collect()).unwrap_or_default();
        let archive = r.feeds.iter().map(|(c, _)| c.archive).max().unwrap_or(0);
        let epg_id = r.feeds.iter().find(|(c, _)| !c.epg.is_empty()).map(|(c, _)| c.epg.as_str()).unwrap_or("");
        json!({
            "key": r.key, "country": r.country, "title": r.title,
            "logo": r.feeds.iter().map(|f| f.0.poster.clone()).find(|p| !p.is_empty()).unwrap_or_default(),
            "id": main.id, "epg": epg_id, "archive": archive, "archiveFeedId": archive_feed,
            "fav": r.feeds.iter().any(|(c, _)| favs.contains(&format!("l:{}", c.id))),
            "feeds": r.feeds.iter().map(|(c, l)| json!({ "id": c.id, "label": l, "archive": c.archive, "channel": c.title })).collect::<Vec<_>>(),
            "progs": progs
        })
    }).collect();
    Json(json!({ "from": from, "to": to, "now": now_ms(), "rows": rows })).into_response()
}

/// Alternate feeds for a single live channel (same base name & compatible country in its category or English/sport).
pub fn feeds_for(app: &App, ch: &Item) -> Vec<Value> {
    let cat = app.catalog();
    let (key, _) = base_name(&ch.title);
    if key.is_empty() { return vec![]; }
    let cc = country_tag(&ch.title);
    let mut matches: Vec<(&Item, String)> = cat.live.iter()
        .filter(|x| {
            if !(x.cat == ch.cat || x.en || x.sport) { return false; }
            if base_name(&x.title).0 != key { return false; }
            let xcc = country_tag(&x.title);
            cc.is_empty() || xcc.is_empty() || xcc == cc
        })
        .map(|x| { let (_, l) = base_name(&x.title); (x, l) })
        .collect();
    if matches.len() <= 1 { return vec![]; }
    matches.sort_by_key(|(c, l)| ((c.id != ch.id) as i32, live_label_rank(l, &c.title), c.cat != ch.cat));
    let mut seen = std::collections::HashSet::new();
    matches.retain(|(c, _)| seen.insert(c.id));
    matches.truncate(8);
    matches.into_iter().map(|(c, l)| json!({ "id": c.id, "label": if l.is_empty() { c.title.clone() } else { format!("{l} · {}", c.title) }, "channel": c.title, "archive": c.archive })).collect()
}

/// Compute safely clamped ±3-minute padded timeshift `(start_ms, minutes)`.
pub(crate) fn pad_timeshift_window(start_ms: i64, minutes: i64, archive_days: i64, now: i64) -> (i64, i64) {
    let pad_before: i64 = 180 * 1000; // 3 minutes before
    let pad_after: i64 = 180 * 1000;  // 3 minutes after
    let days = archive_days.max(1);
    let earliest = (now - days * 86_400_000).min(start_ms);
    let padded_start = (start_ms - pad_before).max(earliest).max(0);
    let raw_end = start_ms + minutes.max(1) * 60_000;
    let padded_end = if raw_end <= now {
        (raw_end + pad_after).min(now)
    } else if now > padded_start + 60_000 {
        now
    } else {
        raw_end
    };
    let total_mins = if padded_end > padded_start {
        ((padded_end - padded_start + 59_999) / 60_000).max(1)
    } else {
        minutes.max(1)
    };
    (padded_start, total_mins)
}

/// Provider timeshift URL for a past programme. `start_ms` UTC; `minutes` length.
/// Automatically resolves an archive-capable sibling feed when `id` is a 4K-primary feed without archive,
/// applies safe ±3-minute padding, and URL-encodes credentials cleanly.
pub fn timeshift_url(app: &App, id: i64, start_ms: i64, minutes: i64) -> String {
    let cat = app.catalog();
    let requested = cat.live_item(id);
    let effective = match requested {
        Some(ch) if ch.archive > 0 => Some(ch),
        Some(ch) => {
            let (base, _) = base_name(&ch.title);
            let cc = country_tag(&ch.title);
            cat.live.iter()
                .filter(|x| x.archive > 0 && (
                    (!ch.epg.is_empty() && x.epg == ch.epg)
                    || (!base.is_empty() && base_name(&x.title).0 == base && {
                        let xcc = country_tag(&x.title);
                        cc.is_empty() || xcc.is_empty() || xcc == cc
                    })
                ))
                .min_by_key(|x| {
                    let (_, l) = base_name(&x.title);
                    (live_label_rank(&l, &x.title), -x.archive)
                })
                .or(Some(ch))
        }
        None => None,
    };
    let eff_id = effective.map(|c| c.id).unwrap_or(id);
    let archive_days = effective.map(|c| c.archive).filter(|&a| a > 0).unwrap_or(7);
    let (padded_start, total_mins) = pad_timeshift_window(start_ms, minutes, archive_days, now_ms());

    let tz: chrono_tz::Tz = app.cfg.str("serverTimezone").parse().unwrap_or(chrono_tz::Europe::Amsterdam);
    use chrono::TimeZone;
    let t = chrono::Utc.timestamp_millis_opt(padded_start).single().unwrap_or_default().with_timezone(&tz);
    let base = app.xc.base();
    let u = crate::util::enc(&app.cfg.str("username"));
    let p = crate::util::enc(&app.cfg.str("password"));
    format!("{base}/timeshift/{u}/{p}/{total_mins}/{}/{eff_id}.ts", t.format("%Y-%m-%d:%H-%M"))
}

pub(crate) fn tonight_end_ms<Tz: chrono::TimeZone>(local: chrono::DateTime<Tz>) -> i64 {
    let target_date = if local.hour() < 1 {
        local.date_naive()
    } else {
        local.date_naive() + chrono::Duration::days(1)
    };
    let end = target_date.and_hms_opt(1, 0, 0).unwrap();
    local.timezone()
        .from_local_datetime(&end)
        .earliest()
        .map(|t| t.timestamp_millis())
        .unwrap_or_else(|| local.timestamp_millis() + 6 * 3_600_000)
}

/// Tonight: new episodes of followed shows + what's on the favourite channels this evening.
pub async fn tonight(State(app): A) -> Response {
    let now = now_ms();
    let cat = app.catalog();
    let new_eps: Vec<Value> = app.state.read(|st| st["newEps"].as_object().map(|m| m.iter().map(|(sid, v)| (sid.clone(), v.clone())).collect::<Vec<_>>()).unwrap_or_default())
        .into_iter().filter_map(|(sid, v)| { let it = cat.series_item(sid.parse().ok()?)?; let mut c = app.card_meta(it); c["newEp"] = v["latest"].clone(); c["newCount"] = json!(v["ids"].as_array().map(|a| a.len()).unwrap_or(0)); Some(c) }).collect();
    // evening window in local time: now .. 01:00 (same day if already 00:00..00:59, else tomorrow 01:00)
    let end_ms = tonight_end_ms(chrono::Local::now());
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn mk_ch(id: i64, title: &str, epg: &str, archive: i64) -> Item {
        Item { k: "l", id, title: title.into(), epg: epg.into(), archive, en: true, ..Default::default() }
    }

    #[test]
    fn merge_keeps_us_and_au_channels_separate_while_merging_same_country_feeds() {
        let us_hd = mk_ch(1, "US | ESPN HD", "espn.us", 3);
        let us_4k = mk_ch(2, "US | ESPN 4K", "", 0);
        let au_hd = mk_ch(3, "AU | ESPN HD", "x:espn.au", 0);
        let au_fhd = mk_ch(4, "AU | ESPN FHD", "x:espn.au", 0);

        let refs = vec![&us_hd, &us_4k, &au_hd, &au_fhd];
        let rows = merge(&refs);
        assert_eq!(rows.len(), 2, "US | ESPN and AU | ESPN must NEVER collapse into one row");
        assert_eq!(rows[0].country, "US");
        assert_eq!(rows[0].feeds.len(), 2);
        assert_eq!(rows[0].feeds[0].0.id, 2, "4K feed should be primary for US | ESPN");
        assert_eq!(rows[0].title, "ESPN (US)");

        assert_eq!(rows[1].country, "AU");
        assert_eq!(rows[1].feeds.len(), 2);
        assert_eq!(rows[1].feeds[0].0.id, 4, "FHD feed should be primary for AU | ESPN");
        assert_eq!(rows[1].title, "ESPN (AU)");
    }

    #[test]
    fn tonight_window_handles_post_midnight_correctly() {
        let post_midnight = chrono::Utc.with_ymd_and_hms(2026, 11, 20, 0, 20, 0).unwrap();
        let end = tonight_end_ms(post_midnight);
        let expected = chrono::Utc.with_ymd_and_hms(2026, 11, 20, 1, 0, 0).unwrap().timestamp_millis();
        assert_eq!(end, expected, "00:20 must target today's 01:00, not tomorrow's");

        let evening = chrono::Utc.with_ymd_and_hms(2026, 11, 20, 21, 0, 0).unwrap();
        let end2 = tonight_end_ms(evening);
        let expected2 = chrono::Utc.with_ymd_and_hms(2026, 11, 21, 1, 0, 0).unwrap().timestamp_millis();
        assert_eq!(end2, expected2, "21:00 must target tomorrow's 01:00");
    }

    #[test]
    fn timeshift_padding_adds_3min_each_side_and_clamps_to_now() {
        let now = 1_700_000_000_000i64;
        // Programme finished 1 hour ago (60m long) -> padded 3m before + 3m after = 66m
        let start = now - 2 * 3_600_000;
        let (ps, mins) = pad_timeshift_window(start, 60, 7, now);
        assert_eq!(ps, start - 180_000);
        assert_eq!(mins, 66);

        // Programme finished 1 minute ago -> after-padding clamps at `now` (60 + 3 + 1 = 64m)
        let start2 = now - 61 * 60_000;
        let (ps2, mins2) = pad_timeshift_window(start2, 60, 7, now);
        assert_eq!(ps2, start2 - 180_000);
        assert_eq!(mins2, 64);
    }
}
