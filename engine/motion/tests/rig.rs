//! Pure deform-stage contracts. The lane lead runs these; pixel proof uses
//! motion_draw on the native GPU separately.
use makepad_motion::{character, geometry, rig, *};

fn load(layer: &str) -> MotionDoc {
    let source = format!("use mod.motion.*; Motion{{duration: 6 bpm: 120 layers: [{layer}]}}");
    MotionDoc::load(&source).unwrap_or_else(|d| panic!("{d:#?}\n{source}"))
}

fn near(a: [f64; 2], b: [f64; 2]) {
    assert!((a[0] - b[0]).hypot(a[1] - b[1]) < 1e-7, "{a:?} != {b:?}");
}

fn bones(doc: &MotionDoc) -> &rig::Bones {
    let LayerKind::Bones(rig) = &doc.layers[0].kind else { panic!("expected Bones") };
    rig
}

fn sample(p: &Prop<1>, t: f64) -> f64 {
    match p { Prop::Const([v]) => *v, Prop::Track(track) => sample_track(track, t)[0], _ => panic!("not a key track") }
}

#[test]
fn fk_chains_branches_and_forward_parent_references() {
    let doc = load(r#"Bones{bones: [
        Bone{name: "upper" length: 100 rotate: 90}
        Bone{name: "lower" length: 80 rotate: -90}
        Bone{name: "branch" parent: "upper" length: 30 rotate: 90}
        Bone{name: "other" parent: none length: 40}
    ] layers: []}"#);
    let r = bones(&doc);
    let pose = rig::fk(&r.bones, &r.order, |i| r.bones[i].rest);
    near(pose[0].base, [0.0, 0.0]);
    near(pose[0].end, [0.0, 100.0]);
    near(pose[1].base, pose[0].end);
    near(pose[1].end, [80.0, 100.0]);
    near(pose[2].end, [-30.0, 100.0]);
    near(pose[3].base, [0.0, 0.0]);
    near(pose[3].end, [40.0, 0.0]);
    let forward = load(r#"Bones{bones: [Bone{name: "child" parent: "root" length: 5} Bone{name: "root" parent: none length: 10}] layers: []}"#);
    near(bones(&forward).rest[0].end, [15.0, 0.0]);
}

#[test]
fn two_bone_reaches_both_bends_and_extends_without_stretching() {
    let base = [13.0, -7.0];
    for bend in [rig::Bend::Cw, rig::Bend::Ccw] {
        let target = [123.0, 43.0];
        let solved = rig::two_bone(base, [100.0, 80.0], target, bend);
        assert!(!solved.unreachable);
        near(solved.end, target);
        assert!(((solved.joint[0] - base[0]).hypot(solved.joint[1] - base[1]) - 100.0).abs() < 1e-9);
        assert!(((solved.end[0] - solved.joint[0]).hypot(solved.end[1] - solved.joint[1]) - 80.0).abs() < 1e-9);
        let a = [solved.joint[0] - base[0], solved.joint[1] - base[1]];
        let b = [solved.end[0] - solved.joint[0], solved.end[1] - solved.joint[1]];
        assert_eq!(a[0] * b[1] - a[1] * b[0] > 0.0, bend == rig::Bend::Cw);
        let far = rig::two_bone(base, [100.0, 80.0], [313.0, -7.0], bend);
        assert!(far.unreachable);
        near(far.joint, [113.0, -7.0]);
        near(far.end, [193.0, -7.0]);
        let folded = rig::two_bone(base, [100.0, 100.0], base, bend);
        near(folded.end, base);
        assert!(folded.joint.iter().all(|v| v.is_finite()));
    }
}

#[test]
fn solver_orders_follow_before_ik_and_carries_branches() {
    let doc = load(r#"Bones{bones: [Bone{name: "a" length: 100 rotate: 20} Bone{name: "b" length: 80 rotate: 15} Bone{name: "tip" length: 10}]
        follow: [Follow{bone: "a" path: "M 20 30 L 20 130" along: 0 orient: true}]
        ik: [IK{end: "b" target: vec2(130, 80) bend: @cw}]
        layers: []}"#);
    let r = bones(&doc);
    let mut solve = rig::Solver::new(r, |i| r.bones[i].rest);
    solve.follow(&r.follow[0], 0.0);
    near(solve.pose[0].base, [20.0, 30.0]);
    near(solve.pose[0].end, [20.0, 130.0]);
    let before = solve.pose.clone();
    assert!(!solve.ik(&r.ik[0], [130.0, 80.0], 0.0));
    assert_eq!(solve.pose, before);
    assert!(!solve.ik(&r.ik[0], [130.0, 80.0], 1.0));
    near(solve.pose[1].end, [130.0, 80.0]);
    near(solve.pose[2].base, solve.pose[1].end);
}

