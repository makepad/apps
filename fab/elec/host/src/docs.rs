//! The project's documents in front of the engine (C5, landing 6): the text is the truth for
//! every statement it writes, and the model follows it.
//!
//! A board commit applies its ops to the model (the validator: edit rules refuse there), then
//! splices them into the board document and follows the source map. Undo and redo apply the
//! logged text step and follow it, so the map's ledger and therefore the item ids return exactly.
//! Factory choices splice into the board header. Other system ops stay model-only until the host lowers `.sys.splash` (gap G1); circuit and bench
//! ops are spliced and verified on the pool before adoption.
//!
//! Cost per board commit: one parse of the edited board document and one pass over the map
//! (`splice`), the map's clone when a snapshot holds it, and one rebuilt [`DocsView`] (O(documents
//! + units + notes) Arc clones). Nothing here runs per frame or per item outside the edit.

use crate::DocSet;
use fab_elec_core::{BoardId, Diag, DocId, Sel};
use fab_elec_design::{Design, DesignOp, TextEdit, TextStep};
use fab_elec_lang::sys_sim::{self, SimEval};
use fab_elec_lang::{bench_views, defs_catalog, evaluate, splice, BenchOut, BenchView, BenchScope, Catalog, DocKind, Evaluated, Libs, ProjectDefs, SourceMap, SpanEdit, Sources, SysEval, UnitEval, UnitState};
use fab_elec_view::{DocText, DocsView};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

pub struct Docs {
    sources: Sources,
    libs: Arc<Libs>,
    map: Arc<SourceMap>,
    units: Vec<UnitEval>,
    unit_ids: Vec<(DocId, BoardId)>,
    /// Evaluated benches keep their unit bindings for canvas edits.
    benches: Vec<BenchOut>,
    /// Their views (`fab_elec_lang::bench_views`), shared with every snapshot.
    bench_views: Arc<Vec<BenchView>>,
    /// The system pass's diagnostics survive between evaluations.
    system: SysEval,
    /// The last pre-save check's mismatch between the system text and the model
    /// (`fab_elec_lang::check_system`), shown with the notes until a check passes.
    mismatch: Option<Diag>,
    /// The system document's `sim` statement, read whenever that text changes (system text does
    /// not re-evaluate), with the text it was read from.
    sim: SimEval,
    sim_text: Option<(DocId, Arc<str>)>,
    notes: Vec<Diag>,
    /// Per document in sources order: the presence and text the design follows.
    seen: Vec<(bool, Arc<str>)>,
    /// The asset names a mesh found at the last evaluation (the project's and the part library's,
    /// `Sources::mesh_assets`), also used to invalidate unit reuse.
    assets: Vec<String>,
    /// Definition inputs and lazy catalog retained across circuit-only edits.
    defs: Arc<ProjectDefs>,
    /// The Library panel's entries for `defs` ([`defs_catalog`]): the same Arc until a definition
    /// document changes. A failed evaluation keeps the last one.
    catalog: Arc<Catalog>,
    /// Per document, in `sources` order: one more with each text or presence change, or addition,
    /// from [`Docs::first_rev`].
    rev: Vec<u64>,
    /// The revision every document of this load starts at: above every revision an earlier load
    /// in this process gave, so a revision names one text for the process's life and an edit
    /// addressed to a replaced load never applies to this one.
    first_rev: u64,
    /// One more each time a step of this history changes the asset list (an import, its undo or
    /// redo); registering the plate's own assets does not, as the plate holds them already.
    asset_rev: u64,
    /// Per unit: one more each time its board is lowered again from text, which may move its item
    /// and net ids. A logged board entry takes the model's fast path only at its epochs.
    epoch: BTreeMap<BoardId, u64>,
    view: Arc<DocsView>,
}

impl Docs {
    /// Evaluate `sources` from nothing. When no unit builds, the design is empty and the located
    /// diags are the documents' notes: a load never falls back to another design. A failed load
    /// still lists its definitions in the catalog.
    /// Cost: a full evaluation, then O(documents + units + notes) view construction, and the
    /// catalog ([`Docs::from_evaluated`]).
    pub fn load(sources: Sources, libs: Libs) -> (Design, Docs) {
        match evaluate(&sources, &libs) {
            Ok(evaluated) => Docs::from_evaluated(sources, evaluated),
            Err(diags) => {
                let assets = sources.mesh_assets().to_vec();
                let defs = Arc::new(ProjectDefs::new(&sources, &libs));
                let docs = Docs::new(sources, Arc::new(libs), SourceMap::new(), Vec::new(), Vec::new(), Vec::new(), Arc::default(), SysEval::default(), diags, assets, defs);
                (Design::default(), docs)
            }
        }
    }

