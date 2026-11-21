// UFC & Sports Replays VOD hub: structured fight/event parser, multi-category deduplication
// with failover alternates, ANZ headliner detection, curated hub, faceted browser, and new-UFC watcher.
use crate::app::App;
use crate::catalog::{Catalog, Item};
use crate::meta::Meta;
use crate::re;
use crate::util::{norm, s, tmdb_img};
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct FightEntry {
    pub sport_key: &'static str,
    pub sport_label: &'static str,
    pub clean_title: String,
    /// UFC: "ppv" | "fn" | "special"; WWE: "ple" | "weekly"; F1: "race"; others: "match"
    pub event_kind: &'static str,
    /// UFC Fight Night sub-brand: "fn" | "espn" | "abc" | "fox" | "vegas" | ""
    pub fn_sub: &'static str,
    pub event_num: Option<u32>,
    pub matchup: String,
    pub fighters: Vec<String>,
    pub date_str: Option<String>,
    pub year: Option<i32>,
    pub sort_ts: i64,
    pub is_anz: bool,
    pub is_prelims: bool,
}

#[derive(Clone, Debug)]
pub struct FightAlt {
    pub id: i64,
    pub ext: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct CanonicalFight {
    pub id: i64,
    pub ext: String,
    pub tmdb: Option<String>,
    pub uhd: bool,
    pub poster: String,
    pub backdrop: String,
    pub cat: String,
    pub added: i64,
    pub entry: FightEntry,
    pub alternates: Vec<FightAlt>,
    pub all_ids: Vec<i64>,
    pub all_tmdbs: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct WatchInfo {
    progress: f64,
    resume: f64,
    duration: f64,
    watched: bool,
    updated_at: i64,
    dismissed: bool,
}

pub fn sport_label(key: &str) -> &'static str {
    match key {
        "ufc" => "UFC",
        "boxing" => "Boxing",
        "f1" => "Formula 1",
        "rugby" => "Rugby",
        "afl" => "AFL",
        "nba" => "Basketball",
        "nfl" => "American Football",
        "football" => "Football",
        "hockey" => "Hockey",
        "baseball" => "Baseball",
        "wwe" => "WWE",
        _ => "Sports",
    }
}

fn is_english_vod_cat(cat_upper: &str) -> bool {
    cat_upper.starts_with("EN -")
        || cat_upper.starts_with("NETFLIX")
        || cat_upper.starts_with("AMAZON")
        || cat_upper.starts_with("DISNEY+")
        || cat_upper.starts_with("APPLE+")
        || cat_upper.starts_with("HBO")
        || cat_upper.starts_with("PARAMOUNT")
        || cat_upper.starts_with("PEACOCK")
        || cat_upper.starts_with("SHOWTIME")
        || cat_upper == "SKY"
        || cat_upper.starts_with("TOP MOVIES")
}

fn is_ufc_event_title(t: &str) -> bool {
    re!(r"(?i)^UFC\s+(?:\d{1,4}\b|Fight\s+Night\b|on\s+(?:ESPN|ABC|FOX|FX|Fuel)\b|Vegas\b|Apex\b|Freedom\b|Countdown\b|Embedded\b|Ultimate\b|:)")
        .is_match(t)
}

fn is_afl_matchup(t: &str) -> bool {
    re!(r"(?i)\b(Western\s+Bulldogs|Melbourne\s+Demons|Adelaide\s+Crows|Geelong\s+Cats|Sydney\s+Swans|Brisbane\s+Lions|Collingwood|Carlton|Essendon|Hawthorn|St\s+Kilda|Fremantle|Port\s+Adelaide|GWS\s+Giants|West\s+Coast\s+Eagles|North\s+Melbourne|Gold\s+Coast\s+Suns|Richmond\s+Tigers)\b")
        .is_match(t)
}

fn is_wwe_weekly(raw: &str) -> bool {
    let weekly = re!(r"(?i)\b(SmackDown|Monday\s+Night\s+Raw|WWE\s+Raw|Raw\s+is\s+War|(?:WWE|WW)\s+NXT|NXT\s+Level\s+Up|Main\s+Event|Superstars|205\s+Live|WWE\s+Speed|AEW\s+Dynamite|AEW\s+(?:Saturday\s+)?Collision|AEW\s+Rampage|TNA\s+iMPACT|Impact\s+Wrestling)\b|\(\d{1,2}/\d{1,2}/\d{4}\)");
    let ple = re!(r"(?i)\b(TakeOver|Stand\s*&\s*Deliver|Stand\s+and\s+Deliver|Battleground|Vengeance\s+Day|Deadline|Halloween\s+Havoc|No\s+Mercy|Heatwave|Great\s+American\s+Bash|Spring\s+Breakin|Roadblock|New\s+Year.?s\s+Evil|WrestleMania|SummerSlam|Royal\s+Rumble|Survivor\s+Series|Money\s+in\s+the\s+Bank|Elimination\s+Chamber|Backlash|Crown\s+Jewel|Clash\s+at|Clash\s+in|King\s+and\s+Queen|Bad\s+Blood|Night\s+of\s+Champions|Hell\s+in\s+a\s+Cell|Extreme\s+Rules|Fastlane|Payback|Bash\s+in\s+Berlin|Saturday\s+Night.?s\s+Main\s+Event)\b");
    weekly.is_match(raw) && !ple.is_match(raw)
}

fn classify_sport(clean_raw: &str, cat_name: &str) -> Option<&'static str> {
    let cu = cat_name.trim().to_uppercase();
    if cu == "EN - UFC" || cu == "SOCCER UFC" || cu == "UFC ARABIA" {
        return Some("ufc");
    }
    if (cu.starts_with("EN -") || cu.starts_with("SOCCER ")) && re!(r"\b(UFC|MMA)\b").is_match(&cu) {
        return Some("ufc");
    }
    if cu == "EN - BOXING" || (cu.starts_with("SOCCER ") && re!(r"\bBOXING\b").is_match(&cu)) {
        return Some("boxing");
    }
    if cu == "EN - WWE" || (cu.starts_with("SOCCER ") && re!(r"\bWWE\b").is_match(&cu)) {
        return Some("wwe");
    }
    if cu.starts_with("SOCCER ") && (cu.contains("FORMULA") || re!(r"\bF1\b").is_match(&cu)) {
        return Some("f1");
    }
    if cu == "SOCCER AFL" {
        return Some("afl");
    }
    if cu == "SOCCER RUGBY" {
        return if is_afl_matchup(clean_raw) { Some("afl") } else { Some("rugby") };
    }
    if cu == "SOCCER BASKETBALL" {
        return Some("nba");
    }
    if cu == "SOCCER AMERICAN FOOTBALL" {
        return Some("nfl");
    }
    if cu == "SOCCER FOOTBALL" {
        return Some("football");
    }
    if cu == "SOCCER HOCKEY" {
        return Some("hockey");
    }
    if cu == "SOCCER BASEBALL" {
        return Some("baseball");
    }
    if (cu.is_empty() || is_english_vod_cat(&cu)) && is_ufc_event_title(clean_raw) {
        return Some("ufc");
    }
    None
}

fn strip_prefix_and_noise(title: &str) -> String {
    let mut t = title.trim().to_string();
    // Strip "UFC - " only when it duplicates a following "UFC" prefix (e.g. UFC ARABIA).
    if re!(r"(?i)^UFC\s*-\s+UFC\b").is_match(&t) {
        t = re!(r"(?i)^UFC\s*-\s+").replace(&t, "").into_owned();
    }
    for _ in 0..2 {
        t = re!(r"(?i)^\s*(?:EN|SOC|NF)\s*-\s*").replace(&t, "").into_owned();
    }
    t = re!(r"(?i)\s*\(Multi[^)]*\)").replace_all(&t, "").into_owned();
    t = re!(r"\[[^\]]*\]").replace_all(&t, "").into_owned();
    t = re!(r"(?i)\s+Ad\s*Free\s*$").replace(&t, "").into_owned();
    t = re!(r"(?i)\s+(HEVC|H\.?265|x265|UHD|HDR10\+?|HDR|4K|FHD|1080p|2160p)\b")
        .replace_all(&t, "")
        .into_owned();
    // Normalize broken apostrophe spacing from provider titles ("O' Malley" -> "O'Malley").
    t = re!(r"\b([A-Za-z])'\s+([A-Za-z])").replace_all(&t, "$1'$2").into_owned();
    re!(r"\s{2,}").replace_all(t.trim(), " ").into_owned()
}

fn date_to_ts(y: i32, m: u32, d: u32) -> Option<i64> {
    NaiveDate::from_ymd_opt(y, m, d)?.and_hms_opt(12, 0, 0).map(|dt| dt.and_utc().timestamp())
}

fn parse_iso_date_ts(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = iso[0..4].parse().ok()?;
    let m: u32 = iso[5..7].parse().ok()?;
    let d: u32 = iso[8..10].parse().ok()?;
    date_to_ts(y, m, d)
}

fn year_from_ts(ts: i64) -> Option<i32> {
    if ts < 946_684_800 {
        return None;
    }
    chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.year())
}

fn added_in_year(added: i64, y: i32) -> bool {
    let lo = date_to_ts(y, 1, 1).unwrap_or(0);
    let hi = date_to_ts(y + 1, 3, 15).unwrap_or(i64::MAX);
    added >= lo && added <= hi
}

