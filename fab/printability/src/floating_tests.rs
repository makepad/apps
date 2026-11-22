use super::*;
use fab_plate::{machine::Machine, Part, PartMesh, Plate};
use makepad_fab_settings::{machine::Job, profiles::{JobSetup, Preset}};
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt};
use makepad_fab_toolpath::{par::Ctl, skeleton::Skeleton, support_paint::Masks};

#[test]
fn floating_placement_uses_the_support_plan_at_the_lowest_face() {
    let mut process = Preset::default();
    for (k, v) in [("enable_support", "1"), ("support_type", "tree(auto)"), ("support_on_build_plate_only", "1")] {
        process.values.set_str(k, v);
    }
    let setup = JobSetup { machine: Preset::default(), process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    let params = makepad_fab_toolpath::Params::from_job(&Job::new(setup.build_config()), 0).support.expect("supports enabled");
    let region = ExPolygon::new(Polygon::rect(Pt::from_mm(100.0, 100.0), Pt::from_mm(108.0, 108.0)));
    let slices: Vec<_> = (0..30).map(|l| if l >= 25 { vec![region.clone()] } else { Vec::new() }).collect();
    let heights: Vec<_> = (0..30).map(|l| (0.2 * (l + 1) as f64, 0.2)).collect();
    let mut held = Skeleton::default();
    makepad_fab_toolpath::support::generate_recording(Ctl::default(), &slices, &heights, &params, &mut held).unwrap();
    assert!(!held.chains.is_empty(), "the fixture has real supports");
    assert!(held.given_up.is_empty(), "the lowest face is held: {:?}", held.given_up);

    let mesh = PartMesh {
        positions: (0..8).map(|i| [100.0 + 8.0 * (i & 1) as f64, 100.0 + 8.0 * ((i >> 1) & 1) as f64, 5.0 + ((i >> 2) & 1) as f64]).collect(),
        triangles: vec![[0,2,1], [1,2,3], [4,5,6], [5,7,6], [0,1,4], [1,5,4], [2,6,3], [3,6,7], [0,4,2], [2,4,6], [1,3,5], [3,7,5]],
    };
    let mut part = Part { body_of: None, print: vec![("enable_support".into(), "true".into())], print_mesh: None, paint: None, object: "raised".into(), name: "raised".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
    let machine = Machine::dual_nozzle_350();
    let materials = Materials::default();
    let report = |p: &Part, plan: Option<&Skeleton>| {
        let plate = Plate { parts: vec![p.clone()], excluded: Vec::new(), settings: Vec::new() };
        let mut r = analyse_plate(&machine, &plate, &materials);
        if let Some(plan) = plan {
            r.issues.extend(support_issues(&p.object, plan, &heights, [0.0; 2]));
            r.issues.extend(supported_placement(&machine, p, &materials, plan, Some(25)));
        }
        r.issues.into_iter().filter(|i| i.kind == "mid_air").collect::<Vec<_>>()
    };
    let supported = report(&part, Some(&held));
    assert_eq!(supported.len(), 1, "one floating placement verdict");
    assert_eq!(supported[0].severity, Severity::Info, "held above the plate: {supported:?}");
    assert_eq!(supported[0].measure, 5.0);
    assert!(supported[0].message.contains("stands on its supports") && supported[0].message.contains("5.00"));
    assert!(!supported[0].hint.contains("add supports"));
    assert!(report(&part, None).is_empty(), "no claim before the plan exists");
    assert!(supported_placement(&machine, &part, &materials, &held, None).is_none(), "no claim without an occupied model layer");
    assert_eq!(report(&part, Some(&Skeleton::default()))[0].severity, Severity::Warning, "an empty plan cannot hold the part");

    part.print[0].1 = "false".into();
    for plan in [None, Some(&held)] {
        let off = report(&part, plan);
        assert_eq!(off.len(), 1);
        assert_eq!(off[0].severity, Severity::Warning, "the part's disabled setting wins");
    }
    part.print[0].1 = "true".into();

    let paint = makepad_fab_slicer::source::SupportPaint {
        block: Some(makepad_fab_slicer::source::SupportRegion::new(
            std::sync::Arc::new(|x, y, z| ((x - 104.0).abs().max((y - 104.0).abs()) - 5.0).max((z - 3.0).abs() - 3.0)),
            Some(([99.0, 99.0, 0.0], [109.0, 109.0, 6.0])),
        )),
        enforce: None,
        no_trees: None,
    };
    let zs: Vec<_> = heights.iter().map(|&(z, h)| z - h * 0.5).collect();
    // Read where supports can be: all of the block, its own box.
    let masks = Masks::of(Ctl::default(), Some(paint), &zs, ([100.0, 100.0, 5.0], [108.0, 108.0, 6.0]), ([99.0, 99.0], [109.0, 109.0])).unwrap();
    let mut blocked = Skeleton::default();
    makepad_fab_toolpath::support::generate_painted(Ctl::default(), &slices, &heights, &params, &masks, &mut blocked).unwrap();
    assert!(blocked.blocked.iter().any(|g| g.layer == 25 && g.area > 0.0));
    let warning = report(&part, Some(&blocked));
    assert_eq!(warning.len(), 1);
    assert_eq!(warning[0].severity, Severity::Warning, "blocked underside: {warning:?}");

    // The same missing area can be reported by reachability or enclosure.
    let mut unreached = held.clone();
    unreached.given_up = blocked.blocked.clone();
    assert_eq!(report(&part, Some(&unreached))[0].severity, Severity::Warning);
    unreached.inside = std::mem::take(&mut unreached.given_up);
    assert_eq!(report(&part, Some(&unreached))[0].severity, Severity::Warning);
    // An upper ledge does not change the verdict about the lowest face.
    for g in &mut unreached.inside { g.layer = 28; }
    assert_eq!(report(&part, Some(&unreached))[0].severity, Severity::Info);
}
