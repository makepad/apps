//! The rules' check against prints that keep them and prints that break each one.

use crate::grid::PixelGrid;
use crate::image::LayerStack;
use crate::load::Load;
use crate::parts::{cured, LayerParts};
use crate::points::PointSettings;
use crate::printer::simulated;
use crate::estimate::resin_ml;
use crate::rules::{check, checked, Break, Level, Rule};
use crate::section::{Lifted, MeshSection, Section};
use crate::supports::{supported, SupportSettings, Supported};
use crate::tests::{field, prism, rect};
use makepad_csg_sdf::SdfNode;
use std::sync::Arc;

fn on_supports(parts: Vec<Arc<dyn Section>>, s: &SupportSettings) -> Supported {
    let p = simulated();
    supported(parts, PixelGrid::of(&p.screen), p.layer_height, s, &PointSettings::default(), &Load::default())
}

/// The rules' breaks in `stack`, its first `parts` sections the parts.
fn breaks(stack: &LayerStack, parts: usize, supports: &crate::supports::Supports) -> Vec<Break> {
    let alone = LayerStack { sections: stack.sections[..parts].to_vec(), ..stack.clone() };
    let layers: Vec<LayerParts> = cured(&alone);
    check(stack, parts, supports, &layers, &|| false)
}

/// The breaks the print fails on.
fn errors(found: Vec<Break>) -> Vec<Break> {
    found.into_iter().filter(|b| b.level == Level::Error).collect()
}

fn cube() -> Arc<dyn Section> {
    Arc::new(field(SdfNode::Box { size: [10.0, 4.0, 10.0], radius: 0.0 }, [30.0, 30.0, 2.0], ([24.9, 24.9, -0.1], [35.1, 35.1, 4.1])))
}

/// A cube on its supports keeps every rule, from a field and a mesh alike.
#[test]
fn a_supported_cube_keeps_the_rules() {
    let mesh: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([25.0, 25.0], [35.0, 35.0]), None, 0.0, 4.0)));
    for part in [cube(), mesh] {
        let on = on_supports(vec![part], &SupportSettings::default());
        let found = errors(check(&on.stack, 1, &on.supports, &on.cured, &|| false));
        assert!(found.is_empty(), "{found:?}");
    }
}

/// A cube's resin is its volume: its layers' covered area a layer thick.
#[test]
fn a_cubes_resin_is_its_volume() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let ml = resin_ml(on.covered, &on.stack.grid, on.stack.layers.height);
    assert!((ml - 0.4).abs() < 0.002, "{ml} ml for 400 mm³");
}

/// The supports' own resin is what they light where the part doesn't cure, pixel by pixel: the
/// tips sunk into the part are the part's.
#[test]
fn the_supports_own_resin_leaves_out_the_part() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let own = checked(&on.stack, 1, &on.supports, &on.cured, &|| false).support_covered;
    let sup = LayerStack { sections: on.stack.sections[1..].to_vec(), ..on.stack.clone() };
    let (mut all, mut outside) = (0u64, 0u64);
    for k in 0..on.stack.layers.count {
        let spans = sup.layer(k).spans;
        let r = spans.rect;
        for j in r.y0..r.y1 {
            for i in r.x0..r.x1 {
                let s = spans.at(i, j) as u64;
                all += s;
                if !on.cured[k as usize].cured(i, j) {
                    outside += s;
                }
            }
        }
    }
    let (all, outside) = (all as f64 / 255.0, outside as f64 / 255.0);
    assert!((own - outside).abs() < 1e-6 * all, "own {own}, pixel by pixel {outside}");
    assert!(own > 0.0 && own < all, "own {own} of the {all} the supports light");
}

/// The parts' cured layers with the supports' cut over them are the whole stack's layers: the
/// check need not cut the parts again.
#[test]
fn the_parts_layers_and_the_supports_join_to_the_stacks() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let sup = LayerStack { sections: on.stack.sections[1..].to_vec(), ..on.stack.clone() };
    for k in (0..on.stack.layers.count).step_by(7) {
        let whole = LayerParts::of(&on.stack.layer(k).spans);
        let joined = on.cured[k as usize].union(&LayerParts::of(&sup.layer(k).spans));
        assert_eq!(joined.parts, whole.parts, "layer {k}");
    }
}

