//! The stock projects' 3D models: each part names one (but the modules no stock part matches),
//! the project evaluates while it waits for them (a warning per model, the part keeping its own
//! body), and once they are held every part's body is its model with the part's own height,
//! ports and light: the model's pose places its mesh, and the ports stay where the part put them
//! on its footprint.

use fab_elec_core::Severity;
use fab_elec_design::Design;
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_parts::{Body3d, BodyPort, BodySource, PartDef};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Every component of every unit, by its unit and instance path.
fn parts(design: &Design) -> BTreeMap<(String, String), Arc<PartDef>> {
    design.units.iter()
        .flat_map(|unit| unit.circuit.comps.iter().map(move |comp| ((unit.name.clone(), comp.path.clone()), comp.part.clone())))
        .collect()
}

/// A body's ports on its footprint, where the case cuts their openings: each port posed by the
/// body's offset, compared to within a nanometre of rounding.
fn footprint_ports(body: &Body3d) -> Vec<BodyPort> {
    body.ports.iter().map(|port| port.moved(&body.offset)).collect()
}

fn same_place(a: &[BodyPort], b: &[BodyPort]) -> bool {
    let near = |x: i32, y: i32| (x - y).abs() <= 2;
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| {
        let (ab, bb) = (a.profile.bbox(), b.profile.bbox());
        a.kind == b.kind && (a.reach, a.margin, a.travel, a.stand) == (b.reach, b.margin, b.travel, b.stand)
            && (0..3).all(|i| near(a.at[i], b.at[i]) && (a.dir[i] - b.dir[i]).abs() < 1e-9)
            && near(ab.min.x, bb.min.x) && near(ab.min.y, bb.min.y) && near(ab.max.x, bb.max.x) && near(ab.max.y, bb.max.y)
    })
}

/// `project` names one model per part, a stock part's, which the app fetches by itself when the
/// board is open, but the parts in `own`, which no stock part matches and which keep their own
/// bodies.
fn names_a_model_for_every_part(project: &str, own: &[&str]) {
    let mut sources = fab_elec_fixtures::docs(project).expect("a stock project");
    let waiting = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("{project} evaluates without its models: {d:?}"));
    let plain = parts(&waiting.design);
    let mut names: Vec<&str> = plain.values().map(|part| part.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    for name in own {
        assert!(names.contains(name), "{project}: {name}, which keeps its own body, is one of its parts ({names:?})");
    }
    names.retain(|name| !own.contains(name));
    let missing = waiting.design.missing_meshes();
    assert_eq!(missing.len(), names.len(),
        "{project}: one model per part ({names:?}); a part new to the project names its model in {project}.models.part.splash: {missing:?}");
    for path in &missing {
        let lcsc = path.strip_prefix("parts/lcsc/C").and_then(|n| n.strip_suffix(".glb")).is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()));
        assert!(lcsc, "{project}: {path} is an LCSC part's converted mesh");
        assert!(waiting.notes.iter().any(|n| n.severity == Severity::Warning && n.message.contains(path.as_str())),
            "{project}: the missing {path} is a warning");
    }
    assert!(!waiting.notes.iter().any(|n| n.severity == Severity::Error), "{project}: no model makes an error: {:?}", waiting.notes);

    for path in &missing {
        sources.add_asset(path.clone());
    }
    let held = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("{project} evaluates with its models: {d:?}"));
    assert!(held.design.missing_meshes().is_empty(), "{project}: nothing is missing once held");
    let with = parts(&held.design);
    assert_eq!(with.len(), plain.len(), "{project}: the models change no component");
    for (at, part) in &with {
        let own_body = own.contains(&part.name.as_str());
        let own = &plain[at];
        if own_body {
            assert_eq!(part.body, own.body, "{project} {at:?}: a part no stock model matches keeps its own body");
            continue;
        }
        let (body, plain_body) = (part.body.as_ref().expect("a modelled body"), own.body.as_ref().expect("std bodies"));
        assert!(matches!(&body.source, BodySource::Mesh(path) if missing.contains(path)),
            "{project} {at:?}: the body is its model, got {:?}", body.source);
        assert_eq!(body.height, plain_body.height, "{project} {at:?}: the part keeps its height");
        let (ports, plain_ports) = (footprint_ports(body), footprint_ports(plain_body));
        assert!(same_place(&ports, &plain_ports), "{project} {at:?}: the part's ports stay on its footprint: {ports:?} vs {plain_ports:?}");
        assert_eq!(part.emitter, own.emitter, "{project} {at:?}: an LED keeps its light");
    }
}

#[test]
fn ledavr_names_a_model_for_every_part_and_builds_without_them() {
    names_a_model_for_every_part("ledavr", &[]);
}

#[test]
fn drum16_names_a_model_for_every_part_and_builds_without_them() {
    names_a_model_for_every_part("drum16", &[]);
}

#[test]
fn handheld_names_a_model_for_every_stock_part_and_builds_without_them() {
    names_a_model_for_every_part("handheld", &["panel", "touch", "usb_c_usb2"]);
}

#[test]
fn tonebox_names_a_model_for_every_part_and_builds_without_them() {
    names_a_model_for_every_part("tonebox", &[]);
}
