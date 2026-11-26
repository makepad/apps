//! Full three.js-level Scene3D layers (lane R3's motion3d): Motion keeps
//! the layer's place and time, and motion3d reads the rest of the object
//! from the document's own VM (the document evaluates once).

use makepad_motion::*;
#[cfg(feature = "scene3d")]
use makepad_stage_motion3d::doc::FileResolver;
#[cfg(feature = "scene3d")]
use makepad_stage_motion3d::eval::{EvalCache, EvalInput};

/// The scene a frame's item shows, loaded as the drawing side loads it,
/// evaluated at `t`.
#[cfg(feature = "scene3d")]
fn scene_frame(item: &Item, t: f32) -> makepad_stage_motion3d::frame::SceneFrame {
    let resolver = FileResolver::default();
    let mut scene = makepad_motion::scene::load_scene(&item.kind, "test", &resolver).expect("a scene item").unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    scene.eval(&EvalInput { t, aspect: 16.0 / 9.0, audio: &makepad_stage_motion3d::anim::NoAudio, edit_offset: 0.0 }, &mut EvalCache::default())
}

#[cfg(feature = "scene3d")]
const DOC: &str = r#"use mod.motion.*; use mod.scene; Motion{
    size: vec2(640, 360) duration: 4.0
    layers: [
        Rect{size: vec2(640, 360) fill: #101010}
        Scene3D{
            at: vec2(20, 10) span: [0.5, 3.0] opacity: tween(0, 1, 0.5, 1.0)
            size: vec2(600, 340)
            environment: scene.Environment{preset: @studio}
            camera: Camera{position: vec3(0, 1, 6) target: vec3(0, 0, 0) fov: 35}
            children: [
                scene.Mesh{geometry: scene.BoxGeometry{} material: scene.MeshStandardMaterial{color: #ff5a36} rotation: fn(t, edit) { vec3(0, t * 90, 0) }}
                scene.Model{src: asset("ast_stufm3o7yer634iv7s5tliljom", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f")}
            ]
        }
    ]
}"#;

#[cfg(feature = "scene3d")]
#[test]
fn a_full_scene_is_handed_to_motion3d_with_the_layers_place_kept() {
    let doc = MotionDoc::load(DOC).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let layer = &doc.layers[1];
    let LayerKind::Scene3dDoc { scene, loc, size, assets, .. } = &layer.kind else { panic!("{:?}", layer.kind) };
    assert!(matches!(scene, SceneSource::Object(_)), "the scene is an object of the document's VM");
    assert_eq!(loc.line, 5, "the scene is written at its own line");
    assert!(size.is_some());
    assert_eq!(assets.len(), 1);
    assert!(doc.dependencies().contains(&assets[0]));
    assert!(layer.animated);

    assert!(doc.eval(0.2).items.len() == 1, "not before its span");
    let frame = doc.eval(1.5);
    let ItemKind::Scene3dDoc { local_time, first_line, size, .. } = &frame.items[1].kind else { panic!() };
    assert_eq!((*local_time, *first_line, *size), (1.0, 5, [600.0, 340.0]));
    assert!((frame.items[1].opacity - 1.0).abs() < 1e-9);
}

#[cfg(feature = "scene3d")]
#[test]
fn the_scene_is_read_from_the_documents_vm_with_its_lets_and_helpers() {
    // The scene reads the document's lets and Motion's helpers where it
    // stands (no copy of them: one VM), and evaluates its closures there.
    let doc = MotionDoc::load("use mod.motion.*; use mod.scene; let R = 2.0\nlet spin = fn(t) { return prog(t, 0.0, 1.0) * 90.0 }\nMotion{ size: vec2(100, 100) duration: 2 layers: [\n  Rect{size: vec2(R, R)}\n  Scene3D{\n    camera: Camera{}\n    lights: [scene.AmbientLight{}]\n    children: [scene.Mesh{geometry: scene.BoxGeometry{} position: vec3(R, 0, hash01(1, 2)) rotation: fn(t, edit) { vec3(0, spin(t) + ease(@ease_out_expo, 0.5), 0) }}]\n  }\n]}\n").unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(1.0);
    let f = scene_frame(&frame.items[1], 1.0);
    assert!(!f.meshes.is_empty());
    assert!(f.errors.is_empty(), "{:?}", f.errors);
    // The same scene in another document VM (a fork) is its own object.
    let LayerKind::Scene3dDoc { scene: SceneSource::Object(a), .. } = &doc.layers[1].kind else { panic!() };
    let fork = doc.fork().unwrap();
    let LayerKind::Scene3dDoc { scene: SceneSource::Object(b), .. } = &fork.layers[1].kind else { panic!() };
    assert!(a != b && *a == a.clone());
}

#[cfg(feature = "scene3d")]
#[test]
fn a_scene_uses_motions_animation_forms_and_times() {
    let doc = MotionDoc::load("use mod.motion.*; use mod.scene; Motion{ size: vec2(100, 100) duration: 4 bpm: 120 layers: [\n  Scene3D{\n    camera: Camera{position: tween(vec3(0, 0, 5), vec3(0, 2, 5), 0, 2, @linear) target: vec3(0, 0, 0)}\n    lights: [scene.AmbientLight{}]\n    children: [scene.Mesh{geometry: scene.BoxGeometry{} position: keys([0, vec3(0, 0, 0)], [2, vec3(4, 0, 0)]) rotation: repeat(tween(vec3(0, 0, 0), vec3(0, 90, 0), 0, 1), 1, @pingpong) scale: tween(1, 3, scene.beat(1), scene.beat(2) + 0.25)}\n      scene.Mesh{geometry: scene.BoxGeometry{} scale: scene.follow_music(@energy, 1.0, 2.0)}]\n  }\n]}\n").unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(1.0);
    let f = scene_frame(&frame.items[0], 1.0);
    assert!(f.errors.is_empty(), "{:?}", f.errors);
    // scene.beat(1) is 0.5 s at 120 bpm: the scale tween runs 0.5 .. 1.25 s, so it is 2.333 at 1 s.
    let v = &f.meshes[0].instances[0].transform.v;
    let scale = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    assert!((scale - (1.0 + 2.0 * (0.5 / 0.75))).abs() < 1e-3, "{scale}");
    let m = &f.meshes[0].instances[0].transform.v;
    assert!((m[12] - 2.0).abs() < 1e-4, "keys: {}", m[12]);
    assert!((f.camera.position[1] - 1.0).abs() < 1e-4, "tween: {:?}", f.camera.position);
}

#[cfg(feature = "scene3d")]
#[test]
fn a_scenes_music_follows_the_audio_and_the_documents_music_is_the_track() {
    let doc = MotionDoc::load("use mod.motion.*; use mod.scene; Motion{ size: vec2(100, 100) duration: 4 layers: [\n  Scene3D{\n    camera: Camera{}\n    lights: [scene.AmbientLight{}]\n    children: [scene.Mesh{geometry: scene.BoxGeometry{} position: fn(t, edit) { vec3(music(@m).beat * 10.0, music(@m).energy, 0) }}]\n  }\n]}\n").unwrap_or_else(|d| panic!("{d:?}"));
    let frame = doc.eval(1.0);
    let resolver = FileResolver::default();
    let mut scene = makepad_motion::scene::load_scene(&frame.items[0].kind, "test", &resolver).unwrap().unwrap_or_else(|d| panic!("{d:?}"));
    struct Loud;
    impl makepad_stage_motion3d::anim::AudioSource for Loud {
        fn sample(&self, _source: Option<&str>, feature: &makepad_stage_motion3d::anim::AudioFeature, _t: f32) -> f32 {
            match feature {
                makepad_stage_motion3d::anim::AudioFeature::BeatPulse => 0.5,
                makepad_stage_motion3d::anim::AudioFeature::Energy => 0.25,
                _ => 0.0,
            }
        }
    }
    let f = scene.eval(&EvalInput { t: 1.0, aspect: 1.0, audio: &Loud, edit_offset: 0.0 }, &mut EvalCache::default());
    let m = &f.meshes[0].instances[0].transform.v;
    assert!((m[12] - 5.0).abs() < 1e-4 && (m[13] - 0.25).abs() < 1e-4, "{:?}", &m[12..15]);
    // The document's own `music(@m)` outside a scene's closure is its track.
    let two_d = MotionDoc::load("use mod.motion.*; Motion{ size: vec2(100, 100) duration: 4 layers: [Rect{size: vec2(10, 10) x: music(@m).bpm}] }").unwrap();
    assert!(matches!(two_d.layers[0].kind, LayerKind::Rect { .. }));
}

#[test]
fn a_simple_scene_stays_motions_own() {
    let doc = MotionDoc::load("use mod.motion.*; Motion{size: vec2(640, 360) duration: 2.0 inputs: {m: input(@model)} layers: [Scene3D{model: in(@m) camera: {yaw: 30 distance: 3}}]}").unwrap();
    assert!(matches!(doc.layers[0].kind, LayerKind::Scene3D { .. }));
}

#[cfg(feature = "scene3d")]
#[test]
fn a_full_scene_gets_the_documents_bpm_and_its_host_pictures() {
    let doc = MotionDoc::load(r#"use mod.motion.*; use mod.scene; Motion{
    size: vec2(640, 360) duration: 4.0 bpm: 96
    layers: [
        Scene3D{
            textures: {screen: asset("ast_stufm3o7yer634iv7s5tliljom", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f")}
            videos: {app: asset("ast_aaaaaaaaaaaaaaaaaaaaaaaaaa", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f")}
            environment: scene.Environment{preset: @studio}
            children: [scene.Mesh{geometry: scene.BoxGeometry{} material: scene.MeshStandardMaterial{texture: "screen"}}]
        }
    ]
}"#).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let LayerKind::Scene3dDoc { textures, videos, .. } = &doc.layers[0].kind else { panic!("{:?}", doc.layers[0].kind) };
    // (The scene reads without a warning for the layer's own fields.)
    scene_frame(&doc.eval(1.0).items[0], 1.0);
    assert_eq!((textures.len(), videos.len()), (1, 1));
    assert_eq!(doc.dependencies().len(), 2);
    let ItemKind::Scene3dDoc { bpm, textures, .. } = &doc.eval(1.0).items[0].kind else { panic!() };
    assert_eq!(*bpm, 96.0);
    assert_eq!(textures[0].0, "screen");
}

#[cfg(feature = "scene3d")]
#[test]
fn a_full_scene_draws_2d_layers_into_named_textures() {
    let doc = MotionDoc::load(r#"use mod.motion.*; use mod.scene; Motion{
    size: vec2(640, 360) duration: 4.0
    layers: [
        Scene3D{
            layers: {intro: Group{layers: [Rect{size: vec2(640, 360) fill: #ff5a36} Text{text: "Intro" size: 80 at: vec2(320, 180) align: @center animate: @fade_up}]}
                     main: Text{text: "Main" size: 80 at: vec2(320, 180) align: @center}}
            environment: scene.Environment{preset: @studio}
            children: [scene.Mesh{geometry: scene.BoxGeometry{} material: scene.MeshStandardMaterial{texture: "intro"}}]
        }
    ]
}"#).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")));
    let LayerKind::Scene3dDoc { layers, .. } = &doc.layers[0].kind else { panic!() };
    assert_eq!(layers.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["intro", "main"]);
    let ItemKind::Scene3dDoc { layers, .. } = &doc.eval(2.0).items[0].kind else { panic!() };
    assert_eq!(layers.len(), 2);
    assert!(layers.iter().all(|(_, items)| items.len() == 1), "each name draws its layer at the scene's time");
}
