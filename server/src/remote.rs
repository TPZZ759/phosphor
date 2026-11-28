// Phone remote. Off by default. When enabled, a second, separate listener on the home network
// (port 7880) serves ONLY the remote page and its small API; the main app stays on localhost.
// A phone pairs once with a 6-digit code shown on the PC and then holds a random token.
// Commands go to the TV page (or directly to mpv) and live playback state streams back to the
// phone over `/r/events` SSE (plus `/r/state` fallback).
use crate::app::App;
use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const PORT: u16 = 7880;
const PAIR_WINDOW_MS: i64 = 600_000;
const PAIR_MAX_PER_IP: usize = 10;
const MAX_PAIRED_DEVICES: usize = 8;

pub struct Remote {
    pub tx: tokio::sync::broadcast::Sender<String>,
    pub phone_tx: tokio::sync::broadcast::Sender<String>,
    code: Mutex<Option<(String, i64)>>,
    attempts: Mutex<HashMap<IpAddr, Vec<i64>>>,
    pub tv_state: Mutex<(i64, Value)>,
    pending: Mutex<Option<(i64, String)>>,
    started: AtomicBool,
}

#[derive(Clone, Debug)]
struct PairedDevice {
    id: String,
    token: String,
    label: String,
    paired_at: i64,
    last_seen: i64,
}

fn now() -> i64 {
    crate::util::now_ms()
}

impl Remote {
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let (phone_tx, _) = tokio::sync::broadcast::channel(64);
        Remote {
            tx,
            phone_tx,
            code: Mutex::new(None),
            attempts: Mutex::new(HashMap::new()),
            tv_state: Mutex::new((0, Value::Null)),
            pending: Mutex::new(None),
            started: AtomicBool::new(false),
        }
    }
    pub fn new_code(&self) -> String {
        use rand::Rng;
        let c = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000));
        *self.code.lock().unwrap() = Some((c.clone(), now() + 10 * 60_000));
        c
    }
    pub fn code(&self) -> Option<String> {
        self.code
            .lock()
            .unwrap()
            .as_ref()
            .filter(|c| c.1 > now())
            .map(|c| c.0.clone())
    }
    /// Send a command to the TV page; if no TV page is open, open the app and deliver it on connect.
    pub fn send(&self, data_dir: &FsPath, cmd: String) {
        if self.tx.receiver_count() == 0 {
            *self.pending.lock().unwrap() = Some((now(), cmd));
            open_app(Some(data_dir));
        } else {
            let _ = self.tx.send(cmd);
        }
    }
    pub fn take_pending(&self) -> Option<String> {
        self.pending
            .lock()
            .unwrap()
            .take()
            .filter(|p| now() - p.0 < 30_000)
            .map(|p| p.1)
    }
    fn check_and_record_attempt(&self, ip: IpAddr, now_ms: i64) -> bool {
        let mut map = self.attempts.lock().unwrap();
        // Prune stale attempts across all IPs so the table stays bounded.
        map.retain(|_, list| {
            list.retain(|t| now_ms - *t < PAIR_WINDOW_MS);
            !list.is_empty()
        });
        let entry = map.entry(ip).or_default();
        if entry.len() >= PAIR_MAX_PER_IP {
            return false;
        }
        entry.push(now_ms);
        true
    }
    fn clear_attempts(&self, ip: IpAddr) {
        self.attempts.lock().unwrap().remove(&ip);
    }
}

