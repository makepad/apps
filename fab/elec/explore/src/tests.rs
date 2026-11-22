use crate::molecules::{self, MolKind};
use crate::{adapt, block_origin, layout, sheet, to_local, to_world, Mover, Opts, SheetLayout};
use fab_elec_circuit::{
    AvoidRegion, Circuit, CircuitOp, Component, GuidePoint, Intent, Net, NetKind, QtyUnit, Quantity, SheetHint,
    SheetHints, Target, Topology, Value,
};
use fab_elec_core::{BlockId, ClassId, CompId, NetId, SchPt};
use fab_elec_design::{fixture, Design, DesignOp};
use fab_elec_parts::{Fields, PartDef, PartKind};
use std::sync::Arc;

fn order_kept(a: &SheetLayout, b: &SheetLayout, pairs: &[(usize, usize)]) -> f64 {
    let sign = |d: f64| {
        if d.abs() < 1.0 { 0 } else if d < 0.0 { -1 } else { 1 }
    };
    let centre = |body: [i32; 4], axis: usize| {
        (body[axis] as f64 + body[axis + 2] as f64) * 0.5
    };
    let mut counted = 0;
    let mut kept = 0;
    for &(i, j) in pairs {
        let (Some(ai), Some(aj), Some(bi), Some(bj)) =
            (a.parts.get(i), a.parts.get(j), b.parts.get(i), b.parts.get(j))
        else {
            continue;
        };
        if !(ai.placed && aj.placed && bi.placed && bj.placed) {
            continue;
        }
        counted += 1;
        if (0..2).all(|axis| {
            let sa = sign(centre(ai.body, axis) - centre(aj.body, axis));
            let sb = sign(centre(bi.body, axis) - centre(bj.body, axis));
            sa == 0 || sb == 0 || sa == sb
        }) {
            kept += 1;
        }
    }
    if counted == 0 { 1.0 } else { kept as f64 / counted as f64 }
}

fn pairs_in(d: &Design, pred: impl Fn(&Component) -> bool) -> Vec<(usize, usize)> {
    let indices: Vec<_> = d.units[0]
        .circuit
        .comps
        .iter()
        .enumerate()
        .filter_map(|(i, c)| pred(c).then_some(i))
        .collect();
    let mut pairs = Vec::new();
    for (i, &a) in indices.iter().enumerate() {
        for &b in &indices[i + 1..] {
            pairs.push((a, b));
        }
    }
    pairs
}

// Keep component construction identical to the Rust probe fixture's helper.
fn component(
    c: &mut Circuit,
    path: &str,
    refdes: &str,
    part: Arc<PartDef>,
    value: Option<Value>,
    connections: &[(&str, NetId)],
    nc: &[&str],
    block: BlockId,
) -> CompId {
    let id = CompId(c.comps.len() as u32);
    let mut conns: Vec<_> = connections
        .iter()
        .map(|&(name, net)| (part.pin(name).expect("fixture connection names a part pin"), net))
        .collect();
    conns.sort_by_key(|&(pin, _)| pin);
    let nc = nc.iter().map(|name| part.pin(name).expect("fixture NC names a part pin")).collect();
    c.comps.push(Component {
        id, path: path.into(), refdes: refdes.into(), ref_kind: Default::default(), part, value, conns, nc, block, dnp: false,
        fields: Fields::default(),
    });
    id
}

fn add_part(d: &Design) -> (Design, BlockId) {
    let mut edited = d.clone();
    let mut c = (*edited.units[0].circuit).clone();
    let block = c.comp("mcu.u").expect("fixture has MCU").block;
    let v33 = c.net("3V3").expect("fixture has 3V3").id;
    let gnd = c.net("GND").expect("fixture has GND").id;
    component(
        &mut c,
        "mcu.dec[3]",
        "C7",
        fixture::parts::capacitor("0402"),
        Some(Value {
            text: "100nF".into(),
            qty: Some(Quantity { si: 100e-9, unit: QtyUnit::Farad }),
            tol: None,
        }),
        &[("1", v33), ("2", gnd)],
        &[],
        block,
    );
    edited.units[0].circuit = Arc::new(c);
    (edited, block)
}

fn remove_part(d: &Design) -> (Design, BlockId) {
    let mut edited = d.clone();
    let mut c = (*edited.units[0].circuit).clone();
    let removed = c.comp("mcu.boot_r").expect("fixture has boot resistor").id;
    let block = c.comps.remove(removed.ix()).block;
    for (i, comp) in c.comps.iter_mut().enumerate().skip(removed.ix()) {
        comp.id = CompId(i as u32);
    }
    let remap = |target: &mut Target| match target {
        Target::Comp(id) | Target::Pin(id, _) => {
            if *id == removed {
                return false;
            }
            if id.0 > removed.0 {
                id.0 -= 1;
            }
            true
        }
        Target::Block(_) => true,
    };
    // Drop an entire intent if any of its targets names the removed component.
    c.intents.retain_mut(|intent| match intent {
        Intent::Near { a, b, .. } => remap(a) && remap(b),
        Intent::Group { members, .. } => members.iter_mut().all(remap),
        Intent::Side { t, .. } | Intent::Fixed { t, .. } | Intent::Star { at: t, .. } => remap(t),
        Intent::Guide { through, .. } => through.iter_mut().all(|point| match point {
            GuidePoint::At(_) => true,
            GuidePoint::Pass { t, .. } => remap(t),
        }),
        Intent::Topology { topo, .. } => match topo {
            Topology::Tree => true,
            Topology::Star(t) => remap(t),
            Topology::Chain { order, .. } => order.iter_mut().all(remap),
        },
        Intent::Avoid { region, .. } => match region {
            AvoidRegion::Area(_) => true,
            AvoidRegion::Near(t, _) => remap(t),
        },
        Intent::Pair { .. }
        | Intent::LengthMatch { .. }
        | Intent::Width { .. }
        | Intent::LayerPref { .. }
        | Intent::Bundle { .. }
        | Intent::Lane { .. }
        | Intent::MaxVias { .. }
        | Intent::NeedsRef { .. }
        | Intent::Style { .. } => true,
    });
    // Nets retain their indices; mcu.u.BOOT0 now has only the MCU pin.
    edited.units[0].circuit = Arc::new(c);
    (edited, block)
}

fn add_net(d: &Design) -> (Design, BlockId) {
    let mut edited = d.clone();
    let mut c = (*edited.units[0].circuit).clone();
    let mcu = c.comp("mcu.u").expect("fixture has MCU").id;
    let block = c.comps[mcu.ix()].block;
    let net = NetId(c.nets.len() as u32);
    c.nets.push(Net {
        id: net, name: "mcu.TP".into(), class: ClassId(0), volts: None, kind: NetKind::Signal, scope: None,
    });
    let u = &mut c.comps[mcu.ix()];
    let pa8 = u.part.pin("PA8").expect("fixture MCU has PA8");
    u.nc.retain(|&pin| pin != pa8);
    u.conns.push((pa8, net));
    u.conns.sort_by_key(|&(pin, _)| pin);
    component(
        &mut c, "mcu.tp", "J3", fixture::parts::header(1, 2.54), None, &[("1", net)], &[], block,
    );
    edited.units[0].circuit = Arc::new(c);
    (edited, block)
}

fn contains(outer: [i32; 4], inner: [i32; 4]) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

/// `ops` committed through the controller's text as one step: the edited documents, their
/// evaluation and the step's undo.
fn commit_text(sources: &fab_elec_lang::Sources, ev: &fab_elec_lang::Evaluated, ops: &[DesignOp])
    -> (fab_elec_lang::Sources, fab_elec_lang::Evaluated, fab_elec_lang::SpanEdit)
{
    let edit = fab_elec_lang::splice(sources, &ev.map, &ev.design, ops).expect("commit: the ops splice into the text as one commit");
    let undo = edit.inverse(sources, &ev.map);
    let mut after = sources.clone();
    after.apply(&edit.edits).expect("commit: the edits apply as one step");
    let evaluated = fab_elec_lang::evaluate(&after, &fab_elec_lang::std_libs()).expect("commit: the edited text evaluates");
    (after, evaluated, undo)
}

/// The text of every document, to compare two states byte for byte.
fn texts(sources: &fab_elec_lang::Sources) -> Vec<std::sync::Arc<str>> {
    sources.docs().iter().map(|doc| doc.text.clone()).collect()
}

#[test]
fn wire_groups_join_each_net_piece_on_the_fixture() {
    use crate::groups::{spanning, wire_groups};
    use std::collections::BTreeSet;

    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    let d = fab_elec_fixtures::ledctl();
    let hints = SheetHints::default();
    let sheet = layout(&d, 0, &hints, &Opts::default());
    let g = adapt::graph_of(&d.units[0].circuit, &hints);
    let port_nets = g.port_nets();
    let mut pins = Vec::new();
    let mut nodes = Vec::new();
    for (n, (part, node)) in sheet.parts.iter().zip(&g.nodes).enumerate() {
        if !part.placed {
            continue;
        }
        for (p, port) in node.ports.iter().enumerate() {
            let Some(net) = port_nets[n][p] else { continue };
            let at = crate::wiretree::rot(port.at, part.turn, part.mirror);
            pins.push((net, [part.at[0] + at[0], part.at[1] + at[1]]));
            nodes.push(n as u32);
        }
    }

    let groups = wire_groups(&sheet, &pins);
    let mut run_count = vec![0; sheet.wires.len()];
    let mut pin_count = vec![0; pins.len()];
    let mut junction_count = vec![0; sheet.junctions.len()];
    let mut tag_count = vec![0; sheet.tags.len()];
    let mut net_count = vec![0; g.nets.len()];
    let mut spans_parts = false;
    for group in &groups {
        net_count[group.net as usize] += 1;
        for list in [&group.pins, &group.runs, &group.junctions, &group.tags] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "ordering: each member list strictly ascends");
        }
        for &run in &group.runs {
            run_count[run as usize] += 1;
            assert_eq!(sheet.wires[run as usize].net, group.net, "net ownership: every run has its group's net");
        }
        for &pin in &group.pins {
            pin_count[pin as usize] += 1;
            assert_eq!(pins[pin as usize].0, group.net, "net ownership: every pin has its group's net");
        }
        for &junction in &group.junctions {
            junction_count[junction as usize] += 1;
            assert_eq!(sheet.junctions[junction as usize].0, group.net, "net ownership: every junction has its group's net");
        }
        for &tag in &group.tags {
            tag_count[tag as usize] += 1;
            let tag = &sheet.tags[tag as usize];
            if let Some(net) = tag.net {
                assert_eq!(net, group.net, "net ownership: every connected tag has its group's net");
            } else {
                assert_eq!(tag.kind, sheet::TagKind::NoConnect, "no-connect ownership: a netless tag is a no-connect");
                let first = pins.iter().position(|&(_, at)| at == tag.at)
                    .expect("no-connect ownership: a grouped no-connect sits on a pin");
                assert!(group.pins.contains(&(first as u32)), "no-connect ownership: the group contains the first pin at the tag's point");
            }
        }

        let at: Vec<_> = group.pins.iter().map(|&pin| {
            let p = pins[pin as usize].1;
            [p[0] as f64, p[1] as f64]
        }).collect();
        let node: Vec<_> = group.pins.iter().map(|&pin| nodes[pin as usize]).collect();
        let distinct: Vec<_> = node.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
        spans_parts |= distinct.len() >= 2;
        let edges = spanning(&at, &node);
        assert_eq!(edges.len(), distinct.len().saturating_sub(1), "spanning edge count: one fewer arc than distinct parts, or zero for no parts");
        let mut parent: Vec<_> = (0..distinct.len()).collect();
        for [a, b] in edges {
            assert!(a < b && (b as usize) < at.len(), "spanning indices: each edge has ascending indices into the pin points");
            let (a, b) = (node[a as usize], node[b as usize]);
            assert_ne!(a, b, "spanning endpoints: each arc joins different parts");
            let a = distinct.binary_search(&a).expect("spanning nodes: the first endpoint has a part");
            let b = distinct.binary_search(&b).expect("spanning nodes: the second endpoint has a part");
            let (a, b) = (root(&mut parent, a), root(&mut parent, b));
            assert_ne!(a, b, "spanning tree: each arc joins previously separate sets");
            parent[b] = a;
        }
        if !distinct.is_empty() {
            let first = root(&mut parent, 0);
            assert!((0..distinct.len()).all(|i| root(&mut parent, i) == first), "spanning connectivity: arcs and zero-cost part joins leave one set");
        }
    }
    assert!(run_count.iter().all(|&n| n == 1), "run coverage: every wire run belongs to exactly one group");
    assert!(pin_count.iter().all(|&n| n == 1), "pin coverage: every pin belongs to exactly one group");
    assert!(junction_count.iter().all(|&n| n == 1), "junction coverage: every junction belongs to exactly one group");
    assert!(tag_count.iter().all(|&n| n <= 1), "tag ownership: no tag belongs to multiple groups");
    for (i, tag) in sheet.tags.iter().enumerate() {
        if matches!(tag.kind, sheet::TagKind::Power | sheet::TagKind::Ground) {
            assert_eq!(tag_count[i], 1, "rail coverage: every Power and Ground tag belongs to a group");
        }
    }
    assert!(spans_parts, "wired pieces: at least one group spans two or more parts");
    // Rails recur as separate tags and local stubs on parts in different fixture blocks.
    assert!(net_count.iter().any(|&n| n > 1), "net pieces: the fixture has a net with more than one wire group");
    assert_eq!(groups, wire_groups(&sheet, &pins), "determinism: repeated grouping gives equal output");
}

