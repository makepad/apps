//! Colour on the real GPU, end to end: clips written in each colour (BT.709
//! limited and full, BT.601 525 and 625, BT.2020 SDR, PQ and HLG) are cut
//! into an edit, exported through the standalone binary in a hidden window
//! (the frames drawn by `DrawNv12` / `mod.draw.VideoColor`, read back and
//! encoded in the edit's output colour), and decoded again. Each flat patch
//! must come back at the display colour `VideoColor::reference_rgb` gives for
//! the source bytes, within the two encodes' rounding; the export must be
//! tagged with the output colour. A second pass writes one source into
//! every output colour (`output.matrix` / `output.range`), whose bytes must
//! be the picture in that colour (primaries, matrix and range converted on
//! the GPU by the export's picture pass). Needs a GPU and a window server:
//!
//! `cargo test --release -p makepad-story-edit-render --test color_gpu -- --nocapture`

use makepad_asset_data::{AssetId, BlobId};
use makepad_story_cut::{OutputFormat, OutputMatrix, OutputRange};
use makepad_story_edit_render::render::output_color;
use makepad_widgets::makepad_platform::video_file::color::{pq_inverse_eotf, HDR_REFERENCE_WHITE_NITS};
use makepad_widgets::makepad_platform::video_file::{
    VideoColor, VideoFileCodec, VideoFileDecoder, VideoFileEncoder, VideoFileEncoderOptions, VideoMatrix, VideoPrimaries, VideoRange, VideoTransfer,
};
use std::path::{Path, PathBuf};
use std::process::Command;

const PATCHES: usize = 4;
/// Two 8-bit quantisations, a 4:2:0 round trip and two hardware encodes.
const TOLERANCE: i32 = 4;

struct Clip {
    name: &'static str,
    width: u32,
    height: u32,
    codec: VideoFileCodec,
    color: VideoColor,
    signal: [[f32; 3]; PATCHES],
}

fn clips() -> Vec<Clip> {
    let sdr = [[0.9, 0.9, 0.9], [0.8, 0.15, 0.1], [0.1, 0.7, 0.2], [0.15, 0.2, 0.85]];
    let c = |matrix, range, primaries, transfer| VideoColor { matrix, range, primaries, transfer };
    use VideoMatrix as M;
    use VideoPrimaries as P;
    use VideoRange as R;
    use VideoTransfer as T;
    let pq = |nits: f32| pq_inverse_eotf(nits);
    let w = HDR_REFERENCE_WHITE_NITS;
    vec![
        Clip { name: "bt709-limited", width: 1280, height: 720, codec: VideoFileCodec::H264, color: VideoColor::BT709, signal: sdr },
        Clip { name: "bt709-full", width: 1280, height: 720, codec: VideoFileCodec::H264, color: c(M::Bt709, R::Full, P::Bt709, T::Sdr), signal: sdr },
        Clip { name: "bt601-525", width: 720, height: 480, codec: VideoFileCodec::H264, color: c(M::Bt601, R::Limited, P::Smpte170m, T::Sdr), signal: sdr },
        Clip { name: "bt601-625", width: 720, height: 576, codec: VideoFileCodec::H264, color: c(M::Bt601, R::Limited, P::Bt470bg, T::Sdr), signal: sdr },
        Clip { name: "bt2020-sdr", width: 1920, height: 1080, codec: VideoFileCodec::H265, color: c(M::Bt2020, R::Limited, P::Bt2020, T::Sdr), signal: sdr },
        Clip { name: "bt2020-pq", width: 1920, height: 1080, codec: VideoFileCodec::H265, color: c(M::Bt2020, R::Limited, P::Bt2020, T::Pq), signal: [[pq(100.0); 3], [pq(w); 3], [pq(1000.0); 3], [pq(w), pq(5.0), pq(5.0)]] },
        Clip { name: "bt2020-hlg", width: 1920, height: 1080, codec: VideoFileCodec::H265, color: c(M::Bt2020, R::Limited, P::Bt2020, T::Hlg), signal: [[0.5; 3], [0.75; 3], [1.0; 3], [0.75, 0.2, 0.2]] },
    ]
}

fn write_clip(clip: &Clip, path: &Path) -> [[u8; 3]; PATCHES] {
    let bytes = clip.signal.map(|s| clip.color.encode_ycbcr(s));
    let (w, h) = (clip.width as usize, clip.height as usize);
    let band = w / PATCHES;
    let mut nv12 = vec![0u8; w * h + w * h / 2];
    for y in 0..h {
        for x in 0..w {
            nv12[y * w + x] = bytes[(x / band).min(PATCHES - 1)][0];
        }
    }
    for y in 0..h / 2 {
        for bx in 0..w / 2 {
            let p = bytes[(bx * 2 / band).min(PATCHES - 1)];
            nv12[w * h + y * w + bx * 2] = p[1];
            nv12[w * h + y * w + bx * 2 + 1] = p[2];
        }
    }
    let options = VideoFileEncoderOptions { codec: clip.codec, width: clip.width, height: clip.height, fps_num: 30, fps_den: 1, video_bitrate_bps: 30_000_000, keyframe_only: true, color: clip.color, ..Default::default() };
    let mut e = VideoFileEncoder::new(path.to_str().unwrap(), options).unwrap();
    for k in 0..30 {
        e.push_frame_nv12(&nv12, Some(k * 333_333)).unwrap();
    }
    e.finish().unwrap();
    bytes
}

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-edit-render")).args(args).env("MAKEPAD_HIDE_WINDOWS", "1").output().expect("makepad-edit-render runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "makepad-edit-render {args:?} failed:\n{text}");
    text
}

