//! An explorer no host feeds follows the electronics host (`ElecLive`), as the board pane does,
//! and samples the followed unit's schematic item states (selection, findings, emphasis) from the
//! host. A host that calls `set_design` pins the view and feeds it itself. The unpinned sheet
//! follows the live bench's unit when it has a sheet; a design with no unit with parts (a plate
//! with no circuit) empties it, as the board pane then shows no board. A unit is the one shown
//! before by its id and its name: ids count from 0 in every plate's evaluation, so another
//! plate's first unit has the id of this one's. A unit that is not the one shown before is shown
//! fresh, its camera fitted to its sheet; the one shown before keeps the person's camera through
//! its edits.
//!
//! The view shows the live design with the rows the host published for it, including a circuit
//! preview's own index (`StateOf::Preview`). While an edit this view committed is landing it
//! applies nothing, so the sheet never flashes back to the committed layout during evaluation.
//! Only a new circuit or rows needs a layout; a board edit swaps the design in place. A
//! proposal borrows the camera and uses its own published rows and a separate layout slot.
//!
//! Cost: per signal one global read and `Arc` compares; per new circuit or rows one row-index
//! clone (O(rows)) and the layout request a feeding host would make; nothing more per board
//! sample. Per draw one scan of the item-state owners (O(owners)); per action batch one scan for
//! focus actions (O(actions)).

use super::ExplorerView;
use crate::{ElecLive, SheetIndex, StatesOf, ViewAction};
use crate::snapshot::{DesignSnapshot, ProposalView, Tagged};
use fab_elec_circuit::Circuit;
use fab_elec_core::BoardId;
use fab_elec_design::{Design, Unit};
use makepad_widgets_core::*;
use std::sync::Arc;

/// The followed unit (its id and name), and the circuit and sheet rows its sheet was laid out from.
pub(super) struct Followed {
    unit: BoardId,
    name: String,
    circuit: Arc<Circuit>,
    rows: Arc<SheetIndex>,
}

impl Followed {
    pub(super) fn matches(&self, s: &SheetSource) -> bool {
        let u = &s.design.units[s.unit];
        self.is(u) && step(self, &u.circuit, &s.rows) == Step::Swap
    }

    /// Whether `unit` is the unit followed: the same id and name (another plate's unit may have
    /// this one's id).
    fn is(&self, unit: &Unit) -> bool {
        self.unit == unit.id && self.name == unit.name
    }
}

/// What a newer design asks of a followed sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    /// The unit's circuit and rows are the ones laid out: take the design, keep the sheet.
    Swap,
    /// Lay the sheet out again (`update_design`).
    Relayout,
}

/// Swap when the unit's circuit and rows are the laid-out ones (by pointer), else lay out again.
fn step(shown: &Followed, circuit: &Arc<Circuit>, rows: &Arc<SheetIndex>) -> Step {
    if Arc::ptr_eq(&shown.circuit, circuit) && Arc::ptr_eq(&shown.rows, rows) { Step::Swap } else { Step::Relayout }
}

/// Pair live geometry with its published rows from one snapshot, waiting until both exist.
fn shown<'a>(snap: &'a DesignSnapshot, id: BoardId) -> Option<(&'a Arc<Design>, &'a Tagged<Arc<SheetIndex>>)> {
    let design = snap.live();
    design.unit(id)?;
    Some((design, snap.of(&snap.sheets, id)?))
}

/// The design, unit position in that design, and matching stable rows to display.
pub(super) struct SheetSource {
    pub design: Arc<Design>,
    pub unit: usize,
    pub rows: Arc<SheetIndex>,
}

impl SheetSource {
    pub(super) fn same_sheet(&self, other: &Self) -> bool {
        let (a, b) = (&self.design.units[self.unit], &other.design.units[other.unit]);
        a.id == b.id && Arc::ptr_eq(&a.circuit, &b.circuit) && Arc::ptr_eq(&self.rows, &other.rows)
    }
}

/// A held picture keeps the displayed unit by id, including its absence. Only an
/// unassigned pane chooses a first sheet. Ordinary following keeps the live bench rule.
pub(super) fn source(host: &ElecLive, proposal: Option<&ProposalView>, previous: Option<(BoardId, &str)>) -> Option<SheetSource> {
    if let Some(view) = proposal {
        let design = view.design.as_ref()?;
        let unit = match previous {
            Some((id, _)) => design.units.iter().position(|u| u.id == id)?,
            None => design.units.iter().position(|u| view.sheets.contains_key(&u.id))?,
        };
        return Some(SheetSource { design: design.clone(), rows: view.sheets.get(&design.units[unit].id)?.clone(), unit });
    }
    let unit = host.sheet_unit(previous)?;
    let (design, sheet) = shown(&host.snap, host.snap.live().units[unit].id)?;
    Some(SheetSource { design: design.clone(), unit, rows: sheet.value.clone() })
}

