//! Stamps the still renderer into the build: STILL_RENDERER_STAMP is an FNV-1a
//! hash of every source a slide thumbnail's pixels depend on. The still cache
//! key (src/still_cache.rs) includes it, so a renderer fix makes old
//! cached thumbnails a cache miss on its own.
use std::{fs, path::{Path, PathBuf}};

/// The one list: paths relative to the makepad checkout root (the manifest dir
/// is <root>/apps/apps/story/app, so it resolves in any worktree). Follow
/// a still's render path: Splash VM and shader lowering, the HAL and its
/// client, the draw/pass/texture core, the motion graphics and kinetic type
/// that title slides draw with, the edit renderer, its effects and the kits.
const RENDERER: &[&str] = &[
    // makepad: script VM + shader lowering, shader compiler, GPU backends.
    "platform/script", "platform/shader", "platform/hal", "platform/src/os/hal",
    // makepad: draw core a pass is built from.
    "platform/src/frame_build.rs", "platform/src/draw_shader.rs", "platform/src/draw_shader_layout.rs",
    "platform/src/draw_pass.rs", "platform/src/draw_list.rs", "platform/src/draw_vars.rs",
    "platform/src/texture.rs", "platform/src/gpu_texture.rs", "platform/src/shader_pack.rs",
    "draw/src",
    // makepad: render libraries a slide's graphics use.
    "libs/kinetic_type", "libs/render", "libs/render_graph", "libs/render_kernels",
    "libs/render_material", "libs/render_lines",
    // makepad/apps: motion graphics, the edit renderer, effects and kits.
    "apps/apps/engine/motion", "apps/apps/engine/motion_draw",
    "apps/apps/story/libs/edit_render", "apps/apps/engine/render_kit",
    "apps/apps/engine/motion3d",
    "apps/apps/story/libs/cut",
];

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() { out.push(path.to_path_buf()); return; }
    let entries = fs::read_dir(path).unwrap_or_else(|e| panic!("still renderer stamp: {}: {e}", path.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | "tests" | "examples" | "docs") { collect(&path, out); }
        } else if matches!(path.extension().and_then(|e| e.to_str()), Some("rs" | "splash" | "glsl" | "wgsl" | "metal" | "hlsl" | "msl" | "frag" | "vert" | "slang")) {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../../..");
    let mut files = Vec::new();
    for entry in RENDERER {
        let path = root.join(entry);
        println!("cargo:rerun-if-changed={}", path.display());
        collect(&path, &mut files);
    }
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| for byte in bytes { hash ^= *byte as u64; hash = hash.wrapping_mul(0x0000_0100_0000_01b3); };
    for file in &files {
        let relative = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().into_owned();
        feed(relative.as_bytes());
        feed(&[0]);
        let bytes = fs::read(file).unwrap_or_else(|e| panic!("still renderer stamp: {}: {e}", file.display()));
        feed(&(bytes.len() as u64).to_le_bytes());
        feed(&bytes);
    }
    println!("cargo:rustc-env=STILL_RENDERER_STAMP={hash:016x}-{}", files.len());
}
