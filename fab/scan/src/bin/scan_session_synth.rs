//! A made-up capture session in the headset's own records, with its model: a textured box on a
//! textured table, a rig of two cameras carried once around it, and the headset's depth image
//! of each keyframe as its runtime would give it. What `scan-session` is tried on
//! before it meets a real one; no camera ever saw these.
//!
//! usage: scan-session-synth [--keyframes <n>] [--size <px>] [--noise-mm <mm>] [--noise-deg <deg>]
//!        [--radius <m>] [--height <m>] [--grow <k>] [--shape box|stepped] [--step-mm <mm>] [--turned] <out>
//!
//! `--radius` and `--height`: the ring the head walks, about the object and over the table;
//! `--grow`: the object's size, times the 100 x 80 x 60 mm box; `--shape stepped`: the box with a
//! plate on it, 0.6 times its width and depth and `--step-mm` high (the step a scan must show).

use makepad_ai_vision::scan_scenes::{render_with, Camera, RenderOptions, Scene, Shape, Surface};
use makepad_desk_link::xr_capture::{encode_plane, CaptureCamera, CaptureDepth, CaptureEye, CaptureKeyframe, CaptureLensPose, CapturePose, CaptureRecord, CaptureView};
use makepad_fab_scan::colour::i420_planes;
use makepad_fab_scan::session::camera_to_world;
use std::path::PathBuf;

/// The object's half sizes: x, height / 2, z.
const HALF: [f64; 3] = [0.05, 0.04, 0.03];
const RING_RADIUS: f64 = 0.45;
const RING_HEIGHT: f64 = 0.28;
const TILT_DEG: f64 = 11.25;
/// The headset's depth image: its size a side, half its field of view (rad), its near plane and
/// half the distance between its two views (m).
const DEPTH_SIZE: usize = 160;
const DEPTH_HALF_FOV: f64 = 0.6;
const DEPTH_NEAR: f32 = 0.1;
const DEPTH_HALF_APART: f64 = 0.032;