fn extract_date_and_year(input: &str) -> (String, Option<String>, Option<i64>, Option<i32>) {
    let mut t = input.to_string();
    let mut date_str: Option<String> = None;
    let mut date_ts: Option<i64> = None;
    let mut year: Option<i32> = None;

    // 1. ISO YYYY-MM-DD
    if let Some(c) = re!(r"\s*\(?(19\d\d|20\d\d)-(\d{2})-(\d{2})\)?").captures(&t) {
        if let (Ok(y), Ok(m), Ok(d)) = (c[1].parse::<i32>(), c[2].parse::<u32>(), c[3].parse::<u32>()) {
            if let Some(ts) = date_to_ts(y, m, d) {
                date_str = Some(format!("{y:04}-{m:02}-{d:02}"));
                date_ts = Some(ts);
                year = Some(y);
                let r = c.get(0).unwrap();
                t = format!("{} {}", &t[..r.start()], &t[r.end()..]);
            }
        }
    }

    // 2. DD.MM.YYYY (provider SOCCER * primary format)
    if date_ts.is_none() {
        if let Some(c) = re!(r"\s*\(?(\d{1,2})\.(\d{1,2})\.(19\d\d|20\d\d)\)?").captures(&t) {
            if let (Ok(a), Ok(b), Ok(y)) = (c[1].parse::<u32>(), c[2].parse::<u32>(), c[3].parse::<i32>()) {
                let (d, m) = if b > 12 && a <= 12 { (b, a) } else { (a, b) };
                if let Some(ts) = date_to_ts(y, m, d) {
                    date_str = Some(format!("{y:04}-{m:02}-{d:02}"));
                    date_ts = Some(ts);
                    year = Some(y);
                    let r = c.get(0).unwrap();
                    t = format!("{} {}", &t[..r.start()], &t[r.end()..]);
                }
            }
        }
    }

    // 3. DD/MM/YYYY or MM/DD/YYYY (e.g. WWE SmackDown (11/07/2025))
    if date_ts.is_none() {
        if let Some(c) = re!(r"\s*\(?(\d{1,2})/(\d{1,2})/(19\d\d|20\d\d)\)?").captures(&t) {
            if let (Ok(a), Ok(b), Ok(y)) = (c[1].parse::<u32>(), c[2].parse::<u32>(), c[3].parse::<i32>()) {
                let (d, m) = if a > 12 && b <= 12 {
                    (a, b)
                } else if b > 12 && a <= 12 {
                    (b, a)
                } else {
                    (a, b)
                };
                if let Some(ts) = date_to_ts(y, m, d) {
                    date_str = Some(format!("{y:04}-{m:02}-{d:02}"));
                    date_ts = Some(ts);
                    year = Some(y);
                    let r = c.get(0).unwrap();
                    t = format!("{} {}", &t[..r.start()], &t[r.end()..]);
                }
            }
        }
    }

    // 4. DD.MM.YY (e.g. SOCCER BASEBALL "06.09.25", SOCCER FORMULA 1 "06.12.20")
    if date_ts.is_none() {
        if let Some(c) = re!(r"\s*\(?(\d{2})\.(\d{2})\.(\d{2})\)?(?:\s*$|\s)").captures(&t) {
            if let (Ok(a), Ok(b), Ok(yy)) = (c[1].parse::<u32>(), c[2].parse::<u32>(), c[3].parse::<i32>()) {
                let (d, m) = if b > 12 && a <= 12 { (b, a) } else { (a, b) };
                let y = 2000 + yy;
                if (2000..=2050).contains(&y) {
                    if let Some(ts) = date_to_ts(y, m, d) {
                        date_str = Some(format!("{y:04}-{m:02}-{d:02}"));
                        date_ts = Some(ts);
                        year = Some(y);
                        let r = c.get(0).unwrap();
                        t = format!("{} {}", &t[..r.start()], &t[r.end()..]);
                    }
                }
            }
        }
    }

    // 5. Parenthesized year (YYYY)
    if let Some(c) = re!(r"\s*\((19\d\d|20\d\d)\)").captures(&t) {
        if year.is_none() {
            year = c[1].parse::<i32>().ok();
        }
        t = re!(r"\s*\((19\d\d|20\d\d)\)").replace_all(&t, "").into_owned();
    }

    let cleaned = re!(r"\s{2,}").replace_all(t.trim(), " ").trim().to_string();
    (cleaned, date_str, date_ts, year)
}

fn clean_fighter_piece(raw: &str) -> String {
    let t = raw.trim().trim_matches(|c: char| matches!(c, ':' | '-' | '–' | '—' | '|' | ','));
    let t = re!(r"\s{2,}").replace_all(t.trim(), " ");
    // Preserve trailing period on Jr. / Sr., otherwise strip stray trailing punctuation.
    let s = t.as_ref();
    if re!(r"(?i)\b(?:Jr|Sr)\.$").is_match(s) {
        s.to_string()
    } else {
        s.trim_end_matches('.').trim().to_string()
    }
}

fn parse_vs_matchup(body: &str) -> Option<(String, Vec<String>)> {
    let sep = re!(r"(?i)\s+(?:vs\.?|v\.)\s+");
    let m = sep.find(body)?;
    let left = clean_fighter_piece(&body[..m.start()]);
    let right_raw = clean_fighter_piece(&body[m.end()..]);
    if left.is_empty() || right_raw.is_empty() {
        return None;
    }
    let (right, rematch) = if let Some(c) = re!(r"^(.+?)\s+([2-9]|II|III|IV)\s*$").captures(&right_raw) {
        (clean_fighter_piece(&c[1]), Some(c[2].to_string()))
    } else {
        (right_raw, None)
    };
    if right.is_empty() {
        return None;
    }
    let matchup = match rematch {
        Some(rm) => format!("{left} vs. {right} {rm}"),
        None => format!("{left} vs. {right}"),
    };
    Some((matchup, vec![left, right]))
}

fn parse_team_matchup(body: &str) -> String {
    if let Some(m) = re!(r"(?i)\s+(?:vs\.?|v\.?)\s+").find(body) {
        let a = body[..m.start()].trim();
        let b = body[m.end()..].trim();
        if !a.is_empty() && !b.is_empty() {
            return format!("{a} vs. {b}");
        }
    }
    if let Some(m) = re!(r"(?i)\s+at\s+").find(body) {
        let a = body[..m.start()].trim();
        let b = body[m.end()..].trim();
        if !a.is_empty() && !b.is_empty() {
            return format!("{a} at {b}");
        }
    }
    if let Some(m) = re!(r"\s+-\s+").find(body) {
        let a = body[..m.start()].trim();
        let b = body[m.end()..].trim();
        if !a.is_empty() && !b.is_empty() {
            return format!("{a} vs. {b}");
        }
    }
    String::new()
}

fn is_anz_ufc(fighters: &[String], title: &str) -> bool {
    let r = re!(r"\b(volkanovski|della maddalena|ulberg|whittaker|adesanya|hooker|kara france|tuivasa|erceg|pedro|crute|tafa|jenkins|matthews|culibao|mullarkey|riddell|volk|jdm)\b");
    fighters.iter().any(|f| r.is_match(&norm(f))) || r.is_match(&norm(title))
}

fn compute_sort_ts(
    sport_key: &str,
    event_kind: &str,
    fn_sub: &str,
    event_num: Option<u32>,
    is_prelims: bool,
    date_ts: Option<i64>,
    eff_year: Option<i32>,
    added: i64,
) -> i64 {
    let prelim_adj = if is_prelims { -3600 } else { 0 };
    if sport_key == "ufc" && event_kind == "ppv" {
        if let Some(n) = event_num {
            // Anchor UFC 300 = 2024-04-13 (1713009600), ~28 days per PPV -> strictly monotonic in PPV number
            // and aligned to real Unix time so PPVs interleave naturally with Fight Nights & other sports.
            return 1_713_009_600i64 + (n as i64 - 300) * 2_419_200i64 + prelim_adj;
        }
    }
    if let Some(ts) = date_ts {
        return ts + prelim_adj;
    }
    if sport_key == "ufc" && event_kind == "fn" {
        if let Some(n) = event_num {
            let est = match fn_sub {
                "fn" => Some(1_759_000_000i64 + (n as i64 - 260) * 1_919_000i64),
                "espn" => Some(1_757_000_000i64 + (n as i64 - 72) * 2_798_000i64),
                "abc" => Some(1_753_500_000i64 + (n as i64 - 9) * 15_552_000i64),
                _ => None,
            };
            if let Some(ts) = est {
                return ts + prelim_adj;
            }
        }
    }
    if let Some(y) = eff_year {
        if added_in_year(added, y) {
            return added + prelim_adj;
        }
        let base = date_to_ts(y, 7, 1).unwrap_or(0);
        let tie = event_num.map(|n| n as i64 * 3600).unwrap_or_else(|| added.abs() % 86_400);
        return base + tie + prelim_adj;
    }
    added + prelim_adj
}

