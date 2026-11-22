//! The export validator: reads every file back with our own readers and compares it with the
//! model. A file that cannot be read, or differs from the model by more than the profile's
//! tolerance, fails with the cause named.
//!
//! Geometry tolerance: the layer read back and the layer the model draws are compared as
//! regions; their exclusive-or is shrunk by the tolerance, and anything left (a part thicker
//! than twice the tolerance) is a difference. Rounding to the file's coordinate grid and chord
//! errors of curves are far below that; a shifted or missing object is far above it.

use crate::drill::read_excellon;
use crate::gerber_read::read_gerber;
use crate::image::{items_polyset, DrillHole};
use crate::model::{drill_file_attrs, gerber_file_attrs, CamModel};
use crate::profile::{Check, CheckKind, FileKind, OutFile, ResolvedProfile, Validation};
use crate::text::csv_rows;
use fab_elec_board::FillLattice;
use fab_elec_core::{to_mm, BBox, Nm, PolySet, Side, P};
use std::collections::BTreeSet;

fn pass(file: &str, kind: CheckKind, cause: impl Into<String>) -> Check {
    Check { file: file.into(), kind, ok: true, cause: cause.into(), hint: String::new(), at: None, tile: None }
}
fn at(mut c: Check, p: P) -> Check {
    c.at = Some(p);
    c
}

fn in_tile(mut c: Check, t: Option<(i32, i32)>) -> Check {
    c.tile = t;
    c
}
fn fail(file: &str, kind: CheckKind, cause: impl Into<String>, hint: impl Into<String>) -> Check {
    Check { file: file.into(), kind, ok: false, cause: cause.into(), hint: hint.into(), at: None, tile: None }
}

/// The part of `a` xor `b` thicker than twice `tol`: `None` when the two agree within it.
pub fn region_difference(a: &PolySet, b: &PolySet, tol: Nm) -> Option<(f64, BBox)> {
    let x = a.xor(b);
    if x.is_empty() {
        return None;
    }
    let core = x.offset(-tol.max(1), (tol / 4).max(1));
    if core.is_empty() {
        return None;
    }
    Some((x.area() / 1e12, core.bbox()))
}

