// Sports: parse event channel names (fixtures + times), streaming XMLTV EPG now/next, sports networks.
use crate::app::{App, Prog};
use crate::re;
use crate::util::now_ms;
use chrono::{Datelike, NaiveDate, TimeZone};
use chrono_tz::Tz;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub k: &'static str, pub id: i64, pub title: String, pub league: String, pub start: Option<i64>, pub status: String,
    pub channel: String, pub logo: String, pub cat: String, pub en: bool, pub ended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")] pub alts: Vec<Value>,
}
pub struct Parsed { pub title: String, pub start: Option<i64>, pub league: String, pub ended: bool }

const ET: Tz = chrono_tz::America::New_York;

pub(crate) fn tz_abbr(a: &str) -> Tz {
    match a.trim().to_uppercase().as_str() {
        "EST" | "EDT" | "ET" => ET,
        "CST" | "CDT" | "CT" => chrono_tz::America::Chicago,
        "MST" | "MDT" | "MT" => chrono_tz::America::Denver,
        "PST" | "PDT" | "PT" => chrono_tz::America::Los_Angeles,
        "CET" | "CEST" => chrono_tz::Europe::Paris,
        "BST" | "UK" => chrono_tz::Europe::London,
        "AEST" | "AEDT" => chrono_tz::Australia::Sydney,
        "NZST" | "NZDT" => chrono_tz::Pacific::Auckland,
        _ => chrono_tz::UTC,
    }
}

fn zoned(y: i32, mo: u32, d: u32, h: u32, mi: u32, tz: Tz) -> Option<i64> {
    let nd = NaiveDate::from_ymd_opt(y, mo + 1, d)?.and_hms_opt(h.min(23), mi.min(59), 0)?;
    tz.from_local_datetime(&nd).earliest().map(|dt| dt.timestamp_millis())
}

/// Resolve a yearless (month0, day, hour, minute) date to the nearest year within ±6 months of `ref_ms`.
pub(crate) fn zoned_nearest_year(mo: u32, d: u32, h: u32, mi: u32, tz: Tz, ref_ms: i64) -> Option<i64> {
    let ref_dt = chrono::DateTime::from_timestamp_millis(ref_ms)?.with_timezone(&tz);
    let y = ref_dt.year();
    let mut best: Option<(i64, i64)> = None;
    for cand_y in [y - 1, y, y + 1] {
        if let Some(ts) = zoned(cand_y, mo, d, h, mi, tz) {
            let diff = (ts - ref_ms).abs();
            if best.map_or(true, |(bd, _)| diff < bd) {
                best = Some((diff, ts));
            }
        }
    }
    best.map(|(_, ts)| ts)
}

fn today_at(tz: Tz, ref_ms: i64) -> (i32, u32, u32) {
    let n = chrono::DateTime::from_timestamp_millis(ref_ms)
        .unwrap_or_else(chrono::Utc::now)
        .with_timezone(&tz);
    (n.year(), n.month0(), n.day())
}

fn to24(h: u32, mi: u32, ap: &str) -> (u32, u32) {
    let mut h = h; let ap = ap.to_lowercase();
    if ap == "pm" && h < 12 { h += 12; }
    if ap == "am" && h == 12 { h = 0; }
    (h, mi)
}

fn month(m: &str) -> Option<u32> {
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
        .iter()
        .position(|x| m.to_lowercase().starts_with(x))
        .map(|x| x as u32)
}

pub fn league_of(s: &str) -> String {
    let l: &[(&str, &regex::Regex)] = &[
        ("NFL", re!(r"(?i)\bNFL\b")), ("NCAAF", re!(r"(?i)NCAAF|COLLEGE FOOTBALL")), ("WNBA", re!(r"(?i)WNBA")),
        ("NBA", re!(r"(?i)\bNBA\b")), ("NCAAB", re!(r"(?i)NCAAB")), ("NHL", re!(r"(?i)\bNHL\b")),
        ("MLB", re!(r"(?i)\bMLB\b|MILB")), ("MLS", re!(r"(?i)\bMLS\b")), ("UFC", re!(r"(?i)UFC")),
        ("Boxing", re!(r"(?i)BOXING|MATCHROOM|BOX OFFICE")), ("F1", re!(r"(?i)\bF1\b|FORMULA")),
        ("AFL", re!(r"(?i)\bAFL\b")), ("NRL", re!(r"(?i)\bNRL\b")), ("Cricket", re!(r"(?i)CRICKET")),
        ("Rugby", re!(r"(?i)RUGBY")), ("Tennis", re!(r"(?i)TENNIS")), ("Golf", re!(r"(?i)GOLF|MASTERS")),
        ("Wrestling", re!(r"(?i)WWE|AEW|WRESTL")), ("Racing", re!(r"(?i)RACING|SUPERCROSS|MOTOGP|NASCAR|INDYCAR")),
        ("Soccer", re!(r"(?i)SOCCER|UEFA|FIFA|EPL|PREMIER|LALIGA|SERIE A|BUNDESLIGA|FOOTBALL|DAZN")),
    ];
    l.iter().find(|(_, r)| r.is_match(s)).map(|x| x.0.to_string()).unwrap_or_else(|| "Sports".into())
}