/// Classify and parse a VOD sport/combat replay title into a structured `FightEntry`.
/// Returns `None` for non-sport VOD items.
pub fn parse_fight(title: &str, cat_name: &str, year: Option<i32>, added: i64) -> Option<FightEntry> {
    let raw_clean = strip_prefix_and_noise(title);
    if raw_clean.is_empty() {
        return None;
    }
    let sport_key = classify_sport(&raw_clean, cat_name)?;
    let sport_lbl = sport_label(sport_key);
    let wwe_weekly = sport_key == "wwe" && is_wwe_weekly(&raw_clean);

    let (mut body, date_str_full, date_ts, parsed_year) = extract_date_and_year(&raw_clean);
    let eff_year = parsed_year.or(year).or_else(|| year_from_ts(added));
    let date_str = date_str_full.or_else(|| eff_year.map(|y| y.to_string()));

    let mut event_kind: &'static str = match sport_key {
        "f1" => "race",
        "wwe" => if wwe_weekly { "weekly" } else { "ple" },
        _ => "match",
    };
    let mut fn_sub: &'static str = "";
    let mut event_num: Option<u32> = None;
    let mut matchup = String::new();
    let mut fighters: Vec<String> = vec![];
    let mut is_prelims = false;
    let mut clean_title = body.clone();

    if sport_key == "ufc" {
        if re!(r"(?i)(?:\s*[-–—]\s*|\s*\(\s*|\s+)(?:early\s+)?prelims\)?\s*$").is_match(&body) {
            is_prelims = true;
            body = re!(r"(?i)(?:\s*[-–—]\s*|\s*\(\s*|\s+)(?:early\s+)?prelims\)?\s*$")
                .replace(&body, "")
                .trim()
                .to_string();
        }
        body = re!(r"(?i)(?:\s*[-–—]\s*|\s+)main(?:\s+card)?\s*$")
            .replace(&body, "")
            .trim()
            .to_string();

        let is_special_kw = re!(r"(?i)\b(Countdown|Embedded|Road\s+to\s+UFC|Ultimate\s+Fighter|\bTUF\b|Thrill\s+and\s+the\s+Agony|25\s+Years|Best\s+of|Greatest|Unfiltered|All\s+Access|Knockouts)\b")
            .is_match(&body);

        let mut remainder = String::new();
        if is_special_kw {
            event_kind = "special";
        } else if let Some(c) = re!(r"(?i)^UFC\s+Fight\s+Night(?:\s+(\d{1,4}))?(?:\s*[:\-–—]\s*|\s+|$)(.*)$").captures(&body) {
            event_kind = "fn";
            fn_sub = "fn";
            event_num = c.get(1).and_then(|m| m.as_str().parse().ok());
            remainder = c[2].trim().to_string();
        } else if let Some(c) = re!(r"(?i)^UFC\s+on\s+ESPN\+?(?:\s+(\d{1,4}))?(?:\s*[:\-–—]\s*|\s+|$)(.*)$").captures(&body) {
            event_kind = "fn";
            fn_sub = "espn";
            event_num = c.get(1).and_then(|m| m.as_str().parse().ok());
            remainder = c[2].trim().to_string();
        } else if let Some(c) = re!(r"(?i)^UFC\s+on\s+ABC(?:\s+(\d{1,4}))?(?:\s*[:\-–—]\s*|\s+|$)(.*)$").captures(&body) {
            event_kind = "fn";
            fn_sub = "abc";
            event_num = c.get(1).and_then(|m| m.as_str().parse().ok());
            remainder = c[2].trim().to_string();
        } else if let Some(c) = re!(r"(?i)^UFC\s+on\s+FOX(?:\s+(\d{1,4}))?(?:\s*[:\-–—]\s*|\s+|$)(.*)$").captures(&body) {
            event_kind = "fn";
            fn_sub = "fox";
            event_num = c.get(1).and_then(|m| m.as_str().parse().ok());
            remainder = c[2].trim().to_string();
        } else if let Some(c) = re!(r"(?i)^UFC\s+(?:Vegas|Apex)(?:\s+(\d{1,4}))?(?:\s*[:\-–—]\s*|\s+|$)(.*)$").captures(&body) {
            event_kind = "fn";
            fn_sub = "vegas";
            event_num = c.get(1).and_then(|m| m.as_str().parse().ok());
            remainder = c[2].trim().to_string();
        } else if let Some(c) = re!(r"(?i)^UFC\s+(\d{1,4})(?:\s*[:\-–—]\s*|\s+|$)(.*)$").captures(&body) {
            event_kind = "ppv";
            event_num = c[1].parse().ok();
            remainder = c[2].trim().to_string();
        } else {
            event_kind = "special";
            remainder = re!(r"(?i)^UFC\s*[:\-–—]?\s*").replace(&body, "").trim().to_string();
        }

        if let Some((mu, fs)) = parse_vs_matchup(&remainder) {
            matchup = mu;
            fighters = fs;
        } else if !remainder.is_empty() && event_kind != "special" {
            matchup = remainder.clone();
        }

        let prefix = match (event_kind, fn_sub, event_num) {
            ("ppv", _, Some(n)) => format!("UFC {n}"),
            ("fn", "fn", Some(n)) => format!("UFC Fight Night {n}"),
            ("fn", "fn", None) => "UFC Fight Night".to_string(),
            ("fn", "espn", Some(n)) => format!("UFC on ESPN {n}"),
            ("fn", "espn", None) => "UFC on ESPN".to_string(),
            ("fn", "abc", Some(n)) => format!("UFC on ABC {n}"),
            ("fn", "abc", None) => "UFC on ABC".to_string(),
            ("fn", "fox", Some(n)) => format!("UFC on FOX {n}"),
            ("fn", "fox", None) => "UFC on FOX".to_string(),
            ("fn", "vegas", Some(n)) => format!("UFC Vegas {n}"),
            ("fn", "vegas", None) => "UFC Vegas".to_string(),
            _ => String::new(),
        };
        if !prefix.is_empty() {
            clean_title = if matchup.is_empty() {
                prefix
            } else {
                format!("{prefix}: {matchup}")
            };
        } else {
            clean_title = body.clone();
        }
        if is_prelims {
            clean_title.push_str(" (Prelims)");
        }
    } else if sport_key == "boxing" {
        if let Some((mu, fs)) = parse_vs_matchup(&body) {
            matchup = mu.clone();
            fighters = fs;
            clean_title = mu;
        }
    } else if matches!(sport_key, "rugby" | "afl" | "nba" | "nfl" | "football" | "hockey" | "baseball") {
        matchup = parse_team_matchup(&body);
    }

    let is_anz = sport_key == "ufc" && is_anz_ufc(&fighters, &clean_title);
    let sort_ts = compute_sort_ts(
        sport_key,
        event_kind,
        fn_sub,
        event_num,
        is_prelims,
        date_ts,
        eff_year,
        added,
    );

    Some(FightEntry {
        sport_key,
        sport_label: sport_lbl,
        clean_title,
        event_kind,
        fn_sub,
        event_num,
        matchup,
        fighters,
        date_str,
        year: eff_year,
        sort_ts,
        is_anz,
        is_prelims,
    })
}

fn source_rank(cat_name: &str) -> u8 {
    let u = cat_name.trim().to_uppercase();
    if u == "EN - UFC" || u == "EN - BOXING" || u == "EN - WWE" {
        return 4;
    }
    if u.starts_with("SOCCER ") {
        return 3;
    }
    if u.contains("NETFLIX") || is_english_vod_cat(&u) {
        return 2;
    }
    if u.contains("UFC ARABIA") {
        return 1;
    }
    0
}

fn has_non_ascii(s: &str) -> bool {
    !s.is_ascii()
}

/// Normalize a fighter name to a canonical surname/key for deduplication & topFighters grouping.
fn fighter_canon_key(name: &str) -> String {
    let n = norm(name);
    let n = n.strip_prefix("the ").unwrap_or(&n);
    // Preserve known multi-word surnames & monikers intact.
    if re!(r"^(della maddalena|du plessis|de ridder|dos anjos|saint denis|bueno silva|machado garry|korean zombie|kara france|rountree jr|rosas jr|van \w+|da silva|de oliveira|de andrade|del valle|o malley|roy jones jr)$")
        .is_match(n)
    {
        return n.to_string();
    }
    // Strip trailing "jr"/"sr" when counting tokens, handle "Sean O'Malley" -> "o malley",
    // "Aljamain Sterling" -> "sterling", "Cory Sandhagen" -> "sandhagen".
    if let Some(m) = re!(r"\b(della maddalena|du plessis|de ridder|dos anjos|saint denis|bueno silva|machado garry|korean zombie|kara france|o malley)$").find(n) {
        return m.as_str().to_string();
    }
    let toks: Vec<&str> = n.split_whitespace().collect();
    if toks.len() == 2 && !matches!(toks[1], "jr" | "sr") {
        // Two-word full name in UFC (e.g. "Aljamain Sterling", "Umar Nurmagomedov") -> surname.
        toks[1].to_string()
    } else {
        n.to_string()
    }
}

