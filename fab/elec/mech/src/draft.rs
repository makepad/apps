//! Face draft classification in the generator's z-up frame.

use makepad_csg_mesh::mesh::TriMesh;

/// How a face releases along the mould's pull axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftClass {
    /// Meets the minimum draft for release along +z.
    ReleasesUp,
    /// Meets the minimum draft for release along -z.
    ReleasesDown,
    /// Has some draft, but less than the required minimum.
    TooLittle,
    /// Has less than 0.1 degree of draft and does not meet the minimum.
    Straight,
    /// Turns against its own mould half's pull direction.
    Locked,
}

/// A shut-off region, in mm, with the growth used to show its draft.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DraftZone {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub z0: f64,
    pub z1: f64,
    pub grow: f64,
}

/// Draft requirements and shut-off regions for a part pulled along z.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftSpec {
    pub pull_min_deg: f64,
    /// Parting height in mm, or no restriction to either mould half.
    pub split: Option<f64>,
    pub zones: Vec<DraftZone>,
}

/// Classify an outward normal at a position in the generator's frame.
pub fn classify(normal: [f64; 3], pos: [f64; 3], spec: &DraftSpec) -> DraftClass {
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    let ang = (normal[2] / length).clamp(-1.0, 1.0).asin().to_degrees();
    let m = spec.pull_min_deg - 0.02;
    let mut side = 0;
    if let Some(split) = spec.split {
        side = if pos[2] >= split - 2e-3 { 1 } else { -1 };
        for zone in &spec.zones {
            let g = (pos[2] - zone.z0).max(0.0) * zone.grow;
            if pos[2] >= zone.z0
                && pos[2] <= zone.z1
                && pos[0] >= zone.x0 - g
                && pos[0] <= zone.x1 + g
                && pos[1] >= zone.y0 - g
                && pos[1] <= zone.y1 + g
            {
                side = 0;
            }
        }
    }
    if (side == 1 && ang <= -m) || (side == -1 && ang >= m) {
        DraftClass::Locked
    } else if ang >= m {
        DraftClass::ReleasesUp
    } else if ang <= -m {
        DraftClass::ReleasesDown
    } else if ang.abs() < 0.1 {
        DraftClass::Straight
    } else {
        DraftClass::TooLittle
    }
}

fn classify_face(mesh: &TriMesh, index: usize, spec: &DraftSpec) -> DraftClass {
    let [a, b, c] = mesh.triangles[index].map(|i| mesh.vertices[i as usize]);
    let pos = (a + b + c) / 3.0;
    let normal = mesh.triangle_normal(index);
    classify(
        [normal.x, normal.y, normal.z],
        [pos.x, pos.y, pos.z],
        spec,
    )
}

/// Classify each triangle's face normal at its centroid, in triangle order.
pub fn classify_mesh(mesh: &TriMesh, spec: &DraftSpec) -> Vec<DraftClass> {
    (0..mesh.triangle_count())
        .map(|index| classify_face(mesh, index, spec))
        .collect()
}

/// Surface area grouped by draft class.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftReport {
    /// Areas in square mm, indexed by `DraftClass as usize` in declaration order.
    pub area: [f64; 5],
}

