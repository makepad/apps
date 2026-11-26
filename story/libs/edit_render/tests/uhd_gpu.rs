//! UHD 4K end to end: a 3840x2160 source cut into a 3840x2160 edit (two
//! clips, a transition and an effect) exports through the standalone binary
//! in a hidden window at full size, and so does its 1080p twin for scale.
//! Prints each export's frames, wall time and the binary's peak resident
//! memory (`/usr/bin/time -l`); timings are only meaningful on the quiet
//! benchmark machine. Needs a GPU and a window server:
//!
//! `cargo test --release -p makepad-story-edit-render --test uhd_gpu -- --nocapture`

#![cfg(target_os = "macos")]

use makepad_asset_data::{AssetId, BlobId};
use makepad_widgets::makepad_platform::video_file::{VideoFileCodec, VideoFileDecoder, VideoFileEncoder, VideoFileEncoderOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const SECONDS: u32 = 4;
const FPS: u32 = 30;

/// A moving gradient with a bar, every frame different.
fn write_clip(path: &Path, width: u32, height: u32) {
    let options = VideoFileEncoderOptions { codec: VideoFileCodec::H265, width, height, fps_num: FPS, fps_den: 1, video_bitrate_bps: 40_000_000, ..Default::default() };
    let mut e = VideoFileEncoder::new(path.to_str().unwrap(), options).unwrap();
    let (w, h) = (width as usize, height as usize);
    let mut nv12 = vec![128u8; w * h + w * h / 2];
    for k in 0..(SECONDS * FPS) as usize {
        let bar = (k * w / (SECONDS * FPS) as usize) % w;
        for y in 0..h {
            let row = &mut nv12[y * w..(y + 1) * w];
            for (x, px) in row.iter_mut().enumerate() {
                *px = if x.abs_diff(bar) < w / 64 { 235 } else { (16 + (x + y + k * 8) * 219 / (w + h + 960)) as u8 };
            }
        }
        for (i, c) in nv12[w * h..].iter_mut().enumerate() {
            *c = (64 + (i / w * 128 / (h / 2)) as u8).wrapping_add(k as u8);
        }
        e.push_frame_nv12(&nv12, Some(k as i64 * 10_000_000 / FPS as i64)).unwrap();
    }
    e.finish().unwrap();
}

fn rev(path: &Path) -> String {
    let digest = BlobId::hash_of(&std::fs::read(path).unwrap());
    format!("arev_{}", digest.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Export the edit at `width` x `height`: (frames, seconds, peak RSS MB).
fn export(dir: &Path, tag: &str, width: u32, height: u32) -> (u64, f64, u64) {
    let clip = dir.join(format!("{tag}.mp4"));
    write_clip(&clip, width, height);
    let id = AssetId::from_bytes([0x4b; 16]).to_string();
    let r = rev(&clip);
    let edit = format!(
        "use mod.flow.*\nuse mod.cut.*\n\nlet a = Clip{{src: asset(\"{id}\", \"{r}\") span: [0.0, 2.5] fit: @fill}}\nlet b = Clip{{src: asset(\"{id}\", \"{r}\") in: 1.0 span: [1.5, 2.5] fit: @fill}}\nlet wipe = Transition{{a: a.out(@frame) b: b.out(@frame) effect: \"\" span: [1.5, 1.0]}}\nlet tint = Fx{{input: wipe.out(@frame) effect: \"kit/hue_saturation\" span: [0.0, 4.0]}}\nlet out = CutOutput{{frame: tint.out(@frame) audio: []}}\n\nFlow{{\n    kind: @cut\n    label: \"uhd {tag}\"\n    output: {{width: {width} height: {height} fps: {FPS}}}\n    duration: {SECONDS}.0\n    a, b, wipe, tint, out\n}}\n"
    );
    let edit_path = dir.join(format!("{tag}.splash"));
    std::fs::write(&edit_path, edit).unwrap();
    let media = dir.join(format!("{tag}.media.txt"));
    std::fs::write(&media, format!("{id}={}\n", clip.display())).unwrap();
    let out = dir.join(format!("{tag}.out.mp4"));
    let start = Instant::now();
    let output = Command::new("/usr/bin/time")
        .arg("-l")
        .arg(env!("CARGO_BIN_EXE_makepad-edit-render"))
        .args(["render", "--edit", &edit_path.to_string_lossy(), "--media-file", &media.to_string_lossy(), "--out", &out.to_string_lossy(), "--no-burn", "--loudness", "off"])
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("makepad-edit-render runs");
    let seconds = start.elapsed().as_secs_f64();
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{tag} export failed:\n{text}");
    let rss = text.lines().find(|l| l.contains("maximum resident set size")).and_then(|l| l.split_whitespace().next()).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0) >> 20;
    let mut d = VideoFileDecoder::open(out.to_str().unwrap()).unwrap();
    assert_eq!((d.info().width, d.info().height), (width, height), "{tag}: the export's size");
    let mut frames = 0u64;
    while d.next_frame().unwrap().is_some() {
        frames += 1;
    }
    assert_eq!(frames, (SECONDS * FPS) as u64, "{tag}: every frame exported");
    for line in text.lines().filter(|l| l.contains("stages") || l.starts_with("done")) {
        println!("{tag}: {line}");
    }
    (frames, seconds, rss)
}

#[test]
fn uhd_edit_exports_at_full_size() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_uhd_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (tag, w, h) in [("hd", 1920, 1080), ("uhd", 3840, 2160), ("uhd-portrait", 2160, 3840)] {
        let (frames, seconds, rss) = export(&dir, tag, w, h);
        println!("{tag} {w}x{h}: {frames} frames in {seconds:.2} s ({:.1} fps), peak RSS {rss} MB", frames as f64 / seconds);
    }
}
