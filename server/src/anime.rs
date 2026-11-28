// English-dubbed Anime hub: pure Japanese animation discovery, persistent episode-level
// English dub verification (probe-cache + dub-cache with persisted sid index + idle background worker),
// and curated anime rows / filterable browse.
use crate::app::App;
use crate::catalog::Item;
use crate::meta::Meta;
use crate::rec::{match_results, sp};
use crate::tmdb::TTL_H;
use crate::util::{now_ms, s};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static DUB_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn app_port(app: &App) -> u16 {
    std::env::var("TV_PORT").ok().and_then(|p| p.parse().ok())
        .unwrap_or_else(|| app.cfg.get("port").as_u64().unwrap_or(7878) as u16)
}

struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DubEntry {
    pub has_en: bool,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub src: String, // "probe" | "meta" | "name"
    #[serde(default)]
    pub t: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<i64>,
}

#[derive(Default)]
struct DubStore {
    map: HashMap<String, DubEntry>,
    by_sid: HashMap<i64, HashSet<String>>,
}

pub struct DubCache {
    file: PathBuf,
    store: Mutex<DubStore>, // "e:<id>" or "m:<id>" + persistent sid -> keys index
}

fn atomic_write_dub(path: &Path, body: &[u8]) {
    let seq = DUB_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{seq}.tmp", std::process::id()));
    if std::fs::write(&tmp, body).is_err() { return; }
    if std::fs::rename(&tmp, path).is_ok() { return; }
    for attempt in 0..5u64 {
        let _ = std::fs::remove_file(path);
        if std::fs::rename(&tmp, path).is_ok() { return; }
        std::thread::sleep(Duration::from_millis(25 * (attempt + 1)));
    }
    let _ = std::fs::remove_file(&tmp);
}

impl DubCache {
    pub fn load(data: &Path) -> Self {
        let file = data.join("dub-cache.json");
        let map: HashMap<String, DubEntry> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut by_sid: HashMap<i64, HashSet<String>> = HashMap::new();
        for (k, v) in &map {
            if let Some(sid) = v.sid.filter(|&s| s > 0) {
                by_sid.entry(sid).or_default().insert(k.clone());
            }
        }
        DubCache { file, store: Mutex::new(DubStore { map, by_sid }) }
    }
    pub fn get(&self, key: &str) -> Option<DubEntry> {
        self.store.lock().unwrap().map.get(key).cloned()
    }
    pub fn set(&self, key: String, mut entry: DubEntry) {
        let body = {
            let mut g = self.store.lock().unwrap();
            // Never overwrite a ground-truth probe result with a weaker heuristic,
            // though we still backfill `sid` if previously missing.
            let prev_info = g.map.get(&key).map(|p| (p.src == "probe", p.sid));
            if let Some((prev_is_probe, prev_sid)) = prev_info {
                if entry.src != "probe" && prev_is_probe {
                    if prev_sid.is_none() {
                        if let Some(new_sid) = entry.sid.filter(|&s| s > 0) {
                            if let Some(p) = g.map.get_mut(&key) { p.sid = Some(new_sid); }
                            g.by_sid.entry(new_sid).or_default().insert(key.clone());
                            serde_json::to_vec(&g.map).ok()
                        } else {
                            return;
                        }
                    } else {
                        return;
                    }
                } else {
                    if entry.sid.is_none() { entry.sid = prev_sid; }
                    if let Some(old_sid) = prev_sid.filter(|&s| s > 0 && Some(s) != entry.sid) {
                        if let Some(set) = g.by_sid.get_mut(&old_sid) { set.remove(&key); }
                    }
                    if let Some(new_sid) = entry.sid.filter(|&s| s > 0) {
                        g.by_sid.entry(new_sid).or_default().insert(key.clone());
                    }
                    g.map.insert(key, entry);
                    serde_json::to_vec(&g.map).ok()
                }
            } else {
                if let Some(new_sid) = entry.sid.filter(|&s| s > 0) {
                    g.by_sid.entry(new_sid).or_default().insert(key.clone());
                }
                g.map.insert(key, entry);
                serde_json::to_vec(&g.map).ok()
            }
        };
        if let Some(body) = body {
            atomic_write_dub(&self.file, &body);
        }
    }
    pub fn snapshot(&self) -> HashMap<String, DubEntry> {
        self.store.lock().unwrap().map.clone()
    }
    pub fn counts(&self) -> (usize, usize, usize) {
        let g = self.store.lock().unwrap();
        let mut yes = 0usize;
        let mut no = 0usize;
        for d in g.map.values() {
            if d.has_en { yes += 1; } else { no += 1; }
        }
        (yes, no, g.map.len())
    }
    /// Evaluate a series's dub status under a single brief lock (ZERO HashMap clone),
    /// combining live `xinfo` episodes (if loaded) AND persisted `by_sid` entries (surviving restarts!).
    fn eval_series(&self, sids: &[i64], ep_list: &[(i64, i64, usize)]) -> (&'static str, usize, usize) {
        let g = self.store.lock().unwrap();
        let mut seen_eids: HashSet<i64> = HashSet::new();
        let mut total_eps = 0usize;
        let mut yes = 0usize;
        let mut no = 0usize;
        let mut early_no = false;
        for &(eid, sn, idx) in ep_list {
            if eid <= 0 || !seen_eids.insert(eid) { continue; }
            total_eps += 1;
            if let Some(d) = g.map.get(&format!("e:{eid}")) {
                if d.has_en {
                    yes += 1;
                } else {
                    no += 1;
                    if sn == 1 && idx < 3 { early_no = true; }
                }
            }
        }
        // Also incorporate persisted `by_sid` entries so summaries work immediately after app restarts!
        for &s in sids {
            if let Some(keys) = g.by_sid.get(&s) {
                for k in keys {
                    if let Some(eid) = k.strip_prefix("e:").and_then(|x| x.parse::<i64>().ok()) {
                        if !seen_eids.insert(eid) { continue; }
                    }
                    if let Some(d) = g.map.get(k) {
                        if d.has_en { yes += 1; } else { no += 1; }
                    }
                }
            }
        }
        if yes > 0 {
            ("yes", yes, no)
        } else if no > 0 && ((total_eps > 0 && (total_eps <= 2 || no >= total_eps || (early_no && no >= 2) || no * 2 >= total_eps))
            || (total_eps == 0 && no >= 2))
        {
            ("no", 0, no)
        } else {
            ("unknown", 0, no)
        }
    }
}

fn is_en_lang(lang: &str) -> bool {
    matches!(lang, "en" | "eng" | "en-us" | "en-gb" | "en-au" | "en-ca")
        || lang.starts_with("en-")
        || lang.starts_with("en_")
}

fn is_untagged_lang(lang: &str) -> bool {
    lang.is_empty() || matches!(lang, "und" | "unk" | "zxx" | "mul" | "qaa")
}