fn dedup_keys(id: i64, e: &FightEntry) -> (String, Option<String>) {
    let prelim = if e.is_prelims { ":prelim" } else { "" };
    if e.sport_key == "ufc" {
        if e.event_kind == "ppv" {
            if let Some(n) = e.event_num {
                return (format!("ufc:ppv:{n}{prelim}"), None);
            }
        }
        if e.event_kind == "fn" {
            let num_key = e
                .event_num
                .filter(|_| !e.fn_sub.is_empty())
                .map(|n| format!("ufc:fnnum:{}:{n}{prelim}", e.fn_sub));
            if e.fighters.len() == 2 {
                let mut pair = [fighter_canon_key(&e.fighters[0]), fighter_canon_key(&e.fighters[1])];
                pair.sort();
                let yr = e.year.unwrap_or(0);
                return (format!("ufc:fn:{}|{}:{yr}{prelim}", pair[0], pair[1]), num_key);
            }
            if let Some(nk) = num_key {
                return (nk, None);
            }
        }
        return (format!("ufc:sp:{}:{}", norm(&e.clean_title), e.year.unwrap_or(0)), None);
    }
    if e.sport_key == "boxing" && e.fighters.len() == 2 {
        let mut pair = [norm(&e.fighters[0]), norm(&e.fighters[1])];
        pair.sort();
        return (format!("boxing:{}|{}:{}", pair[0], pair[1], e.year.unwrap_or(0)), None);
    }
    // Single-category sports (WWE weekly episodes, MLB doubleheaders, etc.) keep unique stream IDs.
    (format!("id:{id}"), None)
}

fn alt_label(cat_name: &str, primary_cat: &str, existing: &[FightAlt]) -> String {
    let base = if cat_name.trim().is_empty() { "Alternate" } else { cat_name.trim() };
    let used_count = usize::from(primary_cat.eq_ignore_ascii_case(base))
        + existing.iter().filter(|a| a.label.eq_ignore_ascii_case(base) || a.label.starts_with(&format!("{base} (Alt"))).count();
    if used_count == 0 {
        base.to_string()
    } else if used_count == 1 {
        format!("{base} (Alt)")
    } else {
        format!("{base} (Alt {used_count})")
    }
}

fn norm_poster(url: &str) -> String {
    url.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/")
}

/// Collect, classify, and deduplicate all fight & sports VOD streams in the catalog.
pub fn collect_fights(cat: &Catalog, meta: Option<&HashMap<String, Meta>>) -> Vec<CanonicalFight> {
    let mut raw: Vec<(&Item, String, FightEntry)> = Vec::new();
    for it in cat.movie_raw.values() {
        if it.k != "m" {
            continue;
        }
        let cat_name = cat.cat_name("movie", &it.cat);
        if let Some(entry) = parse_fight(&it.title, &cat_name, it.year, it.added) {
            raw.push((it, cat_name, entry));
        }
    }

    // Sort descending by source priority so the best copy is always elected primary first.
    raw.sort_by(|a, b| {
        let ka = (
            source_rank(&a.1),
            !a.0.poster.is_empty(),
            a.0.tmdb.is_some(),
            a.2.event_num.is_some(),
            has_non_ascii(&a.2.clean_title),
            a.0.added,
            a.0.id,
        );
        let kb = (
            source_rank(&b.1),
            !b.0.poster.is_empty(),
            b.0.tmdb.is_some(),
            b.2.event_num.is_some(),
            has_non_ascii(&b.2.clean_title),
            b.0.added,
            b.0.id,
        );
        kb.cmp(&ka)
    });

    let mut out: Vec<CanonicalFight> = Vec::new();
    let mut key_map: HashMap<String, usize> = HashMap::new();

    for (it, cat_name, entry) in raw {
        let (k1, k2) = dedup_keys(it.id, &entry);
        let existing_idx = key_map
            .get(&k1)
            .copied()
            .or_else(|| k2.as_ref().and_then(|k| key_map.get(k).copied()));

        if let Some(idx) = existing_idx {
            let cf = &mut out[idx];
            if !cf.all_ids.contains(&it.id) {
                cf.all_ids.push(it.id);
                let lbl = alt_label(&cat_name, &cf.cat, &cf.alternates);
                cf.alternates.push(FightAlt {
                    id: it.id,
                    ext: it.ext.clone(),
                    label: lbl,
                });
            }
            if let Some(tm) = &it.tmdb {
                if !cf.all_tmdbs.contains(tm) {
                    cf.all_tmdbs.push(tm.clone());
                }
                if cf.tmdb.is_none() {
                    cf.tmdb = Some(tm.clone());
                }
            }
            if cf.poster.is_empty() && !it.poster.is_empty() {
                cf.poster = norm_poster(&it.poster);
            }
            if cf.backdrop.is_empty() && !it.backdrop.is_empty() {
                cf.backdrop = it.backdrop.clone();
            }
            cf.uhd = cf.uhd || it.uhd;
            cf.added = cf.added.max(it.added);

            // Upgrade date_str if the alternate carries an exact YYYY-MM-DD calendar date.
            if let Some(alt_d) = &entry.date_str {
                let primary_has_full = cf.entry.date_str.as_deref().map_or(false, |d| d.len() == 10);
                if alt_d.len() == 10 && !primary_has_full {
                    cf.entry.date_str = Some(alt_d.clone());
                    if cf.entry.year.is_none() {
                        cf.entry.year = entry.year;
                    }
                    if cf.entry.event_kind != "ppv" {
                        if let Some(ts) = parse_iso_date_ts(alt_d) {
                            cf.entry.sort_ts = ts - if cf.entry.is_prelims { 3600 } else { 0 };
                        }
                    }
                }
            }
            if cf.entry.event_num.is_none() && entry.event_num.is_some() {
                cf.entry.event_num = entry.event_num;
                if cf.entry.fn_sub.is_empty() {
                    cf.entry.fn_sub = entry.fn_sub;
                }
                if !entry.clean_title.is_empty() {
                    cf.entry.clean_title = entry.clean_title.clone();
                }
            }
            if !has_non_ascii(&cf.entry.matchup) && has_non_ascii(&entry.matchup) {
                cf.entry.matchup = entry.matchup.clone();
                cf.entry.fighters = entry.fighters.clone();
            }
            cf.entry.is_anz = cf.entry.is_anz || entry.is_anz;

            key_map.entry(k1).or_insert(idx);
            if let Some(k2) = k2 {
                key_map.entry(k2).or_insert(idx);
            }
        } else {
            let idx = out.len();
            let all_tmdbs = it.tmdb.iter().cloned().collect();
            out.push(CanonicalFight {
                id: it.id,
                ext: it.ext.clone(),
                tmdb: it.tmdb.clone(),
                uhd: it.uhd,
                poster: norm_poster(&it.poster),
                backdrop: it.backdrop.clone(),
                cat: cat_name,
                added: it.added,
                entry,
                alternates: vec![],
                all_ids: vec![it.id],
                all_tmdbs,
            });
            key_map.insert(k1, idx);
            if let Some(k2) = k2 {
                key_map.insert(k2, idx);
            }
        }
    }

    // Enrich poster, backdrop, and exact release date from local TMDB metadata cache (zero network).
    if let Some(meta_map) = meta {
        for cf in &mut out {
            let m = cf
                .all_tmdbs
                .iter()
                .find_map(|t| meta_map.get(&format!("movie:{t}")).filter(|m| !m.missing));
            if let Some(m) = m {
                if cf.poster.is_empty() && !m.pp.is_empty() {
                    cf.poster = tmdb_img(&m.pp, "w342");
                }
                if cf.backdrop.is_empty() && !m.bd.is_empty() {
                    cf.backdrop = tmdb_img(&m.bd, "w1280");
                }
                let primary_has_full = cf.entry.date_str.as_deref().map_or(false, |d| d.len() == 10);
                if !primary_has_full && m.d.len() == 10 {
                    if let Some(ts) = parse_iso_date_ts(&m.d) {
                        cf.entry.date_str = Some(m.d.clone());
                        if cf.entry.year.is_none() {
                            cf.entry.year = m.d.get(0..4).and_then(|y| y.parse().ok());
                        }
                        if cf.entry.event_kind != "ppv" {
                            cf.entry.sort_ts = ts - if cf.entry.is_prelims { 3600 } else { 0 };
                        }
                    }
                }
            }
        }
    }

    out
}

struct StateSnap {
    progress: HashMap<i64, (f64, f64, i64)>, // stream_id -> (pos, dur, t)
    watched_keys: HashSet<String>,           // "movie:<tmdb>" or "m:<id>"
    dismissed: HashSet<i64>,                 // stream_id from cwDismissed["m:<id>"]
}

