// Pro player: hands a title to mpv (gpu-next/libplacebo, true OLED infinite contrast, exact HDR /
// Dolby Vision & gamma 2.2 SDR colour, GPU upscaling, display-synced motion). mpv reads the
// provider directly over the single connection, so the browser stream is stopped first. Progress,
// intro-skipping, phone remote control, and auto-next-episode run over mpv's IPC pipe; when mpv
// closes, the display goes back to its own settings.
use crate::app::App;
use axum::{extract::State, response::Response, Json};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const PIPE: &str = r"\\.\pipe\tv-mpv";

pub struct Session {
    pub gen: u64,
    pub stream_id: i64,
    pub child: std::process::Child,
    pub key: String,
    pub url: String,
    pub used_spool: bool,
    pub body: Value,
    pub start_pos: f64,
    pub pos: f64,
    pub dur: f64,
    pub title: String,
    pub live: bool,
    pub skips: Vec<(f64, f64, String)>,
    pub credits: Option<f64>,
    pub autoskip: bool,
    pub skipped: Vec<usize>,
    pub next_triggered: bool,
    pub has_osd: bool,
    pub health_reported: bool,
}

pub struct Mpv {
    pub s: Mutex<Option<Session>>,
    gen: std::sync::atomic::AtomicU64,
    install: Mutex<Option<String>>,
    pub last_done: Mutex<Option<Value>>,
}
impl Mpv {
    pub fn new() -> Self {
        Mpv {
            s: Mutex::new(None),
            gen: Default::default(),
            install: Mutex::new(None),
            last_done: Mutex::new(None),
        }
    }
}

pub fn running(app: &App) -> bool {
    let mut g = app.mpv.s.lock().unwrap();
    match g.as_mut() {
        Some(s) => match s.child.try_wait() {
            Ok(None) => true,
            Ok(Some(st)) => {
                if st.code() == Some(42) { s.next_triggered = true; }
                false
            }
            Err(_) => false,
        },
        None => false,
    }
}

/// mpv.exe: cfg "mpvPath", the app folder (mpv\mpv.exe), PATH, then the usual install places.
pub fn find(app: &App) -> Option<PathBuf> {
    let c = app.cfg.str("mpvPath");
    if !c.is_empty() && std::path::Path::new(&c).exists() { return Some(c.into()); }
    let root = app.data.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let mut cands = vec![
        root.join("mpv").join("mpv.exe"),
        PathBuf::from(&home).join("scoop").join("apps").join("mpv").join("current").join("mpv.exe"),
        PathBuf::from(r"C:\Program Files\MPV Player\mpv.exe"),
        PathBuf::from(r"C:\Program Files (x86)\MPV Player\mpv.exe"),
        PathBuf::from(r"C:\Program Files\mpv\mpv.exe"),
        PathBuf::from(&local).join("Programs").join("mpv").join("mpv.exe"),
        PathBuf::from(&local).join("Microsoft").join("WinGet").join("Links").join("mpv.exe"),
    ];
    if let Ok(path) = std::env::var("PATH") {
        for d in path.split(';') { cands.push(PathBuf::from(d).join("mpv.exe")); }
    }
    if let Ok(rd) = std::fs::read_dir(PathBuf::from(&local).join("Microsoft").join("WinGet").join("Packages")) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().to_lowercase().contains("mpv") {
                cands.push(e.path().join("mpv.exe"));
                if let Ok(r2) = std::fs::read_dir(e.path()) {
                    for f in r2.flatten() { cands.push(f.path().join("mpv.exe")); }
                }
            }
        }
    }
    cands.into_iter().find(|p| p.is_file())
}

/// One IPC request (blocking): {"command": [...]} -> "data".
fn ipc(cmd: Value) -> Option<Value> {
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(PIPE).ok()?;
    let req = json!({ "command": cmd, "request_id": 7 });
    f.write_all(format!("{req}\n").as_bytes()).ok()?;
    let mut r = BufReader::new(f);
    for _ in 0..50 {
        let mut l = String::new();
        if r.read_line(&mut l).ok()? == 0 { return None; }
        let v: Value = serde_json::from_str(&l).ok()?;
        if v["request_id"] == 7 {
            return if v["error"] == "success" { Some(v["data"].clone()) } else { None };
        }
    }
    None
}

