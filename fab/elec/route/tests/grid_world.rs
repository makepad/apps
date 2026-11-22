//! Guards the world-backed grid as a cache of DRC legality: free neighbour steps keep the
//! board's clearances, other nets' pads stay fixed, and only solved, unlocked copper is rippable.

use fab_elec_board::{
    check_to, resolve_rules, Anchor, ConnItem, Copper, Flash, Item, Origin, RuleItem, RuleKey, Tent, Track,
    Via, ViaKind, ARC_ERR,
};
use fab_elec_circuit::NetKind;
use fab_elec_core::{clearance, mm, BBox, ItemId, LayerId, Path, Shape, P};
use fab_elec_fixtures::ledctl;
use fab_elec_route::grid::{Cell, Grid, GridSpec, ObstacleSource, Pass, Probe};
use fab_elec_route::{new_pieces, solvers, verify, Fixity, RBase, RItem, World, WorldSource};
use std::ops::ControlFlow;

#[test]
fn grid_steps_are_legal_on_the_fixture() {
    let design = ledctl();
    let board = design.units[0].board.as_deref().expect("grid fixture has a board");
    let rules = resolve_rules(board, None);
    let base = RBase::build(board);
    let world = World::new(&base);
    let source = WorldSource::new(&world, &rules, board, &|it: &RItem| it.fixity == Fixity::Solved && !it.locked);
    let pads: Vec<_> = board
        .all_copper()
        .into_iter()
        .filter(|c| matches!(c.item, ConnItem::Pad { .. }))
        .collect();
    let nets: Vec<_> = design.units[0]
        .circuit
        .nets
        .iter()
        .filter(|n| {
            n.kind == NetKind::Signal && n.name != "GND" && pads.iter().any(|pad| pad.net == Some(n.id))
        })
        .map(|n| n.id)
        .take(3)
        .collect();
    assert_eq!(
        nets.len(), 3,
        "signal-net fixture property, cell=(0, 0), layer=F_CU, nets={nets:?}: need three nets with pads"
    );
    let pad_box = pads
        .iter()
        .filter(|pad| pad.net == Some(nets[0]))
        .fold(BBox::EMPTY, |area, pad| area.union(pad.shape.bbox()));
    let centre = pad_box.center();
    let outline = board.outline_bbox();
    let origin = P::new(
        (centre.x - mm(5.0)).clamp(outline.min.x, outline.max.x - mm(10.0)),
        (centre.y - mm(5.0)).clamp(outline.min.y, outline.max.y - mm(10.0)),
    );
    let window = BBox::new(origin, origin.add(P::mm(10.0, 10.0)));
    let cases: Vec<_> = nets[..2]
        .iter()
        .map(|&net| {
            let class = board.nets[net.ix()].class;
            let w = rules.class(class).track.unwrap_or_else(|| rules.track_min(class));
            let key = RuleKey { class, item: RuleItem::Track };
            let c = rules.clearance(key, key, LayerId::F_CU);
            let spec = GridSpec::covering(
                window,
                GridSpec::pitch_for(w + c),
                vec![LayerId::F_CU, LayerId::B_CU],
            );
            (net, Probe { class, w, drill: None }, spec)
        })
        .collect();

    for (net, probe, spec) in &cases {
        let (net, probe) = (*net, *probe);
        let mut grid = Grid::new(spec.clone());
        let plane = grid.plane(probe);
        let reach = source.max_need(&probe);
        let index = |cell: Cell| (cell.y * spec.cols + cell.x) as usize;
        for (li, &layer) in spec.layers.iter().enumerate() {
            let mut values = Vec::with_capacity((spec.cols * spec.rows) as usize);
            for y in 0..spec.rows {
                for x in 0..spec.cols {
                    values.push(grid.state(&source, plane, li, Cell { x, y }));
                }
            }
            let mut checked = 0;
            for y in 0..spec.rows {
                for x in 0..spec.cols {
                    let a = Cell { x, y };
                    if !values[index(a)].free_for(net) {
                        continue;
                    }
                    // One orientation per undirected pair covers all eight neighbours.
                    for (dx, dy) in [(1, 0), (0, 1), (1, 1), (-1, 1)] {
                        let (bx, by) = (i64::from(x) + dx, i64::from(y) + dy);
                        if bx < 0 || bx >= i64::from(spec.cols) || by >= i64::from(spec.rows) {
                            continue;
                        }
                        let b = Cell { x: bx as u32, y: by as u32 };
                        if !values[index(b)].free_for(net) {
                            continue;
                        }
                        let step = Copper {
                            item: ConnItem::Track { item: ItemId(u32::MAX - 1), seg: 0 },
                            net: Some(net),
                            class: probe.class,
                            layer,
                            shape: Shape::Segment { a: spec.centre(a), b: spec.centre(b), w: probe.w },
                        };
                        world.query(layer, step.shape.bbox().inflate(reach), |_, it| {
                            assert_eq!(
                                check_to(&rules, &step, &it.obstacle(), layer), None,
                                "step legality, cells={a:?}->{b:?}, layer={layer:?}, net={net:?}, obstacle={it:?}"
                            );
                            ControlFlow::Continue(())
                        });
                        checked += 1;
                    }
                }
            }
            assert!(
                checked >= 1000,
                "step non-vacuity, cells={:?}, layer={layer:?}, net={net:?}: checked {checked} steps",
                spec.all()
            );
        }

        let mut checked_pads = 0;
        for pad in &pads {
            if pad.layer != LayerId::F_CU || pad.net.is_none() || pad.net == Some(net) {
                continue;
            }
            let at = pad.shape.bbox().center();
            if !window.contains(at) {
                continue;
            }
            let cell = spec.cell_at(at);
            assert_eq!(
                grid.state(&source, plane, 0, cell).pass(net), Pass::Blocked,
                "pads are fixed, cell={cell:?}, layer={:?}, net={net:?}, pad={:?}",
                pad.layer, pad.item
            );
            checked_pads += 1;
        }
        assert!(
            checked_pads > 0,
            "fixed-pad non-vacuity, cells={:?}, layer=F_CU, net={net:?}: no other-net pads in {window:?}",
            spec.all()
        );
    }

    let (net, probe, spec) = cases[0].clone();
    let layer = LayerId::B_CU;
    let mut free_grid = Grid::new(spec.clone());
    let plane = free_grid.plane(probe);
    let length = mm(3.0);
    // Include the cell beyond the end if 3 mm is not an exact multiple of the pitch.
    let span = ((length + spec.pitch - 1) / spec.pitch) as u32;
    let mut run = None;
    'find_run: for y in 0..spec.rows {
        for x in 0..spec.cols.saturating_sub(span) {
            let a = spec.centre(Cell { x, y });
            let b = a.add(P::new(length, 0));
            if !window.contains(a) || !window.contains(b) {
                continue;
            }
            if (x..=x + span).all(|x| {
                free_grid.state(&source, plane, 1, Cell { x, y }).pass(net) == Pass::Free
            }) {
                run = Some((a, b));
                break 'find_run;
            }
        }
    }
    let (a, b) = run.unwrap_or_else(|| {
        panic!(
            "free 3 mm run property, cells={:?}, layer={layer:?}, net={net:?}: no horizontal run in {window:?}",
            spec.all()
        )
    });
    let midpoint = spec.cell_at(BBox::new(a, b).center());
    let third = nets[2];
    let class = board.nets[third.ix()].class;
    let track = Item::Track(Track {
        id: ItemId(u32::MAX - 2),
        net: third,
        layer,
        w: rules.class(class).track.unwrap_or_else(|| rules.track_min(class)),
        widths: None,
        path: Path::new(a).line(b),
        ends: [Anchor::Free; 2],
        locked: false,
        origin: Origin::Solved { by: solvers::GRID },
    });
    let mut solved_world = world.clone();
    let mut authored_world = world.clone();
    for mut piece in new_pieces(board, &track) {
        solved_world.add(piece.clone());
        piece.fixity = Fixity::Authored;
        authored_world.add(piece);
    }
    for (branch, expected, property) in [
        (&solved_world, Pass::Rip(Some(third)), "solved copper is rippable"),
        (&authored_world, Pass::Blocked, "authored copper is fixed"),
    ] {
        let branch_source = WorldSource::new(branch, &rules, board, source.rippable);
        let mut grid = Grid::new(spec.clone());
        let plane = grid.plane(probe);
        assert_eq!(
            grid.state(&branch_source, plane, 1, midpoint).pass(net), expected,
            "{property}, cell={midpoint:?}, layer={layer:?}, net={net:?}, owner={third:?}"
        );
    }
}

