// Display control for the OLED: match the refresh rate to the video's frame rate (no 3:2 judder on
// 24p films) and switch Windows HDR on for HDR titles. Every change is temporary (not saved to the
// registry) and remembered, so it's undone when playback ends, by the player or by the watchdog
// below if the player never says so (window closed, crash).
use crate::app::App;
use axum::{extract::State, response::Response, Json};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct Display { pub gdi: String, pub name: String, pub width: u32, pub height: u32, pub hz: u32, pub rates: Vec<u32>, pub hdr_supported: bool, pub hdr_on: bool, pub primary: bool, adapter: (u32, i32), target: u32 }

/// What we changed and must undo: (gdi device, original hz if changed, original HDR state if changed).
#[derive(Default)]
pub struct Changes { pub gdi: String, pub hz: Option<u32>, pub hdr_orig: Option<bool>, pub since: i64 }
pub struct DisplayCtl { pub changed: Mutex<Option<Changes>>, file: std::path::PathBuf }
impl DisplayCtl {
    /// Picks up an undo record left by a previous run (server restarted while the screen was switched).
    pub fn load(data: &std::path::Path) -> Self {
        let file = data.join("display-restore.json");
        let changed = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| {
            let gdi = v["gdi"].as_str()?.to_string();
            let hdr_orig = v["hdrOrig"].as_bool().or_else(|| if v["hdr"].as_bool().unwrap_or(false) { Some(false) } else { None });
            Some(Changes { gdi, hz: v["hz"].as_u64().map(|x| x as u32), hdr_orig, since: 0 })
        });
        DisplayCtl { changed: Mutex::new(changed), file }
    }
    fn persist(&self, c: Option<&Changes>) {
        match c { Some(c) => { let _ = std::fs::write(&self.file, json!({ "gdi": c.gdi, "hz": c.hz, "hdrOrig": c.hdr_orig }).to_string()); } None => { let _ = std::fs::remove_file(&self.file); } }
    }
}

#[cfg(windows)]
mod ffi {
    #![allow(non_snake_case)]
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct Luid { pub low: u32, pub high: i32 }
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct Rational { pub n: u32, pub d: u32 }
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct PathSource { pub adapter: Luid, pub id: u32, pub mode_idx: u32, pub status: u32 }
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct PathTarget { pub adapter: Luid, pub id: u32, pub mode_idx: u32, pub tech: u32, pub rotation: u32, pub scaling: u32, pub refresh: Rational, pub scanline: u32, pub available: u32, pub status: u32 }
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct PathInfo { pub source: PathSource, pub target: PathTarget, pub flags: u32 }
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct ModeInfo { pub kind: u32, pub id: u32, pub adapter: Luid, pub data: [u64; 6] }
    #[repr(C)] #[derive(Clone, Copy, Default)] pub struct Header { pub kind: u32, pub size: u32, pub adapter: Luid, pub id: u32 }
    #[repr(C)] pub struct SourceName { pub h: Header, pub gdi: [u16; 32] }
    #[repr(C)] pub struct TargetName { pub h: Header, pub flags: u32, pub tech: u32, pub mfg: u16, pub product: u16, pub connector: u32, pub friendly: [u16; 64], pub path: [u16; 128] }
    #[repr(C)] pub struct ColorInfo { pub h: Header, pub value: u32, pub encoding: u32, pub bpc: u32, pub mode: u32 }
    #[repr(C)] pub struct SetState { pub h: Header, pub value: u32 }
    #[repr(C)] #[derive(Clone, Copy)] pub struct DevMode {
        pub name: [u16; 32], pub spec: u16, pub drv: u16, pub size: u16, pub extra: u16, pub fields: u32,
        pub pos: [u32; 4], pub color: i16, pub duplex: i16, pub yres: i16, pub tt: i16, pub collate: i16, pub form: [u16; 32],
        pub logpix: u16, pub bpp: u32, pub w: u32, pub h: u32, pub dflags: u32, pub hz: u32, pub rest: [u32; 8],
    }
    impl Default for DevMode { fn default() -> Self { let mut d: DevMode = unsafe { std::mem::zeroed() }; d.size = std::mem::size_of::<DevMode>() as u16; d } }
    #[link(name = "user32")]
    extern "system" {
        pub fn GetDisplayConfigBufferSizes(flags: u32, np: *mut u32, nm: *mut u32) -> i32;
        pub fn QueryDisplayConfig(flags: u32, np: *mut u32, paths: *mut PathInfo, nm: *mut u32, modes: *mut ModeInfo, topo: *mut u32) -> i32;
        pub fn DisplayConfigGetDeviceInfo(h: *mut Header) -> i32;
        pub fn DisplayConfigSetDeviceInfo(h: *mut Header) -> i32;
        pub fn EnumDisplaySettingsW(dev: *const u16, mode: u32, dm: *mut DevMode) -> i32;
        pub fn ChangeDisplaySettingsExW(dev: *const u16, dm: *mut DevMode, hwnd: *mut core::ffi::c_void, flags: u32, lp: *mut core::ffi::c_void) -> i32;
    }
}

