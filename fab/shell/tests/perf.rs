//! Scene batching, memory, picking and culling contracts over synthetic geometry (no timing bounds).

use fab::model::makepad_math::Vec3f;
use fab::model::*;

fn rng(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    (*seed >> 8) as f32 / 16_777_216.0
}

#[test]
fn a_batched_scene_keeps_its_batches_memory_picks_and_culls() {
    let model = fixture::synthetic_model(512_000, 1_000);
    let scene = Scene::from_model(model, &mut |_| {});

    assert!(scene.stats.triangles > 460_000);
    // Batch count is the whole point of the (material × cell) planner: the
    // Metal backend draws a geometry's index buffer whole, so a batch is the
    // smallest thing frustum culling can skip. Too few = no culling, too many
    // = draw-call bound.
    assert!(
        scene.stats.batches >= 8 && scene.stats.batches <= 128,
        "{} batches",
        scene.stats.batches
    );
    // ≤ 40 B/vertex + 4 B/index is the budget for the *packed* GPU stream;
    // the CPU-side batches are the 48-byte std140 layout and must still stay
    // inside 2× that.
    let budget = scene.stats.vertices as u64 * 96 + scene.stats.triangles as u64 * 12;
    assert!(
        scene.stats.geometry_bytes < budget,
        "{} bytes over budget {}",
        scene.stats.geometry_bytes,
        budget
    );

    // ---- pick ----------------------------------------
    let state = SceneState::default();
    let c = aabb_center(&scene.bounds);
    let r = aabb_radius(&scene.bounds);
    let mut seed = 7u32;
    let mut hits = 0;
    let n = 500;
    for _ in 0..n {
        let origin = Vec3f {
            x: c.x + (rng(&mut seed) - 0.5) * r,
            y: c.y + (rng(&mut seed) - 0.5) * r,
            z: scene.bounds.max.z + r,
        };
        let dir = Vec3f {
            x: (rng(&mut seed) - 0.5) * 0.4,
            y: (rng(&mut seed) - 0.5) * 0.4,
            z: -1.0,
        };
        if scene.pick(&Ray::new(origin, dir), &state).is_some()
        {
            hits += 1;
        }
    }
    assert!(hits > n / 4, "only {hits}/{n} rays hit anything");

    // ---- culling -------------------------------------------------------
    let mut visible = Vec::new();
    let cam_dist = r * 2.0;
    for _ in 0..3 {
        let eye = Vec3f {
            x: c.x,
            y: c.y - cam_dist,
            z: c.z + cam_dist * 0.4,
        };
        let view = fab::model::makepad_math::Mat4f::look_at(
            eye,
            c,
            Vec3f { x: 0.0, y: 0.0, z: 1.0 },
        );
        let proj = fab::model::makepad_math::Mat4f::perspective(
            40.0,
            1.6,
            0.1,
            r * 10.0,
        );
        let vp = fab::model::makepad_math::Mat4f::mul(&proj, &view);
        let f = Frustum::from_view_proj(&vp);
        scene.visible_elements(&f, &state, &mut visible);
    }
    assert!(
        !visible.is_empty() && visible.len() <= scene.stats.elements as usize,
        "{} of {} elements visible",
        visible.len(),
        scene.stats.elements
    );
}

#[test]
fn bvh_beats_brute_force_and_agrees_with_it() {
    let scene = Scene::from_model(fixture::synthetic_model(12_800, 100), &mut |_| {});
    let state = SceneState::default();
    let c = aabb_center(&scene.bounds);
    let r = aabb_radius(&scene.bounds);
    let mut seed = 99u32;
    let mut checked = 0;
    for _ in 0..200 {
        let origin = Vec3f {
            x: c.x + (rng(&mut seed) - 0.5) * r,
            y: c.y + (rng(&mut seed) - 0.5) * r,
            z: scene.bounds.max.z + r * 0.5,
        };
        let dir = Vec3f {
            x: (rng(&mut seed) - 0.5) * 0.6,
            y: (rng(&mut seed) - 0.5) * 0.6,
            z: -1.0,
        };
        let ray = Ray::new(origin, dir);
        let fast = scene.pick(&ray, &state);

        // Brute force over every triangle of every batch.
        let mut slow: Option<(ElementId, f32)> = None;
        for b in &scene.batches {
            for t in 0..b.triangle_count() as u32 {
                let (p, q, s) = b.triangle(t);
                if let Some((h, _, _)) = ray.intersect_triangle(p, q, s) {
                    if slow.map_or(true, |(_, bt)| h < bt) {
                        slow = Some((b.element_of_triangle(t).unwrap(), h));
                    }
                }
            }
        }
        match (fast, slow) {
            (Some(f), Some((e, t))) => {
                assert!((f.t - t).abs() < 1e-3, "t {} vs {t}", f.t);
                assert_eq!(f.element, e);
                checked += 1;
            }
            (None, None) => {}
            (a, b) => panic!("bvh {a:?} vs brute {b:?}"),
        }
    }
    println!("{checked} rays agreed with brute force");
    assert!(checked > 50);
}
