//! The test parts. Each step makes one coupon: a few parts laid out round the origin (the app
//! centres them on the bed), the variants it compares (the k-th spinner, hook, step or band),
//! optional height bands that change flow, temperature or pressure advance while it prints,
//! and process settings it needs. Variants are numbered 1.. on the part by dots, so the result
//! is entered as "the smallest number that ...".
//!
//! Sizes assume a 0.4 mm class nozzle; heights snap to whole layers of the job's layer height.

use crate::mesh::{cuboid, cylinder, lathe, mass_centre, merge, moved, prism_xz, TURN_SEGMENTS};
use crate::Step;
use fab_plate::PartMesh;

/// One compared feature of a coupon: its number on the part and the value it tests.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    /// 1-based, as the dots on the part count it.
    pub number: usize,
    /// The tested value (a clearance in mm, a flow scale, a temperature, a K value ...).
    pub value: f64,
    pub label: String,
}

/// A height band of a coupon (the slicer's `fab_calibration_bands`): from `z_from` up, these
/// values apply until the next band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub z_from: f64,
    pub flow: Option<f64>,
    pub temp: Option<f64>,
    pub pa: Option<f64>,
}

/// A test part as a plate: its parts, what it compares, and how it prints.
#[derive(Clone, Debug)]
pub struct Coupon {
    pub step: Step,
    pub name: String,
    /// The printer and filament from the setup that made this part.
    pub printer: String,
    pub filament: String,
    pub parts: Vec<(String, PartMesh)>,
    pub variants: Vec<Variant>,
    pub bands: Vec<Band>,
    /// Process settings for this print only (key, value in profile text form).
    pub process: Vec<(String, String)>,
    /// What to do with the part once printed and what to enter.
    pub check: String,
    /// Extra numbers the result needs (e.g. the friction arm's lever), by name.
    pub facts: Vec<(String, f64)>,
}

impl Coupon {
    /// The bands in the slicer's text form (`z:flow:temp:pa;...`, `-` = unchanged).
    pub fn bands_text(&self) -> String {
        let f = |v: Option<f64>, d: usize| v.map(|v| format!("{v:.d$}")).unwrap_or_else(|| "-".into());
        self.bands.iter().map(|b| format!("{:.3}:{}:{}:{}", b.z_from, f(b.flow, 3), f(b.temp, 0), f(b.pa, 4))).collect::<Vec<_>>().join(";")
    }

    pub fn fact(&self, name: &str) -> Option<f64> {
        self.facts.iter().find(|f| f.0 == name).map(|f| f.1)
    }

    pub fn variant(&self, number: usize) -> Option<&Variant> {
        self.variants.iter().find(|v| v.number == number)
    }
}

/// What a coupon is made for: the nozzle, layer height and the filament's starting values.
#[derive(Clone, Debug, PartialEq)]
pub struct CouponSetup {
    pub nozzle: f64,
    pub layer: f64,
    /// The filament type ("PLA", "PETG", ...).
    pub material: String,
    /// The filament's flow ratio and nozzle temperature as the print would use them now.
    pub flow_ratio: f64,
    pub nozzle_temp: f64,
    /// The in-plane clearance to build the friction part with (calibrated or default).
    pub c_xy: f64,
}

impl Default for CouponSetup {
    fn default() -> Self {
        CouponSetup { nozzle: 0.4, layer: 0.2, material: "PLA".into(), flow_ratio: 1.0, nozzle_temp: 220.0, c_xy: 0.35 }
    }
}

impl CouponSetup {
    /// `h` rounded to whole layers (at least one).
    fn layers(&self, h: f64) -> f64 {
        let l = self.layer.max(0.05);
        (h / l).round().max(1.0) * l
    }
}

