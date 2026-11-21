// Catalogue: load the provider's lists, classify English/sports, de-duplicate by TMDB id, search.
use crate::re;
use crate::util::{i, num, s};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const RAW: [&str; 6] = ["get_live_categories", "get_vod_categories", "get_series_categories", "get_live_streams", "get_vod_streams", "get_series"];

#[derive(Clone, Debug, Default)]
pub struct Variant { pub id: i64, pub uhd: bool }

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

fn supers() -> &'static Regex { re!(r"[\x{1D2C}-\x{1D6A}\x{1D9C}-\x{1DBF}\x{2070}-\x{209F}\x{2C7D}\x{00B2}\x{00B3}\x{00B9}ⱽᶦ]+") }
pub fn clean_cat(n: &str) -> String {
    let t = supers().replace_all(n, "");
    let t = re!(r"\s{2,}").replace_all(&t, " ");
    re!(r"[ /]+$").replace(&t, "").trim().to_string()
}

pub struct Clean { pub title: String, pub year: Option<i32>, pub origin: String }
pub fn clean_title(name: &str) -> Clean {
    let mut t = supers().replace_all(name, "").trim().to_string();
    for _ in 0..3 { t = re!(r"^\s*[A-Z0-9+]{1,4}(?:-[A-Z0-9+]{1,5})?\s*[-|:]\s+").replace(&t, "").to_string(); }
    t = re!(r"^\d{1,3}\.\s*").replace(&t, "").to_string();
    t = re!(r"(?i)\s+(HEVC|H\.?265|x265|UHD|HDR10\+?|HDR|4K|FHD|1080p|2160p|DOLBY VISION|REMASTERED)\b").replace_all(&t, "").to_string();
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
    re!(r"(?i)^(EN\s*-|NETFLIX|DISNEY\+|APPLE\+|AMAZON|HBO|PARAMOUNT|PEACOCK|SHOWTIME|SKY$|UNIVERSAL|MARVEL|DREAMWORKS|TOP (MOVIES|KIDS)|JAMES BOND|ENGLISH (SERIES|REALITY|KIDS)|NICKELODEON|CRUNCHYROLL|DISCOVERY\+ (SERIES|MOVIES)$|CHRISTIAN MOVIES|ASIA (MOVIES|SERIES) \(MULTI)").is_match(n)
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
        for (kind, file) in [("live", "get_live_categories"), ("movie", "get_vod_categories"), ("series", "get_series_categories")] {
            for x in read(&dir, file).as_array().into_iter().flatten() {
                let raw = s(&x["category_name"]); let name = clean_cat(&raw);
                let (en, sport) = if kind == "live" { (is_en_live(&name), is_sport_live(&name)) } else { (is_en_vod(&name), is_sport_vod(&name)) };
                let id = s(&x["category_id"]);
                c.cats.insert(format!("{kind}:{id}"), Cat { id, kind, name, en, sport, uhd: is_uhd(&raw), count: 0 });
            }
        }
        // movies
        let vod = read(&dir, "get_vod_streams");
        for v in vod.as_array().into_iter().flatten() {
            let cat_id = s(&v["category_id"]);
            let Some(cat) = c.cats.get_mut(&format!("movie:{cat_id}")) else { continue };
            cat.count += 1;
            let cl = clean_title(&s(&v["name"]));
            let tm = s(&v["tmdb"]); let tm = if tm.is_empty() || tm == "0" { None } else { Some(tm) };
            let it = Item { k: "m", id: i(&v["stream_id"]), title: cl.title, year: cl.year, origin: cl.origin, poster: s(&v["stream_icon"]), rating: rating(&v["rating"]),
                tmdb: tm, added: i(&v["added"]), cat: cat_id, ext: { let e = s(&v["container_extension"]); if e.is_empty() { "mp4".into() } else { e } },
                en: cat.en, sport: cat.sport, uhd: cat.uhd, trailer: s(&v["trailer"]), ..Default::default() };
            c.movie_raw.insert(it.id, it.clone());
            if it.en { if let Some(t) = it.tmdb.clone() {
                if let Some(&ix) = c.movie_by_tmdb.get(&t) {
                    let prev = &mut c.movies[ix];
                    prev.variants.push(Variant { id: it.id, uhd: it.uhd });
                    if prev.uhd && !it.uhd { swap_primary(prev, &it); }
                    prev.added = prev.added.max(it.added);
                    continue;
                }
                c.movie_by_tmdb.insert(t, c.movies.len());
            } }
            c.movies.push(it);
        }
        drop(vod);
        // series
        let ser = read(&dir, "get_series");
        for x in ser.as_array().into_iter().flatten() {
            let cat_id = s(&x["category_id"]);
            let Some(cat) = c.cats.get_mut(&format!("series:{cat_id}")) else { continue };
            cat.count += 1;
            let cl = clean_title(&s(&x["name"]));
            let rd = s(&x["releaseDate"]);
            let tm = s(&x["tmdb"]); let tm = if tm.is_empty() || tm == "0" { None } else { Some(tm) };
            let it = Item { k: "s", id: i(&x["series_id"]), title: cl.title, origin: cl.origin,
                year: cl.year.or_else(|| rd.get(0..4).and_then(|y| y.parse().ok())), poster: s(&x["cover"]),
                backdrop: x["backdrop_path"].as_array().and_then(|a| a.first()).map(s).unwrap_or_default(),
                rating: rating(&x["rating"]), tmdb: tm, added: i(&x["last_modified"]), cat: cat_id, genre: s(&x["genre"]), plot: s(&x["plot"]),
                en: cat.en, sport: cat.sport, uhd: cat.uhd, ..Default::default() };
            c.series_raw.insert(it.id, it.clone());
            if it.en { if let Some(t) = it.tmdb.clone() {
                if let Some(&ix) = c.series_by_tmdb.get(&t) {
                    let prev = &mut c.series[ix];
                    prev.variants.push(Variant { id: it.id, uhd: it.uhd });
                    if prev.uhd && !it.uhd { swap_primary(prev, &it); }
                    prev.added = prev.added.max(it.added);
                    continue;
                }
                c.series_by_tmdb.insert(t, c.series.len());
            } }
            c.series.push(it);
        }
        drop(ser);
        // live (guide ids from the extra guide where the provider has none: see epgx.rs)
        let aliases = crate::epgx::load_aliases(data);
        for l in read(&dir, "get_live_streams").as_array().into_iter().flatten() {
            let cat_id = s(&l["category_id"]);
            let Some(cat) = c.cats.get_mut(&format!("live:{cat_id}")) else { continue };
            let name = supers().replace_all(&s(&l["name"]), "").trim().to_string();
            if name.is_empty() || name.starts_with('#') { continue; }
            cat.count += 1;
            let sid = i(&l["stream_id"]);
            let it = Item { k: "l", id: sid, title: name, poster: s(&l["stream_icon"]), cat: cat_id, epg: aliases.get(&sid).cloned().unwrap_or_else(|| s(&l["epg_channel_id"])), en: cat.en, sport: cat.sport, added: i(&l["added"]),
                archive: if i(&l["tv_archive"]) == 1 { i(&l["tv_archive_duration"]).max(1) } else { 0 }, ..Default::default() };
            c.live_by_id.insert(it.id, c.live.len());
            c.live.push(it);
        }
        c.loaded_at = crate::util::now_ms();
        println!("[catalog] {} movies, {} series, {} channels in {}ms", c.movies.len(), c.series.len(), c.live.len(), t0.elapsed().as_millis());
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
    let keep = Variant { id: prev.id, uhd: prev.uhd };
    prev.id = it.id; prev.cat = it.cat.clone(); prev.ext = it.ext.clone(); prev.uhd = it.uhd;
    if !it.poster.is_empty() { prev.poster = it.poster.clone(); }
    if !it.trailer.is_empty() { prev.trailer = it.trailer.clone(); }
    if !it.backdrop.is_empty() { prev.backdrop = it.backdrop.clone(); }
    if !it.genre.is_empty() { prev.genre = it.genre.clone(); }
    if !it.plot.is_empty() { prev.plot = it.plot.clone(); }
    prev.variants.retain(|v| v.id != it.id);
    prev.variants.push(keep);
}
