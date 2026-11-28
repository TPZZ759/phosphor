// Catalogue: load the provider's lists, classify English/sports, de-duplicate by TMDB id, search.
use crate::re;
use serde::Deserialize;
use serde::de::{self, DeserializeOwned, DeserializeSeed, Deserializer, IgnoredAny, SeqAccess, Visitor};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::BufReader;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

pub const RAW: [&str; 6] = ["get_live_categories", "get_vod_categories", "get_series_categories", "get_live_streams", "get_vod_streams", "get_series"];

#[derive(Clone, Debug, Default)]
pub struct Variant {
    pub id: i64,
    pub uhd: bool,
    pub hdr: bool,
    pub cam: bool,
    pub dv: bool,
    pub bluray: bool,
    pub dolby_audio: bool,
    /// True for clean subtitle-only cross-market 4K/HDR/Dolby-Audio upgrade variants
    /// (e.g. Nordic 4K Dolby Audio, Nordic 4K HDR, NL Bioscoop 4K) attached by TMDB ID.
    pub sub_market: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Item {
    pub k: &'static str, // "m" | "s" | "l"
    pub id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub origin: String,
    pub poster: String,
    pub backdrop: String,
    pub rating: f64,
    pub tmdb: Option<String>,
    pub added: i64,
    pub cat: String,
    pub ext: String,
    pub en: bool,
    pub sport: bool,
    pub uhd: bool,
    pub hdr: bool,
    pub cam: bool,
    pub dv: bool,
    pub bluray: bool,
    pub dolby_audio: bool,
    pub trailer: String,
    pub genre: String,
    pub plot: String,
    pub epg: String,
    pub variants: Vec<Variant>,
    /// live: catch-up days available (0 = none)
    pub archive: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Cat { pub id: String, pub kind: &'static str, pub name: String, pub en: bool, pub sport: bool, pub uhd: bool, pub count: usize }

#[derive(Default)]
pub struct Catalog {
    pub cats: HashMap<String, Cat>, // "kind:id"
    pub movies: Vec<Item>,          // de-duplicated list (primaries)
    pub series: Vec<Item>,
    pub live: Vec<Item>,
    pub movie_raw: HashMap<i64, Item>, // every movie by stream id (incl. variants)
    pub series_raw: HashMap<i64, Item>,
    pub movie_by_tmdb: HashMap<String, usize>, // -> index into movies
    pub series_by_tmdb: HashMap<String, usize>,
    pub movie_by_id: HashMap<i64, usize>,      // primary + every variant stream_id -> canonical index in movies
    pub series_by_id: HashMap<i64, usize>,     // primary + every variant series_id -> canonical index in series
    pub live_by_id: HashMap<i64, usize>,
    pub loaded_at: i64,
}

pub fn clean_cat(n: &str) -> String {
    let t = crate::showcase::fold_superscripts(n);
    let t = re!(r"\bDOLBy\b").replace_all(&t, "DOLBY");
    let t = re!(r"\s{2,}").replace_all(&t, " ");
    re!(r"[ /]+$").replace(&t, "").trim().to_string()
}

/// Known IPTV country / language / streaming-network / quality prefix tokens allowed before `-` (e.g. `4K-NF -`, `EN-CAM -`, `IT -`).
fn is_iptv_prefix_token(tok: &str) -> bool {
    matches!(
        tok,
        // Countries / languages / regions
        "EN" | "ENG" | "US" | "USA" | "UK" | "GB" | "AU" | "AUS" | "CA" | "CAN" | "NZ" | "IE"
        | "NO" | "NOR" | "SE" | "SWE" | "DK" | "DEN" | "FI" | "FIN" | "IS" | "NL" | "BE"
        | "DE" | "GER" | "AT" | "CH" | "FR" | "FRA" | "ES" | "ESP" | "LA" | "LAT" | "MX" | "BR"
        | "PT" | "POR" | "IT" | "ITA" | "PL" | "POL" | "TR" | "TUR" | "AR" | "ARA" | "IN" | "IND"
        | "HI" | "HIN" | "PK" | "RU" | "RUS" | "UA" | "GR" | "RO" | "BG" | "CZ" | "SK" | "HU"
        | "HR" | "RS" | "AL" | "IL" | "IR" | "KR" | "KOR" | "JP" | "JPN" | "CN" | "HK" | "TW"
        | "TH" | "VN" | "ID" | "PH" | "MY" | "SG" | "ZA" | "AF" | "EU" | "EXYU" | "YU" | "SC"
        | "NORD" | "ASIA" | "ARAB" | "TURK"
        // Streaming platforms / IPTV networks
        | "NF" | "NFLX" | "AMZ" | "AMZN" | "PRIME" | "DSNP" | "D+" | "DIS" | "DIS+"
        | "ATVP" | "AP" | "AP+" | "APTV" | "ATV" | "ATV+" | "HBO" | "MAX" | "HBOM"
        | "OSN" | "OSN+" | "PM" | "PM+" | "PAR" | "PARA" | "P+" | "PEA" | "PCK"
        | "SHW" | "SHOW" | "SKY" | "DISC" | "DMAX" | "CR" | "CRUN" | "HULU" | "STAN" | "BINGE"
        | "BBC" | "ITV" | "ABC" | "CBS" | "NBC" | "FOX" | "CW" | "AMC" | "FX" | "TNT" | "TBS"
        | "STARZ" | "MGM" | "MGM+" | "BRIT" | "ACORN" | "SHUD" | "MUBI" | "HALL" | "LIFE"
        | "NAT" | "NG" | "HIST" | "NICK" | "UNI" | "WB" | "SONY" | "MARV" | "DW" | "PIX"
        // Quality / release / bucket tags
        | "4K" | "8K" | "UHD" | "FHD" | "HD" | "SD" | "HEVC" | "H265" | "X265"
        | "HDR" | "HDR10" | "DV" | "DOVI" | "DOLBY" | "ATMOS" | "MULTI" | "DUAL"
        | "SUB" | "DUB" | "VIP" | "TOP" | "NEW" | "VOD" | "CAM" | "TS" | "HDTS" | "HDCAM"
        | "SCR" | "BLURAY" | "REMUX" | "3D" | "RAW" | "60FPS" | "50FPS" | "PPV"
        | "MOV" | "SER" | "DOC" | "KIDS" | "ANI" | "ANIME"
    )
}

/// Narrower token list allowed before `:` so real titles like `IT: Chapter Two`, `MAX: ...`, `NEW: ...`,
/// `CSI: Miami`, `NCIS: Sydney`, `FBI: Most Wanted`, `9-1-1: Lone Star`, `24: Live Another Day`,
/// `X-MEN: First Class`, `STAR: ...`, `SEAL: ...`, `ER: ...`, `V: ...` are NEVER stripped.
fn is_iptv_colon_prefix_token(tok: &str) -> bool {
    matches!(
        tok,
        "EN" | "ENG" | "US" | "USA" | "UK" | "GB" | "AU" | "AUS" | "CA" | "CAN" | "NZ"
        | "NO" | "SE" | "DK" | "FI" | "NL" | "DE" | "FR" | "ES" | "PT" | "PL" | "TR" | "AR"
        | "NF" | "NFLX" | "AMZ" | "AMZN" | "DSNP" | "D+" | "ATVP" | "AP+" | "HBO" | "OSN+" | "PM+"
        | "4K" | "8K" | "UHD" | "FHD" | "HD" | "SD" | "HEVC" | "HDR" | "HDR10" | "DV"
        | "MULTI" | "DUAL" | "VIP" | "VOD" | "CAM" | "HDCAM" | "HDTS"
    )
}

fn is_iso2_country(code: &str) -> bool {
    matches!(
        code,
        "US" | "UK" | "GB" | "AU" | "CA" | "IE" | "NZ"
        | "JP" | "KR" | "FR" | "DE" | "ES" | "IT" | "SE" | "NO" | "DK" | "FI" | "IS" | "NL" | "BE" | "AT" | "CH"
        | "IN" | "CN" | "HK" | "TW" | "MX" | "BR" | "AR" | "CL" | "CO" | "PE"
        | "PL" | "TR" | "PT" | "RU" | "UA" | "CZ" | "SK" | "HU" | "RO" | "BG" | "GR" | "HR" | "RS"
        | "IL" | "ZA" | "EG" | "SA" | "AE" | "TH" | "VN" | "ID" | "PH" | "MY" | "SG" | "PK" | "IR"
    )
}

pub struct Clean { pub title: String, pub year: Option<i32>, pub origin: String }
pub fn clean_title(name: &str) -> Clean {
    let mut t = crate::showcase::fold_superscripts(name).trim().to_string();
    for _ in 0..3 {
        // 1. Pipe prefixes (`US | Show`, `4K| Movie`, `EN-4K | Show`): always IPTV scaffolding
        if let Some(m) = re!(r"^\s*([A-Z0-9+]{1,6}(?:-[A-Z0-9+]{1,6}){0,2})\s*\|\s*").captures(&t) {
            let pref = &m[1];
            if pref.split('-').all(|tok| is_iptv_prefix_token(tok) || (tok.len() == 2 && is_iso2_country(tok))) {
                t = t[m.get(0).unwrap().end()..].to_string();
                continue;
            }
        }
        // 2. Dash prefixes (`EN - Title`, `4K-NF - Title`, `EN-CAM - Title`, `IT - Title`)
        if let Some(m) = re!(r"^\s*([A-Z0-9+]{1,6}(?:-[A-Z0-9+]{1,6}){0,2})\s*-\s+").captures(&t) {
            let pref = &m[1];
            if pref.split('-').all(is_iptv_prefix_token) {
                t = t[m.get(0).unwrap().end()..].to_string();
                continue;
            }
        }
        // 3. Colon prefixes (`EN: Title`, `4K-NF: Title`) — strict allowlist so CSI:/NCIS:/FBI:/9-1-1:/24:/X-MEN:/ER:/V:/STAR:/SEAL: survive!
        if let Some(m) = re!(r"^\s*([A-Z0-9+]{1,6}(?:-[A-Z0-9+]{1,6}){0,2})\s*:\s+").captures(&t) {
            let pref = &m[1];
            if pref.split('-').all(is_iptv_colon_prefix_token) {
                t = t[m.get(0).unwrap().end()..].to_string();
                continue;
            }
        }
        break;
    }
    t = re!(r"^\d{1,3}\.\s*").replace(&t, "").to_string();
    t = re!(r"(?i)\s+(HEVC|H\.?265|x265|UHD|HDR10\+?|HDR|4K|FHD|1080p|2160p|3840p|DOLBY\s+VISION|DOLBY\s+AUDIO|REMASTERED)\b").replace_all(&t, "").to_string();
    if !t.contains(' ') && t.contains('.') { t = t.replace('.', " "); }
    // Only extract genuine ISO-2 country codes as `origin` — never `(HD)`, `(DV)`, `(TV)`, `(TS)`, `(II)`, `(3D)`, `(4K)`, `(SD)`, `(CAM)`.
    let origin = re!(r"\(([A-Z]{2})\)")
        .captures_iter(&t)
        .find_map(|c| {
            let code = &c[1];
            is_iso2_country(code).then(|| code.to_string())
        })
        .unwrap_or_default();
    let year = re!(r"\((\d{4})\)").captures(&t).or_else(|| re!(r"\s(19\d\d|20\d\d)\s*$").captures(&t)).and_then(|c| c[1].parse().ok());
    t = re!(r"\(\d{4}\)").replace_all(&t, "").to_string();
    t = re!(r"\s(19\d\d|20\d\d)\s*$").replace(&t, "").to_string();
    t = re!(r"\[[^\]]*\]").replace_all(&t, "").to_string();
    // Strip parenthesized CAM / Telesync / release / quality / MULTI / DUAL tags cleanly
    t = re!(r"(?i)\s*\(\s*(?:cam|camrip|hd-?cam|telesync|hd-?ts|ts|telecine|hdtc|workprint|dvdscr|scr|r5|clean\s*audio|line\s*audio|hd|sd|fhd|uhd|4k|3d|dv|hdr10?\+?|hevc|h\.?265|x265|bluray|blu-ray|remux|multi[^)]*|dual[^)]*)(?:\s*v\d+)?\s*\)")
        .replace_all(&t, "")
        .to_string();
    // Strip parenthesized ISO-2 country codes (e.g. `(US)`, `(UK)`, `(JP)`) without stripping `(II)` / `(TV)`
    t = re!(r"\s*\(([A-Z]{2})\)")
        .replace_all(&t, |caps: &regex::Captures| {
            if is_iso2_country(&caps[1]) { String::new() } else { caps[0].to_string() }
        })
        .to_string();
    t = re!(r"\s{2,}").replace_all(&t, " ").trim().to_string();
    if t.is_empty() { t = name.trim().to_string(); }
    Clean { title: t, year, origin }
}

fn is_en_vod(n: &str) -> bool {
    if re!(r"(?i)^ENGLISH SERIES \(TR\)").is_match(n) { return false; }
    re!(r"(?i)^(EN\s*-|NETFLIX|DISNEY\+|APPLE\+|AMAZON|HBO|OSN\+\s*(?:SERIES|MOVIES)|PARAMOUNT|PEACOCK|SHOWTIME|SKY$|UNIVERSAL|MARVEL|DREAMWORKS|TOP (MOVIES|KIDS)|JAMES BOND|ENGLISH (SERIES|REALITY|KIDS)|NICKELODEON|CRUNCHYROLL|DISCOVERY\+ (SERIES|MOVIES)(?:\s|$)|CHRISTIAN MOVIES|ASIA (MOVIES|SERIES) \(MULTI)").is_match(n)
}
/// Clean subtitle-only cross-market 4K / Dolby Audio TV series categories whose
/// English shows carry original English audio + soft subtitles (never dubbed, no burned-in subs).
fn is_sub_market_en_upgrade(clean_cat_name: &str) -> bool {
    re!(r"(?i)^NORDIC\s+(?:SERIES?|KIDS)\b.*(?:4K|3840|2160|UHD|DOLBY)").is_match(clean_cat_name)
}
/// Clean subtitle-only adult cinema 4K / 4K HDR / Dolby Audio movie categories whose
/// English movies carry original English audio + soft subtitles (excludes kids categories that may be dubbed).
fn is_sub_market_en_movie_upgrade(clean_cat_name: &str) -> bool {
    re!(r"(?i)^(?:NORDIC\s+FILM\b.*(?:4K|3840|2160|UHD|DOLBY)|NORDIC\s+4K\s+HDR|NL\s*-\s*BIOSCOOP\s+4K)").is_match(clean_cat_name)
}
fn has_arabic_script(s: &str) -> bool {
    s.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c))
}
fn is_sport_vod(n: &str) -> bool { re!(r"(?i)^(SOCCER |EN - (WWE|BOXING|UFC)|FIFA\+)").is_match(n) }
fn is_en_live(n: &str) -> bool { re!(r"(?i)^(US|UK|CA|AU|IE|NZ|4K)\s*\|").is_match(n) && !re!(r"(?i)\b(FR|FRENCH|ESPA|LATINO|SPANISH|UKRAINE)\b").is_match(n) }
pub fn is_sport_live(n: &str) -> bool {
    re!(r"(?i)SPORT|PPV|NFL|NBA|WNBA|NHL|MLB|MILB|MLS|UFC|WWE|AEW|\bF1\b|FORMULA|SOCCER|FOOTBALL|RUGBY|CRICKET|TENNIS|GOLF|BOXING|DAZN|ESPN|TNT SPORT|SKY SPORT|BEIN|NRL|AFL|EPL|PREMIER|LALIGA|SERIE A|BUNDESLIGA|UEFA|FIFA|NCAA|MOTOGP|RACING|SUPERCROSS|MATCHROOM|FLO|TSN|SPORTSNET|OPTUS|KAYO|FOX ?SPORT|EVENT|REPLAY|MASTERS|NFHS|BTN").is_match(n)
}
fn is_uhd(n: &str) -> bool { re!(r"(?i)⁴ᴷ|3840|2160|4K|UHD|ᵁᴴᴰ").is_match(n) }

/// Extract canonical streaming platform / studio name from a provider category name.
pub fn platform_from_cat(cat_name: &str) -> Option<&'static str> {
    let u = cat_name.to_uppercase();
    if u.contains("NETFLIX") { return Some("Netflix"); }
    if u.contains("APPLE+") || u.contains("APPLE TV") { return Some("Apple TV+"); }
    if u.contains("DISNEY+") || u.contains("DISNEY PLUS") { return Some("Disney+"); }
    if u.contains("AMAZON") || u.contains("PRIME VIDEO") { return Some("Amazon"); }
    if u.contains("HBO") { return Some("HBO"); }
    if u.contains("DISCOVERY+") || u.contains("DISCOVERY PLUS") { return Some("Discovery+"); }
    if u.contains("PARAMOUNT") { return Some("Paramount+"); }
    if u.contains("PEACOCK") { return Some("Peacock"); }
    if u.contains("SHOWTIME") { return Some("Showtime"); }
    if u.contains("CRUNCHYROLL") { return Some("Crunchyroll"); }
    if u.contains("MARVEL") { return Some("Marvel"); }
    if re!(r"\bSKY\b").is_match(&u) { return Some("Sky"); }
    None
}

