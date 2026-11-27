//! Document-level tests: load real Scene3D sources and check the frames.

use crate::anim::{NoAudio, SyntheticMusic};
use crate::doc::{FileResolver, LoadOptions, Scene3dDoc};
use crate::eval::{EvalCache, EvalInput};
use crate::frame::*;
use makepad_geom3d::math::transform_point;
use std::collections::HashMap;

fn load(src: &str) -> Scene3dDoc {
    let resolver = FileResolver::default();
    match Scene3dDoc::load(src, LoadOptions { name: "test".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }) {
        Ok(d) => d,
        Err(e) => panic!("{}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")),
    }
}

fn eval(doc: &mut Scene3dDoc, t: f32) -> SceneFrame {
    let mut cache = EvalCache::default();
    doc.eval(&EvalInput { t, aspect: 16.0 / 9.0, audio: &NoAudio, edit_offset: 0.0 }, &mut cache)
}

const KITCHEN_SINK: &str = r#"
use mod.scene.*; use mod.post; Scene3D{
    background: #101218
    environment: Environment{preset: @studio intensity: 0.8}
    fog: Fog{color: #101218 near: 10 far: 40}
    camera: Camera{position: vec3(0, 2, 8) target: vec3(0, 0, 0) fov: 40}
    tone_mapping: @aces
    post: [post.Bloom{strength: 0.5} SSAO{radius: 0.4} post.DepthOfField{focus: "hero" aperture: 0.05} post.Outline{color: #ffcc00}]
    lights: [
        AmbientLight{intensity: 0.2}
        DirectionalLight{position: vec3(4, 8, 3) intensity: 3 cast_shadow: true shadow_softness: 3}
        SpotLight{position: vec3(-3, 5, 2) target: vec3(0, 0, 0) angle: 25 penumbra: 0.3 cast_shadow: true}
        PointLight{position: vec3(0, 3, 0) color: #ff8040 intensity: 30 distance: 12}
    ]
    children: [
        Group3D{
            name: "rig"
            rotation: tween(vec3(0, 0, 0), vec3(0, 90, 0), 0, 2, @ease_in_out_cubic)
            children: [
                Mesh{name: "hero" outline: true geometry: TorusKnotGeometry{radius: 1 tube: 0.3} material: MeshPhysicalMaterial{color: #ff5a36 roughness: 0.2 clearcoat: 1 transmission: 0.0}}
                Mesh{position: vec3(2, 0, 0) geometry: BoxGeometry{width: 1 height: 1 depth: 1 radius: 0.1} material: MeshToonMaterial{color: #33aaff steps: 4}}
            ]
        }
        Mesh{geometry: PlaneGeometry{width: 20 height: 20} rotation: vec3(-90, 0, 0) position: vec3(0, -1.2, 0) material: MeshStandardMaterial{color: #808080 roughness: 0.9}}
        Text3D{text: "HI THERE" size: 0.5 depth: 0.1 position: vec3(0, 2, 0) by: @letter stagger: 0.1
               each: {position: tween(vec3(0, -1, 0), vec3(0, 0, 0), 0, 0.5, @ease_out_back) opacity: tween(0, 1, 0, 0.3)}
               material: MeshStandardMaterial{color: #ffffff metalness: 1 roughness: 0.3}}
        Instances{geometry: SphereGeometry{radius: 0.05 width_segments: 8 height_segments: 6} layout: GridLayout{count: vec3(20, 20, 1) spacing: vec3(0.3, 0.3, 1)}
                  stagger: 0.01 order: @center each: {scale: tween(0, 1, 0, 0.5)} color_jitter: 0.3 material: MeshStandardMaterial{color: #44ff88}}
        Points{layout: SphereLayout{count: 500 radius: 4} size: 0.05 color: #aaddff}
        Particles{count: 300 rate: 100 lifetime: 2 speed: 2 spread: 30 direction: vec3(0, 1, 0) gravity: vec3(0, -2, 0)}
        Line3D{curve: HelixCurve{radius: 2 height: 3 turns: 4} width: 3 color: #ff00ff trim_end: tween(0, 1, 0, 2)}
        Mesh{geometry: TubeGeometry{path: CatmullRomCurve3{points: [vec3(-2, 0, 0), vec3(0, 1, 1), vec3(2, 0, 0)]} radius: 0.05 draw_end: tween(0, 1, 0, 1)}}
        Mesh{geometry: ExtrudeGeometry{shape: StarShape{points: 5 inner: 0.4 outer: 1} depth: 0.2 bevel_size: 0.05 bevel_thickness: 0.05} position: vec3(-3, 0, 0)}
        Mesh{geometry: IcosahedronGeometry{radius: 0.8 detail: 3} displace: 0.2 displace_speed: 0.5 position: vec3(3, 1, 0)
             material: ShaderMaterial{uniforms: {amount: tween(0, 1, 0, 2)} surface: fn(p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return vec4(n * 0.5 + 0.5, 1.0) }}}
    ]
}
"#;

#[test]
fn kitchen_sink_loads_and_evaluates() {
    let mut doc = load(KITCHEN_SINK);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings.iter().map(|d| d.to_string()).collect::<Vec<_>>());
    let f0 = eval(&mut doc, 0.0);
    let f1 = eval(&mut doc, 1.0);
    assert_eq!(f1.lights.len(), 3);
    assert!(f1.lights.iter().filter(|l| l.shadow.is_some()).count() == 2);
    assert!(f1.env.is_some() && f1.fog.is_some());
    assert!(f1.post.bloom.is_some() && f1.post.ssao.is_some() && f1.post.dof.is_some() && f1.post.outline.is_some());
    // DOF focuses on "hero" at the origin: the camera is ~8.2 away.
    assert!((f1.post.dof.unwrap().0 - (68.0f32).sqrt()).abs() < 0.01);
    // Instances: one draw of 400.
    assert!(f1.meshes.iter().any(|m| m.instance_count() == 400));
    // Letters appear over time.
    let letters = |f: &SceneFrame| f.meshes.iter().filter(|m| m.material.metalness == 1.0).count();
    assert!(letters(&f0) < letters(&f1), "{} {}", letters(&f0), letters(&f1));
    assert_eq!(f1.points.len(), 2);
    assert_eq!(f1.lines.len(), 1);
    assert!(f1.meshes.iter().any(|m| m.material.shader.is_some() && m.deform[0] > 0.0));
    assert!(f1.meshes.iter().any(|m| m.dynamic), "the tube draws on");
    assert!(f1.meshes.iter().any(|m| m.outline));
    // The rig turned: its box went from +x toward -z.
    let boxes = |f: &SceneFrame| f.meshes.iter().find(|m| m.material.model == crate::scene::ShadingModel::Toon).map(|m| transform_point(&m.instances[0].transform, [0.0; 3])).unwrap();
    assert!((boxes(&f0)[0] - 2.0).abs() < 1e-4);
    let p2 = boxes(&eval(&mut doc, 2.0));
    assert!(p2[0].abs() < 1e-3 && (p2[2] + 2.0).abs() < 1e-3, "{p2:?}");
}

#[test]
fn frames_are_pure_functions_of_t() {
    let mut a = load(KITCHEN_SINK);
    let mut b = load(KITCHEN_SINK);
    let fa = eval(&mut a, 1.37);
    // b visits other times first: no state may leak between frames.
    eval(&mut b, 0.2);
    eval(&mut b, 5.0);
    let fb = eval(&mut b, 1.37);
    assert_eq!(fa.meshes.len(), fb.meshes.len());
    for (x, y) in fa.meshes.iter().zip(fb.meshes.iter()) {
        assert_eq!(x.instances, y.instances);
        assert_eq!(x.geo_key, y.geo_key);
    }
    assert_eq!(fa.points[0].points, fb.points[0].points);
    assert_eq!(fa.points[1].points, fb.points[1].points);
    assert_eq!(fa.camera.view.v, fb.camera.view.v);
}

#[test]
fn diagnostics_teach_and_locate() {
    let resolver = FileResolver::default();
    let src = "use mod.scene.*; Scene3D{\n  children: [\n    Mesh{geometry: SphereGeometry{radius: 1 widthSegments: 8} material: MeshStandardMaterial{metalnes: 1}}\n    Mesh{geometry: BoxGeometry{} material: MeshStandardMaterial{roughness: tween(1, 0, 2, 1)}}\n  ]\n}";
    let err = match Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 10, origin_file: None, song: None }) {
        Ok(_) => panic!("should fail"),
        Err(e) => e,
    };
    let text: Vec<String> = err.iter().map(|d| d.to_string()).collect();
    assert!(text.iter().any(|t| t.contains("width_segments")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("metalness")), "{text:?}");
    let e = err.iter().find(|d| d.is_error()).unwrap();
    assert!(e.message.contains("before it starts"), "{text:?}");
    assert_eq!(e.loc.line, 13, "{text:?}");
    // A syntax error reports its line in the host's numbering.
    let bad = Scene3dDoc::load("use mod.scene.*; Scene3D{\n  children: [Mesh{geometry: }\n}", LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None });
    assert!(bad.is_err());
}

#[test]
fn audio_binding_stays_on_the_edit_grid() {
    let src = "use mod.scene.*; Scene3D{ camera: Camera{} lights: [AmbientLight{}] children: [Mesh{geometry: BoxGeometry{} scale: follow_music(@beat, 1, 2)}] }";
    let mut doc = load(src);
    let music = SyntheticMusic::default();
    let mut cache = EvalCache::default();
    let scale_at = |doc: &mut Scene3dDoc, cache: &mut EvalCache, t: f32, off: f32| {
        let f = doc.eval(&EvalInput { t, aspect: 1.0, audio: &music, edit_offset: off }, cache);
        f.meshes[0].instances[0].transform.v[0]
    };
    // 120 bpm: beats at edit 0, 0.5, 1.0 ... A clip placed at edit 0.25
    // pulses at its local 0.25, not at its local 0.
    assert!((scale_at(&mut doc, &mut cache, 0.0, 0.0) - 2.0).abs() < 1e-4);
    assert!(scale_at(&mut doc, &mut cache, 0.0, 0.25) < 1.5);
    assert!((scale_at(&mut doc, &mut cache, 0.25, 0.25) - 2.0).abs() < 1e-4);
}

#[test]
fn closures_drive_values_and_instances() {
    let src = "use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [
        Mesh{geometry: BoxGeometry{} position: fn(t) { vec3(t * 2.0, 0.0, 0.0) }}
        Instances{geometry: BoxGeometry{} count: 4 layout: LineLayout{count: 4}
                  each: fn(i, n, t) { return {position: vec3(i, t, 0.0) scale: vec3(0.5, 0.5, 0.5)} }}
    ] }";
    let mut doc = load(src);
    let f = eval(&mut doc, 1.5);
    assert!((f.meshes[0].instances[0].transform.v[12] - 3.0).abs() < 1e-5);
    let inst = &f.meshes[1].instances;
    assert_eq!(inst.len(), 4);
    assert!((inst[3].transform.v[12] - 3.0).abs() < 1e-5 && (inst[3].transform.v[13] - 1.5).abs() < 1e-5);
    assert!((inst[0].transform.v[0] - 0.5).abs() < 1e-5);
}

#[test]
fn memo_computes_shared_state_once_per_evaluation() {
    let src = "use mod.scene.*; let mk = fn(t) { return {x: t} }
Scene3D{ lights: [AmbientLight{}] children: [
        Mesh{geometry: BoxGeometry{} position: fn(t) { if memo(mk, t) == memo(mk, t) { vec3(memo(mk, t).x, 0.0, 0.0) } else { vec3(0.0, 5.0, 0.0) } }}
    ] }";
    let mut doc = load(src);
    let f = eval(&mut doc, 1.5);
    assert!((f.meshes[0].instances[0].transform.v[12] - 1.5).abs() < 1e-5 && f.meshes[0].instances[0].transform.v[13].abs() < 1e-5, "{:?}", &f.meshes[0].instances[0].transform.v[12..15]);
    let f = eval(&mut doc, 2.0);
    assert!((f.meshes[0].instances[0].transform.v[12] - 2.0).abs() < 1e-5);
}

#[test]
fn closures_see_the_hosts_exact_frame_time() {
    // 36.6 is not an f32: a closure comparing against it misses the frame
    // exactly at 36.6 unless the host's f64 time reaches it.
    let src = "use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [
        Mesh{geometry: BoxGeometry{} position: fn(t) { if t >= 36.6 { vec3(1.0, 0.0, 0.0) } else { vec3(0.0, 0.0, 0.0) } }}
    ] }";
    let mut doc = load(src);
    assert!((eval(&mut doc, 36.6).meshes[0].instances[0].transform.v[12]).abs() < 1e-5, "f32 time alone is before 36.6");
    doc.set_exact_time(Some(36.6));
    assert!((eval(&mut doc, 36.6).meshes[0].instances[0].transform.v[12] - 1.0).abs() < 1e-5, "the exact frame time fires on the frame");
    // Another time (a sub-frame) keeps its own clock.
    assert!((eval(&mut doc, 36.5).meshes[0].instances[0].transform.v[12]).abs() < 1e-5);
}

#[test]
fn document_closures_are_bounded_and_cannot_keep_state() {
    // `hits` would count frames; the document is frozen after its load, so
    // the write is refused (and reported) and every frame reads 0.
    let src = "use mod.scene.*; var hits = 0
Scene3D{ lights: [AmbientLight{}] children: [
    Mesh{geometry: BoxGeometry{} position: fn(t) { hits = hits + 1 vec3(t + hits, 0.0, 0.0) }}
    Mesh{geometry: BoxGeometry{} position: fn(t) { if t > 2.0 { loop {} } vec3(0.0, t, 0.0) }}
] }";
    let mut doc = load(src);
    let a = eval(&mut doc, 1.0).meshes[0].instances[0].transform.v[12];
    eval(&mut doc, 0.5);
    let b = eval(&mut doc, 1.0).meshes[0].instances[0].transform.v[12];
    assert_eq!((a, b), (1.0, 1.0));
    let errors = doc.take_eval_errors();
    assert!(!errors.is_empty(), "the write into the frozen document was not reported");
    // A runaway keyable is stopped by the frame limits and reported.
    // The frame returning at all shows the loop was stopped; no wall-clock
    // bound is asserted (it fails on a loaded box). The stop is the frame
    // limits': the instruction limit, or the time budget.
    eval(&mut doc, 3.0);
    let errs = doc.take_eval_errors();
    assert!(errs.iter().any(|e| e.contains("instruction limit") || e.contains("time budget")), "{errs:?}");
}

#[test]
fn m1_scene3d_shorthand_is_accepted() {
    // M1's Scene3D fields (camera orbit, Light3D, exposure) read as-is.
    let src = "use mod.scene.*; Scene3D{ camera: Camera3D{orbit: true yaw: 30 pitch: 10 distance: 5 fov: 40} lights: [Light3D{direction: vec3(-1, -1, -1) intensity: 2}] exposure: 1.2 children: [Mesh{geometry: SphereGeometry{}}] }";
    let mut doc = load(src);
    let f = eval(&mut doc, 0.0);
    assert!((makepad_geom3d::mesh::len3(f.camera.position) - 5.0).abs() < 1e-3);
    assert_eq!(f.lights[0].kind, LightType::Directional);
    assert!((f.exposure - 1.2).abs() < 1e-6);
}

#[test]
fn kits_camera_moves_and_beat_times() {
    let src = "use mod.scene.*; Scene3D{ bpm: 120 camera: Camera{position: vec3(0, 0, 10) target: vec3(0, 0, 0) moves: [CameraMove{type: @push_in start: beat(0) end: beat(4) amount: 0.5 ease: @linear}]}
        lights: [LightRig{preset: @product}]
        children: [
            Device{kind: @laptop open: tween(0, 100, 0, 1)} Device{kind: @phone screen: \"app\"} Device{kind: @tablet} Device{kind: @monitor screen: @off}
            Turntable{children: [ShapeSet{preset: @rings}]}
            Field{preset: @galaxy count: 500} Field{preset: @swarm count: 100} Field{preset: @wave_grid grid: 10} Field{preset: @pillars grid: 10 react: @low}
            Field{preset: @confetti count: 50} Field{preset: @dots grid: 10}
            ShapeSet{preset: @floating} ShapeSet{preset: @stack} ShapeSet{preset: @blobs}
            Ground{shadow_only: true}
        ] }";
    let mut doc = load(src);
    let f0 = eval(&mut doc, 0.0);
    let f1 = eval(&mut doc, 1.0);
    let f2 = eval(&mut doc, 2.0);
    // beat(4) at 120 bpm is 2 s: halfway at 1 s.
    assert!((makepad_geom3d::mesh::len3(f0.camera.position) - 10.0).abs() < 1e-3);
    assert!((makepad_geom3d::mesh::len3(f1.camera.position) - 7.5).abs() < 1e-3, "{:?}", f1.camera.position);
    assert!((makepad_geom3d::mesh::len3(f2.camera.position) - 5.0).abs() < 1e-3);
    assert!(f1.meshes.iter().any(|m| m.material.external.as_deref() == Some("app")));
    assert!(f1.meshes.iter().any(|m| m.material.shadow_catcher));
    assert!(f1.meshes.iter().any(|m| m.instance_count() >= 100));
    assert!(!f1.points.is_empty());
    // Isometric is orthographic at 45 degrees of yaw.
    let mut iso = load("use mod.scene.*; Scene3D{ camera: Camera{preset: @isometric} lights: [AmbientLight{}] children: [] }");
    let fi = eval(&mut iso, 0.0);
    assert!(fi.camera.orthographic);
    assert!((fi.camera.position[0] - fi.camera.position[2]).abs() < 1e-3);
}

#[test]
fn kit_errors_teach() {
    let resolver = FileResolver::default();
    let err = Scene3dDoc::load("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Field{preset: @galaxxy} Device{kind: @phnoe} Camera{}] camera: Camera{move: @push_inn} }", LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }).err().unwrap();
    let text: Vec<String> = err.iter().map(|d| d.to_string()).collect();
    assert!(text.iter().any(|t| t.contains("@galaxy")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("@phone")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("push_in")), "{text:?}");
}

#[test]
fn every_fx_preset_loads_and_evaluates() {
    // The tree, metaballs and ribbons grow on the effect pack's engines, which a host installs; none is here, so
    // they grow nothing, and a static one says its geometry is empty (the pack's own tests grow them).
    for name in crate::kits::FX {
        let src = format!("use mod.scene.*; Scene3D{{ look: @studio camera: Camera{{}} children: [Fx{{preset: @{name} count: 200 start: 0.2 from: \"a\" to: \"b\"}}] }}");
        let mut doc = load(&src);
        let engine = matches!(*name, "tree" | "metaballs" | "ribbons");
        assert!(doc.warnings.iter().all(|d| engine && d.to_string().contains("the geometry is empty")), "{name}: {:?}", doc.warnings.iter().map(|d| d.to_string()).collect::<Vec<_>>());
        let a = eval(&mut doc, 0.9);
        let b = eval(&mut doc, 0.9);
        assert!(!a.meshes.is_empty() || !a.points.is_empty() || engine || name == &"burst" || name == &"sparks", "{name} draws nothing");
        assert_eq!(a.meshes.iter().map(|m| m.instances.clone()).collect::<Vec<_>>(), b.meshes.iter().map(|m| m.instances.clone()).collect::<Vec<_>>(), "{name} is deterministic");
    }
}

#[test]
fn looks_restyle_materials() {
    for look in crate::scene::Look::NAMES {
        let mut doc = load(&format!("use mod.scene.*; Scene3D{{ look: @{look} children: [Mesh{{geometry: SphereGeometry{{}} material: MeshPhysicalMaterial{{color: #ff0000}}}}] }}"));
        let f = eval(&mut doc, 0.0);
        assert!(f.clear.is_some(), "{look}");
        let m = &f.meshes[0].material;
        match *look {
            "clay" => assert!(m.roughness > 0.7 && m.color[0] < 0.9),
            "blueprint" => assert!(m.wireframe),
            "holographic" => assert!(m.rim_power > 0.0),
            "toon" => assert_eq!(m.model, crate::scene::ShadingModel::Toon),
            "ink" => assert!(m.model == crate::scene::ShadingModel::Toon && (m.color[0] - m.color[1]).abs() < 1e-6),
            _ => assert_eq!(m.color, [1.0, 0.0, 0.0, 1.0]),
        }
    }
}

#[test]
fn morph_scatter_and_trails_move_as_asked() {
    let mut doc = load("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Instances{geometry: BoxGeometry{} layout: LineLayout{count: 3 start: vec3(0, 0, 0) end: vec3(2, 0, 0)} morph_to: LineLayout{count: 3 start: vec3(0, 5, 0) end: vec3(2, 5, 0)} morph: tween(0, 1, 0, 1, @linear) scatter: tween(1, 0, 0, 1, @linear)} Particles{count: 50 rate: 50 speed: 0 gravity: vec3(0, 0, 0) path: LineCurve3{points: [vec3(0, 0, 0), vec3(10, 0, 0)]} path_duration: 1}] }");
    let f1 = eval(&mut doc, 1.0);
    // At t = 1: fully morphed, fully assembled.
    assert!((f1.meshes[0].instances[0].transform.v[13] - 5.0).abs() < 1e-3, "{}", f1.meshes[0].instances[0].transform.v[13]);
    let f0 = eval(&mut doc, 0.0);
    assert!((f0.meshes[0].instances[0].transform.v[12] - 0.0).abs() > 0.01, "scattered at t = 0");
    // Trail: particles born earlier sit further back along the path.
    let f = eval(&mut doc, 0.5);
    let xs: Vec<f32> = f.points[0].points.iter().map(|p| p.pos[0]).collect();
    assert!(xs.iter().cloned().fold(f32::MAX, f32::min) < xs.iter().cloned().fold(f32::MIN, f32::max) - 1.0, "{xs:?}");
}

#[test]
fn voxelize_fills_a_sphere() {
    let m = makepad_geom3d::SphereGeo { radius: 1.0, ..Default::default() }.build();
    let v = crate::doc::voxelize(&m, 10);
    // A sphere fills about pi/6 of its bounding cube.
    let frac = v.len() as f32 / 1000.0;
    assert!((frac - 0.52).abs() < 0.1, "{frac}");
    assert!(v.iter().all(|p| makepad_geom3d::mesh::len3(*p) < 1.01));
}

#[test]
fn every_seq3d_template_builds_loads_and_evaluates() {
    for (name, _) in crate::seq3d::templates() {
        let src = format!("use mod.scene.*; Seq3D{{template: @{name} text: \"HELLO\" subtitle: \"world\" image: \"clip\" bpm: 120 points: [vec2(10, 20), vec2(-30, 100), vec2(50, -60)] from: 3 to: 1 logo: \"M 0 0 L 10 0 L 5 10 Z\"}}");
        let mut doc = load(&src);
        assert!(doc.warnings.is_empty(), "{name}: {:?}", doc.warnings.iter().map(|d| d.to_string()).collect::<Vec<_>>());
        let a = eval(&mut doc, 2.5);
        assert!(!a.meshes.is_empty(), "{name} draws nothing");
        assert_eq!(a.meshes.iter().map(|m| m.instances.clone()).collect::<Vec<_>>(), eval(&mut doc, 2.5).meshes.iter().map(|m| m.instances.clone()).collect::<Vec<_>>(), "{name} is deterministic");
    }
}

#[test]
fn abstract_loop_is_seamless() {
    let mut doc = load("use mod.scene.*; Seq3D{template: @abstract_loop duration: 6}");
    let (a, b) = (eval(&mut doc, 0.0), eval(&mut doc, 6.0));
    for (u, v) in a.camera.view.v.iter().zip(b.camera.view.v.iter()) {
        assert!((u - v).abs() < 1e-3, "camera {u} vs {v}");
    }
    assert_eq!(a.meshes.len(), b.meshes.len());
    for (x, y) in a.meshes.iter().zip(b.meshes.iter()) {
        for (u, v) in makepad_render::TransformTint::floats(&x.instances).iter().zip(makepad_render::TransformTint::floats(&y.instances)) {
            assert!((u - v).abs() < 2e-3, "{u} vs {v}");
        }
    }
}

#[test]
fn screen_pictures_never_stretch() {
    use crate::shared::uv_placement;
    use crate::scene::MapFit;
    let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-4);
    // A 16:9 app run on a 9:19.5 phone, covered: full height, a centred slice.
    let phone = 9.0 / 19.5;
    let (uv, bars) = uv_placement(MapFit::Cover, phone, Some((1920.0, 1080.0)), None);
    let w = 1080.0 * phone / 1920.0;
    assert!(close(uv, [(1.0 - w) * 0.5, 0.0, w, 1.0]) && !bars, "{uv:?}");
    // A crop keeps its region (a reframe may zoom); grown to the aspect, clamped.
    let (uv, _) = uv_placement(MapFit::Cover, phone, Some((1920.0, 1080.0)), Some([1700.0, 0.0, 200.0, 1080.0]));
    assert!(close(uv, [1.0 - w, 0.0, w, 1.0]), "{uv:?}");
    // A phone-size run on a screen of its own aspect: exactly the whole picture.
    let (uv, _) = uv_placement(MapFit::Cover, 1080.0 / 1920.0, Some((1080.0, 1920.0)), None);
    assert!(close(uv, [0.0, 0.0, 1.0, 1.0]), "{uv:?}");
    // Contain letterboxes; fill stretches.
    let (uv, bars) = uv_placement(MapFit::Contain, 1.0, Some((200.0, 100.0)), None);
    assert!(close(uv, [0.0, -0.5, 1.0, 2.0]) && bars, "{uv:?}");
    assert!(close(uv_placement(MapFit::Fill, 1.0, Some((200.0, 100.0)), None).0, [0.0, 0.0, 1.0, 1.0]));
}

#[test]
fn device_screens_cover_by_default_and_take_crop_keys() {
    let mut doc = load("use mod.scene.*; Scene3D{ children: [Device{kind: @phone screen: \"clip\" aspect: 9 / 16 crop: keys([0, [0, 0, 100, 100]], [1, [100, 0, 100, 100], @pin])}] }");
    let f = eval(&mut doc, 0.5);
    let screen = f.meshes.iter().find(|m| m.material.external.as_deref() == Some("clip")).expect("the screen");
    assert_eq!(screen.material.fit, crate::scene::MapFit::Cover);
    assert!((screen.material.aspect - 9.0 / 16.0).abs() < 1e-4);
    assert_eq!(screen.material.crop, Some([50.0, 0.0, 100.0, 100.0]));
}

#[test]
fn toon_and_ink_looks_band_ink_and_paper() {
    for look in ["toon", "ink"] {
        let mut doc = load(&format!("use mod.scene.*; Scene3D{{ look: @{look} children: [Mesh{{geometry: SphereGeometry{{}} material: MeshStandardMaterial{{color: #ff5a36}}}} Mesh{{geometry: PlaneGeometry{{width: 9 height: 9}} rotation: vec3(-90, 0, 0)}}] }}"));
        let f = eval(&mut doc, 0.0);
        let sphere = &f.meshes[0];
        assert_eq!(sphere.material.model, crate::scene::ShadingModel::Toon, "{look}");
        assert_eq!(sphere.material.steps, if look == "ink" { 2 } else { 3 });
        // The ink hull follows the sphere: reversed winding, drawn in ink.
        let hull = f.meshes.iter().find(|m| m.ink > 0.0).expect("an ink hull");
        assert_eq!(hull.mesh.positions.len(), sphere.mesh.positions.len());
        assert_eq!(&hull.mesh.indices[..3], &[sphere.mesh.indices[0], sphere.mesh.indices[2], sphere.mesh.indices[1]]);
        assert_eq!(hull.material.model, crate::scene::ShadingModel::Basic);
        assert!(!hull.cast_shadow);
        assert!(f.post.paper.is_some(), "{look} brings paper");
    }
    // An ink width on any material, without a look.
    let mut doc = load("use mod.scene.*; Scene3D{ children: [Mesh{geometry: BoxGeometry{} material: MeshStandardMaterial{ink: 4 ink_color: #102030}}] }");
    let f = eval(&mut doc, 0.0);
    assert_eq!(f.meshes.iter().filter(|m| m.ink == 4.0).count(), 1);
}

#[test]
fn crowd_people_line_up_and_hop_on_the_beat() {
    let src = "use mod.scene.*; Scene3D{ bpm: 120 children: [Crowd{count: 60 shape: @block variants: 2 react: @beat hop: 0.3 bob: 0 signs: 0.2}] }";
    let mut doc = load(src);
    let music = SyntheticMusic::default();
    let at = |doc: &mut Scene3dDoc, t: f32| {
        let mut cache = EvalCache::default();
        doc.eval(&EvalInput { t, aspect: 16.0 / 9.0, audio: &music, edit_offset: 0.0 }, &mut cache)
    };
    let f = at(&mut doc, 2.0);
    let body_verts = f.meshes[0].mesh.positions.len();
    let people: usize = f.meshes.iter().filter(|m| m.ink == 0.0 && m.mesh.positions.len() == body_verts).map(|m| m.instance_count()).sum();
    assert_eq!(people, 60, "the variants split the crowd between the body meshes");
    // On the beat everyone is up; between beats back down.
    let y = |f: &SceneFrame| f.meshes[0].instances[0].transform.v[13];
    let (on, off) = (at(&mut doc, 2.0), at(&mut doc, 2.24));
    assert!(y(&on) > y(&off) + 0.05, "{} vs {}", y(&on), y(&off));
    assert_eq!(at(&mut doc, 2.0).meshes[0].instances, on.meshes[0].instances, "deterministic");
}

#[test]
fn spotlights_slam_on_with_hits() {
    let mut doc = load("use mod.scene.*; Scene3D{ bpm: 120 children: [Spotlights{count: 2 react: @beat}] }");
    let music = SyntheticMusic::default();
    let cone_opacity = |t: f32, doc: &mut Scene3dDoc| {
        let mut cache = EvalCache::default();
        let f = doc.eval(&EvalInput { t, aspect: 16.0 / 9.0, audio: &music, edit_offset: 0.0 }, &mut cache);
        f.meshes.iter().find(|m| m.material.blending == crate::scene::Blending::Additive).map(|m| m.material.opacity).unwrap_or(0.0)
    };
    let (hit, later) = (cone_opacity(2.0, &mut doc), cone_opacity(2.4, &mut doc));
    assert!(hit > later + 0.3, "{hit} vs {later}");
}

#[test]
fn slab_blocks_lift_on_the_beat_and_the_dive_reaches_the_surface() {
    let src = "use mod.scene.*; Scene3D{ bpm: 120 camera: Camera{position: vec3(0, 5, 8) target: vec3(0, 0, 0) moves: [CameraMove{type: @dive start: 1 end: 2}]} children: [Instances{geometry: BoxGeometry{height: 0.5} layout: GridLayout{count: vec3(12, 1, 12) spacing: vec3(0.3, 1, 0.3)} heights: Heightfield{kind: @blocks amount: 1 cell: vec2(0.9, 0.6) seed: 3}}] }";
    let mut doc = load(src);
    // y scale of each instance (column 1, row 1 of the matrix).
    let heights = |f: &SceneFrame| f.meshes[0].instances.iter().map(|m| m.transform.v[5]).collect::<Vec<f32>>();
    let (a, b) = (heights(&eval(&mut doc, 0.4)), heights(&eval(&mut doc, 0.9)));
    assert!(a.iter().any(|h| *h > 1.5) && a.iter().any(|h| (*h - 1.0).abs() < 1e-3), "some blocks lifted, some flat");
    assert_ne!(a, b, "a new block pattern on the next beat");
    assert_eq!(a, heights(&eval(&mut doc, 0.4)), "deterministic");
    let len = |p: [f32; 3]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
    let start = eval(&mut doc, 0.9).camera.position;
    let end = eval(&mut doc, 2.0).camera.position;
    assert!(len(end) < 0.2 && len(start) > 5.0, "{end:?} {start:?}");
}

#[test]
fn motion_blur_and_graph_passes_read() {
    let mut doc = load("use mod.scene.*; use mod.post; Scene3D{ post: [post.MotionBlur{shutter: 180 samples: @auto tol: 2} post.Glow{strength: 0.8 tint: #ff8040} post.Pass{at: @final uniforms: {k: fn(t) { return t * 2.0 }} pixel: fn() -> vec4 { return self.color.sample(self.uv()) * self.k }} post.Aberration{}] children: [Mesh{geometry: BoxGeometry{}}] }");
    let f = eval(&mut doc, 1.0);
    assert_eq!(f.motion_blur, Some((180.0, 324, 2.0)));
    let (decls, values) = f.post.graph.clone().expect("graph passes");
    // Glow's pyramid (10 passes), the Pass, Aberration's one pass.
    assert_eq!(decls.len(), 12);
    assert_eq!(values.len(), 12);
    // Kit pass names carry the kit's place in `post` (Glow is post[1]);
    // reads follow them.
    assert_eq!(decls[0].name.as_deref(), Some("p1_pre"));
    assert!(decls[9].reads.contains(&"p1_u0".to_string()));
    // Keyable uniforms are evaluated per frame; colours are linear.
    let pass = &decls[10];
    assert_eq!(pass.stage, makepad_render_graph::Stage::Final);
    assert_eq!(values[10][0][0], 2.0);
    let tint = decls[9].uniforms.iter().position(|u| u.name == "tint").unwrap();
    assert!((values[9][tint][0] - 1.0).abs() < 1e-4 && values[9][tint][1] < 0.3, "{:?}", values[9][tint]);
}

#[test]
fn graph_pass_uniforms_of_every_width_keep_their_values() {
    // Numbers, vec2, vec3 and vec4 uniforms mixed, constant and keyed: each
    // keeps its own width and values (a fn(t) giving a vec2 read as zeros).
    let mut doc = load("use mod.scene.*; use mod.post; Scene3D{ post: [post.Pass{at: @final uniforms: {a: fn(t) { return t * 2.0 } c: fn(t) { return vec2(t, 3.0) } d: fn(t) { return vec3(4.0, t, 5.0) } e: vec2(6.0, 7.0) f: 8.0 g: fn(t) { return vec4(1.0, 2.0, t, 9.0) }} pixel: fn() -> vec4 { return self.color.sample(self.uv()) * self.a + vec4(self.c, self.e) + vec4(self.d, self.f) + self.g }}] children: [Mesh{geometry: BoxGeometry{}}] }");
    let f = eval(&mut doc, 1.5);
    let (decls, values) = f.post.graph.clone().expect("graph passes");
    let pass = &decls[0];
    let widths: Vec<(&str, u8)> = pass.uniforms.iter().map(|u| (u.name.as_str(), u.width)).collect();
    assert_eq!(widths, vec![("a", 1), ("c", 2), ("d", 3), ("e", 2), ("f", 1), ("g", 4)]);
    let v = &values[0];
    assert_eq!(v[0][0], 3.0);
    assert_eq!(&v[1][..2], &[1.5, 3.0]);
    assert_eq!(&v[2][..3], &[4.0, 1.5, 5.0]);
    assert_eq!(&v[3][..2], &[6.0, 7.0]);
    assert_eq!(v[4][0], 8.0);
    assert_eq!(v[5][..4], [1.0, 2.0, 1.5, 9.0]);
}

#[test]
fn flight_through_shots_holds_and_flies() {
    let mut doc = load("use mod.scene.*; Scene3D{ camera: Camera{shots: [CameraShot{at: 0 hold: 1 position: vec3(0, 0, 5) target: vec3(0, 0, 0)} CameraShot{at: 3 position: vec3(10, 2, 5) target: vec3(10, 0, 0) roll: 20 fov: 30}]} children: [Mesh{name: \"a\" geometry: BoxGeometry{} position: vec3(10, 0, 0)}] }");
    let near = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3);
    assert!(near(eval(&mut doc, 0.5).camera.position, [0.0, 0.0, 5.0]), "holds at the first shot");
    let mid = eval(&mut doc, 2.0).camera.position;
    assert!(mid[0] > 1.0 && mid[0] < 9.0, "flies between: {mid:?}");
    let end = eval(&mut doc, 3.5);
    assert!(near(end.camera.position, [10.0, 2.0, 5.0]) && (end.camera.fov_y - 30.0).abs() < 0.01);
    // The named node is an anchor, centred on screen at the last shot.
    let a = end.anchors.iter().find(|a| a.name == "a").unwrap();
    assert!(a.in_front && (a.screen[0] - 0.5).abs() < 0.02, "{:?}", a.screen);
}

#[test]
fn path_built_curves_and_wrap() {
    use makepad_geom3d::{braid_points, paperclip_points};
    let clip = paperclip_points(1.0, 0.3);
    let (miny, maxy) = clip.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    assert!((maxy - miny - 1.0).abs() < 0.05, "a paperclip spans its length");
    assert!(clip.iter().all(|p| p[2] == 0.0 && p[0].abs() <= 0.151), "flat, within its width");
    let (a, b) = (braid_points(0, 3, 0.3, 3.0, 4.0), braid_points(1, 3, 0.3, 3.0, 4.0));
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(b.iter()).all(|(p, q)| ((p[0] - q[0]).powi(2) + (p[2] - q[2]).powi(2)).sqrt() > 1e-3 || p[1] != q[1]), "strands never meet");
    let mut doc = load("use mod.scene.*; Scene3D{ children: [Mesh{geometry: PlaneGeometry{} wrap_radius: 1.5} Callout{target: vec3(0, 1, 0) text: \"HI\"}] }");
    let f = eval(&mut doc, 1.0);
    assert_eq!(f.meshes[0].deform2[3], 1.5);
}

#[test]
fn light_shafts_follow_their_node() {
    let mut doc = load("use mod.scene.*; Scene3D{ camera: Camera{position: vec3(0, 0, 5) target: vec3(0, 0, 0)} post: [LightShafts{node: \"sun\"}] children: [Mesh{name: \"sun\" geometry: SphereGeometry{} position: vec3(0, 0, 0)}] }");
    let f = eval(&mut doc, 0.0);
    let (uv, strength, ..) = f.post.shafts.unwrap();
    assert!((uv[0] - 0.5).abs() < 1e-3 && (uv[1] - 0.5).abs() < 1e-3 && strength > 0.0);
}

#[test]
fn generic_primitives_field_fncurve_polylines_postpass() {
    let src = r#"
use mod.scene.*; let f = fn(x, z) { return 0.1 * x + 0.2 * z }
Scene3D{
  post: [PostPass{code: fn(uv: vec2, c: vec3) -> vec3 { return c * self.u_post0.x } uniforms: {gain: tween(0, 1, 0, 1, @linear)}}]
  children: [
    Mesh{geometry: FieldGeometry{field: f size: vec2(4, 2) segments: vec2(8, 4)} material: MeshBasicMaterial{wire_fill: #000000 wireframe: true uniforms: {a: 1 b: 2 c: 3 d: 4 e: 5 g: 6} hooks: {
        shade_fn: fn(c: vec4, p: vec3, n: vec3, uv: vec2, t: float) -> vec4 { return c * self.u_custom2.y }
      }}}
    Line3D{curve: FnCurve{func: fn(s) { return vec3(s, s * s, 0) } samples: 32} polylines: [[vec3(0, 0, 0), vec3(1, 0, 0)], [vec3(0, 1, 0), vec3(0, 2, 0)]] intensity: 3 tail: 0.5 fade: vec2(5, 20)}
  ] }"#;
    let mut doc = load(src);
    assert!(doc.warnings.iter().all(|w| !w.to_string().contains("no parameter")), "{:?}", doc.warnings.iter().map(|d| d.to_string()).collect::<Vec<_>>());
    let f = eval(&mut doc, 0.5);
    let m = &f.meshes[0];
    assert_eq!(m.mesh.positions.len(), 9 * 5);
    // Height follows the field: the corner (2, 1) is 0.1*2 + 0.2*1.
    let p = m.mesh.positions.iter().find(|p| (p[0] - 2.0).abs() < 1e-4 && (p[2] - 1.0).abs() < 1e-4).unwrap();
    assert!((p[1] - 0.4).abs() < 1e-4, "{p:?}");
    assert_eq!(m.material.custom[5], 6.0, "eight uniforms");
    assert!(m.material.wire_fill.is_some());
    let l = &f.lines[0];
    assert_eq!(l.fade, (5.0, 20.0));
    // The curve plus the two polylines; colours are boosted by intensity.
    let segs = l.segments.len();
    assert!(segs >= 3, "{segs}");
    assert!(l.segments.iter().any(|s| s.color_b[0] > 2.9));
    // The comet tail: the drawn window's start is transparent.
    assert!(l.segments[0].color_a[3] < 0.05, "{}", l.segments[0].color_a[3]);
    let (_, codes, u, _) = f.post.passes.as_ref().unwrap();
    assert_eq!(codes.len(), 1);
    assert!((u[0] - 0.5).abs() < 1e-4);
}

#[test]
fn a_kernel_runs_only_its_active_elements() {
    // `active_count: fn(t)`: the first elements run each frame (at most
    // `count`), so a kernel whose tail is invisible most of the time does
    // not compute it.
    let src = r#"
use mod.scene.*; use mod.kernel; let dots = Kernel{
    count: 8
    active_count: fn(t) { if t < 1.0 { 3 } else { 20 } }
    out: kernel.output(kernel.Sprite)
    element: fn(i) {
        out[i].pos = vec3(float(i), time * 0.0, 0.0)
        out[i].size = 0.1
        out[i].color = vec4(1.0, 1.0, 1.0, 1.0)
    }
}
Scene3D{ children: [Points{source: dots.out}] }"#;
    let mut doc = load(src);
    assert_eq!(eval(&mut doc, 0.5).points[0].points.len(), 3);
    assert_eq!(eval(&mut doc, 1.5).points[0].points.len(), 8, "at most count");
}

#[test]
fn kernels_read_arrays_the_document_computes() {
    // `pts: input(vec3, PTS)`: an array made by ordinary Splash at load
    // (here a loop) is a kernel input, for a load-time kernel and for a
    // per-frame one alike.
    let src = r#"
use mod.scene.*; use mod.kernel; var PTS = []
for k in 0..6 { PTS.push(vec3(k, 2.0 * k, 0.0)) }
let SCALE = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0]
let copy = Kernel{
    count: PTS.len()
    pts: kernel.input(vec3, PTS)
    out: kernel.output(vec3)
    element: fn(i) { out[i] = pts[i] }
}
let dots = Kernel{
    count: 6
    pts: kernel.input(vec3, PTS)
    scale: kernel.input(f32, SCALE)
    out: kernel.output(kernel.Sprite)
    element: fn(i) {
        out[i].pos = pts[i] + vec3(0.0, 0.0, scale[i] + time)
        out[i].size = 0.1
        out[i].color = vec4(1.0, 1.0, 1.0, 1.0)
    }
}
Scene3D{ children: [Line3D{points: copy.out} Points{source: dots.out}] }"#;
    let mut doc = load(src);
    let f = eval(&mut doc, 0.5);
    assert!(f.lines[0].segments.len() >= 5, "the load-time kernel copied the points");
    let p = &f.points[0].points;
    assert_eq!(p.len(), 6);
    // Element 3: pts[3] = (3, 6, 0), z = scale[3] + time = 40.5.
    assert_eq!(p[3].pos, [3.0, 6.0, 40.5], "{:?}", p[3]);
}

#[test]
fn a_kernel_reads_an_array_the_document_computes_every_frame() {
    // `rows: input(vec2, fn(t) {..})`: the closure runs every frame and its
    // array is the kernel's input (a camera per copy, worked out once).
    let src = r#"
use mod.scene.*; use mod.kernel; let rows = fn(t) { var a = [] for k in 0..3 { a.push(vec2(k, 10.0 * t)) } return a }
let dots = Kernel{
    count: 3
    rows: kernel.input(vec2, fn(t) { rows(t) })
    out: kernel.output(kernel.Sprite)
    element: fn(i) {
        out[i].pos = vec3(rows[i].x, rows[i].y, 0.0)
        out[i].size = 0.1
        out[i].color = vec4(1.0, 1.0, 1.0, 1.0)
    }
}
Scene3D{ children: [Points{source: dots.out}] }"#;
    let mut doc = load(src);
    let p = eval(&mut doc, 0.5).points[0].points.clone();
    assert_eq!([p[2].pos[0], p[2].pos[1]], [2.0, 5.0], "{:?}", p[2]);
    let p = eval(&mut doc, 1.5).points[0].points.clone();
    assert_eq!([p[2].pos[0], p[2].pos[1]], [2.0, 15.0]);
}

#[test]
fn kernels_feed_lines_and_points_every_frame() {
    // A load-time kernel's output is an array the document reads; a kernel
    // that reads `time` runs every frame for the Points that draw it.
    let src = r#"
use mod.scene.*; use mod.kernel; let ring = Kernel{
    pts: kernel.emit_buffer(vec3, 16)
    element: fn(i) { for k in 0..16 { let a = float(k) * 0.3927 emit(pts, vec3(cos(a), 0.0, sin(a))) } }
}
let dots = Kernel{
    count: 4
    ring: ring.pts
    lift: kernel.param(fn(t) { return 2.0 * t })
    out: kernel.output(kernel.Sprite)
    element: fn(i) {
        out[i].pos = ring[i * 4] + vec3(0.0, lift + time, 0.0)
        out[i].size = 0.1
        out[i].color = vec4(1.0, 0.5, 0.0, 1.0)
    }
}
Scene3D{ children: [Line3D{points: ring.pts} Points{source: dots.out}] }"#;
    let mut doc = load(src);
    let f = eval(&mut doc, 0.5);
    assert!(f.lines[0].segments.len() >= 15);
    let p = &f.points[0].points;
    assert_eq!(p.len(), 4);
    // lift(0.5) + time = 1.5; ring[4] = (cos(4 * 0.3927), 0, sin(..)).
    assert!((p[0].pos[1] - 1.5).abs() < 1e-4, "{:?}", p[0]);
    assert!((p[1].pos[0] - (4.0f32 * 0.3927).cos()).abs() < 1e-4);
    assert!((p[0].size - 0.1).abs() < 1e-6, "sprite size");
    let f = eval(&mut doc, 1.0);
    assert!((f.points[0].points[0].pos[1] - 3.0).abs() < 1e-4);
}

#[test]
fn scenes_are_timed_by_the_song() {
    use crate::script::{SongTiming, SongWord};
    let w = |text: &str, start: f32, end: f32, line: u32| SongWord { text: text.into(), start, end, line };
    let song = SongTiming {
        words: vec![w("We", 0.2, 0.5, 0), w("fold", 0.5, 0.7, 0), w("a", 0.7, 0.8, 0), w("silver", 0.8, 1.3, 0), w("road,", 1.3, 1.8, 0), w("road", 4.0, 4.5, 1)],
        beats: (0..20).map(|b| b as f32 * 0.5).collect(),
        beats_per_bar: 4,
        downbeat: 1,
    };
    let src = r#"
use mod.scene.*; let a = word(@song, "silver")
let b = word_end(@song, "a silver road")
let c = word(@song, "road", 0.1, 1)
let k = words(@song, "fold a")
Scene3D{ children: [Mesh{geometry: BoxGeometry{} position: vec3(a, b, c) scale: vec3(k[1].x, beat_near(1.1), downbeat_near(1.1))}] }"#;
    let resolver = FileResolver::default();
    let mut doc = Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: Some(std::sync::Arc::new(song)) })
        .unwrap_or_else(|e| panic!("{:?}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>()));
    let f = eval(&mut doc, 0.0);
    let m = &f.meshes[0].instances[0].transform.v;
    let (pos, col) = ([m[12], m[13], m[14]], |k: usize| (m[4 * k] * m[4 * k] + m[4 * k + 1] * m[4 * k + 1] + m[4 * k + 2] * m[4 * k + 2]).sqrt());
    assert!((pos[0] - 0.8).abs() < 1e-5 && (pos[1] - 1.8).abs() < 1e-5 && (pos[2] - 4.1).abs() < 1e-5, "{pos:?}");
    // Punctuation is ignored when matching ("road," is "road").
    let (sx, sy, sz) = (col(0), col(1), col(2));
    assert!((sx - 0.7).abs() < 1e-5, "{sx}");
    assert!((sy - 1.0).abs() < 1e-5 && (sz - 0.5).abs() < 1e-5, "beat 1.0, downbeat (bar starts on beat 1) 0.5: {sy} {sz}");
}

/// Effect physics (KERNELS.md §3.6): a frame is the same whether the
/// document played up to it or seeked to it cold, in any order.
fn physics_frames_match_in_any_order(src: &str, times: &[f32]) {
    let mut linear = load(src);
    let mut sorted: Vec<f32> = times.to_vec();
    sorted.sort_by(f32::total_cmp);
    let want: Vec<(f32, SceneFrame)> = sorted.iter().map(|&t| (t, eval(&mut linear, t))).collect();
    let mut shuffled = load(src);
    // A fixed shuffle: every other one from the end, then the rest.
    let mut order: Vec<usize> = (0..want.len()).rev().step_by(2).collect();
    order.extend((0..want.len()).rev().skip(1).step_by(2));
    for k in order {
        let (t, a) = &want[k];
        let b = eval(&mut shuffled, *t);
        assert!(b.errors.is_empty() && a.errors.is_empty(), "{:?} {:?}", a.errors, b.errors);
        assert_eq!(a.meshes.len(), b.meshes.len(), "t {t}");
        for (x, y) in a.meshes.iter().zip(b.meshes.iter()) {
            assert_eq!(x.instances, y.instances, "instances at t {t}");
        }
        assert_eq!(a.points.len(), b.points.len(), "t {t}");
        for (x, y) in a.points.iter().zip(b.points.iter()) {
            assert_eq!(x.points, y.points, "points at t {t}");
        }
    }
}

/// The confetti showcase, small: 48 pieces, not 600, launched on beat 1
/// (0.5 s), not beat 4. Seeking, the burst, the floor contacts and the
/// sparkles they make are the showcase's; only the work is smaller.
fn small_confetti() -> String {
    let src = include_str!("../showcase/physics/beat_confetti.splash");
    assert!(src.contains("count: 600") && src.contains("at: beat(4)"), "the showcase's burst changed: update small_confetti");
    src.replace("count: 600", "count: 48").replace("at: beat(4)", "at: beat(1)")
}

#[test]
fn physics_confetti_seeks_like_it_plays() {
    let src = small_confetti();
    let src = src.as_str();
    // 8 times over the burst (beat 1 = 0.5 s) and the landing, off the step
    // grid, sub-frame times among them.
    let times: Vec<f32> = (0..8).map(|k| 0.4 + k as f32 * 0.21 + if k % 3 == 0 { 0.0021 } else { 0.0 }).collect();
    physics_frames_match_in_any_order(src, &times);
    // The pieces are there after the beat, not before.
    let mut doc = load(src);
    let before = eval(&mut doc, 0.49);
    let after = eval(&mut doc, 1.1);
    let bodies = |f: &SceneFrame| f.meshes.iter().map(|m| m.instances.len()).max().unwrap_or(0);
    assert!(bodies(&after) > bodies(&before), "{} vs {}", bodies(&after), bodies(&before));
    // Sparkles once they land.
    let late = eval(&mut doc, 1.95);
    assert!(late.points.iter().any(|p| !p.points.is_empty()), "sparkles at the floor hits");
}

#[test]
fn physics_sims_and_kinematic_bodies_seek_like_they_play() {
    let src = include_str!("../showcase/physics/sparkle_contact.splash");
    let times: Vec<f32> = (0..20).map(|k| 0.3 + k as f32 * 0.311).collect();
    physics_frames_match_in_any_order(src, &times);
    let mut doc = load(src);
    let f = eval(&mut doc, 2.2);
    assert!(f.points.iter().any(|p| p.points.len() > 12), "sparks in flight");
}

#[test]
fn physics_mistakes_teach() {
    let resolver = FileResolver::default();
    let fails = |src: &str| match Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }) {
        Ok(_) => panic!("loaded:\n{src}"),
        Err(e) => e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"),
    };
    let e = fails("use mod.scene.*; Scene3D{ physics: Physics{bodies: [Body{name: \"b\" shape: @cone}]} }");
    assert!(e.contains("no body shape `cone`") && e.contains("@box"), "{e}");
    let e = fails("use mod.scene.*; use mod.kernel; let k = Kernel{ out: kernel.output(vec3) element: fn(i) { out[i] = vec3(0.0, 0.0, 0.0) } }\nScene3D{ physics: Physics{spawn: [Spawn{group: \"g\" from: k}]} }");
    assert!(e.contains("Body records"), "{e}");
    let e = fails("use mod.scene.*; Scene3D{ physics: Physics{step: 1 / 11} }");
    assert!(e.contains("does not divide the time grid"), "{e}");
    let e = fails("use mod.scene.*; let s = Sim{ count: 4 update: fn(i) { next[i] = prev[i] } }\nScene3D{}");
    assert!(e.contains("state"), "{e}");
}