/// Inspect ffprobe's audio track array: Some(true) = English dub present,
/// Some(false) = confirmed non-English only (e.g. Japanese/French only), None = untagged/uncertain.
///
/// Checks `lang` FIRST so explicit non-English languages (`fr`, `es`, `de`, `it`, `pt`, `ru`, `ar`, `tr`, `pl`, `nl`, etc.)
/// are NEVER misclassified as English even if title says `"French Dub"`, and treats dual-audio `[jpn, und/""]`
/// as `"unknown"` (`None`) instead of falsely condemning an untagged second audio track as Japanese-only.
pub fn has_english_audio(audio: &[Value]) -> Option<bool> {
    if audio.is_empty() { return None; }
    let mut saw_non_en = false;
    let mut saw_untagged = false;
    let non_en_title_re = crate::re!(r"(?i)\b(japanese|jpn|jap|original|french|fran[çc]ais|vff|vfq|vf|vostfr|spanish|espa[ñn]ol|latino|castellano|german|deutsch|italian|italiano|portuguese|portugu[êe]s|russian|arabic|turkish|polish|dutch|korean|chinese|mandarin|cantonese|hindi)\b");
    for a in audio {
        let lang = a["lang"].as_str().unwrap_or("").trim().to_lowercase();
        let title = a["title"].as_str().unwrap_or("").trim().to_lowercase();
        let is_comm = crate::re!(r"(?i)\b(commentary|audio description|descriptive|ad)\b").is_match(&title);
        if is_comm { continue; }
        // 1. Check explicit language tag FIRST!
        if is_en_lang(&lang) {
            return Some(true);
        }
        if !is_untagged_lang(&lang) {
            // Explicit non-English language tag (`ja`, `jpn`, `fr`, `fre`, `es`, `de`, `it`, `pt`, `ru`, `ar`, `tr`, `pl`, `nl`, etc.)
            saw_non_en = true;
            continue;
        }
        // 2. Untagged track (`""` / `"und"` / `"unk"`): inspect title carefully
        if non_en_title_re.is_match(&title) {
            saw_non_en = true;
            continue;
        }
        if crate::re!(r"(?i)\b(english|eng|dub|dubbed)\b").is_match(&title) && !crate::re!(r"(?i)\b(sub|subbed)\b").is_match(&title) {
            return Some(true);
        }
        saw_untagged = true;
    }
    if saw_non_en && saw_untagged {
        // Dual-audio with one known non-English track (e.g. `jpn`) and another untagged track (`""`/`"und"`):
        // do NOT falsely condemn it as Japanese-only.
        None
    } else if saw_non_en {
        Some(false)
    } else {
        None
    }
}

fn infer_episode_sid(app: &App, eid: i64) -> Option<i64> {
    if eid <= 0 { return None; }
    let key = format!("e:{eid}");
    let from_state = app.state.read(|st| {
        st["progress"].get(&key).and_then(|p| p["item"]["sid"].as_i64())
            .or_else(|| st["dubWatch"].get(&key).and_then(|w| w["sid"].as_i64()))
    });
    if let Some(raw_sid) = from_state.filter(|&s| s > 0) {
        return Some(app.catalog().series_item(raw_sid).map(|it| it.id).unwrap_or(raw_sid));
    }
    // Check cached xinfo entries if already loaded
    let from_xinfo: Option<i64> = {
        let g = app.xinfo.lock().unwrap();
        let mut found = None;
        'outer: for (xk, (_, v)) in g.iter() {
            let Some(sid_s) = xk.strip_prefix("get_series_info:") else { continue };
            let Ok(raw_sid) = sid_s.parse::<i64>() else { continue };
            for list in v["episodes"].as_object().into_iter().flat_map(|m| m.values()) {
                for ep in list.as_array().into_iter().flatten() {
                    if crate::util::i(&ep["id"]) == eid {
                        found = Some(raw_sid);
                        break 'outer;
                    }
                }
            }
        }
        found
    };
    from_xinfo.map(|raw_sid| app.catalog().series_item(raw_sid).map(|it| it.id).unwrap_or(raw_sid))
}

/// Record audio tracks from any ffprobe call into `dub-cache.json` with an explicit series ID (when known).
pub fn record_probe_for(app: &App, kind: &str, id: i64, sid: Option<i64>, audio: &[Value]) -> Option<bool> {
    let has = has_english_audio(audio)?;
    let prefix = if kind == "movie" { "m" } else { "e" };
    let langs: Vec<String> = audio.iter().filter_map(|a| {
        let l = a["lang"].as_str().unwrap_or("").trim();
        if l.is_empty() { None } else { Some(l.to_lowercase()) }
    }).collect();
    let canon_sid = if kind == "movie" {
        None
    } else {
        sid.filter(|&s| s > 0)
            .map(|raw| app.catalog().series_item(raw).map(|it| it.id).unwrap_or(raw))
            .or_else(|| infer_episode_sid(app, id))
    };
    app.dubs.set(format!("{prefix}:{id}"), DubEntry { has_en: has, langs, src: "probe".into(), t: now_ms(), sid: canon_sid });
    Some(has)
}

/// Record audio tracks from any ffprobe call into `dub-cache.json`.
pub fn record_probe(app: &App, kind: &str, id: i64, audio: &[Value]) -> Option<bool> {
    record_probe_for(app, kind, id, None, audio)
}

/// Episode-level dub status: "yes" | "no" | "unknown".
pub fn ep_dub_status(app: &App, eid: i64, ep_val: Option<&Value>, cat_name: &str) -> &'static str {
    let key = format!("e:{eid}");
    if let Some(d) = app.dubs.get(&key) {
        return if d.has_en { "yes" } else { "no" };
    }
    if let Some(audio) = app.stream.audio_of(eid) {
        if let Some(has) = record_probe(app, "series", eid, &audio) {
            return if has { "yes" } else { "no" };
        }
    }
    if let Some(ev) = ep_val {
        let title = s(&ev["title"]);
        if crate::re!(r"(?i)(\bsub(bed)?\b|\[sub\]|\(sub\)|\bjap\b|\braw\b|\bjpn only\b)").is_match(&title)
            && !crate::re!(r"(?i)\bdub(bed)?\b|multi").is_match(&title) {
            return "no";
        }
        if crate::re!(r"(?i)(\bdub(bed)?\b|\[dub\]|\(dub\)|\beng(lish)? dub\b)").is_match(&title) {
            return "yes";
        }
        // Xtream get_series_info sometimes carries episode.info.audio.tags.language
        let lang = ev["info"]["audio"]["tags"]["language"].as_str()
            .or_else(|| ev["info"]["audio"]["language"].as_str())
            .unwrap_or("").trim().to_lowercase();
        if is_en_lang(&lang) {
            return "yes";
        }
    }
    if crate::re!(r"(?i)\bSUBS?\b").is_match(cat_name) && !crate::re!(r"(?i)MULTI-SUBS|DUB").is_match(cat_name) {
        return "no";
    }
    "unknown"
}

/// Movie-level dub status: "yes" | "no" | "unknown".
pub fn movie_dub_status(app: &App, it: &Item) -> &'static str {
    let mut ids = vec![it.id];
    for v in &it.variants { ids.push(v.id); }
    let mut saw_no = false;
    for id in ids {
        if let Some(d) = app.dubs.get(&format!("m:{id}")) {
            if d.has_en { return "yes"; }
            saw_no = true;
            continue;
        }
        if let Some(audio) = app.stream.audio_of(id) {
            if let Some(has) = record_probe(app, "movie", id, &audio) {
                if has { return "yes"; }
                saw_no = true;
            }
        }
    }
    if saw_no { "no" } else { "unknown" }
}

fn is_anime_pre_meta_fallback(cat_upper: &str, origin: &str, genre: &str) -> bool {
    if origin == "JP" && (genre.to_lowercase().contains("anim") || cat_upper.contains("ANIME") || cat_upper.contains("ANIMI") || cat_upper.contains("MANGA") || cat_upper.contains("CRUNCHYROLL")) {
        return true;
    }
    let non_jp_origin = matches!(origin, "US" | "GB" | "UK" | "FR" | "AU" | "CA" | "DE" | "ES" | "IT");
    if !non_jp_origin && (cat_upper.contains("CRUNCHYROLL")
        || cat_upper.contains("NETFLIX ANIME")
        || cat_upper.contains("EN - ANIME")
        || cat_upper.contains("MANGA/ANIME")
        || cat_upper.contains("ANIME SERIES")
        || cat_upper.contains("ANIME MOVIES"))
    {
        return true;
    }
    false
}

