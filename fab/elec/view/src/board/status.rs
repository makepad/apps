//! The Board status row. The pad count visits the placements once per drawn board. The
//! connectivity worker already lists the unrouted nets, including when its result arrives
//! after the picture. The row writes its controls only when their contents change.

use crate::snapshot::{DesignSnapshot, ElecLive, StateOf};
use fab_elec_board::{Board, Connectivity};
use fab_elec_core::{mm, BoardId, ClassId, LayerId, Nm};
use makepad_widgets_core::*;
use makepad_widgets_pickers::combo_box::ComboBoxWidgetRefExt;
use std::sync::{Arc, Weak};

pub(super) const BAR_PARTS: &[&[LiveId]] = &[ids!(bar)];

/// A decimal measurement in mm, optionally suffixed with `mm`, within the inclusive limits.
pub fn parse_mm(text: &str, lo: f64, hi: f64) -> Option<f64> {
    let text = text.trim();
    let number = text.strip_suffix("mm").unwrap_or(text).trim_end();
    if !number.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let mm = number.parse::<f64>().ok()?;
    (mm.is_finite() && (lo..=hi).contains(&mm)).then_some(mm)
}

/// A field's committed measurement, in mm without trailing fractional zeros.
pub fn mm_text(mm: f64) -> String {
    format!("{mm} mm")
}

pub(super) enum BarPress {
    Layer(LayerId),
    Track(Nm),
    Via(Nm),
    Grid(f64),
}

struct Field {
    id: &'static [LiveId],
    presets: &'static [f64],
    range: (f64, f64),
    hint: &'static str,
    press: fn(f64) -> BarPress,
}

const FIELDS: [Field; 3] = [
    Field { id: ids!(s_track), presets: &[0.15, 0.25, 0.5, 1.0], range: (0.05, 10.0),
        hint: "Type a width from 0.05 to 10 mm", press: |width| BarPress::Track(mm(width)) },
    Field { id: ids!(s_via), presets: &[0.45, 0.6, 0.8], range: (0.2, 5.0),
        hint: "Type a diameter from 0.2 to 5 mm", press: |diameter| BarPress::Via(mm(diameter)) },
    Field { id: ids!(s_grid), presets: &[0.05, 0.1, 0.25, 0.5, 1.27, 2.54], range: (0.01, 10.0),
        hint: "Type a grid from 0.01 to 10 mm", press: BarPress::Grid },
];

/// Track width and via pad diameter for the default net class, as resolved by the host.
fn rule_sizes(snap: Option<&DesignSnapshot>, unit: BoardId) -> (Nm, Nm) {
    let rules = snap.and_then(|s| s.of(&s.rules, unit));
    rules.map_or((250_000, 600_000), |r| (r.value.track_min(ClassId(0)), r.value.via(ClassId(0)).pad))
}

/// The worker's count for this picture. A preview and the committed board can share a
/// generation, so match the drawn board to its design before taking the connectivity.
fn unrouted(snap: Option<&DesignSnapshot>, board: &Arc<Board>, supplied: Option<&Connectivity>) -> Option<usize> {
    if let Some(snap) = snap {
        for (design, of) in [(snap.preview.as_ref(), StateOf::Preview), (Some(&snap.design), StateOf::Committed)] {
            let candidate = design.and_then(|d| d.unit(board.unit)).and_then(|u| u.board.as_ref());
            if candidate.is_some_and(|b| Arc::ptr_eq(b, board)) {
                if let Some(conn) = snap.conn.iter().find(|c| c.unit == Some(board.unit)
                    && c.of == of && c.value.generation == board.generation) {
                    return Some(conn.value.unrouted.len());
                }
            }
        }
        return None;
    }
    supplied.filter(|c| c.generation == board.generation).map(|c| c.unrouted.len())
}

