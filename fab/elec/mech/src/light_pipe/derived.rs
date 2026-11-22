//! The board features required by a built light pipe.

use super::{led_package, LegStyle, PipeBuild, PipeParams};
use fab_elec_board::{BoardDelta, BoardKeepout, Cutout, Hole, Item, Origin, Placed};
use fab_elec_core::{mm, BBox, BoardId, Contour, LayerId, LayerSet, PolySet, Shape, P};
use fab_elec_design::{ops::DesignOpError, Design};
use fab_elec_parts::{footprint::PAD_POLY_ERR, Forbid};

/// Board features in the LED footprint's frame, using the build's fitted dimensions.
/// The pipe's footprint keeps other parts' bodies out from the LED's top to the panel, so the
/// LED under it is spared. That keepout sits on the LED side's copper layer, which is how DRC
/// knows a board keepout's side; its band starts above the surface, so it never catches copper. Stake-head discs use conservative polygons at the board's 1 um curve
/// tolerance.
pub fn board_items(p: &PipeParams, b: &PipeBuild) -> Vec<Item> {
    let report = &b.report;
    let mut items = Vec::new();
    let extent = P::new(mm(report.extent_x), mm(report.extent_y));
    items.push(Item::Keepout(BoardKeepout {
        id: Default::default(),
        area: PolySet::rect(BBox::new(P::ZERO.sub(extent), extent)),
        layers: LayerSet::of(&[LayerId::F_CU]),
        forbid: Forbid::FOOTPRINTS,
        reason: "light pipe body".into(),
        origin: Origin::default(),
        z: Some((mm(report.led_top), mm(p.led_to_panel))),
    }));
    if report.pcb_holes {
        let style = p.leg_style();
        let half_spacing = mm(report.hole_spacing / 2.0);
        let drill = mm(p.pcb_hole);
        let layers = LayerSet::of(&[LayerId::B_CU, LayerId::B_MASK, LayerId::B_SILK, LayerId::B_CRTYD]);
        for x in [-half_spacing, half_spacing] {
            let at = P::new(x, 0);
            let hook = matches!(style, LegStyle::HookOut | LegStyle::HookIn | LegStyle::SplitPin)
                || (style == LegStyle::HookAndPeg && x > 0);
            items.push(Item::Hole(Hole {
                id: Default::default(),
                name: if hook { "light pipe leg" } else { "light pipe peg" }.into(),
                at,
                drill,
                pad: None,
                net: None,
                plated: false,
                origin: Origin::default(),
            }));
            let keepout = if hook {
                let half = P::new(
                    mm(p.pcb_hole / 2.0 + report.hook_overhang_built),
                    mm(p.pcb_hole.max(report.leg_width_built) / 2.0),
                );
                Some((PolySet::rect(BBox::new(at.sub(half), at.add(half))), "light pipe hook catch"))
            } else if style == LegStyle::HeatStake {
                let r = mm(report.stake_head_diameter / 2.0);
                Some((Shape::Circle { c: at, r }.to_polyset(PAD_POLY_ERR), "heat stake head"))
            } else {
                None
            };
            if let Some((area, reason)) = keepout {
                items.push(Item::Keepout(BoardKeepout {
                    id: Default::default(), area, layers, forbid: Forbid::ALL,
                    reason: reason.into(), origin: Origin::default(), z: None,
                }));
            }
        }
    }
    let led = led_package(p);
    if led.reverse {
        let half = P::new(mm((led.w + 0.2) / 2.0), mm((led.d + 0.2) / 2.0));
        items.push(Item::Cutout(Cutout {
            id: Default::default(),
            name: "light pipe LED cut-out".into(),
            contour: Contour::rect(P::new(-half.x, -half.y), half, 0),
            origin: Origin::default(),
        }));
    }
    items
}

