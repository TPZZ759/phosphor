// Cast to a TV over DLNA/UPnP (Samsung, LG, Sony, Panasonic, Philips, Android TV renderers...).
// Discovery: SSDP M-SEARCH bound directly to the physical LAN adapter (VPN-safe!) for MediaRenderer:1,
// then device description; only video renderers are offered (Sonos/WiiM speakers are filtered out).
// Playback: the TV pulls an MPEG-TS stream (`video/MP2T`) from this PC's home-network listener
// (port 7880) under a random per-session token; DLNA HEAD probes return instant 200 headers without
// spawning ffmpeg, and watch progress is saved automatically while casting.
use crate::app::App;
use crate::util::now_ms;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};

type A = State<Arc<App>>;

const DLNA_FEATURES: &str = "DLNA.ORG_OP=00;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000";

#[derive(Clone, Debug)]
pub struct Device { pub id: String, pub name: String, pub model: String, pub av: String, pub rc: String }

#[derive(Clone, Debug)]
pub struct Session {
    pub dev: Device,
    pub token: String,
    pub kind: String,
    pub id: String,
    pub params: HashMap<String, String>,
    pub t: f64,
    pub title: String,
    pub started: i64,
    pub progress_key: String,
    pub dur: f64,
    pub item: Value,
    pub tmdb: Value,
    pub last_saved: i64,
}

#[derive(Default)]
pub struct Cast { devices: Mutex<(i64, Vec<Device>)>, pub session: Mutex<Option<Session>> }

