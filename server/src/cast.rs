// Cast to a TV over DLNA/UPnP (Samsung, LG, Sony, Panasonic, Philips, Android TV renderers...).
// Discovery: SSDP M-SEARCH for MediaRenderer:1, then the device description; only renderers whose
// ConnectionManager sink list includes video are offered (Sonos/WiiM speakers are filtered out).
// Playback: the TV pulls an MPEG-TS stream (most compatible) from this PC's home-network listener
// (port 7880) under a random per-session token; same ffmpeg pipeline and single provider connection.
// Chromecast-type devices are handled by the browser's own casting (Remote Playback) on the TV page.
use crate::app::App;
use crate::util::now_ms;
use axum::{extract::{Path, Query, State}, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

type A = State<Arc<App>>;

#[derive(Clone, Debug)]
pub struct Device { pub id: String, pub name: String, pub model: String, pub av: String, pub rc: String }

#[derive(Clone, Debug)]
pub struct Session { pub dev: Device, pub token: String, pub kind: String, pub id: String, pub params: HashMap<String, String>, pub t: f64, pub title: String, pub started: i64 }

#[derive(Default)]
pub struct Cast { devices: Mutex<(i64, Vec<Device>)>, pub session: Mutex<Option<Session>> }

fn err(code: StatusCode, m: &str) -> Response { (code, Json(json!({ "error": m }))).into_response() }
fn tag(x: &str, t: &str) -> Option<String> { let a = x.find(&format!("<{t}"))?; let b = a + x[a..].find('>')? + 1; let e = b + x[b..].find(&format!("</{t}>"))?; Some(x[b..e].trim().to_string()) }
fn xml_esc(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;") }
fn unesc(s: &str) -> String { s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&") }

fn ssdp(wait_ms: u64) -> Vec<String> {
    let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") else { return vec![] };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(300)));
    let msg = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
    for _ in 0..2 { let _ = s.send_to(msg.as_bytes(), "239.255.255.250:1900"); }
    let until = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    let mut locs = vec![]; let mut buf = [0u8; 2048];
    while std::time::Instant::now() < until {
        if let Ok((n, _)) = s.recv_from(&mut buf) {
            let t = String::from_utf8_lossy(&buf[..n]).to_string();
            if let Some(l) = t.lines().find(|l| l.to_ascii_lowercase().starts_with("location:")) { let l = l[9..].trim().to_string(); if !locs.contains(&l) { locs.push(l); } }
        }
    }
    locs
}

fn abs(base: &str, path: &str) -> String {
    if path.starts_with("http") { return path.to_string(); }
    let root = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    format!("{root}{}{path}", if path.starts_with('/') { "" } else { "/" })
}

pub async fn soap(app: &App, url: &str, service: &str, action: &str, args: &str) -> Result<String, String> {
    let body = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"urn:schemas-upnp-org:service:{service}:1\">{args}</u:{action}></s:Body></s:Envelope>");
    let r = app.http.post(url).header("Content-Type", "text/xml; charset=\"utf-8\"").header("SOAPACTION", format!("\"urn:schemas-upnp-org:service:{service}:1#{action}\""))
        .body(body).timeout(std::time::Duration::from_secs(6)).send().await.map_err(|e| e.to_string())?;
    let ok = r.status().is_success(); let t = r.text().await.unwrap_or_default();
    if ok { Ok(t) } else { Err(tag(&t, "errorDescription").unwrap_or_else(|| "the TV refused the request".into())) }
}

async fn describe(app: &App, loc: &str) -> Option<Device> {
    let x = app.http.get(loc).timeout(std::time::Duration::from_secs(3)).send().await.ok()?.text().await.ok()?;
    let base = tag(&x, "URLBase").unwrap_or_else(|| loc.to_string());
    let mut av = None; let mut cm = None; let mut rc = None;
    for blk in x.split("<service>").skip(1) {
        let ty = tag(blk, "serviceType").unwrap_or_default(); let ctl = tag(blk, "controlURL").unwrap_or_default();
        if ty.contains("AVTransport") { av = Some(abs(&base, &ctl)); } else if ty.contains("ConnectionManager") { cm = Some(abs(&base, &ctl)); } else if ty.contains("RenderingControl") { rc = Some(abs(&base, &ctl)); }
    }
    let (av, cm) = (av?, cm?);
    // video renderers only
    let sink = soap(app, &cm, "ConnectionManager", "GetProtocolInfo", "").await.ok().and_then(|t| tag(&t, "Sink")).unwrap_or_default();
    if !sink.contains("video/") { return None; }
    Some(Device { id: tag(&x, "UDN").unwrap_or_else(|| loc.to_string()), name: unesc(&tag(&x, "friendlyName").unwrap_or_else(|| "TV".into())), model: unesc(&tag(&x, "modelName").unwrap_or_default()), av, rc: rc.unwrap_or_default() })
}

pub async fn devices(State(app): A) -> Response {
    let cached = app.cast.devices.lock().unwrap().clone();
    let list = if now_ms() - cached.0 < 60_000 && !cached.1.is_empty() { cached.1 } else {
        let locs = tokio::task::spawn_blocking(|| ssdp(2500)).await.unwrap_or_default();
        let found: Vec<Device> = futures_util::future::join_all(locs.iter().map(|l| describe(&app, l))).await.into_iter().flatten().collect();
        let mut uniq: Vec<Device> = vec![]; for d in found { if !uniq.iter().any(|u| u.id == d.id) { uniq.push(d); } }
        *app.cast.devices.lock().unwrap() = (now_ms(), uniq.clone()); uniq
    };
    Json(json!({ "devices": list.iter().map(|d| json!({ "id": d.id, "name": d.name, "model": d.model })).collect::<Vec<_>>(), "lan": crate::remote::lan_ip() })).into_response()
}

fn stream_url(s: &Session) -> Option<String> {
    let ip = std::env::var("TV_CAST_HOST").ok().or_else(crate::remote::lan_ip)?; // override for tests
    let mut q: Vec<String> = s.params.iter().filter(|(k, _)| k.as_str() != "t").map(|(k, v)| format!("{}={}", k, crate::util::enc(v))).collect();
    q.push(format!("t={}", s.t.floor())); q.push("fmt=ts".into());
    Some(format!("http://{ip}:{}/c/{}/{}/{}.ts?{}", crate::remote::PORT, s.token, s.kind, s.id, q.join("&")))
}

async fn load(app: &App, s: &Session) -> Result<(), String> {
    let url = stream_url(s).ok_or("This PC isn't on a home network")?;
    let meta = format!("<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\"><item id=\"1\" parentID=\"0\" restricted=\"1\"><dc:title>{}</dc:title><upnp:class>object.item.videoItem.movie</upnp:class><res protocolInfo=\"http-get:*:video/mpeg:DLNA.ORG_OP=00;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000\">{}</res></item></DIDL-Lite>", xml_esc(&s.title), xml_esc(&url));
    let _ = soap(app, &s.dev.av, "AVTransport", "Stop", "<InstanceID>0</InstanceID>").await;
    soap(app, &s.dev.av, "AVTransport", "SetAVTransportURI", &format!("<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>", xml_esc(&url), xml_esc(&meta))).await?;
    soap(app, &s.dev.av, "AVTransport", "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>").await?;
    Ok(())
}

/// Start casting {device, kind, id, t, title, params{ext,a,sid,cs,dur,...}}.
pub async fn start(State(app): A, Json(b): Json<Value>) -> Response {
    let dev_id = b["device"].as_str().unwrap_or("");
    let Some(dev) = app.cast.devices.lock().unwrap().1.iter().find(|d| d.id == dev_id).cloned() else { return err(StatusCode::NOT_FOUND, "That TV isn't available any more. Look for TVs again.") };
    crate::remote::ensure_listener(app.clone());
    use rand::Rng; let token: String = (0..24).map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16u8))).collect();
    let params: HashMap<String, String> = b["params"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), crate::util::s(v))).collect()).unwrap_or_default();
    let s = Session { dev, token, kind: crate::util::s(&b["kind"]), id: crate::util::s(&b["id"]), params, t: b["t"].as_f64().unwrap_or(0.0), title: crate::util::s(&b["title"]), started: now_ms() };
    if !["movie", "series", "live", "catchup"].contains(&s.kind.as_str()) { return err(StatusCode::BAD_REQUEST, "kind"); }
    app.stream.stop("casting");
    *app.cast.session.lock().unwrap() = Some(s.clone());
    match load(&app, &s).await { Ok(()) => Json(json!({ "ok": true, "device": s.dev.name })).into_response(), Err(e) => { *app.cast.session.lock().unwrap() = None; err(StatusCode::BAD_GATEWAY, &format!("{} didn't start playback: {e}", s.dev.name)) } }
}

