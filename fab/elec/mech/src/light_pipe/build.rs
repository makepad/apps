//! Light pipe solids, fit measurements and moulding warnings.

use super::params::*;
use crate::csg::{Csg, Prim, Ring};
use std::f64::consts::PI;

const TAU: f64 = 2.0 * PI;

fn deg(d: f64) -> f64 {
    d * PI / 180.0
}

fn clampf(v: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(v))
}

fn mm(v: f64) -> String {
    format!("{v:.2} mm")
}

fn circle(n: i32, r: f64, z: f64) -> Ring {
    Ring {
        z,
        pts: (0..n)
            .map(|i| {
                [
                    r * (TAU * i as f64 / n as f64).cos(),
                    r * (TAU * i as f64 / n as f64).sin(),
                ]
            })
            .collect(),
        ..Ring::default()
    }
}

// Point counts per half edge and corner keep rounded rectangles matched in a loft.
#[derive(Clone, Copy, Default)]
struct RectSplit {
    ex: i32,
    ey: i32,
    a: i32,
}

fn rounded_rect(n: i32, hx: f64, hy: f64, rc: f64, z: f64, split: &mut RectSplit) -> Ring {
    let rc = clampf(rc, 0.02, hx.min(hy) - 0.02);
    let mut sp = *split;
    if sp.a == 0 {
        let lx = hy - rc;
        let ly = hx - rc;
        let la = 0.5 * PI * rc;
        let tot = lx + ly + la;
        let q = 8.max(n / 4);
        sp.ex = 1.max((q as f64 * lx / tot).round() as i32);
        sp.ey = 1.max((q as f64 * ly / tot).round() as i32);
        sp.a = 2.max(q - sp.ex - sp.ey);
        *split = sp;
    }
    let mut g = Ring { z, ..Ring::default() };
    for k in 0..4 {
        let ux = if k % 2 != 0 { hy } else { hx };
        let uy = if k % 2 != 0 { hx } else { hy };
        let e1 = if k % 2 != 0 { sp.ey } else { sp.ex };
        let e2 = if k % 2 != 0 { sp.ex } else { sp.ey };
        // Exact quarter turns: cos and sin of k * PI / 2 round off zero, and a side's samples would
        // leave its line by an ulp (a convex hull then keeps them as flat corners).
        let (c, s) = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)][k];
        let mut put = |x: f64, y: f64| g.pts.push([x * c - y * s, x * s + y * c]);
        for i in 0..e1 {
            put(ux, (uy - rc) * i as f64 / e1 as f64);
        }
        for i in 0..sp.a {
            let t = 0.5 * PI * i as f64 / sp.a as f64;
            // At t = 0 the arc starts exactly on the side.
            put(ux - rc * (1.0 - t.cos()), uy - rc + rc * t.sin());
        }
        for i in 0..e2 {
            put(ux - rc - (ux - rc) * i as f64 / e2 as f64, uy);
        }
    }
    g
}

fn tan_deg(d: f64) -> f64 {
    deg(d).tan()
}

// A dome, spherical lens or chamfer from zc to the apex; plain is an unchamfered flat cap.
fn cap_prim(n: i32, top: i32, zc: f64, z_apex: f64, r0: f64, r_top: f64, plain: bool, frosted: bool) -> Prim {
    let mut rings = Vec::new();
    let mut crease = Vec::new();
    let mut add = |z, r, cr| {
        rings.push(circle(n, r, z));
        crease.push(cr);
    };
    let cap = z_apex - zc;
    add(zc, r0, true);
    if plain {
        add(z_apex, r_top, false);
    } else if top == TipTop::Dome.index() {
        const K: i32 = 10;
        for k in 1..=K {
            let phi = 0.5 * PI * k as f64 / (K as f64 + 0.5);
            add(zc + cap * phi.sin(), r0 * phi.cos(), false);
        }
    } else if top == TipTop::Lens.index() {
        let rs = (r0 * r0 + cap * cap) / (2.0 * cap);
        const K: i32 = 10;
        for k in 1..=K {
            let rho = r0 * (1.0 - k as f64 / (K as f64 + 0.5));
            add(zc + cap - (rs - (0.0_f64.max(rs * rs - rho * rho)).sqrt()), rho, false);
        }
    } else {
        add(z_apex, 0.1_f64.max(r0 - cap), false);
    }
    *crease.last_mut().unwrap() = true;
    Prim::loft(if frosted { "tip top (frosted)" } else { "tip top" }, rings)
        .crease(crease)
        .finish(u32::from(frosted))
}

// Shape the entry face; return the convex lens's sag below it.
fn entry_face(csg: &mut Csg, p: &PipeParams, n: i32, led: &LedPackage, z_entry: f64, room_up: f64) -> f64 {
    let lens_c = 0.9 * led.w.min(led.d) * 0.5 + p.pocket_clearance * 0.5;
    if p.entry_face == EntryFace::ConcaveDimple.index() {
        let c = lens_c;
        let s = p.lens_sag.min(0.6 * room_up);
        let rl = (c * c + s * s) / (2.0 * s);
        let mut rings = vec![circle(n, c, z_entry - 0.05)];
        const K: i32 = 8;
        for k in 0..=K {
            let rho = c * (1.0 - k as f64 / (K as f64 + 0.5));
            rings.push(circle(n, rho, z_entry + s - (rl - (0.0_f64.max(rl * rl - rho * rho)).sqrt()) + if k == 0 { 1e-3 } else { 0.0 }));
        }
        let mut crease = vec![false; rings.len()];
        crease[0] = true;
        crease[1] = true;
        *crease.last_mut().unwrap() = true;
        csg.sub(Prim::loft("entry dimple", rings).crease(crease));
        return 0.0;
    }
    let c = lens_c;
    let s = p.lens_sag.min(0.02_f64.max(z_entry - 0.05));
    if p.entry_face != EntryFace::ConvexLens.index() {
        return 0.0;
    }
    let rl = (c * c + s * s) / (2.0 * s);
    let mut rings = Vec::new();
    const K: i32 = 8;
    for k in (1..=K).rev() {
        let rho = c * (1.0 - k as f64 / (K as f64 + 0.5)) + 0.02;
        rings.push(circle(n, rho, z_entry - s + (rl - (0.0_f64.max(rl * rl - rho * rho)).sqrt())));
    }
    rings.push(circle(n, c, z_entry));
    rings.push(circle(n, c, z_entry + 0.05));
    for k in 1..rings.len() {
        if rings[k].z <= rings[k - 1].z {
            rings[k].z = rings[k - 1].z + 1e-3;
        }
    }
    let mut crease = vec![false; rings.len()];
    crease[0] = true;
    let len = crease.len();
    crease[len - 1] = true;
    crease[len - 2] = true;
    csg.add(Prim::loft("entry lens", rings).crease(crease));
    s
}

