//! A document written before vocabulary modules, brought up to date by
//! `upgrade::namespaces`: the `use` lines it needs and its scene, post and
//! kernel names qualified; it then loads, and a second pass changes nothing.
#![cfg(feature = "scene3d")]
use makepad_motion::upgrade::{kind_of, namespaces, Vocabularies};
use makepad_motion::MotionDoc;

const OLD: &str = r#"let glow = Glow{strength: 0.4}
Motion{size: vec2(320, 180) duration: 2.0 post: [glow] layers: [
    Rect{size: vec2(10, 10) fill: #fff opacity: tween(0, 1, 0, 1)}
    Scene3D{size: vec2(320, 180) camera: Camera{position: vec3(0, 0, 5) target: vec3(0, 0, 0)}
        lights: [AmbientLight{}]
        children: [Mesh{geometry: BoxGeometry{} scale: music(@beat, 1, 1.4) position: tween(vec3(0, 0, 0), vec3(1, 0, 0), beat(1), beat(2))}]}
]}
"#;

#[test]
fn an_old_document_is_brought_up_to_date_and_loads() {
    assert!(MotionDoc::load(OLD).is_err(), "the old document should not load as it is");
    let vocab = Vocabularies::installed();
    let kind = kind_of(OLD, false).unwrap();
    let new = namespaces(OLD, kind, &vocab).expect("it needs upgrading");
    assert!(new.starts_with("use mod.motion.*; use mod.scene; use mod.post\n"), "{new}");
    for written in ["post.Glow{", "scene.AmbientLight{", "scene.Mesh{", "scene.BoxGeometry{", "scene.follow_music(@beat, 1, 1.4)", "scene.beat(1)", "Camera{position"] {
        assert!(new.contains(written), "no `{written}` in:\n{new}");
    }
    // Motion's own names stay as they are (`tween`, `Rect`, the 2D Camera type is Motion's too).
    assert!(new.contains("opacity: tween(0, 1, 0, 1)"), "{new}");
    if let Err(diags) = MotionDoc::load(&new) {
        panic!("{}\n{new}", diags.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"));
    }
    assert_eq!(namespaces(&new, kind, &vocab), None, "a second pass changes nothing");
}
