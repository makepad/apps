//! GPU pixel tests: small scenes rendered on the real renderer, checked by
//! what their pixels must show (a colour at a place, one render against
//! another). All cases share one hidden window and GPU context.

use makepad_stage_motion3d::frame_tool::{encode_png, set_batch, Job, Options, Picture};
use std::path::{Path, PathBuf};

mod entry {
    use makepad_stage_motion3d::frame_tool::App;
    use makepad_widgets::*;

    app_main!(App);

    pub fn on_app_thread() {
        app_main();
    }
}

type Check = fn(&[&Picture]) -> Result<String, String>;

struct Test {
    name: &'static str,
    /// (document, time, options tweak)
    renders: Vec<(String, f32, Options)>,
    check: Check,
}

fn main() {
    let start = std::time::Instant::now();
    std::env::set_var("MAKEPAD_HIDE_WINDOWS", "1");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("motion3d_pixels_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the pixel test fixture directory");

    let mut jobs = Vec::new();
    let mut checks = Vec::new();
    for case in tests(&dir) {
        let mut labels = Vec::new();
        for (i, (source, t, opts)) in case.renders.into_iter().enumerate() {
            let label = format!("{}#{i}", case.name);
            labels.push(label.clone());
            jobs.push(Job { label, source, first_line: 1, base: PathBuf::from("."), t, opts });
        }
        checks.push((case.name, labels, case.check));
    }

    set_batch(jobs, move |pictures| {
        let count = checks.len();
        let mut failures = Vec::new();
        for (name, labels, check) in checks {
            let frames: Result<Vec<_>, _> = labels.iter().map(|label| pictures.get(label).ok_or_else(|| format!("missing render {label}"))).collect();
            if let Err(error) = frames.and_then(|frames| check(&frames)) {
                failures.push(format!("{name}: {error}"));
            }
        }
        if !failures.is_empty() {
            return Err(failures.join("\n"));
        }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(format!("pixels: {count} cases in {:.1} s", start.elapsed().as_secs_f64()))
    });
    makepad_widgets::makepad_platform::run_app(entry::on_app_thread);
}

fn lum(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

fn mean_lum(p: &Picture) -> f32 {
    p.rgba.chunks_exact(4).map(|c| lum([c[0], c[1], c[2], c[3]])).sum::<f32>() / (p.width * p.height) as f32
}

fn fraction(p: &Picture, pred: impl Fn([u8; 4]) -> bool) -> f32 {
    p.rgba.chunks_exact(4).filter(|c| pred([c[0], c[1], c[2], c[3]])).count() as f32 / (p.width * p.height) as f32
}

fn near(a: [u8; 4], b: [u8; 3], tol: i32) -> bool {
    (0..3).all(|k| (a[k] as i32 - b[k] as i32).abs() <= tol)
}

fn diff_fraction(a: &Picture, b: &Picture, tol: i32) -> f32 {
    a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)).filter(|(x, y)| (0..3).any(|k| (x[k] as i32 - y[k] as i32).abs() > tol)).count() as f32 / (a.width * a.height) as f32
}

fn small() -> Options {
    Options { size: (256, 144), ..Options::default() }
}

const CAM: &str = "camera: Camera{position: vec3(0, 0, 5) target: vec3(0, 0, 0) fov: 40}";

