//! Evaluated bench values and their shared splice decisions, built once at the edit pause.

use crate::bench_splice::{self, DriveChange, DriveSite};
use crate::eval::{BenchOut, Evaluated, UnitState};
use crate::map::{BenchKey, SourceMap, SourceRange};
use crate::source::{Sources, located};
use fab_elec_core::{Diag, DocId};
use makepad_script::syntax::Syntax;
use std::collections::HashMap;

/// One evaluated bench and what of it an edit can change.
#[derive(Clone, Debug, PartialEq)]
pub struct BenchView {
    pub out: BenchOut,
    /// One entry per stub pin (`BenchKey::Stub`), expectation (`Expect`), pressed button
    /// (`Press`) and probe (`Probe`) of `out`, in bench order: stubs (pins in spec order), then
    /// expectations, presses, probes.
    pub edits: Vec<(BenchKey, BenchEdit)>,
    /// Whether new statements (pressing another button, adding a probe) can be written into this
    /// bench's body.
    pub insert: BenchEdit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BenchEdit {
    /// Written in place: `splice` writes any new value of this kind of op.
    Literal,
    /// Made from written numbers elsewhere: numeric changes and alternative `if` forms
    /// go through `splice_verified`, which edits these leaves.
    Derived { leaves: Vec<SourceRange> },
    /// Not editable here; the located reason a card shows.
    Refused(Diag),
}

/// Every evaluated bench's view, in `ev.benches` order. A bench that did not build has an empty
/// `edits` and a refused `insert` carrying its first diagnostic.
pub fn bench_views(ev: &Evaluated, sources: &Sources) -> Vec<BenchView> {
    let mut documents = Documents { sources, trees: HashMap::new() };
    ev.benches.iter().map(|out| {
        if out.state != UnitState::Built {
            let diag = out.state.diags().first().cloned().unwrap_or_else(|| {
                located(out.doc, 0..0, format!("{}: the bench did not build", out.name))
            });
            return BenchView { out: out.clone(), edits: Vec::new(), insert: BenchEdit::Refused(diag) };
        }
        let mut classify = |key: &BenchKey| {
            edit(&mut documents, &ev.map, &out.name, key).unwrap_or_else(BenchEdit::Refused)
        };
        let insert = classify(&BenchKey::Body);
        let keys = out.spec.stubs.iter().flat_map(|stub| {
            stub.pins.iter().map(|(pin, _)| BenchKey::Stub(stub.path.clone(), pin.clone()))
        })
            .chain(out.expects.iter().map(|expect| BenchKey::Expect(expect.name.clone())))
            .chain(out.spec.pressed.iter().cloned().map(BenchKey::Press))
            .chain(out.probes.iter().map(|probe| BenchKey::Probe(probe.of.clone())));
        let edits = keys.map(|key| {
            let edit = classify(&key);
            (key, edit)
        }).collect();
        BenchView { out: out.clone(), edits, insert }
    }).collect()
}

/// Several benches may share a document; even a parse failure is cached for this call.
struct Documents<'a> {
    sources: &'a Sources,
    trees: HashMap<DocId, Result<Syntax, Diag>>,
}

impl Documents<'_> {
    fn tree(&mut self, at: &SourceRange) -> Result<&Syntax, Diag> {
        self.trees.entry(at.doc).or_insert_with(|| {
            bench_splice::document(self.sources, at).and_then(bench_splice::parse)
        }).as_ref().map_err(Clone::clone)
    }
}

fn edit(
    documents: &mut Documents<'_>,
    map: &SourceMap,
    bench: &str,
    key: &BenchKey,
) -> Result<BenchEdit, Diag> {
    let (_, body_at) = bench_splice::mapped(documents.sources, map, bench)?;
    match key {
        BenchKey::Body => {
            bench_splice::insert_site(documents.tree(body_at)?, body_at)?;
        }
        BenchKey::Stub(stub, pin) => {
            let (at, current) = bench_splice::drive_entry(map, bench, body_at, stub, pin)?;
            let tree = documents.tree(body_at)?;
            let site = bench_splice::drive_site(
                tree, map, body_at, at, map.bench_origin(bench, key),
                &format!("{stub}.{pin}"), current, DriveChange::same_kind(current), false,
            )?;
            let (origin, terms): (_, Vec<_>) = match &site {
                DriveSite::Derived { site, .. } => (site.origin, vec![
                    (&site.terms[0], Some(fab_elec_circuit::QtyUnit::Hertz)), (&site.terms[1], None),
                ]),
                DriveSite::Alternatives { site, .. } => (site.origin, site.terms().collect()),
                DriveSite::Literal { .. } => return Ok(BenchEdit::Literal),
            };
            let mut leaves = Vec::new();
            for (term, unit) in terms {
                if let Ok((leaf, _, _)) = term.choose(tree, origin, unit) {
                    let at = SourceRange {
                        doc: origin.call.doc,
                        span: leaf.at.start as u32..leaf.at.end as u32,
                    };
                    if !leaves.contains(&at) {
                        leaves.push(at);
                    }
                }
            }
            return Ok(BenchEdit::Derived { leaves });
        }
        BenchKey::Expect(expect) => {
            let (at, current) = bench_splice::range_entry(map, bench, expect)?;
            let tree = documents.tree(at)?;
            let mut leaves = Vec::new();
            let mut derived = false;
            for bound in ["min", "max"] {
                let site = bench_splice::range_site(
                    tree, at, map.bench_origin(bench, key), bound, &format!("{expect} {bound}"),
                    bench_splice::range_unit(current),
                )?;
                let span = if let Some(input) = site.derived {
                    derived = true;
                    Some(input.leaf.at)
                } else {
                    site.field.map(|field| field.value.span.clone())
                };
                if let Some(span) = span {
                    let at = SourceRange { doc: at.doc, span: span.start as u32..span.end as u32 };
                    if !leaves.contains(&at) {
                        leaves.push(at);
                    }
                }
            }
            if derived {
                return Ok(BenchEdit::Derived { leaves });
            }
        }
        BenchKey::Press(what) => {
            if let Some(at) = map.bench_site(bench, key) {
                bench_splice::remove_site(
                    documents.tree(body_at)?, map, bench, body_at, at, key, what, "press",
                )?;
            }
        }
        BenchKey::Probe(of) => {
            if let Some(at) = map.bench_site(bench, key) {
                bench_splice::remove_site(
                    documents.tree(body_at)?, map, bench, body_at, at, key, &of.to_string(), "probe",
                )?;
            }
        }
        // a supply's or a source's statement, written directly (its values splice in place)
        BenchKey::Supply(net) => {
            if let Some(at) = map.bench_site(bench, key) {
                bench_splice::remove_site(
                    documents.tree(body_at)?, map, bench, body_at, at, key, &format!("supply {net}"), "supply",
                )?;
            }
        }
        BenchKey::Source(a, b) => {
            if let Some(at) = map.bench_site(bench, key) {
                bench_splice::remove_site(
                    documents.tree(body_at)?, map, bench, body_at, at, key, &format!("source ({a}, {b})"), "source or load",
                )?;
            }
        }
    }
    Ok(BenchEdit::Literal)
}