pub fn validate(files: &[OutFile], profile: &ResolvedProfile, model: &CamModel) -> Validation {
    let mut v = Validation::default();
    // the file set against the profile
    let names: BTreeSet<&str> = files.iter().map(|f| f.name.as_str()).collect();
    let want: BTreeSet<&str> = profile.files.iter().map(|f| f.name.as_str()).collect();
    let missing: Vec<&&str> = want.difference(&names).collect();
    let extra: Vec<&&str> = names.difference(&want).collect();
    if missing.is_empty() && extra.is_empty() && names.len() == files.len() {
        v.checks.push(pass("", CheckKind::FileSet, format!("{} files, names as the profile \"{}\" says", files.len(), profile.name)));
    } else {
        let cause = format!("missing {:?}, not in the profile {:?}{}", missing, extra, if names.len() != files.len() { ", duplicate names" } else { "" });
        v.checks.push(fail("", CheckKind::FileSet, cause, "write the file set the profile lists, under its names"));
    }
    let tol = profile.tolerance;
    for spec in &profile.files {
        let Some(f) = files.iter().find(|f| f.name == spec.name) else { continue };
        match &spec.kind {
            FileKind::Gerber { layer, variant } => {
                let img = match read_gerber(&f.bytes) {
                    Ok(i) => i,
                    Err(e) => {
                        v.checks.push(fail(&f.name, CheckKind::Syntax, e.to_string(), "the Gerber file is malformed: rewrite it"));
                        continue;
                    }
                };
                v.checks.push(pass(&f.name, CheckKind::Syntax, format!("read back {} objects", img.items.len())));
                let model_items = model.layers.get(layer).map(|i| i.as_slice()).unwrap_or(&[]);
                if *variant == crate::profile::GerberVariant::X2 {
                    let want = gerber_file_attrs(*layer, model.copper_layers);
                    let func = |attrs: &[(String, Vec<String>)]| attrs.iter().find(|(k, _)| k == "FileFunction").map(|(_, v)| v.clone());
                    if func(&img.file_attrs) == func(&want) {
                        v.checks.push(pass(&f.name, CheckKind::Attributes, format!("file function {:?}", func(&want).unwrap_or_default())));
                    } else {
                        v.checks.push(fail(&f.name, CheckKind::Attributes, format!("file function {:?}, the layer is {:?}", func(&img.file_attrs), func(&want)), "write the layer's file function"));
                    }
                    // a polygon set is written as one region per polygon, so objects are not
                    // counted: the nets (and the pins) named must be the model's
                    let nets = |it: &[crate::image::GItem]| it.iter().filter_map(|i| i.attrs.net.clone()).collect::<BTreeSet<_>>();
                    let pins = |it: &[crate::image::GItem]| it.iter().filter_map(|i| i.attrs.pin.clone()).collect::<BTreeSet<_>>();
                    if nets(&img.items) != nets(model_items) || pins(&img.items) != pins(model_items) {
                        v.checks.push(fail(&f.name, CheckKind::Attributes, format!("nets {:?} and {} pins in the file, the model has {:?} and {}", nets(&img.items).len(), pins(&img.items).len(), nets(model_items).len(), pins(model_items).len()), "write the net and pin attributes of every net-bound object"));
                    }
                }
                // chords of curves stay within half the tolerance of the true curve on each side: both
                // sides together are within the tolerance, inside the band the erosion ignores
                let chord = (tol / 2).max(50);
                let a = img.to_polyset(chord);
                let b = items_polyset(model_items, chord);
                let tile_of = |p: P| model.lattice.map(|l| tile_at(l, p));
                match region_difference(&a, &b, tol) {
                    None => v.checks.push(pass(&f.name, CheckKind::Geometry, format!("{} equals the model within {:.3} um", layer.name(), tol as f64 / 1000.0))),
                    Some((area, bb)) => {
                        let tile = tile_of(bb.center());
                        let place = tile.map(|(x, y)| format!(", tile ({x}, {y})")).unwrap_or_default();
                        v.checks.push(in_tile(at(fail(
                            &f.name,
                            CheckKind::Geometry,
                            format!("{} differs from the model by {:.4} mm2, around ({:.3}, {:.3}) mm{place}", layer.name(), area, to_mm(bb.center().x), to_mm(bb.center().y)),
                            "an object is missing, shifted or drawn with another size: replot the layer",
                        ), bb.center()), tile));
                    }
                }
                // a pour written in tiles must not show where they met
                if let Some(lattice) = model.lattice {
                    let seams = seam_faults(&a, &b, lattice, tol);
                    if let Some(first) = seams.first() {
                        v.checks.push(in_tile(at(fail(
                            &f.name,
                            CheckKind::Geometry,
                            format!(
                                "{} has a seam: copper is missing across the tile edge {} = {:.3} mm between tiles ({}, {}) and ({}, {}), at ({:.3}, {:.3}) mm{}",
                                layer.name(), if first.vertical { "x" } else { "y" }, to_mm(first.line), first.tiles.0 .0, first.tiles.0 .1, first.tiles.1 .0, first.tiles.1 .1,
                                to_mm(first.at.x), to_mm(first.at.y), if seams.len() > 1 { format!(" and {} more places", seams.len() - 1) } else { String::new() }
                            ),
                            "the fill's tiles were written separately: union a fill's tiles into one region before writing it",
                        ), first.at), Some(first.tiles.0)));
                    }
                }
            }
            FileKind::Drill { plated } => {
                let img = match read_excellon(&f.bytes) {
                    Ok(i) => i,
                    Err(e) => {
                        v.checks.push(fail(&f.name, CheckKind::Syntax, e.to_string(), "the drill file is malformed: rewrite it"));
                        continue;
                    }
                };
                v.checks.push(pass(&f.name, CheckKind::Syntax, format!("read back {} holes", img.holes.len())));
                let want_attr = drill_file_attrs(*plated, model.copper_layers);
                if img.file_attr("FileFunction") != want_attr.first().map(|(_, v)| v.as_slice()) {
                    v.checks.push(fail(&f.name, CheckKind::Attributes, format!("file function {:?}", img.file_attr("FileFunction")), "write the plating of the file as its file function"));
                }
                let mut want: Vec<&DrillHole> = model.holes.iter().filter(|(p, _)| plated.map_or(true, |w| w == *p)).map(|(_, h)| h).collect();
                want.sort_by_key(|h| (h.at.x, h.at.y, h.dia));
                let mut got: Vec<&DrillHole> = img.holes.iter().collect();
                got.sort_by_key(|h| (h.at.x, h.at.y, h.dia));
                v.checks.push(compare_holes(&f.name, &got, &want, tol));
            }
            FileKind::Bom => {
                let rows = csv_rows(&String::from_utf8_lossy(&f.bytes), profile.bom.delimiter);
                let mut c = pass(&f.name, CheckKind::Bom, format!("{} lines, references equal the circuit's", profile.bom.rows.len()));
                let want_rows: Vec<&Vec<String>> = std::iter::once(&profile.bom.header).chain(profile.bom.rows.iter().map(|r| &r.cells)).collect();
                if rows.len() != want_rows.len() || rows.iter().zip(&want_rows).any(|(a, b)| a != *b) {
                    c = fail(&f.name, CheckKind::Bom, format!("the file has {} lines and does not equal the profile's table of {}", rows.len(), want_rows.len()), "write the BOM table as the profile resolved it");
                } else {
                    let got: BTreeSet<&str> = profile.bom.rows.iter().flat_map(|r| r.refs.iter().map(|s| s.as_str())).collect();
                    let want: BTreeSet<&str> = model.bom_refs.iter().map(|s| s.as_str()).collect();
                    if got != want {
                        let m: Vec<&&str> = want.difference(&got).collect();
                        let x: Vec<&&str> = got.difference(&want).collect();
                        c = fail(&f.name, CheckKind::Bom, format!("references missing from the BOM {m:?}, not in the circuit {x:?}"), "regroup the BOM from the circuit");
                    }
                }
                v.checks.push(c);
            }
            FileKind::Placement => {
                let rows = csv_rows(&String::from_utf8_lossy(&f.bytes), profile.placement.delimiter);
                let table = &profile.placement;
                let c = if rows.len() != table.rows.len() + 1 || rows[0] != table.header || rows[1..].iter().zip(&table.rows).any(|(a, b)| *a != b.cells) {
                    fail(&f.name, CheckKind::Placement, format!("the file has {} lines and does not equal the profile's table of {}", rows.len(), table.rows.len() + 1), "write the placement table as the profile resolved it")
                } else {
                    placement_vs_poses(&f.name, profile, model)
                };
                v.checks.push(c);
            }
            FileKind::Readme => {
                if f.bytes == profile.readme.as_bytes() {
                    v.checks.push(pass(&f.name, CheckKind::Syntax, "readme text as the profile says"));
                } else {
                    v.checks.push(fail(&f.name, CheckKind::Syntax, "readme differs from the profile's text", "write the profile's readme"));
                }
            }
        }
    }
    v
}