#[test]
fn free_via_cells_are_legal_vias_drills_included() {
    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("grid fixture has a board").clone();
    board.rules.base.hole_to_hole = Some(mm(1.0));
    let rules = resolve_rules(&board, None);
    let net = design.units[0]
        .circuit
        .nets
        .iter()
        .find(|net| net.kind == NetKind::Signal)
        .expect("grid fixture has a signal net")
        .id;
    let class = board.nets[net.ix()].class;
    let size = rules.class(class).via.unwrap_or_else(|| rules.via(class));
    let probe = Probe { class, w: size.pad, drill: Some(size.drill) };
    let key = RuleKey { class, item: RuleItem::Via };
    let spec = GridSpec::covering(
        board.outline_bbox(),
        GridSpec::pitch_for(probe.w + rules.clearance(key, key, LayerId::F_CU)),
        board.stackup.copper(),
    );
    assert!(
        rules.hole_to_hole() + (size.drill + 1) / 2
            > rules.hole_clearance() + (probe.w + 1) / 2 + spec.slack(),
        "hole-to-hole must bind beyond the pad's clearance and grid slack"
    );
    let id = board.alloc_id();
    let outline = board.outline_polyset(ARC_ERR);
    let base = RBase::build(&board);
    let world = World::new(&base);
    let source = WorldSource::new(&world, &rules, &board, &|it: &RItem| it.fixity == Fixity::Solved && !it.locked);
    let mut grid = Grid::new(spec.clone());
    let plane = grid.plane(probe);
    let mut candidates: Vec<_> = (0..spec.cols * spec.rows).collect();
    let mut seed = 0x4d59_5df4_d0f3_3173u64;
    let mut checked = 0;
    // Sample without replacement, bounded by the finite grid even if no cells are free.
    while checked < 400 && !candidates.is_empty() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let index = candidates.swap_remove((seed % candidates.len() as u64) as usize);
        let cell = Cell { x: index % spec.cols, y: index / spec.cols };
        let at = spec.centre(cell);
        if !outline.contains(at)
            || !(0..spec.layers.len()).all(|li| grid.state(&source, plane, li, cell).free_for(net))
        {
            continue;
        }
        let via = Item::Via(Via {
            id,
            net,
            at,
            drill: probe.drill.expect("via probe has a drill"),
            pad: probe.w,
            span: (LayerId::F_CU, LayerId::B_CU),
            kind: ViaKind::Through,
            tent: Tent::Both,
            backdrill: None,
            locked: false,
            origin: Origin::Solved { by: solvers::GRID },
            flash: Flash::All,
        });
        let collisions = verify(&world, &rules, &board, &new_pieces(&board, &via));
        assert!(
            collisions.is_empty(),
            "free via cells are legal vias, drills included: cell={cell:?}, via={via:?}, collisions={collisions:?}"
        );
        checked += 1;
    }
    assert_eq!(checked, 400, "via legality must check 400 free cells inside the outline");
}