/// The same cube lifted with nothing under it cures onto the film: its first layer, 7 mm up,
/// is connected to nothing.
#[test]
fn a_cube_on_nothing_is_not_connected() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let s = on.supports.settings;
    let alone: Arc<dyn Section> = Arc::new(Lifted { section: cube(), lift: s.lift() });
    let p = simulated();
    let stack = LayerStack::new(vec![alone], &p);
    let breaks = errors(breaks(&stack, 1, &crate::supports::Supports { pillars: Vec::new(), bridges: Vec::new(), heads: Vec::new(), solids: Vec::new(), pads: Vec::new(), ..(*on.supports).clone() }));
    assert_eq!(breaks.len(), 1, "{breaks:?}");
    assert_eq!(breaks[0].rule, Rule::Connected);
    assert_eq!(breaks[0].layer, (s.lift() / p.layer_height).round() as u32, "its first layer");
    assert_eq!(breaks[0].part, Some(0));
}

/// A slab slid under the cube after its supports were built, raised for pillars that stand
/// tall: every pillar goes through it.
#[test]
fn a_pillar_through_a_part_is_not_clear() {
    let on = on_supports(vec![cube()], &SupportSettings { elevation: 15.0, ..SupportSettings::default() });
    let slab: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([20.0, 20.0], [40.0, 40.0]), None, 3.5, 4.5)));
    let mut parts = on.stack.sections.clone();
    parts.insert(1, slab);
    let stack = LayerStack { sections: parts, ..on.stack.clone() };
    let breaks = breaks(&stack, 2, &on.supports);
    let through = breaks.iter().filter(|b| b.rule == Rule::Clear).count();
    assert_eq!(through, on.supports.pillars.len(), "{breaks:?}");
    assert!(breaks.iter().filter(|b| b.rule == Rule::Clear).all(|b| b.at[2] > 3.4 && b.at[2] < 4.6 + on.supports.settings.pillar_radius && b.part == Some(1)), "{breaks:?}");
}

/// Supports with no pad: every pillar's base hangs off nothing.
#[test]
fn a_pillar_off_the_pad_is_not_hung() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let bare = crate::supports::Supports { pads: Vec::new(), ..(*on.supports).clone() };
    let breaks: Vec<_> = check(&on.stack, 1, &bare, &on.cured, &|| false).into_iter().filter(|b| b.rule == Rule::Hung).collect();
    assert_eq!(breaks.len(), on.supports.pillars.len(), "{breaks:?}");
}

/// Supports thinner than the peel they carry: the cube's tips and pillars shrunk after they were
/// sized, each named on a layer of the cube, where what it holds peels.
#[test]
fn a_thin_support_is_not_strong() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let mut thin = (*on.supports).clone();
    thin.heads.iter_mut().for_each(|h| h.tip_radius = 0.05);
    thin.pillars.iter_mut().for_each(|p| p.radius = 0.05);
    let found = check(&on.stack, 1, &thin, &on.cured, &|| false);
    let weak: Vec<_> = found.iter().filter(|b| b.rule == Rule::Strong).collect();
    assert_eq!(weak.len(), thin.held() + thin.pillars.len(), "{found:?}");
    let cube = on.stack.layers.at(on.supports.settings.lift() + 0.01)..=on.stack.layers.at(on.supports.settings.lift() + 3.99);
    assert!(weak.iter().all(|b| cube.contains(&b.layer) && b.part == Some(0)), "{weak:?}");
}

/// A cap 25 mm up on a stem: the pillars under it stand tall enough that the push back down
/// would buckle the settings' 1 mm, so they are built wider (R7 §3.11.4); what can't widen
/// clear of the stem is named.
#[test]
fn tall_pillars_are_built_wide_enough_to_stand() {
    let at = |y: f64, size: [f64; 3]| SdfNode::Transform { at: [0.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) };
    let tee = SdfNode::Combine { op: makepad_csg_sdf::SdfOp::Union, blend: 0.0, of: vec![at(13.0, [6.0, 26.0, 6.0]), at(27.0, [30.0, 2.0, 30.0])] };
    let part: Arc<dyn Section> = Arc::new(field(tee, [50.0, 40.0, 0.0], ([34.9, 24.9, -0.1], [65.1, 55.1, 28.1])));
    let s = SupportSettings::default();
    let on = on_supports(vec![part], &s);
    let tall: Vec<_> = on.supports.pillars.iter().filter(|p| p.top[2] - s.foot(p.top)[2] > 20.0).collect();
    let wide = tall.iter().filter(|p| p.radius > s.pillar_radius).count();
    let found = errors(check(&on.stack, 1, &on.supports, &on.cured, &|| false));
    println!("tee: {} pillars, {} tall, {wide} built wider; {} breaks: {found:?}", on.supports.pillars.len(), tall.len(), found.len());
    assert!(!tall.is_empty() && wide >= tall.len() / 2, "{wide} of {} tall pillars wider", tall.len());
    assert!(found.iter().all(|b| b.rule == Rule::Strong && b.what.contains("pillar")), "{found:?}");
}