fn err(code: StatusCode, m: &str) -> Response { (code, Json(json!({ "error": m }))).into_response() }
fn xml_esc(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;") }
fn unesc(s: &str) -> String { crate::sports::decode_xml_entities(s) }

fn local_xml_name(raw: &str) -> &str {
    raw.rsplit_once(':').map(|(_, l)| l).unwrap_or(raw)
}

/// Namespace-safe XML element text extractor: matches `<tag ...>...</tag>` AND `<ns:tag ...>...</ns:tag>`
/// with strict tag-name boundary checks so `<service>` never falsely matches `<serviceList>`.
pub(crate) fn tag(x: &str, local: &str) -> Option<String> {
    let bytes = x.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(rel_lt) = x[i..].find('<') else { break };
        let lt = i + rel_lt;
        let next = lt + 1;
        if next >= bytes.len() { break; }
        let b = bytes[next];
        if b == b'/' || b == b'!' || b == b'?' {
            i = next;
            continue;
        }
        let mut name_end = next;
        while name_end < bytes.len()
            && !matches!(bytes[name_end], b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        {
            name_end += 1;
        }
        let raw_name = &x[next..name_end];
        let Some(rel_gt) = x[name_end..].find('>') else { break };
        let gt = name_end + rel_gt;
        if local_xml_name(raw_name).eq_ignore_ascii_case(local) {
            if gt > name_end && bytes[gt - 1] == b'/' {
                return Some(String::new());
            }
            let content_start = gt + 1;
            // Find matching closing tag `</raw_name>` (or `</*:local>`).
            let mut search = content_start;
            while let Some(rel_c) = x[search..].find("</") {
                let c_lt = search + rel_c;
                let c_name_start = c_lt + 2;
                let Some(rel_c_gt) = x[c_name_start..].find('>') else { break };
                let c_name = x[c_name_start..c_name_start + rel_c_gt].trim();
                if c_name.eq_ignore_ascii_case(raw_name)
                    || local_xml_name(c_name).eq_ignore_ascii_case(local)
                {
                    return Some(x[content_start..c_lt].trim().to_string());
                }
                search = c_name_start + rel_c_gt + 1;
            }
        }
        i = gt + 1;
    }
    None
}

/// Extract all `<service>...</service>` (including namespaced `<ns:service ...>`) blocks from device XML.
fn extract_service_blocks(x: &str) -> Vec<&str> {
    let mut out = vec![];
    let bytes = x.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(rel_lt) = x[i..].find('<') else { break };
        let lt = i + rel_lt;
        let next = lt + 1;
        if next >= bytes.len() { break; }
        if matches!(bytes[next], b'/' | b'!' | b'?') {
            i = next;
            continue;
        }
        let mut name_end = next;
        while name_end < bytes.len()
            && !matches!(bytes[name_end], b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        {
            name_end += 1;
        }
        let raw_name = &x[next..name_end];
        let Some(rel_gt) = x[name_end..].find('>') else { break };
        let gt = name_end + rel_gt;
        if local_xml_name(raw_name).eq_ignore_ascii_case("service") && !(gt > name_end && bytes[gt - 1] == b'/') {
            let content_start = gt + 1;
            let mut search = content_start;
            let mut closed_at = None;
            while let Some(rel_c) = x[search..].find("</") {
                let c_lt = search + rel_c;
                let c_name_start = c_lt + 2;
                let Some(rel_c_gt) = x[c_name_start..].find('>') else { break };
                let c_name = x[c_name_start..c_name_start + rel_c_gt].trim();
                if local_xml_name(c_name).eq_ignore_ascii_case("service") {
                    out.push(&x[content_start..c_lt]);
                    closed_at = Some(c_name_start + rel_c_gt + 1);
                    break;
                }
                search = c_name_start + rel_c_gt + 1;
            }
            if let Some(end_i) = closed_at {
                i = end_i;
                continue;
            }
        }
        i = gt + 1;
    }
    out
}

/// Bind SSDP M-SEARCH specifically to the physical LAN IPv4 interface so multicast 239.255.255.250:1900
/// leaves on the home Wi-Fi/Ethernet adapter instead of being swallowed by a VPN tunnel.
fn ssdp(wait_ms: u64) -> Vec<String> {
    let bind_sock = || -> Option<std::net::UdpSocket> {
        if let Some(ip_str) = crate::remote::lan_ip() {
            if let Ok(v4) = ip_str.parse::<Ipv4Addr>() {
                if let Ok(s) = std::net::UdpSocket::bind(SocketAddrV4::new(v4, 0)) {
                    let _ = s.set_multicast_ttl_v4(4);
                    let _ = s.set_multicast_loop_v4(false);
                    return Some(s);
                }
            }
        }
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        let _ = s.set_multicast_ttl_v4(4);
        Some(s)
    };
    let Some(s) = bind_sock() else { return vec![] };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(300)));
    let msg = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
    for _ in 0..2 { let _ = s.send_to(msg.as_bytes(), "239.255.255.250:1900"); }
    let until = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    let mut locs = vec![]; let mut buf = [0u8; 2048];
    while std::time::Instant::now() < until {
        if let Ok((n, _)) = s.recv_from(&mut buf) {
            let t = String::from_utf8_lossy(&buf[..n]).to_string();
            if let Some(l) = t.lines().find(|l| l.to_ascii_lowercase().starts_with("location:")) {
                let l = l[9..].trim().to_string();
                if !l.is_empty() && !locs.contains(&l) { locs.push(l); }
            }
        }
    }
    locs
}

fn abs(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") { return path.to_string(); }
    let root = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    format!("{root}{}{path}", if path.starts_with('/') { "" } else { "/" })
}

pub async fn soap(app: &App, url: &str, service: &str, action: &str, args: &str) -> Result<String, String> {
    let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"urn:schemas-upnp-org:service:{service}:1\">{args}</u:{action}></s:Body></s:Envelope>");
    let r = app.http.post(url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPACTION", format!("\"urn:schemas-upnp-org:service:{service}:1#{action}\""))
        .body(body)
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let ok = r.status().is_success();
    let t = r.text().await.unwrap_or_default();
    if ok {
        Ok(t)
    } else {
        Err(tag(&t, "errorDescription").unwrap_or_else(|| "the TV refused the request".into()))
    }
}

