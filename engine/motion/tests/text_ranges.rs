//! Range selection, composition, layer clocks, variable axes and unchanged
//! legacy frames. Rendering/layout is verified separately on the native GPU.

use makepad_motion::plain::{PrimitiveFrame, TextProps};
use makepad_motion::*;

fn source(text: &str) -> String {
    format!("use mod.motion.*; Motion{{duration: 8 bpm: 120 layers: [{text}]}}")
}

fn load(text: &str) -> MotionDoc {
    MotionDoc::load(&source(text)).unwrap_or_else(|d| panic!("{d:#?}\n{text}"))
}

fn units(doc: &MotionDoc, t: f64) -> Vec<TextUnit> {
    let frame = doc.eval(t);
    assert!(frame.errors.is_empty(), "{t}: {:?}", frame.errors);
    let ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) = frame.items.into_iter().next().unwrap().kind else { panic!() };
    units
}

fn xs(doc: &MotionDoc, t: f64) -> Vec<f64> {
    units(doc, t).iter().map(|u| u.offset[0]).collect()
}

fn near(got: f64, want: f64) {
    assert!((got - want).abs() < 1e-9, "{got} != {want}");
}

fn axis(unit: &TextUnit, tag: &str) -> f64 {
    unit.axes.iter().find(|(t, _)| t == tag).unwrap_or_else(|| panic!("missing {tag}: {unit:?}")).1
}