#[derive(Default)]
pub(super) struct BoardStatus {
    // Cached worker results. Weak pointers distinguish replacements without keeping old
    // designs or connectivity alive. An ordinary animation frame only compares identities.
    source_board: Option<(BoardId, usize)>,
    snapshot: Option<Weak<DesignSnapshot>>,
    supplied: Option<Weak<Connectivity>>,
    pub(super) default_track: Nm,
    default_via: Nm,
    open: Option<usize>,
    /// The unit waits for its whole-board checks: the row says so in place of the unrouted count.
    checking: bool,
    board: Option<(usize, u64)>,
    counts: [usize; 4],
    shown_counts: Option<([usize; 4], Option<usize>, bool, Option<String>)>,
    layers: Vec<LayerId>,
    active: Option<LayerId>,
    /// The swatch's ink as written, and the style reload it was written after (a reload puts
    /// the swatch's own colour back).
    ink: Option<([f32; 4], u64)>,
    shown_mm: [Option<f64>; 3],
}

impl BoardStatus {
    /// Takes new rules and connectivity when either publication or its board changes.
    /// Returns whether an arriving worker result changes the row while the canvas is idle.
    pub(super) fn sync_sources(&mut self, cx: &Cx, board: &Arc<Board>, supplied: Option<&Arc<Connectivity>>) -> bool {
        let snap = cx.get_global_ref::<ElecLive>().map(|live| &live.snap);
        let key = (board.unit, Arc::as_ptr(board) as usize);
        if self.source_board == Some(key) && self.snapshot.as_ref().map(Weak::as_ptr) == snap.map(Arc::as_ptr)
            && self.supplied.as_ref().map(Weak::as_ptr) == supplied.map(Arc::as_ptr) {
            return false;
        }
        let (track, via) = rule_sizes(snap.map(Arc::as_ref), board.unit);
        let checking = snap.is_some_and(|snap| snap.checking(board.unit));
        let open = if checking { None } else { unrouted(snap.map(Arc::as_ref), board, supplied.map(Arc::as_ref)) };
        let changed = (self.default_track, self.default_via, self.open, self.checking) != (track, via, open, checking);
        self.source_board = Some(key);
        self.snapshot = snap.map(Arc::downgrade);
        self.supplied = supplied.map(Arc::downgrade);
        self.default_track = track;
        self.default_via = via;
        self.open = open;
        self.checking = checking;
        changed
    }

    /// A placement can ask before the first draw or between a rule publication and its draw.
    pub(super) fn default_via(&self, cx: &Cx, unit: BoardId) -> Nm {
        let snap = cx.get_global_ref::<ElecLive>().map(|live| &live.snap);
        if self.source_board.is_some_and(|(u, _)| u == unit) && self.snapshot.as_ref().map(Weak::as_ptr) == snap.map(Arc::as_ptr) {
            self.default_via
        } else {
            rule_sizes(snap.map(Arc::as_ref), unit).1
        }
    }

    /// Selections and validated submissions in the row. A typed commit also needs its
    /// canonical text when the person re-enters the current value in another spelling.
    pub(super) fn bar_presses(&mut self, cx: &mut Cx, status: &WidgetRef, actions: &Actions) -> Vec<BarPress> {
        let mut presses = Vec::new();
        if actions.is_empty() {
            return presses;
        }
        let layer = status.widget(cx, ids!(s_layer)).as_combo_box();
        if let Some(layer) = layer.selected(actions).and_then(|i| self.layers.get(i)) {
            presses.push(BarPress::Layer(*layer));
        }
        for (field, shown) in FIELDS.iter().zip(&mut self.shown_mm) {
            let combo = status.widget(cx, field.id).as_combo_box();
            if let Some(value) = combo.selected(actions).and_then(|i| field.presets.get(i)) {
                presses.push((field.press)(*value));
                *shown = Some(*value);
            } else if let Some(text) = combo.typed(actions) {
                if let Some(value) = parse_mm(&text, field.range.0, field.range.1) {
                    presses.push((field.press)(value));
                    *shown = None;
                } else {
                    combo.set_error(cx, Some(field.hint));
                }
            }
        }
        presses
    }