pub(crate) fn zone_for(t: &str, cat: &str) -> Tz {
    if re!(r"(?i)\b(EST|EDT|ET)\b").is_match(t) { return ET; }
    if re!(r"(?i)\b(CST|CDT|CT)\b").is_match(t) { return chrono_tz::America::Chicago; }
    if re!(r"(?i)\b(MST|MDT|MT)\b").is_match(t) { return chrono_tz::America::Denver; }
    if re!(r"(?i)\b(PST|PDT|PT)\b").is_match(t) { return chrono_tz::America::Los_Angeles; }
    if re!(r"(?i)\b(NZDT|NZST)\b").is_match(t) { return chrono_tz::Pacific::Auckland; }
    if re!(r"(?i)\b(AEST|AEDT)\b").is_match(t) { return chrono_tz::Australia::Sydney; }
    if re!(r"(?i)\b(CET|CEST)\b").is_match(t) { return chrono_tz::Europe::Paris; }
    if re!(r"(?i)\b(UK|GMT|BST)\b").is_match(t) { return chrono_tz::Europe::London; }
    let c = cat.trim();
    if re!(r"(?i)^(US|CA)\b").is_match(c) || re!(r"(?i)^(US|CA)\s*[|:]").is_match(t) { return ET; }
    if re!(r"(?i)^AU\b").is_match(c) || re!(r"(?i)^AU\s*[|:]").is_match(t) { return chrono_tz::Australia::Sydney; }
    if re!(r"(?i)^NZ\b").is_match(c) || re!(r"(?i)^NZ\s*[|:]").is_match(t) { return chrono_tz::Pacific::Auckland; }
    if re!(r"(?i)^(EU|DE|FR|ES|IT|NL|BE|SE|NO|DK|PL)\b").is_match(c) { return chrono_tz::Europe::Paris; }
    chrono_tz::Europe::London
}

fn is_day_first_region(cat: &str, zone: Tz) -> bool {
    let c = cat.trim();
    if re!(r"(?i)^(US|CA)\b").is_match(c) {
        return false;
    }
    if re!(r"(?i)^(AU|NZ|UK|IE|EU|DE|FR|ES|IT|NL|BE|PT|SE|NO|DK|PL)\b").is_match(c) {
        return true;
    }
    !matches!(
        zone,
        chrono_tz::America::New_York
            | chrono_tz::America::Chicago
            | chrono_tz::America::Denver
            | chrono_tz::America::Los_Angeles
    )
}

fn parse_structured_at(name: &str, cat: &str, ref_ms: i64) -> Option<Parsed> {
    let segs: Vec<&str> = name.split('|').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    if segs.len() < 3 { return None; }
    let ended = re!(r"(?i)^(ENDED|End|FINISHED)$").is_match(segs[0]);
    let title_seg = segs.iter().find(|s| re!(r"(?i)\s(vs?\.?|at|@|x)\s").is_match(s))
        .or_else(|| segs.iter().find(|s| re!(r"(?i)[a-z]{3}").is_match(s) && !re!(r"(?i)^(ENDED|End|NEXT|LIVE|all|8K EXCLUSIVE)$").is_match(s) && !s.starts_with(|c: char| c.is_ascii_digit())))?;
    let mut start = None;
    if let Some(c) = re!(r"(?i)(\d{2})-(\d{2})-(\d{4})\s*\|\s*(\d{1,2}):(\d\d)\s*\((GMT|UTC|CET|CEST|EST|EDT|ET|CST|CDT|CT|MST|MDT|MT|PST|PDT|PT|BST|AEST|AEDT|NZST|NZDT)\)").captures(name) {
        start = zoned(c[3].parse().ok()?, c[2].parse::<u32>().ok()?.checked_sub(1)?, c[1].parse().ok()?, c[4].parse().ok()?, c[5].parse().ok()?, tz_abbr(&c[6]));
    } else if let Some(c) = re!(r"(?i)(\d{1,2})\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\s+(\d{1,2}):(\d\d)\s*([ap]m)?\s*(EST|EDT|ET|CST|CDT|CT|MST|MDT|MT|PST|PDT|PT|CET|CEST|GMT|UTC|BST|AEST|AEDT|NZST|NZDT)").captures(name) {
        let tz = tz_abbr(&c[6]);
        let (h, mi) = to24(c[3].parse().ok()?, c[4].parse().ok()?, c.get(5).map_or("", |m| m.as_str()));
        start = zoned_nearest_year(month(&c[2])?, c[1].parse().ok()?, h, mi, tz, ref_ms);
    }
    let mut t = re!(r"^#\d+\s+").replace(title_seg, "").to_string();
    t = re!(r"\s#\d+\s").replace_all(&t, " ").to_string();
    t = re!(r"\s{2,}").replace_all(&t, " ").trim().to_string();
    if t == t.to_uppercase() { t = re!(r"\bVs\b").replace_all(&crate::util::title_case(&t.to_lowercase()), "vs").to_string(); }
    if t.len() < 5 { return None; }
    Some(Parsed { title: t, start, league: league_of(&format!("{cat} {name}")), ended })
}

