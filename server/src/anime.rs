// English-dubbed Anime hub: pure Japanese animation discovery, persistent episode-level
// English dub verification (probe-cache + dub-cache + metadata + idle background worker),
// and curated anime rows / filterable browse.
use crate::app::App;
use crate::catalog::Item;
use crate::meta::Meta;
use crate::rec::{match_results, sp};
use crate::tmdb::{TTL_D, TTL_H};
use crate::util::{now_ms, s};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DubEntry {
    pub has_en: bool,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub src: String, // "probe" | "meta" | "name"
    #[serde(default)]
    pub t: i64,
}

pub struct DubCache {
    file: PathBuf,
    map: Mutex<HashMap<String, DubEntry>>, // "e:<id>" or "m:<id>"
}

impl DubCache {
    pub fn load(data: &Path) -> Self {
        let file = data.join("dub-cache.json");
        let map = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        DubCache { file, map: Mutex::new(map) }
    }
    pub fn get(&self, key: &str) -> Option<DubEntry> {
        self.map.lock().unwrap().get(key).cloned()
    }
    pub fn set(&self, key: String, entry: DubEntry) {
        let mut g = self.map.lock().unwrap();
        // Never overwrite a ground-truth probe result with a weaker heuristic
        if entry.src != "probe" {
            if let Some(prev) = g.get(&key) {
                if prev.src == "probe" { return; }
            }
        }
        g.insert(key, entry);
        let body = serde_json::to_vec(&*g).unwrap();
        drop(g);
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() { let _ = std::fs::rename(&tmp, &self.file); }
    }
    pub fn snapshot(&self) -> HashMap<String, DubEntry> {
        self.map.lock().unwrap().clone()
    }
}

/// Inspect ffprobe's audio track array: Some(true) = English dub present,
/// Some(false) = confirmed non-English only (e.g. Japanese only), None = untagged/uncertain.
pub fn has_english_audio(audio: &[Value]) -> Option<bool> {
    if audio.is_empty() { return None; }
    let mut saw_non_en = false;
    for a in audio {
        let lang = a["lang"].as_str().unwrap_or("").trim().to_lowercase();
        let title = a["title"].as_str().unwrap_or("").trim().to_lowercase();
        let is_comm = crate::re!(r"(?i)\b(commentary|audio description|descriptive|ad)\b").is_match(&title);
        if is_comm { continue; }
        if matches!(lang.as_str(), "en" | "eng" | "en-us" | "en-gb" | "en-au" | "en-ca") || lang.starts_with("en-") || lang.starts_with("en_") {
            return Some(true);
        }
        if crate::re!(r"(?i)\b(english|eng|dub|dubbed)\b").is_match(&title) && !crate::re!(r"(?i)\b(sub|subbed)\b").is_match(&title) {
            return Some(true);
        }
        if !lang.is_empty() && lang != "und" && lang != "unk" && lang != "zxx" {
            saw_non_en = true;
        } else if crate::re!(r"(?i)\b(japanese|jpn|jap|original)\b").is_match(&title) {
            saw_non_en = true;
        }
    }
    if saw_non_en { Some(false) } else { None }
}