#[test]
fn unconnected_parts_pack_in_rows_to_the_pane_shape() {
    use std::collections::BTreeSet;

    let mut d = fab_elec_fixtures::ledctl();
    assert!(!layout(&d, 0, &SheetHints::default(), &Opts::default()).packed,
        "packed: a sheet whose parts all connect does not follow the pane shape");
    let c = Arc::make_mut(&mut d.units[0].circuit);
    // A parts showcase has one level and no net bindings or position hints.
    for comp in &mut c.comps {
        comp.conns.clear();
        comp.block = BlockId(0);
    }
    c.tree.truncate(1);
    c.tree[0].ports.clear();
    c.nets.clear();
    c.buses.clear();
    c.intents.clear();
    c.hints = SheetHints::default();

    for aspect in [1.5, 0.5] {
        let opts = Opts { aspect, ..Opts::default() };
        let a = layout(&d, 0, &SheetHints::default(), &opts);
        let b = layout(&d, 0, &SheetHints::default(), &opts);
        assert!(a.parts.iter().all(|part| part.placed), "placement: every loose part is placed for aspect {aspect}");
        assert!(a.packed, "packed: a sheet of loose parts follows the pane shape");
        let tops: BTreeSet<_> = a.parts.iter().map(|part| part.body[3]).collect();
        let lefts: BTreeSet<_> = a.parts.iter().map(|part| part.body[0]).collect();
        assert!(tops.len() > 1, "rows: loose parts span more than one body top for aspect {aspect}");
        assert!(lefts.len() > 1, "columns: loose parts span more than one body left for aspect {aspect}");
        for (i, part) in a.parts.iter().enumerate() {
            for (j, other) in a.parts.iter().enumerate().skip(i + 1) {
                assert!(part.body[2] <= other.body[0] || other.body[2] <= part.body[0]
                    || part.body[3] <= other.body[1] || other.body[3] <= part.body[1],
                    "body separation: parts {i} and {j} overlap for aspect {aspect}");
            }
        }
        let width = a.bounds[2] - a.bounds[0];
        let height = a.bounds[3] - a.bounds[1];
        let ratio = f64::from(width) / f64::from(height);
        assert!((aspect / 2.5..=aspect * 2.5).contains(&ratio),
            "pane shape: bounds ratio {ratio} is outside tolerance for aspect {aspect}");
        assert!(if aspect < 1.0 { height > width } else { width > height },
            "pane orientation: bounds {width} by {height} must follow aspect {aspect}");
        assert_eq!(a.parts, b.parts, "determinism: repeated layout gives equal parts for aspect {aspect}");
    }
}

#[test]
fn the_drum_machine_draws_without_overlaps() {
    let sources = fab_elec_fixtures::docs("drum16").expect("the drum machine has stock documents");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("the drum machine evaluates");
    let unit = ev.design.units.iter().position(|u| u.name == "drum16").expect("the project has the drum16 board");
    for aspect in [1.5, 2.98] {
        let sheet = layout(&ev.design, unit, &SheetHints::default(), &Opts { aspect, ..Opts::default() });
        let pairs = sheet::overlaps(&sheet);
        assert!(pairs.is_empty(), "drum16 at aspect {aspect} has overlapping pairs: {pairs:?}");
    }
}

/// R1, R2 and the free rule (fable-answers A1, A4): a paste whose end joins a signal net keeps the
/// drawn sheet. Unkept, the edited design moves most of drum16. Kept, the disturbed level (the
/// root) is pinned as drawn; the pasted part, which would stack below its layer's pins, goes free
/// at the nearest clear place to its drop and names its VREF end. Nothing else moves, the commit
/// lays out exactly the preview, a second paste moves nothing (the first paste included) and
/// rewrites none of its hints, and a drop beside the columns moves nothing either.
#[test]
fn a_paste_on_a_signal_net_keeps_the_drawn_sheet() {
    use fab_elec_circuit::End;
    use fab_elec_lang::{evaluate, splice_verified, std_libs, Evaluated, Sources, NO_CANCEL};
    let sources = fab_elec_fixtures::docs("drum16").expect("the drum machine has stock documents");
    let libs = std_libs();
    let ev = evaluate(&sources, &libs).expect("the drum machine evaluates");
    let unit_of = |d: &Design| d.units.iter().position(|u| u.name == "drum16").expect("the design keeps the drum16 board");
    let unit = unit_of(&ev.design);
    let c = ev.design.units[unit].circuit.clone();
    let opts = Opts { aspect: 2.98, ..Opts::default() };
    let before = layout(&ev.design, unit, &SheetHints::default(), &opts);
    assert!(sheet::overlaps(&before).is_empty(), "keep setup: drum16 draws clean");
    let pins = |id: NetId| c.comps.iter().flat_map(|p| p.conns.iter()).filter(|(_, n)| *n == id).count();
    let vref = c.nets.iter().find(|n| n.name == "VREF").expect("keep setup: drum16 has its VREF reference");
    assert!(vref.kind == NetKind::Signal && pins(vref.id) > 2, "keep setup: VREF is a signal net with many pins");
    let after = c.comps.iter().filter(|p| p.block.0 == 0).last().expect("keep setup: the root has parts").id;
    let paste = |sources: &Sources, ev: &Evaluated, name: &str, at: [i32; 2]| {
        let op = CircuitOp::AddPart {
            stmt: format!("let {name} = e.r(100kohm, _n0, _n1, {{package: \"0402\"}})"), after: Some(after),
            ends: vec![End::Net(vref.id), End::Open],
            hint: Some(SheetHint { at: Some(SchPt::new(at[0], at[1])), ..SheetHint::default() }),
        };
        let ops = vec![DesignOp::Circuit { unit: c.unit, op }];
        (splice_verified(sources, &libs, ev, &ops, &NO_CANCEL).expect("the paste splices"), ops)
    };
    let new_path = |old: &Circuit, now: &Circuit| now.comps.iter().find(|q| old.comp(&q.path).is_none()).expect("the paste adds a part").path.clone();
    // Parts of `old` that `s` (a layout of `now`) draws anywhere else than `was` did.
    let moved = |old: &Circuit, was: &SheetLayout, now: &Circuit, s: &SheetLayout| old.comps.iter().enumerate()
        .filter(|(n, p)| was.parts[*n].placed && now.comps.iter().position(|q| q.path == p.path).is_some_and(|m| s.parts[m].at != was.parts[*n].at))
        .map(|(_, p)| p.path.clone()).collect::<Vec<_>>();
    let hinted = |ops: &[DesignOp], hints: &[(String, SheetHint)]| ops.iter().cloned().chain(hints.iter().map(|(path, hint)|
        DesignOp::Circuit { unit: c.unit, op: CircuitOp::Hint { path: path.clone(), hint: Some(hint.clone()) } })).collect::<Vec<_>>();
    let base = crate::KeepBase { sheet: before.clone(), circuit: c.clone() };
    // Nets other than `own` whose wires `s` (a layout of `now`) draws otherwise than `before` did.
    let rewired = |now: &Circuit, s: &SheetLayout, own: &str| {
        let by_name = |circ: &Circuit, s: &SheetLayout| {
            let mut m: std::collections::BTreeMap<String, Vec<Vec<[i32; 2]>>> = Default::default();
            for w in &s.wires { m.entry(circ.nets[w.net as usize].name.clone()).or_default().push(sheet::canon_run(&w.pts)); }
            m.values_mut().for_each(|v| v.sort());
            m
        };
        let (was, is) = (by_name(&c, &before), by_name(now, s));
        was.keys().chain(is.keys()).filter(|name| *name != own && was.get(*name) != is.get(*name)).cloned()
            .collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>()
    };

    let (pasted, ops) = paste(&sources, &ev, "pasted", [250, 390]);
    let d = &pasted.evaluated.design;
    let u = unit_of(d);
    let pc = d.units[u].circuit.clone();
    let first = new_path(&c, &pc);
    let free = layout(d, u, &SheetHints::default(), &opts);
    assert!(moved(&c, &before, &pc, &free).len() > 100, "keep setup: unkept, the paste moves most of the sheet");
    let (kept, hints) = crate::kept_layout(d, u, &base, &[first.as_str()], &opts).expect("kept: the root has room");
    assert_eq!(moved(&c, &before, &pc, &kept), Vec::<String>::new(), "kept: no part but the paste moves");
    assert!(crate::keep_drawn(&base, &kept, &pc, &[first.as_str()]).is_empty(), "kept: the keep converges");
    assert!(sheet::overlaps(&kept).is_empty(), "kept: the pasted part lands clear of the drawing");
    assert_eq!(rewired(&pc, &kept, "VREF"), Vec::<String>::new(), "kept: every other net's wires stay as drawn");
    let root_items = c.comps.iter().enumerate().filter(|(n, p)| p.block.0 == 0 && before.movers[*n] == Mover::Itself && before.parts[*n].placed).count()
        + c.tree.iter().enumerate().filter(|(k, b)| b.parent.is_some_and(|p| p.0 == 0) && before.block_movable[*k]).count();
    assert_eq!(hints.iter().filter(|(path, _)| *path != first).count(), root_items, "kept: only the disturbed level, the root, is pinned, item for item");
    let (_, own) = hints.iter().find(|(path, _)| *path == first).expect("kept: the pasted part, off its drop in the stack, takes a free hint");
    let n = pc.comps.iter().position(|p| p.path == first).expect("the pasted part is in the circuit");
    let drawn = to_local(&kept, kept.parts[n].block, kept.parts[n].at);
    assert!(own.free && own.at.map(|at| [at.x, at.y]) == drawn, "kept: the free part is drawn exactly at its hint ({:?} at {drawn:?})", own.at);
    let body = kept.parts[n].body;
    let on_body = |p: [i32; 2]| p[0] >= body[0] && p[0] <= body[2] && p[1] >= body[1] && p[1] <= body[3];
    let vref_net = pc.nets.iter().position(|net| net.name == "VREF").expect("the paste keeps VREF") as u32;
    assert!(kept.wires.iter().any(|w| w.net == vref_net && [w.pts[0], w.pts[w.pts.len() - 1]].into_iter().any(on_body))
        || kept.tags.iter().any(|t| t.kind == sheet::TagKind::Label && t.text == "VREF" && on_body(t.at)),
        "kept: the free part's VREF end is wired, or continues by name");

    let committed = splice_verified(&sources, &libs, &ev, &hinted(&ops, &hints), &NO_CANCEL).expect("the paste with its hints splices");
    let cd = &committed.evaluated.design;
    let cu = unit_of(cd);
    let landed = layout(cd, cu, &SheetHints::default(), &opts);
    for (n, (a, b)) in landed.parts.iter().zip(&kept.parts).enumerate() {
        assert!(a.at == b.at && a.turn == b.turn && a.mirror == b.mirror, "commit equals preview: {} lands where it was shown", pc.comps[n].path);
    }
    assert!(landed.wires == kept.wires, "commit equals preview: the wires are drawn as shown");

    let cc = cd.units[cu].circuit.clone();
    let base2 = crate::KeepBase { sheet: landed.clone(), circuit: cc.clone() };
    let (second, _) = paste(&committed.sources, &committed.evaluated, "second", [250, 330]);
    let d2 = &second.evaluated.design;
    let u2 = unit_of(d2);
    let c2 = d2.units[u2].circuit.clone();
    let next = new_path(&cc, &c2);
    let (kept2, hints2) = crate::kept_layout(d2, u2, &base2, &[next.as_str()], &opts).expect("a second paste: the root has room");
    assert_eq!(moved(&cc, &landed, &c2, &kept2), Vec::<String>::new(), "a second paste moves no part, the first paste included");
    assert!(hints2.iter().all(|(path, _)| *path == next), "a second paste rewrites no hint but its own: {:?}", hints2.iter().map(|(p, _)| p).collect::<Vec<_>>());

    let (beside, _) = paste(&sources, &ev, "beside", [970, 410]);
    let d3 = &beside.evaluated.design;
    let u3 = unit_of(d3);
    let c3 = d3.units[u3].circuit.clone();
    let (kept3, _) = crate::kept_layout(d3, u3, &base, &[new_path(&c, &c3).as_str()], &opts).expect("beside: the root has room");
    assert_eq!(moved(&c, &before, &c3, &kept3), Vec::<String>::new(), "a drop beside every column moves no part");

    // A part pasted into mcu at its top edge, just under hats: it stays in mcu's drawn content, so
    // the frame does not grow into hats and nothing in the root moves.
    let k = c.tree.iter().position(|b| b.path == "mcu").expect("keep setup: drum16 has its mcu block");
    let in_mcu = c.comps.iter().filter(|p| p.block.0 as usize == k).last().expect("keep setup: mcu has parts").id;
    let mcu_net = c.comps.iter().filter(|p| p.block.0 as usize == k).flat_map(|p| p.conns.iter().map(|(_, n)| *n))
        .find(|n| c.nets[n.0 as usize].kind == fab_elec_circuit::NetKind::Signal).expect("keep setup: mcu has a signal net");
    let op = CircuitOp::AddPart {
        stmt: "let edge = e.r(100kohm, _n0, _n1, {package: \"0402\"})".into(), after: Some(in_mcu),
        ends: vec![End::Net(mcu_net), End::Open], hint: Some(SheetHint { at: Some(SchPt::new(40, -4)), ..SheetHint::default() }),
    };
    let edge = splice_verified(&sources, &libs, &ev, &[DesignOp::Circuit { unit: c.unit, op }], &NO_CANCEL).expect("the edge paste splices");
    let d4 = &edge.evaluated.design;
    let u4 = unit_of(d4);
    let c4 = d4.units[u4].circuit.clone();
    let (kept4, _) = crate::kept_layout(d4, u4, &base, &[new_path(&c, &c4).as_str()], &opts).expect("edge: mcu has room inside");
    assert_eq!(moved(&c, &before, &c4, &kept4), Vec::<String>::new(), "a paste at mcu's edge moves no part, in mcu or around it");
    assert_eq!(kept4.frames[k].rect, before.frames[k].rect, "a paste at mcu's edge keeps mcu's frame as drawn");
    assert_eq!(rewired(&c4, &kept4, &c.nets[mcu_net.0 as usize].name), Vec::<String>::new(),
        "a paste at mcu's edge pins mcu as drawn, and its other wires stay as drawn too");

    // A part on TEMPO in a root column: the gates array takes no hint, but it may not move either.
    let tempo = c.nets.iter().find(|n| n.name == "TEMPO").expect("keep setup: drum16 has TEMPO").id;
    let op = CircuitOp::AddPart {
        stmt: "let tempo_pull = e.r(100kohm, _n0, _n1, {package: \"0402\"})".into(), after: Some(after),
        ends: vec![End::Net(tempo), End::Open], hint: Some(SheetHint { at: Some(SchPt::new(540, 390)), ..SheetHint::default() }),
    };
    let arr = splice_verified(&sources, &libs, &ev, &[DesignOp::Circuit { unit: c.unit, op }], &NO_CANCEL).expect("the TEMPO paste splices");
    let d5 = &arr.evaluated.design;
    let u5 = unit_of(d5);
    let c5 = d5.units[u5].circuit.clone();
    let (kept5, _) = crate::kept_layout(d5, u5, &base, &[new_path(&c, &c5).as_str()], &opts).expect("TEMPO: the root has room");
    assert_eq!(moved(&c, &before, &c5, &kept5), Vec::<String>::new(), "a paste on TEMPO moves no part, the gates array's included");
    let frames = |s: &crate::SheetLayout| s.frames[1..].iter().map(|f| f.rect).collect::<Vec<_>>();
    assert_eq!(frames(&kept5), frames(&before), "a paste on TEMPO moves no block's frame (the root may grow)");
    assert_eq!(rewired(&c5, &kept5, "TEMPO"), Vec::<String>::new(), "a paste on TEMPO keeps every other net's wires as drawn");
}