#[test]
fn physics_preview_catches_up_without_blocking() {
    let src = small_confetti();
    let src = src.as_str();
    let mut locked = load(src);
    let want = eval(&mut locked, 2.5);
    let mut preview = load(src);
    preview.set_physics_budget(Some(std::time::Duration::from_millis(2)));
    // A jump 2.5 s ahead (600 steps, 2 s of them with the pieces flying):
    // each evaluation steps at most ~2 ms and shows the newest state it
    // has; it reaches the locked frame in the end.
    let t0 = std::time::Instant::now();
    let first = eval(&mut preview, 2.5);
    assert!(t0.elapsed() < std::time::Duration::from_millis(500), "{:?}", t0.elapsed());
    let same = |a: &SceneFrame, b: &SceneFrame| a.meshes.iter().zip(b.meshes.iter()).all(|(x, y)| x.instances == y.instances) && a.meshes.len() == b.meshes.len();
    assert!(!same(&first, &want), "the first preview frame cannot have stepped 2.5 s in 2 ms");
    let mut n = 0;
    loop {
        let f = eval(&mut preview, 2.5);
        n += 1;
        if same(&f, &want) {
            break;
        }
        assert!(n < 10_000, "preview never caught up");
    }
}


/// The world translation of the first instance of the mesh drawn for a
/// physics group.
fn body_translation(f: &SceneFrame, mesh: usize) -> [f32; 3] {
    let m = &f.meshes[mesh].instances[0].transform.v;
    [m[12], m[13], m[14]]
}