fn spelling(output: &OutputFormat) -> String {
    let matrix = match output.matrix {
        OutputMatrix::Bt709 => "@bt709",
        OutputMatrix::Bt601 => "@bt601",
        OutputMatrix::Bt2020 => "@bt2020",
    };
    let range = match output.range {
        OutputRange::Limited => "@limited",
        OutputRange::Full => "@full",
    };
    format!("matrix: {matrix} range: {range}")
}

/// Export `clip_path` (`width` x `height`) through an edit with `output`, and
/// return the export's patch bytes and its colour.
fn export(dir: &Path, tag: &str, clip_path: &Path, width: u32, height: u32, output: &OutputFormat) -> ([[u8; 3]; PATCHES], VideoColor) {
    let id = AssetId::from_bytes([0x5c; 16]).to_string();
    let digest = BlobId::hash_of(&std::fs::read(clip_path).unwrap());
    let rev = format!("arev_{}", digest.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>());
    let edit = format!(
        "use mod.flow.*\nuse mod.cut.*\n\nlet a = Clip{{src: asset(\"{id}\", \"{rev}\") span: [0.0, 1.0] fit: @fill}}\nlet out = CutOutput{{frame: a.out(@frame) audio: []}}\n\nFlow{{\n    kind: @cut\n    label: \"colour {tag}\"\n    output: {{width: {width} height: {height} fps: 30 {}}}\n    duration: 1.0\n    a, out\n}}\n",
        spelling(output)
    );
    let edit_path = dir.join(format!("{tag}.splash"));
    std::fs::write(&edit_path, edit).unwrap();
    let media = dir.join(format!("{tag}.media.txt"));
    std::fs::write(&media, format!("{id}={}\n", clip_path.display())).unwrap();
    let out = dir.join(format!("{tag}.out.mp4"));
    run(&["render", "--edit", &edit_path.to_string_lossy(), "--media-file", &media.to_string_lossy(), "--out", &out.to_string_lossy(), "--no-burn", "--loudness", "off"]);
    let mut d = VideoFileDecoder::open(out.to_str().unwrap()).unwrap();
    let color = d.info().color;
    let mut frame = d.next_frame().unwrap().unwrap();
    // a frame from the middle: past any encoder ramp
    for _ in 0..10 {
        if let Some(next) = d.next_frame().unwrap() {
            frame = next;
        }
    }
    assert_eq!(frame.color, color, "{tag}: frame and stream colour of the export");
    let (w, h) = (frame.width as usize, frame.height as usize);
    let band = w / PATCHES;
    let bytes = [0, 1, 2, 3].map(|i| {
        let (x, y) = (i * band + band / 2, h / 2);
        [frame.nv12[y * w + x], frame.nv12[w * h + (y / 2) * w + (x / 2) * 2], frame.nv12[w * h + (y / 2) * w + (x / 2) * 2 + 1]]
    });
    (bytes, color)
}

fn to8(rgb: [f32; 3]) -> [i32; 3] {
    rgb.map(|v| (v * 255.0).round() as i32)
}

fn close(a: [i32; 3], b: [i32; 3]) -> bool {
    (0..3).all(|c| (a[c] - b[c]).abs() <= TOLERANCE)
}

#[test]
fn every_source_colour_is_drawn_right_and_every_output_colour_is_written_right() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_color_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();
    // Every source colour into the default output (BT.709 limited).
    let mut sources = Vec::new();
    for clip in clips() {
        let path = dir.join(format!("{}.mp4", clip.name));
        let bytes = write_clip(&clip, &path);
        let (out, color) = export(&dir, clip.name, &path, clip.width, clip.height, &OutputFormat::default());
        assert_eq!(color, VideoColor::BT709, "{}: the default export colour", clip.name);
        for i in 0..PATCHES {
            let want = to8(clip.color.reference_rgb(bytes[i][0], bytes[i][1], bytes[i][2]));
            let got = to8(color.reference_rgb(out[i][0], out[i][1], out[i][2]));
            let ok = close(want, got);
            println!("{:14} patch {i}: source {:?} -> drawn {got:?}, reference {want:?} {}", clip.name, bytes[i], if ok { "ok" } else { "FAIL" });
            if !ok {
                failures.push(format!("{} patch {i}: {got:?} vs {want:?}", clip.name));
            }
        }
        sources.push((clip, path, bytes));
    }
    // One SDR source into every output colour.
    let (clip, path, bytes) = &sources[0];
    for (matrix, range) in [(OutputMatrix::Bt709, OutputRange::Full), (OutputMatrix::Bt601, OutputRange::Limited), (OutputMatrix::Bt2020, OutputRange::Limited), (OutputMatrix::Bt2020, OutputRange::Full)] {
        let output = OutputFormat { matrix, range, ..OutputFormat::default() };
        let tag = format!("out-{matrix:?}-{range:?}").to_lowercase();
        let (out, color) = export(&dir, &tag, path, clip.width, clip.height, &output);
        assert_eq!(color, output_color(&output), "{tag}: the export is tagged with its output colour");
        for i in 0..PATCHES {
            // the picture (BT.709 display RGB) in the output's primaries and
            // YCbCr: compared where it is written, since a round trip back
            // to BT.709 amplifies the encodes' rounding in dark channels and
            // cannot restore what a smaller gamut clipped
            let picture = clip.color.reference_rgb(bytes[i][0], bytes[i][1], bytes[i][2]);
            let want = color.encode_ycbcr(color.reference_from_bt709(picture)).map(|v| v as i32);
            let got = out[i].map(|v| v as i32);
            let ok = close(want, got);
            println!("{tag:18} patch {i}: picture {:?} -> written {got:?}, reference {want:?} {}", to8(picture), if ok { "ok" } else { "FAIL" });
            if !ok {
                failures.push(format!("{tag} patch {i}: {got:?} vs {want:?}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
