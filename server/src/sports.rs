// Sports: parse event channel names (fixtures + times), XMLTV EPG now/next, sports networks.
use crate::app::{App, Prog};
use crate::re;
use crate::util::now_ms;
use chrono::{Datelike, NaiveDate, TimeZone};
use chrono_tz::Tz;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub k: &'static str, pub id: i64, pub title: String, pub league: String, pub start: Option<i64>, pub status: String,
    pub channel: String, pub logo: String, pub cat: String, pub en: bool, pub ended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")] pub alts: Vec<Value>,
}
pub struct Parsed { pub title: String, pub start: Option<i64>, pub league: String, pub ended: bool }

const ET: Tz = chrono_tz::America::New_York;
fn tz_abbr(a: &str) -> Tz {
    match a.to_uppercase().as_str() { "EST" | "EDT" | "ET" => ET, "CET" | "CEST" => chrono_tz::Europe::Paris, "BST" => chrono_tz::Europe::London, "AEST" | "AEDT" => chrono_tz::Australia::Sydney, _ => chrono_tz::UTC }
}
fn zoned(y: i32, mo: u32, d: u32, h: u32, mi: u32, tz: Tz) -> Option<i64> {
    let nd = NaiveDate::from_ymd_opt(y, mo + 1, d)?.and_hms_opt(h.min(23), mi.min(59), 0)?;
    tz.from_local_datetime(&nd).earliest().map(|dt| dt.timestamp_millis())
}
fn today(tz: Tz) -> (i32, u32, u32) { let n = chrono::Utc::now().with_timezone(&tz); (n.year(), n.month0(), n.day()) }
fn to24(h: u32, mi: u32, ap: &str) -> (u32, u32) {
    let mut h = h; let ap = ap.to_lowercase();
    if ap == "pm" && h < 12 { h += 12; } if ap == "am" && h == 12 { h = 0; }
    (h, mi)
}
fn month(m: &str) -> Option<u32> { ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|x| m.to_lowercase().starts_with(x)).map(|x| x as u32) }

pub fn league_of(s: &str) -> String {
    let l: &[(&str, &regex::Regex)] = &[("NFL", re!(r"(?i)\bNFL\b")), ("NCAAF", re!(r"(?i)NCAAF|COLLEGE FOOTBALL")), ("WNBA", re!(r"(?i)WNBA")), ("NBA", re!(r"(?i)\bNBA\b")), ("NCAAB", re!(r"(?i)NCAAB")),
        ("NHL", re!(r"(?i)\bNHL\b")), ("MLB", re!(r"(?i)\bMLB\b|MILB")), ("MLS", re!(r"(?i)\bMLS\b")), ("UFC", re!(r"(?i)UFC")), ("Boxing", re!(r"(?i)BOXING|MATCHROOM|BOX OFFICE")),
        ("F1", re!(r"(?i)\bF1\b|FORMULA")), ("AFL", re!(r"(?i)\bAFL\b")), ("NRL", re!(r"(?i)\bNRL\b")), ("Cricket", re!(r"(?i)CRICKET")), ("Rugby", re!(r"(?i)RUGBY")),
        ("Tennis", re!(r"(?i)TENNIS")), ("Golf", re!(r"(?i)GOLF|MASTERS")), ("Wrestling", re!(r"(?i)WWE|AEW|WRESTL")), ("Racing", re!(r"(?i)RACING|SUPERCROSS|MOTOGP|NASCAR|INDYCAR")),
        ("Soccer", re!(r"(?i)SOCCER|UEFA|FIFA|EPL|PREMIER|LALIGA|SERIE A|BUNDESLIGA|FOOTBALL|DAZN"))];
    l.iter().find(|(_, r)| r.is_match(s)).map(|x| x.0.to_string()).unwrap_or_else(|| "Sports".into())
}

fn zone_for(t: &str, cat: &str) -> Tz {
    if re!(r"\bET\b|EST|EDT").is_match(t) { return ET; }
    if re!(r"\bUK\b|GMT|BST").is_match(t) { return chrono_tz::Europe::London; }
    if re!(r"\bAEST|AEDT\b").is_match(t) { return chrono_tz::Australia::Sydney; }
    if re!(r"^US|^CA").is_match(cat) { ET } else { chrono_tz::Europe::London }
}