fn platform_canonical(q: &str) -> Option<&'static str> {
    match q.trim().to_lowercase().as_str() {
        "netflix" => Some("Netflix"),
        "apple" | "apple+" | "apple tv" | "apple tv+" | "appletv" => Some("Apple TV+"),
        "disney" | "disney+" | "disney plus" | "disneyplus" => Some("Disney+"),
        "amazon" | "prime" | "prime video" | "amazon prime" | "amazon prime video" => Some("Amazon"),
        "hbo" | "hbo max" => Some("HBO"),
        "discovery" | "discovery+" | "discovery plus" => Some("Discovery+"),
        "paramount" | "paramount+" | "paramount plus" => Some("Paramount+"),
        "peacock" => Some("Peacock"),
        "showtime" => Some("Showtime"),
        "crunchyroll" => Some("Crunchyroll"),
        "marvel" => Some("Marvel"),
        "sky" => Some("Sky"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Lightweight streaming JSON deserializers (avoids 400 MB serde_json::Value DOM)
// ---------------------------------------------------------------------------

fn de_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("string, number, or null") }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<String, E> { Ok(String::new()) }
        fn visit_unit<E: de::Error>(self) -> Result<String, E> { Ok(String::new()) }
        fn visit_none<E: de::Error>(self) -> Result<String, E> { Ok(String::new()) }
        fn visit_some<D2: Deserializer<'de>>(self, d2: D2) -> Result<String, D2::Error> { de_str(d2) }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<String, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(String::new())
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<String, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(String::new())
        }
    }
    d.deserialize_any(V)
}

