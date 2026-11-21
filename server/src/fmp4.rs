// A/V alignment for streamed fragmented MP4.
// ffmpeg's fragmented-MP4 muxer (empty_moov, required for streaming to a pipe) can't write edit
// lists, so it starts EVERY track at time 0, throwing away the fact that copied video begins at a
// keyframe while audio begins at its own first packet. Browsers then play the two tracks out of
// sync by that gap, for the whole session. We fix it on the way out: ffmpeg also records each
// track's true first timestamp (framecrc markers); this rewriter adds the difference back into
// every fragment's baseMediaDecodeTime (tfdt), so the browser aligns audio and video by their real
// timestamps. Exact for any seek point, any GOP length, either direction.
use bytes::Bytes;
use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

/// (video first dts, video first pts, audio start) in seconds of film time; audio already adjusted
/// for AAC priming.
pub type Starts = (f64, f64, f64);

#[derive(Clone, Copy)]
struct Track { video: bool, timescale: u32 }

fn children(b: &[u8]) -> Vec<(&[u8; 4], std::ops::Range<usize>)> {
    let mut out = vec![]; let mut o = 0usize;
    while o + 8 <= b.len() {
        let mut size = u32::from_be_bytes(b[o..o + 4].try_into().unwrap()) as usize; let mut h = 8;
        if size == 1 && o + 16 <= b.len() { size = u64::from_be_bytes(b[o + 8..o + 16].try_into().unwrap()) as usize; h = 16; }
        if size == 0 { size = b.len() - o; }
        if size < h || o + size > b.len() { break; }
        out.push((b[o + 4..o + 8].try_into().unwrap(), o + h..o + size));
        o += size;
    }
    out
}
fn find<'a>(b: &'a [u8], t: &[u8; 4]) -> Option<&'a [u8]> { children(b).into_iter().find(|(k, _)| *k == t).map(|(_, r)| &b[r]) }
fn u32_at(b: &[u8], o: usize) -> Option<u32> { Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?)) }

/// track_ID -> kind/timescale, from the moov box body.
fn parse_moov(moov: &[u8]) -> HashMap<u32, Track> {
    let mut m = HashMap::new();
    for (t, r) in children(moov) {
        if t != b"trak" { continue; }
        let trak = &moov[r];
        let Some(tkhd) = find(trak, b"tkhd") else { continue };
        let Some(id) = u32_at(tkhd, if tkhd[0] == 1 { 20 } else { 12 }) else { continue };
        let Some(mdia) = find(trak, b"mdia") else { continue };
        let ts = find(mdia, b"mdhd").and_then(|h| u32_at(h, if h[0] == 1 { 20 } else { 12 })).unwrap_or(0);
        let video = find(mdia, b"hdlr").and_then(|h| h.get(8..12)).map_or(false, |x| x == b"vide");
        if ts > 0 { m.insert(id, Track { video, timescale: ts }); }
    }
    m
}

/// Adds the per-track shift to every tfdt inside a whole moof box (header included).
fn patch_moof(moof: &mut [u8], shift: &HashMap<u32, i64>) {
    let hdr = if u32_at(moof, 0) == Some(1) { 16 } else { 8 };
    let trafs: Vec<_> = children(&moof[hdr..]).into_iter().filter(|(t, _)| *t == b"traf").map(|(_, r)| (r.start + hdr)..(r.end + hdr)).collect();
    for tr in trafs {
        let kids: Vec<_> = children(&moof[tr.clone()]).into_iter().map(|(t, r)| (*t, (r.start + tr.start)..(r.end + tr.start))).collect();
        let Some(id) = kids.iter().find(|(t, _)| t == b"tfhd").and_then(|(_, r)| u32_at(&moof[r.clone()], 4)) else { continue };
        let d = shift.get(&id).copied().unwrap_or(0);
        if d == 0 { continue; }
        if let Some((_, r)) = kids.iter().find(|(t, _)| t == b"tfdt") {
            let b = &mut moof[r.clone()];
            if b[0] == 1 && b.len() >= 12 { let v = u64::from_be_bytes(b[4..12].try_into().unwrap()) as i64 + d; b[4..12].copy_from_slice(&(v.max(0) as u64).to_be_bytes()); }
            else if b.len() >= 8 { let v = u32::from_be_bytes(b[4..8].try_into().unwrap()) as i64 + d; b[4..8].copy_from_slice(&(v.clamp(0, u32::MAX as i64) as u32).to_be_bytes()); }
        }
    }
}

/// Per-track shift in each track's timescale; the earliest track stays at 0.
pub fn shifts(tracks: &HashMap<u32, Track>, v: f64, a: f64) -> HashMap<u32, i64> {
    let base = v.min(a);
    tracks.iter().map(|(&id, t)| (id, (((if t.video { v } else { a }) - base) * t.timescale as f64).round() as i64)).collect()
}

