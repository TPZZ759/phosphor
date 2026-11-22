// Playback: ffmpeg remuxes any upstream stream into fragmented MP4 for the browser and, in the same
// pass, writes text subtitles to WebVTT. The account allows ONE connection: one upstream read at a time.
use crate::util::now_ms;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::process::{Child, Command};

pub struct Streamer {
    active: Mutex<Option<(u64, String, Child)>>,
    seq: std::sync::atomic::AtomicU64,
    last_stop: std::sync::atomic::AtomicI64,
    sub_dir: PathBuf,
    probe_file: PathBuf,
    probes: Mutex<HashMap<String, Value>>,
    /// what the current session's start markers will contain: (video marker expected, audio marker expected, video transcoded)
    markers: Mutex<(bool, bool, bool, f64)>,
    /// called with the session's audio tap when it ends (intro detection)
    on_stop: Mutex<Option<(u64, Box<dyn FnOnce(PathBuf) + Send>)>>,
    bytes: std::sync::atomic::AtomicU64,
    /// recently cut sessions (stream key, time): stopped by us, not by the stream failing
    cut: Mutex<Vec<(String, i64)>>,
    /// page that owns the current session (several app windows share one connection)
    pub owner: Mutex<String>,
    /// (seq, probe-cache key, start time) of the current session
    began: Mutex<(u64, String, i64)>,
}

const TEXT_SUBS: &[&str] = &["subrip", "ass", "ssa", "mov_text", "webvtt", "text", "srt"];
fn pkey(url: &str) -> String { crate::re!(r"^https?://[^/]+").replace(url, "").to_string() }
pub fn pick_subs(subs: &Value) -> Value {
    let mut v: Vec<Value> = subs.as_array().cloned().unwrap_or_default();
    v.sort_by_key(|s| (!s["lang"].as_str().unwrap_or("").starts_with("en"), s["i"].as_i64().unwrap_or(0)));
    v.truncate(8); Value::Array(v)
}

impl Streamer {
    pub fn new(data: &std::path::Path) -> Self {
        let sub_dir = data.join("subs"); let _ = std::fs::create_dir_all(&sub_dir);
        let probe_file = data.join("probe-cache.json");
        let probes = std::fs::read(&probe_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Streamer { active: Mutex::new(None), seq: Default::default(), last_stop: Default::default(), sub_dir, probe_file, probes: Mutex::new(probes), markers: Mutex::new((false, false, false, 0.0)), on_stop: Mutex::new(None), bytes: Default::default(), cut: Mutex::new(vec![]), owner: Mutex::new(String::new()), began: Mutex::new((0, String::new(), 0)) }
    }
    pub fn probe_count(&self) -> usize { self.probes.lock().unwrap().len() }
    pub fn evict_probe(&self, url: &str) {
        self.probes.lock().unwrap().remove(&pkey(url));
    }
    /// Was this stream id cut by us (another session, a probe, a stop) in the last `ms`? Then a player
    /// error for it is not the stream's fault and mustn't count against its health.
    pub fn was_cut(&self, id: i64, ms: i64) -> bool { let suf = format!(":{id}"); self.cut.lock().unwrap().iter().any(|(k, t)| k.ends_with(&suf) && now_ms() - t < ms) }
    pub fn count(&self, n: usize) { self.bytes.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed); }
    pub fn sent(&self) -> u64 { self.bytes.load(std::sync::atomic::Ordering::Relaxed) }
    pub fn is_active(&self) -> bool { self.active.lock().unwrap().is_some() }
    pub fn last_stop(&self) -> i64 { self.last_stop.load(std::sync::atomic::Ordering::Relaxed) }