/// Forward a remote / tray / API control command directly to mpv over IPC when mpv is active.
pub fn control_sync(app: &App, cmd: &str, v: Option<&Value>) -> bool {
    if !running(app) { return false; }
    match cmd {
        "toggle" | "pause" => ipc(json!(["cycle", "pause"])).is_some(),
        "play" => ipc(json!(["set_property", "pause", false])).is_some(),
        "seek" => {
            let d = v.and_then(|x| x.as_f64()).unwrap_or(10.0);
            ipc(json!(["seek", d, "relative"])).is_some()
        }
        "seekto" => {
            let t = v.and_then(|x| x.as_f64()).unwrap_or(0.0).max(0.0);
            ipc(json!(["seek", t, "absolute"])).is_some()
        }
        "vol" => {
            let vol = v.and_then(|x| x.as_f64()).unwrap_or(1.0);
            let pct = if vol <= 1.5 { (vol * 100.0).clamp(0.0, 130.0) } else { vol.clamp(0.0, 130.0) };
            ipc(json!(["set_property", "volume", pct])).is_some()
        }
        "mute" => ipc(json!(["cycle", "mute"])).is_some(),
        "subs" => ipc(json!(["cycle", "sub"])).is_some(),
        "audio" => ipc(json!(["cycle", "audio"])).is_some(),
        "full" => ipc(json!(["cycle", "fullscreen"])).is_some(),
        "skip" => {
            let target = app.mpv.s.lock().unwrap().as_ref().and_then(|s| {
                s.skips.iter().find(|(st, en, _)| s.pos >= *st - 2.0 && s.pos < *en - 2.0).map(|(_, en, _)| *en)
            });
            if let Some(en) = target {
                let _ = ipc(json!(["show-text", "Skipped intro", 2500]));
                ipc(json!(["seek", en, "absolute"])).is_some()
            } else {
                ipc(json!(["seek", 85.0, "relative"])).is_some()
            }
        }
        "close" | "stop" => ipc(json!(["quit"])).is_some(),
        "osd" => {
            let msg = v.and_then(|x| x.as_str()).unwrap_or("");
            if msg.is_empty() { false } else { ipc(json!(["show-text", msg, 3000])).is_some() }
        }
        _ => false,
    }
}

pub async fn control_route(State(app): State<Arc<App>>, Json(b): Json<Value>) -> Response {
    let cmd = b["cmd"].as_str().unwrap_or("").to_string();
    let v = b.get("v").cloned();
    let a = app.clone();
    let ok = tokio::task::spawn_blocking(move || control_sync(&a, &cmd, v.as_ref())).await.unwrap_or(false);
    crate::j(json!({ "ok": ok }))
}

pub async fn info(State(app): State<Arc<App>>) -> Response {
    let p = find(&app);
    let s = app.mpv.s.lock().unwrap().as_ref().map(|s| json!({
        "gen": s.gen, "title": s.title, "pos": s.pos, "dur": s.dur, "key": s.key, "live": s.live
    }));
    let last_done = app.mpv.last_done.lock().unwrap().clone();
    crate::j(json!({
        "available": p.is_some(),
        "path": p.map(|x| x.to_string_lossy().to_string()),
        "playing": running(&app),
        "session": s,
        "lastDone": last_done,
        "installError": app.mpv.install.lock().unwrap().clone()
    }))
}

