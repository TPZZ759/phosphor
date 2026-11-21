// 4K OLED Showcase: bulletproof CAM/Telesync detector, quality-tier classifier,
// CAM-free 4K/Dolby Vision/HDR/Blu-Ray showcase hub + faceted browse, and
// persistent quality-upgrade watcher & history.
use crate::app::App;
use crate::catalog::{Catalog, Item};
use crate::discover::watched_set;
use crate::meta::Meta;
use crate::rec::hidden_ratings;
use crate::util::{now_ms, s, tmdb_img};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const UPGRADES_MAX: usize = 60;
const TRENDING_ALERT_POP: f64 = 40.0;
const TRENDING_ALERT_CAP: usize = 5;

// ---------------------------------------------------------------------------
// 1. Superscript folding & release/quality tag classifiers
// ---------------------------------------------------------------------------

/// Fold Unicode superscript & modifier letters/digits (used heavily in provider
/// category names like `⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ`, `ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ`, `²¹⁶⁰ᵖ ᴴᴰᴿ¹⁰`)
/// into plain ASCII while leaving normal ASCII strings untouched without allocation.
fn fold_superscripts(input: &str) -> Cow<'_, str> {
    if input.is_ascii() {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let mapped = match ch {
            '⁰' => Some('0'),
            '¹' => Some('1'),
            '²' => Some('2'),
            '³' => Some('3'),
            '⁴' => Some('4'),
            '⁵' => Some('5'),
            '⁶' => Some('6'),
            '⁷' => Some('7'),
            '⁸' => Some('8'),
            '⁹' => Some('9'),
            'ᴬ' => Some('A'),
            'ᴮ' => Some('B'),
            'ᴰ' => Some('D'),
            'ᴱ' => Some('E'),
            'ᴳ' => Some('G'),
            'ᴴ' => Some('H'),
            'ᴵ' => Some('I'),
            'ᴶ' => Some('J'),
            'ᴷ' => Some('K'),
            'ᴸ' => Some('L'),
            'ᴹ' => Some('M'),
            'ᴺ' => Some('N'),
            'ᴼ' => Some('O'),
            'ᴾ' => Some('P'),
            'ᴿ' => Some('R'),
            'ᵀ' => Some('T'),
            'ᵁ' => Some('U'),
            'ⱽ' => Some('V'),
            'ᵂ' => Some('W'),
            'ᵃ' => Some('a'),
            'ᵇ' => Some('b'),
            'ᶜ' => Some('c'),
            'ᵈ' => Some('d'),
            'ᵉ' => Some('e'),
            'ᶠ' => Some('f'),
            'ᵍ' => Some('g'),
            'ʰ' => Some('h'),
            'ⁱ' | 'ᶦ' => Some('i'),
            'ʲ' => Some('j'),
            'ᵏ' => Some('k'),
            'ˡ' => Some('l'),
            'ᵐ' => Some('m'),
            'ⁿ' => Some('n'),
            'ᵒ' => Some('o'),
            'ᵖ' => Some('p'),
            'ʳ' => Some('r'),
            'ˢ' => Some('s'),
            'ᵗ' => Some('t'),
            'ᵘ' => Some('u'),
            'ᵛ' => Some('v'),
            'ʷ' => Some('w'),
            'ˣ' => Some('x'),
            'ʸ' => Some('y'),
            'ᶻ' => Some('z'),
            _ => None,
        };
        match mapped {
            Some(c) => out.push(c),
            None => out.push(ch),
        }
    }
    Cow::Owned(out)
}

/// Bulletproof CAM / Telesync / Screener rip detector.
///
/// Must be called on the RAW provider item name and RAW category name BEFORE
/// `catalog::clean_title()` / `catalog::clean_cat()` strips prefixes and brackets.
///
/// Designed for ZERO false positives on real titles such as `"EN - Cam Girls (2021)"`,
/// `"NF - Cam"`, `"EN - Body Cam"`, `"Camera"`, `"Camelot"`, `"Camp Rock"`,
/// `"Tsunami"`, `"Camille"`, `"The Cameraman"`, `"HTC"`, or `"TR - YESIL CAM SINEMA"`.
pub fn is_cam_release(raw_name: &str, raw_cat: &str) -> bool {
    let cat = fold_superscripts(raw_cat);
    if !cat.is_empty() && !crate::re!(r"(?i)ye[sş]il\s*cam").is_match(&cat) {
        if crate::re!(r"(?i)\b(?:cam|camrip|hdcam|hd-?cam|telesync|hdts|hd-?ts|telecine|workprint|dvdscr)\b")
            .is_match(&cat)
        {
            return true;
        }
    }

    let name = fold_superscripts(raw_name);
    if name.is_empty() {
        return false;
    }

    // 1. Provider prefix: `EN-CAM - ...`, `IN-CAM - ...`, `CAM - ...`
    if crate::re!(r"(?i)^\s*(?:[A-Z]{2,3}-CAM|CAM)\s*[-|:]").is_match(&name) {
        return true;
    }

    // 2. Bracketed / parenthesized release tags: `(Cam)`, `(Telesync)`, `[HDTS]`, `(Clean Audio)`, etc.
    if crate::re!(
        r"(?i)[\(\[]\s*(?:cam|camrip|hdcam|hd-?cam|ts|hdts|hd-?ts|telesync|telecine|hdtc|workprint|dvdscr|scr|r5|clean\s*audio|line\s*audio)(?:\s*v\d+)?\s*[\)\]]"
    )
    .is_match(&name)
    {
        return true;
    }

    // 3. Unambiguous standalone release-tag tokens (never real title words)
    crate::re!(r"(?i)\b(?:hdcam|camrip|hd-?cam|hdts|hd-?ts|telesync|telecine|hdtc|workprint|dvdscr)\b")
        .is_match(&name)
}

/// True when raw title or raw category carries a Dolby Vision tag (ASCII or Unicode superscript).
pub fn is_dv_tag(raw_name: &str, raw_cat: &str) -> bool {
    let cat = fold_superscripts(raw_cat);
    if !cat.is_empty() && crate::re!(r"(?i)\b(?:dolby\s*vision|dovi|dv)\b").is_match(&cat) {
        return true;
    }
    let name = fold_superscripts(raw_name);
    if name.is_empty() {
        return false;
    }
    crate::re!(
        r"(?i)(?:\b(?:dolby\s*vision|dovi)\b|[\(\[]\s*dv\s*[\)\]]|\b(?:4k|2160p?|3840p?|uhd|hdr10\+?)\b[\s._-]{0,6}\bdv\b|\bdv\b[\s._-]{0,6}\b(?:4k|2160p?|3840p?|uhd|hdr10\+?)\b)"
    )
    .is_match(&name)
}

/// True when raw title or raw category indicates a Blu-Ray master / remux release.
pub fn is_bluray_tag(raw_name: &str, raw_cat: &str) -> bool {
    let re = crate::re!(r"(?i)\b(?:blu-?ray|bdrip|bdremux|brrip|remux)\b");
    let cat = fold_superscripts(raw_cat);
    if !cat.is_empty() && re.is_match(&cat) {
        return true;
    }
    let name = fold_superscripts(raw_name);
    !name.is_empty() && re.is_match(&name)
}

/// True when raw title or raw category indicates Dolby Audio / Atmos / TrueHD (ASCII or superscript).
pub fn is_dolby_audio_tag(raw_name: &str, raw_cat: &str) -> bool {
    let re = crate::re!(r"(?i)\b(?:dolby\s*(?:audio|atmos|digital|truehd)|atmos|truehd|dd\+|ddp\s*5\.1|eac3)\b");
    let cat = fold_superscripts(raw_cat);
    if !cat.is_empty() && re.is_match(&cat) {
        return true;
    }
    let name = fold_superscripts(raw_name);
    !name.is_empty() && re.is_match(&name)
}

/// True when raw title or raw category indicates HDR10 / HDR10+ / HLG (ASCII or superscript).
pub fn is_hdr_tag(raw_name: &str, raw_cat: &str) -> bool {
    let cat = fold_superscripts(raw_cat);
    if !cat.is_empty() && crate::re!(r"(?i)\b(?:hdr10\+?|hdr|hlg)\b").is_match(&cat) {
        return true;
    }
    let name = fold_superscripts(raw_name);
    !name.is_empty()
        && crate::re!(r"(?i)(?:[\(\[]\s*hdr10\+?\s*[\)\]]|\bhdr10\+?\b|^[A-Z0-9+]+-[A-Z]{2}-HDR\s*-)")
            .is_match(&name)
}

/// True when raw title or raw category indicates 4K / 3840p / 2160p / UHD (ASCII or superscript).
pub fn is_uhd_tag(raw_name: &str, raw_cat: &str) -> bool {
    let re = crate::re!(r"(?i)(?:\b4k\b|3840p?|2160p?|\buhd\b)");
    let cat = fold_superscripts(raw_cat);
    if !cat.is_empty() && re.is_match(&cat) {
        return true;
    }
    let name = fold_superscripts(raw_name);
    !name.is_empty() && re.is_match(&name)
}

// ---------------------------------------------------------------------------
// 2. QualityTier enum & item quality resolution (catalog flags + cached probes)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QualityTier {
    Cam = 0,
    Hd = 1,
    Bluray = 2,
    Uhd4k = 3,
    Hdr4k = 4,
    Dv4k = 5,
}