fn open_app(data_dir: Option<&FsPath>) {
    let mut candidates = Vec::new();
    if let Some(root) = data_dir.and_then(|d| d.parent()) {
        candidates.push(root.join("Open TV.vbs"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("Open TV.vbs"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("Open TV.vbs"));
        }
    }
    if let Some(vbs) = candidates.into_iter().find(|p| p.is_file()) {
        let _ = std::process::Command::new("wscript").arg(vbs).spawn();
    }
}

pub fn enabled(app: &App) -> bool {
    app.state
        .read(|st| st["remote"]["enabled"].as_bool().unwrap_or(false))
}

fn parse_device(v: &Value) -> Option<PairedDevice> {
    if let Some(tok) = v.as_str() {
        if tok.len() >= 32 {
            return Some(PairedDevice {
                id: tok.chars().take(8).collect(),
                token: tok.to_string(),
                label: "Paired device".into(),
                paired_at: 0,
                last_seen: 0,
            });
        }
        return None;
    }
    let tok = v["token"].as_str()?;
    if tok.len() < 32 {
        return None;
    }
    let id = v["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| tok.chars().take(8).collect());
    let label = v["label"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "Paired device".into());
    Some(PairedDevice {
        id,
        token: tok.to_string(),
        label,
        paired_at: v["pairedAt"].as_i64().unwrap_or(0),
        last_seen: v["lastSeen"].as_i64().unwrap_or(0),
    })
}

fn device_to_value(d: &PairedDevice) -> Value {
    json!({
        "id": d.id,
        "token": d.token,
        "label": d.label,
        "pairedAt": d.paired_at,
        "lastSeen": d.last_seen,
    })
}

fn paired_devices(app: &App) -> Vec<PairedDevice> {
    app.state.read(|st| {
        st["remote"]["tokens"]
            .as_array()
            .map(|arr| arr.iter().filter_map(parse_device).collect())
            .unwrap_or_default()
    })
}

fn sanitize_label(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(48)
        .collect();
    cleaned.trim().to_string()
}

fn derive_label(ua: &str) -> String {
    let low = ua.to_ascii_lowercase();
    let dev = if low.contains("iphone") {
        "iPhone"
    } else if low.contains("ipad") {
        "iPad"
    } else if low.contains("pixel") {
        "Pixel"
    } else if low.contains("sm-") || low.contains("samsung") || low.contains("galaxy") {
        "Galaxy"
    } else if low.contains("android") {
        "Android"
    } else if low.contains("macintosh") || low.contains("mac os") {
        "Mac"
    } else if low.contains("windows") {
        "Windows PC"
    } else if low.contains("linux") {
        "Linux"
    } else {
        "Phone"
    };
    let browser = if low.contains("crios") || (low.contains("chrome") && !low.contains("edg")) {
        Some("Chrome")
    } else if low.contains("fxios") || low.contains("firefox") {
        Some("Firefox")
    } else if low.contains("edg") {
        Some("Edge")
    } else if low.contains("safari") {
        Some("Safari")
    } else {
        None
    };
    match browser {
        Some(b) => format!("{dev} ({b})"),
        None => dev.to_string(),
    }
}

fn touch_device_last_seen(app: &App, tok: &str) {
    let ts = now();
    let mut dirty = false;
    app.state.with(|st| {
        let Some(arr) = st["remote"]["tokens"].as_array_mut() else {
            return;
        };
        for entry in arr.iter_mut() {
            let Some(mut d) = parse_device(entry) else {
                continue;
            };
            if d.token == tok {
                if entry.is_string() || ts - d.last_seen >= 60_000 {
                    d.last_seen = ts;
                    if d.paired_at == 0 {
                        d.paired_at = ts;
                    }
                    *entry = device_to_value(&d);
                    dirty = true;
                }
                break;
            }
        }
    });
    if dirty {
        app.state.save();
    }
}

fn revoke_device(app: &App, id: &str) -> bool {
    let target = id.trim();
    if target.is_empty() {
        return false;
    }
    let mut removed = false;
    app.state.with(|st| {
        let Some(arr) = st["remote"]["tokens"].as_array_mut() else {
            return;
        };
        let before = arr.len();
        arr.retain(|entry| parse_device(entry).map_or(false, |d| d.id != target));
        removed = arr.len() != before;
    });
    if removed {
        app.state.save();
    }
    removed
}

fn rename_device(app: &App, id: &str, label: &str) -> bool {
    let target = id.trim();
    let clean = sanitize_label(label);
    if target.is_empty() || clean.is_empty() {
        return false;
    }
    let mut updated = false;
    app.state.with(|st| {
        let Some(arr) = st["remote"]["tokens"].as_array_mut() else {
            return;
        };
        for entry in arr.iter_mut() {
            let Some(mut d) = parse_device(entry) else {
                continue;
            };
            if d.id == target {
                d.label = clean.clone();
                *entry = device_to_value(&d);
                updated = true;
                break;
            }
        }
    });
    if updated {
        app.state.save();
    }
    updated
}

fn is_rfc1918(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

/// Reject loopback, link-local, CGNAT/Tailscale (100.64.0.0/10), and Mullvad/WireGuard tunnel
/// subnets (10.64.0.0..=10.127.255.255) so LAN discovery and phone pairing never advertise a VPN IP.
pub(crate) fn is_vpn_or_virtual_ip(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
    {
        return true;
    }
    // RFC 6598 Carrier-Grade NAT / Tailscale (100.64.0.0/10)
    if o[0] == 100 && (o[1] & 0xc0) == 64 {
        return true;
    }
    // Mullvad & common WireGuard private overlay range (10.64.0.0/10)
    if o[0] == 10 && (64..=127).contains(&o[1]) {
        return true;
    }
    false
}

fn is_vpn_or_virtual_adapter(name_lower: &str) -> bool {
    const BAD: &[&str] = &[
        "mullvad",
        "wireguard",
        "wintun",
        "openvpn",
        "tap-windows",
        "tap ",
        "tun ",
        "nordlynx",
        "proton",
        "surfshark",
        "expressvpn",
        "tailscale",
        "zerotier",
        "hamachi",
        "radmin",
        "vethernet",
        "wsl",
        "hyper-v",
        "vmware",
        "virtualbox",
        "vbox",
        "docker",
        "bluetooth",
        "loopback",
        "teredo",
        "isatap",
        "pseudo",
    ];
    BAD.iter().any(|k| name_lower.contains(k))
}

fn extract_ipv4(text: &str) -> Option<Ipv4Addr> {
    for tok in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if tok.chars().filter(|&c| c == '.').count() == 3 {
            if let Ok(ip) = tok.parse::<Ipv4Addr>() {
                return Some(ip);
            }
        }
    }
    None
}

/// Parse Windows `ipconfig` output and pick the highest-priority physical RFC1918 LAN IPv4,
/// skipping VPN/WireGuard/Mullvad/WSL/Hyper-V/virtual adapters.
pub(crate) fn parse_ipconfig_lan_ip(output: &str) -> Option<String> {
    let mut best: Option<(i32, Ipv4Addr)> = None;

    let mut cur_name = String::new();
    let mut disconnected = false;
    let mut cur_ip: Option<Ipv4Addr> = None;
    let mut cur_gw: Option<Ipv4Addr> = None;
    let mut in_gw_section = false;

    let flush = |name: &str,
                 disconnected: bool,
                 ip: Option<Ipv4Addr>,
                 gw: Option<Ipv4Addr>,
                 best: &mut Option<(i32, Ipv4Addr)>| {
        if disconnected || name.is_empty() {
            return;
        }
        let low = name.to_ascii_lowercase();
        if is_vpn_or_virtual_adapter(&low) {
            return;
        }
        let Some(v4) = ip else { return };
        if is_vpn_or_virtual_ip(v4) {
            return;
        }
        let o = v4.octets();
        let mut score = 0i32;
        if is_rfc1918(v4) {
            score += 100;
        }
        if let Some(g) = gw {
            if !is_vpn_or_virtual_ip(g) {
                score += 60;
            }
        }
        if o[0] == 192 && o[1] == 168 {
            score += 30;
        } else if o[0] == 172 && (16..=31).contains(&o[1]) {
            score += 20;
        } else if o[0] == 10 {
            score += 10;
        }
        if low.contains("ethernet")
            || low.contains("wi-fi")
            || low.contains("wifi")
            || low.contains("wireless")
            || low.contains("wlan")
            || low.contains("local area connection")
        {
            score += 15;
        }
        if best.map_or(true, |(bs, _)| score > bs) {
            *best = Some((score, v4));
        }
    };

    for raw in output.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            in_gw_section = false;
            continue;
        }
        let starts_indented = raw.starts_with(' ') || raw.starts_with('\t');
        if !starts_indented && trimmed.ends_with(':') {
            flush(&cur_name, disconnected, cur_ip, cur_gw, &mut best);
            let header = trimmed.trim_end_matches(':');
            if header.eq_ignore_ascii_case("Windows IP Configuration") {
                cur_name.clear();
            } else {
                cur_name = header.to_string();
            }
            disconnected = false;
            cur_ip = None;
            cur_gw = None;
            in_gw_section = false;
            continue;
        }
        if cur_name.is_empty() {
            continue;
        }
        let low = trimmed.to_ascii_lowercase();
        if low.contains("media disconnected") {
            disconnected = true;
            in_gw_section = false;
            continue;
        }
        if let Some((lhs, rhs)) = trimmed.split_once(':') {
            let ll = lhs.to_ascii_lowercase();
            if ll.contains("ipv4 address") || ll.contains("ip address") {
                in_gw_section = false;
                if cur_ip.is_none() {
                    cur_ip = extract_ipv4(rhs);
                }
            } else if ll.contains("default gateway") {
                in_gw_section = true;
                if cur_gw.is_none() {
                    cur_gw = extract_ipv4(rhs);
                }
            } else {
                in_gw_section = false;
            }
        } else if in_gw_section && cur_gw.is_none() {
            // IPv6 default gateway often appears on the `Default Gateway :` line while the IPv4
            // default gateway appears indented on the following continuation line.
            cur_gw = extract_ipv4(trimmed);
        }
    }
    flush(&cur_name, disconnected, cur_ip, cur_gw, &mut best);
    best.map(|(_, ip)| ip.to_string())
}

