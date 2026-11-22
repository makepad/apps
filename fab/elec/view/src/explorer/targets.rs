//! What the pointer aims at for an edit gesture, resolved from the scene's hit lists.

use super::scene::{HitTable, PinHit};
use fab_elec_core::{CompId, NetId, PinIx};
use fab_elec_explore::edit::{Target, Toward};
use fab_elec_explore::sheet::{to_local, SheetLayout};

pub const PICK: f64 = 0.6; // grid steps
pub const HOLD: f64 = 0.2; // hysteresis, grid steps

/// Resolve `at` (world) to a target. `prev` is the last result: a target the point is still within
/// PICK + HOLD of is kept, and `toward` changes only past its zone edge plus HOLD.
/// `px_per_unit`: screen pixels per grid step (when a pin's line or its wire end is under 8 px
/// on screen, the pin wins). Pure; O(pins + tags + wire segments), then parts/frames for canvas.
pub(super) fn resolve(hits: &HitTable, sheet: &SheetLayout, at: [f64; 2], px_per_unit: f64, prev: &Target) -> Target {
    if let Some(target) = kept(hits, sheet, at, px_per_unit, prev) { return target; }

    let mut nearest = PICK;
    let mut hit = None;
    for pin in &hits.pins {
        // The two capsules overlap at the outer end. At usable zoom the graphic outside
        // that end owns fresh picks; holding a pin above still keeps its full capsule.
        let dir = outward(pin);
        if px_per_unit >= 8.0 && pin.net.is_some() && !pin.nc && !pin.open_end
            && (at[0] - pin.outer[0]) * dir[0] + (at[1] - pin.outer[1]) * dir[1] > 0.0
        { continue; }
        let distance = distance(at, closest(at, pin.inner, pin.outer));
        if distance <= PICK && (hit.is_none() || distance < nearest) {
            nearest = distance;
            hit = Some(pin_target(pin, at, None));
        }
    }
    if let Some(hit) = hit { return hit; }

    if px_per_unit >= 8.0 {
        nearest = PICK;
        for pin in &hits.pins {
            let Some(net) = pin.net.filter(|_| !pin.nc && !pin.open_end) else { continue };
            let distance = distance(at, closest(at, pin.outer, end(pin)));
            if distance <= PICK && (hit.is_none() || distance < nearest) {
                nearest = distance;
                hit = Some(Target::WireEnd { comp: CompId(pin.node), pin: PinIx(pin.pin as u16), net: NetId(net) });
            }
        }
        if let Some(hit) = hit { return hit; }
    }

    // Attachment distance breaks ties between overlapping boxes without depending on text size.
    nearest = f64::INFINITY;
    for tag in &hits.tags {
        if contains(tag.bx, at, PICK * 0.5) {
            let distance = distance(at, tag.at);
            if hit.is_none() || distance < nearest {
                nearest = distance;
                hit = Some(Target::Tag { net: NetId(tag.net), kind: tag.kind });
            }
        }
    }
    if let Some(hit) = hit { return hit; }

    nearest = PICK;
    let mut wire_hit = None;
    for wire in &hits.wires {
        let Some(run) = wire.run.filter(|_| wire.net.is_some()) else { continue };
        let point = closest(at, wire.a, wire.b);
        let distance = distance(at, point);
        if distance <= PICK && (wire_hit.is_none() || distance < nearest) {
            nearest = distance;
            wire_hit = Some((wire, run, point));
        }
    }
    if let Some((wire, index, point)) = wire_hit {
        if let Some(run) = sheet.wires.get(index as usize) {
            // A live piece may span several collinear segments. Its start index bounds
            // this one walk, so snapping and the semantic segment still agree.
            if let Some((seg, _)) = run.pts.windows(2).enumerate().skip(wire.seg as usize).find(|(_, pair)| {
                distance(point, closest(point, pair[0].map(f64::from), pair[1].map(f64::from))) <= 1e-9
            }) {
                return Target::Wire { net: NetId(run.net), run: index, seg: seg as u32, at: snap(point) };
            }
        }
    }

    if sheet.parts.iter().any(|part| part.placed && contains(part.body.map(f64::from), at, 0.0)) {
        return Target::None;
    }
    let block = canvas_block(sheet, at);
    let world = snap(at);
    Target::Canvas { block, cell: to_local(sheet, block, world).unwrap_or([world[0], -world[1]]) }
}