fn parse_structured(name: &str, cat: &str) -> Option<Parsed> {
    let segs: Vec<&str> = name.split('|').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 { return None; }
    let ended = re!(r"(?i)^(ENDED|End|FINISHED)$").is_match(segs[0]);
    let title_seg = segs.iter().find(|s| re!(r"(?i)\s(vs?\.?|at|@|x)\s").is_match(s))
        .or_else(|| segs.iter().find(|s| re!(r"(?i)[a-z]{3}").is_match(s) && !re!(r"(?i)^(ENDED|End|NEXT|LIVE|all|8K EXCLUSIVE)$").is_match(s) && !s.starts_with(|c: char| c.is_ascii_digit())))?;
    let mut start = None;
    if let Some(c) = re!(r"(\d{2})-(\d{2})-(\d{4})\s*\|\s*(\d{1,2}):(\d\d)\s*\((GMT|UTC|CET|EST|ET|BST)\)").captures(name) {
        start = zoned(c[3].parse().ok()?, c[2].parse::<u32>().ok()? - 1, c[1].parse().ok()?, c[4].parse().ok()?, c[5].parse().ok()?, tz_abbr(&c[6]));
    } else if let Some(c) = re!(r"(?i)(\d{1,2})\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\s+(\d{1,2}):(\d\d)\s*(EST|EDT|ET|CET|CEST|GMT|UTC|BST|AEST|AEDT)").captures(name) {
        let tz = tz_abbr(&c[5]); let (y, _, _) = today(tz);
        start = zoned(y, month(&c[2])?, c[1].parse().ok()?, c[3].parse().ok()?, c[4].parse().ok()?, tz);
    }
    let mut t = re!(r"^#\d+\s+").replace(title_seg, "").to_string();
    t = re!(r"\s#\d+\s").replace_all(&t, " ").to_string();
    t = re!(r"\s{2,}").replace_all(&t, " ").trim().to_string();
    if t == t.to_uppercase() { t = re!(r"\bVs\b").replace_all(&crate::util::title_case(&t.to_lowercase()), "vs").to_string(); }
    if t.len() < 5 { return None; }
    Some(Parsed { title: t, start, league: league_of(&format!("{cat} {name}")), ended })
}