impl QualityTier {
    pub fn label(&self) -> &'static str {
        match self {
            QualityTier::Cam => "CAM",
            QualityTier::Hd => "HD",
            QualityTier::Bluray => "Blu-Ray",
            QualityTier::Uhd4k => "4K",
            QualityTier::Hdr4k => "4K HDR",
            QualityTier::Dv4k => "4K DV",
        }
    }

    pub fn as_u8(&self) -> u8 {
        *self as u8
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => QualityTier::Cam,
            1 => QualityTier::Hd,
            2 => QualityTier::Bluray,
            3 => QualityTier::Uhd4k,
            4 => QualityTier::Hdr4k,
            5 => QualityTier::Dv4k,
            _ => QualityTier::Hd,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ProbeSummary {
    tier: Option<QualityTier>,
    kbps: Option<u64>,
    hdr: Option<String>,
    codec: Option<String>,
    w: i64,
    h: i64,
    ch: i64,
}

fn flag_tier(cam: bool, uhd: bool, dv: bool, bluray: bool) -> QualityTier {
    if cam {
        QualityTier::Cam
    } else if dv {
        QualityTier::Dv4k
    } else if uhd {
        QualityTier::Uhd4k
    } else if bluray {
        QualityTier::Bluray
    } else {
        QualityTier::Hd
    }
}

fn probe_to_summary(pic: &Value) -> ProbeSummary {
    let w = pic["w"].as_i64().unwrap_or(0);
    let h = pic["h"].as_i64().unwrap_or(0);
    let is_4k = w >= 3600 || h >= 1800;
    let hdr_raw = pic["hdr"].as_str().unwrap_or("").trim();
    let has_dv = !pic["dv"].is_null() || hdr_raw.eq_ignore_ascii_case("DV");
    let has_hdr = !hdr_raw.is_empty();

    let tier = if is_4k && has_dv {
        Some(QualityTier::Dv4k)
    } else if is_4k && has_hdr {
        Some(QualityTier::Hdr4k)
    } else if is_4k {
        Some(QualityTier::Uhd4k)
    } else {
        None
    };

    let kbps = pic["kbps"]
        .as_f64()
        .filter(|&k| k.is_finite() && k >= 100.0)
        .map(|k| k.round() as u64);
    let hdr = if has_dv {
        Some("DV".to_string())
    } else if !hdr_raw.is_empty() {
        Some(hdr_raw.to_string())
    } else {
        None
    };
    let codec = pic["codec"]
        .as_str()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from);
    let ch = pic["ch"].as_i64().unwrap_or(0);

    ProbeSummary { tier, kbps, hdr, codec, w, h, ch }
}

/// True ONLY when the item AND every one of its variants is a CAM/Telesync rip.
/// If a title has both a CAM copy and a clean HD/4K copy, this returns `false`.
pub fn is_cam_only(_app: &App, it: &Item) -> bool {
    is_cam_only_item(it)
}

fn is_cam_only_item(it: &Item) -> bool {
    it.cam && it.variants.iter().all(|v| v.cam)
}

#[derive(Clone, Debug)]
struct ItemQuality {
    tier: QualityTier,
    cam_only: bool,
    is_dv: bool,
    is_bluray: bool,
    has_dolby_audio: bool,
    probe: ProbeSummary,
}

fn assess_quality(app: &App, it: &Item, check_probes: bool) -> ItemQuality {
    let cam_only = is_cam_only_item(it);
    if cam_only {
        return ItemQuality {
            tier: QualityTier::Cam,
            cam_only: true,
            is_dv: false,
            is_bluray: false,
            has_dolby_audio: false,
            probe: ProbeSummary::default(),
        };
    }

    let mut best = flag_tier(it.cam, it.uhd, it.dv, it.bluray);
    let mut is_dv = !it.cam && it.dv;
    let mut is_bluray = !it.cam && it.bluray;
    let mut has_dolby_audio = !it.cam && it.dolby_audio;

    for v in &it.variants {
        if v.cam {
            continue;
        }
        let vt = flag_tier(false, v.uhd, v.dv, v.bluray);
        if vt > best {
            best = vt;
        }
        if v.dv {
            is_dv = true;
        }
        if v.bluray {
            is_bluray = true;
        }
        if v.dolby_audio {
            has_dolby_audio = true;
        }
    }

    let mut best_probe = ProbeSummary::default();
    if check_probes && it.k == "m" && app.stream.probe_count() > 0 {
        let mut consider_probe = |sid: i64| {
            if let Some(pic) = app.stream.quality_of(sid) {
                let ps = probe_to_summary(&pic);
                if let Some(pt) = ps.tier {
                    if pt > best {
                        best = pt;
                    }
                    if pt == QualityTier::Dv4k {
                        is_dv = true;
                    }
                }
                let better = match (ps.tier, best_probe.tier) {
                    (Some(a), Some(b)) if a != b => a > b,
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    _ => ps.kbps.unwrap_or(0) > best_probe.kbps.unwrap_or(0),
                };
                if better || (best_probe.kbps.is_none() && ps.kbps.is_some()) {
                    best_probe = ps;
                }
            }
        };
        if !it.cam {
            consider_probe(it.id);
        }
        for v in &it.variants {
            if !v.cam {
                consider_probe(v.id);
            }
        }
    }

    ItemQuality {
        tier: best,
        cam_only: false,
        is_dv: is_dv || best == QualityTier::Dv4k,
        is_bluray,
        has_dolby_audio,
        probe: best_probe,
    }
}

/// Best available quality tier across an item, all its variants, and cached stream probes.
pub fn best_tier(app: &App, it: &Item) -> QualityTier {
    if is_cam_only_item(it) {
        return QualityTier::Cam;
    }
    let mut best = flag_tier(it.cam, it.uhd, it.dv, it.bluray);
    for v in &it.variants {
        if !v.cam {
            best = best.max(flag_tier(false, v.uhd, v.dv, v.bluray));
        }
    }
    if best < QualityTier::Dv4k && it.k == "m" && app.stream.probe_count() > 0 {
        if !it.cam {
            if let Some(pic) = app.stream.quality_of(it.id) {
                if let Some(pt) = probe_to_summary(&pic).tier {
                    best = best.max(pt);
                }
            }
        }
        for v in &it.variants {
            if best == QualityTier::Dv4k {
                break;
            }
            if !v.cam {
                if let Some(pic) = app.stream.quality_of(v.id) {
                    if let Some(pt) = probe_to_summary(&pic).tier {
                        best = best.max(pt);
                    }
                }
            }
        }
    }
    best
}

// ---------------------------------------------------------------------------
// 3. Internal pool & scoring helpers
// ---------------------------------------------------------------------------

fn current_year() -> i32 {
    chrono::Datelike::year(&chrono::Utc::now())
}

fn is_doc_fast(cat: &Catalog, it: &Item, meta: Option<&Meta>) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") {
        return false;
    }
    if let Some(m) = meta {
        if m.g.contains(&99) {
            return true;
        }
    }
    let kind = if it.k == "s" { "series" } else { "movie" };
    let check_cat = |cid: &str| -> bool {
        let cu = cat.cat_name(kind, cid).to_uppercase();
        if cu.contains("DOCU") || cu.contains("DOCUMENTAR") {
            return meta.map_or(true, |m| !m.g.contains(&16));
        }
        cu.contains("DISCOVERY+") && meta.is_none()
    };
    if check_cat(&it.cat) {
        return true;
    }
    let raw = if it.k == "s" { &cat.series_raw } else { &cat.movie_raw };
    for v in &it.variants {
        if let Some(vit) = raw.get(&v.id) {
            if check_cat(&vit.cat) {
                return true;
            }
        }
    }
    it.genre.to_lowercase().contains("documentary")
}

fn canonical_genre_id(g: i64) -> i64 {
    match g {
        10759 => 28,  // Action & Adventure -> Action
        10765 => 878, // Sci-Fi & Fantasy -> Sci-Fi
        10768 => 10752, // War & Politics -> War
        other => other,
    }
}

fn genre_matches(m_genres: &[i64], g: i64) -> bool {
    if m_genres.contains(&g) {
        return true;
    }
    match g {
        28 | 12 => m_genres.contains(&10759),
        10759 => m_genres.contains(&28) || m_genres.contains(&12),
        878 | 14 => m_genres.contains(&10765),
        10765 => m_genres.contains(&878) || m_genres.contains(&14),
        10752 => m_genres.contains(&10768),
        10768 => m_genres.contains(&10752),
        _ => false,
    }
}

struct ShowcaseEntry {
    item: Item,
    meta: Option<Meta>,
    qual: ItemQuality,
    platforms: Vec<&'static str>,
    is_doc: bool,
    is_nature: bool,
}

impl ShowcaseEntry {
    fn bayes(&self) -> f64 {
        match &self.meta {
            Some(m) if m.va > 0.0 => m.bayes(),
            _ => (self.item.rating / 10.0).clamp(0.0, 1.0) * 6.5,
        }
    }

    fn votes(&self) -> i64 {
        self.meta.as_ref().map(|m| m.vc).unwrap_or(0)
    }

    fn pop(&self) -> f64 {
        self.meta.as_ref().map(|m| m.p).unwrap_or(self.item.rating).max(0.0)
    }

    fn year(&self) -> i32 {
        let my = self.meta.as_ref().map(|m| m.year()).unwrap_or(0);
        if my > 0 { my } else { self.item.year.unwrap_or(0) }
    }

    fn sort_date(&self) -> String {
        if let Some(m) = &self.meta {
            if !m.d.is_empty() {
                return m.d.clone();
            }
        }
        let y = self.year();
        if y > 0 { format!("{y:04}-01-01") } else { String::new() }
    }

    fn has_poster(&self) -> bool {
        !self.item.poster.is_empty() || self.meta.as_ref().map_or(false, |m| !m.pp.is_empty())
    }

    fn has_backdrop(&self) -> bool {
        !self.item.backdrop.is_empty() || self.meta.as_ref().map_or(false, |m| !m.bd.is_empty())
    }

    fn has_overview(&self) -> bool {
        self.meta.as_ref().map_or(false, |m| m.ov.len() >= 20) || self.item.plot.len() >= 20
    }

    fn hidden(&self, hide: &HashSet<String>) -> bool {
        if let Some(t) = &self.item.tmdb {
            let mt = if self.item.k == "s" { "tv" } else { "movie" };
            return hide.contains(&format!("{mt}:{t}"));
        }
        false
    }