// Drafted ribs bearing from z_a to z_b, with a ramp into the core at z_lead.
fn add_ribs(
    csg: &mut Csg, count: i32, start_deg: f64, cx: f64, z_lead: f64, z_a: f64, z_b: f64,
    r_core: impl Fn(f64) -> f64, r_out: impl Fn(f64) -> f64, width: impl Fn(f64) -> f64,
) {
    for k in 0..count {
        let a = deg(start_deg) + TAU * k as f64 / count as f64;
        let ca = a.cos();
        let sa = a.sin();
        let mut pts = Vec::new();
        for z in [z_lead, z_a, z_b] {
            let rc = r_core(z);
            let w = 0.08_f64.max(width(z));
            let ro = if z == z_lead { rc } else { rc.max(r_out(z)) };
            let ri = rc - 0.15 - w * w / (4.0 * rc);
            for rr in [ri, ro] {
                for s in [-0.5, 0.5] {
                    pts.push([cx + rr * ca - s * w * sa, rr * sa + s * w * ca, z]);
                }
            }
        }
        csg.add(Prim::hull("crush rib", pts));
    }
}

fn foot_gap(p: &PipeParams, rep: &mut PipeReport, warnings: &mut Vec<String>, gap: f64) {
    rep.foot_gap_min = gap - p.stackup_tol;
    rep.foot_gap_max = gap + p.stackup_tol;
    if rep.foot_gap_min < 0.05 {
        warnings.push(format!("the foot can touch the LED at the short end of the stack-up (gap {}): a bigger air gap or a tighter tolerance", mm(rep.foot_gap_min)));
    }
    if rep.foot_gap_max > 1.27 {
        warnings.push(format!("air gap up to {} at the long end of the stack-up: light bleeds past the foot", mm(rep.foot_gap_max)));
    }
}

fn rib_warnings(p: &PipeParams, warnings: &mut Vec<String>, strain_limit: f64, location: &str) {
    if p.crush_ribs == 0 {
        warnings.push(format!("no crush ribs: nothing holds the pipe in the {location} (glue it)"));
        return;
    }
    if p.rib_interference < 0.03 {
        warnings.push("rib interference under 0.03 mm: hole tolerance can leave the pipe loose".into());
    }
    if p.rib_interference > 0.15 {
        warnings.push("rib interference over 0.15 mm: high press force, the ribs shave or the part cracks".into());
    } else if strain_limit < 3.0 && p.rib_interference > 0.08 {
        warnings.push("rib interference over 0.08 mm in a brittle grade: it may crack (PC takes more)".into());
    }
}

fn frost_side_draft(p: &PipeParams, warnings: &mut Vec<String>) {
    let fm = p.frost_mode;
    let ff = p.frost_finish.get();
    let pm = p.material.get();
    let need = f64::from(ff.min_draft_deg(pm));
    if (fm == FrostMode::TopAndSides.index() || fm == FrostMode::WholeTip.index()) && p.draft < need - 1e-3 {
        warnings.push(format!("frosted tip sides ({} on {}) want {need:.1} deg draft, the tip has {:.1}", ff.label(), pm.short_name, p.draft));
    }
}

// A front-inserted head and rod, parting at the panel front. The lower half forms the ribs.
fn front_mount(p: &PipeParams, out: &mut PipeBuild, n: i32, led: &LedPackage, z_entry: f64) {
    let PipeBuild { csg, report: rep, warnings, .. } = out;
    let tan_d = tan_deg(p.draft);
    let tan_b = tan_deg(p.body_taper);
    let z_panel = p.led_to_panel;
    let z_front = z_panel + p.panel_thickness;
    let rh = p.hole_diameter * 0.5;
    let csk = p.panel_head == PanelHead::Countersunk.index();
    let fm = p.frost_mode;

    // Leave at least 0.2 mm of straight hole below the countersink.
    let half = deg(clampf(p.countersink_angle, 30.0, 150.0) * 0.5);
    let mut over = p.head_overhang;
    if csk && over / half.tan() > p.panel_thickness - 0.2 {
        let o2 = 0.0_f64.max((p.panel_thickness - 0.2) * half.tan());
        rep.fit_notes.push(format!("head overhang {over:.2} -> {o2:.2} mm: the countersink fits the {:.2} mm panel", p.panel_thickness));
        over = o2;
    }
    let d_csk = if csk { over / half.tan() } else { 0.0 };
    let z_cb = z_front - d_csk;
    let r0 = rh - p.tip_clearance;
    let rod_r = |z: f64| {
        0.2_f64.max(if z >= z_panel {
            r0 - (z_cb - z) * tan_d
        } else {
            r0 - (z_cb - z_panel) * tan_d - (z_panel - z) * tan_b
        })
    };
    let cone_r = |z: f64| r0 + (z - z_cb) * half.tan();
    let r_head = if csk { cone_r(z_front) } else { rh + over };
    let head_r = |z: f64| r_head - (z - z_front) * tan_d;

    let head_top = z_front + if csk { 0.0 } else { p.flange_thickness };
    let z_apex = (z_front + p.tip_protrusion).max(head_top);
    if z_apex > z_front + p.tip_protrusion + 1e-3 {
        rep.fit_notes.push(format!("apex {:.2} mm proud (the flange), not {:.2}", z_apex - z_front, p.tip_protrusion));
    }
    let top = p.tip_top;
    let capped = top == TipTop::Dome.index() || top == TipTop::Lens.index();
    let mut cap = if capped { p.tip_dome } else if p.tip_chamfer > 0.01 { p.tip_chamfer } else { 0.0 };
    cap = cap.min(0.0_f64.max(z_apex - z_front - if csk { 0.0 } else { 0.2 }));
    let flat_cap = cap <= 0.01;
    let zc = z_apex - if flat_cap { 0.05 } else { cap };
    let radius = |z: f64| {
        if z > z_front + 1e-4 { head_r(z) }
        else if csk && z > z_cb { cone_r(z) }
        else { rod_r(z) }
    };
    let mut piece = |label: &str, z0: f64, z1: f64, frosted: bool, ra: f64, rb: f64| {
        if z1 <= z0 + 1e-3 {
            return;
        }
        let label = if frosted { format!("{label} (frosted)") } else { label.to_owned() };
        csg.add(Prim::loft(label, vec![
            circle(n, if ra > 0.0 { ra } else { radius(z0) }, z0),
            circle(n, if rb > 0.0 { rb } else { radius(z1) }, z1),
        ]).crease(vec![true, true]).finish(u32::from(frosted)));
    };
    let sides = fm == FrostMode::WholeTip.index() || fm == FrostMode::TopAndSides.index();
    let whole = fm == FrostMode::WholeTip.index();
    piece("rod", z_entry, z_panel, false, -1.0, -1.0);
    if csk {
        piece("rod, in the panel", z_panel, z_cb.min(zc), whole, -1.0, -1.0);
    } else {
        piece("rod, in the panel", z_panel, z_front + 0.05, whole, rod_r(z_panel), rod_r(z_front + 0.05));
    }
    if csk {
        piece("countersunk head", z_cb, z_front.min(zc), whole, -1.0, -1.0);
    }
    if !csk {
        piece("flange", z_front, zc, sides, r_head, head_r(zc));
    } else {
        piece("head side", z_front, zc, sides, -1.0, -1.0);
    }
    let rc = if zc > z_front { head_r(zc) } else { radius(zc) };
    csg.add(cap_prim(n, top, zc, z_apex, rc,
        if z_apex > z_front { head_r(z_apex) } else { cone_r(z_apex) },
        flat_cap, fm != FrostMode::None.index()));
    frost_side_draft(p, warnings);

    // The lead-in is below the back face for insertion from the front.
    let z_mid = 0.5 * (z_panel + z_cb);
    let r_out = |z: f64| rh + p.rib_interference + (z - z_mid) * tan_d;
    let width = |z: f64| p.rib_width - 2.0 * (z_cb - z) * tan_d;
    let lead = (1.732 * (r_out(z_panel) - rod_r(z_panel))).min(0.8 * (z_panel - z_entry));
    if p.crush_ribs > 0 {
        add_ribs(csg, p.crush_ribs, 90.0, 0.0, z_panel - lead, z_panel,
            if csk { z_cb } else { z_front + 0.05 }, rod_r, r_out, width);
    }
    // The snap limit is for one-time assembly, not repeated flexing.
    rib_warnings(p, warnings, f64::from(p.material.get().snap_strain_limit(false)), "panel hole");
    let sag = entry_face(csg, p, n, led, z_entry, z_panel - z_entry);
    foot_gap(p, rep, warnings, z_entry - sag - led.h);

    rep.led_top = led.h;
    rep.entry_z = z_entry;
    rep.shoulder_bottom = z_panel;
    rep.shoulder_top = z_panel;
    rep.panel_front = z_front;
    rep.apex = z_apex;
    // Place the flange's downward-facing underside below the reported parting plane.
    rep.parting_z = if csk { z_front } else { z_front + 0.005 };
    rep.tip_diameter = 2.0 * r0;
    rep.tip_clearance_back = rh - rod_r(z_panel);
    rep.body_diameter = 2.0 * rod_r(z_entry);
    rep.shoulder_diameter = 2.0 * r_head;
    rep.ribs = p.crush_ribs;
    rep.rib_interference = p.rib_interference;
    rep.rib_engaged = if p.crush_ribs > 0 { z_cb - z_panel } else { 0.0 };
    rep.extent_y = r_head;
    rep.extent_x = r_head;
    if z_cb - z_panel < 0.4 {
        warnings.push("under 0.4 mm of straight hole for the ribs to bear on: a thicker panel or a shallower countersink".into());
    }
    if rod_r(z_entry) < 0.5 * led.w.min(led.d) {
        warnings.push("rod foot narrower than the LED: part of its light misses the entry face".into());
    }
    if p.body_taper < p.draft - 1e-3 {
        warnings.push("rod taper is under the draft angle".into());
    }
    if !csk && p.head_overhang < 0.3 {
        warnings.push("flange under 0.3 mm wide: little to stop against".into());
    }
    if p.tip_clearance < 0.05 {
        warnings.push("rod clearance under 0.05 mm: it may bind in the panel hole".into());
    }
}

