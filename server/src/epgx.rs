// Extra TV guide data. The provider's guide covers only a small subset of AU/regional channels, so we
// load free public XMLTV feeds (default: epgshare01 AU, or any custom `extraEpg` URLs in config.json)
// and match them to channels missing EPG data. Matched channels get the extra guide's id ("x:<id>"),
// so the guide, now/next, Tonight and sports pages all pick it up automatically.
use crate::app::App;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_SOURCES: &[&str] = &["https://epgshare01.online/epgshare01/epg_ripper_AU1.xml.gz"];

pub fn files(data: &Path) -> Vec<PathBuf> {
    let dir = data.join("raw");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("epgx_") && n.ends_with(".xml"))).collect();
    v.sort(); v
}

fn has_custom_sources(app: &App) -> bool {
    app.cfg.get("extraEpg").as_array().map_or(false, |a| !a.is_empty())
}

fn sources(app: &App) -> Vec<String> {
    let c = app.cfg.get("extraEpg");
    c.as_array()
        .filter(|a| !a.is_empty())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_else(|| DEFAULT_SOURCES.iter().map(|s| s.to_string()).collect())
}

/// Download the extra guides (at most every 12 h). Returns true if anything changed.
pub async fn refresh(app: &App) -> bool {
    let mut changed = false;
    for (i, url) in sources(app).iter().enumerate() {
        let dest = app.data.join("raw").join(format!("epgx_{i}.xml"));
        let age = std::fs::metadata(&dest).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(u64::MAX);
        if age < 12 * 3600 { continue; }
        let Ok(r) = app.http.get(url).timeout(std::time::Duration::from_secs(120)).send().await else { continue };
        if !r.status().is_success() { continue; }
        let Ok(bytes) = r.bytes().await else { continue };
        let xml = if bytes.starts_with(&[0x1f, 0x8b]) {
            use std::io::Read; let mut s = String::new();
            if flate2::read::GzDecoder::new(&bytes[..]).read_to_string(&mut s).is_err() { continue; } s
        } else { String::from_utf8_lossy(&bytes).to_string() };
        if !xml.contains("<programme") { continue; }
        if crate::store::atomic_write(&dest, xml.as_bytes()).is_ok() {
            changed = true;
            println!("[epgx] updated {url}");
        }
    }
    changed
}

/// Normalise a channel name for matching: lower case, no country prefix, quality tags or punctuation.
pub fn norm(n: &str) -> String {
    let mut t = n.to_lowercase();
    t = crate::re!(r"^\s*[a-z]{2,3}\s*[:|]\s*").replace(&t, "").to_string();
    t = crate::re!(r"\((nz|au|uk|us|ca)\)|\b(hd|fhd|uhd|4k|sd|raw|\d{2}\s?fps)\b|ᴴᴰ|ᶠᴴᴰ|ᵁᴴᴰ|ᴿᴬᵂ|◉|[⁰¹²³⁴⁵⁶⁷⁸⁹ᶠᵖˢ]").replace_all(&t, " ").to_string();
    t = crate::re!(r"\bchannel\s*(\d)").replace_all(&t, "$1").to_string();
    t = crate::re!(r"[^a-z0-9+ ]").replace_all(&t, " ").to_string();
    crate::re!(r"\s+").replace_all(t.trim(), " ").to_string()
}

const AU_ALIASES: &[(&str, &str)] = &[
    ("fox sports 501", "fox cricket"), ("fox sports 502", "fox league"),
    ("fox sports 504", "fox footy"), ("fox sports 507", "fox sports more"),
    ("espn 1", "espn"), ("espn 2", "espn2"), ("sbs", "sbs"),
    ("racing", "racing com"), ("10", "10"), ("abc news australia", "abc news"),
];

fn infer_epg_country(ch_id: &str, source_hint: &str) -> String {
    if let Some(c) = crate::re!(r"(?i)\.(au|nz|uk|gb|us|ca|ie|de|fr|es|it|nl|se|no|dk)(?:\d|\b|$)").captures(ch_id) {
        let code = c[1].to_uppercase();
        return if code == "GB" { "UK".to_string() } else { code };
    }
    if let Some(c) = crate::re!(r"(?i)[/_-](AU|NZ|UK|GB|US|CA|IE|DE|FR|ES|IT|NL)\d*(?:\.|_|$)").captures(source_hint) {
        let code = c[1].to_uppercase();
        return if code == "GB" { "UK".to_string() } else { code };
    }
    String::new()
}

fn channel_country(title: &str, cat_name: &str) -> String {
    let from_cat = crate::guide::country_tag(cat_name);
    if !from_cat.is_empty() { return from_cat; }
    crate::guide::country_tag(title)
}