fn snapshot_state(app: &App) -> StateSnap {
    app.state.read(|st| {
        let mut progress = HashMap::new();
        for (k, v) in st["progress"].as_object().into_iter().flatten() {
            if let Some(id_str) = k.strip_prefix("m:") {
                if let Ok(id) = id_str.parse::<i64>() {
                    let pos = v["pos"].as_f64().unwrap_or(0.0).max(0.0);
                    let dur = v["dur"].as_f64().unwrap_or(0.0).max(0.0);
                    let t = v["t"].as_i64().unwrap_or(0);
                    progress.insert(id, (pos, dur, t));
                }
            }
        }
        let mut watched_keys: HashSet<String> = st["watched"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        for h in st["history"].as_array().into_iter().flatten() {
            if h["kind"] == "m" {
                let tm = s(&h["tmdb"]);
                if !tm.is_empty() {
                    watched_keys.insert(format!("movie:{tm}"));
                }
            }
        }
        let mut dismissed = HashSet::new();
        for k in st["cwDismissed"].as_object().into_iter().flat_map(|m| m.keys()) {
            if let Some(id) = k.strip_prefix("m:").and_then(|x| x.parse::<i64>().ok()) {
                dismissed.insert(id);
            }
        }
        StateSnap { progress, watched_keys, dismissed }
    })
}

fn watch_info_for(cf: &CanonicalFight, snap: &StateSnap) -> WatchInfo {
    let mut best: Option<(f64, f64, i64)> = None;
    let mut dismissed = false;
    let mut watched = cf.all_tmdbs.iter().any(|t| snap.watched_keys.contains(&format!("movie:{t}")));

    for &id in &cf.all_ids {
        if snap.watched_keys.contains(&format!("m:{id}")) {
            watched = true;
        }
        if snap.dismissed.contains(&id) {
            dismissed = true;
        }
        if let Some(&(pos, dur, t)) = snap.progress.get(&id) {
            let take = match best {
                None => true,
                Some((_, _, bt)) => t > bt,
            };
            if take {
                best = Some((pos, dur, t));
            }
        }
    }

    let (resume, duration, updated_at) = best.unwrap_or((0.0, 0.0, 0));
    let ratio = if duration > 0.0 { (resume / duration).clamp(0.0, 1.0) } else { 0.0 };
    if ratio >= 0.92 {
        watched = true;
    }
    WatchInfo {
        progress: (ratio * 10000.0).round() / 10000.0,
        resume,
        duration,
        watched,
        updated_at,
        dismissed,
    }
}

fn fight_card(cf: &CanonicalFight, wi: WatchInfo) -> Value {
    let alts: Vec<Value> = cf
        .alternates
        .iter()
        .map(|a| json!({ "id": a.id, "ext": a.ext, "label": a.label }))
        .collect();
    json!({
        "k": "m",
        "id": cf.id,
        "ext": cf.ext,
        "tmdb": cf.tmdb,
        "uhd": cf.uhd,
        "title": cf.entry.clean_title,
        "cleanTitle": cf.entry.clean_title,
        "matchup": cf.entry.matchup,
        "eventNum": cf.entry.event_num,
        "eventKind": cf.entry.event_kind,
        "fighters": cf.entry.fighters,
        "dateStr": cf.entry.date_str,
        "year": cf.entry.year,
        "sportKey": cf.entry.sport_key,
        "sportLabel": cf.entry.sport_label,
        "cat": cf.cat,
        "poster": cf.poster,
        "backdrop": cf.backdrop,
        "isAnz": cf.entry.is_anz,
        "added": cf.added,
        "progress": wi.progress,
        "resume": wi.resume,
        "duration": wi.duration,
        "watched": wi.watched,
        "variantCount": 1 + cf.alternates.len(),
        "alternates": alts
    })
}

fn sport_counts_json(items: &[CanonicalFight]) -> Value {
    let (mut ufc, mut ppv, mut fn_cnt, mut anz) = (0usize, 0usize, 0usize, 0usize);
    let (mut boxing, mut f1, mut rugby, mut afl) = (0usize, 0usize, 0usize, 0usize);
    let (mut nba, mut nfl, mut football, mut hockey, mut baseball, mut wwe) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for cf in items {
        match cf.entry.sport_key {
            "ufc" => {
                ufc += 1;
                if cf.entry.event_kind == "ppv" {
                    ppv += 1;
                } else if cf.entry.event_kind == "fn" {
                    fn_cnt += 1;
                }
                if cf.entry.is_anz {
                    anz += 1;
                }
            }
            "boxing" => boxing += 1,
            "f1" => f1 += 1,
            "rugby" => rugby += 1,
            "afl" => afl += 1,
            "nba" => nba += 1,
            "nfl" => nfl += 1,
            "football" => football += 1,
            "hockey" => hockey += 1,
            "baseball" => baseball += 1,
            "wwe" => wwe += 1,
            _ => {}
        }
    }
    json!({
        "ufc": ufc,
        "ppv": ppv,
        "fn": fn_cnt,
        "anz": anz,
        "boxing": boxing,
        "f1": f1,
        "rugby": rugby,
        "afl": afl,
        "nba": nba,
        "nfl": nfl,
        "football": football,
        "hockey": hockey,
        "baseball": baseball,
        "wwe": wwe,
        "total": items.len()
    })
}

fn top_fighters_list(items: &[&CanonicalFight], limit: usize) -> Vec<Value> {
    // key -> (count, preferred display name)
    let mut map: HashMap<String, (usize, String)> = HashMap::new();
    for cf in items {
        if cf.entry.sport_key != "ufc" && cf.entry.sport_key != "boxing" {
            continue;
        }
        let mut seen_in_fight: HashSet<String> = HashSet::new();
        for f in &cf.entry.fighters {
            let key = if cf.entry.sport_key == "ufc" {
                fighter_canon_key(f)
            } else {
                norm(f)
            };
            if key.is_empty() || !seen_in_fight.insert(key.clone()) {
                continue;
            }
            // Prefer shorter surname form when UFC has both "Sean O'Malley" and "O'Malley",
            // and prefer accented spelling ("Procházka" over "Prochazka").
            let display = if cf.entry.sport_key == "ufc" {
                let toks: Vec<&str> = f.split_whitespace().collect();
                if toks.len() == 2 && !matches!(toks[1].to_ascii_lowercase().as_str(), "jr" | "jr." | "sr" | "sr.")
                    && !re!(r"(?i)^(della|du|de|dos|saint|bueno|machado|korean|kara|van|da|del)$").is_match(toks[0])
                {
                    toks[1].to_string()
                } else {
                    f.clone()
                }
            } else {
                f.clone()
            };
            map.entry(key)
                .and_modify(|(cnt, best)| {
                    *cnt += 1;
                    if (!has_non_ascii(best) && has_non_ascii(&display))
                        || (has_non_ascii(best) == has_non_ascii(&display) && display.len() < best.len())
                    {
                        *best = display.clone();
                    }
                })
                .or_insert((1, display));
        }
    }
    let mut list: Vec<(String, usize)> = map.into_values().map(|(c, n)| (n, c)).collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    list.into_iter()
        .take(limit)
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect()
}

fn cmp_latest(a: &CanonicalFight, b: &CanonicalFight) -> std::cmp::Ordering {
    b.entry
        .sort_ts
        .cmp(&a.entry.sort_ts)
        .then(b.entry.event_num.cmp(&a.entry.event_num))
        .then(b.added.cmp(&a.added))
        .then(b.id.cmp(&a.id))
}

fn push_row(rows: &mut Vec<Value>, id: &str, title: &str, sub: Option<&str>, items: Vec<Value>) {
    if items.is_empty() {
        return;
    }
    let mut r = json!({ "id": id, "title": title, "items": items });
    if let Some(sub) = sub {
        r["sub"] = json!(sub);
    }
    rows.push(r);
}

/// Curated UFC & Sports Replays hub: hero PPV + non-empty sport rows + counts + top fighters + years.
pub fn hub(app: &App) -> Value {
    let cat = app.catalog();
    let pool = {
        let meta = app.meta.read().unwrap();
        collect_fights(&cat, Some(&meta))
    };
    let snap = snapshot_state(app);

    let counts = sport_counts_json(&pool);
    let mut ufc_refs: Vec<&CanonicalFight> = pool.iter().filter(|c| c.entry.sport_key == "ufc").collect();
    ufc_refs.sort_by(|a, b| cmp_latest(a, b));
    let top_fighters = top_fighters_list(&ufc_refs, 24);

    let mut year_set: HashSet<i32> = HashSet::new();
    for cf in &pool {
        if let Some(y) = cf.entry.year {
            if (1980..=2100).contains(&y) {
                year_set.insert(y);
            }
        }
    }
    let mut years: Vec<i32> = year_set.into_iter().collect();
    years.sort_by(|a, b| b.cmp(a));

    // Hero: latest numbered UFC PPV (fallback to latest UFC event).
    let mut ppvs: Vec<&CanonicalFight> = pool
        .iter()
        .filter(|c| c.entry.sport_key == "ufc" && c.entry.event_kind == "ppv" && !c.entry.is_prelims)
        .collect();
    ppvs.sort_by(|a, b| {
        b.entry
            .event_num
            .cmp(&a.entry.event_num)
            .then(cmp_latest(a, b))
    });

    let hero = ppvs
        .first()
        .copied()
        .or_else(|| ufc_refs.first().copied())
        .map(|cf| {
            let wi = watch_info_for(cf, &snap);
            let mut h = fight_card(cf, wi);
            if s(&h["backdrop"]).is_empty() && !cf.poster.is_empty() {
                h["backdrop"] = json!(cf.poster.replace("/t/p/w342/", "/t/p/w780/"));
            }
            let mut variants = vec![json!({
                "id": cf.id,
                "ext": cf.ext,
                "label": cf.cat,
                "primary": true
            })];
            for a in &cf.alternates {
                variants.push(json!({
                    "id": a.id,
                    "ext": a.ext,
                    "label": a.label,
                    "primary": false
                }));
            }
            h["variants"] = json!(variants);
            h
        })
        .unwrap_or(Value::Null);

    let mut rows: Vec<Value> = Vec::new();

    // 1. Continue Watching (in-progress fight/sports replays: 0.02 < pos/dur < 0.92)
    let mut cont: Vec<(&CanonicalFight, WatchInfo)> = pool
        .iter()
        .filter_map(|cf| {
            let wi = watch_info_for(cf, &snap);
            if !wi.dismissed && wi.progress > 0.02 && wi.progress < 0.92 {
                Some((cf, wi))
            } else {
                None
            }
        })
        .collect();
    cont.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
    let cont_cards: Vec<Value> = cont
        .into_iter()
        .take(20)
        .map(|(cf, wi)| fight_card(cf, wi))
        .collect();
    push_row(&mut rows, "continue", "Continue Watching", Some("Pick up right where you left off"), cont_cards);

    // 2. UFC Numbered PPVs (sorted newest PPV number first: 331, 329, 328, 327, 326, 325, 324...)
    let ppv_cards: Vec<Value> = ppvs
        .iter()
        .take(30)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "ufc-ppv", "UFC Numbered PPVs", Some("Main cards, newest PPV first"), ppv_cards);

    // 3. UFC Fight Nights & ESPN Cards
    let mut fns: Vec<&CanonicalFight> = pool
        .iter()
        .filter(|c| c.entry.sport_key == "ufc" && c.entry.event_kind == "fn")
        .collect();
    fns.sort_by(|a, b| cmp_latest(a, b));
    let fn_cards: Vec<Value> = fns
        .into_iter()
        .take(30)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "ufc-fn", "UFC Fight Nights & ESPN Cards", Some("Fight Nights, ESPN & ABC cards"), fn_cards);

    // 4. Aussie & Kiwi Headliners
    let mut anz_list: Vec<&CanonicalFight> = pool
        .iter()
        .filter(|c| c.entry.sport_key == "ufc" && c.entry.is_anz)
        .collect();
    anz_list.sort_by(|a, b| cmp_latest(a, b));
    let anz_cards: Vec<Value> = anz_list
        .into_iter()
        .take(24)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "ufc-anz", "Aussie & Kiwi Headliners", Some("ANZ fighters in the Octagon"), anz_cards);

    // 5. Championship Boxing
    let mut boxing_list: Vec<&CanonicalFight> = pool.iter().filter(|c| c.entry.sport_key == "boxing").collect();
    boxing_list.sort_by(|a, b| cmp_latest(a, b));
    let boxing_cards: Vec<Value> = boxing_list
        .into_iter()
        .take(24)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "boxing", "Championship Boxing", None, boxing_cards);

    // 6. Formula 1 Replays
    let mut f1_list: Vec<&CanonicalFight> = pool.iter().filter(|c| c.entry.sport_key == "f1").collect();
    f1_list.sort_by(|a, b| cmp_latest(a, b));
    let f1_cards: Vec<Value> = f1_list
        .into_iter()
        .take(24)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "f1", "Formula 1 Replays", Some("Grand Prix races, sprints & qualifying"), f1_cards);

    // 7. Rugby & AFL Replays
    let mut rugby_afl: Vec<&CanonicalFight> = pool
        .iter()
        .filter(|c| c.entry.sport_key == "rugby" || c.entry.sport_key == "afl")
        .collect();
    rugby_afl.sort_by(|a, b| cmp_latest(a, b));
    let rugby_afl_cards: Vec<Value> = rugby_afl
        .into_iter()
        .take(24)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "rugby-afl", "Rugby & AFL Replays", Some("NRL, Super Rugby, Rugby Union & AFL"), rugby_afl_cards);

    // 8. WWE Premium Live Events (top 24 newest, excluding weekly Raw/SmackDown/NXT filler unless needed)
    let mut wwe_ples: Vec<&CanonicalFight> = pool
        .iter()
        .filter(|c| c.entry.sport_key == "wwe" && c.entry.event_kind == "ple")
        .collect();
    if wwe_ples.is_empty() {
        wwe_ples = pool.iter().filter(|c| c.entry.sport_key == "wwe").collect();
    }
    wwe_ples.sort_by(|a, b| cmp_latest(a, b));
    let wwe_cards: Vec<Value> = wwe_ples
        .into_iter()
        .take(24)
        .map(|cf| fight_card(cf, watch_info_for(cf, &snap)))
        .collect();
    push_row(&mut rows, "wwe", "WWE Premium Live Events", Some("WrestleMania, SummerSlam, Royal Rumble & more"), wwe_cards);

    json!({
        "hero": hero,
        "rows": rows,
        "counts": counts,
        "topFighters": top_fighters,
        "years": years
    })
}