fn wstr(a: &[u16]) -> String { String::from_utf16_lossy(&a[..a.iter().position(|&c| c == 0).unwrap_or(a.len())]) }
fn wz(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

#[cfg(windows)]
fn color(adapter: ffi::Luid, target: u32) -> (bool, bool) {
    use ffi::*;
    unsafe {
        // Windows 11 24H2+: HDR is its own switch (type 15); older builds: advanced colour (type 9)
        let mut c = ColorInfo { h: Header { kind: 15, size: std::mem::size_of::<ColorInfo>() as u32, adapter, id: target }, value: 0, encoding: 0, bpc: 0, mode: 0 };
        if DisplayConfigGetDeviceInfo(&mut c.h) == 0 { return (c.value & (1 << 4) != 0, c.value & (1 << 5) != 0); }
        let mut c = ColorInfo { h: Header { kind: 9, size: (std::mem::size_of::<ColorInfo>() - 4) as u32, adapter, id: target }, value: 0, encoding: 0, bpc: 0, mode: 0 };
        if DisplayConfigGetDeviceInfo(&mut c.h) == 0 { return (c.value & 1 != 0, c.value & 2 != 0); }
    }
    (false, false)
}

#[cfg(windows)]
pub fn list() -> Vec<Display> {
    use ffi::*;
    let mut out = vec![];
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(2, &mut np, &mut nm) != 0 { return out; }
        let mut paths = vec![PathInfo::default(); np as usize]; let mut modes = vec![ModeInfo::default(); nm as usize];
        if QueryDisplayConfig(2, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) != 0 { return out; }
        paths.truncate(np as usize);
        for p in &paths {
            let mut sn = SourceName { h: Header { kind: 1, size: std::mem::size_of::<SourceName>() as u32, adapter: p.source.adapter, id: p.source.id }, gdi: [0; 32] };
            if DisplayConfigGetDeviceInfo(&mut sn.h) != 0 { continue; }
            let mut tn: TargetName = std::mem::zeroed(); tn.h = Header { kind: 2, size: std::mem::size_of::<TargetName>() as u32, adapter: p.target.adapter, id: p.target.id };
            let name = if DisplayConfigGetDeviceInfo(&mut tn.h) == 0 { wstr(&tn.friendly) } else { String::new() };
            let gdi = wstr(&sn.gdi); let g = wz(&gdi);
            let mut cur = DevMode::default();
            if EnumDisplaySettingsW(g.as_ptr(), u32::MAX, &mut cur) == 0 { continue; }
            let mut rates = vec![]; let mut i = 0;
            loop { let mut dm = DevMode::default(); if EnumDisplaySettingsW(g.as_ptr(), i, &mut dm) == 0 { break; } i += 1;
                if dm.w == cur.w && dm.h == cur.h && dm.hz > 1 && !rates.contains(&dm.hz) { rates.push(dm.hz); } }
            rates.sort();
            let (hs, ho) = color(p.target.adapter, p.target.id);
            out.push(Display { name: if name.is_empty() { gdi.clone() } else { name }, gdi, width: cur.w, height: cur.h, hz: cur.hz, rates, hdr_supported: hs, hdr_on: ho,
                primary: cur.pos[0] == 0 && cur.pos[1] == 0, adapter: (p.target.adapter.low, p.target.adapter.high), target: p.target.id });
        }
    }
    out
}
#[cfg(not(windows))] pub fn list() -> Vec<Display> { vec![] }

/// Set a refresh rate (temporary: not saved, Windows restores it on reboot). test=true only checks it's allowed.
#[cfg(windows)]
pub fn set_hz(gdi: &str, hz: u32, test: bool) -> bool {
    use ffi::*;
    let g = wz(gdi);
    unsafe {
        let mut cur = DevMode::default();
        if EnumDisplaySettingsW(g.as_ptr(), u32::MAX, &mut cur) == 0 { return false; }
        if cur.hz == hz { return true; }
        let mut dm = DevMode::default(); dm.w = cur.w; dm.h = cur.h; dm.bpp = cur.bpp; dm.hz = hz;
        dm.fields = 0x80000 | 0x100000 | 0x40000 | 0x400000;
        ChangeDisplaySettingsExW(g.as_ptr(), &mut dm, std::ptr::null_mut(), if test { 2 } else { 0 }, std::ptr::null_mut()) == 0
    }
}
#[cfg(not(windows))] pub fn set_hz(_: &str, _: u32, _: bool) -> bool { false }