pub async fn control(State(app): A, Json(b): Json<Value>) -> Response {
    let Some(mut s) = app.cast.session.lock().unwrap().clone() else { return err(StatusCode::NOT_FOUND, "Not casting") };
    let r = match b["action"].as_str().unwrap_or("") {
        "pause" => soap(&app, &s.dev.av, "AVTransport", "Pause", "<InstanceID>0</InstanceID>").await.map(|_| ()),
        "play" => soap(&app, &s.dev.av, "AVTransport", "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>").await.map(|_| ()),
        "seek" => { s.t = b["t"].as_f64().unwrap_or(0.0).max(0.0); s.started = now_ms(); *app.cast.session.lock().unwrap() = Some(s.clone()); load(&app, &s).await }
        "volume" => soap(&app, &s.dev.rc, "RenderingControl", "SetVolume", &format!("<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>{}</DesiredVolume>", (b["v"].as_f64().unwrap_or(0.3) * 100.0).round() as i64)).await.map(|_| ()),
        "stop" => { let _ = soap(&app, &s.dev.av, "AVTransport", "Stop", "<InstanceID>0</InstanceID>").await; *app.cast.session.lock().unwrap() = None; app.stream.stop("cast stopped"); Ok(()) }
        _ => Err("unknown action".into()),
    };
    match r { Ok(()) => Json(json!({ "ok": true })).into_response(), Err(e) => err(StatusCode::BAD_GATEWAY, &e) }
}