fn matches_sport_filter(cf: &CanonicalFight, sport: &str) -> bool {
    match sport {
        "" | "ufc" => cf.entry.sport_key == "ufc",
        "ppv" | "ufc-ppv" => cf.entry.sport_key == "ufc" && cf.entry.event_kind == "ppv",
        "fn" | "ufc-fn" => cf.entry.sport_key == "ufc" && cf.entry.event_kind == "fn",
        "anz" | "ufc-anz" => cf.entry.sport_key == "ufc" && cf.entry.is_anz,
        "rugby-afl" => cf.entry.sport_key == "rugby" || cf.entry.sport_key == "afl",
        "boxing" => cf.entry.sport_key == "boxing",
        "f1" | "formula1" | "formula-1" => cf.entry.sport_key == "f1",
        "rugby" => cf.entry.sport_key == "rugby",
        "afl" => cf.entry.sport_key == "afl",
        "nba" | "basketball" => cf.entry.sport_key == "nba",
        "nfl" | "american-football" => cf.entry.sport_key == "nfl",
        "football" | "soccer" => cf.entry.sport_key == "football",
        "hockey" | "nhl" => cf.entry.sport_key == "hockey",
        "baseball" | "mlb" => cf.entry.sport_key == "baseball",
        "wwe" => cf.entry.sport_key == "wwe",
        "wwe-ple" => cf.entry.sport_key == "wwe" && cf.entry.event_kind == "ple",
        "all" => true,
        other => cf.entry.sport_key == other,
    }
}

/// Accent-insensitive fighter matching (e.g. "prochazka" matches "Procházka", "omalley" matches "O'Malley").
pub fn matches_fighter_query(entry: &FightEntry, query: &str) -> bool {
    let needle = norm(query);
    if needle.is_empty() {
        return true;
    }
    let expanded = match needle.as_str() {
        "jdm" => "della maddalena",
        "volk" => "volkanovski",
        other => other,
    };
    let compact = expanded.replace(' ', "");
    let check = |hay: &str| -> bool {
        let h = norm(hay);
        if h.contains(expanded) {
            return true;
        }
        !compact.is_empty() && h.replace(' ', "").contains(&compact)
    };
    entry.fighters.iter().any(|f| check(f))
        || check(&entry.matchup)
        || check(&entry.clean_title)
}

fn matches_text_query(cf: &CanonicalFight, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut hay = String::with_capacity(192);
    hay.push_str(&cf.entry.clean_title);
    hay.push(' ');
    hay.push_str(&cf.entry.matchup);
    hay.push(' ');
    for f in &cf.entry.fighters {
        hay.push_str(f);
        hay.push(' ');
    }
    if let Some(n) = cf.entry.event_num {
        hay.push_str(&n.to_string());
        hay.push(' ');
    }
    if let Some(y) = cf.entry.year {
        hay.push_str(&y.to_string());
        hay.push(' ');
    }
    if let Some(d) = &cf.entry.date_str {
        hay.push_str(d);
        hay.push(' ');
    }
    hay.push_str(cf.entry.sport_key);
    hay.push(' ');
    hay.push_str(cf.entry.sport_label);
    hay.push(' ');
    hay.push_str(&cf.cat);
    if cf.entry.is_anz {
        hay.push_str(" anz aussie kiwi australia");
    }
    let h = norm(&hay);
    let h_compact = h.replace(' ', "");
    terms.iter().all(|w| {
        let target = match w.as_str() {
            "jdm" => "della maddalena",
            "volk" => "volkanovski",
            other => other,
        };
        h.contains(target) || h_compact.contains(&target.replace(' ', ""))
    })
}