async fn describe(app: &App, loc: &str) -> Option<Device> {
    let x = app.http.get(loc).timeout(std::time::Duration::from_secs(3)).send().await.ok()?.text().await.ok()?;
    let base = tag(&x, "URLBase").filter(|u| !u.is_empty()).unwrap_or_else(|| loc.to_string());
    let mut av = None; let mut cm = None; let mut rc = None;
    for blk in extract_service_blocks(&x) {
        let ty = tag(blk, "serviceType").unwrap_or_default();
        let ctl = tag(blk, "controlURL").unwrap_or_default();
        if ctl.is_empty() { continue; }
        if ty.contains("AVTransport") { av = Some(abs(&base, &ctl)); }
        else if ty.contains("ConnectionManager") { cm = Some(abs(&base, &ctl)); }
        else if ty.contains("RenderingControl") { rc = Some(abs(&base, &ctl)); }
    }
    let (av, cm) = (av?, cm?);
    // video renderers only
    let sink = soap(app, &cm, "ConnectionManager", "GetProtocolInfo", "").await.ok().and_then(|t| tag(&t, "Sink")).unwrap_or_default();
    if !sink.to_ascii_lowercase().contains("video/") { return None; }
    Some(Device {
        id: tag(&x, "UDN").unwrap_or_else(|| loc.to_string()),
        name: unesc(&tag(&x, "friendlyName").unwrap_or_else(|| "TV".into())),
        model: unesc(&tag(&x, "modelName").unwrap_or_default()),
        av,
        rc: rc.unwrap_or_default(),
    })
}

pub async fn devices(State(app): A) -> Response {
    let cached = app.cast.devices.lock().unwrap().clone();
    let list = if now_ms() - cached.0 < 60_000 && !cached.1.is_empty() {
        cached.1
    } else {
        let locs = tokio::task::spawn_blocking(|| ssdp(2500)).await.unwrap_or_default();
        let found: Vec<Device> = futures_util::future::join_all(locs.iter().map(|l| describe(&app, l))).await.into_iter().flatten().collect();
        let mut uniq: Vec<Device> = vec![];
        for d in found { if !uniq.iter().any(|u| u.id == d.id) { uniq.push(d); } }
        *app.cast.devices.lock().unwrap() = (now_ms(), uniq.clone());
        uniq
    };
    Json(json!({
        "devices": list.iter().map(|d| json!({ "id": d.id, "name": d.name, "model": d.model })).collect::<Vec<_>>(),
        "lan": crate::remote::lan_ip()
    })).into_response()
}

fn stream_url(s: &Session) -> Option<String> {
    let ip = std::env::var("TV_CAST_HOST").ok().or_else(crate::remote::lan_ip)?; // override for tests
    let mut q: Vec<String> = s.params.iter().filter(|(k, _)| k.as_str() != "t").map(|(k, v)| format!("{}={}", k, crate::util::enc(v))).collect();
    q.push(format!("t={}", s.t.floor())); q.push("fmt=ts".into());
    Some(format!("http://{ip}:{}/c/{}/{}/{}.ts?{}", crate::remote::PORT, s.token, s.kind, s.id, q.join("&")))
}

async fn load(app: &App, s: &Session) -> Result<(), String> {
    let url = stream_url(s).ok_or("This PC isn't on a home network")?;
    let meta = format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"1\" parentID=\"0\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>object.item.videoItem.movie</upnp:class><res protocolInfo=\"http-get:*:video/MP2T:{DLNA_FEATURES}\">{}</res></item></DIDL-Lite>",
        xml_esc(&s.title),
        xml_esc(&url)
    );
    let _ = soap(app, &s.dev.av, "AVTransport", "Stop", "<InstanceID>0</InstanceID>").await;
    soap(
        app,
        &s.dev.av,
        "AVTransport",
        "SetAVTransportURI",
        &format!(
            "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
            xml_esc(&url),
            xml_esc(&meta)
        ),
    ).await?;
    soap(app, &s.dev.av, "AVTransport", "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>").await?;
    Ok(())
}