/// A box with a pocket `side` mm square and 12 mm deep in it, opening toward the film (`up`) or
/// toward the plate, on its supports.
fn pocket(side: f64, up: bool) -> Supported {
    let at = |y: f64, size: [f64; 3]| SdfNode::Transform { at: [0.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) };
    let outer = side + 10.0;
    let hole = at(if up { 9.5 } else { 5.5 }, [side, 13.0, side]);
    let part = SdfNode::Combine { op: makepad_csg_sdf::SdfOp::Difference, blend: 0.0, of: vec![at(7.5, [outer, 15.0, outer]), hole] };
    let h = 0.5 * outer + 0.1;
    on_supports(vec![Arc::new(field(part, [60.0, 40.0, 0.0], ([60.0 - h, 40.0 - h, -0.1], [60.0 + h, 40.0 + h, 15.1])))], &SupportSettings::default())
}

/// A pocket that opens toward the film seals against it from its floor to the box's top: a
/// warning at 20 mm square, an error at 25. Opening toward the plate, it is vented by the open
/// space under it and seals nothing.
#[test]
fn a_pocket_open_to_the_film_is_a_cup() {
    for (side, level) in [(20.0, Level::Warning), (25.0, Level::Error)] {
        let on = pocket(side, true);
        let found = check(&on.stack, 1, &on.supports, &on.cured, &|| false);
        let cups: Vec<_> = found.iter().filter(|b| b.rule == Rule::Cup).collect();
        println!("pocket {side} mm: {cups:?}");
        assert_eq!(cups.len(), 1, "{found:?}");
        let (c, lift) = (cups[0], on.supports.settings.lift());
        let layers = &on.stack.layers;
        // layer k is cut half a layer over its bottom: the first cut above the floor, the last
        // under the top
        let h = 0.5 * layers.height;
        assert_eq!((c.level, c.layer, c.to), (level, layers.at(lift + 3.0 + h), Some(layers.at(lift + 15.0 - h))), "{c:?}");
        let area: f64 = c.what.split("seals ").nth(1).and_then(|w| w.split(' ').next()).and_then(|a| a.parse().ok()).expect("its area");
        assert!((area * 100.0 - side * side).abs() < 0.1 * side * side, "{} cm² for {side} mm square", area);
        assert!(errors(found).iter().all(|b| b.rule == Rule::Cup), "the structure keeps its rules");
    }
    let down = pocket(20.0, false);
    let found = check(&down.stack, 1, &down.supports, &down.cured, &|| false);
    assert!(found.iter().all(|b| b.rule != Rule::Cup), "{found:?}");
}

/// A closed hollow traps the resin it holds once the layers close over it.
#[test]
fn a_closed_hollow_traps_resin() {
    let at = |y: f64, size: [f64; 3]| SdfNode::Transform { at: [0.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) };
    let part = SdfNode::Combine { op: makepad_csg_sdf::SdfOp::Difference, blend: 0.0, of: vec![at(5.0, [20.0, 10.0, 20.0]), at(5.0, [16.0, 6.0, 16.0])] };
    let on = on_supports(vec![Arc::new(field(part, [60.0, 40.0, 0.0], ([49.9, 29.9, -0.1], [70.1, 50.1, 10.1])))], &SupportSettings::default());
    let found = check(&on.stack, 1, &on.supports, &on.cured, &|| false);
    let cups: Vec<_> = found.iter().filter(|b| b.rule == Rule::Cup).collect();
    assert_eq!(cups.len(), 1, "{found:?}");
    let ml: f64 = cups[0].what.split("trapping ").nth(1).and_then(|w| w.split(' ').next()).and_then(|a| a.parse().ok()).expect("what it traps");
    assert!((ml - 1.536).abs() < 0.1, "{ml} ml in a 16 x 16 x 6 mm hollow: {:?}", cups[0]);
    let layers = &on.stack.layers;
    assert_eq!((cups[0].level, cups[0].to), (Level::Warning, Some(layers.at(on.supports.settings.lift() + 8.0 - 0.5 * layers.height))), "{:?}", cups[0]);
}