    /// Updates the row from the drawn board and the three effective dimensions in mm.
    /// An unchanged frame compares cached values without visiting placements or widgets.
    /// `hover`: the hovered copper's net and its voltage at the play head, ahead of the counts.
    pub(super) fn sync_status(&mut self, cx: &mut Cx, status: &WidgetRef, board: &Arc<Board>,
        active: LayerId, ink: [f32; 4], sizes: [f64; 3], hover: Option<String>) {
        let key = (Arc::as_ptr(board) as usize, board.generation);
        let changed = self.board != Some(key);
        if self.board.is_none() {
            for field in &FIELDS {
                status.widget(cx, field.id).as_combo_box().set_labels(cx, field.presets.iter().copied().map(mm_text).collect());
            }
        }
        if changed {
            self.board = Some(key);
            let pads = board.placed.iter().map(|p| p.footprint.pads.len()).sum();
            self.counts = [pads, board.vias.len(), board.tracks.len(), board.nets.len()];
        }
        let open = self.open;
        let shown = Some((self.counts, open, self.checking, hover));
        if self.shown_counts != shown {
            let [pads, vias, tracks, nets] = self.counts;
            let mut text = shown.as_ref().and_then(|s| s.3.as_ref()).map_or_else(String::new, |hover| format!("{hover}   ·   "));
            text.push_str(&format!("Pads {pads}   Vias {vias}   Tracks {tracks}   Nets {nets}"));
            if self.checking {
                text.push_str("   Checking…");
            } else if let Some(n) = open {
                text.push_str(&format!("   Unrouted {n}"));
            }
            status.label(cx, ids!(s_counts)).set_text(cx, &text);
            self.shown_counts = shown;
        }
        if changed || self.active != Some(active) {
            let mut layers = board.stackup.copper();
            // A closed-set combo can only show a member. If the rail activated a
            // documentation layer, include just that current row after the copper choices.
            let index = layers.iter().position(|&l| l == active).unwrap_or_else(|| {
                layers.push(active);
                layers.len() - 1
            });
            let field = status.widget(cx, ids!(s_layer)).as_combo_box();
            if self.layers != layers {
                field.set_labels(cx, layers.iter().map(|&l| layer_label(l)).collect());
                self.layers = layers;
            }
            if field.selected_item() != index {
                field.set_selected_item(cx, index);
            }
            self.active = Some(active);
        }
        let ink = (ink, cx.reload_generation);
        if self.ink != Some(ink) {
            if let Some(mut swatch) = status.view(cx, ids!(s_swatch)).borrow_mut() {
                swatch.draw_bg.set_dyn_instance(cx, id!(color), &ink.0);
            }
            self.ink = Some(ink);
        }
        for ((field, shown), mm) in FIELDS.iter().zip(&mut self.shown_mm).zip(sizes) {
            if *shown != Some(mm) {
                status.widget(cx, field.id).as_combo_box().set_text(cx, &mm_text(mm));
                *shown = Some(mm);
            }
        }
    }
}


/// A layer's plain name in the Layer field: the copper by where it sits (Top, Bottom, Inner n),
/// any other layer by its conventional short name.
fn layer_label(layer: LayerId) -> String {
    match layer {
        LayerId::F_CU => "Top copper".into(),
        LayerId::B_CU => "Bottom copper".into(),
        _ => match layer.inner_index() {
            Some(n) => format!("Inner {n}"),
            None => layer.name(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{mm_text, parse_mm};

    #[test]
    fn millimetre_fields_parse_and_display() {
        for (input, value) in [("0.3", 0.3), (" 0.3 mm ", 0.3), ("0.3mm", 0.3), (".5", 0.5), ("0.05", 0.05), ("10", 10.0)] {
            assert_eq!(parse_mm(input, 0.05, 10.0), Some(value), "accepted measurement {input:?}");
        }
        for input in ["abc", "0", "11", "-1", "1e3", "1e0", "NaN", "inf", "1.2.3", "mm"] {
            assert_eq!(parse_mm(input, 0.05, 10.0), None, "invalid measurement {input:?}");
        }
        for (value, label) in [(0.25, "0.25 mm"), (0.1, "0.1 mm"), (1.27, "1.27 mm"), (2.0, "2 mm")] {
            assert_eq!(mm_text(value), label, "displayed measurement for {value}");
        }
    }
}