/// POST /api/mpv/play {kind,id,ext,t,a,sub,title,dur,fps,hdr,dv,bits,h,w,ch,sc,night,deint,isAnime,
///   skips,credits,autoskip,audioDelay,subDelay,progress:{key,item,tmdb,historyKind,historyTitle}}
pub async fn play(State(app): State<Arc<App>>, Json(b): Json<Value>) -> Response {
    let Some(exe) = find(&app) else { return crate::err(404, "The pro player (mpv) isn't installed") };
    stop_inner(&app, false).await;
    app.stream.stop("pro player");
    crate::livebuf::detach();
    crate::yield_background(&app).await;

    let kind = b["kind"].as_str().unwrap_or("movie").to_string();
    let live = kind == "live";
    let id = crate::util::s(&b["id"]);
    let url = if kind == "catchup" {
        crate::guide::timeshift_url(&app, id.parse().unwrap_or(0), b["cs"].as_i64().unwrap_or(0), b["durMin"].as_i64().unwrap_or(60))
    } else if !live {
        if let Ok(test_url) = std::env::var("TV_TEST_SOURCE") {
            if !test_url.is_empty() { test_url } else { crate::upstream(&app, &kind, &id, b["ext"].as_str().map(String::from).as_ref(), false) }
        } else {
            crate::upstream(&app, &kind, &id, b["ext"].as_str().map(String::from).as_ref(), false)
        }
    } else if let Ok(test_live) = std::env::var("TV_TEST_LIVE_SOURCE") {
        if !test_live.is_empty() { test_live } else { crate::upstream(&app, &kind, &id, b["ext"].as_str().map(String::from).as_ref(), false) }
    } else {
        crate::upstream(&app, &kind, &id, b["ext"].as_str().map(String::from).as_ref(), false)
    };

    // If /api/probe just warmed up the read-ahead spool for this exact title, reuse it directly!
    // That eliminates destroying 15-30 MB of already-buffered 4K data, eliminates the 1.5s slot
    // release wait, eliminates a second provider 302+TTFB roundtrip, and keeps mpv on IPv4!
    let warm_spool = if !live && kind != "catchup" { crate::spool::local_for(&url) } else { None };
    let used_spool = warm_spool.is_some();
    if used_spool {
        let dur_hint = b["dur"].as_f64().unwrap_or(0.0);
        if dur_hint > 60.0 { crate::spool::set_duration(&url, dur_hint); }
    } else {
        crate::spool::detach();
    }
    let play_url = warm_spool.unwrap_or_else(|| url.clone());

    let t = if live { 0.0 } else { b["t"].as_f64().unwrap_or(0.0) };
    let title = b["title"].as_str().unwrap_or("").to_string();
    let fps = b["fps"].as_f64().unwrap_or(0.0);
    let hdr = b["hdr"].as_bool().unwrap_or(false);
    let dv = b["dv"].as_i64().unwrap_or(0);
    let bits = b["bits"].as_i64().unwrap_or(if hdr { 10 } else { 8 });
    let vh = b["h"].as_i64().unwrap_or(0);
    let vw = b["w"].as_i64().unwrap_or(0);
    let ch = b["ch"].as_i64().unwrap_or(2);
    let sc = b["sc"].as_bool().unwrap_or(false);
    let night = b["night"].as_bool().unwrap_or(false);
    let deint = live && b["deint"].as_bool().unwrap_or(false);
    let is_anime = b["isAnime"].as_bool().unwrap_or(false);
    let audio_delay = b["audioDelay"].as_f64().unwrap_or(0.0);
    let sub_delay = b["subDelay"].as_f64().unwrap_or(0.0);
    let sub_label = b["subTitle"].as_str().unwrap_or("").to_string();
    let has_next = b["hasNext"].as_bool().unwrap_or(false);
    let next_label = b["nextLabel"].as_str().unwrap_or("").to_string();
    let autoplay = b["autoplay"].as_bool().unwrap_or(true);
    let key = b["progress"]["key"].as_str().unwrap_or("").to_string();

    let (oled_peak, mpv_vsr, mpv_anime_sharp, mpv_shaders) = app.state.read(|st| (
        st["prefs"]["oledPeak"].as_str().unwrap_or("auto").to_string(),
        st["prefs"]["mpvVsr"].as_bool().unwrap_or(false),
        st["prefs"]["mpvAnimeSharp"].as_bool().unwrap_or(true),
        st["prefs"]["mpvShaders"].as_str().unwrap_or("auto").to_string(),
    ));

    // Match OLED refresh rate + True Black HDR/SDR switch, overlapping any provider slot cooldown
    // with the display settle window so 4K startup doesn't double-sleep 3+ seconds sequentially.
    let dm = crate::display::match_for(&app, fps, hdr).await;
    let changed_arr = dm["changed"].as_array();
    let hdr_toggled = changed_arr.map_or(false, |arr| arr.iter().any(|x| x.as_str().map_or(false, |s| s.contains("HDR"))));
    let hz_changed = changed_arr.map_or(false, |arr| arr.iter().any(|x| x.as_str().map_or(false, |s| s.ends_with("Hz"))));
    let disp_wait_ms: i64 = if hdr_toggled { 1200 } else if hz_changed { 350 } else { 0 };
    let slot_wait_ms: i64 = if used_spool { 0 } else { (1500 - (crate::util::now_ms() - app.stream.last_stop())).max(0) };
    let settle_ms = disp_wait_ms.max(slot_wait_ms);
    if settle_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(settle_ms as u64)).await;
    }
    let gdi = dm["gdi"].as_str().unwrap_or("").to_string();
    let hdr_now = dm["hdrOn"].as_bool().unwrap_or(hdr);

    let use_vsr = mpv_vsr && !hdr && dv == 0 && (live || (vh > 0 && vh < 1800));
    let is_native_4k = vh >= 1800 || vw >= 3600;
    let shader_cache_dir = app.data.join("mpv-shader-cache");
    let _ = std::fs::create_dir_all(&shader_cache_dir);
    let mut a: Vec<String> = vec![
        play_url,
        "--no-config".into(),
        "--fs".into(),
        "--force-window=immediate".into(),
        "--msg-level=all=error,cplayer=warn".into(),
        "--gpu-shader-cache=yes".into(),
        format!("--gpu-shader-cache-dir={}", shader_cache_dir.to_string_lossy()),
        "--demuxer-mkv-probe-start-time=no".into(),
        format!("--title={title}"),
        format!("--force-media-title={title}"),
        "--vo=gpu-next".into(),
        "--gpu-api=d3d11".into(),
        "--profile=high-quality".into(),
        if use_vsr { "--hwdec=d3d11va".into() } else { "--hwdec=auto-safe".into() },
        // Hardware-fused bilinear chroma scaling avoids extra 4K VRAM passes (polar ewa_lanczossharp
        // or 2-pass spline36 spike 20-30ms when the GPU downclocks to P3/P5); native 4K luma is 1:1.
        "--cscale=bilinear".into(),
        "--dscale=bilinear".into(),
        if is_native_4k { "--scale=bilinear".into() } else { "--scale=ewa_lanczossharp".into() },
        // OLED True Black core: infinite contrast (0-nit black floor) + full 0-255 range
        "--target-contrast=inf".into(),
        "--video-output-levels=full".into(),
        "--background-color=#000000".into(),
        // Audio-clock sync + 3-frame swapchain prevents mixed-Hz multi-monitor DWM focus jitter
        // from snowballing into frame drops while keeping crisp 240Hz / integer cadence.
        "--video-sync=audio".into(),
        "--swapchain-depth=3".into(),
        "--interpolation=no".into(),
        format!("--user-agent={}", app.cfg.str("userAgent")),
        format!("--input-ipc-server={PIPE}"),
        "--keep-open=no".into(),
        "--save-position-on-quit=no".into(),
        "--cache=yes".into(),
        format!("--demuxer-max-bytes={}", if live || used_spool { "256MiB" } else { "1024MiB" }),
        format!("--demuxer-max-back-bytes={}", if live || used_spool { "128MiB" } else { "256MiB" }),
        format!("--demuxer-readahead-secs={}", if live || used_spool { "30" } else { "180" }),
        format!("--cache-secs={}", if live || used_spool { "30" } else { "180" }),
        "--stream-lavf-o=reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_delay_max=5".into(),
        "--audio-channels=auto".into(),
        "--osd-font-size=36".into(),
        "--osd-bar=yes".into(),
        "--cursor-autohide=1000".into(),
        "--alang=en,eng".into(),
    ];

    // Pin fullscreen to the configured OLED monitor
    if !gdi.is_empty() {
        a.push(format!("--fs-screen-name={gdi}"));
    }

    // HDR vs SDR OLED black-level pipeline:
    //  - Never run --deband=yes with default grain (32) — grain destroys #000000 pixel-off OLED blacks!
    //  - HDR/DV on HDR display: native 10-bit PQ swapchain + source hint + bt.2390 + infinite contrast.
    //  - SDR: pure power-law gamma 2.2 (never lifted Windows sRGB piecewise!) + grain-free deband on 8-bit.
    if hdr && hdr_now {
        a.push("--d3d11-output-csp=pq".into());
        a.push("--d3d11-output-format=rgb10_a2".into());
        a.push("--target-colorspace-hint=yes".into());
        // Use static 'target' hint mode so Windows DWM does NOT drop per-app SetHDRMetaData on focus loss
        // (prevents visible brightness/contrast snap when clicking onto a second monitor!).
        a.push("--target-colorspace-hint-mode=target".into());
        a.push("--target-trc=pq".into());
        a.push("--target-prim=bt.2020".into());
        if dv > 0 {
            // Dolby Vision already supplies per-frame dynamic metadata; skip redundant 4K compute passes.
            a.push("--hdr-compute-peak=no".into());
        } else {
            a.push("--hdr-compute-peak=auto".into());
            a.push("--allow-delayed-peak-detect=yes".into());
        }
        a.push("--hdr-contrast-recovery=0".into());
        a.push("--tone-mapping=bt.2390".into());
        a.push("--gamut-mapping-mode=perceptual".into());
        if !oled_peak.is_empty() && oled_peak != "auto" {
            a.push(format!("--target-peak={oled_peak}"));
        }
        a.push("--deband=no".into());
    } else if hdr {
        // HDR title when user disabled Windows HDR: tone-map cleanly to OLED SDR gamma 2.2
        a.push("--target-colorspace-hint=no".into());
        a.push("--d3d11-output-csp=srgb".into());
        a.push("--target-trc=gamma2.2".into());
        if dv > 0 {
            a.push("--hdr-compute-peak=no".into());
        } else {
            a.push("--hdr-compute-peak=auto".into());
            a.push("--allow-delayed-peak-detect=yes".into());
        }
        a.push("--hdr-contrast-recovery=0".into());
        a.push("--tone-mapping=bt.2390".into());
        a.push("--gamut-mapping-mode=perceptual".into());
        a.push("--deband=no".into());
    } else {
        a.push("--target-colorspace-hint=no".into());
        a.push("--d3d11-output-csp=srgb".into());
        a.push("--target-trc=gamma2.2".into());
        if bits <= 8 {
            a.push("--deband=yes".into());
            a.push("--deband-grain=0".into());
        } else {
            a.push("--deband=no".into());
        }
        // GPU upscaling & live deinterlace
        if use_vsr {
            if deint {
                a.push("--vf-append=d3d11vpp=deint=yes:mode=bob:scale=2:scaling-mode=nvidia".into());
            } else {
                a.push("--vf-append=d3d11vpp=scale=2:scaling-mode=nvidia".into());
            }
        } else {
            if deint {
                a.push("--vf-append=lavfi=[bwdif=mode=send_field]".into());
            }
            if is_anime && mpv_anime_sharp {
                a.push("--scale=ewa_lanczossharp".into());
                a.push("--cscale=bilinear".into());
                a.push("--sharpen=0.15".into());
            }
        }
    }

    // Parse intro/recap/credits skips BEFORE spawning mpv so we can pass them into tv_osd.lua!
    let skips: Vec<(f64, f64, String)> = b["skips"].as_array().map(|arr| {
        arr.iter().filter_map(|x| {
            let st = x["s"].as_f64()?;
            let en = x["e"].as_f64()?;
            Some((st, en, x["title"].as_str().unwrap_or("Intro").to_string()))
        }).collect()
    }).unwrap_or_default();
    let credits = b["credits"].as_f64().filter(|x| *x > 30.0);
    let credits_end = b["creditsEnd"].as_f64().filter(|x| *x > 35.0);
    let has_post_credits = b["hasPostCredits"].as_bool().unwrap_or(false);
    let autoskip = b["autoskip"].as_bool().unwrap_or(true);
    let autoskip_credits = b["autoskipCredits"].as_bool().unwrap_or(true);

    // Custom pure-black OLED Lua OSD + neural GPU shader chain
    let root = app.data.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let osd_script = root.join("mpv-scripts").join("tv_osd.lua");
    let shader_dir = root.join("mpv-shaders");
    let has_osd = osd_script.is_file();
    let shader_preset = if !shader_dir.is_dir() || mpv_shaders == "off" || use_vsr || is_native_4k {
        "off"
    } else if is_anime {
        if mpv_shaders == "anime_hq" { "anime_hq" } else { "anime" }
    } else if mpv_shaders == "auto" && (vh > 0 && vh < 1800) {
        "film"
    } else {
        "off"
    };

    if has_osd {
        a.push("--osc=no".into());
        a.push(format!("--script={}", osd_script.to_string_lossy()));
        let mut add_opt = |k: &str, v: String| {
            // Avoid breaking mpv's script-opts parser on commas/newlines
            let clean = v.replace('\r', " ").replace('\n', " ").replace(',', " ·");
            a.push(format!("--script-opts-append=tv_osd-{k}={clean}"));
        };
        add_opt("title", title.clone());
        add_opt("sub", sub_label.clone());
        // Combined "Recap + Intro" segment goes to intro_s/intro_e with custom label so mpv performs one seamless skip.
        let combined = skips.iter().find(|(_, _, l)| {
            let ll = l.to_lowercase();
            ll.contains("recap") && (ll.contains("intro") || ll.contains("opening"))
        });
        if let Some((cst, cen, clab)) = combined {
            add_opt("intro_s", format!("{cst:.2}"));
            add_opt("intro_e", format!("{cen:.2}"));
            add_opt("intro_label", clab.clone());
        } else {
            if let Some((rst, ren, _)) = skips.iter().find(|(_, _, l)| {
                let ll = l.to_lowercase(); ll.contains("recap") || ll.contains("previously")
            }) {
                add_opt("recap_s", format!("{rst:.2}"));
                add_opt("recap_e", format!("{ren:.2}"));
            }
            if let Some((ist, ien, ilab)) = skips.iter().find(|(_, _, l)| {
                let ll = l.to_lowercase(); ll.contains("intro") || ll.contains("opening") || ll == "op"
            }) {
                add_opt("intro_s", format!("{ist:.2}"));
                add_opt("intro_e", format!("{ien:.2}"));
                if is_anime && ilab.eq_ignore_ascii_case("intro") {
                    add_opt("intro_label", "Opening".into());
                } else {
                    add_opt("intro_label", ilab.clone());
                }
            }
        }
        if let Some(cr) = credits { add_opt("credits", format!("{cr:.2}")); }
        if let Some(ce) = credits_end { add_opt("credits_end", format!("{ce:.2}")); }
        add_opt("has_post_credits", if has_post_credits { "yes" } else { "no" }.into());
        add_opt("is_anime", if is_anime { "yes" } else { "no" }.into());
        add_opt("has_next", if has_next { "yes" } else { "no" }.into());
        add_opt("next_label", next_label.clone());
        add_opt("autoplay", if autoplay { "yes" } else { "no" }.into());
        add_opt("autoskip", if autoskip { "yes" } else { "no" }.into());
        add_opt("autoskip_credits", if autoskip_credits { "yes" } else { "no" }.into());
        add_opt("shader_dir", shader_dir.to_string_lossy().to_string());
        add_opt("shader_preset", shader_preset.into());
        add_opt("night", if night { "yes" } else { "no" }.into());
        add_opt("is_live", if live { "yes" } else { "no" }.into());
        let port = std::env::var("TV_PORT").ok().unwrap_or_else(|| app.cfg.get("port").as_u64().unwrap_or(7878).to_string());
        add_opt("api_base", format!("http://127.0.0.1:{port}"));
    }

    // Audio parity: dialogue-preserving 5.1->stereo downmix + Clear Voice / Night Mode
    if ch >= 6 && !sc {
        a.push("--af-append=lavfi=[pan=stereo|FL=0.5*FC+0.36*FL+0.25*BL+0.25*SL+0.15*LFE|FR=0.5*FC+0.36*FR+0.25*BR+0.25*SR+0.15*LFE]".into());
    }
    // When tv_osd.lua is loaded, it manages @tv_night live via tv_osd-night so toggling Voice never duplicates filters!
    if night && !has_osd {
        a.push("--af-append=lavfi=[dynaudnorm=f=150:g=15:p=0.90:m=8.0:s=12.0]".into());
    }
    if audio_delay.abs() > 1.0 {
        a.push(format!("--audio-delay={:.3}", audio_delay / 1000.0));
    }
    if sub_delay.abs() > 1.0 {
        a.push(format!("--sub-delay={:.3}", sub_delay / 1000.0));
    }

    if t > 1.0 { a.push(format!("--start={t}")); }
    if let Some(ai) = b["a"].as_i64() {
        if !live { a.push(format!("--aid={}", ai + 1)); }
    }
    // Auto-load downloaded external English subtitle if cached for this title/episode!
    let ext_srt = if !key.is_empty() { crate::extsubs::cached_srt_path(&app, &key) } else { None };
    let use_ext_sub = b["useExtSub"].as_bool().unwrap_or(false);
    if let Some(ref sp) = ext_srt {
        a.push(format!("--sub-file={}", sp.to_string_lossy()));
    }
    match b["sub"].as_i64() {
        Some(si) if si >= 0 => a.push(format!("--sid={}", si + 1)),
        _ if use_ext_sub && ext_srt.is_some() => a.push("--sid=auto".into()),
        _ if is_anime => {
            // Auto-select English Forced / Signs & Songs subtitles on dubbed anime!
            a.push("--slang=en,eng".into());
            a.push("--subs-with-matching-audio=forced".into());
        }
        _ => a.push("--sid=no".into()),
    }

    let err_log = app.data.join("mpv-last.log");
    let stderr_io = std::fs::File::create(&err_log)
        .map(std::process::Stdio::from)
        .unwrap_or_else(|_| std::process::Stdio::null());
    let mut cmd = std::process::Command::new(exe);
    cmd.args(&a).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr_io);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return crate::err(500, &format!("mpv failed to start: {e}")),
    };

    let stream_id = id.parse::<i64>().unwrap_or(0);
    let gen = app.mpv.gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    *app.mpv.last_done.lock().unwrap() = None;
    *app.mpv.s.lock().unwrap() = Some(Session {
        gen,
        stream_id,
        child,
        key,
        url,
        used_spool,
        body: b["progress"].clone(),
        start_pos: t,
        pos: t,
        dur: b["dur"].as_f64().unwrap_or(0.0),
        title: title.clone(),
        live,
        skips,
        credits,
        autoskip,
        skipped: vec![],
        next_triggered: false,
        has_osd,
        health_reported: false,
    });
    println!("[mpv] playing {kind}:{id} from {t:.0}s (hdr={hdr} hdr_now={hdr_now} spool={used_spool} shader={shader_preset} osd={has_osd} vsr={use_vsr} oled={gdi})");
    let a2 = app.clone();
    tokio::spawn(async move { watch(a2, gen).await });
    crate::j(json!({ "ok": true, "gen": gen }))
}