#[cfg(windows)]
pub fn set_hdr(d: &Display, on: bool) -> bool {
    use ffi::*;
    let adapter = Luid { low: d.adapter.0, high: d.adapter.1 };
    unsafe {
        let mut s = SetState { h: Header { kind: 16, size: std::mem::size_of::<SetState>() as u32, adapter, id: d.target }, value: on as u32 };
        if DisplayConfigSetDeviceInfo(&mut s.h) == 0 { return true; }
        let mut s = SetState { h: Header { kind: 10, size: std::mem::size_of::<SetState>() as u32, adapter, id: d.target }, value: on as u32 };
        DisplayConfigSetDeviceInfo(&mut s.h) == 0
    }
}
#[cfg(not(windows))] pub fn set_hdr(_: &Display, _: bool) -> bool { false }

/// The best refresh rate for a frame rate: an exact whole multiple (24p -> 120/240 Hz, 25p -> 100/200,
/// 23.976 -> 119.88 shown by Windows as 119), else the current rate. Highest multiple wins (smoothest UI).
pub fn best_rate(rates: &[u32], fps: f64, current: u32) -> u32 {
    if fps < 10.0 || fps > 130.0 { return current; }
    let real = |r: u32| if [23, 29, 47, 59, 71, 119, 143, 239].contains(&r) { (r + 1) as f64 * 1000.0 / 1001.0 } else { r as f64 };
    let err = |r: u32| { let k = (real(r) / fps).round(); if k < 1.0 { 9.0 } else { (real(r) - k * fps).abs() / fps } };
    // the current rate is fine if it is already a clean multiple (no switch, no blank screen)
    if err(current) < 0.002 { return current; }
    rates.iter().copied().filter(|&r| err(r) < 0.002).max().unwrap_or(current)
}

/// The display to control: Settings choice, else the HDR-capable one with the highest refresh rate.
pub fn target(app: &App) -> Option<Display> {
    let all = list(); let want = app.state.read(|st| st["prefs"]["display"].as_str().unwrap_or("").to_string());
    if let Some(d) = all.iter().find(|d| !want.is_empty() && d.gdi == want) { return Some(d.clone()); }
    all.into_iter().max_by_key(|d| (d.hdr_supported, d.rates.last().copied().unwrap_or(0), d.width))
}

fn pref(app: &App, k: &str, def: bool) -> bool { app.state.read(|st| st["prefs"][k].as_bool().unwrap_or(def)) }

/// GET /api/display: displays + what the app may do.
pub async fn info(State(app): State<Arc<App>>) -> Response {
    let ds = tokio::task::spawn_blocking(list).await.unwrap_or_default();
    let t = target(&app).map(|d| d.gdi).unwrap_or_default();
    crate::j(json!({ "displays": ds.iter().map(|d| json!({ "gdi": d.gdi, "name": d.name, "width": d.width, "height": d.height, "hz": d.hz, "rates": d.rates, "hdrSupported": d.hdr_supported, "hdrOn": d.hdr_on, "target": d.gdi == t })).collect::<Vec<_>>(),
        "matchRate": pref(&app, "matchRate", true), "autoHdr": pref(&app, "autoHdr", true), "sdrOffHdr": pref(&app, "sdrOffHdr", true), "changed": app.display.changed.lock().unwrap().is_some() }))
}