    /// The documents `evaluated` was made from (`sources`), and its design.
    /// Cost: O(documents + units + notes) to initialise tracking and the view, the bench views
    /// (one parse of each bench document), and the catalog: one enumeration VM for a library
    /// snapshot not enumerated before and one for the project's definitions, on the caller's
    /// thread (the strand's load). Later evaluations fill it on the pool.
    pub fn from_evaluated(sources: Sources, evaluated: Evaluated) -> (Design, Docs) {
        let views = Arc::new(bench_views(&evaluated, &sources));
        let Evaluated { libs, design, map, units, unit_ids, benches, system, notes, assets, defs } = evaluated;
        (design, Docs::new(sources, Arc::new(libs), map, units, unit_ids, benches, views, system, notes, assets, defs))
    }

    /// Initialise tracking at the texts just evaluated, including a failed load.
    fn new(
        sources: Sources, libs: Arc<Libs>, map: SourceMap, units: Vec<UnitEval>, unit_ids: Vec<(DocId, BoardId)>,
        benches: Vec<BenchOut>, bench_views: Arc<Vec<BenchView>>, system: SysEval, notes: Vec<Diag>, assets: Vec<String>, defs: Arc<ProjectDefs>,
    ) -> Docs {
        // Loads take turns of 2^32 revisions: no load makes that many edits.
        static LOADS: AtomicU64 = AtomicU64::new(0);
        let first_rev = LOADS.fetch_add(1, Relaxed) << 32;
        let rev = vec![first_rev; sources.all_docs().len()];
        let seen = sources.all_docs().iter().map(|d| (d.present, d.text.clone())).collect();
        let epoch = units.iter().map(|u| (u.id, 0)).collect();
        let catalog = defs_catalog(&libs, &defs);
        let mut docs = Docs { sources, libs, map: Arc::new(map), units, unit_ids, benches, bench_views, system, mismatch: None, sim: SimEval::default(), sim_text: None, notes, seen, assets, defs, catalog, rev, first_rev, asset_rev: 0, epoch, view: Arc::default() };
        docs.refresh_view();
        docs
    }

    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    /// Register a project-relative asset name once; the next evaluation sees the new list.
    /// Cost: O(log assets) lookup, O(assets) shifts for a new name.
    pub fn add_asset(&mut self, name: impl Into<String>) {
        self.sources.add_asset(name);
    }

    pub fn libs(&self) -> &Arc<Libs> {
        &self.libs
    }

    pub fn map(&self) -> &Arc<SourceMap> {
        &self.map
    }

    pub fn view(&self) -> &Arc<DocsView> {
        &self.view
    }

    /// The Library panel's entries for the last evaluated definitions.
    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// Every evaluated bench's view, in bench order; the same Arc until an evaluation changes one.
    pub fn bench_views(&self) -> &Arc<Vec<BenchView>> {
        &self.bench_views
    }

    /// One more with each document addition, text edit or presence change, and with each change of
    /// the asset list by a step: what the plate stores moved.
    pub fn revision(&self) -> u64 {
        self.rev.iter().sum::<u64>() + self.asset_rev
    }

    /// True when `texts()` would contain no files. Stops at the first present file without allocating.
    pub(crate) fn is_empty(&self) -> bool {
        !self.sources.all_docs().iter().any(|d| d.present && d.is_file())
    }

    /// The project documents by file name, as the plate stores them: O(documents) Arc clones. An
    /// empty system document is no file (the plate's load adds one back).
    pub fn texts(&self) -> Arc<BTreeMap<String, Arc<str>>> {
        Arc::new(self.sources.docs().iter()
            .filter(|d| !(d.kind == DocKind::System && d.text.is_empty()))
            .map(|d| (d.name.clone(), d.text.clone())).collect())
    }

    /// The unit's board document at its last evaluation, including one since removed.
    pub(crate) fn board_document(&self, unit: BoardId) -> Option<&fab_elec_lang::Doc> {
        let id = self.units.iter().find(|u| u.id == unit)?.docs.get(1)?;
        self.sources.all_docs().get(id.ix()).filter(|d| d.kind == DocKind::Board)
    }

    pub fn epoch(&self, unit: BoardId) -> u64 {
        self.epoch.get(&unit).copied().unwrap_or(0)
    }

