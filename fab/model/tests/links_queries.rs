use fab_model::FabModel;
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::{Links, SdfNode};
use fab_sdf_engine::{program, twin};
use std::sync::Arc;

#[path = "fixtures/sources.rs"]
mod sources;
#[path = "../../sdf_engine/tests/support/eval_program.rs"]
mod eval_program;

fn visit(n: &SdfNode, out: &mut Vec<Arc<Links>>) {
    match n {
        SdfNode::Links(l) => out.push(l.clone()),
        SdfNode::Combine { of, .. } => for n in of { visit(n, out); },
        SdfNode::Id { of, .. } | SdfNode::Transform { of, .. } | SdfNode::Material { of, .. } => visit(of, out),
        _ => {}
    }
}

fn corpus() -> Vec<(String, Arc<Links>)> {
    let mut docs = vec![
        ("chain.cad.splash".to_string(), include_str!("../../app/resources/chain.cad.splash").to_string()),
        ("chainmail.cad.splash".into(), include_str!("../../app/resources/chainmail.cad.splash").into()),
        ("weave.rs:MAIL".into(), sources::WEAVE_MAIL.into()),
        ("weave.rs:chainmail_scale".into(), sources::WEAVE_LARGE.into()),
        ("mechanics.rs:sheet".into(), sources::MECHANICS_SHEET.into()),
    ];
    docs.extend(sources::literals(include_str!("../../app/tests/mechanics.rs")).into_iter()
        .filter(|(_, s)| s.contains("m.mail(")).map(|(line, s)| (format!("mechanics.rs:{line}"), s)));
    let mut out = Vec::new();
    for (name, source) in docs {
        let c = FabModel::with_script("links", "links.cad.splash", &source).compile().unwrap();
        let mut links = Vec::new();
        for inst in &c.instances {
            for (_, shape, _) in fab_model::eval::part_fields(inst, &inst.script, &c.strokes) { visit(&shape.root, &mut links); }
        }
        assert!(!links.is_empty(), "{name}: fixture lost Links");
        for l in links { out.push((name.clone(), l)); }
    }
    out
}

fn points(l: &Links) -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    // Every loop, with offsets inside, on either side, outside the exact band,
    // and far outside the grid. Irregular fractions avoid geometric slot ties.
    for link in &l.loops {
        for i in [0, link.pts.len() / 3] {
            let a = link.pts[i]; let b = link.pts[(i + 1) % link.pts.len()];
            for scale in [0.0, 0.97, 1.03, 4.3, 20.7] {
                out.push(std::array::from_fn(|k| (a[k] + (b[k] - a[k]) * 0.371 + l.radius * scale * [0.237, 0.831, -0.503][k]) as f32));
            }
        }
    }
    let (cell, origin, dims, ..) = l.grid();
    // Cell boundaries and their adjacent f32 values; clip and box boundaries.
    for i in 0..384 {
        out.push(std::array::from_fn(|k| {
            let u = ((i * [137, 251, 359][k] + 71) % 383) as f64 / 382.0;
            (l.min[k] - cell * 3.0 + u * (l.max[k] - l.min[k] + cell * 6.0)) as f32
        }));
    }
    for k in 0..3 { for c in 0..dims[k] {
        let mut q = std::array::from_fn(|a| ((l.min[a] + l.max[a]) * 0.5) as f32);
        let edge = (origin[k] + c as f64 * cell) as f32;
        for v in [edge.next_down(), edge, edge.next_up()] { q[k] = v; out.push(q); }
    } }
    out
}

#[test]
fn links_twin_and_gpu_match() {
    let mut cases = Vec::new();
    for (name, l) in corpus() {
        let ps = points(&l);
        let n = SdfNode::Links(l.clone());
        let p = program::lower(&n).unwrap();
        let scaled = p.clone().scaled(0.75);
        let mut worst = 0.0f64;
        for q in &ps {
            let (d, slot) = twin::eval(&p.texels, *q);
            let truth = l.distance_slot(Vec3d::new(q[0] as f64, q[1] as f64, q[2] as f64));
            let error = (d as f64 - truth.0).abs();
            worst = worst.max(error);
            assert!(error <= 1e-4, "{name} {q:?}: twin {d}, f64 {}", truth.0);
            assert_eq!(slot, truth.1, "{name} {q:?}: nearest loop slot");
            assert!(d as f64 >= truth.0 - 1e-4, "{name} {q:?}: conservative far-field rounding");
            assert_eq!(twin::eval(&scaled.texels, *q).0.to_bits(), (d * 0.75).to_bits(), "scaled payload relocation");
        }
        let (cell, _, dims, starts, refs, segs) = l.grid();
        println!("LINKS {name}: {} loops, {} segments; {} buffer bytes ({} payload bytes); grid {dims:?}, {} starts, {} references, max {} segments/cell, reach {cell}; {} points, worst f64 error {worst:e} mm",
            l.loops.len(), segs.len(), p.texels.len() * 16, p.texels[1][2].to_bits() as usize * 16,
            starts.len(), refs.len(), starts.windows(2).map(|s| s[1] - s[0]).max().unwrap_or(0), ps.len());
        // Also test painted links, including slot-zero inheritance. At exact
        // symmetry planes f64 and rounded f32 geometry can pick different tied
        // loops; compare f64 slots at the loop-local samples, GPU/twin everywhere.
        let mut loops = l.loops.clone();
        for (i, link) in loops.iter_mut().enumerate() { link.slot = (i % 251) as u8; }
        let coloured = Arc::new(Links::new(loops, l.radius, l.clip));
        let coloured_node = SdfNode::Links(coloured.clone());
        let coloured_program = program::lower(&coloured_node).unwrap();
        for q in &ps[..l.loops.len() * 10] {
            assert_eq!(twin::eval(&coloured_program.texels, *q).1,
                coloured.distance_slot(Vec3d::new(q[0] as f64, q[1] as f64, q[2] as f64)).1,
                "{name} {q:?}: painted loop slot");
        }
        cases.push((name, coloured_node, ps));
    }
    eval_program::check(cases);
}