/// A level pinned where it is drawn is drawn the same, wires included: the keep pins disturbed
/// levels as drawn, so a paste redraws no wire it does not touch.
#[test]
fn a_sheet_pinned_as_drawn_draws_the_same() {
    use fab_elec_lang::{evaluate, std_libs};
    let sources = fab_elec_fixtures::docs("drum16").expect("the drum machine has stock documents");
    let ev = evaluate(&sources, &std_libs()).expect("the drum machine evaluates");
    let unit = ev.design.units.iter().position(|u| u.name == "drum16").expect("the design keeps the drum16 board");
    let c = &ev.design.units[unit].circuit;
    let opts = Opts { aspect: 2.98, ..Opts::default() };
    let free = layout(&ev.design, unit, &SheetHints::default(), &opts);
    let hint = |at: [i32; 2], turn, mirror| SheetHint { at: Some(SchPt::new(at[0], at[1])), turn, mirror, ..SheetHint::default() };
    let parts = c.comps.iter().enumerate().filter(|(n, _)| free.parts[*n].placed && free.movers[*n] == Mover::Itself)
        .filter_map(|(n, p)| Some((p.path.clone(), hint(to_local(&free, free.parts[n].block, free.parts[n].at)?, free.parts[n].turn, free.parts[n].mirror))));
    let blocks = c.tree.iter().enumerate().filter(|(k, _)| free.block_movable[*k])
        .filter_map(|(k, b)| Some((b.path.clone(), hint(to_local(&free, b.parent?.0, block_origin(&free, k as u32)?)?, 0, false))));
    let hints = SheetHints { parts: parts.chain(blocks).collect(), ..SheetHints::default() };
    assert!(hints.parts.len() > 100, "setup: most of drum16 takes a hint");
    let pinned = layout(&ev.design, unit, &hints, &opts);
    let moved = c.comps.iter().enumerate().filter(|(n, _)| pinned.parts[*n].at != free.parts[*n].at).map(|(_, p)| p.path.as_str()).collect::<Vec<_>>();
    assert_eq!(moved, Vec::<&str>::new(), "every part stays where it is drawn");
    // Per net, its runs in a canonical direction and order: the drawing, not the routing order.
    let drawn = |s: &SheetLayout| {
        let mut runs: Vec<(u32, Vec<[i32; 2]>)> = s.wires.iter().map(|w| (w.net, sheet::canon_run(&w.pts))).collect();
        runs.sort();
        runs
    };
    let (was, is) = (drawn(&free), drawn(&pinned));
    let rewired = was.iter().filter(|r| !is.contains(r)).map(|(net, _)| c.nets[*net as usize].name.as_str()).collect::<Vec<_>>();
    assert!(rewired.is_empty() && was.len() == is.len(), "every wire is drawn as before, point for point: {rewired:?}");
    assert!(free.buses == pinned.buses && free.junctions == pinned.junctions, "buses and junctions are drawn as before");
}

#[test]
fn layout_is_pure_and_clean_on_the_fixture() {
    let d = fab_elec_fixtures::ledctl();
    let a = layout(&d, 0, &SheetHints::default(), &Opts::default());
    let b = layout(&d, 0, &SheetHints::default(), &Opts::default());
    assert_eq!(a, b, "purity: repeated layout of the same design gives equal sheets");
    let cloned = d.clone();
    let threaded = std::thread::spawn(move || {
        layout(&cloned, 0, &SheetHints::default(), &Opts::default())
    })
    .join()
    .expect("purity: layout of a cloned design on another thread must finish");
    assert_eq!(a, threaded, "purity: a clone on another thread gives the same sheet");

    let c = &d.units[0].circuit;
    assert_eq!(a.parts.len(), c.comps.len(), "placement: one entry per component");
    assert_eq!(a.frames.len(), c.tree.len(), "containment: one frame per block, including root");
    assert_eq!(sheet::overlaps(&a), Vec::<(String, String)>::new(), "cleanliness: no body or label overlaps");
    let strips: Vec<_> = a.labels.iter().filter_map(|label| match label.owner {
        sheet::Owner::Trace(net) => Some((net, label)),
        _ => None,
    }).collect();
    let g = adapt::graph_of(c, &SheetHints::default());
    let led_nets: Vec<u32> = (0..8).map(|i| {
        c.net(&format!("LED{i}")).expect("bundle: the fixture has each LED net").id.0
    }).collect();
    let bundles: Vec<_> = a.labels.iter().filter_map(|label| match label.owner {
        sheet::Owner::Bundle(bus) if g.buses[bus as usize].nets.len() == led_nets.len()
            && led_nets.iter().all(|net| g.buses[bus as usize].nets.contains(net)) => Some(label),
        _ => None,
    }).collect();
    assert_eq!(bundles.len(), 1, "bundle: the lamps' bus carries one stacked strip");
    let bundle = bundles[0];
    assert!((bundle.bx[3] - bundle.bx[1] - (8.0 * sheet::BUNDLE_ROW + 0.4)).abs() < 1e-9,
        "bundle: its height reserves one row per member");
    assert!((bundle.bx[2] - bundle.bx[0] - sheet::TRACE_W).abs() < 1e-9,
        "bundle: its width is the strip width");
    assert!(bundle.leader.is_none(), "bundle: the stacked strip has no leader");
    assert!(bundle.lines.is_empty(), "bundle: the stacked strip has no text lines");
    assert!(!strips.iter().any(|(net, _)| led_nets.contains(net)),
        "bundle: its members carry no single strips");
    let named: Vec<&str> = a.labels.iter().filter_map(|label| match label.owner {
        sheet::Owner::Net(net) => Some(g.nets[net as usize].name.as_str()),
        _ => None,
    }).collect();
    let traced: Vec<&str> = strips.iter().map(|(net, _)| g.nets[*net as usize].name.as_str()).collect();
    // Strips go where the sheet has room beside a wire (a strip never slides away on a leader).
    assert!(strips.len() >= 2, "strips: at least two nets carry a strip (strips {traced:?}, names {named:?})");
    let mut unique = traced.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), traced.len(), "strips: one strip per net on the sheet ({traced:?})");
    assert!(strips.iter().all(|(_, label)| label.leader.is_none()), "strips: no strip has a leader");
    // The eight lamps are one array: their nets carry strips all together or not at all.
    let lamps = traced.iter().filter(|name| name.starts_with("LED")).count();
    assert!(lamps == 0 || lamps == 8, "strips: an array's members are all-or-none ({traced:?})");
    assert!(strips.iter().all(|(net, _)| g.nets[*net as usize].kind == crate::graph::NetKind::Signal),
        "strips: only signal nets");
    assert_eq!(sheet::overlap_count(&a), 0, "strips: no strip overlaps another label, a body or a tag");
    let gap = crate::labels::LabelOpts::default().gap;
    for (net, label) in strips {
        assert!(label.lines.is_empty(), "strips: a strip has no text lines");
        assert!(a.wires.iter().filter(|run| run.net == net).flat_map(|run| run.pts.windows(2))
            .filter(|piece| piece[0][1] == piece[1][1] && (piece[0][0] - piece[1][0]).abs() >= 3)
            .any(|piece| {
                let x0 = piece[0][0].min(piece[1][0]) as f64;
                let x1 = piece[0][0].max(piece[1][0]) as f64;
                let y = piece[0][1] as f64;
                let dx = (x0 - label.bx[2]).max(label.bx[0] - x1).max(0.0);
                let dy = (y - label.bx[3]).max(label.bx[1] - y).max(0.0);
                dx.hypot(dy) <= sheet::TRACE_H + gap
            }), "strips: each strip touches its net's piece: net {net}");
    }
    let slanted: Vec<_> = a
        .wires
        .iter()
        .chain(a.buses.iter())
        .flat_map(|r| {
            r.pts.windows(2).filter(|w| w[0][0] != w[1][0] && w[0][1] != w[1][1]).map(move |w| (r.net, w[0], w[1]))
        })
        .collect();
    assert_eq!(slanted, vec![], "orthogonal wires: every stroke of a wire or bus run is horizontal or vertical");
    let far: Vec<_> = a
        .labels
        .iter()
        .filter_map(|l| {
            let [p, q] = l.leader?;
            let len = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
            (len > 3.0).then(|| (l.owner, l.lines.first().map(|(text, _)| text.clone()).unwrap_or_default(), len))
        })
        .collect();
    assert_eq!(far, vec![], "labels near what they name: no leader longer than three grid steps");
    for (i, comp) in c.comps.iter().enumerate() {
        let part = &a.parts[i];
        assert!(part.placed, "placement: {} must be placed", comp.path);
        assert_eq!(part.block, comp.block.0, "containment: {} belongs to its own block", comp.path);
        if part.block != 0 {
            let frame = &a.frames[part.block as usize];
            assert!(
                contains(frame.rect, part.body),
                "containment: {} body {:?} must lie inside block frame {:?}",
                comp.path, part.body, frame.rect,
            );
        }
    }
    for block in &c.tree {
        let frame = &a.frames[block.id.ix()];
        assert!(frame.placed, "containment: block {:?} must have a placed frame", block.path);
        if block.id != BlockId(0) {
            let parent = block.parent.expect("containment: a non-root block has a parent");
            let parent_frame = &a.frames[parent.ix()];
            assert!(
                contains(parent_frame.rect, frame.rect),
                "containment: block {} frame {:?} must lie inside parent frame {:?}",
                block.path, frame.rect, parent_frame.rect,
            );
        }
    }
}