/// Follow the mpv session: position & intro-skip/next-ep check every 2 s, progress saved every 10 s,
/// natural-finish detection for auto-next episode, cleanup & display restore when it exits.
async fn watch(app: Arc<App>, gen: u64) {
    let mut n = 0u32;
    let mut history = false;
    let mine = |app: &App| app.mpv.s.lock().unwrap().as_ref().map_or(false, |s| s.gen == gen);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        n += 1;
        if !mine(&app) { return; }
        if !running(&app) { break; }
        let (p, d, nx) = tokio::task::spawn_blocking(|| {
            (
                ipc(json!(["get_property", "time-pos"])),
                ipc(json!(["get_property", "duration"])),
                ipc(json!(["get_property", "user-data/tv-next"])),
            )
        }).await.unwrap_or((None, None, None));
        let mut skip_cmd: Option<(f64, bool)> = None;
        let mut report_start_id: i64 = 0;
        {
            let mut g = app.mpv.s.lock().unwrap();
            let Some(s) = g.as_mut() else { return };
            if let Some(p) = p.and_then(|x| x.as_f64()) { s.pos = p; }
            if let Some(d) = d.and_then(|x| x.as_f64()) { s.dur = d; }
            if !s.health_reported && s.stream_id > 0 && ((s.pos - s.start_pos).abs() >= 10.0 || (n >= 6 && s.pos > 2.0)) {
                s.health_reported = true;
                report_start_id = s.stream_id;
            }
            if nx.as_ref().and_then(|x| x.as_str()) == Some("1") || nx.as_ref().and_then(|x| x.as_i64()) == Some(1) {
                s.next_triggered = true;
            }
            if !s.has_osd && !s.live && !s.skips.is_empty() {
                for (i, (st, en, label)) in s.skips.iter().enumerate() {
                    if !s.skipped.contains(&i) && s.pos >= *st && s.pos < *en - 2.0 {
                        let ll = label.to_lowercase();
                        let is_skip = ll.contains("intro") || ll.contains("opening") || ll.contains("recap") || ll.contains("previously");
                        s.skipped.push(i);
                        skip_cmd = Some((*en, s.autoskip && is_skip));
                        break;
                    }
                }
            }
        }
        if report_start_id > 0 {
            crate::record_mpv_health(&app, report_start_id, 0.0, true);
        }
        if let Some((en, do_auto)) = skip_cmd {
            let _ = tokio::task::spawn_blocking(move || {
                if do_auto {
                    let _ = ipc(json!(["seek", en, "absolute"]));
                    let _ = ipc(json!(["show-text", "Skipped intro", 2500]));
                } else {
                    let _ = ipc(json!(["show-text", "Intro — press skip on remote or → to jump ahead", 3500]));
                }
            }).await;
        }
        if n % 5 == 0 { save(&app, &mut history).await; }
    }
    if !mine(&app) { return; }
    save(&app, &mut history).await;
    let s = app.mpv.s.lock().unwrap().take();
    if let Some(s) = s {
        let watched = (s.pos - s.start_pos).max(0.0).min(4.0 * 3600.0);
        if s.stream_id > 0 && watched >= 10.0 {
            crate::record_mpv_health(&app, s.stream_id, watched, !s.health_reported);
        }
        let finished = s.next_triggered || (!s.live && s.dur > 60.0 && (
            s.pos / s.dur >= 0.90
            || s.credits.map_or(false, |c| c > s.dur * 0.55 && s.pos >= c - 5.0)
        ));
        let mut err_msg: Option<String> = None;
        if s.pos < 1.0 && !s.next_triggered && n <= 8 {
            if let Ok(raw) = std::fs::read_to_string(app.data.join("mpv-last.log")) {
                let low = raw.to_lowercase();
                if low.contains("403 forbidden") || low.contains("404 not found") || low.contains("460") {
                    app.xc.mark_stream_failed(&s.url);
                }
                let pick = raw.lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty() && !l.starts_with("Exiting"))
                    .rev()
                    .find(|l| l.contains("HTTP error") || l.contains("Failed to open") || l.contains("error") || l.contains("403") || l.contains("404") || l.contains("460"));
                if let Some(line) = pick {
                    // Never leak provider credentials in logs or UI toasts
                    let clean = if line.contains("403") {
                        "Provider returned 403 Forbidden (check VPN / split tunneling)".to_string()
                    } else if line.contains("404") {
                        "Provider stream returned 404 Not Found (rotated server — try again)".to_string()
                    } else if line.contains("460") {
                        "Provider connection busy (460 — 1/1 active slot)".to_string()
                    } else {
                        let u = app.cfg.str("username");
                        let p = app.cfg.str("password");
                        let mut c = line.to_string();
                        if !u.is_empty() { c = c.replace(&u, "***"); }
                        if !p.is_empty() { c = c.replace(&p, "***"); }
                        c.chars().take(160).collect()
                    };
                    println!("[mpv] early exit reason: {clean}");
                    err_msg = Some(clean);
                }
            }
        }
        if s.used_spool { crate::spool::detach(); }
        *app.mpv.last_done.lock().unwrap() = Some(json!({
            "gen": s.gen, "key": s.key, "finished": finished, "nextNow": s.next_triggered, "pos": s.pos, "dur": s.dur, "error": err_msg
        }));
        println!("[mpv] closed at {:.0}s (finished={finished} next_now={})", s.pos, s.next_triggered);
    }
    app.stream.mark_stop();
    let a = app.clone();
    let _ = tokio::task::spawn_blocking(move || crate::display::restore(&a)).await;
}