    /// The engine's evaluation state, with its current design and followed map, for unit reuse.
    /// Cost: O(map entries + units + benches + assets), cloning the evaluation metadata.
    pub fn prev(&self, design: &Design) -> Evaluated {
        Evaluated {
            libs: (*self.libs).clone(), assets: self.assets.clone(), design: design.clone(),
            map: (*self.map).clone(), units: self.units.clone(), unit_ids: self.unit_ids.clone(), benches: self.benches.clone(),
            notes: self.notes.clone(), system: self.system.clone(), defs: self.defs.clone(),
        }
    }

    /// Whether text an evaluation reads, its presence, or an asset is newer than the design:
    /// system and firmware text never are. Cost: O(documents + assets); nothing per frame.
    pub fn behind(&self) -> bool {
        self.sources.mesh_assets() != self.assets.as_slice() || self.sources.all_docs().iter().enumerate()
            .any(|(ix, doc)| doc.kind.edit_reevaluates() && match self.seen.get(ix) {
                Some((present, text)) => doc.present != *present || (doc.present && !Arc::ptr_eq(&doc.text, text)),
                None => doc.present,
            })
    }

    /// Whether a job's documents and assets still match. Cost: O(documents + assets).
    pub fn same_texts(&self, sources: &Sources) -> bool {
        self.sources.assets() == sources.assets() && self.sources.library() == sources.library()
            && self.sources.all_docs().len() == sources.all_docs().len() && self.sources.all_docs().iter().zip(sources.all_docs())
            .all(|(a, b)| a.present == b.present && Arc::ptr_eq(&a.text, &b.text))
    }

    /// The unit's last evaluation state, for refusing edits of broken text. Cost: O(units).
    pub fn unit_state(&self, unit: BoardId) -> Option<&UnitState> {
        self.units.iter().find(|u| u.id == unit).map(|u| &u.state)
    }

    /// The design unit a named bench evaluates against; None for a bench that builds its own
    /// circuit. Cost: O(benches).
    pub fn bench_unit(&self, bench: &str) -> Option<BoardId> {
        self.benches.iter().find(|b| b.name == bench).and_then(|b| match b.scope {
            BenchScope::Design(unit) => Some(unit),
            BenchScope::Own { .. } => None,
        })
    }

    /// The model already equals these documents' current text (a board splice or fast-path
    /// undo/redo), so later evaluation can reuse each affected unit. Cost: O(units' documents
    /// times edited documents), with Arc swaps only.
    fn model_follows(&mut self, docs: &[DocId]) {
        for doc in docs {
            let text = &self.sources.doc(*doc).expect("an applied edit names a project document").text;
            if let Some(seen) = self.seen.get_mut(doc.ix()) {
                *seen = (true, text.clone());
            }
            for unit in &mut self.units {
                if let Some(pos) = unit.docs.iter().position(|id| id == doc) {
                    unit.texts[pos] = text.clone();
                }
            }
        }
    }

    /// The text side of a board commit whose model side is applied: splice `ops` (board ops)
    /// against `before`, the design the documents describe, apply the edit and follow the map.
    /// Returns the forward step and its inverse. All or nothing: an error changes nothing.
    /// Cost: a parse of edited boards, O(map entries + edited text), then view construction.
    pub fn commit(&mut self, before: &Design, ops: &[DesignOp]) -> Result<(TextStep, TextStep), Diag> {
        let edit = splice(&self.sources, &self.map, before, ops)?;
        let inverse = edit.inverse(&self.sources, &self.map);
        self.sources.apply(&edit.edits)?;
        self.followed(&edit);
        self.model_follows(&edit.edits.iter().map(|e| e.doc).collect::<Vec<_>>());
        Ok((edit.into(), inverse.into()))
    }

