// Display control for the OLED: match the refresh rate to the video's frame rate (no 3:2 judder on
// 24p films) and switch Windows HDR on for HDR titles. Every change is temporary (not saved to the
// registry) and remembered, so it's undone when playback ends, by the player or by the watchdog
// below if the player never says so (window closed, crash).
use crate::app::App;
use axum::{extract::State, response::Response, Json};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct Display {
    pub gdi: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub hz_exact: f64,
    pub rates: Vec<u32>,
    pub hdr_supported: bool,
    pub hdr_on: bool,
    pub primary: bool,
    adapter: (u32, i32),
    target: u32,
}

/// What we changed and must undo: (gdi device, original hz if changed, original HDR state if changed).
#[derive(Default)]
pub struct Changes {
    pub gdi: String,
    pub hz: Option<u32>,
    pub hdr_orig: Option<bool>,
    pub since: i64,
}

pub struct DisplayCtl {
    pub changed: Mutex<Option<Changes>>,
    file: std::path::PathBuf,
}

impl DisplayCtl {
    /// Picks up an undo record left by a previous run (server restarted while the screen was switched).
    pub fn load(data: &std::path::Path) -> Self {
        let file = data.join("display-restore.json");
        let changed = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| {
                let gdi = v["gdi"].as_str()?.to_string();
                let hdr_orig = v["hdrOrig"].as_bool().or_else(|| {
                    if v["hdr"].as_bool().unwrap_or(false) {
                        Some(false)
                    } else {
                        None
                    }
                });
                Some(Changes {
                    gdi,
                    hz: v["hz"].as_u64().map(|x| x as u32),
                    hdr_orig,
                    since: 0,
                })
            });
        DisplayCtl {
            changed: Mutex::new(changed),
            file,
        }
    }

    fn persist(&self, c: Option<&Changes>) {
        match c {
            Some(c) => {
                let mut tmp_name = self.file.file_name().unwrap_or_default().to_os_string();
                tmp_name.push(".tmp");
                let tmp = self.file.with_file_name(tmp_name);
                let data = json!({ "gdi": c.gdi, "hz": c.hz, "hdrOrig": c.hdr_orig }).to_string();
                if std::fs::write(&tmp, data).is_ok() {
                    if std::fs::rename(&tmp, &self.file).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
            None => {
                let _ = std::fs::remove_file(&self.file);
            }
        }
    }
}

#[cfg(windows)]
mod ffi {
    #![allow(non_snake_case)]
    pub const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Luid {
        pub low: u32,
        pub high: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Rational {
        pub n: u32,
        pub d: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct PathSource {
        pub adapter: Luid,
        pub id: u32,
        pub mode_idx: u32,
        pub status: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct PathTarget {
        pub adapter: Luid,
        pub id: u32,
        pub mode_idx: u32,
        pub tech: u32,
        pub rotation: u32,
        pub scaling: u32,
        pub refresh: Rational,
        pub scanline: u32,
        pub available: u32,
        pub status: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct PathInfo {
        pub source: PathSource,
        pub target: PathTarget,
        pub flags: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct ModeInfo {
        pub kind: u32,
        pub id: u32,
        pub adapter: Luid,
        pub data: [u64; 6],
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Header {
        pub kind: u32,
        pub size: u32,
        pub adapter: Luid,
        pub id: u32,
    }
    #[repr(C)]
    pub struct SourceName {
        pub h: Header,
        pub gdi: [u16; 32],
    }
    #[repr(C)]
    pub struct TargetName {
        pub h: Header,
        pub flags: u32,
        pub tech: u32,
        pub mfg: u16,
        pub product: u16,
        pub connector: u32,
        pub friendly: [u16; 64],
        pub path: [u16; 128],
    }
    #[repr(C)]
    pub struct ColorInfo {
        pub h: Header,
        pub value: u32,
        pub encoding: u32,
        pub bpc: u32,
        pub mode: u32,
    }
    #[repr(C)]
    pub struct SetState {
        pub h: Header,
        pub value: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct DevMode {
        pub name: [u16; 32],
        pub spec: u16,
        pub drv: u16,
        pub size: u16,
        pub extra: u16,
        pub fields: u32,
        pub pos: [u32; 4],
        pub color: i16,
        pub duplex: i16,
        pub yres: i16,
        pub tt: i16,
        pub collate: i16,
        pub form: [u16; 32],
        pub logpix: u16,
        pub bpp: u32,
        pub w: u32,
        pub h: u32,
        pub dflags: u32,
        pub hz: u32,
        pub rest: [u32; 8],
    }
    impl Default for DevMode {
        fn default() -> Self {
            let mut d: DevMode = unsafe { std::mem::zeroed() };
            d.size = std::mem::size_of::<DevMode>() as u16;
            d
        }
    }
    #[link(name = "user32")]
    extern "system" {
        pub fn GetDisplayConfigBufferSizes(flags: u32, np: *mut u32, nm: *mut u32) -> i32;
        pub fn QueryDisplayConfig(
            flags: u32,
            np: *mut u32,
            paths: *mut PathInfo,
            nm: *mut u32,
            modes: *mut ModeInfo,
            topo: *mut u32,
        ) -> i32;
        pub fn DisplayConfigGetDeviceInfo(h: *mut Header) -> i32;
        pub fn DisplayConfigSetDeviceInfo(h: *mut Header) -> i32;
        pub fn EnumDisplaySettingsW(dev: *const u16, mode: u32, dm: *mut DevMode) -> i32;
        pub fn ChangeDisplaySettingsExW(
            dev: *const u16,
            dm: *mut DevMode,
            hwnd: *mut core::ffi::c_void,
            flags: u32,
            lp: *mut core::ffi::c_void,
        ) -> i32;
    }
}

fn wstr(a: &[u16]) -> String {
    String::from_utf16_lossy(&a[..a.iter().position(|&c| c == 0).unwrap_or(a.len())])
}
fn wz(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn color(adapter: ffi::Luid, target: u32) -> (bool, bool) {
    use ffi::*;
    unsafe {
        // Windows 11 24H2+: HDR is its own switch (type 15); older builds: advanced colour (type 9)
        let mut c = ColorInfo {
            h: Header {
                kind: 15,
                size: std::mem::size_of::<ColorInfo>() as u32,
                adapter,
                id: target,
            },
            value: 0,
            encoding: 0,
            bpc: 0,
            mode: 0,
        };
        if DisplayConfigGetDeviceInfo(&mut c.h) == 0 {
            return (c.value & (1 << 4) != 0, c.value & (1 << 5) != 0);
        }
        let mut c = ColorInfo {
            h: Header {
                kind: 9,
                size: (std::mem::size_of::<ColorInfo>() - 4) as u32,
                adapter,
                id: target,
            },
            value: 0,
            encoding: 0,
            bpc: 0,
            mode: 0,
        };
        if DisplayConfigGetDeviceInfo(&mut c.h) == 0 {
            return (c.value & 1 != 0, c.value & 2 != 0);
        }
    }
    (false, false)
}

#[cfg(windows)]
pub fn list() -> Vec<Display> {
    use ffi::*;
    let mut out = vec![];
    unsafe {
        let mut paths = Vec::new();
        let mut modes = Vec::new();
        let mut ok = false;
        for _ in 0..5 {
            let (mut np, mut nm) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(2, &mut np, &mut nm) != 0 {
                return out;
            }
            paths.resize(np as usize, PathInfo::default());
            modes.resize(nm as usize, ModeInfo::default());
            let rc = QueryDisplayConfig(
                2,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            );
            if rc == 0 {
                paths.truncate(np as usize);
                ok = true;
                break;
            }
            if rc != ERROR_INSUFFICIENT_BUFFER {
                return out;
            }
        }
        if !ok {
            return out;
        }
        for p in &paths {
            let mut sn = SourceName {
                h: Header {
                    kind: 1,
                    size: std::mem::size_of::<SourceName>() as u32,
                    adapter: p.source.adapter,
                    id: p.source.id,
                },
                gdi: [0; 32],
            };
            if DisplayConfigGetDeviceInfo(&mut sn.h) != 0 {
                continue;
            }
            let mut tn: TargetName = std::mem::zeroed();
            tn.h = Header {
                kind: 2,
                size: std::mem::size_of::<TargetName>() as u32,
                adapter: p.target.adapter,
                id: p.target.id,
            };
            let name = if DisplayConfigGetDeviceInfo(&mut tn.h) == 0 {
                wstr(&tn.friendly)
            } else {
                String::new()
            };
            let gdi = wstr(&sn.gdi);
            let g = wz(&gdi);
            let mut cur = DevMode::default();
            if EnumDisplaySettingsW(g.as_ptr(), u32::MAX, &mut cur) == 0 {
                continue;
            }
            let mut rates = vec![];
            let mut i = 0;
            loop {
                let mut dm = DevMode::default();
                if EnumDisplaySettingsW(g.as_ptr(), i, &mut dm) == 0 {
                    break;
                }
                i += 1;
                if dm.w == cur.w && dm.h == cur.h && dm.hz > 1 && !rates.contains(&dm.hz) {
                    rates.push(dm.hz);
                }
            }
            rates.sort();
            let hz_exact = if p.target.refresh.d > 0 && p.target.refresh.n > 0 {
                let r = p.target.refresh.n as f64 / p.target.refresh.d as f64;
                if r.is_finite() && r > 1.0 && r < 1000.0 {
                    r
                } else {
                    cur.hz as f64
                }
            } else {
                cur.hz as f64
            };
            let (hs, ho) = color(p.target.adapter, p.target.id);
            out.push(Display {
                name: if name.is_empty() { gdi.clone() } else { name },
                gdi,
                width: cur.w,
                height: cur.h,
                hz: cur.hz,
                hz_exact,
                rates,
                hdr_supported: hs,
                hdr_on: ho,
                primary: cur.pos[0] == 0 && cur.pos[1] == 0,
                adapter: (p.target.adapter.low, p.target.adapter.high),
                target: p.target.id,
            });
        }
    }
    out
}
#[cfg(not(windows))]
pub fn list() -> Vec<Display> {
    vec![]
}

/// Set a refresh rate (temporary: not saved, Windows restores it on reboot). test=true only checks it's allowed.
#[cfg(windows)]
pub fn set_hz(gdi: &str, hz: u32, test: bool) -> bool {
    use ffi::*;
    let g = wz(gdi);
    unsafe {
        let mut cur = DevMode::default();
        if EnumDisplaySettingsW(g.as_ptr(), u32::MAX, &mut cur) == 0 {
            return false;
        }
        if cur.hz == hz {
            return true;
        }
        let mut dm = DevMode::default();
        dm.w = cur.w;
        dm.h = cur.h;
        dm.bpp = cur.bpp;
        dm.hz = hz;
        dm.fields = 0x80000 | 0x100000 | 0x40000 | 0x400000;
        ChangeDisplaySettingsExW(
            g.as_ptr(),
            &mut dm,
            std::ptr::null_mut(),
            if test { 2 } else { 0 },
            std::ptr::null_mut(),
        ) == 0
    }
}
#[cfg(not(windows))]
pub fn set_hz(_: &str, _: u32, _: bool) -> bool {
    false
}

#[cfg(windows)]
pub fn set_hdr(d: &Display, on: bool) -> bool {
    use ffi::*;
    let adapter = Luid {
        low: d.adapter.0,
        high: d.adapter.1,
    };
    unsafe {
        let mut s = SetState {
            h: Header {
                kind: 16,
                size: std::mem::size_of::<SetState>() as u32,
                adapter,
                id: d.target,
            },
            value: on as u32,
        };
        if DisplayConfigSetDeviceInfo(&mut s.h) == 0 {
            return true;
        }
        let mut s = SetState {
            h: Header {
                kind: 10,
                size: std::mem::size_of::<SetState>() as u32,
                adapter,
                id: d.target,
            },
            value: on as u32,
        };
        DisplayConfigSetDeviceInfo(&mut s.h) == 0
    }
}
#[cfg(not(windows))]
pub fn set_hdr(_: &Display, _: bool) -> bool {
    false
}

/// Real refresh rate corresponding to Windows' truncated integer DevMode `hz`.
fn real_hz(r: u32) -> f64 {
    if [23, 29, 47, 59, 71, 119, 143, 239].contains(&r) {
        (r + 1) as f64 * 1000.0 / 1001.0
    } else {
        r as f64
    }
}

/// Relative cadence error of refresh rate `rr` against an integer multiple `k` of `fps`.
fn cadence_err(rr: f64, fps: f64) -> f64 {
    let k = (rr / fps).round();
    if k < 1.0 {
        9.0
    } else {
        (rr - k * fps).abs() / rr.max(1.0)
    }
}

/// The best refresh rate for a frame rate: an exact whole multiple (23.976 -> 239.76 / 119.88,
/// else 240 / 120; 24p -> 240 / 120; 25p -> 100 / 200), else the current rate.
/// Prefers the highest exact-cadence high-refresh mode (239.76/119.88 > 240/120 > 60 > 24) so a
/// 4K 240Hz / 120Hz QD-OLED never drops down to 24Hz on 23.976 fps movies.
pub fn best_rate(rates: &[u32], fps: f64, current: u32) -> u32 {
    if fps < 10.0 || fps > 130.0 {
        return current;
    }
    const EXACT_TOL: f64 = 0.0003; // zero-drift fractional/integer cadence (e.g. 239.76 for 23.976)
    const NEAR_TOL: f64 = 0.002;   // 0.1% cadence (e.g. 240/120 for 23.976 when 239/119 unavailable)
    let err = |r: u32| cadence_err(real_hz(r), fps);

    // Tier 1: highest exact-cadence high-refresh mode (>= 50 Hz: 239.76, 143.86, 119.88, 240, 120, 100, 59.94, 50)
    if let Some(best_hi_exact) = rates.iter().copied().filter(|&r| r >= 50 && err(r) < EXACT_TOL).max() {
        // Stay on `current` if it is already an exact-cadence high-refresh rate >= 50 Hz
        if current >= 50 && err(current) < EXACT_TOL && current >= best_hi_exact {
            return current;
        }
        return best_hi_exact;
    }

    // Tier 2: highest near-exact high-refresh mode (>= 50 Hz: e.g. 240 / 120 Hz for 23.976 fps)
    if let Some(best_hi_near) = rates.iter().copied().filter(|&r| r >= 50 && err(r) < NEAR_TOL).max() {
        if current >= 50 && err(current) < NEAR_TOL && current >= best_hi_near {
            return current;
        }
        return best_hi_near;
    }

    // Tier 3: low-refresh exact/near match (e.g. 23/24 Hz on a 60 Hz-only TV to avoid 3:2 judder)
    if let Some(best_lo_exact) = rates.iter().copied().filter(|&r| err(r) < EXACT_TOL).max() {
        if err(current) < EXACT_TOL {
            return current;
        }
        return best_lo_exact;
    }
    if err(current) < NEAR_TOL {
        return current;
    }
    rates.iter().copied().filter(|&r| err(r) < NEAR_TOL).max().unwrap_or(current)
}

/// The display to control: Settings choice, else the HDR-capable one with the highest refresh rate.
pub fn target(app: &App) -> Option<Display> {
    let all = list();
    let want = app.state.read(|st| st["prefs"]["display"].as_str().unwrap_or("").to_string());
    if let Some(d) = all.iter().find(|d| !want.is_empty() && d.gdi == want) {
        return Some(d.clone());
    }
    all.into_iter()
        .max_by_key(|d| (d.hdr_supported, d.rates.last().copied().unwrap_or(0), d.width))
}

fn pref(app: &App, k: &str, def: bool) -> bool {
    app.state.read(|st| st["prefs"][k].as_bool().unwrap_or(def))
}

/// GET /api/display: displays + what the app may do.
pub async fn info(State(app): State<Arc<App>>) -> Response {
    let ds = tokio::task::spawn_blocking(list).await.unwrap_or_default();
    let t = target(&app).map(|d| d.gdi).unwrap_or_default();
    crate::j(json!({
        "displays": ds.iter().map(|d| json!({
            "gdi": d.gdi,
            "name": d.name,
            "width": d.width,
            "height": d.height,
            "hz": d.hz,
            "hzExact": (d.hz_exact * 1000.0).round() / 1000.0,
            "rates": d.rates,
            "hdrSupported": d.hdr_supported,
            "hdrOn": d.hdr_on,
            "target": d.gdi == t
        })).collect::<Vec<_>>(),
        "matchRate": pref(&app, "matchRate", true),
        "autoHdr": pref(&app, "autoHdr", true),
        "sdrOffHdr": pref(&app, "sdrOffHdr", true),
        "changed": app.display.changed.lock().unwrap().is_some()
    }))
}

/// Apply HDR toggle FIRST -> 250ms settle -> refresh rate switch so NVIDIA DP 1.4 DSC never drops
/// one of the two transitions. Returns JSON with changed[], gdi, hz (actual), hdrOn.
pub async fn match_for(app: &Arc<App>, fps: f64, hdr: bool) -> Value {
    let (mr, ah, sdr_off) = (
        pref(app, "matchRate", true),
        pref(app, "autoHdr", true),
        pref(app, "sdrOffHdr", true),
    );
    let a = app.clone();
    tokio::task::spawn_blocking(move || {
        let Some(d) = target(&a) else {
            return json!({ "ok": false, "changed": [], "gdi": "", "hdrOn": false });
        };
        let mut g = a.display.changed.lock().unwrap();
        let ch = g.get_or_insert_with(|| Changes {
            gdi: d.gdi.clone(),
            since: crate::util::now_ms(),
            ..Default::default()
        });
        // Always refresh `since` so the watchdog never restores mid-startup of a second title.
        ch.since = crate::util::now_ms();
        let mut did = vec![];

        // 1) HDR toggle FIRST
        let mut hdr_now = d.hdr_on;
        let mut hdr_changed = false;
        if ah && d.hdr_supported {
            if hdr && !d.hdr_on && set_hdr(&d, true) {
                if ch.hdr_orig.is_none() {
                    ch.hdr_orig = Some(false);
                } else if ch.hdr_orig == Some(true) {
                    ch.hdr_orig = None;
                }
                hdr_now = true;
                hdr_changed = true;
                did.push("HDR on".into());
                println!("[display] HDR on for an HDR title");
            } else if !hdr && d.hdr_on && (sdr_off || ch.hdr_orig == Some(false)) && set_hdr(&d, false) {
                if ch.hdr_orig.is_none() {
                    ch.hdr_orig = Some(true);
                } else if ch.hdr_orig == Some(false) {
                    ch.hdr_orig = None;
                }
                hdr_now = false;
                hdr_changed = true;
                did.push("HDR off (OLED true black)".into());
                println!("[display] HDR off for SDR title (OLED true black)");
            }
        }

        // 2) Compute target Hz; if HDR just toggled and we also need an Hz switch, settle 250ms first
        let base_hz = ch.hz.unwrap_or(d.hz);
        let want = if mr && fps > 0.0 {
            best_rate(&d.rates, fps, base_hz)
        } else {
            base_hz
        };
        // Avoid redundant mode switch if Windows already drives the exact rational cadence
        let already_exact = mr
            && fps > 0.0
            && want != base_hz
            && want == d.hz.saturating_sub(1)
            && cadence_err(d.hz_exact, fps) < 0.0003;
        let need_hz_switch = want != d.hz && !already_exact;

        if hdr_changed && need_hz_switch {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }

        let mut actual_hz = d.hz;
        if need_hz_switch {
            if set_hz(&d.gdi, want, false) {
                actual_hz = want;
                if want == base_hz {
                    ch.hz = None;
                } else if ch.hz.is_none() {
                    ch.hz = Some(d.hz);
                }
                did.push(format!("{want} Hz"));
                println!("[display] {} -> {want} Hz for {fps:.3} fps", d.name);
            } else {
                println!("[display] switch to {want} Hz FAILED on {}", d.name);
                did.push(format!("{want} Hz failed"));
            }
        } else if ch.hz == Some(d.hz) {
            ch.hz = None;
        }

        if ch.hz.is_none() && ch.hdr_orig.is_none() {
            *g = None;
        }
        a.display.persist(g.as_ref());
        json!({
            "ok": true,
            "changed": did,
            "display": d.name,
            "gdi": d.gdi,
            "hz": actual_hz,
            "hdrOn": hdr_now
        })
    })
    .await
    .unwrap_or(json!({ "ok": false, "changed": [], "gdi": "", "hdrOn": false }))
}

/// POST /api/display/match {fps, hdr}: switch for this video. Returns what changed (the player waits
/// for the screen to settle before starting when something did).
pub async fn matcher(State(app): State<Arc<App>>, Json(b): Json<Value>) -> Response {
    let fps = b["fps"].as_f64().unwrap_or(0.0);
    let hdr = b["hdr"].as_bool().unwrap_or(false);
    crate::j(match_for(&app, fps, hdr).await)
}

/// Undo everything we changed in clean reverse order: restore Hz FIRST -> 250ms settle -> restore HDR.
pub fn restore(app: &App) -> bool {
    let Some(ch) = app.display.changed.lock().unwrap().take() else {
        return false;
    };
    let ds = list();
    let d = ds.iter().find(|d| d.gdi == ch.gdi);
    // display briefly unavailable (re-syncing): keep the record and try again later
    if d.is_none() {
        *app.display.changed.lock().unwrap() = Some(ch);
        return false;
    }
    app.display.persist(None);
    let mut hz_restored = false;
    if let Some(hz) = ch.hz {
        if set_hz(&ch.gdi, hz, false) {
            hz_restored = true;
            println!("[display] restored {hz} Hz");
        }
    }
    if let Some(orig) = ch.hdr_orig {
        if hz_restored {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        if let Some(d) = d {
            if set_hdr(d, orig) {
                println!("[display] HDR restored {}", if orig { "on" } else { "off" });
            }
        }
    }
    true
}

pub async fn restore_route(State(app): State<Arc<App>>) -> Response {
    let a = app.clone();
    let r = tokio::task::spawn_blocking(move || restore(&a))
        .await
        .unwrap_or(false);
    crate::j(json!({ "ok": true, "restored": r }))
}

/// Watchdog: nothing playing for 20 s (the player was closed without telling us) -> undo.
pub async fn watchdog(app: Arc<App>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let pending = app.display.changed.lock().unwrap().is_some();
        if !pending || app.stream.is_active() || crate::mpv::running(&app) {
            continue;
        }
        if crate::util::now_ms()
            - app
                .stream
                .last_stop()
                .max(app.display.changed.lock().unwrap().as_ref().map_or(0, |c| c.since))
            < 20_000
        {
            continue;
        }
        let a = app.clone();
        let _ = tokio::task::spawn_blocking(move || restore(&a)).await;
    }
}

/// `tv.exe --display`: list displays, check (without applying) each refresh rate switch.
pub fn self_test() {
    for d in list() {
        println!(
            "{} [{}] {}x{} @{} Hz ({:.3} Hz exact), rates {:?}, HDR supported={} on={}",
            d.name, d.gdi, d.width, d.height, d.hz, d.hz_exact, d.rates, d.hdr_supported, d.hdr_on
        );
        for fps in [23.976, 24.0, 25.0, 29.97, 50.0, 59.94, 60.0] {
            let r = best_rate(&d.rates, fps, d.hz);
            println!("  {fps} fps -> {r} Hz (allowed: {})", set_hz(&d.gdi, r, true));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oled_240hz_never_drops_to_24hz_on_23_976_fps() {
        // Without fractional 239/119 modes: must pick 240 Hz (NOT 24 Hz!)
        assert_eq!(best_rate(&[24, 60, 120, 240], 23.976, 240), 240);
        assert_eq!(best_rate(&[24, 60, 120, 240], 23.976, 60), 240);
        assert_eq!(best_rate(&[24, 60, 120, 240], 23.976, 24), 240);
        // Even if 23 Hz is also advertised alongside 120/240 Hz, keep 240 Hz
        assert_eq!(best_rate(&[23, 24, 60, 120, 240], 23.976, 240), 240);
        assert_eq!(best_rate(&[23, 24, 60, 120], 23.976, 120), 120);
    }

    #[test]
    fn oled_240hz_prefers_exact_239_76_and_119_88() {
        // With fractional 239/119 modes: must pick 239 (239.76 Hz) over 240 / 119 / 24
        assert_eq!(best_rate(&[24, 60, 119, 120, 239, 240], 23.976, 240), 239);
        assert_eq!(best_rate(&[24, 60, 119, 120, 239, 240], 23.976, 60), 239);
        assert_eq!(best_rate(&[24, 60, 119, 120, 239, 240], 23.976, 239), 239);
        assert_eq!(best_rate(&[24, 60, 119, 120], 23.976, 120), 119);
    }

    #[test]
    fn standard_cadence_rates_and_60hz_tv_fallback() {
        assert_eq!(best_rate(&[24, 60, 119, 120, 239, 240], 24.0, 240), 240);
        assert_eq!(best_rate(&[24, 60, 119, 120, 239, 240], 24.0, 60), 240);
        assert_eq!(best_rate(&[24, 50, 60, 100, 120, 240], 25.0, 240), 100);
        assert_eq!(best_rate(&[24, 60, 119, 120, 239, 240], 29.97, 240), 239);
        assert_eq!(best_rate(&[24, 60, 120, 240], 60.0, 240), 240);
        // 60Hz-only TV still drops to 23/24 Hz to avoid 3:2 judder
        assert_eq!(best_rate(&[23, 24, 60], 23.976, 60), 23);
        assert_eq!(best_rate(&[24, 60], 23.976, 60), 24);
    }
}
