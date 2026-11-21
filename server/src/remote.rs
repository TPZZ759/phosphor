// Phone remote. Off by default. When enabled, a second, separate listener on the home network
// (port 7880) serves ONLY the remote page and its small API; the main app stays on localhost.
// A phone pairs once with a 6-digit code shown on the PC and then holds a random token.
// Commands go to the TV page over a server-sent-events stream; the TV page reports what's playing.
use crate::app::App;
use axum::{extract::{Query, State}, http::{header, HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const PORT: u16 = 7880;

pub struct Remote {
    pub tx: tokio::sync::broadcast::Sender<String>,
    code: Mutex<Option<(String, i64)>>,
    attempts: Mutex<Vec<i64>>,
    pub tv_state: Mutex<(i64, Value)>,
    pending: Mutex<Option<(i64, String)>>,
    started: AtomicBool,
}

fn now() -> i64 { crate::util::now_ms() }

impl Remote {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        Remote { tx, code: Mutex::new(None), attempts: Mutex::new(vec![]), tv_state: Mutex::new((0, Value::Null)), pending: Mutex::new(None), started: AtomicBool::new(false) }
    }
    pub fn new_code(&self) -> String {
        use rand::Rng; let c = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000));
        *self.code.lock().unwrap() = Some((c.clone(), now() + 10 * 60_000)); c
    }
    pub fn code(&self) -> Option<String> { self.code.lock().unwrap().as_ref().filter(|c| c.1 > now()).map(|c| c.0.clone()) }
    /// Send a command to the TV page; if no TV page is open, open the app and deliver it on connect.
    pub fn send(&self, cmd: String) {
        if self.tx.receiver_count() == 0 {
            *self.pending.lock().unwrap() = Some((now(), cmd));
            open_app();
        } else { let _ = self.tx.send(cmd); }
    }
    pub fn take_pending(&self) -> Option<String> { self.pending.lock().unwrap().take().filter(|p| now() - p.0 < 30_000).map(|p| p.1) }
}

fn open_app() {
    let root = std::env::current_dir().unwrap_or_default();
    let vbs = root.join("Open Phosphor.vbs");
    let vbs = if vbs.exists() { vbs } else { root.join("Open TV.vbs") };
    if vbs.exists() { let _ = std::process::Command::new("wscript").arg(vbs).spawn(); }
}

pub fn enabled(app: &App) -> bool { app.state.read(|st| st["remote"]["enabled"].as_bool().unwrap_or(false)) }
fn tokens(app: &App) -> Vec<String> { app.state.read(|st| st["remote"]["tokens"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()) }

/// This PC's address on the home network (no packets are sent: UDP connect only picks a route).
pub fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?; s.connect("192.168.0.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip(); if ip.is_loopback() || ip.is_unspecified() { None } else { Some(ip.to_string()) }
}
pub fn url() -> Option<String> { lan_ip().map(|ip| format!("http://{ip}:{PORT}")) }
pub fn qr_svg(text: &str) -> String {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(c) => c.render::<qrcode::render::svg::Color>().min_dimensions(180, 180).dark_color(qrcode::render::svg::Color("#000")).light_color(qrcode::render::svg::Color("#fff")).build(),
        Err(_) => String::new(),
    }
}

/// Start the LAN listener once (it rejects everything while the remote is switched off).
pub fn ensure_listener(app: Arc<App>) {
    if app.remote.started.swap(true, Ordering::SeqCst) { return; }
    tokio::spawn(async move {
        let r = Router::new()
            .route("/", get(page)).route("/r/pair", post(pair)).route("/r/state", get(state)).route("/r/cmd", post(cmd))
            .route("/r/home", get(home)).route("/c/{tok}/{kind}/{file}", get(crate::cast::lan_stream)).route("/r/search", get(search)).route("/r/play", post(play))
            .with_state(app.clone());
        // PHOSPHOR_REMOTE_HOST=127.0.0.1 (or TV_REMOTE_HOST) keeps test instances off the network
        let host = std::env::var("PHOSPHOR_REMOTE_HOST").or_else(|_| std::env::var("TV_REMOTE_HOST")).unwrap_or_else(|_| "0.0.0.0".into());
        match tokio::net::TcpListener::bind((host.as_str(), PORT)).await {
            Ok(l) => { println!("[remote] listening on {host}:{PORT}"); let _ = axum::serve(l, r).await; }
            Err(e) => { println!("[remote] can't listen on {PORT}: {e}"); app.remote.started.store(false, Ordering::SeqCst); }
        }
    });
}

