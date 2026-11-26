//! The layer catalogue for editors and AI prompts (lane A1): every layer
//! type's fields ([`layer_fields`]) and a small document that uses it
//! ([`layer_example`]; each one loads, see the tests).

pub use crate::layers::{fields as layer_fields, layer_types};

/// A complete document showing layer type `kind`.
pub fn layer_example(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "Rect" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Rect{at: vec2(170, 130) size: tween(vec2(0, 100), vec2(300, 100), 0.0, 0.8, @ease_out_cubic) radius: 12 fill: #ff5a36 stroke: #ffffff width: 2}
]}"#,
        "Ellipse" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Ellipse{at: vec2(260, 120) size: vec2(120, 120) stroke: #36c2ff width: 10 cap: @round trim_end: tween(0.0, 1.0, 0.0, 1.5, @ease_in_out)}
]}"#,
        "Path" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Path{svg: "M 80 280 C 200 40, 440 40, 560 280" stroke: #ffd400 width: 8 cap: @round trim_end: tween(0.0, 1.0, 0.0, 1.2, @ease_out_quad)}
]}"#,
        "Svg" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Svg{svg: "<svg viewBox='0 0 10 10'><circle cx='5' cy='5' r='4' fill='#36c2ff'/></svg>" at: vec2(270, 130) size: vec2(100, 100) rotate: tween(0, 360, 0.0, 2.0)}
]}"#,
        "Text" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 3.0 layers: [
    Text{font: Font{family: "Inter" weight: 800} text: "Make it move" size: 56 color: #ffffff at: vec2(320, 180) align: @center valign: @center by: @letter stagger: 0.04
         opacity: tween(0.0, 1.0, 0.0, 0.4) y: tween(30, 0, 0.0, 0.6, @ease_out_back)}
]}"#,
        "Vector" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 3.0 layers: [
    Vector{icon: "stage-app/audio" at: vec2(220, 80) size: vec2(200, 200) stroke: #ffd400 draw_on: tween(0, 1, 0, 2, @ease_in_out)}
]}"#,
        "Repeat" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 3.0 layers: [
    Repeat{count: 24 layout: @radial radius: 120 face: true delay: 0.04 at: vec2(320, 180)
        layer: Rect{at: vec2(-6, -24) size: vec2(12, 48) radius: 6 fill: #36c2ff scale: tween(0, 1, 0, 0.4, @ease_out_back)}}
]}"#,
        "Pattern" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Group{size: vec2(300, 300) at: vec2(170, 30) mask: Ellipse{size: vec2(300, 300)}
        layers: [Pattern{kind: @stripes color: #ff5a36 color2: #2a1a14 cell: 24 angle: 30 weight: 0.4 size: vec2(300, 300) speed: 0.5}]}
]}"#,
        "Crowd" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 4.0 bpm: 120 layers: [
    Crowd{count: 80 rows: 3 at: vec2(0, 380) size: vec2(1280, 340) seed: 3 eyes: #ff8a1f eyes2: #b6ff3b react: @beat nod: 1
          arms: keys([1.5, 0], [2.0, 1, @ease_out_back]) signs: 0.15 sign: "YES" sign_up: keys([2.5, 0], [2.9, 1])}
]}"#,
        "Character" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 6.0 bpm: 100 layers: [
    Character{model: @spark at: vec2(160, 690) height: 460 walk: {start: beat(0) steps: 6 stride: 110}
              pose: [[0, @idle], [beat(7), @on_mic, 0.3]] expression: [[0, @deadpan], [beat(8), @smile]] nod: 5 blink: 3.0 seed: 2}
]}"#,
        "Freeze" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    Ellipse{at: vec2(100, 260) x: tween(0, 900, 0, 3) size: vec2(200, 200) fill: #ffc23a}
    Freeze{time: 1.0 at: vec2(820, 40) size: vec2(400, 225)}
]}"#,
        "Comic" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    Comic{panels: [{time: 0.2 caption: "one"} {time: 0.7 caption: "two" burst: "POW"} {time: 1.2 caption: "three"}]}
]}"#,
        "Polaroid" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    Rect{size: vec2(1280, 720) fill: #1d2a44}
    Ellipse{at: vec2(100, 260) x: tween(0, 900, 0, 3) size: vec2(200, 200) fill: #ffc23a}
    Polaroid{time: 1.0 span: [1.0, 2.0] caption: "that moment"}
]}"#,
        "FlyThrough" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    FlyThrough{into: "title" letter: 1 time: 1.2 duration: 0.8 zoom: 40 layers: [
        Rect{size: vec2(1280, 720) fill: #ff5a1f}
        Text{name: "title" text: "MOVE." font: Font{family: "Anton"} size: 260 color: #111111 at: vec2(640, 360) align: @center valign: @center window: 1
             inside: Pattern{kind: @checker color: #1f3dff color2: #f0ebe0 cell: 40 size: vec2(1280, 720)}}]}
]}"#,
        "Hud" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 4.0 bpm: 120 layers: [
    Hud{title: "Story  motion designer" tag: "Showreel '26" chapters: [[0, "EASING"], [2.0, "TYPOGRAPHY", #111111]]}
]}"#,
        "TokenStream" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    TokenStream{at: vec2(140, 100) size: vec2(1000, 520) prompt: "Lantern, shine!" tokens: ["Lant", "ern, ", "shine!"] header: "Harbor log" every: 0.5 enter: true}
]}"#,
        "Form" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    Form{at: vec2(360, 20) size: vec2(560, 680) title: "Form 7-B" fields: [["Harbor", "East quay"]] checks: {options: ["closed", "open"] pick: 1} signature: "We" stamp: "open"}
]}"#,
        "CodeDraw" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    CodeDraw{code: "\\draw (0,0) circle (1); % head\n\\draw (0,1) -- (0,2); % horn" layers: [Ellipse{at: vec2(200, 300) size: vec2(160, 160) stroke: #ff5a1f width: 4} Path{svg: "M 280 300 L 280 150" stroke: #ffffff width: 4}]}
]}"#,
        "UiWindow" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    UiWindow{at: vec2(340, 200) size: vec2(600, 300) title: "Auto-reply" lines: ["Harbor gates open at dawn."] button: "OK"}
]}"#,
        "UiButton" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 3.0 layers: [
    UiButton{at: vec2(520, 330) label: "Regenerate" click: 2.0}
]}"#,
        "Shorts" => r#"use mod.motion.*
