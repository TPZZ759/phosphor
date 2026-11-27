// Personal streaming front-end for an Xtream Codes subscription — Rust server.
mod subsync; mod fmp4; mod intro; mod skipdb; mod remote; mod guide; mod sync; mod cast; mod epgx; mod display; mod mpv; mod spool; mod livebuf; mod anime; mod docs; mod extsubs; mod filler; mod fights; mod showcase; mod radar; mod dna; mod marathons; mod app; mod catalog; mod discover; mod extras; mod fixtures; mod meta; mod rec; mod sports; mod store; mod stream; mod tmdb; mod util; mod xc;
pub use extras::notify;

pub fn notify_all(app: &App, kind: &str, title: &str, body: &str, link: &str, action: Value) {
    let _ = app.remote.tx.send(json!({ "cmd": "alert", "kind": kind, "title": title, "body": body, "link": link, "action": action, "ts": now_ms() }).to_string());
    notify::notify(title, body, link);
}

use app::App;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rec::sp;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tmdb::TTL_D;
use util::{now_ms, s, tmdb_img};

type A = State<Arc<App>>;
type Q = Query<HashMap<String, String>>;

fn j(v: Value) -> Response { ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response() }
fn ja(v: Arc<Value>) -> Response { ([(header::CACHE_CONTROL, "no-store"), (header::CONTENT_TYPE, "application/json; charset=utf-8")], serde_json::to_vec(&*v).unwrap()).into_response() }
fn err(code: u16, msg: &str) -> Response { (StatusCode::from_u16(code).unwrap(), Json(json!({ "ok": false, "error": msg }))).into_response() }
fn not_ready(app: &App) -> Option<Response> {
    if app.ready() { return None; }
    Some((StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "loading": true, "error": *app.sync_error.lock().unwrap() }))).into_response())
}

#[tokio::main]
async fn main() {
    // Self-test hook: tv.exe --subsync <audio.pcm> <subs.vtt>
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 4 && args[1] == "--intro" {
        let rd = |p: &str| -> Vec<i16> { std::fs::read(p).unwrap_or_default().chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect() };
        let (a, b) = (intro::fingerprint(&rd(&args[2])), intro::fingerprint(&rd(&args[3])));
        let t = std::time::Instant::now(); let m = intro::best_match(&a, &b, 10.0);
        println!("{}", serde_json::json!({ "match": m.map(|m| [m.0, m.1, m.2, m.3]), "ms": t.elapsed().as_millis(), "frames": [a.len(), b.len()] })); return;
    }
    if args.len() == 5 && args[1] == "--subsync-at" { println!("{}", subsync::sync_at(std::path::Path::new(&args[2]), std::path::Path::new(&args[3]), args[4].parse().unwrap_or(0))); return; }
    if args.len() == 2 && args[1] == "--display" { display::self_test(); return; }
    if args.len() == 4 && args[1] == "--subsync" { println!("{}", subsync::sync(std::path::Path::new(&args[2]), std::path::Path::new(&args[3]))); return; }
    let root = std::env::var("TV_ROOT").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::current_dir().unwrap());
    let data = root.join("data"); let _ = std::fs::create_dir_all(data.join("raw"));
    let cfg = Arc::new(store::Config::load(root.join("config.json")));
    let http = reqwest::Client::builder()
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .gzip(true)
        .pool_max_idle_per_host(16)
        .build().unwrap();
    let app = Arc::new(App {
        public: std::env::var("TV_PUBLIC").map(std::path::PathBuf::from).unwrap_or_else(|_| root.join("public")),
        state: store::State::load(data.join("state.json")),
        xc: xc::Xc::new(cfg.clone(), http.clone()),
        tmdb: tmdb::Tmdb::new(cfg.clone(), http.clone(), data.join("tmdb-cache.json")),
        cat: Default::default(), meta: std::sync::RwLock::new(meta::load(&data.join("meta.json"))),
        meta_running: Default::default(), meta_progress: Default::default(), memo: Default::default(), epg: Default::default(),
        syncing: Default::default(), sync_error: Default::default(), stream: stream::Streamer::new(&data), fixtures: Default::default(),
        omdb: extras::Omdb::new(data.join("omdb.json")), kw_cache: Default::default(), acct: Default::default(), events_raw: Default::default(),
        net_idx: Default::default(), xinfo: Default::default(), tri: Default::default(), skips: intro::Skips::new(&data), skipdb: skipdb::SkipDb::new(&data), remote: remote::Remote::new(), jobs: Default::default(), cast: Default::default(), display: display::DisplayCtl::load(&data), stream_gen: Default::default(), mpv: mpv::Mpv::new(), dubs: anime::DubCache::load(&data), http, cfg, data,
    });

    tokio::spawn(startup(app.clone()));
    if remote::enabled(&app) { remote::ensure_listener(app.clone()); }
    spawn_loops(app.clone());
    tokio::spawn(display::watchdog(app.clone()));

    let api = Router::new()
        .route("/api/status", get(status)).route("/api/account", get(account)).route("/api/preflight", get(preflight))
        .route("/api/settings", get(settings_get).post(settings_post)).route("/api/sync", post(sync_now))
        .route("/api/hero", get(hero)).route("/api/home", get(home)).route("/api/movies", get(movies_page)).route("/api/series", get(series_page))
        .route("/api/anime", get(anime_page)).route("/api/anime/browse", get(anime_browse)).route("/api/anime/check-dub", post(anime_check_dub)).route("/api/anime/check-series/{id}", post(anime_check_series)).route("/api/anime/dub-watch", get(anime_dub_watch_get).post(anime_dub_watch_post))
        .route("/api/docs", get(docs_page)).route("/api/docs/browse", get(docs_browse))
        .route("/api/fights", get(fights_page)).route("/api/fights/browse", get(fights_browse)).route("/api/fights/card/{id}", get(fights_card))
        .route("/api/showcase", get(showcase_page)).route("/api/showcase/browse", get(showcase_browse)).route("/api/showcase/upgrades", get(showcase_upgrades))
        .route("/api/radar", get(radar_page))
        .route("/api/dna", get(dna_page)).route("/api/specials", get(specials_page)).route("/api/specials/browse", get(specials_browse))
        .route("/api/marathons", get(marathons_page)).route("/api/marathons/{slug}", get(marathons_detail))
        .route("/api/extsubs/search", get(extsubs_search)).route("/api/extsubs/fetch", post(extsubs_fetch)).route("/api/extsubs/file/{name}", get(extsubs_file)).route("/api/extsubs/sync/{key}", get(extsubs_sync))
        .route("/api/concierge", post(concierge_route))
        .route("/api/browse", get(browse)).route("/api/genres", get(genres)).route("/api/collection/{id}", get(collection)).route("/api/person/{id}", get(person))
        .route("/api/peek/{k}/{id}", get(peek)).route("/api/surprise", get(surprise)).route("/api/external/{mt}/{id}", get(external))
        .route("/api/wanted", post(wanted)).route("/api/watched", post(watched)).route("/api/episodes-watched", post(episodes_watched))
        .route("/api/prefs", get(prefs_get).post(prefs_post)).route("/api/saved-rows", post(saved_rows))
        .route("/api/sports", get(sports_page)).route("/api/leagues", get(leagues)).route("/api/teams/{league}", get(teams_get)).route("/api/teams", post(teams_post))
        .route("/api/rate", post(rate)).route("/api/follow", post(follow)).route("/api/trailer/{k}/{id}", get(trailer)).route("/api/subs/{i}", get(subs)).route("/api/subsync/{i}", get(subsync_route)).route("/api/streamstart", get(stream_start))
        .route("/api/live/groups", get(live_groups)).route("/api/live/channels", get(live_channels)).route("/api/live/recent", get(live_recent_get).post(live_recent_post)).route("/api/live/channel/{id}", get(live_channel_info)).route("/api/live/keepalive", post(live_keepalive)).route("/api/epg/{id}", get(epg))
        .route("/api/search", get(search)).route("/api/movie/{id}", get(movie_detail)).route("/api/series/{id}", get(series_detail))
        .route("/api/mylist", get(mylist)).route("/api/progress", post(progress))
        .route("/api/continue", get(continue_get)).route("/api/continue/remove", post(continue_remove)).route("/api/continue/restore", post(continue_restore)).route("/api/continue/pin", post(continue_pin)).route("/api/continue/done", post(continue_done))
        .route("/api/list", post(list)).route("/api/fav", post(fav))
        .route("/api/reminders", get(reminders_get).post(reminders_post))
        .route("/api/probe/{kind}/{id}", get(probe)).route("/api/skips/stats", get(skips_stats)).route("/api/skips/resolve", get(skips_resolve)).route("/api/health", post(health)).route("/api/health/reset", post(health_reset)).route("/api/syshealth", get(sys_health)).route("/api/pick", post(pick)).route("/api/guide", get(guide::guide)).route("/api/tonight", get(guide::tonight)).route("/api/import/letterboxd", post(sync::letterboxd)).route("/api/import/status", get(sync::import_status)).route("/api/trakt", get(sync::trakt_info)).route("/api/trakt/connect", post(sync::trakt_connect)).route("/api/trakt/disconnect", post(sync::trakt_disconnect)).route("/api/trakt/sync", post(sync::trakt_sync)).route("/api/cast/devices", get(cast::devices)).route("/api/cast/start", post(cast::start)).route("/api/cast/control", post(cast::control)).route("/api/cast/status", get(cast::status)).route("/api/remote", get(remote::info).post(remote::set)).route("/api/remote/events", get(remote::events)).route("/api/remote/state", post(remote::tv_state)).route("/api/remote/send", post(remote::send_local)).route("/api/ep-alts/{sid}/{season}/{n}", get(ep_alts)).route("/api/display", get(display::info)).route("/api/display/match", post(display::matcher)).route("/api/display/restore", post(display::restore_route)).route("/api/mpv", get(mpv::info)).route("/api/mpv/play", post(mpv::play)).route("/api/mpv/stop", post(mpv::stop)).route("/api/mpv/control", post(mpv::control_route)).route("/api/mpv/install", post(mpv::install)).route("/stream/{kind}/{id}", get(stream_route)).route("/spool/{gen}", get(spool::serve)).route("/livebuf/{gen}", get(livebuf::serve)).route("/api/stop", post(stop)).route("/api/streamstats", get(stream_stats))
        .with_state(app.clone());
    let static_files = tower_http::services::ServeDir::new(&app.public).fallback(tower_http::services::ServeFile::new(app.public.join("index.html")));
    // no-cache: the browser revalidates (cheap 304s), so front-end updates always load.
    let router = api.fallback_service(static_files).layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(header::CACHE_CONTROL, header::HeaderValue::from_static("no-cache")));
    let port = app.cfg.get("port").as_u64().unwrap_or(7878);
    let addr = std::env::var("TV_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(port as u16);
    spool::set_port(addr);
    livebuf::set_port(addr);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", addr)).await.expect("port in use");
    if let Ok(l6) = tokio::net::TcpListener::bind(("::1", addr)).await { let r6 = router.clone(); tokio::spawn(async move { let _ = axum::serve(l6, r6).await; }); }
    println!("TV running at http://localhost:{addr}");
    axum::serve(listener, router).await.unwrap();
}

// ---------------------------------------------------------------- background
async fn startup(app: Arc<App>) {
    if catalog::Catalog::have_cache(&app.data) {
        let a = app.clone(); let c = tokio::task::spawn_blocking(move || catalog::Catalog::load(&a.data)).await.unwrap();
        *app.cat.write().unwrap() = Arc::new(c);
        let age = std::fs::metadata(app.data.join("raw/get_vod_streams.json")).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(u64::MAX);
        if age > app.cfg.get("catalogRefreshHours").as_u64().unwrap_or(12) * 3600 { tokio::spawn(sync(app.clone())); }
    } else { sync(app.clone()).await; }
    after_catalog(app).await;
}
async fn after_catalog(app: Arc<App>) {
    { let a = app.clone(); tokio::task::spawn_blocking(move || sports::load_epg(&a)).await.ok(); }
    tokio::spawn(extra_guide(app.clone()));
    *app.tri.write().unwrap() = None; app.net_idx.lock().unwrap().clear(); app.memo.expire();
    discover::check_wanted(&app);
    for ev in fights::check_new_ufc(&app) {
        let title = s(&ev["title"]);
        let body = if !s(&ev["matchup"]).is_empty() { s(&ev["matchup"]) } else { "New UFC replay available".into() };
        notify_all(&app, "ufc", &format!("New UFC Replay: {title}"), &body, "http://localhost:7878/#/fights",
            json!({ "k": "m", "id": ev["id"], "title": title, "label": "Watch replay" }));
    }
    for u in showcase::check_upgrades(&app) {
        let title = s(&u["alertTitle"]);
        let body = s(&u["alertBody"]);
        notify_all(&app, "upgrade", &title, &body, "http://localhost:7878/#/4k",
            json!({ "k": u["k"], "id": u["id"], "title": u["title"], "label": "View title" }));
    }
    tokio::spawn(meta::build(app.clone()));
    prewarm(app).await;
}
async fn sync(app: Arc<App>) {
    if app.syncing.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
    *app.sync_error.lock().unwrap() = None;
    let dir = catalog::Catalog::raw_dir(&app.data); let mut ok = true;
    for n in catalog::RAW { if let Err(e) = app.xc.download(n, &dir.join(format!("{n}.json"))).await { *app.sync_error.lock().unwrap() = Some(e); ok = false; break; } }
    if ok {
        let _ = app.xc.download(&app.xc.epg_path(), &dir.join("epg.xml")).await;
        let a = app.clone(); let c = tokio::task::spawn_blocking(move || catalog::Catalog::load(&a.data)).await.unwrap();
        *app.cat.write().unwrap() = Arc::new(c);
    }
    app.syncing.store(false, std::sync::atomic::Ordering::SeqCst);
    if ok { Box::pin(after_catalog(app)).await; }
}
pub async fn prewarm_pub(app: Arc<App>) { prewarm(app).await }
/// Compute every shared page in the background so requests are answered from memory.
async fn prewarm(app: Arc<App>) {
    if !app.ready() { return; }
    let t0 = std::time::Instant::now();
    let _ = tokio::join!(
        app.memo("hero", 30 * 60_000, rec::hero), app.memo("home-shared", 30 * 60_000, rec::home_shared),
        app.memo("movies", 30 * 60_000, |a| async move { page_rows(a, false).await }), app.memo("series", 30 * 60_000, |a| async move { page_rows(a, true).await }),
        app.memo("sports", 45_000, sports_compute), app.memo("foryou", 20 * 60_000, rec::for_you), app.memo("anime", 30 * 60_000, anime::page), app.memo("docs", 30 * 60_000, docs::page),
        app.memo("showcase", 20 * 60_000, |a| async move { tokio::task::spawn_blocking(move || showcase::hub(&a)).await.unwrap_or_default() }));
    println!("[prewarm] pages ready in {}ms", t0.elapsed().as_millis());
}
fn spawn_loops(app: Arc<App>) {
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(1)).await; let b = a.clone(); let _ = tokio::task::spawn_blocking(move || b.state.flush()).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(10)).await; let b = a.clone(); let _ = tokio::task::spawn_blocking(move || { b.tmdb.flush(); b.skipdb.save_if_dirty(false); }).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(600)).await; if !a.stream.is_active() && !mpv::running(&a) && !livebuf::is_active() { prewarm(a.clone()).await; } } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(60)).await; if a.ready() { fixtures::alerts(&a).await; } } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(30)).await; reminders_tick(&a); } });
    let a = app.clone(); tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(20)).await; loop { if a.ready() { extras::check_episodes(a.clone()).await; } tokio::time::sleep(std::time::Duration::from_secs(7200)).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(3 * 3600)).await; let b = a.clone(); let _ = tokio::task::spawn_blocking(move || sports::load_epg(&b)).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(12 * 3600)).await; extra_guide(a.clone()).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(a.cfg.get("catalogRefreshHours").as_u64().unwrap_or(12) * 3600)).await;
        for _ in 0..240 { if !a.stream.is_active() && !mpv::running(&a) && !livebuf::is_active() { break; } tokio::time::sleep(std::time::Duration::from_secs(60)).await; } // not mid-film
        sync(a.clone()).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(12 * 3600)).await; meta::build(a.clone()).await; } });
    let a = app.clone(); tokio::spawn(async move { loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; expiry_reminder(&a).await; } });
    let a = app.clone(); tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(300)).await; loop { if a.ready() { sync::pull(a.clone()).await; } tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await; } });
    let a = app.clone(); tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(25)).await; loop { if a.ready() { skipdb::prefetch_worker(a.clone()).await; } tokio::time::sleep(std::time::Duration::from_secs(1800)).await; } });
    let a = app; tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(180)).await; loop { if a.ready() { intro_worker(&a).await; } tokio::time::sleep(std::time::Duration::from_secs(900)).await; } });
}
async fn expiry_reminder(app: &App) {
    let Ok(a) = app.account(false).await else { return };
    let days = ((a.exp - now_ms()) as f64 / 86_400_000.0).ceil() as i64; let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if a.exp > 0 && days <= 7 && app.state.read(|st| s(&st["prefs"]["expiryNotified"])) != today {
        app.state.with(|st| st["prefs"]["expiryNotified"] = json!(today)); app.state.save();
        notify_all(app, "expiry", "IPTV subscription ending soon", &if days > 0 { format!("{days} day{} left", if days == 1 { "" } else { "s" }) } else { "It has expired".into() }, "http://localhost:7878/", json!({}));
    }
}

fn reminders_tick(app: &App) {
    let now = now_ms();
    let mut to_fire: Vec<Value> = vec![];
    let mut changed = false;
    app.state.with(|st| {
        let Some(arr) = st["reminders"].as_array_mut() else { return };
        for r in arr.iter_mut() {
            if r["fired"].as_bool().unwrap_or(false) { continue; }
            let start = r["start"].as_i64().unwrap_or(0);
            let mins = (start - now) as f64 / 60_000.0;
            if mins <= 2.0 && mins > -10.0 {
                r["fired"] = json!(true);
                to_fire.push(r.clone());
                changed = true;
            }
        }
        let before = arr.len();
        arr.retain(|r| {
            let end = r["end"].as_i64().unwrap_or_else(|| r["start"].as_i64().unwrap_or(0) + 3_600_000);
            now < end + 2 * 3_600_000
        });
        if arr.len() != before { changed = true; }
    });
    if changed { app.state.save(); }
    for r in to_fire {
        let title = s(&r["title"]);
        let ch = s(&r["chTitle"]);
        let start = r["start"].as_i64().unwrap_or(0);
        let mins = ((start - now) as f64 / 60_000.0).round() as i64;
        let when = if mins > 1 { format!("starts in {mins} min") } else { "starting now".into() };
        notify_all(app, "reminder", &format!("{title} ({when})"), &ch, "http://localhost:7878/#/live",
            json!({ "k": "l", "id": r["chId"], "title": ch, "epg": r["epg"], "label": "Tune in" }));
    }
}

