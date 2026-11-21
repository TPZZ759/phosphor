// Release Radar & Digital-Drop Calendar + Spoiler-Free UFC Fight Card Lineup Fetcher.
// Uses ONLY local catalog/state + free public APIs (TMDB via cached `app.tmdb` and ESPN's public
// MMA UFC scoreboard API cached locally). Never touches the IPTV provider.
use crate::app::App;
use crate::catalog::Item;
use crate::fights::{collect_fights, parse_fight, CanonicalFight};
use crate::rec::{hidden_ratings, sp};
use crate::showcase::{best_tier, is_cam_only, recent_upgrades, QualityTier};
use crate::tmdb::{NO, TTL_H};
use crate::util::{norm, now_ms, s, tmdb_img};
use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Australia::Sydney;
use futures_util::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const ESPN_UFC_SCOREBOARD: &str =
    "https://site.api.espn.com/apis/site/v2/sports/mma/ufc/scoreboard";
const RADAR_UFC_TTL_MS: i64 = 6 * 3_600_000;
const FIGHT_CARD_TTL_MS: i64 = 14 * 86_400_000;
const CURRENT_YEAR_REFRESH_MS: i64 = 6 * 3_600_000;
const ESPN_TIMEOUT_SECS: u64 = 4;
const TMDB_CONCURRENCY: usize = 6;
const MAX_TV_SHOWS: usize = 24;
const MAX_WANTED_ENRICH: usize = 24;

static CACHE_IO_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn io_lock() -> &'static Mutex<()> {
    CACHE_IO_LOCK.get_or_init(|| Mutex::new(()))
}

// ---------------------------------------------------------------------------
// Date, timezone & atomic JSON cache helpers
// ---------------------------------------------------------------------------

fn sydney_today() -> NaiveDate {
    Utc::now().with_timezone(&Sydney).date_naive()
}

fn parse_iso_date(date_str: &str) -> Option<NaiveDate> {
    let trimmed = date_str.trim();
    let prefix = trimmed.get(0..10)?;
    NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok()
}

fn parse_espn_utc(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%MZ") {
        return Some(ndt.and_utc());
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(ndt.and_utc());
    }
    parse_iso_date(trimmed)
        .and_then(|d| d.and_hms_opt(12, 0, 0))
        .map(|ndt| ndt.and_utc())
}

/// Integer days between an ISO `YYYY-MM-DD` date and `today` (negative = in the past).
pub(crate) fn days_until(date_iso: &str, today: NaiveDate) -> Option<i64> {
    let d = parse_iso_date(date_iso)?;
    Some(d.signed_duration_since(today).num_days())
}

fn format_sydney_date_time(dt_utc: DateTime<Utc>) -> (String, String, String, NaiveDate) {
    let syd = dt_utc.with_timezone(&Sydney);
    let date_display = syd.format("%a, %-d %b %Y").to_string();
    let date_iso = syd.format("%Y-%m-%d").to_string();
    let time_display = format!("{} {}", syd.format("%-I:%M %p"), syd.format("%Z"));
    (date_display, date_iso, time_display, syd.date_naive())
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

// ---------------------------------------------------------------------------
// 1. Upcoming TV Episodes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct TrackedShow {
    tmdb: String,
    sid: Option<i64>,
    title: String,
    poster: String,
    backdrop: String,
    in_library: bool,
    activity_t: i64,
}

fn episode_status_text(days: i64, is_last_aired: bool) -> String {
    if days < 0 {
        format!("Aired {}d ago — watch for drop", -days)
    } else if days == 0 {
        if is_last_aired {
            "Aired today — watch for drop".to_string()
        } else {
            "Airs today".to_string()
        }
    } else if days == 1 {
        "Airs tomorrow".to_string()
    } else {
        format!("Airs in {days}d")
    }
}