Motion{size: vec2(1080, 1920) duration: 4.0 layers: [
    Shorts{tag: "one prompt test" headline: ["One prompt.", "It made this"] accent: 1 meta: "run 3 · unedited" clip: "reel" total: 3
           inset: Group{name: "reel" span: [0.5, 3.0] layers: [Rect{size: vec2(1920, 1080) fill: #ff5a1f}]}}
]}"#,
        "Words" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 2.0 bpm: 120 layers: [
    Words{every: beats(0.5) words: ["TIMING", {text: "DEPTH" look: @echo bg: #1f3dff color: #f0ebe0}, {text: "TEXTURE" look: @halftone}]}
]}"#,
        "Chart" => r#"use mod.motion.*
Motion{size: vec2(1280, 720) duration: 5.0 layers: [
    Chart{type: @bar at: vec2(77, 80) size: vec2(1126, 560) title: "Sales" csv: "month,sales\nJan,12\nFeb,19\nMar,15\nApr,26" prefix: "$" suffix: "k"
          land: [1.0, 1.6, 2.2, 2.8] annotate: [[3, "record"]]}
]}"#,
        "Image" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 4.0 inputs: {photo: input(@image)} layers: [
    Image{src: in(@photo) size: vec2(640, 360) fit: @fill zoom: tween(1.0, 1.15, 0.0, 4.0)}
]}"#,
        "Video" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 4.0 inputs: {clip: input(@video)} layers: [
    Video{src: in(@clip) size: vec2(640, 360) fit: @fill in: 2.0 speed: 1.0}
]}"#,
        "Bones" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 3.0 layers: [
    Bones{at: vec2(200, 180) bones: [Bone{name: "upper" length: 120} Bone{name: "lower" length: 100}]
        ik: [IK{end: "lower" target: tween(vec2(200, 40), vec2(120, -100), 0.0, 3.0, @ease_in_out) bend: @ccw}]
        layers: [Path{svg: "M 0 -14 L 220 -14 L 220 14 L 0 14 Z" fill: #ff8a3d}]}
]}"#,
        "Group" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Group{size: vec2(640, 360) mask: Rect{at: vec2(0, 0) size: tween(vec2(0, 360), vec2(640, 360), 0.0, 1.0, @ease_out_cubic)} layers: [
        Rect{at: vec2(0, 0) size: vec2(640, 360) fill: #1a1f2b}
        Text{text: "Revealed" size: 48 color: #ffffff at: vec2(320, 180) align: @center valign: @center}
    ]}
]}"#,
        "Shader" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 4.0 layers: [
    Shader{size: vec2(640, 360) params: {speed: 0.5}
        pixel: fn() {
            let d = length(self.pos - vec2(0.5, 0.5))
            let r = 0.5 + 0.5 * sin(d * 30.0 - self.time * self.speed * 6.0)
            return vec4(r * 0.2, r * 0.5, r, 1.0)
        }}
]}"#,
        "KineticText" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 4.0 layers: [
    KineticText{preset: "313_wave_line" text: "KINETIC" size: vec2(640, 360) p1: 0.5}
]}"#,
        "Text3D" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 4.0 layers: [
    Text3D{preset: @chrome_title text: "3D TYPE" font: Font{family: "Inter" weight: 900} fit: @safe}
]}"#,
        "Seq3D" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 6.0 bpm: 120 layers: [
    Seq3D{template: @logo_sting text: "BRAND" subtitle: "tagline" logo: "M 0 90 L 0 0 L 45 50 L 90 0 L 90 90 Z" accent: #ff5a36}
]}"#,
        "Scene3D" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 4.0 inputs: {model: input(@model)} layers: [
    Scene3D{model: in(@model) size: vec2(640, 360) camera: {yaw: tween(0, 90, 0.0, 4.0) pitch: 15 distance: 3 fov: 40}
            lights: [{type: @directional direction: vec3(-0.5, -1, -0.3) intensity: 3}]}
]}"#,
        "Draw2D" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Draw2D{draw: fn(c, t, edit) {
        for i in 0..24 {
            let a = i / 24 * 6.2832 + t
            c.seg(320, 180, 320 + cos(a) * 140, 180 + sin(a) * 140, 3, #36c2ff, 0.8)
        }
        c.text("Draw2D", 320, 190, {px: 32 align: @center color: #ffffff})
    }}
]}"#,
        "Widget" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Widget{size: vec2(400, 80) at: vec2(120, 140) props: {label: "Loading" progress: tween(0.0, 1.0, 0.0, 2.0)}
        ui: fn(props) {
            View{width: Fill height: Fill flow: Down spacing: 8
                Label{text: props.label draw_text.color: #ffffff}
                View{width: Fill height: 12 show_bg: true draw_bg.color: #36c2ff}
            }
        }}
]}"#,
        "Morph" => r#"use mod.motion.*
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Morph{time: 0.6 duration: 0.5
        from: Text{text: "MAKE" size: 72 at: vec2(80, 140)}
        to: Text{text: "MOVE" size: 120 at: vec2(220, 120)}}
]}"#,
        _ => return crate::procedural::example(kind),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MotionDoc;

    #[test]
    fn every_layer_type_has_fields_and_an_example_that_loads() {
        for kind in layer_types() {
            assert!(layer_fields(kind).iter().any(|f| f == "opacity"), "{kind}");
            let example = layer_example(kind).unwrap_or_else(|| panic!("{kind}: no example"));
            // The 3D layers lower through motion3d (the `scene3d` feature).
            if cfg!(not(feature = "scene3d")) && matches!(kind, "Text3D" | "Seq3D") {
                continue;
            }
            // Text3D's presets come with a host's text pack: with none installed its
            // example is refused for that reason alone.
            if kind == "Text3D" && crate::text3d::presets().is_empty() {
                let diags = MotionDoc::load(example).expect_err("a Text3D without presets");
                assert!(diags.iter().all(|d| d.message.contains("this host has not installed")), "{diags:?}");
                continue;
            }
            if let Err(diags) = MotionDoc::load(example) {
                panic!("{kind}:\n{}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
            }
        }
        assert!(layer_fields("Blob").is_empty());
    }
}