/// Record audio tracks from any ffprobe call into `dub-cache.json`.
pub fn record_probe(app: &App, kind: &str, id: i64, audio: &[Value]) -> Option<bool> {
    let has = has_english_audio(audio)?;
    let prefix = if kind == "movie" { "m" } else { "e" };
    let langs: Vec<String> = audio.iter().filter_map(|a| {
        let l = a["lang"].as_str().unwrap_or("").trim();
        if l.is_empty() { None } else { Some(l.to_lowercase()) }
    }).collect();
    app.dubs.set(format!("{prefix}:{id}"), DubEntry { has_en: has, langs, src: "probe".into(), t: now_ms() });
    Some(has)
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
        if matches!(lang.as_str(), "en" | "eng") || lang.starts_with("en-") {
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

/// True for authentic Japanese anime (movies & series) in English/Anime categories,
/// excluding Western cartoons (e.g. Batman / Tom & Jerry) that providers sometimes toss into MANGA/ANIME.
pub fn is_anime_item(app: &App, it: &Item) -> bool {
    if !it.en { return false; }
    let cat_name = app.catalog().cat_name(if it.k == "s" { "series" } else { "movie" }, &it.cat).to_uppercase();
    if cat_name.contains("NETFLIX ANIME") || cat_name.contains("NETFLIX ANIMI") || cat_name.contains("CRUNCHYROLL") {
        return true;
    }
    if let Some(m) = app.meta_of(it) {
        let is_anim = m.g.contains(&16);
        let is_jp = m.l == "ja" || m.oc.iter().any(|c| c == "JP");
        if is_anim && is_jp { return true; }
        // If TMDB explicitly says it's Western non-Japanese animation, exclude even if in EN - MANGA/ANIME
        if !m.l.is_empty() && m.l != "ja" && !m.oc.iter().any(|c| c == "JP") { return false; }
    }
    // Fallback before TMDB metadata is indexed
    if it.origin == "JP" && (it.genre.to_lowercase().contains("anim") || cat_name.contains("ANIME") || cat_name.contains("MANGA")) {
        return true;
    }
    false
}

/// Series-level dub summary across cached episode probes: ("yes" | "no" | "unknown", dubbed_count, sub_only_count)
pub fn series_dub_summary(app: &App, sid: i64) -> (&'static str, usize, usize) {
    let key = format!("get_series_info:{sid}");
    let info = app.xinfo.lock().unwrap().get(&key).map(|(_, v)| v.clone());
    let Some(info) = info else { return ("unknown", 0, 0); };
    let dubs = app.dubs.snapshot();
    let mut yes = 0usize; let mut no = 0usize;
    if let Some(m) = info["episodes"].as_object() {
        for list in m.values() {
            for e in list.as_array().into_iter().flatten() {
                let eid = crate::util::i(&e["id"]);
                if let Some(d) = dubs.get(&format!("e:{eid}")) {
                    if d.has_en { yes += 1; } else { no += 1; }
                }
            }
        }
    }
    if yes > 0 { ("yes", yes, no) }
    else if no > 0 { ("no", 0, no) }
    else { ("unknown", 0, 0) }
}

fn anime_card(app: &App, it: &Item, tm: Option<&Value>) -> Value {
    let mut c = match tm { Some(t) => app.card(it, Some(t)), None => app.card_meta(it) };
    c["isAnime"] = json!(true);
    let (ds, dc) = if it.k == "s" {
        let (st, y, _) = series_dub_summary(app, it.id);
        (st, y)
    } else {
        (movie_dub_status(app, it), 0)
    };
    c["dubStatus"] = json!(ds);
    if dc > 0 { c["dubbedCount"] = json!(dc); }
    c
}

fn anime_pool(app: &App) -> (Vec<Item>, Vec<Item>) {
    let cat = app.catalog();
    let tv: Vec<Item> = cat.series.iter().filter(|it| is_anime_item(app, it)).filter(|it| {
        let (st, _, _) = series_dub_summary(app, it.id);
        st != "no"
    }).cloned().collect();
    let mv: Vec<Item> = cat.movies.iter().filter(|it| is_anime_item(app, it)).filter(|it| {
        movie_dub_status(app, it) != "no"
    }).cloned().collect();
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

/// GET /api/anime: Hero + curated English-dubbed anime rows.
pub async fn page(app: Arc<App>) -> Value {
    let (tv, mv) = anime_pool(&app);
    let mut rows: Vec<Value> = vec![];

    // Trending / Popular via TMDB Japanese animation ∩ our anime pool
    let mut hero_items: Vec<Value> = vec![];
    if app.tmdb.enabled() {
        let (tr_tv, tr_mv) = tokio::join!(
            app.tmdb.pages("/discover/tv", 3, [sp("with_genres", "16"), sp("with_original_language", "ja"), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "50")], 6 * TTL_H),
            app.tmdb.pages("/discover/movie", 2, [sp("with_genres", "16"), sp("with_original_language", "ja"), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "50")], 6 * TTL_H)
        );
        let tv_hits: Vec<Value> = match_results(&app, &tr_tv, Some("tv"), 40).into_iter().filter(|c| {
            let id = c["id"].as_i64().unwrap_or(0);
            series_dub_summary(&app, id).0 != "no"
        }).map(|mut c| {
            let id = c["id"].as_i64().unwrap_or(0);
            let (ds, dc) = { let (st, y, _) = series_dub_summary(&app, id); (st, y) };
            c["isAnime"] = json!(true); c["dubStatus"] = json!(ds); if dc > 0 { c["dubbedCount"] = json!(dc); }
            c
        }).collect();
        let mv_hits: Vec<Value> = match_results(&app, &tr_mv, Some("movie"), 30).into_iter().filter(|c| {
            let id = c["id"].as_i64().unwrap_or(0);
            app.catalog().movie(id).map_or(true, |it| movie_dub_status(&app, it) != "no")
        }).map(|mut c| {
            let id = c["id"].as_i64().unwrap_or(0);
            let ds = app.catalog().movie(id).map(|it| movie_dub_status(&app, it)).unwrap_or("unknown");
            c["isAnime"] = json!(true); c["dubStatus"] = json!(ds);
            c
        }).collect();
        hero_items = tv_hits.iter().chain(mv_hits.iter()).filter(|x| !s(&x["backdrop"]).is_empty() && !s(&x["overview"]).is_empty()).take(6).cloned().collect();
        if !tv_hits.is_empty() { rows.push(json!({ "id": "an-trend", "title": "Trending Anime Series", "sub": "English dubbed · non-dubbed episodes filtered", "items": tv_hits })); }
        if !mv_hits.is_empty() { rows.push(json!({ "id": "an-films", "title": "Popular Anime Films & Studio Ghibli", "items": mv_hits })); }
    }

    let tv_refs: Vec<&Item> = tv.iter().collect();
    let mv_refs: Vec<&Item> = mv.iter().collect();

    if hero_items.len() < 3 {
        hero_items = sort_meta(&app, tv_refs.clone(), "popular").into_iter()
            .filter(|x| !x.backdrop.is_empty() || app.meta_of(x).map_or(false, |m| !m.bd.is_empty()))
            .take(6).map(|x| anime_card(&app, x, None)).collect();
    }

    // Verified English Dubbed row (shows with confirmed EN audio tracks!)
    let verified: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| series_dub_summary(&app, it.id).0 == "yes").collect(), "popular")
        .into_iter().chain(sort_meta(&app, mv_refs.iter().copied().filter(|it| movie_dub_status(&app, it) == "yes").collect(), "popular"))
        .take(36).map(|x| anime_card(&app, x, None)).collect();
    if !verified.is_empty() {
        rows.push(json!({ "id": "an-verified", "title": "Verified English Dubs", "sub": "Confirmed English audio tracks", "items": verified }));
    }

    // Shonen & Action
    let shonen: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[10759], crate::re!(r"(?i)battle|shonen|fighter|tournament|ninja|pirate|hero|hunter|slayer|curse|alchemy"))).collect(), "popular")
        .into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !shonen.is_empty() { rows.push(json!({ "id": "an-shonen", "title": "Shonen & Action", "items": shonen })); }

    // Dark Fantasy & Seinen
    let dark: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[80, 9648], crate::re!(r"(?i)demon|vampire|blood|death|dark|curse|ghoul|psycho|assassin|revenge|monster|titan|chainsaw|berserk|parasyte|hell"))).collect(), "rating")
        .into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !dark.is_empty() { rows.push(json!({ "id": "an-dark", "title": "Dark Fantasy & Seinen", "items": dark })); }

    // Isekai & Sci-Fi / Fantasy
    let fantasy: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| has_kw_or_genre(&app, it, &[10765], crate::re!(r"(?i)reincarnat|another world|isekai|dungeon|magic|guild|level|rpg|mecha|gundam|cyber|future|space"))).collect(), "popular")
        .into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !fantasy.is_empty() { rows.push(json!({ "id": "an-fantasy", "title": "Isekai, Fantasy & Sci-Fi", "items": fantasy })); }

    // Anime Films (fallback / full list including Ghibli)
    let films: Vec<Value> = sort_meta(&app, mv_refs.clone(), "rating").into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !films.is_empty() && !rows.iter().any(|r| r["id"] == "an-films") {
        rows.push(json!({ "id": "an-films", "title": "Anime Films & Studio Ghibli", "items": films }));
    }

    // Completed Binge-Ready Series
    let completed: Vec<Value> = sort_meta(&app, tv_refs.iter().copied().filter(|it| {
        app.meta_of(it).map_or(false, |m| m.st.eq_ignore_ascii_case("Ended") && m.va >= 7.5)
    }).collect(), "rating").into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !completed.is_empty() { rows.push(json!({ "id": "an-done", "title": "Completed Series (Binge-Ready)", "items": completed })); }

    // Recently Updated Anime
    let recent: Vec<Value> = sort_meta(&app, tv_refs.clone(), "added").into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !recent.is_empty() { rows.push(json!({ "id": "an-new", "title": "Recently Updated Anime", "items": recent })); }

    // All-Time Greats (8.0+)
    let all_items: Vec<&Item> = tv_refs.into_iter().chain(mv_refs.into_iter()).filter(|it| {
        app.meta_of(it).map_or(it.rating >= 8.0, |m| m.va >= 8.0 && m.vc >= 100)
    }).collect();
    let greats: Vec<Value> = sort_meta(&app, all_items, "rating").into_iter().take(40).map(|x| anime_card(&app, x, None)).collect();
    if !greats.is_empty() { rows.push(json!({ "id": "an-top", "title": "All-Time Anime Greats (8.0+)", "items": greats })); }

    json!({
        "hero": hero_items,
        "rows": rows,
        "counts": { "series": tv.len(), "movies": mv.len() },
        "dubStats": dub_stats(&app)
    })
}