fn collect_tracked_shows(app: &App) -> Vec<TrackedShow> {
    let cat = app.catalog();
    let mut by_tmdb: HashMap<String, TrackedShow> = HashMap::new();

    let mut upsert = |tmdb: String,
                      sid_hint: Option<i64>,
                      title_hint: String,
                      poster_hint: String,
                      backdrop_hint: String,
                      t: i64| {
        let tmdb = tmdb.trim().to_string();
        if tmdb.is_empty() || tmdb == "0" {
            return;
        }
        let lib_item = cat
            .by_tmdb("tv", &tmdb)
            .or_else(|| sid_hint.and_then(|sid| cat.series_item(sid)));
        let in_library = lib_item.is_some();
        let sid = lib_item.map(|it| it.id).or(sid_hint);
        let title = lib_item
            .map(|it| it.title.clone())
            .filter(|x| !x.is_empty())
            .unwrap_or(title_hint);
        let poster = lib_item
            .map(|it| it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/"))
            .filter(|x| !x.is_empty())
            .unwrap_or(poster_hint);
        let backdrop = lib_item
            .map(|it| it.backdrop.clone())
            .filter(|x| !x.is_empty())
            .unwrap_or(backdrop_hint);

        by_tmdb
            .entry(tmdb.clone())
            .and_modify(|existing| {
                if t > existing.activity_t {
                    existing.activity_t = t;
                }
                if existing.sid.is_none() && sid.is_some() {
                    existing.sid = sid;
                }
                existing.in_library = existing.in_library || in_library;
                if existing.title.is_empty() && !title.is_empty() {
                    existing.title = title.clone();
                }
                if existing.poster.is_empty() && !poster.is_empty() {
                    existing.poster = poster.clone();
                }
                if existing.backdrop.is_empty() && !backdrop.is_empty() {
                    existing.backdrop = backdrop.clone();
                }
            })
            .or_insert(TrackedShow {
                tmdb,
                sid,
                title,
                poster,
                backdrop,
                in_library,
                activity_t: t,
            });
    };

    app.state.read(|st| {
        // 1. Explicitly followed shows: state["follows"][sid]
        for (sid_str, f) in st["follows"].as_object().into_iter().flatten() {
            let sid = sid_str.parse::<i64>().ok();
            let tmdb = sid
                .and_then(|id| cat.series_item(id))
                .and_then(|it| it.tmdb.clone())
                .or_else(|| {
                    let t = s(&f["tmdb"]);
                    if t.is_empty() { None } else { Some(t) }
                });
            if let Some(tm) = tmdb {
                upsert(
                    tm,
                    sid,
                    s(&f["title"]),
                    s(&f["poster"]),
                    s(&f["backdrop"]),
                    f["t"].as_i64().unwrap_or(0),
                );
            }
        }

        // 2. In-progress series from state["progress"]
        for (key, p) in st["progress"].as_object().into_iter().flatten() {
            let item = &p["item"];
            let is_series = key.starts_with("e:")
                || key.starts_with("s:")
                || item["k"] == "e"
                || item["k"] == "s";
            if !is_series {
                continue;
            }
            let sid = item["sid"]
                .as_i64()
                .or_else(|| item["sid"].as_str().and_then(|x| x.parse().ok()))
                .or_else(|| key.strip_prefix("s:").and_then(|x| x.parse().ok()))
                .or_else(|| {
                    if item["k"] == "s" {
                        item["id"].as_i64()
                    } else {
                        None
                    }
                });
            let tmdb = sid
                .and_then(|id| cat.series_item(id))
                .and_then(|it| it.tmdb.clone())
                .or_else(|| {
                    let t = s(&item["tmdb"]);
                    if t.is_empty() { None } else { Some(t) }
                });
            if let Some(tm) = tmdb {
                upsert(
                    tm,
                    sid,
                    s(&item["title"]),
                    s(&item["poster"]),
                    s(&item["backdrop"]),
                    p["t"].as_i64().unwrap_or(0),
                );
            }
        }

        // 3. My List series: state["list"]["s:<id>"]
        for (key, entry) in st["list"].as_object().into_iter().flatten() {
            let item = &entry["item"];
            let sid = key
                .strip_prefix("s:")
                .and_then(|x| x.parse::<i64>().ok())
                .or_else(|| {
                    if item["k"] == "s" {
                        item["id"].as_i64()
                    } else {
                        None
                    }
                });
            if sid.is_none() && item["k"] != "s" {
                continue;
            }
            let tmdb = sid
                .and_then(|id| cat.series_item(id))
                .and_then(|it| it.tmdb.clone())
                .or_else(|| {
                    let t = s(&item["tmdb"]);
                    if t.is_empty() { None } else { Some(t) }
                });
            if let Some(tm) = tmdb {
                upsert(
                    tm,
                    sid,
                    s(&item["title"]),
                    s(&item["poster"]),
                    s(&item["backdrop"]),
                    entry["t"].as_i64().unwrap_or(0),
                );
            }
        }

        // 4. Wanted TV shows: state["wanted"]["tv:<tmdb>"]
        for (key, entry) in st["wanted"].as_object().into_iter().flatten() {
            if let Some(tm) = key.strip_prefix("tv:") {
                upsert(
                    tm.to_string(),
                    None,
                    s(&entry["title"]),
                    s(&entry["poster"]),
                    String::new(),
                    entry["t"].as_i64().unwrap_or(0),
                );
            }
        }
    });

    let mut list: Vec<TrackedShow> = by_tmdb.into_values().collect();
    list.sort_by(|a, b| {
        b.activity_t
            .cmp(&a.activity_t)
            .then(a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    list.truncate(MAX_TV_SHOWS);
    list
}

fn extract_episode_row(
    show: &TrackedShow,
    detail: &Value,
    ep: &Value,
    today: NaiveDate,
    is_last_aired: bool,
) -> Option<((i64, i64), Value)> {
    if !ep.is_object() {
        return None;
    }
    let air_date = s(&ep["air_date"]);
    let days = days_until(&air_date, today)?;
    if is_last_aired {
        if !(-7..=0).contains(&days) {
            return None;
        }
    } else if !(-7..=365).contains(&days) {
        return None;
    }

    let season = ep["season_number"].as_i64().unwrap_or(0);
    let episode = ep["episode_number"].as_i64().unwrap_or(0);
    if season <= 0 && episode <= 0 {
        return None;
    }

    let show_title = {
        let n = s(&detail["name"]);
        if n.is_empty() { show.title.clone() } else { n }
    };
    let poster = {
        let p = s(&detail["poster_path"]);
        if !p.is_empty() { tmdb_img(&p, "w342") } else { show.poster.clone() }
    };
    let backdrop = {
        let b = s(&detail["backdrop_path"]);
        if !b.is_empty() { tmdb_img(&b, "w1280") } else { show.backdrop.clone() }
    };
    let ep_name = s(&ep["name"]);
    let overview = {
        let o = s(&ep["overview"]);
        if o.is_empty() { s(&detail["overview"]) } else { o }
    };
    let status = episode_status_text(days, is_last_aired);

    let mut row = json!({
        "k": "s",
        "tmdb": show.tmdb,
        "sid": show.sid,
        "showTitle": show_title,
        "poster": poster,
        "backdrop": backdrop,
        "season": season,
        "episode": episode,
        "epName": ep_name,
        "airDate": air_date,
        "daysUntil": days,
        "status": status,
        "overview": overview,
        "inLibrary": show.in_library
    });
    if let Some(sid) = show.sid {
        row["id"] = json!(sid);
    }
    let still = s(&ep["still_path"]);
    if !still.is_empty() {
        row["still"] = json!(tmdb_img(&still, "w500"));
    }
    Some(((season, episode), row))
}

/// Upcoming & recently aired (last 7 days) TV episodes for followed / in-progress / My List shows.
pub async fn upcoming_episodes(app: Arc<App>) -> Vec<Value> {
    if !app.tmdb.enabled() {
        return vec![];
    }
    let shows = collect_tracked_shows(&app);
    if shows.is_empty() {
        return vec![];
    }
    let today = sydney_today();

    let fetched: Vec<Vec<Value>> = stream::iter(shows.into_iter().map(|show| {
        let app = app.clone();
        async move {
            let Some(detail) = app
                .tmdb
                .get(format!("/tv/{}", show.tmdb), NO, 12 * TTL_H)
                .await
            else {
                return vec![];
            };
            let mut rows: Vec<Value> = Vec::new();
            let mut seen_se: HashSet<(i64, i64)> = HashSet::new();

            if let Some((se, row)) =
                extract_episode_row(&show, &detail, &detail["next_episode_to_air"], today, false)
            {
                if seen_se.insert(se) {
                    rows.push(row);
                }
            }
            if let Some((se, row)) =
                extract_episode_row(&show, &detail, &detail["last_episode_to_air"], today, true)
            {
                if seen_se.insert(se) {
                    rows.push(row);
                }
            }
            rows
        }
    }))
    .buffer_unordered(TMDB_CONCURRENCY)
    .collect()
    .await;

    let mut out: Vec<Value> = fetched.into_iter().flatten().collect();
    out.sort_by(|a, b| {
        s(&a["airDate"])
            .cmp(&s(&b["airDate"]))
            .then(s(&a["showTitle"]).to_lowercase().cmp(&s(&b["showTitle"]).to_lowercase()))
            .then(a["season"].as_i64().unwrap_or(0).cmp(&b["season"].as_i64().unwrap_or(0)))
            .then(a["episode"].as_i64().unwrap_or(0).cmp(&b["episode"].as_i64().unwrap_or(0)))
    });
    out
}

// ---------------------------------------------------------------------------
// 2. Upcoming Movies & Digital-Drop Radar
// ---------------------------------------------------------------------------

pub(crate) fn classify_tier_state(cam_only: bool, tier: QualityTier) -> &'static str {
    if cam_only {
        "cam"
    } else if tier >= QualityTier::Uhd4k {
        "4k"
    } else {
        "hd"
    }
}

pub(crate) fn classify_movie_library_state(app: &App, it: Option<&Item>) -> &'static str {
    match it {
        None => "missing",
        Some(item) => {
            let cam_only = is_cam_only(app, item);
            let tier = best_tier(app, item);
            classify_tier_state(cam_only, tier)
        }
    }
}

pub(crate) fn movie_radar_status(library_state: &str, days: Option<i64>) -> String {
    match library_state {
        "cam" => "CAM only — waiting for clean HD/4K".to_string(),
        "hd" => "In HD — waiting for 4K".to_string(),
        "4k" => "Available in 4K".to_string(),
        _ => match days {
            Some(d) if d < 0 => "In cinemas · digital soon".to_string(),
            Some(0) => "Releases today".to_string(),
            Some(1) => "Releases tomorrow".to_string(),
            Some(d) => format!("In cinemas in {d}d"),
            None => "Release date TBA".to_string(),
        },
    }
}

#[derive(Clone, Debug)]
struct MovieCandidate {
    tmdb: String,
    title: String,
    release_date: String,
    days_until: Option<i64>,
    poster: String,
    backdrop: String,
    rating: f64,
    overview: String,
    popularity: f64,
    wanted: bool,
    library_state: &'static str,
    lib_id: Option<i64>,
}

fn build_movie_json(c: &MovieCandidate) -> Value {
    let year = c
        .release_date
        .get(0..4)
        .and_then(|y| y.parse::<i32>().ok());
    let status = movie_radar_status(c.library_state, c.days_until);
    json!({
        "k": "m",
        "id": c.lib_id,
        "tmdb": c.tmdb,
        "title": c.title,
        "year": year,
        "releaseDate": c.release_date,
        "daysUntil": c.days_until,
        "status": status,
        "libraryState": c.library_state,
        "inLibrary": c.lib_id.is_some(),
        "wanted": c.wanted,
        "poster": c.poster,
        "backdrop": c.backdrop,
        "rating": (c.rating * 10.0).round() / 10.0,
        "overview": c.overview
    })
}

/// Upcoming movies, Wanted watchlist, in-cinema digital drops, CAM-only watch, and HD-awaiting-4K.
pub async fn upcoming_movies(app: Arc<App>) -> Vec<Value> {
    let today = sydney_today();
    let hide = hidden_ratings(&app);

    // Snapshot Wanted movie entries from state.json
    let wanted_raw: Vec<(String, String, String, Option<i32>)> = app.state.read(|st| {
        st["wanted"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| {
                let tmdb = k.strip_prefix("movie:")?.trim().to_string();
                if tmdb.is_empty() {
                    return None;
                }
                let yr = v["year"]
                    .as_i64()
                    .map(|y| y as i32)
                    .or_else(|| s(&v["year"]).parse().ok());
                Some((tmdb, s(&v["title"]), s(&v["poster"]), yr))
            })
            .collect()
    });
    let wanted_set: HashSet<String> = wanted_raw.iter().map(|x| x.0.clone()).collect();

    // Fetch TMDB upcoming (AU region) + trending week concurrently (12h cache)
    let (upcoming_res, trending_res) = if app.tmdb.enabled() {
        tokio::join!(
            app.tmdb.pages("/movie/upcoming", 2, [sp("region", "AU")], 12 * TTL_H),
            app.tmdb.pages("/trending/movie/week", 2, NO, 12 * TTL_H),
        )
    } else {
        (vec![], vec![])
    };

    let cat = app.catalog();
    let mut by_tmdb: HashMap<String, MovieCandidate> = HashMap::new();

    // Helper closure to ingest a TMDB movie list item
    let mut ingest_tmdb = |m: &Value| {
        let tmdb = m["id"]
            .as_i64()
            .map(|id| id.to_string())
            .unwrap_or_else(|| s(&m["id"]));
        if tmdb.is_empty() || tmdb == "0" {
            return;
        }
        let is_wanted = wanted_set.contains(&tmdb);
        if !is_wanted && hide.contains(&format!("movie:{tmdb}")) {
            return;
        }

        let release_date = s(&m["release_date"]);
        let days = days_until(&release_date, today);
        let poster_path = s(&m["poster_path"]);
        let backdrop_path = s(&m["backdrop_path"]);
        let lib_it = cat.by_tmdb("movie", &tmdb);
        let lib_state = classify_movie_library_state(&app, lib_it);

        if !is_wanted {
            if poster_path.is_empty() {
                return;
            }
            let Some(d) = days else { return };
            if !(-90..=180).contains(&d) {
                return;
            }
            if lib_state == "4k" {
                return;
            }
        }

        let title = {
            let t = s(&m["title"]);
            if !t.is_empty() {
                t
            } else {
                lib_it.map(|it| it.title.clone()).unwrap_or_default()
            }
        };
        if title.is_empty() {
            return;
        }
        let poster = if !poster_path.is_empty() {
            tmdb_img(&poster_path, "w342")
        } else {
            lib_it
                .map(|it| it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/"))
                .unwrap_or_default()
        };
        let backdrop = if !backdrop_path.is_empty() {
            tmdb_img(&backdrop_path, "w1280")
        } else {
            lib_it.map(|it| it.backdrop.clone()).unwrap_or_default()
        };
        let rating = m["vote_average"]
            .as_f64()
            .filter(|&v| v > 0.0)
            .or_else(|| lib_it.map(|it| it.rating))
            .unwrap_or(0.0);
        let overview = s(&m["overview"]);
        let popularity = m["popularity"].as_f64().unwrap_or(0.0);

        by_tmdb
            .entry(tmdb.clone())
            .and_modify(|e| {
                e.wanted = e.wanted || is_wanted;
                if popularity > e.popularity {
                    e.popularity = popularity;
                }
                if e.release_date.is_empty() && !release_date.is_empty() {
                    e.release_date = release_date.clone();
                    e.days_until = days;
                }
                if e.backdrop.is_empty() && !backdrop.is_empty() {
                    e.backdrop = backdrop.clone();
                }
                if e.overview.is_empty() && !overview.is_empty() {
                    e.overview = overview.clone();
                }
                if e.rating <= 0.0 && rating > 0.0 {
                    e.rating = rating;
                }
            })
            .or_insert(MovieCandidate {
                tmdb,
                title,
                release_date,
                days_until: days,
                poster,
                backdrop,
                rating,
                overview,
                popularity,
                wanted: is_wanted,
                library_state: lib_state,
                lib_id: lib_it.map(|it| it.id),
            });
    };

    for m in &upcoming_res {
        ingest_tmdb(m);
    }
    for m in &trending_res {
        ingest_tmdb(m);
    }

    // Ensure every Wanted movie is included; enrich missing details via local meta or TMDB /movie/{id}.
    let mut need_detail: Vec<(String, String, String, Option<i32>)> = Vec::new();
    for (tmdb, title, poster, yr) in wanted_raw {
        if let Some(existing) = by_tmdb.get_mut(&tmdb) {
            existing.wanted = true;
            continue;
        }
        let lib_it = cat.by_tmdb("movie", &tmdb);
        let lib_state = classify_movie_library_state(&app, lib_it);
        let meta = app.meta_get("movie", &tmdb).filter(|m| !m.missing);
        if let Some(m) = meta.filter(|m| !m.d.is_empty()) {
            let days = days_until(&m.d, today);
            let p_url = if !m.pp.is_empty() {
                tmdb_img(&m.pp, "w342")
            } else if !poster.is_empty() {
                poster.clone()
            } else {
                lib_it
                    .map(|it| it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/"))
                    .unwrap_or_default()
            };
            let b_url = if !m.bd.is_empty() {
                tmdb_img(&m.bd, "w1280")
            } else {
                lib_it.map(|it| it.backdrop.clone()).unwrap_or_default()
            };
            by_tmdb.insert(
                tmdb.clone(),
                MovieCandidate {
                    tmdb,
                    title: if !title.is_empty() {
                        title
                    } else {
                        lib_it.map(|it| it.title.clone()).unwrap_or_default()
                    },
                    release_date: m.d,
                    days_until: days,
                    poster: p_url,
                    backdrop: b_url,
                    rating: m.va,
                    overview: m.ov,
                    popularity: m.p.max(50.0),
                    wanted: true,
                    library_state: lib_state,
                    lib_id: lib_it.map(|it| it.id),
                },
            );
        } else {
            need_detail.push((tmdb, title, poster, yr));
        }
    }

    if !need_detail.is_empty() {
        let fetched_wanted: Vec<MovieCandidate> = stream::iter(
            need_detail
                .into_iter()
                .take(MAX_WANTED_ENRICH)
                .map(|(tmdb, title, poster, yr)| {
                    let app = app.clone();
                    async move {
                        let d = if app.tmdb.enabled() {
                            app.tmdb
                                .get(format!("/movie/{tmdb}"), NO, 12 * TTL_H)
                                .await
                        } else {
                            None
                        };
                        let cat = app.catalog();
                        let lib_it = cat.by_tmdb("movie", &tmdb);
                        let lib_state = classify_movie_library_state(&app, lib_it);

                        let release_date = d
                            .as_ref()
                            .map(|v| s(&v["release_date"]))
                            .filter(|x| !x.is_empty())
                            .unwrap_or_else(|| {
                                yr.map(|y| format!("{y:04}-01-01")).unwrap_or_default()
                            });
                        let days = days_until(&release_date, today);
                        let eff_title = d
                            .as_ref()
                            .map(|v| s(&v["title"]))
                            .filter(|x| !x.is_empty())
                            .or_else(|| if !title.is_empty() { Some(title) } else { None })
                            .or_else(|| lib_it.map(|it| it.title.clone()))
                            .unwrap_or_else(|| format!("Movie #{tmdb}"));
                        let eff_poster = d
                            .as_ref()
                            .map(|v| s(&v["poster_path"]))
                            .filter(|x| !x.is_empty())
                            .map(|p| tmdb_img(&p, "w342"))
                            .or_else(|| if !poster.is_empty() { Some(poster) } else { None })
                            .or_else(|| {
                                lib_it.map(|it| {
                                    it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/")
                                })
                            })
                            .unwrap_or_default();
                        let eff_backdrop = d
                            .as_ref()
                            .map(|v| s(&v["backdrop_path"]))
                            .filter(|x| !x.is_empty())
                            .map(|b| tmdb_img(&b, "w1280"))
                            .or_else(|| lib_it.map(|it| it.backdrop.clone()))
                            .unwrap_or_default();
                        let rating = d
                            .as_ref()
                            .and_then(|v| v["vote_average"].as_f64())
                            .unwrap_or(0.0);
                        let overview = d.as_ref().map(|v| s(&v["overview"])).unwrap_or_default();
                        let popularity = d
                            .as_ref()
                            .and_then(|v| v["popularity"].as_f64())
                            .unwrap_or(50.0);

                        MovieCandidate {
                            tmdb,
                            title: eff_title,
                            release_date,
                            days_until: days,
                            poster: eff_poster,
                            backdrop: eff_backdrop,
                            rating,
                            overview,
                            popularity,
                            wanted: true,
                            library_state: lib_state,
                            lib_id: lib_it.map(|it| it.id),
                        }
                    }
                }),
        )
        .buffer_unordered(TMDB_CONCURRENCY)
        .collect()
        .await;

        for c in fetched_wanted {
            by_tmdb.entry(c.tmdb.clone()).or_insert(c);
        }
    }

    // Also include recent English non-sport CAM-only movies from the local catalog (zero network)
    // so the Radar's CAM Watch tracks all active CAM rips waiting for clean HD/4K drops.
    {
        let db = app.meta.read().unwrap();
        let mut cam_extras: Vec<MovieCandidate> = Vec::new();
        for it in &cat.movies {
            if !it.en || it.sport || !is_cam_only(&app, it) {
                continue;
            }
            let Some(tmdb) = it.tmdb.as_ref().filter(|t| !t.is_empty()) else {
                continue;
            };
            if by_tmdb.contains_key(tmdb) || hide.contains(&format!("movie:{tmdb}")) {
                continue;
            }
            let m = db.get(&format!("movie:{tmdb}")).filter(|m| !m.missing);
            let release_date = m.map(|m| m.d.clone()).unwrap_or_default();
            let days = days_until(&release_date, today);
            let recent_enough = match days {
                Some(d) => (-180..=30).contains(&d),
                None => it.year.map_or(false, |y| y >= today.year() - 1),
            };
            if !recent_enough {
                continue;
            }
            let poster = m
                .filter(|m| !m.pp.is_empty())
                .map(|m| tmdb_img(&m.pp, "w342"))
                .unwrap_or_else(|| it.poster.replace("/t/p/w600_and_h900_bestv2/", "/t/p/w342/"));
            let backdrop = m
                .filter(|m| !m.bd.is_empty())
                .map(|m| tmdb_img(&m.bd, "w1280"))
                .unwrap_or_else(|| it.backdrop.clone());
            cam_extras.push(MovieCandidate {
                tmdb: tmdb.clone(),
                title: it.title.clone(),
                release_date,
                days_until: days,
                poster,
                backdrop,
                rating: m.map(|m| m.va).filter(|&v| v > 0.0).unwrap_or(it.rating),
                overview: m
                    .map(|m| m.ov.clone())
                    .filter(|o| !o.is_empty())
                    .unwrap_or_else(|| it.plot.clone()),
                popularity: m.map(|m| m.p).unwrap_or(it.rating),
                wanted: wanted_set.contains(tmdb),
                library_state: "cam",
                lib_id: Some(it.id),
            });
        }
        drop(db);
        cam_extras.sort_by(|a, b| b.popularity.total_cmp(&a.popularity));
        for c in cam_extras.into_iter().take(16) {
            by_tmdb.entry(c.tmdb.clone()).or_insert(c);
        }
    }

    let mut list: Vec<MovieCandidate> = by_tmdb.into_values().collect();
    // Sort: Wanted first, then CAM-only drops, then upcoming chronologically (soonest first),
    // then recent theatrical releases (newest first).
    list.sort_by(|a, b| {
        let bucket = |c: &MovieCandidate| -> u8 {
            if c.wanted {
                0
            } else if c.library_state == "cam" {
                1
            } else if c.days_until.map_or(false, |d| d >= 0) {
                2
            } else {
                3
            }
        };
        let ba = bucket(a);
        let bb = bucket(b);
        if ba != bb {
            return ba.cmp(&bb);
        }
        match ba {
            0 => {
                let da = a.days_until.unwrap_or(i64::MAX);
                let db = b.days_until.unwrap_or(i64::MAX);
                da.cmp(&db).then(b.popularity.total_cmp(&a.popularity))
            }
            1 => b.popularity.total_cmp(&a.popularity),
            2 => a
                .days_until
                .unwrap_or(i64::MAX)
                .cmp(&b.days_until.unwrap_or(i64::MAX))
                .then(b.popularity.total_cmp(&a.popularity)),
            _ => b
                .days_until
                .unwrap_or(i64::MIN)
                .cmp(&a.days_until.unwrap_or(i64::MIN))
                .then(b.popularity.total_cmp(&a.popularity)),
        }
    });

    // Always keep every wanted & CAM movie, cap total non-wanted at 48.
    let mut non_wanted_kept = 0usize;
    let mut out: Vec<Value> = Vec::new();
    for c in &list {
        if !c.wanted && c.library_state != "cam" {
            if non_wanted_kept >= 48 {
                continue;
            }
            non_wanted_kept += 1;
        }
        out.push(build_movie_json(c));
    }
    out
}

// ---------------------------------------------------------------------------
// 3 & 4. Spoiler-Free ESPN UFC Parser, Matcher, Cache, Schedule & Fight Cards
// ---------------------------------------------------------------------------

/// Strictly spoiler-free bout representation.
/// Never contains winner, loser, finish method, finished round, clock, score, or post-fight records.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpoilerFreeBout {
    pub order: usize,
    pub fighter_a: String,
    pub fighter_b: String,
    pub weight_class: String,
    /// "Main Card" | "Prelims"
    pub card: String,
    /// "Main Event" | "Co-Main Event" | "Main Card" | "Prelims"
    pub section: String,
    pub is_main_event: bool,
    pub is_title: bool,
    /// Scheduled regulation rounds (3 or 5) — NEVER the round a fight ended in.
    pub rounds: u8,
}

/// Strictly spoiler-free UFC event representation (safe to cache on disk).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpoilerFreeUfcEvent {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub event_num: Option<u32>,
    pub date_iso: String,
    #[serde(default)]
    pub main_card_iso: String,
    pub venue: String,
    /// Coarse lifecycle state only ("pre" | "in" | "post") — NEVER status detail/result!
    pub state: String,
    pub main_event: String,
    pub bouts: Vec<SpoilerFreeBout>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RadarUfcCache {
    fetched_at: i64,
    range: String,
    events: Vec<SpoilerFreeUfcEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedCardEvent {
    fetched_at: i64,
    event: SpoilerFreeUfcEvent,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UfcCardsCache {
    #[serde(default)]
    events: HashMap<String, CachedCardEvent>,
    #[serde(default)]
    years: HashMap<String, i64>,
    #[serde(default)]
    windows: HashMap<String, i64>,
}

fn is_fight_night_like(title: &str) -> bool {
    crate::re!(r"(?i)\b(fight\s+night|on\s+(?:espn|abc|fox|fx|fuel)|vegas|apex)\b").is_match(title)
}

/// Extract numbered UFC PPV number (e.g. `UFC 327` -> `Some(327)`), ignoring Fight Night / ESPN numbers.
pub(crate) fn extract_ufc_ppv_num(title: &str) -> Option<u32> {
    if is_fight_night_like(title) {
        return None;
    }
    crate::re!(r"(?i)\bUFC\s+(\d{1,4})\b")
        .captures(title)
        .and_then(|c| c[1].parse::<u32>().ok())
}

fn parse_headline_fighters(name: &str) -> Vec<String> {
    let body = name
        .split_once(':')
        .map(|(_, rhs)| rhs)
        .unwrap_or(name)
        .trim();
    let sep = crate::re!(r"(?i)\s+(?:vs\.?|v\.)\s+");
    let Some(m) = sep.find(body) else {
        return vec![];
    };
    let clean = |part: &str| -> String {
        let p = part
            .trim()
            .trim_matches(|c: char| matches!(c, ':' | '-' | '–' | '—' | ','))
            .trim();
        let p = crate::re!(r"\s+([2-9]|II|III|IV)\s*$").replace(p, "");
        p.trim().to_string()
    };
    let a = clean(&body[..m.start()]);
    let b = clean(&body[m.end()..]);
    if a.is_empty() || b.is_empty() {
        vec![]
    } else {
        vec![a, b]
    }
}

/// Accent-insensitive canonical surname extraction for MMA fighter matching.
pub(crate) fn fighter_surname(name: &str) -> String {
    let n = norm(name);
    if n.is_empty() {
        return String::new();
    }
    let n = n.strip_prefix("the ").unwrap_or(&n);
    // Strip trailing generational/rematch suffixes (Jr., Sr., II, III, IV)
    let stripped = crate::re!(r"\s+(?:jr|sr|ii|iii|iv)$")
        .replace(n, "")
        .trim()
        .to_string();
    let base = if stripped.is_empty() { n } else { stripped.as_str() };

    if let Some(m) = crate::re!(
        r"(?:^|\s)(della maddalena|du plessis|de ridder|dos anjos|dos santos|saint denis|st pierre|st preux|bueno silva|machado garry|korean zombie|kara france|o malley|da silva|de oliveira|de andrade|del valle|de freitas|cortes acosta|mar fan|letho duclos)$"
    )
    .find(base)
    {
        return m.as_str().trim().to_string();
    }
    base.split_whitespace().last().unwrap_or(base).to_string()
}

/// Accent-, punctuation-, and full-name-vs-surname-insensitive fighter match.
pub(crate) fn fighter_matches(a: &str, b: &str) -> bool {
    let na = norm(a);
    let nb = norm(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na == nb {
        return true;
    }
    let sa = fighter_surname(a);
    let sb = fighter_surname(b);
    if !sa.is_empty() && sa == sb {
        return true;
    }
    let ca = sa.replace(' ', "");
    let cb = sb.replace(' ', "");
    if ca.len() >= 3 && ca == cb {
        return true;
    }
    // Handle e.g. "Ian Machado Garry" vs "Garry" (last token match)
    let la = na.split_whitespace().last().unwrap_or("");
    let lb = nb.split_whitespace().last().unwrap_or("");
    if la.len() >= 4 && (la == sb || lb == sa || (la == lb && (na.contains(' ') != nb.contains(' ')))) {
        return true;
    }
    false
}

fn is_tba_fighter(name: &str) -> bool {
    let n = norm(name);
    n.is_empty() || n == "tba" || n == "tbd" || n == "opponent tba" || n == "opponent tbd"
}

fn format_venue(ev: &Value, comps: &[Value]) -> String {
    let v = ev["venues"]
        .as_array()
        .and_then(|a| a.first())
        .or_else(|| comps.last().and_then(|c| c.get("venue")))
        .or_else(|| comps.first().and_then(|c| c.get("venue")));
    let Some(v) = v else {
        return String::new();
    };
    let full_name = s(&v["fullName"]);
    let addr = &v["address"];
    let city = s(&addr["city"]);
    let state = s(&addr["state"]);
    let country = s(&addr["country"]);

    let loc = if !city.is_empty() && !state.is_empty() {
        format!("{city}, {state}")
    } else if !city.is_empty() && !country.is_empty() {
        format!("{city}, {country}")
    } else if !city.is_empty() {
        city
    } else {
        country
    };

    match (full_name.is_empty(), loc.is_empty()) {
        (false, false) => format!("{full_name} · {loc}"),
        (false, true) => full_name,
        (true, false) => loc,
        (true, true) => String::new(),
    }
}

fn bout_mentions_title(comp: &Value) -> bool {
    let re = crate::re!(r"(?i)\b(title|championship|belt|interim\s+title)\b");
    if re.is_match(&s(&comp["type"]["text"])) || re.is_match(&s(&comp["type"]["abbreviation"])) {
        return true;
    }
    for note in comp["notes"].as_array().into_iter().flatten() {
        if re.is_match(&s(&note["headline"])) || re.is_match(&s(&note["text"])) {
            return true;
        }
    }
    false
}

/// Parse a single ESPN MMA event JSON into a **100% spoiler-free** `SpoilerFreeUfcEvent`.
/// Intentionally NEVER reads `winner`, `status.type.detail`, `status.period`, `status.clock`,
/// `score`, `linescores`, `details`, `records`, or `highlights`.
pub(crate) fn parse_espn_ufc_event(ev: &Value) -> Option<SpoilerFreeUfcEvent> {
    let id = s(&ev["id"]);
    let name = s(&ev["name"]);
    if name.is_empty() {
        return None;
    }
    let short_name = {
        let sn = s(&ev["shortName"]);
        if sn.is_empty() { name.clone() } else { sn }
    };
    let event_num = extract_ufc_ppv_num(&name).or_else(|| extract_ufc_ppv_num(&short_name));
    let date_iso = s(&ev["date"]);
    let state = match ev["status"]["type"]["state"].as_str().unwrap_or("pre") {
        "in" => "in".to_string(),
        "post" => "post".to_string(),
        _ => "pre".to_string(),
    };

    let raw_comps = ev["competitions"].as_array().cloned().unwrap_or_default();
    let venue = format_venue(ev, &raw_comps);

    struct RawBout {
        fighter_a: String,
        fighter_b: String,
        weight_class: String,
        start_ts: i64,
        start_iso: String,
        sched_rounds: u8,
        explicit_card: Option<&'static str>,
        explicit_title: bool,
    }

    let mut raw_bouts: Vec<RawBout> = Vec::new();
    for comp in &raw_comps {
        let Some(competitors) = comp["competitors"].as_array() else {
            continue;
        };
        if competitors.len() < 2 {
            continue;
        }
        let mut sorted: Vec<&Value> = competitors.iter().collect();
        sorted.sort_by_key(|c| c["order"].as_i64().unwrap_or(99));

        let athlete_name = |c: &Value| -> String {
            let a = s(&c["athlete"]["displayName"]);
            if !a.is_empty() {
                return a;
            }
            let f = s(&c["athlete"]["fullName"]);
            if !f.is_empty() {
                return f;
            }
            s(&c["team"]["displayName"])
        };

        let fighter_a = athlete_name(sorted[0]);
        let fighter_b = athlete_name(sorted[1]);
        if is_tba_fighter(&fighter_a) && is_tba_fighter(&fighter_b) {
            continue;
        }

        let weight_class = {
            let abbr = s(&comp["type"]["abbreviation"]);
            if !abbr.is_empty() {
                abbr
            } else {
                s(&comp["type"]["text"])
            }
        };

        let start_iso = {
            let sd = s(&comp["startDate"]);
            if !sd.is_empty() { sd } else { s(&comp["date"]) }
        };
        let start_ts = parse_espn_utc(&start_iso)
            .map(|dt| dt.timestamp())
            .unwrap_or(0);

        // Read ONLY scheduled regulation rounds (`format.regulation.periods`), never `status.period`.
        let periods = comp["format"]["regulation"]["periods"].as_u64().unwrap_or(0);
        let sched_rounds = match periods {
            5 => 5,
            3 => 3,
            _ => 0,
        };

        let seg_text = format!(
            "{} {}",
            s(&comp["cardSegment"]["name"]),
            s(&comp["cardSegment"]["description"])
        )
        .to_lowercase();
        let explicit_card = if seg_text.contains("main") {
            Some("Main Card")
        } else if seg_text.contains("prelim") {
            Some("Prelims")
        } else {
            None
        };

        raw_bouts.push(RawBout {
            fighter_a,
            fighter_b,
            weight_class,
            start_ts,
            start_iso,
            sched_rounds,
            explicit_card,
            explicit_title: bout_mentions_title(comp),
        });
    }

    // Determine whether `raw_bouts` is chronological (ESPN default: early prelims -> Main Event)
    // or already Main-Event-first, and normalize to Main-Event-first (`order = 1`).
    if raw_bouts.len() > 1 {
        let head = parse_headline_fighters(&name);
        let matches_head = |b: &RawBout| -> bool {
            head.len() == 2
                && ((fighter_matches(&b.fighter_a, &head[0])
                    && fighter_matches(&b.fighter_b, &head[1]))
                    || (fighter_matches(&b.fighter_a, &head[1])
                        && fighter_matches(&b.fighter_b, &head[0])))
        };
        let first = &raw_bouts[0];
        let last = &raw_bouts[raw_bouts.len() - 1];
        let should_reverse = if matches_head(first) && !matches_head(last) {
            false
        } else if matches_head(last) && !matches_head(first) {
            true
        } else if last.start_ts > first.start_ts {
            true
        } else if first.start_ts > last.start_ts {
            false
        } else if last.sched_rounds > first.sched_rounds {
            true
        } else if first.sched_rounds > last.sched_rounds {
            false
        } else {
            true
        };
        if should_reverse {
            raw_bouts.reverse();
        }
    }

    // Identify main card timestamp block if competitions have multiple distinct segment times.
    let valid_ts: Vec<i64> = raw_bouts
        .iter()
        .map(|b| b.start_ts)
        .filter(|&t| t > 0)
        .collect();
    let max_ts = valid_ts.iter().copied().max().unwrap_or(0);
    let min_ts = valid_ts.iter().copied().min().unwrap_or(0);
    let has_distinct_segments = max_ts > 0 && max_ts > min_ts;

    let main_card_iso = raw_bouts
        .first()
        .map(|b| b.start_iso.clone())
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| date_iso.clone());

    let is_ppv = event_num.is_some();
    let bouts: Vec<SpoilerFreeBout> = raw_bouts
        .into_iter()
        .enumerate()
        .map(|(idx, rb)| {
            let order = idx + 1;
            let is_main_event = order == 1;
            let card = if is_main_event {
                "Main Card"
            } else if let Some(explicit) = rb.explicit_card {
                explicit
            } else if has_distinct_segments && rb.start_ts > 0 {
                if rb.start_ts == max_ts {
                    "Main Card"
                } else {
                    "Prelims"
                }
            } else if order <= 5 {
                "Main Card"
            } else {
                "Prelims"
            };

            let section = if is_main_event {
                "Main Event"
            } else if order == 2 && card == "Main Card" {
                "Co-Main Event"
            } else {
                card
            };

            let rounds = if rb.sched_rounds == 5 || rb.sched_rounds == 3 {
                rb.sched_rounds
            } else if is_main_event {
                5
            } else {
                3
            };

            let is_title = rb.explicit_title || (is_ppv && rounds == 5 && order <= 2);

            SpoilerFreeBout {
                order,
                fighter_a: rb.fighter_a,
                fighter_b: rb.fighter_b,
                weight_class: rb.weight_class,
                card: card.to_string(),
                section: section.to_string(),
                is_main_event,
                is_title,
                rounds,
            }
        })
        .collect();

    let main_event = bouts
        .first()
        .map(|b| format!("{} vs. {}", b.fighter_a, b.fighter_b))
        .unwrap_or_else(|| {
            name.split_once(':')
                .map(|(_, rhs)| rhs.trim().to_string())
                .unwrap_or_else(|| name.clone())
        });

    Some(SpoilerFreeUfcEvent {
        id,
        name,
        short_name,
        event_num,
        date_iso,
        main_card_iso,
        venue,
        state,
        main_event,
        bouts,
    })
}

fn bout_to_json(b: &SpoilerFreeBout) -> Value {
    json!({
        "order": b.order,
        "fighterA": b.fighter_a,
        "fighterB": b.fighter_b,
        "fighters": [b.fighter_a, b.fighter_b],
        "weightClass": b.weight_class,
        "card": b.card,
        "section": b.section,
        "isMainEvent": b.is_main_event,
        "isTitle": b.is_title,
        "isTitleBout": b.is_title,
        "rounds": b.rounds
    })
}

#[derive(Clone, Debug)]
pub(crate) struct UfcMatchQuery {
    pub ppv_num: Option<u32>,
    pub fighters: Vec<String>,
    pub full_date: Option<NaiveDate>,
    pub year: Option<i32>,
}

fn build_ufc_query(
    title: &str,
    event_num: Option<u32>,
    date_str: Option<&str>,
    fighters: &[String],
) -> UfcMatchQuery {
    let ppv_num = extract_ufc_ppv_num(title).or_else(|| {
        if is_fight_night_like(title) {
            None
        } else {
            event_num
        }
    });

    let mut eff_fighters: Vec<String> = fighters
        .iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    if eff_fighters.len() < 2 {
        let parsed = parse_headline_fighters(title);
        if parsed.len() == 2 {
            eff_fighters = parsed;
        }
    }

    let full_date = date_str.and_then(parse_iso_date);
    let mut year = full_date.map(|d| d.year()).or_else(|| {
        date_str
            .and_then(|d| d.trim().get(0..4))
            .and_then(|y| y.parse::<i32>().ok())
    });
    if year.is_none() {
        year = crate::re!(r"\b(199\d|20[0-4]\d)\b")
            .captures(title)
            .and_then(|c| c[1].parse::<i32>().ok());
    }
    if year.is_none() {
        if let Some(n) = ppv_num {
            // Rough anchor: UFC 300 = April 2024 (~13 PPVs/year)
            let est = 2024 + ((n as i32 - 300) / 13);
            if (1993..=2040).contains(&est) {
                year = Some(est);
            }
        }
    }

    UfcMatchQuery {
        ppv_num,
        fighters: eff_fighters,
        full_date,
        year,
    }
}

fn event_headliners(ev: &SpoilerFreeUfcEvent) -> Vec<String> {
    if let Some(main) = ev.bouts.first() {
        return vec![main.fighter_a.clone(), main.fighter_b.clone()];
    }
    parse_headline_fighters(&ev.name)
}

/// Score how well an ESPN `SpoilerFreeUfcEvent` matches a UFC query (higher = stronger; 0 = no match).
pub(crate) fn score_ufc_event_match(ev: &SpoilerFreeUfcEvent, q: &UfcMatchQuery) -> i32 {
    // 1. Exact UFC PPV number match (e.g. UFC 327 == UFC 327)
    if let (Some(qn), Some(en)) = (q.ppv_num, ev.event_num) {
        if qn == en {
            return 1000;
        }
        // Both are explicitly different numbered PPVs -> never match
        return 0;
    }

    let ev_date = parse_espn_utc(&ev.date_iso).map(|dt| dt.date_naive());
    let ev_year = ev_date.map(|d| d.year());

    let head = event_headliners(ev);
    let mut name_head = parse_headline_fighters(&ev.name);
    if name_head.is_empty() {
        name_head = head.clone();
    }

    let pair_matches = |pair: &[String]| -> bool {
        pair.len() == 2
            && q.fighters.len() >= 2
            && ((fighter_matches(&pair[0], &q.fighters[0])
                && fighter_matches(&pair[1], &q.fighters[1]))
                || (fighter_matches(&pair[0], &q.fighters[1])
                    && fighter_matches(&pair[1], &q.fighters[0])))
    };

    let both_fighters = pair_matches(&head) || pair_matches(&name_head);
    if both_fighters {
        if let (Some(qd), Some(ed)) = (q.full_date, ev_date) {
            let diff = (qd - ed).num_days().abs();
            if diff <= 3 {
                return 900;
            }
            if diff <= 21 {
                return 800;
            }
            return 0;
        }
        if let (Some(qy), Some(ey)) = (q.year, ev_year) {
            if qy == ey {
                return 850;
            }
            if (qy - ey).abs() <= 1 {
                return 650;
            }
            return 0;
        }
        return 700;
    }

    // 3. Exact date (±2 days) + at least one headliner fighter (handles late-replacement opponents)
    if let (Some(qd), Some(ed)) = (q.full_date, ev_date) {
        if (qd - ed).num_days().abs() <= 2 && !q.fighters.is_empty() {
            let one_match = q.fighters.iter().any(|qf| {
                head.iter().any(|ef| fighter_matches(ef, qf))
                    || name_head.iter().any(|ef| fighter_matches(ef, qf))
            });
            if one_match {
                return 600;
            }
        }
    }

    0
}

pub(crate) fn match_ufc_event<'a>(
    events: &'a [SpoilerFreeUfcEvent],
    q: &UfcMatchQuery,
) -> Option<&'a SpoilerFreeUfcEvent> {
    events
        .iter()
        .filter_map(|ev| {
            let sc = score_ufc_event_match(ev, q);
            if sc > 0 { Some((sc, ev)) } else { None }
        })
        .max_by_key(|(sc, ev)| (*sc, ev.bouts.len()))
        .map(|(_, ev)| ev)
}

fn match_espn_to_replay<'a>(
    ev: &SpoilerFreeUfcEvent,
    replays: &'a [CanonicalFight],
) -> Option<&'a CanonicalFight> {
    let mut best: Option<(i32, &'a CanonicalFight)> = None;
    for cf in replays {
        if cf.entry.sport_key != "ufc" {
            continue;
        }
        let q = UfcMatchQuery {
            ppv_num: if cf.entry.event_kind == "ppv" {
                cf.entry.event_num
            } else {
                None
            },
            fighters: cf.entry.fighters.clone(),
            full_date: cf.entry.date_str.as_deref().and_then(parse_iso_date),
            year: cf.entry.year,
        };
        let mut sc = score_ufc_event_match(ev, &q);
        if sc <= 0 {
            continue;
        }
        // Prefer main card replay over prelims replay
        if !cf.entry.is_prelims {
            sc += 50;
        }
        if best.map_or(true, |(bsc, _)| sc > bsc) {
            best = Some((sc, cf));
        }
    }
    best.map(|(_, cf)| cf)
}

async fn fetch_espn_ufc_range(http: &reqwest::Client, range: &str) -> Option<Vec<SpoilerFreeUfcEvent>> {
    let url = format!("{ESPN_UFC_SCOREBOARD}?dates={range}&limit=100");
    let resp = http
        .get(&url)
        .timeout(Duration::from_secs(ESPN_TIMEOUT_SECS))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    let events: Vec<SpoilerFreeUfcEvent> = body["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(parse_espn_ufc_event)
        .collect();
    Some(events)
}

fn read_radar_ufc_cache(path: &Path) -> Option<RadarUfcCache> {
    let _g = io_lock().lock().unwrap();
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_radar_ufc_cache(path: &Path, cache: &RadarUfcCache) {
    let _g = io_lock().lock().unwrap();
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let _ = atomic_write_json(path, &bytes);
    }
}

fn read_ufc_cards_cache(path: &Path) -> UfcCardsCache {
    let _g = io_lock().lock().unwrap();
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_ufc_cards_cache(path: &Path, cache: &UfcCardsCache) {
    let _g = io_lock().lock().unwrap();
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let _ = atomic_write_json(path, &bytes);
    }
}

fn upcoming_ufc_event_json(
    ev: &SpoilerFreeUfcEvent,
    today: NaiveDate,
    now_utc: DateTime<Utc>,
    replay: Option<&CanonicalFight>,
) -> Value {
    let dt_utc = parse_espn_utc(&ev.date_iso).unwrap_or(now_utc);
    let (syd_date, syd_date_iso, syd_time, syd_naive) = format_sydney_date_time(dt_utc);
    let main_card_syd_time = parse_espn_utc(&ev.main_card_iso)
        .map(|mdt| format_sydney_date_time(mdt).2)
        .unwrap_or_else(|| syd_time.clone());
    let days = syd_naive.signed_duration_since(today).num_days();

    let status = match ev.state.as_str() {
        "in" => "live",
        "post" => "recent",
        _ => {
            if now_utc.timestamp() - dt_utc.timestamp() > 12 * 3600 {
                "recent"
            } else {
                "upcoming"
            }
        }
    };

    let bouts_json: Vec<Value> = ev.bouts.iter().map(bout_to_json).collect();

    json!({
        "id": ev.id,
        "name": ev.name,
        "shortName": ev.short_name,
        "eventNum": ev.event_num,
        "isPpv": ev.event_num.is_some(),
        "dateIso": ev.date_iso,
        "sydneyDate": syd_date,
        "sydneyDateIso": syd_date_iso,
        "sydneyTime": syd_time,
        "mainCardSydneyTime": main_card_syd_time,
        "daysUntil": days,
        "venue": ev.venue,
        "mainEvent": ev.main_event,
        "boutCount": bouts_json.len(),
        "status": status,
        "spoilerFree": true,
        "replayAvailable": replay.is_some(),
        "replayId": replay.map(|cf| cf.id),
        "bouts": bouts_json
    })
}

/// Upcoming & recent (last 3 days -> next 75 days) spoiler-free UFC schedule in Sydney time.
pub async fn upcoming_ufc(app: Arc<App>) -> Vec<Value> {
    let today = sydney_today();
    let start = today.checked_sub_days(Days::new(3)).unwrap_or(today);
    let end = today.checked_add_days(Days::new(75)).unwrap_or(today);
    let range = format!("{}-{}", start.format("%Y%m%d"), end.format("%Y%m%d"));
    let cache_path = app.data.join("radar-ufc-cache.json");
    let now = now_ms();

    let cached = read_radar_ufc_cache(&cache_path);
    let events = if let Some(ref c) = cached {
        if c.range == range && now - c.fetched_at < RADAR_UFC_TTL_MS {
            c.events.clone()
        } else {
            match fetch_espn_ufc_range(&app.http, &range).await {
                Some(fresh) => {
                    write_radar_ufc_cache(
                        &cache_path,
                        &RadarUfcCache {
                            fetched_at: now,
                            range: range.clone(),
                            events: fresh.clone(),
                        },
                    );
                    fresh
                }
                None => c.events.clone(),
            }
        }
    } else {
        match fetch_espn_ufc_range(&app.http, &range).await {
            Some(fresh) => {
                write_radar_ufc_cache(
                    &cache_path,
                    &RadarUfcCache {
                        fetched_at: now,
                        range: range.clone(),
                        events: fresh.clone(),
                    },
                );
                fresh
            }
            None => vec![],
        }
    };

    if events.is_empty() {
        return vec![];
    }

    let replays: Vec<CanonicalFight> = {
        let cat = app.catalog();
        let meta = app.meta.read().unwrap();
        collect_fights(&cat, Some(&meta))
            .into_iter()
            .filter(|cf| cf.entry.sport_key == "ufc")
            .collect()
    };

    let now_utc = Utc::now();
    let mut sorted = events;
    sorted.sort_by_key(|ev| {
        parse_espn_utc(&ev.date_iso)
            .map(|dt| dt.timestamp())
            .unwrap_or(i64::MAX)
    });

    sorted
        .iter()
        .map(|ev| {
            let replay = match_espn_to_replay(ev, &replays);
            upcoming_ufc_event_json(ev, today, now_utc, replay)
        })
        .collect()
}

fn fight_card_found_json(ev: &SpoilerFreeUfcEvent) -> Value {
    let date_short = ev
        .date_iso
        .get(0..10)
        .unwrap_or(&ev.date_iso)
        .to_string();
    let (syd_date, syd_time) = parse_espn_utc(&ev.date_iso)
        .map(|dt| {
            let (sd, _, st, _) = format_sydney_date_time(dt);
            (sd, st)
        })
        .unwrap_or_default();
    let bouts: Vec<Value> = ev.bouts.iter().map(bout_to_json).collect();
    json!({
        "ok": true,
        "found": true,
        "eventId": ev.id,
        "eventTitle": ev.name,
        "shortName": ev.short_name,
        "date": date_short,
        "dateIso": ev.date_iso,
        "sydneyDate": syd_date,
        "sydneyTime": syd_time,
        "venue": ev.venue,
        "spoilerFree": true,
        "bouts": bouts
    })
}

fn fight_card_not_found_json() -> Value {
    json!({
        "ok": true,
        "found": false,
        "spoilerFree": true,
        "bouts": []
    })
}

/// Fetch a 100% spoiler-free UFC fight card lineup by title, optional PPV number, date/year, and fighters.
pub async fn fight_card_for_meta(
    app: Arc<App>,
    title: &str,
    event_num: Option<u32>,
    date_str: Option<&str>,
    fighters: &[String],
) -> Value {
    let query = build_ufc_query(title, event_num, date_str, fighters);
    if query.ppv_num.is_none() && query.fighters.is_empty() {
        return fight_card_not_found_json();
    }

    let cards_path = app.data.join("ufc-cards-cache.json");
    let radar_path = app.data.join("radar-ufc-cache.json");
    let now = now_ms();
    let mut cache = read_ufc_cards_cache(&cards_path);

    // 1. Check fresh entries already in ufc-cards-cache.json (14-day TTL)
    let fresh_events: Vec<SpoilerFreeUfcEvent> = cache
        .events
        .values()
        .filter(|ce| now - ce.fetched_at < FIGHT_CARD_TTL_MS)
        .map(|ce| ce.event.clone())
        .collect();
    if let Some(ev) = match_ufc_event(&fresh_events, &query) {
        return fight_card_found_json(ev);
    }

    // 2. Also check radar-ufc-cache.json for zero-latency hits on recent/upcoming events
    if let Some(rc) = read_radar_ufc_cache(&radar_path) {
        if now - rc.fetched_at < FIGHT_CARD_TTL_MS {
            if let Some(ev) = match_ufc_event(&rc.events, &query) {
                return fight_card_found_json(ev);
            }
        }
    }

    let mut cache_dirty = false;
    let ingest = |cache: &mut UfcCardsCache, fetched: Vec<SpoilerFreeUfcEvent>, ts: i64| {
        for ev in fetched {
            if !ev.id.is_empty() {
                cache.events.insert(
                    ev.id.clone(),
                    CachedCardEvent {
                        fetched_at: ts,
                        event: ev,
                    },
                );
            }
        }
    };

    // 3. If exact YYYY-MM-DD is known, query a ±2 day window around that date.
    if let Some(d) = query.full_date {
        let lo = d.checked_sub_days(Days::new(2)).unwrap_or(d);
        let hi = d.checked_add_days(Days::new(2)).unwrap_or(d);
        let win = format!("{}-{}", lo.format("%Y%m%d"), hi.format("%Y%m%d"));
        let win_fresh = cache
            .windows
            .get(&win)
            .map_or(false, |&t| now - t < FIGHT_CARD_TTL_MS);
        if !win_fresh {
            if let Some(fetched) = fetch_espn_ufc_range(&app.http, &win).await {
                let matched = match_ufc_event(&fetched, &query).cloned();
                ingest(&mut cache, fetched, now);
                cache.windows.insert(win, now);
                cache_dirty = true;
                if let Some(ev) = matched {
                    write_ufc_cards_cache(&cards_path, &cache);
                    return fight_card_found_json(&ev);
                }
            }
        }
    }

    // 4. Otherwise (or if ±2d missed due to provider date skew), query the full year in ONE call
    //    and cache all UFC events for that year in ufc-cards-cache.json.
    if let Some(yr) = query.year.filter(|&y| (1993..=2040).contains(&y)) {
        let current_yr = sydney_today().year();
        let years_to_try: Vec<i32> = if query.full_date.is_none() && yr >= 2015 {
            vec![yr, yr - 1]
        } else {
            vec![yr]
        };

        for target_yr in years_to_try {
            let ykey = target_yr.to_string();
            let ttl = if target_yr >= current_yr {
                CURRENT_YEAR_REFRESH_MS
            } else {
                FIGHT_CARD_TTL_MS
            };
            let yr_fresh = cache.years.get(&ykey).map_or(false, |&t| now - t < ttl);
            if !yr_fresh {
                let range = format!("{target_yr}0101-{target_yr}1231");
                if let Some(fetched) = fetch_espn_ufc_range(&app.http, &range).await {
                    let matched = match_ufc_event(&fetched, &query).cloned();
                    ingest(&mut cache, fetched, now);
                    cache.years.insert(ykey, now);
                    cache_dirty = true;
                    if let Some(ev) = matched {
                        write_ufc_cards_cache(&cards_path, &cache);
                        return fight_card_found_json(&ev);
                    }
                }
            }
        }
    }

    if cache_dirty {
        write_ufc_cards_cache(&cards_path, &cache);
    }

    // Final fallback: check stale cached events if network was unavailable
    let all_cached: Vec<SpoilerFreeUfcEvent> = cache
        .events
        .values()
        .map(|ce| ce.event.clone())
        .collect();
    if let Some(ev) = match_ufc_event(&all_cached, &query) {
        return fight_card_found_json(ev);
    }

    fight_card_not_found_json()
}

/// Fetch a 100% spoiler-free UFC fight card lineup for a provider fight replay stream `id`.
pub async fn fight_card(app: Arc<App>, id: i64) -> Value {
    let lookup: Option<(String, Option<u32>, Option<String>, Vec<String>)> = {
        let cat = app.catalog();
        let meta = app.meta.read().unwrap();
        let fights_pool = collect_fights(&cat, Some(&meta));
        if let Some(cf) = fights_pool
            .into_iter()
            .find(|cf| cf.id == id || cf.all_ids.contains(&id))
        {
            if cf.entry.sport_key == "ufc" {
                let ppv = if cf.entry.event_kind == "ppv" {
                    cf.entry.event_num
                } else {
                    None
                };
                Some((
                    cf.entry.clean_title,
                    ppv,
                    cf.entry.date_str,
                    cf.entry.fighters,
                ))
            } else {
                None
            }
        } else if let Some(raw) = cat.movie_raw.get(&id) {
            let cat_name = cat.cat_name("movie", &raw.cat);
            parse_fight(&raw.title, &cat_name, raw.year, raw.added).and_then(|entry| {
                if entry.sport_key == "ufc" {
                    let ppv = if entry.event_kind == "ppv" {
                        entry.event_num
                    } else {
                        None
                    };
                    Some((entry.clean_title, ppv, entry.date_str, entry.fighters))
                } else {
                    None
                }
            })
        } else {
            None
        }
    };

    let Some((title, ppv_num, date_str, fighters)) = lookup else {
        return fight_card_not_found_json();
    };

    fight_card_for_meta(app, &title, ppv_num, date_str.as_deref(), &fighters).await
}

// ---------------------------------------------------------------------------
// 5. Combined Release Radar endpoint payload
// ---------------------------------------------------------------------------

/// Combined Release Radar & Digital-Drop Calendar payload:
/// upcoming episodes, upcoming/digital-drop movies, spoiler-free UFC schedule,
/// recent 4K/clean quality upgrades, and summary counts.
pub async fn radar(app: Arc<App>) -> Value {
    let (episodes, movies, ufc) = tokio::join!(
        upcoming_episodes(app.clone()),
        upcoming_movies(app.clone()),
        upcoming_ufc(app.clone()),
    );
    let upgrades = recent_upgrades(&app);

    let wanted_count = app.state.read(|st| {
        st["wanted"]
            .as_object()
            .map(|m| m.len())
            .unwrap_or(0)
    });
    let cam_watch = movies
        .iter()
        .filter(|m| m["libraryState"] == "cam")
        .count();

    json!({
        "episodes": episodes,
        "movies": movies,
        "ufc": ufc,
        "upgrades": upgrades,
        "camWatch": cam_watch,
        "counts": {
            "episodes": episodes.len(),
            "movies": movies.len(),
            "ufc": ufc.len(),
            "upgrades": upgrades.len(),
            "wanted": wanted_count,
            "camWatch": cam_watch
        }
    })
}

// ---------------------------------------------------------------------------
// 6. Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn realistic_espn_ufc_327_json() -> Value {
        json!({
            "id": "600058745",
            "name": "UFC 327: Procházka vs. Ulberg",
            "shortName": "UFC 327",
            "date": "2026-04-11T21:30Z",
            "status": {
                "type": {
                    "id": "3",
                    "name": "STATUS_FINAL",
                    "state": "post",
                    "completed": true,
                    "description": "Final",
                    "detail": "Final - KO/TKO R2 3:14",
                    "shortDetail": "KO R2"
                }
            },
            "venues": [{
                "id": "3438",
                "fullName": "Kaseya Center",
                "address": { "city": "Miami", "state": "FL", "country": "USA" }
            }],
            "competitions": [
                {
                    "id": "401860000",
                    "date": "2026-04-11T21:30Z",
                    "startDate": "2026-04-11T21:30Z",
                    "type": { "id": "986", "abbreviation": "" },
                    "format": { "regulation": { "periods": 3 } },
                    "competitors": [
                        { "order": 1, "winner": false, "athlete": { "displayName": "TBA" } },
                        { "order": 2, "winner": false, "athlete": { "displayName": "Opponent TBA" } }
                    ]
                },
                {
                    "id": "401867663",
                    "date": "2026-04-11T21:30Z",
                    "startDate": "2026-04-11T21:30Z",
                    "type": { "id": "969", "abbreviation": "Welterweight" },
                    "format": { "regulation": { "periods": 3 } },
                    "status": {
                        "period": 3,
                        "displayClock": "5:00",
                        "type": { "state": "post", "completed": true, "detail": "Unanimous Decision 29-28" }
                    },
                    "details": [{ "type": { "text": "Fight Over" } }],
                    "competitors": [
                        {
                            "order": 2,
                            "winner": false,
                            "score": "28",
                            "athlete": { "displayName": "Francisco Prado", "fullName": "Francisco Prado" },
                            "records": [{ "summary": "13-5-0" }],
                            "linescores": [{ "value": 42.0 }]
                        },
                        {
                            "order": 1,
                            "winner": true,
                            "score": "29",
                            "athlete": { "displayName": "Charles Radtke", "fullName": "Charles Radtke" },
                            "records": [{ "summary": "12-6-0" }],
                            "linescores": [{ "value": 64.0 }]
                        }
                    ]
                },
                {
                    "id": "401862588",
                    "date": "2026-04-12T01:00Z",
                    "startDate": "2026-04-12T01:00Z",
                    "type": { "id": "990", "abbreviation": "Light Heavyweight" },
                    "format": { "regulation": { "periods": 3 } },
                    "status": {
                        "period": 1,
                        "displayClock": "1:52",
                        "type": { "state": "post", "completed": true, "detail": "Submission - Rear Naked Choke" }
                    },
                    "competitors": [
                        {
                            "order": 1,
                            "winner": true,
                            "athlete": { "displayName": "Azamat Murzakanov" },
                            "records": [{ "summary": "16-2-0" }]
                        },
                        {
                            "order": 2,
                            "winner": false,
                            "athlete": { "displayName": "Paulo Costa" },
                            "records": [{ "summary": "16-4-0" }]
                        }
                    ]
                },
                {
                    "id": "401863171",
                    "date": "2026-04-12T01:00Z",
                    "startDate": "2026-04-12T01:00Z",
                    "type": { "id": "990", "abbreviation": "Light Heavyweight" },
                    "format": { "regulation": { "periods": 5 } },
                    "status": {
                        "period": 2,
                        "displayClock": "3:14",
                        "type": { "state": "post", "completed": true, "detail": "KO/TKO - R2 3:14" }
                    },
                    "details": [
                        { "type": { "text": "Knockdown" } },
                        { "type": { "text": "Fight Over" } }
                    ],
                    "competitors": [
                        {
                            "order": 2,
                            "winner": false,
                            "score": "0",
                            "athlete": { "displayName": "Carlos Ulberg", "fullName": "Carlos Ulberg" },
                            "records": [{ "summary": "15-1-0" }]
                        },
                        {
                            "order": 1,
                            "winner": true,
                            "score": "1",
                            "athlete": { "displayName": "Jiří Procházka", "fullName": "Jiří Procházka" },
                            "records": [{ "summary": "32-6-1" }]
                        }
                    ]
                }
            ]
        })
    }

    #[test]
    fn spoiler_free_espn_parser_strips_all_results_and_orders_main_event_first() {
        let raw = realistic_espn_ufc_327_json();
        let ev = parse_espn_ufc_event(&raw).expect("UFC 327 parsed");

        assert_eq!(ev.id, "600058745");
        assert_eq!(ev.event_num, Some(327));
        assert_eq!(ev.venue, "Kaseya Center · Miami, FL");
        assert_eq!(ev.main_event, "Jiří Procházka vs. Carlos Ulberg");
        // Placeholder TBA vs Opponent TBA bout must be excluded
        assert_eq!(ev.bouts.len(), 3);

        // Main Event must be order 1 with A-side (order=1) fighter first & scheduled 5 rounds (NOT finished round 2!)
        let main = &ev.bouts[0];
        assert_eq!(main.order, 1);
        assert_eq!(main.fighter_a, "Jiří Procházka");
        assert_eq!(main.fighter_b, "Carlos Ulberg");
        assert_eq!(main.weight_class, "Light Heavyweight");
        assert_eq!(main.card, "Main Card");
        assert_eq!(main.section, "Main Event");
        assert!(main.is_main_event);
        assert!(main.is_title);
        assert_eq!(main.rounds, 5);

        // Co-Main Event (order 2, same 01:00Z main card timestamp, 3 rounds, non-title)
        let comain = &ev.bouts[1];
        assert_eq!(comain.order, 2);
        assert_eq!(comain.fighter_a, "Azamat Murzakanov");
        assert_eq!(comain.fighter_b, "Paulo Costa");
        assert_eq!(comain.card, "Main Card");
        assert_eq!(comain.section, "Co-Main Event");
        assert!(!comain.is_main_event);
        assert!(!comain.is_title);
        assert_eq!(comain.rounds, 3);

        // Prelim bout (earlier 21:30Z timestamp, A-side Charles Radtke sorted before Francisco Prado)
        let prelim = &ev.bouts[2];
        assert_eq!(prelim.order, 3);
        assert_eq!(prelim.fighter_a, "Charles Radtke");
        assert_eq!(prelim.fighter_b, "Francisco Prado");
        assert_eq!(prelim.card, "Prelims");
        assert_eq!(prelim.section, "Prelims");
        assert_eq!(prelim.rounds, 3);

        // Serialize BOTH fight_card JSON and upcoming_ufc JSON and verify ZERO spoiler leakage
        let card_json = fight_card_found_json(&ev);
        let sched_json = upcoming_ufc_event_json(
            &ev,
            NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
            Utc::now(),
            None,
        );
        for payload in [card_json.to_string(), sched_json.to_string()] {
            let low = payload.to_lowercase();
            for forbidden in [
                "winner",
                "ko/tko",
                "3:14",
                "1:52",
                "rear naked",
                "submission",
                "unanimous",
                "29-28",
                "knockdown",
                "fight over",
                "linescores",
                "32-6-1",
                "15-1-0",
                "displayclock",
                "\"score\"",
                "\"method\"",
            ] {
                assert!(
                    !low.contains(forbidden),
                    "Spoiler token {forbidden:?} leaked into payload: {payload}"
                );
            }
        }
        assert_eq!(card_json["spoilerFree"], json!(true));
        assert_eq!(card_json["found"], json!(true));
        assert_eq!(card_json["date"], json!("2026-04-11"));
        assert_eq!(sched_json["status"], json!("recent"));
        // 2026-04-11T21:30Z in Australia/Sydney (AEST UTC+10 in April after DST ends) -> Sun, 12 Apr 2026 7:30 AM AEST
        assert_eq!(sched_json["sydneyDateIso"], json!("2026-04-12"));
        assert_eq!(sched_json["daysUntil"], json!(2));
    }

    #[test]
    fn matches_ufc_by_ppv_number_and_fighter_surnames() {
        let u327 = parse_espn_ufc_event(&realistic_espn_ufc_327_json()).unwrap();
        let fn_sydney = parse_espn_ufc_event(&json!({
            "id": "600062420",
            "name": "UFC Fight Night: Salkilld vs. Saint Denis",
            "shortName": "UFC Fight Night",
            "date": "2027-02-06T19:00Z",
            "status": { "type": { "state": "pre" } },
            "venues": [{ "fullName": "Afterpay Arena", "address": { "city": "Sydney", "state": "NSW", "country": "Australia" } }],
            "competitions": [{
                "id": "1",
                "date": "2027-02-06T22:00Z",
                "type": { "abbreviation": "Lightweight" },
                "format": { "regulation": { "periods": 5 } },
                "competitors": [
                    { "order": 1, "athlete": { "displayName": "Quillan Salkilld" } },
                    { "order": 2, "athlete": { "displayName": "Benoît Saint Denis" } }
                ]
            }]
        }))
        .unwrap();
        let u322 = parse_espn_ufc_event(&json!({
            "id": "600055322",
            "name": "UFC 322: Della Maddalena vs. Makhachev",
            "shortName": "UFC 322",
            "date": "2025-11-15T23:00Z",
            "status": { "type": { "state": "post" } },
            "competitions": [{
                "id": "2",
                "date": "2025-11-16T03:00Z",
                "type": { "abbreviation": "Welterweight" },
                "format": { "regulation": { "periods": 5 } },
                "competitors": [
                    { "order": 1, "athlete": { "displayName": "Jack Della Maddalena" } },
                    { "order": 2, "athlete": { "displayName": "Islam Makhachev" } }
                ]
            }]
        }))
        .unwrap();

        let pool = vec![u327, fn_sydney, u322];

        // 1. Match by UFC PPV number alone
        let q_num = build_ufc_query("UFC 327", Some(327), None, &[]);
        assert_eq!(match_ufc_event(&pool, &q_num).map(|e| e.id.as_str()), Some("600058745"));

        // 2. Match by accent-insensitive fighter surnames in reversed order (no PPV number)
        let q_names = build_ufc_query(
            "Prochazka vs Ulberg",
            None,
            Some("2026"),
            &["ulberg".into(), "prochazka".into()],
        );
        assert_eq!(match_ufc_event(&pool, &q_names).map(|e| e.id.as_str()), Some("600058745"));

        // 3. Multi-word accented surnames on Fight Night (Fight Night 5-round main event is NOT a title bout!)
        let q_fn = build_ufc_query(
            "UFC Fight Night 290: Salkilld vs. Saint Denis",
            Some(290),
            Some("2027-02-06"),
            &["Salkilld".into(), "Saint Denis".into()],
        );
        assert_eq!(q_fn.ppv_num, None, "Fight Night number must never be treated as PPV number");
        let matched_fn = match_ufc_event(&pool, &q_fn).expect("Fight Night matched");
        assert_eq!(matched_fn.id, "600062420");
        assert!(!matched_fn.bouts[0].is_title, "Fight Night main event should not be marked isTitle");

        // 4. Multi-word surname ("Della Maddalena") + Makhachev
        let q_jdm = build_ufc_query(
            "Della Maddalena vs Makhachev",
            None,
            Some("2025"),
            &["Della Maddalena".into(), "Makhachev".into()],
        );
        assert_eq!(match_ufc_event(&pool, &q_jdm).map(|e| e.id.as_str()), Some("600055322"));

        // 5. Fight Night 327 must NOT falsely match PPV UFC 327 by number
        let q_fn_collision = build_ufc_query(
            "UFC Fight Night 327: Someone vs. Other",
            Some(327),
            Some("2026"),
            &["Someone".into(), "Other".into()],
        );
        assert!(match_ufc_event(&pool, &q_fn_collision).is_none());

        // 6. Late-replacement opponent matched by exact date ±2d + one headliner
        let q_repl = build_ufc_query(
            "Prochazka vs Ankalaev",
            None,
            Some("2026-04-12"),
            &["Prochazka".into(), "Ankalaev".into()],
        );
        assert_eq!(match_ufc_event(&pool, &q_repl).map(|e| e.id.as_str()), Some("600058745"));
    }

    #[test]
    fn days_until_and_library_state_classification() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        assert_eq!(days_until("2026-11-21", today), Some(0));
        assert_eq!(days_until("2026-11-22", today), Some(1));
        assert_eq!(days_until("2026-12-01", today), Some(10));
        assert_eq!(days_until("2026-11-18", today), Some(-3));
        assert_eq!(days_until("2026-11-21T15:00Z", today), Some(0));
        assert_eq!(days_until("", today), None);
        assert_eq!(days_until("2026", today), None);

        // Library state classification across quality tiers
        assert_eq!(classify_tier_state(true, QualityTier::Cam), "cam");
        assert_eq!(classify_tier_state(false, QualityTier::Hd), "hd");
        assert_eq!(classify_tier_state(false, QualityTier::Bluray), "hd");
        assert_eq!(classify_tier_state(false, QualityTier::Uhd4k), "4k");
        assert_eq!(classify_tier_state(false, QualityTier::Hdr4k), "4k");
        assert_eq!(classify_tier_state(false, QualityTier::Dv4k), "4k");

        // Status copy verification
        assert_eq!(movie_radar_status("missing", Some(-14)), "In cinemas · digital soon");
        assert_eq!(movie_radar_status("missing", Some(0)), "Releases today");
        assert_eq!(movie_radar_status("missing", Some(12)), "In cinemas in 12d");
        assert_eq!(movie_radar_status("cam", Some(-5)), "CAM only — waiting for clean HD/4K");
        assert_eq!(movie_radar_status("hd", Some(-30)), "In HD — waiting for 4K");
        assert_eq!(movie_radar_status("4k", Some(-60)), "Available in 4K");

        // Episode status copy verification
        assert_eq!(episode_status_text(-3, true), "Aired 3d ago — watch for drop");
        assert_eq!(episode_status_text(0, true), "Aired today — watch for drop");
        assert_eq!(episode_status_text(0, false), "Airs today");
        assert_eq!(episode_status_text(1, false), "Airs tomorrow");
        assert_eq!(episode_status_text(6, false), "Airs in 6d");
    }

    #[test]
    fn ufc_cards_cache_roundtrip_is_spoiler_free() {
        let tmp = std::env::temp_dir().join(format!(
            "tv_radar_test_{}_{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let cache_path = tmp.join("ufc-cards-cache.json");

        let ev = parse_espn_ufc_event(&realistic_espn_ufc_327_json()).unwrap();
        let mut cache = UfcCardsCache::default();
        cache.events.insert(
            ev.id.clone(),
            CachedCardEvent {
                fetched_at: 123_456,
                event: ev,
            },
        );
        cache.years.insert("2026".into(), 123_456);
        write_ufc_cards_cache(&cache_path, &cache);

        let raw_text = std::fs::read_to_string(&cache_path).unwrap().to_lowercase();
        assert!(!raw_text.contains("winner"));
        assert!(!raw_text.contains("ko/tko"));
        assert!(!raw_text.contains("32-6-1"));

        let loaded = read_ufc_cards_cache(&cache_path);
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events["600058745"].event.bouts.len(), 3);
        assert_eq!(loaded.years.get("2026"), Some(&123_456));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