#[test]
fn legible_labels_keep_references_clear_and_values_only_where_they_fit() {
    use crate::legible::legible_labels;
    use sheet::{Owner, TextRole, LINE_GAP, TEXT_REFDES, TEXT_VALUE};

    let d = fab_elec_fixtures::ledctl();
    let sheet = layout(&d, 0, &SheetHints::default(), &Opts::default());
    let parts: Vec<_> = sheet.labels.iter().enumerate()
        .filter(|(_, label)| matches!(label.owner, Owner::Part(_))).collect();
    let names = parts.iter().map(|(_, label)| label.lines[0].0.as_str()).collect::<Vec<_>>().join(", ");
    let original = legible_labels(&sheet, 1.0);
    assert_eq!(original.len(), parts.len(), "{names}: original coverage: one entry per part label");
    for (&(index, label), placed) in parts.iter().zip(&original) {
        let refdes = &label.lines[0].0;
        assert_eq!(placed.label, index, "{refdes}: original ordering follows sheet labels");
        assert_eq!(placed.refdes, Some([label.bx[0], label.bx[3] - TEXT_REFDES,
            label.bx[0] + sheet::text_w(refdes, TEXT_REFDES), label.bx[3]]),
            "{refdes}: original reference occupies its own top line");
        let value = label.lines.get(1).filter(|(_, role)| *role == TextRole::Value).map(|(value, _)| {
            let top = label.bx[3] - TEXT_REFDES - LINE_GAP * TEXT_VALUE;
            [label.bx[0], top - TEXT_VALUE, label.bx[0] + sheet::text_w(value, TEXT_VALUE), top]
        });
        assert_eq!(placed.value, value, "{refdes}: original value occupies its own second line iff present");
    }

    // A usual enlargement, and the view's cap, which a narrow pane reaches.
    for scale in [2.5, 4.0] {
        let enlarged = legible_labels(&sheet, scale);
        assert_eq!(enlarged.len(), parts.len(), "{names}: enlarged coverage: one entry per part label");
        assert_eq!(enlarged, legible_labels(&sheet, scale), "{names}: determinism: repeated placement is equal");
        let overlaps = |a: [f64; 4], b: [f64; 4]| {
            a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
        };
        assert!(enlarged.iter().any(|placed| placed.refdes.is_some()), "{names}: enlarged references show where they fit");
        let sheet_bounds = sheet.bounds.map(f64::from);
        for (&(index, label), placed) in parts.iter().zip(&enlarged) {
            let refdes = format!("{} at {scale}x", label.lines[0].0);
            assert_eq!(placed.label, index, "{refdes}: enlarged ordering follows sheet labels");
            let Some(own_box) = placed.refdes else {
                assert!(placed.value.is_none(), "{refdes}: a hidden reference shows no value");
                continue;
            };
            assert!((own_box[3] - own_box[1] - TEXT_REFDES * scale).abs() < 1e-9,
                "{refdes}: reference height uses the enlargement");
            let Owner::Part(owner) = label.owner else { unreachable!() };
            let own = crate::legible::box_distance(own_box, sheet.parts[owner as usize].body.map(f64::from));
            for (n, part) in sheet.parts.iter().enumerate().filter(|(_, part)| part.placed) {
                assert!(!overlaps(own_box, part.body.map(f64::from)),
                    "{refdes}: reference clears every part body");
                if n != owner as usize {
                    assert!(crate::legible::box_distance(own_box, part.body.map(f64::from)) >= own,
                        "{refdes}: a shown reference is not nearer another part than its own");
                }
            }
            for other in &enlarged {
                if let Some(other_box) = other.refdes.filter(|_| other.label != placed.label) {
                    assert!(!overlaps(own_box, other_box), "{refdes}: reference clears every other reference");
                }
                if let Some(value) = other.value {
                    assert!(!overlaps(own_box, value), "{refdes}: reference clears every shown value");
                }
            }
            // Both passes also keep clear of tags, frame borders, non-part labels and runs, inside the sheet.
            for bounds in std::iter::once(own_box).chain(placed.value) {
                assert!(bounds[0] >= sheet_bounds[0] && bounds[1] >= sheet_bounds[1]
                    && bounds[2] <= sheet_bounds[2] && bounds[3] <= sheet_bounds[3],
                    "{refdes}: enlarged text stays inside the sheet's bounds");
                for other in sheet.labels.iter().filter(|other| !matches!(other.owner, Owner::Part(_))) {
                    assert!(!overlaps(bounds, other.bx), "{refdes}: enlarged text clears non-part labels");
                }
                for tag in &sheet.tags {
                    assert!(!overlaps(bounds, sheet::tag_item_box(tag)), "{refdes}: enlarged text clears tags");
                }
                for run in sheet.wires.iter().chain(&sheet.buses) {
                    for segment in run.pts.windows(2) {
                        assert!(sheet::segment_box_distance(segment[0], segment[1], bounds) > 0.0,
                            "{refdes}: enlarged text clears every wire and bus segment");
                    }
                }
                for frame in &sheet.frames {
                    let [left, bottom, right, top] = frame.rect;
                    let corners = [[left, bottom], [right, bottom], [right, top], [left, top], [left, bottom]];
                    for edge in corners.windows(2) {
                        assert!(sheet::segment_box_distance(edge[0], edge[1], bounds) > 0.0,
                            "{refdes}: enlarged text clears frame borders");
                    }
                }
            }
            if let Some(value) = placed.value {
                assert!(label.lines.get(1).is_some_and(|(_, role)| *role == TextRole::Value),
                    "{refdes}: a shown value has a source value line");
                let own = crate::legible::box_distance(value, own_box);
                for other in enlarged.iter().filter(|other| other.label != placed.label) {
                    if let Some(other_box) = other.refdes {
                        assert!(crate::legible::box_distance(value, other_box) > own,
                            "{refdes}: a shown value is nearer its own reference than any other");
                    }
                }
                assert!((value[3] - value[1] - TEXT_VALUE * scale).abs() < 1e-9,
                    "{refdes}: value height uses the same enlargement");
                for part in &sheet.parts {
                    assert!(!overlaps(value, part.body.map(f64::from)), "{refdes}: value clears every part body");
                }
                for other in &enlarged {
                    if let Some(other_box) = other.refdes {
                        assert!(!overlaps(value, other_box), "{refdes}: value clears every reference");
                    }
                    if let Some(other_value) = other.value.filter(|_| other.label != placed.label) {
                        assert!(!overlaps(value, other_value), "{refdes}: value clears every other shown value");
                    }
                }
            }
        }
    }
}

#[test]
fn edits_keep_the_mental_map() {
    let d = fab_elec_fixtures::ledctl();
    let before = layout(&d, 0, &SheetHints::default(), &Opts::default());
    for (name, (edited, touched)) in [
        ("add_part", add_part(&d)),
        ("remove_part", remove_part(&d)),
        ("add_net", add_net(&d)),
    ] {
        let after = layout(&edited, 0, &SheetHints::default(), &Opts::default());
        let comps = &edited.units[0].circuit.comps;
        let mapping: Vec<_> = d.units[0]
            .circuit
            .comps
            .iter()
            .map(|comp| comps.iter().position(|other| other.path == comp.path))
            .collect();
        // Compare the same components after removal shifts all the later indices.
        // Only placements are needed for the order metric.
        let aligned = SheetLayout {
            parts: mapping
                .iter()
                .map(|index| index.and_then(|i| after.parts.get(i)).copied().unwrap_or_default())
                .collect(),
            ..Default::default()
        };
        let inside = pairs_in(&d, |c| c.block == touched && mapping[c.id.ix()].is_some());
        let outside = pairs_in(&d, |c| c.block != touched && mapping[c.id.ix()].is_some());
        let inside_ratio = order_kept(&before, &aligned, &inside);
        let outside_ratio = order_kept(&before, &aligned, &outside);
        eprintln!("{name}: order kept inside={inside_ratio:.6}, outside={outside_ratio:.6}");

        assert!(
            before.parts.len() == d.units[0].circuit.comps.len()
                && before.parts.iter().all(|p| p.placed)
                && after.parts.len() == comps.len()
                && after.parts.iter().all(|p| p.placed),
            "{name}: placement: every component in both designs must be placed",
        );
        assert!(!inside.is_empty() && !outside.is_empty(), "{name}: order: both groups have pairs to compare");
        assert_eq!(sheet::overlaps(&after), Vec::<(String, String)>::new(), "{name}: cleanliness: no body or label overlaps");
        assert!(inside_ratio >= 0.98, "{name}: order inside the touched block: {inside_ratio} must be >= 0.98");
        assert_eq!(outside_ratio, 1.0, "{name}: order outside the touched block must be unchanged");
    }
}