fn compare_holes(file: &str, got: &[&DrillHole], want: &[&DrillHole], tol: Nm) -> Check {
    if got.len() != want.len() {
        return fail(file, CheckKind::Drill, format!("{} holes in the file, {} in the model", got.len(), want.len()), "a hole is missing or extra: redrill from the board's pads, vias and holes");
    }
    let near = |a: Nm, b: Nm| (a as i64 - b as i64).abs() <= tol as i64;
    for (g, w) in got.iter().zip(want) {
        let slot_ok = match (g.to, w.to) {
            (None, None) => true,
            (Some(a), Some(b)) => near(a.x, b.x) && near(a.y, b.y),
            _ => false,
        };
        if !(near(g.at.x, w.at.x) && near(g.at.y, w.at.y) && near(g.dia, w.dia) && slot_ok) {
            return at(fail(
                file,
                CheckKind::Drill,
                format!("hole at ({:.3}, {:.3}) mm diameter {:.3} mm in the file, model has ({:.3}, {:.3}) mm diameter {:.3} mm", to_mm(g.at.x), to_mm(g.at.y), to_mm(g.dia), to_mm(w.at.x), to_mm(w.at.y), to_mm(w.dia)),
                "the drill file does not match the board's holes: redrill",
            ), w.at);
        }
    }
    pass(file, CheckKind::Drill, format!("{} holes equal the model's", want.len()))
}

