//! Renders perspective views of a generator map straight from the engine (no app, no bake): a preset with a JSON
//! patch over it (`HdriParams::merge_json`), each view `yaw,pitch,fov,ev,width,height` (yaw an azimuth, 0 north, 90
//! east; pitch up; fov horizontal; ev the view's exposure), 2 x 2 rays a pixel, tonemapped as the preview.
//!
//!   cargo run --release -p makepad-hdri-engine --example candle_look -- "Blue hour" patch.json out/prefix 0,-10,60,2,800,600
use makepad_draw::*;
use makepad_hdri_engine::{dir_from_az_el, export, presets, Env};

fn main() {
    let mut args = std::env::args().skip(1);
    let preset = args.next().unwrap_or_else(|| "Blue hour".to_string());
    let patch = args.next().map(|p| std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))).unwrap_or_else(|| "{}".to_string());
    let prefix = args.next().unwrap_or_else(|| "view".to_string());
    let views: Vec<Vec<f32>> = args.map(|v| v.split(',').map(|x| x.trim().parse::<f32>().expect("a number")).collect()).collect();
    let base = presets::preset(&preset).unwrap_or_else(|| panic!("unknown preset {preset}"));
    let (params, notes) = base.merge_json(&patch).unwrap_or_else(|e| panic!("patch: {e}"));
    for n in notes {
        println!("note: {n}");
    }
    let started = std::time::Instant::now();
    let env = Env::new(&params);
    println!("env built in {:.2} s", started.elapsed().as_secs_f32());
    for (k, v) in views.iter().enumerate() {
        let (yaw, pitch, fov, ev) = (v[0], v[1], v[2], v[3]);
        let (w, h) = (v.get(4).copied().unwrap_or(800.0) as usize, v.get(5).copied().unwrap_or(600.0) as usize);
        let started = std::time::Instant::now();
        let f = dir_from_az_el(yaw, pitch);
        let right = Vec3f::cross(f, vec3f(0.0, 1.0, 0.0)).normalize();
        let up = Vec3f::cross(right, f);
        let half = (0.5 * fov).to_radians().tan();
        let mut rgba = vec![0u8; w * h * 4];
        let threads = std::thread::available_parallelism().map_or(8, |n| n.get());
        let rows: Vec<std::sync::Mutex<Vec<u8>>> = (0..h).map(|_| std::sync::Mutex::new(Vec::new())).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let j = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if j >= h {
                        break;
                    }
                    let mut row = Vec::with_capacity(w * 4);
                    for i in 0..w {
                        let mut sum = Vec3f::default();
                        for (a, b) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                            let x = ((i as f32 + a) / w as f32 * 2.0 - 1.0) * half;
                            let y = (1.0 - (j as f32 + b) / h as f32 * 2.0) * half * h as f32 / w as f32;
                            sum += env.radiance((f + right * x + up * y).normalize());
                        }
                        let c = sum * 0.25;
                        row.extend_from_slice(&export::tonemap_rgba(std::iter::once([c.x, c.y, c.z]), ev));
                    }
                    *rows[j].lock().unwrap() = row;
                });
            }
        });
        for (j, row) in rows.into_iter().enumerate() {
            rgba[j * w * 4..(j + 1) * w * 4].copy_from_slice(&row.into_inner().unwrap());
        }
        let out = format!("{prefix}_{k}.png");
        std::fs::write(&out, Cx::encode_rgba_as_png(w as u32, h as u32, &rgba).expect("a PNG")).expect("written");
        println!("wrote {out} in {:.2} s", started.elapsed().as_secs_f32());
    }
}