fn infer_progress_meta(app: &App, kind: &str, id: &str, params: &HashMap<String, String>, b: &Value) -> (String, f64, Value, Value) {
    let explicit_key = crate::util::s(&b["progress"]["key"]);
    let key = if !explicit_key.is_empty() {
        explicit_key
    } else if kind == "movie" {
        format!("m:{id}")
    } else if kind == "series" {
        format!("e:{id}")
    } else if kind == "catchup" {
        params.get("cs").map(|cs| format!("cu:{id}:{cs}")).unwrap_or_default()
    } else {
        String::new()
    };
    if key.is_empty() {
        return (String::new(), 0.0, Value::Null, Value::Null);
    }

    let prev = app.state.read(|st| st["progress"].get(&key).cloned().unwrap_or(Value::Null));
    let mut dur = b["dur"].as_f64()
        .or_else(|| b["progress"]["dur"].as_f64())
        .or_else(|| prev["dur"].as_f64())
        .unwrap_or(0.0);
    let mut item = if b["progress"]["item"].is_object() {
        b["progress"]["item"].clone()
    } else if prev["item"].is_object() {
        prev["item"].clone()
    } else {
        Value::Null
    };
    let mut tmdb = if !b["progress"]["tmdb"].is_null() {
        b["progress"]["tmdb"].clone()
    } else {
        Value::Null
    };

    let cat = app.catalog();
    if kind == "movie" {
        if let Ok(mid) = id.parse::<i64>() {
            if let Some(m) = cat.movie(mid) {
                if tmdb.is_null() { tmdb = json!(m.tmdb); }
                if item.is_null() { item = app.card(m, None); }
                if dur <= 0.0 {
                    if let Some(meta) = app.meta_of(m) {
                        if meta.rt > 0 { dur = (meta.rt * 60) as f64; }
                    }
                }
            }
        }
    } else if kind == "series" {
        if let Some(sid) = params.get("sid").and_then(|x| x.parse::<i64>().ok()) {
            if let Some(ser) = cat.series_item(sid) {
                if tmdb.is_null() { tmdb = json!(ser.tmdb); }
                if item.is_null() {
                    item = json!({ "k": "e", "id": id.parse::<i64>().unwrap_or(0), "sid": ser.id, "title": ser.title, "poster": ser.poster, "backdrop": ser.backdrop });
                }
                if dur <= 0.0 {
                    if let Some(meta) = app.meta_of(ser) {
                        if meta.rt > 0 { dur = (meta.rt * 60) as f64; }
                    }
                }
            }
        }
    } else if kind == "catchup" && dur <= 0.0 {
        if let Some(mins) = params.get("dur").and_then(|x| x.parse::<f64>().ok()) {
            dur = mins * 60.0;
        }
    }
    if dur <= 0.0 {
        let up = crate::upstream(app, kind, id, params.get("ext"), false);
        dur = app.stream.cached_duration(&up);
    }
    (key, dur, item, tmdb)
}

async fn save_cast_progress(app: &Arc<App>, s: &Session, pos: f64) {
    if s.kind == "live" || s.progress_key.is_empty() || pos < 5.0 || s.dur <= 0.0 {
        return;
    }
    let body = json!({
        "key": s.progress_key,
        "pos": pos.min(s.dur),
        "dur": s.dur,
        "item": s.item,
        "tmdb": s.tmdb
    });
    let _ = crate::progress(State(app.clone()), Json(body)).await;
}