    fn showcase_score(&self, cy: i32) -> f64 {
        let mut sc = self.bayes() * 1.2 + (self.pop() + 1.0).log10() * 0.8;
        let yr = self.year();
        if yr >= cy - 1 {
            sc += 0.65;
        } else if yr >= cy - 3 {
            sc += 0.3;
        }
        if self.qual.probe.kbps.unwrap_or(0) >= 15_000 {
            sc += 0.45;
        } else if self.qual.probe.kbps.is_some() {
            sc += 0.2;
        }
        if self.qual.has_dolby_audio {
            sc += 0.2;
        }
        if !self.has_poster() {
            sc -= 3.0;
        }
        sc
    }
}

fn is_nature_doc(it: &Item, meta: Option<&Meta>) -> bool {
    let re = crate::re!(
        r"(?i)\b(nature|wildlife|planet earth|our planet|blue planet|frozen planet|green planet|living planet|life on earth|prehistoric planet|attenborough|ocean|oceans|coral reef|rainforest|savanna|wilderness|national park|volcano|glacier|antarctica|arctic|galapagos|serengeti|deep sea|marine life|animal|animals)\b"
    );
    re.is_match(&it.title)
        || meta.map_or(false, |m| re.is_match(&m.ov))
        || re.is_match(&it.plot)
}

/// Build the full English non-sport showcase candidate pool (drops `app.meta` read lock before returning).
fn build_pool(app: &App) -> (Vec<ShowcaseEntry>, Vec<ShowcaseEntry>, Vec<ShowcaseEntry>) {
    let cat = app.catalog();
    let db = app.meta.read().unwrap();
    let have_probes = app.stream.probe_count() > 0;

    let mut showcase: Vec<ShowcaseEntry> = Vec::new();
    let mut dolby_hd: Vec<ShowcaseEntry> = Vec::new();
    let mut cam_only: Vec<ShowcaseEntry> = Vec::new();

    for (mt, list) in [("movie", &cat.movies), ("tv", &cat.series)] {
        for it in list {
            if !it.en || it.sport {
                continue;
            }
            let qual = assess_quality(app, it, have_probes);
            let keep = qual.cam_only || qual.tier >= QualityTier::Bluray || qual.has_dolby_audio;
            if !keep {
                continue;
            }
            let meta = it
                .tmdb
                .as_ref()
                .and_then(|t| db.get(&format!("{mt}:{t}")))
                .filter(|m| !m.missing);
            let is_doc = is_doc_fast(&cat, it, meta);
            let is_nature = is_doc && is_nature_doc(it, meta);
            let platforms = cat.item_platforms(it);
            let entry = ShowcaseEntry {
                item: it.clone(),
                meta: meta.cloned(),
                qual,
                platforms,
                is_doc,
                is_nature,
            };
            if entry.qual.cam_only {
                cam_only.push(entry);
            } else if entry.qual.tier >= QualityTier::Bluray {
                showcase.push(entry);
            } else {
                dolby_hd.push(entry);
            }
        }
    }
    drop(db);
    (showcase, dolby_hd, cam_only)
}

/// Enrich a catalog item card via `app.card_meta` with showcase quality fields.
pub fn showcase_card(app: &App, it: &Item) -> Value {
    let qual = assess_quality(app, it, app.stream.probe_count() > 0);
    enrich_card(app, it, &qual)
}

fn enrich_card(app: &App, it: &Item, qual: &ItemQuality) -> Value {
    let mut c = app.card_meta(it);
    c["uhd"] = json!(qual.tier >= QualityTier::Uhd4k);
    c["qualityTier"] = json!(qual.tier.as_u8());
    c["qualityLabel"] = json!(qual.tier.label());
    c["isCam"] = json!(qual.cam_only);
    c["isDv"] = json!(qual.is_dv);
    c["isBluray"] = json!(qual.is_bluray);
    c["hasDolbyAudio"] = json!(qual.has_dolby_audio);
    if let Some(kbps) = qual.probe.kbps {
        c["probedKbps"] = json!(kbps);
    }
    if let Some(hdr) = &qual.probe.hdr {
        c["probedHdr"] = json!(hdr);
    }
    if let Some(codec) = &qual.probe.codec {
        c["probedCodec"] = json!(codec);
    }
    if qual.probe.w > 0 && qual.probe.h > 0 {
        c["probedRes"] = json!(format!("{}x{}", qual.probe.w, qual.probe.h));
    }
    if qual.probe.ch > 0 {
        c["probedCh"] = json!(qual.probe.ch);
    }
    c
}

fn push_row(rows: &mut Vec<Value>, id: &str, title: &str, sub: Option<&str>, items: Vec<Value>, min_len: usize) {
    if items.len() < min_len {
        return;
    }
    let mut r = json!({ "id": id, "title": title, "items": items });
    if let Some(sub) = sub {
        r["sub"] = json!(sub);
    }
    rows.push(r);
}

// ---------------------------------------------------------------------------
// 4. 4K OLED Showcase Hub (`hub`)
// ---------------------------------------------------------------------------