/// Find the smallest placed frame containing the world point, falling back to the root. O(frames).
pub(crate) fn canvas_block(sheet: &SheetLayout, at: [f64; 2]) -> u32 {
    sheet.frames.iter().enumerate().filter(|(_, frame)| {
        frame.placed && contains(frame.rect.map(f64::from), at, 0.0)
    }).min_by_key(|(_, frame)| {
        let [x0, y0, x1, y1] = frame.rect.map(i64::from);
        (x1 - x0) * (y1 - y0)
    }).map_or(0, |(block, _)| block as u32)
}

fn kept(hits: &HitTable, sheet: &SheetLayout, at: [f64; 2], px_per_unit: f64, prev: &Target) -> Option<Target> {
    let radius = PICK + HOLD;
    match *prev {
        Target::Pin { comp, pin, toward } => {
            let pin = hits.pins.iter().find(|p| p.node == comp.0 && p.pin == u32::from(pin.0))?;
            (distance(at, closest(at, pin.inner, pin.outer)) <= radius).then(|| pin_target(pin, at, Some(toward)))
        }
        Target::WireEnd { comp, pin, net } if px_per_unit >= 8.0 => {
            let pin = hits.pins.iter().find(|p| p.node == comp.0 && p.pin == u32::from(pin.0)
                && p.net == Some(net.0) && !p.nc)?;
            (distance(at, closest(at, pin.outer, end(pin))) <= radius).then_some(*prev)
        }
        Target::Tag { net, kind } => hits.tags.iter().any(|tag| {
            let point = [at[0].clamp(tag.bx[0], tag.bx[2]), at[1].clamp(tag.bx[1], tag.bx[3])];
            tag.net == net.0 && tag.kind == kind && distance(at, point) <= radius
        }).then_some(*prev),
        Target::Wire { net, run, seg, .. } => {
            let route = sheet.wires.get(run as usize).filter(|route| route.net == net.0)?;
            if !hits.wires.iter().any(|wire| wire.run == Some(run) && wire.net == Some(net.0)) { return None; }
            let a = route.pts.get(seg as usize)?.map(f64::from);
            let b = route.pts.get(seg as usize + 1)?.map(f64::from);
            let point = closest(at, a, b);
            (distance(at, point) <= radius).then(|| Target::Wire { net, run, seg, at: snap(point) })
        }
        _ => None,
    }
}

fn pin_target(pin: &PinHit, at: [f64; 2], prev: Option<Toward>) -> Target {
    let dy = at[1] - pin.outer[1];
    let edge = if prev == Some(Toward::Across) { 0.5 + HOLD } else { 0.5 };
    let toward = match prev {
        Some(Toward::Up) if dy > 0.5 - HOLD => Toward::Up,
        Some(Toward::Down) if dy < -0.5 + HOLD => Toward::Down,
        _ if dy > edge => Toward::Up,
        _ if dy < -edge => Toward::Down,
        _ => Toward::Across,
    };
    Target::Pin { comp: CompId(pin.node), pin: PinIx(pin.pin as u16), toward }
}

fn outward(pin: &PinHit) -> [f64; 2] {
    [pin.outer[0] - pin.inner[0], pin.outer[1] - pin.inner[1]]
}

fn end(pin: &PinHit) -> [f64; 2] {
    let dir = outward(pin);
    [pin.outer[0] + dir[0], pin.outer[1] + dir[1]]
}

