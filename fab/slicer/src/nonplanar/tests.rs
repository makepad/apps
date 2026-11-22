use super::*;

/// The validator's placeholder head, left nozzle active (boxes, mm): the 40 deg cone as
/// rings around the soft tip zone (as `cone_rings` makes them), the profile's 4 mm block
/// height, footprints bigger than assumed.
pub(crate) fn head() -> Vec<NpBox> {
    let b = |name: &str, ox: f64, hx: f64, hy: f64, hole: f64, e: f64, margin: f64| NpBox { name: name.into(), ox, oy: 0.0, hx, hy, hole, e, margin };
    let rise = 1.0 / 40f64.to_radians().tan();
    let edges = [1.1, 1.4, 1.8, 2.3, 2.9, 3.6, 4.2, 4.8];
    let margins = [0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.5];
    let mut v: Vec<NpBox> = edges.windows(2).enumerate().map(|(i, w)| {
        let e = (((w[0] - 0.5) * rise).min(4.0) - 0.02).max(margins[i]);
        b(&format!("nozzle cone {}", i + 1), 0.0, w[1], w[1], w[0], e, margins[i])
    }).collect();
    v.extend([
        b("heater block", 0.0, 12.0, 14.0, 0.0, 4.0, 1.0),
        b("cooling duct", 0.0, 20.0, 20.0, 0.0, 6.0, 1.0),
        b("head body", 12.5, 47.5, 40.0, 0.0, 12.0, 1.5),
        b("other nozzle", 25.0, 4.5, 4.5, 0.0, 4.5, 1.0),
        b("other heater block", 25.0, 12.0, 14.0, 0.0, 8.5, 1.0),
    ]);
    v
}

/// A low dome: cylinder r22 h10 under a sphere R80 cap (3.1 mm, rim 16 deg), centred at (100, 100).
fn low_dome(x: f64, y: f64, z: f64) -> f64 {
    let (x, y) = (x - 100.0, y - 100.0);
    let cyl = (x * x + y * y).sqrt() - 22.0;
    let sph = (x * x + y * y + (z - 13.1 + 80.0).powi(2)).sqrt() - 80.0;
    cyl.max(-z).max(sph)
}
const DOME: ([f64; 3], [f64; 3]) = ([78.0, 78.0, 0.0], [122.0, 122.0, 13.1]);

/// A gradient top: 60 x 40 block rising 4 deg along X (10 -> 14.2 mm).
fn wedge(x: f64, y: f64, z: f64) -> f64 {
    let (x, y) = (x - 100.0, y - 100.0);
    let (s, c) = 4f64.to_radians().sin_cos();
    (x.abs() - 30.0).max(y.abs() - 20.0).max(-z).max(-s * x + c * z - c * 12.1)
}
const WEDGE: ([f64; 3], [f64; 3]) = ([70.0, 80.0, 0.0], [130.0, 120.0, 14.3]);

fn smin(a: f64, b: f64, k: f64) -> f64 { let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0); b + (a - b) * h - k * h * (1.0 - h) }
/// A blobby lid: a 30 x 30 x 6 slab smooth-blended with five low spheres.
fn blobby(x: f64, y: f64, z: f64) -> f64 {
    let (x, y) = (x - 100.0, y - 100.0);
    let slab = (x.abs() - 15.0).max(y.abs() - 15.0).max((z - 3.0).abs() - 3.0);
    let mut d = slab;
    for b in [[-4.8, -3.6, 3.0, 4.5], [5.4, -2.4, 2.4, 4.2], [0.0, 6.0, 1.8, 5.4], [-7.2, 7.2, 2.4, 3.9], [7.2, 7.2, 2.1, 4.2f64]] {
        d = smin(d, ((x - b[0]).powi(2) + (y - b[1]).powi(2) + (z - b[2]).powi(2)).sqrt() - b[3], 2.4);
    }
    d.max(x.abs() - 15.0).max(y.abs() - 15.0).max(-z)
}
const BLOB: ([f64; 3], [f64; 3]) = ([85.0, 85.0, 0.0], [115.0, 115.0, 8.0]);