/// True for authentic Japanese anime (movies & series) in English/Anime categories,
/// excluding Western cartoons (e.g. Korra, Cuphead, Disenchantment, Alvin & the Chipmunks)
/// that IPTV providers frequently file into NETFLIX ANIME or EN - MANGA/ANIME.
pub fn is_anime_item(app: &App, it: &Item) -> bool {
    if !it.en { return false; }
    let cat_name = app.catalog().cat_name(if it.k == "s" { "series" } else { "movie" }, &it.cat).to_uppercase();
    if let Some(m) = app.meta_of(it) {
        let is_anim = m.g.contains(&16);
        let is_jp = m.l == "ja" || m.oc.iter().any(|c| c == "JP");
        if is_anim && is_jp { return true; }
        // If TMDB explicitly reports a non-Japanese language/origin or non-animation genre, exclude it
        // even if the provider placed it inside NETFLIX ANIME or CRUNCHYROLL.
        if !m.l.is_empty() && !is_jp { return false; }
        if !m.g.is_empty() && !is_anim { return false; }
    }
    // Fallback before TMDB metadata is indexed
    is_anime_pre_meta_fallback(&cat_name, &it.origin, &it.genre)
}

fn canonical_anime_genre(g: i64) -> i64 {
    match g {
        28 | 12 => 10759, // Action / Adventure -> Action & Adventure
        14 | 878 => 10765, // Fantasy / Sci-Fi -> Sci-Fi & Fantasy
        10752 => 10768, // War -> War & Politics
        x => x,
    }
}

/// Series-level dub summary across cached episode probes (checks primary + native EN variants):
/// ("yes" | "no" | "unknown", dubbed_count, sub_only_count).
///
/// Uses persistent `by_sid` index in `DubCache` (survives restarts without waiting for `xinfo`)
/// and queries under a single brief lock (zero HashMap cloning).
pub fn series_dub_summary(app: &App, sid: i64) -> (&'static str, usize, usize) {
    let cat = app.catalog();
    let mut sids: Vec<i64> = vec![sid];
    if let Some(it) = cat.series_item(sid) {
        if it.id != sid { sids.insert(0, it.id); }
        for v in &it.variants {
            if !v.sub_market && !sids.contains(&v.id) { sids.push(v.id); }
        }
    }
    let infos: Vec<Arc<Value>> = {
        let g = app.xinfo.lock().unwrap();
        sids.iter().filter_map(|id| g.get(&format!("get_series_info:{id}")).map(|(_, v)| v.clone())).collect()
    };
    let mut ep_list: Vec<(i64, i64, usize)> = vec![];
    for info in &infos {
        if let Some(m) = info["episodes"].as_object() {
            for (sk, list) in m {
                let sn: i64 = sk.parse().unwrap_or(0);
                for (idx, e) in list.as_array().into_iter().flatten().enumerate() {
                    let eid = crate::util::i(&e["id"]);
                    if eid > 0 { ep_list.push((eid, sn, idx)); }
                }
            }
        }
    }
    app.dubs.eval_series(&sids, &ep_list)
}

fn anime_card_with(app: &App, it: &Item, tm: Option<&Value>, tv_dubs: &HashMap<i64, (&'static str, usize, usize)>) -> Value {
    let mut c = match tm { Some(t) => app.card(it, Some(t)), None => app.card_meta(it) };
    c["isAnime"] = json!(true);
    let (ds, dc) = if it.k == "s" {
        let (st, y, _) = tv_dubs.get(&it.id).copied().unwrap_or_else(|| series_dub_summary(app, it.id));
        (st, y)
    } else {
        (movie_dub_status(app, it), 0)
    };
    c["dubStatus"] = json!(ds);
    if dc > 0 { c["dubbedCount"] = json!(dc); }
    if it.k == "s" {
        if let Some(sf) = crate::filler::builtin_lookup(&it.title) {
            c["fillerPct"] = json!(sf.filler_pct().round() as i64);
        }
    }
    c
}

fn anime_pool_with_cache(app: &App, include_sub_only: bool) -> (Vec<Item>, Vec<Item>, HashMap<i64, (&'static str, usize, usize)>) {
    let cat = app.catalog();
    let mut tv_dubs: HashMap<i64, (&'static str, usize, usize)> = HashMap::new();
    let mut tv: Vec<Item> = vec![];
    for it in cat.series.iter().filter(|it| is_anime_item(app, it)) {
        let sum = series_dub_summary(app, it.id);
        tv_dubs.insert(it.id, sum);
        if include_sub_only || sum.0 != "no" {
            tv.push(it.clone());
        }
    }
    let mv: Vec<Item> = cat.movies.iter().filter(|it| is_anime_item(app, it)).filter(|it| {
        include_sub_only || movie_dub_status(app, it) != "no"
    }).cloned().collect();
    (tv, mv, tv_dubs)
}

fn anime_pool(app: &App) -> (Vec<Item>, Vec<Item>) {
    let (tv, mv, _) = anime_pool_with_cache(app, false);
    (tv, mv)
}

fn sort_pairs<'a>(mut v: Vec<(&'a Item, Option<Meta>)>, mode: &str) -> Vec<&'a Item> {
    match mode {
        "rating" => v.sort_by(|a, b| {
            let sa = a.1.as_ref().map(|m| m.bayes()).unwrap_or(a.0.rating);
            let sb = b.1.as_ref().map(|m| m.bayes()).unwrap_or(b.0.rating);
            sb.total_cmp(&sa)
        }),
        "votes" => v.sort_by(|a, b| {
            let va = a.1.as_ref().map(|m| m.vc).unwrap_or(0);
            let vb = b.1.as_ref().map(|m| m.vc).unwrap_or(0);
            vb.cmp(&va).then_with(|| {
                let sa = a.1.as_ref().map(|m| m.bayes()).unwrap_or(a.0.rating);
                let sb = b.1.as_ref().map(|m| m.bayes()).unwrap_or(b.0.rating);
                sb.total_cmp(&sa)
            })
        }),
        "newest" => v.sort_by(|a, b| {
            let da = a.1.as_ref().map(|m| m.d.as_str()).unwrap_or("");
            let db = b.1.as_ref().map(|m| m.d.as_str()).unwrap_or("");
            db.cmp(da).then(b.0.year.cmp(&a.0.year))
        }),
        "oldest" => v.sort_by(|a, b| {
            let da = a.1.as_ref().map(|m| m.d.as_str()).filter(|s| !s.is_empty()).unwrap_or("9999");
            let db = b.1.as_ref().map(|m| m.d.as_str()).filter(|s| !s.is_empty()).unwrap_or("9999");
            da.cmp(db).then(a.0.year.unwrap_or(9999).cmp(&b.0.year.unwrap_or(9999)))
        }),
        "added" => v.sort_by(|a, b| b.0.added.cmp(&a.0.added)),
        "az" => v.sort_by(|a, b| {
            a.0.title.bytes().map(|c| c.to_ascii_lowercase())
                .cmp(b.0.title.bytes().map(|c| c.to_ascii_lowercase()))
        }),
        _ => v.sort_by(|a, b| {
            let pa = a.1.as_ref().map(|m| m.p).unwrap_or(a.0.rating);
            let pb = b.1.as_ref().map(|m| m.p).unwrap_or(b.0.rating);
            pb.total_cmp(&pa)
        }),
    }
    v.into_iter().map(|x| x.0).collect()
}

fn sort_meta<'a>(app: &App, list: Vec<&'a Item>, mode: &str) -> Vec<&'a Item> {
    let v: Vec<(&'a Item, Option<Meta>)> = list.into_iter().map(|it| (it, app.meta_of(it))).collect();
    sort_pairs(v, mode)
}

fn has_kw_or_genre(app: &App, it: &Item, genres: &[i64], plot_re: &regex::Regex) -> bool {
    if let Some(m) = app.meta_of(it) {
        if genres.iter().any(|g| m.g.contains(g)) { return true; }
        if plot_re.is_match(&m.ov) { return true; }
    }
    plot_re.is_match(&it.plot) || plot_re.is_match(&it.title) || plot_re.is_match(&it.genre)
}