/// Composition offset (pts - dts) of the first sample of `track` in a moof, in that track's units.
/// Lets us place the video exactly whatever the encoder's reordering delay is.
fn first_cto(moof: &[u8], track: u32) -> Option<i64> {
    let hdr = if u32_at(moof, 0) == Some(1) { 16 } else { 8 };
    for (t, r) in children(&moof[hdr..]) {
        if t != b"traf" { continue; }
        let traf = &moof[hdr + r.start..hdr + r.end];
        if find(traf, b"tfhd").and_then(|h| u32_at(h, 4)) != Some(track) { continue; }
        let trun = find(traf, b"trun")?;
        let ver = trun[0]; let flags = u32_at(trun, 0)? & 0xFFFFFF;
        if flags & 0x800 == 0 { return Some(0); }
        let mut o = 8; // version/flags + sample_count
        if flags & 0x1 != 0 { o += 4; }
        if flags & 0x4 != 0 { o += 4; }
        if flags & 0x100 != 0 { o += 4; }
        if flags & 0x200 != 0 { o += 4; }
        if flags & 0x400 != 0 { o += 4; }
        let raw = u32_at(trun, o)?;
        return Some(if ver == 1 { raw as i32 as i64 } else { raw as i64 });
    }
    None
}

/// Streams `r` through, fixing tfdt once the start markers are known.
/// When `emit_start` is true (MSE mode), emits a 20-byte `free` box ("TVST" + f64 BE base seconds)
/// right before the first `moof` so the MSE client sets `timestampOffset` with zero round-trips.
pub fn rewrite<R, F>(mut r: R, starts: F, emit_start: bool) -> mpsc::Receiver<std::io::Result<Bytes>>
where R: AsyncRead + Unpin + Send + 'static, F: Fn() -> Option<Starts> + Send + 'static {
    let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(8);
    tokio::spawn(async move {
        let mut tracks: HashMap<u32, Track> = HashMap::new();
        let mut shift: Option<HashMap<u32, i64>> = None;
        loop {
            let mut h = [0u8; 8];
            if r.read_exact(&mut h).await.is_err() { break; }
            let mut size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
            let typ: [u8; 4] = h[4..8].try_into().unwrap();
            let mut head = h.to_vec();
            if size == 1 { let mut x = [0u8; 8]; if r.read_exact(&mut x).await.is_err() { break; } size = u64::from_be_bytes(x); head.extend_from_slice(&x); }
            if size == 0 || size < head.len() as u64 || (&typ == b"mdat") {
                // payload: forward without buffering (size 0 = runs to end of stream)
                if tx.send(Ok(Bytes::from(head.clone()))).await.is_err() { return; }
                let mut left = if size == 0 { u64::MAX } else { size.saturating_sub(head.len() as u64) };
                let mut buf = vec![0u8; 256 * 1024];
                while left > 0 {
                    let want = left.min(buf.len() as u64) as usize; let n = match r.read(&mut buf[..want]).await { Ok(0) | Err(_) => { if size != 0 { let _ = tx.send(Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "stream cut"))).await; } return } Ok(n) => n };
                    left = left.saturating_sub(n as u64);
                    if tx.send(Ok(Bytes::copy_from_slice(&buf[..n]))).await.is_err() { return; }
                }
                continue;
            }
            if size > 64 * 1024 * 1024 { let _ = tx.send(Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "box too large"))).await; return; }
            let mut boxv = head; let hl = boxv.len(); boxv.resize(size as usize, 0);
            if r.read_exact(&mut boxv[hl..]).await.is_err() { break; }
            if &typ == b"moov" { tracks = parse_moov(&boxv[hl..]); }
            if &typ == b"moof" {
                if shift.is_none() {
                    // markers are written within milliseconds of the first packets; allow a little slack
                    let mut s = None;
                    for _ in 0..60 { s = starts(); if s.is_some() { break; } tokio::time::sleep(std::time::Duration::from_millis(50)).await; }
                    let (m, base_sec) = match s {
                        Some((vdts, vpts, a)) => {
                            // true video start = first frame's pts minus its composition offset in THIS mp4
                            let vt = tracks.iter().find(|(_, t)| t.video).map(|(&id, t)| (id, t.timescale));
                            let v = match vt.and_then(|(id, ts)| first_cto(&boxv, id).map(|c| vpts - c as f64 / ts as f64)) { Some(v) => v, None => vdts };
                            println!("[avsync] video starts {v:.3}s, audio {a:.3}s -> aligning (audio {:+} ms vs video)", ((a - v) * 1000.0).round());
                            (shifts(&tracks, v, a), v.min(a))
                        }
                        None => { println!("[avsync] start markers missing; tracks left as muxed"); (HashMap::new(), 0.0) } };
                    if emit_start {
                        let mut marker = Vec::with_capacity(20);
                        marker.extend_from_slice(&20u32.to_be_bytes());
                        marker.extend_from_slice(b"free");
                        marker.extend_from_slice(b"TVST");
                        marker.extend_from_slice(&base_sec.max(0.0).to_be_bytes());
                        if tx.send(Ok(Bytes::from(marker))).await.is_err() { return; }
                    }
                    shift = Some(m);
                }
                patch_moof(&mut boxv, shift.as_ref().unwrap());
            }
            if tx.send(Ok(Bytes::from(boxv))).await.is_err() { return; }
        }
    });
    rx
}