type A = State<Arc<App>>;
fn deny(code: StatusCode, msg: &str) -> Response { (code, Json(json!({ "error": msg }))).into_response() }
fn authed(app: &App, h: &HeaderMap) -> Result<(), Response> {
    if !enabled(app) { return Err(deny(StatusCode::FORBIDDEN, "The phone remote is switched off on the PC.")); }
    let t = h.get("x-remote").and_then(|v| v.to_str().ok()).unwrap_or("");
    if t.len() >= 32 && tokens(app).iter().any(|x| x == t) { Ok(()) } else { Err(deny(StatusCode::UNAUTHORIZED, "pair")) }
}

async fn page(State(app): A) -> Response {
    if !enabled(&app) { return (StatusCode::FORBIDDEN, "The phone remote is switched off on the PC.").into_response(); }
    match std::fs::read_to_string(app.public.join("remote.html")) {
        Ok(h) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], h).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "missing").into_response(),
    }
}
async fn pair(State(app): A, Json(b): Json<Value>) -> Response {
    if !enabled(&app) { return deny(StatusCode::FORBIDDEN, "The phone remote is switched off on the PC."); }
    { let mut at = app.remote.attempts.lock().unwrap(); at.retain(|t| now() - t < 600_000); if at.len() >= 10 { return deny(StatusCode::TOO_MANY_REQUESTS, "Too many tries. Wait a few minutes."); } at.push(now()); }
    let given = b["code"].as_str().unwrap_or("").trim().to_string();
    match app.remote.code() { Some(c) if !given.is_empty() && c == given => {}, _ => return deny(StatusCode::UNAUTHORIZED, "That code didn't match. Check the code on the PC (Settings → Phone remote).") }
    use rand::Rng; let tok: String = (0..32).map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16u8))).collect();
    app.state.with(|st| { if st["remote"].is_null() { st["remote"] = json!({}); } let mut t = st["remote"]["tokens"].as_array().cloned().unwrap_or_default(); t.push(json!(tok.clone())); if t.len() > 8 { t.remove(0); } st["remote"]["tokens"] = json!(t); });
    app.state.save(); app.remote.new_code();
    let _ = app.remote.tx.send(json!({ "cmd": "paired" }).to_string());
    Json(json!({ "token": tok })).into_response()
}
async fn state(State(app): A, h: HeaderMap) -> Response {
    if let Err(r) = authed(&app, &h) { return r; }
    if crate::mpv::running(&app) {
        if let Some(s) = app.mpv.s.lock().unwrap().as_ref().map(|s| json!({
            "open": true, "playing": true, "pro": true, "title": s.title, "live": s.live, "pos": s.pos, "dur": s.dur
        })) {
            return Json(json!({ "tv": true, "fresh": true, "s": s })).into_response();
        }
    }
    let (t, s) = app.remote.tv_state.lock().unwrap().clone();
    Json(json!({ "tv": app.remote.tx.receiver_count() > 0, "fresh": now() - t < 6000, "s": s })).into_response()
}
const CMDS: &[&str] = &["toggle", "seek", "seekto", "next", "subs", "vol", "mute", "full", "close", "key", "skip", "audio", "source", "tvmode", "last"];
async fn cmd(State(app): A, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if let Err(r) = authed(&app, &h) { return r; }
    let c = b["cmd"].as_str().unwrap_or("");
    if !CMDS.contains(&c) { return deny(StatusCode::BAD_REQUEST, "unknown command"); }
    if crate::mpv::running(&app) && ["toggle", "seek", "seekto", "vol", "mute", "subs", "audio", "full", "skip", "close"].contains(&c) {
        let a = app.clone();
        let cs = c.to_string();
        let v = b.get("v").cloned();
        let _ = tokio::task::spawn_blocking(move || crate::mpv::control_sync(&a, &cs, v.as_ref())).await;
        return Json(json!({ "ok": true, "pro": true })).into_response();
    }
    let _ = app.remote.tx.send(json!({ "cmd": c, "v": b["v"] }).to_string());
    Json(json!({ "ok": true })).into_response()
}
async fn home(State(app): A, h: HeaderMap) -> Response {
    if let Err(r) = authed(&app, &h) { return r; }
    // deduplicated continue watching + my list + favourite live channels
    let cw = crate::rec::continue_watching(&app).await;
    let cont: Vec<Value> = cw.into_iter().take(15).map(|it| json!({
        "k": it["k"], "id": it["id"], "sid": it["sid"], "title": it["title"], "sub": it["sub"],
        "img": if it["backdrop"].is_string() && !it["backdrop"].as_str().unwrap_or("").is_empty() { it["backdrop"].clone() } else { it["poster"].clone() },
        "pct": it["progress"].as_f64().unwrap_or(0.0)
    })).collect();
    let (list, favs_raw) = app.state.read(|st| (st["list"].clone(), st["favs"].clone()));
    let mine: Vec<Value> = list.as_object().map(|m| m.values().take(30).map(|v| { let it = if v["item"].is_object() { &v["item"] } else { v }; json!({ "k": it["k"], "id": it["id"], "title": it["title"], "img": it["poster"] }) }).collect()).unwrap_or_default();
    let favs: Vec<Value> = favs_raw.as_object().map(|m| m.values().take(24).map(|v| {
        let it = &v["item"];
        let nn = crate::sports::now_next(&app, it["epg"].as_str().unwrap_or(""));
        json!({ "k": "l", "id": it["id"], "title": it["title"], "sub": nn["now"]["title"].as_str().unwrap_or(""), "img": it["logo"].as_str().filter(|s| !s.is_empty()).or(it["poster"].as_str()).unwrap_or("") })
    }).collect()).unwrap_or_default();
    Json(json!({ "cont": cont, "list": mine, "favs": favs })).into_response()
}
async fn search(State(app): A, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if let Err(r) = authed(&app, &h) { return r; }
    let cat = app.catalog(); let (m, s, l) = cat.search(q.get("q").map(|x| x.as_str()).unwrap_or(""), false, 12);
    let f = |x: &crate::catalog::Item| json!({ "k": x.k, "id": x.id, "title": x.title, "year": x.year, "img": x.poster });
    let fl = |x: &crate::catalog::Item| {
        let nn = crate::sports::now_next(&app, &x.epg);
        json!({ "k": "l", "id": x.id, "title": x.title, "sub": nn["now"]["title"].as_str().unwrap_or(""), "img": x.poster })
    };
    Json(json!({ "movies": m.into_iter().map(f).collect::<Vec<_>>(), "series": s.into_iter().map(f).collect::<Vec<_>>(), "live": l.into_iter().map(fl).collect::<Vec<_>>() })).into_response()
}
async fn play(State(app): A, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if let Err(r) = authed(&app, &h) { return r; }
    let k = b["k"].as_str().unwrap_or(""); if !["m", "s", "e", "l"].contains(&k) { return deny(StatusCode::BAD_REQUEST, "kind"); }
    let mut payload = json!({ "cmd": "play", "k": k, "id": b["id"], "sid": b["sid"] });
    if k == "l" {
        if let Some(id) = b["id"].as_i64().or_else(|| b["id"].as_str().and_then(|x| x.parse().ok())) {
            if let Some(ch) = app.catalog().live_item(id) {
                payload["title"] = json!(ch.title);
                payload["epg"] = json!(ch.epg);
                payload["logo"] = json!(ch.poster);
            }
        }
    }
    app.remote.send(payload.to_string());
    Json(json!({ "ok": true })).into_response()
}

