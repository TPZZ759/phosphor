// Catalogue: load the provider's lists, classify English/sports, de-duplicate by TMDB id, search.
use crate::re;
use crate::util::{i, num, s};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const RAW: [&str; 6] = ["get_live_categories", "get_vod_categories", "get_series_categories", "get_live_streams", "get_vod_streams", "get_series"];

#[derive(Clone, Debug, Default)]
pub struct Variant {
    pub id: i64,
    pub uhd: bool,
    pub cam: bool,
    pub dv: bool,
    pub bluray: bool,
    pub dolby_audio: bool,
    /// True for clean subtitle-only cross-market 4K/Dolby-Audio upgrade variants
    /// (e.g. Nordic 4K Dolby Audio) attached to an existing English show by TMDB ID.
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
    pub live_by_id: HashMap<i64, usize>,
    pub loaded_at: i64,
}

pub fn clean_cat(n: &str) -> String {
    let t = crate::showcase::fold_superscripts(n);
    let t = re!(r"\bDOLBy\b").replace_all(&t, "DOLBY");
    let t = re!(r"\s{2,}").replace_all(&t, " ");
    re!(r"[ /]+$").replace(&t, "").trim().to_string()
}

pub struct Clean { pub title: String, pub year: Option<i32>, pub origin: String }
pub fn clean_title(name: &str) -> Clean {
    let mut t = crate::showcase::fold_superscripts(name).trim().to_string();
    for _ in 0..3 { t = re!(r"^\s*[A-Z0-9+]{1,5}(?:-[A-Z0-9+]{1,5})?\s*[-|:]\s+").replace(&t, "").to_string(); }
    t = re!(r"^\d{1,3}\.\s*").replace(&t, "").to_string();
    t = re!(r"(?i)\s+(HEVC|H\.?265|x265|UHD|HDR10\+?|HDR|4K|FHD|1080p|2160p|3840p|DOLBY\s+VISION|DOLBY\s+AUDIO|REMASTERED)\b").replace_all(&t, "").to_string();
    if !t.contains(' ') && t.contains('.') { t = t.replace('.', " "); }
    let origin = re!(r"\(([A-Z]{2})\)").captures(&t).map(|c| c[1].to_string()).unwrap_or_default();
    let year = re!(r"\((\d{4})\)").captures(&t).or_else(|| re!(r"\s(19\d\d|20\d\d)\s*$").captures(&t)).and_then(|c| c[1].parse().ok());
    t = re!(r"\(\d{4}\)").replace_all(&t, "").to_string();
    t = re!(r"\s(19\d\d|20\d\d)\s*$").replace(&t, "").to_string();
    t = re!(r"\[[^\]]*\]").replace_all(&t, "").to_string();
    t = re!(r"\s*\((?:[A-Z]{2}|MULTI[^)]*)\)").replace_all(&t, "").to_string();
    t = re!(r"\s{2,}").replace_all(&t, " ").trim().to_string();
    if t.is_empty() { t = name.trim().to_string(); }
    Clean { title: t, year, origin }
}