fn de_i64<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = i64;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("int, float, numeric string, or null") }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<i64, E> { Ok(v) }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<i64, E> { Ok(v as i64) }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<i64, E> { Ok(v as i64) }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<i64, E> { Ok(v.trim().parse::<f64>().map(|f| f as i64).unwrap_or(0)) }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<i64, E> { Ok(if v { 1 } else { 0 }) }
        fn visit_unit<E: de::Error>(self) -> Result<i64, E> { Ok(0) }
        fn visit_none<E: de::Error>(self) -> Result<i64, E> { Ok(0) }
        fn visit_some<D2: Deserializer<'de>>(self, d2: D2) -> Result<i64, D2::Error> { de_i64(d2) }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<i64, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(0)
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<i64, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(0)
        }
    }
    d.deserialize_any(V)
}

fn de_f64<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = f64;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("number, numeric string, or null") }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<f64, E> { Ok(if v.is_finite() { v } else { 0.0 }) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<f64, E> { Ok(v as f64) }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<f64, E> { Ok(v as f64) }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<f64, E> { Ok(v.trim().parse::<f64>().ok().filter(|x| x.is_finite()).unwrap_or(0.0)) }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<f64, E> { Ok(0.0) }
        fn visit_unit<E: de::Error>(self) -> Result<f64, E> { Ok(0.0) }
        fn visit_none<E: de::Error>(self) -> Result<f64, E> { Ok(0.0) }
        fn visit_some<D2: Deserializer<'de>>(self, d2: D2) -> Result<f64, D2::Error> { de_f64(d2) }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<f64, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(0.0)
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<f64, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(0.0)
        }
    }
    d.deserialize_any(V)
}

fn de_first_str<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct FirstStr(String);
    impl<'de> Deserialize<'de> for FirstStr {
        fn deserialize<D2: Deserializer<'de>>(d2: D2) -> Result<Self, D2::Error> { Ok(FirstStr(de_str(d2)?)) }
    }
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("string, array of strings, or null") }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<String, E> { Ok(String::new()) }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<String, E> { Ok(String::new()) }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<String, E> { Ok(String::new()) }
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<String, E> { Ok(String::new()) }
        fn visit_unit<E: de::Error>(self) -> Result<String, E> { Ok(String::new()) }
        fn visit_none<E: de::Error>(self) -> Result<String, E> { Ok(String::new()) }
        fn visit_some<D2: Deserializer<'de>>(self, d2: D2) -> Result<String, D2::Error> { de_first_str(d2) }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<String, A::Error> {
            let first = seq.next_element::<FirstStr>()?.map(|x| x.0).unwrap_or_default();
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(first)
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<String, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(String::new())
        }
    }
    d.deserialize_any(V)
}

#[derive(Deserialize, Default)]
struct RawCat {
    #[serde(default, deserialize_with = "de_str")]
    category_id: String,
    #[serde(default, deserialize_with = "de_str")]
    category_name: String,
}

#[derive(Deserialize, Default)]
struct RawMovie {
    #[serde(default, deserialize_with = "de_str")]
    category_id: String,
    #[serde(default, deserialize_with = "de_str")]
    name: String,
    #[serde(default, deserialize_with = "de_str")]
    tmdb: String,
    #[serde(default, deserialize_with = "de_i64")]
    stream_id: i64,
    #[serde(default, deserialize_with = "de_str")]
    stream_icon: String,
    #[serde(default, deserialize_with = "de_f64")]
    rating: f64,
    #[serde(default, deserialize_with = "de_i64")]
    added: i64,
    #[serde(default, deserialize_with = "de_str")]
    container_extension: String,
    #[serde(default, deserialize_with = "de_str")]
    trailer: String,
}

#[derive(Deserialize, Default)]
struct RawSeries {
    #[serde(default, deserialize_with = "de_str")]
    category_id: String,
    #[serde(default, deserialize_with = "de_str")]
    name: String,
    #[serde(default, rename = "releaseDate", deserialize_with = "de_str")]
    release_date_camel: String,
    #[serde(default, deserialize_with = "de_str")]
    release_date: String,
    #[serde(default, deserialize_with = "de_str")]
    tmdb: String,
    #[serde(default, deserialize_with = "de_i64")]
    series_id: i64,
    #[serde(default, deserialize_with = "de_str")]
    cover: String,
    #[serde(default, deserialize_with = "de_first_str")]
    backdrop_path: String,
    #[serde(default, deserialize_with = "de_f64")]
    rating: f64,
    #[serde(default, deserialize_with = "de_i64")]
    last_modified: i64,
    #[serde(default, deserialize_with = "de_str")]
    genre: String,
    #[serde(default, deserialize_with = "de_str")]
    plot: String,
}

#[derive(Deserialize, Default)]
struct RawLive {
    #[serde(default, deserialize_with = "de_str")]
    category_id: String,
    #[serde(default, deserialize_with = "de_str")]
    name: String,
    #[serde(default, deserialize_with = "de_i64")]
    stream_id: i64,
    #[serde(default, deserialize_with = "de_str")]
    stream_icon: String,
    #[serde(default, deserialize_with = "de_str")]
    epg_channel_id: String,
    #[serde(default, deserialize_with = "de_i64")]
    added: i64,
    #[serde(default, deserialize_with = "de_i64")]
    tv_archive: i64,
    #[serde(default, deserialize_with = "de_i64")]
    tv_archive_duration: i64,
}

/// Stream-deserialize a top-level JSON array from disk one element at a time without building a DOM tree.
fn stream_json_array<T: DeserializeOwned>(dir: &Path, name: &str, mut on_item: impl FnMut(T)) {
    let Ok(file) = File::open(dir.join(format!("{name}.json"))) else { return };
    let reader = BufReader::with_capacity(256 * 1024, file);
    struct ArraySeed<'a, T, F>(&'a mut F, PhantomData<T>);
    impl<'de, 'a, T: DeserializeOwned, F: FnMut(T)> DeserializeSeed<'de> for ArraySeed<'a, T, F> {
        type Value = ();
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            struct SeqVis<'a, T, F>(&'a mut F, PhantomData<T>);
            impl<'de, 'a, T: DeserializeOwned, F: FnMut(T)> Visitor<'de> for SeqVis<'a, T, F> {
                type Value = ();
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("JSON array") }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                    while let Some(item) = seq.next_element::<T>()? {
                        (self.0)(item);
                    }
                    Ok(())
                }
                fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                    Ok(())
                }
            }
            d.deserialize_any(SeqVis(self.0, PhantomData))
        }
    }
    let mut de = serde_json::Deserializer::from_reader(reader);
    if let Err(e) = ArraySeed::<T, _>(&mut on_item, PhantomData).deserialize(&mut de) {
        eprintln!("[catalog] error streaming {name}.json: {e}");
    }
}

fn norm_rating(mut n: f64) -> f64 {
    if !n.is_finite() || n < 0.0 { return 0.0; }
    if n > 10.0 { n /= 10.0; }
    (n * 10.0).round() / 10.0
}

fn origins_compatible(a: &str, b: &str) -> bool {
    a.is_empty() || b.is_empty() || a == b
}

