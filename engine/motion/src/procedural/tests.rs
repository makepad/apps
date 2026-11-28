use crate::plain::PathProps;
use super::bounce::Bouncer;
use super::ease_graph::parse_curve;
use super::particles::{particle, Flow};
use super::*;
use crate::MotionDoc;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|diags| panic!("{}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

/// A one-Pattern document's drawn layer, as text.
fn pattern_layer(kind: &str, seed: u32) -> String {
    let doc = load(&format!("use mod.motion.*; Motion{{size: vec2(640, 360) duration: 1.0 layers: [Pattern{{kind: @{kind} seed: {seed} cell: 40 size: vec2(640, 360)}}]}}"));
    format!("{:?}", doc.all_layers()[0].kind)
}

#[test]
fn every_pattern_kind_loads_and_seeds_its_tiles() {
    for kind in pattern_kinds() {
        let a = pattern_layer(kind, 1);
        assert!(a.contains("Inline"), "{kind}: drawn by its pixel function");
        let seeded = kind.starts_with("truchet") || matches!(kind, "voronoi" | "scales" | "leds");
        assert_eq!(a != pattern_layer(kind, 2), seeded, "{kind}: the seed turns the tiles of the seeded kinds only");
    }
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Pattern{kind: @stripez}]}"#).is_err());
}

#[test]
fn truchet_patterns_load() {
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [
        Pattern{kind: @truchet seed: 7 color: #1f3cff color2: #f0ebe0 cell: 60 size: vec2(640, 360)}
        Pattern{kind: @truchet_hex seed: 3 cell: 40 size: vec2(640, 360)}
    ]}"#);
    assert_eq!(doc.eval(0.5).hash(), doc.eval(0.5).hash());
    let other = load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [
        Pattern{kind: @truchet seed: 8 color: #1f3cff color2: #f0ebe0 cell: 60 size: vec2(640, 360)}
        Pattern{kind: @truchet_hex seed: 3 cell: 40 size: vec2(640, 360)}
    ]}"#);
    assert_ne!(format!("{:?}", doc.all_layers()[0].kind), format!("{:?}", other.all_layers()[0].kind), "seed reaches the shader");
}

/// `parse_curve` of the Splash value `source` evaluates to.
fn curve(source: &str) -> Result<super::EaseCurve, String> {
    let mut host = makepad_script::ScriptVmHost::new((), ());
    let mut vm = makepad_script::ScriptVm {
        host: host.host_context(),
        bx: Box::new(makepad_script::ScriptVmBase::new()),
    };
    let v = vm.eval(makepad_script::ScriptMod::new(
        "",
        "",
        "curve",
        0,
        0,
        &source,
        &[],
    ));
    parse_curve(&vm, Some(v))
}

#[test]
fn ease_curve_samples() {
    let c = curve("\"cubic-bezier(0.70, 0.00, 0.30, 1.00)\"").unwrap();
    assert_eq!(c.value(0.0), 0.0);
    assert_eq!(c.value(1.0), 1.0);
    // Symmetric about the middle.
    assert!((c.value(0.5) - 0.5).abs() < 1e-6, "{}", c.value(0.5));
    for i in 0..20 {
        let x = i as f64 / 20.0;
        assert!((c.value(x) + c.value(1.0 - x) - 1.0).abs() < 1e-5, "symmetry at {x}");
        assert!(c.value(x + 0.05) >= c.value(x), "monotonic at {x}");
    }
    // Slow at the ends, fast in the middle: the spacing row's dots crowd the ends.
    assert!(c.value(0.1) < 0.02 && c.value(0.9) > 0.98);
    // The curve's own points agree with the solver.
    for s in [0.2, 0.4, 0.6, 0.8] {
        let [x, y] = c.point(s);
        assert!((c.value(x) - y).abs() < 1e-4, "at s {s}: {} vs {y}", c.value(x));
    }
    assert_eq!(c.css(), "cubic-bezier(0.70, 0.00, 0.30, 1.00)");
    // Names and arrays.
    let named = curve("@ease_in_out_cubic").unwrap();
    assert_eq!(named.points, [0.645, 0.045, 0.355, 1.0]);
    let css = curve("\"ease-in-out\"").unwrap();
    assert_eq!(css.points, [0.42, 0.0, 0.58, 1.0]);
    assert!(curve("\"cubic-bezier(1.5, 0, 0.3, 1)\"").is_err());
}