/// GET /api/anime: Modern hero + rich curated anime rows (matching & exceeding the TV Series hub).
pub async fn page(app: Arc<App>) -> Value {
    let (tv, mv, tv_dubs) = anime_pool_with_cache(&app, false);
    let cat = app.catalog();
    let mut rows: Vec<Value> = vec![];
    let mut hero_items: Vec<Value> = vec![];

    if app.tmdb.enabled() {
        let cur_year = chrono::Datelike::year(&chrono::Utc::now());
        let modern_from = format!("{}-01-01", (cur_year - 9).max(2016));
        let recent_from = format!("{}-01-01", (cur_year - 2).max(2024));
        let (wk_trend, on_air, mod_pop, new_rel, tr_mv) = tokio::join!(
            app.tmdb.pages("/trending/tv/week", 4, crate::tmdb::NO, 3 * TTL_H),
            app.tmdb.pages("/tv/on_the_air", 4, crate::tmdb::NO, 3 * TTL_H),
            app.tmdb.pages("/discover/tv", 3, [sp("with_genres", "16"), sp("with_original_language", "ja"), sp("first_air_date.gte", &modern_from), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "80")], 6 * TTL_H),
            app.tmdb.pages("/discover/tv", 3, [sp("with_genres", "16"), sp("with_original_language", "ja"), sp("first_air_date.gte", &recent_from), sp("sort_by", "popularity.desc")], 6 * TTL_H),
            app.tmdb.pages("/discover/movie", 2, [sp("with_genres", "16"), sp("with_original_language", "ja"), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "50")], 6 * TTL_H)
        );
        let decorate_tv = |app: &App, list: Vec<Value>| -> Vec<Value> {
            let cat = app.catalog();
            list.into_iter().filter_map(|mut c| {
                let id = c["id"].as_i64().unwrap_or(0);
                let it = cat.series_item(id)?;
                if !is_anime_item(app, it) { return None; }
                let (st, dc, _) = tv_dubs.get(&it.id).copied().unwrap_or_else(|| series_dub_summary(app, it.id));
                if st == "no" { return None; }
                c["isAnime"] = json!(true);
                c["dubStatus"] = json!(st);
                if dc > 0 { c["dubbedCount"] = json!(dc); }
                if let Some(sf) = crate::filler::builtin_lookup(&s(&c["title"])) {
                    c["fillerPct"] = json!(sf.filler_pct().round() as i64);
                }
                Some(c)
            }).collect()
        };
        let decorate_mv = |app: &App, list: Vec<Value>| -> Vec<Value> {
            let cat = app.catalog();
            list.into_iter().filter_map(|mut c| {
                let id = c["id"].as_i64().unwrap_or(0);
                let it = cat.movie(id)?;
                if !is_anime_item(app, it) { return None; }
                let ds = movie_dub_status(app, it);
                if ds == "no" { return None; }
                c["isAnime"] = json!(true);
                c["dubStatus"] = json!(ds);
                Some(c)
            }).collect()
        };
        let wk_hits = decorate_tv(&app, match_results(&app, &wk_trend, Some("tv"), 40));
        let air_hits = decorate_tv(&app, match_results(&app, &on_air, Some("tv"), 40));
        let mod_hits = decorate_tv(&app, match_results(&app, &mod_pop, Some("tv"), 40));
        let new_hits = decorate_tv(&app, match_results(&app, &new_rel, Some("tv"), 40));
        let mv_hits = decorate_mv(&app, match_results(&app, &tr_mv, Some("movie"), 40));

        let mut trend_seen: HashSet<i64> = HashSet::new();
        let mut trending: Vec<Value> = vec![];
        for c in wk_hits.iter().chain(mod_hits.iter()) {
            let id = c["id"].as_i64().unwrap_or(0);
            if id > 0 && trend_seen.insert(id) { trending.push(c.clone()); }
        }
        trending.truncate(40);

        let mut air_seen: HashSet<i64> = HashSet::new();
        let mut airing: Vec<Value> = vec![];
        for c in air_hits.iter().chain(new_hits.iter()) {
            let id = c["id"].as_i64().unwrap_or(0);
            if id > 0 && air_seen.insert(id) { airing.push(c.clone()); }
        }
        let local_new: Vec<&Item> = sort_meta(&app, tv.iter().filter(|it| {
            let yr = app.meta_of(it).map(|m| m.year()).filter(|&y| y > 0).or(it.year).unwrap_or(0);
            yr >= cur_year - 1
        }).collect(), "popular");
        for it in local_new {
            if air_seen.insert(it.id) { airing.push(anime_card_with(&app, it, None, &tv_dubs)); }
        }
        airing.truncate(40);

        let mut hero_seen: HashSet<i64> = HashSet::new();
        let mut classics = 0usize;
        for c in wk_hits.iter().chain(mod_hits.iter()).chain(air_hits.iter()).chain(mv_hits.iter()) {
            if hero_items.len() >= 6 { break; }
            let id = c["id"].as_i64().unwrap_or(0);
            if id == 0 || s(&c["backdrop"]).is_empty() || s(&c["overview"]).is_empty() { continue; }
            let yr = c["year"].as_i64().unwrap_or(0);
            if yr > 0 && yr < 2015 {
                if classics >= 1 { continue; }
                classics += 1;
            }
            if hero_seen.insert(id) { hero_items.push(c.clone()); }
        }

        if !trending.is_empty() {
            rows.push(json!({ "id": "an-trend", "title": "Trending Anime This Week", "sub": "What's hot right now · auto-skip ready", "items": trending }));
        }
        if !airing.is_empty() {
            rows.push(json!({ "id": "an-air", "title": "Airing Now & New Releases", "sub": format!("Currently airing & {}-{} drops", cur_year - 1, cur_year), "items": airing }));
        }
        if !mv_hits.is_empty() {
            rows.push(json!({ "id": "an-films", "title": "Studio Ghibli & Popular Anime Films", "items": mv_hits }));
        }
    }

    let tv_refs: Vec<&Item> = tv.iter().collect();
    let mv_refs: Vec<&Item> = mv.iter().collect();

    if hero_items.len() < 3 {
        let mut seen: HashSet<i64> = hero_items.iter().filter_map(|x| x["id"].as_i64()).collect();
        for x in sort_meta(&app, tv_refs.clone(), "popular") {
            if hero_items.len() >= 6 { break; }
            let has_bd = !x.backdrop.is_empty() || app.meta_of(x).map_or(false, |m| !m.bd.is_empty());
            if has_bd && seen.insert(x.id) { hero_items.push(anime_card_with(&app, x, None, &tv_dubs)); }
        }
    }

    // Platform rows
    let cr_items: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| cat.item_has_platform(it, "Crunchyroll")).collect(), "popular")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if cr_items.len() >= 4 {
        rows.push(json!({ "id": "an-plat-cr", "title": "Popular on Crunchyroll", "sub": "Simuldub & Crunchyroll originals", "items": cr_items }));
    }
    let nf_items: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| cat.item_has_platform(it, "Netflix")).collect(), "popular")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if nf_items.len() >= 6 {
        rows.push(json!({ "id": "an-plat-nf", "title": "Popular on Netflix Anime", "items": nf_items }));
    }

    // Shonen & Battle Classics
    let shonen: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[10759], crate::re!(r"(?i)battle|shonen|fighter|tournament|ninja|pirate|hero|hunter|slayer|curse|alchemy|clover|magic knight|soul reaper"))).collect(), "popular")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !shonen.is_empty() { rows.push(json!({ "id": "an-shonen", "title": "Shonen & Battle Classics", "items": shonen })); }

    // Dark Fantasy, Seinen & Psychological
    let dark: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[80, 9648], crate::re!(r"(?i)demon|vampire|blood|death|dark|curse|ghoul|psycho|assassin|revenge|monster|titan|chainsaw|berserk|parasyte|hell|cyberpunk|vinland"))).collect(), "rating")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !dark.is_empty() { rows.push(json!({ "id": "an-dark", "title": "Dark Fantasy & Seinen", "items": dark })); }

    // Isekai, Magic & Fantasy
    let fantasy: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[10765], crate::re!(r"(?i)reincarnat|another world|isekai|dungeon|magic|guild|level|rpg|mecha|gundam|cyber|future|space|elf|slime"))).collect(), "popular")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !fantasy.is_empty() { rows.push(json!({ "id": "an-fantasy", "title": "Isekai, Magic & Sci-Fi", "items": fantasy })); }

    // Zero & Low-Filler Bangers
    let no_filler: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| {
        let good = app.meta_of(it).map_or(it.rating >= 7.8, |m| m.va >= 7.8 && m.vc >= 150);
        if !good { return false; }
        match crate::filler::builtin_lookup(&it.title) {
            Some(sf) => sf.filler_pct() <= 15.0,
            None => true,
        }
    }).collect(), "rating").into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if no_filler.len() >= 6 {
        rows.push(json!({ "id": "an-nofiller", "title": "Zero & Low-Filler Bangers", "sub": "All story, minimal filler (<15%)", "items": no_filler }));
    }

    // Short & Complete
    let short_binge: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| {
        app.meta_of(it).map_or(false, |m| m.st.eq_ignore_ascii_case("Ended") && m.va >= 7.6 && m.vc >= 80 && m.ns > 0 && m.ns <= 2)
    }).collect(), "rating").into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if short_binge.len() >= 6 {
        rows.push(json!({ "id": "an-short", "title": "Short & Complete (1–2 Seasons)", "sub": "Finished stories you can binge this weekend", "items": short_binge }));
    }

    // Comedy & Slice of Life
    let comedy: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[35], crate::re!(r"(?i)comedy|school|slice of life|hilarious|spy|family|romantic|romance"))).collect(), "rating")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if comedy.len() >= 6 { rows.push(json!({ "id": "an-comedy", "title": "Comedy & Slice of Life", "items": comedy })); }

    // Mystery & Supernatural Thriller
    let mystery: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[9648], crate::re!(r"(?i)mystery|detective|supernatural|time travel|conspiracy|murder|thriller|psychic"))).collect(), "rating")
        .into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if mystery.len() >= 6 { rows.push(json!({ "id": "an-mystery", "title": "Mystery & Supernatural", "items": mystery })); }

    // Anime Films fallback if TMDB disabled
    let films: Vec<Value> = sort_meta(&app, mv_refs.clone(), "rating").into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !films.is_empty() && !rows.iter().any(|r| r["id"] == "an-films") {
        rows.push(json!({ "id": "an-films", "title": "Studio Ghibli & Anime Films", "items": films }));
    }

    // 4K & Remastered Anime
    let uhd_items: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().chain(mv_refs.iter().copied()).filter(|it| {
        (!it.cam && (it.uhd || it.dv || it.hdr)) || it.variants.iter().any(|v| !v.cam && (v.uhd || v.dv || v.hdr))
    }).collect(), "popular").into_iter().take(36).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if uhd_items.len() >= 3 {
        rows.push(json!({ "id": "an-4k", "title": "4K & HDR Anime", "sub": "Ultra-sharp OLED showcase", "items": uhd_items }));
    }

    // Completed Binge-Ready Series
    let completed: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| {
        app.meta_of(it).map_or(false, |m| m.st.eq_ignore_ascii_case("Ended") && m.va >= 7.5)
    }).collect(), "rating").into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !completed.is_empty() { rows.push(json!({ "id": "an-done", "title": "Completed Series (Binge-Ready)", "items": completed })); }

    // Verified English Dubbed row
    let verified: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| tv_dubs.get(&it.id).map_or(false, |d| d.0 == "yes")).collect(), "popular")
        .into_iter().chain(sort_meta(&app, mv_refs.iter().copied().filter(|it| movie_dub_status(&app, it) == "yes").collect(), "popular"))
        .take(36).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if verified.len() >= 3 {
        rows.push(json!({ "id": "an-verified", "title": "Verified English Dubs", "sub": "Confirmed English audio tracks", "items": verified }));
    }

    // Recently Updated Anime
    let recent: Vec<Value> = sort_meta(&app, tv_refs.clone(), "added").into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !recent.is_empty() { rows.push(json!({ "id": "an-new", "title": "Recently Updated Anime", "items": recent })); }

    // All-Time Greats (8.0+)
    let all_items: Vec<&Item> = tv_refs.iter().copied().chain(mv_refs.iter().copied()).filter(|it| {
        app.meta_of(it).map_or(it.rating >= 8.0, |m| m.va >= 8.0 && m.vc >= 100)
    }).collect();
    let greats: Vec<Value> = sort_meta(&app, all_items, "rating").into_iter().take(40).map(|x| anime_card_with(&app, x, None, &tv_dubs)).collect();
    if !greats.is_empty() { rows.push(json!({ "id": "an-top", "title": "All-Time Anime Greats (8.0+)", "items": greats })); }

    // Precompute filter-bar facet counts (canonicalized movie + TV genres, deduped per item)
    let mut g_counts: HashMap<i64, usize> = HashMap::new();
    let mut p_counts: HashMap<&'static str, usize> = HashMap::new();
    let mut uhd_total = 0usize;
    for it in tv.iter().chain(mv.iter()) {
        if (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd) { uhd_total += 1; }
        if let Some(m) = app.meta_of(it) {
            let mut seen_g = HashSet::new();
            for &g in &m.g {
                let canon = canonical_anime_genre(g);
                if seen_g.insert(canon) { *g_counts.entry(canon).or_default() += 1; }
            }
        }
        for p in cat.item_platforms(it) { *p_counts.entry(p).or_default() += 1; }
    }
    let mut plat_vec: Vec<(&str, usize)> = p_counts.into_iter().collect();
    plat_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    plat_vec.truncate(10);
    let g_map: serde_json::Map<String, Value> = g_counts.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect();

    json!({
        "hero": hero_items,
        "rows": rows,
        "counts": { "series": tv.len(), "movies": mv.len(), "total": tv.len() + mv.len(), "uhd": uhd_total },
        "genres": Value::Object(g_map),
        "platforms": plat_vec.into_iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>(),
        "dubStats": dub_stats(&app),
        "skipStats": app.skipdb.stats()
    })
}