/// A fin 0.02 mm thick on a block is lit at 40 % in the one column of pixels it lies in: too
/// dim to cure, so it is lost on each of its layers, as one feature. At 0.06 mm it cures.
#[test]
fn a_fin_under_half_a_pixel_is_lost() {
    let block: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([25.0, 25.0], [35.0, 35.0]), None, 0.0, 4.0)));
    for (width, lost) in [(0.02, true), (0.06, false)] {
        let fin: Arc<dyn Section> = Arc::new(MeshSection::new(&prism(&rect([30.01, 26.0], [30.01 + width, 34.0]), None, 4.0, 7.0)));
        let on = on_supports(vec![block.clone(), fin], &SupportSettings::default());
        let found = check(&on.stack, 2, &on.supports, &on.cured, &|| false);
        let gone: Vec<_> = found.iter().filter(|b| b.rule == Rule::Lost).collect();
        if !lost {
            assert!(gone.is_empty(), "{gone:?}");
            continue;
        }
        assert_eq!(gone.len(), 1, "{found:?}");
        let (layers, lift) = (&on.stack.layers, on.supports.settings.lift());
        let g = gone[0];
        let h = 0.5 * layers.height;
        assert_eq!((g.level, g.layer, g.to, g.part), (Level::Info, layers.at(lift + 4.0 + h), Some(layers.at(lift + 7.0 - h)), Some(1)), "{g:?}");
        assert!((g.at[0] - 30.025).abs() < 0.05 && (g.size[1] - 8.0).abs() < 0.11, "{g:?}");
    }
}

/// An 80 x 50 mm slab hung on nothing: it peels 42 % of the screen at once, its first layer
/// jumps from nothing, and the plate holds none of it. A 60 x 40 mm one peels 25 %.
#[test]
fn a_wide_slab_peels_too_much_at_once() {
    let on = on_supports(vec![cube()], &SupportSettings::default());
    let none = crate::supports::Supports { pillars: Vec::new(), bridges: Vec::new(), heads: Vec::new(), solids: Vec::new(), pads: Vec::new(), ..(*on.supports).clone() };
    let p = simulated();
    let lift = on.supports.settings.lift();
    for (hi, wide) in [([100.0, 65.0], true), ([80.0, 55.0], false)] {
        let slab: Arc<dyn Section> = Arc::new(Lifted { section: Arc::new(MeshSection::new(&prism(&rect([20.0, 15.0], hi), None, 0.0, 2.0))), lift });
        let stack = LayerStack::new(vec![slab], &p);
        let found = breaks(&stack, 1, &none);
        let first = stack.layers.at(lift + 0.5 * stack.layers.height);
        let of = |rule: Rule| found.iter().filter(|b| b.rule == rule).map(|b| (b.level, b.layer)).collect::<Vec<_>>();
        assert_eq!(of(Rule::Jump), vec![(Level::Info, first)], "{found:?}");
        assert_eq!(of(Rule::Bond), vec![(Level::Warning, 0)], "{found:?}");
        assert_eq!(of(Rule::Peel).len(), wide as usize, "{found:?}");
        assert!(of(Rule::Peel).iter().all(|&(level, layer)| level == Level::Warning && layer >= first), "{found:?}");
    }
}

/// Islands are what the layer image cures: beside a block standing on the plate, three
/// features start 1 mm up. A pixel meeting the block only at a corner cures onto it, and a speck
/// under half a pixel cures nothing, so neither is an island; a pixel standing apart cures onto
/// the film, an island on its first layer.
#[test]
fn an_island_is_a_pixel_cured_apart() {
    let p = simulated();
    let square = |lo: [f64; 2], side: f64, z0: f64| Arc::new(MeshSection::new(&prism(&rect(lo, [lo[0] + side, lo[1] + side]), None, z0, 1.5))) as Arc<dyn Section>;
    let pixel = p.screen.pitch[0];
    assert_eq!(p.screen.pitch, [pixel; 2], "square pixels");
    let block = square([10.0, 10.0], 1.0, 0.0);
    let corner = square([11.0, 11.0], pixel, 1.0);
    let apart = square([12.0, 10.0], pixel, 1.0);
    let speck = square([13.0 + 0.3 * pixel, 10.0 + 0.3 * pixel], 0.4 * pixel, 1.0);
    let parts = vec![block, corner, apart, speck];
    let stack = LayerStack::new(parts.clone(), &p);
    let layers = cured(&stack);
    let none = crate::supports::Supports::new(&[], &parts, &SupportSettings::default(), &Load::default(), &layers, &stack.grid, &stack.layers);
    let islands: Vec<(u32, Option<usize>, String)> = breaks(&stack, 4, &none).into_iter().filter(|b| b.rule == Rule::Connected).map(|b| (b.layer, b.part, b.what)).collect();
    let first = (1.0 / p.layer_height).round() as u32;
    assert_eq!(islands, vec![(first, Some(2), "a 1-pixel island cures onto the film".to_string())]);
}