/// Faceted fight & sports replays browser with filtering, accent-insensitive search, sorting, and pagination.
pub fn browse(app: &App, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).map(|x| x.trim()).unwrap_or("");
    let sport = {
        let s = get("sport").to_lowercase();
        if s.is_empty() { "ufc".to_string() } else { s }
    };
    let fighter = get("fighter");
    let year_filter = get("year").parse::<i32>().ok().filter(|&y| y > 0);
    let q_str = get("q");
    let unwatched = matches!(get("unwatched"), "1" | "true" | "yes");
    let sort = get("sort").to_lowercase();
    let page = get("page").parse::<usize>().unwrap_or(0);
    let limit = q
        .get("limit")
        .or_else(|| q.get("per"))
        .and_then(|x| x.trim().parse::<usize>().ok())
        .unwrap_or(60)
        .clamp(1, 120);

    let terms: Vec<String> = norm(q_str).split_whitespace().map(String::from).collect();

    let cat = app.catalog();
    let pool = {
        let meta = app.meta.read().unwrap();
        collect_fights(&cat, Some(&meta))
    };
    let snap = snapshot_state(app);

    let sport_counts = sport_counts_json(&pool);

    // Filter to the requested sport first so yearCounts and topFighters reflect the active sport tab.
    let sport_slice: Vec<&CanonicalFight> = pool
        .iter()
        .filter(|cf| matches_sport_filter(cf, &sport))
        .collect();

    let mut yc_map: HashMap<i32, usize> = HashMap::new();
    for cf in &sport_slice {
        if let Some(y) = cf.entry.year {
            if (1980..=2100).contains(&y) {
                *yc_map.entry(y).or_default() += 1;
            }
        }
    }
    let mut yc_vec: Vec<(i32, usize)> = yc_map.into_iter().collect();
    yc_vec.sort_by(|a, b| b.0.cmp(&a.0));
    let year_counts: Vec<Value> = yc_vec
        .into_iter()
        .map(|(year, count)| json!({ "year": year, "count": count }))
        .collect();

    let top_fighters = top_fighters_list(&sport_slice, 25);

    let mut matched: Vec<(&CanonicalFight, WatchInfo)> = sport_slice
        .into_iter()
        .filter(|cf| {
            if !fighter.is_empty() && !matches_fighter_query(&cf.entry, fighter) {
                return false;
            }
            if let Some(yr) = year_filter {
                if cf.entry.year != Some(yr) {
                    return false;
                }
            }
            if !matches_text_query(cf, &terms) {
                return false;
            }
            true
        })
        .filter_map(|cf| {
            let wi = watch_info_for(cf, &snap);
            if unwatched && wi.watched {
                return None;
            }
            Some((cf, wi))
        })
        .collect();

    match sort.as_str() {
        "num_desc" => matched.sort_by(|a, b| {
            let pa = (a.0.entry.event_num.is_some(), a.0.entry.event_num.unwrap_or(0), a.0.entry.event_kind == "ppv");
            let pb = (b.0.entry.event_num.is_some(), b.0.entry.event_num.unwrap_or(0), b.0.entry.event_kind == "ppv");
            pb.cmp(&pa).then(cmp_latest(a.0, b.0))
        }),
        "num_asc" => matched.sort_by(|a, b| {
            let pa = (!a.0.entry.event_num.is_some(), a.0.entry.event_num.unwrap_or(u32::MAX), a.0.entry.event_kind != "ppv");
            let pb = (!b.0.entry.event_num.is_some(), b.0.entry.event_num.unwrap_or(u32::MAX), b.0.entry.event_kind != "ppv");
            pa.cmp(&pb).then(a.0.entry.sort_ts.cmp(&b.0.entry.sort_ts))
        }),
        "oldest" => matched.sort_by(|a, b| {
            (a.0.entry.sort_ts == 0, a.0.entry.sort_ts)
                .cmp(&(b.0.entry.sort_ts == 0, b.0.entry.sort_ts))
                .then(a.0.entry.event_num.cmp(&b.0.entry.event_num))
                .then(a.0.id.cmp(&b.0.id))
        }),
        "title" => matched.sort_by(|a, b| {
            norm(&a.0.entry.clean_title)
                .cmp(&norm(&b.0.entry.clean_title))
                .then(cmp_latest(a.0, b.0))
        }),
        _ => matched.sort_by(|a, b| cmp_latest(a.0, b.0)),
    }

    let total = matched.len();
    let start = page.saturating_mul(limit);
    let more = start.saturating_add(limit) < total;
    let items: Vec<Value> = matched
        .into_iter()
        .skip(start)
        .take(limit)
        .map(|(cf, wi)| fight_card(cf, wi))
        .collect();

    json!({
        "items": items,
        "total": total,
        "page": page,
        "more": more,
        "sportCounts": sport_counts,
        "yearCounts": year_counts,
        "topFighters": top_fighters
    })
}

fn ufc_seen_key(cf: &CanonicalFight) -> String {
    let prelim = if cf.entry.is_prelims { ":prelim" } else { "" };
    if cf.entry.event_kind == "ppv" {
        if let Some(n) = cf.entry.event_num {
            return format!("ppv:{n}{prelim}");
        }
    }
    if cf.entry.event_kind == "fn" {
        if let Some(n) = cf.entry.event_num {
            if !cf.entry.fn_sub.is_empty() {
                return format!("{}:{n}{prelim}", cf.entry.fn_sub);
            }
        }
    }
    format!("id:{}", cf.id)
}