    /// Apply a logged text step (undo applies an entry's `text_inverse`, redo its `text`): edits,
    /// then presence, then asset availability; the engine follows the assets' keys.
    /// An error changes nothing; `follows` records an already-applied model fast path.
    /// Cost: O(edited text + map entries + documents + units + notes).
    pub fn apply(&mut self, step: &TextStep, follows: bool) -> Result<(), Diag> {
        if step.is_empty() {
            return Ok(());
        }
        let mut sources = self.sources.clone();
        sources.apply(&step.edits)?;
        let mut changed = Vec::new();
        for &(doc, present) in &step.present {
            let before = sources.all_docs().get(doc.ix()).map(|d| d.present);
            sources.set_present(doc, present)?;
            if before != Some(present) {
                changed.push(doc);
            }
        }
        for asset in &step.assets {
            if asset.available {
                sources.add_asset(asset.name.clone());
            } else {
                sources.remove_asset(&asset.name);
            }
        }
        if sources.assets() != self.sources.assets() {
            self.asset_rev += 1;
        }
        self.sources = sources;
        self.rev.resize(self.sources.all_docs().len(), self.first_rev);
        for doc in changed {
            self.rev[doc.ix()] += 1;
        }
        self.followed(&SpanEdit::from(step.clone()));
        if follows && step.present.is_empty() {
            self.model_follows(&step.edits.iter().map(|e| e.doc).collect::<Vec<_>>());
        }
        Ok(())
    }

    /// Plan a writer's document changes for a verifier. The returned sources already include
    /// the text edits and presence changes. [`Docs::doc_set`] plans here, then installs.
    /// Cost: one Sources clone (shared text), document lookups and circuit-board scans for
    /// the changes, and a pass over the replaced texts.
    pub fn plan_set(&self, set: &[DocSet]) -> Result<(Sources, SpanEdit), Diag> {
        let mut names = HashSet::new();
        for change in set {
            let name = match change {
                DocSet::Add { name, .. } | DocSet::Replace { name, .. } | DocSet::Remove { name, .. } => name,
                DocSet::Asset { .. } => continue,
            };
            if !names.insert(name) {
                return Err(Diag::error("one change per document", vec![]));
            }
        }
        let mut sources = self.sources.clone();
        let mut edit = SpanEdit::default();
        let mut touched = HashSet::new();
        for change in set {
            match change {
                DocSet::Add { name, text } => {
                    let doc = sources.add(name.clone(), text.clone())?;
                    edit.present.push((doc, true));
                }
                DocSet::Replace { name, base_rev, .. } | DocSet::Remove { name, base_rev } => {
                    let id = self.sources.find(name).ok_or_else(|| Diag::error(format!("no document named {name}"), vec![]))?;
                    let site = vec![Sel::Source { doc: id, start: 0, end: 0 }];
                    if !touched.insert(id) {
                        return Err(Diag::error("one change per document", site));
                    }
                    if self.rev[id.ix()] != *base_rev {
                        return Err(Diag::error("the document changed since the writer read it", site));
                    }
                    let doc = self.sources.doc(id).expect("a found document is present");
                    if let DocSet::Replace { text, .. } = change {
                        if doc.text != *text {
                            edit.edits.push(TextEdit::trimmed(id, &doc.text, text));
                        }
                    } else {
                        edit.present.push((id, false));
                        if doc.kind == DocKind::Circuit {
                            for board in self.units.iter().filter(|unit| unit.docs.first() == Some(&id))
                                .flat_map(|unit| &unit.docs).filter_map(|id| self.sources.doc(*id))
                                .filter(|doc| doc.kind == DocKind::Board)
                            {
                                if !touched.insert(board.id) {
                                    return Err(Diag::error("one change per document", vec![Sel::Source { doc: board.id, start: 0, end: 0 }]));
                                }
                                edit.present.push((board.id, false));
                            }
                        }
                    }
                }
                DocSet::Asset { .. } => {}
            }
        }
        sources.apply(&edit.edits)?;
        for &(doc, value) in &edit.present {
            sources.set_present(doc, value)?;
        }
        edit.reevaluate = !edit.edits.is_empty() || !edit.present.is_empty();
        Ok((sources, edit))
    }

    /// Apply another writer's document changes as one undo step; assets are handled by the engine.
    /// All or nothing, including revisions and the source map.
    /// Cost per Cmd::Docs: O(documents + edited text + map entries), plus one Sources clone
    /// (Arc texts, O(documents)); nothing per frame.
    pub fn doc_set(&mut self, set: &[DocSet]) -> Result<(TextStep, TextStep), Diag> {
        let (sources, mut edit) = self.plan_set(set)?;
        // Document sets record presence in plan order in both directions.
        let present = std::mem::take(&mut edit.present);
        let mut inverse = TextStep::from(edit.inverse(&self.sources, &self.map));
        inverse.present = present.iter().map(|&(doc, value)| (doc, !value)).collect();
        self.sources = sources;
        self.rev.resize(self.sources.all_docs().len(), self.first_rev);
        for &(doc, _) in &present {
            self.rev[doc.ix()] += 1;
        }
        self.followed(&edit);
        let mut forward = TextStep::from(edit);
        forward.present = present;
        Ok((forward, inverse))
    }