pub fn parse_event(name: &str, cat: &str) -> Option<Parsed> {
    if !re!(r"(?i)\bvs?\.?\s|\s@\s|\sat\s|\sx\s|start:\d{4}|name:|\bv\b|\bvs\b").is_match(name) { return None; }
    if re!(r"(?i)^En Español").is_match(name) { return None; }
    if name.matches('|').count() >= 2 { if let Some(p) = parse_structured(name, cat) { return Some(p); } }
    let mut t = name.to_string();
    if t.contains("//") {
        let parts: Vec<&str> = t.split("//").collect();
        let et = parts.iter().find(|p| re!(r"\bET\b").is_match(p)).or(parts.get(1)).copied().unwrap_or("");
        t = format!("{} {}", re!(r"(?i)\d{1,2}(:\d\d)?\s*[ap]m.*$").replace(parts[0], ""), et);
    }
    let zone = zone_for(&t, cat);
    let mut start: Option<i64> = None;
    if let Some(c) = re!(r"start:(\d{4})-(\d\d)-(\d\d) (\d\d):(\d\d)").captures(&t) {
        start = zoned(c[1].parse().unwrap_or(0), c[2].parse::<u32>().unwrap_or(1) - 1, c[3].parse().unwrap_or(1), c[4].parse().unwrap_or(0), c[5].parse().unwrap_or(0), chrono_tz::UTC);
    }
    t = re!(r"start:\S+ \S+").replace_all(&t, "").to_string(); t = re!(r"stop:\S+ \S+").replace_all(&t, "").to_string();
    if start.is_none() { if let Some(c) = re!(r"\((\d{4})-(\d\d)-(\d\d) (\d\d):(\d\d)(?::\d\d)?\)").captures(&t) {
        start = zoned(c[1].parse().unwrap_or(0), c[2].parse::<u32>().unwrap_or(1) - 1, c[3].parse().unwrap_or(1), c[4].parse().unwrap_or(0), c[5].parse().unwrap_or(0), zone); } }
    if start.is_none() { if let Some(c) = re!(r"([A-Za-z]{3})[a-z]*\.?\s+(\d{1,2})\s+(\d{1,2})(?::(\d\d))?\s*([AaPp][Mm])?").captures(&t) {
        if let Some(mo) = month(&c[1]) { let (y, _, _) = today(zone);
            let (h, mi) = to24(c[3].parse().unwrap_or(0), c.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0)), c.get(5).map_or("", |m| m.as_str()));
            start = zoned(y, mo, c[2].parse().unwrap_or(1), h, mi, zone); } } }
    if start.is_none() { if let Some(c) = re!(r"\((\d{1,2})\.(\d{1,2})\s+(\d{1,2})(?::(\d\d))?\s*([AaPp][Mm])").captures(&t) {
        let (y, _, _) = today(zone); let (h, mi) = to24(c[3].parse().unwrap_or(0), c.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0)), &c[5]);
        start = zoned(y, c[1].parse::<u32>().unwrap_or(1) - 1, c[2].parse().unwrap_or(1), h, mi, zone); } }
    if start.is_none() { if let Some(c) = re!(r"(?i)\b(\d{1,2})(?::(\d\d))?\s*([ap]m)\b").captures(&t) {
        let (y, mo, d) = today(zone);
        let (h, mi) = to24(c[1].parse().unwrap_or(0), c.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0)), &c[3]);
        let base = zoned(y, mo, d, h, mi, zone);
        let wd = re!(r"(?i)\b(Sun|Mon|Tue|Wed|Thu|Fri|Sat)[a-z]*\b").captures(&t).map(|w| w[1].to_lowercase());
        start = base;
        if let (Some(wd), Some(b)) = (wd, base) {
            let want = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"].iter().position(|x| wd.starts_with(x)).unwrap_or(0) as u32;
            for k in -1i64..=6 { let c2 = b + k * 86_400_000; let wdk = chrono::Utc.timestamp_millis_opt(c2).unwrap().with_timezone(&zone).weekday().num_days_from_sunday(); if wdk == want { start = Some(c2); break; } }
        } else if let Some(b) = base { if b < now_ms() - 14 * 3_600_000 { start = Some(b + 86_400_000); } }
    } }
    // strip times, dates and channel scaffolding
    let steps: &[(&regex::Regex, &str)] = &[
        (re!(r"\(\d{4}-\d\d-\d\d[^)]*\)"), ""), (re!(r"\(\d{1,2}\.\d{1,2}[^)]*\)"), ""),
        (re!(r"@?\s*\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.?\s+\d{1,2}\b(\s+\d{1,2}(:\d\d)?\s*([AaPp][Mm])?)?"), ""),
        (re!(r"(?i)\b\d{1,2}(:\d\d)?\s*[ap]m\b"), ""), (re!(r"\b(ET|UK|GMT|BST|AEDT|AEST)\b"), ""),
        (re!(r"(?i)\b(Sun|Mon|Tue|Wed|Thu|Fri|Sat)(day|urday|nesday|sday|rsday)?\b\s*$"), ""),
        (re!(r"^[^|]*\|\s*"), ""), (re!(r"(?i)^\s*(4k|\d{1,3})\s*[-:]\s*"), ""), (re!(r"\s*:\s*[^:]{2,28}?\s+\d{1,3}\s*$"), ""),
        (re!(r"^[A-Za-z+ ]{2,14}\s*\d{1,3}\s*:\s*"), ""), (re!(r"(?i)^.*?event:\s*\d*\s*name:\s*"), ""), (re!(r"(?i)^\s*(SNF|MNF|TNF)\s*"), ""),
        (re!(r"\(\s*\)"), ""), (re!(r"[@:\-|/]+\s*$"), ""), (re!(r"^\s*[@:\-|]+"), ""), (re!(r"\s{2,}"), " "),
        (re!(r"\[\d+\]"), ""), (re!(r"^\s*\(?[A-Z]{2,6}\s*\d{2,4}\)?\s*\|\s*"), ""),
        (re!(r"(?i)^(live|replay|racing|soccer|cricket|tennis|golf|rugby|wrestling|basketball|football|hockey|baseball)\s*:\s*"), ""),
        (re!(r"^(UFC|NFL|NBA|NHL|MLB|MLS|F1|AFL|NRL|WWE|AEW)\s*:\s*"), ""), (re!(r"\s+-\s*$"), ""), (re!(r"\s{2,}"), " "),
    ];
    for (r, rep) in steps { t = r.replace_all(&t, *rep).to_string(); }
    let t = t.replace('_', " ").replace('`', "'").trim().to_string();
    if t.len() < 5 || !re!(r"(?i)[a-z]").is_match(&t) { return None; }
    Some(Parsed { title: t, start, league: league_of(&format!("{cat} {name}")), ended: false })
}

pub fn short_channel(name: &str) -> String {
    let segs: Vec<&str> = name.split('|').map(|s| s.trim()).collect();
    if segs.len() < 3 { return name.to_string(); }
    segs.iter().find(|s| re!(r"^[A-Z]{2}\s*[:(]").is_match(s) && !re!(r"(?i)vs\.?|\bat\b").is_match(s)).unwrap_or(segs.last().unwrap()).to_string()
}

