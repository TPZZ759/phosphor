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
pub struct Track {
    pub video: bool,
    pub timescale: u32,
}

const MAX_PENDING_MOOFS: usize = 8;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

fn children(b: &[u8]) -> Vec<(&[u8; 4], std::ops::Range<usize>)> {
    let mut out = vec![];
    let mut o = 0usize;
    while o + 8 <= b.len() {
        let raw_size = u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let (mut size, h) = if raw_size == 1 {
            if o + 16 > b.len() {
                break;
            }
            let sz64 = u64::from_be_bytes(b[o + 8..o + 16].try_into().unwrap());
            let Ok(sz) = usize::try_from(sz64) else {
                break;
            };
            (sz, 16usize)
        } else {
            (raw_size as usize, 8usize)
        };
        if size == 0 {
            size = b.len() - o;
        }
        if size < h {
            break;
        }
        let Some(end) = o.checked_add(size) else {
            break;
        };
        if end > b.len() {
            break;
        }
        out.push((b[o + 4..o + 8].try_into().unwrap(), o + h..end));
        o = end;
    }
    out
}

fn find<'a>(b: &'a [u8], t: &[u8; 4]) -> Option<&'a [u8]> {
    children(b)
        .into_iter()
        .find(|(k, _)| *k == t)
        .map(|(_, r)| &b[r])
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// track_ID -> kind/timescale, from the moov box body.
fn parse_moov(moov: &[u8]) -> HashMap<u32, Track> {
    let mut m = HashMap::new();
    for (t, r) in children(moov) {
        if t != b"trak" {
            continue;
        }
        let trak = &moov[r];
        let Some(tkhd) = find(trak, b"tkhd") else {
            continue;
        };
        let Some(&tkhd_ver) = tkhd.first() else {
            continue;
        };
        let Some(id) = u32_at(tkhd, if tkhd_ver == 1 { 20 } else { 12 }) else {
            continue;
        };
        let Some(mdia) = find(trak, b"mdia") else {
            continue;
        };
        let ts = find(mdia, b"mdhd")
            .and_then(|h| {
                let &ver = h.first()?;
                u32_at(h, if ver == 1 { 20 } else { 12 })
            })
            .unwrap_or(0);
        let video = find(mdia, b"hdlr")
            .and_then(|h| h.get(8..12))
            .map_or(false, |x| x == b"vide");
        if ts > 0 {
            m.insert(id, Track { video, timescale: ts });
        }
    }
    m
}

/// Adds the per-track shift to every tfdt inside a whole moof box (header included).
fn patch_moof(moof: &mut [u8], shift: &HashMap<u32, i64>) {
    let hdr = if u32_at(moof, 0) == Some(1) { 16 } else { 8 };
    if moof.len() < hdr {
        return;
    }
    let trafs: Vec<_> = children(&moof[hdr..])
        .into_iter()
        .filter(|(t, _)| *t == b"traf")
        .map(|(_, r)| (r.start + hdr)..(r.end + hdr))
        .collect();
    for tr in trafs {
        let kids: Vec<_> = children(&moof[tr.clone()])
            .into_iter()
            .map(|(t, r)| (*t, (r.start + tr.start)..(r.end + tr.start)))
            .collect();
        let Some(id) = kids
            .iter()
            .find(|(t, _)| t == b"tfhd")
            .and_then(|(_, r)| u32_at(&moof[r.clone()], 4))
        else {
            continue;
        };
        let d = shift.get(&id).copied().unwrap_or(0);
        if d == 0 {
            continue;
        }
        if let Some((_, r)) = kids.iter().find(|(t, _)| t == b"tfdt") {
            let b = &mut moof[r.clone()];
            if let Some(&ver) = b.first() {
                if ver == 1 && b.len() >= 12 {
                    let cur = u64::from_be_bytes(b[4..12].try_into().unwrap());
                    let next = if d >= 0 {
                        cur.saturating_add(d as u64)
                    } else {
                        cur.saturating_sub(d.unsigned_abs())
                    };
                    b[4..12].copy_from_slice(&next.to_be_bytes());
                } else if ver == 0 && b.len() >= 8 {
                    let cur = u32::from_be_bytes(b[4..8].try_into().unwrap()) as i64;
                    let next = cur.saturating_add(d).clamp(0, u32::MAX as i64) as u32;
                    b[4..8].copy_from_slice(&next.to_be_bytes());
                }
            }
        }
    }
}