#[test]
fn defaults_and_single_range_match_the_list_form() {
    let single = load(r#"Text{text: "Aé BC" by: @letter range: Range{x: 1}}"#);
    let list = load(r#"Text{text: "Aé BC" by: @letter ranges: [Range{x: 1}]}"#);
    assert_eq!(units(&single, 0.0), units(&list, 0.0));
    assert_eq!(xs(&single, 0.0), vec![1.0; 4]);
    assert!(!single.layers[0].animated);
    assert!(units(&single, 0.0).iter().all(|u| u.axes.is_empty()));
    let empty = load(r#"Text{text: "" by: @letter range: Range{x: 1}}"#);
    assert!(units(&empty, 0.0).is_empty());
}

#[test]
fn letter_word_line_and_coarser_selections_use_unit_centres() {
    for (by, selection, from, to, want) in [
        ("letter", "", 0.0, 0.25, vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("word", "", 0.25, 0.5, vec![0.0, 1.0, 0.0, 0.0]),
        ("line", "", 0.5, 1.0, vec![0.0, 1.0]),
        ("letter", "by: @word", 0.25, 0.5, vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
        ("letter", "by: @line", 0.5, 1.0, vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]),
        ("word", "by: @line", 0.5, 1.0, vec![0.0, 0.0, 1.0, 1.0]),
    ] {
        let doc = load(&format!(r#"Text{{text: "Aé BC\nDEF G" by: @{by}
            range: Range{{{selection} from: {from} to: {to} soft: 0 x: 1}}}}"#));
        assert_eq!(xs(&doc, 0.0), want, "{by} {selection}");
    }
    // Unicode whitespace and empty lines do not add letters/words or an
    // empty line's selection index; byte ranges still slice the UTF-8 text.
    let doc = load("Text{text: \"  A\u{2003}é\\n\\nBC D\\n\" by: @letter range: Range{by: @line to: 0.5 soft: 0 x: 1}}");
    assert_eq!(xs(&doc, 0.0), vec![1.0, 1.0, 0.0, 0.0, 0.0]);
}

#[test]
fn slide_wipes_through_letters_and_scrubs_purely() {
    let doc = load(r#"Text{text: "ABCD" by: @letter
        range: Range{to: 0.25 slide: tween(-0.25, 1, 0, 1) soft: 0 x: 1}}"#);
    for (t, want) in [
        (0.0, vec![0.0, 0.0, 0.0, 0.0]),
        (0.2, vec![1.0, 0.0, 0.0, 0.0]),
        (0.4, vec![0.0, 1.0, 0.0, 0.0]),
        (0.6, vec![0.0, 0.0, 1.0, 0.0]),
        (0.8, vec![0.0, 0.0, 0.0, 1.0]),
        (1.0, vec![0.0, 0.0, 0.0, 0.0]),
    ] {
        assert_eq!(xs(&doc, t), want, "t {t}");
    }
    let reference = doc.eval(0.4);
    for t in [7.0, 0.0, 0.9, 0.2] { doc.eval(t); }
    assert_eq!(doc.eval(0.4), reference);
    assert!(doc.layers[0].animated && !reference.items[0].is_static);
}

#[test]
fn soft_coarser_selections_share_one_weight_despite_different_lengths() {
    for (text, by) in [("A BC DEF GHIJ", "word"), ("A\\nBC\\nDEF\\nGHIJ", "line")] {
        let doc = load(&format!(r#"Text{{text: "{text}" by: @letter range: Range{{by: @{by}
            from: 0.25 to: 0.75 soft: 0.25 ease: @linear x: 1}}}}"#));
        assert_eq!(xs(&doc, 0.0), vec![0.5, 1.0, 1.0, 1.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.5]);
    }
    let doc = load(r#"Text{text: "A BBB" by: @letter range: Range{by: @word from: 0.5 soft: 0 wght: 900}}"#);
    let us = units(&doc, 0.0);
    assert!(us[0].axes.is_empty());
    assert!(us[1..].iter().all(|u| axis(u, "wght") == 900.0));
}

#[test]
fn hard_soft_default_ease_and_invert() {
    for (soft, invert, want) in [
        (0.0, false, vec![0.0, 1.0, 1.0, 0.0]),
        (0.25, false, vec![0.5, 1.0, 1.0, 0.5]),
        (0.0, true, vec![1.0, 0.0, 0.0, 1.0]),
        (0.25, true, vec![0.5, 0.0, 0.0, 0.5]),
    ] {
        let doc = load(&format!(r#"Text{{text: "ABCD" by: @letter
            range: Range{{from: 0.25 to: 0.75 soft: {soft} ease: @linear invert: {invert} x: 1}}}}"#));
        assert_eq!(xs(&doc, 0.0), want);
    }
    let doc = load(r#"Text{text: "ABCD" by: @letter range: Range{from: 0.5 soft: 0.5 x: 1}}"#);
    assert_eq!(xs(&doc, 0.0), vec![0.125, 0.875, 1.0, 1.0]);
    let doc = load(r#"Text{text: "AB" by: @letter range: Range{to: 0.2 soft: 0.1 x: 1}}"#);
    near(xs(&doc, 0.0)[0], 0.5);
}

#[test]
fn falloff_accepts_all_named_document_eases_and_keeps_weights_bounded() {
    for ease in Ease::ids().into_iter().chain(Ease::shaper_ids()) {
        let doc = load(&format!(r#"Text{{text: "ABCD" by: @letter
            range: Range{{from: 0.5 to: 0.5 soft: 0.5 ease: @{ease} x: 1}}}}"#));
        for w in xs(&doc, 0.0) { assert!((0.0..=1.0).contains(&w), "{ease}: {w}"); }
    }
}

#[test]
fn targets_blend_after_stagger_on_the_layer_clock() {
    let doc = load(r#"Text{text: "AB" by: @letter span: [2, 4] stagger: 0.5
        x: tween(0, 10, 0, 1) y: 4 scale: vec2(2, 3) rotate: 10 opacity: 0.8 blur: 2 color: #000
        range: Range{from: 0.5 to: 0.5 soft: 0.5 ease: @linear
            x: tween(0, 20, 0, 1) y: -8 scale: vec2(3, 0.5) rotate: 40 opacity: 0.25 blur: 6 color: #ff000080}}"#);
    let us = units(&doc, 2.5);
    near(us[0].offset[0], 10.0); // unit clock .5: 5, range clock .5: +5
    near(us[1].offset[0], 5.0);  // unit clock 0: 0, same range clock: +5
    for u in &us {
        assert_eq!(u.offset[1], 0.0);
        assert_eq!(u.scale, [4.0, 2.25]);
        assert_eq!(u.rotate, 30.0);
        near(u.opacity, 0.5);
        assert_eq!(u.blur, 5.0);
        assert_eq!(u.color[..3], [0.5, 0.0, 0.0]);
        near(u.color[3], (1.0 + 128.0 / 255.0) * 0.5);
        assert!(u.axes.is_empty());
    }
    let uniform = load(r#"Text{text: "A" by: @letter scale: vec2(2, 3) range: Range{scale: 1.5}}"#);
    assert_eq!(units(&uniform, 0.0)[0].scale, [3.0, 4.5]);
}

#[test]
fn ranges_compose_in_written_order() {
    let range = |color| format!("Range{{from: 0.5 to: 0.5 soft: 0.5 ease: @linear color: {color}}}");
    let red = range("#f00");
    let blue = range("#00f");
    let a = load(&format!(r#"Text{{text: "AB" by: @letter color: #000 ranges: [{red}, {blue}]}}"#));
    let b = load(&format!(r#"Text{{text: "AB" by: @letter color: #000 ranges: [{blue}, {red}]}}"#));
    assert_eq!(units(&a, 0.0)[0].color, [0.25, 0.0, 0.5, 1.0]);
    assert_eq!(units(&b, 0.0)[0].color, [0.5, 0.0, 0.25, 1.0]);
}

#[test]
fn every_numeric_range_field_uses_the_shared_animation_forms() {
    for field in ["from", "to", "slide", "soft", "x", "y", "rotate", "blur", "scale", "opacity", "wght", "wdth", "slnt", "axes"] {
        for form in [
            "tween(0, 0.5, 0, 1)",
            "keys([0, 0], [1, 0.5])",
            "sequence([[0, 0], [0.5, 1]])",
            "repeat(tween(0, 0.5, 0, 1), 1, @pingpong)",
            "fn(t) { t * 0.1 }",
        ] {
            let value = if field == "axes" { format!("{{GRAD: {form}}}") } else { form.to_string() };
            let doc = load(&format!(r#"Text{{text: "AB" by: @letter font: Font{{family: "Roboto Flex"}}
                range: Range{{{field}: {value}}}}}"#));
            assert!(doc.layers[0].animated, "{field}: {form}");
            assert!(doc.warnings.is_empty(), "{field}: {:?}", doc.warnings);
            for t in [0.0, 0.25, 0.75, 1.25] { units(&doc, t); }
        }
    }
    for form in [
        "tween(#000, #fff, 0, 1)", "keys([0, #000], [1, #fff])",
        "sequence([[#000, 0], [#fff, 1]])", "repeat(tween(#000, #fff, 0, 1))",
        "fn(t) { color(t, 0, 0) }",
    ] {
        let doc = load(&format!(r#"Text{{text: "AB" by: @letter range: Range{{color: {form}}}}}"#));
        near(units(&doc, 0.5)[0].color[0], 0.5);
    }
}

#[test]
fn range_selection_and_targets_resolve_musical_times() {
    let doc = load(r#"Text{text: "AB" by: @letter span: [1, 4] stagger: 0.5
        range: Range{from: tween(0, 0.1, beat(2), beat(4)) to: tween(0.9, 1, beat(2), beat(4))
            slide: tween(-0.1, 0, beat(2), beat(4)) soft: tween(0, 0.1, beat(2), beat(4))
            x: tween(0, 10, beat(2), beat(4)) y: keys([beat(2), 0], [beat(4), 20])
            scale: tween(1, 2, beat(2), beat(4)) color: tween(#000, #fff, beat(2), beat(4))
            wght: tween(400, 900, beat(2), beat(4))}}"#);
    assert!(uses_musical_time(&doc));
    // beat 2 = document 1 s = this layer's 0; beat 4 = its 1 s.
    for u in units(&doc, 1.5) {
        near(u.offset[0], 5.0);
        near(u.offset[1], 10.0);
        assert_eq!(u.scale, [1.5, 1.5]);
        assert_eq!(u.color, [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(axis(&u, "wght"), 650.0);
    }
}

#[test]
fn axes_mix_from_item_axes_and_font_defaults() {
    let doc = load(r#"Text{text: "AB" by: @letter
        font: Font{family: "Roboto Flex" weight: 600 axes: {GRAD: 20}}
        wdth: 120 wght: tween(400, 600, 0, 1)
        range: Range{from: 0.5 to: 0.5 soft: 0.5 ease: @linear
            wght: 900 wdth: 140 slnt: -10 axes: {GRAD: 100 opsz: 40}}}"#);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    for u in units(&doc, 0.5) {
        assert_eq!(axis(&u, "wght"), 700.0);
        assert_eq!(axis(&u, "wdth"), 130.0);
        assert_eq!(axis(&u, "slnt"), -5.0);
        assert_eq!(axis(&u, "GRAD"), 60.0);
        assert_eq!(axis(&u, "opsz"), 27.0); // Roboto Flex's optical-size default is 14.
    }
    let doc = load(r#"Text{text: "AB" by: @letter font: Font{family: "Inter" weight: 600}
        range: Range{from: 0.5 to: 0.5 soft: 0.5 ease: @linear wght: 900}}"#);
    assert_eq!(axis(&units(&doc, 0.0)[0], "wght"), 750.0);
    let doc = load(r#"Text{text: "AB" by: @letter font: Font{family: "Roboto Flex"}
        range: Range{from: 0.5 to: 0.5 soft: 0.5 ease: @linear wdth: 140}}"#);
    assert_eq!(axis(&units(&doc, 0.0)[0], "wdth"), 120.0);
}

#[test]
fn axes_are_stepped_after_all_ranges_and_only_allocated_where_targeted() {
    let doc = load(r#"Text{text: "AB" by: @letter ranges: [
        Range{from: 0.5 to: 0.5 soft: 0.5 ease: @linear wght: 403}
        Range{from: 0.5 to: 0.5 soft: 0.5 ease: @linear wght: 404}]}"#);
    assert_eq!(axis(&units(&doc, 0.0)[0], "wght"), 405.0); // 400 → 401.5 → 402.75 → step
    let doc = load(r#"Text{text: "AB" by: @letter font: Font{family: "Roboto Flex" axes: {GRAD: 20}}
        range: Range{to: 0.5 soft: 0 wght: 887.2 wdth: 121.3 slnt: -7.4}}"#);
    let us = units(&doc, 0.0);
    assert_eq!(axis(&us[0], "wght"), 885.0);
    assert_eq!(axis(&us[0], "wdth"), 121.0);
    assert_eq!(axis(&us[0], "slnt"), -7.0);
    assert_eq!(axis(&us[0], "GRAD"), 20.0);
    assert!(us[1].axes.is_empty(), "unselected units inherit the item");
    let doc = load(r#"Text{text: "A" by: @letter range: Range{wght: 500 axes: {wght: 800}}}"#);
    assert_eq!(units(&doc, 0.0)[0].axes, vec![("wght".to_string(), 800.0)]);
}

#[test]
fn ranges_diagnose_invalid_fields_windows_and_finer_units() {
    for (body, field, message) in [
        (r#"Text{text: "A" by: @letter range: Range{slied: 1}}"#, "slied", "Range"),
        (r#"Text{text: "A" by: @letter range: Range{from: 0.8 to: 0.2}}"#, "to", "greater than or equal"),
        (r#"Text{text: "A B" by: @word range: Range{by: @letter}}"#, "by", "finer than"),
        (r#"Text{text: "A B" by: @line range: Range{by: @word}}"#, "by", "finer than"),
        (r#"Text{text: "A B" by: @line range: Range{by: @letter}}"#, "by", "finer than"),
        (r#"Text{text: "A" by: @whole ranges: [Range{x: 1}]}"#, "ranges", "ranges need by: @letter, @word or @line"),
        (r#"Text{text: "A" range: Range{x: 1}}"#, "range", "ranges need by: @letter, @word or @line"),
        (r#"Text{text: "A" by: @letter range: Range{by: @whole}}"#, "by", "@letter, @word or @line"),
        (r#"Text{text: "A" by: @letter range: Range{soft: -0.1}}"#, "soft", "non-negative"),
        (r#"Text{text: "A" by: @letter range: Range{ease: @smooooth}}"#, "ease", "not an ease"),
        (r#"Text{text: "A" by: @letter ranges: [42]}"#, "ranges[0]", "expects Range"),
        (r#"Text{text: "A" by: @letter range: Range{axes: {weight: 800}}}"#, "axes.weight", "axis tag"),
    ] {
        let errors = MotionDoc::load(&source(body)).unwrap_err();
        assert!(errors.iter().any(|e| (e.path.contains(field) || e.message.contains(field)) && e.message.contains(message)), "{body}: {errors:#?}");
    }
    let doc = load(r#"Text{text: "A" by: @letter ranges: [Range{from: tween(0, 1, 0, 1) to: 0.5 x: 1}]}"#);
    assert!(doc.eval(0.8).errors.iter().any(|e| e.path == "layers[0].ranges[0].to" && e.message.contains("from")));
}

#[test]
fn unsupported_axes_warn_and_leave_other_targets_working() {
    let doc = load(r#"Text{text: "AB" by: @letter font: Font{family: "Inter"}
        range: Range{x: 3 axes: {NOPE: 50}}}"#);
    assert!(doc.warnings.iter().any(|w| w.path.contains("axes.NOPE") && w.message.contains("Inter") && w.message.contains("ignored")), "{:?}", doc.warnings);
    assert_eq!(xs(&doc, 0.0), vec![3.0, 3.0]);
    assert!(units(&doc, 0.0).iter().all(|u| u.axes.is_empty()));
    let static_font = load(r#"Text{text: "A" by: @letter font: Font{family: "Anton"} range: Range{wght: 900}}"#);
    assert!(static_font.warnings.iter().any(|w| w.message.contains("wght") && w.message.contains("Anton")));
}

#[test]
fn documents_without_ranges_keep_plain_text_units() {
    let doc = MotionDoc::load(include_str!("../fixtures/kinetic_title.splash")).unwrap();
    // Sample the plain per-unit animation every half second, independently
    // of the frame's Debug representation and pinned fixture hashes.
    for step in 0..(doc.duration * 2.0).ceil() as usize {
        let t = step as f64 * 0.5;
        let frame = doc.eval(t);
        assert!(frame.errors.is_empty());
        for (layer, item) in doc.layers.iter().zip(&frame.items) {
            let LayerKind::Text { source: TextSource::Text(text), props: TextProps { by, stagger, color, .. }, ranges, .. } = &layer.kind else { continue };
            let ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) = &item.kind else { panic!() };
            assert!(ranges.is_empty());
            let spans = unit_ranges(text, *by);
            assert_eq!(units.len(), spans.len());
            for (i, (unit, range)) in units.iter().zip(spans).enumerate() {
                let ut = t - stagger.delay(i, units.len());
                let tr = &layer.transform;
                let offset = [sample(&tr.x, ut)[0], sample(&tr.y, ut)[0]];
                let scale = sample(&tr.scale, ut);
                let rotate = sample(&tr.rotate, ut)[0];
                let opacity = sample(&layer.opacity, ut)[0].clamp(0.0, 1.0);
                let color = sample(color, ut);
                let blur = sample(&layer.blur, ut)[0].max(0.0);
                let plain = TextUnit { range, offset, scale, rotate, opacity, color, blur, axes: Vec::new() };
                assert_eq!(unit, &plain, "{} unit {i}, t {t}", layer.path);
            }
        }
    }
}

fn sample<const N: usize>(prop: &Prop<N>, t: f64) -> [f64; N] {
    match prop {
        Prop::Const(value) => *value,
        Prop::Track(track) => sample_track(track, t),
        Prop::Fn(_) | Prop::Blend(_) => panic!("this fixture uses only constants and tracks"),
    }
}

#[test]
fn authoring_guide_range_examples_load() {
    let skill = include_str!("../guide/SKILL.md");
    let section = skill.split("### Text ranges").nth(1).unwrap().split("### Rules").next().unwrap();
    let examples: Vec<_> = section.split("```splash\n").skip(1).map(|s| s.split("```").next().unwrap()).collect();
    assert_eq!(examples.len(), 2);
    for example in examples {
        let doc = MotionDoc::load(example).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        for t in [0.0, 0.5, 1.5, 2.5, 3.9] { units(&doc, t); }
    }
}