/// A via plane enforces each pad's drill-wall gap. Its own net may bring the ring closer than
/// copper clearance. A class with via-in-pad permission (filled and capped vias) drops the gap,
/// as DRC does, and may drill in its own net's pads only.
#[test]
fn via_cells_keep_the_drill_off_pads_and_only_the_drill() {
    let design = ledctl();
    let mut board = design.units[0].board.as_deref().expect("grid fixture has a board").clone();
    let rules = resolve_rules(&board, None);
    // Force one pad's need beyond the ordinary tile search bound.
    let resistor = board.placed.iter_mut().find(|p| p.path == "lamps[0].r").expect("the fixture has the LED resistor");
    let resistor_id = resistor.id;
    let pad = resistor.footprint.pad("1").expect("the LED resistor has pad 1");
    let wide_pad = ConnItem::Pad { placed: resistor_id, pad };
    std::sync::Arc::make_mut(&mut resistor.footprint).pads[pad.ix()].mask_margin = Some(mm(0.6));

    let base = RBase::build(&board);
    let world = World::new(&base);
    let rippable = |it: &RItem| it.fixity == Fixity::Solved && !it.locked;
    let source = WorldSource::new(&world, &rules, &board, &rippable);
    let pads: Vec<_> = board.all_copper().into_iter().filter_map(|c| {
        let ConnItem::Pad { placed, pad } = c.item else { return None };
        Some((c, rules.via_drill_to_board_pad(&board, placed, pad)))
    }).collect();

    let mut allowed = board.clone();
    for class in &mut allowed.classes {
        class.rules.via_in_pad = Some(true);
    }
    let allowed_rules = resolve_rules(&allowed, None);
    let allowed_source = WorldSource::new(&world, &allowed_rules, &allowed, &rippable);
    // A fine pitch samples the band between the drill limit and the usual ring-to-pad clearance.
    let spec = GridSpec::covering(board.outline_bbox(), mm(0.05), board.stackup.copper());
    let mut grid = Grid::new(spec.clone());
    let mut allowed_grid = Grid::new(spec.clone());
    let mut inside_web = 0;
    let mut inside_override = 0;
    let mut close_ring = 0;
    let mut drill_on_own_pad = 0;
    // Compare the same cells with and without permission, so other obstacles cannot explain a difference.
    let sample = pads.iter().filter(|(p, _)| p.net.is_some() && p.item.item() != resistor_id).take(16)
        .chain(pads.iter().filter(|(p, _)| p.item.item() == resistor_id));
    for (pad, need) in sample {
        let Some(net) = pad.net else { continue };
        let class = board.nets[net.ix()].class;
        let size = rules.via(class);
        let probe = Probe { class, w: size.pad, drill: Some(size.drill) };
        let plane = grid.plane(probe);
        let allowed_plane = allowed_grid.plane(probe);
        let copper_gap = rules.clearance(RuleKey { class, item: RuleItem::Via }, pad.key(), pad.layer);
        let reach = size.pad.max(need.saturating_add(size.drill / 2));
        let Some(cells) = spec.cells_in(pad.shape.bbox().inflate(reach)) else { continue };
        for y in cells.min.y..=cells.max.y {
            for x in cells.min.x..=cells.max.x {
                let cell = Cell { x, y };
                let free = (0..spec.layers.len()).all(|li| grid.state(&source, plane, li, cell).free_for(net));
                let allowed_free = (0..spec.layers.len()).all(|li| {
                    allowed_grid.state(&allowed_source, allowed_plane, li, cell).free_for(net)
                });
                assert!(!free || allowed_free, "permission preserves a free via cell: cell={cell:?}, net={net:?}");
                if !allowed_free {
                    continue;
                }
                let at = spec.centre(cell);
                let drill = Shape::Circle { c: at, r: size.drill / 2 };
                let mut clears_pads = true;
                for (other, other_need) in &pads {
                    let wall = clearance(&drill, &other.shape);
                    if wall == 0 {
                        assert_eq!(other.net, Some(net),
                            "permission lets the drill touch only its own net's pads: cell={cell:?}, pad={:?}", other.item);
                    }
                    if wall < *other_need {
                        clears_pads = false;
                    }
                }
                assert_eq!(free, clears_pads,
                    "an otherwise free cell is blocked exactly inside a pad's drill-wall need: cell={cell:?}, net={net:?}");
                let gap = clearance(&drill, &pad.shape);
                if gap == 0 {
                    drill_on_own_pad += 1;
                } else if gap < *need {
                    inside_web += 1;
                    if pad.item == wide_pad && gap > rules.max_clearance() {
                        inside_override += 1;
                    }
                }
                let ring = Shape::Circle { c: at, r: size.pad / 2 };
                if free && clearance(&ring, &pad.shape) < copper_gap {
                    close_ring += 1;
                }
            }
        }
    }
    assert!(inside_web > 0, "a drill off its own pad but inside its mask-safe gap is blocked without permission");
    assert!(inside_override > 0, "the pad override blocks otherwise free via cells beyond the ordinary search bound");
    assert!(close_ring > 0, "a free drill at or beyond the need can have its ring within copper clearance of its own pad");
    assert!(drill_on_own_pad > 0, "allowing vias in pads frees cells whose drills touch their own net's pads");
}
