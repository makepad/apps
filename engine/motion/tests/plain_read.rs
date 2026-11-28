use makepad_motion::{layers, plain::{self, subset, Source}, MotionDoc};

#[test]
fn peer_subset_classifies_every_motion_constructor() {
    let mut missing = Vec::new();
    for layer in layers::LAYERS {
        if subset::layer(layer.name).is_none() {
            missing.push(layer.name.to_owned());
        }
        let fields = layers::field_specs(layer.name);
        assert!(!fields.is_empty(), "no fields reflected for {}", layer.name);
        for spec in fields {
            if subset::field(layer.name, &spec.name).is_none() {
                missing.push(format!("{}.{}", layer.name, spec.name));
            }
            if layers::is_common(&spec.name) && subset::field("Common", &spec.name).is_none() {
                missing.push(format!("Common.{}", spec.name));
            }
        }
    }
    for spec in layers::document_specs() {
        if subset::field("Motion", &spec.name).is_none() {
            missing.push(format!("Motion.{}", spec.name));
        }
    }

    // The fixture enumerates the fully installed module, after the forms and
    // shared vocabulary have been re-exported. Only this oracle runs Splash.
    // The scene installer registers these two names by numeric LiveId. Supply
    // their spellings for key.to_string(), just as an author using them does.
    for name in ["Kernel", "Sim"] {
        makepad_motion::input_id(name);
    }
    let doc = MotionDoc::load(r#"use mod.motion.*
        let names = []
        for key, value in mod.motion { names.push(key.to_string()) }
        Motion{duration: 1 layers: [] tags: names}"#).unwrap();
    assert!(doc.tags.iter().any(|name| name == "keys"), "missing forms re-export");
    for name in doc.tags {
        if subset::constructor(&name).is_none() {
            missing.push(format!("mod.motion.{name}"));
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "unclassified names: {}", missing.join(", "));
}

fn a_use_after_a_prototype_changes_the_picture() {
    // Keep both sources byte-aligned so locations are identical too.
    let imported = "use mod.motion.*\nlet Rect = Ellipse{fill: #f00}\nuse mod.motion.Rect\nMotion{duration: 1 layers: [Rect{size: vec2(20, 10) fill: #f00}]}";
    let local = imported.replace("use mod.motion.Rect", "//  mod.motion.Rect");
    let syntax = makepad_splash_syntax::parse(imported);
    assert!(syntax.errors.is_empty(), "{:?}", syntax.errors);
    let local_syntax = makepad_splash_syntax::parse(&local);
    assert!(local_syntax.errors.is_empty(), "{:?}", local_syntax.errors);
    assert_eq!(syntax.stmts, local_syntax.stmts);
    assert_ne!(syntax.uses, local_syntax.uses);

    let both = plain::read(imported).unwrap_err();
    assert!(both.iter().any(|d| d.message.contains("`use mod.motion.Rect`") && d.loc.line == 3));
    for src in [imported, &local] {
        let errors = plain::read(src).unwrap_err();
        assert!(errors.iter().any(|d| d.message.contains("`let Rect` shadows Motion's `Rect`") && d.loc.line == 2));
    }
    let a = MotionDoc::load(imported).unwrap();
    let b = MotionDoc::load(&local).unwrap();
    assert!(matches!(a.layers[0].kind, makepad_motion::LayerKind::Rect(_)));
    assert!(matches!(b.layers[0].kind, makepad_motion::LayerKind::Ellipse(_)));
}

#[test]
fn peer_reader_refuses_the_hostile_corpus() {
    a_use_after_a_prototype_changes_the_picture();
    // The full loader also refuses color(...): the form produces an array.
    let channels = r#"use mod.motion.*
        Motion{duration: 1 layers: [Ellipse{size: [32, 20] fill: color(0.5, 0.25, 1)}]}"#;
    let errors = plain::read(channels).unwrap_err();
    assert!(errors.iter().any(|d| d.path == "layers[0].fill" && d.message == "`color(...)` is not in the data subset: write the colour as `#rrggbb` or `#rrggbbaa`"));
    assert!(MotionDoc::load(channels).is_err());
    let prefix = "use mod.motion.*\n";
    // A zero-copy Repeat still owns the expanded template in both layer models.
    let zero = format!("{prefix}let A = Rect{{size: [1, 1]}}\nlet B = Group{{layers: [A A A A]}}\nMotion{{duration: 1 layers: [Repeat{{count: 0 layer: B}}]}}");
    let parsed = Source::parse(&zero).unwrap();
    assert_eq!(parsed.cost(), 6, "lowering builds the template even when nothing repeats");
    let data = parsed.lower().unwrap();
    let vm = MotionDoc::load(&zero).unwrap();
    assert_eq!(data.layers.clone().into_iter().map(full_layer).collect::<Vec<_>>(), vm.layers, "zero-copy repeat retains its template");
    let plain::LayerKind::Repeat { layer, .. } = &data.layers[0].kind else { panic!("missing repeat") };
    assert!(matches!(&layer.kind, plain::LayerKind::Group { layers, .. } if layers.len() == 4));
}

// Compare the entire lowered layer, including defaults for rich-only fields.
fn full_layer(node: plain::Layer) -> makepad_motion::Layer {
    use makepad_motion::{Layer, LayerKind, TextSource};
    use plain::LayerKind as Data;
    let plain::Layer { name, path, loc, span, transform, opacity, blur, feather, feather_side,
        blend, kind, animated, visible, locked, hard_cut } = node;
    let kind = match kind {
        Data::Rect(props) => LayerKind::Rect(props),
        Data::Ellipse(props) => LayerKind::Ellipse(props),
        Data::Path(props) => LayerKind::Path(props),
        Data::Text { text, props } => LayerKind::Text { source: TextSource::Text(text), props,
            ranges: vec![], axes: vec![], voice: Default::default(), plate: None, caret: None,
            path: None, flicker: None, only: None, stroke: None },
        Data::Group { layers, mask, props } => LayerKind::Group {
            layers: layers.into_iter().map(full_layer).collect(),
            mask: mask.map(|layer| Box::new(full_layer(*layer))), props,
            post: makepad_motion::GroupPost(vec![]), glyphs: false },
        Data::Repeat { layer, props } => LayerKind::Repeat {
            layer: Box::new(full_layer(*layer)), items: vec![], props },
    };
    Layer { name, path, loc, span, transform, opacity, blur, feather, feather_side, blend, kind,
        animated, visible, locked, hard_cut, effects: vec![], data: None }
}

fn compare(name: &str, source: &str) {
    let vm = MotionDoc::load(source).unwrap_or_else(|e| panic!("{name} VM: {e:?}"));
    let data = plain::read(source).unwrap_or_else(|e| panic!("{name} data: {e:?}"));
    macro_rules! equal {
        ($($field:ident),*) => {
            $(assert_eq!(data.$field, vm.$field, "{name}: {}", stringify!($field));)*
        }
    }
    equal!(size, duration, fps, bpm, background, stops, inputs, input_values);
    assert_eq!(data.layers.clone().into_iter().map(full_layer).collect::<Vec<_>>(), vm.layers, "{name}: layers");
    assert_eq!(data.all_layers().into_iter().map(|l| &l.path).collect::<Vec<_>>(),
        vm.all_layers().into_iter().map(|l| &l.path).collect::<Vec<_>>(), "{name}: layer traversal");
    assert_eq!(data.dependencies(), vm.dependencies(), "{name}: library revisions");
    for i in 0..6 {
        let t = data.duration * i as f64 / 5.0;
        let expected = vm.eval(t);
        let sampled = data.eval(t);
        assert_eq!(sampled.frame().hash(), expected.hash(), "{name} frame at {t}");
        assert_eq!(sampled.frame().errors, expected.errors, "{name} data diagnostics at {t}");
    }
    let scripted = source.replacen("Motion{", "Motion{color: @display ", 1);
    assert!(plain::read(&scripted).is_err(), "{name} admitted scripted field");
    MotionDoc::load(&scripted).unwrap_or_else(|e| panic!("{name} scripted VM: {e:?}"));
}

#[test]
fn peer_subset_reads_like_motion_load() {
    compare("prototype", r#"use mod.motion.*
        let Card = Rect{size: [10, 20] radius: 2 fill: #f00}
        Motion{label: "slide" brief: "shapes" tags: ["slide", @title] size: vec2(440, 320)
            duration: 3 fps: 48 bpm: 92 background: #202020 layers: [
            Card{name: "card" size: vec2(10.3, 12.2) fill: #0af stroke: #cdf width: 4
                 trim_start: 0.1 trim_end: 0.9 cap: @round at: [5, 12] x: 2 y: 7
                 scale: [1, 2] rotate: 16 anchor: [0.2, 0.3] opacity: 0.8 blend: @screen
                 span: [0, 2] blur: 0.3 feather: 1 feather_side: @outer visible: true locked: true hard_cut: true}
            Ellipse{size: vec2(16, 9)}
        ]}"#);
    compare("animated shapes", r#"use mod.motion.*
        Motion{duration: 4 stops: [0, 1.5, 4] layers: [
            Ellipse{name: "orbit" size: tween(vec2(12, 15), [40, 30], 0, 2, @ease_out)
                fill: keys([0, #fff], [2, #369, @ease_in_sine]) stroke: #abcd width: 3
                trim_start: 0.1 trim_end: 0.8 cap: @square
                x: repeat(keys([0, 1], [1, 50]), 1.25, @pingpong, 3)
                at: keys([0, [1, 2]], [2, vec2(6, 8), @hold])}
            Rect{size: [3, 5] stroke: #def rotate: repeat(tween(0, 90, 0, 2), 0)}
        ]}"#);
    compare("structural inputs", r#"use mod.motion.*
        Motion{duration: 4 inputs: {
            w: input(@number, 12, "width") frame: input(@vec2, [222.25, 108], "canvas")
            ink: input(@color, #bcdefaaa, "accent") show: input(@bool, true, "visible")
            title: input(@text, "Introduction", "label")
        } size: in(@frame) background: in(@ink)
        layers: [Rect{name: in(@title) size: vec2(in(@w), 8) fill: in(@ink) visible: in(@show) x: in(@show)}]}"#);

    compare("masked group", r#"use mod.motion.*
        Motion{duration: 3 layers: [Group{size: [180, 90] stagger: 0.25 mask_mode: @luma
            mask: Ellipse{size: [50, 30]}
            layers: [Rect{size: [30, 10] fill: #baf x: tween(0, 80, 0, 1)}
                     Group{layers: []}]
        }]}"#);
    compare("repeat layouts", r#"use mod.motion.*
        let DotShape = Ellipse{size: [10, 10] fill: #fed}
        Motion{duration: 4 layers: [
            Repeat{layer: DotShape count: 6 layout: @grid columns: 3 spacing: [20, 15] delay: 0.12
                step: {x: 1 y: 2 rotate: 5 scale: 0.05 opacity: -0.04} jitter: {x: 2} seed: 13}
            Repeat{layer: DotShape count: 3 layout: @radial radius: 30 start: 15 sweep: 270 face: true}
            Repeat{layer: DotShape}
        ]}"#);
    compare("paths", r#"use mod.motion.*
        Motion{duration: 2 layers: [
            Path{svg: "M0 0 L20 30 Z" stroke: #fd8 width: 3 trim_start: 0.1
                 trim_end: tween(0, 1, 0, 1) cap: @butt join: @bevel}
            Path{points: [[0, 0], vec2(20, 10), [16, 25]] fill: #26a closed: true smooth: true}
        ]}"#);

    compare("title slide", r#"use mod.motion.*
        let Title = Font{family: "Inter" weight: 640 italic: true}
        Motion{duration: 3 inputs: {title: input(@text, "A new beginning", "Headline")}
            layers: [Text{text: in(@title) font: Title by: @word stagger: 0.14
                size: 52 color: #aef align: @center valign: @bottom line_height: 1.3
                tracking: tween(-1, 1.5, 0, 2) wrap: 350 fit: @safe reveal: tween(0, 1, 0, 2)}
                Text{text: "Sübtitle \u{2605} \x41 \n \other"}]}"#);
    compare("input declaration order", r#"use mod.motion.*
        let n = 1
        Motion{duration: 2 layers: [] inputs: {amount: input(@number, n) ink: input(@color, #f80) amount: input(@number, 3)}}"#);

}