fn is_en_vod(n: &str) -> bool {
    if re!(r"(?i)^ENGLISH SERIES \(TR\)").is_match(n) { return false; }
    re!(r"(?i)^(EN\s*-|NETFLIX|DISNEY\+|APPLE\+|AMAZON|HBO|OSN\+\s*SERIES|PARAMOUNT|PEACOCK|SHOWTIME|SKY$|UNIVERSAL|MARVEL|DREAMWORKS|TOP (MOVIES|KIDS)|JAMES BOND|ENGLISH (SERIES|REALITY|KIDS)|NICKELODEON|CRUNCHYROLL|DISCOVERY\+ (SERIES|MOVIES)(?:\s|$)|CHRISTIAN MOVIES|ASIA (MOVIES|SERIES) \(MULTI)").is_match(n)
}
/// Clean subtitle-only cross-market 4K / Dolby Audio TV series categories whose
/// English shows carry original English audio + soft subtitles (never dubbed, no burned-in subs).
fn is_sub_market_en_upgrade(clean_cat_name: &str) -> bool {
    re!(r"(?i)^NORDIC\s+(?:SERIES?|KIDS)\b.*(?:4K|3840|2160|UHD|DOLBY)").is_match(clean_cat_name)
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

fn read(dir: &Path, n: &str) -> Value {
    std::fs::read(dir.join(format!("{n}.json"))).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Array(vec![]))
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
            for x in read(&dir, file).as_array().into_iter().flatten() {
                let raw = s(&x["category_name"]); let name = clean_cat(&raw);
                let (en, sport) = if kind == "live" { (is_en_live(&name), is_sport_live(&name)) } else { (is_en_vod(&name), is_sport_vod(&name)) };
                let id = s(&x["category_id"]);
                let key = format!("{kind}:{id}");
                raw_cats.insert(key.clone(), raw.clone());
                c.cats.insert(key, Cat { id, kind, name, en, sport, uhd: is_uhd(&raw), count: 0 });
            }
        }
        // movies
        let vod = read(&dir, "get_vod_streams");
        for v in vod.as_array().into_iter().flatten() {
            let cat_id = s(&v["category_id"]);
            let cat_key = format!("movie:{cat_id}");
            let Some(cat) = c.cats.get_mut(&cat_key) else { continue };
            cat.count += 1;
            let raw_name = s(&v["name"]);
            let raw_cat = raw_cats.get(&cat_key).map(String::as_str).unwrap_or("");
            let cl = clean_title(&raw_name);
            let tm = s(&v["tmdb"]); let tm = if tm.is_empty() || tm == "0" { None } else { Some(tm) };
            let cam = crate::showcase::is_cam_release(&raw_name, raw_cat);
            let dv = !cam && crate::showcase::is_dv_tag(&raw_name, raw_cat);
            let bluray = !cam && crate::showcase::is_bluray_tag(&raw_name, raw_cat);
            let dolby_audio = !cam && crate::showcase::is_dolby_audio_tag(&raw_name, raw_cat);
            let uhd = !cam && (cat.uhd || dv || crate::showcase::is_uhd_tag(&raw_name, raw_cat));
            let it = Item { k: "m", id: i(&v["stream_id"]), title: cl.title, year: cl.year, origin: cl.origin, poster: s(&v["stream_icon"]), rating: rating(&v["rating"]),
                tmdb: tm, added: i(&v["added"]), cat: cat_id, ext: { let e = s(&v["container_extension"]); if e.is_empty() { "mp4".into() } else { e } },
                en: cat.en, sport: cat.sport, uhd, cam, dv, bluray, dolby_audio, trailer: s(&v["trailer"]), ..Default::default() };
            c.movie_raw.insert(it.id, it.clone());
            if it.en { if let Some(t) = it.tmdb.clone() {
                if let Some(&ix) = c.movie_by_tmdb.get(&t) {
                    let prev = &mut c.movies[ix];
                    prev.variants.push(Variant { id: it.id, uhd: it.uhd, cam: it.cam, dv: it.dv, bluray: it.bluray, dolby_audio: it.dolby_audio, sub_market: false });
                    if (prev.cam && !it.cam) || (!it.cam && prev.uhd && !it.uhd) { swap_primary(prev, &it); }
                    prev.added = prev.added.max(it.added);
                    continue;
                }
                c.movie_by_tmdb.insert(t, c.movies.len());
            } }
            c.movies.push(it);
        }
        drop(vod);
        // series (two-pass: first build EN catalog & deduplicate; then attach clean subtitle-only 4K/Dolby-Audio upgrades!)
        let ser = read(&dir, "get_series");
        let mut sub_market_candidates: Vec<(String, Variant, i64)> = vec![];
        let mut en_series_by_title: HashMap<String, usize> = HashMap::new();
        for x in ser.as_array().into_iter().flatten() {
            let cat_id = s(&x["category_id"]);
            let cat_key = format!("series:{cat_id}");
            let Some(cat) = c.cats.get_mut(&cat_key) else { continue };
            cat.count += 1;
            let raw_name = s(&x["name"]);
            let raw_cat = raw_cats.get(&cat_key).map(String::as_str).unwrap_or("");
            let cl = clean_title(&raw_name);
            let rd = s(&x["releaseDate"]);
            let tm = s(&x["tmdb"]); let tm = if tm.is_empty() || tm == "0" { None } else { Some(tm) };
            let cam = crate::showcase::is_cam_release(&raw_name, raw_cat);
            let dv = !cam && crate::showcase::is_dv_tag(&raw_name, raw_cat);
            let bluray = !cam && crate::showcase::is_bluray_tag(&raw_name, raw_cat);
            let dolby_audio = !cam && crate::showcase::is_dolby_audio_tag(&raw_name, raw_cat);
            let uhd = !cam && (cat.uhd || dv || crate::showcase::is_uhd_tag(&raw_name, raw_cat));
            // OSN+ 4K is an EN HBO/Sky/Showtime category, except occasional Arabic-script standup specials
            let is_osn_non_en = cat.name.to_uppercase().starts_with("OSN+") && has_arabic_script(&raw_name);
            let en = cat.en && !is_osn_non_en;
            let it = Item { k: "s", id: i(&x["series_id"]), title: cl.title, origin: cl.origin,
                year: cl.year.or_else(|| rd.get(0..4).and_then(|y| y.parse().ok())), poster: s(&x["cover"]),
                backdrop: x["backdrop_path"].as_array().and_then(|a| a.first()).map(s).unwrap_or_default(),
                rating: rating(&x["rating"]), tmdb: tm.clone(), added: i(&x["last_modified"]), cat: cat_id, genre: s(&x["genre"]), plot: s(&x["plot"]),
                en, sport: cat.sport, uhd, cam, dv, bluray, dolby_audio, ..Default::default() };
            c.series_raw.insert(it.id, it.clone());
            if !en && !cam && (uhd || dolby_audio) && is_sub_market_en_upgrade(&cat.name) {
                if let Some(ref t) = tm {
                    sub_market_candidates.push((t.clone(), Variant { id: it.id, uhd, cam: false, dv, bluray, dolby_audio, sub_market: true }, it.added));
                }
            }
            if en {
                let norm_t = crate::util::norm(&it.title);
                if let Some(t) = it.tmdb.clone() {
                    if let Some(&ix) = c.series_by_tmdb.get(&t) {
                        let prev = &mut c.series[ix];
                        prev.variants.push(Variant { id: it.id, uhd: it.uhd, cam: it.cam, dv: it.dv, bluray: it.bluray, dolby_audio: it.dolby_audio, sub_market: false });
                        if (prev.cam && !it.cam) || (!it.cam && prev.uhd && !it.uhd) { swap_primary(prev, &it); }
                        prev.added = prev.added.max(it.added);
                        continue;
                    }
                    // Also check if an earlier TMDB-less EN entry with the exact same normalized title exists
                    if !norm_t.is_empty() {
                        if let Some(&ix) = en_series_by_title.get(&norm_t) {
                            if c.series[ix].tmdb.is_none() {
                                let prev = &mut c.series[ix];
                                prev.tmdb = Some(t.clone());
                                prev.variants.push(Variant { id: it.id, uhd: it.uhd, cam: it.cam, dv: it.dv, bluray: it.bluray, dolby_audio: it.dolby_audio, sub_market: false });
                                if (prev.cam && !it.cam) || (!it.cam && prev.uhd && !it.uhd) { swap_primary(prev, &it); }
                                prev.added = prev.added.max(it.added);
                                c.series_by_tmdb.insert(t, ix);
                                continue;
                            }
                        }
                    }
                    let ix = c.series.len();
                    c.series_by_tmdb.insert(t, ix);
                    if !norm_t.is_empty() { en_series_by_title.entry(norm_t).or_insert(ix); }
                } else if !norm_t.is_empty() {
                    if let Some(&ix) = en_series_by_title.get(&norm_t) {
                        let prev = &mut c.series[ix];
                        prev.variants.push(Variant { id: it.id, uhd: it.uhd, cam: it.cam, dv: it.dv, bluray: it.bluray, dolby_audio: it.dolby_audio, sub_market: false });
                        if (prev.cam && !it.cam) || (!it.cam && prev.uhd && !it.uhd) { swap_primary(prev, &it); }
                        prev.added = prev.added.max(it.added);
                        continue;
                    }
                    en_series_by_title.insert(norm_t, c.series.len());
                }
            }
            c.series.push(it);
        }
        // Pass 2: attach clean subtitle-only 4K / Dolby Audio upgrade variants onto existing English shows by TMDB ID
        let mut attached_sub_4k = 0usize;
        for (tmdb, v, added) in sub_market_candidates {
            if let Some(&ix) = c.series_by_tmdb.get(&tmdb) {
                let prev = &mut c.series[ix];
                if prev.id != v.id && !prev.variants.iter().any(|x| x.id == v.id) {
                    prev.variants.push(v);
                    prev.added = prev.added.max(added);
                    attached_sub_4k += 1;
                }
            }
        }
        drop(ser);
        // live (fold superscripts to clean ASCII so 4K/UHD/RAW/60fps tags survive for guide/sports/fixtures!)
        let aliases = crate::epgx::load_aliases(data);
        for l in read(&dir, "get_live_streams").as_array().into_iter().flatten() {
            let cat_id = s(&l["category_id"]);
            let Some(cat) = c.cats.get_mut(&format!("live:{cat_id}")) else { continue };
            let name = re!(r"\s{2,}").replace_all(&crate::showcase::fold_superscripts(&s(&l["name"])), " ").trim().to_string();
            if name.is_empty() || name.starts_with('#') { continue; }
            cat.count += 1;
            let sid = i(&l["stream_id"]);
            let it = Item { k: "l", id: sid, title: name, poster: s(&l["stream_icon"]), cat: cat_id, epg: aliases.get(&sid).cloned().unwrap_or_else(|| s(&l["epg_channel_id"])), en: cat.en, sport: cat.sport, added: i(&l["added"]),
                archive: if i(&l["tv_archive"]) == 1 { i(&l["tv_archive_duration"]).max(1) } else { 0 }, ..Default::default() };
            c.live_by_id.insert(it.id, c.live.len());
            c.live.push(it);
        }
        c.loaded_at = crate::util::now_ms();
        println!("[catalog] {} movies, {} series (+{} cross-market 4K/Dolby upgrades), {} channels in {}ms", c.movies.len(), c.series.len(), attached_sub_4k, c.live.len(), t0.elapsed().as_millis());
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

    /// Primary (de-duplicated) record for any stream id.
    pub fn movie(&self, id: i64) -> Option<&Item> {
        let raw = self.movie_raw.get(&id)?;
        if raw.en { if let Some(t) = &raw.tmdb { if let Some(&ix) = self.movie_by_tmdb.get(t) { return self.movies.get(ix); } } }
        // non-English primary lives in `movies` too; find by id
        self.movies.iter().find(|m| m.id == id).or(Some(raw))
    }
    pub fn series_item(&self, id: i64) -> Option<&Item> {
        let raw = self.series_raw.get(&id)?;
        if raw.en { if let Some(t) = &raw.tmdb { if let Some(&ix) = self.series_by_tmdb.get(t) { return self.series.get(ix); } } }
        self.series.iter().find(|m| m.id == id).or(Some(raw))
    }
    pub fn by_tmdb(&self, mt: &str, id: &str) -> Option<&Item> {
        if mt == "tv" { self.series_by_tmdb.get(id).map(|&i| &self.series[i]) } else { self.movie_by_tmdb.get(id).map(|&i| &self.movies[i]) }
    }
    pub fn live_item(&self, id: i64) -> Option<&Item> { self.live_by_id.get(&id).map(|&i| &self.live[i]) }

    pub fn search<'a>(&'a self, q: &str, all: bool, limit: usize) -> (Vec<&'a Item>, Vec<&'a Item>, Vec<&'a Item>) {
        let terms: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        if terms.is_empty() { return (vec![], vec![], vec![]); }
        let qq = terms.join(" ");
        let run = |list: &'a [Item]| {
            let mut out: Vec<(&Item, f64)> = list.iter().filter(|x| (all || x.en || x.sport)).filter_map(|x| {
                let t = x.title.to_lowercase();
                if !terms.iter().all(|w| t.contains(w.as_str())) { return None; }
                let r = (if t == qq { 1000.0 } else if t.starts_with(&qq) { 500.0 } else { 0.0 }) + if x.en { 100.0 } else { 0.0 } + x.year.unwrap_or(0) as f64 / 100.0 + x.rating;
                Some((x, r))
            }).collect();
            out.sort_by(|a, b| b.1.total_cmp(&a.1));
            out.into_iter().take(limit).map(|x| x.0).collect::<Vec<_>>()
        };
        (run(&self.movies), run(&self.series), run(&self.live))
    }
}

fn rating(v: &Value) -> f64 { let mut n = num(v); if n > 10.0 { n /= 10.0; } (n * 10.0).round() / 10.0 }

fn swap_primary(prev: &mut Item, it: &Item) {
    let keep = Variant { id: prev.id, uhd: prev.uhd, cam: prev.cam, dv: prev.dv, bluray: prev.bluray, dolby_audio: prev.dolby_audio, sub_market: false };
    prev.id = it.id; prev.cat = it.cat.clone(); prev.ext = it.ext.clone();
    prev.uhd = it.uhd; prev.cam = it.cam; prev.dv = it.dv; prev.bluray = it.bluray; prev.dolby_audio = it.dolby_audio;
    if !it.poster.is_empty() { prev.poster = it.poster.clone(); }
    if !it.trailer.is_empty() { prev.trailer = it.trailer.clone(); }
    if !it.backdrop.is_empty() { prev.backdrop = it.backdrop.clone(); }
    if !it.genre.is_empty() { prev.genre = it.genre.clone(); }
    if !it.plot.is_empty() { prev.plot = it.plot.clone(); }
    prev.variants.retain(|v| v.id != it.id);
    prev.variants.push(keep);
}