/// Start casting {device, kind, id, t, title, params{ext,a,sid,cs,dur,...}}.
pub async fn start(State(app): A, Json(b): Json<Value>) -> Response {
    let dev_id = b["device"].as_str().unwrap_or("");
    let Some(dev) = app.cast.devices.lock().unwrap().1.iter().find(|d| d.id == dev_id).cloned() else {
        return err(StatusCode::NOT_FOUND, "That TV isn't available any more. Look for TVs again.");
    };
    crate::remote::ensure_listener(app.clone());
    use rand::Rng;
    let token: String = (0..24).map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16u8))).collect();
    let params: HashMap<String, String> = b["params"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), crate::util::s(v))).collect()).unwrap_or_default();
    let kind = crate::util::s(&b["kind"]);
    let id = crate::util::s(&b["id"]);
    if !["movie", "series", "live", "catchup"].contains(&kind.as_str()) {
        return err(StatusCode::BAD_REQUEST, "kind");
    }
    let (progress_key, dur, item, tmdb) = infer_progress_meta(&app, &kind, &id, &params, &b);
    let s = Session {
        dev,
        token,
        kind,
        id,
        params,
        t: b["t"].as_f64().unwrap_or(0.0),
        title: crate::util::s(&b["title"]),
        started: now_ms(),
        progress_key,
        dur,
        item,
        tmdb,
        last_saved: 0,
    };

    // Cleanly release browser stream, mpv pro player, local buffers, and background probe jobs!
    app.stream.stop("casting");
    if crate::mpv::running(&app) {
        let _ = crate::mpv::stop(State(app.clone())).await;
    }
    crate::spool::detach();
    crate::livebuf::detach();
    crate::yield_background(&app).await;

    *app.cast.session.lock().unwrap() = Some(s.clone());
    match load(&app, &s).await {
        Ok(()) => Json(json!({ "ok": true, "device": s.dev.name })).into_response(),
        Err(e) => {
            *app.cast.session.lock().unwrap() = None;
            err(StatusCode::BAD_GATEWAY, &format!("{} didn't start playback: {e}", s.dev.name))
        }
    }
}

pub async fn control(State(app): A, Json(b): Json<Value>) -> Response {
    let Some(mut s) = app.cast.session.lock().unwrap().clone() else {
        return err(StatusCode::NOT_FOUND, "Not casting");
    };
    let r = match b["action"].as_str().unwrap_or("") {
        "pause" => soap(&app, &s.dev.av, "AVTransport", "Pause", "<InstanceID>0</InstanceID>").await.map(|_| ()),
        "play" => soap(&app, &s.dev.av, "AVTransport", "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>").await.map(|_| ()),
        "seek" => {
            s.t = b["t"].as_f64().unwrap_or(0.0).max(0.0);
            s.started = now_ms();
            *app.cast.session.lock().unwrap() = Some(s.clone());
            load(&app, &s).await
        }
        "volume" => soap(
            &app,
            &s.dev.rc,
            "RenderingControl",
            "SetVolume",
            &format!("<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>{}</DesiredVolume>", (b["v"].as_f64().unwrap_or(0.3) * 100.0).round() as i64),
        ).await.map(|_| ()),
        "stop" => {
            // Capture final TV position before stopping so watch progress is never lost
            if let Ok(pi) = soap(&app, &s.dev.av, "AVTransport", "GetPositionInfo", "<InstanceID>0</InstanceID>").await {
                if let Some(rel) = tag(&pi, "RelTime").map(|x| hms(&x)) {
                    save_cast_progress(&app, &s, s.t + rel).await;
                }
            }
            let _ = soap(&app, &s.dev.av, "AVTransport", "Stop", "<InstanceID>0</InstanceID>").await;
            *app.cast.session.lock().unwrap() = None;
            app.stream.stop("cast stopped");
            crate::spool::detach();
            crate::livebuf::detach();
            Ok(())
        }
        _ => Err("unknown action".into()),
    };
    match r { Ok(()) => Json(json!({ "ok": true })).into_response(), Err(e) => err(StatusCode::BAD_GATEWAY, &e) }
}

fn hms(t: &str) -> f64 {
    t.trim().split(':').fold(0.0, |a, x| a * 60.0 + x.trim().parse::<f64>().unwrap_or(0.0))
}