async fn reminders_get(State(app): A) -> Response {
    let now = now_ms();
    let list: Vec<Value> = app.state.read(|st| st["reminders"].as_array().into_iter().flatten()
        .filter(|r| !r["fired"].as_bool().unwrap_or(false) && r["start"].as_i64().unwrap_or(0) > now - 10 * 60_000)
        .cloned().collect());
    j(Value::Array(list))
}

async fn reminders_post(State(app): A, Json(b): Json<Value>) -> Response {
    let id = s(&b["id"]); if id.is_empty() { return err(400, "missing id"); }
    let on = b["on"].as_bool().unwrap_or(true);
    app.state.with(|st| {
        if !st["reminders"].is_array() { st["reminders"] = json!([]); }
        let arr = st["reminders"].as_array_mut().unwrap();
        arr.retain(|x| s(&x["id"]) != id);
        if on {
            arr.push(json!({
                "id": id, "chId": b["chId"], "chTitle": s(&b["chTitle"]), "epg": s(&b["epg"]),
                "title": s(&b["title"]), "start": b["start"].as_i64().unwrap_or(0),
                "end": b["end"].as_i64().unwrap_or(0), "desc": s(&b["desc"]), "fired": false, "t": now_ms()
            }));
            arr.sort_by_key(|x| x["start"].as_i64().unwrap_or(0));
        }
    });
    app.state.save();
    reminders_get(State(app)).await
}

// ---------------------------------------------------------------- status / settings / account
async fn status(State(app): A) -> Response {
    let c = app.catalog(); let p = *app.meta_progress.lock().unwrap();
    j(json!({ "ready": app.ready(), "syncing": app.syncing.load(std::sync::atomic::Ordering::Relaxed), "error": *app.sync_error.lock().unwrap(), "lastSync": c.loaded_at,
        "counts": if app.ready() { json!({ "movies": c.movies.len(), "series": c.series.len(), "live": c.live.len() }) } else { Value::Null },
        "tmdb": app.tmdb.enabled(), "tmdbError": *app.tmdb.last_error.lock().unwrap(),
        "meta": { "done": p.0, "total": p.1, "running": app.meta_running.load(std::sync::atomic::Ordering::Relaxed), "indexed": app.meta.read().unwrap().len() },
        "epgChannels": app.epg.read().unwrap().len(), "streaming": app.stream.is_active() || mpv::running(&app) || livebuf::is_active(), "proPlayer": mpv::running(&app), "engine": "rust" }))
}
async fn account(State(app): A, Query(q): Q) -> Response {
    match app.account(q.get("fresh").map_or(false, |x| x == "1")).await {
        Ok(a) => j(json!({ "status": a.status, "active": a.active, "max": a.max, "exp": a.exp, "daysLeft": if a.exp > 0 { Some(((a.exp - now_ms()) as f64 / 86_400_000.0).ceil() as i64) } else { None }, "ours": app.stream.is_active() })),
        Err(e) => err(502, &e),
    }
}
async fn preflight(State(app): A, Query(q): Q) -> Response {
    if std::env::var("TV_TEST_SOURCE").is_ok() || std::env::var("TV_TEST_LIVE_SOURCE").is_ok() { return j(json!({ "busy": false, "ours": true })); }
    let pg = q.get("pg").cloned().unwrap_or_default();
    if app.stream.is_active() { let owner = app.stream.owner.lock().unwrap().clone(); if !pg.is_empty() && !owner.is_empty() && owner != pg { return j(json!({ "busy": true, "ours": true, "otherWindow": true })); } }
    if app.skips.busy.load(std::sync::atomic::Ordering::SeqCst) { return j(json!({ "busy": false, "ours": true })); }
    if app.stream.is_active() || mpv::running(&app) || now_ms() - app.stream.last_stop() < 45_000 { return j(json!({ "busy": false, "ours": true })); }
    match app.account(true).await { Ok(a) => j(json!({ "busy": a.active >= a.max, "active": a.active, "max": a.max })), Err(_) => j(json!({ "busy": false, "unknown": true })) }
}
async fn settings_get(State(app): A) -> Response {
    j(json!({ "savedRows": app.state.read(|st| st["savedRows"].clone()), "omdbKeySet": !app.cfg.str("omdbKey").trim().is_empty(), "tmdbKeySet": app.tmdb.enabled(), "servers": app.cfg.get("servers"), "active": app.xc.base() }))
}
async fn settings_post(State(app): A, Json(b): Json<Value>) -> Response {
    if let Some(k) = b["tmdbKey"].as_str() {
        let prev = app.cfg.str("tmdbKey"); app.cfg.set("tmdbKey", json!(k.trim()));
        if !k.trim().is_empty() && !app.tmdb.test().await { app.cfg.set("tmdbKey", json!(prev)); return err(400, "TMDB rejected that key. Check it and try again."); }
        app.memo.clear(); tokio::spawn(prewarm(app.clone()));
    }
    if let Some(k) = b["omdbKey"].as_str() {
        if !k.trim().is_empty() && !extras::Omdb::test(&app, k.trim()).await { return err(400, "OMDb rejected that key. Check your email for the activation link first."); }
        app.cfg.set("omdbKey", json!(k.trim()));
    }
    j(json!({ "ok": true, "tmdbKeySet": app.tmdb.enabled(), "omdbKeySet": !app.cfg.str("omdbKey").is_empty() }))
}
async fn sync_now(State(app): A) -> Response { tokio::spawn(sync(app.clone())); j(json!({ "ok": true })) }

// ---------------------------------------------------------------- pages
async fn hero(State(app): A) -> Response { if let Some(r) = not_ready(&app) { return r; } ja(app.memo("hero", 30 * 60_000, rec::hero).await) }
async fn home(State(app): A) -> Response { if let Some(r) = not_ready(&app) { return r; } j(rec::home(&app).await) }
async fn page_rows(app: Arc<App>, tv: bool) -> Value {
    let mt = if tv { "tv" } else { "movie" };
    let base = if tv { rec::series(app.clone()).await } else { rec::movies(app.clone()).await };
    let curated = app.memo(&format!("curated-{mt}"), 20 * 60_000, move |a| async move { discover::curated(a, tv).await }).await;
    let curated: Vec<Value> = curated.as_array().cloned().unwrap_or_default();
    let extra: Vec<Value> = if tv { vec![] } else { vec![
        json!({ "id": "franchises", "title": "Franchises", "sub": "Play the whole series in order", "items": discover::franchises(&app), "style": "franchise" }),
        json!({ "id": "coming", "title": "Coming Soon to Cinemas", "sub": "Get alerted when they land", "items": discover::coming_soon(&app).await })] };
    let mut out: Vec<Value> = vec![];
    for r in base.as_array().into_iter().flatten() {
        out.push(r.clone());
        if r["id"] == "m-new" || r["id"] == "s-new" { out.extend(curated.iter().take(3).cloned()); out.extend(extra.iter().take(1).cloned()); }
    }
    let rest: Vec<Value> = curated.iter().skip(3).cloned().chain(extra.iter().skip(1).cloned()).collect();
    match out.iter().position(|r| crate::re!(r"^(m|s)-g").is_match(r["id"].as_str().unwrap_or(""))) { Some(at) => { out.splice(at..at, rest); } None => out.extend(rest) }
    Value::Array(out.into_iter().filter(|r| r["items"].as_array().map_or(false, |a| !a.is_empty())).collect())
}
fn saved_rows_for(app: &App, mt: &str) -> Vec<Value> {
    let rows: Vec<Value> = app.state.read(|st| st["savedRows"].as_array().cloned().unwrap_or_default());
    rows.into_iter().filter(|r| r["kind"] == mt).map(|r| {
        let mut q: HashMap<String, String> = r["params"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), s(v))).collect()).unwrap_or_default();
        q.insert("kind".into(), mt.into()); q.insert("per".into(), "40".into());
        json!({ "id": format!("saved-{}", s(&r["id"])), "savedId": r["id"], "title": r["name"], "sub": "Your saved filter", "items": discover::browse(app, &q)["items"] })
    }).filter(|r| r["items"].as_array().map_or(false, |a| !a.is_empty())).collect()
}
async fn movies_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let rows = app.memo("movies", 30 * 60_000, |a| async move { page_rows(a, false).await }).await;
    let mut out = saved_rows_for(&app, "movie"); out.extend(rows.as_array().cloned().unwrap_or_default()); j(Value::Array(out))
}
async fn series_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let rows = app.memo("series", 30 * 60_000, |a| async move { page_rows(a, true).await }).await;
    let mut out = saved_rows_for(&app, "tv"); out.extend(rows.as_array().cloned().unwrap_or_default()); j(Value::Array(out))
}
async fn anime_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let (base, cw_all, fy_all) = tokio::join!(
        app.memo("anime", 30 * 60_000, anime::page),
        rec::continue_watching(&app),
        app.memo("foryou", 20 * 60_000, rec::for_you)
    );
    let mut v = (*base).clone();
    let cat = app.catalog();
    let cw: Vec<Value> = cw_all.into_iter().filter(|it| {
        if it["k"] == "e" { it["sid"].as_i64().and_then(|sid| cat.series_item(sid)).map_or(false, |s| anime::is_anime_item(&app, s)) }
        else if it["k"] == "m" { it["id"].as_i64().and_then(|id| cat.movie(id)).map_or(false, |m| anime::is_anime_item(&app, m)) }
        else { false }
    }).collect();
    let new_eps: Vec<Value> = rec::new_episodes(&app).into_iter().filter(|it| {
        it["id"].as_i64().and_then(|sid| cat.series_item(sid)).map_or(false, |s| anime::is_anime_item(&app, s))
    }).collect();
    let fy_anime: Vec<Value> = fy_all.as_array().into_iter().flatten().filter(|it| {
        let id = it["id"].as_i64().unwrap_or(0);
        if it["k"] == "s" { cat.series_item(id).map_or(false, |s| anime::is_anime_item(&app, s)) }
        else if it["k"] == "m" { cat.movie(id).map_or(false, |m| anime::is_anime_item(&app, m)) }
        else { false }
    }).cloned().collect();
    if let Some(rows) = v["rows"].as_array_mut() {
        let mut insert_at = 0usize;
        if !cw.is_empty() {
            rows.insert(insert_at, json!({ "id": "an-continue", "title": "Continue Watching Anime", "items": cw, "style": "continue" }));
            insert_at += 1;
        }
        if !new_eps.is_empty() {
            rows.insert(insert_at, json!({ "id": "an-new-eps", "title": "New Episodes", "sub": "From anime you follow", "items": new_eps, "style": "wide" }));
            insert_at += 1;
        }
        if fy_anime.len() >= 4 {
            let at = (insert_at + 1).min(rows.len());
            rows.insert(at, json!({ "id": "an-foryou", "title": "Picked For You", "sub": "Based on what you watch and like", "items": fy_anime }));
        }
    }
    j(v)
}
async fn anime_browse(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    j(anime::browse(&app, &q))
}
async fn docs_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let base = app.memo("docs", 30 * 60_000, docs::page).await;
    let mut v = (*base).clone();
    let cat = app.catalog();
    let cw: Vec<Value> = rec::continue_watching(&app).await.into_iter().filter(|it| {
        if it["k"] == "e" { it["sid"].as_i64().and_then(|sid| cat.series_item(sid)).map_or(false, |s| docs::is_doc_item(&app, s)) }
        else if it["k"] == "m" { it["id"].as_i64().and_then(|id| cat.movie(id)).map_or(false, |m| docs::is_doc_item(&app, m)) }
        else { false }
    }).collect();
    if !cw.is_empty() {
        if let Some(rows) = v["rows"].as_array_mut() {
            rows.insert(0, json!({ "id": "doc-continue", "title": "Continue Watching Documentaries", "items": cw, "style": "continue" }));
        }
    }
    j(v)
}
async fn docs_browse(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    j(docs::browse(&app, &q))
}
async fn fights_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || fights::hub(&a)).await.unwrap_or_default();
    j(v)
}
async fn fights_browse(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || fights::browse(&a, &q)).await.unwrap_or_default();
    j(v)
}
async fn fights_card(State(app): A, Path(id): Path<i64>) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    j(radar::fight_card(app, id).await)
}
async fn showcase_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let v = app.memo("showcase", 20 * 60_000, |a| async move {
        tokio::task::spawn_blocking(move || showcase::hub(&a)).await.unwrap_or_default()
    }).await;
    ja(v)
}
async fn showcase_browse(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || showcase::browse(&a, &q)).await.unwrap_or_default();
    j(v)
}
async fn showcase_upgrades(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || json!({ "upgrades": showcase::recent_upgrades(&a) })).await.unwrap_or_default();
    j(v)
}
async fn radar_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let v = app.memo("radar", 15 * 60_000, radar::radar).await;
    ja(v)
}
async fn dna_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let v = app.memo("dna", 10 * 60_000, dna::profile).await;
    ja(v)
}
async fn specials_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let v = app.memo("specials", 20 * 60_000, |a| async move {
        tokio::task::spawn_blocking(move || dna::specials_hub(&a)).await.unwrap_or_default()
    }).await;
    ja(v)
}
async fn specials_browse(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || dna::specials_browse(&a, &q)).await.unwrap_or_default();
    j(v)
}
async fn marathons_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || marathons::list(&a)).await.unwrap_or_default();
    j(v)
}
async fn marathons_detail(State(app): A, Path(slug): Path<String>, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let order = q.get("order").cloned().unwrap_or_else(|| "chrono".into());
    let a = app.clone();
    let v = tokio::task::spawn_blocking(move || marathons::detail(&a, &slug, &order)).await.unwrap_or(None);
    match v { Some(x) => j(x), None => err(404, "marathon not found") }
}
async fn anime_check_dub(State(app): A, Json(b): Json<Value>) -> Response {
    if app.stream.is_active() || mpv::running(&app) || livebuf::is_active() {
        return err(409, "Cannot check dub while streaming");
    }
    let kind = s(&b["kind"]);
    let id = b["id"].as_i64().unwrap_or(0);
    if id <= 0 { return err(400, "missing id"); }
    let ext = b["ext"].as_str().map(String::from);
    let url = upstream(&app, if kind == "movie" { "movie" } else { "series" }, &id.to_string(), ext.as_ref(), false);
    let p = app.stream.probe(&url, &app.cfg.str("userAgent")).await;
    let audio = p.as_ref().and_then(|x| x["audio"].as_array()).cloned().unwrap_or_default();
    let has_en = anime::record_probe(&app, if kind == "movie" { "movie" } else { "series" }, id, &audio);
    app.memo.remove_prefix("anime");
    j(json!({ "ok": true, "hasEnAudio": has_en, "dub": match has_en { Some(true) => "yes", Some(false) => "no", None => "unknown" }, "audio": audio }))
}
async fn anime_check_series(State(app): A, Path(id): Path<i64>, body: Option<Json<Value>>) -> Response {
    let season = body.as_ref().and_then(|b| b["season"].as_i64());
    match anime::check_series(&app, id, season).await {
        Ok(v) => j(v),
        Err(msg) => err(409, &msg),
    }
}
async fn anime_dub_watch_get(State(app): A) -> Response { j(anime::dub_watch_list(&app)) }
async fn anime_dub_watch_post(State(app): A, Json(b): Json<Value>) -> Response { j(anime::dub_watch_set(&app, &b)) }
async fn extsubs_search(State(app): A, Query(q): Q) -> Response {
    let mt = q.get("mt").map(|x| x.as_str()).unwrap_or("movie");
    let tmdb = q.get("tmdb").map(|x| x.as_str()).unwrap_or("");
    let imdb = q.get("imdb").map(|x| x.as_str()).unwrap_or("");
    let title = q.get("title").map(|x| x.as_str()).unwrap_or("");
    let season = q.get("s").and_then(|x| x.parse::<i64>().ok());
    let ep = q.get("e").and_then(|x| x.parse::<i64>().ok());
    j(extsubs::search(&app, mt, tmdb, imdb, season, ep, title).await)
}
async fn extsubs_fetch(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]);
    let url = s(&b["url"]);
    if key.is_empty() || url.is_empty() { return err(400, "missing key or url"); }
    match extsubs::fetch(&app, &key, &url).await {
        Ok(v) => j(v),
        Err(e) => err(502, &e),
    }
}
async fn extsubs_file(State(app): A, Path(name): Path<String>) -> Response {
    match extsubs::read_vtt_file(&app, &name) {
        Some(vtt) => ([(header::CONTENT_TYPE, "text/vtt; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], vtt).into_response(),
        None => err(404, "subtitle not found"),
    }
}
async fn extsubs_sync(State(app): A, Path(key): Path<String>) -> Response {
    j(extsubs::sync_cached(&app, &key).await)
}
async fn concierge_route(State(app): A, Json(b): Json<Value>) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    j(discover::concierge(app, b).await)
}
async fn browse(State(app): A, Query(q): Q) -> Response { if let Some(r) = not_ready(&app) { return r; } j(discover::browse(&app, &q)) }
async fn genres() -> Response {
    let m = |g: &[(i64, &str)]| Value::Object(g.iter().map(|(k, v)| (k.to_string(), json!(v))).collect());
    let mut all_map = serde_json::Map::new();
    for (k, v) in meta::MOVIE_GENRES { all_map.insert(k.to_string(), json!(v)); }
    for (k, v) in meta::TV_GENRES { all_map.entry(k.to_string()).or_insert_with(|| json!(v)); }
    let doc_topics: Vec<Value> = docs::DOC_TOPICS.iter().map(|(id, label)| json!({ "id": id, "label": label })).collect();
    j(json!({ "movie": m(meta::MOVIE_GENRES), "tv": m(meta::TV_GENRES), "all": Value::Object(all_map), "docTopics": doc_topics }))
}
async fn collection(State(app): A, Path(id): Path<String>) -> Response { j(discover::collection(&app, &id).await) }
async fn person(State(app): A, Path(id): Path<String>) -> Response { match discover::person(&app, &id).await { Some(p) => j(p), None => err(404, "not found") } }
async fn peek(State(app): A, Path((k, id)): Path<(String, i64)>) -> Response {
    let v = discover::peek(&app, &k, id);
    let (a, tm, pid) = (app.clone(), s(&v["tmdb"]), v["id"].as_i64().unwrap_or(id));
    tokio::spawn(async move {
        if k == "s" { a.xc_info("get_series_info", "series_id", pid, 2 * 3_600_000).await; if !tm.is_empty() { let _ = tokio::join!(a.tmdb.get(format!("/tv/{tm}"), [sp("append_to_response", "credits,videos,content_ratings")], TTL_D), a.tmdb.get(format!("/tv/{tm}/recommendations"), tmdb::NO, TTL_D)); } }
        else { a.xc_info("get_vod_info", "vod_id", pid, 24 * 3_600_000).await; if !tm.is_empty() { let _ = tokio::join!(a.tmdb.get(format!("/movie/{tm}"), [sp("append_to_response", "credits,videos,release_dates,content_ratings")], TTL_D), a.tmdb.get(format!("/movie/{tm}/recommendations"), tmdb::NO, TTL_D)); } }
    });
    j(v)
}
async fn surprise(State(app): A, Query(q): Q) -> Response { j(discover::surprise(&app, q.get("kind").map(|x| x.as_str()).unwrap_or("")).await) }
async fn external(State(app): A, Path((mt, id)): Path<(String, String)>) -> Response {
    let Some(d) = app.tmdb.get(format!("/{mt}/{id}"), [sp("append_to_response", "videos,credits")], TTL_D).await else { return err(404, "not found") };
    let wanted = app.state.read(|st| st["wanted"].get(format!("{mt}:{id}")).is_some());
    let mut x = discover::external(&mt, &d, wanted);
    x["trailer"] = json!(trailer_of(Some(&d), ""));
    x["cast"] = json!(d["credits"]["cast"].as_array().into_iter().flatten().take(6).map(|c| s(&c["name"])).collect::<Vec<_>>());
    x["genres"] = json!(d["genres"].as_array().into_iter().flatten().map(|g| s(&g["name"])).collect::<Vec<_>>());
    j(x)
}
async fn wanted(State(app): A, Json(b): Json<Value>) -> Response {
    let k = format!("{}:{}", s(&b["mt"]), s(&b["tmdb"]));
    app.state.with(|st| if b["on"].as_bool().unwrap_or(false) { st["wanted"][&k] = json!({ "title": b["title"], "poster": b["poster"], "year": b["year"], "t": now_ms() }); } else if let Some(m) = st["wanted"].as_object_mut() { m.remove(&k); });
    sync::push(&app, if b["on"].as_bool().unwrap_or(false) { "watchlist" } else { "unwatchlist" }, sync::ids_body(&s(&b["mt"]), &s(&b["tmdb"])));
    app.state.save(); j(json!({ "ok": true }))
}
async fn watched(State(app): A, Json(b): Json<Value>) -> Response {
    if s(&b["tmdb"]).is_empty() { return err(400, "missing tmdb"); }
    let k = format!("{}:{}", s(&b["mt"]), s(&b["tmdb"]));
    app.state.with(|st| if b["on"].as_bool().unwrap_or(false) { st["watched"][&k] = json!({ "t": now_ms(), "title": b["title"] }); } else if let Some(m) = st["watched"].as_object_mut() { m.remove(&k); });
    sync::push(&app, if b["on"].as_bool().unwrap_or(false) { "history" } else { "unhistory" }, sync::ids_body(&s(&b["mt"]), &s(&b["tmdb"])));
    app.state.save(); app.memo.remove_prefix("curated"); app.memo.remove_prefix("foryou"); j(json!({ "ok": true }))
}
async fn episodes_watched(State(app): A, Json(b): Json<Value>) -> Response {
    let on = b["on"].as_bool().unwrap_or(false);
    let ids: Vec<String> = b["ids"].as_array().into_iter().flatten().map(s).filter(|x| !x.is_empty()).collect();
    app.state.with(|st| for id in &ids {
        let k = format!("e:{id}");
        if on {
            let mut item = b["item"].clone();
            if item.is_object() { item["k"] = json!("e"); item["id"] = json!(id.parse::<i64>().unwrap_or(0)); }
            st["progress"][&k] = json!({ "key": k, "pos": 1, "dur": 1, "t": now_ms(), "item": item });
            if st["traktPushed"].is_null() { st["traktPushed"] = json!({}); }
            st["traktPushed"][&k] = json!(now_ms());
        } else {
            if let Some(m) = st["progress"].as_object_mut() { m.remove(&k); }
            if let Some(m) = st["traktPushed"].as_object_mut() { m.remove(&k); }
        }
    });
    let tmdb = s(&b["tmdb"]);
    if let (Some(sn), Some(eps)) = (b["season"].as_i64(), b["episodes"].as_array()) {
        let ep_nums: Vec<i64> = eps.iter().filter_map(|x| x.as_i64()).collect();
        if let Some(body) = sync::season_episodes_body(&tmdb, sn, &ep_nums) {
            sync::push(&app, if on { "history" } else { "unhistory" }, body);
        }
    }
    app.state.save(); j(json!({ "ok": true }))
}
async fn prefs_get(State(app): A) -> Response { j(app.state.read(|st| st["prefs"].clone())) }
async fn prefs_post(State(app): A, Json(b): Json<Value>) -> Response {
    if let Some(h) = b["hide"].as_array() { let h: Vec<Value> = h.iter().filter(|x| x.is_string()).cloned().collect(); app.state.with(|st| st["prefs"]["hide"] = json!(h)); }
    for k in ["prefer4k", "matchRate", "autoHdr", "sdrOffHdr", "preferHdr", "mse", "mpvVsr", "mpvAnimeSharp", "skipFiller", "hideCams", "ufcSpoilerFree", "autoplay", "autoskip", "autoSkipCredits"] { if let Some(v) = b[k].as_bool() { app.state.with(|st| st["prefs"][k] = json!(v)); } }
    for k in ["display", "pro", "oledPeak", "mpvShaders"] { if let Some(v) = b[k].as_str() { app.state.with(|st| st["prefs"][k] = json!(v)); } }
    app.state.save(); app.memo.clear(); tokio::spawn(prewarm(app.clone()));
    j(json!({ "ok": true, "prefs": app.state.read(|st| st["prefs"].clone()) }))
}
async fn health_reset(State(app): A) -> Response {
    app.state.with(|st| { st["health"] = json!({}); });
    app.state.save();
    app.memo.clear();
    j(json!({ "ok": true }))
}
fn read_net_link_cached() -> Value {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<(i64, Value)>> = OnceLock::new();
    let c = CACHE.get_or_init(|| Mutex::new((0, Value::Null)));
    let now = now_ms();
    {
        let g = c.lock().unwrap();
        if now - g.0 < 30_000 && !g.1.is_null() { return g.1.clone(); }
    }
    #[cfg(windows)]
    let val = {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and $_.HardwareInterface -eq $true } | Sort-Object LinkSpeed -Descending | Select-Object -First 3 Name,InterfaceDescription,LinkSpeed,Speed | ConvertTo-Json -Compress"])
            .creation_flags(0x08000000)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let txt = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let parsed: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
                let arr = if let Some(a) = parsed.as_array() { a.clone() } else if parsed.is_object() { vec![parsed] } else { vec![] };
                let primary = arr.first().cloned().unwrap_or(Value::Null);
                let bps = primary["Speed"].as_u64().unwrap_or(0);
                let mbps = bps / 1_000_000;
                json!({
                    "name": primary["Name"],
                    "desc": primary["InterfaceDescription"],
                    "linkSpeed": primary["LinkSpeed"],
                    "mbps": mbps,
                    "slow": bps > 0 && mbps < 500,
                    "adapters": arr
                })
            }
            _ => json!({ "error": "unavailable" })
        }
    };
    #[cfg(not(windows))]
    let val = json!({});
    *c.lock().unwrap() = (now, val.clone());
    val
}
fn read_hdr_cal_profile() -> Value {
    #[cfg(windows)]
    {
        let dir = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\spool\drivers\color");
        let Ok(rd) = std::fs::read_dir(&dir) else { return Value::Null };
        let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.ends_with(".icc") && (name.contains("hdr") || name.contains("mhc")) {
                if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
                    if newest.as_ref().map_or(true, |(t, _)| mt > *t) { newest = Some((mt, e.path())); }
                }
            }
        }
        let Some((_, path)) = newest else { return Value::Null };
        let Ok(b) = std::fs::read(&path) else { return Value::Null };
        if b.len() < 132 { return Value::Null; }
        let n = u32::from_be_bytes(b[128..132].try_into().unwrap()) as usize;
        for i in 0..n {
            let off = 132 + i * 12;
            if off + 12 > b.len() { break; }
            if &b[off..off + 4] == b"MHC2" {
                let t_off = u32::from_be_bytes(b[off + 4..off + 8].try_into().unwrap()) as usize;
                let t_sz = u32::from_be_bytes(b[off + 8..off + 12].try_into().unwrap()) as usize;
                if t_off + 20 <= b.len() && t_sz >= 20 {
                    let min_l = i32::from_be_bytes(b[t_off + 12..t_off + 16].try_into().unwrap()) as f64 / 65536.0;
                    let peak_l = i32::from_be_bytes(b[t_off + 16..t_off + 20].try_into().unwrap()) as f64 / 65536.0;
                    return json!({
                        "file": path.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(),
                        "minNits": (min_l * 1000.0).round() / 1000.0,
                        "peakNits": peak_l.round(),
                        "raisedBlacks": min_l > 0.05
                    });
                }
            }
        }
        Value::Null
    }
    #[cfg(not(windows))]
    { Value::Null }
}
async fn sys_health(State(app): A) -> Response {
    let (net, disp, hdr_cal) = tokio::join!(
        tokio::task::spawn_blocking(read_net_link_cached),
        tokio::task::spawn_blocking(display::list),
        tokio::task::spawn_blocking(read_hdr_cal_profile)
    );
    let net = net.unwrap_or_else(|_| json!({}));
    let ds = disp.unwrap_or_default();
    let hdr_cal = hdr_cal.unwrap_or(Value::Null);
    let target_gdi = display::target(&app).map(|d| d.gdi).unwrap_or_default();
    let cur_disp = ds.into_iter().find(|d| d.gdi == target_gdi);
    let bad_streams = app.state.read(|st| {
        st["health"].as_object().map(|m| m.values().filter(|v| v["f"].as_i64().unwrap_or(0) > 0 || v["st"].as_i64().unwrap_or(0) >= 3).count()).unwrap_or(0)
    });
    let total_health = app.state.read(|st| st["health"].as_object().map(|m| m.len()).unwrap_or(0));
    let mpv_path = mpv::find(&app).map(|p| p.to_string_lossy().to_string());
    j(json!({
        "net": net,
        "hdrCal": hdr_cal,
        "display": cur_disp.map(|d| json!({ "name": d.name, "gdi": d.gdi, "hz": d.hz, "hdrOn": d.hdr_on, "hdrSupported": d.hdr_supported })),
        "displayChanged": app.display.changed.lock().unwrap().is_some(),
        "mpv": { "available": mpv_path.is_some(), "path": mpv_path, "playing": mpv::running(&app) },
        "streaming": app.stream.is_active(),
        "spool": spool::stats(),
        "livebuf": livebuf::stats(),
        "dubs": anime::dub_stats(&app),
        "probes": app.stream.probe_count(),
        "health": { "tracked": total_health, "penalised": bad_streams }
    }))
}
async fn saved_rows(State(app): A, Json(b): Json<Value>) -> Response {
    app.state.with(|st| {
        let rows = st["savedRows"].as_array_mut().unwrap();
        if let Some(r) = b["remove"].as_str() { rows.retain(|x| x["id"] != r); }
        else if b["name"].is_string() && b["kind"].is_string() {
            rows.push(json!({ "id": format!("{:x}", now_ms()), "name": s(&b["name"]).chars().take(60).collect::<String>(), "kind": if b["kind"] == "tv" { "tv" } else { "movie" }, "params": if b["params"].is_object() { b["params"].clone() } else { json!({}) } }));
        }
    });
    app.state.save(); j(json!({ "ok": true, "savedRows": app.state.read(|st| st["savedRows"].clone()) }))
}