/// Event channels (raw = before de-duplication; also keeps ended feeds for matching).
pub fn events(app: &App, raw: bool) -> Arc<Vec<Event>> {
    if raw { if let Some((t, v)) = app.events_raw.lock().unwrap().clone() { if now_ms() - t < 60_000 { return v; } } }
    let cat = app.catalog(); let now = now_ms(); let mut out = vec![];
    for ch in cat.live.iter().filter(|c| c.sport) {
        let cname = cat.cat_name("live", &ch.cat);
        let Some(ev) = parse_event(&ch.title, &cname) else { continue };
        let dur = if re!(r"UFC|Boxing|Wrestling|F1|Racing|Golf|Cricket|Tennis").is_match(&ev.league) { 6 * 3_600_000 } else { 3_600_000 * 7 / 2 };
        let mut status = "listed";
        if let Some(st) = ev.start {
            if now >= st - 600_000 && now < st + dur { status = "live"; } else if st > now { status = "upcoming"; } else { continue; }
            if st > now + 7 * 86_400_000 { continue; }
        }
        if ev.ended && !raw { continue; }
        out.push(Event { k: "l", id: ch.id, title: ev.title, league: ev.league, start: ev.start, status: status.into(), channel: short_channel(&ch.title), logo: ch.poster.clone(), cat: cname, en: ch.en, ended: ev.ended, alts: vec![] });
    }
    if raw { let v = Arc::new(out); *app.events_raw.lock().unwrap() = Some((now_ms(), v.clone())); return v; }
    // de-duplicate the same fixture across feeds
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new(); let mut kept: Vec<Event> = vec![];
    for e in out {
        let key: String = re!(r"\bvs?\b\.?").replace_all(&e.title.to_lowercase(), "v").chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let list = by_title.entry(key).or_default();
        if let Some(&pi) = list.iter().find(|&&i| kept[i].start.is_none() || e.start.is_none() || (kept[i].start.unwrap() - e.start.unwrap()).abs() < 4 * 3_600_000) {
            let p = &mut kept[pi];
            if !p.en && e.en {
                p.alts.push(json!({ "id": p.id, "channel": p.channel }));
                p.id = e.id; p.channel = e.channel.clone(); p.en = true;
            } else {
                p.alts.push(json!({ "id": e.id, "channel": e.channel }));
            }
            if p.start.is_none() && e.start.is_some() { p.start = e.start; p.status = e.status.clone(); }
        } else { list.push(kept.len()); kept.push(e); }
    }
    let ord = |s: &str| match s { "live" => 0, "upcoming" => 1, _ => 2 };
    kept.sort_by(|a, b| ord(&a.status).cmp(&ord(&b.status)).then(a.start.unwrap_or(i64::MAX).cmp(&b.start.unwrap_or(i64::MAX))).then(b.en.cmp(&a.en)));
    Arc::new(kept)
}

pub fn now_next(app: &App, epg: &str) -> Value {
    if epg.is_empty() { return json!({}); }
    let g = app.epg.read().unwrap().clone();
    let Some(list) = g.get(epg) else { return json!({}) };
    let now = now_ms();
    let p = |x: &Prog| json!({ "s": x.s, "e": x.e, "title": x.title, "desc": x.desc });
    match list.iter().position(|x| x.s <= now && x.e > now) {
        Some(i) => json!({ "now": p(&list[i]), "next": list.get(i + 1).map(p) }),
        None => list.iter().find(|x| x.s > now).map(|x| json!({ "next": p(x) })).unwrap_or(json!({})),
    }
}
pub fn merge(mut a: Value, b: Value) -> Value { if let (Some(ao), Value::Object(bo)) = (a.as_object_mut(), b) { for (k, v) in bo { ao.insert(k, v); } } a }

pub fn live_qtag(title: &str, cat_name: &str) -> String {
    let hay = format!("{title} {cat_name}").to_uppercase();
    let fps = if re!(r"60\s?FPS|⁶⁰ᶠᵖˢ").is_match(&hay) { " 60fps" } else if re!(r"50\s?FPS|⁵⁰ᶠᵖˢ").is_match(&hay) { " 50fps" } else { "" };
    if re!(r"\b(4K|UHD|3840P?|2160P?)\b|ᵁᴴᴰ|⁴ᴷ").is_match(&hay) { format!("4K{fps}") }
    else if re!(r"\bRAW\b|ᴿᴬᵂ").is_match(&hay) { format!("RAW{fps}") }
    else if !fps.is_empty() { fps.trim().to_string() }
    else if re!(r"\bDOLBY\b|ᴰᴼᴸᴮ").is_match(&hay) { "DOLBY".into() }
    else if re!(r"\bFHD\b|ᶠᴴᴰ").is_match(&hay) { "FHD".into() }
    else { String::new() }
}