pub async fn status(State(app): A) -> Response {
    let Some(s) = app.cast.session.lock().unwrap().clone() else {
        return Json(json!({ "casting": false })).into_response();
    };
    let (pi, ti) = tokio::join!(
        soap(&app, &s.dev.av, "AVTransport", "GetPositionInfo", "<InstanceID>0</InstanceID>"),
        soap(&app, &s.dev.av, "AVTransport", "GetTransportInfo", "<InstanceID>0</InstanceID>")
    );
    let rel = pi.ok().and_then(|x| tag(&x, "RelTime")).map(|x| hms(&x)).unwrap_or(0.0);
    let state = ti.ok().and_then(|x| tag(&x, "CurrentTransportState")).unwrap_or_else(|| "UNKNOWN".into());
    let pos = s.t + rel;

    // Periodically persist watch progress during active casting (every >= 10s, or on stop/pause)
    let now = now_ms();
    if s.kind != "live" && pos >= 5.0 && (now - s.last_saved >= 10_000 || state == "STOPPED") {
        let mut should_save = false;
        {
            let mut g = app.cast.session.lock().unwrap();
            if let Some(cur) = g.as_mut() {
                if cur.token == s.token {
                    cur.last_saved = now;
                    should_save = true;
                }
            }
        }
        if should_save {
            save_cast_progress(&app, &s, pos).await;
        }
    }

    Json(json!({ "casting": true, "device": s.dev.name, "state": state, "pos": pos, "title": s.title })).into_response()
}

/// The TV's request for the stream (home network, token + kind + id checked).
/// DLNA `HEAD` probes return immediate 200 MPEG-TS headers without spawning ffmpeg!
pub async fn lan_stream(
    method: Method,
    State(app): A,
    Path((tok, kind, file)): Path<(String, String, String)>,
    Query(mut q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let id = file.trim_end_matches(".ts").to_string();
    let ok = app.cast.session.lock().unwrap().as_ref().map_or(false, |s| {
        s.token == tok && s.kind == kind && s.id == id
    });
    if !ok {
        return (StatusCode::FORBIDDEN, "no").into_response();
    }
    if method == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "video/MP2T")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::ACCEPT_RANGES, "none")
            .header("transferMode.dlna.org", "Streaming")
            .header("contentFeatures.dlna.org", DLNA_FEATURES)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    q.insert("fmt".into(), "ts".into());
    crate::stream_route(State(app), Path((kind, id)), Query(q), headers).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_safe_xml_tag_and_service_extraction() {
        let soap_resp = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
            <s:Body>
                <u:GetPositionInfoResponse xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
                    <avt:RelTime>00:12:34</avt:RelTime>
                    <u:TrackDuration>01:45:00</u:TrackDuration>
                </u:GetPositionInfoResponse>
            </s:Body>
        </s:Envelope>"#;
        assert_eq!(tag(soap_resp, "RelTime").as_deref(), Some("00:12:34"));
        assert_eq!(hms(&tag(soap_resp, "RelTime").unwrap()), 754.0);

        let desc_xml = r#"<root xmlns:upnp="urn:schemas-upnp-org:device-1-0">
            <device>
                <friendlyName>LG OLED C3 &amp; Living Room</friendlyName>
                <serviceList>
                    <upnp:service>
                        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
                        <controlURL>/upnp/control/avt</controlURL>
                    </upnp:service>
                    <service id="cm">
                        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
                        <controlURL>/upnp/control/cm</controlURL>
                    </service>
                </serviceList>
            </device>
        </root>"#;
        let blocks = extract_service_blocks(desc_xml);
        assert_eq!(blocks.len(), 2, "must not match <serviceList> and must handle namespaced <upnp:service>");
        assert_eq!(tag(blocks[0], "controlURL").as_deref(), Some("/upnp/control/avt"));
        assert_eq!(tag(blocks[1], "controlURL").as_deref(), Some("/upnp/control/cm"));
        assert_eq!(unesc(&tag(desc_xml, "friendlyName").unwrap()), "LG OLED C3 & Living Room");
    }
}