// ---------------------------------------------------------------- sports
async fn sports_compute(app: Arc<App>) -> Value {
    let fx = fixtures::with_feeds(&app).await;
    let shown: Vec<Value> = fx.into_iter().filter(|f| f["feeds"].as_array().map_or(false, |a| !a.is_empty()) || f["fav"].as_bool().unwrap_or(false)).collect();
    let used: std::collections::HashSet<i64> = shown.iter().flat_map(|f| f["feeds"].as_array().cloned().unwrap_or_default()).filter_map(|x| x["id"].as_i64()).collect();
    let evs: Vec<sports::Event> = sports::events(&app, false).iter().filter(|e| !used.contains(&e.id) && !e.alts.iter().any(|a| a["id"].as_i64().map_or(false, |i| used.contains(&i)))).cloned().collect();
    let mut events: Vec<&sports::Event> = evs.iter().filter(|e| e.status != "listed").take(300).collect();
    events.extend(evs.iter().filter(|e| e.status == "listed" && e.en).take(60));
    let mut leagues: Vec<String> = vec![];
    for l in shown.iter().map(|f| s(&f["sport"])).chain(events.iter().map(|e| e.league.clone())) { if !leagues.contains(&l) { leagues.push(l); } }
    let cat = app.catalog();
    let mut rep: Vec<&catalog::Item> = cat.movies.iter().filter(|x| x.sport && !x.poster.is_empty()).collect(); rep.sort_by(|a, b| b.added.cmp(&a.added));
    json!({ "fixtures": shown, "events": events, "leagues": leagues, "replays": rep.into_iter().take(40).map(|x| app.card(x, None)).collect::<Vec<_>>() })
}
async fn sports_page(State(app): A) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let base = app.memo("sports", 45_000, sports_compute).await;
    let mut v = (*base).clone();
    // favourites + networks are cheap and personal: computed per request
    let favset: std::collections::HashSet<String> = app.state.read(|st| st["favs"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default());
    let nets = sports::networks(&app);
    let (on, off): (Vec<Value>, Vec<Value>) = nets.into_iter().partition(|n| !n["now"].is_null() || favset.contains(&format!("l:{}", n["id"])));
    v["networks"] = json!(on.into_iter().take(80).chain(off.into_iter().take(120)).collect::<Vec<_>>());
    v["teams"] = app.state.read(|st| st["teams"].clone());
    // re-flag favourites (teams may have changed since the memo)
    let favs: std::collections::HashSet<String> = app.state.read(|st| st["teams"].as_array().into_iter().flatten().map(|t| format!("{}:{}", s(&t["league"]), s(&t["id"]))).collect());
    for f in v["fixtures"].as_array_mut().into_iter().flatten() {
        let lg = s(&f["league"]);
        f["fav"] = json!(!f["home"].is_null() && (favs.contains(&format!("{lg}:{}", s(&f["home"]["id"]))) || favs.contains(&format!("{lg}:{}", s(&f["away"]["id"])))));
    }
    j(v)
}
async fn leagues() -> Response { j(Value::Array(fixtures::LEAGUES.iter().filter(|l| !l.solo).map(|l| json!({ "id": l.id, "name": l.name })).collect())) }
async fn teams_get(State(app): A, Path(league): Path<String>) -> Response {
    let favs: std::collections::HashSet<String> = app.state.read(|st| st["teams"].as_array().into_iter().flatten().map(|t| format!("{}:{}", s(&t["league"]), s(&t["id"]))).collect());
    let list = fixtures::teams(&app, &league).await;
    j(Value::Array(list.iter().map(|t| { let mut t = t.clone(); t["fav"] = json!(favs.contains(&format!("{}:{}", s(&t["league"]), s(&t["id"])))); t }).collect()))
}
async fn teams_post(State(app): A, Json(b): Json<Value>) -> Response {
    let t = &b["team"]; if s(&t["league"]).is_empty() || s(&t["id"]).is_empty() { return err(400, "bad team"); }
    app.state.with(|st| {
        let a = st["teams"].as_array_mut().unwrap();
        a.retain(|x| !(x["league"] == t["league"] && s(&x["id"]) == s(&t["id"])));
        if b["on"].as_bool().unwrap_or(false) { a.push(json!({ "league": t["league"], "id": s(&t["id"]), "name": t["name"], "short": t["short"], "logo": t["logo"] })); }
    });
    app.state.save(); j(json!({ "ok": true, "teams": app.state.read(|st| st["teams"].clone()) }))
}