fn merge_variant(prev: &mut Item, it: &Item) {
    if prev.id == it.id {
        prev.added = prev.added.max(it.added);
        return;
    }
    if !prev.variants.iter().any(|v| v.id == it.id) {
        prev.variants.push(Variant {
            id: it.id, uhd: it.uhd, hdr: it.hdr, cam: it.cam,
            dv: it.dv, bluray: it.bluray, dolby_audio: it.dolby_audio, sub_market: false,
        });
    }
    if (prev.cam && !it.cam) || (!it.cam && prev.uhd && !it.uhd) {
        swap_primary(prev, it);
    }
    prev.added = prev.added.max(it.added);
}

impl Catalog {
    pub fn raw_dir(data: &Path) -> PathBuf { data.join("raw") }
    pub fn have_cache(data: &Path) -> bool { RAW.iter().all(|n| Self::raw_dir(data).join(format!("{n}.json")).exists()) }

    pub fn load(data: &Path) -> Catalog {
        let t0 = std::time::Instant::now();
        let dir = Self::raw_dir(data);
        let mut c = Catalog::default();
        let mut raw_cats: HashMap<String, String> = HashMap::new();
        for (kind, file) in [("live", "get_live_categories"), ("movie", "get_vod_categories"), ("series", "get_series_categories")] {
            stream_json_array::<RawCat>(&dir, file, |x| {
                let raw = x.category_name;
                let name = clean_cat(&raw);
                let (en, sport) = if kind == "live" { (is_en_live(&name), is_sport_live(&name)) } else { (is_en_vod(&name), is_sport_vod(&name)) };
                let id = x.category_id;
                let key = format!("{kind}:{id}");
                raw_cats.insert(key.clone(), raw.clone());
                c.cats.insert(key, Cat { id, kind, name, en, sport, uhd: is_uhd(&raw), count: 0 });
            });
        }

        // Movies (two-pass: stream-parse EN catalog & deduplicate by TMDB + (title, year) fallback;
        // then attach clean subtitle-only 4K/HDR/Dolby upgrades and map every primary + variant ID in O(1))
        let mut movie_sub_candidates: Vec<(String, Variant, i64)> = vec![];
        let mut en_movie_by_ty: HashMap<(String, i32), usize> = HashMap::new();
        stream_json_array::<RawMovie>(&dir, "get_vod_streams", |v| {
            let cat_id = v.category_id;
            let cat_key = format!("movie:{cat_id}");
            let Some(cat) = c.cats.get_mut(&cat_key) else { return };
            cat.count += 1;
            let raw_name = v.name;
            let raw_cat = raw_cats.get(&cat_key).map(String::as_str).unwrap_or("");
            let cl = clean_title(&raw_name);
            let tm = { let t = v.tmdb.trim(); if t.is_empty() || t == "0" { None } else { Some(t.to_string()) } };
            let cam = crate::showcase::is_cam_release(&raw_name, raw_cat);
            let dv = !cam && crate::showcase::is_dv_tag(&raw_name, raw_cat);
            let hdr = !cam && (dv || crate::showcase::is_hdr_tag(&raw_name, raw_cat));
            let bluray = !cam && crate::showcase::is_bluray_tag(&raw_name, raw_cat);
            let dolby_audio = !cam && crate::showcase::is_dolby_audio_tag(&raw_name, raw_cat);
            let uhd = !cam && (cat.uhd || dv || hdr || crate::showcase::is_uhd_tag(&raw_name, raw_cat));
            let is_osn_non_en = cat.name.to_uppercase().starts_with("OSN+") && has_arabic_script(&raw_name);
            let en = cat.en && !is_osn_non_en;
            let it = Item {
                k: "m", id: v.stream_id, title: cl.title, year: cl.year, origin: cl.origin,
                poster: v.stream_icon, rating: norm_rating(v.rating),
                tmdb: tm.clone(), added: v.added, cat: cat_id,
                ext: if v.container_extension.is_empty() { "mp4".into() } else { v.container_extension },
                en, sport: cat.sport, uhd, hdr, cam, dv, bluray, dolby_audio, trailer: v.trailer, ..Default::default()
            };
            c.movie_raw.insert(it.id, it.clone());
            if !en && !cam && (uhd || hdr || dolby_audio) && is_sub_market_en_movie_upgrade(&cat.name) {
                if let Some(ref t) = tm {
                    movie_sub_candidates.push((t.clone(), Variant { id: it.id, uhd, hdr, cam: false, dv, bluray, dolby_audio, sub_market: true }, it.added));
                }
            }
            if en {
                let ty_key = it.year.and_then(|y| { let nt = crate::util::norm(&it.title); if nt.is_empty() { None } else { Some((nt, y)) } });
                if let Some(t) = it.tmdb.clone() {
                    if let Some(&ix) = c.movie_by_tmdb.get(&t) {
                        merge_variant(&mut c.movies[ix], &it);
                        c.movie_by_id.insert(it.id, ix);
                        return;
                    }
                    if let Some(ref tyk) = ty_key {
                        if let Some(&ix) = en_movie_by_ty.get(tyk) {
                            if c.movies[ix].tmdb.is_none() {
                                c.movies[ix].tmdb = Some(t.clone());
                                merge_variant(&mut c.movies[ix], &it);
                                c.movie_by_tmdb.insert(t, ix);
                                c.movie_by_id.insert(it.id, ix);
                                return;
                            }
                        }
                    }
                    let ix = c.movies.len();
                    c.movie_by_tmdb.insert(t, ix);
                    if let Some(tyk) = ty_key { en_movie_by_ty.entry(tyk).or_insert(ix); }
                    c.movie_by_id.insert(it.id, ix);
                    c.movies.push(it);
                    return;
                } else if let Some(tyk) = ty_key {
                    if let Some(&ix) = en_movie_by_ty.get(&tyk) {
                        merge_variant(&mut c.movies[ix], &it);
                        c.movie_by_id.insert(it.id, ix);
                        return;
                    }
                    let ix = c.movies.len();
                    en_movie_by_ty.insert(tyk, ix);
                    c.movie_by_id.insert(it.id, ix);
                    c.movies.push(it);
                    return;
                }
            }
            let ix = c.movies.len();
            c.movie_by_id.entry(it.id).or_insert(ix);
            c.movies.push(it);
        });
        // Pass 2 (movies): attach clean subtitle-only 4K / 4K HDR / Dolby Audio upgrade variants onto existing English movies by TMDB ID
        let mut attached_movie_4k = 0usize;
        for (tmdb, v, added) in movie_sub_candidates {
            if let Some(&ix) = c.movie_by_tmdb.get(&tmdb) {
                let prev = &mut c.movies[ix];
                if prev.id != v.id && !prev.variants.iter().any(|x| x.id == v.id) {
                    let vid = v.id;
                    prev.variants.push(v);
                    prev.added = prev.added.max(added);
                    c.movie_by_id.insert(vid, ix);
                    attached_movie_4k += 1;
                }
            }
        }

        // Series (two-pass: build EN catalog & deduplicate by TMDB + (norm_title, year_or_zero) fallback;
        // then attach clean subtitle-only 4K/Dolby-Audio upgrades and map every primary + variant ID in O(1))
        let mut sub_market_candidates: Vec<(String, Variant, i64)> = vec![];
        let mut en_series_by_title: HashMap<(String, i32), usize> = HashMap::new();
        let mut en_series_by_norm: HashMap<String, Vec<usize>> = HashMap::new();
        stream_json_array::<RawSeries>(&dir, "get_series", |x| {
            let cat_id = x.category_id;
            let cat_key = format!("series:{cat_id}");
            let Some(cat) = c.cats.get_mut(&cat_key) else { return };
            cat.count += 1;
            let raw_name = x.name;
            let raw_cat = raw_cats.get(&cat_key).map(String::as_str).unwrap_or("");
            let cl = clean_title(&raw_name);
            let rd = if !x.release_date_camel.is_empty() { x.release_date_camel } else { x.release_date };
            let tm = { let t = x.tmdb.trim(); if t.is_empty() || t == "0" { None } else { Some(t.to_string()) } };
            let cam = crate::showcase::is_cam_release(&raw_name, raw_cat);
            let dv = !cam && crate::showcase::is_dv_tag(&raw_name, raw_cat);
            let hdr = !cam && (dv || crate::showcase::is_hdr_tag(&raw_name, raw_cat));
            let bluray = !cam && crate::showcase::is_bluray_tag(&raw_name, raw_cat);
            let dolby_audio = !cam && crate::showcase::is_dolby_audio_tag(&raw_name, raw_cat);
            let uhd = !cam && (cat.uhd || dv || hdr || crate::showcase::is_uhd_tag(&raw_name, raw_cat));
            // OSN+ 4K is an EN HBO/Sky/Showtime category, except occasional Arabic-script standup specials
            let is_osn_non_en = cat.name.to_uppercase().starts_with("OSN+") && has_arabic_script(&raw_name);
            let en = cat.en && !is_osn_non_en;
            let it = Item {
                k: "s", id: x.series_id, title: cl.title, origin: cl.origin,
                year: cl.year.or_else(|| rd.get(0..4).and_then(|y| y.parse().ok())),
                poster: x.cover, backdrop: x.backdrop_path,
                rating: norm_rating(x.rating), tmdb: tm.clone(), added: x.last_modified,
                cat: cat_id, genre: x.genre, plot: x.plot,
                en, sport: cat.sport, uhd, hdr, cam, dv, bluray, dolby_audio, ..Default::default()
            };
            c.series_raw.insert(it.id, it.clone());
            if !en && !cam && (uhd || hdr || dolby_audio) && is_sub_market_en_upgrade(&cat.name) {
                if let Some(ref t) = tm {
                    sub_market_candidates.push((t.clone(), Variant { id: it.id, uhd, hdr, cam: false, dv, bluray, dolby_audio, sub_market: true }, it.added));
                }
            }
            if en {
                let norm_t = crate::util::norm(&it.title);
                let yr_key = it.year.unwrap_or(0);
                // Helper: find compatible series fallback index without colliding distinct-year remakes (The Office 2001 vs 2005, Doctor Who 1963 vs 2005)
                fn find_series_fallback(
                    series: &[Item],
                    by_title: &HashMap<(String, i32), usize>,
                    by_norm: &HashMap<String, Vec<usize>>,
                    norm_t: &str,
                    yr_key: i32,
                    origin: &str,
                    require_no_tmdb: bool,
                ) -> Option<usize> {
                    if norm_t.is_empty() { return None; }
                    if let Some(&ix) = by_title.get(&(norm_t.to_string(), yr_key)) {
                        let prev = &series[ix];
                        if (!require_no_tmdb || prev.tmdb.is_none()) && origins_compatible(&prev.origin, origin) {
                            return Some(ix);
                        }
                    }
                    if yr_key > 0 {
                        // A dated copy can match an earlier yearless entry (yr_key == 0) if origins don't conflict
                        if let Some(&ix) = by_title.get(&(norm_t.to_string(), 0)) {
                            let prev = &series[ix];
                            if (!require_no_tmdb || prev.tmdb.is_none()) && origins_compatible(&prev.origin, origin) {
                                return Some(ix);
                            }
                        }
                    } else if let Some(cands) = by_norm.get(norm_t) {
                        // Yearless entry only merges if there is a single unambiguous candidate with compatible origin
                        if cands.len() == 1 {
                            let ix = cands[0];
                            let prev = &series[ix];
                            if (!require_no_tmdb || prev.tmdb.is_none()) && origins_compatible(&prev.origin, origin) {
                                return Some(ix);
                            }
                        }
                    }
                    None
                }

                if let Some(t) = it.tmdb.clone() {
                    if let Some(&ix) = c.series_by_tmdb.get(&t) {
                        merge_variant(&mut c.series[ix], &it);
                        c.series_by_id.insert(it.id, ix);
                        return;
                    }
                    if let Some(ix) = find_series_fallback(&c.series, &en_series_by_title, &en_series_by_norm, &norm_t, yr_key, &it.origin, true) {
                        c.series[ix].tmdb = Some(t.clone());
                        if c.series[ix].year.is_none() && it.year.is_some() { c.series[ix].year = it.year; }
                        merge_variant(&mut c.series[ix], &it);
                        c.series_by_tmdb.insert(t, ix);
                        if yr_key > 0 && !norm_t.is_empty() {
                            en_series_by_title.entry((norm_t.clone(), yr_key)).or_insert(ix);
                        }
                        c.series_by_id.insert(it.id, ix);
                        return;
                    }
                    let ix = c.series.len();
                    c.series_by_tmdb.insert(t, ix);
                    if !norm_t.is_empty() {
                        en_series_by_title.entry((norm_t.clone(), yr_key)).or_insert(ix);
                        en_series_by_norm.entry(norm_t).or_default().push(ix);
                    }
                    c.series_by_id.insert(it.id, ix);
                    c.series.push(it);
                    return;
                } else if !norm_t.is_empty() {
                    if let Some(ix) = find_series_fallback(&c.series, &en_series_by_title, &en_series_by_norm, &norm_t, yr_key, &it.origin, false) {
                        if c.series[ix].year.is_none() && it.year.is_some() { c.series[ix].year = it.year; }
                        merge_variant(&mut c.series[ix], &it);
                        if yr_key > 0 {
                            en_series_by_title.entry((norm_t.clone(), yr_key)).or_insert(ix);
                        }
                        c.series_by_id.insert(it.id, ix);
                        return;
                    }
                    let ix = c.series.len();
                    en_series_by_title.entry((norm_t.clone(), yr_key)).or_insert(ix);
                    en_series_by_norm.entry(norm_t).or_default().push(ix);
                    c.series_by_id.insert(it.id, ix);
                    c.series.push(it);
                    return;
                }
            }
            let ix = c.series.len();
            c.series_by_id.entry(it.id).or_insert(ix);
            c.series.push(it);
        });
        // Pass 2 (series): attach clean subtitle-only 4K / Dolby Audio upgrade variants onto existing English shows by TMDB ID
        let mut attached_sub_4k = 0usize;
        for (tmdb, v, added) in sub_market_candidates {
            if let Some(&ix) = c.series_by_tmdb.get(&tmdb) {
                let prev = &mut c.series[ix];
                if prev.id != v.id && !prev.variants.iter().any(|x| x.id == v.id) {
                    let vid = v.id;
                    prev.variants.push(v);
                    prev.added = prev.added.max(added);
                    c.series_by_id.insert(vid, ix);
                    attached_sub_4k += 1;
                }
            }
        }

        // Live (fold superscripts to clean ASCII and populate uhd/hdr/dv/dolby_audio from raw tags)
        let aliases = crate::epgx::load_aliases(data);
        stream_json_array::<RawLive>(&dir, "get_live_streams", |l| {
            let cat_id = l.category_id;
            let cat_key = format!("live:{cat_id}");
            let Some(cat) = c.cats.get_mut(&cat_key) else { return };
            let raw_name = l.name;
            let name = re!(r"\s{2,}").replace_all(&crate::showcase::fold_superscripts(&raw_name), " ").trim().to_string();
            if name.is_empty() || name.starts_with('#') { return; }
            cat.count += 1;
            let raw_cat = raw_cats.get(&cat_key).map(String::as_str).unwrap_or("");
            let dv = crate::showcase::is_dv_tag(&raw_name, raw_cat);
            let hdr = dv || crate::showcase::is_hdr_tag(&raw_name, raw_cat) || re!(r"(?i)\b(?:hdr10\+?|hdr|hlg)\b").is_match(&name);
            let dolby_audio = crate::showcase::is_dolby_audio_tag(&raw_name, raw_cat);
            let uhd = cat.uhd || dv || hdr || crate::showcase::is_uhd_tag(&raw_name, raw_cat);
            let sid = l.stream_id;
            let it = Item {
                k: "l", id: sid, title: name, poster: l.stream_icon, cat: cat_id,
                epg: aliases.get(&sid).cloned().unwrap_or(l.epg_channel_id),
                en: cat.en, sport: cat.sport, uhd, hdr, dv, dolby_audio, added: l.added,
                archive: if l.tv_archive == 1 { l.tv_archive_duration.max(1) } else { 0 },
                ..Default::default()
            };
            c.live_by_id.insert(it.id, c.live.len());
            c.live.push(it);
        });
        c.loaded_at = crate::util::now_ms();
        println!("[catalog] {} movies (+{} cross-market 4K/HDR/Dolby), {} series (+{} cross-market 4K/Dolby), {} channels in {}ms", c.movies.len(), attached_movie_4k, c.series.len(), attached_sub_4k, c.live.len(), t0.elapsed().as_millis());
        c
    }

