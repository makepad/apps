//! A transition between two Graphics, on the real GPU through the standalone
//! renderer in a hidden window: each graphic renders its frame at its own
//! local time into a texture, as a video clip's frame does, and the
//! transition mixes the two. A red and a blue Motion document with a
//! dissolve over their overlap: red before it, blue after it, and both mixed
//! in the middle. Needs a GPU and a window server:
//!
//! `cargo test --release -p makepad-story-edit-render --test graphic_transition_gpu`

use makepad_widgets::ImageBuffer;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_makepad-edit-render"))
        .args(args)
        .env("MAKEPAD_HIDE_WINDOWS", "1")
        .output()
        .expect("makepad-edit-render runs");
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "makepad-edit-render {args:?} failed:\n{text}");
    text
}

/// The mean (red, green, blue) of frame `n`.
fn mean(dir: &Path, n: usize) -> [f64; 3] {
    let bytes = std::fs::read(dir.join(format!("frame_{n:05}.jpg"))).expect("frame dumped");
    let image = ImageBuffer::from_jpg(&bytes).expect("a jpeg");
    let mut sum = [0f64; 3];
    for p in &image.data {
        sum[0] += ((p >> 16) & 0xff) as f64;
        sum[1] += ((p >> 8) & 0xff) as f64;
        sum[2] += (p & 0xff) as f64;
    }
    sum.map(|v| v / image.data.len() as f64)
}

#[test]
fn a_transition_mixes_two_graphics() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_graphic_transition_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let s = |p: &str| dir.join(p).to_string_lossy().to_string();
    let (red, blue) = ("ast_aaaaaaaaaaaaaaaaaaaaaaaaaa", "ast_eeeeeeeeeeeeeeeeeeeeeeeeee");
    let rev = format!("arev_{}", "5".repeat(64));
    for (name, colour) in [("red", "#ff0000"), ("blue", "#0000ff")] {
        std::fs::write(dir.join(format!("{name}.splash")), format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 4.0 background: {colour} layers: []}}")).unwrap();
    }
    std::fs::write(dir.join("media.txt"), format!("{red}=red.splash\n{blue}=blue.splash\n")).unwrap();
    std::fs::write(dir.join("cut.splash"), format!(
        "use mod.flow.*\nuse mod.cut.*\nlet a = Graphic{{src: asset(\"{red}\", \"{rev}\") span: [0.0, 2.5]}}\nlet b = Graphic{{src: asset(\"{blue}\", \"{rev}\") span: [1.5, 2.5]}}\n\
         let tr = Transition{{a: a.out(@frame) b: b.out(@frame) effect: \"\" span: [1.5, 1.0]}}\nlet st = Stack{{layers: [tr.out(@frame)]}}\nlet out = CutOutput{{frame: st.out(@frame)}}\n\
         Flow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 4.0\n    bpm: 120\n    a, b, tr, st, out\n}}\n"
    )).unwrap();
    let out = run(&["render", "--edit", &s("cut.splash"), "--media-file", &s("media.txt"), "--out", &s("out.mp4"), "--dump", &s("."), "--every", "2"]);
    assert!(out.contains("done 120 frames"), "{out}");
    let (before, middle, after) = (mean(&dir, 20), mean(&dir, 60), mean(&dir, 110));
    eprintln!("before {before:?} middle {middle:?} after {after:?}");
    assert!(before[0] > 200.0 && before[2] < 40.0, "red before the transition: {before:?}");
    assert!(after[2] > 200.0 && after[0] < 40.0, "blue after it: {after:?}");
    assert!(middle[0] > 60.0 && middle[2] > 60.0 && middle[0] < 200.0 && middle[2] < 200.0, "both graphics mixed mid-transition: {middle:?}");
}