pub fn channel_json(app: &App, ch: &crate::catalog::Item) -> Value {
    let cn = app.catalog().cat_name("live", &ch.cat);
    let qt = live_qtag(&ch.title, &cn);
    merge(json!({ "k": "l", "id": ch.id, "title": ch.title, "logo": ch.poster, "cat": ch.cat, "epg": ch.epg, "en": ch.en, "sport": ch.sport, "added": ch.added, "qtag": qt }), now_next(app, &ch.epg))
}

pub fn networks(app: &App) -> Vec<Value> {
    let cat = app.catalog(); let mut out = vec![];
    for ch in cat.live.iter().filter(|c| c.sport && c.en) {
        if parse_event(&ch.title, "").is_some() { continue; }
        let tail = re!(r"^.*?\|").replace(&ch.title, "");
        if re!(r"^\s*[A-Z+ ]{2,10}\s*\d{1,3}\s*[:|-]?\s*$").is_match(&tail) { continue; }
        let mut j = channel_json(app, ch); j["catName"] = json!(cat.cat_name("live", &ch.cat)); out.push(j);
    }
    out
}

/// Stream-parse the XMLTV file, keeping only channels we show (now-4h .. now+36h).
pub fn load_epg(app: &App) {
    let cat = app.catalog();
    // epg id -> keep 3 days of history (catch-up channels) or 4 hours
    let mut wanted: HashMap<&str, bool> = HashMap::new();
    for x in cat.live.iter().filter(|x| !x.epg.is_empty() && (x.en || x.sport)) { let e = wanted.entry(x.epg.as_str()).or_insert(false); *e = *e || x.archive > 0; }
    let (lo_short, lo_long, hi) = (now_ms() - 4 * 3_600_000, now_ms() - 3 * 86_400_000, now_ms() + 48 * 3_600_000);
    let mut sources: Vec<(String, String)> = vec![];
    if let Ok(t) = std::fs::read_to_string(app.data.join("raw").join("epg.xml")) { sources.push((String::new(), t)); }
    for f in crate::epgx::files(&app.data) { if let Ok(t) = std::fs::read_to_string(&f) { sources.push(("x:".into(), t)); } }
    let ts = |s: &str, z: Option<&str>| -> i64 {
        let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
        let mut t = chrono::Utc.with_ymd_and_hms(n(0, 4) as i32, n(4, 6) as u32, n(6, 8) as u32, n(8, 10) as u32, n(10, 12) as u32, n(12, 14) as u32).single().map(|d| d.timestamp_millis()).unwrap_or(0);
        if let Some(z) = z { if z.len() == 5 { let sign = if z.starts_with('-') { -1 } else { 1 }; let h: i64 = z[1..3].parse().unwrap_or(0); let m: i64 = z[3..5].parse().unwrap_or(0); t -= sign * (h * 60 + m) * 60_000; } }
        t
    };
    let dec = |s: &str| re!(r"<!\[CDATA\[([\s\S]*?)\]\]>").replace_all(s, "$1").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").trim().to_string();
    let mut epg: HashMap<String, Vec<Prog>> = HashMap::new();
    let r = re!(r#"<programme\s+start="(\d{14})\s*([+-]\d{4})?"\s+stop="(\d{14})\s*([+-]\d{4})?"\s+channel="([^"]*)"\s*>([\s\S]*?)</programme>"#);
    for (prefix, text) in &sources { for c in r.captures_iter(text) {
        let key = format!("{prefix}{}", &c[5]);
        let Some(&long) = wanted.get(key.as_str()) else { continue };
        let s = ts(&c[1], c.get(2).map(|m| m.as_str())); let e = ts(&c[3], c.get(4).map(|m| m.as_str()));
        if e < (if long { lo_long } else { lo_short }) || s > hi { continue; }
        let body = &c[6];
        let title = re!(r"<title[^>]*>([\s\S]*?)</title>").captures(body).map(|x| dec(&x[1])).unwrap_or_default();
        let desc: String = re!(r"<desc[^>]*>([\s\S]*?)</desc>").captures(body).map(|x| dec(&x[1])).unwrap_or_default().chars().take(300).collect();
        epg.entry(key).or_default().push(Prog { s, e, title, desc });
    } }
    for l in epg.values_mut() { l.sort_by_key(|p| p.s); }
    println!("[epg] {} channels", epg.len());
    *app.epg.write().unwrap() = Arc::new(epg);
}