#[test]
fn kinematic_bodies_follow_added_keys_and_scene_nodes() {
    // Arithmetic on animations builds the derived animation (the same as
    // add(..)/mul(..)): on a kinematic body, on a mesh, and with beat times.
    let resolver = FileResolver::default();
    let load_err = |src: &str| match Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }) {
        Ok(_) => panic!("loaded:\n{src}"),
        Err(e) => e.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n"),
    };
    for position in ["tween(vec3(0, 0, 0), vec3(2, 0, 0), 0, 1) + vec3(0, 1, 0)", "add(tween(vec3(0, 0, 0), vec3(2, 0, 0), 0, 1), vec3(0, 1, 0))", "(tween(vec3(0, 2, 0), vec3(2, 2, 0), 0, 1) - vec3(0, 1, 0)) / 2 * 2"] {
        let src = format!(
            "use mod.scene.*; Scene3D{{ lights: [AmbientLight{{}}] physics: Physics{{bodies: [Body{{name: \"k\" kinematic: true shape: @box size: vec3(0.2, 0.2, 0.2) position: {position}}}]}}
               children: [Mesh{{geometry: BoxGeometry{{}} position: {position}}} Instances{{source: bodies(\"k\") geometry: BoxGeometry{{width: 0.2 height: 0.2 depth: 0.2}}}}] }}"
        );
        let mut doc = load(&src);
        assert!(doc.warnings.iter().all(|d| !d.message.contains("NaN")), "{position}: {:?}", doc.warnings);
        let f = eval(&mut doc, 0.5);
        let (mesh, body) = (body_translation(&f, 0), body_translation(&f, 1));
        assert!((mesh[0] - 1.0).abs() < 1e-5 && (mesh[1] - 1.0).abs() < 1e-5, "{position}: mesh {mesh:?}");
        assert!((body[0] - 1.0).abs() < 1e-2 && (body[1] - 1.0).abs() < 1e-3, "{position}: body {body:?}");
    }
    // A beat time moved by seconds stays a time.
    let mut doc = load("use mod.scene.*; Scene3D{ bpm: 120 lights: [AmbientLight{}] children: [Mesh{geometry: BoxGeometry{} position: tween(vec3(0, 0, 0), vec3(1, 0, 0), beat(2) + 0.5, beat(2) + 1.5)}] }");
    let x = body_translation(&eval(&mut doc, 2.0), 0)[0];
    assert!((x - 0.5).abs() < 0.2 && x > 0.0 && x < 1.0, "halfway through [1.5, 2.5]: {x}");
    // What the VM still cannot compute (a function of an animation) loads
    // with a warning that says what to write, and reads 0.
    let doc = load("use mod.scene.*; Scene3D{ lights: [AmbientLight{}] children: [Mesh{geometry: BoxGeometry{} position: -tween(vec3(0, 0, 0), vec3(2, 0, 0), 0, 1)}] }");
    assert!(doc.warnings.iter().any(|d| d.message.contains("not a number") && d.hint.as_deref().is_some_and(|h| h.contains("mul(tween(..), -1)"))), "{:?}", doc.warnings);

    // `kinematic: "node"`: the body follows a scene node's world pose, here
    // a child of a turned, moving group.
    let src = "use mod.scene.*; Scene3D{ lights: [AmbientLight{}]
        physics: Physics{bodies: [Body{name: \"collider\" kinematic: \"hand\" shape: @box size: vec3(0.3, 0.3, 0.3)}]}
        children: [
            Group3D{position: tween(vec3(0, 0, 0), vec3(0, 0, -4), 0, 2) rotation: vec3(0, 90, 0) children: [
                Mesh{name: \"hand\" position: vec3(1, 0.5, 0) geometry: BoxGeometry{width: 0.3 height: 0.3 depth: 0.3}}
            ]}
            Instances{source: bodies(\"collider\") geometry: BoxGeometry{width: 0.3 height: 0.3 depth: 0.3}}
        ] }";
    for t in [0.0f32, 0.75, 1.5] {
        let mut doc = load(src);
        let f = eval(&mut doc, t);
        // The mesh (drawn first) and the body (second) sit in one place.
        let (node, body) = (body_translation(&f, 0), body_translation(&f, 1));
        for k in 0..3 {
            assert!((node[k] - body[k]).abs() < 2e-3, "t {t}: node {node:?} body {body:?}");
        }
    }
    // The node is (1, 0.5, 0) in a group turned 90° about y: world x ≈ 0, z ≈ -1 - 2 * 0.75.
    let mut doc = load(src);
    let p = body_translation(&eval(&mut doc, 0.75), 1);
    assert!(p[0].abs() < 2e-3 && (p[2] + 1.0 + 1.5).abs() < 2e-3, "{p:?}");

    let e = load_err("use mod.scene.*; Scene3D{ physics: Physics{bodies: [Body{name: \"c\" kinematic: \"hands\"}]} children: [Group3D{name: \"hand\"}] }");
    assert!(e.contains("no scene node named \"hands\"") && e.contains("\"hand\""), "{e}");
}

#[test]
fn a_missing_name_is_named_where_it_is_read() {
    let resolver = FileResolver::default();
    let src = "use mod.scene.*; Scene3D{\n  camera: Camera{}\n  children: [Mesh{geometry: BoxGeometry{} position: vec3(R, 0, 0)}]\n}";
    let err = match Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 10, origin_file: None, song: None }) {
        Ok(_) => panic!("should fail"),
        Err(e) => e,
    };
    let first = &err[0];
    assert!(first.message.starts_with("variable R not found"), "{err:?}");
    assert!(!first.message.contains("platform/script"), "{}", first.message);
    assert_eq!(first.loc.line, 12, "{err:?}");
    assert!((58..=59).contains(&first.loc.col), "at the R: {err:?}");
}

/// Motion's lyric lookups mean the same in a scene: the words of the line
/// a sung word is in, from the song's timing; the kit's helpers are there.
#[test]
fn a_scene_looks_lyrics_up_like_motion() {
    use crate::script::{SongTiming, SongWord};
    let w = |text: &str, start: f32, end: f32, line: u32| SongWord { text: text.into(), start, end, line };
    let song = SongTiming { words: vec![w("sharp", 0.2, 0.5, 0), w("left", 0.5, 0.9, 0), w("turn", 0.9, 1.3, 0), w("there", 2.0, 2.4, 1)], beats: vec![], beats_per_bar: 4, downbeat: 0 };
    let src = r#"
use mod.scene.*; let L = lyric_line(voice(@song), "turn", 0)
let W = lyric_word(voice(@song), "there", 0)
Scene3D{ children: [Mesh{geometry: BoxGeometry{} position: vec3(L[1].start, W.start, prog(0.5, 0.0, 1.0) + pulse(0.0, 1.0, 0.5))}] }"#;
    let resolver = FileResolver::default();
    let mut doc = Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: Some(std::sync::Arc::new(song)) })
        .unwrap_or_else(|e| panic!("{:?}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>()));
    let f = eval(&mut doc, 0.0);
    let m = &f.meshes[0].instances[0].transform.v;
    assert!((m[12] - 0.5).abs() < 1e-5 && (m[13] - 2.0).abs() < 1e-5 && (m[14] - 0.5).abs() < 1e-5, "{:?}", &m[12..15]);
}

/// A scene that computes a non-number says so.
#[test]
fn a_scene_that_computes_a_non_number_says_so() {
    let mut nan = load("use mod.scene.*; Scene3D{ camera: Camera{} lights: [AmbientLight{}] children: [Mesh{geometry: BoxGeometry{} position: fn(t, edit) { vec3(0.0 / 0.0, 0, 0) }}] }");
    let f = eval(&mut nan, 1.0);
    assert!(f.errors.iter().any(|e| e.contains("non-number")), "{:?}", f.errors);
}

/// Lookups from a second of the scene's clock: a repeated chorus word finds
/// the occurrence at or after `after` (`here()` is the scene's start).
#[test]
fn a_scene_looks_lyrics_up_from_a_second() {
    use crate::script::{SongTiming, SongWord};
    let w = |text: &str, start: f32, line: u32| SongWord { text: text.into(), start, end: start + 0.4, line };
    let song = SongTiming { words: vec![w("lit", -5.0, 0), w("fuse", -4.0, 0), w("lit", 3.0, 1), w("fuse", 4.0, 1), w("again", 6.0, 2)], beats: vec![], beats_per_bar: 4, downbeat: 0 };
    let src = r#"
use mod.scene.*; let A = lyric_word(voice(@song), "fuse", 0, here())
let L = lyric_line(voice(@song), "lit", 0, 0.0)
let N = lyric_line_after(voice(@song), 5.0)
Scene3D{ children: [Mesh{geometry: BoxGeometry{} position: vec3(A.start, L[1].start, N[0].start + word(@song, "fuse", 0.0, 0, 0.0))}] }"#;
    let resolver = FileResolver::default();
    let mut doc = Scene3dDoc::load(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: Some(std::sync::Arc::new(song)) })
        .unwrap_or_else(|e| panic!("{:?}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>()));
    let f = eval(&mut doc, 0.0);
    let m = &f.meshes[0].instances[0].transform.v;
    assert!((m[12] - 4.0).abs() < 1e-5 && (m[13] - 4.0).abs() < 1e-5 && (m[14] - 10.0).abs() < 1e-5, "{:?}", &m[12..15]);
}

/// A scene imports its document's Splash modules: a library revision through
/// `import(asset(id, rev))` and a named one through `mod.motion.modules`,
/// with what they import, as the document does.
#[test]
fn a_scene_imports_its_documents_modules() {
    use makepad_script_imports::{resolve, scan, MemoryModules, ModuleRef, ModuleSource};
    let mut mem = MemoryModules::default();
    mem.add_asset("ast_data", "r1", "{lift: 2.5}");
    mem.named.push(ModuleSource::new("shift", "1", "use mod.motion.*; let base = import(asset(\"ast_data\", \"r1\"))\nlet shift = fn(x) { x + base.lift }\n{shift: shift}"));
    let src = "use mod.scene.*; use mod.motion.modules.shift.*\nlet data = import(asset(\"ast_data\", \"r1\"))\nScene3D{ children: [Mesh{geometry: BoxGeometry{} position: vec3(shift(1.0), data.lift, 0.0)}] }";
    let modules = resolve(&scan(src, Some("mod.motion.modules.")), &mem, Some("mod.motion.modules.")).unwrap();
    assert!(modules.iter().any(|m| m.reference == ModuleRef::Named("shift".into())));
    let resolver = FileResolver::default();
    let mut doc = Scene3dDoc::load_with_modules(src, LoadOptions { name: "t".into(), inputs: HashMap::new(), resolver: &resolver, first_line: 1, origin_file: None, song: None }, &modules)
        .unwrap_or_else(|e| panic!("{:?}", e.iter().map(|d| d.to_string()).collect::<Vec<_>>()));
    let f = eval(&mut doc, 0.0);
    let m = &f.meshes[0].instances[0].transform.v;
    assert!((m[12] - 3.5).abs() < 1e-5 && (m[13] - 2.5).abs() < 1e-5, "{:?}", &m[12..15]);
}