/// A part just placed on empty canvas (a position hint, each pin on a fresh net of its own, as
/// the editor writes it: `e.net()` per open end) sits exactly at its hint and moves nothing else.
/// Before, it entered the layered graph as an isolated pin: its column stretched to the hint and
/// every later column was pushed past it (the whole sheet jumped). A pasted part whose other end
/// is a rail (power or ground, drawn as a tag) has no more to wire than that, and the same holds.
#[test]
fn a_placed_unconnected_part_sits_at_its_hint_and_moves_nothing() {
    let d = fab_elec_fixtures::ledctl();
    let opts = Opts::default();
    let before = layout(&d, 0, &SheetHints::default(), &opts);
    let frame = before.frames[0].rect;
    let hint = to_local(&before, 0, [frame[2] + 20, (frame[1] + frame[3]) / 2])
        .expect("placement setup: the root has a content origin");
    let ground = d.units[0].circuit.nets.iter().find(|n| n.kind == NetKind::Ground)
        .expect("placement setup: the fixture has a ground rail").id;
    for rail in [false, true] {
        let mut placed = d.clone();
        let mut c = (*placed.units[0].circuit).clone();
        let root = c.tree[0].id;
        let fresh: Vec<NetId> = (0..2).map(|i| {
            let id = NetId(c.nets.len() as u32);
            c.nets.push(Net { id, name: format!("r_placed.n{i}"), class: ClassId(0), volts: None, kind: NetKind::Signal, scope: None });
            id
        }).collect();
        let new = component(&mut c, "r_placed", "R99", fixture::parts::resistor("0402"), None,
            &[("1", fresh[0]), ("2", if rail { ground } else { fresh[1] })], &[], root);
        placed.units[0].circuit = Arc::new(c);
        let hints = SheetHints {
            parts: vec![("r_placed".into(), SheetHint { at: Some(SchPt::new(hint[0], hint[1])), ..Default::default() })],
            ..Default::default()
        };
        let after = layout(&placed, 0, &hints, &opts);
        assert_eq!(block_origin(&after, 0), block_origin(&before, 0), "rail={rail}: the root keeps its origin");
        assert_eq!(to_world(&after, 0, hint), Some(after.parts[new.ix()].at), "rail={rail}: the placed part's origin is exactly at its hint");
        for (i, comp) in d.units[0].circuit.comps.iter().enumerate() {
            assert_eq!(after.parts[i].at, before.parts[i].at, "rail={rail}: placement moves nothing: {} keeps its place", comp.path);
        }
        for (b, frame) in before.frames.iter().enumerate().skip(1) {
            assert_eq!(after.frames[b].rect, frame.rect, "rail={rail}: placement moves nothing: block {b} keeps its frame");
        }
    }
}

#[test]
fn a_pinned_part_stays_and_its_block_makes_room() {
    let d = fab_elec_fixtures::ledctl();
    let opts = Opts::default();
    let before = layout(&d, 0, &SheetHints::default(), &opts);
    let c = &d.units[0].circuit;
    let mcu = c.comps.iter().find(|comp| comp.part.kind == PartKind::Mcu)
        .expect("pin setup: fixture has an MCU");
    let block = mcu.block.0;
    let local = to_local(&before, block, before.parts[mcu.id.ix()].at)
        .expect("pin setup: MCU block has a content origin");
    let hint = [local[0] + 4, local[1] + 6];
    let hints = SheetHints {
        parts: vec![(mcu.path.clone(), SheetHint { at: Some(SchPt::new(hint[0], hint[1])), ..Default::default() })],
        ..Default::default()
    };
    let after = layout(&d, 0, &hints, &opts);
    assert_eq!(
        to_world(&after, block, hint), Some(after.parts[mcu.id.ix()].at),
        "pin position: the part origin stays exactly at the block-local hint",
    );
    assert_eq!(
        to_local(&after, block, after.parts[mcu.id.ix()].at), Some(hint),
        "pin coordinates: world and block-local conversions are inverses",
    );
    // The free parts keep their block-local places, so the pin moves its part against them:
    // the debug header left of the MCU stays put while the MCU takes its hint.
    let header = c.comps.iter().find(|comp| comp.block == mcu.block && comp.part.kind == PartKind::Connector)
        .expect("pin setup: the MCU block has a header");
    assert_eq!(
        to_local(&after, block, after.parts[header.id.ix()].at), to_local(&before, block, before.parts[header.id.ix()].at),
        "pin effect: a free part of the pinned part's block keeps its block-local place",
    );
    assert_eq!(block_origin(&after, 0), Some([0, 0]), "root origin: the root uses its arrange origin");
    assert_eq!(to_local(&after, u32::MAX, [0, 0]), None, "missing block: no local coordinates");
    assert_eq!(to_world(&after, u32::MAX, [0, 0]), None, "missing block: no world coordinates");
    assert_eq!(sheet::overlaps(&after), Vec::<(String, String)>::new(), "pin clearance: no body, tag or label overlaps");
    let slanted: Vec<_> = after
        .wires
        .iter()
        .chain(after.buses.iter())
        .flat_map(|r| {
            r.pts.windows(2).filter(|w| w[0][0] != w[1][0] && w[0][1] != w[1][1]).map(move |w| (r.net, w[0], w[1]))
        })
        .collect();
    assert_eq!(slanted, vec![], "pin routing: every wire and bus stroke is orthogonal");
    for (i, comp) in c.comps.iter().enumerate() {
        let part = &after.parts[i];
        let frame = &after.frames[comp.block.ix()];
        assert!(part.placed && frame.placed, "pin placement: {} and its frame are placed", comp.path);
        assert!(contains(frame.rect, part.body), "pin containment: {} body stays inside its frame", comp.path);
    }
    for block in &c.tree {
        if let Some(parent) = block.parent {
            assert!(
                contains(after.frames[parent.ix()].rect, after.frames[block.id.ix()].rect),
                "pin containment: block {} stays inside its parent", block.path,
            );
        }
    }
    let outside = pairs_in(&d, |comp| comp.block != mcu.block);
    assert!(!outside.is_empty(), "pin mental map: there are pairs outside the MCU block");
    assert_eq!(order_kept(&before, &after, &outside), 1.0, "pin mental map: order outside the MCU block is unchanged");
    assert_eq!(after, layout(&d, 0, &hints, &opts), "pin purity: repeated layout with the hint gives equal sheets");

    // The header dragged left of its block: a view that keeps the block's origin where it was
    // (translate) sees the block grow left while the MCU and the rest of the block stay.
    let header_place = before.parts[header.id.ix()];
    let header_local = to_local(&before, block, header_place.at).expect("grow setup: header is placed");
    let move_header = |at: [i32; 2]| SheetHint {
        at: Some(SchPt::new(at[0], at[1])), turn: header_place.turn, mirror: header_place.mirror, ..Default::default()
    };
    let left = [header_local[0] - 8, header_local[1]];
    let hints = SheetHints {
        parts: vec![(header.path.clone(), move_header(left))],
        ..Default::default()
    };
    let mut grown = layout(&d, 0, &hints, &opts);
    let (was, now) = (block_origin(&before, block).expect("grow setup: origin before"), block_origin(&grown, block).expect("grow setup: origin after"));
    grown.translate([was[0] - now[0], was[1] - now[1]]);
    assert_eq!(block_origin(&grown, block), Some(was), "translate: the block's origin is back where it was");
    assert_eq!(to_local(&grown, block, grown.parts[header.id.ix()].at), Some(left), "translate: block-local hints are unchanged");
    assert_eq!(grown.parts[mcu.id.ix()].at, before.parts[mcu.id.ix()].at, "grow left: the MCU keeps its world place");
    assert!(grown.frames[block as usize].rect[0] < before.frames[block as usize].rect[0], "grow left: the block's frame grows left");
    assert!(contains(grown.frames[block as usize].rect, grown.parts[header.id.ix()].body), "grow left: the frame holds the moved header");

    // The MCU dragged left past the column of the parts that feed it: they keep their side and
    // are pushed left (no neighbour jumps across), and its block grows left round them.
    let far = [local[0] - 24, local[1]];
    let hints = SheetHints {
        parts: vec![(mcu.path.clone(), SheetHint { at: Some(SchPt::new(far[0], far[1])), ..Default::default() })],
        ..Default::default()
    };
    let mut pushed = layout(&d, 0, &hints, &opts);
    let now = block_origin(&pushed, block).expect("push setup: origin after");
    pushed.translate([was[0] - now[0], was[1] - now[1]]);
    assert_eq!(to_local(&pushed, block, pushed.parts[mcu.id.ix()].at), Some(far), "push: the MCU takes its hint");
    for comp in c.comps.iter().filter(|comp| comp.block == mcu.block && comp.id != mcu.id) {
        let side = |s: &SheetLayout| s.parts[comp.id.ix()].body[2] <= s.parts[mcu.id.ix()].body[0];
        assert_eq!(side(&pushed), side(&before), "push: {} keeps its side of the MCU", comp.refdes);
    }
    assert!(pushed.parts[header.id.ix()].at[0] < before.parts[header.id.ix()].at[0], "push: the header is pushed left");
    assert!(pushed.frames[block as usize].rect[0] < before.frames[block as usize].rect[0], "push: the block grows left");
    assert_eq!(sheet::overlaps(&pushed), Vec::<(String, String)>::new(), "push: no body, tag or label overlaps");

    // Two pins: the header dragged right (the layout's own hint, so it leads) past the MCU
    // pinned at its own place in the circuit. The header stays the MCU's feeder on its left and
    // pushes the pinned MCU right (no crossing columns).
    let past = [header_local[0] + 40, header_local[1]];
    let mut stored = d.clone();
    Arc::make_mut(&mut stored.units[0].circuit).hints.parts
        .push((mcu.path.clone(), SheetHint { at: Some(SchPt::new(local[0], local[1])), ..Default::default() }));
    let hints = SheetHints {
        parts: vec![(header.path.clone(), move_header(past))],
        ..Default::default()
    };
    let both = layout(&stored, 0, &hints, &opts);
    assert_eq!(to_local(&both, block, both.parts[header.id.ix()].at), Some(past), "two pins: the dragged header takes its hint");
    assert!(both.parts[header.id.ix()].body[2] < both.parts[mcu.id.ix()].body[0], "two pins: the header stays left of the MCU");
    let mcu_x = to_local(&both, block, both.parts[mcu.id.ix()].at).expect("two pins: MCU placed")[0];
    assert!(mcu_x > local[0], "two pins: the pinned MCU is pushed right, {mcu_x} > {}", local[0]);
    assert_eq!(sheet::overlaps(&both), Vec::<(String, String)>::new(), "two pins: no body, tag or label overlaps");

    // The MCU dragged left into the header pinned at its own place in the text: the dragged part
    // leads, so the pinned header gives way to the left, as the pinned MCU gave way to the right
    // above. The drop writes the MCU's hint and pins the parts that gave way where the preview
    // drew them, in one text step: the committed layout is the preview, position for position,
    // and one undo restores the text byte for byte.
    let unit = d.units[0].id;
    let at = |p: [i32; 2]| SheetHint { at: Some(SchPt::new(p[0], p[1])), ..Default::default() };
    let hint_op = |path: &str, hint: SheetHint| DesignOp::Circuit { unit, op: CircuitOp::Hint { path: path.into(), hint: Some(hint) } };
    let sources = fab_elec_fixtures::docs("ledctl").expect("lead setup: the controller's documents");
    let (pinned_src, pinned, _) = commit_text(&sources, &fab_elec_fixtures::ledctl_eval(), &[hint_op(&header.path, move_header(header_local))]);
    let stored = &pinned.design.units[0].circuit.hints;
    let into = [header_local[0] + 4, local[1]];
    let drag = crate::drag_layout(&pinned.design, 0, &mcu.path, &at(into), &opts);
    let preview = &drag.sheet;
    assert_eq!(to_local(preview, block, preview.parts[mcu.id.ix()].at), Some(into), "lead: the dragged MCU takes its hint");
    assert!(preview.parts[header.id.ix()].body[2] < preview.parts[mcu.id.ix()].body[0], "lead: the header stays left of the MCU");
    let header_now = to_local(preview, block, preview.parts[header.id.ix()].at).expect("lead: the header is placed");
    assert!(header_now[0] < header_local[0], "lead: the pinned header is pushed left, {} < {}", header_now[0], header_local[0]);
    assert_eq!(sheet::overlaps(preview), Vec::<(String, String)>::new(), "lead: no body, tag or label overlaps");
    assert!(drag.drop.iter().any(|(path, _)| path == &header.path), "lead: the drop rewrites the pushed header's hint: {:?}", drag.drop);
    assert!(drag.drop.iter().any(|(path, _)| stored.parts.iter().all(|(p, _)| p != path)),
        "lead: the drop pins an unhinted part that gave way with the header: {:?}", drag.drop);
    for (path, hint) in &drag.drop {
        let n = c.comps.iter().position(|comp| &comp.path == path).expect("drop: a part of the circuit");
        assert_ne!(path, &mcu.path, "drop: the dragged part's own hint is not in the drop list");
        let drawn = to_local(preview, preview.parts[n].block, preview.parts[n].at).map(|p| SchPt::new(p[0], p[1]));
        assert_eq!(hint.at, drawn, "drop: {path} is pinned where the preview drew it");
    }
    assert_eq!(crate::drag_drop(&pinned.design, 0, &mcu.path, &at(into), &opts), drag.drop, "drop: a release's own drop hints are the preview's");
    // The MCU's new hint, the yielded part's new one and the header's replaced one: one commit.
    let mut ops = vec![hint_op(&mcu.path, at(into))];
    ops.extend(drag.drop.iter().map(|(path, hint)| hint_op(path, hint.clone())));
    let (dropped_src, dropped, undo) = commit_text(&pinned_src, &pinned, &ops);
    let committed = layout(&dropped.design, 0, &SheetHints::default(), &opts);
    for (i, comp) in c.comps.iter().enumerate() {
        assert_eq!(committed.parts[i], preview.parts[i], "drop: {} is where the preview drew it", comp.refdes);
    }
    assert_eq!(committed.frames, preview.frames, "drop: every block frame is where the preview drew it");
    let mut undone = dropped_src.clone();
    undone.apply(&undo.edits).expect("undo: the commit's inverse applies as one step");
    assert_eq!(texts(&undone), texts(&pinned_src), "undo: one step restores the text byte for byte");
    let restored = fab_elec_lang::evaluate(&undone, &fab_elec_lang::std_libs()).expect("undo: the restored text evaluates");
    assert_eq!(&restored.design.units[0].circuit.hints, stored, "undo: one step restores every rewritten hint");

    // Grip the MCU again, this time with a second pinned part in its block.
    let mut group = pinned.design.clone();
    let circuit = Arc::make_mut(&mut group.units[0].circuit);
    let other = component(circuit, "mcu.spare", "R99", fixture::parts::resistor("0402"), None, &[], &[], mcu.block);
    let spare = [local[0], local[1] + 40];
    circuit.hints.parts.push((mcu.path.clone(), at(local)));
    circuit.hints.parts.push(("mcu.spare".into(), at(spare)));
    let delta = [into[0] - local[0], 6];
    let shift = |p: [i32; 2]| [p[0] + delta[0], p[1] + delta[1]];
    let heads = SheetHints {
        parts: vec![(mcu.path.clone(), at(shift(local))), ("mcu.spare".into(), at(shift(spare)))], ..Default::default()
    };
    let preview = crate::drag_group_layout(&group, 0, &mcu.path, &heads, &opts);
    for (node, expected) in [(mcu.id, shift(local)), (other, shift(spare))] {
        assert_eq!(to_local(&preview.sheet, block, preview.sheet.parts[node.ix()].at), Some(expected),
            "group preview: head {node:?} takes its own translated hint");
    }
    let drop = crate::drag_group_drop(&group, 0, &mcu.path, &heads, &opts);
    assert_eq!(drop, preview.drop, "group drop: release uses the preview's displaced hints");
    assert!(heads.parts.iter().all(|(head, _)| drop.iter().all(|(path, _)| path != head)),
        "group drop: neither moved head is written a second time");
    assert!(drop.iter().any(|(path, hint)| path == &header.path && hint.at.is_some_and(|p| p.x < header_local[0])),
        "group drop: the pinned header that gave way is written at its new position");
}