/// Apply refresh rate + OLED True Black HDR match synchronously. Returns JSON with changed[], gdi, hdrOn.
pub async fn match_for(app: &Arc<App>, fps: f64, hdr: bool) -> Value {
    let (mr, ah, sdr_off) = (pref(app, "matchRate", true), pref(app, "autoHdr", true), pref(app, "sdrOffHdr", true));
    let a = app.clone();
    tokio::task::spawn_blocking(move || {
        let Some(d) = target(&a) else { return json!({ "ok": false, "changed": [], "gdi": "", "hdrOn": false }) };
        let mut g = a.display.changed.lock().unwrap();
        let ch = g.get_or_insert_with(|| Changes { gdi: d.gdi.clone(), since: crate::util::now_ms(), ..Default::default() });
        let mut did = vec![];
        let base_hz = ch.hz.unwrap_or(d.hz);
        let want = if mr && fps > 0.0 { best_rate(&d.rates, fps, base_hz) } else { base_hz };
        if want != d.hz && set_hz(&d.gdi, want, false) {
            if ch.hz.is_none() { ch.hz = Some(d.hz); }
            did.push(format!("{want} Hz"));
            println!("[display] {} -> {want} Hz for {fps:.3} fps", d.name);
        }
        let mut hdr_now = d.hdr_on;
        if ah && d.hdr_supported {
            if hdr && !d.hdr_on && set_hdr(&d, true) {
                if ch.hdr_orig.is_none() { ch.hdr_orig = Some(false); }
                else if ch.hdr_orig == Some(true) { ch.hdr_orig = None; }
                hdr_now = true;
                did.push("HDR on".into());
                println!("[display] HDR on for an HDR title");
            } else if !hdr && d.hdr_on && (sdr_off || ch.hdr_orig == Some(false)) && set_hdr(&d, false) {
                if ch.hdr_orig.is_none() { ch.hdr_orig = Some(true); }
                else if ch.hdr_orig == Some(false) { ch.hdr_orig = None; }
                hdr_now = false;
                did.push("HDR off (OLED true black)".into());
                println!("[display] HDR off for SDR title (OLED true black)");
            }
        }
        if want != d.hz && !did.iter().any(|x| x.ends_with("Hz")) { println!("[display] switch to {want} Hz FAILED on {}", d.name); did.push(format!("{want} Hz failed")); }
        if ch.hz.is_none() && ch.hdr_orig.is_none() { *g = None; }
        a.display.persist(g.as_ref());
        json!({ "ok": true, "changed": did, "display": d.name, "gdi": d.gdi, "hz": want, "hdrOn": hdr_now })
    }).await.unwrap_or(json!({ "ok": false, "changed": [], "gdi": "", "hdrOn": false }))
}

/// POST /api/display/match {fps, hdr}: switch for this video. Returns what changed (the player waits
/// for the screen to settle before starting when something did).
pub async fn matcher(State(app): State<Arc<App>>, Json(b): Json<Value>) -> Response {
    let fps = b["fps"].as_f64().unwrap_or(0.0); let hdr = b["hdr"].as_bool().unwrap_or(false);
    crate::j(match_for(&app, fps, hdr).await)
}

/// Undo everything we changed (back to the desktop's own refresh rate / original HDR state).
pub fn restore(app: &App) -> bool {
    let Some(ch) = app.display.changed.lock().unwrap().take() else { return false };
    let ds = list(); let d = ds.iter().find(|d| d.gdi == ch.gdi);
    // display briefly unavailable (re-syncing): keep the record and try again later
    if d.is_none() { *app.display.changed.lock().unwrap() = Some(ch); return false; }
    app.display.persist(None);
    if let Some(hz) = ch.hz { if set_hz(&ch.gdi, hz, false) { println!("[display] restored {hz} Hz"); } }
    if let Some(orig) = ch.hdr_orig { if let Some(d) = d { if set_hdr(d, orig) { println!("[display] HDR restored {}", if orig { "on" } else { "off" }); } } }
    true
}
pub async fn restore_route(State(app): State<Arc<App>>) -> Response {
    let a = app.clone(); let r = tokio::task::spawn_blocking(move || restore(&a)).await.unwrap_or(false);
    crate::j(json!({ "ok": true, "restored": r }))
}

/// Watchdog: nothing playing for 20 s (the player was closed without telling us) -> undo.
pub async fn watchdog(app: Arc<App>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let pending = app.display.changed.lock().unwrap().is_some();
        if !pending || app.stream.is_active() || crate::mpv::running(&app) { continue; }
        if crate::util::now_ms() - app.stream.last_stop().max(app.display.changed.lock().unwrap().as_ref().map_or(0, |c| c.since)) < 20_000 { continue; }
        let a = app.clone(); let _ = tokio::task::spawn_blocking(move || restore(&a)).await;
    }
}

/// `tv.exe --display`: list displays, check (without applying) each refresh rate switch.
pub fn self_test() {
    for d in list() {
        println!("{} [{}] {}x{} @{} Hz, rates {:?}, HDR supported={} on={}", d.name, d.gdi, d.width, d.height, d.hz, d.rates, d.hdr_supported, d.hdr_on);
        for fps in [23.976, 24.0, 25.0, 29.97, 50.0, 59.94, 60.0] { let r = best_rate(&d.rates, fps, d.hz); println!("  {fps} fps -> {r} Hz (allowed: {})", set_hz(&d.gdi, r, true)); }
    }
}