fn hms(t: &str) -> f64 { t.split(':').fold(0.0, |a, x| a * 60.0 + x.parse::<f64>().unwrap_or(0.0)) }
pub async fn status(State(app): A) -> Response {
    let Some(s) = app.cast.session.lock().unwrap().clone() else { return Json(json!({ "casting": false })).into_response() };
    let (pi, ti) = tokio::join!(soap(&app, &s.dev.av, "AVTransport", "GetPositionInfo", "<InstanceID>0</InstanceID>"), soap(&app, &s.dev.av, "AVTransport", "GetTransportInfo", "<InstanceID>0</InstanceID>"));
    let rel = pi.ok().and_then(|x| tag(&x, "RelTime")).map(|x| hms(&x)).unwrap_or(0.0);
    let state = ti.ok().and_then(|x| tag(&x, "CurrentTransportState")).unwrap_or_else(|| "UNKNOWN".into());
    Json(json!({ "casting": true, "device": s.dev.name, "state": state, "pos": s.t + rel, "title": s.title })).into_response()
}

/// The TV's request for the stream (home network, token-checked).
pub async fn lan_stream(State(app): A, Path((tok, kind, file)): Path<(String, String, String)>, Query(mut q): Query<HashMap<String, String>>) -> Response {
    let ok = app.cast.session.lock().unwrap().as_ref().map_or(false, |s| s.token == tok && s.kind == kind);
    if !ok { return (StatusCode::FORBIDDEN, "no").into_response(); }
    q.insert("fmt".into(), "ts".into());
    let id = file.trim_end_matches(".ts").to_string();
    crate::stream_route(State(app), Path((kind, id)), Query(q), axum::http::HeaderMap::new()).await
}