// ---------------------------------------------------------------- ratings / follow / trailers / subs
async fn rate(State(app): A, Json(b): Json<Value>) -> Response {
    if s(&b["mt"]).is_empty() || s(&b["tmdb"]).is_empty() { return err(400, "This title has no TMDB match, so it can\u{2019}t be rated."); }
    let k = format!("{}:{}", s(&b["mt"]), s(&b["tmdb"])); let v = b["v"].as_i64().unwrap_or(0);
    app.state.with(|st| if v == 0 { if let Some(m) = st["ratings"].as_object_mut() { m.remove(&k); } } else { st["ratings"][&k] = json!({ "v": if v > 0 { 1 } else { -1 }, "title": b["title"], "t": now_ms() }); });
    { let mut body = sync::ids_body(&s(&b["mt"]), &s(&b["tmdb"])); if v != 0 { for k in ["movies", "shows"] { if let Some(x) = body[k].get_mut(0) { x["rating"] = json!(if v > 0 { 8 } else { 3 }); } } } sync::push(&app, if v == 0 { "unrate" } else { "rate" }, body); }
    app.state.save(); app.memo.clear(); tokio::spawn(prewarm(app.clone())); j(json!({ "ok": true }))
}
async fn follow(State(app): A, Json(b): Json<Value>) -> Response { extras::follow(&app, &s(&b["sid"]), b["on"].as_bool().unwrap_or(false), &b["item"]); j(json!({ "ok": true })) }
fn trailer_of(d: Option<&Value>, fallback: &str) -> String {
    let vids = d.map(|d| if d["videos"]["results"].is_array() { d["videos"]["results"].clone() } else { d["results"].clone() }).unwrap_or(Value::Null);
    let list = vids.as_array().cloned().unwrap_or_default();
    list.iter().find(|v| v["site"] == "YouTube" && v["type"] == "Trailer").or(list.iter().find(|v| v["site"] == "YouTube")).map(|v| s(&v["key"])).unwrap_or_else(|| fallback.to_string())
}
async fn trailer(State(app): A, Path((k, id)): Path<(String, i64)>) -> Response {
    let cat = app.catalog(); let it = if k == "s" { cat.series_item(id) } else { cat.movie(id) };
    let Some(it) = it.cloned() else { return j(json!({ "key": "" })) };
    let Some(t) = it.tmdb.clone().filter(|_| app.tmdb.enabled()) else { return j(json!({ "key": it.trailer })) };
    let d = app.tmdb.get(format!("/{}/{t}/videos", if k == "s" { "tv" } else { "movie" }), crate::tmdb::NO, 7 * TTL_D).await;
    j(json!({ "key": trailer_of(d.as_deref(), &it.trailer) }))
}
async fn stream_start(State(app): A) -> Response {
    // ffmpeg writes the marker as soon as the first frame passes; give it a moment
    for _ in 0..40 { if let Some(k) = app.stream.session_start() { return j(json!({ "ok": true, "start": k })); } tokio::time::sleep(std::time::Duration::from_millis(100)).await; }
    j(json!({ "ok": false }))
}
async fn subsync_route(State(app): A, Path(i): Path<i64>) -> Response {
    let (pcm, vtt) = app.stream.sync_paths(i);
    let base = (app.stream.session_start().unwrap_or(0.0) * 1000.0) as i64;
    let mut r = tokio::task::spawn_blocking(move || subsync::sync_at(&pcm, &vtt, base)).await.unwrap_or(json!({ "ready": false }));
    r["base"] = json!(base); j(r)
}
async fn subs(State(app): A, Path(i): Path<i64>) -> Response { ([(header::CONTENT_TYPE, "text/vtt; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], app.stream.read_sub(i)).into_response() }

// ---------------------------------------------------------------- live TV
fn record_recent_live(app: &App, id: i64, fallback_title: &str) {
    let cat = app.catalog();
    let (title, epg, logo) = match cat.live_item(id) {
        Some(ch) => (ch.title.clone(), ch.epg.clone(), ch.poster.clone()),
        None => (fallback_title.to_string(), String::new(), String::new()),
    };
    if id <= 0 || title.is_empty() { return; }
    app.state.with(|st| {
        if !st["recentLive"].is_array() { st["recentLive"] = json!([]); }
        let arr = st["recentLive"].as_array_mut().unwrap();
        arr.retain(|x| x["id"].as_i64() != Some(id));
        arr.insert(0, json!({ "id": id, "title": title, "epg": epg, "logo": logo, "t": now_ms() }));
        arr.truncate(12);
    });
    app.state.save();
}
fn recent_live_list(app: &App) -> Vec<Value> {
    let cat = app.catalog();
    let favs: std::collections::HashSet<String> = app.state.read(|st| st["favs"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default());
    let raw: Vec<Value> = app.state.read(|st| st["recentLive"].as_array().cloned().unwrap_or_default());
    raw.into_iter().filter_map(|r| {
        let id = r["id"].as_i64()?;
        let mut v = if let Some(ch) = cat.live_item(id) {
            sports::channel_json(app, ch)
        } else {
            sports::merge(json!({ "k": "l", "id": id, "title": s(&r["title"]), "logo": s(&r["logo"]), "epg": s(&r["epg"]) }), sports::now_next(app, &s(&r["epg"])))
        };
        v["fav"] = json!(favs.contains(&format!("l:{id}")));
        v["t"] = r["t"].clone();
        Some(v)
    }).collect()
}
async fn live_recent_get(State(app): A) -> Response { j(Value::Array(recent_live_list(&app))) }
async fn live_recent_post(State(app): A, Json(b): Json<Value>) -> Response {
    let id = b["id"].as_i64().unwrap_or(0);
    if id > 0 { record_recent_live(&app, id, &s(&b["title"])); }
    j(Value::Array(recent_live_list(&app)))
}
async fn live_channel_info(State(app): A, Path(id): Path<i64>) -> Response {
    let cat = app.catalog();
    let Some(ch) = cat.live_item(id) else { return err(404, "not found") };
    let fav = app.state.read(|st| st["favs"].get(&format!("l:{id}")).is_some());
    let mut v = sports::channel_json(&app, ch);
    v["fav"] = json!(fav);
    v["feeds"] = json!(guide::feeds_for(&app, ch));
    j(v)
}
async fn live_keepalive() -> Response { livebuf::touch(); j(json!({ "ok": true })) }
async fn live_groups(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let all = q.get("all").map_or(false, |x| x == "1"); let cat = app.catalog();
    let mut groups: Vec<&catalog::Cat> = cat.cats.values().filter(|c| c.kind == "live" && c.count > 0 && (all || c.en || c.sport)).collect();
    groups.sort_by_key(|c| c.id.parse::<i64>().unwrap_or(0));
    let favs: Vec<Value> = app.state.read(|st| st["favs"].as_object().map(|m| m.values().map(|f| f["item"].clone()).collect()).unwrap_or_default());
    let ep = app.epg.read().unwrap().clone(); let mut cov: HashMap<&str, usize> = HashMap::new();
    for x in cat.live.iter() { if ep.get(&x.epg).map_or(false, |l| !l.is_empty()) { *cov.entry(x.cat.as_str()).or_default() += 1; } }
    j(json!({ "groups": groups.iter().map(|c| json!({ "id": c.id, "name": c.name, "count": c.count, "sport": c.sport, "epg": cov.get(c.id.as_str()).copied().unwrap_or(0), "country": crate::re!(r"^([A-Z0-9]{2,4})\s*\|").captures(&c.name).map(|x| x[1].to_string()).unwrap_or_default() })).collect::<Vec<_>>(), "favs": favs, "recent": recent_live_list(&app) }))
}
async fn live_channels(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let cat = app.catalog(); let g = q.get("cat").cloned().unwrap_or_default();
    if g == "recent" { return j(Value::Array(recent_live_list(&app))); }
    let favs: std::collections::HashSet<String> = app.state.read(|st| st["favs"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default());
    let list: Vec<&catalog::Item> = if g == "favs" { favs.iter().filter_map(|k| k.strip_prefix("l:").and_then(|x| x.parse().ok())).filter_map(|id| cat.live_item(id)).collect() } else { cat.live.iter().filter(|x| x.cat == g).collect() };
    j(Value::Array(list.into_iter().map(|ch| { let mut v = sports::channel_json(&app, ch); v["fav"] = json!(favs.contains(&format!("l:{}", ch.id))); v }).collect()))
}
async fn epg(State(app): A, Path(id): Path<i64>) -> Response {
    let cat = app.catalog(); let e = app.epg.read().unwrap().clone();
    let list = cat.live_item(id).and_then(|c| e.get(&c.epg)).map(|l| l.iter().map(|p| json!({ "s": p.s, "e": p.e, "title": p.title, "desc": p.desc })).collect::<Vec<_>>()).unwrap_or_default();
    j(Value::Array(list))
}

// ---------------------------------------------------------------- search & details
async fn search(State(app): A, Query(q): Q) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    j(discover::deep_search(app, &q).await)
}

fn duration_of(i: &Value) -> i64 {
    if let Some(c) = crate::re!(r"^(\d+):(\d\d):(\d\d)$").captures(&s(&i["duration"])) { return c[1].parse::<i64>().unwrap() * 3600 + c[2].parse::<i64>().unwrap() * 60 + c[3].parse::<i64>().unwrap(); }
    let d = util::i(&i["duration_secs"]); if d > 0 && d < 400 { d * 60 } else { d }
}
fn cert_of(d: &Value, movie: bool) -> String {
    for cc in ["AU", "US", "GB"] {
        if movie {
            if let Some(r) = d["release_dates"]["results"].as_array().into_iter().flatten().find(|x| x["iso_3166_1"] == cc) {
                if let Some(c) = r["release_dates"].as_array().into_iter().flatten().map(|x| s(&x["certification"])).find(|c| !c.is_empty()) { return c; }
            }
        } else if let Some(r) = d["content_ratings"]["results"].as_array().into_iter().flatten().find(|x| x["iso_3166_1"] == cc) { let c = s(&r["rating"]); if !c.is_empty() { return c; } }
    }
    String::new()
}
fn cast_ids(d: &Value) -> Value {
    Value::Array(d["credits"]["cast"].as_array().into_iter().flatten().take(12).map(|c| json!({ "id": c["id"], "name": c["name"], "role": c["character"], "photo": tmdb_img(c["profile_path"].as_str().unwrap_or(""), "w185") })).collect())
}
fn split_list(v: &str) -> Vec<String> { v.split(|c| c == ',' || c == '/').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect() }

async fn movie_detail(State(app): A, Path(id): Path<i64>) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let cat = app.catalog(); let Some(it) = cat.movie(id).cloned() else { return err(404, "not found") };
    let tm = it.tmdb.clone().filter(|_| app.tmdb.enabled());
    let (info, d) = tokio::join!(async { Ok::<Value, String>(app.xc_info("get_vod_info", "vod_id", it.id, 24 * 3_600_000).await) },
        async { match &tm { Some(t) => app.tmdb.get(format!("/movie/{t}"), [sp("append_to_response", "credits,videos,release_dates,content_ratings")], TTL_D).await, None => None } });
    let info = info.unwrap_or(Value::Null); let i = &info["info"]; let d = d.as_deref().cloned();
    let card = app.card(&it, d.as_ref());
    let imdb = d.as_ref().map(|d| s(&d["imdb_id"])).filter(|x| !x.is_empty()).or_else(|| tm.as_ref().and_then(|t| app.meta_get("movie", t)).map(|m| m.im)).unwrap_or_default();
    let similar = match &tm { Some(t) => { let r = app.tmdb.get(format!("/movie/{t}/recommendations"), crate::tmdb::NO, TTL_D).await; rec::match_results(&app, r.as_ref().and_then(|r| r["results"].as_array()).map(|a| a.as_slice()).unwrap_or(&[]), Some("movie"), 18) } None => vec![] };
    let mut variants = vec![(it.id, it.uhd, it.hdr, it.cam, it.dv, it.bluray, it.dolby_audio, false, it.ext.clone(), it.cat.clone(), cat.cat_name("movie", &it.cat))];
    for v in &it.variants {
        let raw_v = cat.movie_raw.get(&v.id);
        let cid = raw_v.map(|x| x.cat.clone()).unwrap_or_default();
        let ext = raw_v.map(|x| x.ext.clone()).unwrap_or_else(|| "mp4".into());
        let cname = cat.cat_name("movie", &cid);
        variants.push((v.id, v.uhd, v.hdr, v.cam, v.dv, v.bluray, v.dolby_audio, v.sub_market, ext, cid, cname));
    }
    let mut n: HashMap<String, usize> = HashMap::new();
    let mut variants: Vec<Value> = variants.into_iter().map(|(id, uhd, hdr, cam, dv, bluray, dolby_audio, sub_mkt, ext, cid, c)| {
        let base = if cam { "CAM" }
            else if dv { "4K DV" }
            else if uhd && hdr { "4K HDR" }
            else if uhd && dolby_audio { "4K · Dolby Audio" }
            else if uhd { "4K" }
            else if bluray { "Blu-Ray" }
            else if dolby_audio { "HD · Dolby Audio" }
            else { "HD" };
        let base_s = if sub_mkt {
            let tag = if c.to_uppercase().starts_with("NL") { "NL" } else { "Nordic" };
            format!("{base} · {tag}")
        } else { base.to_string() };
        let k = n.entry(base_s.clone()).or_insert(0); *k += 1;
        let label = if *k > 1 { format!("{base_s} {k}") } else { base_s };
        let non_en = sub_mkt && app.dubs.get(&format!("m:{id}")).map_or(false, |d| !d.has_en);
        json!({
            "id": id, "ext": ext, "cat": c, "catId": cid,
            "uhd": uhd, "hdr": hdr, "cam": cam, "dv": dv, "bluray": bluray, "dolbyAudio": dolby_audio,
            "subMarket": sub_mkt, "nonEn": non_en, "label": label,
            "health": source_health(&app, id), "q": app.stream.quality_of(id)
        })
    }).collect();
    rank_sources(&app, &mut variants); apply_pick(&app, &format!("m:{}", it.id), &mut variants);
    let mut out = card;
    let dd = d.clone().unwrap_or(Value::Null);
    let pick = |a: &Value, b: &Value| { let x = s(a); if x.is_empty() { s(b) } else { x } };
    out["kind"] = json!("movie");
    let ov = s(&dd["overview"]); out["overview"] = json!(if ov.is_empty() { pick(&i["plot"], &i["description"]) } else { ov });
    if s(&out["backdrop"]).is_empty() { out["backdrop"] = json!(i["backdrop_path"].get(0).map(s).unwrap_or_default()); }
    out["genres"] = json!(dd["genres"].as_array().map(|a| a.iter().map(|g| s(&g["name"])).collect::<Vec<_>>()).unwrap_or_else(|| split_list(&s(&i["genre"]))));
    let dur = duration_of(i);
    out["runtime"] = json!(dd["runtime"].as_i64().filter(|x| *x > 0).unwrap_or((dur as f64 / 60.0).round() as i64));
    out["duration"] = json!(if dur > 0 { dur } else { dd["runtime"].as_i64().unwrap_or(0) * 60 });
    out["cast"] = json!(dd["credits"]["cast"].as_array().map(|a| a.iter().take(8).map(|c| s(&c["name"])).collect::<Vec<_>>()).unwrap_or_else(|| split_list(&s(&i["cast"])).into_iter().take(8).collect()));
    out["director"] = json!(dd["credits"]["crew"].as_array().and_then(|a| a.iter().find(|c| c["job"] == "Director")).map(|c| s(&c["name"])).unwrap_or_else(|| s(&i["director"])));
    out["cert"] = json!(if d.is_some() { cert_of(&dd, true) } else { String::new() });
    out["trailer"] = json!(trailer_of(d.as_ref(), &{ let t = it.trailer.clone(); if t.is_empty() { s(&i["youtube_trailer"]) } else { t } }));
    out["tagline"] = json!(s(&dd["tagline"])); out["best"] = variants.first().map(|v| v["id"].clone()).unwrap_or(json!(it.id)); out["variants"] = json!(variants);
    out["castIds"] = cast_ids(&dd); out["scores"] = extras::Omdb::ratings(&app, &imdb).await;
    let (watched, in_list, resume, my) = app.state.read(|st| (st["watched"].get(format!("movie:{}", tm.clone().unwrap_or_default())).is_some(), st["list"].get(format!("m:{}", it.id)).is_some(), st["progress"][format!("m:{}", it.id)]["pos"].as_f64().unwrap_or(0.0), tm.as_ref().and_then(|t| st["ratings"][format!("movie:{t}")]["v"].as_i64()).unwrap_or(0)));
    out["watched"] = json!(watched); out["inList"] = json!(in_list); out["tmdb"] = json!(it.tmdb); out["imdb"] = json!(imdb); out["myRating"] = json!(my); out["resume"] = json!(resume); out["similar"] = json!(similar);
    out["extSub"] = json!(extsubs::cached_vtt(&app, &format!("m:{}", it.id)));
    let m_meta = app.meta_of(&it);
    let plats = cat.item_platforms(&it);
    if !plats.is_empty() { out["platforms"] = json!(plats); }
    out["versions"] = json!(1 + it.variants.len());
    if let Some(ref mm) = m_meta {
        if mm.vc > 0 { out["votes"] = json!(mm.vc); }
        if !mm.l.is_empty() { out["lang"] = json!(mm.l); }
        if !mm.oc.is_empty() { out["countries"] = json!(mm.oc); }
    }
    let is_doc = docs::is_doc_item(&app, &it);
    if is_doc {
        out["isDoc"] = json!(true);
        out["docTopics"] = json!(docs::doc_topic_labels(&app, &it));
    }
    let best_t = showcase::best_tier(&app, &it);
    out["qualityTier"] = json!(best_t.as_u8());
    out["qualityLabel"] = json!(best_t.label());
    out["camOnly"] = json!(showcase::is_cam_only(&app, &it));
    let is_anime = anime::is_anime_item(&app, &it);
    out["isAnime"] = json!(is_anime);
    if is_anime {
        out["dubStatus"] = json!(anime::movie_dub_status(&app, &it));
        out["dubWatching"] = json!(app.state.read(|st| st["dubWatch"].get(format!("m:{}", it.id)).is_some()));
    }
    j(out)
}

async fn series_detail(State(app): A, Path(id): Path<i64>) -> Response {
    if let Some(r) = not_ready(&app) { return r; }
    let cat = app.catalog(); let Some(it) = cat.series_item(id).cloned() else { return err(404, "not found") };
    let tm = it.tmdb.clone().filter(|_| app.tmdb.enabled());
    let is_anime = anime::is_anime_item(&app, &it);
    // Fetch primary + all non-CAM variants in parallel (2h cached in xinfo — also pre-warms ep_alts!)
    // so newer seasons/episodes from any EN variant are merged in and per-episode 4K/DV badges are exact.
    let mut var_specs: Vec<(i64, bool, bool, bool, bool, String)> = vec![(it.id, it.uhd, it.dv, it.dolby_audio, false, it.cat.clone())];
    let mut extra_vars: Vec<(i64, bool, bool, bool, bool, String)> = it.variants.iter()
        .filter(|v| !v.cam)
        .map(|v| (v.id, v.uhd, v.dv, v.dolby_audio, v.sub_market, cat.series_raw.get(&v.id).map(|x| x.cat.clone()).unwrap_or_default()))
        .collect();
    // Native EN variants first (4K/DV prioritized), then subtitle-only 4K upgrades
    extra_vars.sort_by_key(|x| (x.4, !x.2, !x.1, !x.3));
    extra_vars.truncate(5);
    var_specs.extend(extra_vars);

    let (var_infos, d, show_filler) = tokio::join!(
        futures_util::future::join_all(var_specs.iter().map(|(vsid, _, _, _, _, _)| app.xc_info("get_series_info", "series_id", *vsid, 2 * 3_600_000))),
        async { match &tm { Some(t) => app.tmdb.get(format!("/tv/{t}"), [sp("append_to_response", "credits,videos,content_ratings")], TTL_D).await, None => None } },
        async { if is_anime { filler::lookup_show(&app, &it.title).await } else { None } }
    );
    let info = var_infos.first().cloned().unwrap_or(Value::Null); let i = &info["info"]; let dd = d.as_deref().cloned().unwrap_or(Value::Null);
    let sid = it.id.to_string();
    let (new_ids, progress, dub_watches, skip_filler) = app.state.read(|st| (
        st["newEps"][&sid]["ids"].as_array().into_iter().flatten().filter_map(|x| x.as_i64()).collect::<std::collections::HashSet<i64>>(),
        st["progress"].clone(),
        st["dubWatch"].as_object().map(|m| m.keys().cloned().collect::<std::collections::HashSet<String>>()).unwrap_or_default(),
        st["prefs"]["skipFiller"].as_bool().unwrap_or(false),
    ));
    let cat_name = cat.cat_name("series", &it.cat);
    // Merge seasons & episodes across all native EN variants (primary wins on duplicate (sn, ep_num) so
    // existing e:{id} progress keys never change), while collecting per-episode 4K/DV/Dolby availability
    // and alternate episode IDs across ALL variants (including sub_market 4K upgrades).
    let mut merged_seasons: std::collections::BTreeMap<i64, std::collections::BTreeMap<i64, (Value, String)>> = std::collections::BTreeMap::new();
    let mut ep_qual: HashMap<(i64, i64), (bool, bool, bool)> = HashMap::new();
    let mut ep_var_ids: HashMap<(i64, i64), Vec<i64>> = HashMap::new();
    for ((_, v_uhd, v_dv, v_da, v_sub, v_cat), vinfo) in var_specs.iter().zip(var_infos.iter()) {
        let Some(eps_obj) = vinfo["episodes"].as_object() else { continue };
        let v_cat_name = cat.cat_name("series", v_cat);
        for (k, arr) in eps_obj {
            let Ok(sn) = k.parse::<i64>() else { continue };
            for e in arr.as_array().into_iter().flatten() {
                let ep_num = util::i(&e["episode_num"]);
                let eid = util::i(&e["id"]);
                if eid <= 0 { continue; }
                let q = ep_qual.entry((sn, ep_num)).or_insert((false, false, false));
                if *v_uhd { q.0 = true; }
                if *v_dv { q.1 = true; }
                if *v_da { q.2 = true; }
                let vids = ep_var_ids.entry((sn, ep_num)).or_default();
                if !vids.contains(&eid) { vids.push(eid); }
                if !*v_sub {
                    merged_seasons.entry(sn).or_default().entry(ep_num).or_insert_with(|| (e.clone(), v_cat_name.clone()));
                }
            }
        }
    }
    // Universal TMDB episode enrichment: fetch TMDB season episode lists (24h cached on disk!) so
    // generic provider placeholders ("Episode 52"), blank plots, missing air dates, and missing/w185
    // stills are upgraded to crisp TMDB titles, synopses, and w500 HD stills across ALL TV & Anime!
    let prov_non_special_total: i64 = merged_seasons.iter().filter(|(&sn, _)| sn > 0).map(|(_, m)| m.len() as i64).sum();
    let max_prov_ep: i64 = merged_seasons.iter().filter(|(&sn, _)| sn > 0)
        .flat_map(|(_, m)| m.keys().copied()).max().unwrap_or(0);
    let continuous_numbering = merged_seasons.iter()
        .any(|(&sn, m)| sn > 1 && m.keys().next().copied().unwrap_or(0) > 10);
    let tmdb_seasons_meta: Vec<(i64, i64, String)> = dd["seasons"].as_array().into_iter().flatten()
        .filter_map(|x| {
            let sn = x["season_number"].as_i64()?;
            let ec = x["episode_count"].as_i64().unwrap_or(0);
            if sn > 0 && ec > 0 { Some((sn, ec, s(&x["name"]))) } else { None }
        }).collect();
    let tmdb_s1_count = tmdb_seasons_meta.iter().find(|(sn, _, _)| *sn == 1).map(|x| x.1).unwrap_or(0);
    // Detect shows like Black Clover where the provider splits TMDB Season 1 (170 eps) into S1..S4
    let split_from_tmdb_s1 = continuous_numbering && tmdb_s1_count > 40 && prov_non_special_total <= tmdb_s1_count + 5;

    let mut tmdb_by_se: HashMap<(i64, i64), Value> = HashMap::new();
    let mut tmdb_by_abs: HashMap<i64, Value> = HashMap::new();
    if let Some(ref t) = tm {
        let fetch_sns: Vec<i64> = if split_from_tmdb_s1 {
            vec![1]
        } else {
            let mut sns: Vec<i64> = tmdb_seasons_meta.iter().map(|x| x.0).collect();
            if sns.is_empty() {
                sns = merged_seasons.keys().copied().filter(|&sn| sn > 0).collect();
            }
            sns.sort_unstable();
            sns.dedup();
            // Cap at 24 seasons so even mega-shows (One Piece, Simpsons) load fast on cold cache
            sns.truncate(24);
            sns
        };
        let fetched = futures_util::future::join_all(
            fetch_sns.iter().map(|&sn| app.tmdb.get(format!("/tv/{t}/season/{sn}"), crate::tmdb::NO, TTL_D))
        ).await;
        let mut cum_abs = 0i64;
        for (sn, sdata) in fetch_sns.into_iter().zip(fetched) {
            let Some(sd) = sdata else { continue };
            for ep in sd["episodes"].as_array().into_iter().flatten() {
                let en = ep["episode_number"].as_i64().unwrap_or(0);
                if en <= 0 { continue; }
                cum_abs += 1;
                tmdb_by_se.insert((sn, en), ep.clone());
                tmdb_by_abs.insert(cum_abs, ep.clone());
            }
        }
    }

    // Auto-heal single-number provider episode skips on continuous-numbered shows (e.g. Black Clover
    // 19425 jumps 102 -> 104..171 across 170 episodes while TMDB S1 has exactly 170 episodes!).
    let sequential_abs = continuous_numbering
        && prov_non_special_total > 0
        && max_prov_ep > prov_non_special_total
        && (tmdb_by_abs.len() as i64) >= prov_non_special_total
        && (tmdb_s1_count == prov_non_special_total || (max_prov_ep - prov_non_special_total) <= 3);

    let mut dubbed_count = 0usize; let mut sub_only_count = 0usize;
    let mut seasons: Vec<Value> = vec![];
    let mut max_abs = 0i64;
    let mut running_seq = 0i64;
    let max_prov_season = merged_seasons.keys().copied().filter(|&sn| sn > 0).max().unwrap_or(0);
    for (sn, ep_map) in merged_seasons {
        let tm_s = dd["seasons"].as_array().and_then(|a| a.iter().find(|x| x["season_number"].as_i64() == Some(sn)));
        let raw_eps: Vec<(Value, String)> = ep_map.into_values().collect();
        let first_num = raw_eps.first().map(|(e, _)| util::i(&e["episode_num"])).unwrap_or(0);
        let season_offset = if sn > 0 && max_abs > 0 && first_num > 0 && first_num <= max_abs { max_abs } else { 0 };
        let mut season_max = max_abs;
        let mut first_season_abs = 0i64;
        let mut last_season_abs = 0i64;
        let eps: Vec<Value> = raw_eps.into_iter().map(|(e, ep_cat_name)| {
            let eid = util::i(&e["id"]); let ekey = format!("e:{eid}");
            let raw_ep_num = util::i(&e["episode_num"]);
            // Check canonical progress key first, then any variant episode ID for this (season, raw_ep_num)
            let p = if !progress[&ekey].is_null() {
                &progress[&ekey]
            } else {
                ep_var_ids.get(&(sn, raw_ep_num))
                    .and_then(|ids| ids.iter().find_map(|&vid| {
                        let k = format!("e:{vid}");
                        (!progress[&k].is_null()).then_some(&progress[&k])
                    }))
                    .unwrap_or(&Value::Null)
            };
            let abs_ep = if sn > 0 {
                running_seq += 1;
                let a = if sequential_abs {
                    running_seq
                } else if raw_ep_num > 0 {
                    season_offset + raw_ep_num
                } else {
                    season_max + 1
                };
                if a > season_max { season_max = a; }
                if first_season_abs == 0 { first_season_abs = a; }
                last_season_abs = a;
                a
            } else { 0 };
            let display_n = if sequential_abs && abs_ep > 0 { abs_ep } else { raw_ep_num };

            // Look up matching TMDB episode metadata (abs-first for continuous/split-S1 anime, S:E-first for standard shows)
            let tm_ep = if sn > 0 {
                if split_from_tmdb_s1 || continuous_numbering {
                    tmdb_by_abs.get(&abs_ep).or_else(|| tmdb_by_se.get(&(sn, display_n)))
                } else {
                    tmdb_by_se.get(&(sn, display_n)).or_else(|| tmdb_by_abs.get(&abs_ep))
                }
            } else {
                None
            };

            let raw_title = crate::re!(r"(?i)^.*?S\d+E\d+\s*-?\s*").replace(&s(&e["title"]), "").trim().to_string();
            let is_generic_title = raw_title.is_empty()
                || crate::re!(r"(?i)^(?:episode|ep\.?)\s*\d+$").is_match(&raw_title)
                || raw_title.chars().all(|c| c.is_ascii_digit());
            let tm_title = tm_ep.map(|x| s(&x["name"])).filter(|x| !x.is_empty());
            let title = if is_generic_title {
                tm_title.unwrap_or_else(|| format!("Episode {display_n}"))
            } else {
                raw_title
            };

            let raw_plot = s(&e["info"]["plot"]);
            let plot = if !raw_plot.is_empty() {
                raw_plot
            } else {
                tm_ep.map(|x| s(&x["overview"])).unwrap_or_default()
            };

            let raw_still = s(&e["info"]["movie_image"]);
            let tm_still = tm_ep.and_then(|x| x["still_path"].as_str()).filter(|x| !x.is_empty()).map(|sp| tmdb_img(sp, "w500"));
            let still = if raw_still.is_empty() || (raw_still.contains("image.tmdb.org/t/p/w185/") && tm_still.is_some()) {
                tm_still.unwrap_or(raw_still)
            } else {
                raw_still
            };

            let raw_aired = { let a = s(&e["info"]["releasedate"]); if a.is_empty() { s(&e["info"]["air_date"]) } else { a } };
            let aired = if !raw_aired.is_empty() {
                raw_aired
            } else {
                tm_ep.map(|x| s(&x["air_date"])).unwrap_or_default()
            };

            let raw_dur = duration_of(&e["info"]);
            let ep_dur = if raw_dur > 0 {
                raw_dur
            } else {
                tm_ep.and_then(|x| x["runtime"].as_i64()).filter(|&r| r > 0).map(|r| r * 60).unwrap_or(0)
            };

            let dur = p["dur"].as_f64().unwrap_or(0.0);
            let ext = { let x = s(&e["container_extension"]); if x.is_empty() { "mkv".to_string() } else { x } };
            let dub = if is_anime { anime::ep_dub_status(&app, eid, Some(&e), if ep_cat_name.is_empty() { &cat_name } else { &ep_cat_name }) } else { "yes" };
            if is_anime { if dub == "yes" { dubbed_count += 1; } else if dub == "no" { sub_only_count += 1; } }
            let ftag = if abs_ep > 0 {
                show_filler.as_ref().map(|sf| filler::episode_tag(sf, abs_ep)).unwrap_or("")
            } else { "" };
            let (ep_uhd, ep_dv, ep_da) = ep_qual.get(&(sn, raw_ep_num)).copied().unwrap_or((false, false, false));
            json!({ "id": eid, "n": display_n, "rawN": raw_ep_num, "abs": abs_ep, "title": title,
                "plot": plot, "still": still, "duration": ep_dur, "ext": ext,
                "aired": aired, "dub": dub, "filler": ftag, "dubWatching": dub_watches.contains(&ekey),
                "uhd": ep_uhd, "dv": ep_dv, "dolbyAudio": ep_da,
                "extSub": extsubs::cached_vtt(&app, &ekey),
                "progress": if dur > 0.0 { p["pos"].as_f64().unwrap_or(0.0) / dur } else { 0.0 }, "resume": p["pos"].as_f64().unwrap_or(0.0), "isNew": new_ids.contains(&eid) })
        }).collect();
        max_abs = season_max;
        let season_name = if split_from_tmdb_s1 && first_season_abs > 0 && last_season_abs >= first_season_abs {
            format!("Season {sn} (Eps {first_season_abs}–{last_season_abs})")
        } else {
            tm_s.map(|x| s(&x["name"])).filter(|x| !x.is_empty()).unwrap_or_else(|| format!("Season {sn}"))
        };
        seasons.push(json!({ "n": sn, "name": season_name, "episodes": eps }));
    }

    // Detect if a newer TMDB season is currently airing / released that hasn't reached the English provider
    // catalog yet (e.g. Black Clover Season 2 airing Oct-Nov 2026 while provider has S1 1..170).
    let airing_season = {
        let last_air = &dd["last_episode_to_air"];
        let next_air = &dd["next_episode_to_air"];
        let last_sn = last_air["season_number"].as_i64().unwrap_or(0);
        let next_sn = next_air["season_number"].as_i64().unwrap_or(0);
        let target_sn = if next_sn > 0 { next_sn } else { last_sn };
        let missing_newer = if split_from_tmdb_s1 {
            target_sn >= 2
        } else {
            target_sn > max_prov_season && max_prov_season > 0
        };
        if missing_newer {
            let sname = tmdb_seasons_meta.iter().find(|(sn, _, _)| *sn == target_sn)
                .map(|x| x.2.clone()).filter(|x| !x.is_empty())
                .unwrap_or_else(|| format!("Season {target_sn}"));
            let aired_eps = if last_sn == target_sn { last_air["episode_number"].as_i64().unwrap_or(0) } else { 0 };
            Some(json!({
                "season": target_sn,
                "name": sname,
                "airedEps": aired_eps,
                "lastEp": if last_sn == target_sn { last_air["episode_number"].clone() } else { Value::Null },
                "lastTitle": if last_sn == target_sn { last_air["name"].clone() } else { Value::Null },
                "lastDate": if last_sn == target_sn { last_air["air_date"].clone() } else { Value::Null },
                "nextEp": if next_sn == target_sn { next_air["episode_number"].clone() } else { Value::Null },
                "nextTitle": if next_sn == target_sn { next_air["name"].clone() } else { Value::Null },
                "nextDate": if next_sn == target_sn { next_air["air_date"].clone() } else { Value::Null },
                "status": s(&dd["status"]),
            }))
        } else {
            None
        }
    };
    // next up: first unfinished episode after the most recently watched one (skipping sub-only and optional filler on anime)
    let flat: Vec<Value> = seasons.iter().flat_map(|s0| s0["episodes"].as_array().cloned().unwrap_or_default().into_iter().map(move |mut e| { e["season"] = s0["n"].clone(); e })).collect();
    let playable: Vec<Value> = if is_anime {
        flat.iter().filter(|e| e["dub"] != "no" && (!skip_filler || e["filler"] != "filler")).cloned().collect()
    } else { flat.clone() };
    let mut next_up = Value::Null; let mut last_t = 0;
    for (ix, e) in playable.iter().enumerate() {
        let p = &progress[format!("e:{}", e["id"])];
        if let Some(t) = p["t"].as_i64() { if t > last_t { last_t = t; let dur = p["dur"].as_f64().unwrap_or(1.0); next_up = if p["pos"].as_f64().unwrap_or(0.0) / dur > 0.94 { playable.get(ix + 1).cloned().unwrap_or(Value::Null) } else { e.clone() }; } }
    }
    if next_up.is_null() { next_up = playable.first().cloned().unwrap_or(Value::Null); }
    app.state.with(|st| if let Some(m) = st["newEps"].as_object_mut() { m.remove(&sid); }); app.state.save();
    let similar = match &tm { Some(t) => { let r = app.tmdb.get(format!("/tv/{t}/recommendations"), crate::tmdb::NO, TTL_D).await; rec::match_results(&app, r.as_ref().and_then(|r| r["results"].as_array()).map(|a| a.as_slice()).unwrap_or(&[]), Some("tv"), 18) } None => vec![] };
    let imdb = tm.as_ref().and_then(|t| app.meta_get("tv", t)).map(|m| m.im).unwrap_or_default();
    let mut out = app.card(&it, d.as_deref());
    out["kind"] = json!("series");
    let ov = s(&dd["overview"]); let ip = s(&i["plot"]);
    out["overview"] = json!(if !ov.is_empty() { ov } else if ip.is_empty() { it.plot.clone() } else { ip });
    if s(&out["backdrop"]).is_empty() { out["backdrop"] = json!(i["backdrop_path"].get(0).map(s).filter(|x| !x.is_empty()).unwrap_or(it.backdrop.clone())); }
    out["genres"] = json!(dd["genres"].as_array().map(|a| a.iter().map(|g| s(&g["name"])).collect::<Vec<_>>()).unwrap_or_else(|| split_list(&{ let g = s(&i["genre"]); if g.is_empty() { it.genre.clone() } else { g } })));
    out["cast"] = json!(dd["credits"]["cast"].as_array().map(|a| a.iter().take(8).map(|c| s(&c["name"])).collect::<Vec<_>>()).unwrap_or_else(|| split_list(&s(&i["cast"])).into_iter().take(8).collect()));
    out["cert"] = json!(if d.is_some() { cert_of(&dd, false) } else { String::new() });
    out["trailer"] = json!(trailer_of(d.as_deref(), &s(&i["youtube_trailer"])));
    out["status"] = json!(s(&dd["status"])); out["network"] = json!(s(&dd["networks"][0]["name"]));
    out["seasons"] = json!(seasons); out["nextUp"] = next_up; out["castIds"] = cast_ids(&dd); out["scores"] = extras::Omdb::ratings(&app, &imdb).await;
    let (watched, in_list, my, following) = app.state.read(|st| (st["watched"].get(format!("tv:{}", tm.clone().unwrap_or_default())).is_some(), st["list"].get(format!("s:{}", it.id)).is_some(), tm.as_ref().and_then(|t| st["ratings"][format!("tv:{t}")]["v"].as_i64()).unwrap_or(0), st["follows"].get(&sid).is_some()));
    out["watched"] = json!(watched); out["inList"] = json!(in_list); out["tmdb"] = json!(it.tmdb); out["imdb"] = json!(imdb); out["myRating"] = json!(my); out["following"] = json!(following); out["newCount"] = json!(new_ids.len()); out["similar"] = json!(similar);
    let m_meta = app.meta_of(&it);
    let plats = cat.item_platforms(&it);
    if !plats.is_empty() { out["platforms"] = json!(plats); }
    out["versions"] = json!(1 + it.variants.len());
    if let Some(ref mm) = m_meta {
        if mm.vc > 0 { out["votes"] = json!(mm.vc); }
        if !mm.l.is_empty() { out["lang"] = json!(mm.l); }
        if !mm.oc.is_empty() { out["countries"] = json!(mm.oc); }
    }
    let is_doc = docs::is_doc_item(&app, &it);
    if is_doc {
        out["isDoc"] = json!(true);
        out["docTopics"] = json!(docs::doc_topic_labels(&app, &it));
    }
    let best_t = showcase::best_tier(&app, &it);
    out["qualityTier"] = json!(best_t.as_u8());
    out["qualityLabel"] = json!(best_t.label());
    out["isAnime"] = json!(is_anime);
    if let Some(air) = airing_season { out["airingSeason"] = air; }
    if is_anime {
        out["dubbedCount"] = json!(dubbed_count);
        out["subOnlyCount"] = json!(sub_only_count);
        out["skipFiller"] = json!(skip_filler);
        if let Some(sf) = show_filler { out["fillerSummary"] = sf.to_summary_json(); }
    }
    j(out)
}

// ---------------------------------------------------------------- viewer state
async fn mylist(State(app): A) -> Response {
    let (list, favs) = app.state.read(|st| {
        let mut l: Vec<&Value> = st["list"].as_object().map(|m| m.values().collect()).unwrap_or_default(); l.sort_by(|a, b| b["t"].as_i64().cmp(&a["t"].as_i64()));
        (l.into_iter().map(|x| x["item"].clone()).collect::<Vec<_>>(), st["favs"].as_object().map(|m| m.values().map(|f| f["item"].clone()).collect::<Vec<_>>()).unwrap_or_default())
    });
    let favs: Vec<Value> = favs.into_iter().map(|f| { let e = s(&f["epg"]); sports::merge(f, sports::now_next(&app, &e)) }).collect();
    j(json!({ "continue": rec::continue_watching(&app).await, "list": list, "favs": favs }))
}
async fn continue_get(State(app): A) -> Response {
    let items = rec::continue_watching(&app).await;
    let dismissed: Vec<String> = app.state.read(|st| st["cwDismissed"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default());
    j(json!({ "items": items, "dismissedCount": dismissed.len() }))
}
async fn continue_remove(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]);
    if key.is_empty() { return err(400, "missing key"); }
    app.state.with(|st| {
        if !st["cwDismissed"].is_object() { st["cwDismissed"] = json!({}); }
        st["cwDismissed"][&key] = json!(now_ms());
        if let Some(m) = st["cwPinned"].as_object_mut() { m.remove(&key); }
    });
    app.state.save();
    j(json!({ "ok": true, "items": rec::continue_watching(&app).await }))
}
async fn continue_restore(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]);
    app.state.with(|st| {
        if let Some(m) = st["cwDismissed"].as_object_mut() {
            if key.is_empty() { m.clear(); } else { m.remove(&key); }
        }
    });
    app.state.save();
    j(json!({ "ok": true, "items": rec::continue_watching(&app).await }))
}
async fn continue_pin(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]);
    if key.is_empty() { return err(400, "missing key"); }
    let on = b["on"].as_bool().unwrap_or(true);
    app.state.with(|st| {
        if !st["cwPinned"].is_object() { st["cwPinned"] = json!({}); }
        if on { st["cwPinned"][&key] = json!(now_ms()); }
        else if let Some(m) = st["cwPinned"].as_object_mut() { m.remove(&key); }
    });
    app.state.save();
    j(json!({ "ok": true, "items": rec::continue_watching(&app).await }))
}
async fn continue_done(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]);
    if key.is_empty() { return err(400, "missing key"); }
    let mut item = b["item"].clone();
    if !item.is_object() { item = json!({}); }
    let dur = b["dur"].as_f64().filter(|x| *x > 0.0).unwrap_or(1.0);
    let tmdb = b["tmdb"].clone();
    app.state.with(|st| {
        let prev = st["progress"].get(&key).cloned().unwrap_or(Value::Null);
        let d = prev["dur"].as_f64().filter(|x| *x > 0.0).unwrap_or(dur);
        let it = if prev["item"].is_object() { prev["item"].clone() } else { item.clone() };
        st["progress"][&key] = json!({ "key": key, "pos": d, "dur": d, "t": now_ms(), "item": it });
        if key.starts_with("m:") {
            let tm = s(&tmdb);
            if !tm.is_empty() {
                st["watched"][format!("movie:{tm}")] = json!({ "t": now_ms(), "title": s(&it["title"]) });
            }
            if let Some(m) = st["cwPinned"].as_object_mut() { m.remove(&key); }
        }
    });
    sync::finished(&app, &key, &item, &tmdb);
    app.state.save();
    app.memo.remove_prefix("curated");
    app.memo.remove_prefix("foryou");
    j(json!({ "ok": true, "items": rec::continue_watching(&app).await }))
}
pub(crate) async fn progress(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]); if key.is_empty() { return err(400, "missing key"); }
    let d = b["dur"].as_f64().unwrap_or(0.0).max(0.0);
    let p = if d > 0.0 { b["pos"].as_f64().unwrap_or(0.0).clamp(0.0, d) } else { b["pos"].as_f64().unwrap_or(0.0).max(0.0) };
    app.state.with(|st| {
        st["progress"][&key] = json!({ "key": key, "pos": p, "dur": d, "t": now_ms(), "item": b["item"] });
        if b["historyKind"].is_string() && b["historyTitle"].is_string() {
            let mut h: Vec<Value> = st["history"].as_array().cloned().unwrap_or_default();
            h.retain(|x| !(x["kind"] == b["historyKind"] && x["tmdb"] == b["tmdb"]));
            h.insert(0, json!({ "kind": b["historyKind"], "tmdb": b["tmdb"], "title": b["historyTitle"], "t": now_ms() })); h.truncate(50);
            st["history"] = json!(h);
        }
    });
    if b["historyKind"].is_string() { app.memo.remove_prefix("foryou"); }
    if d > 0.0 && p / d > 0.9 { sync::finished(&app, &key, &b["item"], &b["tmdb"]); }
    app.state.save(); j(json!({ "ok": true }))
}
async fn list(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]); let on = b["on"].as_bool().unwrap_or(false);
    app.state.with(|st| if on { st["list"][&key] = json!({ "item": b["item"], "t": now_ms() }); } else if let Some(m) = st["list"].as_object_mut() { m.remove(&key); });
    app.state.save();
    if on { if let Some(sid) = key.strip_prefix("s:") { extras::follow(&app, sid, true, &b["item"]); } }
    j(json!({ "ok": true }))
}
async fn fav(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]);
    app.state.with(|st| if b["on"].as_bool().unwrap_or(false) { st["favs"][&key] = json!({ "item": b["item"], "t": now_ms() }); } else if let Some(m) = st["favs"].as_object_mut() { m.remove(&key); });
    app.state.save(); j(json!({ "ok": true }))
}