/// Per-track shift in each track's timescale; the earliest track stays at 0.
pub fn shifts(tracks: &HashMap<u32, Track>, v: f64, a: f64) -> HashMap<u32, i64> {
    let base = v.min(a);
    tracks
        .iter()
        .map(|(&id, t)| {
            (
                id,
                (((if t.video { v } else { a }) - base) * t.timescale as f64).round() as i64,
            )
        })
        .collect()
}

/// Composition offset (pts - dts) of the first sample of `track` in a moof, in that track's units.
/// Lets us place the video exactly whatever the encoder's reordering delay is.
fn first_cto(moof: &[u8], track: u32) -> Option<i64> {
    let hdr = if u32_at(moof, 0) == Some(1) { 16 } else { 8 };
    if moof.len() < hdr {
        return None;
    }
    for (t, r) in children(&moof[hdr..]) {
        if t != b"traf" {
            continue;
        }
        let traf = &moof[hdr + r.start..hdr + r.end];
        if find(traf, b"tfhd").and_then(|h| u32_at(h, 4)) != Some(track) {
            continue;
        }
        let trun = find(traf, b"trun")?;
        let ver = *trun.first()?;
        let flags = u32_at(trun, 0)? & 0xFFFFFF;
        let sample_count = u32_at(trun, 4)?;
        if sample_count == 0 {
            return None;
        }
        if flags & 0x800 == 0 {
            return Some(0);
        }
        let mut o = 8usize; // version/flags + sample_count
        if flags & 0x1 != 0 {
            o += 4;
        }
        if flags & 0x4 != 0 {
            o += 4;
        }
        if flags & 0x100 != 0 {
            o += 4;
        }
        if flags & 0x200 != 0 {
            o += 4;
        }
        if flags & 0x400 != 0 {
            o += 4;
        }
        let raw = u32_at(trun, o)?;
        return Some(if ver == 1 {
            raw as i32 as i64
        } else {
            raw as i64
        });
    }
    None
}

fn build_tvst_box(base_sec: f64) -> Bytes {
    let mut marker = Vec::with_capacity(20);
    marker.extend_from_slice(&20u32.to_be_bytes());
    marker.extend_from_slice(b"free");
    marker.extend_from_slice(b"TVST");
    marker.extend_from_slice(&base_sec.max(0.0).to_be_bytes());
    Bytes::from(marker)
}

async fn flush_pending(
    tx: &mpsc::Sender<std::io::Result<Bytes>>,
    pending: &mut Vec<(bool, Vec<u8>)>,
    shift: &HashMap<u32, i64>,
    emit_start: bool,
    base_sec: f64,
) -> bool {
    if emit_start && tx.send(Ok(build_tvst_box(base_sec))).await.is_err() {
        return false;
    }
    for (is_moof, mut b) in pending.drain(..) {
        if is_moof {
            patch_moof(&mut b, shift);
        }
        if tx.send(Ok(Bytes::from(b))).await.is_err() {
            return false;
        }
    }
    true
}

/// Streams `r` through, fixing tfdt once the start markers are known (backward-compatible wrapper
/// with `requested_start = 0.0`).
pub fn rewrite<R, F>(r: R, starts: F, emit_start: bool) -> mpsc::Receiver<std::io::Result<Bytes>>
where
    R: AsyncRead + Unpin + Send + 'static,
    F: Fn() -> Option<Starts> + Send + 'static,
{
    rewrite_with_fallback(r, starts, emit_start, 0.0)
}