fn closest(at: [f64; 2], a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let d = [b[0] - a[0], b[1] - a[1]];
    let len2 = d[0] * d[0] + d[1] * d[1];
    let t = if len2 > 0.0 {
        (((at[0] - a[0]) * d[0] + (at[1] - a[1]) * d[1]) / len2).clamp(0.0, 1.0)
    } else { 0.0 };
    [a[0] + t * d[0], a[1] + t * d[1]]
}

fn distance(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn contains(b: [f64; 4], at: [f64; 2], radius: f64) -> bool {
    at[0] >= b[0] - radius && at[0] <= b[2] + radius && at[1] >= b[1] - radius && at[1] <= b[3] + radius
}

fn snap(at: [f64; 2]) -> [i32; 2] {
    at.map(|v| v.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Tokens;
    use crate::explorer::liveix::{sheet_trees, SceneLive};
    use crate::explorer::scene::{Scene, SceneState};
    use fab_elec_circuit::SheetHints;
    use fab_elec_explore::sheet::{Run, TagKind};
    use fab_elec_explore::{layout, rows::SheetRows, Opts};

    #[test]
    fn ledctl_edit_targets_and_hysteresis() {
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let tokens = Tokens::for_theme(true);
        let scene = Scene::build(&design, 0, &sheet, &rows, tokens, &SceneState::default(), &SceneLive::default());
        let pick = |at, prev: &Target| resolve(&scene.hits, &sheet, at, 16.0, prev);
        let mcu = circuit.comps.iter().find(|comp| comp.refdes == "U2")
            .expect("targets: the fixture has U2");
        let pin = scene.hits.pins.iter().find(|pin| pin.node == mcu.id.0
            && mcu.part.pins[pin.pin as usize].name == "PA13")
            .expect("targets: U2's PA13 has a pin hit");
        let target = |toward| Target::Pin { comp: mcu.id, pin: PinIx(pin.pin as u16), toward };
        let across = target(Toward::Across);
        let midpoint = [(pin.inner[0] + pin.outer[0]) * 0.5, (pin.inner[1] + pin.outer[1]) * 0.5];
        assert_eq!(pick(midpoint, &Target::None), across,
            "targets: a point on a pin's line finds that pin");
        let dir = outward(pin);
        let outside = [pin.outer[0] + 0.5 * dir[0], pin.outer[1] + 0.5 * dir[1]];
        let wire_end = Target::WireEnd { comp: mcu.id, pin: PinIx(pin.pin as u16),
            net: NetId(pin.net.expect("targets: PA13 is connected")) };
        assert_eq!(pick(outside, &Target::None), wire_end,
            "targets: just outside a connected pin's end is its wire end");
        let mut open = (*scene.hits).clone();
        open.pins.iter_mut().filter(|hit| hit.node == pin.node && hit.pin == pin.pin).for_each(|hit| hit.open_end = true);
        assert_eq!(resolve(&open, &sheet, outside, 16.0, &Target::None), across,
            "targets: an open end (an unnamed net on this pin only) has no wire end: its tip is the pin");
        let nc = scene.hits.pins.iter().find(|pin| pin.node == mcu.id.0 && pin.nc)
            .expect("targets: U2 has an nc pin");
        let nc_dir = outward(nc);
        assert!(!matches!(pick([nc.outer[0] + 0.5 * nc_dir[0], nc.outer[1] + 0.5 * nc_dir[1]], &Target::None), Target::WireEnd { .. }),
            "targets: an nc pin has no wire end");
        assert_eq!(resolve(&scene.hits, &sheet, outside, 4.0, &Target::None), across,
            "targets: at small zoom the pin wins over the wire end");
        assert_eq!(resolve(&scene.hits, &sheet, outside, 4.0, &wire_end), across,
            "targets: zooming out releases a held wire end to the pin");
        assert_eq!(pick(midpoint, &wire_end), wire_end,
            "targets: a kept wire end survives a move across the pin boundary");

        for (dy, prev, expected) in [
            (0.6, Toward::Across, Toward::Across),
            (0.75, Toward::Across, Toward::Up),
            (0.4, Toward::Up, Toward::Up),
            (0.25, Toward::Up, Toward::Across),
            (-0.6, Toward::Across, Toward::Across),
            (-0.75, Toward::Across, Toward::Down),
            (-0.4, Toward::Down, Toward::Down),
            (-0.25, Toward::Down, Toward::Across),
        ] {
            assert_eq!(pick([pin.outer[0], pin.outer[1] + dy], &target(prev)), target(expected),
                "targets: toward follows the pointer with hysteresis (dy={dy}, prev={prev:?})");
        }
        let held_at = [midpoint[0] - dir[1] * (PICK + 0.1), midpoint[1] + dir[0] * (PICK + 0.1)];
        assert!(matches!(pick(held_at, &across), Target::Pin { comp, pin: found, .. }
            if comp == mcu.id && found == PinIx(pin.pin as u16)),
            "targets: a kept target survives a small move");
        assert_eq!(pick(sheet.parts[mcu.id.ix()].at.map(f64::from), &Target::None), Target::None,
            "targets: a part body is not a drop target");
        for (node, _) in sheet.parts.iter().enumerate().filter(|(_, part)| part.placed) {
            assert_eq!(scene.hits.pins.iter().filter(|pin| pin.node == node as u32).count(), circuit.comps[node].part.pins.len(),
                "targets: every placed part includes all its pins, including glyphs and nc pins");
        }

        let clear_of_pins = |at| scene.hits.pins.iter().all(|pin| {
            distance(at, closest(at, pin.inner, pin.outer)) > PICK
                && (pin.net.is_none() || pin.nc || distance(at, closest(at, pin.outer, end(pin))) > PICK)
        });
        let (tag, tag_at) = scene.hits.tags.iter().filter(|tag| tag.kind == TagKind::Power
            && circuit.nets[tag.net as usize].name == "3V3").find_map(|tag| {
            let [x0, y0, x1, y1] = tag.bx;
            [[x0 + 0.05, y0 + 0.05], [x1 - 0.05, y1 - 0.05],
                [x0 + 0.05, y1 - 0.05], [x1 - 0.05, y0 + 0.05]]
                .into_iter().find(|&at| clear_of_pins(at) && scene.hits.tags.iter().all(|other| {
                    (other.net == tag.net && other.kind == tag.kind) || !contains(other.bx, at, PICK * 0.5)
                })).map(|at| (tag, at))
        }).expect("targets: a 3V3 power tag has box space clear of pins and wire ends");
        let tag_target = Target::Tag { net: NetId(tag.net), kind: TagKind::Power };
        assert_eq!(pick(tag_at, &Target::None), tag_target, "targets: a tag box finds its net");
        assert_eq!(pick([tag.bx[2] + PICK + 0.1, (tag.bx[1] + tag.bx[3]) * 0.5], &tag_target), tag_target,
            "targets: a kept tag survives a move outside its fresh pick box");

        let clear_of_tags = |at| scene.hits.tags.iter().all(|tag| !contains(tag.bx, at, PICK * 0.5));
        let (run, seg, wire_at) = sheet.wires.iter().enumerate().find_map(|(run, route)| {
            route.pts.windows(2).enumerate().find_map(|(seg, pair)| {
                let a = pair[0].map(f64::from);
                let b = pair[1].map(f64::from);
                let at = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
                (distance(a, b) > 2.0 && clear_of_pins(at) && clear_of_tags(at)
                    && scene.hits.wires.iter().all(|wire| wire.run.is_none()
                        || (wire.run == Some(run as u32) && wire.seg == seg as u32)
                        || distance(at, closest(at, wire.a, wire.b)) > PICK))
                    .then_some((run, seg, at))
            })
        }).expect("targets: a sheet wire has a midpoint clear of pins, tags and other wires");
        let wire_target = Target::Wire { net: NetId(sheet.wires[run].net), run: run as u32, seg: seg as u32, at: snap(wire_at) };
        assert_eq!(pick(wire_at, &Target::None), wire_target,
            "targets: a wire's middle finds the run and segment");
        let [a, b] = [sheet.wires[run].pts[seg], sheet.wires[run].pts[seg + 1]];
        assert!(contains([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])].map(f64::from), snap(wire_at).map(f64::from), 0.0),
            "targets: the snapped wire point remains on its segment");
        let normal = if a[0] == b[0] { [PICK + 0.1, 0.0] } else { [0.0, PICK + 0.1] };
        assert_eq!(pick([wire_at[0] + normal[0], wire_at[1] + normal[1]], &wire_target), wire_target,
            "targets: a kept wire survives a small move off its segment");

        let block = circuit.tree.iter().position(|block| block.path == "mcu")
            .expect("targets: the fixture has an mcu block") as u32;
        let frame = &sheet.frames[block as usize];
        let empty = (frame.rect[0] + 1..frame.rect[2]).flat_map(|x| {
            (frame.rect[1] + 1..frame.rect[3]).map(move |y| [x as f64 + 0.25, y as f64 + 0.25])
        }).find(|&at| clear_of_pins(at) && clear_of_tags(at)
            && sheet.parts.iter().all(|part| !part.placed || !contains(part.body.map(f64::from), at, 0.0))
            && scene.hits.wires.iter().all(|wire| distance(at, closest(at, wire.a, wire.b)) > PICK))
            .expect("targets: mcu's frame has an empty canvas cell");
        assert_eq!(pick(empty, &Target::None), Target::Canvas { block,
            cell: to_local(&sheet, block, snap(empty)).expect("targets: mcu has a placed local origin") },
            "targets: empty canvas inside mcu's frame is a Canvas cell of mcu");
        let outside = [sheet.bounds[2] as f64 + 10.25, sheet.bounds[3] as f64 + 10.75];
        assert_eq!(pick(outside, &across), Target::Canvas { block: 0,
            cell: to_local(&sheet, 0, snap(outside)).expect("targets: the root has a local origin") },
            "targets: outside all frames releases the held pin and uses the root's local grid");

        // Reuse the one layout: real live pieces and a remote test run exercise sub-spans,
        // a piece spanning collinear segments, and the start of the segment after a corner.
        let trees = sheet_trees(&design, 0, &sheet);
        let mut live = SceneLive { runs: trees.pieces.runs.iter().map(|pieces| {
            pieces.iter().map(|piece| (piece.a, piece.b, -1.0)).collect()
        }).collect(), ..SceneLive::default() };
        let mut with_run = sheet.clone();
        let x = sheet.bounds[2] + 20;
        let net = sheet.wires[run].net;
        let extra = with_run.wires.len() as u32;
        with_run.wires.push(Run { net, pts: vec![[x, 0], [x + 4, 0], [x + 8, 0], [x + 8, 4]] });
        live.runs.push(vec![([x, 0], [x + 2, 0], -1.0), ([x + 2, 0], [x + 8, 0], -1.0), ([x + 8, 0], [x + 8, 4], -1.0)]);
        let live_scene = Scene::build(&design, 0, &with_run, &rows, tokens, &SceneState::default(), &live);
        assert_eq!(resolve(&live_scene.hits, &with_run, wire_at, 16.0, &Target::None), wire_target,
            "targets: live pieces preserve a wire's sheet run and segment");
        assert_eq!(live_scene.hits.wires.iter().filter(|wire| wire.run == Some(extra)).map(|wire| wire.seg).collect::<Vec<_>>(), vec![0, 0, 2],
            "targets: live pieces record the sheet segment containing their start");
        for (at, seg, cell) in [([x as f64 + 1.2, 0.0], 0, [x + 1, 0]),
            ([x as f64 + 6.2, 0.0], 1, [x + 6, 0]), ([x as f64 + 8.0, 2.2], 2, [x + 8, 2])]
        {
            assert_eq!(resolve(&live_scene.hits, &with_run, at, 16.0, &Target::None),
                Target::Wire { net: NetId(net), run: extra, seg, at: cell },
                "targets: a live piece resolves and snaps to the source segment under the pointer");
        }
    }
}