#[test]
fn ease_graph_loads_and_is_deterministic() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 layers: [
        EaseGraph{curve: "cubic-bezier(0.7, 0, 0.3, 1)" at: vec2(160, 60) size: vec2(960, 600) fill_at: 1.6}
    ]}"#;
    let (a, b) = (load(src), load(src));
    for t in [0.0, 0.3, 0.9, 1.5, 1.9] {
        assert_eq!(a.eval(t).hash(), b.eval(t).hash(), "t {t}");
    }
    assert_ne!(a.eval(0.5).hash(), a.eval(1.0).hash());
}

#[test]
fn bounce_contacts_and_squash() {
    let b = Bouncer::new(300.0, 3, 0.6, 2.0, 800.0);
    let contacts = b.contacts();
    assert_eq!(contacts.len(), 4);
    assert!((contacts[3] - 2.0).abs() < 1e-9, "the hops fill the duration: {contacts:?}");
    for c in &contacts {
        let s = b.state(*c);
        assert!(s.height.abs() < 1e-6, "on the line at contact");
        assert!(s.squash > 0.0, "squashed at contact");
    }
    let mid = b.state((contacts[0] + contacts[1]) * 0.5);
    assert_eq!(mid.squash, 0.0);
    assert!(mid.height > 0.0 && mid.height < 300.0 * 0.6 * 0.6 + 1.0, "the first hop peaks at e² of the drop: {}", mid.height);
    assert_eq!(b.state(0.0).height, 300.0);
}

#[test]
fn onion_and_bounce_load() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 layers: [
        Bounce{at: vec2(140, 200) size: vec2(1000, 420) labels: true onion: {count: 6 spacing: 0.04 outline: true}}
        Ellipse{size: vec2(40, 40) fill: #ff0000 x: tween(0, 800, 0, 2) onion: 4}
    ]}"#;
    let doc = load(src);
    assert_eq!(doc.eval(1.0).hash(), load(src).eval(1.0).hash());
}

#[test]
fn particles_are_a_function_of_time() {
    let flow = Flow { converge: 0.9, release: 1.5, centre: [640.0, 360.0], swirl: 300.0, wind: [-4000.0, -200.0] };
    let target = [700.0, 380.0];
    for t in [0.0, 0.4, 0.95, 1.2, 1.7, 2.3] {
        assert_eq!(particle(&flow, 3, 17, target, 0.4, t), particle(&flow, 3, 17, target, 0.4, t));
    }
    // Settled on its target between arriving and the release.
    let (p, o) = particle(&flow, 3, 17, target, 0.4, 1.2);
    assert!((p[0] - target[0]).abs() < 2.0 && (p[1] - target[1]).abs() < 2.0, "{p:?}");
    assert_eq!(o, 1.0);
    // Blown downwind (left) and gone after the release.
    let (late, o) = particle(&flow, 3, 17, target, 0.4, 2.75);
    assert!(late[0] < target[0] - 100.0 && o == 0.0, "{late:?} {o}");
}

#[test]
fn glyph_points_fill_the_letters() {
    let a = glyph_points("flow", "Inter", 800.0, false, 300.0, 800, 5).unwrap();
    assert_eq!(a.len(), 800);
    assert_eq!(a, glyph_points("flow", "Inter", 800.0, false, 300.0, 800, 5).unwrap());
    assert_ne!(a, glyph_points("flow", "Inter", 800.0, false, 300.0, 800, 6).unwrap());
    // Centred, within the set size.
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in &a {
        for i in 0..2 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    assert!(hi[1] - lo[1] < 300.0 && hi[0] - lo[0] > 300.0, "{lo:?} {hi:?}");
    assert!((lo[0] + hi[0]).abs() < 20.0 && (lo[1] + hi[1]).abs() < 20.0, "centred: {lo:?} {hi:?}");
}

#[test]
fn particle_text_loads() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 3.0 layers: [ParticleText{text: "flow" count: 400 seed: 2}]}"#;
    let doc = load(src);
    let again = load(src);
    for t in [0.2, 1.1, 2.0] {
        assert_eq!(doc.eval(t).hash(), again.eval(t).hash(), "t {t}");
    }
}