pub fn dub_stats(app: &App) -> Value {
    let (yes, no, total) = app.dubs.counts();
    let watching = app.state.read(|st| st["dubWatch"].as_object().map(|m| m.len()).unwrap_or(0));
    json!({ "verifiedYes": yes, "verifiedNo": no, "total": total, "watching": watching })
}

/// GET /api/anime/dub-watch: episodes currently watched for an English dub release.
pub fn dub_watch_list(app: &App) -> Value {
    let mut items: Vec<Value> = app.state.read(|st| {
        st["dubWatch"].as_object().map(|m| m.values().cloned().collect()).unwrap_or_default()
    });
    items.sort_by_key(|x| -x["t"].as_i64().unwrap_or(0));
    json!({ "items": items })
}

/// POST /api/anime/dub-watch { key, kind, id, sid, season, epNum, ext, title, sub, on }
pub fn dub_watch_set(app: &App, b: &Value) -> Value {
    let kind = if s(&b["kind"]) == "movie" { "movie" } else { "series" };
    let id = b["id"].as_i64().unwrap_or(0);
    let key = {
        let k = s(&b["key"]);
        if !k.is_empty() { k } else if id > 0 { format!("{}:{id}", if kind == "movie" { "m" } else { "e" }) } else { String::new() }
    };
    if key.is_empty() { return json!({ "ok": false, "error": "missing key" }); }
    let ext = { let x = s(&b["ext"]); if x.is_empty() { "mkv".to_string() } else { x } };
    let on = b["on"].as_bool().unwrap_or(true);
    app.state.with(|st| {
        if !st["dubWatch"].is_object() { st["dubWatch"] = json!({}); }
        if on {
            st["dubWatch"][&key] = json!({
                "key": key, "kind": kind, "id": id, "sid": b["sid"].as_i64(),
                "season": b["season"].as_i64(), "epNum": b["epNum"].as_i64(),
                "ext": ext,
                "title": s(&b["title"]), "sub": s(&b["sub"]),
                "t": now_ms(), "lastCheck": 0
            });
        } else if let Some(m) = st["dubWatch"].as_object_mut() {
            m.remove(&key);
        }
    });
    app.state.save();
    json!({ "ok": true, "watching": on, "items": dub_watch_list(app)["items"] })
}