    pub fn cat_name(&self, kind: &str, id: &str) -> String { self.cats.get(&format!("{kind}:{id}")).map(|c| c.name.clone()).unwrap_or_default() }

    /// Canonical platforms/studios for an item, checking both primary `cat` and all `variants`' categories.
    pub fn item_platforms(&self, it: &Item) -> Vec<&'static str> {
        let kind = match it.k { "s" => "series", "l" => "live", _ => "movie" };
        let mut out: Vec<&'static str> = vec![];
        let mut add = |cat_id: &str| {
            if let Some(c) = self.cats.get(&format!("{kind}:{cat_id}")) {
                if let Some(p) = platform_from_cat(&c.name) {
                    if !out.contains(&p) { out.push(p); }
                }
            }
        };
        add(&it.cat);
        let raw = match it.k { "s" => Some(&self.series_raw), "m" => Some(&self.movie_raw), _ => None };
        if let Some(raw) = raw {
            for v in &it.variants {
                if let Some(vit) = raw.get(&v.id) {
                    add(&vit.cat);
                }
            }
        }
        out
    }

    pub fn item_has_platform(&self, it: &Item, q: &str) -> bool {
        let ql = q.trim();
        if ql.is_empty() { return false; }
        let plats = self.item_platforms(it);
        if plats.is_empty() { return false; }
        if let Some(canon) = platform_canonical(ql) {
            return plats.contains(&canon);
        }
        plats.iter().any(|p| p.eq_ignore_ascii_case(ql))
    }

    /// Primary (de-duplicated) record for any stream id (O(1) via `movie_by_id` across primaries & all variants).
    pub fn movie(&self, id: i64) -> Option<&Item> {
        if let Some(&ix) = self.movie_by_id.get(&id) {
            if let Some(m) = self.movies.get(ix) {
                return Some(m);
            }
        }
        let raw = self.movie_raw.get(&id)?;
        if raw.en { if let Some(t) = &raw.tmdb { if let Some(&ix) = self.movie_by_tmdb.get(t) { return self.movies.get(ix); } } }
        self.movies.iter().find(|m| m.id == id).or(Some(raw))
    }
    /// Primary (de-duplicated) record for any series id (O(1) via `series_by_id` across primaries & all variants).
    pub fn series_item(&self, id: i64) -> Option<&Item> {
        if let Some(&ix) = self.series_by_id.get(&id) {
            if let Some(m) = self.series.get(ix) {
                return Some(m);
            }
        }
        let raw = self.series_raw.get(&id)?;
        if raw.en { if let Some(t) = &raw.tmdb { if let Some(&ix) = self.series_by_tmdb.get(t) { return self.series.get(ix); } } }
        self.series.iter().find(|m| m.id == id).or(Some(raw))
    }
    pub fn by_tmdb(&self, mt: &str, id: &str) -> Option<&Item> {
        if mt == "tv" { self.series_by_tmdb.get(id).map(|&i| &self.series[i]) } else { self.movie_by_tmdb.get(id).map(|&i| &self.movies[i]) }
    }
    pub fn live_item(&self, id: i64) -> Option<&Item> { self.live_by_id.get(&id).map(|&i| &self.live[i]) }

    /// Unified rich search: NFD/diacritic + punctuation folding (`spiderman` -> `Spider-Man`, `amelie` -> `Amélie`),
    /// acronym expansion (`lotr`, `aot`, `jjk`, `fmab`, `mha`, `got`, `himym`, `bcs`, `bb`, `hxh`, `ds`, `opm`, `sds`, `7ds`, `dbz`, `dbs`, `sao`, `csm`),
    /// Damerau-Levenshtein short-word edit distance (`<= 1` for len 4..=6, `<= 2` for len >= 7 so `duen` matches `Dune`),
    /// and matching against `meta.orig_title` (romaji/Japanese/world cinema titles) via `meta_orig`.
    pub fn search_rich<'a, 'b, F>(&'a self, q: &str, all: bool, limit: usize, meta_orig: F) -> (Vec<&'a Item>, Vec<&'a Item>, Vec<&'a Item>)
    where
        F: Fn(&Item) -> Option<&'b str>,
    {
        let q_trim = q.trim();
        if q_trim.is_empty() { return (vec![], vec![], vec![]); }
        let q_norm = crate::util::norm(q_trim);
        let q_lower = q_trim.to_lowercase();
        let has_non_ascii_q = q_trim.chars().any(|c| !c.is_ascii() && c.is_alphabetic());
        if q_norm.is_empty() && !has_non_ascii_q { return (vec![], vec![], vec![]); }

        let q_terms: Vec<&str> = q_norm.split_whitespace().collect();
        let q_compact: String = q_norm.chars().filter(|c| !c.is_whitespace()).collect();
        let q_term_masks: Vec<u32> = q_terms.iter().map(|w| ascii_mask(w)).collect();
        let q_compact_mask = ascii_mask(&q_compact);

        // Acronym expansions (whole query, or leading >=3-char acronym + remaining terms)
        let mut expansions: Vec<String> = vec![];
        if let Some(alts) = expand_acronym(&q_norm) {
            for a in alts { expansions.push((*a).to_string()); }
        } else if q_terms.len() > 1 && q_terms[0].len() >= 3 {
            if let Some(alts) = expand_acronym(q_terms[0]) {
                let tail = q_terms[1..].join(" ");
                for a in alts { expansions.push(format!("{a} {tail}")); }
            }
        }
        let exp_terms: Vec<Vec<&str>> = expansions.iter().map(|e| e.split_whitespace().collect()).collect();

        let score_text = |text_norm: &str, text_compact: &str, raw_lower: Option<&str>, allow_fuzzy: bool| -> Option<f64> {
            // Non-ASCII literal script query (e.g. kanji/kana/hangul/cyrillic against orig_title)
            if has_non_ascii_q && q_norm.is_empty() {
                let rl = raw_lower?;
                if rl == q_lower { return Some(950.0); }
                if rl.starts_with(&q_lower) { return Some(500.0); }
                if rl.contains(&q_lower) { return Some(260.0); }
                return None;
            }
            if text_norm.is_empty() {
                if let Some(rl) = raw_lower {
                    if has_non_ascii_q && rl.contains(&q_lower) { return Some(260.0); }
                }
                return None;
            }
            // 1. Exact / prefix normalized or compact match (`spiderman` == `Spider-Man`, `amelie` == `Amélie`)
            if text_norm == q_norm || (!q_compact.is_empty() && text_compact == q_compact) {
                return Some(1000.0);
            }
            if text_norm.starts_with(&q_norm) || (q_compact.len() >= 4 && text_compact.starts_with(&q_compact)) {
                return Some(520.0);
            }
            // 2. Acronym expansion match
            for (ei, et) in exp_terms.iter().enumerate() {
                let exp_s = &expansions[ei];
                if text_norm == *exp_s { return Some(900.0); }
                if text_norm.starts_with(exp_s) { return Some(480.0); }
                if !et.is_empty() && et.iter().all(|w| text_norm.contains(*w)) {
                    return Some(320.0);
                }
            }
            // 3. All query terms present via substring or compact form
            let mut exact_all = true;
            for w in &q_terms {
                if !text_norm.contains(*w) && !(w.len() >= 4 && text_compact.contains(*w)) {
                    exact_all = false;
                    break;
                }
            }
            if exact_all {
                let bonus = if text_norm.contains(&q_norm) { 60.0 } else { 0.0 };
                return Some(250.0 + bonus);
            }
            if q_compact.len() >= 4 && text_compact.contains(&q_compact) {
                return Some(260.0);
            }
            // 4. Non-ASCII direct match fallback
            if has_non_ascii_q {
                if let Some(rl) = raw_lower {
                    if rl.contains(&q_lower) { return Some(260.0); }
                }
            }
            // 5. Short-word Damerau-Levenshtein edit distance (`<= 1` for len 4..=6, `<= 2` for len >= 7)
            if allow_fuzzy && !q_terms.is_empty() {
                let words: Vec<&str> = text_norm.split_whitespace().collect();
                let mut total_dist = 0usize;
                let mut all_ok = true;
                for (ti, &w) in q_terms.iter().enumerate() {
                    if text_norm.contains(w) || (w.len() >= 4 && text_compact.contains(w)) {
                        continue;
                    }
                    let max_d = fuzzy_max_dist(w.len());
                    if max_d == 0 { all_ok = false; break; }
                    let wm = q_term_masks[ti];
                    let mut best = usize::MAX;
                    for &tw in &words {
                        if let Some(d) = osa_bounded(w, tw, wm, max_d) {
                            if d < best { best = d; if best == 1 { break; } }
                        }
                    }
                    if best > max_d && q_terms.len() == 1 && q_compact.len() >= 4 {
                        if let Some(d) = osa_bounded(&q_compact, text_compact, q_compact_mask, max_d) {
                            best = d;
                        }
                    }
                    if best <= max_d {
                        total_dist += best;
                    } else {
                        all_ok = false;
                        break;
                    }
                }
                if all_ok {
                    return Some((110.0 - (total_dist as f64) * 30.0).max(20.0));
                }
            }
            None
        };

        let run = |list: &'a [Item], check_orig: bool, allow_fuzzy: bool| -> Vec<&'a Item> {
            let mut out: Vec<(&Item, f64)> = list.iter().filter(|x| all || x.en || x.sport).filter_map(|x| {
                let t_norm = crate::util::norm(&x.title);
                let t_compact: String = t_norm.chars().filter(|c| !c.is_whitespace()).collect();
                let mut best = score_text(&t_norm, &t_compact, None, allow_fuzzy);
                if check_orig {
                    if let Some(orig) = meta_orig(x) {
                        if !orig.is_empty() {
                            let o_norm = crate::util::norm(orig);
                            let o_compact: String = o_norm.chars().filter(|c| !c.is_whitespace()).collect();
                            let o_low = if has_non_ascii_q { Some(orig.to_lowercase()) } else { None };
                            if let Some(os) = score_text(&o_norm, &o_compact, o_low.as_deref(), allow_fuzzy) {
                                // Slight discount vs primary English title so native title wins ties
                                let adj = os * 0.92;
                                if best.map_or(true, |b| adj > b) { best = Some(adj); }
                            }
                        }
                    }
                }
                let base = best?;
                let r = base + if x.en { 100.0 } else { 0.0 } + x.year.unwrap_or(0) as f64 / 100.0 + x.rating;
                Some((x, r))
            }).collect();
            out.sort_by(|a, b| b.1.total_cmp(&a.1));
            out.into_iter().take(limit).map(|x| x.0).collect()
        };

        (run(&self.movies, true, true), run(&self.series, true, true), run(&self.live, false, false))
    }

    pub fn search<'a>(&'a self, q: &str, all: bool, limit: usize) -> (Vec<&'a Item>, Vec<&'a Item>, Vec<&'a Item>) {
        self.search_rich(q, all, limit, |_| None)
    }
}