/// Streams `r` through, fixing tfdt once the start markers are known.
/// - Waits up to 1.2 s for framecrc start markers; if missing/NOPTS, emits `requested_start`
///   in the `TVST` box instead of snapping back to 0.0 s.
/// - If the first `moof` is audio-only (no video `trun` CTO yet), defers locking the video CTO
///   and buffers the tiny leading audio fragment(s) until the first video `moof` arrives so
///   B-frame lip-sync stays exact.
pub fn rewrite_with_fallback<R, F>(
    mut r: R,
    starts: F,
    emit_start: bool,
    requested_start: f64,
) -> mpsc::Receiver<std::io::Result<Bytes>>
where
    R: AsyncRead + Unpin + Send + 'static,
    F: Fn() -> Option<Starts> + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<std::io::Result<Bytes>>(8);
    tokio::spawn(async move {
        let mut tracks: HashMap<u32, Track> = HashMap::new();
        let mut shift: Option<HashMap<u32, i64>> = None;
        let mut cached_starts: Option<Option<Starts>> = None;
        let mut pending: Vec<(bool, Vec<u8>)> = Vec::new();
        let mut pending_moofs: usize = 0;
        let mut pending_bytes: usize = 0;
        // Hoist the 256 KB read buffer outside the box loop to avoid per-fragment allocations
        let mut buf = vec![0u8; 256 * 1024];
        let fallback_start = if requested_start.is_finite() && requested_start > 0.0 {
            requested_start
        } else {
            0.0
        };

        loop {
            let mut h = [0u8; 8];
            if r.read_exact(&mut h).await.is_err() {
                break;
            }
            let mut size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
            let typ: [u8; 4] = h[4..8].try_into().unwrap();
            let mut head = h.to_vec();
            if size == 1 {
                let mut x = [0u8; 8];
                if r.read_exact(&mut x).await.is_err() {
                    break;
                }
                size = u64::from_be_bytes(x);
                head.extend_from_slice(&x);
            }

            if size == 0 || size < head.len() as u64 || (&typ == b"mdat") {
                // If we are deferring shift lock waiting for the first video moof, buffer small audio mdats
                if shift.is_none()
                    && !pending.is_empty()
                    && &typ == b"mdat"
                    && size >= head.len() as u64
                    && size <= (2 * 1024 * 1024) as u64
                    && pending_bytes + (size as usize) <= MAX_PENDING_BYTES
                {
                    let hl = head.len();
                    let mut mdat = head;
                    mdat.resize(size as usize, 0);
                    if r.read_exact(&mut mdat[hl..]).await.is_err() {
                        break;
                    }
                    pending_bytes += mdat.len();
                    pending.push((false, mdat));
                    continue;
                }

                // If we had pending audio moofs and hit a large/unbounded mdat, lock with vdts fallback first
                if shift.is_none() && !pending.is_empty() {
                    let (m, base_sec) = match cached_starts.flatten() {
                        Some((vdts, _, a)) => {
                            println!(
                                "[avsync] video starts {vdts:.3}s (fallback), audio {a:.3}s -> aligning (audio {:+} ms vs video)",
                                ((a - vdts) * 1000.0).round()
                            );
                            (shifts(&tracks, vdts, a), vdts.min(a))
                        }
                        None => (HashMap::new(), fallback_start),
                    };
                    if !flush_pending(&tx, &mut pending, &m, emit_start, base_sec).await {
                        return;
                    }
                    pending_moofs = 0;
                    pending_bytes = 0;
                    shift = Some(m);
                }

                // Payload: forward without buffering (size 0 = runs to end of stream)
                if tx.send(Ok(Bytes::from(head.clone()))).await.is_err() {
                    return;
                }
                let mut left = if size == 0 {
                    u64::MAX
                } else {
                    size.saturating_sub(head.len() as u64)
                };
                while left > 0 {
                    let want = left.min(buf.len() as u64) as usize;
                    let n = match r.read(&mut buf[..want]).await {
                        Ok(0) | Err(_) => {
                            if size != 0 {
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        "stream cut",
                                    )))
                                    .await;
                            }
                            return;
                        }
                        Ok(n) => n,
                    };
                    left = left.saturating_sub(n as u64);
                    if tx.send(Ok(Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        return;
                    }
                }
                continue;
            }

            if size > 64 * 1024 * 1024 {
                let _ = tx
                    .send(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "box too large",
                    )))
                    .await;
                return;
            }
            let mut boxv = head;
            let hl = boxv.len();
            boxv.resize(size as usize, 0);
            if r.read_exact(&mut boxv[hl..]).await.is_err() {
                break;
            }
            if &typ == b"moov" {
                tracks = parse_moov(&boxv[hl..]);
            }
            if &typ == b"moof" {
                if shift.is_none() {
                    // Fetch markers once (waiting up to 1.2 s instead of 3.0 s)
                    let s = match cached_starts {
                        Some(prev) => prev,
                        None => {
                            let mut got = None;
                            for _ in 0..24 {
                                got = starts();
                                if got.is_some() {
                                    break;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }
                            cached_starts = Some(got);
                            got
                        }
                    };
                    let vt = tracks
                        .iter()
                        .find(|(_, t)| t.video)
                        .map(|(&id, t)| (id, t.timescale));
                    let video_cto = vt.and_then(|(id, _)| first_cto(&boxv, id));

                    // Audio-only leading moof on a video+audio stream: defer locking video CTO
                    // until the first video moof arrives (bounded by MAX_PENDING_MOOFS / MAX_PENDING_BYTES).
                    if s.is_some()
                        && vt.is_some()
                        && video_cto.is_none()
                        && pending_moofs < MAX_PENDING_MOOFS
                        && pending_bytes + boxv.len() <= MAX_PENDING_BYTES
                    {
                        pending_moofs += 1;
                        pending_bytes += boxv.len();
                        pending.push((true, boxv));
                        continue;
                    }

                    let (m, base_sec) = match s {
                        Some((vdts, vpts, a)) => {
                            let v = match (vt, video_cto) {
                                (Some((_, ts)), Some(c)) => vpts - c as f64 / ts as f64,
                                _ => vdts,
                            };
                            println!(
                                "[avsync] video starts {v:.3}s, audio {a:.3}s -> aligning (audio {:+} ms vs video)",
                                ((a - v) * 1000.0).round()
                            );
                            (shifts(&tracks, v, a), v.min(a))
                        }
                        None => {
                            println!(
                                "[avsync] start markers missing; tracks left as muxed (fallback TVST={fallback_start:.3}s)"
                            );
                            (HashMap::new(), fallback_start)
                        }
                    };

                    if !flush_pending(&tx, &mut pending, &m, emit_start, base_sec).await {
                        return;
                    }
                    pending_moofs = 0;
                    pending_bytes = 0;
                    shift = Some(m);
                }
                patch_moof(&mut boxv, shift.as_ref().unwrap());
            } else if shift.is_none() && !pending.is_empty() {
                // Any non-moof/non-mdat box appearing between buffered moofs stays ordered
                if pending_bytes + boxv.len() <= MAX_PENDING_BYTES {
                    pending_bytes += boxv.len();
                    pending.push((false, boxv));
                    continue;
                }
                let (m, base_sec) = match cached_starts.flatten() {
                    Some((vdts, _, a)) => (shifts(&tracks, vdts, a), vdts.min(a)),
                    None => (HashMap::new(), fallback_start),
                };
                if !flush_pending(&tx, &mut pending, &m, emit_start, base_sec).await {
                    return;
                }
                pending_moofs = 0;
                pending_bytes = 0;
                shift = Some(m);
            }
            if tx.send(Ok(Bytes::from(boxv))).await.is_err() {
                return;
            }
        }

        // Flush any buffered audio-only fragments on unexpected early EOF
        if shift.is_none() && !pending.is_empty() {
            let (m, base_sec) = match cached_starts.flatten() {
                Some((vdts, _, a)) => (shifts(&tracks, vdts, a), vdts.min(a)),
                None => (HashMap::new(), fallback_start),
            };
            let _ = flush_pending(&tx, &mut pending, &m, emit_start, base_sec).await;
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap_box(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(typ);
        out.extend_from_slice(body);
        out
    }

    fn build_trak(id: u32, video: bool, timescale: u32) -> Vec<u8> {
        let mut tkhd = vec![0u8; 20];
        tkhd[12..16].copy_from_slice(&id.to_be_bytes());
        let mut mdhd = vec![0u8; 20];
        mdhd[12..16].copy_from_slice(&timescale.to_be_bytes());
        let mut hdlr = vec![0u8; 16];
        hdlr[8..12].copy_from_slice(if video { b"vide" } else { b"soun" });
        let mut mdia_body = Vec::new();
        mdia_body.extend_from_slice(&wrap_box(b"mdhd", &mdhd));
        mdia_body.extend_from_slice(&wrap_box(b"hdlr", &hdlr));
        let mut trak_body = Vec::new();
        trak_body.extend_from_slice(&wrap_box(b"tkhd", &tkhd));
        trak_body.extend_from_slice(&wrap_box(b"mdia", &mdia_body));
        wrap_box(b"trak", &trak_body)
    }

    fn build_moof(track_id: u32, tfdt_val: u64, cto: Option<i32>) -> Vec<u8> {
        let mut tfhd = vec![0u8; 8];
        tfhd[4..8].copy_from_slice(&track_id.to_be_bytes());
        let mut tfdt = vec![0u8; 12];
        tfdt[0] = 1; // v1 64-bit
        tfdt[4..12].copy_from_slice(&tfdt_val.to_be_bytes());
        let mut trun = Vec::new();
        if let Some(c) = cto {
            // ver=1, flags=0x000800 (sample_composition_time_offsets_present)
            trun.extend_from_slice(&((1u32 << 24) | 0x800).to_be_bytes());
            trun.extend_from_slice(&1u32.to_be_bytes()); // sample_count = 1
            trun.extend_from_slice(&(c as u32).to_be_bytes());
        } else {
            trun.extend_from_slice(&0u32.to_be_bytes());
            trun.extend_from_slice(&1u32.to_be_bytes());
        }
        let mut traf_body = Vec::new();
        traf_body.extend_from_slice(&wrap_box(b"tfhd", &tfhd));
        traf_body.extend_from_slice(&wrap_box(b"tfdt", &tfdt));
        traf_body.extend_from_slice(&wrap_box(b"trun", &trun));
        wrap_box(b"moof", &wrap_box(b"traf", &traf_body))
    }

    #[test]
    fn truncated_and_empty_boxes_never_panic() {
        // Empty tkhd/mdhd inside moov
        let bad_trak = wrap_box(
            b"trak",
            &[
                wrap_box(b"tkhd", &[]),
                wrap_box(b"mdia", &wrap_box(b"mdhd", &[])),
            ]
            .concat(),
        );
        assert!(parse_moov(&bad_trak).is_empty());

        // Empty tfdt and trun inside moof
        let bad_traf = wrap_box(
            b"traf",
            &[
                wrap_box(b"tfhd", &[0, 0, 0, 0, 0, 0, 0, 1]),
                wrap_box(b"tfdt", &[]),
                wrap_box(b"trun", &[]),
            ]
            .concat(),
        );
        let mut bad_moof = wrap_box(b"moof", &bad_traf);
        let mut sh = HashMap::new();
        sh.insert(1, 5000);
        patch_moof(&mut bad_moof, &sh);
        assert_eq!(first_cto(&bad_moof, 1), None);

        // Truncated moof header
        let mut tiny = vec![0u8; 4];
        patch_moof(&mut tiny, &sh);
        assert_eq!(first_cto(&tiny, 1), None);
    }

    #[tokio::test]
    async fn audio_first_moof_defers_video_cto_and_aligns_both() {
        let mut stream_bytes = Vec::new();
        let moov = wrap_box(
            b"moov",
            &[build_trak(1, true, 1000), build_trak(2, false, 1000)].concat(),
        );
        stream_bytes.extend_from_slice(&moov);
        // Audio-only first moof (track 2) + small mdat
        stream_bytes.extend_from_slice(&build_moof(2, 0, None));
        stream_bytes.extend_from_slice(&wrap_box(b"mdat", &[1, 2, 3, 4]));
        // Video moof (track 1) with CTO = 120 ms (B-frame reorder delay)
        stream_bytes.extend_from_slice(&build_moof(1, 0, Some(120)));
        stream_bytes.extend_from_slice(&wrap_box(b"mdat", &[5, 6, 7, 8]));

        // vdts=10.0, vpts=10.120, audio=10.200 -> true v = 10.120 - 0.120 = 10.000 -> audio shift = +200 ms!
        let mut rx = rewrite_with_fallback(
            std::io::Cursor::new(stream_bytes),
            || Some((10.0, 10.120, 10.200)),
            true,
            10.0,
        );
        let mut out = Vec::new();
        while let Some(item) = rx.recv().await {
            out.extend_from_slice(&item.unwrap());
        }
        let boxes = children(&out);
        // Verify TVST free box precedes the first moof and carries base_sec = 10.0
        let free_box = boxes
            .iter()
            .find(|(t, _)| *t == b"free")
            .expect("missing TVST box");
        let fb = &out[free_box.1.clone()];
        assert_eq!(&fb[0..4], b"TVST");
        let tvst = f64::from_be_bytes(fb[4..12].try_into().unwrap());
        assert!((tvst - 10.0).abs() < 1e-6);

        // Verify audio moof tfdt was shifted by +200 (using deferred video CTO!)
        let first_moof_range = boxes
            .iter()
            .find(|(t, _)| *t == b"moof")
            .expect("missing audio moof")
            .1
            .clone();
        let traf = find(&out[first_moof_range], b"traf").unwrap();
        let tfdt = find(traf, b"tfdt").unwrap();
        let audio_tfdt = u64::from_be_bytes(tfdt[4..12].try_into().unwrap());
        assert_eq!(audio_tfdt, 200);
    }

    #[tokio::test]
    async fn missing_markers_emit_requested_start_fallback() {
        let mut stream_bytes = Vec::new();
        let moov = wrap_box(b"moov", &build_trak(1, true, 1000));
        stream_bytes.extend_from_slice(&moov);
        stream_bytes.extend_from_slice(&build_moof(1, 0, Some(0)));

        let mut rx = rewrite_with_fallback(
            std::io::Cursor::new(stream_bytes),
            || None,
            true,
            1234.5,
        );
        let mut out = Vec::new();
        while let Some(item) = rx.recv().await {
            out.extend_from_slice(&item.unwrap());
        }
        let boxes = children(&out);
        let free_box = boxes
            .iter()
            .find(|(t, _)| *t == b"free")
            .expect("missing TVST box");
        let fb = &out[free_box.1.clone()];
        assert_eq!(&fb[0..4], b"TVST");
        let tvst = f64::from_be_bytes(fb[4..12].try_into().unwrap());
        assert!((tvst - 1234.5).abs() < 1e-6);
    }
}