impl ExplorerView {
    /// Show the host's newest design when it is not the one shown: in place for a board edit,
    /// laid out again for a new circuit or rows.
    pub(super) fn follow(&mut self, cx: &mut Cx) {
        if self.pinned || !cx.has_global::<ElecLive>() { return; }
        let host = cx.get_global::<ElecLive>();
        let previous = self.followed.as_ref().map(|f| (f.unit, f.name.as_str()));
        if let Some(view) = host.previewed() {
            if self.review.as_ref().is_some_and(|r| Arc::ptr_eq(&r.view, view)) { return; }
            let displayed = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)).map(|u| (u.id, u.name.as_str()));
            let candidate = source(host, Some(view), displayed);
            let view = view.clone();
            self.follow_comparison(cx, view, candidate, displayed.is_some());
            return;
        }
        let next = source(host, None, previous);
        let no_unit = next.is_none() && host.sheet_unit(previous).is_none();
        let returning = self.review.is_some() || self.return_picture.is_some();
        if self.review.is_none() {
            self.follow_live(cx, next, no_unit, returning, None);
            return;
        }

        // Finish live work against the live source. A newer snapshot or an adopted sheet
        // advances gen before deferred results are considered, so an older result cannot land.
        let deferred = std::mem::take(&mut self.review.as_mut().unwrap().deferred);
        let completing_live = self.layout_jobs.busy();
        self.wire_snapshot(cx);
        self.keep_snapshot(cx);
        let prepared = self.leave_comparison(cx, next.as_ref());
        self.follow_live(cx, next, no_unit, returning, prepared);
        for result in deferred { self.layout_result(cx, result); }
        if completing_live {
            if let Some(result) = self.layout_jobs.poll(cx) { self.layout_result(cx, result); }
        }
        self.pane.nav.stop();
        if self.assemble_pending { self.request_frame(cx); }
    }

    fn follow_live(&mut self, cx: &mut Cx, next: Option<SheetSource>, no_unit: bool, returning: bool, prepared: Option<super::Laid>) {
        if self.holding() { return; }
        let Some(SheetSource { design, unit, rows }) = next else {
            if no_unit {
                self.return_picture = None;
                if self.design.is_some() { self.unload(cx); }
            }
            return;
        };
        let id = design.units[unit].id;
        if self.unit == unit && self.design.as_ref().is_some_and(|shown| Arc::ptr_eq(shown, &design))
            && self.followed.as_ref().is_some_and(|f| Arc::ptr_eq(&f.rows, &rows)) {
            return;
        }
        let circuit = design.units[unit].circuit.clone();
        let name = design.units[unit].name.clone();
        let same_unit = self.design.is_some() && self.unit == unit && self.followed.as_ref().is_some_and(|f| f.is(&design.units[unit]));
        match self.followed.as_ref().filter(|_| same_unit).map(|shown| step(shown, &circuit, &rows)) {
            Some(Step::Swap) => {
                self.design = Some(design.clone());
                self.draw_bg.redraw(cx);
            }
            Some(Step::Relayout) => {
                self.followed = Some(Followed { unit: id, name, circuit, rows: rows.clone() });
                self.change_design(cx, design, rows.rows.clone(), returning, prepared);
            }
            None => {
                self.followed = Some(Followed { unit: id, name, circuit, rows: rows.clone() });
                self.load(cx, design, unit, rows.rows.clone(), returning, prepared);
            }
        }
    }

    /// The host's schematic item states of the followed unit, while following.
    pub(super) fn followed_items(&self, cx: &Cx) -> Option<Texture> {
        if self.pinned {
            return None;
        }
        let unit = self.followed.as_ref()?.unit;
        cx.get_global_ref::<ElecLive>()?.item_states(StatesOf::Schematic(unit)).cloned()
    }

    /// A focus from any view moves the host's item states: draw again to show them.
    pub(super) fn follow_focus(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.pinned {
            return;
        }
        let focus = actions.iter().any(|action| {
            let action = action.as_widget_action().and_then(|widget| widget.action.downcast_ref::<ViewAction>())
                .or_else(|| action.downcast_ref::<ViewAction>());
            matches!(action, Some(ViewAction::Focus(_)))
        });
        if focus {
            self.draw_bg.redraw(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::gesture::tests::snapshot;
    use crate::snapshot::StateOf;
    use fab_elec_explore::rows::SheetRows;

    /// drum16 has a unit per voice and a bench per voice: a fresh explorer shows the unit the
    /// first bench that built runs, whichever unit comes first in the design.
    #[test]
    fn a_fresh_explorer_shows_the_unit_the_live_bench_runs() {
        let sources = fab_elec_fixtures::docs("drum16").expect("follow: drum16 is a stock project");
        let ev = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
            .unwrap_or_else(|diags| panic!("follow: drum16 evaluates: {diags:?}"));
        let unit_of = |name: &str| {
            let bench = ev.benches.iter().find(|bench| bench.name == name).expect("follow: drum16 has the bench");
            ev.design.units.iter().position(|unit| unit.id == bench.scope_id())
        };
        assert!(unit_of("kick").is_some() && unit_of("kick") != unit_of("toms"), "follow: the kick and the toms are units of their own");
        let mut snap = snapshot(Arc::new(ev.design.clone()));
        snap.benches = Arc::new(fab_elec_lang::bench_views(&ev, &sources));
        let mut live = ElecLive::new(Arc::new(snap));
        assert_eq!(live.sheet_unit(None), unit_of(&ev.benches[0].name),
            "follow: the first bench that built picks the unit");
        let last = &ev.benches.last().unwrap().name;
        live.chosen_bench = Some(Arc::from(last.as_str()));
        assert_eq!(live.sheet_unit(None), unit_of(last), "follow: the chosen bench picks its own unit");
        let rows = Arc::make_mut(&mut Arc::make_mut(&mut live.snap).benches);
        let mut failed = rows.pop().unwrap();
        assert_eq!(live.run_bench().unwrap().out.name, ev.benches[0].name, "a removed choice falls back to the first usable bench");
        failed.out.state = fab_elec_lang::UnitState::Failed(Vec::new());
        Arc::make_mut(&mut Arc::make_mut(&mut live.snap).benches).push(failed);
        assert!(live.run_bench().is_none(), "a present choice that failed does not run a different bench");
        let first_with_parts = ev.design.units.iter().position(|unit| !unit.circuit.comps.is_empty());
        Arc::make_mut(&mut live.snap).benches = Arc::default();
        assert_eq!(live.sheet_unit(None), first_with_parts, "follow: without a bench, the first unit with parts");
    }

    #[test]
    fn a_board_edit_swaps_and_a_circuit_or_rows_change_lays_out_again() {
        let design = Arc::new(fab_elec_fixtures::ledctl());
        let unit = &design.units[0];
        let mut sheet_rows = SheetRows::new();
        let rows = Arc::new(SheetIndex { rows: sheet_rows.sync(&unit.circuit), count: sheet_rows.count() });
        let followed = Followed { unit: unit.id, name: unit.name.clone(), circuit: unit.circuit.clone(), rows: rows.clone() };
        assert!(followed.is(unit), "follow: the unit shown is the one followed");
        let mut other_plate = unit.clone();
        other_plate.name.push_str(" of another plate");
        assert!(!followed.is(&other_plate),
            "follow: another plate's unit with the followed id is shown fresh, its camera fitted");

        let mut board_edit = (*design).clone();
        let board = board_edit.units[0].board.as_mut().expect("follow: the fixture's unit has a board");
        Arc::make_mut(board).generation += 1;
        let board_edit = Arc::new(board_edit);
        assert_eq!(step(&followed, &board_edit.units[0].circuit, &rows), Step::Swap,
            "follow: a board edit (same circuit, same rows) swaps the design without a layout");

        let mut circuit_edit = (*design).clone();
        Arc::make_mut(&mut circuit_edit.units[0].circuit).hints.parts.clear();
        assert_eq!(step(&followed, &circuit_edit.units[0].circuit, &rows), Step::Relayout,
            "follow: a new circuit lays the sheet out again");
        let republished = Arc::new((*rows).clone());
        assert_eq!(step(&followed, &unit.circuit, &republished), Step::Relayout,
            "follow: new rows lay the sheet out again");

        let preview = Arc::new(circuit_edit);
        let mut preview_sheet_rows = sheet_rows.clone();
        let preview_rows = Arc::new(SheetIndex {
            rows: preview_sheet_rows.sync(&preview.units[0].circuit), count: preview_sheet_rows.count(),
        });
        let mut snap = snapshot(design.clone());
        assert!(shown(&snap, unit.id).is_none(), "follow: no published rows means wait for the host");
        snap.preview = Some(preview.clone());
        snap.sheets = vec![
            Tagged { unit: Some(unit.id), of: StateOf::Committed, generation: 0, value: rows.clone() },
            Tagged { unit: Some(unit.id), of: StateOf::Preview, generation: 1, value: preview_rows.clone() },
        ];
        let (live, index) = shown(&snap, unit.id).expect("follow: the circuit preview has published rows");
        assert!(Arc::ptr_eq(live, &preview), "follow: a circuit preview shows the live design");
        assert!(index.of == StateOf::Preview && Arc::ptr_eq(&index.value, &preview_rows),
            "follow: a circuit preview uses its own published index");

        snap.preview = None;
        let (live, index) = shown(&snap, unit.id).expect("follow: the committed design has published rows");
        assert!(Arc::ptr_eq(live, &design), "follow: without a preview the committed design is shown");
        assert!(index.of == StateOf::Committed && Arc::ptr_eq(&index.value, &rows),
            "follow: without a preview the committed index is shown");

        snap.preview = Some(board_edit.clone());
        snap.sheets.retain(|entry| entry.of == StateOf::Committed);
        let (live, index) = shown(&snap, unit.id).expect("follow: a board preview shares the committed rows");
        assert!(Arc::ptr_eq(live, &board_edit), "follow: a board-only preview shows the live design");
        assert!(Arc::ptr_eq(&index.value, &rows), "follow: a board-only preview uses the committed index");
    }
}