/// GET /api/showcase: CAM-free 4K / Dolby Vision / HDR / Blu-Ray OLED Showcase hub.
pub fn hub(app: &App) -> Value {
    let (showcase, dolby_hd, cam_only) = build_pool(app);
    let hide = hidden_ratings(app);
    let cy = current_year();

    let visible: Vec<usize> = showcase
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.hidden(&hide))
        .map(|(i, _)| i)
        .collect();

    // Counts & hub facets across the strictly CAM-free showcase pool.
    let (mut dv_cnt, mut hdr_cnt, mut uhd4k_cnt, mut bluray_cnt, mut movies4k_cnt, mut series4k_cnt, mut docs4k_cnt) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut plat_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut genre_counts: HashMap<i64, usize> = HashMap::new();

    for e in &showcase {
        if e.qual.tier == QualityTier::Dv4k {
            dv_cnt += 1;
        }
        if e.qual.tier == QualityTier::Hdr4k {
            hdr_cnt += 1;
        }
        if e.qual.tier >= QualityTier::Uhd4k {
            uhd4k_cnt += 1;
            if e.item.k == "s" {
                series4k_cnt += 1;
            } else {
                movies4k_cnt += 1;
            }
            if e.is_doc {
                docs4k_cnt += 1;
            }
        }
        if e.qual.is_bluray {
            bluray_cnt += 1;
        }
        for &p in &e.platforms {
            *plat_counts.entry(p).or_default() += 1;
        }
        if let Some(m) = &e.meta {
            let mut seen_g = HashSet::new();
            for &g in &m.g {
                let cg = canonical_genre_id(g);
                if seen_g.insert(cg) {
                    *genre_counts.entry(cg).or_default() += 1;
                }
            }
        }
    }

    let mut plats_vec: Vec<(&str, usize)> = plat_counts.into_iter().collect();
    plats_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let platforms: Vec<Value> = plats_vec
        .into_iter()
        .take(12)
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect();

    let mut genres_vec: Vec<(i64, &str, usize)> = genre_counts
        .into_iter()
        .filter_map(|(id, count)| {
            let name = crate::meta::genre_name(false, id).or_else(|| crate::meta::genre_name(true, id))?;
            Some((id, name, count))
        })
        .collect();
    genres_vec.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(b.1)));
    let genres: Vec<Value> = genres_vec
        .into_iter()
        .take(16)
        .map(|(id, name, count)| json!({ "id": id, "name": name, "count": count }))
        .collect();

    // Hero: highest-scoring 4K Dolby Vision / 4K HDR OLED reference titles with crisp backdrop & overview.
    let mut hero_cands: Vec<(f64, usize)> = Vec::new();
    for &idx in &visible {
        let e = &showcase[idx];
        if !e.has_backdrop() {
            continue;
        }
        let mut sc = e.showcase_score(cy);
        sc += match e.qual.tier {
            QualityTier::Dv4k => 2.4,
            QualityTier::Hdr4k => 2.0,
            QualityTier::Uhd4k => 0.9,
            _ => -1.5,
        };
        if e.has_overview() {
            sc += 0.6;
        } else {
            sc -= 1.2;
        }
        if e.votes() < 30 {
            sc -= 1.2;
        }
        hero_cands.push((sc, idx));
    }
    hero_cands.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut hero_picks: Vec<usize> = Vec::with_capacity(6);
    let mut hero_cols: HashSet<i64> = HashSet::new();
    let (mut mv_h, mut tv_h) = (0usize, 0usize);
    for pass in 0..2 {
        for &(_, idx) in &hero_cands {
            if hero_picks.len() >= 6 {
                break;
            }
            if hero_picks.contains(&idx) {
                continue;
            }
            let e = &showcase[idx];
            if pass == 0 {
                if e.qual.tier < QualityTier::Uhd4k {
                    continue;
                }
                if e.item.k == "m" && mv_h >= 4 {
                    continue;
                }
                if e.item.k == "s" && tv_h >= 4 {
                    continue;
                }
                if let Some(cid) = e.meta.as_ref().and_then(|m| m.col.as_ref()).map(|c| c.id) {
                    if !hero_cols.insert(cid) {
                        continue;
                    }
                }
            }
            if e.item.k == "s" { tv_h += 1; } else { mv_h += 1; }
            hero_picks.push(idx);
        }
        if hero_picks.len() >= 6 {
            break;
        }
    }
    let hero: Vec<Value> = hero_picks
        .into_iter()
        .map(|idx| enrich_card(app, &showcase[idx].item, &showcase[idx].qual))
        .collect();

    // Curated rows
    let mut rows: Vec<Value> = Vec::new();

    // 1. Recently Upgraded to 4K / Clean HD (from data/quality-upgrades.json)
    let upgrades_raw = recent_upgrades(app);
    let upgrades_cnt = upgrades_raw.len();
    if !upgrades_raw.is_empty() {
        let cat = app.catalog();
        let mut up_cards: Vec<Value> = Vec::with_capacity(upgrades_raw.len().min(30));
        let mut seen_up = HashSet::new();
        for u in upgrades_raw.iter().take(30) {
            let key = s(&u["key"]);
            if !seen_up.insert(key) {
                continue;
            }
            let k = s(&u["k"]);
            let id = u["id"].as_i64().unwrap_or(0);
            let tmdb = u["tmdb"].as_str().filter(|x| !x.is_empty());
            let found = tmdb
                .and_then(|t| cat.by_tmdb(if k == "s" { "tv" } else { "movie" }, t))
                .or_else(|| if k == "s" { cat.series_item(id) } else { cat.movie(id) });

            let mut card = if let Some(it) = found {
                if is_cam_only(app, it) {
                    continue;
                }
                showcase_card(app, it)
            } else {
                json!({
                    "k": k,
                    "id": id,
                    "tmdb": u["tmdb"],
                    "title": u["title"],
                    "year": u["year"],
                    "poster": u["poster"],
                    "backdrop": u["backdrop"],
                    "uhd": u["toTier"].as_u64().unwrap_or(0) >= QualityTier::Uhd4k.as_u8() as u64,
                    "qualityTier": u["toTier"],
                    "qualityLabel": u["toLabel"],
                    "isCam": false,
                    "isDv": u["toTier"].as_u64().unwrap_or(0) == QualityTier::Dv4k.as_u8() as u64,
                    "isBluray": u["toTier"].as_u64().unwrap_or(0) == QualityTier::Bluray.as_u8() as u64,
                    "hasDolbyAudio": false,
                })
            };
            let from_l = s(&u["fromLabel"]);
            let to_l = s(&u["toLabel"]);
            card["upgradeBadge"] = json!(format!("{from_l} → {to_l}"));
            card["fromTier"] = u["fromTier"].clone();
            card["fromLabel"] = json!(from_l);
            card["toTier"] = u["toTier"].clone();
            card["toLabel"] = json!(to_l);
            card["upgradedAt"] = u["at"].clone();
            up_cards.push(card);
        }
        push_row(
            &mut rows,
            "upgrades",
            "Recently Upgraded to 4K / Clean HD",
            Some("New 4K / Dolby Vision drops & clean HD replacements"),
            up_cards,
            1,
        );
    }

    // 2. 4K Dolby Vision Films
    let mut dv_mv: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| showcase[i].item.k == "m" && showcase[i].qual.tier == QualityTier::Dv4k)
        .collect();
    dv_mv.sort_by(|a, b| showcase[*b].showcase_score(cy).total_cmp(&showcase[*a].showcase_score(cy)));
    let dv_mv_cards: Vec<Value> = dv_mv
        .into_iter()
        .take(40)
        .map(|i| enrich_card(app, &showcase[i].item, &showcase[i].qual))
        .collect();
    push_row(&mut rows, "dv-movies", "4K Dolby Vision Films", Some("True OLED Dolby Vision · 3840p"), dv_mv_cards, 3);

    // 3. 4K Dolby Vision Series
    let mut dv_tv: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| showcase[i].item.k == "s" && showcase[i].qual.tier == QualityTier::Dv4k)
        .collect();
    dv_tv.sort_by(|a, b| showcase[*b].showcase_score(cy).total_cmp(&showcase[*a].showcase_score(cy)));
    let dv_tv_cards: Vec<Value> = dv_tv
        .into_iter()
        .take(40)
        .map(|i| enrich_card(app, &showcase[i].item, &showcase[i].qual))
        .collect();
    push_row(&mut rows, "dv-series", "4K Dolby Vision Series", Some("Dolby Vision originals & prestige TV"), dv_tv_cards, 3);

    // 4. 4K UHD Movies (3840p) — showcase 4K HDR & 3840p movies first to avoid duplicating dv-movies
    let mut uhd_mv: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| {
            showcase[i].item.k == "m"
                && matches!(showcase[i].qual.tier, QualityTier::Uhd4k | QualityTier::Hdr4k)
        })
        .collect();
    uhd_mv.sort_by(|a, b| {
        let sa = showcase[*a].showcase_score(cy) + if showcase[*a].qual.tier == QualityTier::Hdr4k { 0.8 } else { 0.0 };
        let sb = showcase[*b].showcase_score(cy) + if showcase[*b].qual.tier == QualityTier::Hdr4k { 0.8 } else { 0.0 };
        sb.total_cmp(&sa)
    });
    if uhd_mv.len() < 12 {
        let existing: HashSet<usize> = uhd_mv.iter().copied().collect();
        let mut extra: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| showcase[i].item.k == "m" && showcase[i].qual.tier >= QualityTier::Uhd4k && !existing.contains(&i))
            .collect();
        extra.sort_by(|a, b| showcase[*b].showcase_score(cy).total_cmp(&showcase[*a].showcase_score(cy)));
        uhd_mv.extend(extra.into_iter().take(40 - uhd_mv.len()));
    }
    let uhd_mv_cards: Vec<Value> = uhd_mv
        .into_iter()
        .take(40)
        .map(|i| enrich_card(app, &showcase[i].item, &showcase[i].qual))
        .collect();
    push_row(&mut rows, "4k-movies", "4K UHD Movies (3840p)", Some("Ultra HD 3840p & 4K HDR films"), uhd_mv_cards, 3);

    // 5. 4K UHD Series
    let mut uhd_tv: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| {
            showcase[i].item.k == "s"
                && matches!(showcase[i].qual.tier, QualityTier::Uhd4k | QualityTier::Hdr4k)
        })
        .collect();
    uhd_tv.sort_by(|a, b| {
        let sa = showcase[*a].showcase_score(cy) + if showcase[*a].qual.tier == QualityTier::Hdr4k { 0.8 } else { 0.0 };
        let sb = showcase[*b].showcase_score(cy) + if showcase[*b].qual.tier == QualityTier::Hdr4k { 0.8 } else { 0.0 };
        sb.total_cmp(&sa)
    });
    if uhd_tv.len() < 12 {
        let existing: HashSet<usize> = uhd_tv.iter().copied().collect();
        let mut extra: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| showcase[i].item.k == "s" && showcase[i].qual.tier >= QualityTier::Uhd4k && !existing.contains(&i))
            .collect();
        extra.sort_by(|a, b| showcase[*b].showcase_score(cy).total_cmp(&showcase[*a].showcase_score(cy)));
        uhd_tv.extend(extra.into_iter().take(40 - uhd_tv.len()));
    }
    let uhd_tv_cards: Vec<Value> = uhd_tv
        .into_iter()
        .take(40)
        .map(|i| enrich_card(app, &showcase[i].item, &showcase[i].qual))
        .collect();
    push_row(&mut rows, "4k-series", "4K UHD Series", Some("Amazon, HBO, Marvel & more in 3840p"), uhd_tv_cards, 3);

    // 6. 4K Nature & Documentaries (OLED eye candy!)
    let mut doc_4k: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| showcase[i].is_doc && showcase[i].qual.tier >= QualityTier::Uhd4k)
        .collect();
    doc_4k.sort_by(|a, b| {
        let score = |i: usize| {
            let e = &showcase[i];
            e.showcase_score(cy)
                + if e.is_nature { 1.8 } else { 0.0 }
                + if e.qual.tier == QualityTier::Dv4k { 0.9 } else if e.qual.tier == QualityTier::Hdr4k { 0.6 } else { 0.0 }
        };
        score(*b).total_cmp(&score(*a))
    });
    let doc_4k_cards: Vec<Value> = doc_4k
        .into_iter()
        .take(40)
        .map(|i| enrich_card(app, &showcase[i].item, &showcase[i].qual))
        .collect();
    push_row(&mut rows, "4k-docs", "4K Nature & Documentaries", Some("OLED eye candy · 4K & Dolby Vision docs"), doc_4k_cards, 3);

    // 7. Blu-Ray Masters & Dolby Audio (`TOP MOVIES BLURAY` + `DOLBY AUDIO`)
    let mut br_cands: Vec<(f64, &ShowcaseEntry)> = Vec::new();
    for &i in &visible {
        let e = &showcase[i];
        if e.qual.is_bluray || e.qual.has_dolby_audio {
            let bonus = match (e.qual.is_bluray, e.qual.has_dolby_audio) {
                (true, true) => 1.2,
                (true, false) => 0.7,
                (false, true) => 0.9,
                _ => 0.0,
            };
            br_cands.push((e.showcase_score(cy) + bonus, e));
        }
    }
    for e in &dolby_hd {
        if !e.hidden(&hide) && e.qual.has_dolby_audio {
            br_cands.push((e.showcase_score(cy) + 0.6, e));
        }
    }
    br_cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    let br_cards: Vec<Value> = br_cands
        .into_iter()
        .take(40)
        .map(|(_, e)| enrich_card(app, &e.item, &e.qual))
        .collect();
    push_row(&mut rows, "bluray", "Blu-Ray Masters & Dolby Audio", Some("High-bitrate Blu-Ray masters & Dolby Audio"), br_cards, 3);

    // 8. Highest-Rated in 4K (TMDB >= 7.5, sorted by Bayesian rating)
    let mut top_4k: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|&i| {
            let e = &showcase[i];
            if e.qual.tier < QualityTier::Uhd4k {
                return false;
            }
            match &e.meta {
                Some(m) if m.va > 0.0 => m.va >= 7.5 && m.vc >= 50,
                _ => e.item.rating >= 7.5,
            }
        })
        .collect();
    top_4k.sort_by(|a, b| {
        showcase[*b]
            .bayes()
            .total_cmp(&showcase[*a].bayes())
            .then(showcase[*b].qual.tier.cmp(&showcase[*a].qual.tier))
            .then(showcase[*b].votes().cmp(&showcase[*a].votes()))
    });
    let top_4k_cards: Vec<Value> = top_4k
        .into_iter()
        .take(40)
        .map(|i| enrich_card(app, &showcase[i].item, &showcase[i].qual))
        .collect();
    push_row(&mut rows, "top-4k", "Highest-Rated in 4K", Some("Acclaimed 7.5+ cinema & TV in 4K"), top_4k_cards, 3);

    // 9. Currently CAM-Only (New Releases) — compact watch shelf so users can hit Wanted/Notify
    let cam_cnt = cam_only.len();
    if !cam_only.is_empty() {
        let wanted_keys: HashSet<String> = app.state.read(|st| {
            st["wanted"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        });
        let mut cam_sorted: Vec<&ShowcaseEntry> = cam_only.iter().filter(|e| !e.hidden(&hide)).collect();
        cam_sorted.sort_by(|a, b| {
            b.year()
                .cmp(&a.year())
                .then(b.item.added.cmp(&a.item.added))
                .then(b.pop().total_cmp(&a.pop()))
        });
        let cam_cards: Vec<Value> = cam_sorted
            .into_iter()
            .take(24)
            .map(|e| {
                let mut c = enrich_card(app, &e.item, &e.qual);
                let mt = if e.item.k == "s" { "tv" } else { "movie" };
                let is_w = e
                    .item
                    .tmdb
                    .as_ref()
                    .map_or(false, |t| wanted_keys.contains(&format!("{mt}:{t}")));
                c["wanted"] = json!(is_w);
                c
            })
            .collect();
        push_row(
            &mut rows,
            "cam-watch",
            "Currently CAM-Only (New Releases)",
            Some("No clean HD/4K copy yet · add to Wanted for release alerts"),
            cam_cards,
            1,
        );
    }

    json!({
        "hero": hero,
        "rows": rows,
        "counts": {
            "dv": dv_cnt,
            "hdr": hdr_cnt,
            "uhd4k": uhd4k_cnt,
            "bluray": bluray_cnt,
            "movies4k": movies4k_cnt,
            "series4k": series4k_cnt,
            "docs4k": docs4k_cnt,
            "upgrades": upgrades_cnt,
            "camOnly": cam_cnt,
            "total": showcase.len()
        },
        "platforms": platforms,
        "genres": genres
    })
}

