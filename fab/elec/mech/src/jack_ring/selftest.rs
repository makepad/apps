use super::*;
use crate::csg::Shape;
use fab_materials::{FinishId, MaterialId};
use makepad_csg_mesh::{
    validate::{pieces, validate_mesh},
    volume::mesh_volume,
};

#[derive(Clone, Copy)]
enum VolumeCheck {
    None,
    Annulus,
    Thread,
}

struct Case {
    name: String,
    p: RingParams,
    warnings: Vec<&'static str>,
    notes: Vec<&'static str>,
    volume: VolumeCheck,
}

impl Case {
    fn new(name: impl Into<String>, p: RingParams, notes: &[&'static str]) -> Self {
        Self {
            name: name.into(),
            p,
            warnings: Vec::new(),
            notes: notes.to_vec(),
            volume: VolumeCheck::None,
        }
    }
}

fn parameters() -> RingParams {
    RingParams {
        segments: 48,
        ..RingParams::default()
    }
}

fn set(p: &mut RingParams, key: &str, value: f64) {
    let table = param_table();
    table.set(p, table.find(key).expect("case parameter exists"), value);
}

fn preset(name: &str, text: &str, notes: &[&'static str]) -> Case {
    let mut p = parameters();
    param_table()
        .from_text(&text.replace(';', "\n"), &mut p, None)
        .unwrap_or_else(|e| panic!("{name}: parameters parse: {e}"));
    p.name = name.to_owned();
    Case::new(name, p, notes)
}

fn cases() -> Vec<Case> {
    let mut all = vec![
        preset(
            "small_silver_knurl",
            "thread_std=M6 x 0.5; thread_d=6; pitch=0.5; lead_in=0.4;
             diameter=9; height=2.2; top_edge_size=0.4; bottom_edge_size=0.15;
             grip=Straight knurl; ribs=30; grip_depth=0.2; grip_from=0.15; grip_to=0.85;
             bushing_length=4.5; material=aluminium_anodised; anodize=Clear;
             finish=cnc_3_2; color=#E8EBED",
            &[
                "Body sampled at 240 points around (whole teeth / flats).",
                "The bushing stands 0.30 mm proud of the ring.",
            ],
        ),
        preset(
            "black_knurled",
            "height=4; top_edge=Round; top_edge_size=0.5; grip=Diamond knurl;
             ribs=40; grip_from=0.1; grip_to=0.85; material=aluminium_anodised;
             finish=cnc_3_2; color=#121214",
            &[
                "Body sampled at 320 points around (whole teeth / flats).",
                "The bushing stands 1.00 mm proud of the ring.",
            ],
        ),
        preset(
            "black_oxide_flats",
            "top_edge=Round; grip=Spanner flats; material=steel_black_oxide;
             finish=turned; color=#1C1C1F",
            &["The bushing stands 1.50 mm proud of the ring."],
        ),
        preset(
            "brass_bevel",
            "height=3.2; top_edge_size=1.2; top_angle=35; color=#F2DCAC",
            &["The bushing stands 1.80 mm proud of the ring."],
        ),
        preset(
            "nickel_step_flange",
            "diameter=14.5; height=4; top_edge=Round; top_edge_size=0.5;
             top_style=Step; step_diameter=12.4; flange=true; flange_diameter=18;
             material=nickel_brass; color=#E7E5E1",
            &["The bushing stands 1.00 mm proud of the ring."],
        ),
        preset(
            "stainless_hex",
            "diameter=16.17; top_edge_size=0.7; top_angle=30; bottom_edge_size=0.5;
             grip=Hex; material=stainless_303; finish=turned; color=#E2E2E4",
            &["The bushing stands 1.50 mm proud of the ring."],
        ),
        preset(
            "teal_fluted",
            "diameter=16.5; height=4; top_edge=Round; top_edge_size=0.4;
             top_style=Dome; dome_height=0.9; grip=Flutes; ribs=18; grip_depth=0.45;
             grip_from=0; grip_to=0.9; material=aluminium_anodised; anodize=Teal;
             finish=turned; color=#126E6E",
            &[
                "Body sampled at 216 points around (whole teeth / flats).",
                "The bushing stands 1.32 mm proud of the ring.",
            ],
        ),
        preset(
            "unef_brass_knurl",
            "thread_std=3/8\"-32 UNEF; thread_d=9.525; pitch=0.79375;
             diameter=14; height=3.2; top_edge_size=0.5; grip=Straight knurl;
             ribs=48; grip_depth=0.25; grip_from=0.15; grip_to=0.85;
             bushing_length=6.35; finish=turned; color=#F2DCAC",
            &[
                "Body sampled at 384 points around (whole teeth / flats).",
                "The bushing stands 1.15 mm proud of the ring.",
            ],
        ),
    ];

    let mut plain = RingParams {
        thread_mode: ThreadMode::Cosmetic.index(),
        top_edge: EdgeStyle::Sharp.index(),
        bottom_edge: EdgeStyle::Sharp.index(),
        lead_in: 0.0,
        // A 48-sided circle misses the 0.2 percent analytic volume tolerance.
        segments: 64,
        ..parameters()
    };
    let mut annulus = Case::new(
        "plain_annulus",
        plain.clone(),
        &["The bushing stands 1.50 mm proud of the ring."],
    );
    annulus.volume = VolumeCheck::Annulus;
    all.push(annulus);
    plain.thread_mode = ThreadMode::Modelled.index();
    plain.segments = 48;
    let mut threaded = Case::new(
        "plain_thread",
        plain,
        &["The bushing stands 1.50 mm proud of the ring."],
    );
    threaded.volume = VolumeCheck::Thread;
    all.push(threaded);

    for grip in 0..Grip::NAMES.len() as i32 {
        let mut p = parameters();
        set(&mut p, "grip", grip as f64);
        let mut notes = Vec::new();
        match p.grip() {
            Grip::StraightKnurl | Grip::DiamondKnurl => {
                notes.push("Body sampled at 288 points around (whole teeth / flats).");
            }
            Grip::Flutes => {
                notes.push("Body sampled at 432 points around (whole teeth / flats).");
            }
            _ => {}
        }
        notes.push("The bushing stands 1.50 mm proud of the ring.");
        all.push(Case::new(format!("grip_{grip}"), p, &notes));
    }
    for top in 0..TopStyle::NAMES.len() as i32 {
        for edge in 0..EdgeStyle::NAMES.len() as i32 {
            let mut p = parameters();
            set(&mut p, "top_style", top as f64);
            set(&mut p, "top_edge", edge as f64);
            set(&mut p, "height", 4.0);
            let note = match (p.top_style(), p.top_edge()) {
                (TopStyle::Dome, EdgeStyle::Sharp) => {
                    "The bushing stands 1.28 mm proud of the ring."
                }
                (TopStyle::Dome, _) => "The bushing stands 1.34 mm proud of the ring.",
                _ => "The bushing stands 1.00 mm proud of the ring.",
            };
            all.push(Case::new(format!("top_{top}_edge_{edge}"), p, &[note]));
        }
    }
    for standard in 1..ThreadStd::NAMES.len() as i32 {
        let data = thread_data(ThreadStd::from_index(standard));
        let mut p = parameters();
        set(&mut p, "thread_std", standard as f64);
        set(&mut p, "diameter", data.d + 5.0);
        set(&mut p, "height", 3.5_f64.max(4.0 * data.pitch));
        let length = p.panel_thickness + p.height + 1.0;
        set(&mut p, "bushing_length", length);
        set(&mut p, "top_edge_size", 0.5);
        let mut notes = vec!["The bushing stands 1.00 mm proud of the ring."];
        if data.pitch >= 1.25 {
            notes.push("Lead-in stops inside the thread's depth: the first turn starts sharp.");
        }
        all.push(Case::new(format!("thread_{standard}"), p, &notes));
    }
    let mut p = RingParams {
        flange: true,
        slots: 4,
        ..parameters()
    };
    set(&mut p, "thread_mode", ThreadMode::Clearance.index() as f64);
    set(&mut p, "grip", Grip::DiamondKnurl.index() as f64);
    all.push(Case::new(
        "flange_slots_knurl",
        p,
        &[
            "Body sampled at 288 points around (whole teeth / flats).",
            "Clearance bore: the ring slides over the bushing; the jack's own nut (under it) or glue holds it.",
        ],
    ));

    let mut p = parameters();
    set(&mut p, "grip", Grip::Hex.index() as f64);
    set(&mut p, "across_flats", 11.5);
    set(&mut p, "bushing_length", 2.6);
    let mut bad = Case::new(
        "bad_wall_engagement",
        p,
        &[
            "Hex corners (13.28 mm) inside the diameter: it is a plain hex.",
            "The bushing ends 2.90 mm below the ring's top.",
        ],
    );
    bad.warnings = vec![
        "Thin wall: 0.20 mm between the thread's root and the side (keep 0.8 or more).",
        "Only 0.5 threads engage on the bushing (3 or more hold).",
    ];
    all.push(bad);
    all
}

fn check_thread(case: &Case, built: &RingBuild) {
    let name = &case.name;
    let p = &case.p;
    let step = built.csg.steps.iter().find(|s| s.prim.label == "thread")
        .unwrap_or_else(|| panic!("{name}: modelled thread cutter exists"));
    let Shape::Loft { rings, .. } = &step.prim.shape else {
        panic!("{name}: modelled thread is a loft");
    };
    let start = rings.iter().position(|ring| ring.z >= p.lead_in)
        .unwrap_or_else(|| panic!("{name}: thread extends beyond the lower countersink"));
    assert!(start + 24 < rings.len(), "{name}: a whole thread turn is available");
    let a = &rings[start];
    let quarter = &rings[start + 6];
    let b = &rings[start + 24];
    assert!(b.z <= built.report.top_z - p.lead_in,
        "{name}: measured turn lies below the upper countersink");
    assert!((b.z - a.z - p.pitch).abs() < 5e-5,
        "{name}: measured axial pitch {} must equal {}", b.z - a.z, p.pitch);
    let radius = |point: &[f64; 2]| point[0].hypot(point[1]);
    let low = a.pts.iter().map(radius).fold(f64::INFINITY, f64::min);
    let high = a.pts.iter().map(radius).fold(f64::NEG_INFINITY, f64::max);
    assert!((high - low - 0.54125 * p.pitch).abs() < 5e-6,
        "{name}: measured radial thread depth {} must be 0.54125 pitch", high - low);
    assert!((high - (p.thread_d * 0.5 + p.clearance)).abs() < 5e-6,
        "{name}: thread root includes the specified radial clearance");
    for (i, point) in a.pts.iter().enumerate() {
        assert!((radius(point) - radius(&b.pts[i])).abs() < 5e-5,
            "{name}: thread radius repeats after one pitch at sample {i}");
        let j = (i + a.pts.len() / 4) % a.pts.len();
        assert!((radius(point) - radius(&quarter.pts[j])).abs() < 5e-5,
            "{name}: a positive quarter turn rises by a quarter pitch at sample {i}");
    }
}

#[test]
fn presets_and_features_are_closed_with_expected_volumes_and_fit() {
    for case in cases() {
        let name = &case.name;
        let p = &case.p;
        let built = build(p);
        assert_eq!(built.warnings, case.warnings, "{name}: expected warning text and order");
        assert_eq!(built.notes, case.notes, "{name}: expected note text and order");
        if p.thread_mode() == ThreadMode::Modelled {
            check_thread(&case, &built);
        }
        let mesh = crate::lower::mesh(&built.csg)
            .unwrap_or_else(|e| panic!("{name}: meshing failed: {e}"));
        assert!(!mesh.triangles.is_empty(), "{name}: body is nonempty");
        let report = validate_mesh(&mesh);
        assert!(report.is_closed, "{name}: body is closed: {report:?}");
        assert!(report.is_manifold, "{name}: body is manifold: {report:?}");
        assert!(report.is_consistently_oriented,
            "{name}: body has consistent winding: {report:?}");
        assert_eq!((report.near_duplicate_pairs, report.coincident_pairs), (0, 0),
            "{name}: no vertex pairs within the near-duplicate radius: {report:?}");
        assert_eq!(pieces(&mesh), 1, "{name}: body is one piece");
        let volume = mesh_volume(&mesh);
        assert!(volume.is_finite() && volume > 0.0,
            "{name}: outward-oriented body has positive finite volume, got {volume}");
        let outer = p.diameter * 0.5;
        let annulus = |r: f64| {
            3.14159265358979 * (outer * outer - r * r) * p.height
        };
        let minor = minor_diameter(p) * 0.5 + p.clearance;
        match case.volume {
            VolumeCheck::Annulus => {
                let expected = annulus(minor);
                let deviation = 100.0 * (volume - expected) / expected;
                assert!(deviation.abs() <= 0.2,
                    "{name}: annulus volume {volume} must match {expected} within 0.2 percent, got {deviation}");
            }
            VolumeCheck::Thread => {
                let lo = annulus(p.thread_d * 0.5 + p.clearance);
                let hi = annulus(minor);
                let t = (hi - volume) / (hi - lo);
                assert!(volume > lo && volume < hi,
                    "{name}: thread volume {volume} must lie between major/minor bore volumes {lo} and {hi}");
                assert!((0.3..=0.7).contains(&t),
                    "{name}: thread must fill 30–70 percent of the bore gap, got {}", 100.0 * (1.0 - t));
            }
            VolumeCheck::None => {}
        }
    }
}

#[test]
fn material_values_and_parameter_text_preserve_ring_choices() {
    let mut p = parameters();
    for (name, density) in [
        ("brass", 8.47_f32), ("nickel_brass", 8.47), ("bronze", 8.80),
        ("copper", 8.94), ("aluminium_anodised", 2.70),
        ("stainless_303", 8.00), ("steel_black_oxide", 7.85),
    ] {
        p.material = MaterialId::lookup(name).expect("ring metal exists");
        assert_eq!(ring_mass_grams(&p, 1000.0), density,
            "{name}: one cubic centimetre has the specified mass");
    }
    for (name, expected) in [
        ("metal_polish", 0.04_f32), ("turned", 0.28),
        ("cnc_0_8", 0.36), ("cnc_3_2", 0.55),
    ] {
        p.finish = FinishId::lookup(name).expect("ring finish exists");
        p.material = MaterialId::lookup("brass").unwrap();
        assert_eq!(surface_roughness(&p), expected, "{name}: exact ring roughness");
        p.material = MaterialId::lookup("steel_black_oxide").unwrap();
        assert_eq!(surface_roughness(&p), expected.max(0.35),
            "{name}: black oxide keeps its satin floor");
    }
    let table = param_table();
    assert!(table.set_text(&mut p, table.find("anodize").unwrap(), "Titanium grey"),
        "titanium grey anodising parses by its full label");
    assert_eq!(p.anodize(), Anodize::Titanium, "titanium grey selects the titanium swatch");
    p.thread_std = ThreadStd::Custom.index();
    for (diameter, pitch, text) in [
        (10.5004, 0.70004, "M10.5 x 0.7"),
        (12345.678, 0.79375, "M12345.7 x 0.7938"),
        (1_000_000.0, 0.0001, "M1e+06 x 0.0001"),
    ] {
        p.thread_d = diameter;
        p.pitch = pitch;
        assert_eq!(thread_text(&p), text, "custom thread uses six significant digits");
    }
}