#[test]
fn a_hinted_turn_holds_where_the_part_turns_on_its_own() {
    let d = fab_elec_fixtures::ledctl();
    let opts = Opts::default();
    let c = &d.units[0].circuit;
    let free = layout(&d, 0, &SheetHints::default(), &opts);
    let comp = |path: &str| c.comp(path).expect("turn setup: the fixture has the part");
    let (mcu, button, lamp) = (comp("mcu.u"), comp("btn.sw"), comp("lamps[0].r"));
    let header = c.comps.iter().find(|comp| comp.block == mcu.block && comp.part.kind == PartKind::Connector)
        .expect("turn setup: the MCU block has a header");
    let (u, h, s, r) = (mcu.id.ix(), header.id.ix(), button.id.ix(), lamp.id.ix());
    assert_eq!([free.parts[u].turn_step, free.parts[h].turn_step, free.parts[s].turn_step], [1, 1, 1],
        "turn step: a chip, a connector and a two-pin part placed on their own turn by quarters");
    assert_eq!(free.parts[r].turn_step, 0, "turn step: a chain member's turn follows its chain");
    let drawn = |n: usize| {
        let p = &free.parts[n];
        let at = to_local(&free, p.block, p.at).expect("turn setup: the part is in its block");
        SchPt::new(at[0], at[1])
    };
    let hint = |comp: &Component, turn, mirror| (comp.path.clone(), SheetHint {
        at: Some(drawn(comp.id.ix())), turn, mirror, ..Default::default()
    });
    let half = |n: usize| (free.parts[n].turn + 2) % 4;
    let quarter = |n: usize| (free.parts[n].turn + 1) % 4;
    let hints = SheetHints {
        parts: vec![hint(header, half(h), free.parts[h].mirror), hint(button, quarter(s), free.parts[s].mirror),
            hint(lamp, half(r), !free.parts[r].mirror)],
        ..Default::default()
    };
    let turned = layout(&d, 0, &hints, &opts);
    assert_eq!((turned.parts[h].turn, turned.parts[h].mirror), (half(h), free.parts[h].mirror),
        "turn: a connector can rotate while keeping its drawn mirror in the hint");
    assert_eq!(turned.parts[s].turn, quarter(s), "turn: a hinted two-pin part takes a quarter turn");
    assert_eq!((turned.parts[r].turn, turned.parts[r].mirror), (free.parts[r].turn, free.parts[r].mirror),
        "a chain determines its member's turn and mirror even when the hint requests both to change");
    assert_eq!(sheet::overlaps(&turned), Vec::<(String, String)>::new(), "turn: no body, tag or label overlaps");
    // A chip turned a quarter either way: its pin flags on the top and bottom read sideways and
    // fit a pin step, so neighbouring flags (the LED and SWD names) never cross.
    for turn in [1, 3] {
        let chip = layout(&d, 0, &SheetHints { parts: vec![hint(mcu, turn, free.parts[u].mirror)], ..Default::default() }, &opts);
        assert_eq!(chip.parts[u].turn, turn, "turn: a hinted chip takes its hint's turn ({turn})");
        assert!(chip.parts[u].quarter, "turn: a chip turned an odd number of quarters is drawn in its quarter fit");
        assert_eq!(sheet::overlaps(&chip), Vec::<(String, String)>::new(), "turn: a chip turned {turn} quarters crosses nothing");
    }

    // R while carrying: the dragged part's hint carries the new turn; the preview draws it and
    // the drop pins every pushed part at its drawn turn, so the commit is the preview.
    let into = drawn(u);
    let into = SchPt::new(into.x - 4, into.y);
    let drag = crate::drag_layout(&d, 0, &mcu.path, &SheetHint { at: Some(into), turn: quarter(u), ..Default::default() }, &opts);
    assert_eq!(drag.sheet.parts[u].turn, quarter(u), "turn while carrying: the preview turns the dragged part");
    assert_eq!(sheet::overlaps(&drag.sheet), Vec::<(String, String)>::new(), "turn while carrying: no overlaps");
    for (path, hint) in &drag.drop {
        let n = c.comp(path).map(|comp| comp.id.ix());
        assert!(n.is_none_or(|n| (hint.turn, hint.mirror) == (drag.sheet.parts[n].turn, drag.sheet.parts[n].mirror)),
            "turn while carrying: {path}'s drop hint keeps the orientation the preview drew");
    }
}

#[test]
fn a_chain_and_a_decoupling_row_draw_their_hinted_turns() {
    use crate::plan::{mirror_part, turn_part, Planned};
    use fab_elec_circuit::{Block, BlockSource};

    let mut c = Circuit::default();
    let root = BlockId(0);
    c.tree.push(Block {
        id: root, path: String::new(), def: None, parent: None, ports: vec![], params: vec![], source: BlockSource::Code,
    });
    let [gnd, link, vcc] = [("GND", NetKind::Ground), ("link", NetKind::Signal), ("VCC", NetKind::Power)].map(|(name, kind)| {
        let id = NetId(c.nets.len() as u32);
        c.nets.push(Net { id, name: name.into(), kind, class: ClassId(0), volts: None, scope: None });
        id
    });
    // Ground at the head makes the unhinted chain draw in reverse order, with the head below.
    let r = fixture::parts::resistor("0402");
    let head = component(&mut c, "r1", "R1", r.clone(), None, &[("1", gnd), ("2", link)], &[], root);
    let tail = component(&mut c, "r2", "R2", r, None, &[("1", vcc), ("2", link)], &[], root);
    let [cap, other] = [("c1", "C1"), ("c2", "C2")].map(|(path, name)| {
        component(&mut c, path, name, fixture::parts::capacitor("0402"), None, &[("1", vcc), ("2", gnd)], &[], root)
    });
    let draw = |parts| sheet::layout_graph(&adapt::graph_of(&c, &SheetHints { parts, ..Default::default() }), &Opts::default());
    let hint = |path: &str, turn, mirror| (path.to_owned(), SheetHint { turn, mirror, ..Default::default() });
    let chain = |s: &SheetLayout| {
        let (a, b) = (&s.parts[head.ix()], &s.parts[tail.ix()]);
        (a.turn, b.turn, [b.at[0] - a.at[0], b.at[1] - a.at[1]])
    };
    let free = draw(vec![]);
    assert_eq!(chain(&free), (1, 3, [0, 5]), "the unhinted chain has its supply above its ground");
    let same = draw(vec![hint("r1", free.parts[head.ix()].turn, false), hint("c1", 1, true)]);
    assert_eq!(chain(&same), chain(&free), "hinting the head's drawn turn preserves both poses and the chain's order");
    assert_eq!((same.parts[cap.ix()].turn, same.parts[cap.ix()].mirror), (1, true), "the hinted cap takes its turn and mirror");
    assert_eq!((same.parts[other.ix()].turn, same.parts[other.ix()].mirror), (3, false), "the other cap keeps supply up");
    let segments = |s: &SheetLayout| {
        let mut all: Vec<_> = s.wires.iter()
            .flat_map(|r| r.pts.windows(2).map(move |w| (r.net, w[0].min(w[1]), w[0].max(w[1])))).collect();
        all.sort();
        all
    };
    for (vertical, reverses) in [(false, false), (true, true)] {
        let Planned::Edit(edit) = mirror_part(&c, &free, tail, vertical) else { panic!("X/Y on a chain member plans a mirror") };
        let [DesignOp::Circuit { op: CircuitOp::Hint { path, hint: Some(planned) }, .. }] = edit.ops.as_slice()
            else { panic!("X/Y produces one part hint") };
        let shown = draw(vec![(path.clone(), planned.clone())]);
        let (a, b) = (&shown.parts[head.ix()], &shown.parts[tail.ix()]);
        assert!(a.mirror && b.mirror && (a.turn, a.mirror) == (planned.turn, planned.mirror),
            "vertical={vertical}: every chain member is drawn mirrored, the head at its hinted pose");
        let [dx, dy] = chain(&free).2;
        assert_eq!(chain(&shown).2, if reverses { [dx, -dy] } else { [dx, dy] },
            "vertical={vertical}: the upright chain keeps its order under X and reverses top to bottom under Y");
        if !reverses {
            assert!(a.at == free.parts[head.ix()].at && segments(&shown) == segments(&free)
                && shown.tags.len() == free.tags.len() && shown.tags.iter().all(|t| free.tags.contains(t)),
                "X on an upright chain mirrors it across its own axis: no part, wire or rail tag moves");
        }
    }

    let Planned::Edit(edit) = turn_part(&c, &free, tail, false) else { panic!("R on a chain member plans a turn") };
    let [DesignOp::Circuit { op: CircuitOp::Hint { path, hint: Some(planned) }, .. }] = edit.ops.as_slice()
        else { panic!("R produces one part hint") };
    let turned = draw(vec![(path.clone(), planned.clone()), hint("c1", 0, false)]);
    assert_eq!(chain(&turned), ((free.parts[head.ix()].turn + 3) % 4, (free.parts[tail.ix()].turn + 3) % 4, [5, 0]),
        "R on the tail turns both parts clockwise and puts their centres on one horizontal line");
    let [x, y] = turned.parts[head.ix()].at;
    let [tx, ty] = turned.parts[tail.ix()].at;
    let (a, b) = ([x + 2, y], [tx - 2, ty]);
    assert!(turned.wires.iter().any(|r| r.net == link.0 && r.pts.windows(2).any(|seg| seg == [a, b] || seg == [b, a])),
        "the link joins the facing pins along x");
    for (net, at, dir) in [(gnd, [x - 2, y], [-1, 0]), (vcc, [tx + 2, ty], [1, 0])] {
        assert!(turned.tags.iter().any(|t| t.net == Some(net.0) && t.at == at && t.dir == dir),
            "the rail at {at:?} points out of the turned chain");
    }
    let (flat, upright) = (&turned.parts[cap.ix()], &turned.parts[other.ix()]);
    assert_eq!((flat.turn, upright.turn), (0, 3), "one cap can lie down while its neighbour stays upright");
    assert!(flat.at[1] == upright.at[1] && flat.body[2] < upright.body[0], "the row makes room for the flat cap's width");
}