// ---------------------------------------------------------------------------
// 5. 4K Showcase Faceted Browse (`browse`)
// ---------------------------------------------------------------------------

fn parse_year_range(year_param: &str, y1_param: &str, y2_param: &str) -> (i32, i32) {
    let raw = year_param.trim().to_lowercase();
    if !raw.is_empty() {
        if let Some(dec) = raw.strip_suffix('s') {
            if let Ok(y) = dec.parse::<i32>() {
                return (y, y + 9);
            }
        }
        if let Some(base) = raw.strip_suffix('+') {
            if let Ok(y) = base.parse::<i32>() {
                return (y, 9999);
            }
        }
        if let Some((a, b)) = raw.split_once('-') {
            let lo = a.trim().parse::<i32>().unwrap_or(0);
            let hi = b.trim().parse::<i32>().unwrap_or(9999);
            return (lo, if hi == 0 { 9999 } else { hi });
        }
        if let Ok(y) = raw.parse::<i32>() {
            return (y, y);
        }
    }
    let y1 = y1_param.trim().parse::<i32>().unwrap_or(0);
    let y2 = {
        let v = y2_param.trim().parse::<i32>().unwrap_or(0);
        if v == 0 { 9999 } else { v }
    };
    (y1, y2)
}

fn matches_tier_filter(e: &ShowcaseEntry, tier: &str) -> bool {
    match tier {
        "cam" => e.qual.cam_only,
        "dv" | "dolbyvision" => !e.qual.cam_only && e.qual.tier == QualityTier::Dv4k,
        "hdr" | "4khdr" => !e.qual.cam_only && e.qual.tier >= QualityTier::Hdr4k,
        "4k" | "uhd" => !e.qual.cam_only && e.qual.tier >= QualityTier::Uhd4k,
        "bluray" | "blu-ray" => !e.qual.cam_only && (e.qual.is_bluray || e.qual.tier == QualityTier::Bluray),
        "dolby" | "dolbyaudio" | "atmos" => !e.qual.cam_only && e.qual.has_dolby_audio,
        _ => !e.qual.cam_only && e.qual.tier >= QualityTier::Bluray,
    }
}

fn matches_kind_filter(e: &ShowcaseEntry, kind: &str) -> bool {
    match kind {
        "movie" | "movies" | "m" => e.item.k == "m",
        "tv" | "series" | "s" => e.item.k == "s",
        "docs" | "doc" | "documentary" => e.is_doc,
        _ => true,
    }
}

fn matches_genre_filter(e: &ShowcaseEntry, req_genres: &[String]) -> bool {
    if req_genres.is_empty() {
        return true;
    }
    let is_tv = e.item.k == "s";
    req_genres.iter().any(|tok| {
        if let Ok(gid) = tok.parse::<i64>() {
            if let Some(m) = &e.meta {
                if genre_matches(&m.g, gid) {
                    return true;
                }
            }
            if let Some(gn) = crate::meta::genre_name(is_tv, gid).or_else(|| crate::meta::genre_name(!is_tv, gid)) {
                return e.item.genre.to_lowercase().contains(&gn.to_lowercase());
            }
            return false;
        }
        let tl = tok.to_lowercase();
        if let Some(m) = &e.meta {
            if m.g
                .iter()
                .filter_map(|&g| crate::meta::genre_name(is_tv, g))
                .any(|gn| gn.to_lowercase().contains(&tl))
            {
                return true;
            }
        }
        e.item.genre.to_lowercase().contains(&tl)
    })
}

fn matches_search_terms(e: &ShowcaseEntry, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut hay = String::with_capacity(256);
    hay.push_str(&e.item.title);
    hay.push(' ');
    hay.push_str(e.qual.tier.label());
    hay.push(' ');
    if e.qual.is_bluray {
        hay.push_str("bluray blu-ray ");
    }
    if e.qual.has_dolby_audio {
        hay.push_str("dolby audio atmos ");
    }
    if e.qual.is_dv {
        hay.push_str("dolby vision dv ");
    }
    let yr = e.year();
    if yr > 0 {
        hay.push_str(&yr.to_string());
        hay.push(' ');
    }
    if let Some(m) = &e.meta {
        hay.push_str(&m.ov);
        hay.push(' ');
        for &g in &m.g {
            if let Some(gn) = crate::meta::genre_name(e.item.k == "s", g) {
                hay.push_str(gn);
                hay.push(' ');
            }
        }
    }
    if !e.item.plot.is_empty() {
        hay.push_str(&e.item.plot);
        hay.push(' ');
    }
    if !e.item.genre.is_empty() {
        hay.push_str(&e.item.genre);
        hay.push(' ');
    }
    for p in &e.platforms {
        hay.push_str(p);
        hay.push(' ');
    }
    let lo = hay.to_lowercase();
    terms.iter().all(|w| lo.contains(w.as_str()))
}