pub fn dub_stats(app: &App) -> Value {
    let snap = app.dubs.snapshot();
    let mut yes = 0usize;
    let mut no = 0usize;
    for d in snap.values() {
        if d.has_en { yes += 1; } else { no += 1; }
    }
    let watching = app.state.read(|st| st["dubWatch"].as_object().map(|m| m.len()).unwrap_or(0));
    json!({ "verifiedYes": yes, "verifiedNo": no, "total": snap.len(), "watching": watching })
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
    for w in due.into_iter().take(4) {
        if !idle(app) { return; }
        match app.account(true).await { Ok(a) if a.active == 0 => {} _ => return }
        let kind = if s(&w["kind"]) == "movie" { "movie" } else { "series" };
        let id = w["id"].as_i64().unwrap_or(0);
        let key = s(&w["key"]);
        let ext = { let x = s(&w["ext"]); if x.is_empty() { "mkv".to_string() } else { x } };
        if id <= 0 { continue; }
        let url = app.xc.stream_url(kind, &id.to_string(), &ext, false);
        app.skips.cancel.store(false, SeqCst);
        app.skips.busy.store(true, SeqCst);
        // Bypass stale probe cache by re-probing via ffprobe if needed? stream.probe caches forever, so evict cache entry first!
        app.stream.evict_probe(&url);
        let probed = tokio::select! {
            r = app.stream.probe(&url, &ua) => r,
            _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => None,
        };
        app.skips.busy.store(false, SeqCst);
        app.stream.mark_stop();
        if let Some(p) = probed {
            let audio = p["audio"].as_array().cloned().unwrap_or_default();
            let has_en = record_probe(app, kind, id, &audio);
            if has_en == Some(true) {
                let title = s(&w["title"]);
                let sub = s(&w["sub"]);
                let label = if sub.is_empty() { title.clone() } else { format!("{title} · {sub}") };
                let action = if kind == "movie" { json!({ "k": "m", "id": id }) } else { json!({ "k": "s", "id": w["sid"].as_i64().unwrap_or(0) }) };
                crate::notify_all(app, "dub", "English dub is out!", &label, "#/anime", action);
                app.state.with(|st| { if let Some(m) = st["dubWatch"].as_object_mut() { m.remove(&key); } });
                app.state.save();
                app.memo.remove_prefix("anime");
                println!("[anime-dub-watch] DUB ARRIVED: {label}");
            } else {
                app.state.with(|st| { if let Some(ent) = st["dubWatch"].get_mut(&key) { ent["lastCheck"] = json!(now_ms()); } });
                app.state.save();
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// On-demand (idle-only!) season dub verifier for an anime series page.
/// Probes up to 3 unverified episodes (first, middle, last) of the requested season (or 1 per season)
/// ONLY when nothing is streaming and the provider account has 0 active connections.
pub async fn check_series(app: &Arc<App>, sid: i64, season_filter: Option<i64>) -> Result<Value, String> {
    use std::sync::atomic::Ordering::SeqCst;
    if app.stream.is_active() || crate::mpv::running(app) || crate::livebuf::is_active() {
        return Err("Can't verify dubs while something is playing (single-connection account).".into());
    }
    let test_mode = std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty());
    if !test_mode {
        match app.account(true).await {
            Ok(a) if a.active == 0 => {}
            Ok(_) => return Err("Another device is using the account right now.".into()),
            Err(_) => {}
        }
    }
    let info = app.xc_info("get_series_info", "series_id", sid, 6 * 3_600_000).await;
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
        app.skips.busy.store(true, SeqCst);
        let probed = tokio::select! {
            r = app.stream.probe(&url, &ua) => r,
            _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => None,
        };
        app.skips.busy.store(false, SeqCst);
        app.stream.mark_stop();
        if let Some(p) = probed {
            let audio = p["audio"].as_array().cloned().unwrap_or_default();
            let res = record_probe(app, "series", eid, &audio);
            checked += 1;
            if res == Some(true) { dubbed += 1; } else if res == Some(false) { sub_only += 1; }
            results.push(json!({ "season": sn, "ep": en, "id": eid, "hasEn": res }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
    }
    if checked > 0 { app.memo.remove_prefix("anime"); }
    Ok(json!({ "ok": true, "checked": checked, "dubbed": dubbed, "subOnly": sub_only, "results": results, "dubStats": dub_stats(app) }))
}

/// GET /api/anime/browse: filterable English-dubbed anime grid with dynamic facets.
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
    let completed = get("completed") == "1" || get("status").eq_ignore_ascii_case("ended");
    let unwatched = get("unwatched") == "1";
    let verified_only = get("dub") == "verified";
    let no_filler = get("noFiller") == "1" || get("nofiller") == "1";
    let sort = get("sort");
    let seen = if unwatched { Some(crate::discover::watched_set(app)) } else { None };
    let cat = app.catalog();
    let (tv, mv) = anime_pool(app);
    let mut pool: Vec<Item> = vec![];
    if kind != "movie" { pool.extend(tv); }
    if kind != "tv" { pool.extend(mv); }

    let mut matched: Vec<(&Item, Option<Meta>, bool)> = vec![];
    for it in &pool {
        if uhd && !(it.uhd || it.variants.iter().any(|v| v.uhd)) { continue; }
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
                m.as_ref().map_or(false, |m| {
                    m.g.contains(&g)
                        || (g == 10759 && (m.g.contains(&28) || m.g.contains(&12)))
                        || ((g == 28 || g == 12) && m.g.contains(&10759))
                        || (g == 10765 && (m.g.contains(&14) || m.g.contains(&878)))
                        || ((g == 14 || g == 878) && m.g.contains(&10765))
                        || (g == 10768 && m.g.contains(&10752))
                        || (g == 10752 && m.g.contains(&10768))
                })
            };
            let ok = if gmode == "all" { genres.iter().all(|&g| g_match(g)) } else { genres.iter().any(|&g| g_match(g)) };
            if !ok { continue; }
        }
        if completed && !m.as_ref().map_or(false, |m| m.st.eq_ignore_ascii_case("Ended")) { continue; }
        if no_filler && it.k == "s" {
            if let Some(sf) = crate::filler::builtin_lookup(&it.title) {
                if sf.filler_pct() > 20.0 { continue; }
            }
        }
        if !terms.is_empty() {
            let is_tv = it.k == "s";
            let mut hay = it.title.to_lowercase();
            if let Some(m) = &m {
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
        let is_ver = if it.k == "s" { series_dub_summary(app, it.id).0 == "yes" } else { movie_dub_status(app, it) == "yes" };
        if verified_only && !is_ver { continue; }
        matched.push((it, m, is_ver));
    }

    let mut g_counts: HashMap<i64, usize> = HashMap::new();
    let mut p_counts: HashMap<&'static str, usize> = HashMap::new();
    let (mut uhd_c, mut ver_c, mut mv_c, mut tv_c, mut done_c) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for (it, m, is_ver) in &matched {
        if it.k == "s" { tv_c += 1; } else { mv_c += 1; }
        if it.uhd || it.variants.iter().any(|v| v.uhd) { uhd_c += 1; }
        if *is_ver { ver_c += 1; }
        if let Some(m) = m {
            for &g in &m.g { *g_counts.entry(g).or_default() += 1; }
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
    let items: Vec<Value> = sorted.into_iter().skip(page * per).take(per).map(|it| anime_card(app, it, None)).collect();
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
    // Prioritize popular anime series (1 representative episode per season + latest episode)
    let top_tv: Vec<Item> = {
        let refs: Vec<&Item> = tv.iter().collect();
        sort_meta(app, refs, "popular").into_iter().take(80).cloned().collect()
    };
    let mut todo: Vec<(&'static str, i64, String)> = vec![];
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
        for eps in by_s.values() {
            if let Some(first) = eps.first() { picks.push((first.1, first.2.clone())); }
        }
        if let Some((_, last_s)) = by_s.iter().next_back() {
            if let Some(last) = last_s.last() { picks.push((last.1, last.2.clone())); }
        }
        for (eid, ext) in picks {
            if todo.len() >= 12 { break; }
            if app.dubs.get(&format!("e:{eid}")).is_none() && app.stream.audio_of(eid).is_none() && !todo.iter().any(|x| x.1 == eid) {
                todo.push(("series", eid, ext));
            }
        }
    }
    if todo.len() < 12 {
        let refs: Vec<&Item> = mv.iter().collect();
        for m_it in sort_meta(app, refs, "popular").into_iter().take(40) {
            if todo.len() >= 12 { break; }
            if app.dubs.get(&format!("m:{}", m_it.id)).is_none() && app.stream.audio_of(m_it.id).is_none() {
                todo.push(("movie", m_it.id, m_it.ext.clone()));
            }
        }
    }
    let ua = app.cfg.str("userAgent");
    let mut changed = false;
    for (kind, id, ext) in todo.into_iter().take(10) {
        if !idle(app) { return; }
        match app.account(true).await { Ok(a) if a.active == 0 => {} _ => return }
        let url = app.xc.stream_url(kind, &id.to_string(), &ext, false);
        app.skips.cancel.store(false, SeqCst);
        app.skips.busy.store(true, SeqCst);
        let probed = tokio::select! {
            r = app.stream.probe(&url, &ua) => r,
            _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => None,
        };
        app.skips.busy.store(false, SeqCst);
        app.stream.mark_stop();
        if let Some(p) = probed {
            let audio = p["audio"].as_array().cloned().unwrap_or_default();
            let res = record_probe(app, kind, id, &audio);
            changed = true;
            println!("[anime-dub] probed {kind}:{id} -> en_dub={res:?}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    if changed { app.memo.remove_prefix("anime"); }
}
