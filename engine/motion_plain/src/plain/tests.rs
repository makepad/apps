use super::{self as plain, LayerKind, Source};

#[test]
fn reader_refuses_the_hostile_corpus() {
    let prefix = "use mod.motion.*\n";
    for (name, body, expected) in [
        ("cycle", "let a = b\nlet b = a\nMotion{duration: 1 layers: []}", "cycle"),
        ("number", "Motion{duration: 1e999 layers: []}", "1e999"),
        ("nonfinite channel", "Motion{duration: 1 background: color(1e999, 0, 1) layers: []}", "1e999"),
        ("asset", "Motion{duration: 1 layers: [Rect{size: [1, 2] fill: asset(1, 2)}]}", "asset"),
        ("shader", "Motion{duration: 1 layers: [Shader{}]}", "Shader"),
        ("future layer", "Motion{duration: 1 layers: [Sparkle{}]}", "newer Work"),
        ("subtraction", "Motion{duration: 1 layers: [Rect{size: [8 -2]}]}", "subtraction"),
        ("index", "let a = [2, 4]\nMotion{duration: 1 layers: [Rect{size: [a[0], 4]}]}", "indexing"),
        ("function", "Motion{duration: 1 layers: [Rect{x: fn(t) { t } size: [1, 1]}]}", "`fn` is code"),
        ("font file", r#"Motion{duration: 1 layers: [Text{text: "hi" font: Font{family: "folder/face.TTF"}}]}"#, "path"),
        ("file extension", r#"Motion{duration: 1 layers: [Text{text: "hi" font: Font{family: "face.WoFf2"}}]}"#, "path"),
        ("invalid path", r#"Motion{duration: 1 layers: [Path{svg: "M0 0 ?"}]}"#, "SVG path data"),
        ("missing path operand", r#"Motion{duration: 1 layers: [Path{svg: "M0 0 L"}]}"#, "expects"),
        ("nonfinite path operand", r#"Motion{duration: 1 layers: [Path{svg: "M0 0 L1e999 0"}]}"#, "finite"),
        ("key handles", "Motion{duration: 1 layers: [Rect{size: [2, 2] x: keys([0, 1], [1, 2, @bezier])}]}", "bezier"),
        ("key record", "Motion{duration: 1 layers: [Rect{size: [2, 2] y: keys([0, 1, {out: [1, 2]}])}]}", "handles"),
        ("path morph", r#"Motion{duration: 1 layers: [Path{svg: tween("M0 0", "M1 0", 0, 1)}]}"#, "keyed"),
        ("repeat count", "let n = 3\nMotion{duration: 1 layers: [Repeat{count: n layer: Rect{size: [1,1]}}]}", "literal"),
    ] {
        let errors = plain::read(&format!("{prefix}{body}")).unwrap_err();
        assert!(errors.iter().any(|d| d.message.contains(expected)), "{name}: {errors:?}");
    }
    let refusals = plain::read(&format!("{prefix}let unused = keys([0, 1, @bezier], [1, 2, @wiggle])\nlet unused_input = input(@list)\nMotion{{duration: 1 post: [] layers: [Rect{{size: [1, 2] x: fn(t) {{ t }} y: beat(2)}} Shader{{}}]}}")).unwrap_err();
    for (path, term) in [("post", "post"), ("layers[0].x", "fn"), ("layers[0].y", "beat"), ("layers[1]", "Shader")] {
        assert!(refusals.iter().any(|d| d.path == path && d.message.contains(term) && d.loc.line == 4), "{path}: {refusals:?}");
    }

    for (path, term) in [("unused[0][2]", "bezier"), ("unused[1][2]", "wiggle"), ("unused_input[0]", "@list")] {
        assert!(refusals.iter().any(|d| d.path == path && d.message.contains(term)), "{path}: {refusals:?}");
    }

    let deep = format!("{prefix}Motion{{duration: 1 layers: {}0{}}}", "[".repeat(100_000), "]".repeat(100_000));
    match Source::parse(&deep) {
        Err(errors) => assert!(errors.iter().any(|d| d.message.contains("deep")), "missing nesting diagnostic: {:?}", errors.first()),
        Ok(source) => assert!(source.lower().is_err(), "nested arrays cannot form a layer"),
    }

    let mut fan = format!("{prefix}let a0 = Rect{{size: [1, 1]}}\n");
    for i in 1..=32 {
        let prev = format!("a{}", i - 1);
        fan.push_str(&format!("let a{i} = [{prev} {prev} {prev} {prev}]\n"));
        if i == 20 || i == 32 {
            let src = Source::parse(&format!("{fan}Motion{{duration: 1 layers: a{i}}}")).unwrap();
            assert_eq!(src.cost(), if i == 20 { 1 << 40 } else { u64::MAX }, "fan-out at {i}");
        }
    }
    let repeated = Source::parse(&format!("{prefix}let R = Repeat{{count: 4 layer: Rect{{size: [1, 1]}}}}\nMotion{{duration: 1 layers: [Repeat{{layer: R count: 3}}]}}")).unwrap();
    assert_eq!(repeated.cost(), 12);
    // A zero-copy Repeat still owns the expanded template in both layer models.
    let zero = format!("{prefix}let A = Rect{{size: [1, 1]}}\nlet B = Group{{layers: [A A A A]}}\nMotion{{duration: 1 layers: [Repeat{{count: 0 layer: B}}]}}");
    let parsed = Source::parse(&zero).unwrap();
    assert_eq!(parsed.cost(), 6, "lowering builds the template even when nothing repeats");
    let data = parsed.lower().unwrap();
    let LayerKind::Repeat { layer, .. } = &data.layers[0].kind else { panic!("missing repeat") };
    assert!(matches!(&layer.kind, LayerKind::Group { layers, .. } if layers.len() == 4));

    let mixed = format!("{prefix}let A = Rect{{size: [1, 1]}}\nMotion{{duration: 1 layers: [Repeat{{count: 0 layer: A}} Repeat{{count: 3 layer: A}}]}}");
    assert_eq!(Source::parse(&mixed).unwrap().cost(), 4, "the whole document lowers four layers and draws three");

    let long = "a".repeat(4 * 1024 * 1024);
    let doc = plain::read(&format!("{prefix}Motion{{duration: 1 layers: [Text{{text: \"{long}\"}}]}}")).unwrap();
    assert!(matches!(&doc.layers[0].kind, LayerKind::Text { text: t, .. } if t.len() == long.len()));

    let whole = format!("{prefix}Motion{{duration: 1 layers: [Rect{{size: [2, 3] fill: #fed}}]}}");
    for end in 0..whole.len() {
        assert!(plain::read(&whole[..end]).is_err(), "accepted truncated source at byte {end}");
    }
    plain::read(&whole).unwrap();
    let root = "Motion{duration: 1 layers: []}";
    for (source, message) in [
        (root.to_string(), "a data source starts with `use mod.motion.*`"),
        (format!("// use mod.motion.*\n{root}"), "a data source starts with"),
        (format!("use mod.motion.Rect\n{root}"), "`use mod.motion.Rect` is not in the data subset"),
        (format!("use mod.motion.*\nuse mod.other.*\n{root}"), "`use mod.other.*` is not in the data subset"),
        (format!("use mod.motion.*\nlet A = 1\nuse mod.motion.*\n{root}"), "`use mod.motion.*` is not in the data subset"),
        (format!("use mod.motion.*; let keys = 1; {root}"), "`let keys` shadows Motion's `keys`"),
        (format!("use mod.motion.*; let Motion = 1; {root}"), "`let Motion` shadows Motion's `Motion`"),
    ] {
        let errors = Source::parse(&source).unwrap_err();
        assert!(errors.iter().any(|d| d.message.contains(message)), "{source}: {errors:?}");
    }
    plain::read(&format!("/* header */ use mod.motion.*; use mod.motion.*; {root}")).unwrap();
}