// ---------------------------------------------------------------- playback
pub(crate) fn upstream(app: &App, kind: &str, id: &str, ext: Option<&String>, alt: bool) -> String {
    // Test hooks: play a local HTTP source instead of the provider (never set in normal use).
    if kind == "live" { if let Ok(p) = std::env::var("TV_TEST_LIVE_SOURCE") { return p; } }
    if let Ok(p) = std::env::var("TV_TEST_SOURCE") { if kind != "live" { return p; } }
    match kind {
        "live" => app.xc.stream_url("live", id, "ts", alt),
        "movie" => { let e = ext.cloned().filter(|x| !x.is_empty()).or_else(|| id.parse().ok().and_then(|i| app.catalog().movie_raw.get(&i).map(|m| m.ext.clone()))).unwrap_or_else(|| "mp4".into()); app.xc.stream_url("movie", id, &e, alt) }
        _ => app.xc.stream_url("series", id, ext.map(|x| x.as_str()).filter(|x| !x.is_empty()).unwrap_or("mkv"), alt),
    }
}
fn resolve_series_ids(app: &App, sid: i64, q_tmdb: Option<&str>, q_imdb: Option<&str>, q_anime: bool) -> (String, String, bool) {
    let mut tmdb = q_tmdb.unwrap_or("").trim().to_string();
    let mut imdb = q_imdb.unwrap_or("").trim().to_string();
    let mut is_anime = q_anime;
    if sid > 0 {
        if let Some(it) = app.catalog().series_item(sid) {
            if tmdb.is_empty() || tmdb == "0" {
                if let Some(ref tm) = it.tmdb { tmdb = tm.clone(); }
            }
            if !is_anime && anime::is_anime_item(app, it) { is_anime = true; }
        }
    }
    if !tmdb.is_empty() && tmdb != "0" {
        if let Some(m) = app.meta_get("tv", &tmdb) {
            if imdb.is_empty() && m.im.starts_with("tt") { imdb = m.im.clone(); }
            if !is_anime && m.g.contains(&16) && (m.l == "ja" || m.oc.iter().any(|c| c == "JP")) { is_anime = true; }
        }
    }
    (tmdb, imdb, is_anime)
}