/// Re-probes episodes/films in `state.dubWatch` once every 24h while completely idle.
/// Fires an on-screen + Windows alert the moment an English dub track appears!
pub async fn check_dub_watches(app: &Arc<App>) {
    use std::sync::atomic::Ordering::SeqCst;
    let idle = |app: &Arc<App>| !app.stream.is_active() && !crate::mpv::running(app) && !crate::livebuf::is_active() && now_ms() - app.stream.last_stop() > 120_000;
    if !idle(app) { return; }
    let now = now_ms();
    let due: Vec<Value> = app.state.read(|st| {
        st["dubWatch"].as_object().map(|m| {
            m.values().filter(|v| now - v["lastCheck"].as_i64().unwrap_or(0) > 20 * 3_600_000).cloned().collect()
        }).unwrap_or_default()
    });
    if due.is_empty() { return; }
    let ua = app.cfg.str("userAgent");
    let link = format!("http://localhost:{}/#/anime", app_port(app));
    for w in due.into_iter().take(4) {
        if !idle(app) { return; }
        match app.account(true).await { Ok(a) if a.active == 0 => {} _ => return }
        let kind = if s(&w["kind"]) == "movie" { "movie" } else { "series" };
        let id = w["id"].as_i64().unwrap_or(0);
        let w_sid = w["sid"].as_i64().filter(|&x| x > 0);
        let key = s(&w["key"]);
        let ext = { let x = s(&w["ext"]); if x.is_empty() { "mkv".to_string() } else { x } };
        if id <= 0 { continue; }
        if app.skips.busy.compare_exchange(false, true, SeqCst, SeqCst).is_err() { return; }
        let url = app.xc.stream_url(kind, &id.to_string(), &ext, false);
        let probed = {
            let _busy = BusyGuard(&app.skips.busy);
            app.skips.cancel.store(false, SeqCst);
            app.stream.evict_probe(&url);
            tokio::select! {
                r = app.stream.probe(&url, &ua) => r,
                _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(Duration::from_millis(100)).await; } } => None,
            }
        };
        app.stream.mark_stop();
        if let Some(p) = probed {
            let audio = p["audio"].as_array().cloned().unwrap_or_default();
            let has_en = record_probe_for(app, kind, id, w_sid, &audio);
            if has_en == Some(true) {
                let title = s(&w["title"]);
                let sub = s(&w["sub"]);
                let label = if sub.is_empty() { title.clone() } else { format!("{title} · {sub}") };
                let action = if kind == "movie" { json!({ "k": "m", "id": id }) } else { json!({ "k": "s", "id": w_sid.unwrap_or(0) }) };
                crate::notify_all(app, "dub", "English dub is out!", &label, &link, action);
                app.state.with(|st| { if let Some(m) = st["dubWatch"].as_object_mut() { m.remove(&key); } });
                app.state.save();
                app.memo.remove_prefix("anime");
                println!("[anime-dub-watch] DUB ARRIVED: {label}");
            } else {
                app.state.with(|st| { if let Some(ent) = st["dubWatch"].get_mut(&key) { ent["lastCheck"] = json!(now_ms()); } });
                app.state.save();
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// On-demand (idle-only!) season dub verifier for an anime series page.
/// Probes up to 3 unverified episodes (first, middle, last) of the requested season (or 1 per season)
/// ONLY when nothing is streaming, no other probe/intro job holds `app.skips.busy`, and account has 0 active connections.
pub async fn check_series(app: &Arc<App>, sid: i64, season_filter: Option<i64>) -> Result<Value, String> {
    use std::sync::atomic::Ordering::SeqCst;
    if app.stream.is_active() || crate::mpv::running(app) || crate::livebuf::is_active() {
        return Err("Can't verify dubs while something is playing (single-connection account).".into());
    }
    if app.skips.busy.compare_exchange(false, true, SeqCst, SeqCst).is_err() {
        return Err("A background dub or quality check is already running — try again in a moment.".into());
    }
    let _busy = BusyGuard(&app.skips.busy);

    let test_mode = std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty());
    if !test_mode {
        match app.account(true).await {
            Ok(a) if a.active == 0 => {}
            Ok(_) => return Err("Another device is using the account right now.".into()),
            Err(_) => {}
        }
    }
    let canon_sid = app.catalog().series_item(sid).map(|it| it.id).unwrap_or(sid);
    let info = app.xc_info("get_series_info", "series_id", canon_sid, 6 * 3_600_000).await;
    let mut by_season: std::collections::BTreeMap<i64, Vec<(i64, i64, String)>> = std::collections::BTreeMap::new();
    if let Some(m) = info["episodes"].as_object() {
        for (sk, list) in m {
            let sn: i64 = sk.parse().unwrap_or(0);
            if let Some(f) = season_filter { if sn != f { continue; } }
            let ent = by_season.entry(sn).or_default();
            for e in list.as_array().into_iter().flatten() {
                let eid = crate::util::i(&e["id"]);
                let en = crate::util::i(&e["episode_num"]);
                let ext = { let x = s(&e["container_extension"]); if x.is_empty() { "mkv".to_string() } else { x } };
                ent.push((en, eid, ext));
            }
            ent.sort_by_key(|x| x.0);
        }
    }
    let mut picks: Vec<(i64, i64, i64, String)> = vec![];
    for (sn, eps) in &by_season {
        if eps.is_empty() { continue; }
        let idxs: Vec<usize> = if season_filter.is_some() {
            let mut v = vec![0];
            if eps.len() > 2 { v.push(eps.len() / 2); }
            if eps.len() > 1 { v.push(eps.len() - 1); }
            v
        } else {
            let mut v = vec![0];
            if eps.len() > 1 { v.push(eps.len() - 1); }
            v
        };
        for ix in idxs {
            let (en, eid, ext) = &eps[ix];
            if app.dubs.get(&format!("e:{eid}")).is_none() && app.stream.audio_of(*eid).is_none() && !picks.iter().any(|x| x.2 == *eid) {
                picks.push((*sn, *en, *eid, ext.clone()));
            }
        }
    }
    let ua = app.cfg.str("userAgent");
    let mut checked = 0usize;
    let mut dubbed = 0usize;
    let mut sub_only = 0usize;
    let mut results: Vec<Value> = vec![];
    for (sn, en, eid, ext) in picks.into_iter().take(3) {
        if app.stream.is_active() || crate::mpv::running(app) || crate::livebuf::is_active() { break; }
        let url = app.xc.stream_url("series", &eid.to_string(), &ext, false);
        app.skips.cancel.store(false, SeqCst);
        let probed = tokio::select! {
            r = app.stream.probe(&url, &ua) => r,
            _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(Duration::from_millis(100)).await; } } => None,
        };
        app.stream.mark_stop();
        if let Some(p) = probed {
            let audio = p["audio"].as_array().cloned().unwrap_or_default();
            let res = record_probe_for(app, "series", eid, Some(canon_sid), &audio);
            checked += 1;
            if res == Some(true) { dubbed += 1; } else if res == Some(false) { sub_only += 1; }
            results.push(json!({ "season": sn, "ep": en, "id": eid, "hasEn": res }));
        }
        if app.skips.cancel.load(SeqCst) { break; }
        tokio::time::sleep(Duration::from_millis(1800)).await;
    }
    if checked > 0 { app.memo.remove_prefix("anime"); }
    Ok(json!({ "ok": true, "checked": checked, "dubbed": dubbed, "subOnly": sub_only, "results": results, "dubStats": dub_stats(app) }))
}

