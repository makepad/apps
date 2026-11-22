//! What a capture session holds, as numbers: its cameras (picture size, focal lengths, how far
//! apart they sit), and how far the headset's depth says the surfaces were from it, keyframe by
//! keyframe and over the session. No picture is read.
//!
//! usage: scan-session-facts <session>

use makepad_desk_link::xr_capture::{decode_plane, CaptureRecord};
use makepad_fab_scan::session::picture_intrinsics;
use std::path::PathBuf;

fn at(sorted: &[f32], q: f64) -> f32 {
    sorted.get((q * sorted.len().saturating_sub(1) as f64).round() as usize).copied().unwrap_or(0.0)
}

fn run() -> Result<(), String> {
    let folder = PathBuf::from(std::env::args().nth(1).ok_or("usage: scan-session-facts <session>")?);
    let mut names: Vec<PathBuf> = std::fs::read_dir(&folder).map_err(|e| format!("the session: {}", e.kind()))?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bin")).collect();
    names.sort();
    let (mut keyframes, mut middles, mut all) = (0usize, Vec::new(), Vec::new());
    for name in names {
        let Ok(record) = CaptureRecord::decode(&std::fs::read(&name).map_err(|e| e.kind().to_string())?) else { continue };
        match record {
            CaptureRecord::Cameras(list) => {
                for (i, camera) in list.iter().enumerate() {
                    let k = picture_intrinsics(camera).map_or("none".to_string(), |k| format!("{:.1}/{:.1}/{:.1}/{:.1}", k[0], k[1], k[2], k[3]));
                    println!("CAMERA {i} size={}x{} fx_fy_cx_cy={k} distortion={} lens_at={:?}", camera.size[0], camera.size[1], camera.distortion.is_some(), camera.lens_pose.as_ref().map(|l| l.translation));
                }
                let lenses: Vec<[f32; 3]> = list.iter().filter_map(|c| c.lens_pose.as_ref().map(|l| l.translation)).collect();
                if let [a, b, ..] = lenses[..] {
                    println!("CAMERAS apart_mm={:.1}", 1000.0 * (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt());
                }
            }
            CaptureRecord::Keyframe(keyframe) => {
                keyframes += 1;
                let sizes: Vec<String> = keyframe.views.iter().map(|v| v.planes.iter().map(|p| format!("{}x{}", p.width, p.height)).collect::<Vec<_>>().join("+")).collect();
                let Some(depth) = &keyframe.depth else {
                    println!("KEYFRAME {} views={} no_depth", keyframe.index, sizes.join(" "));
                    continue;
                };
                let layers = depth.layers.max(1) as usize;
                let (w, h) = (depth.texels.width as usize, depth.texels.height as usize / layers);
                let bytes = decode_plane(&depth.texels)?;
                // the middle half of the first eye's image: what the wearer looks at
                let mut metres = Vec::new();
                for y in h / 4..3 * h / 4 {
                    for x in w / 4..3 * w / 4 {
                        let i = (y * w + x) * 2;
                        let value = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
                        if value == 0 || value == u16::MAX {
                            continue;
                        }
                        let ndc = f32::from(value) / f32::from(u16::MAX) * 2.0 - 1.0;
                        let z = if depth.far > depth.near { 2.0 * depth.far * depth.near / ((depth.far + depth.near) - ndc * (depth.far - depth.near)) } else { 2.0 * depth.near / (1.0 - ndc) };
                        if z.is_finite() && z > 0.05 && z < 10.0 {
                            metres.push(z);
                        }
                    }
                }
                metres.sort_by(f32::total_cmp);
                println!("KEYFRAME {} views={} depth={w}x{h}x{layers} middle_m p10={:.2} p50={:.2} p90={:.2}", keyframe.index, sizes.join(" "), at(&metres, 0.1), at(&metres, 0.5), at(&metres, 0.9));
                middles.push(at(&metres, 0.5));
                all.extend(metres.iter().step_by(16));
            }
            _ => {}
        }
    }
    middles.sort_by(f32::total_cmp);
    all.sort_by(f32::total_cmp);
    println!("SESSION keyframes={keyframes} keyframe_middle_m p10={:.2} p50={:.2} p90={:.2} all_middle_pixels_m p10={:.2} p50={:.2} p90={:.2}", at(&middles, 0.1), at(&middles, 0.5), at(&middles, 0.9), at(&all, 0.1), at(&all, 0.5), at(&all, 0.9));
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("scan-session-facts: {e}");
        std::process::exit(1);
    }
}