async fn skips_stats(State(app): A) -> Response {
    let mut s = app.skipdb.stats();
    let audio_eps = app.skips.count_matched();
    s["audioMatched"] = json!(audio_eps);
    j(s)
}

async fn skips_resolve(State(app): A, Query(q): Q) -> Response {
    let num = |k: &str| q.get(k).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let sid = num("sid");
    let sn = num("s");
    let en = num("n");
    let abs_ep = num("abs");
    let ep = num("ep");
    let dur = q.get("dur").and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let q_anime = q.get("anime").map_or(false, |v| v == "1" || v == "true");
    let (tmdb, imdb, is_anime) = resolve_series_ids(&app, sid, q.get("tmdb").map(|s| s.as_str()), q.get("imdb").map(|s| s.as_str()), q_anime);
    let remote = if sn >= 0 && en > 0 {
        skipdb::resolve_episode(&app, &tmdb, &imdb, sn, en, abs_ep, dur, is_anime).await
    } else { None };
    let e = if ep > 0 { app.skips.entry(ep) } else { Value::Null };
    let a_intro = e["intro"].as_array().and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
    let a_credits = e["credits"].as_f64();
    let m = skipdb::merge_all(&[], None, a_intro, a_credits, remote.as_ref(), dur);
    j(json!({
        "tmdb": tmdb,
        "imdb": imdb,
        "isAnime": is_anime,
        "skips": m.skips,
        "intro": m.intro.map(|(s, e)| json!([s, e])),
        "recap": m.recap.map(|(s, e)| json!([s, e])),
        "credits": m.credits,
        "creditsEnd": m.credits_end,
        "postCredits": m.post_credits.map(|(s, e)| json!([s, e])),
        "hasPostCredits": m.has_post_credits,
        "source": m.source,
    }))
}

async fn probe(State(app): A, Path((kind, id)): Path<(String, String)>, Query(q): Q) -> Response {
    let url = upstream(&app, &kind, &id, q.get("ext"), false);
    yield_background(&app).await;
    let num = |k: &str| q.get(k).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let (sid, sn, en, abs_ep) = (num("sid"), num("s"), num("n"), num("abs"));
    let q_anime = q.get("anime").map_or(false, |v| v == "1" || v == "true");
    let (tmdb, imdb, is_anime) = if kind == "series" {
        resolve_series_ids(&app, sid, q.get("tmdb").map(|s| s.as_str()), q.get("imdb").map(|s| s.as_str()), q_anime)
    } else {
        (String::new(), String::new(), false)
    };

    // Run public skipdb lookup (IntroDB + TheIntroDB v2 + AniSkip) IN PARALLEL with stream probe
    // so resolving crowd-sourced intro/recap/credits adds 0ms to playback startup!
    let probe_fut = async {
        if kind == "movie" || kind == "series" {
            if !spool::attached(&url) && !app.stream.is_active() { let since = now_ms() - app.stream.last_stop(); if since < 1500 { tokio::time::sleep(std::time::Duration::from_millis((1500 - since) as u64)).await; } }
            if !app.stream.is_active() || spool::attached(&url) { spool::attach(&url, &app.cfg.str("userAgent")).await; }
        }
        match app.stream.probe(&url, &app.cfg.str("userAgent")).await { Some(mut p) => { p["subs"] = stream::pick_subs(&p["subs"]); p } None => json!({}) }
    };
    let skip_fut = async {
        if kind == "series" && sn >= 0 && en > 0 {
            skipdb::resolve_episode(&app, &tmdb, &imdb, sn, en, abs_ep, 0.0, is_anime).await
        } else {
            None
        }
    };
    let (mut p, remote_skips) = tokio::join!(probe_fut, skip_fut);

    if let (Ok(nid), Some(aud)) = (id.parse::<i64>(), p["audio"].as_array()) {
        let has_en = anime::record_probe(&app, &kind, nid, aud);
        p["hasEnAudio"] = json!(has_en);
    }
    if kind == "series" {
        let dur = p["duration"].as_f64().unwrap_or(0.0);
        let ep_id = id.parse::<i64>().unwrap_or(0);
        if ep_id > 0 && sid > 0 { app.skips.register(ep_id, sid, sn, en, dur); }
        let e = if ep_id > 0 { app.skips.entry(ep_id) } else { Value::Null };
        let ch_skips = p["skips"].as_array().cloned().unwrap_or_default();
        let ch_credits = p["chapterCredits"].as_f64();
        let a_intro = e["intro"].as_array().and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
        let a_credits = e["credits"].as_f64();
        let merged = skipdb::merge_all(&ch_skips, ch_credits, a_intro, a_credits, remote_skips.as_ref(), dur);
        p["skips"] = Value::Array(merged.skips);
        p["intro"] = merged.intro.map(|(s, e)| json!([s, e])).unwrap_or(Value::Null);
        p["recap"] = merged.recap.map(|(s, e)| json!([s, e])).unwrap_or(Value::Null);
        p["credits"] = merged.credits.map(|c| json!(c)).unwrap_or(Value::Null);
        p["creditsEnd"] = merged.credits_end.map(|c| json!(c)).unwrap_or(Value::Null);
        p["postCredits"] = merged.post_credits.map(|(s, e)| json!([s, e])).unwrap_or(Value::Null);
        p["hasPostCredits"] = json!(merged.has_post_credits);
        if !merged.source.is_empty() { p["skipSource"] = json!(merged.source); }
        if sn >= 0 && en > 0 {
            skipdb::spawn_prefetch_ahead(app.clone(), tmdb, imdb, sn, en, abs_ep, is_anime);
        }
    }
    j(p)
}
struct Guard { app: Arc<App>, seq: u64 }
impl Drop for Guard { fn drop(&mut self) { self.app.stream.finished(self.seq); } }
pub(crate) async fn stream_route(State(app): A, Path((kind, id)): Path<(String, String)>, Query(q): Q, headers: axum::http::HeaderMap) -> Response {
    // The browser silently retries a dropped stream with "Range: bytes=N-". Our output is generated from the
    // URL's start time, so honouring that would re-read the whole film from there: refuse it, and the player
    // resumes at the exact time instead (t=current position).
    if let Some(start) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split('-').next()).and_then(|n| n.parse::<u64>().ok()) {
        if start > 0 { println!("[stream] refused mid-stream range retry ({start} bytes in)"); return (StatusCode::RANGE_NOT_SATISFIABLE, "resume with ?t=").into_response(); }
    }
    let ts = q.get("fmt").map_or(false, |x| x == "ts");
    // catch-up: a past programme via the provider's timeshift (minute resolution; played like live TV)
    let catchup = kind == "catchup";
    let url = if catchup && std::env::var("TV_TEST_SOURCE").is_ok() { std::env::var("TV_TEST_SOURCE").unwrap() } else if catchup {
        let n = |k: &str| q.get(k).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
        let skip_min = (n("t") / 60.0).floor() as i64;
        guide::timeshift_url(&app, id.parse().unwrap_or(0), n("cs") as i64 + skip_min * 60_000, n("dur") as i64 - skip_min)
    } else { upstream(&app, &kind, &id, q.get("ext"), q.get("alt").map_or(false, |x| x == "1")) };
    let ua = app.cfg.str("userAgent"); let live = kind == "live" || catchup;
    let is_live_ch = kind == "live";
    let mse = q.get("mse").map_or(false, |x| x == "1");
    let num = |k: &str| q.get(k).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    // The provider takes a moment to release the single connection after a stop: don't race it.
    // newest request wins: an older one still waiting below must not start ffmpeg over it
    let gen = app.stream_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let page = q.get("pg").cloned().unwrap_or_default();
    app.stream.stop("replaced");
    if mpv::running(&app) { let _ = mpv::stop(State(app.clone())).await; }
    yield_background(&app).await;
    // films/episodes play through the read-ahead spool; live channels play through the live TS ring buffer
    let spooled_before = (!live && spool::attached(&url)) || (is_live_ch && livebuf::attached(&url));
    if live { spool::detach(); }
    if !is_live_ch { livebuf::detach(); }
    if !spooled_before {
        let last_end = app.stream.last_stop().max(livebuf::last_close());
        let since = now_ms() - last_end;
        if since < 1500 { tokio::time::sleep(std::time::Duration::from_millis((1500 - since) as u64)).await; }
    }
    if is_live_ch {
        if let Ok(cid) = id.parse::<i64>() { record_recent_live(&app, cid, ""); }
    }
    let live_local = if is_live_ch && !ts { livebuf::attach(&url, &ua, num("back").max(0.0)).await } else { None };
    let spooled = !live && !ts_cast_only(&q) && spool::attach(&url, &ua).await.is_some();
    let mut info = if live { None } else { app.stream.probe(&url, &ua).await };
    // the provider sometimes still holds the previous connection (e.g. straight after an episode ends): retry once
    if !live && info.is_none() { tokio::time::sleep(std::time::Duration::from_millis(2500)).await; info = app.stream.probe(&url, &ua).await; }
    // a fresh direct probe just used (and closed) the connection: give the provider its release time again
    if !spooled && live_local.is_none() {
        let since = now_ms() - app.stream.last_stop();
        if since < 1500 { tokio::time::sleep(std::time::Duration::from_millis((1500 - since) as u64)).await; }
    }
    if let Some(d) = info.as_ref().and_then(|i| i["duration"].as_f64()) { spool::set_duration(&url, d); }
    if app.stream_gen.load(std::sync::atomic::Ordering::SeqCst) != gen { return err(409, "superseded"); }
    *app.stream.owner.lock().unwrap() = page;
    let night = q.get("nd").map_or(false, |x| x == "1");
    let deint = q.get("di").map_or(false, |x| x == "1");
    let Some((seq, out)) = app.stream.start(&url, &ua, &format!("{kind}:{id}"), live, if catchup { 0.0 } else { num("t") }, num("a") as i64, q.get("tx").map_or(false, |x| x == "1"), info.as_ref(), num("ad") as i64, ts, q.get("sc").map_or(false, |x| x == "1"), live_local.as_deref(), night, deint) else { return err(500, "ffmpeg failed to start") };
    let guard = Guard { app: app.clone(), seq };
    if kind == "series" { if let (Ok(ep), Some(sid)) = (id.parse::<i64>(), q.get("sid").and_then(|x| x.parse::<i64>().ok())) {
        let (a2, start, dur) = (app.clone(), num("t"), info.as_ref().and_then(|i| i["duration"].as_f64()).unwrap_or(0.0));
        app.stream.on_stop(seq, Box::new(move |pcm| { let a3 = a2.clone(); std::thread::spawn(move || intro::from_session(&a3.skips, &pcm, ep, sid, start, dur)); }));
    } }
    let body = if live || ts {
        let a3 = app.clone();
        Body::from_stream(end_checked(app.clone(), seq, futures_util::StreamExt::map(tokio_util::io::ReaderStream::with_capacity(out, 256 * 1024), move |r| { let _g = &guard; if let Ok(b) = &r { a3.stream.count(b.len()); } r })))
    } else {
        // VOD: re-align audio/video start times on the way out (see fmp4.rs)
        let a2 = app.clone();
        let rx = fmp4::rewrite(out, move || a2.stream.starts(), mse);
        let a3 = app.clone();
        Body::from_stream(end_checked(app.clone(), seq, futures_util::StreamExt::map(tokio_stream_rx(rx), move |r| { let _g = &guard; if let Ok(b) = &r { a3.stream.count(b.len()); } r })))
    };
    Response::builder().header(header::CONTENT_TYPE, if ts { "video/mpeg" } else { "video/mp4" }).header(header::CACHE_CONTROL, "no-store").header("transferMode.dlna.org", "Streaming").body(body).unwrap()
}
/// Bytes sent for the current session (the player's stats overlay turns this into a bitrate).
async fn stream_stats(State(app): A) -> Response { livebuf::touch(); j(json!({ "bytes": app.stream.sent(), "active": app.stream.is_active(), "spool": spool::stats(), "livebuf": livebuf::stats() })) }
/// Casting to a TV uses the same path (TS output) — it benefits from the cache too.
fn ts_cast_only(_q: &HashMap<String, String>) -> bool { false }
/// Stop: only this page's own session (another app window may be the one playing).
async fn stop(State(app): A, body: Option<Json<Value>>) -> Response {
    let pg = body.as_ref().and_then(|b| b["pg"].as_str()).unwrap_or("").to_string();
    let owner = app.stream.owner.lock().unwrap().clone();
    if pg.is_empty() || owner.is_empty() || owner == pg { app.stream.stop("user"); spool::detach(); livebuf::detach(); }
    j(json!({ "ok": true }))
}
/// After the stream's last byte: if the session was cut or ffmpeg failed, end the response with an
/// error so the player resumes, instead of a clean end that looks like "film finished".
fn end_checked<S>(app: Arc<App>, seq: u64, s: S) -> impl futures_util::Stream<Item = std::io::Result<bytes::Bytes>>
where S: futures_util::Stream<Item = std::io::Result<bytes::Bytes>> {
    use futures_util::StreamExt;
    let tail = futures_util::stream::once(async move {
        if app.stream.ended_ok(seq).await { None } else { Some(Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "stream interrupted"))) }
    }).filter_map(|x| async move { x });
    s.chain(tail)
}
/// Background jobs (intro clips, quality probes) give the single connection back immediately.
pub(crate) async fn yield_background(app: &App) {
    use std::sync::atomic::Ordering::SeqCst;
    if app.skips.busy.load(SeqCst) {
        app.skips.cancel.store(true, SeqCst);
        for _ in 0..40 { if !app.skips.busy.load(SeqCst) { break; } tokio::time::sleep(std::time::Duration::from_millis(100)).await; }
        app.stream.mark_stop();
    }
}