// ---- localhost side (TV page + settings)
pub async fn events(State(app): A) -> axum::response::sse::Sse<impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let rx = app.remote.tx.subscribe();
    let first = app.remote.take_pending();
    let hello = json!({ "cmd": "hello", "enabled": enabled(&app) }).to_string();
    let init: Vec<String> = std::iter::once(hello).chain(first).collect();
    let s = futures_util::stream::unfold((init, rx), |(mut init, mut rx)| async move {
        if !init.is_empty() { let m = init.remove(0); return Some((Ok(Event::default().data(m)), (init, rx))); }
        loop { match rx.recv().await { Ok(m) => return Some((Ok(Event::default().data(m)), (init, rx))), Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue, Err(_) => return None } }
    });
    Sse::new(s).keep_alive(KeepAlive::default())
}
pub async fn tv_state(State(app): A, Json(b): Json<Value>) -> Response { *app.remote.tv_state.lock().unwrap() = (now(), b); Json(json!({ "ok": true })).into_response() }
pub async fn info(State(app): A) -> Response {
    let on = enabled(&app); let u = url();
    if on && app.remote.code().is_none() { app.remote.new_code(); }
    Json(json!({ "enabled": on, "url": u, "code": if on { app.remote.code() } else { None }, "qr": if on { u.as_deref().map(qr_svg) } else { None }, "paired": tokens(&app).len() })).into_response()
}
pub async fn set(State(app): A, Json(b): Json<Value>) -> Response {
    if let Some(on) = b["enabled"].as_bool() {
        app.state.with(|st| { if st["remote"].is_null() { st["remote"] = json!({}); } st["remote"]["enabled"] = json!(on); }); app.state.save();
        if on { ensure_listener(app.clone()); app.remote.new_code(); }
    }
    if b["forget"].as_bool() == Some(true) { app.state.with(|st| st["remote"]["tokens"] = json!([])); app.state.save(); }
    info(State(app)).await
}

/// Localhost only (main API): send a command straight to the TV page (automation / tests). Returns how many pages listen.
pub async fn send_local(State(app): A, Json(b): Json<Value>) -> Response {
    let n = app.remote.tx.receiver_count();
    let _ = app.remote.tx.send(b.to_string());
    Json(json!({ "ok": true, "listeners": n })).into_response()
}