/// GET /api/showcase/browse: filterable, searchable 4K/DV/HDR/Blu-Ray (and CAM-watch) explorer.
pub fn browse(app: &App, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let tier = get("tier").trim().to_lowercase();
    let kind = get("kind").trim().to_lowercase();
    let req_platforms: Vec<String> = get("platform")
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    let genre_raw = q.get("genre").or_else(|| q.get("g")).cloned().unwrap_or_default();
    let req_genres: Vec<String> = genre_raw
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    let (y1, y2) = parse_year_range(&get("year"), &get("y1"), &get("y2"));
    let min_rating = q
        .get("minRating")
        .or_else(|| q.get("rmin"))
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    let unwatched = matches!(get("unwatched").trim(), "1" | "true");
    let terms: Vec<String> = get("q")
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let sort = get("sort").trim().to_lowercase();
    let page = get("page").trim().parse::<usize>().unwrap_or(0);
    let limit = q
        .get("limit")
        .or_else(|| q.get("per"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(60)
        .clamp(1, 120);

    let seen = if unwatched { Some(watched_set(app)) } else { None };
    let (showcase, dolby_hd, cam_only) = build_pool(app);
    let cat = app.catalog();
    let cy = current_year();

    // Base attribute filter (everything except tier, kind, platform — used to compute sibling facet counts)
    let pass_common = |e: &ShowcaseEntry| -> bool {
        if !matches_genre_filter(e, &req_genres) {
            return false;
        }
        if y1 > 0 || y2 < 9999 {
            let yr = e.year();
            if yr == 0 || yr < y1 || yr > y2 {
                return false;
            }
        }
        if min_rating > 0.0 {
            let ok = match &e.meta {
                Some(m) if m.va > 0.0 => m.va >= min_rating && m.vc >= 15,
                _ => e.item.rating >= min_rating,
            };
            if !ok {
                return false;
            }
        }
        if let Some(seen) = &seen {
            if let Some(t) = &e.item.tmdb {
                let mt = if e.item.k == "s" { "tv" } else { "movie" };
                if seen.contains(&format!("{mt}:{t}")) {
                    return false;
                }
            }
        }
        matches_search_terms(e, &terms)
    };
    let pass_platform = |e: &ShowcaseEntry| -> bool {
        req_platforms.is_empty() || req_platforms.iter().any(|p| cat.item_has_platform(&e.item, p))
    };

    // Tally tierCounts (with kind + platform + common applied), kindCounts (with tier + platform + common),
    // platformCounts (with tier + kind + common), and collect matched items.
    let (mut t_all, mut t_dv, mut t_hdr, mut t_4k, mut t_bluray, mut t_dolby, mut t_cam) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut k_all, mut k_movie, mut k_tv, mut k_docs) = (0usize, 0usize, 0usize, 0usize);
    let mut plat_map: HashMap<&'static str, usize> = HashMap::new();
    let mut matched: Vec<&ShowcaseEntry> = Vec::new();

    for e in showcase.iter().chain(dolby_hd.iter()).chain(cam_only.iter()) {
        if !pass_common(e) {
            continue;
        }
        let ok_tier = matches_tier_filter(e, &tier);
        let ok_kind = matches_kind_filter(e, &kind);
        let ok_plat = pass_platform(e);

        if ok_kind && ok_plat {
            if e.qual.cam_only {
                t_cam += 1;
            } else {
                if e.qual.tier >= QualityTier::Bluray {
                    t_all += 1;
                }
                if e.qual.tier == QualityTier::Dv4k {
                    t_dv += 1;
                }
                if e.qual.tier >= QualityTier::Hdr4k {
                    t_hdr += 1;
                }
                if e.qual.tier >= QualityTier::Uhd4k {
                    t_4k += 1;
                }
                if e.qual.is_bluray || e.qual.tier == QualityTier::Bluray {
                    t_bluray += 1;
                }
                if e.qual.has_dolby_audio {
                    t_dolby += 1;
                }
            }
        }
        if ok_tier && ok_plat {
            k_all += 1;
            if e.item.k == "s" {
                k_tv += 1;
            } else {
                k_movie += 1;
            }
            if e.is_doc {
                k_docs += 1;
            }
        }
        if ok_tier && ok_kind {
            for &p in &e.platforms {
                *plat_map.entry(p).or_default() += 1;
            }
        }
        if ok_tier && ok_kind && ok_plat {
            matched.push(e);
        }
    }

    match sort.as_str() {
        "rating" => matched.sort_by(|a, b| {
            b.bayes()
                .total_cmp(&a.bayes())
                .then(b.votes().cmp(&a.votes()))
                .then(b.qual.tier.cmp(&a.qual.tier))
        }),
        "newest" => matched.sort_by(|a, b| {
            b.sort_date()
                .cmp(&a.sort_date())
                .then(b.item.added.cmp(&a.item.added))
        }),
        "added" => matched.sort_by(|a, b| {
            b.item
                .added
                .cmp(&a.item.added)
                .then(b.qual.tier.cmp(&a.qual.tier))
                .then(b.bayes().total_cmp(&a.bayes()))
        }),
        "tier" => matched.sort_by(|a, b| {
            b.qual
                .tier
                .cmp(&a.qual.tier)
                .then(b.qual.probe.kbps.unwrap_or(0).cmp(&a.qual.probe.kbps.unwrap_or(0)))
                .then(b.bayes().total_cmp(&a.bayes()))
                .then(b.pop().total_cmp(&a.pop()))
        }),
        "az" => matched.sort_by(|a, b| a.item.title.to_lowercase().cmp(&b.item.title.to_lowercase())),
        _ => matched.sort_by(|a, b| {
            let sa = (a.pop() + 1.0).log10() * 1.4 + a.showcase_score(cy) * 0.65;
            let sb = (b.pop() + 1.0).log10() * 1.4 + b.showcase_score(cy) * 0.65;
            sb.total_cmp(&sa)
        }),
    }

    let total = matched.len();
    let more = (page + 1).saturating_mul(limit) < total;
    let items: Vec<Value> = matched
        .into_iter()
        .skip(page.saturating_mul(limit))
        .take(limit)
        .map(|e| enrich_card(app, &e.item, &e.qual))
        .collect();

    let mut plats_vec: Vec<(&str, usize)> = plat_map.into_iter().collect();
    plats_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let platform_counts: Vec<Value> = plats_vec
        .into_iter()
        .take(12)
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect();

    json!({
        "items": items,
        "total": total,
        "page": page,
        "limit": limit,
        "more": more,
        "tierCounts": {
            "all": t_all,
            "dv": t_dv,
            "hdr": t_hdr,
            "4k": t_4k,
            "bluray": t_bluray,
            "dolby": t_dolby,
            "cam": t_cam
        },
        "kindCounts": {
            "all": k_all,
            "movie": k_movie,
            "tv": k_tv,
            "docs": k_docs
        },
        "platformCounts": platform_counts
    })
}

// ---------------------------------------------------------------------------
// 6. Quality-Upgrade Watcher & History (`check_upgrades` / `recent_upgrades`)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct TierSnap {
    tier: u8,
    label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpgradeRecord {
    key: String,
    k: String,
    id: i64,
    tmdb: Option<String>,
    title: String,
    year: Option<i32>,
    poster: String,
    backdrop: String,
    from_tier: u8,
    from_label: String,
    to_tier: u8,
    to_label: String,
    at: i64,
    watched: bool,
    wanted: bool,
    in_list: bool,
    #[serde(default)]
    followed: bool,
}

#[derive(Clone, Debug)]
struct UpgradeCandidate {
    key: String,
    k: &'static str,
    id: i64,
    tmdb: Option<String>,
    title: String,
    year: Option<i32>,
    poster: String,
    backdrop: String,
    tier: QualityTier,
    pop: f64,
    watched: bool,
    wanted: bool,
    in_list: bool,
    followed: bool,
}

fn track_key(it: &Item) -> String {
    match &it.tmdb {
        Some(t) if !t.is_empty() => format!("{}:{t}", it.k),
        _ => format!("{}:{}", it.k, it.id),
    }
}

/// Notable upgrade transitions:
/// - `Cam -> >=Hd` (clean release dropped!)
/// - `Hd / Bluray -> >=Uhd4k` (true 4K / 4K HDR / 4K DV upgrade dropped!)
fn is_notable_upgrade(from: QualityTier, to: QualityTier) -> bool {
    if to <= from {
        return false;
    }
    (from == QualityTier::Cam && to >= QualityTier::Hd)
        || (matches!(from, QualityTier::Hd | QualityTier::Bluray) && to >= QualityTier::Uhd4k)
}

fn upgrade_alert_copy(title: &str, from: QualityTier, to: QualityTier) -> (String, String) {
    if from == QualityTier::Cam {
        (
            format!("Clean Release Ready: {title}"),
            format!("{title} upgraded from CAM to {}", to.label()),
        )
    } else if to == QualityTier::Dv4k {
        (
            format!("4K Dolby Vision Upgrade: {title}"),
            format!("{title} is now in 4K DV"),
        )
    } else if to == QualityTier::Hdr4k {
        (
            format!("4K HDR Upgrade: {title}"),
            format!("{title} is now in 4K HDR"),
        )
    } else {
        (
            format!("4K UHD Upgrade: {title}"),
            format!("{title} is now in {}", to.label()),
        )
    }
}

fn upgrade_to_value(rec: &UpgradeRecord) -> Value {
    let from = QualityTier::from_u8(rec.from_tier);
    let to = QualityTier::from_u8(rec.to_tier);
    let (alert_title, alert_body) = upgrade_alert_copy(&rec.title, from, to);
    json!({
        "key": rec.key,
        "k": rec.k,
        "id": rec.id,
        "tmdb": rec.tmdb,
        "title": rec.title,
        "year": rec.year,
        "poster": rec.poster,
        "backdrop": rec.backdrop,
        "fromTier": rec.from_tier,
        "fromLabel": rec.from_label,
        "toTier": rec.to_tier,
        "toLabel": rec.to_label,
        "upgradeBadge": format!("{} → {}", rec.from_label, rec.to_label),
        "at": rec.at,
        "watched": rec.watched,
        "wanted": rec.wanted,
        "inList": rec.in_list,
        "followed": rec.followed,
        "alertTitle": alert_title,
        "alertBody": alert_body,
    })
}

fn atomic_write_json(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body)?;
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

fn read_tiers_file(path: &Path) -> Option<HashMap<String, TierSnap>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_upgrades_file(path: &Path) -> Vec<UpgradeRecord> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn check_upgrades_at(data_dir: &Path, candidates: &[UpgradeCandidate], now: i64) -> Vec<Value> {
    let tiers_path = data_dir.join("quality-tiers.json");
    let upgrades_path = data_dir.join("quality-upgrades.json");

    let prev_opt = read_tiers_file(&tiers_path);
    let Some(prev) = prev_opt else {
        // First run: persist silent baseline with zero alerts.
        let mut baseline: HashMap<String, TierSnap> = HashMap::with_capacity(candidates.len());
        for c in candidates {
            let entry = baseline.entry(c.key.clone()).or_insert_with(|| TierSnap {
                tier: c.tier.as_u8(),
                label: c.tier.label().to_string(),
            });
            if c.tier.as_u8() > entry.tier {
                entry.tier = c.tier.as_u8();
                entry.label = c.tier.label().to_string();
            }
        }
        if let Ok(bytes) = serde_json::to_vec(&baseline) {
            let _ = atomic_write_json(&tiers_path, &bytes);
        }
        return vec![];
    };

    let mut next_map = prev.clone();
    let mut new_records: Vec<(UpgradeRecord, f64)> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();

    for c in candidates {
        let cur_u8 = c.tier.as_u8();
        let entry = next_map.entry(c.key.clone()).or_insert_with(|| TierSnap {
            tier: cur_u8,
            label: c.tier.label().to_string(),
        });
        if cur_u8 > entry.tier {
            let from = QualityTier::from_u8(entry.tier);
            let to = c.tier;
            entry.tier = cur_u8;
            entry.label = to.label().to_string();

            if is_notable_upgrade(from, to) && seen_keys.insert(c.key.clone()) {
                new_records.push((
                    UpgradeRecord {
                        key: c.key.clone(),
                        k: c.k.to_string(),
                        id: c.id,
                        tmdb: c.tmdb.clone(),
                        title: c.title.clone(),
                        year: c.year,
                        poster: c.poster.clone(),
                        backdrop: c.backdrop.clone(),
                        from_tier: from.as_u8(),
                        from_label: from.label().to_string(),
                        to_tier: to.as_u8(),
                        to_label: to.label().to_string(),
                        at: now,
                        watched: c.watched,
                        wanted: c.wanted,
                        in_list: c.in_list,
                        followed: c.followed,
                    },
                    c.pop,
                ));
            }
        }
    }

    // Persist updated tier snapshot atomically whenever anything changed.
    if next_map != prev {
        if let Ok(bytes) = serde_json::to_vec(&next_map) {
            let _ = atomic_write_json(&tiers_path, &bytes);
        }
    }

    if new_records.is_empty() {
        return vec![];
    }

    // Rank new upgrades: personal (wanted/list/followed) & CAM->HD drops first, then tier & popularity.
    new_records.sort_by(|(a, pa), (b, pb)| {
        let prio = |r: &UpgradeRecord| -> u8 {
            if r.wanted { 4 }
            else if r.in_list || r.followed { 3 }
            else if r.from_tier == QualityTier::Cam.as_u8() { 2 }
            else { 1 }
        };
        prio(b)
            .cmp(&prio(a))
            .then(b.to_tier.cmp(&a.to_tier))
            .then(pb.total_cmp(pa))
    });

    // Merge into persistent quality-upgrades.json (dedup by key, newest first, max 60).
    let mut history = read_upgrades_file(&upgrades_path);
    let new_keys: HashSet<&str> = new_records.iter().map(|(r, _)| r.key.as_str()).collect();
    history.retain(|old| !new_keys.contains(old.key.as_str()));
    let mut merged: Vec<UpgradeRecord> = new_records.iter().map(|(r, _)| r.clone()).collect();
    merged.extend(history);
    merged.truncate(UPGRADES_MAX);
    if let Ok(bytes) = serde_json::to_vec(&merged) {
        let _ = atomic_write_json(&upgrades_path, &bytes);
    }

    // Return alert-worthy newly upgraded titles:
    // Always include Wanted / My List / Followed; also include unwatched trending (pop >= 40) capped cleanly.
    let mut alerts: Vec<Value> = Vec::new();
    let mut trending_added = 0usize;
    for (rec, pop) in &new_records {
        let personal = rec.wanted || rec.in_list || rec.followed;
        let trending = !rec.watched && *pop >= TRENDING_ALERT_POP;
        if personal {
            alerts.push(upgrade_to_value(rec));
        } else if trending && trending_added < TRENDING_ALERT_CAP {
            trending_added += 1;
            alerts.push(upgrade_to_value(rec));
        }
    }
    alerts
}

struct UserWatchlists {
    watched: HashSet<String>,
    wanted: HashSet<String>,
    list_keys: HashSet<String>,
    list_tmdb: HashSet<String>,
    follows: HashSet<String>,
}

fn load_user_watchlists(app: &App) -> UserWatchlists {
    let watched = watched_set(app);
    let (wanted, list_keys, list_tmdb, follows) = app.state.read(|st| {
        let wanted: HashSet<String> = st["wanted"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let mut list_keys: HashSet<String> = HashSet::new();
        let mut list_tmdb: HashSet<String> = HashSet::new();
        if let Some(m) = st["list"].as_object() {
            for (k, v) in m {
                list_keys.insert(k.clone());
                let tm = s(&v["item"]["tmdb"]);
                if !tm.is_empty() {
                    let mt = if k.starts_with("s:") || v["item"]["k"] == "s" { "tv" } else { "movie" };
                    list_tmdb.insert(format!("{mt}:{tm}"));
                }
            }
        }
        let follows: HashSet<String> = st["follows"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        (wanted, list_keys, list_tmdb, follows)
    });
    UserWatchlists { watched, wanted, list_keys, list_tmdb, follows }
}

/// Compare the current catalog's quality tiers against `data/quality-tiers.json`.
///
/// - First run: silently writes baseline snapshot and returns `vec![]`.
/// - Subsequent runs: logs any `Cam -> >=Hd` or `Hd/Bluray -> >=Uhd4k` upgrades into
///   `data/quality-upgrades.json` (up to 60 entries) and returns newly upgraded
///   alert-worthy titles (Wanted, My List, Followed, or trending `pop >= 40`) ready
///   for `crate::notify_all`.
pub fn check_upgrades(app: &App) -> Vec<Value> {
    let cat = app.catalog();
    let wl = load_user_watchlists(app);
    let db = app.meta.read().unwrap();
    let mut candidates: Vec<UpgradeCandidate> = Vec::with_capacity(cat.movies.len() + cat.series.len());

    for (mt, list) in [("movie", &cat.movies), ("tv", &cat.series)] {
        for it in list {
            if !it.en || it.sport {
                continue;
            }
            let tier = best_tier(app, it);
            let meta = it
                .tmdb
                .as_ref()
                .and_then(|t| db.get(&format!("{mt}:{t}")))
                .filter(|m| !m.missing);
            let tmdb_key = it.tmdb.as_ref().map(|t| format!("{mt}:{t}"));
            let watched = tmdb_key.as_ref().map_or(false, |k| wl.watched.contains(k));
            let wanted = tmdb_key.as_ref().map_or(false, |k| wl.wanted.contains(k));
            let in_list = wl.list_keys.contains(&format!("{}:{}", it.k, it.id))
                || it.variants.iter().any(|v| wl.list_keys.contains(&format!("{}:{}", it.k, v.id)))
                || tmdb_key.as_ref().map_or(false, |k| wl.list_tmdb.contains(k));
            let followed = it.k == "s"
                && (wl.follows.contains(&it.id.to_string())
                    || it.variants.iter().any(|v| wl.follows.contains(&v.id.to_string())));

            let poster = meta
                .filter(|m| !m.pp.is_empty())
                .map(|m| tmdb_img(&m.pp, "w342"))
                .unwrap_or_else(|| it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/"));
            let backdrop = meta
                .filter(|m| !m.bd.is_empty())
                .map(|m| tmdb_img(&m.bd, "w1280"))
                .unwrap_or_else(|| it.backdrop.clone());
            let year = meta.map(|m| m.year()).filter(|&y| y > 0).or(it.year);
            let pop = meta.map(|m| m.p).unwrap_or(0.0);

            candidates.push(UpgradeCandidate {
                key: track_key(it),
                k: it.k,
                id: it.id,
                tmdb: it.tmdb.clone(),
                title: it.title.clone(),
                year,
                poster,
                backdrop,
                tier,
                pop,
                watched,
                wanted,
                in_list,
                followed,
            });
        }
    }
    drop(db);

    check_upgrades_at(&app.data, &candidates, now_ms())
}

/// Return recent quality upgrades stored in `data/quality-upgrades.json` (newest first, up to 60),
/// with live `watched` / `wanted` / `inList` flags refreshed from `app.state`.
pub fn recent_upgrades(app: &App) -> Vec<Value> {
    let mut records = read_upgrades_file(&app.data.join("quality-upgrades.json"));
    if records.is_empty() {
        return vec![];
    }
    let wl = load_user_watchlists(app);
    let cat = app.catalog();
    records.sort_by(|a, b| b.at.cmp(&a.at));
    records.truncate(UPGRADES_MAX);

    records
        .into_iter()
        .map(|mut rec| {
            let mt = if rec.k == "s" { "tv" } else { "movie" };
            let tmdb_key = rec.tmdb.as_deref().filter(|t| !t.is_empty()).map(|t| format!("{mt}:{t}"));
            if let Some(t) = rec.tmdb.as_deref().filter(|t| !t.is_empty()) {
                if let Some(it) = cat.by_tmdb(mt, t) {
                    rec.id = it.id;
                }
            }
            rec.watched = tmdb_key.as_ref().map_or(false, |k| wl.watched.contains(k));
            rec.wanted = tmdb_key.as_ref().map_or(false, |k| wl.wanted.contains(k));
            rec.in_list = wl.list_keys.contains(&format!("{}:{}", rec.k, rec.id))
                || tmdb_key.as_ref().map_or(false, |k| wl.list_tmdb.contains(k));
            rec.followed = rec.k == "s" && wl.follows.contains(&rec.id.to_string());
            upgrade_to_value(&rec)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 7. Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Variant;

    #[test]
    fn cam_detector_catches_all_real_provider_cams() {
        // Real provider CAM titles & categories
        assert!(is_cam_release("EN-CAM - Superman  (2025)", "EN - NEW RELEASE"));
        assert!(is_cam_release("EN-CAM - Valiant One  (2025)", "EN - NEW RELEASE"));
        assert!(is_cam_release("EN-CAM - Brave the Dark  (2025)", "EN - NEW RELEASE"));
        assert!(is_cam_release("IN-CAM - Ramayana: Rise of a Legend (2026)", "IN - NEW RELEASE"));
        assert!(is_cam_release("CAM - Fresh Release (2025)", "EN - NEW RELEASE"));
        assert!(is_cam_release(
            "EN - Mission: Impossible - The Final Reckoning (2025) (Telesync)",
            "EN - NEW RELEASE"
        ));
        assert!(is_cam_release(
            "EN - Ghostbusters: Frozen Empire  (2024) (Cam)",
            "EN - NEW RELEASE"
        ));
        assert!(is_cam_release("Some Movie (2025)", "IN - CAM & NEW RELEASE"));
        assert!(is_cam_release("Un Film (2025)", "FR - FILM CAM"));
        assert!(is_cam_release("Action Film (2025) [HDTS]", "EN - NEW RELEASE"));
        assert!(is_cam_release("Action Film (2025) (HD-CAM)", "EN - NEW RELEASE"));
        assert!(is_cam_release("Action Film (2025) (Clean Audio)", "EN - NEW RELEASE"));
        assert!(is_cam_release("Action.Film.2025.HDCAM.mp4", "EN - NEW RELEASE"));
        assert!(is_cam_release("Action.Film.2025.TELESYNC.x264", "EN - NEW RELEASE"));
        assert!(is_cam_release("Action Film (2025) (DVDSCR)", "EN - NEW RELEASE"));
        assert!(is_cam_release("Action Film (2025) (Workprint)", "EN - NEW RELEASE"));
    }

    #[test]
    fn cam_detector_zero_false_positives_on_real_titles() {
        let safe_titles = [
            ("EN - Cam Girls  (2021)", "EN - NEW RELEASE"),
            ("NF - Cam", "NETFLIX MOVIES"),
            ("EN - Cam  (2018)", "EN - THRILLER"),
            ("EN - Cam-Girl  (2016)", "EN - THRILLER"),
            ("EN - Body Cam", "EN - HORROR"),
            ("EN - Merry Kiss Cam  (2022)", "EN - CHRISTMAS"),
            ("EN - Dory's Reef Cam  (2020)", "EN - KIDS"),
            ("EN - Camera (2020)", "EN - DRAMA"),
            ("EN - The Cameraman (1928)", "EN - CLASSICS"),
            ("EN - Camelot (1967)", "EN - CLASSICS"),
            ("EN - Camp Rock (2008)", "DISNEY+ MOVIES"),
            ("EN - Tsunami (2015)", "EN - ACTION"),
            ("EN - Camille (2008)", "EN - ROMANCE"),
            ("EN - HTC: The Story (2020)", "EN - DOCUMENTARIES"),
            ("EN - The Script (2012)", "EN - MUSIC"),
            ("TR - Hababam Sinifi (1975)", "TR - YESIL CAM SINEMA"),
            ("4K-NF - Extraction (2020)", "NETFLIX MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"),
        ];
        for (name, cat) in safe_titles {
            assert!(
                !is_cam_release(name, cat),
                "False positive CAM detection on title={name:?}, cat={cat:?}"
            );
        }
    }

    #[test]
    fn dv_bluray_dolby_audio_match_exact_provider_superscripts() {
        // Dolby Vision superscript categories
        assert!(is_dv_tag("4K-NF - Extraction (2020)", "NETFLIX MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"));
        assert!(is_dv_tag("4K-D+ - Moana 2 (2024)", "DISNEY+ MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"));
        assert!(is_dv_tag("4K-AP - Severance", "APPLE+ SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"));
        assert!(is_dv_tag("4K-AP - Prehistoric Planet", "APPLE+ DOCU-SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"));
        assert!(is_dv_tag("Show", "GERMANY NETFLIX DV ⁴ᴷ ᴴᴰᴿ"));
        assert!(!is_dv_tag("4K-TOP - Spider-Man", "TOP MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"));
        assert!(!is_dv_tag("EN - Dune (2021)", "EN - MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ"));

        // Dolby Audio superscript & ASCII categories
        assert!(is_dolby_audio_tag("4K-TOP - Spider-Man", "TOP MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"));
        assert!(is_dolby_audio_tag("NF - Movie", "NETFLIX MOVIES DOLBY AUDIO"));
        assert!(is_dolby_audio_tag("NF - Show", "NETFLIX SERIES ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"));
        assert!(is_dolby_audio_tag("Show", "ENGLISH SERIES ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"));
        assert!(is_dolby_audio_tag("Show", "AMAZON REALITY ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ ⁴ᴷ ³⁸⁴⁰ᴾ"));
        assert!(!is_dolby_audio_tag("4K-NF - Extraction (2020)", "NETFLIX MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"));

        // Blu-Ray categories
        assert!(is_bluray_tag("TOP - Dune (2021)", "TOP MOVIES BLURAY (MULTI-SUBS)"));
        assert!(is_bluray_tag("TOP - Frozen", "TOP KIDS BLURAY (MULTI-SUBS)"));
        assert!(is_bluray_tag("Film", "DE - BLURAY FILME ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"));
        assert!(!is_bluray_tag("EN - Dune (2021)", "EN - MOVIES ⁴ᴷ ³⁸⁴⁰ᴾ"));

        // UHD & HDR superscripts
        assert!(is_uhd_tag("Film", "DE - ²¹⁶⁰ᵖ DV ᴴᴰᴿ¹⁰/PLUS"));
        assert!(is_hdr_tag("Film", "DE - ²¹⁶⁰ᵖ DV ᴴᴰᴿ¹⁰/PLUS"));
        assert!(is_hdr_tag("Film", "GERMANY NETFLIX DV ⁴ᴷ ᴴᴰᴿ"));
    }

    #[test]
    fn cam_only_distinguishes_cam_only_from_mixed_variants() {
        let cam_single = Item {
            k: "m",
            id: 1,
            title: "Valiant One".into(),
            en: true,
            cam: true,
            ..Default::default()
        };
        assert!(is_cam_only_item(&cam_single));

        let cam_all_variants = Item {
            k: "m",
            id: 2,
            title: "All Cam".into(),
            en: true,
            cam: true,
            variants: vec![Variant { id: 3, cam: true, ..Default::default() }],
            ..Default::default()
        };
        assert!(is_cam_only_item(&cam_all_variants));

        // Primary is CAM, but a clean HD variant exists -> NOT cam_only!
        let cam_with_clean = Item {
            k: "m",
            id: 4,
            title: "Superman".into(),
            en: true,
            cam: true,
            variants: vec![Variant { id: 5, cam: false, ..Default::default() }],
            ..Default::default()
        };
        assert!(!is_cam_only_item(&cam_with_clean));

        // Primary is clean HD, one variant is CAM -> NOT cam_only!
        let clean_with_cam = Item {
            k: "m",
            id: 6,
            title: "Mission: Impossible".into(),
            en: true,
            cam: false,
            variants: vec![Variant { id: 7, cam: true, ..Default::default() }],
            ..Default::default()
        };
        assert!(!is_cam_only_item(&clean_with_cam));
    }

    #[test]
    fn probe_summary_classifies_4k_hdr_and_dv() {
        let dv_pic = json!({ "w": 3840, "h": 1608, "hdr": "DV", "dv": 5, "kbps": 28622.0, "codec": "hevc", "ch": 6 });
        let dv = probe_to_summary(&dv_pic);
        assert_eq!(dv.tier, Some(QualityTier::Dv4k));
        assert_eq!(dv.kbps, Some(28622));
        assert_eq!(dv.hdr.as_deref(), Some("DV"));

        let hdr_pic = json!({ "w": 3840, "h": 2160, "hdr": "HDR10", "dv": null, "kbps": 14795.0, "codec": "hevc" });
        let hdr = probe_to_summary(&hdr_pic);
        assert_eq!(hdr.tier, Some(QualityTier::Hdr4k));
        assert_eq!(hdr.hdr.as_deref(), Some("HDR10"));

        let sdr4k_pic = json!({ "w": 3832, "h": 1600, "hdr": "", "dv": null, "kbps": 12585.0, "codec": "hevc" });
        assert_eq!(probe_to_summary(&sdr4k_pic).tier, Some(QualityTier::Uhd4k));

        let hd_pic = json!({ "w": 1920, "h": 1080, "hdr": "", "dv": null, "kbps": 5000.0, "codec": "h264" });
        assert_eq!(probe_to_summary(&hd_pic).tier, None);
    }

    #[test]
    fn upgrade_tracker_baseline_and_transitions() {
        assert!(is_notable_upgrade(QualityTier::Cam, QualityTier::Hd));
        assert!(is_notable_upgrade(QualityTier::Cam, QualityTier::Dv4k));
        assert!(is_notable_upgrade(QualityTier::Hd, QualityTier::Uhd4k));
        assert!(is_notable_upgrade(QualityTier::Hd, QualityTier::Dv4k));
        assert!(is_notable_upgrade(QualityTier::Bluray, QualityTier::Hdr4k));
        // Same, lower, or non-notable transitions must NOT trigger
        assert!(!is_notable_upgrade(QualityTier::Hd, QualityTier::Hd));
        assert!(!is_notable_upgrade(QualityTier::Dv4k, QualityTier::Hd));
        assert!(!is_notable_upgrade(QualityTier::Hd, QualityTier::Bluray));
        assert!(!is_notable_upgrade(QualityTier::Uhd4k, QualityTier::Hdr4k));

        let tmp = std::env::temp_dir().join(format!(
            "tv_showcase_test_{}_{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let mk = |key: &str, id: i64, title: &str, tier: QualityTier, pop: f64, wanted: bool| UpgradeCandidate {
            key: key.to_string(),
            k: "m",
            id,
            tmdb: Some(id.to_string()),
            title: title.to_string(),
            year: Some(2025),
            poster: String::new(),
            backdrop: String::new(),
            tier,
            pop,
            watched: false,
            wanted,
            in_list: false,
            followed: false,
        };

        // Run 1: baseline build -> silent, 0 alerts, creates quality-tiers.json
        let run1 = vec![
            mk("m:101", 101, "Superman", QualityTier::Cam, 95.0, true),
            mk("m:102", 102, "Dune", QualityTier::Hd, 80.0, false),
            mk("m:103", 103, "Stable Film", QualityTier::Dv4k, 70.0, true),
            mk("m:104", 104, "Quiet Indie", QualityTier::Hd, 5.0, false),
        ];
        let alerts1 = check_upgrades_at(&tmp, &run1, 1_000);
        assert!(alerts1.is_empty(), "First run must build baseline silently");
        assert!(tmp.join("quality-tiers.json").exists());

        // Run 2: Superman Cam->Hd (wanted), Dune Hd->Dv4k (trending pop>=40),
        // Stable Film Dv4k->Hd (downgrade: ignored), Quiet Indie Hd->4K (logged, not alerted since low pop)
        let run2 = vec![
            mk("m:101", 101, "Superman", QualityTier::Hd, 95.0, true),
            mk("m:102", 102, "Dune", QualityTier::Dv4k, 80.0, false),
            mk("m:103", 103, "Stable Film", QualityTier::Hd, 70.0, true),
            mk("m:104", 104, "Quiet Indie", QualityTier::Uhd4k, 5.0, false),
        ];
        let alerts2 = check_upgrades_at(&tmp, &run2, 2_000);
        assert_eq!(alerts2.len(), 2, "Expected alerts for Wanted + Trending only: {alerts2:?}");
        assert_eq!(alerts2[0]["title"], "Superman");
        assert_eq!(alerts2[0]["fromLabel"], "CAM");
        assert_eq!(alerts2[0]["toLabel"], "HD");
        assert_eq!(alerts2[0]["upgradeBadge"], "CAM → HD");
        assert_eq!(alerts2[1]["title"], "Dune");
        assert_eq!(alerts2[1]["toLabel"], "4K DV");
        assert_eq!(alerts2[1]["upgradeBadge"], "HD → 4K DV");

        // All 3 upgrades were logged to quality-upgrades.json (including Quiet Indie)
        let logged = read_upgrades_file(&tmp.join("quality-upgrades.json"));
        assert_eq!(logged.len(), 3);

        // Run 3: identical state -> 0 new alerts; Stable Film restored to Dv4k also produces 0 alerts (monotonic high-water)
        let run3 = vec![
            mk("m:101", 101, "Superman", QualityTier::Hd, 95.0, true),
            mk("m:102", 102, "Dune", QualityTier::Dv4k, 80.0, false),
            mk("m:103", 103, "Stable Film", QualityTier::Dv4k, 70.0, true),
            mk("m:104", 104, "Quiet Indie", QualityTier::Uhd4k, 5.0, false),
        ];
        let alerts3 = check_upgrades_at(&tmp, &run3, 3_000);
        assert!(alerts3.is_empty(), "No duplicate alerts on stable or restored tiers: {alerts3:?}");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