/// GET /api/anime/browse: filterable English-dubbed anime grid with canonicalized movie+TV genre facets.
pub fn browse(app: &App, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let num = |k: &str| get(k).parse::<f64>().unwrap_or(0.0);
    let kind = get("kind").trim().to_lowercase(); // all | tv | movie | anime
    let genres: Vec<i64> = get("g").split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let gmode = get("gmode").trim().to_lowercase();
    let terms: Vec<String> = get("q").trim().to_lowercase().split_whitespace().map(String::from).collect();
    let y1 = num("y1") as i32;
    let y2 = { let v = num("y2") as i32; if v == 0 { 9999 } else { v } };
    let rmin = num("rmin");
    let platform = get("platform").trim().to_string();
    let uhd = get("uhd") == "1";
    let status_f = get("status").trim().to_lowercase(); // "" | airing | ended | short
    let completed = get("completed") == "1" || status_f == "ended";
    let airing_only = status_f == "airing" || status_f == "returning";
    let short_only = status_f == "short";
    let unwatched = get("unwatched") == "1";
    let dub_mode = get("dub").trim().to_lowercase(); // "" | verified | all
    let verified_only = dub_mode == "verified";
    let include_sub_only = dub_mode == "all" || get("sub") == "1";
    let no_filler = get("noFiller") == "1" || get("nofiller") == "1";
    let sort = get("sort");
    let seen = if unwatched { Some(crate::discover::watched_set(app)) } else { None };
    let cat = app.catalog();
    let (tv, mv, tv_dubs) = anime_pool_with_cache(app, include_sub_only);
    let mut pool: Vec<Item> = vec![];
    if kind != "movie" { pool.extend(tv); }
    if kind != "tv" { pool.extend(mv); }

    let mut matched: Vec<(&Item, Option<Meta>, bool)> = vec![];
    for it in &pool {
        if uhd && !((!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd)) { continue; }
        if !platform.is_empty() && !cat.item_has_platform(it, &platform) { continue; }
        if let Some(seen) = &seen {
            if let Some(t) = &it.tmdb {
                let mt = if it.k == "s" { "tv" } else { "movie" };
                if seen.contains(&format!("{mt}:{t}")) { continue; }
            }
        }
        let m = app.meta_of(it);
        let yr = m.as_ref().map(|m| m.year()).filter(|&y| y > 0).or(it.year).unwrap_or(0);
        if y1 > 0 && yr < y1 { continue; }
        if y2 < 9999 && (yr == 0 || yr > y2) { continue; }
        if rmin > 0.0 {
            let ok = match &m {
                Some(m) => m.va >= rmin && m.vc >= 25,
                None => it.rating >= rmin,
            };
            if !ok { continue; }
        }
        if !genres.is_empty() {
            let g_match = |g: i64| {
                let cg = canonical_anime_genre(g);
                m.as_ref().map_or(false, |m| {
                    m.g.iter().any(|&mg| mg == g || canonical_anime_genre(mg) == cg)
                })
            };
            let ok = if gmode == "all" { genres.iter().all(|&g| g_match(g)) } else { genres.iter().any(|&g| g_match(g)) };
            if !ok { continue; }
        }
        if completed && !m.as_ref().map_or(false, |m| m.st.eq_ignore_ascii_case("Ended")) { continue; }
        if airing_only && !m.as_ref().map_or(false, |m| m.st.eq_ignore_ascii_case("Returning Series") || m.st.eq_ignore_ascii_case("In Production")) { continue; }
        if short_only && (it.k != "s" || !m.as_ref().map_or(false, |m| m.st.eq_ignore_ascii_case("Ended") && m.ns > 0 && m.ns <= 2)) { continue; }
        if no_filler && it.k == "s" {
            if let Some(sf) = crate::filler::builtin_lookup(&it.title) {
                if sf.filler_pct() > 20.0 { continue; }
            }
        }
        if !terms.is_empty() {
            let is_tv = it.k == "s";
            let mut hay = it.title.to_lowercase();
            if let Some(m) = &m {
                if !m.ot.is_empty() { hay.push(' '); hay.push_str(&m.ot.to_lowercase()); }
                if !m.ov.is_empty() { hay.push(' '); hay.push_str(&m.ov.to_lowercase()); }
                for &g in &m.g {
                    if let Some(gn) = crate::meta::genre_name(is_tv, g) {
                        hay.push(' '); hay.push_str(&gn.to_lowercase());
                    }
                }
            }
            if !it.plot.is_empty() { hay.push(' '); hay.push_str(&it.plot.to_lowercase()); }
            if !it.genre.is_empty() { hay.push(' '); hay.push_str(&it.genre.to_lowercase()); }
            for p in cat.item_platforms(it) {
                hay.push(' '); hay.push_str(&p.to_lowercase());
            }
            if !terms.iter().all(|w| hay.contains(w.as_str())) { continue; }
        }
        let is_ver = if it.k == "s" {
            tv_dubs.get(&it.id).map_or(false, |d| d.0 == "yes")
        } else {
            movie_dub_status(app, it) == "yes"
        };
        if verified_only && !is_ver { continue; }
        matched.push((it, m, is_ver));
    }

    // Canonicalize movie + TV genres in browse facets (matching `anime::page`)!
    let mut g_counts: HashMap<i64, usize> = HashMap::new();
    let mut p_counts: HashMap<&'static str, usize> = HashMap::new();
    let (mut uhd_c, mut ver_c, mut mv_c, mut tv_c, mut done_c) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for (it, m, is_ver) in &matched {
        if it.k == "s" { tv_c += 1; } else { mv_c += 1; }
        if (!it.cam && it.uhd) || it.variants.iter().any(|v| !v.cam && v.uhd) { uhd_c += 1; }
        if *is_ver { ver_c += 1; }
        if let Some(m) = m {
            let mut seen_g = HashSet::new();
            for &g in &m.g {
                let canon = canonical_anime_genre(g);
                if seen_g.insert(canon) { *g_counts.entry(canon).or_default() += 1; }
            }
            if it.k == "s" && m.st.eq_ignore_ascii_case("Ended") { done_c += 1; }
        }
        for p in cat.item_platforms(it) { *p_counts.entry(p).or_default() += 1; }
    }
    let mut plat_vec: Vec<(&str, usize)> = p_counts.into_iter().collect();
    plat_vec.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    plat_vec.truncate(12);
    let genres_json: serde_json::Map<String, Value> = g_counts.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let facets = json!({
        "genres": Value::Object(genres_json),
        "platforms": plat_vec.into_iter().map(|(n, c)| json!({ "name": n, "count": c })).collect::<Vec<_>>(),
        "uhd": uhd_c,
        "verified": ver_c,
        "movies": mv_c,
        "series": tv_c,
        "completed": done_c,
    });

    let pairs: Vec<(&Item, Option<Meta>)> = matched.into_iter().map(|(it, m, _)| (it, m)).collect();
    let sorted = sort_pairs(pairs, if sort.is_empty() { "popular" } else { &sort });
    let total = sorted.len();
    let page = get("page").parse::<usize>().unwrap_or(0);
    let per = get("per").parse::<usize>().unwrap_or(60).max(1);
    let items: Vec<Value> = sorted.into_iter().skip(page * per).take(per).map(|it| anime_card_with(app, it, None, &tv_dubs)).collect();
    json!({ "total": total, "page": page, "per": per, "facets": facets, "items": items })
}