    pub fn stop(&self, reason: &str) {
        let taken = self.active.lock().unwrap().take();
        if let Some((seq, key, mut child)) = taken {
            let _ = child.start_kill();
            println!("[stream] stopped {key} ({reason})");
            self.last_stop.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
            { let mut c = self.cut.lock().unwrap(); c.retain(|x| now_ms() - x.1 < 60_000); c.push((key.clone(), now_ms())); }
            self.ended(seq);
        }
    }
    fn stop_if(&self, seq: u64, reason: &str) {
        let taken = { let mut g = self.active.lock().unwrap(); if g.as_ref().map_or(false, |a| a.0 == seq) { g.take() } else { None } };
        if let Some((_, key, mut c)) = taken { let _ = c.start_kill(); println!("[stream] stopped {key} ({reason})"); self.last_stop.store(now_ms(), std::sync::atomic::Ordering::Relaxed); self.ended(seq); }
    }
    /// Session over: hand its audio tap to the registered callback (moved aside so the next session can't delete it).
    fn ended(&self, seq: u64) {
        let cb = { let mut g = self.on_stop.lock().unwrap(); if g.as_ref().map_or(false, |x| x.0 == seq) { g.take() } else { None } };
        if let Some((_, f)) = cb {
            let src = self.sub_dir.join("audio.pcm");
            let dst = self.sub_dir.parent().unwrap_or(&self.sub_dir).join("fp").join(format!("session_{seq}.pcm"));
            // ffmpeg may hold the file for a moment after the kill: retry the move briefly
            std::thread::spawn(move || { for _ in 0..20 { if std::fs::rename(&src, &dst).is_ok() || std::fs::copy(&src, &dst).is_ok() { f(dst); return; } std::thread::sleep(std::time::Duration::from_millis(100)); } });
        }
    }
    pub fn on_stop(&self, seq: u64, f: Box<dyn FnOnce(PathBuf) + Send>) { *self.on_stop.lock().unwrap() = Some((seq, f)); }
    pub fn mark_stop(&self) { self.last_stop.store(now_ms(), std::sync::atomic::Ordering::Relaxed); }