#[test]
fn a_mirror_hint_flips_the_drawn_pin_before_its_turn() {
    use fab_elec_circuit::{Block, BlockSource, NetStyle};

    let mut c = Circuit::default();
    let root = BlockId(0);
    c.tree.push(Block {
        id: root, path: String::new(), def: None, parent: None, ports: vec![], params: vec![], source: BlockSource::Code,
    });
    let sig = NetId(0);
    c.nets.push(Net { id: sig, name: "SIG".into(), class: ClassId(0), volts: None, kind: NetKind::Signal, scope: None });
    let j = component(&mut c, "j", "J1", fixture::parts::header(1, 2.54), None, &[("1", sig)], &[], root);
    c.hints.nets.push(("SIG".into(), NetStyle::Label));
    let hint = |turn, mirror| SheetHints {
        parts: vec![("j".into(), SheetHint { turn, mirror, ..Default::default() })], ..Default::default()
    };
    let draw = |c: &Circuit, hints: &SheetHints| sheet::layout_graph(&adapt::graph_of(c, hints), &Opts::default());
    assert!(draw(&c, &SheetHints::default()).parts[j.ix()].mirror, "the unhinted header faces its signal pin right");
    c.hints.parts = hint(1, true).parts;
    for (hints, turn, mirror, dir) in [
        (hint(0, false), 0, false, [-1, 0]),
        (hint(0, true), 0, true, [1, 0]),
        (SheetHints::default(), 1, true, [0, 1]),
    ] {
        let sheet = draw(&c, &hints);
        let part = &sheet.parts[j.ix()];
        let tag = sheet.tags.iter().find(|tag| tag.kind == sheet::TagKind::Label && tag.net == Some(sig.0))
            .expect("the signal label marks the header's only pin");
        assert_eq!((part.turn, part.mirror), (turn, mirror), "the symbol takes the requested turn and mirror");
        assert_eq!(tag.dir, dir, "the pin points outward after mirror={mirror}, turn={turn}");
        let [x, y] = tag.at;
        assert!((x - part.at[0]) * dir[0] + (y - part.at[1]) * dir[1] > 0,
            "the signal label attaches on the expected side of the symbol after mirror={mirror}, turn={turn}");
    }
}

#[test]
fn a_dragged_block_leads_and_its_neighbours_make_room() {
    let d = fab_elec_fixtures::ledctl();
    let opts = Opts::default();
    let c = &d.units[0].circuit;
    let before = layout(&d, 0, &SheetHints::default(), &opts);
    let block = |path: &str| c.tree.iter().position(|b| b.path == path).expect("block setup: the fixture has the block") as u32;
    let (mcu, reg) = (block("mcu"), block("reg"));
    let local = |s: &SheetLayout, b: u32| to_local(s, 0, block_origin(s, b).expect("block setup: the block is placed"))
        .expect("block setup: the root has an origin");
    let at = |p: [i32; 2]| SheetHint { at: Some(SchPt::new(p[0], p[1])), ..Default::default() };
    assert!(before.block_movable[mcu as usize] && before.block_movable[reg as usize], "movable: the MCU and regulator blocks move");
    assert!(!before.block_movable[0], "movable: the root does not move");
    assert!(c.tree.iter().enumerate().any(|(k, b)| b.path.starts_with("lamps[") && !before.block_movable[k]),
        "movable: an array's members do not move on their own");

    // A block hint places the block's content origin in its parent; its parts keep their places
    // in it and the rest of the sheet makes room.
    let (mcu_at, reg_at) = (local(&before, mcu), local(&before, reg));
    let moved = [mcu_at[0] + 6, mcu_at[1] + 4];
    let after = layout(&d, 0, &SheetHints { parts: vec![("mcu".into(), at(moved))], ..Default::default() }, &opts);
    assert_eq!(local(&after, mcu), moved, "block pin: the block's content origin lands on its hint");
    for comp in c.comps.iter().filter(|comp| comp.block.0 == mcu) {
        let i = comp.id.ix();
        assert_eq!(to_local(&after, mcu, after.parts[i].at), to_local(&before, mcu, before.parts[i].at),
            "block pin: {} keeps its place in the block", comp.refdes);
    }
    assert_eq!(sheet::overlaps(&after), Vec::<(String, String)>::new(), "block pin: no body, tag or label overlaps");

    // The MCU block dragged left into the regulator block pinned at its own place in the text:
    // the dragged block leads, so the regulator gives way to the left. The drop writes both
    // blocks' hints in one text step: the committed layout is the preview, and one undo restores
    // the text byte for byte.
    let unit = d.units[0].id;
    let hint_op = |path: &str, hint: SheetHint| DesignOp::Circuit { unit, op: CircuitOp::Hint { path: path.into(), hint: Some(hint) } };
    let sources = fab_elec_fixtures::docs("ledctl").expect("block setup: the controller's documents");
    let (pinned_src, pinned, _) = commit_text(&sources, &fab_elec_fixtures::ledctl_eval(), &[hint_op("reg", at(reg_at))]);
    assert_eq!(local(&layout(&pinned.design, 0, &SheetHints::default(), &opts), reg), reg_at, "block lead setup: the pinned regulator stays");
    let into = [reg_at[0] + 4, mcu_at[1]];
    let drag = crate::drag_layout(&pinned.design, 0, "mcu", &at(into), &opts);
    let preview = &drag.sheet;
    assert_eq!(local(preview, mcu), into, "block lead: the dragged MCU block takes its hint");
    assert!(preview.frames[reg as usize].rect[2] < preview.frames[mcu as usize].rect[0], "block lead: the regulator stays left of the MCU");
    assert!(local(preview, reg)[0] < reg_at[0], "block lead: the pinned regulator is pushed left");
    assert_eq!(sheet::overlaps(preview), Vec::<(String, String)>::new(), "block lead: no body, tag or label overlaps");
    let reg_now = local(preview, reg);
    assert!(drag.drop.iter().any(|(path, hint)| path == "reg" && hint.at == Some(SchPt::new(reg_now[0], reg_now[1]))),
        "block lead: the drop rewrites the pushed regulator's hint where the preview drew it: {:?}", drag.drop);
    assert!(drag.drop.iter().all(|(path, _)| path != "mcu"), "block lead: the dragged block's own hint is not in the drop list");
    let mut ops = vec![hint_op("mcu", at(into))];
    ops.extend(drag.drop.iter().map(|(path, hint)| hint_op(path, hint.clone())));
    let (dropped_src, dropped, undo) = commit_text(&pinned_src, &pinned, &ops);
    let committed = layout(&dropped.design, 0, &SheetHints::default(), &opts);
    assert_eq!(committed.parts, preview.parts, "block drop: every part is where the preview drew it");
    assert_eq!(committed.frames, preview.frames, "block drop: every block frame is where the preview drew it");
    let mut undone = dropped_src.clone();
    undone.apply(&undo.edits).expect("block undo: the commit's inverse applies as one step");
    assert_eq!(texts(&undone), texts(&pinned_src), "block undo: one step restores the text byte for byte");
}