#[test]
fn shaders_and_slices_load() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 2.0 bpm: 120 layers: [
        GridFloor{color: #ff5a1f horizon: 0.45 speed: 3}
        Metaballs{background: #0d1433 caption: true k: tween(0.2, 0.5, 0, 2)}
        Group{slices: {dir: @cycle count: 10 amp: 90} layers: [Pattern{kind: @truchet_fill size: vec2(1280, 720) cell: 80}]}
        Text{text: "MOVE" size: 200 at: vec2(640, 360) align: @center slices: {dir: @horizontal on: 0.5 amp: tween(0, 72, 0, 1) edge: @clear}}
    ]}"#;
    let doc = load(src);
    assert_eq!(doc.eval(0.75).hash(), load(src).eval(0.75).hash());
    // Both slices run the render kit's effect, `amp` as a fraction of the height.
    let frame = doc.eval(0.5);
    let slices: Vec<_> = frame.walk().into_iter().filter_map(|(_, item)| item.effects.iter().find(|e| e.name == "slices").cloned()).collect();
    assert_eq!(slices.len(), 2);
    let amp = |e: &crate::EffectFrame| e.params.iter().find(|(k, _)| k == "amp").map(|(_, v)| format!("{v:?}")).unwrap();
    assert_eq!(amp(&slices[0]), format!("{:?}", crate::EffectValue::Num(90.0 / 720.0)));
    assert_eq!(amp(&slices[1]), format!("{:?}", crate::EffectValue::Num(0.1 * 0.5)));
    let bad = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Group{slices: {dir: @sideways} layers: []}]}"#);
    assert!(bad.is_err());
}

#[test]
fn every_example_loads() {
    for kind in LAYER_TYPES {
        let example = example(kind).unwrap_or_else(|| panic!("{kind}: no example"));
        load(example);
        // Plot items draw in their plot's data coordinates, a Shape's box is
        // its children's outline. Camera uses the document viewport and an
        // origin pivot: these types have no box of their own.
        if !super::plot::ITEMS.contains(kind) && *kind != "Shape" && *kind != "Camera" {
            assert!(crate::layers::fields(kind).iter().any(|f| f == "size"), "{kind}");
        }
    }
}

#[test]
fn branches_are_seeded_and_grow_outward() {
    use super::branches::{depth_start, grow, Growth};
    let g = Growth { from: [500.0, 900.0], angle: -90.0, length: 200.0, depth: 6, spread: 26.0, shrink: 0.74, seed: 3, loom: false };
    let a = grow(&g);
    assert_eq!(a, grow(&g));
    assert_ne!(a, grow(&Growth { seed: 4, ..g }));
    assert!(a.len() >= 63, "at least a binary tree of depth 6: {}", a.len());
    assert_eq!(a[0].a, [500.0, 900.0]);
    // Every branch starts where its parent ended (breadth first: parents come first).
    for br in a.iter().skip(1) {
        assert!(a.iter().any(|p| p.depth + 1 == br.depth && p.b == br.a), "an orphan branch at depth {}", br.depth);
    }
    // Later generations come faster (explosive growth).
    let gaps: Vec<f64> = (0..6).map(|d| depth_start(d + 1, 6, 1.4) - depth_start(d, 6, 1.4)).collect();
    assert!(gaps.windows(2).all(|w| w[1] < w[0]), "{gaps:?}");
    // The loom reaches right.
    let loom = grow(&Growth { angle: 0.0, loom: true, depth: 4, ..g });
    assert!(loom.iter().all(|b| b.b[0] > b.a[0]));
}

#[test]
fn tear_halves_cover_the_frame() {
    use super::transitions::{tear_halves, tear_line};
    let (w, h) = (1920.0, 1080.0);
    let line = tear_line(w, h, 0.04, 3, 36);
    assert_eq!(line, tear_line(w, h, 0.04, 3, 36));
    assert_ne!(line, tear_line(w, h, 0.04, 4, 36));
    assert!(line.iter().all(|p| (p[0] - w * 0.5).abs() < w * 0.1));
    // Shoelace areas: the halves (with their margins) add up to the padded box.
    let area = |ps: &[[f64; 2]]| (0..ps.len()).map(|i| {
        let (a, b) = (ps[i], ps[(i + 1) % ps.len()]);
        a[0] * b[1] - b[0] * a[1]
    }).sum::<f64>().abs() * 0.5;
    let (l, r) = tear_halves(w, h, &line);
    let m = w.max(h) * 0.05;
    assert!((area(&l) + area(&r) - (w + 2.0 * m) * (h + 2.0 * m)).abs() < 1.0);
}