    pub async fn probe(&self, url: &str, ua: &str) -> Option<Value> {
        let k = pkey(url);
        // v2 entries carry picture details (resolution, HDR, frame rate); older ones are re-probed
        if let Some(mut v) = self.probes.lock().unwrap().get(&k).cloned() { if v["v"].as_i64() == Some(2) { fix_fps(&mut v["pic"]); return Some(v); } }
        self.stop("probe");
        // through the read-ahead cache when this title is spooled (no new provider connection);
        // otherwise the provider needs a moment to release the single connection after any stop
        let src = crate::spool::local_for(url);
        if src.is_none() { let since = now_ms() - self.last_stop(); if since < 1500 { tokio::time::sleep(std::time::Duration::from_millis((1500 - since) as u64)).await; } }
        let src = src.unwrap_or_else(|| url.to_string());
        let out = tokio::time::timeout(std::time::Duration::from_secs(25), {
            let mut c = Command::new("ffprobe");
            c.args(["-v", "error", "-user_agent", ua, "-rw_timeout", "15000000", "-show_entries",
                "format=duration,bit_rate:stream=index,codec_type,codec_name,codec_tag_string,profile,channels,width,height,pix_fmt,color_transfer,color_primaries,avg_frame_rate,r_frame_rate,field_order,bit_rate:stream_side_data=side_data_type,dv_profile:stream_tags=language,title:stream_disposition=forced,default", "-show_chapters", "-of", "json", &src]);
            #[cfg(windows)] c.creation_flags(0x08000000);
            c.kill_on_drop(true).output()
        }).await.ok()?.ok()?;
        self.last_stop.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        if !out.status.success() { return None; }
        let j: Value = serde_json::from_slice(&out.stdout).ok()?;
        let streams = j["streams"].as_array().cloned().unwrap_or_default();
        let video = streams.iter().find(|s| s["codec_type"] == "video").map(|s| s["codec_name"].clone()).unwrap_or(json!(""));
        let audio: Vec<Value> = streams.iter().filter(|s| s["codec_type"] == "audio").enumerate().map(|(i, s)| json!({ "i": i, "codec": s["codec_name"], "ch": s["channels"], "lang": s["tags"]["language"].as_str().unwrap_or(""), "title": s["tags"]["title"].as_str().unwrap_or("") })).collect();
        let subs: Vec<Value> = streams.iter().filter(|s| s["codec_type"] == "subtitle").enumerate().map(|(i, s)| json!({ "i": i, "codec": s["codec_name"], "lang": s["tags"]["language"].as_str().unwrap_or(""), "title": s["tags"]["title"].as_str().unwrap_or(""), "forced": s["disposition"]["forced"].as_i64().unwrap_or(0) == 1 }))
            .filter(|s| TEXT_SUBS.contains(&s["codec"].as_str().unwrap_or(""))).collect();
        let skips: Vec<Value> = j["chapters"].as_array().into_iter().flatten().map(|c| (c["start_time"].as_str().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0), c["end_time"].as_str().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0), c["tags"]["title"].as_str().unwrap_or("").to_string()))
            .filter(|(s, e, t)| crate::re!(r"(?i)intro|opening|recap|previously|title sequence").is_match(t) && e - s < 300.0).map(|(s, e, t)| json!({ "s": s, "e": e, "title": t })).collect();
        let pic = streams.iter().find(|s| s["codec_type"] == "video").map(picture).unwrap_or(json!({}));
        let mut r = json!({ "v": 2, "duration": j["format"]["duration"].as_str().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0), "video": video, "audio": audio, "subs": pick_subs(&Value::Array(subs)), "skips": skips, "pic": pic });
        r["pic"]["kbps"] = json!(j["format"]["bit_rate"].as_str().and_then(|x| x.parse::<f64>().ok()).map(|b| (b / 1000.0).round()).unwrap_or(0.0));
        { let mut p = self.probes.lock().unwrap(); p.insert(k, r.clone()); let _ = std::fs::write(&self.probe_file, serde_json::to_vec(&*p).unwrap()); }
        Some(r)
    }

    /// Picture details of an already-probed stream id (any kind), for ranking versions.
    pub fn quality_of(&self, id: i64) -> Option<Value> {
        let pat = format!("/{id}."); let p = self.probes.lock().unwrap();
        p.iter().find(|(k, v)| k.contains(&pat) && v["v"].as_i64() == Some(2)).map(|(_, v)| { let mut q = v["pic"].clone(); fix_fps(&mut q); q["ch"] = json!(v["audio"].as_array().and_then(|a| a.iter().filter_map(|x| x["ch"].as_i64()).max()).unwrap_or(0)); q })
    }
    /// Audio stream list of an already-probed stream id (for instant English-dub detection).
    pub fn audio_of(&self, id: i64) -> Option<Vec<Value>> {
        let pat = format!("/{id}."); let p = self.probes.lock().unwrap();
        p.iter().find(|(k, v)| k.contains(&pat) && v["audio"].is_array()).and_then(|(_, v)| v["audio"].as_array().cloned())
    }

    fn sub_path(&self, i: i64) -> PathBuf { self.sub_dir.join(format!("sub_{i}.vtt")) }
    pub fn read_sub(&self, i: i64) -> String { std::fs::read_to_string(self.sub_path(i)).unwrap_or_default() }
    /// Session start in seconds (film time), from ffmpeg's framecrc side output.
    /// First packet of a framecrc marker: (dts, pts) in seconds, plus the audio sample rate if any.
    fn marker(&self, name: &str) -> Option<(f64, f64, f64)> {
        let t = std::fs::read_to_string(self.sub_dir.join(name)).ok()?;
        let mut tb = 0.001; let mut sr = 0.0;
        for l in t.lines() {
            if let Some(x) = l.strip_prefix("#tb 0:") { if let Some((n, d)) = x.trim().split_once('/') { tb = n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok()?; } }
            if let Some(x) = l.strip_prefix("#sample_rate 0:") { sr = x.trim().parse().unwrap_or(0.0); }
        }
        let l = t.lines().find(|l| l.starts_with("0,"))?;
        let c: Vec<&str> = l.split(',').map(|x| x.trim()).collect();
        let dts: i64 = c.get(1)?.parse().ok()?; let pts: i64 = c.get(2)?.parse().unwrap_or(dts);
        if dts.abs() > 1_000_000_000_000_000 { return None; } // NOPTS
        Some((dts as f64 * tb, pts as f64 * tb, sr))
    }
    /// Each track's true first timestamp (film time, seconds): video first packet dts and pts (fmp4.rs
    /// resolves the exact start from the MP4's own composition offset); AAC output starts 1024 priming
    /// samples before the first decoded audio.
    pub fn starts(&self) -> Option<(f64, f64, f64)> {
        let (ev, ea, tx, t0) = *self.markers.lock().unwrap();
        // Re-encoded video uses an exact seek: both tracks are cut at t0, unless a track only begins later.
        if tx {
            let v = if ev { let m = self.marker("start_v.txt")?; Some(m.1.max(t0)) } else { None };
            let a = if ea { let m = self.marker("start_a.txt")?; Some(m.0.max(t0)) } else { None };
            return match (v, a) { (Some(v), Some(a)) => Some((v, v, a)), (Some(v), None) => Some((v, v, v)), (None, Some(a)) => Some((a, a, a)), _ => None };
        }
        let v = if ev { let m = self.marker("start_v.txt")?; Some((m.0, m.1)) } else { None };
        let a = if ea { let m = self.marker("start_a.txt")?; Some(m.0 - 1024.0 / if m.2 > 0.0 { m.2 } else { 48000.0 }) } else { None };
        match (v, a) { (Some(v), Some(a)) => Some((v.0, v.1, a)), (Some(v), None) => Some((v.0, v.1, v.0)), (None, Some(a)) => Some((a, a, a)), _ => None }
    }
    /// Session start in film time = the streamed MP4's time zero.
    pub fn session_start(&self) -> Option<f64> { self.starts().map(|(v, _, a)| v.min(a)) }
    pub fn sync_paths(&self, i: i64) -> (PathBuf, PathBuf) { (self.sub_dir.join("audio.pcm"), self.sub_path(i)) }

    /// Start ffmpeg and return (sequence id, stdout). The caller streams stdout to the client.
    pub fn start(&self, url: &str, ua: &str, key: &str, live: bool, start: f64, audio: i64, transcode: bool, info: Option<&Value>, audio_delay_ms: i64, ts: bool, surround_out: bool, local_override: Option<&str>, night: bool, deint: bool) -> Option<(u64, tokio::process::ChildStdout)> {
        self.stop("replaced"); self.bytes.store(0, std::sync::atomic::Ordering::Relaxed);
        if let Ok(rd) = std::fs::read_dir(&self.sub_dir) { for f in rd.flatten() { let _ = std::fs::remove_file(f.path()); } }
        let vcodec = info.and_then(|i| i["video"].as_str()).unwrap_or("").to_string();
        let pic = info.map(|i| i["pic"].clone()).unwrap_or(json!({}));
        let ten_bit_avc = vcodec == "h264" && pic["bits"].as_i64().unwrap_or(8) > 8; // Hi10P: no browser decodes it
        let do_deint = live && deint;
        let must_tx = transcode || do_deint || ten_bit_avc || (!vcodec.is_empty() && !["h264", "hevc", "av1", "vp9"].contains(&vcodec.as_str()));
        let hdr = pic["hdr"].as_str().map_or(false, |h| !h.is_empty());
        // -y: side files (start markers, subtitles) from a session that is still shutting down must never make ffmpeg stop and ask
        let mut a: Vec<String> = vec!["-y".into(), "-nostdin".into(), "-hide_banner".into(), "-loglevel".into(), "error".into(), "-user_agent".into(), ua.into()];
        // live: a short probe so channels start fast (MPEG-TS announces its streams up front)
        // reconnect on network drops too, and never hang forever on a silent provider (rw_timeout, microseconds)
        if live { for x in ["-probesize", "1500000", "-analyzeduration", "1500000", "-rw_timeout", "10000000", "-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_at_eof", "1", "-reconnect_on_network_error", "1", "-reconnect_delay_max", "6", "-fflags", "+genpts+discardcorrupt+igndts"] { a.push(x.into()); } }
        else { for x in ["-analyzeduration", "3000000", "-probesize", "3000000", "-rw_timeout", "15000000", "-reconnect", "1", "-reconnect_streamed", "1", "-reconnect_on_network_error", "1", "-reconnect_on_http_error", "5xx", "-reconnect_delay_max", "10"] { a.push(x.into()); } }
        // A/V sync on seek/resume: copied video can only start at the keyframe BEFORE the target,
        // while accurate seeking trims audio to the exact target. Fragmented MP4 (empty_moov) can't
        // carry per-track start offsets, so both tracks would begin at 0 and the audio would play
        // early by the keyframe gap (0–10 s, varies per seek). -noaccurate_seek starts the audio at
        // the same keyframe as the video; fmp4.rs then restores each track's exact start.
        // Re-encoded video keeps the exact seek instead (both tracks are cut at the same instant;
        // with -noaccurate_seek ffmpeg drops the frames right after the keyframe when encoding).
        if start > 0.0 { if !live && !must_tx { a.push("-noaccurate_seek".into()); } a.push("-ss".into()); a.push(format!("{start}")); }
        if !live { a.push("-copyts".into()); }
        let src = local_override.map(String::from).or_else(|| crate::spool::local_for(url)).unwrap_or_else(|| url.to_string());
        for x in ["-i", src.as_str(), "-map", "0:v:0?", "-map"] { a.push(x.into()); }
        a.push(format!("0:a:{audio}?"));
        if do_deint { a.push("-vf".into()); a.push("bwdif=mode=1:parity=-1:deint=1".into()); }
        // HDR stays HDR when re-encoding: 10-bit HEVC with the colour tags kept (h264 8-bit would flatten it)
        if must_tx && hdr { for x in ["-c:v", "hevc_nvenc", "-preset", "p5", "-cq", "20", "-bf", "0", "-profile:v", "main10", "-pix_fmt", "p010le", "-tag:v", "hvc1",
            "-color_primaries", "bt2020", "-colorspace", "bt2020nc", "-color_trc", if pic["hdr"] == "HLG" { "arib-std-b67" } else { "smpte2084" }] { a.push(x.into()); } }
        else if must_tx { for x in ["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "20", "-bf", "0", "-pix_fmt", "yuv420p"] { a.push(x.into()); } }
        else { a.push("-c:v".into()); a.push("copy".into()); if vcodec == "hevc" { a.push("-tag:v".into()); a.push("hvc1".into()); } }
        let ch = info.and_then(|i| i["audio"].as_array().and_then(|a| a.get(audio as usize)).and_then(|x| x["ch"].as_i64())).unwrap_or(2);
        // 5.1 only to a surround output: on stereo, dialogue-preserving center-boosted downmix keeps speech crisp
        let surround = !ts && surround_out && ch >= 6;
        let mut af_parts: Vec<String> = vec![];
        if audio_delay_ms > 0 { af_parts.push(format!("adelay=delays={audio_delay_ms}:all=1")); }
        else if audio_delay_ms < 0 { af_parts.push(format!("asetpts=PTS-STARTPTS,atrim=start={},asetpts=PTS-STARTPTS", -audio_delay_ms as f64 / 1000.0)); }
        else if live { af_parts.push("aresample=async=1000:first_pts=0".into()); }
        if !ts && !surround_out && ch >= 6 {
            af_parts.push("pan=stereo|FL=0.55*FC+0.707*FL+0.6*BL+0.4*LFE|FR=0.55*FC+0.707*FR+0.6*BR+0.4*LFE".into());
        }
        if night { af_parts.push("dynaudnorm=f=150:g=15:p=0.9".into()); }
        let af = if af_parts.is_empty() { None } else { Some(af_parts.join(",")) };
        if let Some(f) = &af { a.push("-af".into()); a.push(f.clone()); }
        // MPEG-TS for TVs (cast): carries real per-stream timestamps, so no MP4 re-alignment needed
        if surround { for x in ["-c:a", "aac", "-ac", "6", "-b:a", "448k"] { a.push(x.into()); } }
        else { for x in ["-c:a", "aac", "-ac", "2", "-b:a", "256k"] { a.push(x.into()); } }
        if ts { for x in ["-f", "mpegts", "-mpegts_flags", "+resend_headers", "pipe:1"] { a.push(x.into()); } }
        else { for x in ["-movflags", "frag_keyframe+empty_moov+default_base_moof", "-frag_duration", if live { "500000" } else { "1000000" }, "-f", "mp4", "pipe:1"] { a.push(x.into()); } }
        let subs = info.map(|i| pick_subs(&i["subs"])).unwrap_or(json!([]));
        for s in subs.as_array().into_iter().flatten() {
            let i = s["i"].as_i64().unwrap_or(0);
            for x in ["-map".to_string(), format!("0:s:{i}"), "-c:s".into(), "webvtt".into(), "-flush_packets".into(), "1".into(), "-f".into(), "webvtt".into(), self.sub_path(i).to_string_lossy().to_string()] { a.push(x); }
        }
        // Exact start of this session in film time (a seek lands on the keyframe before the target).
        // (records the first video frame AND the audio packets before it: the MP4 timeline starts at the earliest)
        // Start markers: each track's true first timestamp (see fmp4.rs, which re-aligns the MP4 with them).
        let has_v = info.map_or(false, |i| !i["video"].as_str().unwrap_or("").is_empty());
        let has_a = info.map_or(false, |i| i["audio"].as_array().map_or(false, |x| (audio as usize) < x.len()));
        *self.markers.lock().unwrap() = (!live && has_v, !live && has_a, must_tx, start);
        if !live && has_v { for x in ["-map".to_string(), "0:v:0".into(), "-c".into(), "copy".into(), "-frames:v".into(), "1".into(), "-flush_packets".into(), "1".into(), "-f".into(), "framecrc".into(), self.sub_dir.join("start_v.txt").to_string_lossy().to_string()] { a.push(x); } }
        if !live && has_a { for x in ["-map".to_string(), format!("0:a:{audio}"), "-c".into(), "copy".into(), "-frames:a".into(), "1".into(), "-flush_packets".into(), "1".into(), "-f".into(), "framecrc".into(), self.sub_dir.join("start_a.txt").to_string_lossy().to_string()] { a.push(x); } }
        // Low-rate mono copy of the audio: subtitle sync + intro/credits fingerprints.
        if !live && has_a {
            for x in ["-map".to_string(), format!("0:a:{audio}?")] { a.push(x); }
            if let Some(f) = &af { a.push("-af".into()); a.push(f.clone()); }
            for x in ["-ac", "1", "-ar", "8000", "-c:a", "pcm_s16le", "-f", "s16le"] { a.push(x.into()); }
            a.push(self.sub_dir.join("audio.pcm").to_string_lossy().to_string());
        }
        let mut cmd = Command::new("ffmpeg");
        cmd.args(&a).stdin(Stdio::null()).stdout(Stdio::piped()).kill_on_drop(true);
        // TV_FFLOG=1: ffmpeg's errors go to data/ffmpeg.log (diagnostics)
        match std::env::var("TV_FFLOG").ok().and_then(|_| std::fs::File::create(self.sub_dir.parent().unwrap_or(&self.sub_dir).join("ffmpeg.log")).ok()) { Some(f) => { cmd.stderr(f); } None => { cmd.stderr(Stdio::null()); } }
        #[cfg(windows)] cmd.creation_flags(0x08000000);
        let mut child = cmd.spawn().map_err(|e| println!("[stream] ffmpeg failed to start: {e}")).ok()?;
        let out = child.stdout.take()?;
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        *self.active.lock().unwrap() = Some((seq, key.to_string(), child));
        *self.began.lock().unwrap() = (seq, pkey(url), now_ms());
        println!("[stream] {key} start={start} audio={audio} subs={} {}{}", subs.as_array().map_or(0, |a| a.len()), if must_tx { "transcode" } else { "copy" }, if vcodec.is_empty() { String::new() } else { format!(" {vcodec}") });
        Some((seq, out))
    }
    /// Output ended: was it the real end? False when the session was cut (by us or another window)
    /// or ffmpeg failed, so the player gets an error (and resumes) instead of a clean "film over".
    /// A failure within seconds of starting also drops the cached probe (the file may have changed).
    pub async fn ended_ok(&self, seq: u64) -> bool {
        for _ in 0..15 {
            let st = { let mut g = self.active.lock().unwrap(); match g.as_mut() { Some((s, _, c)) if *s == seq => Some(c.try_wait()), _ => None } };
            match st {
                None => return false,
                Some(Ok(Some(code))) => {
                    if !code.success() { let (s, k, t) = self.began.lock().unwrap().clone(); if s == seq && now_ms() - t < 8000 { self.probes.lock().unwrap().remove(&k); } }
                    return code.success();
                }
                Some(Err(_)) => return false,
                Some(Ok(None)) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        true
    }
    pub fn finished(&self, seq: u64) { self.stop_if(seq, "client closed"); }
}

/// Picture details from ffprobe's video stream: size, bit depth, HDR format, frame rate, interlacing.
fn picture(s: &Value) -> Value {
    let rate = |k: &str| s[k].as_str().and_then(|r| r.split_once('/')).and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?.max(1e-9))).filter(|x| x.is_finite() && *x > 0.0 && *x < 300.0);
    // MKV's average rate is often off-grid (e.g. 24.417 for 23.976 video): snap to a real rate, else trust r_frame_rate
    let fps = { let a = rate("avg_frame_rate").map(snap_fps); let r = rate("r_frame_rate").map(snap_fps);
        match (a, r) { (Some(a), _) if is_std(a) => a, (_, Some(r)) if is_std(r) => r, (Some(a), _) => a, (None, Some(r)) => r, _ => 0.0 } };
    let pix = s["pix_fmt"].as_str().unwrap_or("");
    let bits = if crate::re!(r"p1[02]").is_match(pix) || pix.contains("10le") { 10 } else { 8 };
    let side = s["side_data_list"].as_array().cloned().unwrap_or_default();
    let dv = side.iter().find(|x| x["side_data_type"].as_str().map_or(false, |t| t.contains("DOVI"))).and_then(|x| x["dv_profile"].as_i64())
        .or_else(|| if ["dvh1", "dvhe"].contains(&s["codec_tag_string"].as_str().unwrap_or("")) { Some(5) } else { None });
    let trc = s["color_transfer"].as_str().unwrap_or("");
    let hdr = if dv.is_some() && trc != "smpte2084" && trc != "arib-std-b67" { "DV" } else if trc == "smpte2084" { "HDR10" } else if trc == "arib-std-b67" { "HLG" } else if dv.is_some() { "DV" } else { "" };
    let fo = s["field_order"].as_str().unwrap_or("progressive");
    json!({ "w": s["width"], "h": s["height"], "bits": bits, "hdr": hdr, "dv": dv, "fps": (fps * 1000.0).round() / 1000.0, "interlaced": !fo.is_empty() && fo != "progressive" && fo != "unknown", "codec": s["codec_name"] })
}

const STD_FPS: [f64; 9] = [23.976, 24.0, 25.0, 29.97, 30.0, 47.952, 50.0, 59.94, 60.0];
fn is_std(f: f64) -> bool { STD_FPS.iter().any(|s| (s - f).abs() < 1e-6) }
/// Nearest standard frame rate within 1.5 %, else unchanged.
pub fn snap_fps(f: f64) -> f64 { STD_FPS.iter().copied().min_by(|a, b| (a - f).abs().total_cmp(&(b - f).abs())).filter(|s| (s - f).abs() / s < 0.015).unwrap_or(f) }
/// Cached entries from before snapping: an off-grid rate (24.417) is treated as 23.976 film.
fn fix_fps(pic: &mut Value) {
    if let Some(f) = pic["fps"].as_f64() { if f > 0.0 && !is_std(f) { let s = snap_fps(f); pic["fps"] = json!(if is_std(s) { s } else if (f - 24.0).abs() < 1.0 { 23.976 } else { f }); } }
}