/// Idle background worker: probes a handful of unverified anime episodes/films ONLY when
/// nothing is playing and the provider account has 0 active connections. Yields immediately.
pub async fn dub_worker(app: &Arc<App>) {
    use std::sync::atomic::Ordering::SeqCst;
    let idle = |app: &Arc<App>| !app.stream.is_active() && !crate::mpv::running(app) && !crate::livebuf::is_active() && now_ms() - app.stream.last_stop() > 120_000;
    if !idle(app) { return; }
    check_dub_watches(app).await;
    if !idle(app) { return; }
    let (tv, mv) = anime_pool(app);
    let top_tv: Vec<Item> = {
        let refs: Vec<&Item> = tv.iter().collect();
        sort_meta(app, refs, "popular").into_iter().take(80).cloned().collect()
    };
    // (kind, stream/ep id, opt_sid, ext)
    let mut todo: Vec<(&'static str, i64, Option<i64>, String)> = vec![];
    for s_it in top_tv {
        if todo.len() >= 12 { break; }
        if !idle(app) { return; }
        let info = app.xc_info("get_series_info", "series_id", s_it.id, 6 * 3_600_000).await;
        let mut by_s: std::collections::BTreeMap<i64, Vec<(i64, i64, String)>> = std::collections::BTreeMap::new();
        if let Some(m) = info["episodes"].as_object() {
            for (sk, list) in m {
                let sn: i64 = sk.parse().unwrap_or(0);
                let ent = by_s.entry(sn).or_default();
                for e in list.as_array().into_iter().flatten() {
                    let eid = crate::util::i(&e["id"]);
                    let ext = { let x = s(&e["container_extension"]); if x.is_empty() { "mkv".to_string() } else { x } };
                    ent.push((crate::util::i(&e["episode_num"]), eid, ext));
                }
                ent.sort_by_key(|x| x.0);
            }
        }
        if by_s.is_empty() { continue; }
        let mut picks: Vec<(i64, String)> = vec![];
        if let Some(first_s) = by_s.values().next() {
            if let Some(first) = first_s.first() { picks.push((first.1, first.2.clone())); }
        }
        for eps in by_s.values().skip(1) {
            if let Some(first) = eps.first() { picks.push((first.1, first.2.clone())); }
        }
        if let Some((_, last_s)) = by_s.iter().next_back() {
            if let Some(last) = last_s.last() { picks.push((last.1, last.2.clone())); }
        }
        for (eid, ext) in picks {
            if todo.len() >= 12 { break; }
            if app.dubs.get(&format!("e:{eid}")).is_none() && app.stream.audio_of(eid).is_none() && !todo.iter().any(|x| x.1 == eid) {
                todo.push(("series", eid, Some(s_it.id), ext));
            }
        }
    }
    if todo.len() < 12 {
        let refs: Vec<&Item> = mv.iter().collect();
        for m_it in sort_meta(app, refs, "popular").into_iter().take(40) {
            if todo.len() >= 12 { break; }
            if app.dubs.get(&format!("m:{}", m_it.id)).is_none() && app.stream.audio_of(m_it.id).is_none() {
                todo.push(("movie", m_it.id, None, m_it.ext.clone()));
            }
        }
    }
    let ua = app.cfg.str("userAgent");
    let mut changed = false;
    for (kind, id, sid_opt, ext) in todo.into_iter().take(10) {
        if !idle(app) { return; }
        match app.account(true).await { Ok(a) if a.active == 0 => {} _ => return }
        if app.skips.busy.compare_exchange(false, true, SeqCst, SeqCst).is_err() { return; }
        let url = app.xc.stream_url(kind, &id.to_string(), &ext, false);
        let probed = {
            let _busy = BusyGuard(&app.skips.busy);
            app.skips.cancel.store(false, SeqCst);
            tokio::select! {
                r = app.stream.probe(&url, &ua) => r,
                _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(Duration::from_millis(100)).await; } } => None,
            }
        };
        app.stream.mark_stop();
        if let Some(p) = probed {
            let audio = p["audio"].as_array().cloned().unwrap_or_default();
            let res = record_probe_for(app, kind, id, sid_opt, &audio);
            changed = true;
            println!("[anime-dub] probed {kind}:{id} -> en_dub={res:?}");
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    if changed { app.memo.remove_prefix("anime"); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_english_audio_rejects_non_en_dubs_and_preserves_dual_audio_untagged() {
        // Explicit non-English language tag MUST be rejected even when title says "Dub"
        assert_eq!(has_english_audio(&[json!({ "lang": "fr", "title": "French Dub" })]), Some(false));
        assert_eq!(has_english_audio(&[json!({ "lang": "fre", "title": "Dubbed 5.1" })]), Some(false));
        assert_eq!(has_english_audio(&[json!({ "lang": "es", "title": "Dub" })]), Some(false));
        assert_eq!(has_english_audio(&[json!({ "lang": "de", "title": "German Dub" })]), Some(false));
        assert_eq!(has_english_audio(&[json!({ "lang": "und", "title": "French Dub" })]), Some(false));

        // Dual-audio [jpn, und/""] must return None ("unknown"), NOT falsely condemn as Japanese-only!
        assert_eq!(has_english_audio(&[
            json!({ "lang": "jpn", "title": "Japanese" }),
            json!({ "lang": "und", "title": "" })
        ]), None);
        assert_eq!(has_english_audio(&[
            json!({ "lang": "jpn", "title": "" }),
            json!({ "lang": "", "title": "Stereo" })
        ]), None);

        // Single Japanese track or multi non-English tracks -> Some(false)
        assert_eq!(has_english_audio(&[json!({ "lang": "jpn", "title": "Japanese" })]), Some(false));
        assert_eq!(has_english_audio(&[
            json!({ "lang": "jpn", "title": "Japanese" }),
            json!({ "lang": "fre", "title": "French Dub" })
        ]), Some(false));

        // Real English tracks -> Some(true)
        assert_eq!(has_english_audio(&[
            json!({ "lang": "jpn", "title": "Japanese" }),
            json!({ "lang": "eng", "title": "English" })
        ]), Some(true));
        assert_eq!(has_english_audio(&[
            json!({ "lang": "jpn", "title": "Japanese" }),
            json!({ "lang": "und", "title": "English Dub" })
        ]), Some(true));
    }

    #[test]
    fn dub_cache_persists_sid_and_survives_restart_without_xinfo() {
        let tmp = std::env::temp_dir().join(format!("tv_dub_test_{}_{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&tmp).unwrap();
        {
            let dc = DubCache::load(&tmp);
            dc.set("e:5001".into(), DubEntry { has_en: true, langs: vec!["eng".into()], src: "probe".into(), t: 100, sid: Some(900) });
            dc.set("e:6001".into(), DubEntry { has_en: false, langs: vec!["jpn".into()], src: "probe".into(), t: 100, sid: Some(901) });
            dc.set("e:6002".into(), DubEntry { has_en: false, langs: vec!["jpn".into()], src: "probe".into(), t: 100, sid: Some(901) });
        }
        // Simulate cold app restart: reload from disk with empty xinfo ep_list!
        let reloaded = DubCache::load(&tmp);
        assert_eq!(reloaded.eval_series(&[900], &[]), ("yes", 1, 0));
        assert_eq!(reloaded.eval_series(&[901], &[]), ("no", 0, 2));
        assert_eq!(reloaded.counts(), (1, 2, 3));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pre_meta_anime_fallback_and_genre_canonicalization() {
        assert!(is_anime_pre_meta_fallback("NETFLIX ANIME", "", ""));
        assert!(is_anime_pre_meta_fallback("EN - ANIME", "", ""));
        assert!(!is_anime_pre_meta_fallback("NETFLIX ANIME", "US", ""));
        assert_eq!(canonical_anime_genre(28), 10759);
        assert_eq!(canonical_anime_genre(12), 10759);
        assert_eq!(canonical_anime_genre(878), 10765);
        assert_eq!(canonical_anime_genre(14), 10765);
        assert_eq!(canonical_anime_genre(10752), 10768);
    }
}