/// Build the solid, fitted dimensions, draft zones and manufacturing warnings.
pub fn build(p: &PipeParams) -> PipeBuild {
    let mut out = PipeBuild::default();
    let n = 32.max(p.segments / 4 * 4);
    out.csg.segments = 32.max(256.min(n));
    let pm = p.material.get();
    // One-time snap assembly uses the full material limit; repeated use would reduce it.
    let strain_limit = f64::from(pm.snap_strain_limit(false));
    let tan_d = tan_deg(p.draft);
    let tan_i = tan_deg(p.inner_draft);
    let tan_s = tan_deg(p.shutoff_draft);
    let tan_b = tan_deg(p.body_taper);

    let mount = p.mount();
    let panel_back = mount == Mount::PanelBack;
    out.report.panel_held = mount != Mount::Pcb;
    let led = led_package(p);
    let led_top = led.h;
    let z_entry = led_top + p.air_gap;
    if mount == Mount::PanelFront {
        front_mount(p, &mut out, n, &led, z_entry);
        return out;
    }
    let PipeBuild { csg, report: rep, zones, warnings } = &mut out;
    let z_panel = p.led_to_panel;
    let z_front = z_panel + p.panel_thickness;
    let zst = z_panel - if panel_back { 0.0 } else { p.shoulder_gap };
    let zsb = (z_entry + 0.8).max(zst - p.shoulder_height);
    let z_apex = z_front + p.tip_protrusion;
    let zp = if p.parting == Parting::ShoulderTop.index() { zst } else { zsb };
    let z_foot = if panel_back { z_entry } else { 0.0 };
    rep.led_top = led_top;
    rep.entry_z = z_entry;
    rep.shoulder_top = zst;
    rep.shoulder_bottom = zsb;
    rep.panel_front = z_front;
    rep.apex = z_apex;
    rep.parting_z = zp;
    if zst - p.shoulder_height < z_entry + 0.8 {
        warnings.push("panel too close to the LED for this shoulder: the shoulder is thinned".into());
    }
    if z_apex < zst + 0.5 {
        warnings.push("tip shorter than 0.5 mm".into());
    }

    // Pocket size and the body and shoulder needed to enclose it.
    let square = p.body_shape == BodyShape::Square.index();
    let round_pocket = p.pocket_shape == PocketShape::Round.index();
    let pkx = led.w * 0.5 + p.pocket_clearance;
    let pky = led.d * 0.5 + p.pocket_clearance;
    let pkr = (pkx * pkx + pky * pky).sqrt();
    let g0 = z_entry * tan_i;
    let pocket_reach = if round_pocket { pkr + g0 }
        else if square { pkx.max(pky) + g0 }
        else { ((pkx + g0) * (pkx + g0) + (pky + g0) * (pky + g0)).sqrt() };
    let mut a_top = p.body_diameter * 0.5;
    if p.fit_to_led && panel_back {
        a_top = a_top.max(0.5 * led.w.hypot(led.d) + (zsb - z_foot) * tan_b);
    } else if p.fit_to_led {
        a_top = a_top.max(pocket_reach + p.min_wall + 0.05 + zsb * tan_b);
    }
    let rs = if p.fit_to_led { (p.shoulder_diameter * 0.5).max(a_top + 0.6) } else { p.shoulder_diameter * 0.5 };
    rep.body_diameter = 2.0 * a_top;
    rep.shoulder_diameter = 2.0 * rs;

    // Tip: the upper mould half makes it narrower going up.
    let rf = p.hole_diameter * 0.5 - p.tip_clearance;
    let tip_r = |z: f64| rf + (z_front - z) * tan_d;
    rep.tip_diameter = 2.0 * rf;
    rep.tip_clearance_back = p.hole_diameter * 0.5 - tip_r(z_panel);
    if rep.tip_clearance_back < 0.02 {
        warnings.push(format!("tip binds at the panel's back face (draft eats the clearance): {}", mm(rep.tip_clearance_back)));
    }
    {
        // Shared boundary rings keep the tip continuous across its finish changes.
        let top = p.tip_top;
        let capped = top == TipTop::Dome.index() || top == TipTop::Lens.index();
        let mut cap = if capped { p.tip_dome } else if p.tip_chamfer > 0.01 { p.tip_chamfer } else { 0.0 };
        cap = cap.min(0.0_f64.max(z_apex - zst - 0.3));
        let flat_cap = cap <= 0.01;
        let zc = z_apex - if flat_cap { 0.05 } else { cap };
        let za = zst - 0.05;
        let mid = z_panel > za + 0.1 && z_panel < zc - 0.1;
        let fm = p.frost_mode;
        let mut piece = |label: &str, z0: f64, z1: f64, frosted: bool| {
            let label = if frosted { format!("{label} (frosted)") } else { label.to_owned() };
            csg.add(Prim::loft(label, vec![circle(n, tip_r(z0), z0), circle(n, tip_r(z1), z1)])
                .crease(vec![true, true]).finish(u32::from(frosted)));
        };
        if mid {
            piece("tip side, in the panel", za, z_panel, fm == FrostMode::WholeTip.index());
            piece("tip side", z_panel, zc, fm == FrostMode::WholeTip.index() || fm == FrostMode::TopAndSides.index());
        } else {
            piece("tip side", za, zc, fm == FrostMode::WholeTip.index() || fm == FrostMode::TopAndSides.index());
        }
        csg.add(cap_prim(n, top, zc, z_apex, tip_r(zc), tip_r(z_apex), flat_cap, fm != FrostMode::None.index()));
        frost_side_draft(p, warnings);
    }
    if panel_back {
        // The upper-half ribs lead in at the top for insertion from behind.
        let rh = p.hole_diameter * 0.5;
        let z_mid = 0.5 * (z_panel + z_front);
        let r_out = |z: f64| rh + p.rib_interference - (z - z_mid) * tan_d;
        let width = |z: f64| p.rib_width - 2.0 * (z - z_panel) * tan_d;
        let z_lead = z_front - 0.1_f64.min(0.2 * p.panel_thickness);
        let mut lead = 1.732 * (r_out(z_lead) - tip_r(z_lead));
        lead = lead.min(0.5 * (z_lead - z_panel));
        let zb = z_lead - lead;
        if p.crush_ribs > 0 {
            add_ribs(csg, p.crush_ribs, 90.0, 0.0, z_lead, zst - 0.05, zb, tip_r, r_out, width);
        }
        rep.ribs = p.crush_ribs;
        rep.rib_interference = p.rib_interference;
        rep.rib_engaged = if p.crush_ribs > 0 { zb - z_panel } else { 0.0 };
        rib_warnings(p, warnings, strain_limit, "panel hole");
    }

    // Shoulder: draft away from the parting line, chamfer at the other edge.
    let shoulder_r = |z: f64| rs - (z - zp).abs() * tan_d;
    {
        let ch = p.shoulder_chamfer.min((zst - zsb - 0.1).min(rs * 0.4));
        let mut rings = Vec::new();
        let mut add = |z, r| rings.push(circle(n, r, z));
        if p.parting == Parting::ShoulderTop.index() {
            if ch > 0.01 {
                add(zsb, shoulder_r(zsb) - ch);
            }
            add(if ch > 0.01 { zsb + ch } else { zsb }, shoulder_r(zsb + ch.max(0.0)));
            add(zst, shoulder_r(zst));
        } else {
            add(zsb, shoulder_r(zsb));
            add(if ch > 0.01 { zst - ch } else { zst }, shoulder_r(zst - ch.max(0.0)));
            if ch > 0.01 {
                add(zst, shoulder_r(zst) - ch);
            }
        }
        let crease = vec![true; rings.len()];
        csg.add(Prim::loft("shoulder", rings).crease(crease));
        if rs * 2.0 <= p.hole_diameter + 0.6 {
            warnings.push("shoulder barely wider than the panel hole: little to stop against".into());
        }
    }

    // Body: the lower mould half makes it wider at the top.
    let body_a = |z: f64| a_top - (zsb - z) * tan_b;
    {
        let mut sp = RectSplit::default();
        let mut ring = |z| {
            let a = 0.2_f64.max(body_a(z));
            if square { rounded_rect(n, a, a, p.body_corner.min(a * 0.95), z, &mut sp) }
            else { circle(n, a, z) }
        };
        let rings = vec![ring(z_foot), ring(zsb + 0.05)];
        let body = if square {
            // A convex hull preserves planar flats and the rounded corner samples.
            let pts = rings.iter().flat_map(|rg| rg.pts.iter().map(move |q| [q[0], q[1], rg.z])).collect();
            Prim::hull("body", pts)
        } else {
            Prim::loft("body", rings).crease(vec![true, true])
        };
        csg.add(body);
        if a_top >= shoulder_r(zsb) - 0.05 {
            warnings.push("body as wide as the shoulder: its top face is an undercut below the parting line".into());
        }
        if p.body_taper < p.draft - 1e-3 {
            warnings.push("body taper is under the draft angle".into());
        }
    }
    if p.foot_flange > 0.01 && !panel_back {
        let h = p.foot_height.min(zsb * 0.5);
        let r = body_a(0.0) + p.foot_flange;
        csg.add(Prim::loft("foot flange", vec![circle(n, r, 0.0), circle(n, r - h * tan_d, h)])
            .crease(vec![true, true]));
        warnings.push("foot flange: its top face is an undercut between flange and shoulder (side action or a split mould)".into());
    }

    // A mixed fixing has its hook at +x and its peg at -x.
    let ls = if panel_back { LegStyle::None } else { p.leg_style() };
    let legs = ls != LegStyle::None;
    let hook_and_peg = ls == LegStyle::HookAndPeg;
    let pegs_only = matches!(ls, LegStyle::PressPeg | LegStyle::HeatStake | LegStyle::LocatingPeg);
    let hooks = legs && !pegs_only;
    let split = ls == LegStyle::SplitPin;
    let peg_side = |side: f64| pegs_only || (hook_and_peg && side < 0.0);
    let r = p.pcb_hole * 0.5;
    let zc = -(p.pcb_thickness + p.retention_gap);

    // Fit the leg across the hole and leave room for its deflected barb.
    // Width, overhang, thickness and finally catch engagement give way in that order.
    let mut t = p.leg_thickness;
    let mut h = p.hook_overhang;
    let mut leg_w = p.leg_width;
    {
        let rc = 0.2_f64.max(r - p.hole_clearance);
        let margin = 0.05;
        let min_catch = 0.15;
        let mut note = |what: &str, from: f64, to: f64| {
            if (from - to).abs() < 0.005 {
                return;
            }
            rep.fit_notes.push(format!("{what} {from:.2} -> {to:.2} mm to pass the {:.2} mm PCB hole", p.pcb_hole));
        };
        // Keep a usable chord at the PCB top, where the drafted leg is widest in the hole.
        let hw_max = 0.8 * rc - (0.0 - zc) * tan_d;
        if leg_w * 0.5 > hw_max {
            note("leg width", leg_w, 2.0 * 0.15_f64.max(hw_max));
            leg_w = 2.0 * 0.15_f64.max(hw_max);
        }
        if hooks && hw_max < 0.25 {
            let need = 2.0 * ((0.25 + (0.0 - zc) * tan_d) / 0.8 + p.hole_clearance) + 0.4;
            warnings.push(format!("PCB hole too small for a snap leg: needs about {} or more", mm(need)));
        }
        let hw_top = leg_w * 0.5 + (0.0 - zc) * tan_d;
        let chord0 = (0.0_f64.max(rc * rc - hw_top * hw_top)).sqrt();
        let s0 = (0.0_f64.max(r * r - leg_w * leg_w * 0.25)).sqrt();
        let h_min = 0.1_f64.max(r + min_catch - chord0);
        if ls == LegStyle::SplitPin {
            let t_max = 2.0 * (chord0 - margin);
            if t > t_max {
                note("split pin thickness", t, t_max);
                t = t_max;
            }
            // The slot must take both prongs' deflection while retaining their minimum thickness.
            let prong_min = 0.25;
            let d_max = 0.0_f64.max((t - 2.0 * prong_min - 0.1) * 0.5 - 0.05);
            let h_max = 0.05_f64.max(d_max - t * 0.5 + s0);
            if h > h_max {
                note(if h > h_min && h_max < h_min { "hook overhang (catch below the minimum)" } else { "hook overhang" }, h, h_max);
                h = h_max;
            }
            if h < h_min && h_max > h {
                note("hook overhang raised", h, h_min.min(h_max));
                h = h_min.min(h_max);
            }
        } else if hooks {
            let budget = chord0 + s0 - margin;
            if h < h_min && budget - t > h {
                let h2 = h_min.min(budget - t);
                note("hook overhang raised", h, h2);
                h = h2;
            }
            if t + h > budget {
                let h2 = h.min(h_min.max(budget - t));
                note("hook overhang", h, h2);
                h = h2;
            }
            if t + h > budget {
                let t2 = 0.3_f64.max(budget - h);
                note("leg thickness", t, t2);
                t = t2;
            }
            if t + h > budget {
                let h2 = 0.05_f64.max(budget - t);
                note("hook overhang (catch below the minimum)", h, h2);
                h = h2;
            }
            rep.hole_margin = budget - (t + h);
        }
    }
    rep.leg_width_built = leg_w;
    rep.leg_thickness_built = t;
    rep.hook_overhang_built = h;
    let hwc = leg_w * 0.5;
    let hw = |z: f64| hwc + (z - zc) * tan_d;
    let hw0 = hw(0.0);
    let chord = |y: f64| (0.0_f64.max((r - p.hole_clearance) * (r - p.hole_clearance) - y * y)).sqrt();
    let wo = if split { t * 0.5 } else { chord(hw0) };
    let ct = h / deg(p.catch_angle.max(1.0)).tan();
    let catch_drop = if p.catch_angle >= 89.9 { 0.0 } else { ct };
    let land = p.hook_land;
    let ramp_w = 0.05_f64.max(h - land * tan_d);
    let zbot = zc - catch_drop - land - ramp_w / deg(p.lead_in).tan();
    let b = if ls == LegStyle::HookIn { -1.0 } else { 1.0 };
    let mut slot_w = p.slot_width;
    if split {
        let d = 0.0_f64.max(wo + h - (0.0_f64.max(r * r - hwc * hwc)).sqrt());
        let need = 2.0 * d + 2.0 * (zc - zbot) * tan_i + 0.1;
        let most = t - 2.0 * 0.25;
        if slot_w > most {
            rep.fit_notes.push(format!("slot width {slot_w:.2} -> {most:.2} mm: wider would cut through the {t:.2} mm pin"));
            slot_w = most;
        }
        if slot_w < need {
            rep.fit_notes.push(format!("slot width {slot_w:.2} -> {need:.2} mm so the prongs pass the {:.2} mm PCB hole", p.pcb_hole));
            slot_w = need.min(most);
        }
        if slot_w < need {
            warnings.push("split pin too thin for this hole: the prongs cannot bend far enough".into());
        }
        rep.slot_width_built = slot_w;
    }
    let y_tip = 0.0_f64.max(wo + h - (0.0_f64.max(r * r - hwc * hwc)).sqrt());
    let peg_r0 = 0.2_f64.max(r - p.peg_clearance);
    let peg_r = |z: f64| 0.1_f64.max(peg_r0 + z * tan_d);
    let z_peg_bot = (-0.3_f64).min(-(p.pcb_thickness + p.peg_below));
    let mut spacing = p.hole_spacing;
    if legs && p.fit_to_led {
        // Keep the legs' flex and inward shut-off windows clear of the body and shoulder.
        let mut half = spacing * 0.5;
        if hooks {
            if b > 0.0 && !split {
                half = half.max(a_top + 0.3 + y_tip + t - wo);
            }
            if b < 0.0 || split {
                half = half.max((a_top + 0.3).max(rs + 0.3) + wo + h + 0.2 + (zst + 0.5 - zc) * tan_s);
            }
            if split {
                half = half.max(a_top + 0.3 + wo);
            }
        }
        if pegs_only || hook_and_peg {
            half = half.max(a_top + 0.3 + peg_r(zsb));
        }
        spacing = 2.0 * (half * 20.0).ceil() / 20.0;
    }
    rep.hole_spacing = spacing;
    rep.legs = hooks;
    rep.pcb_holes = legs;
    rep.pegs = pegs_only || hook_and_peg;
    rep.catch_z = zc;
    rep.leg_bottom = (if hooks { zbot } else { 0.0 }).min(if rep.pegs { z_peg_bot } else { 0.0 });
    rep.peg_diameter = if rep.pegs { 2.0 * peg_r0 } else { 0.0 };
    let u_outer = |z: f64| {
        if b > 0.0 { wo - (z - zc) * tan_s }
        else { t - wo + (z - zc) * tan_s }
    };
    let clr = 0.2;
    let windows = legs && (ls == LegStyle::HookIn || split);
    let hw_bridge_top = if windows { hwc + clr + (zst + 0.5 - zc) * tan_s } else { hw(zst) };
    let wb_peg = peg_r(zst) + p.bridge_rail;
    let wb_h = if pegs_only { wb_peg } else { (hw_bridge_top + p.bridge_rail).max(if hook_and_peg { wb_peg } else { 0.0 }) };
    let x_end = |side: f64, z: f64| spacing * 0.5 + if peg_side(side) { peg_r(z) + p.bridge_rail } else { u_outer(z) };

    // Bridge at shoulder level, out to both fixings.
    if legs {
        let mut pts = Vec::new();
        for z in [zsb, zst] {
            let yh = wb_h - (z - zp).abs() * tan_d;
            for sx in [-1.0, 1.0] {
                for sy in [-1.0, 1.0] {
                    pts.push([sx * x_end(sx, z), sy * yh, z]);
                }
            }
        }
        csg.add(Prim::hull("bridge", pts));
    }

    // Cores for the LED pocket and inward-facing catches.
    if !panel_back {
        let hx = pkx;
        let hy = pky;
        let rr = pkr;
        let mut sp = RectSplit::default();
        let mut ring = |z| {
            let g = (z_entry - z) * tan_i;
            if round_pocket { circle(n, rr + g, z) }
            else { rounded_rect(n, hx + g, hy + g, 0.15 + g, z, &mut sp) }
        };
        let rings = vec![ring(-1.0), ring(z_entry)];
        let pocket = if !round_pocket {
            let pts = rings.iter().flat_map(|rg| rg.pts.iter().map(move |q| [q[0], q[1], rg.z])).collect();
            Prim::hull("LED pocket", pts)
        } else {
            Prim::loft("LED pocket", rings).crease(vec![true, true])
        };
        csg.sub(pocket);
        let wall = body_a(0.0) - pocket_reach;
        rep.pocket_wall = wall;
        if wall < p.min_wall {
            warnings.push(format!("pocket wall {} at the foot (min {})", mm(wall), mm(p.min_wall)));
        }
        if p.air_gap > 1.27 {
            warnings.push("air gap over 1.3 mm: light bleeds past the entry face".into());
        }
        if z_entry + 1.0 > zsb {
            warnings.push("little body between the pocket and the shoulder".into());
        }
    }
    let to_world = |side: f64, w: f64, y: f64, z: f64| [side * (spacing * 0.5 + b * w), y, z];
    if windows {
        for side in [-1.0, 1.0] {
            let e = if split { -1.0 } else { 1.0 };
            let mut pts = Vec::new();
            let zt = zst + 0.5;
            for z in [zc, zt] {
                let g = (z - zc) * tan_s;
                let v0 = wo - 0.3 - g;
                let v1 = wo + h + clr + g;
                let yh = hwc + clr + g;
                for v in [v0, v1] {
                    for sy in [-1.0, 1.0] {
                        pts.push(to_world(side, e * v, sy * yh, z));
                    }
                }
            }
            csg.sub(Prim::hull("shut-off window", pts));
        }
        let inner = spacing * 0.5 - (wo + h + clr + (zst + 0.5 - zc) * tan_s);
        if inner < rs + 0.3 {
            warnings.push("shut-off window runs into the shoulder: wider hole spacing or a smaller shoulder".into());
        }
    }

    let sag = entry_face(csg, p, n, &led, z_entry, zsb - z_entry);
    if panel_back {
        foot_gap(p, rep, warnings, z_entry - sag - led_top);
    }

    // Legs and hooks, including the windows' draft-view regions.
    if hooks {
        for side in [-1.0, 1.0] {
            if peg_side(side) {
                continue;
            }
            let mut pts = Vec::new();
            for z in [zc, zst] {
                let wf = wo - (z - zc) * tan_s;
                let wbk = if split { -wf } else { wf - t };
                for w in [wbk, wf] {
                    for sy in [-1.0, 1.0] {
                        pts.push(to_world(side, w, sy * hw(z), z));
                    }
                }
            }
            csg.add(Prim::hull("leg", pts));

            let directions: &[f64] = if split { &[1.0, -1.0] } else { &[1.0] };
            for &e in directions {
                let v_in = if split { 0.0 } else { wo - t };
                let v_in_bot = if split { 0.0 } else { v_in + (zc - zbot) * tan_s };
                let prof = [
                    [v_in, zc], [wo, zc], [wo + h, zc - catch_drop],
                    [wo + h - land * tan_d, zc - catch_drop - land],
                    [wo, zbot], [v_in_bot, zbot],
                ];
                let mut pts = Vec::new();
                for q in prof {
                    for sy in [-1.0, 1.0] {
                        pts.push(to_world(side, e * q[0], sy * hw(q[1]), q[1]));
                    }
                }
                csg.add(Prim::hull("snap hook", pts));

                let a = to_world(side, e * (wo - 0.35), 0.0, 0.0);
                let c = to_world(side, e * (wo + h + 0.3), 0.0, 0.0);
                let y1 = (hwc + 0.35).max(wb_h + 0.1);
                zones.push(DraftZone {
                    x0: a[0].min(c[0]), x1: a[0].max(c[0]),
                    y0: -y1, y1, z0: zc - 0.02, z1: zst + 0.6,
                    grow: tan_s * 1.05 + 0.002,
                });
            }
        }
        if split {
            let z_top = zbot + p.slot_length;
            let hs0 = slot_w * 0.5;
            let hs = |z: f64| hs0 - (z - zbot) * tan_i;
            let yh = hw(zst) + 1.0;
            for side in [-1.0, 1.0] {
                let mut pts = Vec::new();
                for z in [zbot - 1.0, z_top] {
                    for w in [-hs(z), hs(z)] {
                        for sy in [-1.0, 1.0] {
                            pts.push(to_world(side, w, sy * yh, z));
                        }
                    }
                }
                csg.sub(Prim::hull("split slot", pts));
                csg.sub(Prim::cylinder("slot end", to_world(side, 0.0, -yh, z_top),
                    0.05_f64.max(hs(z_top)), 2.0 * yh, [0.0, 1.0, 0.0]));
            }
            if hs(z_top) < 0.1 {
                warnings.push("slot closes up at its top: shorter slot or less core draft".into());
            }
            if z_top < zc + 0.5 {
                warnings.push("slot ends below the PCB: the prongs cannot flex".into());
            }
        }

        rep.engagement = wo + h - r;
        rep.deflection = 0.0_f64.max(y_tip);
        if split {
            let z_top = zbot + p.slot_length;
            let hs_top = slot_w * 0.5 - (z_top - zbot) * tan_i;
            rep.leg_length = 0.3_f64.max(z_top - zc);
            rep.leg_thickness = 0.05_f64.max((wo - (z_top - zc) * tan_s) - hs_top);
            if 2.0 * rep.deflection > slot_w - 2.0 * (zc - zbot) * tan_i {
                warnings.push("prongs collide before the barbs pass the hole: wider slot".into());
            }
            if wo > chord(hw0) {
                warnings.push("split pin too thick for the PCB hole".into());
            }
            let root = 2.0 * (wo - (zsb - zc) * tan_s);
            if root < p.min_wall {
                warnings.push(format!("split leg only {} thick at its root (shut-off draft on both faces): shorter leg, thicker pin or less shut-off draft", mm(root)));
            }
            if rep.leg_thickness < f64::from(pm.min_wall) * 0.6 {
                warnings.push(format!("prongs {} thick: fragile", mm(rep.leg_thickness)));
            }
        } else {
            rep.leg_length = zsb - zc;
            rep.leg_thickness = t;
            if wo - t - rep.deflection < -chord(hw0) {
                warnings.push("PCB hole too small for the leg to deflect: bigger hole or thinner leg".into());
            }
            if wo - t < -chord(hw0) {
                warnings.push("leg too thick for the PCB hole".into());
            }
            let gap = spacing * 0.5 - b * (wo - t) - if b > 0.0 { 0.0 } else { h } - rep.deflection;
            if b > 0.0 && gap < a_top.max(0.0) + 0.2 {
                warnings.push("the leg touches the body when it flexes".into());
            }
            if b < 0.0 && spacing * 0.5 - (wo + h) < a_top + 0.3 {
                warnings.push("inward barb runs into the body".into());
            }
        }
        let length = rep.leg_length;
        let tt = rep.leg_thickness;
        // Rectangular cantilever of constant section: strain = 1.5 * y * h / l^2.
        rep.strain = 100.0 * 1.5 * rep.deflection * tt / (length * length);
        rep.strain_limit = strain_limit;
        rep.allowed_deflection = (strain_limit / 100.0) * length * length / (1.5 * tt);
        if rep.engagement < 0.2 {
            warnings.push(format!("barb overlaps the PCB by only {}", mm(rep.engagement)));
        }
        if rep.strain > strain_limit {
            warnings.push(format!("snap strain {:.1} % over the {} limit of {strain_limit:.0} %: longer or thinner leg, smaller hook", rep.strain, pm.short_name));
        }
        if !split && t < p.min_wall {
            warnings.push("leg thinner than the min wall".into());
        }
        if leg_w < p.min_wall {
            warnings.push("leg narrower than the min wall".into());
        }
        if p.shutoff_draft < 3.0 - 1e-3 {
            warnings.push("shut-off faces under 3 deg draft wear the mould".into());
        }
        if spacing * 0.5 - r < rs * 0.5 {
            warnings.push("PCB holes under the shoulder".into());
        }
    }

    // Pegs are round and drafted narrower toward their insertion end.
    if rep.pegs {
        let c = 0.3_f64.min(0.3 * peg_r(z_peg_bot));
        let z_top = 0.5 * (zsb + zst);
        let z_mid = -0.5 * p.pcb_thickness;
        let r_out = |z: f64| r + p.rib_interference + (z - z_mid) * tan_d;
        let width = |z: f64| p.rib_width + 2.0 * z * tan_d;
        let lead = 1.732 * 0.0_f64.max(r_out(-p.pcb_thickness) - peg_r(-p.pcb_thickness));
        let z_lead = (z_peg_bot + c).max(-p.pcb_thickness - lead);
        let z_full = (z_lead + lead).min(-0.1);
        for side in [-1.0, 1.0] {
            if !peg_side(side) {
                continue;
            }
            let cx = side * spacing * 0.5;
            let mut rings = Vec::new();
            let mut add = |z, r| {
                let mut g = circle(32.max(n / 2), r, z);
                g.cx = cx;
                rings.push(g);
            };
            add(z_peg_bot, 0.05_f64.max(peg_r(z_peg_bot + c) - c));
            add(z_peg_bot + c, peg_r(z_peg_bot + c));
            add(z_top, peg_r(z_top));
            csg.add(Prim::loft(if ls == LegStyle::HeatStake { "heat stake" } else { "peg" }, rings)
                .crease(vec![true, true, true]));
            if ls == LegStyle::PressPeg && p.crush_ribs > 0 {
                add_ribs(csg, p.crush_ribs, 0.0, cx, z_lead, z_full, zsb + 0.05, peg_r, r_out, width);
            }
        }
        if ls == LegStyle::PressPeg {
            rep.ribs = p.crush_ribs;
            rep.rib_interference = p.rib_interference;
            rep.rib_engaged = if p.crush_ribs > 0 { p.pcb_thickness.min(-z_full) } else { 0.0 };
            rib_warnings(p, warnings, strain_limit, "PCB holes");
            if rep.rib_engaged < 0.6 * p.pcb_thickness {
                warnings.push(format!("ribs bear on only {} of the PCB hole: a longer peg (Peg below PCB)", mm(rep.rib_engaged)));
            }
        }
        if ls == LegStyle::HeatStake {
            // Melt the protrusion into a spherical cap a quarter as tall as it is wide.
            let length = 0.0_f64.max(-p.pcb_thickness - z_peg_bot);
            let rm = peg_r(-p.pcb_thickness - 0.5 * length);
            let volume = PI * rm * rm * length;
            rep.stake_head_diameter = 2.0 * (48.0 * volume / (13.0 * PI)).cbrt();
            if rep.stake_head_diameter < p.pcb_hole + 0.6 {
                warnings.push(format!("stake melts into a head only {} across over the {} hole: a longer stake", mm(rep.stake_head_diameter), mm(p.pcb_hole)));
            }
            if strain_limit < 3.0 {
                warnings.push("heat staking a brittle grade (PMMA) cracks easily: PC stakes better".into());
            }
        }
        if ls == LegStyle::LocatingPeg && p.shoulder_gap > 0.05 {
            warnings.push(format!("locating pegs do not hold: the pipe can lift {} to the panel (shoulder gap 0 traps it, or glue it)", mm(p.shoulder_gap)));
        }
        if peg_r0 < 0.35 {
            warnings.push("pegs under 0.7 mm across: fragile".into());
        }
        if spacing * 0.5 - r < rs * 0.5 && !hooks {
            warnings.push("PCB holes under the shoulder".into());
        }
    }

    let mut reach = 0.0;
    if hooks {
        reach = u_outer(zst).max(wo + h);
    }
    if rep.pegs {
        reach = reach.max(peg_r(zst) + p.bridge_rail);
    }
    rep.extent_x = rs.max(if legs { spacing * 0.5 + reach + 0.2 } else { 0.0 });
    rep.extent_y = rs.max(if legs { wb_h } else { 0.0 });
    if p.tip_clearance < 0.05 {
        warnings.push("tip clearance under 0.05 mm: the tip may bind in the panel hole".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::{csg_hash, Shape};
    use fab_materials::{Colour, MaterialId};

    #[test]
    fn builds_mounts_and_fixings_with_stable_geometry() {
        let mut p = PipeParams::default();
        let first = build(&p);
        assert!(!first.csg.steps.is_empty(), "default has a solid program");
        let hash = csg_hash(&first.csg);
        assert_eq!(hash, csg_hash(&build(&p).csg), "identical parameters have a stable geometry hash");
        p.colour = Colour::hex(0xff4000).with_translucency(0.25);
        assert_eq!(hash, csg_hash(&build(&p).csg), "display colour and translucency do not change geometry");
        p.hole_diameter += 0.25;
        assert_ne!(hash, csg_hash(&build(&p).csg), "panel hole diameter changes the solid");

        let rep = &first.report;
        assert!(rep.led_top <= rep.entry_z, "entry face is at or above the LED");
        assert!(rep.entry_z < rep.shoulder_bottom, "body separates the entry and shoulder");
        assert!(rep.shoulder_bottom < rep.shoulder_top, "shoulder has positive thickness");
        assert!(rep.shoulder_top <= rep.panel_front, "shoulder stays behind the panel front");
        assert!(rep.panel_front <= rep.apex, "default apex reaches the panel front");
        assert_eq!(rep.strain_limit, f64::from(p.material.get().snap_strain_limit(false)), "snap report uses the one-time assembly limit");

        p = PipeParams { segments: 32, ..PipeParams::default() };
        for mount in [Mount::Pcb, Mount::PanelBack, Mount::PanelFront] {
            p.mount = mount.index();
            for style in [LegStyle::None, LegStyle::HookOut, LegStyle::HookIn, LegStyle::SplitPin,
                LegStyle::PressPeg, LegStyle::HeatStake, LegStyle::LocatingPeg, LegStyle::HookAndPeg] {
                p.leg_style = style.index();
                let built = build(&p);
                assert!(!built.csg.steps.is_empty(), "{mount:?}/{style:?} has a solid program");
                assert_eq!(built.report.panel_held, mount != Mount::Pcb, "{mount:?}/{style:?} reports what holds the pipe");
                assert_eq!(built.report.pcb_holes, mount == Mount::Pcb && style != LegStyle::None,
                    "{mount:?}/{style:?} panel mounts omit PCB fixings");
                let zones = if mount != Mount::Pcb { 0 } else {
                    match style {
                        LegStyle::HookOut | LegStyle::HookIn => 2,
                        LegStyle::SplitPin => 4,
                        LegStyle::HookAndPeg => 1,
                        _ => 0,
                    }
                };
                assert_eq!(built.zones.len(), zones, "{mount:?}/{style:?} has one draft zone per barb");
            }
        }
    }

    #[test]
    fn frost_slots_follow_tip_faces() {
        let mut p = PipeParams { segments: 32, ..PipeParams::default() };
        for (mount, head) in [(Mount::Pcb, PanelHead::Flange), (Mount::PanelBack, PanelHead::Flange),
            (Mount::PanelFront, PanelHead::Flange), (Mount::PanelFront, PanelHead::Countersunk)] {
            p.mount = mount.index();
            p.panel_head = head.index();
            for mode in [FrostMode::None, FrostMode::Top, FrostMode::TopAndSides, FrostMode::WholeTip] {
                p.frost_mode = mode.index();
                let built = build(&p);
                assert_eq!(built.csg.steps.iter().any(|s| s.prim.finish == 1), mode != FrostMode::None,
                    "{mount:?}/{head:?}/{mode:?} emits frost exactly when requested");
                for step in &built.csg.steps {
                    let label = step.prim.label.strip_suffix(" (frosted)").unwrap_or(&step.prim.label);
                    let frosted = match label {
                        "tip top" => mode != FrostMode::None,
                        "tip side" | "flange" | "head side" => matches!(mode, FrostMode::TopAndSides | FrostMode::WholeTip),
                        "tip side, in the panel" | "rod, in the panel" | "countersunk head" => mode == FrostMode::WholeTip,
                        _ => false,
                    };
                    assert_eq!(step.prim.finish, u32::from(frosted),
                        "{mount:?}/{head:?}/{mode:?} preserves the {label} face finish");
                }
            }
        }
    }

    #[test]
    fn cap_entry_and_section_variants_keep_valid_rings() {
        let mut p = PipeParams {
            segments: 32, body_shape: BodyShape::Square.index(), pocket_shape: PocketShape::Round.index(),
            parting: Parting::ShoulderBottom.index(), foot_flange: 0.2,
            ..PipeParams::default()
        };
        for top in [TipTop::Flat, TipTop::Dome, TipTop::Lens] {
            p.tip_top = top.index();
            for entry in [EntryFace::Flat, EntryFace::ConvexLens, EntryFace::ConcaveDimple] {
                p.entry_face = entry.index();
                let built = build(&p);
                assert!(built.csg.steps.iter().any(|s| s.prim.label == "body" && matches!(&s.prim.shape, Shape::Hull { .. })),
                    "square body retains planar flats in a hull");
                assert_eq!(built.csg.steps.iter().any(|s| s.prim.label == "entry lens"), entry == EntryFace::ConvexLens,
                    "convex entry adds its lens");
                assert_eq!(built.csg.steps.iter().any(|s| s.prim.label == "entry dimple"), entry == EntryFace::ConcaveDimple,
                    "concave entry cuts its dimple");
                for step in &built.csg.steps {
                    if let Shape::Loft { rings, crease, .. } = &step.prim.shape {
                        assert!(rings.len() >= 2, "{} has enough rings to loft", step.prim.label);
                        assert_eq!(rings.len(), crease.len(), "{} keeps a crease flag per ring", step.prim.label);
                        for pair in rings.windows(2) {
                            assert!(pair[0].z < pair[1].z, "{} ring heights increase for {top:?}/{entry:?}", step.prim.label);
                            assert_eq!(pair[0].pts.len(), pair[1].pts.len(), "{} ring point counts correspond", step.prim.label);
                        }
                    }
                }
            }
        }
        p.tip_top = TipTop::Flat.index();
        p.tip_chamfer = 0.0;
        let plain = build(&p);
        let cap = plain.csg.steps.iter().find(|s| s.prim.label.starts_with("tip top")).expect("flat tip has a cap");
        assert!(matches!(&cap.prim.shape, Shape::Loft { rings, .. } if rings.len() == 2), "plain flat cap has two boundary rings");
    }

    #[test]
    fn warns_at_air_gap_rib_and_stake_limits() {
        let has = |built: &PipeBuild, word: &str| built.warnings.iter().any(|w| w.contains(word));
        let mut p = PipeParams { segments: 32, air_gap: 1.27, ..PipeParams::default() };
        assert!(!has(&build(&p), "bleeds"), "air-gap threshold itself does not warn");
        p.air_gap = 1.28;
        assert!(has(&build(&p), "bleeds"), "air gap over 1.27 warns of light bleed");
        for (mount, style) in [(Mount::PanelBack, LegStyle::None), (Mount::PanelFront, LegStyle::None),
            (Mount::Pcb, LegStyle::PressPeg)] {
            p.mount = mount.index();
            p.leg_style = style.index();
            for (interference, word, expected) in [(0.02, "loose", true), (0.03, "loose", false),
                (0.15, "shave", false), (0.16, "shave", true)] {
                p.rib_interference = interference;
                assert_eq!(has(&build(&p), word), expected, "{mount:?}/{style:?} rib warning boundary at {interference}");
            }
            p.crush_ribs = 0;
            assert!(has(&build(&p), "glue"), "{mount:?}/{style:?} without ribs needs glue");
            p.crush_ribs = 3;
        }
        p.material = MaterialId::lookup("pmma").expect("PMMA material exists");
        p.rib_interference = 0.09;
        assert!(has(&build(&p), "brittle"), "brittle material warns above 0.08 mm rib interference");

        p.leg_style = LegStyle::HeatStake.index();
        p.peg_below = 0.1;
        let short = build(&p);
        assert!(short.report.stake_head_diameter < p.pcb_hole + 0.6, "short stake cannot form a wide enough head");
        assert!(has(&short, "melts"), "undersized melted stake head warns");
        p.peg_below = 3.0;
        let long = build(&p);
        assert!(long.report.stake_head_diameter >= p.pcb_hole + 0.6, "long stake forms a head over the hole margin");
        assert!(!has(&long, "melts"), "sufficient melted head has no size warning");
    }

    #[test]
    fn hook_fit_leaves_room_in_pcb_holes() {
        let mut p = PipeParams { segments: 32, ..PipeParams::default() };
        let mut adjusted = false;
        for style in [LegStyle::HookOut, LegStyle::HookIn, LegStyle::HookAndPeg] {
            p.leg_style = style.index();
            for hole in [1.8, 2.4, 3.0] {
                p.pcb_hole = hole;
                for width in [0.8, 2.0] {
                    p.leg_width = width;
                    let built = build(&p);
                    let rep = &built.report;
                    assert!(rep.hole_margin >= 0.0, "{style:?}, hole {hole}, width {width}: nonnegative hole margin");
                    let half_width = rep.leg_width_built * 0.5;
                    let top_width = half_width + (0.0 - rep.catch_z) * tan_deg(p.draft);
                    let radius = hole * 0.5;
                    let clear_radius = radius - p.hole_clearance;
                    assert!(top_width < clear_radius, "{style:?}, hole {hole}, width {width}: leg width fits the cleared circle");
                    let chord = (clear_radius * clear_radius - top_width * top_width).sqrt();
                    let edge = (radius * radius - half_width * half_width).sqrt();
                    assert!(rep.leg_thickness_built + rep.hook_overhang_built <= chord + edge - 0.05,
                        "{style:?}, hole {hole}, width {width}: deflected leg and barb stay inside the hole");
                    adjusted |= rep.leg_width_built < width || rep.hook_overhang_built < p.hook_overhang || rep.leg_thickness_built < p.leg_thickness;
                }
            }
        }
        assert!(adjusted, "hole-fit grid exercises dimension reductions");
    }
}