fn tokio_stream_rx<T>(mut rx: tokio::sync::mpsc::Receiver<T>) -> impl futures_util::Stream<Item = T> {
    futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

// ---------------------------------------------------------------- intro/credits: idle-time fingerprinting
/// While nothing is playing and the account is idle, grab short audio clips (head/tail) of the
/// episodes around where the viewer is in each show they're watching, so intros and credits are
/// known before they get there. Yields instantly if playback starts (see stream_route).
async fn intro_worker(app: &Arc<App>) {
    use std::sync::atomic::Ordering::SeqCst;
    let idle = |app: &Arc<App>| !app.stream.is_active() && !mpv::running(app) && !livebuf::is_active() && now_ms() - app.stream.last_stop() > 120_000;
    if !idle(app) { return; }
    quality_worker(app).await;
    if !idle(app) { return; }
    anime::dub_worker(app).await;
    if !idle(app) { return; }
    // recently watched shows: (series id, last watched episode id), newest first
    let mut shows: Vec<(i64, i64, i64)> = app.state.read(|st| st["progress"].as_object().map(|m| m.iter().filter_map(|(k, v)| {
        let ep: i64 = k.strip_prefix("e:")?.parse().ok()?; let sid = v["item"]["sid"].as_i64()?; Some((sid, ep, v["t"].as_i64().unwrap_or(0)))
    }).collect()).unwrap_or_default());
    shows.sort_by_key(|x| -x.2);
    let mut seen = std::collections::HashSet::new(); shows.retain(|x| now_ms() - x.2 < 30 * 86_400_000 && seen.insert(x.0)); shows.truncate(3);
    let ua = app.cfg.str("userAgent"); let mut grabs = 0;
    for (sid, last_ep, _) in shows {
        let info = app.xc_info("get_series_info", "series_id", sid, 2 * 3_600_000).await;
        let mut flat: Vec<(i64, i64, i64, String, f64)> = vec![]; // season, n, id, ext, dur
        if let Some(m) = info["episodes"].as_object() { for (sk, list) in m { let sn: i64 = sk.parse().unwrap_or(0); for e in list.as_array().into_iter().flatten() {
            let ext = { let x = s(&e["container_extension"]); if x.is_empty() { "mkv".to_string() } else { x } };
            flat.push((sn, util::i(&e["episode_num"]), util::i(&e["id"]), ext, duration_of(&e["info"]) as f64));
        } } }
        flat.sort_by_key(|x| (x.0, x.1));
        let Some(ix) = flat.iter().position(|x| x.2 == last_ep) else { continue };
        // the episode being watched, the next two, and one before (a reference for matching)
        let targets: Vec<(usize, (i64, i64, i64, String, f64))> = flat.iter().enumerate()
            .filter(|(i, _)| *i + 1 >= ix && *i <= ix + 2)
            .map(|(i, x)| (i + 1, x.clone()))
            .collect();
        let (tmdb, imdb, is_anime) = resolve_series_ids(app, sid, None, None, false);
        let mut changed = false;
        for (abs_idx, (sn, n, ep, ext, dur)) in targets {
            app.skips.register(ep, sid, sn, n, dur);
            // First check free crowd-sourced skip DBs (IntroDB / TheIntroDB / AniSkip — zero provider load!).
            // Only audio-fingerprint segments still missing after crowd-sourced lookup!
            let remote = skipdb::resolve_episode(app, &tmdb, &imdb, sn, n, abs_idx as i64, dur, is_anime).await;
            let have_remote_intro = remote.as_ref().map_or(false, |r| r.intro.is_some());
            let have_remote_credits = remote.as_ref().map_or(false, |r| r.credits.is_some());
            for which in ["h", "t"] {
                if (which == "h" && have_remote_intro) || (which == "t" && have_remote_credits) { continue; }
                if app.skips.has(ep, which) || (which == "t" && dur < 600.0) || grabs >= 6 { continue; }
                if !idle(app) { return; }
                match app.account(true).await { Ok(a) if a.active == 0 => {} _ => return }
                let url = app.xc.stream_url("series", &ep.to_string(), &ext, false);
                let (from, len) = if which == "h" { (0.0, intro::HEAD_SECS) } else { (dur - intro::TAIL_SECS, intro::TAIL_SECS) };
                app.skips.cancel.store(false, SeqCst); app.skips.busy.store(true, SeqCst);
                let pcm = intro::grab(&app.skips, &url, &ua, from, len).await;
                app.skips.busy.store(false, SeqCst); app.stream.mark_stop(); grabs += 1;
                let Some(pcm) = pcm else { return };
                let a = app.clone(); let fp = tokio::task::spawn_blocking(move || { let _ = &a; intro::fingerprint(&pcm) }).await.unwrap_or_default();
                app.skips.put(ep, which, &fp, from); changed = true;
                println!("[intro] fingerprinted S{sn}E{n} ({which}) of series {sid}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
        if changed { let a = app.clone(); let _ = tokio::task::spawn_blocking(move || a.skips.analyze(sid)).await; }
    }
}

// ---------------------------------------------------------------- source health (best version)
/// Penalty for a stream id from what actually happened when it played here: failures to start,
/// stalls per hour watched and slow starts. 0 = untested or flawless.
fn source_health_inner(h: &Value, now: i64) -> f64 {
    if h.is_null() { return 0.0; }
    let f = h["f"].as_f64().unwrap_or(0.0);
    let ok = h["s"].as_f64().unwrap_or(0.0);
    let stalls = h["st"].as_f64().unwrap_or(0.0);
    let hrs = (h["w"].as_f64().unwrap_or(0.0) / 3600.0).max(0.25);
    let ms = h["ms"].as_f64().unwrap_or(0.0);
    // Time decay: full weight for 48h, linear decay to 0 over 7 days (legacy entries without `t` decay by 50%).
    let age_ms = match h["t"].as_i64() {
        Some(ts) if ts > 0 => now.saturating_sub(ts),
        _ => 72 * 3_600_000,
    };
    let decay = if age_ms <= 48 * 3_600_000 {
        1.0
    } else if age_ms >= 168 * 3_600_000 {
        0.0
    } else {
        1.0 - (age_ms - 48 * 3_600_000) as f64 / (120 * 3_600_000) as f64
    };
    if decay <= 0.0 { return 0.0; }
    // A single isolated transient failure (e.g. VPN hiccup / 460 slot busy) shouldn't permanently
    // downgrade a 4K DV copy below 1080p: cap single-blip fail score at 2.5 (penalty -7.5 vs +10+ 4K lead).
    let fail_score = if f <= 1.0 && ok == 0.0 && stalls == 0.0 {
        f * 2.5
    } else {
        (f / (f + ok).max(1.0)) * 10.0
    };
    let raw = fail_score + (stalls / hrs).min(10.0) + (ms / 10_000.0).min(2.0);
    raw * decay
}
pub(crate) fn source_health(app: &App, id: i64) -> f64 {
    let now = now_ms();
    app.state.read(|st| source_health_inner(&st["health"][id.to_string()], now))
}
/// Picture score from what a stream really is (probed), or its category/tag quality tier when not probed yet:
/// resolution first, then HDR/Dolby Vision, Blu-Ray/Dolby Audio/Platform HD, then bitrate & 5.1 audio.
fn quality_score(v: &Value, pro: bool, hdr_pref: bool) -> f64 {
    let q = &v["q"];
    let cat = v["cat"].as_str().unwrap_or("").to_uppercase();
    let is_uhd = v["uhd"].as_bool().unwrap_or(false);
    let is_dv = v["dv"].as_bool().unwrap_or(false) || (cat.contains("DOLBY") && cat.contains("VISION"));
    let is_hdr = !is_dv && (v["hdr"].as_bool().unwrap_or(false) || crate::re!(r"\bHDR(?:10\+?)?\b").is_match(&cat));
    let is_da = v["dolbyAudio"].as_bool().unwrap_or(false) || (cat.contains("DOLBY") && !cat.contains("VISION"));
    let is_br = v["bluray"].as_bool().unwrap_or(false) || cat.contains("BLURAY") || cat.contains("BLU-RAY") || cat.contains("REMUX");
    let is_sub_mkt = v["subMarket"].as_bool().unwrap_or(false);
    let h = q["h"].as_f64().map(|h| h.max(q["w"].as_f64().unwrap_or(0.0) * 9.0 / 16.0)).unwrap_or(if is_uhd { 2160.0 } else { 1080.0 });
    let tier = if h >= 2000.0 { 3.0 } else if h >= 1000.0 { 2.0 } else if h >= 700.0 { 1.0 } else { 0.0 };
    // HDR in the browser can look greyer than a good SDR copy on an OLED: only prefer HDR where it's shown
    // properly (pro player installed, or the viewer asked for it); otherwise an SDR copy of the same size wins.
    let hdr = match q["hdr"].as_str().unwrap_or("") {
        "" => 0.0,
        "DV" if !pro => -4.0,
        "DV" => 1.35, // Dolby Vision > HDR10 > SDR when pro player (mpv) is installed
        _ if pro || hdr_pref => 1.0,
        _ => -0.5,
    };
    let probed = !q["h"].is_null();
    let cat_boost = if !probed {
        let mut b = 0.0;
        if is_br { b += 1.8; }
        if is_dv { b += if pro { 2.5 } else { -1.5 }; }
        else if is_hdr { b += if pro || hdr_pref { 1.8 } else { -0.8 }; }
        else if is_da { b += 1.5; }
        if cat.contains("HEVC") { b += 0.4; }
        // Platform/Studio WEB-DL categories are reliably high-bitrate 1080p/4K 5.1 compared to generic genre folders.
        let is_platform = ["NETFLIX", "APPLE+", "DISNEY+", "HBO", "AMAZON", "PARAMOUNT", "SHOWTIME", "PEACOCK", "SKY", "MARVEL", "OSN+", "UNIVERSAL", "DREAMWORKS"]
            .iter().any(|p| cat.contains(p));
        if is_platform { b += 1.0; }
        b
    } else {
        0.0
    };
    // Native EN slightly outranks cross-market subtitle-only 4K when both are in the exact same tier
    let native_bonus = if !is_sub_mkt { 0.6 } else { 0.0 };
    let codec_bonus = if probed && h >= 1000.0 && matches!(q["codec"].as_str().unwrap_or(""), "hevc" | "av1") { 0.4 } else { 0.0 };
    let audio_bonus = if q["ch"].as_i64().unwrap_or(0) >= 6 { 1.2 } else { 0.0 };
    tier * 10.0 + hdr * 3.0 + cat_boost + native_bonus + codec_bonus + audio_bonus + (q["kbps"].as_f64().unwrap_or(0.0) / 8_000.0).min(5.0)
}
/// Best first: healthiest, then best picture (default; gigabit) or HD first if the viewer turned 4K off.
fn rank_sources(app: &App, v: &mut Vec<Value>) {
    let pref4k = app.state.read(|st| st["prefs"]["prefer4k"].as_bool().unwrap_or(true));
    let pro = mpv::find(app).is_some(); let hdr_pref = app.state.read(|st| st["prefs"]["preferHdr"].as_bool().unwrap_or(false));
    rank_sources_with(v, pref4k, pro, hdr_pref);
}
fn rank_sources_with(v: &mut Vec<Value>, pref4k: bool, pro: bool, hdr_pref: bool) {
    for x in v.iter_mut() { let qs = quality_score(x, pro, hdr_pref); x["qs"] = json!(qs); let q = x["q"].clone(); if let Some(lab) = quality_label(&q) { x["label"] = json!(lab); } }
    // one score: picture quality minus a penalty for real trouble (failures, frequent stalls, slow
    // starts). CAM rips & confirmed non-English cross-market copies drop to the bottom when clean copies exist.
    let score = |x: &Value| {
        if x["cam"].as_bool().unwrap_or(false) { return -5000.0; }
        if x["nonEn"].as_bool().unwrap_or(false) { return -4000.0; }
        let q = x["qs"].as_f64().unwrap_or(0.0);
        let h = x["health"].as_f64().unwrap_or(0.0);
        (if pref4k { q } else { -q }) - 3.0 * h
    };
    v.sort_by(|a, b| score(b).total_cmp(&score(a)));
}
/// "4K HDR10 · 5.1" for a probed stream.
fn quality_label(q: &Value) -> Option<String> {
    let h = q["h"].as_i64()?; let w = q["w"].as_i64().unwrap_or(0);
    let res = if h >= 2000 || w >= 3600 { "4K" } else if h >= 1000 || w >= 1900 { "1080p" } else if h >= 700 || w >= 1200 { "720p" } else { "SD" };
    let mut s = res.to_string();
    match q["hdr"].as_str().unwrap_or("") { "" => {} "DV" => s.push_str(" Dolby Vision"), x => { s.push(' '); s.push_str(x); } }
    if q["dv"].is_i64() && q["hdr"] != "DV" { s.push_str("+DV"); }
    if q["ch"].as_i64().unwrap_or(0) >= 6 { s.push_str(" · 5.1"); }
    Some(s)
}

/// Idle time (#7 Instant-Start Quality Pre-Prober): quietly pre-probes Continue Watching, My List,
/// Wanted, top 4K/DV Showcase films, and newest UFC PPV replays so playback starts instantly and
/// real verified 4K/HDR10/Dolby Vision bitrates outrank fake upscales. Yields immediately on any play.
async fn quality_worker(app: &Arc<App>) {
    if std::env::var("TV_TEST_SOURCE").map_or(false, |v| !v.is_empty()) { return; }
    let cat = app.catalog();
    let (cw_m_ids, list_m_ids, s_ids): (Vec<i64>, Vec<i64>, Vec<i64>) = app.state.read(|st| {
        let mut cw_mv: Vec<i64> = vec![];
        let mut mv: Vec<i64> = vec![];
        let mut sv: Vec<i64> = vec![];
        for (idx, k) in ["progress", "list", "wanted", "follows"].iter().enumerate() {
            if let Some(m) = st[*k].as_object() {
                for (mk, x) in m {
                    if *k == "follows" {
                        if let Ok(sid) = mk.parse::<i64>() { if sid > 0 && !sv.contains(&sid) { sv.push(sid); } }
                        continue;
                    }
                    let it = &x["item"];
                    let kind = it["k"].as_str().unwrap_or("");
                    if kind == "s" { let id = util::i(&it["id"]); if id > 0 && !sv.contains(&id) { sv.push(id); } }
                    else if kind == "e" { let sid = util::i(&it["sid"]); if sid > 0 && !sv.contains(&sid) { sv.push(sid); } }
                    else {
                        let id = util::i(&it["id"]);
                        if id > 0 {
                            if idx == 0 { if !cw_mv.contains(&id) { cw_mv.push(id); } }
                            else if !mv.contains(&id) { mv.push(id); }
                        }
                    }
                }
            }
        }
        (cw_mv, mv, sv)
    });
    let mut todo: Vec<(&'static str, i64, String)> = vec![];
    let push_todo = |todo: &mut Vec<(&'static str, i64, String)>, kind: &'static str, vid: i64, ext: String| {
        if vid > 0 && app.stream.quality_of(vid).is_none() && !todo.iter().any(|x| x.1 == vid) {
            todo.push((kind, vid, ext));
        }
    };
    // Priority 1: Continue Watching movies (even single-variant for instant resume!) + multi-variant My List / Wanted
    for id in cw_m_ids {
        let Some(it) = cat.movie(id) else { continue };
        for vid in std::iter::once(it.id).chain(it.variants.iter().map(|v| v.id)) {
            let e = cat.movie_raw.get(&vid).map(|m| m.ext.clone()).unwrap_or_else(|| "mp4".into());
            push_todo(&mut todo, "movie", vid, e);
        }
    }
    for id in list_m_ids {
        let Some(it) = cat.movie(id) else { continue };
        for vid in std::iter::once(it.id).chain(it.variants.iter().map(|v| v.id)) {
            let e = cat.movie_raw.get(&vid).map(|m| m.ext.clone()).unwrap_or_else(|| "mp4".into());
            push_todo(&mut todo, "movie", vid, e);
        }
    }
    // Priority 2: Watched/listed/followed series (probe current/next episode across variants so 4K DV vs HD is known!)
    let prog_se: HashMap<i64, (i64, i64)> = app.state.read(|st| {
        let mut m: HashMap<i64, (i64, i64, i64)> = HashMap::new();
        for x in st["progress"].as_object().into_iter().flatten().map(|(_, v)| v) {
            let sid = util::i(&x["item"]["sid"]);
            if sid <= 0 { continue; }
            let sub = x["item"]["sub"].as_str().unwrap_or("");
            if let Some(c) = crate::re!(r"(?i)S(\d+):E(\d+)").captures(sub) {
                let (sn, en) = (c[1].parse::<i64>().unwrap_or(1), c[2].parse::<i64>().unwrap_or(1));
                let ts = x["t"].as_i64().unwrap_or(0);
                if m.get(&sid).map_or(true, |prev| ts > prev.2) { m.insert(sid, (sn, en, ts)); }
            }
        }
        m.into_iter().map(|(k, (sn, en, _))| (k, (sn, en))).collect()
    });
    for sid in s_ids.into_iter().take(4) {
        if todo.len() >= 6 { break; }
        let Some(it) = cat.series_item(sid).cloned() else { continue };
        let target_se = prog_se.get(&it.id).copied();
        for vsid in std::iter::once(it.id).chain(it.variants.iter().filter(|v| !v.cam).map(|v| v.id)).take(4) {
            let info = app.xc_info("get_series_info", "series_id", vsid, 6 * 3_600_000).await;
            let picked = target_se.and_then(|(sn, en)| {
                info["episodes"][sn.to_string()].as_array()
                    .and_then(|a| a.iter().find(|e| util::i(&e["episode_num"]) == en))
            }).or_else(|| {
                info["episodes"].as_object().and_then(|m| m.values().next()).and_then(|a| a.as_array()).and_then(|a| a.first())
            });
            if let Some(ep) = picked {
                let eid = util::i(&ep["id"]);
                let ext = { let x = s(&ep["container_extension"]); if x.is_empty() { "mkv".to_string() } else { x } };
                push_todo(&mut todo, "series", eid, ext);
            }
        }
    }
    // Priority 3: Top unprobed 4K Dolby Vision / 4K UHD Showcase movies (verify real bitrate & HDR/DV!)
    if todo.len() < 10 {
        let mut top_4k: Vec<(f64, i64, String)> = cat.movies.iter()
            .filter(|m| m.en && !m.sport && !m.cam && (m.dv || m.uhd || m.variants.iter().any(|v| !v.cam && (v.dv || v.uhd))))
            .filter_map(|m| {
                let best_vid = if m.dv { m.id } else if let Some(v) = m.variants.iter().find(|v| !v.cam && v.dv) { v.id }
                    else if m.uhd { m.id } else if let Some(v) = m.variants.iter().find(|v| !v.cam && v.uhd) { v.id } else { m.id };
                if app.stream.quality_of(best_vid).is_some() { return None; }
                let pop = app.meta_of(m).map(|mm| mm.p + mm.va * 8.0).unwrap_or(m.rating * 5.0);
                let ext = cat.movie_raw.get(&best_vid).map(|x| x.ext.clone()).unwrap_or_else(|| "mp4".into());
                Some((pop, best_vid, ext))
            })
            .collect();
        top_4k.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, vid, ext) in top_4k.into_iter().take(6) {
            push_todo(&mut todo, "movie", vid, ext);
            if todo.len() >= 10 { break; }
        }
    }
    // Priority 4: Latest 3 unprobed UFC PPV replays so opening a fight is instant!
    if todo.len() < 10 {
        let fight_list = fights::collect_fights(&cat, None);
        for f in fight_list.into_iter().filter(|f| f.entry.sport_key == "ufc" && f.entry.event_kind == "ppv").take(3) {
            push_todo(&mut todo, "movie", f.id, f.ext);
            if todo.len() >= 10 { break; }
        }
    }
    let ua = app.cfg.str("userAgent");
    for (kind, vid, ext) in todo.into_iter().take(10) {
        if app.stream.is_active() || mpv::running(app) || livebuf::is_active() || now_ms() - app.stream.last_stop() < 120_000 { return; }
        match app.account(true).await { Ok(a) if a.active == 0 => {} _ => return }
        let url = app.xc.stream_url(kind, &vid.to_string(), &ext, false);
        use std::sync::atomic::Ordering::SeqCst;
        app.skips.cancel.store(false, SeqCst); app.skips.busy.store(true, SeqCst);
        let got = tokio::select! {
            r = app.stream.probe(&url, &ua) => {
                if let Some(p) = r {
                    if let Some(aud) = p["audio"].as_array() { anime::record_probe(app, kind, vid, aud); }
                    true
                } else { false }
            }
            _ = async { while !app.skips.cancel.load(SeqCst) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => false,
        };
        app.skips.busy.store(false, SeqCst); app.stream.mark_stop();
        if got { println!("[quality] pre-probed {kind} {vid}"); }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    }
}
pub(crate) fn record_mpv_health(app: &App, id: i64, watched_secs: f64, count_start: bool) {
    if id <= 0 { return; }
    app.state.with(|st| {
        if st["health"].is_null() { st["health"] = json!({}); }
        let h = &mut st["health"][id.to_string()];
        if h.is_null() { *h = json!({ "s": 0, "f": 0, "st": 0, "w": 0, "ms": 0 }); }
        if count_start {
            h["s"] = json!(h["s"].as_f64().unwrap_or(0.0) + 1.0);
        }
        if watched_secs > 0.0 {
            h["w"] = json!(h["w"].as_f64().unwrap_or(0.0) + watched_secs.clamp(0.0, 4.0 * 3600.0));
        }
        h["t"] = json!(now_ms());
    });
    app.state.save();
}
async fn health(State(app): A, Json(b): Json<Value>) -> Response {
    let id = b["id"].as_i64().unwrap_or(0); if id == 0 { return err(400, "id"); }
    let ev = b["ev"].as_str().unwrap_or("");
    if (ev == "fail" || ev == "stall") && app.stream.was_cut(id, 20_000) { return j(json!({ "ok": true, "ignored": "stopped by us" })); }
    app.state.with(|st| {
        if st["health"].is_null() { st["health"] = json!({}); }
        let h = &mut st["health"][id.to_string()]; if h.is_null() { *h = json!({ "s": 0, "f": 0, "st": 0, "w": 0, "ms": 0 }); }
        let add = |h: &mut Value, k: &str, x: f64| { h[k] = json!(h[k].as_f64().unwrap_or(0.0) + x); };
        match b["ev"].as_str().unwrap_or("") {
            "start" => { let n = h["s"].as_f64().unwrap_or(0.0); let ms = b["ms"].as_f64().unwrap_or(0.0); h["ms"] = json!(((h["ms"].as_f64().unwrap_or(0.0) * n + ms) / (n + 1.0)).round()); add(h, "s", 1.0); }
            "fail" => add(h, "f", 1.0), "stall" => add(h, "st", 1.0),
            "watched" => { add(h, "w", b["secs"].as_f64().unwrap_or(0.0).clamp(0.0, 4.0 * 3600.0));
                // short buffering pauses and dropped frames (stutter that never reaches a 2.5 s stall)
                add(h, "hc", b["hitches"].as_f64().unwrap_or(0.0).clamp(0.0, 10_000.0)); add(h, "df", b["dropped"].as_f64().unwrap_or(0.0).max(0.0)); add(h, "fr", b["frames"].as_f64().unwrap_or(0.0).max(0.0)); }
            _ => {}
        }
        h["t"] = json!(now_ms());
    });
    app.state.save(); j(json!({ "ok": true }))
}
/// The viewer chose a version by hand: remember it for the title (movie: stream id; show: its category id & name).
async fn pick(State(app): A, Json(b): Json<Value>) -> Response {
    let key = s(&b["key"]); if key.is_empty() { return err(400, "key"); }
    let pid = b["id"].as_i64().unwrap_or(0);
    let cat_id = s(&b["catId"]);
    let cat_name = s(&b["cat"]);
    if pid <= 0 && cat_id.is_empty() && cat_name.is_empty() {
        return j(json!({ "ok": true, "ignored": "empty pick" }));
    }
    app.state.with(|st| {
        if st["picks"].is_null() { st["picks"] = json!({}); }
        st["picks"][&key] = json!({
            "id": if pid > 0 { json!(pid) } else { Value::Null },
            "catId": if !cat_id.is_empty() { json!(cat_id) } else { Value::Null },
            "cat": if !cat_name.is_empty() { json!(cat_name) } else { Value::Null },
            "t": now_ms(),
        });
    });
    app.state.save(); j(json!({ "ok": true }))
}
/// Put the viewer's own pick first (unless it keeps failing or is a bad/non-EN copy).
fn apply_pick_val(p: &Value, v: &mut Vec<Value>) {
    if p.is_null() { return; }
    let pid = p["id"].as_i64().filter(|&x| x > 0);
    let p_cid = p["catId"].as_str().filter(|s| !s.is_empty());
    let p_cat = p["cat"].as_str().filter(|s| !s.is_empty());
    let idx = p_cid.and_then(|cid| v.iter().position(|x| x["catId"].as_str() == Some(cid)))
        .or_else(|| pid.and_then(|id| v.iter().position(|x| x["id"].as_i64() == Some(id))))
        .or_else(|| p_cat.and_then(|c| v.iter().position(|x| x["cat"].as_str().map_or(false, |xc| xc == c || xc.starts_with(&format!("{c} 4K"))))))
        .or_else(|| p_cat.and_then(|c| v.iter().position(|x| x["cat"].as_str() == Some(c))));
    let Some(i) = idx else { return };
    if v[i]["health"].as_f64().unwrap_or(0.0) >= 5.0 || v[i]["nonEn"].as_bool().unwrap_or(false) { return; }
    let x = v.remove(i); v.insert(0, x);
}
fn apply_pick(app: &App, key: &str, v: &mut Vec<Value>) {
    let p = app.state.read(|st| st["picks"][key].clone());
    apply_pick_val(&p, v);
}
/// The same episode in the show's other versions (other categories / 4K DV / Dolby Audio / clean sub-market 4K), best first.
async fn ep_alts(State(app): A, Path((sid, season, n)): Path<(i64, i64, i64)>) -> Response {
    let cat = app.catalog(); let Some(it) = cat.series_item(sid).cloned() else { return j(json!([])) };
    let mut specs = vec![(it.id, it.uhd, it.dv, it.bluray, it.dolby_audio, it.cam, false, it.cat.clone())];
    for v in &it.variants {
        let cid = cat.series_raw.get(&v.id).map(|x| x.cat.clone()).unwrap_or_default();
        specs.push((v.id, v.uhd, v.dv, v.bluray, v.dolby_audio, v.cam, v.sub_market, cid));
    }
    let infos = futures_util::future::join_all(specs.iter().map(|(id, ..)| app.xc_info("get_series_info", "series_id", *id, 2 * 3_600_000))).await;
    let mut cnt: HashMap<String, usize> = HashMap::new(); let mut out = vec![];
    for ((_, uhd, dv, bluray, da, cam, sub_mkt, cid), info) in specs.iter().zip(infos) {
        let Some(e) = info["episodes"][season.to_string()].as_array().and_then(|a| a.iter().find(|e| util::i(&e["episode_num"]) == n)) else { continue };
        let eid = util::i(&e["id"]);
        if eid <= 0 || out.iter().any(|x: &Value| x["id"].as_i64() == Some(eid)) { continue; }
        let ext = { let x = s(&e["container_extension"]); if x.is_empty() { "mkv".to_string() } else { x } };
        let cname = cat.cat_name("series", cid);
        let base = if *cam { "CAM" }
            else if *dv { "4K DV" }
            else if *uhd && *da { "4K · Dolby Audio" }
            else if *uhd { "4K" }
            else if *bluray { "Blu-Ray" }
            else if *da { "HD · Dolby Audio" }
            else { "HD" };
        let base_s = if *sub_mkt { format!("{base} · Nordic") } else { base.to_string() };
        let k = cnt.entry(base_s.clone()).or_insert(0); *k += 1;
        let label = if *k > 1 { format!("{base_s} {k}") } else { base_s };
        let non_en = *sub_mkt && app.dubs.get(&format!("e:{eid}")).map_or(false, |d| !d.has_en);
        out.push(json!({
            "id": eid, "ext": ext, "uhd": uhd, "dv": dv, "bluray": bluray, "dolbyAudio": da, "cam": cam,
            "subMarket": sub_mkt, "nonEn": non_en, "catId": cid, "cat": cname,
            "label": label, "health": source_health(&app, eid), "q": app.stream.quality_of(eid),
        }));
    }
    rank_sources(&app, &mut out); apply_pick(&app, &format!("s:{sid}"), &mut out); j(json!(out))
}

/// Extra guide for channels the provider's guide misses: download, match channels, reload if anything changed.
async fn extra_guide(app: Arc<App>) {
    let fresh = epgx::refresh(&app).await;
    let a = app.clone();
    let aliases = tokio::task::spawn_blocking(move || epgx::compute(&a)).await.unwrap_or_default();
    let changed = epgx::save_aliases(&app.data, &aliases);
    println!("[epgx] {} channels matched to the extra guide{}", aliases.len(), if changed { " (updated)" } else { "" });
    if changed {
        let a = app.clone(); let c = tokio::task::spawn_blocking(move || catalog::Catalog::load(&a.data)).await.unwrap();
        *app.cat.write().unwrap() = Arc::new(c);
    }
    if changed || fresh { let a = app.clone(); let _ = tokio::task::spawn_blocking(move || sports::load_epg(&a)).await; app.memo.clear(); }
}

#[cfg(test)]
mod quality_tests {
    use super::*;

    #[test]
    fn clean_cat_folds_superscripts_without_collisions() {
        assert_eq!(catalog::clean_cat("NETFLIX SERIES ⁴ᴷ ³⁸⁴⁰ᴾ ᴰᵒˡᵇʸ ⱽᶦˢᶦᵒⁿ"), "NETFLIX SERIES 4K 3840P Dolby Vision");
        assert_eq!(catalog::clean_cat("ENGLISH SERIES ᴰᴼᴸᴮʸ ᴬᵁᴰᴵᴼ"), "ENGLISH SERIES DOLBY AUDIO");
        assert_ne!(catalog::clean_cat("HBO MAX SERIES ⁴ᴷ ³⁸⁴⁰ᴾ"), catalog::clean_cat("HBO MAX SERIES"));
        assert_eq!(catalog::clean_cat("4K| ᵁᴴᴰ ³⁸⁴⁰ᴾ"), "4K| UHD 3840P");
        assert_eq!(catalog::clean_cat("AU| PRIME ᴿᴬᵂ ⁶⁰ᶠᵖˢ"), "AU| PRIME RAW 60fps");
    }

    #[test]
    fn rank_sources_prefers_4k_dv_dolby_platform_and_tolerates_single_blip() {
        let now = now_ms();
        let single_blip = source_health_inner(&json!({ "f": 1, "s": 0, "st": 0, "w": 0, "ms": 0, "t": now }), now);
        assert!((single_blip - 2.5).abs() < 0.01, "single blip capped at 2.5, got {single_blip}");
        let old_fail = source_health_inner(&json!({ "f": 5, "s": 0, "st": 0, "w": 0, "ms": 0, "t": now - 8 * 86_400_000 }), now);
        assert_eq!(old_fail, 0.0, "8-day-old failure decays to 0");

        // Even with 1 transient failure, 4K DV still beats plain 1080p HD
        let mut v = vec![
            json!({ "id": 1, "uhd": false, "dv": false, "dolbyAudio": false, "cam": false, "subMarket": false, "catId": "156", "cat": "ENGLISH SERIES", "health": 0.0, "q": Value::Null }),
            json!({ "id": 2, "uhd": false, "dv": false, "dolbyAudio": false, "cam": false, "subMarket": false, "catId": "138", "cat": "NETFLIX SERIES", "health": 0.0, "q": Value::Null }),
            json!({ "id": 3, "uhd": false, "dv": false, "dolbyAudio": true,  "cam": false, "subMarket": false, "catId": "1898", "cat": "ENGLISH SERIES DOLBY AUDIO", "health": 0.0, "q": Value::Null }),
            json!({ "id": 4, "uhd": true,  "dv": false, "dolbyAudio": true,  "cam": false, "subMarket": true,  "catId": "1264", "cat": "NORDIC SERIES 4K 3840P DOLBY AUDIO", "health": 0.0, "q": Value::Null }),
            json!({ "id": 5, "uhd": true,  "dv": true,  "dolbyAudio": false, "cam": false, "subMarket": false, "catId": "427", "cat": "NETFLIX SERIES 4K 3840P Dolby Vision", "health": single_blip, "q": Value::Null }),
            json!({ "id": 6, "uhd": true,  "dv": false, "dolbyAudio": false, "cam": false, "subMarket": true,  "nonEn": true, "catId": "999", "cat": "FOREIGN 4K", "health": 0.0, "q": Value::Null }),
        ];
        rank_sources_with(&mut v, true, true, false);
        let ids: Vec<i64> = v.iter().map(|x| x["id"].as_i64().unwrap()).collect();
        // Nordic 4K Dolby Audio (31.5) > 4K DV with single blip (34.1 - 7.5 = 26.6) > HD Dolby Audio (22.1) > Platform HD (21.6) > Generic HD (20.6) > nonEn (-4000)
        assert_eq!(ids, vec![4, 5, 3, 2, 1, 6]);
        // Without a blip, Native EN 4K DV beats Nordic 4K Dolby Audio
        v.iter_mut().find(|x| x["id"] == 5).unwrap()["health"] = json!(0.0);
        rank_sources_with(&mut v, true, true, false);
        assert_eq!(v[0]["id"].as_i64(), Some(5));
        assert_eq!(v[1]["id"].as_i64(), Some(4));
    }

    #[test]
    fn apply_pick_matches_exact_cat_id_and_ignores_non_en() {
        let mut v = vec![
            json!({ "id": 10, "catId": "1851", "cat": "HBO MAX SERIES 4K 3840P", "health": 0.0, "nonEn": false }),
            json!({ "id": 20, "catId": "1852", "cat": "HBO MAX SERIES", "health": 0.0, "nonEn": false }),
        ];
        apply_pick_val(&json!({ "catId": "1852", "cat": "HBO MAX SERIES" }), &mut v);
        assert_eq!(v[0]["id"].as_i64(), Some(20));
    }

    #[test]
    fn movie_ranking_prefers_4k_dv_over_4k_hdr_over_4k_dolby_over_bluray_over_hd_over_cam() {
        let mut v = vec![
            json!({ "id": 1, "uhd": false, "hdr": false, "dv": false, "bluray": false, "dolbyAudio": false, "cam": true,  "subMarket": false, "cat": "EN - NEW RELEASE", "health": 0.0, "q": Value::Null }),
            json!({ "id": 2, "uhd": false, "hdr": false, "dv": false, "bluray": false, "dolbyAudio": false, "cam": false, "subMarket": false, "cat": "EN - ACTION", "health": 0.0, "q": Value::Null }),
            json!({ "id": 3, "uhd": false, "hdr": false, "dv": false, "bluray": true,  "dolbyAudio": false, "cam": false, "subMarket": false, "cat": "TOP MOVIES BLURAY (MULTI-SUBS)", "health": 0.0, "q": Value::Null }),
            json!({ "id": 4, "uhd": true,  "hdr": false, "dv": false, "bluray": false, "dolbyAudio": true,  "cam": false, "subMarket": true,  "cat": "NORDIC FILM 4K 3840P DOLBY AUDIO", "health": 0.0, "q": Value::Null }),
            json!({ "id": 5, "uhd": true,  "hdr": true,  "dv": false, "bluray": false, "dolbyAudio": false, "cam": false, "subMarket": true,  "cat": "NORDIC 4K HDR", "health": 0.0, "q": Value::Null }),
            json!({ "id": 6, "uhd": true,  "hdr": true,  "dv": true,  "bluray": false, "dolbyAudio": false, "cam": false, "subMarket": false, "cat": "APPLE+ MOVIES 4K 3840P Dolby Vision", "health": 0.0, "q": Value::Null }),
        ];
        rank_sources_with(&mut v, true, true, false);
        let ids: Vec<i64> = v.iter().map(|x| x["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![6, 5, 4, 3, 2, 1]);
    }
}