async fn save(app: &Arc<App>, history: &mut bool) {
    let body = {
        let g = app.mpv.s.lock().unwrap();
        let Some(s) = g.as_ref() else { return };
        if s.live || s.key.is_empty() || s.pos < 5.0 || s.dur <= 0.0 { return; }
        let mut b = json!({ "key": s.key, "pos": s.pos, "dur": s.dur, "item": s.body["item"], "tmdb": s.body["tmdb"] });
        if !*history && s.pos > 60.0 {
            b["historyKind"] = s.body["historyKind"].clone();
            b["historyTitle"] = s.body["historyTitle"].clone();
            *history = true;
        }
        b
    };
    let _ = crate::progress(State(app.clone()), Json(body)).await;
}

async fn stop_inner(app: &Arc<App>, restore: bool) {
    let mut h = false;
    save(app, &mut h).await;
    let s = app.mpv.s.lock().unwrap().take();
    if let Some(mut s) = s {
        let _ = s.child.kill();
        let _ = s.child.wait();
        if s.used_spool { crate::spool::detach(); }
        app.stream.mark_stop();
    }
    if restore {
        let a = app.clone();
        let _ = tokio::task::spawn_blocking(move || crate::display::restore(&a)).await;
    }
}
pub async fn stop(State(app): State<Arc<App>>) -> Response {
    stop_inner(&app, true).await;
    crate::j(json!({ "ok": true }))
}

/// POST /api/mpv/install: the Settings button. Installs the official shinchiro mpv build with winget.
pub async fn install(State(app): State<Arc<App>>) -> Response {
    if find(&app).is_some() { return crate::j(json!({ "ok": true, "already": true })); }
    *app.mpv.install.lock().unwrap() = None;
    let a = app.clone();
    tokio::spawn(async move {
        let mut c = tokio::process::Command::new("winget");
        c.args(["install", "-e", "--id", "shinchiro.mpv", "--silent", "--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"]).stdin(std::process::Stdio::null());
        #[cfg(windows)] c.creation_flags(0x08000000);
        let r = c.output().await;
        let ok = matches!(&r, Ok(o) if o.status.success()) || find(&a).is_some();
        println!("[mpv] winget install: {}", if ok { "ok" } else { "failed" });
        if !ok {
            *a.mpv.install.lock().unwrap() = Some("Install failed. Run `winget install shinchiro.mpv` in a terminal, or set mpvPath in config.json.".into());
        }
    });
    crate::j(json!({ "ok": true }))
}