    /// Apply one editor revision atomically and return both text directions.
    /// Cost: O(edited text + map entries + documents + units + notes).
    /// A checked text change's checks before it is verified (`Cmd::CheckedText`): the edits name
    /// `doc` at its current revision, as `text_edit` asks of typing. O(edits).
    pub fn check_text(&self, doc: DocId, base_rev: u64, edits: &[TextEdit]) -> Result<(), Diag> {
        if base_rev < self.first_rev {
            return Err(Diag::error("the change was made for an earlier load of the documents; read them again", Vec::new()));
        }
        let site = vec![Sel::Source { doc, start: 0, end: 0 }];
        if self.doc_kind(doc).is_none() {
            return Err(Diag::error("the document is not in the project", site));
        }
        if self.rev[doc.ix()] != base_rev {
            return Err(Diag::error(format!("the document changed since rev {base_rev}: it is at rev {} now", self.rev[doc.ix()]), site));
        }
        if edits.iter().any(|edit| edit.doc != doc) {
            return Err(Diag::error("a text edit names another document", site));
        }
        Ok(())
    }

    pub fn text_edit(&mut self, doc: DocId, base_rev: u64, edits: Vec<TextEdit>) -> Result<(TextStep, TextStep), Diag> {
        // Revisions of an earlier load lie below this load's first, whose document ids may name
        // other documents: say so before anything else, naming no site, as the editor waits.
        if base_rev < self.first_rev {
            return Err(Diag::error("the editor read an earlier load of the documents; it reads them again", Vec::new()));
        }
        let site = vec![Sel::Source { doc, start: 0, end: 0 }];
        let kind = self.doc_kind(doc).ok_or_else(|| Diag::error("the document is not in the project", site.clone()))?;
        if self.rev[doc.ix()] != base_rev {
            return Err(Diag::error("the document changed since the editor read it; it reads it again", site));
        }
        if edits.iter().any(|edit| edit.doc != doc) {
            return Err(Diag::error("a text edit names another document", site));
        }
        let edit = SpanEdit { edits, reevaluate: kind.edit_reevaluates(), ..SpanEdit::default() };
        // Sources validates ranges and UTF-8 boundaries before inverse indexes the original text.
        let mut sources = self.sources.clone();
        sources.apply(&edit.edits)?;
        let inverse = edit.inverse(&self.sources, &self.map);
        self.sources = sources;
        self.followed(&edit);
        Ok((edit.into(), inverse.into()))
    }

    /// The pre-save check (ELANG's `check_system`): the system text, lowered with the map's ledger
    /// against `design`'s units, agrees with the model's links and mounts. A mismatch is a located
    /// diag kept with the notes until a later check passes; the text's own diags (a dangling end)
    /// are not one. Until the system text is lowered on every edit (gap G1, step 6, which recovers
    /// a mismatch with `relower_system`), typed system text shows here. Returns the mismatch.
    /// Cost: one lowering of the system document, O(statements + units + links), no VM; the view
    /// is rebuilt only when the result changed.
    pub fn check_system(&mut self, design: &Design) -> Option<&Diag> {
        let mismatch = fab_elec_lang::check_system(&self.sources, design, &self.map).err();
        if mismatch != self.mismatch {
            self.mismatch = mismatch;
            self.refresh_view();
        }
        self.mismatch.as_ref()
    }

    /// A project's document kind, excluding library documents. Cost: O(1).
    pub fn doc_kind(&self, doc: DocId) -> Option<DocKind> {
        self.sources.doc(doc).map(|d| d.kind)
    }

    /// Invalidate model inverses after adopting a replaced board. Cost: O(log units).
    pub fn bump_epoch(&mut self, unit: BoardId) {
        *self.epoch.entry(unit).or_default() += 1;
    }

    /// Adopt the pool's metadata, its bench views and optionally its verified source edits.
    /// Cost: O(documents + units + benches + notes) for tracking and view construction.
    pub fn adopt_evaluated(&mut self, sources: Option<Sources>, evaluated: Evaluated, bench_views: Arc<Vec<BenchView>>) {
        self.adopt_sources(sources);
        self.defs = evaluated.defs;
        // The pool job that made `evaluated` filled its catalog (`EvalJob::run`): one cache read here.
        self.catalog = defs_catalog(&self.libs, &self.defs);
        self.assets = evaluated.assets;
        self.map = Arc::new(evaluated.map);
        self.units = evaluated.units;
        self.unit_ids = evaluated.unit_ids;
        self.benches = evaluated.benches;
        self.bench_views = bench_views;
        self.notes = evaluated.notes;
        self.system = evaluated.system;
        self.refresh_view();
    }

