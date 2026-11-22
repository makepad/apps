//! The fill's classes between a field plan and the planner: the class rules a plan spec carries
//! (from the planner's own [`ClassRules`]) and the GPU's regions as the planner's classes.
use makepad_csg_sdf::plan::{self, Region};
use makepad_fab_slicer::{geom::{ExPolygon, Polygon, Pt}, source::Classes};
use makepad_fab_toolpath::{stages::ClassRules, Params};

/// The class rules of a part, one level set per wall set (`heights[k]` of each set's first layer).
pub fn shell_spec(p: &Params, set_heights: &[f64]) -> plan::ShellSpec {
    plan::ShellSpec {
        top_layers: p.top_layers as u32,
        bottom_layers: p.bottom_layers as u32,
        vertical_shells: p.vertical_shells,
        floating_shells: p.floating_shells,
        narrow_solid: p.narrow_solid,
        sets: set_heights.iter().map(|&h| levels(&ClassRules::of(p, h))).collect(),
    }
}

fn levels(r: &ClassRules) -> plan::ShellLevels {
    plan::ShellLevels {
        speck: r.speck as f32,
        face_grow: r.face_grow as f32,
        bottom_grow: r.bottom_grow as f32,
        shell_open: r.shell_open as f32,
        min_sparse_area: r.min_sparse_area as f32,
        vertical: r.vertical as f32,
        bridge_open: r.bridge_open as f32,
        bridge_area: r.bridge_area as f32,
        bridge_grow: r.bridge_grow as f32,
        under_grow: r.under_grow as f32,
        band_core: r.band_core as f32,
        band_area: r.band_area as f32,
        adjacent: r.adjacent as f32,
        narrow: r.narrow as f32,
    }
}

/// One island's regions as the planner's classes. Loops come each contour before its holes.
pub fn classes(regions: &plan::IslandRegions) -> Classes {
    let mut c = Classes::default();
    for (class, loops) in &regions.classes {
        let into = match class {
            Region::Top => &mut c.top,
            Region::Bottom => &mut c.bottom,
            Region::Solid => &mut c.solid,
            Region::Sparse => &mut c.sparse,
            Region::InternalBridge => &mut c.bridge,
            Region::VerticalShell => &mut c.vertical,
            Region::ShellBand => &mut c.band,
        };
        for ring in loops {
            let points = Polygon::new(ring.pts.iter().map(|p| Pt::new(p[0] as i64, p[1] as i64)).collect());
            if ring.hole {
                into.last_mut().expect("a region's hole follows its contour").holes.push(points);
            } else {
                into.push(ExPolygon::new(points));
            }
        }
    }
    c
}