fn probe_rfc1918_lan_ip() -> Option<String> {
    // No packets are transmitted: UDP connect only asks the OS routing table for a local source IP.
    const TARGETS: &[&str] = &[
        "192.168.1.1:9",
        "192.168.0.1:9",
        "192.168.50.1:9",
        "192.168.68.1:9",
        "192.168.100.1:9",
        "172.16.0.1:9",
        "10.0.0.1:9",
    ];
    for target in TARGETS {
        let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if s.connect(target).is_err() {
            continue;
        }
        if let Ok(addr) = s.local_addr() {
            if let IpAddr::V4(v4) = addr.ip() {
                if is_rfc1918(v4) && !is_vpn_or_virtual_ip(v4) {
                    return Some(v4.to_string());
                }
            }
        }
    }
    None
}

fn detect_lan_ip() -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("ipconfig");
        cmd.creation_flags(0x08000000);
        if let Ok(out) = cmd.output() {
            if out.status.success() {
                let txt = String::from_utf8_lossy(&out.stdout);
                if let Some(ip) = parse_ipconfig_lan_ip(&txt) {
                    return Some(ip);
                }
            }
        }
    }
    probe_rfc1918_lan_ip()
}

/// This PC's physical home-network IPv4 address (VPN-safe: skips Mullvad/WireGuard/virtual adapters,
/// cached for 15s to avoid repeatedly spawning `ipconfig`).
pub fn lan_ip() -> Option<String> {
    static CACHE: OnceLock<Mutex<(i64, Option<String>)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((0, None)));
    let ts = now();
    {
        let g = cache.lock().unwrap();
        if g.0 > 0 && ts - g.0 < 15_000 {
            return g.1.clone();
        }
    }
    let ip = detect_lan_ip();
    *cache.lock().unwrap() = (ts, ip.clone());
    ip
}