#[test]
fn power_flags_ride_the_first_host_pin_and_a_lone_flag_stays_an_anchor() {
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("flags.ckt.splash", r#"let e = mod.elec
let gnd = e.net({name: "GND" kind: @ground})
let signal = e.net({name: "OUT"})
let spare = e.net({name: "SPARE"})
let link = e.net({name: "LINK"})
let r = e.part(mod.std.chip("R", "0402"), {pins: [gnd link]})
let r2 = e.part(mod.std.chip("R", "0402"), {pins: [link]})
let ground_flag = e.part(mod.std.power_flag(), {pins: [gnd]})
let reg = e.part(mod.std.ldo_3v3(), {pins: {GND: gnd VOUT: signal}})
let out_flag = e.part(mod.std.power_flag(), {pins: [signal]})
let link_flag = e.part(mod.std.power_flag(), {pins: [link]})
let lone = e.part(mod.std.power_flag(), {pins: [spare]})
"#).expect("the flag circuit has a valid source name");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("the flag circuit evaluates");
    let c = &ev.design.units[0].circuit;
    let g = adapt::graph_of(c, &SheetHints::default());
    let mols = molecules::find(&g, &g.port_nets(), &vec![false; g.nets.len()]);
    let sheet = sheet::layout_graph(&g, &Opts::default());
    let reg = c.comp("reg").expect("the regulator is the chip host");
    let host = sheet.parts[reg.id.ix()];
    for (path, name) in [("ground_flag", "GND"), ("out_flag", "VOUT")] {
        let flag = c.comp(path).expect("the marker is in the circuit").id;
        let pin = reg.part.pin(name).expect("the regulator has the marked pin");
        let m = mols.iter().find(|m| m.parts.contains(&flag.0)).expect("a connected flag forms a molecule");
        assert_eq!((m.kind, m.host), (MolKind::Flag, Some((reg.id.0, pin.0 as u32))), "{name} chooses the chip pin even with an earlier passive");
        assert_eq!(sheet.movers[flag.ix()], Mover::Rides { host: reg.id.0, port: pin.0 as u32 }, "{name} flag rides its host");
        let place = sheet.parts[flag.ix()];
        assert!(place.placed && place.glyph, "{name} flag draws the family symbol");
        let [x, y] = g.nodes[reg.id.ix()].ports[pin.ix()].at;
        let at = [host.at[0] + x, host.at[1] + y];
        if name == "VOUT" {
            assert_eq!(place.at, at, "the signal flag's foot coincides with the right pin");
            assert_eq!(place.turn, 3, "the signal flag points right, away from the regulator");
        } else {
            let net = c.net("GND").expect("the ground rail exists").id.0;
            assert_eq!(place.turn, 2, "the ground flag points down from the regulator");
            let groups = crate::groups::wire_groups(&sheet, &[(net, at), (net, place.at)]);
            let joined = groups.iter().find(|g| g.pins == [0, 1]).expect("the ground pin and the flag foot share a wired connection");
            assert!(joined.tags.iter().map(|&i| &sheet.tags[i as usize]).any(|t| {
                t.kind == sheet::TagKind::Ground && t.at[0] != place.at[0]
            }), "the wired connection keeps its ground bar beside the flag");
        }
    }
    let r = c.comp("r").expect("the chain has a first resistor");
    let link_flag = c.comp("link_flag").expect("the chain link has a flag").id;
    assert!(mols.iter().any(|m| m.kind == MolKind::Chain && m.parts.contains(&r.id.0)), "a marker on the private link preserves the chain");
    let port = r.part.pin("2").expect("pin 2 joins the chain").0 as u32;
    assert_eq!(sheet.movers[link_flag.ix()], Mover::Rides { host: r.id.0, port }, "a passive-only net chooses its first part in source order");
    assert!(sheet.parts[link_flag.ix()].placed, "a flag riding a chain member is drawn with the chain");
    let lone = c.comp("lone").expect("the lone flag is in the circuit").id;
    assert!(!mols.iter().any(|m| m.parts.contains(&lone.0)), "a flag alone on its net belongs to no molecule");
    assert!(sheet.parts[lone.ix()].placed && sheet.movers[lone.ix()] == Mover::Itself, "the lone flag can be placed independently");
    assert!(!sheet.tags.iter().any(|t| t.kind == sheet::TagKind::Label), "the flags need no net-name tags");
    assert!(!sheet.labels.iter().flat_map(|l| &l.lines).any(|(text, role)| *role == sheet::TextRole::Refdes && text.starts_with('#')),
        "the sheet reserves no label for a hidden flag reference");
}

#[test]
fn molecules_series_and_crystal_hang_on_their_pins() {
    let d = fab_elec_fixtures::ledctl();
    let before = layout(&d, 0, &SheetHints::default(), &Opts::default());
    let mut edited = d.clone();
    let mut c = (*edited.units[0].circuit).clone();
    let mcu = c.comp("mcu.u").expect("fixture has MCU").id;
    let block = c.comps[mcu.ix()].block;
    let gnd = c.net("GND").expect("fixture has ground").id;
    let [xi, xo, series_near, series_far] = ["mcu.XI", "mcu.XO", "mcu.SER", "mcu.TP"].map(|name| {
        let id = NetId(c.nets.len() as u32);
        c.nets.push(Net { id, name: name.into(), class: ClassId(0), volts: None, kind: NetKind::Signal, scope: None });
        id
    });
    // PA8 and PA9 are adjacent on the right. PC14 leaves room below the crystal for the
    // series part. J2 has no spare pin, so its far net goes to a one-pin test header.
    let chip = &mut c.comps[mcu.ix()];
    let [osc0, osc1, signal] = ["PA8", "PA9", "PC14"].map(|name| chip.part.pin(name).expect("fixture has free MCU pin"));
    for (pin, net) in [(osc0, xi), (osc1, xo), (signal, series_near)] {
        assert!(chip.nc.contains(&pin), "fixture edit: each newly connected MCU pin was no-connect");
        chip.nc.retain(|&p| p != pin);
        chip.conns.push((pin, net));
    }
    chip.conns.sort_by_key(|&(pin, _)| pin);
    let mut crystal = (*fixture::parts::resistor("0402")).clone();
    crystal.kind = PartKind::Crystal;
    crystal.name = "XTAL-3215".into();
    let y1 = component(&mut c, "mcu.xtal", "Y1", Arc::new(crystal), None, &[("1", xi), ("2", xo)], &[], block);
    let [c8, c9] = [("mcu.load[0]", "C8", xi), ("mcu.load[1]", "C9", xo)].map(|(path, refdes, net)| {
        component(
            &mut c, path, refdes, fixture::parts::capacitor("0402"),
            Some(Value { text: "12pF".into(), qty: Some(Quantity { si: 12e-12, unit: QtyUnit::Farad }), tol: None }),
            &[("1", net), ("2", gnd)], &[], block,
        )
    });
    let r9 = component(
        &mut c, "mcu.series", "R9", fixture::parts::resistor("0402"),
        Some(Value { text: "33".into(), qty: Some(Quantity { si: 33.0, unit: QtyUnit::Ohm }), tol: None }),
        &[("1", series_near), ("2", series_far)], &[], block,
    );
    component(&mut c, "mcu.tp", "J3", fixture::parts::header(1, 2.54), None, &[("1", series_far)], &[], block);
    edited.units[0].circuit = Arc::new(c);

    let g = adapt::graph_of(&edited.units[0].circuit, &SheetHints::default());
    let mut bound = vec![false; g.nets.len()];
    for b in &g.blocks {
        for port in &b.ports {
            for &net in &port.nets {
                bound[net as usize] = true;
            }
        }
    }
    let mols = molecules::find(&g, &g.port_nets(), &bound);
    let crystals: Vec<_> = mols.iter().filter(|m| m.kind == MolKind::Crystal).collect();
    let series: Vec<_> = mols.iter().filter(|m| m.kind == MolKind::Series).collect();
    assert_eq!(crystals.len(), 1, "detection: exactly one crystal molecule");
    assert_eq!(crystals[0].parts, vec![y1.0, c8.0, c9.0], "detection: crystal first, then its load caps in pin order");
    assert_eq!(crystals[0].host, Some((mcu.0, osc0.0 as u32)), "detection: crystal hangs on its first MCU pin");
    assert_eq!(crystals[0].block, block.0, "detection: crystal remains in the MCU block");
    assert_eq!(series.len(), 1, "detection: exactly one series molecule");
    assert_eq!(series[0].parts, vec![r9.0], "detection: the series resistor is the sole series member");
    assert_eq!(series[0].host, Some((mcu.0, signal.0 as u32)), "detection: series resistor hangs on its MCU pin");
    assert_eq!(series[0].block, block.0, "detection: series resistor remains in the MCU block");
    for member in [y1, c8, c9, r9] {
        assert_eq!(mols.iter().filter(|m| m.parts.contains(&member.0)).count(), 1, "ownership: each new part belongs to exactly one molecule");
    }

    let after = layout(&edited, 0, &SheetHints::default(), &Opts::default());
    assert_eq!(after.parts.len(), edited.units[0].circuit.comps.len(), "placement: one entry per edited component");
    assert!(before.parts.iter().all(|p| p.placed) && after.parts.iter().all(|p| p.placed), "placement: all old and new components are placed");
    assert_eq!(sheet::overlaps(&after), Vec::<(String, String)>::new(), "cleanliness: no body or label overlaps after hanging the molecules");
    let slanted: Vec<_> = after
        .wires
        .iter()
        .chain(after.buses.iter())
        .flat_map(|r| {
            r.pts.windows(2).filter(|w| w[0][0] != w[1][0] && w[0][1] != w[1][1]).map(move |w| (r.net, w[0], w[1]))
        })
        .collect();
    assert_eq!(slanted, vec![], "orthogonal wires: every stroke of a wire or bus run is horizontal or vertical");
    let chip = after.parts[mcu.ix()];
    let crystal = after.parts[y1.ix()];
    let dx = (chip.body[0] - crystal.body[2]).max(crystal.body[0] - chip.body[2]).max(0);
    let dy = (chip.body[1] - crystal.body[3]).max(crystal.body[1] - chip.body[3]).max(0);
    assert!(dx > 0 || dy > 0, "crystal position: its body lies outside the chip body");
    assert!(dx * dx + dy * dy <= 8 * 8, "crystal proximity: its body is within eight steps of the chip");
    assert_eq!(crystal.turn & 1, 1, "crystal pose: its body stands parallel to the chip's right side");
    let resistor = after.parts[r9.ix()];
    let host_pin = g.nodes[mcu.ix()].ports[signal.ix()].at;
    assert_eq!(resistor.at[1], chip.at[1] + host_pin[1], "series pose: the resistor stays in line with its host pin");
    assert_eq!(resistor.turn & 1, 0, "series pose: the resistor lies along its host pin's outward direction");
    // A drag moves what the layout can pin: an anchor, a free chain by its first part; molecule
    // members on a pin ride with their host.
    assert_eq!(after.movers.len(), after.parts.len(), "movers: one entry per part");
    assert_eq!(after.movers[mcu.ix()], Mover::Itself, "movers: an anchor takes its own hint");
    for member in [y1, c8, c9] {
        assert_eq!(after.movers[member.ix()], Mover::Rides { host: mcu.0, port: osc0.0 as u32 }, "movers: crystal members ride on their host pin");
    }
    assert_eq!(after.movers[r9.ix()], Mover::Rides { host: mcu.0, port: signal.0 as u32 }, "movers: the series part rides on its host pin");
    let chain = mols.iter().find(|m| m.kind == MolKind::Chain).expect("fixture has a free chain");
    assert_eq!(after.movers[chain.parts[0] as usize], Mover::Itself, "movers: a free chain's first part takes the hint");
    for &p in &chain.parts[1..] {
        assert_eq!(after.movers[p as usize], Mover::Head(chain.parts[0]), "movers: later chain members move with the first");
    }
    let far = [resistor.at[0] + 2, resistor.at[1]];
    assert!(
        after.wires.iter().any(|w| w.net == series_far.0 && w.pts.contains(&far))
            || after.tags.iter().any(|t| t.net == Some(series_far.0) && t.at == far && t.kind == sheet::TagKind::Label),
        "series routing: the far signal end reaches the block router",
    );
    let inside = pairs_in(&d, |comp| comp.block == block);
    let outside = pairs_in(&d, |comp| comp.block != block);
    assert!(!inside.is_empty() && !outside.is_empty(), "mental map: both groups have original pairs to compare");
    // All edits append parts, so the original indices already align with the edited sheet.
    let inside_ratio = order_kept(&before, &after, &inside);
    let outside_ratio = order_kept(&before, &after, &outside);
    assert!(inside_ratio >= 0.98, "mental map: order kept inside the MCU block is {inside_ratio}, must be >= 0.98");
    assert_eq!(outside_ratio, 1.0, "mental map: order outside the MCU block must be unchanged");
}

/// A pin alone on its net is open: the sheet marks it and gives it no tag. Wiring two placed
/// resistors pin to pin makes a chain whose outer ends stay open (the Library writes each open
/// end as `e.net()`), and a series resistor hosted on a header pin keeps an open far end; neither
/// end may get a label the router continues by name (it read "1" and "2" with an error halo).
#[test]
fn open_ends_of_a_chain_or_a_series_part_get_no_tag() {
    let mut sources = fab_elec_lang::Sources::new();
    sources.add("open.ckt.splash", concat!(
        "let e = mod.elec\n",
        "let mid = e.net()\n",
        "let r = e.r(10kohm, e.net(), mid)\n",
        "let r2 = e.r(10kohm, mid, e.net())\n",
        "let s = e.net()\n",
        "let j = e.part(mod.std.header({pins: 2 pitch: 2.54mm}), {pins: {\"1\": s \"2\": s}})\n",
        "let rs = e.r(1kohm, s, e.net())\n",
    ).to_string()).expect("the circuit document");
    let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("the circuit evaluates");
    let c = &ev.design.units[0].circuit;
    let sheet = layout(&ev.design, 0, &c.hints, &Opts::default());
    let comp = |path: &str| c.comps.iter().find(|k| k.path == path).unwrap_or_else(|| panic!("{path} is evaluated"));
    for (path, pin) in [("r", 0), ("r2", 1), ("rs", 1)] {
        let net = comp(path).conns[pin].1;
        let pins = c.comps.iter().flat_map(|k| k.conns.iter()).filter(|(_, n)| *n == net).count();
        assert_eq!(pins, 1, "{path} pin {}: the end is open, alone on {}", pin + 1, c.nets[net.ix()].name);
        let tags: Vec<_> = sheet.tags.iter().filter(|t| t.net == Some(net.0)).map(|t| t.text.clone()).collect();
        assert!(tags.is_empty(), "{path} pin {}: an open end gets no tag, got {tags:?}", pin + 1);
    }
    let mid = comp("r").conns[1].1;
    assert!(sheet.wires.iter().any(|w| w.net == mid.0), "the wired pins are drawn as a wire");
}