/// Publish the pipe's board features at its LED placement, replacing its previous set.
pub fn publish_pipe(design: &mut Design, unit: BoardId, part: &str, led: &Placed, p: &PipeParams, b: &PipeBuild) -> Result<Option<BoardDelta>, DesignOpError> {
    crate::derived::publish(design, unit, part, crate::derived::place(board_items(p, b), led))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::light_pipe::{build, LedKind, Mount};
    use fab_elec_board::zone::fill_all;
    use fab_elec_board::{connect, drc, resolve_rules, Board};
    use fab_elec_circuit::CheckKind;
    use fab_elec_core::{Angle, FeatureRef, ItemId, Nm, Sel, Side};

    fn fixture() -> (Design, BoardId, Placed) {
        let design = fab_elec_fixtures::ledctl();
        let unit = BoardId(0);
        let board = design.unit(unit).expect("fixture unit exists").board.as_ref().expect("fixture has a board");
        let led = board.placed.iter().find(|placed| placed.refdes == "D1").expect("fixture has LED D1").clone();
        (design, unit, led)
    }

    fn holes(items: &[Item]) -> Vec<&Hole> {
        items.iter().filter_map(|item| if let Item::Hole(hole) = item { Some(hole) } else { None }).collect()
    }

    /// The far-side keepouts (hook catches and stake heads), without the pipe's body keepout.
    fn keepouts(items: &[Item]) -> Vec<&BoardKeepout> {
        items.iter().filter_map(|item| if let Item::Keepout(keepout) = item { Some(keepout) } else { None })
            .filter(|keepout| keepout.reason != "light pipe body").collect()
    }

    fn body_keepout(items: &[Item]) -> &BoardKeepout {
        let mut body = items.iter().filter_map(|item| match item {
            Item::Keepout(keepout) if keepout.reason == "light pipe body" => Some(keepout),
            _ => None,
        });
        let keepout = body.next().expect("the pipe emits its body keepout");
        assert!(body.next().is_none(), "the pipe emits one body keepout");
        keepout
    }

    #[test]
    fn pipe_set_follows_d1_and_replaces_in_place() {
        let (mut design, unit, led) = fixture();
        let from = FeatureRef::Generated("pipe".into());
        let mut p = PipeParams::default();
        let b = build(&p);
        assert!(board_items(&p, &b).iter().all(|item| item.id() == ItemId::default()), "generated items leave ids for set_derived");
        assert!(publish_pipe(&mut design, unit, "pipe", &led, &p, &b).expect("default pipe publishes").is_some(), "first publication changes the board");
        let items = design.derived(unit, &from);
        assert_eq!(items.len(), 5, "default hook legs emit two holes, two catch keepouts and the body keepout, with no cutout");
        assert!(items.iter().all(|item| item.origin() == &Origin::Derived { from: from.clone() }), "every board feature belongs to the generated pipe");
        let half_spacing = mm(b.report.hole_spacing / 2.0);
        assert_eq!(led.rot, Angle::deg(-90.0), "D1 exercises a rotated footprint");
        let expected = [led.at.add(P::new(0, half_spacing)), led.at.sub(P::new(0, half_spacing))];
        let hs = holes(&items);
        assert_eq!(hs.len(), 2, "default pipe has two leg holes");
        for (hole, at) in hs.iter().zip(expected) {
            assert_eq!(hole.at, at, "leg hole follows D1's rotated x axis and position");
            assert_eq!(hole.drill, mm(p.pcb_hole), "leg hole uses the pipe's PCB drill");
            assert_eq!(hole.name, "light pipe leg", "snap holes are named as legs");
            assert!(!hole.plated && hole.pad.is_none() && hole.net.is_none(), "leg holes are non-plated without copper pads or nets");
        }
        let ks = keepouts(&items);
        assert_eq!(ks.len(), 2, "default pipe has two catch keepouts");
        let half = P::new(
            mm(p.pcb_hole / 2.0 + b.report.hook_overhang_built),
            mm(p.pcb_hole.max(b.report.leg_width_built) / 2.0),
        );
        let rotated_half = P::new(half.y, half.x);
        for (keepout, at) in ks.iter().zip(expected) {
            assert_eq!(keepout.area, PolySet::rect(BBox::new(at.sub(rotated_half), at.add(rotated_half))), "catch rectangle uses fitted dimensions and follows D1's rotation");
            assert_eq!(keepout.layers, LayerSet::of(&[LayerId::B_CU, LayerId::B_MASK, LayerId::B_SILK, LayerId::B_CRTYD]), "catch keeps bottom copper, mask, silk and courtyard clear");
            assert_eq!(keepout.forbid, Forbid::ALL, "catch forbids every board item class");
            assert_eq!(keepout.reason, "light pipe hook catch", "catch explains its purpose");
        }
        let body = body_keepout(&items);
        let extent = P::new(mm(b.report.extent_x), mm(b.report.extent_y));
        assert_eq!(body.area, PolySet::rect(BBox::new(led.at.sub(P::new(extent.y, extent.x)), led.at.add(P::new(extent.y, extent.x)))), "body keepout covers the pipe's footprint, turned with D1");
        assert_eq!((body.layers, body.forbid), (LayerSet::of(&[LayerId::F_CU]), Forbid::FOOTPRINTS), "body keepout keeps other parts' bodies off the LED's side");
        let (lo, hi) = body.z.expect("body keepout has a height band");
        assert_eq!((lo, hi), (mm(b.report.led_top), mm(p.led_to_panel)), "body band runs from the LED's top to the panel");
        let (_, led_top) = led.body_z.expect("D1 is placed with its body's band");
        assert!(led_top <= lo, "the band spares D1 under the pipe: D1 reaches {led_top} nm, the band starts at {lo} nm");
        assert!(keepouts(&items).iter().all(|k| k.z.is_none()), "far-side catches forbid every height");
        let generation = design.generation;
        assert_eq!(publish_pipe(&mut design, unit, "pipe", &led, &p, &b), Ok(None), "publishing the same pipe is a no-op");
        assert_eq!(design.generation, generation, "equal publication does not advance the design generation");

        let ids: Vec<_> = hs.iter().map(|hole| hole.id).collect();
        p.pcb_hole = 1.8;
        let b = build(&p);
        assert!(publish_pipe(&mut design, unit, "pipe", &led, &p, &b).expect("changed drill publishes").is_some(), "changing the drill replaces the derived features");
        let changed = design.derived(unit, &from);
        let hs = holes(&changed);
        assert_eq!(hs.iter().map(|hole| hole.id).collect::<Vec<_>>(), ids, "paired holes keep their ids after a drill change");
        assert!(hs.iter().all(|hole| hole.drill == mm(p.pcb_hole)), "both replacement holes use the changed drill");
        p.mount = Mount::PanelBack.index();
        let b = build(&p);
        assert!(!b.report.pcb_holes, "panel mounting needs no PCB holes");
        let delta = publish_pipe(&mut design, unit, "pipe", &led, &p, &b).expect("panel mounting publishes").expect("panel mounting removes the old set");
        assert_eq!(delta.removed.len(), 4, "panel mounting removes both holes and both catches");
        let left = design.derived(unit, &from);
        assert_eq!(left.len(), 1, "panel mounting leaves only the body keepout");
        assert_eq!(body_keepout(&left).area.bbox().width(), 2 * mm(b.report.extent_y) as i64, "the body keepout follows the panel-mounted pipe's footprint");
    }

    #[test]
    fn fixing_styles_and_reverse_led_publish_the_required_shapes() {
        let (mut design, unit, led) = fixture();
        let from = FeatureRef::Generated("pipe".into());
        let mut p = PipeParams { segments: 32, ..PipeParams::default() };
        for style in [LegStyle::HookIn, LegStyle::SplitPin, LegStyle::HookAndPeg, LegStyle::PressPeg, LegStyle::LocatingPeg, LegStyle::HeatStake] {
            p.leg_style = style.index();
            let b = build(&p);
            publish_pipe(&mut design, unit, "pipe", &led, &p, &b).expect("each PCB fixing style publishes");
            let items = design.derived(unit, &from);
            let hs = holes(&items);
            let ks = keepouts(&items);
            assert_eq!(hs.len(), 2, "{style:?}: PCB fixing has two holes");
            let names = match style {
                LegStyle::HookIn | LegStyle::SplitPin => ["light pipe leg", "light pipe leg"],
                LegStyle::HookAndPeg => ["light pipe peg", "light pipe leg"],
                _ => ["light pipe peg", "light pipe peg"],
            };
            assert_eq!(hs.iter().map(|hole| hole.name.as_str()).collect::<Vec<_>>(), names, "{style:?}: hole names distinguish pegs and legs in x order");
            let count = match style {
                LegStyle::HookIn | LegStyle::SplitPin | LegStyle::HeatStake => 2,
                LegStyle::HookAndPeg => 1,
                _ => 0,
            };
            assert_eq!(ks.len(), count, "{style:?}: only hooks and stake heads need far-side keepouts");
            assert_eq!(items.len(), 3 + count, "{style:?}: no extra board features are emitted");
            body_keepout(&items);
            if style == LegStyle::HookAndPeg {
                assert_eq!(ks[0].area.bbox().center(), hs[1].at, "mixed fixing has a catch only at its positive-x leg");
            }
            if style == LegStyle::HeatStake {
                let r = mm(b.report.stake_head_diameter / 2.0);
                assert!(r > 0, "the heat stake report has a formed head diameter");
                for (keepout, hole) in ks.iter().zip(&hs) {
                    assert_eq!(keepout.reason, "heat stake head", "stake keepouts name the formed heads");
                    assert_eq!(keepout.forbid, Forbid::ALL, "stake heads forbid every board item class");
                    assert_eq!(keepout.layers, LayerSet::of(&[LayerId::B_CU, LayerId::B_MASK, LayerId::B_SILK, LayerId::B_CRTYD]), "stake heads clear the four bottom layers");
                    assert!(keepout.area.rings().flatten().all(|point| {
                        let radius = (point.dist2(hole.at) as f64).sqrt();
                        radius >= r as f64 && radius <= (r + PAD_POLY_ERR + 2) as f64
                    }), "stake head vertices follow the reported diameter within the curve tolerance");
                    let bounds = keepout.area.bbox();
                    for diameter in [bounds.width(), bounds.height()] {
                        assert!(diameter >= 2 * r as i64 && diameter <= 2 * (r + PAD_POLY_ERR + 2) as i64, "stake head disc spans the reported diameter within the curve tolerance");
                    }
                }
            }
        }

        p.led_type = LedKind::Rgb3228ReverseMount.index();
        p.leg_style = LegStyle::None.index();
        let b = build(&p);
        publish_pipe(&mut design, unit, "pipe", &led, &p, &b).expect("reverse LED publishes its cutout");
        let items = design.derived(unit, &from);
        assert_eq!(items.len(), 2, "reverse LED without legs emits its body keepout and its cutout");
        body_keepout(&items);
        let Some(cutout) = items.iter().find_map(|item| if let Item::Cutout(cutout) = item { Some(cutout) } else { None }) else { panic!("reverse LED feature is a cutout") };
        let package = led_package(&p);
        let bounds = cutout.contour.bbox();
        assert_eq!(cutout.name, "light pipe LED cut-out", "reverse LED cutout names its purpose");
        assert_eq!(bounds.center(), led.at, "reverse LED cutout is centred on D1");
        assert_eq!((bounds.width(), bounds.height()), (mm(package.d + 0.2) as i64, mm(package.w + 0.2) as i64), "reverse LED cutout adds 0.2 mm to each package dimension and rotates with D1");
    }

    #[test]
    fn bottom_placement_mirrors_geometry_and_swaps_far_side_layers() {
        let (_, _, mut led) = fixture();
        led.side = Side::Bottom;
        led.rot = Angle::deg(37.0);
        let p = PipeParams { led_type: LedKind::Rgb3228ReverseMount.index(), leg_style: LegStyle::HookAndPeg.index(), segments: 32, ..PipeParams::default() };
        let b = build(&p);
        let mut local = board_items(&p, &b);
        local.push(Item::Placed(led.clone()));
        let placed = crate::derived::place(local.clone(), &led);
        assert_eq!(placed.len(), local.len(), "placement preserves the item count");
        for (before, after) in local.iter().zip(&placed) {
            assert_eq!(after.id(), before.id(), "placement preserves item ids");
            assert_eq!(after.origin(), before.origin(), "placement leaves ownership to publication");
            match (before, after) {
                (Item::Hole(a), Item::Hole(b)) => {
                    let expected = P::new(-a.at.x, a.at.y).rotated(led.rot).add(led.at);
                    assert_eq!(b.at, expected, "bottom hole mirrors x before rotation and translation");
                    assert_eq!(b.drill, a.drill, "placement keeps the drill diameter");
                }
                (Item::Keepout(a), Item::Keepout(b)) => {
                    assert_eq!(Shape::Poly(b.area.clone()), led.shape_to_board(&Shape::Poly(a.area.clone())), "bottom keepout follows footprint shape mapping including winding");
                    if a.reason == "light pipe body" {
                        assert_eq!(b.layers, LayerSet::of(&[LayerId::B_CU]), "bottom LED puts its pipe's body keepout on the back side");
                        assert_eq!(b.z, a.z, "the height band is measured from the LED's own side, so it is unchanged");
                    } else {
                        assert_eq!(b.layers, LayerSet::of(&[LayerId::F_CU, LayerId::F_MASK, LayerId::F_SILK, LayerId::F_CRTYD]), "bottom LED puts its catch on the front copper, mask, silk and courtyard");
                    }
                }
                (Item::Cutout(a), Item::Cutout(b)) => {
                    assert_eq!(b.contour.path, a.contour.path.mapped(|point| led.to_board(point)), "bottom cutout maps every contour point through the footprint placement");
                }
                (Item::Placed(_), Item::Placed(_)) => assert_eq!(after, before, "unsupported item kinds pass through unchanged"),
                _ => panic!("placement preserves each item kind"),
            }
        }
    }

    #[test]
    fn drc_flags_a_tall_part_under_the_pipe_and_spares_its_led() {
        let (mut design, unit, led) = fixture();
        let p = PipeParams { segments: 32, ..PipeParams::default() };
        let b = build(&p);
        publish_pipe(&mut design, unit, "pipe", &led, &p, &b).expect("default pipe publishes");
        let body = body_keepout(&design.derived(unit, &FeatureRef::Generated("pipe".into()))).clone();
        let (lo, hi) = body.z.expect("body keepout has a height band");
        let mut board: Board = design.unit(unit).expect("fixture unit exists").board.as_deref().expect("fixture has a board").clone();
        let breaches = |board: &Board| {
            let rules = resolve_rules(board, None);
            let fills = fill_all(board, &rules);
            let conn = connect(board, &fills);
            drc(board, &conn, &rules, &fills, None).violations.into_iter()
                .filter(|v| v.kind == CheckKind::Keepout && v.items.contains(&Sel::Item(unit, body.id)))
                .collect::<Vec<_>>()
        };
        assert!(led.body_z.is_some_and(|(_, top)| top <= lo), "D1 is shorter than the band's start");
        assert!(breaches(&board).is_empty(), "the pipe's LED under its own keepout is no breach");

        let other = board.placed.iter().position(|q| q.id != led.id && q.side == led.side && !q.footprint.courtyard.is_empty())
            .expect("the fixture has another part on D1's side");
        let tall = board.placed[other].id;
        board.placed[other].at = led.at;
        board.placed[other].body_z = Some((0, mm(0.6)));
        assert!(breaches(&board).is_empty(), "a part below the band's start under the pipe is no breach");
        board.placed[other].body_z = Some((0, mm(2.0)));
        let found = breaches(&board);
        assert_eq!(found.len(), 1, "a part taller than the LED under the pipe is flagged once");
        assert!(found[0].items.contains(&Sel::Item(unit, tall)), "the finding names the tall part");
        let band = |v: Nm| format!("{:.3}", v as f64 / 1e6);
        assert_eq!(found[0].message, format!("body 0.000-2.000 mm high reaches into the keepout's {}-{} mm band", band(lo), band(hi)), "the finding names both bands");
    }
}