pub fn parse_event(name: &str, cat: &str) -> Option<Parsed> {
    parse_event_at(name, cat, now_ms())
}

pub(crate) fn parse_event_at(name: &str, cat: &str, ref_ms: i64) -> Option<Parsed> {
    if !re!(r"(?i)\bvs?\.?\s|\s@\s|\sat\s|\sx\s|start:\d{4}|name:|\bv\b|\bvs\b").is_match(name) { return None; }
    if re!(r"(?i)^En Español").is_match(name) { return None; }
    if name.matches('|').count() >= 2 { if let Some(p) = parse_structured_at(name, cat, ref_ms) { return Some(p); } }
    let mut t = name.to_string();
    if t.contains("//") {
        let parts: Vec<&str> = t.split("//").collect();
        let et = parts.iter().find(|p| re!(r"(?i)\b(ET|EST|EDT|AEST|AEDT|NZST|NZDT|UK|GMT|BST)\b").is_match(p)).or(parts.get(1)).copied().unwrap_or("");
        t = format!("{} {}", re!(r"(?i)\d{1,2}(:\d\d)?\s*[ap]m.*$").replace(parts[0], ""), et);
    }
    let zone = zone_for(&t, cat);
    let mut start: Option<i64> = None;
    if let Some(c) = re!(r"start:(\d{4})-(\d\d)-(\d\d) (\d\d):(\d\d)").captures(&t) {
        start = zoned(c[1].parse().unwrap_or(0), c[2].parse::<u32>().unwrap_or(1).saturating_sub(1), c[3].parse().unwrap_or(1), c[4].parse().unwrap_or(0), c[5].parse().unwrap_or(0), chrono_tz::UTC);
    }
    t = re!(r"start:\S+ \S+").replace_all(&t, "").to_string();
    t = re!(r"stop:\S+ \S+").replace_all(&t, "").to_string();
    if start.is_none() {
        if let Some(c) = re!(r"\((\d{4})-(\d\d)-(\d\d) (\d\d):(\d\d)(?::\d\d)?\)").captures(&t) {
            start = zoned(c[1].parse().unwrap_or(0), c[2].parse::<u32>().unwrap_or(1).saturating_sub(1), c[3].parse().unwrap_or(1), c[4].parse().unwrap_or(0), c[5].parse().unwrap_or(0), zone);
        }
    }
    if start.is_none() {
        if let Some(c) = re!(r"([A-Za-z]{3})[a-z]*\.?\s+(\d{1,2})\s+(\d{1,2})(?::(\d\d))?\s*([AaPp][Mm])?").captures(&t) {
            if let Some(mo) = month(&c[1]) {
                let (h, mi) = to24(c[3].parse().unwrap_or(0), c.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0)), c.get(5).map_or("", |m| m.as_str()));
                start = zoned_nearest_year(mo, c[2].parse().unwrap_or(1), h, mi, zone, ref_ms);
            }
        }
    }
    if start.is_none() {
        if let Some(c) = re!(r"\((\d{1,2})\.(\d{1,2})\s+(\d{1,2})(?::(\d\d))?\s*([AaPp][Mm])?").captures(&t) {
            let a = c[1].parse::<u32>().unwrap_or(1);
            let b = c[2].parse::<u32>().unwrap_or(1);
            let (mo1, day) = if a > 12 && b <= 12 {
                (b, a)
            } else if b > 12 && a <= 12 {
                (a, b)
            } else if is_day_first_region(cat, zone) {
                (b, a)
            } else {
                (a, b)
            };
            let (h, mi) = to24(c[3].parse().unwrap_or(0), c.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0)), c.get(5).map_or("", |m| m.as_str()));
            if mo1 >= 1 && mo1 <= 12 {
                start = zoned_nearest_year(mo1 - 1, day, h, mi, zone, ref_ms);
            }
        }
    }
    if start.is_none() {
        if let Some(c) = re!(r"(?i)\b(\d{1,2})(?::(\d\d))?\s*([ap]m)\b").captures(&t) {
            let (y, mo, d) = today_at(zone, ref_ms);
            let (h, mi) = to24(c[1].parse().unwrap_or(0), c.get(2).map_or(0, |m| m.as_str().parse().unwrap_or(0)), &c[3]);
            let base = zoned(y, mo, d, h, mi, zone);
            let wd = re!(r"(?i)\b(Sun|Mon|Tue|Wed|Thu|Fri|Sat)[a-z]*\b").captures(&t).map(|w| w[1].to_lowercase());
            start = base;
            if let (Some(wd), Some(b)) = (wd, base) {
                let want = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"].iter().position(|x| wd.starts_with(x)).unwrap_or(0) as u32;
                for k in -1i64..=6 {
                    let c2 = b + k * 86_400_000;
                    if let Some(dt) = chrono::Utc.timestamp_millis_opt(c2).single() {
                        if dt.with_timezone(&zone).weekday().num_days_from_sunday() == want {
                            start = Some(c2);
                            break;
                        }
                    }
                }
            } else if let Some(b) = base {
                if b < ref_ms - 14 * 3_600_000 { start = Some(b + 86_400_000); }
            }
        }
    }
    // strip times, dates and channel scaffolding
    let steps: &[(&regex::Regex, &str)] = &[
        (re!(r"\(\d{4}-\d\d-\d\d[^)]*\)"), ""), (re!(r"\(\d{1,2}\.\d{1,2}[^)]*\)"), ""),
        (re!(r"@?\s*\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*\.?\s+\d{1,2}\b(\s+\d{1,2}(:\d\d)?\s*([AaPp][Mm])?)?"), ""),
        (re!(r"(?i)\b\d{1,2}(:\d\d)?\s*[ap]m\b"), ""),
        (re!(r"(?i)\b(EST|EDT|ET|CST|CDT|CT|MST|MDT|MT|PST|PDT|PT|CET|CEST|UK|GMT|BST|UTC|AEDT|AEST|NZDT|NZST)\b"), ""),
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

/// Clear the cached parsed live-sport events list (call on catalog reload).
pub fn clear_events_cache(app: &App) {
    *app.events_raw.lock().unwrap() = None;
}

/// Event channels (raw = before de-duplication; also keeps ended feeds for matching).
pub fn events(app: &App, raw: bool) -> Arc<Vec<Event>> {
    if raw {
        if let Some((t, v)) = app.events_raw.lock().unwrap().clone() {
            if now_ms() - t < 60_000 { return v; }
        }
    }
    let cat = app.catalog(); let now = now_ms(); let mut out = vec![];
    for ch in cat.live.iter().filter(|c| c.sport) {
        let cname = cat.cat_name("live", &ch.cat);
        let Some(ev) = parse_event(&ch.title, &cname) else { continue };
        // Generous overtime grace: 8h for Tennis/Cricket/Golf, 6h for Fight/Racing cards, 4.5h for team sports.
        let dur = if re!(r"Tennis|Cricket|Golf").is_match(&ev.league) {
            8 * 3_600_000
        } else if re!(r"UFC|Boxing|Wrestling|F1|Racing").is_match(&ev.league) {
            6 * 3_600_000
        } else {
            3_600_000 * 9 / 2
        };
        let mut status = "listed";
        if let Some(st) = ev.start {
            if now >= st - 600_000 && now < st + dur { status = "live"; }
            else if st > now { status = "upcoming"; }
            else { continue; }
            if st > now + 7 * 86_400_000 { continue; }
        }
        if ev.ended && !raw { continue; }
        out.push(Event { k: "l", id: ch.id, title: ev.title, league: ev.league, start: ev.start, status: status.into(), channel: short_channel(&ch.title), logo: ch.poster.clone(), cat: cname, en: ch.en, ended: ev.ended, alts: vec![] });
    }
    if raw {
        let v = Arc::new(out);
        *app.events_raw.lock().unwrap() = Some((now_ms(), v.clone()));
        return v;
    }
    // de-duplicate the same fixture across feeds
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new(); let mut kept: Vec<Event> = vec![];
    for e in out {
        let key: String = re!(r"\bvs?\b\.?").replace_all(&e.title.to_lowercase(), "v").chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let list = by_title.entry(key).or_default();
        if let Some(&pi) = list.iter().find(|&&i| kept[i].start.is_none() || e.start.is_none() || (kept[i].start.unwrap() - e.start.unwrap()).abs() < 4 * 3_600_000) {
            let p = &mut kept[pi];
            let qrank = |ch: &str, cat: &str| -> i32 {
                let qt = live_qtag(ch, cat);
                if qt.starts_with("4K") { 0 } else if qt.starts_with("RAW") && qt.contains("fps") { 1 }
                else if qt.starts_with("RAW") { 2 } else if qt.contains("fps") { 3 }
                else if qt == "FHD" { 4 } else { 5 }
            };
            let better = (!p.en && e.en) || (p.en == e.en && qrank(&e.channel, &e.cat) < qrank(&p.channel, &p.cat));
            if better {
                p.alts.push(json!({ "id": p.id, "channel": p.channel }));
                p.id = e.id; p.channel = e.channel.clone(); p.cat = e.cat.clone(); p.en = e.en;
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

// ---------------------------------------------------------------- Streaming XMLTV parser

/// Decode CDATA wrappers plus all named HTML/XML entities and numeric (`&#039;`, `&#8217;`, `&#x27;`) entities in a single pass.
pub(crate) fn decode_xml_entities(raw: &str) -> String {
    let stripped = if raw.contains("<![CDATA[") {
        let mut tmp = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(i) = rest.find("<![CDATA[") {
            tmp.push_str(&rest[..i]);
            let after = &rest[i + 9..];
            if let Some(j) = after.find("]]>") {
                tmp.push_str(&after[..j]);
                rest = &after[j + 3..];
            } else {
                tmp.push_str(after);
                rest = "";
                break;
            }
        }
        tmp.push_str(rest);
        std::borrow::Cow::Owned(tmp)
    } else {
        std::borrow::Cow::Borrowed(raw)
    };
    let s = stripped.as_ref();
    if !s.contains('&') {
        return s.trim().to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi_rel) = s[i + 1..].find(';') {
                if semi_rel > 0 && semi_rel <= 12 {
                    let ent = &s[i + 1..i + 1 + semi_rel];
                    let decoded: Option<char> = if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(dec) = ent.strip_prefix('#') {
                        dec.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        match ent {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            "nbsp" => Some(' '),
                            "ndash" => Some('\u{2013}'),
                            "mdash" => Some('\u{2014}'),
                            "lsquo" => Some('\u{2018}'),
                            "rsquo" => Some('\u{2019}'),
                            "ldquo" => Some('\u{201C}'),
                            "rdquo" => Some('\u{201D}'),
                            "hellip" => Some('\u{2026}'),
                            "copy" => Some('\u{00A9}'),
                            "reg" => Some('\u{00AE}'),
                            "trade" => Some('\u{2122}'),
                            "bull" => Some('\u{2022}'),
                            "middot" => Some('\u{00B7}'),
                            "euro" => Some('\u{20AC}'),
                            "pound" => Some('\u{00A3}'),
                            _ => None,
                        }
                    };
                    if let Some(ch) = decoded {
                        out.push(ch);
                        i += semi_rel + 2;
                        continue;
                    }
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out.trim().to_string()
}

fn parse_xmltv_ts(raw: &str) -> i64 {
    let t = raw.trim();
    if t.len() < 12 {
        return 0;
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 12 {
        return 0;
    }
    let n = |a: usize, b: usize| digits.get(a..b).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let sec = if digits.len() >= 14 { n(12, 14) } else { 0 };
    let mut ms = chrono::Utc
        .with_ymd_and_hms(n(0, 4) as i32, n(4, 6) as u32, n(6, 8) as u32, n(8, 10) as u32, n(10, 12) as u32, sec as u32)
        .single()
        .map(|d| d.timestamp_millis())
        .unwrap_or(0);
    let rest = t[digits.len()..].trim();
    if rest.len() >= 5 && (rest.starts_with('+') || rest.starts_with('-')) {
        let sign: i64 = if rest.starts_with('-') { -1 } else { 1 };
        let h: i64 = rest[1..3].parse().unwrap_or(0);
        let m: i64 = rest[3..5].parse().unwrap_or(0);
        ms -= sign * (h * 60 + m) * 60_000;
    }
    ms
}

/// Extract attributes (start, stop, channel) in ANY order from a `<programme ...>` opening tag.
fn parse_programme_attrs<'a>(open_tag: &'a str) -> (Option<&'a str>, Option<&'a str>, Option<&'a str>) {
    let bytes = open_tag.as_bytes();
    let mut i = 0;
    // skip tag name "<programme"
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let (mut start, mut stop, mut channel) = (None, None, None);
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/' || bytes[i] == b'>') {
            i += 1;
        }
        if i >= bytes.len() { break; }
        let name_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
            i += 1;
        }
        let name = &open_tag[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= bytes.len() || bytes[i] != b'=' { continue; }
        i += 1; // skip '='
        while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= bytes.len() { break; }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' { i += 1; }
            continue;
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != quote { i += 1; }
        let val = &open_tag[val_start..i];
        if i < bytes.len() { i += 1; }
        if name.eq_ignore_ascii_case("start") {
            start = Some(val);
        } else if name.eq_ignore_ascii_case("stop") {
            stop = Some(val);
        } else if name.eq_ignore_ascii_case("channel") {
            channel = Some(val);
        }
    }
    (start, stop, channel)
}

fn extract_child_tag<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let needle = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut pos = 0;
    while let Some(rel) = body[pos..].find(&needle) {
        let start = pos + rel;
        let after_name = start + needle.len();
        let next_b = body.as_bytes().get(after_name).copied();
        if matches!(next_b, Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'/')) {
            let gt = body[after_name..].find('>')?;
            if gt > 0 && body.as_bytes()[after_name + gt - 1] == b'/' {
                return Some("");
            }
            let content_start = after_name + gt + 1;
            let end_rel = body[content_start..].find(&close)?;
            return Some(&body[content_start..content_start + end_rel]);
        }
        pos = after_name;
    }
    None
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    let limit = hay.len() - needle.len();
    while i <= limit {
        if hay[i] == first && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// True streaming XMLTV parser (fixed small memory footprint regardless of file size).
pub(crate) fn parse_xmltv_reader<R: BufRead>(
    mut reader: R,
    prefix: &str,
    wanted: &HashMap<&str, bool>,
    lo_short: i64,
    lo_long: i64,
    hi: i64,
    epg: &mut HashMap<String, Vec<Prog>>,
) {
    const OPEN: &[u8] = b"<programme";
    const CLOSE: &[u8] = b"</programme>";
    const MAX_BLOCK: usize = 1024 * 1024;

    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 64 * 1024];
    let mut eof = false;

    loop {
        if !eof {
            match reader.read(&mut chunk) {
                Ok(0) => eof = true,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }

        let mut cursor = 0usize;
        while cursor < buf.len() {
            let Some(rel_open) = find_bytes(&buf[cursor..], OPEN) else {
                // Keep trailing bytes in case "<programme" straddles the next chunk boundary.
                let keep = if eof { 0 } else { (OPEN.len() - 1).min(buf.len() - cursor) };
                cursor = buf.len() - keep;
                break;
            };
            let open_pos = cursor + rel_open;
            let after_open = open_pos + OPEN.len();
            if after_open >= buf.len() {
                if eof { cursor = buf.len(); } else { cursor = open_pos; }
                break;
            }
            let delim = buf[after_open];
            if !matches!(delim, b' ' | b'\t' | b'\r' | b'\n' | b'>') {
                cursor = after_open;
                continue;
            }

            // Locate end of opening tag `>` (accounting for quoted attribute values).
            let mut tag_end = None;
            let mut in_quote: u8 = 0;
            let mut j = after_open;
            while j < buf.len() {
                let b = buf[j];
                if in_quote != 0 {
                    if b == in_quote { in_quote = 0; }
                } else if b == b'"' || b == b'\'' {
                    in_quote = b;
                } else if b == b'>' {
                    tag_end = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(gt_pos) = tag_end else {
                if eof || buf.len() - open_pos > MAX_BLOCK {
                    cursor = buf.len();
                } else {
                    cursor = open_pos;
                }
                break;
            };

            // Self-closing <programme ... />
            if gt_pos > open_pos && buf[gt_pos - 1] == b'/' {
                cursor = gt_pos + 1;
                continue;
            }

            let Some(rel_close) = find_bytes(&buf[gt_pos + 1..], CLOSE) else {
                if eof || buf.len() - open_pos > MAX_BLOCK {
                    cursor = buf.len();
                } else {
                    cursor = open_pos;
                }
                break;
            };
            let close_pos = gt_pos + 1 + rel_close;

            // Parse attributes first so unwanted channels skip body decoding completely.
            let open_str = String::from_utf8_lossy(&buf[open_pos..=gt_pos]);
            let (start_attr, stop_attr, chan_attr) = parse_programme_attrs(&open_str);
            if let (Some(st_s), Some(sp_s), Some(ch_raw)) = (start_attr, stop_attr, chan_attr) {
                let ch_dec = if ch_raw.contains('&') { decode_xml_entities(ch_raw) } else { ch_raw.to_string() };
                let key = if prefix.is_empty() { ch_dec } else { format!("{prefix}{ch_dec}") };
                if let Some(&long) = wanted.get(key.as_str()) {
                    let s = parse_xmltv_ts(st_s);
                    let e = parse_xmltv_ts(sp_s);
                    let lo = if long { lo_long } else { lo_short };
                    if s > 0 && e >= lo && s <= hi {
                        let body = String::from_utf8_lossy(&buf[gt_pos + 1..close_pos]);
                        let title = extract_child_tag(&body, "title").map(decode_xml_entities).unwrap_or_default();
                        let desc: String = extract_child_tag(&body, "desc")
                            .map(decode_xml_entities)
                            .unwrap_or_default()
                            .chars()
                            .take(300)
                            .collect();
                        epg.entry(key).or_default().push(Prog { s, e, title, desc });
                    }
                }
            }

            cursor = close_pos + CLOSE.len();
        }

        if cursor > 0 {
            buf.drain(..cursor);
        }
        if eof {
            break;
        }
    }
}

/// Stream-parse XMLTV files (zero full-file String allocation), keeping 7 days of catch-up
/// history for archive channels and 4 hours for live-only channels.
pub fn load_epg(app: &App) {
    let cat = app.catalog();
    // epg id -> keep 7 days of history (catch-up channels) or 4 hours
    let mut wanted: HashMap<&str, bool> = HashMap::new();
    for x in cat.live.iter().filter(|x| !x.epg.is_empty() && (x.en || x.sport)) {
        let e = wanted.entry(x.epg.as_str()).or_insert(false);
        *e = *e || x.archive > 0;
    }
    let now = now_ms();
    let (lo_short, lo_long, hi) = (now - 4 * 3_600_000, now - 7 * 86_400_000, now + 48 * 3_600_000);
    let mut epg: HashMap<String, Vec<Prog>> = HashMap::new();

    let primary = app.data.join("raw").join("epg.xml");
    if let Ok(f) = std::fs::File::open(&primary) {
        parse_xmltv_reader(BufReader::with_capacity(64 * 1024, f), "", &wanted, lo_short, lo_long, hi, &mut epg);
    }
    for fpath in crate::epgx::files(&app.data) {
        if let Ok(f) = std::fs::File::open(&fpath) {
            parse_xmltv_reader(BufReader::with_capacity(64 * 1024, f), "x:", &wanted, lo_short, lo_long, hi, &mut epg);
        }
    }
    for l in epg.values_mut() {
        l.sort_by_key(|p| p.s);
        l.dedup_by(|a, b| a.s == b.s && a.e == b.e);
    }
    println!("[epg] {} channels (streaming parser)", epg.len());
    *app.epg.write().unwrap() = Arc::new(epg);
    clear_events_cache(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_xmltv_parses_any_attr_order_entities_and_7day_catchup() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap().timestamp_millis();
        let lo_short = now - 4 * 3_600_000;
        let lo_long = now - 7 * 86_400_000;
        let hi = now + 48 * 3_600_000;

        // Reversed attributes + extra catchup-id + single quotes + numeric/named entities + CDATA
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<tv>
  <programme channel="espn.au" catchup-id="99123" stop="20261120140000 +0000" start="20261120120000 +0000">
    <title lang="en">NBA: Don&#039;t Miss &amp; &#8217;Live&#x27; &quot;Finals&quot;</title>
    <desc><![CDATA[Game 7 &mdash; Lakers vs. Celtics &lt;HD&gt;]]></desc>
  </programme>
  <programme catchup-id='77' stop='20261114140000 +0000' channel='bbc1.uk' start='20261114120000 +0000'>
    <title>Match of the Day (6 days ago)</title>
    <desc>Catch-up archive test</desc>
  </programme>
  <programme start="20261114120000 +0000" stop="20261114140000 +0000" channel="espn.au">
    <title>Old non-archive programme</title>
  </programme>
  <programme start="20261120120000 +0000" stop="20261120140000 +0000" channel="unwanted.ch">
    <title>Should be skipped</title>
  </programme>
</tv>"#;

        let mut wanted: HashMap<&str, bool> = HashMap::new();
        wanted.insert("espn.au", false); // live-only (4h retention)
        wanted.insert("bbc1.uk", true);  // archive enabled (7-day retention)

        // Wrap in a tiny 17-byte BufReader to stress chunk-boundary splitting!
        let reader = BufReader::with_capacity(17, std::io::Cursor::new(xml.as_bytes()));
        let mut epg = HashMap::new();
        parse_xmltv_reader(reader, "", &wanted, lo_short, lo_long, hi, &mut epg);

        assert!(!epg.contains_key("unwanted.ch"));
        let espn = epg.get("espn.au").expect("espn.au present");
        assert_eq!(espn.len(), 1, "6-day-old entry must be dropped for non-archive channel");
        assert_eq!(espn[0].title, "NBA: Don't Miss & \u{2019}Live' \"Finals\"");
        assert_eq!(espn[0].desc, "Game 7 \u{2014} Lakers vs. Celtics <HD>");

        let bbc = epg.get("bbc1.uk").expect("bbc1.uk present");
        assert_eq!(bbc.len(), 1, "6-day-old entry must be retained within 7-day catch-up window");
        assert_eq!(bbc[0].title, "Match of the Day (6 days ago)");
    }

    #[test]
    fn zone_for_and_tz_abbr_fix_au_nz_and_us_timezones() {
        assert_eq!(zone_for("NRL: Broncos vs Storm 7:30pm", "AU | SPORTS"), chrono_tz::Australia::Sydney);
        assert_eq!(zone_for("Super Rugby: Blues vs Crusaders 7:05pm", "NZ | SKY SPORT"), chrono_tz::Pacific::Auckland);
        assert_eq!(zone_for("AU | AFL: Swans vs Lions 7:30pm", ""), chrono_tz::Australia::Sydney);
        assert_eq!(zone_for("NZ | Warriors vs Raiders 6pm", ""), chrono_tz::Pacific::Auckland);
        assert_eq!(zone_for("Lakers vs Celtics 8:00pm PST", "US | NBA"), chrono_tz::America::Los_Angeles);
        assert_eq!(zone_for("Bulls vs Heat 7:00pm CST", "US | NBA"), chrono_tz::America::Chicago);
        assert_eq!(zone_for("Nuggets vs Jazz 7:00pm MST", "US | NBA"), chrono_tz::America::Denver);

        assert_eq!(tz_abbr("CST"), chrono_tz::America::Chicago);
        assert_eq!(tz_abbr("CDT"), chrono_tz::America::Chicago);
        assert_eq!(tz_abbr("CT"), chrono_tz::America::Chicago);
        assert_eq!(tz_abbr("PST"), chrono_tz::America::Los_Angeles);
        assert_eq!(tz_abbr("PDT"), chrono_tz::America::Los_Angeles);
        assert_eq!(tz_abbr("MST"), chrono_tz::America::Denver);
        assert_eq!(tz_abbr("MDT"), chrono_tz::America::Denver);
        assert_eq!(tz_abbr("NZDT"), chrono_tz::Pacific::Auckland);
        assert_eq!(tz_abbr("NZST"), chrono_tz::Pacific::Auckland);
        assert_eq!(tz_abbr("AEST"), chrono_tz::Australia::Sydney);
        assert_eq!(tz_abbr("AEDT"), chrono_tz::Australia::Sydney);
        assert_eq!(tz_abbr("EDT"), chrono_tz::America::New_York);
        assert_eq!(tz_abbr("CEST"), chrono_tz::Europe::Paris);
    }

    #[test]
    fn yearless_dates_roll_over_dec_jan_and_support_eu_au_dd_mm() {
        // On Dec 30, 2026: "Jan 2" must resolve to 2027-01-02 (NOT 2026-01-02!)
        let dec30 = chrono::Utc.with_ymd_and_hms(2026, 12, 30, 12, 0, 0).unwrap().timestamp_millis();
        let p_jan = parse_event_at("NFL: Chiefs vs Bills Jan 2 8:00pm ET", "US | NFL", dec30).unwrap();
        let dt_jan = chrono::DateTime::from_timestamp_millis(p_jan.start.unwrap()).unwrap().with_timezone(&ET);
        assert_eq!((dt_jan.year(), dt_jan.month(), dt_jan.day()), (2027, 1, 2));

        // On Jan 2, 2027: "Dec 30" must resolve to 2026-12-30
        let jan2 = chrono::Utc.with_ymd_and_hms(2027, 1, 2, 12, 0, 0).unwrap().timestamp_millis();
        let p_dec = parse_event_at("NFL: Chiefs vs Bills Dec 30 8:00pm ET", "US | NFL", jan2).unwrap();
        let dt_dec = chrono::DateTime::from_timestamp_millis(p_dec.start.unwrap()).unwrap().with_timezone(&ET);
        assert_eq!((dt_dec.year(), dt_dec.month(), dt_dec.day()), (2026, 12, 30));

        // DD.MM in AU/EU categories vs MM.DD in US categories:
        let mid = chrono::Utc.with_ymd_and_hms(2026, 4, 1, 12, 0, 0).unwrap().timestamp_millis();
        let p_au = parse_event_at("NRL: Storm vs Roosters (05.03 7:30pm)", "AU | NRL", mid).unwrap();
        let dt_au = chrono::DateTime::from_timestamp_millis(p_au.start.unwrap()).unwrap().with_timezone(&chrono_tz::Australia::Sydney);
        assert_eq!((dt_au.month(), dt_au.day()), (3, 5), "AU category should treat 05.03 as 5 March");

        let p_us = parse_event_at("NBA: Celtics vs Knicks (05.03 7:30pm)", "US | NBA", mid).unwrap();
        let dt_us = chrono::DateTime::from_timestamp_millis(p_us.start.unwrap()).unwrap().with_timezone(&ET);
        assert_eq!((dt_us.month(), dt_us.day()), (5, 3), "US category should treat 05.03 as May 3");
    }
}