/// Sum triangle areas for each class using face normals and centroids.
pub fn draft_report(mesh: &TriMesh, spec: &DraftSpec) -> DraftReport {
    let mut report = DraftReport::default();
    for index in 0..mesh.triangle_count() {
        report.area[classify_face(mesh, index, spec) as usize] += mesh.triangle_area(index);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{light_pipe, lower};
    use makepad_csg_mesh::validate::validate_mesh;

    fn normal(degrees: f64) -> [f64; 3] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        [cos, 0.0, sin]
    }

    #[test]
    fn angles_and_parting_keep_margins_and_test_order() {
        let mut spec = DraftSpec {
            pull_min_deg: 1.0,
            split: None,
            zones: Vec::new(),
        };
        for (case, angle, expected) in [
            ("three degrees up", 3.0, DraftClass::ReleasesUp),
            ("exact minimum up", 1.0, DraftClass::ReleasesUp),
            ("exact minimum down", -1.0, DraftClass::ReleasesDown),
            ("inside pass margin", 0.99, DraftClass::ReleasesUp),
            ("outside pass margin", 0.97, DraftClass::TooLittle),
            ("near straight up", 0.05, DraftClass::Straight),
            ("near straight down", -0.05, DraftClass::Straight),
            ("insufficient up", 0.5, DraftClass::TooLittle),
            ("insufficient down", -0.5, DraftClass::TooLittle),
            ("horizontal top", 90.0, DraftClass::ReleasesUp),
            ("horizontal bottom", -90.0, DraftClass::ReleasesDown),
        ] {
            assert_eq!(classify(normal(angle), [0.0; 3], &spec), expected,
                "{case}: elevation has the expected draft class");
        }
        assert_eq!(
            classify(normal(3.0).map(|v| v * 7.0), [0.0; 3], &spec),
            DraftClass::ReleasesUp,
            "scaled normal: classification normalizes the outward normal"
        );

        spec.split = Some(5.0);
        for (case, angle, z, expected) in [
            ("upper opposing face", -3.0, 6.0, DraftClass::Locked),
            ("lower matching face", -3.0, 4.0, DraftClass::ReleasesDown),
            ("lower opposing face", 3.0, 4.0, DraftClass::Locked),
            ("upper matching face", 3.0, 6.0, DraftClass::ReleasesUp),
            ("on split", -3.0, 5.0, DraftClass::Locked),
            ("at split margin", -3.0, 5.0 - 2e-3, DraftClass::Locked),
            ("below split margin", -3.0, 4.997, DraftClass::ReleasesDown),
            ("upper slight opposing face", -0.5, 6.0, DraftClass::TooLittle),
            ("upper straight face", -0.05, 6.0, DraftClass::Straight),
        ] {
            assert_eq!(classify(normal(angle), [0.0, 0.0, z], &spec), expected,
                "{case}: split margin and lock threshold are preserved");
        }

        spec.pull_min_deg = 0.0;
        assert_eq!(classify(normal(0.0), [0.0, 0.0, 6.0], &spec), DraftClass::Locked,
            "zero minimum with split: locking precedes release and straight tests");
        spec.split = None;
        assert_eq!(classify(normal(0.0), [0.0; 3], &spec), DraftClass::ReleasesUp,
            "zero minimum without split: upward release precedes downward release and straight tests");
    }

    #[test]
    fn shutoff_zones_accept_both_pulls_and_grow_with_height() {
        let spec = DraftSpec {
            pull_min_deg: 1.0,
            split: Some(5.0),
            zones: vec![DraftZone {
                x0: -1.0, y0: -2.0, x1: 1.0, y1: 2.0,
                z0: 2.0, z1: 8.0, grow: 0.5,
            }],
        };
        for z in [2.0, 4.0, 6.0, 8.0] {
            for (angle, expected) in [(3.0, DraftClass::ReleasesUp), (-3.0, DraftClass::ReleasesDown)] {
                assert_eq!(classify(normal(angle), [0.0, 0.0, z], &spec), expected,
                    "inside shut-off at z={z}, angle={angle}: either pull is accepted, including height endpoints");
            }
        }
        for (case, angle, pos, expected) in [
            ("base xy boundary", 3.0, [1.0, 2.0, 2.0], DraftClass::ReleasesUp),
            ("outside base", 3.0, [1.01, 0.0, 2.0], DraftClass::Locked),
            ("expanded positive corner", -3.0, [3.0, 4.0, 6.0], DraftClass::ReleasesDown),
            ("expanded negative corner", -3.0, [-3.0, -4.0, 6.0], DraftClass::ReleasesDown),
            ("outside positive x", -3.0, [3.01, 0.0, 6.0], DraftClass::Locked),
            ("outside negative x", -3.0, [-3.01, 0.0, 6.0], DraftClass::Locked),
            ("outside positive y", -3.0, [0.0, 4.01, 6.0], DraftClass::Locked),
            ("outside negative y", -3.0, [0.0, -4.01, 6.0], DraftClass::Locked),
            ("below zone", 3.0, [0.0, 0.0, 1.99], DraftClass::Locked),
            ("above zone", -3.0, [0.0, 0.0, 8.01], DraftClass::Locked),
        ] {
            assert_eq!(classify(normal(angle), pos, &spec), expected,
                "{case}: shut-off bounds grow from z0 and remain inclusive");
        }
        assert_eq!(classify(normal(0.5), [0.0, 0.0, 6.0], &spec), DraftClass::TooLittle,
            "inside shut-off: either pull still requires the minimum draft");
    }

    #[test]
    fn light_pipe_has_no_locked_area_and_its_tip_releases_up() {
        let params = light_pipe::PipeParams { segments: 32, ..Default::default() };
        let built = light_pipe::build(&params);
        let mesh = lower::mesh(&built.csg).expect("default pipe: solid meshes");
        let topology = validate_mesh(&mesh);
        assert!(topology.is_closed, "default pipe: mesh is closed: {topology:?}");
        assert_eq!((topology.near_duplicate_pairs, topology.coincident_pairs), (0, 0),
            "default pipe: no vertex pairs within the near-duplicate radius: {topology:?}");
        let spec = DraftSpec {
            pull_min_deg: params.draft,
            split: Some(built.report.parting_z),
            zones: built.zones,
        };
        let classes = classify_mesh(&mesh, &spec);
        let report = draft_report(&mesh, &spec);
        assert_eq!(classes.len(), mesh.triangle_count(),
            "default pipe: one class is returned for every triangle");
        assert_eq!(report.area[DraftClass::Locked as usize], 0.0,
            "default pipe: no face area is locked in its mould half: {report:?}");
        let total_area: f64 = (0..mesh.triangle_count()).map(|i| mesh.triangle_area(i)).sum();
        assert!(total_area > 0.0, "default pipe: mesh has positive surface area");
        assert!((report.area.iter().sum::<f64>() - total_area).abs() <= total_area * 1e-12,
            "default pipe: draft classes account for the entire surface area");
        let mut tip_faces = 0;
        for (index, triangle) in mesh.triangles.iter().enumerate() {
            if triangle.iter().all(|&i| mesh.vertices[i as usize].z > built.report.shoulder_top) {
                tip_faces += 1;
                assert_eq!(classes[index], DraftClass::ReleasesUp,
                    "default pipe tip triangle {index}: face releases upward");
            }
        }
        assert!(tip_faces > 0, "default pipe: tip faces were checked");
    }
}