fn on_shells(f: Field3, p: &NpPlan, tol: f64) {
    let (mut exact, mut total) = (0usize, 0usize);
    for r in &p.regions {
        let ns = r.shells.len();
        for (sh, rings) in r.shells.iter().enumerate() {
            let lev = -((ns - 1 - sh) as f64) * p.settings.layer_h;
            for ring in rings {
                assert_eq!(ring.first(), ring.last(), "a ring is closed");
                for q in ring {
                    let d = (f(q[0], q[1], q[2]) - lev).abs();
                    // At the part's edge the shell is the surface's normal offset, not the field's level.
                    assert!(d < 0.25, "a path point far off its shell: {q:?} by {d}");
                    if d < tol { exact += 1; }
                    total += 1;
                }
            }
        }
    }
    assert!(exact as f64 >= 0.95 * total as f64, "only {exact} of {total} path points on the field's level");
}

/// One plan of the low dome: it goes non-planar on exact shells, its cap clip depends on z
/// only, the plan says which regions meet a box, and a slope cap the dome exceeds refuses.
#[test]
fn a_low_dome_goes_non_planar_clips_by_z_and_reports_its_reach() {
    let s = NpSettings::default();
    let p = plan(&low_dome, DOME, &s, &head()).unwrap();
    let st = &p.stats;
    eprintln!("low dome: {st:?}");
    assert!(st.coverage() > 0.6, "coverage {}", st.coverage());
    assert!(st.max_slope_deg <= s.slope_cap_deg + 1e-6 && st.max_pitch_deg <= s.slope_cap_deg + 1e-6);
    assert!(st.min_clearance.is_some_and(|c| c >= 0.0));
    assert!(st.terrace_p95_np < st.terrace_p95_planar && st.terrace_mean_np < 0.5 * st.terrace_mean_planar);
    assert_eq!(p.regions.len(), 1);
    assert_eq!(p.regions[0].shells.len(), 2);
    assert!(p.regions[0].first_shell.0 >= s.t_min - 1e-6 && p.regions[0].first_shell.1 < s.t_min + s.layer_h + 1e-6);
    on_shells(&low_dome, &p, 1e-6);
    // The cap clip: far below the cap nothing is removed; near the top the rim band is.
    assert!(p.cap_clip(5.0).is_empty(), "no clip far below the cap");
    let a = p.cap_clip(12.0);
    assert!(!a.is_empty());
    let area: f64 = a.iter().map(|e| e.area_mm2()).sum();
    assert!(area > 10.0, "clip area {area}");
    // Every removed column is in a region and capped below z.
    for e in &a { for q in &e.contour.pts {
        let (x, y) = (q.x_mm(), q.y_mm());
        let inner = (x - 100.0).hypot(y - 100.0) < 21.5;
        let near = (-3..=3).any(|i| (-3..=3).any(|j| p.region_at(x + 0.1 * i as f64, y + 0.1 * j as f64).is_some()));
        if inner { assert!(near, "a clip point ({x:.2}, {y:.2}) away from every region"); }
    } }
    // The same z gives the same clip, whatever was traced before.
    let _ = p.cap_clip(11.0);
    assert_eq!(p.cap_clip(12.0), a, "the clip depends on z only");
    // Above the top everything in the regions is removed.
    let top: f64 = p.cap_clip(20.0).iter().map(|e| e.area_mm2()).sum();
    assert!((top - p.stats.np_area).abs() < 0.1 * p.stats.np_area, "{top} vs {}", p.stats.np_area);
    // Which regions a box meets.
    assert_eq!(p.regions_meeting([0.0, 0.0], [10.0, 10.0]), Vec::<usize>::new());
    assert_eq!(p.regions_meeting([150.0, 150.0], [160.0, 160.0]), vec![0]);
    // A slope cap the dome exceeds refuses.
    let mut steep = NpSettings::default();
    steep.slope_cap_deg = 25.0;
    assert!(plan(&low_dome, DOME, &steep, &head()).is_err());
}

#[test]
fn a_gradient_top_loses_its_terraces() {
    let p = plan(&wedge, WEDGE, &NpSettings::default(), &head()).unwrap();
    let st = &p.stats;
    eprintln!("gradient: {st:?}");
    assert!(st.coverage() > 0.9, "coverage {}", st.coverage());
    assert!(st.terrace_p95_np < 0.01 && st.terrace_p95_planar > 0.08);
    on_shells(&wedge, &p, 1e-6);
}

#[test]
fn a_blobby_lid_is_planned_and_checked() {
    let p = plan(&blobby, BLOB, &NpSettings::default(), &head()).unwrap();
    let st = &p.stats;
    eprintln!("blobby: {st:?}");
    assert!(st.coverage() > 0.3, "coverage {}", st.coverage());
    assert!(st.min_clearance.is_some_and(|c| c >= 0.0));
    // A bound field: the shells are iso-levels of the field, exactly.
    on_shells(&blobby, &p, 1e-6);
}