#[test]
fn droste_zoom_loops_seamlessly() {
    use super::transitions::droste_scale;
    for k in 1..5 {
        // The end of a period is the next copy out at its start.
        assert!((droste_scale(0.4, k, 1.0) - droste_scale(0.4, k - 1, 0.0)).abs() < 1e-12);
    }
    assert_eq!(droste_scale(0.4, 0, 0.0), 1.0);
    assert!((droste_scale(0.4, 2, 0.0) - 0.16).abs() < 1e-12);
}

#[test]
fn reference_kit_loads_and_is_deterministic() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 3.0 layers: [
        Pattern{kind: @scales seed: 5 color: #7a8f3a color2: #10140a cell: 40 size: vec2(1280, 720)}
        Pattern{kind: @guilloche color: #1f5a3a color2: #f4efe4 cell: 300 weight: 0.6 size: vec2(1280, 720)}
        Pattern{kind: @leds seed: 2 speed: 8 color: #ff5a1f color2: #0a0a0a cell: 24 size: vec2(1280, 720)}
        Rays{burst: 0.5}
        GridWarp{mode: @ripple mass: 0.5}
        Branches{style: @loom labels: ["Almost", "Exactly", "like", "prophesied"] depth: 3}
        Rect{size: vec2(1280, 720) fill: #f4efe4 tear: {time: 1.0 seed: 2}}
        Group{droste: {depth: 4 scale: 0.35 period: 2 frame: #ff5a1f} layers: [Rect{size: vec2(1280, 720) fill: #111111}]}
    ]}"#;
    let (a, b) = (load(src), load(src));
    for t in [0.2, 1.3, 2.5] {
        assert_eq!(a.eval(t).hash(), b.eval(t).hash(), "t {t}");
    }
    assert_ne!(a.eval(0.2).hash(), a.eval(1.3).hash());
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Rect{size: vec2(10, 10) fill: #fff tear: {when: 1}}]}"#).is_err());
}

#[test]
fn shapes_fill_or_trace() {
    // A square, filled: every point inside a 100-unit square centred on 0.
    let sq = shape_points("M 0 0 L 10 0 L 10 10 L 0 10 Z", 100.0, 300, 1, false).unwrap();
    assert_eq!(sq.len(), 300);
    assert!(sq.iter().all(|p| p[0].abs() <= 51.0 && p[1].abs() <= 51.0));
    assert!(sq.iter().any(|p| p[0].abs() < 20.0 && p[1].abs() < 20.0), "inside, not only the edge");
    // Its outline: every point on the edge.
    let edge = shape_points("M 0 0 L 10 0 L 10 10 L 0 10 Z", 100.0, 300, 1, true).unwrap();
    assert!(edge.iter().all(|p| (p[0].abs() - 50.0).abs() < 2.0 || (p[1].abs() - 50.0).abs() < 2.0));
    // A stroke-only line falls back to its outline.
    assert_eq!(shape_points("M 0 0 L 10 10", 100.0, 50, 1, false).unwrap().len(), 50);
    assert_eq!(sq, shape_points("M 0 0 L 10 0 L 10 10 L 0 10 Z", 100.0, 300, 1, false).unwrap());
}

#[test]
fn staged_particles_land_on_every_stage() {
    use super::particles::{stage_times, staged};
    let flow = Flow { converge: 0.9, release: 5.0, centre: [640.0, 360.0], swirl: 300.0, wind: [-4000.0, 0.0] };
    let targets = [[600.0, 300.0], [800.0, 420.0], [500.0, 500.0]];
    let (leave, arrive) = stage_times(0.9, 0.6, 0.9, 3, 7, 3);
    for k in 0..2 {
        let (a, _) = staged(&flow, 3, 7, &targets, 0.5, 0.6, 0.9, leave[k] - 0.01);
        assert!((a[0] - targets[k][0]).abs() < 3.0 && (a[1] - targets[k][1]).abs() < 3.0, "on stage {k} before leaving: {a:?}");
        let (b, o) = staged(&flow, 3, 7, &targets, 0.5, 0.6, 0.9, arrive[k] + 0.01);
        assert!((b[0] - targets[k + 1][0]).abs() < 3.0 && (b[1] - targets[k + 1][1]).abs() < 3.0, "on stage {} after arriving: {b:?}", k + 1);
        assert_eq!(o, 1.0);
        let (m, _) = staged(&flow, 3, 7, &targets, 0.5, 0.6, 0.9, (leave[k] + arrive[k]) * 0.5);
        assert!(m != targets[k] && m != targets[k + 1]);
    }
    // Deterministic, and blown away after the release.
    assert_eq!(staged(&flow, 3, 7, &targets, 0.5, 0.6, 0.9, 2.7), staged(&flow, 3, 7, &targets, 0.5, 0.6, 0.9, 2.7));
    let (gone, o) = staged(&flow, 3, 7, &targets, 0.5, 0.6, 0.9, 6.5);
    assert!(gone[0] < targets[2][0] - 100.0 && o == 0.0);
}

#[test]
fn particle_morph_loads() {
    let src = r#"use mod.motion.*; Motion{size: vec2(1280, 720) duration: 5.0 layers: [
        ParticleText{count: 300 stages: ["A gust of wind", {svg: "M 0 0 L 10 0 L 10 30 L 2 30 L 2 6 L 8 6 L 8 24" outline: true}, "scattering"] hold: 0.6 move: 0.9}
    ]}"#;
    let doc = load(src);
    assert_eq!(doc.eval(2.0).hash(), load(src).eval(2.0).hash());
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [ParticleText{stages: [{shape: 1}]}]}"#).is_err());
}

// ---- Math ------------------------------------------------------------------

fn math_doc(layer: &str) -> MotionDoc {
    load(&format!("use mod.motion.*; Motion{{size: vec2(1280, 720) duration: 2.0 layers: [{layer}]}}"))
}

/// The Paths a Math lowered to: (name, at, fill is animated).
fn math_paths(doc: &MotionDoc) -> Vec<(String, [f64; 2], bool)> {
    use crate::model::LayerKind;
    doc.all_layers()
        .into_iter()
        .filter_map(|l| match &l.kind {
            LayerKind::Path(PathProps { fill, .. }) => Some((l.name.clone().unwrap_or_default(), l.transform.at.constant().unwrap_or([f64::NAN; 2]), fill.as_ref().is_some_and(|f| f.is_animated()))),
            _ => None,
        })
        .collect()
}

#[test]
fn math_is_a_group_of_named_pieces() {
    let doc = math_doc(r#"Math{tex: "\\frac{a}{b}" size: 48 color: #ffffff}"#);
    let pieces = math_paths(&doc);
    // a, b and the bar
    assert_eq!(pieces.len(), 3, "{pieces:?}");
    let names: std::collections::HashSet<_> = pieces.iter().map(|p| p.0.clone()).collect();
    assert_eq!(names.len(), 3, "keys are unique: {names:?}");
    assert!(pieces.iter().any(|p| p.0.starts_with("0/num")) && pieces.iter().any(|p| p.0.starts_with("0/den")), "{names:?}");
    let a = pieces.iter().find(|p| p.0.starts_with("0/num")).unwrap();
    let b = pieces.iter().find(|p| p.0.starts_with("0/den")).unwrap();
    assert!(a.1[1] < b.1[1], "numerator above denominator");
}

#[test]
fn math_parts_style_by_id_path_and_character() {
    let plain = math_paths(&math_doc(r#"Math{tex: "x + \\frac{a}{b}" color: #ffffff}"#));
    assert!(plain.iter().all(|p| !p.2));
    // by \id: only the id's pieces animate, and no piece moves
    let by_id = math_paths(&math_doc(r#"Math{tex: "x + \\frac{\\id{top}{a}}{b}" color: #ffffff parts: {top: {color: tween(#ffffff, #ff0000, 0, 1, @linear)}}}"#));
    assert_eq!(by_id.iter().filter(|p| p.2).count(), 1);
    let pos = |v: &[(String, [f64; 2], bool)]| v.iter().map(|p| p.1).collect::<Vec<_>>();
    assert_eq!(pos(&plain), pos(&by_id));
    // by path: the denominator
    let by_path = math_paths(&math_doc(r#"Math{tex: "x + \\frac{a}{b}" color: #ffffff paths: {"2/den": {opacity: tween(1, 0, 0, 1, @linear)}}}"#));
    let moving: Vec<_> = by_path.iter().filter(|p| p.0.starts_with("2/den")).collect();
    assert_eq!(moving.len(), 1);
    // by character
    let by_char = math_paths(&math_doc(r#"Math{tex: "x + y + x" color: #ffffff chars: {x: {color: tween(#ffffff, #00ff00, 0, 1, @linear)}}}"#));
    assert_eq!(by_char.iter().filter(|p| p.2).count(), 2);
}

#[test]
fn math_reports_bad_parts() {
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Math{tex: "x" parts: {a: {colour: #fff}}}]}"#).is_err());
    assert!(MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Math{size: 40}]}"#).is_err());
}