pub fn url() -> Option<String> {
    lan_ip().map(|ip| format!("http://{ip}:{PORT}"))
}

pub fn qr_svg(text: &str) -> String {
    match qrcode::QrCode::new(text.as_bytes()) {
        Ok(c) => c
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(180, 180)
            .dark_color(qrcode::render::svg::Color("#000"))
            .light_color(qrcode::render::svg::Color("#fff"))
            .build(),
        Err(_) => String::new(),
    }
}

/// Start the LAN listener once (it rejects remote routes with 403 while the remote is switched off;
/// `/c/...` DLNA cast streams remain gated by their own random per-session token).
pub fn ensure_listener(app: Arc<App>) {
    if app.remote.started.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let r = Router::new()
            .route("/", get(page))
            .route("/r/pair", post(pair))
            .route("/r/state", get(state))
            .route("/r/events", get(phone_events))
            .route("/r/cmd", post(cmd))
            .route("/r/home", get(home))
            .route("/c/{tok}/{kind}/{file}", get(crate::cast::lan_stream))
            .route("/r/search", get(search))
            .route("/r/series/{id}", get(series_info))
            .route("/r/play", post(play))
            .with_state(app.clone());
        // TV_REMOTE_HOST=127.0.0.1 keeps test instances off the network
        let host = std::env::var("TV_REMOTE_HOST").unwrap_or_else(|_| "0.0.0.0".into());
        match tokio::net::TcpListener::bind((host.as_str(), PORT)).await {
            Ok(l) => {
                println!("[remote] listening on {host}:{PORT}");
                let _ = axum::serve(
                    l,
                    r.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await;
            }
            Err(e) => {
                println!("[remote] can't listen on {PORT}: {e}");
                app.remote.started.store(false, Ordering::SeqCst);
            }
        }
    });
}

type A = State<Arc<App>>;

fn deny(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn authed_token(app: &App, tok: &str) -> Result<(), Response> {
    if !enabled(app) {
        return Err(deny(
            StatusCode::FORBIDDEN,
            "The phone remote is switched off on the PC.",
        ));
    }
    if tok.len() >= 32 && paired_devices(app).iter().any(|d| d.token == tok) {
        touch_device_last_seen(app, tok);
        Ok(())
    } else {
        Err(deny(StatusCode::UNAUTHORIZED, "pair"))
    }
}

fn authed(app: &App, h: &HeaderMap) -> Result<(), Response> {
    let tok = h
        .get("x-remote")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    authed_token(app, tok)
}

/// Canonical remote state payload matching `remote.html`'s `r.s.player` schema for both mpv and web.
fn remote_state_json(app: &App) -> Value {
    if let Some(mut snap) = crate::mpv::remote_snapshot(app) {
        snap["canSkip"] = json!(snap["skip"].is_string());
        return json!({
            "tv": true,
            "fresh": true,
            "s": { "player": snap }
        });
    }
    let (t, s) = app.remote.tv_state.lock().unwrap().clone();
    json!({
        "tv": app.remote.tx.receiver_count() > 0,
        "fresh": now() - t < 6000,
        "s": s
    })
}

fn push_phone_state(app: &App) {
    if app.remote.phone_tx.receiver_count() > 0 {
        let _ = app.remote.phone_tx.send(remote_state_json(app).to_string());
    }
}

async fn page(State(app): A) -> Response {
    if !enabled(&app) {
        return (
            StatusCode::FORBIDDEN,
            "The phone remote is switched off on the PC.",
        )
            .into_response();
    }
    match std::fs::read_to_string(app.public.join("remote.html")) {
        Ok(h) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            h,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "missing").into_response(),
    }
}

async fn pair(
    State(app): A,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    Json(b): Json<Value>,
) -> Response {
    if !enabled(&app) {
        return deny(
            StatusCode::FORBIDDEN,
            "The phone remote is switched off on the PC.",
        );
    }
    let ts = now();
    if !app.remote.check_and_record_attempt(addr.ip(), ts) {
        return deny(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many tries from this device. Wait a few minutes.",
        );
    }
    let given = b["code"].as_str().unwrap_or("").trim().to_string();
    match app.remote.code() {
        Some(c) if !given.is_empty() && c == given => {}
        _ => {
            return deny(
                StatusCode::UNAUTHORIZED,
                "That code didn't match. Check the code on the PC (Settings → Phone remote).",
            )
        }
    }
    app.remote.clear_attempts(addr.ip());

    use rand::Rng;
    let tok: String = (0..32)
        .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16u8)))
        .collect();
    let id: String = (0..8)
        .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16u8)))
        .collect();
    let ua = h
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let requested_label = b["label"].as_str().map(sanitize_label).unwrap_or_default();
    let label = if requested_label.is_empty() {
        derive_label(ua)
    } else {
        requested_label
    };
    let dev = PairedDevice {
        id: id.clone(),
        token: tok.clone(),
        label: label.clone(),
        paired_at: ts,
        last_seen: ts,
    };

    app.state.with(|st| {
        if st["remote"].is_null() {
            st["remote"] = json!({});
        }
        let mut devs: Vec<PairedDevice> = st["remote"]["tokens"]
            .as_array()
            .map(|a| a.iter().filter_map(parse_device).collect())
            .unwrap_or_default();
        devs.push(dev);
        while devs.len() > MAX_PAIRED_DEVICES {
            // Evict the least recently used device.
            if let Some((evict_idx, _)) = devs
                .iter()
                .enumerate()
                .min_by_key(|(_, d)| d.last_seen.max(d.paired_at))
            {
                devs.remove(evict_idx);
            } else {
                break;
            }
        }
        st["remote"]["tokens"] = Value::Array(devs.iter().map(device_to_value).collect());
    });
    app.state.save();
    app.remote.new_code();
    let _ = app.remote.tx.send(json!({ "cmd": "paired" }).to_string());
    Json(json!({ "token": tok, "id": id, "label": label })).into_response()
}