/// Map provider stream id -> "x:<extra guide channel id>" for channels missing EPG.
/// When only the default AU feed is active, matches AU/NZ channels; when custom `extraEpg` URLs
/// are configured, generalizes to ALL English/sports channels missing EPG (matching country first).
pub fn compute(app: &App) -> HashMap<i64, String> {
    let custom = has_custom_sources(app);
    let src_urls = sources(app);
    let city = { let c = app.cfg.str("city"); if c.is_empty() { "sydney".to_string() } else { c.to_lowercase() } };
    let mut names_any: HashMap<String, String> = HashMap::new();
    let mut names_by_cc: HashMap<(String, String), String> = HashMap::new(); // (CC, norm_name) -> id

    for (idx, f) in files(&app.data).iter().enumerate() {
        let src_hint = src_urls.get(idx).map(String::as_str).unwrap_or("");
        let Ok(x) = std::fs::read_to_string(f) else { continue };
        for c in crate::re!(r#"<channel\s+id="([^"]+)"[^>]*>([\s\S]*?)</channel>"#).captures_iter(&x) {
            let cid = crate::sports::decode_xml_entities(&c[1]);
            let cc = infer_epg_country(&cid, src_hint);
            for d in crate::re!(r"<display-name[^>]*>([\s\S]*?)</display-name>").captures_iter(&c[2]) {
                let dn = norm(&crate::sports::decode_xml_entities(&d[1]));
                if dn.is_empty() { continue; }
                names_any.entry(dn.clone()).or_insert_with(|| cid.clone());
                if !cc.is_empty() {
                    names_by_cc.entry((cc.clone(), dn)).or_insert_with(|| cid.clone());
                }
            }
        }
    }
    if names_any.is_empty() { return HashMap::new(); }

    let cat = app.catalog();
    let have = app.epg.read().unwrap().clone();
    let mut out = HashMap::new();

    for ch in cat.live.iter() {
        let cname = cat.cat_name("live", &ch.cat);
        let ch_cc = channel_country(&ch.title, &cname);
        let is_anz = ch_cc == "AU" || ch_cc == "NZ" || crate::re!(r"(?i)^(AU|NZ)\s*\|").is_match(&cname);
        if !custom && !is_anz { continue; }
        if custom && !is_anz && !ch.en && !ch.sport { continue; }
        if !ch.epg.is_empty() && !ch.epg.starts_with("x:") && have.get(&ch.epg).map_or(false, |l| !l.is_empty()) {
            continue;
        }

        let mut n = norm(&ch.title);
        if is_anz {
            // "9NOW 9GEM (NSW)" -> "9gem sydney": streaming-service prefixes off, states -> capital cities
            n = crate::re!(r"^(9now|7plus|10play|abc iview|sbs on demand)\s+").replace(&n, "").to_string();
            for (st, c) in [("nsw", "sydney"), ("vic", "melbourne"), ("qld", "brisbane"), ("wa", "perth"), ("sa", "adelaide"), ("tas", "hobart"), ("act", "canberra"), ("nt", "darwin")] {
                n = crate::re!(r"\b(nsw|vic|qld|wa|sa|tas|act|nt)\b")
                    .replace(&n, |m: &regex::Captures| if &m[1] == st { c.to_string() } else { m[0].to_string() })
                    .to_string();
            }
            for (a, b) in AU_ALIASES { if n == *a { n = b.to_string(); } }
        }
        let national = n.ends_with(" national");
        let base = n.trim_end_matches(" national").to_string();
        let tries = if is_anz {
            vec![n.clone(), format!("{base} {city}"), base.clone(), base.replace(' ', ""), format!("{} {city}", base.replace(' ', ""))]
        } else {
            vec![n.clone(), base.clone(), base.replace(' ', "")]
        };

        // Country-specific extra EPG match first, then global fallback (when untagged or country-compatible)
        let mut hit: Option<&String> = None;
        if !ch_cc.is_empty() {
            hit = tries.iter().find_map(|t| names_by_cc.get(&(ch_cc.clone(), t.clone())));
            if hit.is_none() && !national {
                let compact = n.replace(' ', "");
                hit = names_by_cc.iter()
                    .find(|((cc, k), _)| cc == &ch_cc && k.replace(' ', "") == compact)
                    .map(|(_, v)| v);
            }
        }
        if hit.is_none() && (is_anz || ch_cc.is_empty() || !names_by_cc.keys().any(|(cc, _)| cc == &ch_cc)) {
            hit = tries.iter().find_map(|t| names_any.get(t)).or_else(|| {
                if national {
                    None
                } else {
                    let compact = n.replace(' ', "");
                    names_any.iter().find(|(k, _)| k.replace(' ', "") == compact).map(|(_, v)| v)
                }
            });
        }
        if let Some(id) = hit {
            out.insert(ch.id, format!("x:{id}"));
        }
    }
    out
}

pub fn alias_path(data: &Path) -> PathBuf { data.join("epg-alias.json") }
pub fn load_aliases(data: &Path) -> HashMap<i64, String> {
    std::fs::read(alias_path(data)).ok().and_then(|b| serde_json::from_slice::<HashMap<String, String>>(&b).ok())
        .map(|m| m.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()).unwrap_or_default()
}
pub fn save_aliases(data: &Path, m: &HashMap<i64, String>) -> bool {
    let old = load_aliases(data);
    if &old == m { return false; }
    let s: HashMap<String, &String> = m.iter().map(|(k, v)| (k.to_string(), v)).collect();
    if let Ok(bytes) = serde_json::to_vec(&s) {
        let _ = crate::store::atomic_write(&alias_path(data), &bytes);
    }
    true
}
