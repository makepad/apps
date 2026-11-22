//! A non-planar top from a model's own SDF field (a Splash-kernel `m.sdf` part), as the app hands
//! it to the slicer (the part's field in its frame), plans like the same dome written as an
//! analytic field. Kernel fields evaluate in f32: the planner's normals must not read that
//! rounding as the surface (it did, at a 10 nm step: 0° slopes, half the coverage, 100x the time).
//! A half-size dome (22 mm across, a 40 mm sphere) with the same slopes.

use fab_model::eval::Evaluator;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_slice::live::PartField;
use makepad_fab_slicer::nonplanar::{plan, NpBox, NpSettings};

const DOME: &str = r##"use mod.model.*
use mod.fab.*
let m = mod.model
let dome = m.sdf({
    cell: 0.5mm
    bounds: [[-12mm, 0, -12mm], [12mm, 7.5mm, 12mm]]
    field: fn(p) {
        return sdf_intersection(sdf_cylinder(p - vec3(0, 3.275mm, 0), 11mm, 6.55mm), sdf_sphere(p - vec3(0, -33.45mm, 0), 40mm))
    }
})
object(dome)
"##;

/// The project that places it.
const PROJECT: &str = r##"use mod.fab.*
script("dome.cad.splash")
place("dome", at: [160mm 160mm], settings: {fab_nonplanar_top: true})
"##;

fn dome() -> fab_model::eval::Evaluated {
    let scripts = [("dome.cad.splash".to_string(), DOME.to_string())].into_iter().collect();
    let m = FabModel::load("np", PROJECT, scripts).unwrap_or_else(|f| panic!("{f}"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).expect("evaluates").1
}

#[test]
fn a_model_dome_plans_like_the_analytic_dome() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let ev = dome();
    let pf = ev.fields[0].as_ref().expect("the dome is a field part");
    let mut to = pf.to_printer;
    for (r, o) in ev.frames[0].offset.iter().enumerate() {
        to[r][3] -= o;
    }
    let field = PartField::new(pf.shape.clone(), to);
    let (lo, hi) = field.source().unwrap().bounds();
    let f = field.field().unwrap();
    let head = vec![NpBox { name: "test cone".into(), ox: 0.0, oy: 0.0, hx: 4.0, hy: 4.0, hole: 0.5, e: 2.0, margin: 0.2 }];
    let analytic = |x: f64, y: f64, z: f64| {
        let cyl = (x * x + y * y).sqrt() - 11.0;
        let sph = (x * x + y * y + (z - 6.55 + 40.0).powi(2)).sqrt() - 40.0;
        cyl.max(-z).max(sph)
    };
    let a = plan(&analytic, (lo, hi), &NpSettings::default(), &head).expect("plans");
    let k = plan(&|x, y, z| f(x, y, z), (lo, hi), &NpSettings::default(), &head).expect("plans");
    let cov = |p: &makepad_fab_slicer::nonplanar::NpPlan| p.stats.np_area / p.stats.top_area.max(1e-9);
    assert!((cov(&k) - cov(&a)).abs() < 0.02, "coverage: kernel {} vs analytic {}", cov(&k), cov(&a));
    assert!((k.stats.max_slope_deg - a.stats.max_slope_deg).abs() < 0.5, "max slope: kernel {} vs analytic {}", k.stats.max_slope_deg, a.stats.max_slope_deg);
    assert!((k.stats.top_area - a.stats.top_area).abs() < 0.02 * a.stats.top_area, "top area: kernel {} vs analytic {}", k.stats.top_area, a.stats.top_area);
}