/// Detect newly arrived UFC VOD events compared to `state["seenUfc"]`.
/// First run seeds silently and returns an empty Vec (zero notification spam).
pub fn check_new_ufc(app: &App) -> Vec<Value> {
    let cat = app.catalog();
    let mut ufc_events: Vec<CanonicalFight> = collect_fights(&cat, None)
        .into_iter()
        .filter(|cf| cf.entry.sport_key == "ufc")
        .collect();
    if ufc_events.is_empty() {
        return vec![];
    }
    ufc_events.sort_by(cmp_latest);

    let mut fresh: Vec<Value> = Vec::new();
    let changed = app.state.with(|st| {
        let existing: Vec<String> = st["seenUfc"]
            .as_array()
            .map(|a| a.iter().map(s).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default();

        if existing.is_empty() {
            let mut keys: Vec<String> = ufc_events.iter().map(ufc_seen_key).collect();
            keys.sort();
            keys.dedup();
            st["seenUfc"] = json!(keys);
            return true;
        }

        let mut seen: HashSet<String> = existing.into_iter().collect();
        let mut added_any = false;
        for cf in &ufc_events {
            let key = ufc_seen_key(cf);
            if seen.insert(key) {
                added_any = true;
                fresh.push(json!({
                    "id": cf.id,
                    "title": cf.entry.clean_title,
                    "matchup": cf.entry.matchup,
                    "eventNum": cf.entry.event_num,
                    "eventKind": cf.entry.event_kind
                }));
            }
        }
        if added_any {
            let mut all_keys: Vec<String> = seen.into_iter().collect();
            all_keys.sort();
            st["seenUfc"] = json!(all_keys);
        }
        added_any
    });

    if changed {
        app.state.save();
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Cat, Catalog, Item};

    #[test]
    fn parses_real_ufc_and_boxing_titles() {
        let p1 = parse_fight("EN - UFC 327: Procházka vs. Ulberg (2026)", "EN - UFC", Some(2026), 1780288379)
            .expect("UFC 327 parsed");
        assert_eq!(p1.sport_key, "ufc");
        assert_eq!(p1.event_kind, "ppv");
        assert_eq!(p1.event_num, Some(327));
        assert_eq!(p1.matchup, "Procházka vs. Ulberg");
        assert_eq!(p1.fighters, vec!["Procházka", "Ulberg"]);
        assert_eq!(p1.year, Some(2026));
        assert!(p1.is_anz);

        let p2 = parse_fight("EN - UFC 328: Chimaev vs Strickland (2026)", "SOCCER UFC", Some(2026), 1790369220)
            .expect("UFC 328 parsed");
        assert_eq!(p2.sport_key, "ufc");
        assert_eq!(p2.event_kind, "ppv");
        assert_eq!(p2.event_num, Some(328));
        assert_eq!(p2.matchup, "Chimaev vs. Strickland");
        assert_eq!(p2.fighters, vec!["Chimaev", "Strickland"]);
        assert!(!p2.is_anz);

        let p3 = parse_fight("SOC - UFC 309 Jones vs Miocic 16.11.2024", "SOCCER UFC", None, 1733862780)
            .expect("UFC 309 parsed");
        assert_eq!(p3.event_kind, "ppv");
        assert_eq!(p3.event_num, Some(309));
        assert_eq!(p3.matchup, "Jones vs. Miocic");
        assert_eq!(p3.fighters, vec!["Jones", "Miocic"]);
        assert_eq!(p3.date_str.as_deref(), Some("2024-11-16"));
        assert_eq!(p3.year, Some(2024));
        assert_eq!(p3.clean_title, "UFC 309: Jones vs. Miocic");

        let p4 = parse_fight("EN - UFC Fight Night 260: Ulberg vs. Reyes (2025)", "EN - UFC", Some(2025), 1760285228)
            .expect("FN 260 parsed");
        assert_eq!(p4.event_kind, "fn");
        assert_eq!(p4.fn_sub, "fn");
        assert_eq!(p4.event_num, Some(260));
        assert_eq!(p4.matchup, "Ulberg vs. Reyes");
        assert_eq!(p4.fighters, vec!["Ulberg", "Reyes"]);
        assert!(p4.is_anz);

        let p5 = parse_fight("SOC - UFC on ESPN Namajunas vs Cortez 13.07.2024", "SOCCER UFC", None, 1731533220)
            .expect("ESPN parsed");
        assert_eq!(p5.event_kind, "fn");
        assert_eq!(p5.fn_sub, "espn");
        assert_eq!(p5.event_num, None);
        assert_eq!(p5.matchup, "Namajunas vs. Cortez");
        assert_eq!(p5.fighters, vec!["Namajunas", "Cortez"]);
        assert_eq!(p5.date_str.as_deref(), Some("2024-07-13"));
        assert_eq!(p5.year, Some(2024));

        let p6 = parse_fight("EN - Tyson Fury vs. Oleksandr Usyk (2024)", "EN - BOXING", Some(2024), 1716683580)
            .expect("Boxing parsed");
        assert_eq!(p6.sport_key, "boxing");
        assert_eq!(p6.matchup, "Tyson Fury vs. Oleksandr Usyk");
        assert_eq!(p6.fighters, vec!["Tyson Fury", "Oleksandr Usyk"]);
        assert_eq!(p6.year, Some(2024));
        assert!(!p6.is_anz);

        let p7 = parse_fight("EN - UFC 329: McGregor vs. Holloway 2 (2026)", "SOCCER UFC", Some(2026), 1790369220)
            .expect("Rematch parsed");
        assert_eq!(p7.event_num, Some(329));
        assert_eq!(p7.matchup, "McGregor vs. Holloway 2");
        assert_eq!(p7.fighters, vec!["McGregor", "Holloway"]);
    }

    #[test]
    fn dedups_across_en_ufc_soccer_ufc_and_ufc_arabia() {
        let mut cat = Catalog::default();
        cat.cats.insert(
            "movie:387".into(),
            Cat { id: "387".into(), kind: "movie", name: "EN - UFC".into(), en: true, sport: true, ..Default::default() },
        );
        cat.cats.insert(
            "movie:641".into(),
            Cat { id: "641".into(), kind: "movie", name: "SOCCER UFC".into(), sport: true, ..Default::default() },
        );
        cat.cats.insert(
            "movie:1415".into(),
            Cat { id: "1415".into(), kind: "movie", name: "UFC ARABIA".into(), ..Default::default() },
        );

        let add = |cat: &mut Catalog, id: i64, cat_id: &str, title: &str, year: Option<i32>, added: i64, tmdb: Option<&str>| {
            cat.movie_raw.insert(
                id,
                Item {
                    k: "m",
                    id,
                    title: title.into(),
                    year,
                    poster: "https://img/poster.jpg".into(),
                    tmdb: tmdb.map(String::from),
                    added,
                    cat: cat_id.into(),
                    ext: "mkv".into(),
                    ..Default::default()
                },
            );
        };

        // UFC 321 in both EN - UFC and SOCCER UFC -> EN - UFC wins primary, SOCCER UFC becomes alternate.
        add(&mut cat, 1813189, "387", "UFC 321: Aspinall vs. Gane", Some(2025), 1770053220, Some("1547699"));
        add(&mut cat, 1690260, "641", "UFC 321 Aspinall vs. Gane", Some(2025), 1763407980, Some("1547699"));

        // UFC 300 in all 3 categories -> EN - UFC wins primary, SOCCER UFC & UFC ARABIA alternates, exact date merged.
        add(&mut cat, 782065, "387", "UFC 300: Pereira vs. Hill", Some(2024), 1713266220, Some("1228562"));
        add(&mut cat, 1184282, "641", "UFC 300 Pereira vs Hill 13.04.2024", None, 1731533220, None);
        add(&mut cat, 942555, "1415", "UFC 300: Pereira vs. Hill", Some(2024), 1722551075, Some("1228562"));

        // Fight Night dedup across numbered EN - UFC and dated unnumbered SOCCER UFC.
        add(&mut cat, 913539, "387", "UFC on ESPN 59: Namajunas vs. Cortez", Some(2024), 1721229540, Some("1306222"));
        add(&mut cat, 1184296, "641", "UFC on ESPN Namajunas vs Cortez 13.07.2024", None, 1731533220, None);

        let fights = collect_fights(&cat, None);
        assert_eq!(fights.len(), 3);

        let u321 = fights.iter().find(|f| f.entry.event_num == Some(321)).expect("UFC 321");
        assert_eq!(u321.id, 1813189);
        assert_eq!(u321.cat, "EN - UFC");
        assert_eq!(u321.alternates.len(), 1);
        assert_eq!(u321.alternates[0].id, 1690260);
        assert_eq!(u321.alternates[0].label, "SOCCER UFC");

        let u300 = fights.iter().find(|f| f.entry.event_num == Some(300)).expect("UFC 300");
        assert_eq!(u300.id, 782065);
        assert_eq!(u300.cat, "EN - UFC");
        assert_eq!(u300.entry.date_str.as_deref(), Some("2024-04-13"));
        assert_eq!(u300.alternates.len(), 2);
        assert_eq!(u300.alternates[0].label, "SOCCER UFC");
        assert_eq!(u300.alternates[1].label, "UFC ARABIA");

        let espn59 = fights.iter().find(|f| f.entry.event_num == Some(59)).expect("ESPN 59");
        assert_eq!(espn59.id, 913539);
        assert_eq!(espn59.entry.date_str.as_deref(), Some("2024-07-13"));
        assert_eq!(espn59.alternates.len(), 1);
        assert_eq!(espn59.alternates[0].id, 1184296);
    }

    #[test]
    fn accent_insensitive_fighter_matching_and_anz_detection() {
        let u327 = parse_fight("EN - UFC 327: Procházka vs. Ulberg (2026)", "EN - UFC", Some(2026), 1780288379).unwrap();
        assert!(matches_fighter_query(&u327, "prochazka"));
        assert!(matches_fighter_query(&u327, "Procházka"));
        assert!(matches_fighter_query(&u327, "ulberg"));
        assert!(!matches_fighter_query(&u327, "volkanovski"));
        assert!(u327.is_anz);

        let u322 = parse_fight("EN - UFC 322: Della Maddalena vs. Makhachev (2025)", "EN - UFC", Some(2025), 1770053220).unwrap();
        assert!(u322.is_anz);
        assert!(matches_fighter_query(&u322, "della maddalena"));
        assert!(matches_fighter_query(&u322, "jdm"));

        let u314 = parse_fight("EN - UFC 314: Volkanovski vs. Lopes (2025)", "EN - UFC", Some(2025), 1744529138).unwrap();
        assert!(u314.is_anz);
        assert!(matches_fighter_query(&u314, "volk"));

        // Ensure "Volkov" does NOT falsely trigger ANZ detection for "Volk".
        let fn190 = parse_fight("EN - UFC Fight Night 190: Gane vs. Volkov (2021)", "EN - UFC", Some(2021), 1624965780).unwrap();
        assert!(!fn190.is_anz);

        // Apostrophe spacing normalization ("O' Malley" -> "O'Malley").
        let u306 = parse_fight("EN - UFC 306: O' Malley vs. Dvalishvili (2024)", "EN - UFC", Some(2024), 1726577880).unwrap();
        assert_eq!(u306.fighters, vec!["O'Malley", "Dvalishvili"]);
        assert!(matches_fighter_query(&u306, "omalley"));
    }

    #[test]
    fn ppv_ordering_ignores_equal_or_out_of_order_added_timestamps() {
        // Real provider scenario: 328, 329, 331 share identical `added`; 327 has older `added`;
        // 324 has newer `added` than 320, etc.
        let e331 = parse_fight("EN - UFC 331: Van vs. Pantoja 2 (2026)", "SOCCER UFC", Some(2026), 1790369220).unwrap();
        let e329 = parse_fight("EN - UFC 329: McGregor vs. Holloway 2 (2026)", "SOCCER UFC", Some(2026), 1790369220).unwrap();
        let e328 = parse_fight("EN - UFC 328: Chimaev vs Strickland (2026)", "SOCCER UFC", Some(2026), 1790369220).unwrap();
        let e327 = parse_fight("EN - UFC 327: Procházka vs. Ulberg (2026)", "EN - UFC", Some(2026), 1780288379).unwrap();
        let e324 = parse_fight("EN - UFC 324: Gaethje vs. Pimblett (2026)", "EN - UFC", Some(2026), 1799999999).unwrap();

        assert!(e331.sort_ts > e329.sort_ts);
        assert!(e329.sort_ts > e328.sort_ts);
        assert!(e328.sort_ts > e327.sort_ts);
        assert!(e327.sort_ts > e324.sort_ts);
    }

    #[test]
    fn classifies_other_sports_netflix_ufc_and_ignores_regular_movies() {
        let nf = parse_fight("NF - UFC 245: Usman vs. Covington", "NETFLIX MOVIES", None, 1583401946).unwrap();
        assert_eq!(nf.sport_key, "ufc");
        assert_eq!(nf.event_kind, "ppv");
        assert_eq!(nf.event_num, Some(245));
        assert_eq!(nf.fighters, vec!["Usman", "Covington"]);

        assert!(parse_fight("Inception (2010)", "NETFLIX MOVIES", Some(2010), 1700000000).is_none());
        assert!(parse_fight("Dramatic Movie (2024)", "IT - DRAMMA/ROMANCE", Some(2024), 1700000000).is_none());

        let f1 = parse_fight("SOC - Formula 1 Australian Grand Prix 02.04.2023", "SOCCER FORMULA 1", None, 1683045180).unwrap();
        assert_eq!(f1.sport_key, "f1");
        assert_eq!(f1.event_kind, "race");
        assert_eq!(f1.date_str.as_deref(), Some("2023-04-02"));

        let afl_from_rugby = parse_fight("SOC - Sydney Swans v St Kilda 07.06.2026", "SOCCER RUGBY", None, 1782939780).unwrap();
        assert_eq!(afl_from_rugby.sport_key, "afl");

        let rugby = parse_fight("SOC - Wests Tigers v Penrith Panthers 07.06.2026", "SOCCER RUGBY", None, 1782939780).unwrap();
        assert_eq!(rugby.sport_key, "rugby");

        let ple = parse_fight("EN - WWE WrestleMania 42 – Sunday (2026)", "EN - WWE", Some(2026), 1777039380).unwrap();
        assert_eq!(ple.sport_key, "wwe");
        assert_eq!(ple.event_kind, "ple");

        let weekly = parse_fight("EN - WWE SmackDown (11/07/2025)", "EN - WWE", None, 1752496740).unwrap();
        assert_eq!(weekly.sport_key, "wwe");
        assert_eq!(weekly.event_kind, "weekly");
        assert_eq!(weekly.date_str.as_deref(), Some("2025-07-11"));
    }
}