fn expand_acronym(q_norm: &str) -> Option<&'static [&'static str]> {
    match q_norm {
        "lotr" => Some(&["lord of the rings"]),
        "aot" => Some(&["attack on titan", "shingeki no kyojin"]),
        "jjk" => Some(&["jujutsu kaisen"]),
        "fmab" => Some(&["fullmetal alchemist brotherhood"]),
        "fma" => Some(&["fullmetal alchemist"]),
        "mha" | "bnha" => Some(&["my hero academia", "boku no hero academia"]),
        "got" => Some(&["game of thrones"]),
        "hotd" => Some(&["house of the dragon"]),
        "himym" => Some(&["how i met your mother"]),
        "bcs" => Some(&["better call saul"]),
        "bb" => Some(&["breaking bad"]),
        "hxh" => Some(&["hunter x hunter"]),
        "ds" => Some(&["demon slayer", "kimetsu no yaiba"]),
        "opm" => Some(&["one punch man"]),
        "sds" | "7ds" => Some(&["seven deadly sins", "nanatsu no taizai"]),
        "dbz" => Some(&["dragon ball z"]),
        "dbs" => Some(&["dragon ball super"]),
        "sao" => Some(&["sword art online"]),
        "csm" => Some(&["chainsaw man"]),
        "tlou" => Some(&["the last of us"]),
        "twd" => Some(&["the walking dead"]),
        "iasip" => Some(&["its always sunny in philadelphia"]),
        _ => None,
    }
}