/// The coupon for a step, or `None` for a step without one (the head measurement).
pub fn make(step: Step, s: &CouponSetup) -> Option<Coupon> {
    Some(match step {
        Step::Clearance => clearance(s),
        Step::ZGap => z_gap(s),
        Step::SlipFit => slip_fit(s),
        Step::Snap => snap(s),
        Step::Friction => friction(s),
        Step::Flow => flow(s),
        Step::Temperature => temperature(s),
        Step::PressureAdvance => pressure_advance(s),
        Step::HeadEnvelope => return None,
    })
}

/// `n` dots (0.5 mm high bumps) in rows of five, starting at `at` (x along the row) on a face at `z`.
fn dots(n: usize, at: [f64; 2], z: f64) -> PartMesh {
    merge((0..n).map(|i| cylinder([at[0] + 1.6 * (i % 5) as f64, at[1] - 1.6 * (i / 5) as f64], 0.5, z - 0.01, z + 0.6)))
}

/// The hub of a spinner: a post with a 45° ridge that keeps the ring on (r 4, ridge r 6, top `top`).
fn hub_profile(top: f64) -> Vec<[f64; 2]> {
    vec![[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [6.0, 4.0], [4.0, 6.0], [4.0, top], [0.0, top]]
}

/// The ring round a hub with clearance `c` normal to every face (outer radius `ro`, top `top`).
fn ring_profile(c: f64, ro: f64, top: f64) -> Vec<[f64; 2]> {
    let s = c * std::f64::consts::SQRT_2;
    vec![[4.0 + c, 0.0], [ro, 0.0], [ro, top], [4.0 + c, top], [4.0 + c, 6.0 + s - c], [6.0 + s, 4.0], [4.0 + c, 2.0 - s + c]]
}

/// Print-in-place spinners: a ring captive on a ridged hub, nine clearances. Measures the
/// smallest gap that still turns freely (Mechanics' in-plane clearance).
fn clearance(_s: &CouponSetup) -> Coupon {
    let cs: Vec<f64> = (0..9).map(|k| 0.10 + 0.05 * k as f64).collect();
    let mut parts = Vec::new();
    let mut variants = Vec::new();
    for (k, &c) in cs.iter().enumerate() {
        let at = [(k % 3) as f64 * 32.0 - 32.0, 24.0 - (k / 3) as f64 * 24.0];
        let hub = lathe(&hub_profile(9.0), TURN_SEGMENTS);
        let ring = merge([lathe(&ring_profile(c, 10.0, 7.0), TURN_SEGMENTS), cuboid([9.5, -2.5, 0.0], [18.0, 2.5, 3.0]), dots(k + 1, [11.0, 1.2], 3.0)]);
        parts.push((format!("Spinner {} hub", k + 1), moved(hub, [at[0], at[1], 0.0])));
        parts.push((format!("Spinner {} ring", k + 1), moved(ring, [at[0], at[1], 0.0])));
        variants.push(Variant { number: k + 1, value: c, label: format!("{c:.2} mm") });
    }
    Coupon {
        step: Step::Clearance,
        printer: String::new(),
        filament: String::new(),
        name: "Clearance spinners".into(),
        parts,
        variants,
        bands: Vec::new(),
        process: vec![("enable_support".into(), "0".into())],
        check: "Hold each hub and turn its ring by the tab (the dots number it). Enter the smallest number that turns freely all the way round; a ring that is stuck or only turns with force does not count.".into(),
        facts: Vec::new(),
    }
}

/// Flaps printed over a pad with 1 to 4 empty layers between: the smallest gap that comes free
/// (Mechanics' clearance across layers).
fn z_gap(s: &CouponSetup) -> Coupon {
    let pad_top = s.layers(2.0);
    let flap = s.layers(1.2);
    let mut parts = Vec::new();
    let mut variants = Vec::new();
    for g in 1..=4usize {
        let x0 = (g as f64 - 2.5) * 22.0;
        let gap = g as f64 * s.layer;
        let pad = cuboid([x0, 0.0, 0.0], [x0 + 12.0, 12.0, pad_top]);
        let z = pad_top + gap;
        let post = cuboid([x0 - 5.0, 0.0, 0.0], [x0 - 1.0, 12.0, z + flap]);
        let lid = cuboid([x0 - 5.0, 1.0, z], [x0 + 11.0, 11.0, z + flap]);
        parts.push((format!("Gap {g} pad"), pad));
        parts.push((format!("Gap {g} flap"), merge([post, lid, dots(g, [x0 - 4.2, 9.0], z + flap)])));
        variants.push(Variant { number: g, value: g as f64, label: format!("{g} empty layer{}", if g == 1 { "" } else { "s" }) });
    }
    Coupon {
        step: Step::ZGap,
        printer: String::new(),
        filament: String::new(),
        name: "Layer gap flaps".into(),
        parts,
        variants,
        bands: Vec::new(),
        process: vec![("enable_support".into(), "0".into())],
        check: "Lift each flap off its pad (the dots on the post number it). Enter the smallest number that comes free with no more than a light snap.".into(),
        facts: vec![("layer".into(), s.layer)],
    }
}

/// A gauge of holes 0.00-0.30 mm per side over a peg, printed apart: slip and press fits for
/// parts that are assembled after printing.
fn slip_fit(s: &CouponSetup) -> Coupon {
    let h = s.layers(4.0);
    let cs: Vec<f64> = (0..7).map(|k| 0.05 * k as f64).collect();
    let mut gauge = Vec::new();
    let mut variants = Vec::new();
    for (k, &c) in cs.iter().enumerate() {
        let x = k as f64 * 15.0 - 45.0;
        gauge.push(moved(lathe(&[[4.0 + c, 0.0], [7.0, 0.0], [7.0, h], [4.0 + c, h]], TURN_SEGMENTS), [x, 0.0, 0.0]));
        if k + 1 < cs.len() {
            gauge.push(cuboid([x + 6.5, -2.0, 0.0], [x + 8.5, 2.0, h]));
        }
        gauge.push(dots(k + 1, [x - 3.2, -8.5], 1.0));
        variants.push(Variant { number: k + 1, value: c, label: format!("{c:.2} mm per side") });
    }
    // The dots stand on a strip below the holes, tied to each washer.
    gauge.push(cuboid([-50.0, -11.5, 0.0], [52.0, -7.0, 1.0]));
    for x in (0..7).map(|k| k as f64 * 15.0 - 45.0) {
        gauge.push(cuboid([x - 1.5, -7.5, 0.0], [x + 1.5, -6.0, 1.0]));
    }
    let gauge = merge(gauge);
    let peg = merge([
        lathe(&[[0.0, 0.0], [3.6, 0.0], [4.0, 0.4], [4.0, 12.0], [0.0, 12.0]], TURN_SEGMENTS),
        cuboid([-1.0, -6.0, 0.0], [1.0, -3.5, 3.0]),
    ]);
    Coupon {
        step: Step::SlipFit,
        printer: String::new(),
        filament: String::new(),
        name: "Fit gauge".into(),
        parts: vec![("Fit gauge".into(), gauge), ("Peg".into(), moved(peg, [0.0, 16.0, 0.0]))],
        variants,
        bands: Vec::new(),
        process: vec![("enable_support".into(), "0".into())],
        check: "Push the peg (chamfered end first) into each hole. Enter the smallest number it slides and turns in freely, and the smallest it can be pressed into by hand.".into(),
        facts: Vec::new(),
    }
}

/// Cantilever snap hooks with 0.3-0.9 mm interference and their receivers, printed flat: how
/// much a snap may grip before it will not go in or breaks, and how little still holds.
fn snap(s: &CouponSetup) -> Coupon {
    let t = s.layers(1.6);
    let len = 12.0;
    let w = 6.0;
    let slack = 0.3;
    let lead = 30f64.to_radians();
    let deltas = [0.3, 0.5, 0.7, 0.9];
    let mut parts = Vec::new();
    let mut variants = Vec::new();
    for (k, &d) in deltas.iter().enumerate() {
        let y0 = k as f64 * 16.0 - 24.0;
        // Hook (side profile, printed lying flat): handle, beam, bump with a 30° lead-in at the
        // tip and a square catch behind it.
        let bump = d + slack;
        let ramp = bump / lead.tan();
        let catch_x = len - ramp - 1.0;
        let hook = prism_xz(
            &[[-8.0, 0.0], [len, 0.0], [len, t], [len - ramp, t + bump], [catch_x, t + bump], [catch_x, t], [0.0, t], [0.0, 4.0], [-8.0, 4.0]],
            y0,
            y0 + w,
        );
        parts.push((format!("Hook {}", k + 1), merge([hook, dots(k + 1, [-7.2, y0 + w - 1.0], 4.0)])));
        // Receiver: a floor with a pocket the beam bends into, side walls, and a roof with a
        // window the bump snaps up into.
        let rx = 20.0;
        let floor = 0.8;
        let entry = floor + 1.2;
        let roof0 = entry + t + slack;
        let roof1 = roof0 + s.layers(1.6);
        let (ya, yb) = (y0 - 0.3, y0 + w + 0.3);
        let window = [catch_x - 0.2, len + 0.5];
        let receiver = merge([
            cuboid([rx, ya - 2.0, 0.0], [rx + len + 4.0, yb + 2.0, floor]),
            cuboid([rx, ya, floor - 0.01], [rx + 2.0, yb, entry]),
            cuboid([rx, ya - 2.0, floor - 0.01], [rx + len + 4.0, ya, roof1]),
            cuboid([rx, yb, floor - 0.01], [rx + len + 4.0, yb + 2.0, roof1]),
            cuboid([rx, ya - 0.01, roof0], [rx + window[0], yb + 0.01, roof1]),
            cuboid([rx + window[1], ya - 0.01, roof0], [rx + len + 4.0, yb + 0.01, roof1]),
            dots(k + 1, [rx + len + 5.0, yb + 1.0], 1.0),
            cuboid([rx + len + 4.0 - 0.01, ya - 2.0, 0.0], [rx + len + 12.5, yb + 2.0, 1.0]),
        ]);
        parts.push((format!("Receiver {}", k + 1), receiver));
        let strain = 1.5 * t * d / (len * len);
        variants.push(Variant { number: k + 1, value: d, label: format!("{d:.1} mm grip, {:.1}% strain", strain * 100.0) });
    }
    Coupon {
        step: Step::Snap,
        printer: String::new(),
        filament: String::new(),
        name: "Snap hooks".into(),
        parts,
        variants,
        bands: Vec::new(),
        process: vec![("enable_support".into(), "0".into())],
        check: "Push each hook into its receiver (same dots) until it clicks, then pull. Enter the largest number that went in without breaking, and the smallest number that held when pulled.".into(),
        facts: vec![("beam_length".into(), len), ("beam_thickness".into(), t)],
    }
}

/// A spinner at the chosen clearance with a weighted arm: the angle the arm stays off vertical
/// gives the joint friction Mechanics simulates with.
fn friction(s: &CouponSetup) -> Coupon {
    let c = s.c_xy.clamp(0.1, 0.6);
    let hub = lathe(&hub_profile(9.0), TURN_SEGMENTS);
    let arm = merge([lathe(&ring_profile(c, 10.0, 7.0), TURN_SEGMENTS), cuboid([9.5, -2.5, 0.0], [44.0, 2.5, 4.0]), cuboid([44.0, -5.0, 0.0], [54.0, 5.0, 7.0])]);
    let (_, com) = mass_centre(&arm);
    let lever = (com[0] * com[0] + com[1] * com[1]).sqrt();
    Coupon {
        step: Step::Friction,
        printer: String::new(),
        filament: String::new(),
        name: "Friction arm".into(),
        parts: vec![("Friction hub".into(), hub), ("Friction arm".into(), arm)],
        variants: Vec::new(),
        bands: Vec::new(),
        process: vec![("enable_support".into(), "0".into())],
        check: "Hold the hub with its axis level, lift the arm to the side and let it swing until it stops. Measure how far it stays from straight down (a phone level on the arm works), in degrees; repeat from the other side and enter the average.".into(),
        facts: vec![("lever".into(), lever), ("bearing_radius".into(), 5.0 + c), ("clearance".into(), c)],
    }
}

/// A staircase whose six steps print their top surfaces at flow 0.90-1.10 of the current ratio.
fn flow(s: &CouponSetup) -> Coupon {
    let scales = [0.90, 0.94, 0.98, 1.02, 1.06, 1.10];
    let step_h = s.layers(2.0);
    // Step k covers y >= 10k and rises to (k + 1) step heights: its top is the band's last layers.
    let stairs = merge((0..scales.len()).map(|k| cuboid([0.0, 10.0 * k as f64, 0.0], [30.0, 60.0, step_h * (k + 1) as f64])));
    let marks = merge((0..scales.len()).map(|k| dots(k + 1, [31.5, 10.0 * k as f64 + 5.0], 1.0)));
    let tab = cuboid([30.0 - 0.01, 0.0, 0.0], [40.0, 60.0, 1.0]);
    let bands = scales.iter().enumerate().map(|(k, &f)| Band { z_from: if k == 0 { 0.0 } else { step_h * k as f64 + 1e-3 }, flow: Some(f), temp: None, pa: None }).collect();
    Coupon {
        step: Step::Flow,
        printer: String::new(),
        filament: String::new(),
        name: "Flow steps".into(),
        parts: vec![("Flow steps".into(), merge([stairs, marks, tab]))],
        variants: scales.iter().enumerate().map(|(k, &f)| Variant { number: k + 1, value: f, label: format!("flow x{f:.2} ({:.3})", f * s.flow_ratio) }).collect(),
        bands,
        process: vec![("top_shell_layers".into(), "5".into()), ("enable_support".into(), "0".into())],
        check: "Look at the top surface of each step under a light from the side (the dots on the tab number them from the low end). Enter the step whose top is smooth and closed with no ridges or gaps between lines.".into(),
        facts: vec![("flow_ratio".into(), s.flow_ratio)],
    }
}

/// A tower of six blocks printed 10 °C above the filament's temperature down to 15 below, in
/// 5 °C bands, each with a 45° and a steep overhang.
fn temperature(s: &CouponSetup) -> Coupon {
    let seg = s.layers(7.0);
    let temps: Vec<f64> = (0..6).map(|k| s.nozzle_temp + 10.0 - 5.0 * k as f64).collect();
    // Each block: a 45° overhang on one side, a 35° one on the other, a thin ledge between blocks.
    let ledge = s.layers(0.6);
    let block = [[0.0, 0.0], [20.0, 0.0], [26.0, 6.0], [26.0, seg], [-8.0, seg], [-8.0, seg - ledge], [0.0, ledge]];
    let tower = merge((0..temps.len()).map(|k| moved(prism_xz(&block, 0.0, 10.0), [0.0, 0.0, seg * k as f64])));
    let bands = temps.iter().enumerate().map(|(k, &t)| Band { z_from: seg * k as f64, flow: None, temp: Some(t), pa: None }).collect();
    Coupon {
        step: Step::Temperature,
        printer: String::new(),
        filament: String::new(),
        name: "Temperature tower".into(),
        parts: vec![("Temperature tower".into(), tower)],
        variants: temps.iter().enumerate().map(|(k, &t)| Variant { number: k + 1, value: t, label: format!("{t:.0} °C") }).collect(),
        bands,
        process: vec![("enable_support".into(), "0".into())],
        check: "Count the blocks from the bottom (block 1 is the hottest). Enter the block with the cleanest overhangs and least stringing whose layers still hold firmly together (try to snap a corner).".into(),
        facts: Vec::new(),
    }
}

/// A square tube printed with pressure advance K stepping 0.005 every band, with a ledge at
/// every band edge as a ruler; sharp corners mark the right K.
fn pressure_advance(s: &CouponSetup) -> Coupon {
    let band = s.layers(2.0);
    let ks: Vec<f64> = (0..13).map(|k| 0.005 * k as f64).collect();
    let top = band * ks.len() as f64;
    let wall = 2.0 * s.nozzle * 1.1;
    let side = 40.0;
    let tube = merge([
        cuboid([0.0, 0.0, 0.0], [side, wall, top]),
        cuboid([0.0, side - wall, 0.0], [side, side, top]),
        cuboid([0.0, 0.0, 0.0], [wall, side, top]),
        cuboid([side - wall, 0.0, 0.0], [side, side, top]),
    ]);
    let ruler = merge((1..ks.len()).map(|k| cuboid([side - 0.01, side / 2.0 - 3.0, band * k as f64 - s.layer], [side + 0.8, side / 2.0 + 3.0, band * k as f64 + s.layer])));
    let bands = ks.iter().enumerate().map(|(k, &v)| Band { z_from: band * k as f64, flow: None, temp: None, pa: Some(v) }).collect();
    Coupon {
        step: Step::PressureAdvance,
        printer: String::new(),
        filament: String::new(),
        name: "Pressure advance tube".into(),
        parts: vec![("PA tube".into(), merge([tube, ruler]))],
        variants: ks.iter().enumerate().map(|(k, &v)| Variant { number: k + 1, value: v, label: format!("K {v:.3}") }).collect(),
        bands,
        process: vec![
            ("wall_loops".into(), "2".into()),
            ("sparse_infill_density".into(), "0%".into()),
            ("top_shell_layers".into(), "0".into()),
            ("enable_support".into(), "0".into()),
        ],
        check: "Look at the corners from the bottom up; the ledges on one side mark band edges (band 1 is the bottom). Enter the band where corners are sharpest: no bulge before the corner and no gap after it. Print with the printer's own flow calibration off.".into(),
        facts: vec![("band_height".into(), band)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_coupon_is_printable_geometry() {
        let s = CouponSetup::default();
        for step in Step::ALL {
            let Some(c) = make(step, &s) else { continue };
            assert!(!c.parts.is_empty(), "{:?}", step);
            for (name, m) in &c.parts {
                assert!(m.volume() > 0.0, "{name}: outward wound");
                let b = m.bounds();
                assert!(b.min[2] > -1e-6, "{name}: nothing under the bed ({})", b.min[2]);
                assert!(b.size()[0] < 200.0 && b.size()[1] < 200.0, "{name}: fits a bed");
            }
            for w in c.bands.windows(2) {
                assert!(w[1].z_from > w[0].z_from, "{:?}: bands rise", step);
            }
        }
    }

    #[test]
    fn spinner_gap_is_the_clearance() {
        // The ring's inner face is c from the hub's on the straight and the 45° faces.
        for c in [0.1, 0.3, 0.5] {
            let r = ring_profile(c, 10.0, 7.0);
            assert!((r[0][0] - 4.0 - c).abs() < 1e-12);
            // The ridge tip (6, 4) to the ring's groove (6 + c√2, 4) along the 45° normal is c.
            let d = (r[5][0] - 6.0) / std::f64::consts::SQRT_2;
            assert!((d - c).abs() < 1e-12);
            // The ring keeps a wall over its groove.
            assert!(10.0 - r[5][0] > 3.0);
        }
    }

    #[test]
    fn bands_text_round_trips_the_slicer_form() {
        let c = make(Step::Flow, &CouponSetup::default()).unwrap();
        let t = c.bands_text();
        assert!(t.starts_with("0.000:0.900:-:-;"), "{t}");
        assert_eq!(t.split(';').count(), 6);
        let p = make(Step::PressureAdvance, &CouponSetup::default()).unwrap();
        assert!(p.bands_text().contains(":-:-:0.0050"));
    }

    #[test]
    fn friction_lever_is_from_the_arm() {
        let c = make(Step::Friction, &CouponSetup::default()).unwrap();
        let l = c.fact("lever").unwrap();
        assert!(l > 15.0 && l < 45.0, "{l}");
    }
}
