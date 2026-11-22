use fab_elec_route::organic::{geom::*, grid::*, settings::*};
use std::collections::BTreeSet;

#[test]
fn organic_grid_properties() {
    let mut state = 0x4139_7ac5_u32;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / 16_777_216.0
    };

    let mut grid = UniformGrid::default();
    let mut visits = 0;
    grid.query(-1.0, -1.0, 1.0, 1.0, |_| visits += 1);
    assert_eq!(visits, 0, "uninitialized grid queries are empty");

    let cell_size = 1.25;
    grid.init(
        &Box2::new(Vec2::new(0.0, 0.0), Vec2::new(50.0, 30.0)),
        cell_size,
        UNIFORM_GRID_MAX_CELLS,
    );
    let boxes: Vec<_> = (0..300)
        .map(|id| {
            let min = Vec2::new(next() * 48.0, next() * 28.0);
            let b = Box2::new(min, min + Vec2::new(next() * 2.0, next() * 2.0));
            grid.add(id, b.min.x, b.min.y, b.max.x, b.max.y);
            b
        })
        .collect();
    grid.finalize();
    for query_index in 0..100 {
        let min = Vec2::new(next() * 48.0, next() * 28.0);
        let query = Box2::new(min, min + Vec2::new(next() * 2.0, next() * 2.0));
        let mut returned = BTreeSet::new();
        grid.query(query.min.x, query.min.y, query.max.x, query.max.y, |id| {
            returned.insert(id as usize);
        });
        let brute: BTreeSet<_> = boxes
            .iter()
            .enumerate()
            .filter_map(|(id, b)| b.overlaps(&query).then_some(id))
            .collect();
        assert!(
            brute.is_subset(&returned),
            "uniform query {query_index} includes every inclusive box overlap"
        );
        let mut expanded = query;
        expanded.pad(cell_size);
        for id in returned {
            assert!(
                boxes[id].overlaps(&expanded),
                "uniform query {query_index}: returned box {id} is within one cell"
            );
        }
    }

    // Reinitialization also proves that no registrations survive into a new grid.
    grid.init(
        &Box2::new(Vec2::new(0.0, 0.0), Vec2::new(4.0, 4.0)),
        1.0,
        UNIFORM_GRID_MAX_CELLS,
    );
    for (id, (x, y)) in [
        (0.25, 0.25),
        (3.75, 0.25),
        (0.25, 3.75),
        (3.75, 3.75),
        (1.5, 1.5),
    ]
    .into_iter()
    .enumerate()
    {
        grid.add(id as i32, x, y, x, y);
    }
    grid.add(5, 1.5, 1.5, 2.0, 2.0);
    grid.finalize();
    let collect = |minx, miny, maxx, maxy| {
        let mut ids = Vec::new();
        grid.query(minx, miny, maxx, maxy, |id| ids.push(id));
        ids
    };
    assert_eq!(
        collect(-4.0, -4.0, -2.0, -2.0),
        vec![0],
        "entirely outside low query returns only its border cell"
    );
    assert_eq!(
        collect(5.0, 5.0, 8.0, 8.0),
        vec![3],
        "entirely outside high query returns only its border cell"
    );
    assert_eq!(
        collect(f32::NAN, 0.0, -1.0, 4.0),
        vec![0, 2],
        "NaN x clamps to the first border column without panicking"
    );
    assert_eq!(
        collect(0.0, f32::NAN, 4.0, -1.0),
        vec![0, 1],
        "NaN y clamps to the first border row without panicking"
    );
    assert_eq!(
        collect(2.0, 2.0, 2.0, 2.0),
        vec![5],
        "inclusive cell-boundary contact returns the registered box"
    );
    let ordered = vec![0, 1, 4, 5, 5, 5, 5, 2, 3];
    assert_eq!(
        collect(0.0, 0.0, 4.0, 4.0),
        ordered,
        "CSR query preserves row, column and insertion order, including duplicate ids"
    );
    assert_eq!(
        collect(4.0, 4.0, 0.0, 0.0),
        ordered,
        "reversed coordinates swap both cell ranges without changing visit order"
    );

    let mut grown = UniformGrid::default();
    grown.init(&Box2::new(Vec2::new(0.0, 0.0), Vec2::new(100.0, 100.0)), 1.0, 100);
    let (nx, ny) = grown.cells();
    assert!(nx > 0 && ny > 0 && nx * ny <= 100, "cell growth respects max_cells = 100");
    grown.add(7, 0.0, 0.0, 100.0, 100.0);
    grown.finalize();
    let mut cells_visited = 0;
    grown.query(0.0, 0.0, 100.0, 100.0, |id| {
        assert_eq!(id, 7, "grown grid retains registered ids");
        cells_visited += 1;
    });
    assert_eq!(cells_visited, nx * ny, "grown-grid query visits every cell once");

    let mut hash = SpatialHashGrid::default();
    assert_eq!(hash.cell_size, 2.5, "spatial hash uses the reference default cell size");
    assert_eq!(
        hash.get_cell_coords(Vec2::new(-0.1, -2.5)),
        (-1, -1),
        "negative cell coordinates use floor, including an exact boundary"
    );
    assert_eq!(
        SpatialHashGrid::hash_coords(-1, -2),
        0xffff_ffff_ffff_fffe,
        "hash packs signed coordinates through u32 before u64"
    );
    let nodes: Vec<_> = (0..200)
        .map(|id| {
            let node = GridNodeRef {
                trace_index: id,
                point_index: id * 2,
                pos: Vec2::new(next() * 100.0 - 50.0, next() * 60.0 - 30.0),
                ..GridNodeRef::default()
            };
            hash.insert(&node);
            node
        })
        .collect();
    for query_index in 0..100 {
        let pos = Vec2::new(next() * 100.0 - 50.0, next() * 60.0 - 30.0);
        let radius = 0.1 + next() * 5.0;
        let mut first = Vec::new();
        let mut second = Vec::new();
        hash.query_nearby(pos, radius, |node| first.push(node.trace_index));
        hash.query_nearby(pos, radius, |node| second.push(node.trace_index));
        assert_eq!(first, second, "spatial hash query {query_index} repeats in the same order");
        for node in &nodes {
            if node.pos.distance_squared_to(pos) <= radius * radius {
                assert!(
                    first.contains(&node.trace_index),
                    "spatial hash query {query_index} includes every node within its radius"
                );
            }
        }
        let (min_x, min_y) = hash.get_cell_coords(pos - Vec2::new(radius, radius));
        let (max_x, max_y) = hash.get_cell_coords(pos + Vec2::new(radius, radius));
        let mut expected: Vec<_> = nodes
            .iter()
            .filter_map(|node| {
                let (cx, cy) = hash.get_cell_coords(node.pos);
                (cx >= min_x && cx <= max_x && cy >= min_y && cy <= max_y)
                    .then_some((cy, cx, node.trace_index))
            })
            .collect();
        expected.sort_unstable();
        let expected: Vec<_> = expected.into_iter().map(|(_, _, id)| id).collect();
        assert_eq!(
            first, expected,
            "spatial hash query {query_index} visits rows then columns then insertion order"
        );
    }
    hash.clear();
    let mut cleared_visits = 0;
    hash.query_nearby(Vec2::new(0.0, 0.0), 100.0, |_| cleared_visits += 1);
    assert_eq!(cleared_visits, 0, "clearing the spatial hash removes every node");

    let defaults = OrganicSettings::default();
    let base = OrganicSettings::base();
    let expected = OrganicSettings {
        layer_crossing_perpendicularity: 0.0,
        via_via_repulsion: 0.0,
        crossing_disentanglement: 0.0,
        wide_track_straightening: 0.0,
        wide_track_snap_deg: 0.0,
        wide_track_line_pull: 0.0,
        pad_exit_half_plane: 0.0,
        hop_keep_dist: 0.0,
        wire_spread: 0.0,
        straight_pass_relief: 0.0,
        bundle_keep: 0.0,
        straight_bend: 0.0,
        wide_bend_settle: false,
        enable_diff_pair_coupling: false,
        enable_diff_pair_skew_tuning: false,
        enable_length_tuning: false,
        enable_target_length_matching: false,
        enable_dynamic_vias: false,
        enable_loop_simplification: false,
        enable_biarcs: false,
        enable_dynamic_neckdown: false,
        enable_teardrops: false,
        ..defaults.clone()
    };
    assert_eq!(base, expected, "base() turns off exactly the optional terms it lists");
    assert_ne!(base, defaults, "base() differs from the reference defaults");
    for term in ForceTerm::ALL {
        assert_eq!(
            force_term_from_key(force_term_key(term)),
            Some(term),
            "force-term key round trip: {term:?}"
        );
    }
    assert_eq!(force_term_from_key("unknown"), None, "unknown force keys are rejected");
    assert_eq!(force_term_from_key("Tension"), None, "force keys are case sensitive");
}