    /// Record a completed failed evaluation while retaining its last usable metadata.
    /// Cost: O(documents + units + notes + assets) for tracking and view construction.
    pub fn adopt_failed(&mut self, sources: Option<Sources>, diags: Vec<Diag>) {
        self.adopt_sources(sources);
        // A failed attempt consumed these assets too; do not retry it until its inputs change.
        self.assets = self.sources.mesh_assets().to_vec();
        self.notes = diags;
        self.refresh_view();
    }

    /// Follow adopted texts once, moving each changed document's revision by one.
    fn adopt_sources(&mut self, sources: Option<Sources>) {
        if let Some(sources) = sources {
            if sources.assets() != self.sources.assets() {
                self.asset_rev += 1;
            }
            self.rev = sources.all_docs().iter().map(|doc| {
                let previous = self.sources.all_docs().get(doc.id.ix());
                self.rev.get(doc.id.ix()).copied().unwrap_or(self.first_rev)
                    + u64::from(previous.is_none_or(|old| old.present != doc.present || !Arc::ptr_eq(&old.text, &doc.text)))
            }).collect();
            self.sources = sources;
        }
        self.rev.resize(self.sources.all_docs().len(), self.first_rev);
        self.seen = self.sources.all_docs().iter().map(|d| (d.present, d.text.clone())).collect();
    }

    /// Follow an applied splice and rebuild the shared view at the new revisions.
    fn followed(&mut self, edit: &SpanEdit) {
        Arc::make_mut(&mut self.map).follow(edit);
        self.rev.resize(self.sources.all_docs().len(), self.first_rev);
        for e in &edit.edits {
            if let Some(rev) = self.rev.get_mut(e.doc.0 as usize) {
                *rev += 1;
            }
        }
        self.refresh_view();
    }

    /// Where `sel` is written, as a `Sel::Source`.
    pub fn site(&self, sel: &Sel) -> Option<Sel> {
        self.map.locate(sel).map(|r| Sel::Source { doc: r.doc, start: r.span.start, end: r.span.end })
    }

    /// The source range a refusal is about: the diag's own `Sel::Source`, else where its first
    /// located selection is written.
    pub fn site_of(&self, diag: &Diag) -> Option<Sel> {
        diag.sel.iter().find(|s| matches!(s, Sel::Source { .. })).cloned()
            .or_else(|| diag.sel.iter().find_map(|s| self.site(s)))
    }

    /// Rebuild the shared view; the `sim` statement is read again first when the system text
    /// changed. Cost: O(documents + units + notes), plus O(system text) on its change.
    fn refresh_view(&mut self) {
        let system = self.sources.docs().iter().copied().find(|d| d.kind == DocKind::System);
        let same = match (system, &self.sim_text) {
            (Some(doc), Some((id, text))) => doc.id == *id && Arc::ptr_eq(&doc.text, text),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.sim_text = system.map(|doc| (doc.id, doc.text.clone()));
            self.sim = system.map_or_else(SimEval::default, |doc| sys_sim::read(doc.id, &doc.text));
        }
        self.view = self.make_view();
    }

    /// The system pass's diags (a dangling link end) join the notes, replaced at every
    /// evaluation (hook 5), and the `sim` statement's with them, replaced at every change of its
    /// text. Cost: O(documents + units + notes).
    fn make_view(&self) -> Arc<DocsView> {
        Arc::new(DocsView {
            texts: self.sources.docs().iter().map(|d| DocText {
                doc: d.id, name: d.name.clone(), text: d.text.clone(), rev: self.rev[d.id.ix()],
            }).collect(),
            units: self.units.iter().map(|u| (u.id, u.name.clone(), u.state.clone())).collect(),
            notes: self.notes.iter().chain(&self.system.diags).chain(&self.sim.diags).chain(&self.mismatch).cloned().collect(),
            sim: self.sim.settings.clone(),
            system: self.sim_text.as_ref().map(|(doc, _)| *doc),
        })
    }

    /// The text of a document, by id.
    pub fn text(&self, doc: DocId) -> Option<&Arc<str>> {
        self.sources.doc(doc).map(|d| &d.text)
    }
}