/// A unit quaternion (x, y, z, w) of a row-major rotation.
fn quaternion(m: [[f64; 3]; 3]) -> [f64; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = 2.0 * (1.0 + trace).sqrt();
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, s / 4.0]
    } else if m[0][0] >= m[1][1] && m[0][0] >= m[2][2] {
        let s = 2.0 * (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt();
        [s / 4.0, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] >= m[2][2] {
        let s = 2.0 * (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt();
        [(m[0][1] + m[1][0]) / s, s / 4.0, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = 2.0 * (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt();
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, s / 4.0, (m[1][0] - m[0][1]) / s]
    }
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|x| x / n)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// The head at `eye`, with the object mid-view of cameras tilted down from it: forward is -z, up y.
fn head_at(eye: [f64; 3], target: [f64; 3]) -> [[f64; 3]; 3] {
    let forward = unit([target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]]);
    let right = unit(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);
    // pitched up by the tilt, so that the tilted cameras look at the target
    let (s, c) = TILT_DEG.to_radians().sin_cos();
    let pitched_forward: [f64; 3] = std::array::from_fn(|i| c * forward[i] + s * up[i]);
    let pitched_up: [f64; 3] = std::array::from_fn(|i| c * up[i] - s * forward[i]);
    // columns: the head's axes in the world
    std::array::from_fn(|i| [right[i], pitched_up[i], -pitched_forward[i]])
}

/// The headset's depth of `scene` from a head at `eye` with the axes `head` (its columns), as its
/// runtime gives it: two views looking along the head's -z, 16-bit values of a projection with an
/// infinite far plane, rows from the bottom; reported at the `tracked` pose.
fn depth_of(scene: &Scene, head: [[f64; 3]; 3], eye: [f64; 3], tracked: &CapturePose, time_ns: u64) -> CaptureDepth {
    let focal = 0.5 * DEPTH_SIZE as f64 / DEPTH_HALF_FOV.tan();
    let middle = DEPTH_SIZE as f64 / 2.0;
    let fov = [-DEPTH_HALF_FOV as f32, DEPTH_HALF_FOV as f32, DEPTH_HALF_FOV as f32, -DEPTH_HALF_FOV as f32];
    let mut texels = Vec::with_capacity(4 * DEPTH_SIZE * DEPTH_SIZE);
    let mut views = [CaptureEye::default(); 2];
    for (side, view) in [-1.0, 1.0].into_iter().zip(&mut views) {
        let off: [f64; 3] = std::array::from_fn(|i| side * DEPTH_HALF_APART * head[i][0]);
        // the view as a camera: x the head's right, y its down, z its forward; then the scene's flip
        let m: [[f64; 4]; 3] = std::array::from_fn(|i| [head[i][0], -head[i][1], -head[i][2], eye[i] + off[i]]);
        let flipped = [m[0], m[1].map(|v| -v), m[2].map(|v| -v), [0.0, 0.0, 0.0, 1.0]];
        let camera = Camera { width: DEPTH_SIZE, height: DEPTH_SIZE, fx: focal, fy: focal, cx: middle, cy: middle, skew: 0.0, distortion: [0.0; 5], cam_to_world: flipped };
        let render = render_with(scene, &camera, &RenderOptions { supersample: 1, surface: Surface::Uniform });
        for row in (0..DEPTH_SIZE).rev() {
            for z in &render.depth_z[row * DEPTH_SIZE..(row + 1) * DEPTH_SIZE] {
                let value = if *z > 0.0 { ((1.0 - DEPTH_NEAR / z) * f32::from(u16::MAX)).round().clamp(1.0, 65534.0) as u16 } else { 0 };
                texels.extend(value.to_le_bytes());
            }
        }
        *view = CaptureEye { pose: CapturePose { orientation: tracked.orientation, position: std::array::from_fn(|i| tracked.position[i] + off[i] as f32) }, fov };
    }
    CaptureDepth { time_ns, near: DEPTH_NEAR, far: 0.0, views, layers: 2, texels: encode_plane(DEPTH_SIZE as u32, 2 * DEPTH_SIZE as u32, 2, &texels) }
}

/// SplitMix64 in [-1, 1).
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        2.0 * ((z >> 11) as f64 / (1u64 << 53) as f64) - 1.0
    }

    fn direction(&mut self) -> [f64; 3] {
        unit([self.next(), self.next(), self.next() + 1e-9])
    }
}

/// The boxes (half sizes x, height / 2, z; their bottom, m) as a binary STL: millimetres, z up,
/// on its plate.
fn model_stl(boxes: &[([f64; 3], f64)]) -> Vec<u8> {
    let mut out = vec![0u8; 80];
    out.extend((12 * boxes.len() as u32).to_le_bytes());
    for (half, bottom) in boxes {
        let (x, y, z, z0) = (half[0] * 1000.0, half[2] * 1000.0, half[1] * 2000.0, bottom * 1000.0);
        let v = |i: f64, j: f64, k: f64| [(i * x) as f32, (j * y) as f32, (z0 + k * z) as f32];
        let quads = [
            [v(-1., -1., 0.), v(1., -1., 0.), v(1., -1., 1.), v(-1., -1., 1.)],
            [v(1., 1., 0.), v(-1., 1., 0.), v(-1., 1., 1.), v(1., 1., 1.)],
            [v(1., -1., 0.), v(1., 1., 0.), v(1., 1., 1.), v(1., -1., 1.)],
            [v(-1., 1., 0.), v(-1., -1., 0.), v(-1., -1., 1.), v(-1., 1., 1.)],
            [v(-1., -1., 1.), v(1., -1., 1.), v(1., 1., 1.), v(-1., 1., 1.)],
            [v(-1., 1., 0.), v(1., 1., 0.), v(1., -1., 0.), v(-1., -1., 0.)],
        ];
        for [a, b, c, d] in quads {
            for triangle in [[a, b, c], [a, c, d]] {
                out.extend([0u8; 12]);
                for corner in triangle {
                    for value in corner {
                        out.extend(value.to_le_bytes());
                    }
                }
                out.extend([0u8; 2]);
            }
        }
    }
    out
}

