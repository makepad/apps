//! `makepad-motion3d` checks Scene3D documents, renders frames and strips,
//! and benchmarks scenes on the GPU in its own (normally hidden) window.
//!
//! ```text
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion3d check <doc.splash>
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion3d render <doc.splash> --at 0,1.5,3 [--size 1920x1080] [--fps 30] --out <dir>
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion3d strip <doc.splash> [--frames 8] [--cols 4] [--size 480x270] [--duration 6] [--edit-times e,e,..] --out strip.png
//! MAKEPAD_HIDE_WINDOWS=1 makepad-motion3d bench <doc.splash> [--size 3840x2160] [--frames 30]
//! ```
//!
//! The GPU pixel tests run in the `pixels` test target.
//!
//! Common options: `--music synthetic[:bpm]` (a stand-in beat track),
//! `--edit-offset s`, `--library <dir>` (asset ids are `<dir>/<id>.<ext>`),
//! `--media id=file`, `--input name=value`. A document is a scene-only
//! source (`Scene3D{...}`); a Motion document with scenes is drawn by
//! `makepad-motion`.

use makepad_stage_motion3d::frame_tool::App;
use makepad_widgets::*;

app_main!(App);
