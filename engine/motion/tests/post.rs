//! The HDR frame (`color: @hdr`), `post:` passes and kits and whole-frame
//! `motion_blur:` (makepad-render-graph), and `emit()` colours over 1.
//! Existing documents are unaffected: every field is new and optional.

use makepad_motion::*;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

#[test]
fn a_document_without_them_is_a_display_frame_without_post() {
    let doc = load(r#"use mod.motion.*; Motion{duration: 1.0 layers: [Rect{size: vec2(100, 100) fill: #ff0000}]}"#);
    let frame = doc.eval(0.5);
    assert!(!frame.hdr);
    assert!(frame.post.is_none());
    assert!(frame.motion_blur.is_none());
}

#[test]
fn kits_expand_into_passes_with_their_own_names() {
    let doc = load(r#"use mod.motion.*; use mod.post; Motion{duration: 2.0 color: @hdr
        motion_blur: post.MotionBlur{shutter: tween(0, 360, 0, 2) samples: 36}
        post: [
            post.Glow{strength: 0.8 tint: #ff8040}
            post.Glow{}
            post.Pass{at: @final uniforms: {k: fn(t) { return t * 2.0 }} pixel: fn() -> vec4 { return self.color.sample(self.uv()) * self.k }}
            post.Vignette{}
        ]
        layers: [Rect{size: vec2(100, 100) fill: emit(#ff5a1f, 4)}]}"#);
    let frame = doc.eval(1.0);
    assert!(frame.hdr);
    assert_eq!(frame.motion_blur, Some((180.0, 36, 3.0)));
    let (decls, values) = frame.post.clone().expect("post passes");
    // Two Glow pyramids (10 passes each), the Pass, the vignette.
    assert_eq!(decls.len(), 22);
    assert_eq!(decls[0].name.as_deref(), Some("k0_pre"));
    assert_eq!(decls[10].name.as_deref(), Some("k1_pre"));
    assert!(decls[19].reads.contains(&"k1_u0".to_string()));
    assert_eq!(decls[20].stage, makepad_render_graph::Stage::Final);
    assert_eq!(values[20][0][0], 2.0);
    // The tint is linear light.
    let tint = decls[9].uniforms.iter().position(|u| u.name == "tint").unwrap();
    assert!((values[9][tint][0] - 1.0).abs() < 1e-4 && values[9][tint][1] < 0.3, "{:?}", values[9][tint]);
    // emit() keeps components over 1.
    let fill = match &frame.items[0].kind {
        ItemKind::Rect { fill, .. } => fill.unwrap(),
        _ => panic!(),
    };
    assert!(fill[0] > 3.9 && fill[3] == 1.0, "{fill:?}");
}

#[test]
fn a_bad_pass_is_a_diagnostic() {
    let d = MotionDoc::load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 post: [post.Pass{at: @middle pixel: fn() -> vec4 { return vec4(1.0) }}] layers: []}"#).unwrap_err();
    assert!(d.iter().any(|d| d.to_string().contains("not a stage")), "{:?}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>());
}

#[test]
fn a_data_layer_leaves_the_frame_for_its_own_texture() {
    let doc = load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 color: @hdr post: [post.Pass{reads: [@color, "field"] pixel: fn() -> vec4 { return self.field.sample(self.uv()) }}] layers: [
        Rect{name: "field" data_layer: true size: vec2(100, 100) fill: rgba(0.5, 0.25, 0.0, 1.0)}
        Rect{size: vec2(50, 50) fill: #ff0000}
    ]}"#);
    let frame = doc.eval(0.5);
    assert_eq!(frame.items.len(), 1, "the data layer is not in the frame");
    assert_eq!(frame.data_layers.len(), 1);
    assert_eq!(frame.data_layers[0].0, "field");
    let d = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 1.0 layers: [Rect{data_layer: true size: vec2(10, 10)}]}"#).unwrap_err();
    assert!(d.iter().any(|d| d.to_string().contains("needs a `name`")));
}

#[test]
fn a_layer_kind_keeps_its_own_data_field() {
    // The data-layer flag is `data_layer`: a Chart's `data` (its values)
    // stays its own.
    let doc = load(r#"use mod.motion.*; Motion{duration: 1.0 layers: [
        Path{svg: "M 0 0 L 100 100" stroke: #ffffff width: 2}
        Chart{type: @bar size: vec2(400, 300) data: [3, 5, 2]}
    ]}"#);
    let frame = doc.eval(0.5);
    assert!(frame.data_layers.is_empty());
    assert!(!frame.items.is_empty());
}

#[test]
fn a_post_uniform_fn_giving_a_vector_is_a_vector_uniform() {
    let doc = load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 color: @hdr layers: [] post: [post.Pass{reads: [@color] uniforms: {p: fn(t) { vec2(0.25, t) } k: fn(t) { t * 2 }} pixel: fn() -> vec4 { return self.color.sample(self.uv() + self.p * self.k) }}]}"#);
    let frame = doc.eval(0.5);
    let (decls, values) = frame.post.expect("post");
    let widths: Vec<u8> = decls[0].uniforms.iter().map(|u| u.width).collect();
    assert_eq!(widths, vec![2, 1]);
    assert_eq!(values[0][0][..2], [0.25, 0.5]);
    assert_eq!(values[0][1][0], 1.0);
}

#[test]
fn memo_computes_shared_state_once_per_frame() {
    // Two calls at the same time give the very same object (computed once);
    // another time computes again.
    let doc = load(r#"use mod.motion.*; let mk = fn(t) { {x: t} }
Motion{duration: 1.0 layers: [
    Rect{size: vec2(10, 10) x: fn(t) { if memo(mk, t) == memo(mk, t) { 100 } else { 0 } }}
    Rect{size: vec2(10, 10) x: fn(t) { memo(mk, t).x * 100 }}
]}"#);
    let frame = doc.eval(0.5);
    let xs: Vec<f64> = frame.items.iter().map(|i| i.world.e).collect();
    assert_eq!(xs, vec![100.0, 50.0], "{:?}", frame.errors);
}

#[test]
fn pass_code_is_splash_functions_located_in_the_document() {
    // `pixel` and `helpers` are shader functions written in the document;
    // the pass keeps them (moved to the GPU's VM by their tokens) and where
    // they are (a compile error names the document's line). A string is not
    // code.
    let doc = load("use mod.motion.*; use mod.post; Motion{duration: 1.0 color: @hdr layers: [] post: [\n    post.Pass{reads: [@color]\n        helpers: {dim: fn(c: vec4) -> vec4 { return c * 0.5 }}\n        pixel: fn() -> vec4 { return self.dim(self.color.sample(self.uv())) }}]}");
    let frame = doc.eval(0.5);
    let (decls, _) = frame.post.expect("post");
    let d = &decls[0];
    assert_eq!(d.pixel.text(), "fn() -> vec4 { return self.dim(self.color.sample(self.uv())) }");
    assert_eq!((d.helpers[0].name.as_str(), d.helpers[0].code.text()), ("dim", "fn(c: vec4) -> vec4 { return c * 0.5 }"));
    let lines: Vec<u32> = [&d.pixel, &d.helpers[0].code].iter().map(|c| c.as_fn().unwrap().at.line).collect();
    assert_eq!(lines, vec![4, 3], "pixel on line 4, the helper on line 3");
    let e = MotionDoc::load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 layers: [] post: [post.Pass{pixel: "fn() -> vec4 { return vec4(1.0) }"}]}"#).unwrap_err();
    assert!(e.iter().any(|d| d.to_string().contains("shader function")), "{:?}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>());
}

#[test]
fn a_bypassed_entry_stays_written_and_is_not_drawn() {
    let on = load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 color: @hdr post: [post.Vignette{} post.Grain{amount: 0.1}] layers: [Rect{size: vec2(10, 10) fill: #ff0000 effects: [Effect{name: @glow radius: 8}]}]}"#);
    let off = load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 color: @hdr post: [post.Vignette{} post.Grain{on: false amount: 0.1}] layers: [Rect{size: vec2(10, 10) fill: #ff0000 effects: [Effect{on: false name: @glow radius: 8}]}]}"#);
    let (a, _) = on.eval(0.5).post.clone().unwrap();
    let (b, _) = off.eval(0.5).post.clone().unwrap();
    assert!(b.len() < a.len(), "{} {}", a.len(), b.len());
    assert_eq!(on.eval(0.5).items[0].effects.len(), 1);
    assert!(off.eval(0.5).items[0].effects.is_empty());
}

#[test]
fn a_group_carries_its_own_post() {
    let doc = load(r#"use mod.motion.*; use mod.post; Motion{duration: 1.0 color: @hdr layers: [
        Group{name: "lit" post: [post.Glow{strength: 1.2} post.Grain{on: false}] layers: [Rect{size: vec2(10, 10) fill: #ff0000}]}
        Rect{size: vec2(10, 10) fill: #00ff00}]}"#);
    let frame = doc.eval(0.5);
    assert!(frame.post.is_none());
    assert_eq!(frame.group_post.len(), 1);
    let (path, (decls, values)) = &frame.group_post[0];
    assert_eq!(path, "layers[0]");
    assert!(!decls.is_empty() && decls.len() == values.len());
}