fn option<T: std::str::FromStr>(args: &mut impl Iterator<Item = String>, usage: &str) -> Result<T, String> {
    args.next().and_then(|v| v.parse().ok()).ok_or_else(|| usage.to_string())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let usage = "usage: scan-session-synth [--keyframes <n>] [--size <px>] [--noise-mm <mm>] [--noise-deg <deg>] [--seed <n>] [--radius <m>] [--height <m>] [--grow <k>] [--shape box|stepped] [--step-mm <mm>] [--turned] [--surface textured|uniform|glossy] <out>";
    let mut args = std::env::args().skip(1);
    let (mut keyframes, mut size, mut noise_mm, mut noise_deg) = (40_usize, 640_usize, 0.0_f64, 0.0_f64);
    let (mut radius, mut height, mut grow) = (RING_RADIUS, RING_HEIGHT, 1.0_f64);
    let (mut stepped, mut step_mm, mut turned) = (false, 4.0_f64, false);
    // (`--surface`: what the colour cameras see on everything; the depth is the same)
    let mut surface = Surface::Textured;
    // (what the tracker's errors are drawn from: another seed, another walk's errors)
    let mut seed = 21_u64;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--keyframes" => keyframes = option(&mut args, usage)?,
            "--size" => size = option(&mut args, usage)?,
            "--noise-mm" => noise_mm = option(&mut args, usage)?,
            "--noise-deg" => noise_deg = option(&mut args, usage)?,
            "--seed" => seed = option(&mut args, usage)?,
            "--radius" => radius = option(&mut args, usage)?,
            "--height" => height = option(&mut args, usage)?,
            "--grow" => grow = option(&mut args, usage)?,
            "--shape" => {
                stepped = match args.next().as_deref() {
                    Some("box") => false,
                    Some("stepped") => true,
                    _ => return Err(usage.into()),
                }
            }
            "--step-mm" => step_mm = option(&mut args, usage)?,
            "--turned" => turned = true,
            "--surface" => {
                surface = match args.next().as_deref() {
                    Some("textured") => Surface::Textured,
                    Some("uniform") => Surface::Uniform,
                    Some("glossy") => Surface::Glossy,
                    _ => return Err(usage.into()),
                }
            }
            _ => out = Some(PathBuf::from(arg)),
        }
    }
    let out = out.ok_or(usage)?;
    let session = out.join("session");
    std::fs::create_dir_all(&session)?;
    // (`--turned`: the object a quarter turn about its up, its longer side along z)
    let half = if turned { [HALF[2], HALF[1], HALF[0]] } else { HALF }.map(|v| v * grow);
    // the stepped shape's plate on the box: its half sizes, and its height as the step
    let plate = [half[0] * 0.6, step_mm / 2000.0, half[2] * 0.6];
    let mut boxes = vec![(half, 0.0)];
    if stepped {
        boxes.push((plate, half[1] * 2.0));
    }
    std::fs::write(out.join("model.stl"), model_stl(&boxes))?;
    let (s, c) = (0.5 * (180.0 - TILT_DEG).to_radians()).sin_cos();
    let rotation = [-s as f32, 0.0, 0.0, c as f32];
    let lenses = [
        CaptureLensPose { rotation, translation: [-0.032, -0.018, -0.061], reference: 1 },
        CaptureLensPose { rotation, translation: [0.032, -0.018, -0.061], reference: 1 },
    ];
    let focal = 0.68 * size as f64;
    let middle = size as f64 / 2.0;
    let cameras: Vec<CaptureCamera> = lenses.iter().enumerate().map(|(i, lens)| CaptureCamera {
        position: i as u8,
        size: [size as u32; 2],
        intrinsics: Some([focal as f32, focal as f32, middle as f32, middle as f32, 0.0]),
        array: [0, 0, size as i32, size as i32],
        active: [0, 0, size as i32, size as i32],
        distortion: None,
        lens_pose: Some(*lens),
    }).collect();
    let mut n = 0;
    let mut write = |record: &CaptureRecord| -> std::io::Result<()> {
        std::fs::write(session.join(format!("record-{n:06}.bin")), record.encode())?;
        n += 1;
        Ok(())
    };
    write(&CaptureRecord::Cameras(cameras))?;
    // The scene's y points down, ours up, about the table's middle: (x, y, z) -> (x, -y, -z).
    let scene = Scene { shape: if stepped { Shape::Stepped { base: half, top: plate } } else { Shape::Box { half } }, ground: true };
    let options = RenderOptions { supersample: 2, surface };
    let mut noise = Noise(seed);
    let target = [0.0, half[1] + if stepped { plate[1] } else { 0.0 }, 0.0];
    let mut poses = String::new();
    for index in 0..keyframes {
        let angle = index as f64 / keyframes as f64 * std::f64::consts::TAU;
        let eye = [radius * angle.sin(), height, radius * angle.cos()];
        let axes = head_at(eye, target);
        let truth = quaternion(axes);
        let head = CapturePose { orientation: truth.map(|v| v as f32), position: eye.map(|v| v as f32) };
        // what the tracker reports: off by the noise
        let shift = noise.direction();
        let axis = noise.direction();
        let half_angle = 0.5 * noise_deg.to_radians();
        let (s, c) = half_angle.sin_cos();
        let [ax, ay, az] = axis.map(|v| v * s);
        let [x, y, z, w] = truth;
        let reported = [c * x + ax * w + ay * z - az * y, c * y - ax * z + ay * w + az * x, c * z + ax * y - ay * x + az * w, c * w - ax * x - ay * y - az * z];
        let tracked = CapturePose {
            orientation: reported.map(|v| v as f32),
            position: std::array::from_fn(|i| (eye[i] + shift[i] * noise_mm / 1000.0) as f32),
        };
        // (where the head was and where the tracker said: a check's measure of what settling leaves)
        poses.push_str(&format!(
            "{} {} {} {} {} {} {}  {} {} {} {} {} {} {}\n",
            eye[0], eye[1], eye[2], truth[0], truth[1], truth[2], truth[3], tracked.position[0], tracked.position[1], tracked.position[2], reported[0], reported[1], reported[2], reported[3]
        ));
        let mut views = Vec::new();
        for (camera, lens) in lenses.iter().enumerate() {
            let m = camera_to_world(&head, lens);
            let flipped = [m[0], m[1].map(|v| -v), m[2].map(|v| -v), m[3]];
            let render = render_with(&scene, &Camera { width: size, height: size, fx: focal, fy: focal, cx: middle, cy: middle, skew: 0.0, distortion: [0.0; 5], cam_to_world: flipped }, &options);
            let rgb: Vec<[f32; 3]> = render.rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2]].map(|v| f32::from(v) / 255.0)).collect();
            views.push(CaptureView {
                camera: camera as u8,
                time_ns: index as u64 * 1_000_000_000,
                head: tracked,
                planes: i420_planes(size, size, &rgb),
                ..Default::default()
            });
        }
        let depth = depth_of(&scene, axes, eye, &tracked, index as u64 * 1_000_000_000);
        write(&CaptureRecord::Keyframe(CaptureKeyframe { index: index as u32, views, depth: Some(depth) }))?;
    }
    std::fs::write(out.join("poses.txt"), poses)?;
    println!("SYNTH keyframes={keyframes} size={size} noise_mm={noise_mm} noise_deg={noise_deg} radius={radius} height={height} grow={grow} stepped={stepped} step_mm={step_mm} surface={surface:?}");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("scan-session-synth: {e}");
        std::process::exit(1);
    }
}