#[test]
fn follow_uses_arc_length_and_endpoint_tangents() {
    let path = Polyline::of("M 0 0 L 100 0 L 100 100");
    let (p, tangent) = path.sample(0.75);
    near(p, [100.0, 50.0]);
    near(tangent, [0.0, 1.0]);
    near(path.sample(1.0).0, [100.0, 100.0]);
    near(path.sample(1.0).1, [0.0, 1.0]);
    near(path.sample(0.0).1, [1.0, 0.0]);
    let curve = Polyline::of("M 0 0 C 0 100 100 100 100 0");
    near(curve.point(0.5), [50.0, 75.0]);
    assert!(curve.sample(0.0).1[1] > 0.99);
    assert!(curve.sample(1.0).1[1] < -0.99);
    let doc = load(r#"Bones{bones: [Bone{name: "a" length: 20}]
        follow: [Follow{bone: "a" path: "M 0 0 L 100 0 L 100 100" along: tween(0, 1, 0, 2) orient: true}]
        layers: [Path{svg: "M 0 0 L 20 0" stroke: #fff rigid: "a"}]}"#);
    let frame = doc.eval(1.5);
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    near(children[0].transform([20.0, 0.0]).apply([0.0, 0.0]), [100.0, 50.0]);
    near(children[0].transform([20.0, 0.0]).apply([20.0, 0.0]), [100.0, 70.0]);
}

#[test]
fn weights_are_normalized_and_rest_maps_are_identity() {
    let doc = load(r#"Bones{bones: [Bone{name: "a" length: 100 rotate: 30} Bone{name: "b" length: 80 rotate: 20} Bone{name: "c" length: 60} Bone{name: "d" length: 40}] layers: []}"#);
    let r = bones(&doc);
    let solve = rig::Solver::new(r, |i| r.bones[i].rest);
    let maps = solve.skin_maps();
    for p in [[0.0, 0.0], [100.0, 20.0], [250.0, 100.0], [-1e6, 1e6]] {
        let weights = rig::Weights::at(p, &r.rest);
        assert!((weights.0.iter().map(|(_, w)| *w).sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(weights.0.iter().all(|(_, w)| (0.0..=1.0).contains(w)));
        near(weights.apply(p, &maps), p);
    }
}

#[test]
fn first_key_defines_rest_and_rigid_child_obeys_container_transform() {
    let doc = load(r#"Bones{at: vec2(400, 300) rotate: 90
        bones: [Bone{name: "a" length: 100 rotate: keys([-1, 0], [1, 90])} Bone{name: "b" length: 80}]
        layers: [Rect{at: vec2(100, 0) size: vec2(30, 10) fill: #f80 rigid: "b"}]}"#);
    assert_eq!(bones(&doc).bones[0].rest, 0.0); // t=0 would be 45 degrees.
    let frame = doc.eval(1.0);
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    assert!(matches!(children[0].kind, ItemKind::Rect { .. }));
    near(children[0].transform([30.0, 10.0]).apply([0.0, 0.0]), [300.0, 300.0]);
}

#[test]
fn paths_rects_ellipses_and_image_meshes_deform_deterministically() {
    let doc = load(r#"Bones{at: vec2(20, 30) bones: [Bone{name: "a" length: 100 rotate: tween(0, 90, 0, 2)}]
        layers: [Path{svg: "M 0 0 L 100 0" stroke: #fff}
                 Rect{size: vec2(100, 10) fill: #f80}
                 Ellipse{size: vec2(100, 10) fill: #f80}
                 Image{src: asset("ast_arm", "arev_1") size: vec2(100, 20) mesh: vec2(12, 3)}]}"#);
    let a = doc.eval(2.0);
    let _ = doc.eval(0.25);
    assert_eq!(a, doc.eval(2.0));
    assert!(a.errors.is_empty());
    let ItemKind::Group { children, .. } = &a.items[0].kind else { panic!() };
    for item in &children[..3] { assert!(matches!(item.kind, ItemKind::Path { .. })); }
    let ItemKind::Path { path, .. } = &children[0].kind else { panic!() };
    assert!(path.cmds.len() > 2, "straight edges gain vertices to bend near joints");
    let bounds = geometry::bounds(path).unwrap();
    near([bounds[2], bounds[3]], [0.0, 100.0]);
    let ItemKind::Image { mesh: Some(mesh), .. } = &children[3].kind else { panic!() };
    assert_eq!(mesh.positions.len(), 13 * 4);
    assert_eq!(mesh.indices.len(), 12 * 3 * 6);
    near(mesh.positions[12], [0.0, 100.0]);
    near(mesh.positions[51], [-20.0, 100.0]);
    near(mesh.uvs[51], [1.0, 1.0]);
    let b = doc.eval(1.0);
    let ItemKind::Group { children: other, .. } = &b.items[0].kind else { panic!() };
    let ItemKind::Image { mesh: Some(other), .. } = &other[3].kind else { panic!() };
    assert!(std::sync::Arc::ptr_eq(&mesh.indices, &other.indices));
    assert!(std::sync::Arc::ptr_eq(&mesh.uvs, &other.uvs));
}

#[test]
fn ik_tracks_resolve_on_the_bones_clock_and_warn_when_unreachable() {
    let doc = load(r#"Bones{span: [1, 4] bones: [Bone{name: "a" length: 100} Bone{name: "b" length: 80}]
        ik: [IK{end: "b" target: tween(vec2(120, 30), vec2(500, 0), 0, beat(6))}]
        layers: [Rect{at: vec2(180, 0) size: vec2(1, 1) fill: #fff rigid: "b"}]}"#);
    let frame = doc.eval(1.0);
    assert!(frame.errors.is_empty());
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    near(children[0].transform([1.0, 1.0]).apply([0.0, 0.0]), [120.0, 30.0]);
    let frame = doc.eval(3.0);
    assert!(frame.errors.iter().any(|d| d.severity == Severity::Warning && d.path.ends_with("ik[0].target")));
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    near(children[0].transform([1.0, 1.0]).apply([0.0, 0.0]), [180.0, 0.0]);
}

#[test]
fn character_reaches_a_keyed_hand_and_a_foot_without_regrounding() {
    let doc = load(r#"Character{model: @pip height: 500 blink: 0 sway: 0
        reach: {hand_r: tween(vec2(40, -250), vec2(100, -270), 0, 2) foot_l: vec2(-40, -30)}}"#);
    let LayerKind::Character(ch) = &doc.layers[0].kind else { panic!() };
    let model = ch.rig().unwrap();
    for t in [0.0, 1.0, 2.0] {
        let reach: Vec<_> = ch.reach.iter().map(|(joint, p)| (*joint, match p { Prop::Const(v) => *v, Prop::Track(track) => sample_track(track, t), _ => panic!() })).collect();
        let beat = |u: f64| u * 2.0;
        let moment = character::Moment { t, beat: &beat, voice: None, host: 0.0, fps: 60.0, parts: &[], reach: &reach, pose: None, instance: 0, arms: 0.0, hop: 0.0, nod: 0.0 };
        let solved = character::solve(ch, model, &moment);
        assert!(solved.unreachable.is_empty(), "{:?}", solved.unreachable);
        for &(joint, target) in &reach {
            let part = model.joint_part(joint).unwrap();
            let map = solved.parts.iter().find(|(i, _, _)| *i == part).unwrap().1;
            near(map.apply(model.parts[part].pivot), target);
        }
        assert_eq!(doc.eval(t), doc.eval(t));
        assert!(doc.eval(t).errors.is_empty());
    }
}

#[test]
fn invalid_rig_fields_have_teaching_errors() {
    for (layer, field) in [
        (r#"Bones{bones: [Bone{name: "a" parent: "missing"}] layers: []}"#, "parent"),
        (r#"Bones{bones: [Bone{name: "a" parent: "b"} Bone{name: "b"}] layers: []}"#, "parent"),
        (r#"Bones{bones: [Bone{name: "a"}] ik: [IK{end: "missing" target: vec2(0, 0)}] layers: []}"#, "end"),
        (r#"Bones{bones: [Bone{name: "a"}] ik: [IK{end: "a" target: vec2(0, 0)}] layers: []}"#, "end"),
        (r#"Bones{bones: [Bone{name: "a"}] follow: [Follow{bone: "missing" path: "M 0 0 L 10 0"}] layers: []}"#, "bone"),
        (r#"Bones{bones: [Bone{name: "a"}] layers: [Rect{size: vec2(10, 10) rigid: "missing"}]}"#, "rigid"),
        (r#"Image{src: asset("ast_arm", "arev_1") mesh: vec2(0, 3)}"#, "mesh"),
        (r#"Image{src: asset("ast_arm", "arev_1") mesh: vec2(2, -1)}"#, "mesh"),
        (r#"Image{src: asset("ast_arm", "arev_1") mesh: vec2(1.5, 3)}"#, "mesh"),
        (r#"Character{model: @pip reach: {elbow: vec2(10, 20)}}"#, "elbow"),
    ] {
        let source = format!("use mod.motion.*; Motion{{layers: [{layer}]}}");
        let errors = match MotionDoc::load(&source) { Ok(_) => panic!("accepted {layer}"), Err(errors) => errors };
        assert!(errors.iter().any(|e| e.is_error() && e.path.ends_with(field)), "{layer}: {errors:#?}");
    }
}

#[test]
fn keyed_fk_samples_without_advancing_state() {
    let doc = load(r#"Bones{bones: [Bone{name: "a" length: 100 rotate: keys([0, 0], [2, 90])}] layers: []}"#);
    let r = bones(&doc);
    let pose = rig::fk(&r.bones, &r.order, |i| sample(&r.bones[i].rotate, 2.0));
    near(pose[0].end, [0.0, 100.0]);
}

#[test]
fn input_targets_and_function_rest_use_the_authored_values() {
    let doc = MotionDoc::load(r#"use mod.motion.*; Motion{duration: 3 inputs: {hand: vec2(100, 40)}
        layers: [Bones{bones: [Bone{name: "a" length: 100} Bone{name: "b" length: 80}]
            ik: [IK{end: "b" target: in(@hand)}]
            layers: [Rect{at: vec2(180, 0) size: vec2(1, 1) rigid: "b"}]}]}"#).unwrap();
    for target in [[100.0, 40.0], [60.0, -80.0]] {
        let changed = doc.with_inputs(&[(LiveId::from_str("hand"), InputValue::Vec2(target))]).unwrap();
        let frame = changed.eval(1.0);
        assert!(frame.errors.is_empty());
        let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
        near(children[0].transform([1.0, 1.0]).apply([0.0, 0.0]), target);
    }
    let doc = load(r#"Bones{bones: [Bone{name: "a" length: 100 rotate: fn(t) { 30 + t * 45 }}]
        layers: [Image{src: asset("ast_arm", "arev_1") size: vec2(100, 20)}]}"#);
    assert_eq!(bones(&doc).bones[0].rest, 30.0);
    let frame = doc.eval(2.0);
    let ItemKind::Group { children, .. } = &frame.items[0].kind else { panic!() };
    let ItemKind::Image { mesh: Some(mesh), .. } = &children[0].kind else { panic!() };
    assert_eq!(mesh.positions.len(), 9 * 9, "default grid has 8 by 8 cells");
    near(mesh.positions[8], [0.0, 100.0]);
}

#[test]
fn deferred_character_models_report_missing_reach_joints() {
    let mut doc = load(r#"Character{src: asset("ast_character", "arev_1") reach: {hand_r: vec2(10, -20)}}"#);
    let messages = doc.resolve_characters(&|_| Some(r#"<svg viewBox="0 0 100 100"><g id="hips"><rect x="20" y="20" width="60" height="60"/></g></svg>"#.into()));
    assert!(messages.iter().any(|m| m.contains("reach.hand_r") && m.contains("connected")), "{messages:?}");
}

#[test]
fn crowd_character_reach_keeps_unreachable_warnings() {
    let doc = load(r#"Crowd{count: 2 size: vec2(300, 200)
        member: Character{model: @pip height: 500 blink: 0 sway: 0 reach: {hand_r: vec2(10000, 0)}}}"#);
    let frame = doc.eval(1.0);
    assert_eq!(frame.errors.iter().filter(|d| d.severity == Severity::Warning && d.path.ends_with("reach.hand_r")).count(), 2);
}