fn fuzzy_max_dist(len: usize) -> usize {
    if len >= 7 { 2 } else if len >= 4 { 1 } else { 0 }
}

fn ascii_mask(s: &str) -> u32 {
    let mut m = 0u32;
    for b in s.bytes() {
        if b.is_ascii_lowercase() { m |= 1u32 << (b - b'a'); }
        else if b.is_ascii_digit() { m |= 1u32 << (26 + (b - b'0').min(5)); }
    }
    m
}

/// Bounded Optimal String Alignment (Damerau-Levenshtein) distance on ASCII strings.
/// Treats adjacent character transpositions (e.g. `duen` <-> `dune`) as distance 1.
fn osa_bounded(a: &str, b: &str, mask_a: u32, max_d: usize) -> Option<usize> {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (la, lb) = (ab.len(), bb.len());
    if la.abs_diff(lb) > max_d || la > 48 || lb > 48 { return None; }
    let mask_b = ascii_mask(b);
    if (mask_a & !mask_b).count_ones() as usize > max_d || (mask_b & !mask_a).count_ones() as usize > max_d {
        return None;
    }
    let mut prev2 = [0u8; 50];
    let mut prev1 = [0u8; 50];
    let mut cur = [0u8; 50];
    for j in 0..=lb { prev1[j] = j as u8; }
    for i in 1..=la {
        cur[0] = i as u8;
        let mut row_min = cur[0];
        for j in 1..=lb {
            let cost = if ab[i - 1] == bb[j - 1] { 0u8 } else { 1u8 };
            let mut v = (prev1[j] + 1).min(cur[j - 1] + 1).min(prev1[j - 1] + cost);
            if i > 1 && j > 1 && ab[i - 1] == bb[j - 2] && ab[i - 2] == bb[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
            if v < row_min { row_min = v; }
        }
        if (row_min as usize) > max_d { return None; }
        prev2[..=lb].copy_from_slice(&prev1[..=lb]);
        prev1[..=lb].copy_from_slice(&cur[..=lb]);
    }
    let d = prev1[lb] as usize;
    (d <= max_d).then_some(d)
}

fn swap_primary(prev: &mut Item, it: &Item) {
    let replacing_cam = prev.cam && !it.cam;
    let keep = Variant { id: prev.id, uhd: prev.uhd, hdr: prev.hdr, cam: prev.cam, dv: prev.dv, bluray: prev.bluray, dolby_audio: prev.dolby_audio, sub_market: false };
    prev.id = it.id; prev.cat = it.cat.clone(); prev.ext = it.ext.clone();
    prev.uhd = it.uhd; prev.hdr = it.hdr; prev.cam = it.cam; prev.dv = it.dv; prev.bluray = it.bluray; prev.dolby_audio = it.dolby_audio;
    if replacing_cam {
        if !it.title.is_empty() { prev.title = it.title.clone(); }
        if it.year.is_some() { prev.year = it.year; }
        if it.rating > 0.0 { prev.rating = it.rating; }
    } else {
        if prev.year.is_none() && it.year.is_some() { prev.year = it.year; }
        if prev.rating <= 0.0 && it.rating > 0.0 { prev.rating = it.rating; }
    }
    if !it.poster.is_empty() { prev.poster = it.poster.clone(); }
    if !it.trailer.is_empty() { prev.trailer = it.trailer.clone(); }
    if !it.backdrop.is_empty() { prev.backdrop = it.backdrop.clone(); }
    if !it.genre.is_empty() { prev.genre = it.genre.clone(); }
    if !it.plot.is_empty() { prev.plot = it.plot.clone(); }
    prev.variants.retain(|v| v.id != it.id);
    if keep.id != prev.id && !prev.variants.iter().any(|v| v.id == keep.id) {
        prev.variants.push(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_preserves_real_franchise_prefixes_and_strips_iptv_tags() {
        assert_eq!(clean_title("EN - CSI: Miami").title, "CSI: Miami");
        assert_eq!(clean_title("CSI: Miami").title, "CSI: Miami");
        assert_eq!(clean_title("EN - NCIS: Sydney (2023)").title, "NCIS: Sydney");
        assert_eq!(clean_title("NCIS: Los Angeles").title, "NCIS: Los Angeles");
        assert_eq!(clean_title("EN - FBI: Most Wanted").title, "FBI: Most Wanted");
        assert_eq!(clean_title("EN - 9-1-1: Lone Star").title, "9-1-1: Lone Star");
        assert_eq!(clean_title("EN - 24: Live Another Day").title, "24: Live Another Day");
        assert_eq!(clean_title("EN - X-MEN: First Class (2011)").title, "X-MEN: First Class");
        assert_eq!(clean_title("EN - Law & Order: Special Victims Unit").title, "Law & Order: Special Victims Unit");
        assert_eq!(clean_title("EN - STAR: Trek").title, "STAR: Trek");
        assert_eq!(clean_title("EN - S.W.A.T.: Under Siege").title, "S.W.A.T.: Under Siege");
        assert_eq!(clean_title("EN - SEAL: Team").title, "SEAL: Team");
        assert_eq!(clean_title("EN - M*A*S*H (1972)").title, "M*A*S*H");
        assert_eq!(clean_title("EN - ER: Emergency").title, "ER: Emergency");
        assert_eq!(clean_title("EN - V: The Final Battle").title, "V: The Final Battle");
        assert_eq!(clean_title("IT: Chapter Two (2019)").title, "IT: Chapter Two");

        // Real IPTV prefixes DO get stripped
        assert_eq!(clean_title("4K-NF - Extraction (2020)").title, "Extraction");
        assert_eq!(clean_title("EN-CAM - Superman (2025)").title, "Superman");
        assert_eq!(clean_title("US | The Bear (2022)").title, "The Bear");
        assert_eq!(clean_title("4K | Dune: Part Two (2024)").title, "Dune: Part Two");
    }

    #[test]
    fn clean_title_fixes_origin_and_strips_cam_tags() {
        let hd = clean_title("EN - Dune (2021) (HD)");
        assert_eq!(hd.title, "Dune");
        assert_eq!(hd.origin, "");
        assert_eq!(hd.year, Some(2021));

        for tag in ["(HD)", "(DV)", "(TV)", "(TS)", "(II)", "(3D)", "(4K)", "(SD)", "(CAM)"] {
            let cl = clean_title(&format!("EN - Some Film (2024) {tag}"));
            assert_eq!(cl.origin, "", "tag {tag} must NOT be parsed as origin country");
        }

        let us = clean_title("EN - The Office (US) (2005)");
        assert_eq!(us.title, "The Office");
        assert_eq!(us.origin, "US");
        assert_eq!(us.year, Some(2005));

        let cam1 = clean_title("EN - Ghostbusters: Frozen Empire (2024) (Cam)");
        assert_eq!(cam1.title, "Ghostbusters: Frozen Empire");
        let cam2 = clean_title("EN - Mission: Impossible (2025) (Telesync)");
        assert_eq!(cam2.title, "Mission: Impossible");
        let cam3 = clean_title("EN - New Film (2025) (Clean Audio)");
        assert_eq!(cam3.title, "New Film");
        let cam4 = clean_title("EN - New Film (2025) (HDTS)");
        assert_eq!(cam4.title, "New Film");
        let cam5 = clean_title("EN - New Film (2025) (HDCAM)");
        assert_eq!(cam5.title, "New Film");
    }

    #[test]
    fn catalog_load_resolves_variants_in_o1_and_avoids_remake_collisions() {
        let tmp = std::env::temp_dir().join(format!("tv_cat_test_{}_{}", std::process::id(), crate::util::now_ms()));
        let raw = tmp.join("raw");
        std::fs::create_dir_all(&raw).unwrap();

        std::fs::write(raw.join("get_live_categories.json"), r#"[{"category_id":"1","category_name":"UK| SPORTS 4K"}]"#).unwrap();
        std::fs::write(raw.join("get_vod_categories.json"), r#"[
            {"category_id":"10","category_name":"EN - NEW RELEASE"},
            {"category_id":"11","category_name":"EN - ACTION"},
            {"category_id":"12","category_name":"NORDIC FILM 4K 3840P DOLBY AUDIO"}
        ]"#).unwrap();
        std::fs::write(raw.join("get_series_categories.json"), r#"[
            {"category_id":"20","category_name":"ENGLISH SERIES"},
            {"category_id":"21","category_name":"NORDIC SERIES 4K 3840P DOLBY AUDIO"}
        ]"#).unwrap();
        std::fs::write(raw.join("get_vod_streams.json"), r#"[
            {"stream_id":100,"category_id":"10","name":"EN-CAM - Superman (2025) (Cam)","tmdb":"","rating":"4.0","added":1000,"container_extension":"mp4"},
            {"stream_id":101,"category_id":"11","name":"EN - Superman (2025)","tmdb":"1061474","rating":"8.2","added":2000,"container_extension":"mkv"},
            {"stream_id":102,"category_id":"12","name":"NO - Superman (2025)","tmdb":"1061474","rating":"8.2","added":2500,"container_extension":"mkv"}
        ]"#).unwrap();
        std::fs::write(raw.join("get_series.json"), r#"[
            {"series_id":200,"category_id":"20","name":"EN - The Office (UK) (2001)","tmdb":"","releaseDate":"2001-07-09","rating":"8.5","last_modified":100},
            {"series_id":201,"category_id":"20","name":"EN - The Office (US) (2005)","tmdb":"","releaseDate":"2005-03-24","release_date":"2005-03-24","backdrop_path":false,"rating":"8.9","last_modified":200},
            {"series_id":202,"category_id":"20","name":"EN - Doctor Who (1963)","tmdb":"","releaseDate":"1963-11-23","rating":"8.0","last_modified":100},
            {"series_id":203,"category_id":"20","name":"EN - Doctor Who (2005)","tmdb":"57243","releaseDate":"2005-03-26","rating":"8.6","last_modified":200},
            {"series_id":204,"category_id":"21","name":"NO - Doctor Who (2005)","tmdb":"57243","releaseDate":"2005-03-26","rating":"8.6","last_modified":300}
        ]"#).unwrap();
        std::fs::write(raw.join("get_live_streams.json"), r#"[
            {"stream_id":300,"category_id":"1","name":"UK| SKY SPORTS MAIN EVENT 4K HDR","epg_channel_id":"sky.uk","added":100,"tv_archive":1,"tv_archive_duration":3}
        ]"#).unwrap();

        let cat = Catalog::load(&tmp);

        // Clean primary replaced CAM primary and copied clean rating/title/id
        let sup = cat.movie(101).expect("primary movie 101");
        assert_eq!(sup.id, 101);
        assert!(!sup.cam);
        assert_eq!(sup.rating, 8.2);
        // Both CAM variant 100 and Nordic 4K variant 102 resolve to canonical movie 101 in O(1)!
        assert_eq!(cat.movie(100).map(|m| m.id), Some(101));
        assert_eq!(cat.movie(102).map(|m| m.id), Some(101));
        assert!(sup.variants.iter().any(|v| v.id == 102 && v.uhd && v.sub_market));

        // The Office UK (2001) & US (2005) and Doctor Who 1963 & 2005 did NOT collide!
        assert_ne!(cat.series_item(200).map(|s| s.id), cat.series_item(201).map(|s| s.id));
        assert_ne!(cat.series_item(202).map(|s| s.id), cat.series_item(203).map(|s| s.id));
        // Nordic 4K series variant 204 resolves to canonical EN series 203!
        assert_eq!(cat.series_item(204).map(|s| s.id), Some(203));

        // Live item carries uhd/hdr flags
        let ch = cat.live_item(300).expect("live channel 300");
        assert!(ch.uhd);
        assert!(ch.hdr);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn search_rich_supports_diacritics_punctuation_acronyms_fuzzy_and_orig_title() {
        let mut cat = Catalog::default();
        let add_m = |cat: &mut Catalog, id: i64, title: &str, year: i32| {
            let ix = cat.movies.len();
            let it = Item { k: "m", id, title: title.into(), year: Some(year), en: true, rating: 8.0, ..Default::default() };
            cat.movie_by_id.insert(id, ix);
            cat.movies.push(it);
        };
        let add_s = |cat: &mut Catalog, id: i64, title: &str, year: i32| {
            let ix = cat.series.len();
            let it = Item { k: "s", id, title: title.into(), year: Some(year), en: true, rating: 8.5, ..Default::default() };
            cat.series_by_id.insert(id, ix);
            cat.series.push(it);
        };

        add_m(&mut cat, 1, "Spider-Man: Into the Spider-Verse", 2018);
        add_m(&mut cat, 2, "Amélie", 2001);
        add_m(&mut cat, 3, "Dune", 2021);
        add_m(&mut cat, 4, "The Lord of the Rings: The Fellowship of the Ring", 2001);
        add_m(&mut cat, 5, "Spirited Away", 2001);
        add_s(&mut cat, 10, "Attack on Titan", 2013);
        add_s(&mut cat, 11, "Jujutsu Kaisen", 2020);
        add_s(&mut cat, 12, "Better Call Saul", 2015);
        add_s(&mut cat, 13, "Breaking Bad", 2008);

        let orig_map: HashMap<i64, &str> = HashMap::from([
            (5, "Sen to Chihiro no Kamikakushi"),
            (10, "Shingeki no Kyojin"),
        ]);

        // Punctuation folding
        let (m, _, _) = cat.search("spiderman", false, 10);
        assert_eq!(m.first().map(|x| x.id), Some(1));

        // Diacritic folding
        let (m, _, _) = cat.search("amelie", false, 10);
        assert_eq!(m.first().map(|x| x.id), Some(2));

        // Short-word transposition fuzzy match (`duen` -> `Dune`)
        let (m, _, _) = cat.search("duen", false, 10);
        assert_eq!(m.first().map(|x| x.id), Some(3));

        // Acronyms
        assert_eq!(cat.search("lotr", false, 10).0.first().map(|x| x.id), Some(4));
        assert_eq!(cat.search("aot", false, 10).1.first().map(|x| x.id), Some(10));
        assert_eq!(cat.search("jjk", false, 10).1.first().map(|x| x.id), Some(11));
        assert_eq!(cat.search("bcs", false, 10).1.first().map(|x| x.id), Some(12));
        assert_eq!(cat.search("bb", false, 10).1.first().map(|x| x.id), Some(13));

        // Original / romaji title lookup
        let (m, s, _) = cat.search_rich("Shingeki no Kyojin", false, 10, |it| orig_map.get(&it.id).copied());
        assert!(m.is_empty());
        assert_eq!(s.first().map(|x| x.id), Some(10));
        let (m, _, _) = cat.search_rich("Sen to Chihiro", false, 10, |it| orig_map.get(&it.id).copied());
        assert_eq!(m.first().map(|x| x.id), Some(5));
    }
}