async fn state(State(app): A, h: HeaderMap) -> Response {
    if let Err(r) = authed(&app, &h) {
        return r;
    }
    Json(remote_state_json(&app)).into_response()
}

/// Live SSE state stream for the phone remote (`GET /r/events`), eliminating 1s HTTP polling.
/// Accepts auth via `x-remote` header or `?token=...` query string (required by browser `EventSource`).
async fn phone_events(
    State(app): A,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let tok = h
        .get("x-remote")
        .and_then(|v| v.to_str().ok())
        .or_else(|| q.get("token").map(|s| s.as_str()))
        .unwrap_or("");
    if let Err(r) = authed_token(&app, tok) {
        return r;
    }
    use axum::response::sse::{Event, KeepAlive, Sse};
    let rx = app.remote.phone_tx.subscribe();
    let initial = remote_state_json(&app).to_string();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let tok_owned = tok.to_string();

    struct SseState {
        app: Arc<App>,
        tok: String,
        rx: tokio::sync::broadcast::Receiver<String>,
        tick: tokio::time::Interval,
        initial: Option<String>,
        last_sent: String,
    }

    let st = SseState {
        app: app.clone(),
        tok: tok_owned,
        rx,
        tick,
        initial: Some(initial),
        last_sent: String::new(),
    };

    let s = futures_util::stream::unfold(st, |mut st| async move {
        if let Some(first) = st.initial.take() {
            st.last_sent = first.clone();
            return Some((Ok::<Event, std::convert::Infallible>(Event::default().data(first)), st));
        }
        loop {
            tokio::select! {
                msg = st.rx.recv() => {
                    if !enabled(&st.app) || !paired_devices(&st.app).iter().any(|d| d.token == st.tok) {
                        return None;
                    }
                    match msg {
                        Ok(m) => {
                            if m.is_empty() { continue; }
                            st.last_sent = m.clone();
                            return Some((Ok(Event::default().data(m)), st));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return None,
                    }
                }
                _ = st.tick.tick() => {
                    if !enabled(&st.app) || !paired_devices(&st.app).iter().any(|d| d.token == st.tok) {
                        return None;
                    }
                    let snap = remote_state_json(&st.app).to_string();
                    if snap != st.last_sent {
                        st.last_sent = snap.clone();
                        return Some((Ok(Event::default().data(snap)), st));
                    }
                }
            }
        }
    });

    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

const CMDS: &[&str] = &[
    "toggle", "pause", "play", "seek", "seekto", "next", "subs", "vol", "mute", "full", "close",
    "stop", "key", "skip", "undo_skip", "sub_sync", "audio", "source", "tvmode", "last",
];

const MPV_CMDS: &[&str] = &[
    "toggle", "pause", "play", "seek", "seekto", "vol", "mute", "subs", "audio", "full", "skip",
    "undo_skip", "sub_sync", "next", "close", "stop",
];

async fn cmd(State(app): A, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if let Err(r) = authed(&app, &h) {
        return r;
    }
    let c = b["cmd"].as_str().unwrap_or("");
    if !CMDS.contains(&c) {
        return deny(StatusCode::BAD_REQUEST, "unknown command");
    }
    if crate::mpv::running(&app) && MPV_CMDS.contains(&c) {
        let a = app.clone();
        let cs = c.to_string();
        let v = b.get("v").cloned();
        let v2 = v.clone();
        let ok = tokio::task::spawn_blocking(move || crate::mpv::control_sync(&a, &cs, v.as_ref()))
            .await
            .unwrap_or(false);
        if ok {
            // Reflect immediate state changes locally so SSE subscribers see sub-50ms feedback.
            if let Some(s) = app.mpv.s.lock().unwrap().as_mut() {
                match c {
                    "toggle" | "pause" => s.paused = !s.paused,
                    "play" => s.paused = false,
                    "mute" => s.muted = !s.muted,
                    "subs" => s.sub_on = !s.sub_on,
                    "vol" => {
                        if let Some(vol) = v2.as_ref().and_then(|x| x.as_f64()) {
                            s.vol = if vol <= 1.5 {
                                vol.clamp(0.0, 1.3)
                            } else {
                                (vol / 100.0).clamp(0.0, 1.3)
                            };
                        }
                    }
                    "seek" => {
                        if let Some(d) = v2.as_ref().and_then(|x| x.as_f64()) {
                            s.pos = (s.pos + d).max(0.0);
                        }
                    }
                    "seekto" => {
                        if let Some(t) = v2.as_ref().and_then(|x| x.as_f64()) {
                            s.pos = t.max(0.0);
                        }
                    }
                    "audio" => {
                        if let Some(idx) = v2.as_ref().and_then(|x| x.as_i64()) {
                            s.cur_audio = idx;
                        }
                    }
                    _ => {}
                }
            }
            push_phone_state(&app);
        }
        return Json(json!({ "ok": ok, "pro": true })).into_response();
    }
    let _ = app
        .remote
        .tx
        .send(json!({ "cmd": c, "v": b["v"] }).to_string());
    Json(json!({ "ok": true })).into_response()
}

async fn home(State(app): A, h: HeaderMap) -> Response {
    if let Err(r) = authed(&app, &h) {
        return r;
    }
    // deduplicated continue watching + my list + favourite live channels
    let cw = crate::rec::continue_watching(&app).await;
    let cont: Vec<Value> = cw
        .into_iter()
        .take(15)
        .map(|it| {
            json!({
                "k": it["k"], "id": it["id"], "sid": it["sid"], "title": it["title"], "sub": it["sub"],
                "img": if it["backdrop"].is_string() && !it["backdrop"].as_str().unwrap_or("").is_empty() { it["backdrop"].clone() } else { it["poster"].clone() },
                "pct": it["progress"].as_f64().unwrap_or(0.0)
            })
        })
        .collect();
    let (list, favs_raw) = app
        .state
        .read(|st| (st["list"].clone(), st["favs"].clone()));
    let mine: Vec<Value> = list
        .as_object()
        .map(|m| {
            m.values()
                .take(30)
                .map(|v| {
                    let it = if v["item"].is_object() { &v["item"] } else { v };
                    json!({ "k": it["k"], "id": it["id"], "title": it["title"], "img": it["poster"] })
                })
                .collect()
        })
        .unwrap_or_default();
    let favs: Vec<Value> = favs_raw
        .as_object()
        .map(|m| {
            m.values()
                .take(24)
                .map(|v| {
                    let it = &v["item"];
                    let nn = crate::sports::now_next(&app, it["epg"].as_str().unwrap_or(""));
                    json!({
                        "k": "l", "id": it["id"], "title": it["title"],
                        "sub": nn["now"]["title"].as_str().unwrap_or(""),
                        "img": it["logo"].as_str().filter(|s| !s.is_empty()).or(it["poster"].as_str()).unwrap_or("")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Json(json!({ "cont": cont, "list": mine, "favs": favs })).into_response()
}

async fn search(
    State(app): A,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = authed(&app, &h) {
        return r;
    }
    let cat = app.catalog();
    let (m, s, l) = cat.search(q.get("q").map(|x| x.as_str()).unwrap_or(""), false, 12);
    let f = |x: &crate::catalog::Item| {
        json!({ "k": x.k, "id": x.id, "title": x.title, "year": x.year, "img": x.poster })
    };
    let fl = |x: &crate::catalog::Item| {
        let nn = crate::sports::now_next(&app, &x.epg);
        json!({ "k": "l", "id": x.id, "title": x.title, "sub": nn["now"]["title"].as_str().unwrap_or(""), "img": x.poster })
    };
    Json(json!({
        "movies": m.into_iter().map(f).collect::<Vec<_>>(),
        "series": s.into_iter().map(f).collect::<Vec<_>>(),
        "live": l.into_iter().map(fl).collect::<Vec<_>>()
    }))
    .into_response()
}

async fn series_info(State(app): A, h: HeaderMap, Path(id): Path<i64>) -> Response {
    if let Err(r) = authed(&app, &h) {
        return r;
    }
    let cat = app.catalog();
    let Some(it) = cat.series_item(id).cloned() else {
        return deny(StatusCode::NOT_FOUND, "not found");
    };
    let info = app
        .xc_info("get_series_info", "series_id", it.id, 2 * 3_600_000)
        .await;
    let progress = app.state.read(|st| st["progress"].clone());
    let mut by_season: std::collections::BTreeMap<i64, Vec<Value>> =
        std::collections::BTreeMap::new();
    let mut flat: Vec<(i64, i64, Value)> = Vec::new();
    if let Some(eps_obj) = info["episodes"].as_object() {
        for (sk, arr) in eps_obj {
            let Ok(sn) = sk.parse::<i64>() else { continue };
            let mut eps: Vec<(i64, Value)> = Vec::new();
            for e in arr.as_array().into_iter().flatten() {
                let eid = crate::util::i(&e["id"]);
                if eid <= 0 {
                    continue;
                }
                let ep_num = crate::util::i(&e["episode_num"]);
                let raw_title = crate::re!(r"(?i)^.*?S\d+E\d+\s*-?\s*")
                    .replace(&crate::util::s(&e["title"]), "")
                    .trim()
                    .to_string();
                let title = if raw_title.is_empty() {
                    format!("Episode {ep_num}")
                } else {
                    raw_title
                };
                let dur = crate::util::i(&e["info"]["duration_secs"]);
                let dur = if dur > 0 && dur < 400 { dur * 60 } else { dur };
                let p = &progress[format!("e:{eid}")];
                let pdur = p["dur"].as_f64().unwrap_or(0.0);
                let pct = if pdur > 0.0 {
                    (p["pos"].as_f64().unwrap_or(0.0) / pdur).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let ev = json!({
                    "id": eid,
                    "n": ep_num,
                    "title": title,
                    "duration": dur,
                    "progress": pct,
                });
                eps.push((ep_num, ev));
            }
            eps.sort_by_key(|(n, _)| *n);
            for (n, ev) in &eps {
                flat.push((sn, *n, ev.clone()));
            }
            by_season.insert(sn, eps.into_iter().map(|(_, ev)| ev).collect());
        }
    }
    let mut next_up = Value::Null;
    let mut last_t = 0i64;
    for (ix, (sn, _, ev)) in flat.iter().enumerate() {
        let p = &progress[format!("e:{}", ev["id"])];
        if let Some(t) = p["t"].as_i64() {
            if t > last_t {
                last_t = t;
                let pdur = p["dur"].as_f64().unwrap_or(1.0).max(1.0);
                if p["pos"].as_f64().unwrap_or(0.0) / pdur > 0.94 {
                    if let Some((nsn, _, nev)) = flat.get(ix + 1) {
                        next_up = json!({ "season": nsn, "id": nev["id"], "n": nev["n"] });
                    }
                } else {
                    next_up = json!({ "season": sn, "id": ev["id"], "n": ev["n"] });
                }
            }
        }
    }
    if next_up.is_null() {
        if let Some((sn, _, ev)) = flat.first() {
            next_up = json!({ "season": sn, "id": ev["id"], "n": ev["n"] });
        }
    }
    let seasons: Vec<Value> = by_season
        .into_iter()
        .map(|(sn, eps)| {
            json!({
                "n": sn,
                "name": if sn == 0 { "Specials".to_string() } else { format!("Season {sn}") },
                "episodes": eps,
            })
        })
        .collect();
    Json(json!({
        "id": it.id,
        "title": it.title,
        "nextUp": next_up,
        "seasons": seasons,
    }))
    .into_response()
}

async fn play(State(app): A, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if let Err(r) = authed(&app, &h) {
        return r;
    }
    let k = b["k"].as_str().unwrap_or("");
    if !["m", "s", "e", "l"].contains(&k) {
        return deny(StatusCode::BAD_REQUEST, "kind");
    }
    let mut payload = json!({ "cmd": "play", "k": k, "id": b["id"], "sid": b["sid"] });
    if k == "l" {
        if let Some(id) = b["id"]
            .as_i64()
            .or_else(|| b["id"].as_str().and_then(|x| x.parse().ok()))
        {
            if let Some(ch) = app.catalog().live_item(id) {
                payload["title"] = json!(ch.title);
                payload["epg"] = json!(ch.epg);
                payload["logo"] = json!(ch.poster);
            }
        }
    }
    app.remote.send(&app.data, payload.to_string());
    Json(json!({ "ok": true })).into_response()
}

// ---- localhost side (TV page + settings)
pub async fn events(
    State(app): A,
) -> axum::response::sse::Sse<
    impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let rx = app.remote.tx.subscribe();
    let first = app.remote.take_pending();
    let hello = json!({ "cmd": "hello", "enabled": enabled(&app) }).to_string();
    let init: Vec<String> = std::iter::once(hello).chain(first).collect();
    let s = futures_util::stream::unfold((init, rx), |(mut init, mut rx)| async move {
        if !init.is_empty() {
            let m = init.remove(0);
            return Some((Ok(Event::default().data(m)), (init, rx)));
        }
        loop {
            match rx.recv().await {
                Ok(m) => return Some((Ok(Event::default().data(m)), (init, rx))),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    });
    Sse::new(s).keep_alive(KeepAlive::default())
}

pub async fn tv_state(State(app): A, Json(b): Json<Value>) -> Response {
    *app.remote.tv_state.lock().unwrap() = (now(), b);
    push_phone_state(&app);
    Json(json!({ "ok": true })).into_response()
}

pub async fn info(State(app): A) -> Response {
    let on = enabled(&app);
    let ip = lan_ip();
    let u = ip.as_ref().map(|x| format!("http://{x}:{PORT}"));
    if on && app.remote.code().is_none() {
        app.remote.new_code();
    }
    let devs = paired_devices(&app);
    let devices_pub: Vec<Value> = devs
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "label": d.label,
                "pairedAt": d.paired_at,
                "lastSeen": d.last_seen,
            })
        })
        .collect();
    Json(json!({
        "enabled": on,
        "url": u,
        "lanIp": ip,
        "code": if on { app.remote.code() } else { None },
        "qr": if on { u.as_deref().map(qr_svg) } else { None },
        "paired": devs.len(),
        "devices": devices_pub,
    }))
    .into_response()
}

pub async fn set(State(app): A, Json(b): Json<Value>) -> Response {
    if let Some(on) = b["enabled"].as_bool() {
        app.state.with(|st| {
            if st["remote"].is_null() {
                st["remote"] = json!({});
            }
            st["remote"]["enabled"] = json!(on);
        });
        app.state.save();
        if on {
            ensure_listener(app.clone());
            app.remote.new_code();
        } else {
            // Wake any connected phone SSE clients so they immediately observe disabled=403 and close.
            let _ = app.remote.phone_tx.send(String::new());
        }
    }
    if b["forget"].as_bool() == Some(true) {
        app.state.with(|st| st["remote"]["tokens"] = json!([]));
        app.state.save();
        let _ = app.remote.phone_tx.send(String::new());
    }
    if let Some(id) = b["revoke"].as_str() {
        if revoke_device(&app, id) {
            let _ = app.remote.phone_tx.send(String::new());
        }
    }
    if let Some(ren) = b["rename"].as_object() {
        if let (Some(id), Some(lbl)) = (
            ren.get("id").and_then(|v| v.as_str()),
            ren.get("label").and_then(|v| v.as_str()),
        ) {
            rename_device(&app, id, lbl);
        }
    }
    info(State(app)).await
}

/// Optional standalone route (`DELETE /api/remote/device/{id}`) to revoke one paired phone.
pub async fn revoke(State(app): A, Path(id): Path<String>) -> Response {
    let removed = revoke_device(&app, &id);
    if removed {
        let _ = app.remote.phone_tx.send(String::new());
    }
    info(State(app)).await
}

/// Localhost only (main API): send a command straight to the TV page (automation / tests). Returns how many pages listen.
pub async fn send_local(State(app): A, Json(b): Json<Value>) -> Response {
    let n = app.remote.tx.receiver_count();
    let _ = app.remote.tx.send(b.to_string());
    Json(json!({ "ok": true, "listeners": n })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vpn_and_virtual_ips_are_rejected_and_rfc1918_accepted() {
        // Mullvad / WireGuard tunnel range 10.64.0.0..=10.127.255.255 must be skipped
        assert!(is_vpn_or_virtual_ip(Ipv4Addr::new(10, 64, 12, 34)));
        assert!(is_vpn_or_virtual_ip(Ipv4Addr::new(10, 127, 255, 1)));
        // Tailscale / CGNAT 100.64.0.0/10 must be skipped
        assert!(is_vpn_or_virtual_ip(Ipv4Addr::new(100, 100, 12, 5)));
        // Loopback & APIPA must be skipped
        assert!(is_vpn_or_virtual_ip(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_vpn_or_virtual_ip(Ipv4Addr::new(169, 254, 10, 20)));
        // Standard home LAN RFC1918 addresses must be accepted
        assert!(!is_vpn_or_virtual_ip(Ipv4Addr::new(192, 168, 1, 42)));
        assert!(!is_vpn_or_virtual_ip(Ipv4Addr::new(10, 0, 0, 15)));
        assert!(!is_vpn_or_virtual_ip(Ipv4Addr::new(172, 16, 4, 8)));
    }

    #[test]
    fn parse_ipconfig_prefers_physical_lan_over_mullvad_and_wsl() {
        let sample = "\
Windows IP Configuration


Unknown adapter Mullvad:

   Connection-specific DNS Suffix  . :
   IPv4 Address. . . . . . . . . . . : 10.64.88.14(Preferred)
   Subnet Mask . . . . . . . . . . . : 255.255.255.255
   Default Gateway . . . . . . . . . : 0.0.0.0

Ethernet adapter vEthernet (WSL):

   Connection-specific DNS Suffix  . :
   IPv4 Address. . . . . . . . . . . : 172.24.160.1(Preferred)
   Subnet Mask . . . . . . . . . . . : 255.255.240.0
   Default Gateway . . . . . . . . . :

Wireless LAN adapter Wi-Fi:

   Connection-specific DNS Suffix  . : home.local
   Link-local IPv6 Address . . . . . : fe80::1234:5678%9
   IPv4 Address. . . . . . . . . . . : 192.168.1.42(Preferred)
   Subnet Mask . . . . . . . . . . . : 255.255.255.0
   Default Gateway . . . . . . . . . : fe80::1%9
                                       192.168.1.1
";
        assert_eq!(parse_ipconfig_lan_ip(sample).as_deref(), Some("192.168.1.42"));
    }

    #[test]
    fn per_ip_rate_limit_isolates_clients() {
        let r = Remote::new();
        let a: IpAddr = "192.168.1.10".parse().unwrap();
        let b: IpAddr = "192.168.1.20".parse().unwrap();
        let t0 = 1_700_000_000_000i64;
        for i in 0..PAIR_MAX_PER_IP {
            assert!(r.check_and_record_attempt(a, t0 + i as i64 * 1000));
        }
        // Client A is now locked out, but Client B must still be allowed!
        assert!(!r.check_and_record_attempt(a, t0 + 20_000));
        assert!(r.check_and_record_attempt(b, t0 + 20_000));
        // Clearing A restores A immediately.
        r.clear_attempts(a);
        assert!(r.check_and_record_attempt(a, t0 + 21_000));
    }
}
