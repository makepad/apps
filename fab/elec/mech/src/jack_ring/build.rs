//! Jack ring lofts, wrench features and fit checks.

use super::params::*;
use crate::csg::{Csg, Prim, Ring, V3};
use std::f64::consts::PI;

const TAU: f64 = 2.0 * PI;

fn deg(d: f64) -> f64 {
    d * PI / 180.0
}
fn clampf(v: f64, lo: f64, hi: f64) -> f64 {
    lo.max(hi.min(v))
}
fn smooth01(t: f64) -> f64 {
    let t = clampf(t, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn frac(v: f64) -> f64 {
    v - v.floor()
}

#[derive(Clone, Copy)]
struct ProfPt {
    z: f64,
    r: f64,
    crease: bool,
}

// Keep strictly rising heights, retaining creases and the smaller radius at a join.
fn tidy(points: &mut Vec<ProfPt>) {
    let mut out: Vec<ProfPt> = Vec::with_capacity(points.len());
    for q in points.iter().copied() {
        if let Some(last) = out.last_mut() {
            if q.z <= last.z + 1e-4 {
                last.crease |= q.crease;
                last.r = last.r.min(q.r);
                continue;
            }
        }
        out.push(q);
    }
    *points = out;
}

fn profile_at(points: &[ProfPt], z: f64) -> f64 {
    if z <= points[0].z {
        return points[0].r;
    }
    for pair in points.windows(2) {
        let [a, b] = pair else { unreachable!() };
        if z <= b.z {
            let t = (z - a.z) / (b.z - a.z);
            return a.r + (b.r - a.r) * t;
        }
    }
    points.last().expect("profile is nonempty").r
}

// From the side at radius r onto the face at zf.
fn top_edge_pts(points: &mut Vec<ProfPt>, edge: EdgeStyle, r: f64, zf: f64, size: f64, angle: f64) {
    if edge == EdgeStyle::Sharp || size <= 1e-3 {
        points.push(ProfPt {
            z: zf,
            r,
            crease: true,
        });
        return;
    }
    if edge == EdgeStyle::Chamfer {
        let h = size * deg(angle).tan();
        points.push(ProfPt {
            z: zf - h,
            r,
            crease: true,
        });
        points.push(ProfPt {
            z: zf,
            r: r - size,
            crease: true,
        });
        return;
    }
    for k in 0..=10 {
        let a = PI * 0.5 * k as f64 / 10.0;
        points.push(ProfPt {
            z: zf - size + size * a.sin(),
            r: r - size + size * a.cos(),
            crease: k == 0 || k == 10,
        });
    }
}

fn bottom_edge_pts(points: &mut Vec<ProfPt>, edge: EdgeStyle, r: f64, size: f64) {
    if edge == EdgeStyle::Sharp || size <= 1e-3 {
        points.push(ProfPt {
            z: 0.0,
            r,
            crease: true,
        });
        return;
    }
    if edge == EdgeStyle::Chamfer {
        points.push(ProfPt {
            z: 0.0,
            r: r - size,
            crease: true,
        });
        points.push(ProfPt {
            z: size,
            r,
            crease: true,
        });
        return;
    }
    for k in 0..=10 {
        let a = -PI * 0.5 + PI * 0.5 * k as f64 / 10.0;
        points.push(ProfPt {
            z: size + size * a.sin(),
            r: r - size + size * a.cos(),
            crease: k == 0 || k == 10,
        });
    }
}

fn loft_of(rings: Vec<Ring>, crease: Vec<bool>, label: &str) -> Prim {
    Prim::loft(label, rings).crease(crease)
}

fn circle_ring(z: f64, r: f64, n: i32) -> Ring {
    Ring {
        z,
        pts: (0..n)
            .map(|k| {
                let a = TAU * k as f64 / n as f64;
                [r * a.cos(), r * a.sin()]
            })
            .collect(),
        ..Ring::default()
    }
}

fn revolve(profile: &[ProfPt], n: i32, label: &str) -> Prim {
    loft_of(
        profile
            .iter()
            .map(|q| circle_ring(q.z, q.r.max(0.05), n))
            .collect(),
        profile.iter().map(|q| q.crease).collect(),
        label,
    )
}

fn box_prim(center: V3, size: V3, rot_z: f64, label: &str) -> Prim {
    Prim::cuboid(label, center, size).rot_z(rot_z)
}

// ISO 68-1 tap outline: root flat P/8, flanks 5P/16, crest flat P/4.
fn thread_space(u: f64, r_maj: f64, r_min: f64) -> f64 {
    let d = (u - 0.5).abs();
    if d <= 0.125 {
        return r_min;
    }
    if d >= 0.4375 {
        return r_maj;
    }
    r_min + (r_maj - r_min) * (d - 0.125) / 0.3125
}

fn round_up_to(n: i32, m: i32) -> i32 {
    if m <= 1 {
        n
    } else {
        (n + m - 1) / m * m
    }
}

/// Build the ring as CSG data; its bottom face rests on the panel at z = 0.
pub fn build(p: &RingParams) -> RingBuild {
    let mut csg = Csg::default();
    let mut report = RingReport::default();
    let mut warnings = Vec::new();
    let mut notes = Vec::new();
    let r_maj = p.thread_d * 0.5 + p.clearance;
    let r_min = minor_diameter(p) * 0.5 + p.clearance;
    let mode = p.thread_mode();
    let r_plain = if mode == ThreadMode::Clearance {
        p.thread_d * 0.5 + p.clearance.max(0.1)
    } else {
        r_min
    };
    let r_bore_max = if mode == ThreadMode::Modelled {
        r_maj
    } else {
        r_plain
    };
    let r_bore_min = if mode == ThreadMode::Modelled {
        r_min
    } else {
        r_plain
    };
    let mut r = p.diameter * 0.5;
    if r < r_bore_max + 0.4 {
        r = r_bore_max + 0.4;
        warnings.push(format!(
            "Diameter ({:.2}) does not clear the thread: widened to {:.2} mm.",
            p.diameter,
            2.0 * r
        ));
    }
    report.minor_d = minor_diameter(p);
    report.bore_d = 2.0 * r_bore_min;

    let mut top = p.top_style();
    let h = p.height;
    let mut zs = h;
    if top == TopStyle::Dome {
        zs = h - p.dome_height;
        if zs < 0.4 {
            warnings
                .push("Dome higher than the ring: flattened to leave a 0.4 mm side.".to_owned());
            zs = 0.4;
        }
    }
    let step_r = p.step_diameter * 0.5;
    if top == TopStyle::Step {
        if step_r <= r_bore_max + 0.4 || step_r >= r - 0.2 {
            warnings.push(format!(
                "Step diameter must lie between the bore ({:.2}) and the body ({:.2}): built flat.",
                2.0 * r_bore_max + 0.8,
                p.diameter - 0.4
            ));
            top = TopStyle::Flat;
        } else {
            zs = h - p.step_height;
            if zs < 0.4 {
                warnings
                    .push("Step higher than the ring: lowered to leave a 0.4 mm body.".to_owned());
                zs = 0.4;
            }
        }
    }
    let z_lo = if p.flange {
        p.flange_thickness.min(zs - 0.2)
    } else {
        0.0
    };

    let mut ct = if p.top_edge() == EdgeStyle::Sharp {
        0.0
    } else {
        p.top_edge_size
    };
    let mut cb = if p.flange || p.bottom_edge() == EdgeStyle::Sharp {
        0.0
    } else {
        p.bottom_edge_size
    };
    let rim_limit = r - if top == TopStyle::Step {
        step_r + 0.2
    } else {
        r_bore_max + 0.4
    };
    if ct > rim_limit {
        ct = rim_limit.max(0.0);
        notes.push(format!(
            "Top edge narrowed to {ct:.2} mm (it would reach the bore / step)."
        ));
    }
    let ct_h = if p.top_edge() == EdgeStyle::Chamfer {
        ct * deg(p.top_angle).tan()
    } else {
        ct
    };
    let side = zs - z_lo;
    if ct_h + cb > side - 0.05 {
        let k = (side - 0.05).max(0.0) / (ct_h + cb).max(1e-4);
        ct *= k;
        cb *= k;
        notes.push(format!("Edges scaled down to fit the side ({side:.2} mm)."));
    }
    let mut profile = Vec::new();
    if p.flange {
        profile.push(ProfPt {
            z: 0.02_f64.min(z_lo * 0.5),
            r,
            crease: true,
        });
    } else {
        bottom_edge_pts(&mut profile, p.bottom_edge(), r, cb);
    }
    top_edge_pts(&mut profile, p.top_edge(), r, zs, ct, p.top_angle);
    let mut top_z = zs;
    if top == TopStyle::Dome {
        // Sphere through the rim and apex, ending inside the bore that removes its cap.
        let rr = r - ct;
        let d = h - zs;
        let rs = (rr * rr + d * d) / (2.0 * d);
        let zc = h - rs;
        let r_stop = 0.2_f64.max(r_bore_min - 0.3);
        let a0 = clampf(rr / rs, -1.0, 1.0).asin();
        let a1 = clampf(r_stop / rs, -1.0, 1.0).asin();
        for k in 1..=16 {
            let a = a0 + (a1 - a0) * k as f64 / 16.0;
            profile.push(ProfPt {
                z: zc + rs * a.cos(),
                r: rs * a.sin(),
                crease: false,
            });
        }
        top_z = profile.last().expect("dome profile is nonempty").z;
    }
    tidy(&mut profile);

    let grip = p.grip();
    let mut n = 48.max(p.segments);
    let knurl = matches!(
        grip,
        Grip::StraightKnurl | Grip::DiamondKnurl | Grip::Flutes
    );
    if grip == Grip::Hex {
        n = round_up_to(n, 12);
    }
    if grip == Grip::Flats {
        n = round_up_to(n, 4);
    }
    if knurl {
        let per = 4.max((if grip == Grip::Flutes { 12_i32 } else { 8 }).min(768 / p.ribs / 2 * 2));
        n = p.ribs * per;
    }
    let mut nt = 48.max(p.segments);
    if mode == ThreadMode::Modelled {
        nt = 1440.min(round_up_to(nt, 48));
    }
    if n != p.segments {
        notes.push(format!(
            "Body sampled at {n} points around (whole teeth / flats)."
        ));
    }
    if nt != p.segments {
        notes.push(format!(
            "Thread sampled at {nt} points around (its profile's corners)."
        ));
    }

    let band_lo = z_lo + clampf(p.grip_from.min(p.grip_to), 0.0, 1.0) * side;
    let band_hi = z_lo + clampf(p.grip_from.max(p.grip_to), 0.0, 1.0) * side;
    let fade_len = 0.05_f64.max(0.25_f64.min((band_hi - band_lo) * 0.25));
    let fade_lo = p.grip_from > 0.001;
    let fade_hi = p.grip_to < 0.999;
    let tan_k = deg(p.knurl_angle).tan();
    let knurl_lead = p.ribs as f64 * tan_k / (TAU * r);
    let fade = |z: f64| {
        if z < band_lo - 1e-4 || z > band_hi + 1e-4 {
            return 0.0;
        }
        let mut f = 1.0;
        if fade_lo {
            f *= smooth01((z - band_lo) / fade_len);
        }
        if fade_hi {
            f *= smooth01((band_hi - z) / fade_len);
        }
        f
    };
    let v_groove = |a: f64| 1.0 - 2.0 * (frac(a + 0.5) - 0.5).abs();
    let side_at = |th: f64, z: f64| -> f64 {
        match grip {
            Grip::Flats | Grip::Hex => {
                let k = if grip == Grip::Hex { 6 } else { 2 };
                let af = p.across_flats * 0.5;
                let mut radius: f64 = 1e9;
                for i in 0..k {
                    let c = (th - TAU * i as f64 / k as f64).cos();
                    if c > 1e-3 {
                        radius = radius.min(af / c);
                    }
                }
                radius
            }
            Grip::StraightKnurl => {
                let f = fade(z);
                if f > 0.0 {
                    r - p.grip_depth * f * v_groove(p.ribs as f64 * th / TAU)
                } else {
                    1e9
                }
            }
            Grip::DiamondKnurl => {
                let f = fade(z);
                if f <= 0.0 {
                    return 1e9;
                }
                let a = p.ribs as f64 * th / TAU;
                r - p.grip_depth
                    * f
                    * v_groove(a + z * knurl_lead).max(v_groove(a - z * knurl_lead))
            }
            Grip::Flutes => {
                let f = fade(z);
                if f <= 0.0 {
                    return 1e9;
                }
                let d = (frac(p.ribs as f64 * th / TAU + 0.5) - 0.5).abs() / 0.3;
                r - p.grip_depth * f * (1.0 - d * d).max(0.0).sqrt()
            }
            Grip::None => 1e9,
        }
    };

    let mut heights = profile.clone();
    let mut add_z = |z: f64| {
        if z > profile[0].z + 1e-3 && z < profile.last().expect("profile is nonempty").z - 1e-3 {
            heights.push(ProfPt {
                z,
                r: profile_at(&profile, z),
                crease: false,
            });
        }
    };
    if knurl {
        for z in [band_lo, band_lo + fade_len, band_hi - fade_len, band_hi] {
            add_z(z);
        }
        let mut dz = fade_len / 4.0;
        if grip == Grip::DiamondKnurl && knurl_lead > 0.0 {
            dz = dz.min(1.0 / knurl_lead / 12.0);
        }
        let mut z = band_lo;
        while z < band_hi {
            add_z(z);
            z += dz;
        }
    }
    heights.sort_by(|a, b| a.z.total_cmp(&b.z));
    tidy(&mut heights);

    // Flats and hex cut the revolved body with a prism, preserving STEP corners.
    let faceted = matches!(grip, Grip::Flats | Grip::Hex);
    let mut rings = Vec::new();
    let mut crease = Vec::new();
    let mut side_min: f64 = 1e9;
    for q in &heights {
        let mut ring = Ring {
            z: q.z,
            ..Ring::default()
        };
        for k in 0..n {
            let th = TAU * k as f64 / n as f64;
            let radius = 0.05_f64.max(q.r.min(side_at(th, q.z)));
            if q.z >= z_lo && q.z <= zs {
                side_min = side_min.min(radius);
            }
            let radius = if faceted { q.r } else { radius };
            ring.pts.push([radius * th.cos(), radius * th.sin()]);
        }
        rings.push(ring);
        crease.push(q.crease);
    }
    csg.add(loft_of(rings, crease, "body"));
    if faceted {
        let r_cap = r + 0.5;
        let mut lo = Ring {
            z: -0.1,
            ..Ring::default()
        };
        for k in 0..n {
            let th = TAU * k as f64 / n as f64;
            let radius = r_cap.min(side_at(th, 0.0));
            lo.pts.push([radius * th.cos(), radius * th.sin()]);
        }
        let hi = Ring {
            z: top_z + 0.1,
            pts: lo.pts.clone(),
            ..Ring::default()
        };
        csg.intersect(
            loft_of(
                vec![lo, hi],
                vec![true, true],
                if grip == Grip::Hex { "hex" } else { "flats" },
            )
            .prism(),
        );
    }
    let mut outer = r;
    if p.flange {
        let rf = p.flange_diameter * 0.5;
        if rf <= r + 0.1 {
            warnings.push("Flange not wider than the body: it shows as a step at most.".to_owned());
        }
        let mut fp = vec![ProfPt {
            z: 0.0,
            r: rf,
            crease: true,
        }];
        top_edge_pts(
            &mut fp,
            EdgeStyle::Round,
            rf,
            z_lo,
            p.flange_edge.min((z_lo - 0.02).min(rf - r)),
            45.0,
        );
        tidy(&mut fp);
        if fp.len() >= 2 {
            csg.add(revolve(&fp, n, "flange"));
        }
        outer = outer.max(rf);
    }
    if top == TopStyle::Step {
        let mut sp = vec![ProfPt {
            z: zs - 0.02,
            r: step_r,
            crease: true,
        }];
        top_edge_pts(
            &mut sp,
            EdgeStyle::Round,
            step_r,
            h,
            p.step_edge
                .min((p.step_height - 0.02).min(step_r - r_bore_max - 0.2)),
            45.0,
        );
        tidy(&mut sp);
        if sp.len() >= 2 {
            csg.add(revolve(&sp, n, "step"));
        }
        top_z = h;
    }

    // Bore loft: right-hand thread or plain circles, with a countersink at both faces.
    let z0 = -0.05;
    let z1 = top_z + 0.05;
    let cs_r = r_min + p.lead_in;
    let cone = |z: f64| (cs_r - z).max(cs_r - (top_z - z));
    let mut zz = Vec::new();
    if mode == ThreadMode::Modelled {
        let dz = p.pitch / 24.0;
        let mut z = z0;
        while z < z1 {
            zz.push(z);
            z += dz;
        }
        zz.push(z1);
    } else {
        let zb = clampf(cs_r - r_plain, 0.0, top_z * 0.5 - 0.01);
        zz = vec![z0, zb, top_z - zb, z1];
    }
    let mut bore_rings = Vec::new();
    let mut bore_crease = Vec::new();
    for z in zz {
        let mut ring = Ring {
            z,
            ..Ring::default()
        };
        for k in 0..nt {
            let th = TAU * k as f64 / nt as f64;
            let radius = if mode == ThreadMode::Modelled {
                thread_space(frac(z / p.pitch - th / TAU), r_maj, r_min)
            } else {
                r_plain
            };
            let radius = radius.max(cone(z));
            ring.pts.push([radius * th.cos(), radius * th.sin()]);
        }
        bore_rings.push(ring);
        bore_crease.push(mode != ThreadMode::Modelled);
    }
    csg.sub(loft_of(
        bore_rings,
        bore_crease,
        if mode == ThreadMode::Modelled {
            "thread"
        } else {
            "bore"
        },
    ));
    let zb = (cs_r - r_bore_max).max(0.0);
    report.thread_length = (top_z - 2.0 * zb).max(0.0);
    report.protrusion = p.bushing_length - p.panel_thickness;
    report.engaged = (report.protrusion.min(top_z - zb) - zb).max(0.0) / p.pitch;

    if p.slots > 0 {
        let depth = p.slot_depth.min(top_z - 0.2);
        let len = outer + 1.0;
        for i in 0..p.slots {
            let a = 360.0 * i as f64 / p.slots as f64;
            let ar = deg(a);
            csg.sub(box_prim(
                [
                    len * 0.5 * ar.cos(),
                    len * 0.5 * ar.sin(),
                    top_z - depth + (depth + 1.0) * 0.5,
                ],
                [len, p.slot_width, depth + 1.0],
                a,
                "slot",
            ));
        }
        if p.slot_depth > top_z - 0.2 {
            warnings.push("Slots deeper than the ring: cut to 0.2 mm above the bottom.".to_owned());
        }
    }
    csg.segments = 48.max(256.min(n.max(nt) / 2));
    csg.step_max_ring_pts = 256.max(n.max(nt));

    report.outer_d = 2.0 * outer;
    report.top_z = top_z;
    if faceted {
        if p.across_flats >= p.diameter {
            warnings.push(format!(
                "Across flats ({:.2}) not under the diameter: no flats.",
                p.across_flats
            ));
        }
        if grip == Grip::Hex && p.across_flats / deg(30.0).cos() < p.diameter - 0.05 {
            notes.push(format!(
                "Hex corners ({:.2} mm) inside the diameter: it is a plain hex.",
                p.across_flats / deg(30.0).cos()
            ));
        }
    }
    report.min_wall = (if side_min < 1e8 { side_min } else { r }) - r_bore_max;
    if report.min_wall < 0.8 {
        warnings.push(format!(
            "Thin wall: {:.2} mm between the thread's root and the side (keep 0.8 or more).",
            report.min_wall
        ));
    }
    if top == TopStyle::Step && step_r - r_bore_max < 0.6 {
        warnings.push("Step boss wall under 0.6 mm round the thread.".to_owned());
    }
    if mode != ThreadMode::Clearance {
        if report.protrusion <= 0.0 {
            warnings.push(
                "The bushing does not reach through the panel: nothing to screw onto.".to_owned(),
            );
        } else if report.engaged < 3.0 {
            warnings.push(format!(
                "Only {:.1} threads engage on the bushing (3 or more hold).",
                report.engaged
            ));
        }
        if report.protrusion > top_z + 0.05 {
            notes.push(format!(
                "The bushing stands {:.2} mm proud of the ring.",
                report.protrusion - top_z
            ));
        } else if report.protrusion > 0.0 && report.protrusion < top_z - 0.05 {
            notes.push(format!(
                "The bushing ends {:.2} mm below the ring's top.",
                top_z - report.protrusion
            ));
        }
    } else {
        notes.push("Clearance bore: the ring slides over the bushing; the jack's own nut (under it) or glue holds it.".to_owned());
    }
    if p.lead_in > 0.0 && p.lead_in + r_min < r_maj - 0.01 && mode == ThreadMode::Modelled {
        notes.push(
            "Lead-in stops inside the thread's depth: the first turn starts sharp.".to_owned(),
        );
    }
    RingBuild {
        csg,
        report,
        warnings,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg::{csg_hash, CsgOp, Shape};
    use crate::params::PartParams;
    use fab_materials::{Colour, Family, FinishId, FinishStandard, MaterialId, Processes};

    fn loft<'a>(csg: &'a Csg, label: &str) -> &'a [Ring] {
        let step = csg
            .steps
            .iter()
            .find(|step| step.prim.label == label)
            .expect("requested loft exists");
        let Shape::Loft { rings, .. } = &step.prim.shape else {
            panic!("{label} is a loft")
        };
        rings
    }

    #[test]
    fn thread_standards_and_material_parameters() {
        let table = param_table();
        let mut p = RingParams::default();
        for standard in [
            ThreadStd::M2x04,
            ThreadStd::M6x05,
            ThreadStd::M11x1,
            ThreadStd::M14x2,
            ThreadStd::UNEF38x32,
        ] {
            table.set(
                &mut p,
                table.find("thread_std").unwrap(),
                standard.index() as f64,
            );
            let data = thread_data(standard);
            assert_eq!(
                (p.thread_d, p.pitch),
                (data.d, data.pitch),
                "standard sets diameter and pitch: {standard:?}"
            );
            assert!(
                (minor_diameter(&p) - (data.d - 1.0825 * data.pitch)).abs() < 1e-6,
                "ISO 68-1 minor diameter: {standard:?}"
            );
        }
        assert_eq!(thread_text(&p), "3/8\"-32 UNEF", "UNEF thread designation");
        table.set(
            &mut p,
            table.find("thread_std").unwrap(),
            ThreadStd::M11x1.index() as f64,
        );
        assert_eq!(thread_text(&p), "M11 x 1", "metric thread designation");
        table.set(&mut p, table.find("pitch").unwrap(), 0.7);
        assert_eq!(
            p.thread_std(),
            ThreadStd::Custom,
            "editing pitch selects Custom"
        );
        table.set(&mut p, table.find("thread_d").unwrap(), 10.5);
        assert_eq!(
            thread_text(&p),
            "M10.5 x 0.7",
            "custom designation rounds dimensions"
        );

        let material = table.material_desc().unwrap();
        let options = material.options();
        assert_eq!(
            options.len(),
            7,
            "ring picker contains seven machinable metals"
        );
        for id in options {
            let m = MaterialId::lookup(id).unwrap().get();
            assert!(
                m.family == Family::Metal && m.processes.contains(Processes::CNC),
                "metal picker filters family and CNC: {id}"
            );
        }
        for id in table.finish_desc("finish").unwrap().options() {
            assert_eq!(
                FinishId::lookup(id).unwrap().get().standard,
                FinishStandard::Machined,
                "surface picker contains only machined finishes: {id}"
            );
        }
        assert!(
            table.set_text(&mut p, material, "aluminium_anodised"),
            "aluminium material resolves"
        );
        assert_eq!(
            p.colour.srgb,
            [0.07, 0.07, 0.08],
            "aluminium selection applies black anodising"
        );
        table.set(
            &mut p,
            table.find("anodize").unwrap(),
            Anodize::Teal.index() as f64,
        );
        assert_eq!(
            p.colour.srgb,
            [0.07, 0.43, 0.43],
            "anodising selection reads the shared colour"
        );
        assert!(
            table.set_text(&mut p, table.find("color").unwrap(), "#123456"),
            "custom ring colour resolves"
        );
        let manual = p.colour;
        table.set(&mut p, table.find("height").unwrap(), 4.0);
        assert_eq!(p.colour, manual, "dimension edits preserve manual colour");
        assert!(
            table.set_text(&mut p, table.find("color").unwrap(), "Clear"),
            "translucent swatch resolves"
        );
        assert_eq!(
            p.colour.translucency, 0.0,
            "ring colour remains opaque even for a translucent swatch"
        );
        p.set_colour(Colour::srgb([0.5; 3]).with_translucency(0.8));
        assert_eq!(p.colour.translucency, 0.0, "PartParams colour stays opaque");
        assert!(
            table.set_text(&mut p, material, "steel_black_oxide"),
            "black oxide material resolves"
        );
        assert_eq!(
            p.colour,
            ring_colour(&p),
            "metal selection restores its natural colour"
        );
        assert_eq!(
            surface_roughness(&p),
            0.35,
            "black oxide never renders mirror bright"
        );
        assert!(
            (ring_mass_grams(&p, 1000.0) - 7.85).abs() < 1e-5,
            "ring mass uses material density"
        );
        let text = table.to_text(&p, None);
        let mut restored = RingParams::default();
        table
            .from_text(&text, &mut restored, None)
            .expect("ring parameters load");
        assert_eq!(
            table.to_text(&restored, None),
            text,
            "ring parameter files round trip"
        );
    }

    #[test]
    fn bore_radii_follow_thread_mode_and_countersinks() {
        for mode in [
            ThreadMode::Modelled,
            ThreadMode::Cosmetic,
            ThreadMode::Clearance,
        ] {
            let p = RingParams {
                thread_mode: mode.index(),
                lead_in: 0.0,
                segments: 48,
                ..RingParams::default()
            };
            let built = build(&p);
            let r_min = minor_diameter(&p) * 0.5 + p.clearance;
            let r_maj = p.thread_d * 0.5 + p.clearance;
            let r_plain = if mode == ThreadMode::Clearance {
                p.thread_d * 0.5 + p.clearance.max(0.1)
            } else {
                r_min
            };
            let rings = loft(
                &built.csg,
                if mode == ThreadMode::Modelled {
                    "thread"
                } else {
                    "bore"
                },
            );
            assert!(
                rings.windows(2).all(|r| r[0].z < r[1].z),
                "bore heights strictly rise: {mode:?}"
            );
            let mut min_seen = f64::INFINITY;
            let mut max_seen = 0.0_f64;
            for ring in rings {
                for [x, y] in &ring.pts {
                    let radius = x.hypot(*y);
                    if mode == ThreadMode::Modelled {
                        assert!(radius >= r_min - 2e-6 && radius <= r_maj + 2e-6,
                            "every thread point lies between minor and major radii including clearance");
                        min_seen = min_seen.min(radius);
                        max_seen = max_seen.max(radius);
                    } else {
                        // The cutter extends 0.05 mm past each face, following the countersink cone.
                        let expected = r_plain
                            .max((r_min - ring.z).max(r_min - (built.report.top_z - ring.z)));
                        assert!((radius - expected).abs() < 2e-6,
                            "plain bore rings are circles at the specified bore/countersink radius: {mode:?}");
                    }
                }
            }
            if mode == ThreadMode::Modelled {
                assert!(
                    (min_seen - r_min).abs() < 2e-6 && (max_seen - r_maj).abs() < 2e-6,
                    "modelled thread reaches both crest and root flats"
                );
                let middle = &rings[rings.len() / 2];
                let low = middle
                    .pts
                    .iter()
                    .map(|p| p[0].hypot(p[1]))
                    .fold(f64::INFINITY, f64::min);
                let high = middle
                    .pts
                    .iter()
                    .map(|p| p[0].hypot(p[1]))
                    .fold(0.0, f64::max);
                assert!(
                    high - low > 0.5 * (r_maj - r_min),
                    "a modelled bore varies around a single ring"
                );
            }
            assert_eq!(
                built.report.bore_d,
                2.0 * if mode == ThreadMode::Modelled {
                    r_min
                } else {
                    r_plain
                },
                "reported smallest bore diameter follows the selected mode"
            );
        }
    }

    #[test]
    fn grips_keep_positive_star_shaped_body_rings() {
        for grip in [
            Grip::None,
            Grip::Flats,
            Grip::Hex,
            Grip::StraightKnurl,
            Grip::DiamondKnurl,
            Grip::Flutes,
        ] {
            let p = RingParams {
                grip: grip.index(),
                segments: 48,
                ..RingParams::default()
            };
            let built = build(&p);
            let body = loft(&built.csg, "body");
            assert!(
                body.windows(2).all(|r| r[0].z < r[1].z),
                "body heights strictly rise: {grip:?}"
            );
            for ring in body {
                for (i, [x, y]) in ring.pts.iter().enumerate() {
                    let radius = x.hypot(*y);
                    assert!(
                        radius > 0.0 && radius <= p.diameter * 0.5 + 2e-6,
                        "body radius positive and within diameter: {grip:?}"
                    );
                    let theta = TAU * i as f64 / ring.pts.len() as f64;
                    let cross = x * theta.sin() - y * theta.cos();
                    let dot = x * theta.cos() + y * theta.sin();
                    assert!(
                        cross.abs() < 2e-6 && dot > 0.0,
                        "one positive radius per ordered angle: {grip:?}"
                    );
                }
            }
            if matches!(grip, Grip::Flats | Grip::Hex) {
                let label = if grip == Grip::Hex { "hex" } else { "flats" };
                let step = built
                    .csg
                    .steps
                    .iter()
                    .find(|s| s.prim.label == label)
                    .unwrap();
                assert_eq!(step.op, CsgOp::Intersect, "wrench flats clip the body");
                assert!(
                    matches!(&step.prim.shape, Shape::Loft { prism: true, .. }),
                    "flats keep sharp STEP prism corners"
                );
                let prism = loft(&built.csg, label);
                assert!(
                    (prism[0].pts[0][0] - p.across_flats * 0.5).abs() < 1e-6,
                    "wrench face is at half across-flats"
                );
            }
        }
    }

    #[test]
    fn fit_warnings_and_geometry_hash() {
        let p = RingParams::default();
        let built = build(&p);
        assert!(
            built.warnings.is_empty(),
            "default ring has no fit warnings: {:?}",
            built.warnings
        );
        assert!(
            built.report.protrusion > 0.0 && built.report.engaged >= 3.0,
            "default ring reaches and engages three threads"
        );
        assert_eq!(
            csg_hash(&built.csg),
            csg_hash(&build(&p).csg),
            "equal parameters give a stable program hash"
        );
        let changed = RingParams {
            pitch: 0.75,
            ..p.clone()
        };
        assert_ne!(
            csg_hash(&built.csg),
            csg_hash(&build(&changed).csg),
            "thread pitch changes the program hash"
        );
        for protrusion in [0.0, -0.5] {
            let p = RingParams {
                bushing_length: p.panel_thickness + protrusion,
                segments: 48,
                ..p.clone()
            };
            let built = build(&p);
            assert!(
                built
                    .warnings
                    .iter()
                    .any(|w| w
                        == "The bushing does not reach through the panel: nothing to screw onto."),
                "nonpositive protrusion warns: {protrusion}"
            );
        }
        let short = RingParams {
            bushing_length: p.panel_thickness + 1.5,
            segments: 48,
            ..p
        };
        let built = build(&short);
        assert!(
            built.report.protrusion > 0.0 && built.report.engaged < 3.0,
            "short bushing isolates insufficient engagement"
        );
        assert!(
            built
                .warnings
                .iter()
                .any(|w| w == "Only 1.4 threads engage on the bushing (3 or more hold)."),
            "fewer than three engaged threads keeps the fit warning text"
        );
    }
}