fn sun_disc_points_move_the_shadow_edge() -> Test {
    const SIZE: (usize, usize) = (512, 288);
    const H: f32 = 10.0;
    const VIEW_HEIGHT: f32 = 0.5;
    Test {
        name: "sun_disc_points_move_the_shadow_edge",
        renders: {
            let doc = format!(r#"use mod.scene.*;
Scene3D{{
    background: #000000 tone_mapping: @none antialias: 1
    camera: OrthographicCamera{{position: vec3({H}, 30, 30) target: vec3({H}, 0, 0) height: {VIEW_HEIGHT} near: 20 far: 70}}
    lights: [AmbientLight{{intensity: 0.05}} DirectionalLight{{position: vec3(-10, 10, 0) target: vec3(0, 0, 0) intensity: 1.5 cast_shadow: true}}]
    children: [
        Mesh{{geometry: BoxGeometry{{width: 3 height: 0.1 depth: 3}} position: vec3({H}, -0.05, 0) material: MeshLambertMaterial{{color: #bcbcbc}}}}
        Mesh{{geometry: BoxGeometry{{width: 0.5 height: {H} depth: 8}} position: vec3(-0.25, {half_h}, 0) material: MeshLambertMaterial{{color: #bcbcbc}}}}
    ]
}}"#, half_h = H * 0.5);
            let mut renders = vec![(doc.clone(), 0.0, Options { size: SIZE, ..Options::default() })];
            for k in 0..8 {
                renders.push((doc.clone(), 0.0, Options { size: SIZE, sun_disc: Some([0.5, k as f32 / 7.0]), ..Options::default() }));
            }
            renders
        },
        check: |pictures| {
            if pictures.len() != 9 || pictures.iter().any(|p| (p.width, p.height) != SIZE) {
                return Err("the disc-edge proof needs nine 512x288 recordings".into());
            }
            // Picture bytes are premultiplied sRGB: DrawToneMap::pixel
            // encodes sRGB even with @none. This floor is opaque. Decode
            // each channel before any row/recording average or ratio.
            let linear: [f32; 256] = std::array::from_fn(|i| {
                let c = i as f32 / 255.0;
                if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            });
            let profiles: Vec<Vec<f32>> = pictures.iter().map(|p| {
                (0..p.width).map(|x| {
                    let rows = p.height / 3..2 * p.height / 3;
                    let count = rows.len();
                    rows.map(|y| {
                        let c = p.px(x, y);
                        0.2126 * linear[c[0] as usize] + 0.7152 * linear[c[1] as usize] + 0.0722 * linear[c[2] as usize]
                    }).sum::<f32>() / count as f32
                }).collect()
            }).collect();
            let pitch = VIEW_HEIGHT / SIZE.1 as f32;
            let centre = makepad_widgets::vec3f(-1.0, 1.0, 0.0).normalize();
            // The wall's top (x=0,y=H) projects onto y=0 at -H*d.x/d.y.
            // Compare shifts from recording 0 so the common receiver bias
            // and the key centre's angular grid snap cancel out.
            let centre_x = -H * centre.x / centre.y;
            let predicted: Vec<f32> = (0..8).map(|k| {
                let d = makepad_render::sun_disc_direction(centre, makepad_render::SUN_COS_RADIUS, [0.5, k as f32 / 7.0]);
                (-H * d.x / d.y - centre_x) / pitch
            }).collect();
            let mut order: Vec<usize> = (0..8).collect();
            order.sort_by(|&a, &b| predicted[a].total_cmp(&predicted[b]));
            let spacings: Vec<f32> = order.windows(2).map(|pair| predicted[pair[1]] - predicted[pair[0]]).collect();
            let min_spacing = spacings.iter().copied().fold(f32::INFINITY, f32::min);
            let mean = |p: &[f32], a: usize, b: usize| p[a..b].iter().sum::<f32>() / (b - a) as f32;
            let crossing = |p: &[f32], level: f32| -> Option<f32> {
                p.windows(2).enumerate().find_map(|(x, pair)| {
                    (pair[0] <= level && pair[1] > level).then(|| x as f32 + (level - pair[0]) / (pair[1] - pair[0]))
                })
            };
            let at = |i: usize, q: f32, umbra: f32, lit: f32| {
                crossing(&profiles[i], umbra + q * (lit - umbra)).ok_or_else(|| {
                    format!("recording {i} has no {:.0}% shadow crossing (linear umbra {umbra:.6}, lit {lit:.6})", q * 100.0)
                })
            };
            let regions = |edge: f32, hard_width: f32| -> Result<(usize, usize), String> {
                let margin = (2.0 * hard_width).max(8.0);
                let left_end = (edge + predicted[order[0]] - margin).floor();
                let right_start = (edge + predicted[order[7]] + margin).ceil();
                if left_end < 1.0 || right_start >= (SIZE.0 - 1) as f32 || min_spacing <= 0.0 {
                    return Err(format!("shifted edges need separated samples and room for endpoint levels: centre edge {edge:.3}px, shifts {predicted:.3?}px, hard 10-90% width {hard_width:.3}px, margin {margin:.3}px"));
                }
                Ok((left_end as usize, right_start as usize))
            };
            // Bootstrap the centre's width from the outer bands, then
            // measure its levels beyond the shifted edges and their margin.
            let outer = SIZE.0 / 8;
            let umbra = mean(&profiles[0], 0, outer);
            let lit = mean(&profiles[0], SIZE.0 - outer, SIZE.0);
            let centre_edge = at(0, 0.5, umbra, lit)?;
            let hard_width = at(0, 0.9, umbra, lit)? - at(0, 0.1, umbra, lit)?;
            let (left_end, right_start) = regions(centre_edge, hard_width)?;
            let umbra = mean(&profiles[0], 0, left_end);
            let lit = mean(&profiles[0], right_start, SIZE.0);
            let contrast = lit - umbra;
            if umbra <= 0.005 || lit >= 0.9 || contrast <= 0.05 {
                return Err(format!("the reference needs a nonblack umbra and an unclipped lit floor with contrast: linear umbra {umbra:.6}, lit {lit:.6}; hard 10-90% width {hard_width:.3}px"));
            }
            let edges: Vec<f32> = (0..9).map(|i| at(i, 0.5, umbra, lit)).collect::<Result<_, _>>()?;
            let hard_width = at(0, 0.9, umbra, lit)? - at(0, 0.1, umbra, lit)?;
            let (left_end, right_start) = regions(edges[0], hard_width)?;
            let average: Vec<f32> = (0..SIZE.0).map(|x| profiles[1..].iter().map(|p| p[x]).sum::<f32>() / 8.0).collect();
            let sample = |x: f32| {
                let i = x.floor() as usize;
                let next = (i + 1).min(SIZE.0 - 1);
                profiles[0][i] + (profiles[0][next] - profiles[0][i]) * (x - i as f32)
            };
            let edge_errors: Vec<f32> = predicted.iter().enumerate().map(|(k, shift)| (edges[k + 1] - edges[0]) - shift).collect();
            // Each recording rasterizes the wall's edge on its own cascade's texel grid, so its
            // edge sits up to half a texel (projected onto the floor) from the true one, either
            // way: no tolerance under that is honest. The kernel spans about three texels
            // between its 10% and 90% points, so half a texel is about a sixth of the hard
            // edge's width. Their mean has no such spread: it is the disc's centre against the
            // centre's own recording, within a pixel.
            let edge_bound = (hard_width / 6.0).max(1.5);
            let mean_error = edge_errors.iter().sum::<f32>() / 8.0;
            let edges_ok = edge_errors.iter().all(|e| e.abs() <= edge_bound) && mean_error.abs() <= 1.0;
            // The cascade kernel is wider than the real sun's disc steps.
            // Compare the whole average with shifted copies of recording
            // 0's blurred edge, preserving that kernel instead of demanding
            // resolved plateaus. A shift to the right samples at x - shift.
            if left_end as f32 - predicted[order[7]] < 0.0 || right_start as f32 - predicted[order[0]] > (SIZE.0 - 1) as f32 {
                return Err(format!("the centre profile cannot cover the whole shifted comparison span {left_end}..={right_start}: shifts {predicted:.3?}px, hard 10-90% width {hard_width:.3}px"));
            }
            let mut worst_profile = 0.0f32;
            for x in left_end..=right_start {
                let expected = predicted.iter().map(|shift| sample(x as f32 - shift)).sum::<f32>() / 8.0;
                worst_profile = worst_profile.max((average[x] - expected).abs() / contrast);
            }
            let width = edges[1..].iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - edges[1..].iter().copied().fold(f32::INFINITY, f32::min);
            let elevation = std::f32::consts::FRAC_PI_4;
            let radius = 0.2666_f32.to_radians();
            let analytic_width = H * (1.0 / (elevation - radius).tan() - 1.0 / (elevation + radius).tan()) / pitch;
            let umbra = mean(&profiles[0], 0, left_end);
            let lit = mean(&profiles[0], right_start, SIZE.0);
            let avg_umbra = mean(&average, 0, left_end);
            let avg_lit = mean(&average, right_start, SIZE.0);
            let umbra_error = (avg_umbra / umbra - 1.0).abs();
            let lit_error = (avg_lit / lit - 1.0).abs();
            let msg = format!("relative edge errors k=0..7 {edge_errors:.3?}px, bound {edge_bound:.3}px, mean {mean_error:.3}px (within tolerance: {edges_ok}); worst profile difference {worst_profile:.4}; width {width:.3}px vs analytic {analytic_width:.3}px; centre hard 10-90% width {hard_width:.3}px; linear lit {avg_lit:.6} vs {lit:.6} ({:.3}% error), umbra {avg_umbra:.6} vs {umbra:.6} ({:.3}% error)", lit_error * 100.0, umbra_error * 100.0);
            if edges_ok && worst_profile <= 0.03 && (width / analytic_width - 1.0).abs() <= 0.1 && lit_error <= 0.005 && umbra_error <= 0.005 {
                Ok(msg)
            } else {
                Err(msg)
            }
        },
    }
}

fn tests(dir: &Path) -> Vec<Test> {
    let one = |src: String| vec![(src, 0.0, small())];
    let mut t = vec![
        Test {
            name: "unlit_box_exact_colour",
            renders: one(format!("use mod.scene.*; Scene3D{{ background: #202020 tone_mapping: @none {CAM} lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{width: 1.5 height: 1.5 depth: 1.5}} material: MeshBasicMaterial{{color: #ff0000}}}}] }}")),
            check: |p| {
                let (c, bg) = (p[0].at(0.5, 0.5), p[0].at(0.05, 0.05));
                if c[0] > 240 && c[1] < 16 && c[2] < 16 && near(bg, [32, 32, 32], 4) {
                    Ok(format!("centre {c:?}, background {bg:?}"))
                } else {
                    Err(format!("centre {c:?} (want ~255,0,0), background {bg:?} (want ~32)"))
                }
            },
        },
        Test {
            name: "directional_light_lights_its_side",
            renders: one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 {CAM} lights: [DirectionalLight{{position: vec3(5, 0, 0) intensity: 3}}] children: [Mesh{{geometry: SphereGeometry{{radius: 1.2}} material: MeshStandardMaterial{{color: #ffffff roughness: 1}}}}] }}")),
            check: |p| {
                let (r, l) = (lum(p[0].at(0.6, 0.5)), lum(p[0].at(0.4, 0.5)));
                if r > l + 60.0 { Ok(format!("lit {r:.0} vs dark {l:.0}")) } else { Err(format!("right {r:.0} should be far above left {l:.0}")) }
            },
        },
        Test {
            name: "soft_shadow_darkens_the_floor",
            renders: {
                let doc = |cast: bool| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 4, 7) target: vec3(0, 0, 0) fov: 40}} lights: [AmbientLight{{intensity: 0.2}} DirectionalLight{{position: vec3(6, 4, 1) intensity: 3 cast_shadow: {cast} shadow_softness: 2}}] children: [Mesh{{geometry: BoxGeometry{{}} position: vec3(0, 0.5, 0)}} Mesh{{geometry: PlaneGeometry{{width: 12 height: 12}} rotation: vec3(-90, 0, 0)}}] }}");
                vec![(doc(true), 0.0, small()), (doc(false), 0.0, small())]
            },
            check: |p| {
                let darker = p[0].rgba.chunks_exact(4).zip(p[1].rgba.chunks_exact(4)).filter(|(a, b)| lum([a[0], a[1], a[2], 0]) + 20.0 < lum([b[0], b[1], b[2], 0])).count() as f32 / (p[0].width * p[0].height) as f32;
                if darker > 0.008 { Ok(format!("{:.1}% of the frame in shadow", darker * 100.0)) } else { Err(format!("only {:.2}% darker with shadows", darker * 100.0)) }
            },
        },
        Test {
            name: "image_based_light_reflects_the_studio",
            renders: one(format!("use mod.scene.*; Scene3D{{ environment: Environment{{preset: @studio background: true blur: 0.3}} {CAM} children: [Mesh{{geometry: SphereGeometry{{radius: 1.3}} material: MeshStandardMaterial{{color: #ffffff metalness: 1 roughness: 0.15}}}}] }}")),
            check: |p| {
                let vals: Vec<f32> = (0..20).map(|k| lum(p[0].at(0.42 + k as f32 * 0.008, 0.5))).collect();
                let (mn, mx) = vals.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
                if mx > 40.0 && mx - mn > 15.0 { Ok(format!("reflection range {mn:.0}..{mx:.0}")) } else { Err(format!("sphere reads {mn:.0}..{mx:.0}: no environment in it")) }
            },
        },
        Test {
            name: "additive_points_glow",
            renders: one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 {CAM} lights: [AmbientLight{{}}] children: [Points{{layout: SphereLayout{{count: 400 radius: 1.4}} size: 0.2 color: #ffaa00}}] }}")),
            check: |p| {
                let lit = fraction(&p[0], |c| lum(c) > 40.0);
                let mx = p[0].rgba.chunks_exact(4).map(|c| lum([c[0], c[1], c[2], 0])).fold(0.0f32, f32::max);
                if lit > 0.02 && mx > 120.0 { Ok(format!("{:.1}% lit, peak {mx:.0}", lit * 100.0)) } else { Err(format!("{:.2}% lit, peak {mx:.0}", lit * 100.0)) }
            },
        },
        Test {
            name: "fat_line_draws",
            renders: one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} lights: [AmbientLight{{}}] children: [Line3D{{points: [vec3(-2, 0, 0), vec3(2, 0, 0)] width: 8 color: #00ff00}}] }}")),
            check: |p| {
                let (c, off) = (p[0].at(0.5, 0.5), p[0].at(0.5, 0.3));
                if c[1] > 200 && c[0] < 30 && lum(off) < 5.0 { Ok(format!("on {c:?} off {off:?}")) } else { Err(format!("on {c:?} off {off:?}")) }
            },
        },
        Test {
            name: "ten_thousand_instances",
            renders: one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 0, 32) fov: 40}} lights: [AmbientLight{{}}] children: [Instances{{geometry: BoxGeometry{{width: 0.2 height: 0.2 depth: 0.2}} layout: GridLayout{{count: vec3(100, 100, 1) spacing: vec3(0.25, 0.25, 1)}} material: MeshBasicMaterial{{color: #ffffff}} color_jitter: 0.5}}] }}")),
            check: |p| {
                let lit = fraction(&p[0], |c| lum(c) > 20.0);
                if lit > 0.2 { Ok(format!("{:.0}% covered", lit * 100.0)) } else { Err(format!("{:.1}% covered", lit * 100.0)) }
            },
        },
        Test {
            name: "extruded_3d_text",
            renders: one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 {CAM} lights: [AmbientLight{{}}] children: [Text3D{{text: \"HI\" size: 1.5 depth: 0.3 material: MeshBasicMaterial{{color: #ffffff}}}}] }}")),
            check: |p| {
                let lit = fraction(&p[0], |c| lum(c) > 128.0);
                let left = fraction(&Picture { width: p[0].width / 2, height: p[0].height, rgba: p[0].rgba.chunks_exact(4 * p[0].width).flat_map(|row| row[..p[0].width / 2 * 4].to_vec()).collect() }, |c| lum(c) > 128.0);
                if lit > 0.03 && left > 0.01 { Ok(format!("{:.1}% ink", lit * 100.0)) } else { Err(format!("{:.2}% ink", lit * 100.0)) }
            },
        },
        Test {
            // Adaptive locked-time motion blur: a whip converges into a
            // continuous streak (more sub-frames), where four sub-frames
            // leave four stepped copies with hard edges between them.
            name: "adaptive_motion_blur_converges",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{width: 0.3 height: 0.6 depth: 0.3}} position: tween(vec3(-6, 0, 0), vec3(6, 0, 0), 0, 0.5) material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                vec![(doc("post.MotionBlur{shutter: 360 samples: @auto tol: 1}"), 0.25, small()), (doc("post.MotionBlur{shutter: 360 samples: 4}"), 0.25, small())]
            },
            check: |p| {
                // Squared horizontal steps across the streak's middle row:
                // a smooth ramp spreads the same rise over many small steps.
                let steps = |q: &Picture| {
                    let y = q.height / 2;
                    (1..q.width).map(|x| (lum(q.px(x, y)) - lum(q.px(x - 1, y))).powi(2)).sum::<f32>()
                };
                let (smooth, stepped) = (steps(&p[0]), steps(&p[1]));
                let msg = format!("step energy {smooth:.0} adaptive vs {stepped:.0} at four sub-frames");
                if smooth < stepped * 0.6 && smooth > 0.0 { Ok(msg) } else { Err(msg) }
            },
        },
        Test {
            // The render graph's Glow kit (Splash passes): a bright fat line
            // gets a halo; lines draw into the HDR frame, so their light
            // reaches the glow.
            name: "graph_glow_blooms_bright_lines",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Line3D{{points: [vec3(-2, 0, 0), vec3(2, 0, 0)] width: 4 color: #ff5a1f intensity: 4}}] }}");
                vec![(doc("post.Glow{strength: 1.2 threshold: 1}"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let (a, b) = (lum(p[0].at(0.5, 0.44)), lum(p[1].at(0.5, 0.44)));
                let msg = format!("halo {a:.0} vs {b:.0} without");
                if a > b + 6.0 { Ok(msg) } else { Err(msg) }
            },
        },
        Test {
            name: "graph_pass_px_scale_is_supersampling",
            renders: {
                // A pass writing its width in output pixels (its own width
                // over px_scale()) / 1024 in red, and its own width / 1024
                // in green: the output width is the same at antialias 1 and
                // 2 on any lane, whatever size the pass runs at.
                let doc = |aa: u32| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none antialias: {aa} {CAM} post: [post.Pass{{at: @final pixel: fn() -> vec4 {{ return vec4(self.size().x / self.px_scale() / 1024.0, self.size().x / 1024.0, 0.0, 1.0) }}}}] lights: [AmbientLight{{}}] children: [] }}");
                vec![(doc(2), 0.0, small()), (doc(1), 0.0, small())]
            },
            check: |p| {
                let (a, b) = (p[0].at(0.5, 0.5), p[1].at(0.5, 0.5));
                let expect = (p[1].width as f32 / 1024.0 * 255.0).round() as i32;
                let msg = format!("antialias 2 {a:?}, 1 {b:?}, output width {} -> {expect}", p[1].width);
                if (a[0] as i32 - expect).abs() <= 1 && (b[0] as i32 - expect).abs() <= 1 { Ok(msg) } else { Err(msg) }
            },
        },
        Test {
            // A document's own Pass at @final and FramePost's flash.
            name: "graph_pass_and_frame_post_run",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{}} material: MeshBasicMaterial{{color: #ff0000}}}}] }}");
                vec![
                    (doc("post.Pass{at: @final uniforms: {tint: #00ff00} pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) return vec4(c.y, c.x, c.z, c.w) }}"), 0.0, small()),
                    (doc("post.FramePost{flash: 1 flash_color: #0000ff}"), 0.0, small()),
                ]
            },
            check: |p| {
                let (swap, flash) = (p[0].at(0.5, 0.5), p[1].at(0.2, 0.2));
                let msg = format!("red-green swapped {swap:?}, flash {flash:?}");
                if swap[1] > 200 && swap[0] < 30 && flash[2] > 200 && flash[0] < 30 { Ok(msg) } else { Err(msg) }
            },
        },
        Test {
            // A raymarch pass at half resolution with a depth output (MRT),
            // brought up by the depth-aware Upsample over the scene: the
            // marched sphere shows, and a box in front of it stays in front.
            name: "raymarch_pass_upsamples_depth_tested",
            renders: {
                let pass = "post.Pass{name: \"march\" scale: 0.5 outputs: [{name: \"march_depth\" format: @r32f}] pixel: fn() -> vec4 {
                    let ro = self.eye()
                    let rd = self.ray_dir(self.uv())
                    var t = 0.0
                    var hit = 0.0
                    for i in 0..96 {
                        if hit < 0.5 && t < 50.0 {
                            let d = length(ro + rd * t) - 0.8
                            if d < 0.001 {
                                hit = 1.0
                            } else {
                                t = t + d
                            }
                        }
                    }
                    if hit < 0.5 {
                        self.march_depth = vec4(0.0, 0.0, 0.0, 0.0)
                        return vec4(0.0, 0.0, 0.0, 0.0)
                    }
                    let p = ro + rd * t
                    self.march_depth = vec4(self.view_distance(p), 0.0, 0.0, 0.0)
                    let l = max(dot(normalize(p), normalize(vec3(0.4, 0.8, 0.5))), 0.0)
                    return vec4(vec3(0.1, 0.6, 1.0) * (0.3 + l), 1.0)
                }} post.Upsample{src: \"march\" depth: \"march_depth\"}";
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{width: 0.5 height: 0.5 depth: 0.5}} position: vec3(0.45, 0, 1.5) material: MeshBasicMaterial{{color: #ff0000}}}}] }}");
                vec![(doc(pass), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let (sphere, box_) = (p[0].at(0.38, 0.5), p[0].at(0.6, 0.5));
                let (plain_box, bg) = (p[1].at(0.6, 0.5), p[1].at(0.38, 0.5));
                let msg = format!("sphere {sphere:?} (without {bg:?}), box {box_:?} (without {plain_box:?})");
                if sphere[2] > 120 && sphere[0] < 80 && box_[0] > 200 && box_[2] < 60 { Ok(msg) } else { Err(msg) }
            },
        },
        Test {
            // Analytic-derivative sampling in a pass (`sample_grad`): the
            // frame tiled 2x2 with the derivatives of the unwarped uv.
            name: "sample_grad_pass_runs",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{}} material: MeshBasicMaterial{{color: #ff0000}}}}] }}");
                vec![(doc("post.Pass{pixel: fn() -> vec4 { let q = self.uv() * 2.0 return self.color.sample_grad(fract(q), dFdx(q), dFdy(q)) }}"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let (tiled, plain) = (p[0].at(0.25, 0.25), p[1].at(0.25, 0.25));
                let msg = format!("tiled {tiled:?} vs plain {plain:?}");
                if tiled[0] > 200 && plain[0] < 30 { Ok(msg) } else { Err(msg) }
            },
        },
        Test {
            name: "ssao_darkens_contacts",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 3, 6) target: vec3(0, 0.3, 0) fov: 40}} post: [{post}] lights: [AmbientLight{{intensity: 1}}] children: [Mesh{{geometry: BoxGeometry{{}} position: vec3(0, 0.5, 0)}} Mesh{{geometry: PlaneGeometry{{width: 10 height: 10}} rotation: vec3(-90, 0, 0)}}] }}");
                vec![(doc("SSAO{radius: 0.6 intensity: 1.5}"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let (a, b) = (mean_lum(&p[0]), mean_lum(&p[1]));
                let darker = fraction(&Picture { width: p[0].width, height: p[0].height, rgba: p[0].rgba.iter().zip(p[1].rgba.iter()).map(|(x, y)| if *y > x.saturating_add(8) { 255 } else { 0 }).collect() }, |c| c[0] > 0);
                if a < b && darker > 0.005 { Ok(format!("mean {b:.1} -> {a:.1}, {:.1}% darkened", darker * 100.0)) } else { Err(format!("mean {b:.1} -> {a:.1}, {:.2}% darkened", darker * 100.0)) }
            },
        },
        Test {
            name: "ssao_leaves_direct_light",
            renders: {
                // Occlusion is ambient light failing to arrive: a scene lit
                // by a directional light alone looks the same with SSAO.
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 3, 6) target: vec3(0, 0.3, 0) fov: 40}} post: [{post}] lights: [DirectionalLight{{position: vec3(2, 4, 3) intensity: 2}}] children: [Mesh{{geometry: BoxGeometry{{}} position: vec3(0, 0.5, 0)}} Mesh{{geometry: PlaneGeometry{{width: 10 height: 10}} rotation: vec3(-90, 0, 0)}}] }}");
                vec![(doc("SSAO{radius: 0.6 intensity: 1.5}"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let changed = fraction(&Picture { width: p[0].width, height: p[0].height, rgba: p[0].rgba.iter().zip(p[1].rgba.iter()).map(|(x, y)| if x.abs_diff(*y) > 3 { 255 } else { 0 }).collect() }, |c| c[0] > 0);
                let (a, b) = (mean_lum(&p[0]), mean_lum(&p[1]));
                if changed < 0.001 && b > 10.0 { Ok(format!("mean {b:.1} vs {a:.1}, {:.2}% changed", changed * 100.0)) } else { Err(format!("mean {b:.1} vs {a:.1}, {:.2}% changed", changed * 100.0)) }
            },
        },
        Test {
            // SSAO beside a pass that samples the AO before the AO's passes paint (the outline's
            // id map shares the scene's draws): the frame settles. Recorded again every frame
            // while the AO warmed up, the id map was owed a paint after each recording and the
            // frame never painted. The scene is over CORTAO's half-size threshold (2592 x 1600
            // at antialias 2), whose upsample pass is made after the id map's and paints after it.
            name: "ssao_with_an_outline_settles",
            renders: vec![(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none antialias: 2 camera: Camera{{position: vec3(0, 3, 6) target: vec3(0, 0.3, 0) fov: 40}} post: [SSAO{{radius: 0.6 intensity: 1.5}} post.Outline{{color: #ffff00 thickness: 3}}] lights: [AmbientLight{{}}] children: [Mesh{{outline: true geometry: BoxGeometry{{}} position: vec3(0, 0.5, 0) material: MeshBasicMaterial{{color: #2020ff}}}} Mesh{{geometry: PlaneGeometry{{width: 10 height: 10}} rotation: vec3(-90, 0, 0)}}] }}"), 0.0, Options { size: (1296, 800), ..Options::default() })],
            check: |p| {
                let y = fraction(&p[0], |c| c[0] > 180 && c[1] > 180 && c[2] < 90);
                if y > 0.0002 { Ok(format!("{:.3}% outline", y * 100.0)) } else { Err(format!("{:.4}% outline", y * 100.0)) }
            },
        },
        Test {
            name: "planar_reflection_mirrors_the_scene",
            renders: {
                let doc = |r: f32| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none camera: Camera{{position: vec3(0, 1, 5) target: vec3(0, 0.3, 0) fov: 40}} lights: [AmbientLight{{}}] children: [Mesh{{geometry: SphereGeometry{{radius: 0.5}} position: vec3(0, 0.8, 0) material: MeshBasicMaterial{{color: #ff2000}}}} Ground{{color: #000000 reflectivity: {r} reflection_blur: 0}}] }}");
                vec![(doc(1.0), 0.0, small()), (doc(0.0), 0.0, small())]
            },
            check: |p| {
                // Below the sphere on the floor: red only with the reflection.
                let (a, b) = (p[0].at(0.5, 0.75), p[1].at(0.5, 0.75));
                if a[0] as i32 > b[0] as i32 + 40 { Ok(format!("floor {a:?} vs {b:?}")) } else { Err(format!("floor {a:?} vs {b:?}")) }
            },
        },
        Test {
            name: "contact_shadows_ground_objects",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 3, 5) target: vec3(0, 0.3, 0) fov: 40}} post: [{post}] lights: [AmbientLight{{intensity: 1}}] children: [Mesh{{geometry: SphereGeometry{{radius: 0.5}} position: vec3(0, 0.55, 0)}} Ground{{color: #ffffff roughness: 1}}] }}");
                vec![(doc("ContactShadows{opacity: 1 blur: 0.3 far: 1.0}"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let d = diff_fraction(&p[0], &p[1], 20);
                if d > 0.004 { Ok(format!("{:.1}% darkened", d * 100.0)) } else { Err(format!("{:.2}% darkened", d * 100.0)) }
            },
        },
        Test {
            name: "colour_grade_changes_the_image",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; Scene3D{{ background: #808080 {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [] }}");
                vec![(doc("ColorGrade{saturation: 1.2 temperature: 1 contrast: 1.2}"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let (a, b) = (p[0].at(0.5, 0.5), p[1].at(0.5, 0.5));
                if a[0] > a[2] + 10 && (b[0] as i32 - b[2] as i32).abs() < 4 { Ok(format!("{a:?} vs {b:?}")) } else { Err(format!("{a:?} vs {b:?}")) }
            },
        },

        Test {
            name: "transparent_surface_blends",
            renders: one(format!("use mod.scene.*; Scene3D{{ background: #0000ff tone_mapping: @none {CAM} lights: [AmbientLight{{}}] children: [Mesh{{geometry: PlaneGeometry{{width: 3 height: 3}} material: MeshBasicMaterial{{color: #ff0000 opacity: 0.5}}}}] }}")),
            check: |p| {
                let c = p[0].at(0.5, 0.5);
                if c[0] > 140 && c[2] > 140 && c[1] < 30 { Ok(format!("{c:?}")) } else { Err(format!("{c:?} is not a red-over-blue mix")) }
            },
        },


        Test {
            name: "glass_refracts_what_is_behind",
            renders: {
                let doc = |glass: bool| {
                    let s = if glass { "Mesh{geometry: SphereGeometry{radius: 1.2} material: MeshPhysicalMaterial{color: #ffffff transmission: 1 roughness: 0.05 ior: 1.5 thickness: 1}}" } else { "" };
                    format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 {CAM} lights: [AmbientLight{{intensity: 1}}] children: [Instances{{geometry: BoxGeometry{{width: 0.3 height: 3 depth: 0.1}} layout: LineLayout{{count: 9 start: vec3(-2, 0, -3) end: vec3(2, 0, -3)}} material: MeshBasicMaterial{{color: #ff8800}}}} {s}] }}")
                };
                vec![(doc(true), 0.0, small()), (doc(false), 0.0, small())]
            },
            check: |p| {
                let d = diff_fraction(&p[0], &p[1], 30);
                let inside = lum(p[0].at(0.5, 0.5)) + lum(p[0].at(0.45, 0.5)) + lum(p[0].at(0.55, 0.5));
                // Through the sphere the stripes show (orange and dark
                // across its middle row), not a plain surface.
                let row: Vec<[u8; 4]> = (0..21).map(|k| p[0].at(0.42 + k as f32 * 0.008, 0.5)).collect();
                let orange = row.iter().filter(|c| c[0] > 150 && c[1] > 60 && c[1] < 180 && c[2] < 90).count();
                let dark = row.iter().filter(|c| lum(**c) < 90.0).count();
                if d > 0.02 && inside > 30.0 && orange >= 3 && dark >= 3 { Ok(format!("{:.1}% of pixels bent, through: {orange} orange {dark} dark of 21", d * 100.0)) } else { Err(format!("{:.2}% changed, inside {inside:.0}, through: {orange} orange {dark} dark of 21", d * 100.0)) }
            },
        },


        Test {
            name: "fog_fades_distance",
            renders: one("use mod.scene.*; Scene3D{ background: #000000 tone_mapping: @none fog: Fog{color: #000000 near: 4 far: 30} camera: Camera{position: vec3(0, 0, 5) target: vec3(0, 0, -10) fov: 40} lights: [AmbientLight{}] children: [Mesh{geometry: BoxGeometry{} position: vec3(-1.2, 0, 0) material: MeshBasicMaterial{color: #ffffff}} Mesh{geometry: BoxGeometry{width: 6 height: 6 depth: 1} position: vec3(8, 0, -20) material: MeshBasicMaterial{color: #ffffff}}] }".into()),
            check: |p| {
                let near_l = lum(p[0].at(0.3, 0.5));
                let far_l = p[0].rgba.chunks_exact(4).enumerate().filter(|(i, _)| (i % p[0].width) as f32 > p[0].width as f32 * 0.6).map(|(_, c)| lum([c[0], c[1], c[2], 0])).fold(0.0f32, f32::max);
                if near_l > 200.0 && far_l > 5.0 && far_l < near_l * 0.7 { Ok(format!("near {near_l:.0} far {far_l:.0}")) } else { Err(format!("near {near_l:.0} far {far_l:.0}")) }
            },
        },


        Test {
            name: "shader_material_hooks_compile",
            renders: one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 {CAM} lights: [AmbientLight{{intensity: 2}}] children: [Mesh{{geometry: SphereGeometry{{radius: 1.2}} material: ShaderMaterial{{uniforms: {{amount: 1}} surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {{ return vec4(0.0, self.u_custom.x, 0.0, 1.0) }} vertex_displace: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec3 {{ return p * 1.1 }}}}}}] }}")),
            check: |p| {
                let c = p[0].at(0.5, 0.5);
                if c[1] > 60 && (c[1] as i32) > 2 * c[0] as i32 && (c[1] as i32) > 2 * c[2] as i32 { Ok(format!("{c:?}")) } else { Err(format!("{c:?} is not the hook's green")) }
            },
        },


        Test {
            name: "depth_of_field_blurs_out_of_focus",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 0, 4) target: vec3(0, 0, 0) fov: 50}} post: [{post}] lights: [AmbientLight{{}}] children: [\
                    Instances{{geometry: BoxGeometry{{width: 0.08 height: 1 depth: 0.08}} layout: LineLayout{{count: 24 start: vec3(-2, 0.9, 0) end: vec3(2, 0.9, 0)}} material: MeshBasicMaterial{{color: #ffffff}}}}\
                    Instances{{geometry: BoxGeometry{{width: 0.4 height: 5 depth: 0.4}} layout: LineLayout{{count: 24 start: vec3(-12, -2, -18) end: vec3(12, -2, -18)}} material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                let big = Options { size: (512, 288), ..Options::default() };
                vec![(doc("post.DepthOfField{focus: 4.1 aperture: 24 max_blur: 32}"), 0.0, big.clone()), (doc(""), 0.0, big)]
            },
            check: |p| {
                let (far_b, far_s) = (sharpness(&p[0], 0.6, 0.95), sharpness(&p[1], 0.6, 0.95));
                let (near_b, near_s) = (sharpness(&p[0], 0.15, 0.35), sharpness(&p[1], 0.15, 0.35));
                if far_b < far_s * 0.75 && near_b > near_s * 0.9 { Ok(format!("far {far_s:.1}->{far_b:.1}, near {near_s:.1}->{near_b:.1}")) } else { Err(format!("far {far_s:.1}->{far_b:.1}, near {near_s:.1}->{near_b:.1}")) }
            },
        },


        Test {
            name: "motion_blur_smears_fast_motion",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{width: 0.6 height: 0.6 depth: 0.6}} position: tween(vec3(-3, 0, 0), vec3(3, 0, 0), 0, 0.5) material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                vec![(doc("post.MotionBlur{shutter: 360 samples: 8}"), 0.25, small()), (doc(""), 0.25, small())]
            },
            check: |p| {
                let partial = |q: &Picture| fraction(q, |c| lum(c) > 20.0 && lum(c) < 220.0);
                let (a, b) = (partial(&p[0]), partial(&p[1]));
                if a > b + 0.01 { Ok(format!("partial coverage {:.1}% vs {:.1}%", a * 100.0, b * 100.0)) } else { Err(format!("partial coverage {:.2}% vs {:.2}%", a * 100.0, b * 100.0)) }
            },
        },


        Test {
            // A preview's motion blur: the camera's screen motion between two
            // frames (the graph's VelocityBlur), one frame per paint. A box
            // under a panning camera smears; locked time would sum
            // sub-frames instead.
            name: "realtime_velocity_blur_smears_a_pan",
            renders: {
                let doc = |blur: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none camera: Camera{{position: tween(vec3(-2, 0, 5), vec3(2, 0, 5), 0, 1) target: tween(vec3(-2, 0, 0), vec3(2, 0, 0), 0, 1) fov: 40}} post: [{blur}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{width: 0.4 height: 1 depth: 0.4}} material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                let rt = || Options { realtime: true, ..small() };
                vec![(doc("post.MotionBlur{shutter: 360}"), 0.45, rt()), (doc("post.MotionBlur{shutter: 360}"), 0.5, rt()), (doc(""), 0.5, small())]
            },
            check: |p| {
                let partial = |q: &Picture| fraction(q, |c| lum(c) > 20.0 && lum(c) < 220.0);
                let (a, b) = (partial(&p[1]), partial(&p[2]));
                if a > b + 0.01 { Ok(format!("partial coverage {:.1}% vs {:.1}%", a * 100.0, b * 100.0)) } else { Err(format!("partial coverage {:.2}% vs {:.2}%", a * 100.0, b * 100.0)) }
            },
        },


        Test {
            name: "outline_separates_two_outlined_surfaces",
            renders: {
                // Two outlined spheres, one partly in front of the other:
                // the outline follows the id change between them too, so
                // the middle row crosses three outlines (left, seam, right),
                // not only the two of their union's silhouette.
                one(format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [post.Outline{{color: #ffff00 thickness: 3}}] lights: [AmbientLight{{}}] children: [Mesh{{outline: true geometry: SphereGeometry{{radius: 1}} position: vec3(-0.6, 0, 0) material: MeshBasicMaterial{{color: #2020ff}}}} Mesh{{outline: true geometry: SphereGeometry{{radius: 1}} position: vec3(0.6, 0, 0.5) material: MeshBasicMaterial{{color: #2020ff}}}}] }}"))
            },
            check: |p| {
                let q = &p[0];
                let y = q.height / 2;
                let yellow = |x: usize| { let c = q.px(x, y); c[0] > 180 && c[1] > 180 && c[2] < 90 };
                let runs = (0..q.width).filter(|&x| yellow(x) && (x == 0 || !yellow(x - 1))).count();
                if runs >= 3 { Ok(format!("{runs} outlines across the middle")) } else { Err(format!("{runs} outlines across the middle")) }
            },
        },


        Test {
            name: "orthographic_camera_keeps_the_near_half",
            renders: one("use mod.scene.*; Scene3D{ background: #000000 tone_mapping: @none camera: OrthographicCamera{position: vec3(0, 0, 50) target: vec3(0, 0, 0) height: 4 near: 0.1 far: 100} lights: [AmbientLight{}] children: [Mesh{geometry: BoxGeometry{} position: vec3(-1, 0, 40) material: MeshBasicMaterial{color: #ff0000}} Mesh{geometry: BoxGeometry{} position: vec3(1, 0, -40) material: MeshBasicMaterial{color: #00ff00}}] }".into()),
            check: |p| {
                let (near_box, far_box) = (p[0].at(0.5 - 1.0 / 7.1, 0.5), p[0].at(0.5 + 1.0 / 7.1, 0.5));
                if near_box[0] > 200 && far_box[1] > 200 { Ok(format!("near {near_box:?} far {far_box:?}")) } else { Err(format!("near {near_box:?} far {far_box:?}")) }
            },
        },


        Test {
            name: "supersampling_smooths_edges",
            renders: {
                let doc = |aa: u32| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none antialias: {aa} {CAM} lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{}} rotation: vec3(0, 0, 17) material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                vec![(doc(3), 0.0, small()), (doc(1), 0.0, small())]
            },
            check: |p| {
                let partial = |q: &Picture| fraction(q, |c| lum(c) > 20.0 && lum(c) < 235.0);
                let (a, b) = (partial(&p[0]), partial(&p[1]));
                if a > b * 2.0 && a > 0.003 { Ok(format!("edge pixels {:.2}% vs {:.2}%", a * 100.0, b * 100.0)) } else { Err(format!("edge pixels {:.2}% vs {:.2}%", a * 100.0, b * 100.0)) }
            },
        },


        Test {
            // The deformers run in the vertex shader: a half page curl clears
            // the right of a sheet, explode opens gaps in a sphere, and a
            // ripple changes a lit plane's shading.
            name: "vertex_deformers_move_the_surface",
            renders: {
                let sheet = |d: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} lights: [AmbientLight{{}}] children: [Mesh{{geometry: PlaneGeometry{{width: 3 height: 1.7 width_segments: 96}} {d} material: MeshBasicMaterial{{color: #ffffff side: @double}}}}] }}");
                let ball = |d: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} lights: [AmbientLight{{}}] children: [Mesh{{geometry: IcosahedronGeometry{{radius: 1 detail: 2}} {d} material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                let pool = |d: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} lights: [DirectionalLight{{position: vec3(2, 1, 3) intensity: 2}}] children: [Mesh{{geometry: PlaneGeometry{{width: 4 height: 3 width_segments: 128 height_segments: 128}} {d} material: MeshStandardMaterial{{color: #ffffff roughness: 0.6}}}}] }}");
                vec![
                    (sheet("curl: 0.5 curl_radius: 0.3"), 0.0, small()),
                    (sheet(""), 0.0, small()),
                    (ball("explode: 1.2"), 0.0, small()),
                    (ball(""), 0.0, small()),
                    (pool("ripple: 0.08 ripple_frequency: 1.5"), 0.0, small()),
                    (pool(""), 0.0, small()),
                ]
            },
            check: |p| {
                let (curled, flat) = (lum(p[0].at(0.68, 0.5)), lum(p[1].at(0.68, 0.5)));
                let ball_diff = diff_fraction(&p[2], &p[3], 40);
                let pool_diff = diff_fraction(&p[4], &p[5], 12);
                let msg = format!("curl {flat:.0}->{curled:.0}, explode {:.1}% changed, ripple {:.1}% changed", ball_diff * 100.0, pool_diff * 100.0);
                if flat > 200.0 && curled < 60.0 && ball_diff > 0.05 && pool_diff > 0.05 { Ok(msg) } else { Err(msg) }
            },
        },


        Test {
            // Ink: a dark rim around the silhouette (the inverted hull);
            // without it, none.
            name: "ink_hull_outlines_the_silhouette",
            renders: {
                let doc = |look: &str| format!("use mod.scene.*; Scene3D{{ background: #ffffff tone_mapping: @none {CAM} lights: [AmbientLight{{intensity: 0.6}} DirectionalLight{{position: vec3(2, 3, 4) intensity: 1.5}}] children: [Mesh{{geometry: SphereGeometry{{radius: 1 width_segments: 48 height_segments: 32}} material: MeshStandardMaterial{{color: #ff8040 {look}}}}}] }}");
                // Ink widths are 1080p pixels: 16 is about 2 px at this size.
                vec![(doc("ink: 16"), 0.0, small()), (doc(""), 0.0, small())]
            },
            check: |p| {
                let dark = |q: &Picture| fraction(q, |c| lum(c) < 40.0);
                let (toon, plain) = (dark(&p[0]), dark(&p[1]));
                let msg = format!("dark rim {:.2}% vs {:.2}%", toon * 100.0, plain * 100.0);
                if toon > 0.004 && toon > plain * 3.0 + 0.002 { Ok(msg) } else { Err(msg) }
            },
        },


        Test {
            // Contour isolines of a heightfield draw ink lines on paper, and
            // light shafts spill light from a bright point into dark space.
            name: "contour_lines_and_light_shafts",
            renders: {
                let terrain = "use mod.scene.*; Scene3D{ background: #f0ebe0 tone_mapping: @none camera: Camera{position: vec3(0, 6, 6) target: vec3(0, 0, 0) fov: 40} lights: [AmbientLight{}] children: [Fx{preset: @contour_terrain radius: 5 amount: 0.9 spacing: 0.12 color: #f0ebe0 color2: #000000}] }".to_string();
                let sun = |shafts: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{shafts}] lights: [AmbientLight{{}}] children: [Mesh{{name: \"sun\" geometry: SphereGeometry{{radius: 0.3}} material: MeshBasicMaterial{{color: #ffffff}}}}] }}");
                vec![(terrain, 0.0, small()), (sun("LightShafts{node: \"sun\" strength: 2 decay: 0.97 threshold: 0.3}"), 0.0, small()), (sun(""), 0.0, small())]
            },
            check: |p| {
                let ink = fraction(&p[0], |c| lum(c) < 80.0);
                let halo = |q: &Picture| lum(q.at(0.62, 0.5));
                let (with, without) = (halo(&p[1]), halo(&p[2]));
                let msg = format!("ink {:.1}%, spill {with:.0} vs {without:.0}", ink * 100.0);
                if ink > 0.015 && ink < 0.6 && with > without + 8.0 { Ok(msg) } else { Err(msg) }
            },
        },


        Test {
            // Shader code the author writes: a `shade_fn` hook recolours a
            // surface after lighting, a `PostPass` inverts the frame.
            name: "shade_fn_and_post_pass_run",
            renders: {
                let doc = |post: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none {CAM} post: [{post}] lights: [AmbientLight{{}}] children: [Mesh{{geometry: BoxGeometry{{width: 1.5 height: 1.5 depth: 1.5}} material: MeshBasicMaterial{{color: #ff0000 hooks: {{\n shade_fn: fn(c: vec4, p: vec3, n: vec3, uv: vec2, t: float) -> vec4 {{ return vec4(0.0, c.x, 0.0, 1.0) }}\n}}}}}}] }}");
                vec![(doc(""), 0.0, small()), (doc("PostPass{code: fn(uv: vec2, c: vec3) -> vec3 { return vec3(1.0, 1.0, 1.0) - c }}"), 0.0, small())]
            },
            check: |p| {
                let (c, bg) = (p[0].at(0.5, 0.5), p[1].at(0.05, 0.05));
                let msg = format!("hooked {c:?}, inverted background {bg:?}");
                if c[1] > 200 && c[0] < 30 && bg[0] > 200 && bg[1] > 200 { Ok(msg) } else { Err(msg) }
            },
        },


        Test {
            // Seeks (KERNELS.md P6.2 item 4): locked-time frames in random
            // order equal the same frames in linear order, with shadows,
            // SSAO, particles and a moving mesh (nothing a frame shows may
            // depend on the frames drawn before it).
            name: "seeks_equal_linear_order",
            renders: {
                let doc = format!("use mod.scene.*; use mod.post; Scene3D{{ background: #101010 {CAM} environment: Environment{{preset: @sunset}} post: [post.Bloom{{}} SSAO{{}}] lights: [DirectionalLight{{position: vec3(2, 4, 3) cast_shadow: true}}] children: [Mesh{{geometry: TorusKnotGeometry{{radius: 0.8 tube: 0.25}} rotation: spin(vec3(20, 40, 0)) displace: 0.1 material: MeshPhysicalMaterial{{color: #ff5a36 clearcoat: 1}}}} Particles{{count: 200 rate: 100 speed: 2 turbulence: 1}}] }}");
                // linear 0.4, 1.1, 1.9; then seeks 1.9, 0.4, 1.1
                [0.4, 1.1, 1.9, 1.9, 0.4, 1.1].iter().map(|t| (doc.clone(), *t, small())).collect()
            },
            check: |p| {
                let same = (0..3).all(|k| p[k].rgba == p[[4, 5, 3][k]].rgba);
                let moved = p[0].rgba != p[1].rgba;
                let msg = format!("{:016x} {:016x} {:016x} / {:016x} {:016x} {:016x}", p[0].hash(), p[1].hash(), p[2].hash(), p[4].hash(), p[5].hash(), p[3].hash());
                if same && moved { Ok(msg) } else { Err(msg) }
            },
        },


        Test {
            name: "material_zoo_all_shade",
            renders: one(format!("use mod.scene.*; Scene3D{{ background: #000000 camera: Camera{{position: vec3(0, 0, 12) fov: 40}} environment: Environment{{preset: @studio}} lights: [DirectionalLight{{position: vec3(2, 3, 4) intensity: 2}}] children: [\
                Mesh{{position: vec3(-5, 0, 0) geometry: SphereGeometry{{}} material: MeshToonMaterial{{color: #3388ff steps: 3}}}}\
                Mesh{{position: vec3(-3, 0, 0) geometry: SphereGeometry{{}} material: MeshMatcapMaterial{{matcap: @gold}}}}\
                Mesh{{position: vec3(-1, 0, 0) geometry: SphereGeometry{{}} material: MeshNormalMaterial{{}}}}\
                Mesh{{position: vec3(1, 0, 0) geometry: SphereGeometry{{}} material: MeshLambertMaterial{{color: #ff4444}}}}\
                Mesh{{position: vec3(3, 0, 0) geometry: SphereGeometry{{}} material: MeshStandardMaterial{{color: #44ff44 wireframe: true}}}}\
                Mesh{{position: vec3(5, 0, 0) geometry: SphereGeometry{{}} material: MeshPhysicalMaterial{{color: #ffffff clearcoat: 1 iridescence: 1 sheen: 1}}}}\
            ] }}")),
            check: |p| {
                let cols = [0.5 - 5.0 / 17.6, 0.5 - 3.0 / 17.6, 0.5 - 1.0 / 17.6, 0.5 + 1.0 / 17.6, 0.5 + 5.0 / 17.6];
                let px: Vec<[u8; 4]> = cols.iter().map(|x| p[0].at(*x, 0.5)).collect();
                let dark: Vec<_> = px.iter().filter(|c| lum(**c) < 10.0).collect();
                let distinct = (0..px.len()).all(|i| (i + 1..px.len()).all(|j| (0..3).any(|k| (px[i][k] as i32 - px[j][k] as i32).abs() > 12)));
                let wire = fraction(&p[0], |c| c[1] > 60 && c[0] < c[1] / 2);
                if dark.is_empty() && distinct && wire > 0.0005 { Ok(format!("{px:?}")) } else { Err(format!("{px:?} (dark {}, distinct {distinct}, wire {wire})", dark.len())) }
            },
        },
    ];

    // Screen pictures never stretch: a 400×200 picture of four columns
    // (red, green, blue, white; top half bright, bottom half dim) on a
    // square screen, filled, covered, contained and cropped.
    {
        let pic = fit_picture(dir);
        let mut opts = small();
        opts.media.insert("fitpic".into(), pic.clone());
        let doc = |mat: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none camera: Camera{{position: vec3(0, 0, 5) target: vec3(0, 0, 0) fov: 30}} lights: [AmbientLight{{}}] children: [Mesh{{geometry: PlaneGeometry{{width: 2 height: 2}} material: MeshBasicMaterial{{map: asset(\"fitpic\", \"1\") {mat}}}}}] }}");
        t.push(Test {
            name: "screen_fit_cover_contain_crop",
            renders: vec![
                (doc("fit: @fill"), 0.0, opts.clone()),
                (doc("fit: @cover"), 0.0, opts.clone()),
                (doc("fit: @contain"), 0.0, opts.clone()),
                (doc("fit: @cover crop: [300, 0, 100, 100]"), 0.0, opts.clone()),
                (doc("fit: @cover crop: [0, 100, 100, 100]"), 0.0, opts.clone()),
                (doc("fit: @cover crop: keys([0, [0, 0, 100, 200]], [1, [300, 0, 100, 200]])"), 1.0, opts.clone()),
            ],
            check: |p| {
                // The plane spans the middle of the frame: x 0.5 ± 0.19 (16:9), y 0.5 ± 0.34.
                let at = |q: &Picture, u: f32, v: f32| q.at(0.5 + (u - 0.5) * 0.37, 0.5 + (v - 0.5) * 0.66);
                let dom = |c: [u8; 4]| if c[0] > 150 && c[1] > 150 && c[2] > 150 { 'W' } else if c[0] > c[1] && c[0] > c[2] { 'R' } else if c[1] > c[2] { 'G' } else { 'B' };
                let fill = dom(at(&p[0], 0.1, 0.3));
                // Upright: the picture's bright top half is at the top.
                let upright = lum(at(&p[0], 0.1, 0.2)) > lum(at(&p[0], 0.1, 0.8)) + 20.0;
                let cover = dom(at(&p[1], 0.1, 0.3));
                let bar = lum(at(&p[2], 0.5, 0.1));
                let inside = at(&p[2], 0.1, 0.4)[0] as f32;
                let white = dom(at(&p[3], 0.5, 0.5));
                let (dim, bright) = (at(&p[4], 0.5, 0.5), at(&p[3], 0.5, 0.5));
                let followed = dom(at(&p[5], 0.75, 0.3));
                let msg = format!("upright {upright} fill {fill} cover {cover} contain bar {bar:.0} inside {inside:.0} crop top-right {white} bottom-left {dim:?} keys→{followed}");
                if upright && fill == 'R' && cover == 'G' && bar < 10.0 && inside > 200.0 && white == 'W' && dim[0] > 60 && dim[0] < 200 && dim[1] < 40 && bright[0] > 200 && followed == 'W' { Ok(msg) } else { Err(msg) }
            },
        });


        // The same picture as a screen: the host's texture by name.
        let check = t.last().unwrap().check;
        let mut opts = small();
        opts.textures.insert("fitpic".into(), pic);
        let doc = |mat: &str| format!("use mod.scene.*; use mod.post; Scene3D{{ background: #000000 tone_mapping: @none camera: Camera{{position: vec3(0, 0, 5) target: vec3(0, 0, 0) fov: 30}} lights: [AmbientLight{{}}] children: [Mesh{{geometry: PlaneGeometry{{width: 2 height: 2}} material: MeshBasicMaterial{{texture: \"fitpic\" {mat}}}}}] }}");
        t.push(Test {
            name: "screen_texture_fit_cover_contain_crop",
            renders: vec![
                (doc("fit: @fill"), 0.0, opts.clone()),
                (doc("fit: @cover"), 0.0, opts.clone()),
                (doc("fit: @contain"), 0.0, opts.clone()),
                (doc("fit: @cover crop: [300, 0, 100, 100]"), 0.0, opts.clone()),
                (doc("fit: @cover crop: [0, 100, 100, 100]"), 0.0, opts.clone()),
                (doc("fit: @cover crop: keys([0, [0, 0, 100, 200]], [1, [300, 0, 100, 200]])"), 1.0, opts.clone()),
            ],
            check,
        });
    }
    // Last, so the cases before it record in the order they always have.
    t.push(sun_disc_points_move_the_shadow_edge());
    t
}


/// Sum of absolute horizontal differences in a band of rows (sharpness).
fn sharpness(p: &Picture, y0: f32, y1: f32) -> f32 {
    let (a, b) = ((y0 * p.height as f32) as usize, (y1 * p.height as f32) as usize);
    let mut s = 0.0;
    for y in a..b {
        for x in 1..p.width {
            s += (lum(p.px(x, y)) - lum(p.px(x - 1, y))).abs();
        }
    }
    s / ((b - a) * p.width) as f32
}


/// The fit test's picture, written once to the temp directory.
fn fit_picture(dir: &Path) -> PathBuf {
    let (w, h) = (400usize, 200usize);
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let c: [u8; 3] = match x / 100 {
                0 => [255, 0, 0],
                1 => [0, 255, 0],
                2 => [0, 0, 255],
                _ => [255, 255, 255],
            };
            let k = if y < 100 { 1.0 } else { 0.5 };
            let i = (y * w + x) * 4;
            rgba[i..i + 4].copy_from_slice(&[(c[0] as f32 * k) as u8, (c[1] as f32 * k) as u8, (c[2] as f32 * k) as u8, 255]);
        }
    }
    let png = encode_png(&Picture { width: w, height: h, rgba }).expect("encode the screen-fit fixture");
    let path = dir.join("motion3d_fit_picture.png");
    std::fs::write(&path, png).expect("write the screen-fit fixture");
    path
}