fn placement_vs_poses(file: &str, profile: &ResolvedProfile, model: &CamModel) -> Check {
    let rows = &profile.placement.rows;
    let got: BTreeSet<&str> = rows.iter().map(|r| r.refdes.as_str()).collect();
    let want: BTreeSet<&str> = model.poses.iter().map(|p| p.refdes.as_str()).collect();
    if got != want {
        let m: Vec<&&str> = want.difference(&got).collect();
        let x: Vec<&&str> = got.difference(&want).collect();
        return fail(file, CheckKind::Placement, format!("parts without a row {m:?}, rows without a part {x:?}"), "regenerate the placement from the board's parts");
    }
    for r in rows {
        let p = model.poses.iter().find(|p| p.refdes == r.refdes).unwrap();
        let turn = r.rot.sub(p.rot).sub(r.rot_offset).udeg().rem_euclid(360_000_000);
        let mut want = p.at.sub(profile.placement.origin);
        if let (Some(mode), Side::Bottom) = (profile.placement.bottom_x, p.side) {
            want.x = mode.apply(want.x as i64) as Nm;
        }
        let tol = profile.tolerance as i64;
        if (r.at.x as i64 - want.x as i64).abs() > tol || (r.at.y as i64 - want.y as i64).abs() > tol {
            return at(fail(file, CheckKind::Placement, format!("{}: at ({:.3}, {:.3}) mm in the table, the pose gives ({:.3}, {:.3}) mm", r.refdes, to_mm(r.at.x), to_mm(r.at.y), to_mm(want.x), to_mm(want.y)), "recompute the placement position from the pose, the profile's origin and its mirroring"), p.at);
        }
        if !(turn <= 1000 || turn >= 360_000_000 - 1000) || r.side != p.side {
            return at(fail(file, CheckKind::Placement, format!("{}: rotation or side differs from the board's pose", r.refdes), "recompute the placement rotation and side from the pose and the profile's offsets"), p.at);
        }
    }
    pass(file, CheckKind::Placement, format!("{} rows, parts and sides equal the board's poses", rows.len()))
}

/// The lattice tile a point is in (a point on a tile line belongs to the tile above and to the right).
pub fn tile_at(lattice: FillLattice, p: P) -> (i32, i32) {
    let s = lattice.side();
    (p.x.div_euclid(s), p.y.div_euclid(s))
}

/// A seam: the read-back misses copper the model has on a tile edge.
#[derive(Clone, Debug, PartialEq)]
pub struct SeamFault {
    pub at: P,
    /// The lattice line (nm): `x = line` when `vertical`, else `y = line`.
    pub line: Nm,
    pub vertical: bool,
    /// The tiles on either side of the line.
    pub tiles: ((i32, i32), (i32, i32)),
}

/// The places where an edge of the read-back region lies parallel to a lattice line and within `tol`
/// of it, while the model has copper across the line there (on it and a quarter tolerance either side)
/// but the read-back lacks it at one of those points: a region cut at a tile edge (a slit the erosion
/// ignores, or a half missing). An edge that is a real boundary of the model (a board or pad edge)
/// never counts: the model has no copper beyond it. Cost: three containment tests per edge near a
/// lattice line, so O(edges near lines x vertices).
pub fn seam_faults(a: &PolySet, b: &PolySet, lattice: FillLattice, tol: Nm) -> Vec<SeamFault> {
    let s = lattice.side();
    let near = |c: Nm| -> Option<Nm> {
        let line = ((c as f64 / s as f64).round() as i64 * s as i64) as Nm;
        ((c as i64 - line as i64).abs() <= tol as i64).then_some(line)
    };
    // the model's copper must reach across the line (250 nm either side and on it) while the
    // read-back lacks it at one of those points: a slit of the width the erosion ignores, or a half
    let reach = (tol / 4).max(1);
    let mut out = Vec::new();
    for ring in a.rings() {
        for i in 0..ring.len() {
            let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
            let (vertical, c, lo, hi) = if p.x == q.x && p.y != q.y {
                (true, p.x, p.y.min(q.y), p.y.max(q.y))
            } else if p.y == q.y && p.x != q.x {
                (false, p.y, p.x.min(q.x), p.x.max(q.x))
            } else {
                continue;
            };
            if (hi as i64 - lo as i64) < 1000 {
                continue;
            }
            let Some(line) = near(c) else { continue };
            let mid = ((lo as i64 + hi as i64) / 2) as Nm;
            let pts: [P; 3] = if vertical {
                [P::new(line - reach, mid), P::new(line, mid), P::new(line + reach, mid)]
            } else {
                [P::new(mid, line - reach), P::new(mid, line), P::new(mid, line + reach)]
            };
            if pts.iter().all(|&pt| b.contains(pt)) && !pts.iter().all(|&pt| a.contains(pt)) {
                let at = pts[1];
                let t = |pt: P| tile_at(lattice, pt);
                out.push(SeamFault { at, line, vertical, tiles: (t(pts[0]), t(pts[2])) });
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "validate_tiles_tests.rs"]
mod tile_tests;
