//! The board pane's route line, top centre. With no job: how many nets are unrouted, with Route
//! all and, when the selection holds unrouted nets, Route selected. While a route job runs on the
//! board: the net it routes and its progress, with Stop (keep what is routed) and Discard (drop
//! it). Once the job ends the line counts the board's unrouted nets again, so a job leaves no word
//! of its own (no success toasts) and the count never goes stale. A finished job that left nets
//! of its scope open names them under the count until the board next changes; one net it names
//! with why it stays open and what to try, and offers Re-route: those nets routed again, the
//! board's routed copper in their way free to move (each net moved is joined again, or the job
//! keeps it as it was). An organic job's line says so from its first moment
//! (its grid pass streams straight copper before the relax curves it). A running job's line puts its
//! buttons first and keeps the left edge of its first draw, so the changing words never move Stop
//! or Discard under the pointer; an idle line's buttons follow its words, Re-route right after
//! Route all, and Route selected, which comes and goes with the selection, last. Its words are
//! rebuilt only when its key changes; a draw is one plate, up to three buttons and their labels.
//!
//! The pane hit-tests the buttons and the plate before its canvas, so a press on the line never
//! pans the board or starts a marquee.

use super::theme::BoardTheme;
use crate::snapshot::{JobEnd, RouteJobState};
use fab_elec_board::{Board, BoardOp, Connectivity, Item, ItemKind};
use fab_elec_circuit::Measure;
use fab_elec_core::{BlockId, CompId, CompRef, FeatureRef, ItemId, NetId, Sel, P};
use fab_elec_route::{scope_nets, HandCopper, JobId, JobPhase, JobStatus, Reason};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.widgets.BoardRouteLine = set_type_default() do #(RouteLine::script_api(vm)) {
        draw_plate: mod.draw.DrawColor {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0. 0. self.rect_size.x self.rect_size.y 4.)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_first: mod.draw.DrawColor {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0. 0. self.rect_size.x self.rect_size.y 3.)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_second: mod.draw.DrawColor {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0. 0. self.rect_size.x self.rect_size.y 3.)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_third: mod.draw.DrawColor {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0. 0. self.rect_size.x self.rect_size.y 3.)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_label: mod.draw.DrawText {
            text_style: theme.font_regular {font_size: 9}
        }
        draw_note: mod.draw.DrawText {
            text_style: theme.font_regular {font_size: 8.5}
        }
    }
}

/// A button on the line.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LineButton {
    /// Route every unrouted net of the board.
    RouteAll,
    /// Route the selection's unrouted nets.
    RouteSelected,
    /// Route the nets the last route left open again, free to move the board's routed copper in
    /// their way (`elec.route reroute=true`).
    Reroute,
    /// Stop the running job and keep what it has routed, as one undo step.
    Stop,
    /// Stop the running job and drop its preview; nothing is committed.
    Discard,
}

/// What the line shows; the pane builds it, and the line rebuilds its words when it changes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LineKey {
    /// No job runs: `unrouted` nets of the board, `selected` of them in the selection, and the
    /// finished job whose open nets the line names (`LeftOpen`), while the board is as it left it.
    Idle { unrouted: usize, selected: usize, left: Option<JobId> },
    /// A route job running on the board, `organic` for the organic method, and the hand-drawn
    /// copper its shown proposal takes off. Compared by value, so every status the worker
    /// publishes rebuilds the words.
    Job { status: JobStatus, organic: bool, hand: HandLine },
}

/// The hand-drawn tracks and vias a running place job's proposal takes off, from how many parts,
/// and the part when it is one: the line says so while Discard can still keep them.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct HandLine {
    pub tracks: u32,
    pub vias: u32,
    pub parts: u32,
    pub part: Option<CompId>,
}

impl HandLine {
    /// From a place proposal's metrics.
    pub fn of(hand: &[HandCopper]) -> HandLine {
        HandLine {
            tracks: hand.iter().map(|h| h.tracks).sum(),
            vias: hand.iter().map(|h| h.vias).sum(),
            parts: hand.len() as u32,
            part: hand.first().map(|h| h.comp),
        }
    }

    /// "Removes 3 hand-drawn tracks from U1; Discard keeps them", or None when it takes none off.
    fn words(self, part_of: impl Fn(CompId) -> Option<String>) -> Option<String> {
        if self.tracks + self.vias == 0 {
            return None;
        }
        let count = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let what = match (self.tracks, self.vias) {
            (t, 0) => format!("{} hand-drawn {}", t, if t == 1 { "track" } else { "tracks" }),
            (0, v) => format!("{} hand-drawn {}", v, if v == 1 { "via" } else { "vias" }),
            (t, v) => format!("{} and {}", count(t, "hand-drawn track", "hand-drawn tracks"), count(v, "via", "vias")),
        };
        let from = match self.part.and_then(part_of) {
            Some(name) if self.parts == 1 => name,
            _ => count(self.parts, "part", "parts"),
        };
        let them = if self.tracks + self.vias == 1 { "it" } else { "them" };
        Some(format!("Removes {what} from {from}; Discard keeps {them}"))
    }
}

/// The line's words and buttons (at most three).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LineText {
    pub line: String,
    /// A dimmer second line; empty for none.
    pub note: String,
    pub buttons: Vec<(LineButton, &'static str)>,
}

/// The nets of a finished job's scope that were still open on the board its commit made, at that
/// board's generation: the line names them until the board changes, so they never go stale.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LeftOpen {
    pub job: JobId,
    pub generation: u64,
    pub nets: Vec<NetId>,
    /// Why each of `nets` the job gave a reason for stays open, in words (`why_open`).
    pub why: Vec<(NetId, String)>,
}

/// What `job` left open, once it has finished and committed: no preview shown and connectivity
/// computed from the committed `board`. None while that is not so yet, and for a stopped job (the
/// person chose to end it; the count says the rest).
pub fn left_open(job: &RouteJobState, preview_shown: bool, board: &Board, conn: &Connectivity) -> Option<LeftOpen> {
    if job.end != Some(JobEnd::Finished) || preview_shown || conn.generation != board.generation {
        return None;
    }
    let nets = scope_nets(board, conn, &job.scope);
    let why = nets.iter()
        .filter_map(|net| job.open.iter().find(|(open, _)| open == net).map(|(_, reason)| (*net, why_open(board, reason))))
        .collect();
    Some(LeftOpen { job: job.status.job, generation: board.generation, nets, why })
}

/// Why a route left a net open and what to try, in words for the line: "no path. <cause>; try
/// <what>", naming parts by reference and pad, nets by name. O(parts) per named part.
pub fn why_open(board: &Board, reason: &Reason) -> String {
    let mm = |w: i32| format!("{} mm", (f64::from(w) / 1e4).round() / 100.0);
    let net = |n: &NetId| board.nets.get(n.ix()).map_or_else(|| format!("net {}", n.0), |x| x.name.clone());
    match reason {
        Reason::NoRoom { layer, need, have, between, by, .. } => {
            let track = match need {
                Measure::Len(w) => format!("its {} track", mm(*w)),
                Measure::Qty(_) | Measure::VRange(_) => "its track".to_owned(),
            };
            let (a, b) = (sel_words(board, &between[0]), sel_words(board, &between[1]));
            if !by.is_empty() {
                const NAMED: usize = 3;
                let names: Vec<String> = by.iter().take(NAMED).map(|sel| sel_words(board, sel)).collect();
                let more = by.len().saturating_sub(NAMED);
                let more = if more > 0 { format!(" and {more} more") } else { String::new() };
                return format!("no path for {track} from {a} to {b}: kept copper in the way on {}: {}{more}; try Re-route, which may move it, or moving a part",
                    layer.name(), names.join(", "));
            }
            match have {
                Measure::Len(free) if *free > 0 => {
                    format!("no path. No room for {track} between {a} and {b} ({} free); try moving one of them", mm(*free))
                }
                _ => format!("no path for {track} from {a} to {b}; try clearing the routes in its way and routing again, or moving a part"),
            }
        }
        Reason::Blocked { by, .. } => {
            const NAMED: usize = 3;
            let names: Vec<String> = by.iter().take(NAMED).map(net).collect();
            let more = by.len().saturating_sub(NAMED);
            let more = if more > 0 { format!(" and {more} more") } else { String::new() };
            format!("no path. Blocked by {}{more}; try clearing their routes and routing again", names.join(", "))
        }
        Reason::Terminal { with: Some(sel), .. } => {
            format!("no path. {} has no way out; try moving its part", sel_words(board, sel))
        }
        Reason::Terminal { with: None, .. } => "no path. A pad has no way out; try moving its part".to_owned(),
        Reason::NotTried => "not tried before the route ended".to_owned(),
        Reason::Unjoined { at, track, host } => {
            let (track, host) = (sel_words(board, track), sel_words(board, host));
            format!("junction opened. The end of {track} at ({}, {}) is no longer on {host}; try reconnecting it or routing the net again",
                mm(at.x), mm(at.y))
        }
        Reason::SharpCorner { .. } => room_words(board, reason),
    }
}

/// The drag's chip names the first cut and opened junction with their counts, then counts the
/// sharp corners kept. Splitting the answer costs O(1 + junctions + corners). Pushes prefix the
/// findings when present. With pushes alone the preview shows them and the chip is `None`.
pub fn cut_words(board: &Board, open: &[(NetId, Reason)], pushed: usize) -> Option<String> {
    let corner_count = open.iter().rev().take_while(|(_, reason)| matches!(reason, Reason::SharpCorner { .. })).count();
    let open = &open[..open.len() - corner_count];
    let junction_count = open.iter().rev().take_while(|(_, reason)| matches!(reason, Reason::Unjoined { .. })).count();
    let (cuts, junctions) = open.split_at(open.len() - junction_count);
    let cut = cuts.first().map(|(net, reason)| {
        let (net, why) = (net_name(board, *net), room_words(board, reason));
        match cuts.len() {
            1 => format!("{net} cut back: {why}"),
            n => format!("{n} tracks cut back; {net}: {why}"),
        }
    });
    let junction = junctions.first().map(|(net, reason)| {
        let (net, why) = (net_name(board, *net), room_words(board, reason));
        match junctions.len() {
            1 => format!("{net} junction opened: {why}"),
            n => format!("{n} junctions opened; {net}: {why}"),
        }
    });
    let corner = match corner_count {
        0 => None,
        1 => Some("1 sharp corner kept".to_owned()),
        n => Some(format!("{n} sharp corners kept")),
    };
    let words = [cut, junction, corner].into_iter().flatten().reduce(|mut words, next| {
        words.push_str("; ");
        words.push_str(&next);
        words
    })?;
    Some(match pushed {
        0 => words,
        1 => format!("pushed 1 track; {words}"),
        n => format!("pushed {n} tracks; {words}"),
    })
}

/// Why a dragged segment did not push a track, and walks round it instead
/// (`HeadAnswer::unpushed`): which net's track, and what it needed room past, for its chip. O(1).
pub fn unpushed_words(board: &Board, net: NetId, reason: &Reason) -> String {
    format!("{} track not pushed: {}", net_name(board, net), room_words(board, reason))
}

fn net_name(board: &Board, net: NetId) -> String {
    board.nets.get(net.ix()).map_or_else(|| format!("net {}", net.0), |n| n.name.clone())
}

/// Why a track needed room, a junction opened or a sharp corner was kept, in a few words.
fn room_words(board: &Board, reason: &Reason) -> String {
    match reason {
        Reason::NoRoom { need: Measure::Len(need), between, .. } => {
            format!("{} mm needed past {}", (f64::from(*need) / 1e4).round() / 100.0, sel_words(board, &between[1]))
        }
        Reason::Unjoined { .. } => "a track end no longer meets the track it joined".to_owned(),
        Reason::SharpCorner { .. } => "a corner sharper than a right angle, kept".to_owned(),
        _ => "no room".to_owned(),
    }
}

/// The neighbours `ops` push, in op order: tracks replaced with another path whose base ends are
/// on none of `held`, and vias not in `held` that move (`Move`, or replaced at another place).
/// O(ops) index lookups.
pub fn pushed(board: &Board, ops: &[BoardOp], held: &[ItemId]) -> Vec<(ItemId, ItemKind)> {
    ops.iter().filter_map(|op| match op {
        BoardOp::Replace(Item::Track(track)) => {
            let (ItemKind::Track, i) = board.locate(track.id)? else { return None };
            let base = &board.tracks[i];
            (base.path != track.path && !base.ends.iter().any(|end| end.item().is_some_and(|item| held.contains(&item))))
                .then_some((track.id, ItemKind::Track))
        }
        BoardOp::Replace(Item::Via(via)) => {
            let (ItemKind::Via, i) = board.locate(via.id)? else { return None };
            (board.vias[i].at != via.at && !held.contains(&via.id)).then_some((via.id, ItemKind::Via))
        }
        BoardOp::Move { item, by } => {
            let (ItemKind::Via, _) = board.locate(*item)? else { return None };
            (*by != P::ZERO && !held.contains(item)).then_some((*item, ItemKind::Via))
        }
        _ => None,
    }).collect()
}

/// A selection in words: a pad as "U1 pad 2", a part by reference, board copper by kind and net
/// ("the GND track", "the SDA via", "the GND pour").
fn sel_words(board: &Board, sel: &Sel) -> String {
    let part = |c: &CompRef| board.placed_of(c.comp);
    match sel {
        Sel::Feature(FeatureRef::Pad(c, pad)) => match part(c) {
            Some(p) => match p.footprint.pads.get(usize::from(pad.0)) {
                Some(land) => format!("{} pad {}", p.refdes, land.no),
                None => p.refdes.clone(),
            },
            None => "a pad".to_owned(),
        },
        Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => {
            part(c).map_or_else(|| "a part".to_owned(), |p| p.refdes.clone())
        }
        Sel::Item(_, id) => match board.locate(*id).map(|(kind, _)| kind) {
            Some(ItemKind::Placed) => board.placed_item(*id).map_or_else(|| "a part".to_owned(), |p| p.refdes.clone()),
            Some(ItemKind::Track) => board.tracks.iter().find(|t| t.id == *id).and_then(|t| board.nets.get(t.net.ix()))
                .map_or_else(|| "a track".to_owned(), |net| format!("the {} track", net.name)),
            Some(ItemKind::Via) => board.vias.iter().find(|v| v.id == *id).and_then(|v| board.nets.get(v.net.ix()))
                .map_or_else(|| "a via".to_owned(), |net| format!("the {} via", net.name)),
            Some(ItemKind::Zone) => board.zones.iter().find(|z| z.id == *id).and_then(|z| z.net).and_then(|n| board.nets.get(n.ix()))
                .map_or_else(|| "a pour".to_owned(), |net| format!("the {} pour", net.name)),
            Some(ItemKind::Keepout) => "a keepout".to_owned(),
            Some(ItemKind::Hole) => "a hole".to_owned(),
            _ => "board copper".to_owned(),
        },
        _ => "the board".to_owned(),
    }
}

/// The nets `left` names under the count: those of its job still unrouted in `conn`, the
/// connectivity the count comes from, so the names never disagree with the count (a pour refilled
/// after the commit joins its net). None of them once the committed board's generation has moved
/// past `left`'s. O(left x log unrouted).
pub fn still_open(left: &LeftOpen, generation: u64, conn: &Connectivity) -> Vec<NetId> {
    if left.generation != generation {
        return Vec::new();
    }
    left.nets.iter().copied().filter(|net| conn.unrouted.binary_search(net).is_ok()).collect()
}

/// A group of repeated blocks by its first copy's path, the copy's index dropped: `keys[0]` is
/// `keys`.
fn group_name(path: &str) -> &str {
    path.strip_suffix(']').and_then(|p| p.rsplit_once('['))
        .filter(|(_, ix)| !ix.is_empty() && ix.bytes().all(|b| b.is_ascii_digit()))
        .map_or(path, |(base, _)| base)
}

/// The words for `key`, or None when the line shows nothing. `left` holds the nets the key's
/// finished job left open and `why` its reasons, in words, for those it gave one; `name_of` names
/// a net of the board, `part_of` a part and `block_of` a block instance by its path.
pub fn line_text(
    key: LineKey, left: &[NetId], why: &[(NetId, String)], name_of: impl Fn(NetId) -> Option<String>,
    part_of: impl Fn(CompId) -> Option<String>, block_of: impl Fn(BlockId) -> Option<String>,
) -> Option<LineText> {
    let nets = |n: u32| if n == 1 { "net" } else { "nets" };
    match key {
        LineKey::Job { status: s, organic, hand } => {
            let open = if s.failed > 0 { format!(", {} open", s.failed) } else { String::new() };
            // an organic job's grid pass lays straight copper first: the line says what it makes
            let method = if organic { "Organic: " } else { "" };
            let line = match s.phase {
                // The job's first status comes before the worker has counted its nets.
                JobPhase::Replace { pass } => format!("Placing again with the case's rooms ({pass} of 2)"),
                JobPhase::WaitCase => "Waiting for the case to see the new parts".into(),
                JobPhase::Nets if s.total == 0 => format!("{method}Starting the route"),
                JobPhase::Nets => match s.current.and_then(&name_of) {
                    Some(name) => format!("{method}Routing {name}: {} of {} {}{open}", s.routed, s.total, nets(s.total)),
                    None => format!("{method}Routing: {} of {} {}{open}", s.routed, s.total, nets(s.total)),
                },
                JobPhase::Cleanup => format!("Cleaning up: {} of {} {} routed{open}", s.routed, s.total, nets(s.total)),
                JobPhase::Relax { step } => format!("Relaxing the copper, step {step}: {} of {} {} routed{open}", s.routed, s.total, nets(s.total)),
                JobPhase::Seed { round } => {
                    let parts = if s.total == 1 { "part" } else { "parts" };
                    let room = if s.failed > 0 { format!(", {} without room", s.failed) } else { String::new() };
                    match round {
                        0 => format!("Placing {} {parts}", s.total),
                        _ => format!("Making room for the tracks, round {round}: {} of {} {parts} placed{room}", s.routed, s.total),
                    }
                }
                JobPhase::Compare { seated, plain, of } => {
                    use fab_elec_route::progress::CompareStep;
                    let step = |s| match s {
                        CompareStep::Waiting => "waiting".into(),
                        CompareStep::Round(0) => "initial placement".into(),
                        CompareStep::Round(n) => format!("demand round {n}"),
                        CompareStep::Group(n) => format!("matching group {} of {of}", n + 1),
                        CompareStep::Done => "finished".into(),
                    };
                    format!("Comparing with and without the held groups' seats (with seats: {}, without seats: {})", step(seated), step(plain))
                }
                JobPhase::Match { group, of, first } => match block_of(first) {
                    Some(path) => format!("Matching repeated blocks: {} ({} of {of})", group_name(&path), group + 1),
                    None => format!("Matching repeated blocks ({} of {of})", group + 1),
                },
            };
            let note = match (s.phase, s.blocked_by, s.retry) {
                (JobPhase::Nets, Some(other), _) => {
                    let other = name_of(other).unwrap_or_else(|| "another net".to_owned());
                    format!("It stays open: routing it would cut {other} off")
                }
                (JobPhase::Nets, None, Some(why)) => {
                    let label = why.label();
                    let mut chars = label.chars();
                    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
                }
                (JobPhase::Seed { .. } | JobPhase::Match { .. } | JobPhase::Compare { .. }, ..) => hand.words(part_of).unwrap_or_default(),
                _ => String::new(),
            };
            Some(LineText { line, note, buttons: vec![(LineButton::Stop, "Stop"), (LineButton::Discard, "Discard")] })
        }
        LineKey::Idle { unrouted: 0, .. } => None,
        LineKey::Idle { unrouted, selected, left: job } => {
            let mut buttons = vec![(LineButton::RouteAll, "Route all")];
            if job.is_some() && !left.is_empty() {
                buttons.push((LineButton::Reroute, "Re-route"));
            }
            if selected > 0 {
                buttons.push((LineButton::RouteSelected, "Route selected"));
            }
            let nets = if unrouted == 1 { "net" } else { "nets" };
            let note = match job {
                Some(_) if left.len() == 1 && why.iter().any(|(net, _)| *net == left[0]) => {
                    let name = name_of(left[0]).unwrap_or_else(|| format!("net {}", left[0].0));
                    let why = why.iter().find(|(net, _)| *net == left[0]).map_or("", |(_, why)| why.as_str());
                    format!("{name}: {why}")
                }
                Some(_) if !left.is_empty() => {
                    const NAMED: usize = 3;
                    let names: Vec<String> = left.iter().take(NAMED)
                        .map(|&net| name_of(net).unwrap_or_else(|| format!("net {}", net.0))).collect();
                    let more = left.len().saturating_sub(NAMED);
                    let more = if more > 0 { format!(" and {more} more") } else { String::new() };
                    format!("Still open after the last route: {}{more}", names.join(", "))
                }
                _ => String::new(),
            };
            Some(LineText { line: format!("{unrouted} {nets} unrouted"), note, buttons })
        }
    }
}

/// Where a line's plate `width` wide starts in a pane `pane` wide: centred on its `steady` width
/// (the plate without a button that comes and goes at its end), so that button grows the plate
/// rightwards and moves nothing before it; kept inside the pane.
fn plate_left(pane: f64, width: f64, steady: f64) -> f64 {
    ((pane - steady).max(0.0) * 0.5).min((pane - width).max(0.0))
}

/// The unrouted nets the selection holds: a selected net, or the nets of a selected track, via,
/// zone or part's pads; sorted, each once. `unrouted` is sorted (`Connectivity::unrouted`).
/// O(selected items x their pads x log unrouted).
pub fn selected_unrouted(board: &Board, selected: &[Sel], unrouted: &[NetId]) -> Vec<NetId> {
    let mut nets = Vec::new();
    for sel in selected {
        match sel {
            Sel::Net(net) if net.board == board.unit => nets.push(net.net),
            Sel::Item(unit, id) if *unit == board.unit => match board.locate(*id) {
                Some((ItemKind::Track, i)) => nets.push(board.tracks[i].net),
                Some((ItemKind::Via, i)) => nets.push(board.vias[i].net),
                Some((ItemKind::Zone, i)) => nets.extend(board.zones[i].net),
                Some((ItemKind::Placed, i)) => nets.extend(board.placed[i].pad_nets.iter().flatten()),
                _ => {}
            },
            _ => {}
        }
    }
    nets.sort_unstable();
    nets.dedup();
    nets.retain(|net| unrouted.binary_search(net).is_ok());
    nets
}

/// The route line's draw state and button hits.
#[derive(Script, ScriptHook)]
pub struct RouteLine {
    #[live]
    draw_plate: DrawColor,
    #[live]
    draw_first: DrawColor,
    #[live]
    draw_second: DrawColor,
    #[live]
    draw_third: DrawColor,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_note: DrawText,
    /// The key and the named nets `text` was built from.
    #[rust]
    key: Option<LineKey>,
    #[rust]
    left: Vec<NetId>,
    #[rust]
    text: Option<LineText>,
    /// The hovered button's slot.
    #[rust]
    hover: Option<usize>,
    /// The buttons the last draw put on screen, by slot; None when the line was not drawn.
    #[rust]
    drawn: Option<Vec<LineButton>>,
    /// The running job whose line is shown, and its left edge from the pane's (fixed at its first
    /// draw).
    #[rust]
    anchor: Option<(JobId, f64)>,
}

/// What a pointer event did on the line.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct LineHit {
    pub pressed: Option<LineButton>,
    /// The hovered button changed; the pane redraws.
    pub redraw: bool,
}

impl RouteLine {
    /// The key of the last draw.
    pub fn key(&self) -> Option<LineKey> {
        self.key
    }

    /// Draw the line for `key` at the top centre of `pane`; nothing for None or a key without words.
    /// `left` holds the nets the key's finished job left open; `name_of` names a net of the shown
    /// board, `part_of` a part and `block_of` a block instance.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self, cx: &mut Cx2d, pane: Rect, key: Option<LineKey>, left: &[NetId], why: &[(NetId, String)],
        name_of: impl Fn(NetId) -> Option<String>, part_of: impl Fn(CompId) -> Option<String>,
        block_of: impl Fn(BlockId) -> Option<String>, theme: &BoardTheme,
    ) {
        if self.key != key || self.left != left {
            self.text = key.and_then(|key| line_text(key, left, why, name_of, part_of, block_of));
            self.key = key;
            self.left = left.to_vec();
        }
        self.drawn = None;
        let Some(text) = &self.text else {
            self.hover = None;
            return;
        };
        if pane.size.x <= 0.0 || pane.size.y <= 0.0 {
            return;
        }
        let measure = |draw: &mut DrawText, cx: &mut Cx2d, s: &str| {
            let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), s);
            dvec2(
                f64::from(laid.size_in_lpxs.width * draw.font_scale),
                f64::from(laid.size_in_lpxs.height * draw.font_scale),
            )
        };
        let pad = dvec2(10.0, 7.0);
        let gap = 8.0;
        let button_pad = dvec2(8.0, 3.0);
        let line = measure(&mut self.draw_label, cx, &text.line);
        let note = if text.note.is_empty() { Vec2d::default() } else { measure(&mut self.draw_note, cx, &text.note) };
        let sizes: Vec<Vec2d> = text.buttons.iter().map(|(_, label)| measure(&mut self.draw_label, cx, label) + button_pad * 2.0).collect();
        let buttons_w: f64 = sizes.iter().map(|s| s.x + 4.0).sum::<f64>() + if sizes.is_empty() { 0.0 } else { gap - 4.0 };
        let row_h = sizes.iter().fold(line.y, |h, s| h.max(s.y));
        let width = (line.x + buttons_w).max(note.x);
        let height = row_h + if note.y > 0.0 { 4.0 + note.y } else { 0.0 };
        let size = dvec2(width, height) + pad * 2.0;
        // Route selected comes and goes with the selection: the line is centred without it
        let coming = match (text.buttons.last(), sizes.last()) {
            (Some((LineButton::RouteSelected, _)), Some(s)) if text.buttons.len() > 1 => s.x + 4.0,
            _ => 0.0,
        };
        let steady = (line.x + buttons_w - coming).max(note.x) + pad.x * 2.0;
        let centred = plate_left(pane.size.x, size.x, steady);
        let job = match self.key {
            Some(LineKey::Job { status, .. }) => Some(status.job),
            _ => None,
        };
        let left = match (job, self.anchor) {
            (Some(job), Some((at, left))) if at == job => left,
            (Some(job), _) => {
                self.anchor = Some((job, centred));
                centred
            }
            (None, _) => {
                self.anchor = None;
                centred
            }
        };
        let rect = Rect { pos: dvec2(pane.pos.x + left, pane.pos.y + 12.0), size };
        let ink = theme.label;
        let plate = theme.marks[3];
        cx.push_clip_rect(pane);
        self.draw_plate.color = vec4(plate[0], plate[1], plate[2], plate[3].max(0.85));
        self.draw_plate.draw_abs(cx, rect);
        let start = rect.pos + pad;
        let row_y = start.y + row_h * 0.5;
        self.draw_label.color = vec4(ink[0], ink[1], ink[2], ink[3]);
        // A running job's buttons come first, so they stay where its first draw put them.
        let (text_x, mut x) = if job.is_some() { (start.x + buttons_w, start.x) } else { (start.x, start.x + line.x + gap) };
        self.draw_label.draw_abs(cx, dvec2(text_x, row_y - line.y * 0.5), &text.line);
        let mut drawn = Vec::with_capacity(text.buttons.len());
        for (slot, ((button, label), size)) in text.buttons.iter().zip(&sizes).enumerate() {
            let r = Rect { pos: dvec2(x, row_y - size.y * 0.5), size: *size };
            let a = if self.hover == Some(slot) { 0.24 } else { 0.12 };
            let draw = match slot {
                0 => &mut self.draw_first,
                1 => &mut self.draw_second,
                _ => &mut self.draw_third,
            };
            draw.color = vec4(ink[0] * a, ink[1] * a, ink[2] * a, a);
            draw.draw_abs(cx, r);
            self.draw_label.draw_abs(cx, r.pos + button_pad, label);
            drawn.push(*button);
            x += size.x + 4.0;
        }
        if self.hover.is_some_and(|slot| slot >= drawn.len()) {
            self.hover = None;
        }
        if !text.note.is_empty() {
            self.draw_note.color = vec4(ink[0], ink[1], ink[2], ink[3] * 0.7);
            self.draw_note.draw_abs(cx, dvec2(start.x, start.y + row_h + 4.0), &text.note);
        }
        cx.pop_clip_rect();
        self.drawn = Some(drawn);
    }

    /// Hover and presses on the line. The pane calls this before its canvas: the buttons and then
    /// the plate claim a press inside them, so the canvas never sees it.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> LineHit {
        let mut out = LineHit::default();
        let Some(drawn) = &self.drawn else { return out };
        for (slot, button) in drawn.iter().enumerate() {
            let area = match slot {
                0 => self.draw_first.area(),
                1 => self.draw_second.area(),
                _ => self.draw_third.area(),
            };
            match event.hits(cx, area) {
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                    cx.set_cursor(MouseCursor::Hand);
                    if self.hover != Some(slot) {
                        self.hover = Some(slot);
                        out.redraw = true;
                    }
                }
                Hit::FingerHoverOut(_) if self.hover == Some(slot) => {
                    self.hover = None;
                    out.redraw = true;
                }
                Hit::FingerUp(e) if e.is_over && e.device.is_primary_hit() => out.pressed = Some(*button),
                _ => {}
            }
        }
        let _ = event.hits(cx, self.draw_plate.area());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::connect;
    use fab_elec_core::{BoardId, LayerId, PadIx, P};
    use fab_elec_fixtures::ledctl;
    use fab_elec_route::{JobId, Method, RetryWhy, RouteScope};
    use std::sync::Arc;

    #[test]
    fn the_line_offers_routing_the_unrouted_nets_and_the_selection() {
        let line_text = |key| super::line_text(key, &[], &[], |n: NetId| [(NetId(3), "SCL"), (NetId(5), "GND")].iter().find(|(id, _)| *id == n).map(|(_, name)| (*name).to_owned()),
            |c: CompId| (c == CompId(7)).then(|| "U1".to_owned()),
            |b: BlockId| [(BlockId(4), "keys.keys[0]"), (BlockId(5), "snare.hiss")].iter().find(|(id, _)| *id == b).map(|(_, path)| (*path).to_owned()));
        assert_eq!(line_text(LineKey::Idle { unrouted: 0, selected: 0, left: None }), None, "a fully routed board shows no line");
        let one = line_text(LineKey::Idle { unrouted: 1, selected: 0, left: None }).expect("an unrouted net shows the line");
        assert_eq!((one.line.as_str(), one.buttons.as_slice()), ("1 net unrouted", &[(LineButton::RouteAll, "Route all")][..]),
            "one net is singular, and without a selection only Route all is offered");
        let both = line_text(LineKey::Idle { unrouted: 7, selected: 2, left: None }).expect("unrouted nets show the line");
        assert_eq!(both.line, "7 nets unrouted", "the count of unrouted nets");
        assert_eq!(both.buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(), [LineButton::RouteAll, LineButton::RouteSelected],
            "a selection holding unrouted nets adds Route selected");
        assert_eq!(super::plate_left(800.0, 420.0, 320.0), super::plate_left(800.0, 320.0, 320.0),
            "Route selected, coming with the selection, grows the line rightwards: nothing before it moves");
        assert_eq!(super::plate_left(380.0, 420.0, 320.0), 0.0, "a pane narrower than the line keeps its start inside");

        let status = JobStatus {
            job: JobId(1), unit: BoardId(0), phase: JobPhase::Nets, current: Some(NetId(3)), routed: 4, failed: 0,
            total: 9, attempt: 0, retry: None, blocked_by: None,
        };
        let job = |status: JobStatus| line_text(LineKey::Job { status, organic: false, hand: HandLine::default() });
        let running = job(status).expect("a running job has a line");
        assert_eq!((running.line.as_str(), running.note.as_str()), ("Routing SCL: 4 of 9 nets", ""), "running: the net and the progress");
        assert_eq!(running.buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(), [LineButton::Stop, LineButton::Discard],
            "a running job offers Stop and Discard");
        assert_eq!(job(JobStatus { failed: 2, ..status }).unwrap().line, "Routing SCL: 4 of 9 nets, 2 open", "open nets are counted while running");
        assert_eq!(job(JobStatus { attempt: 1, retry: Some(RetryWhy::RipUp), ..status }).unwrap().note,
            "Routing again after ripping up the nets in its way", "a retry says why");
        assert_eq!(job(JobStatus { blocked_by: Some(NetId(5)), retry: Some(RetryWhy::RipUp), ..status }).unwrap().note,
            "It stays open: routing it would cut GND off", "a block names the net it would cut off, over the retry");
        assert_eq!(job(JobStatus { phase: JobPhase::Relax { step: 7 }, current: None, ..status }).unwrap().line,
            "Relaxing the copper, step 7: 4 of 9 nets routed", "relaxing names no net");
        assert_eq!(job(JobStatus { total: 1, routed: 0, ..status }).unwrap().line, "Routing SCL: 0 of 1 net", "one net is singular");
        assert_eq!(job(JobStatus { total: 0, routed: 0, current: None, ..status }).unwrap().line, "Starting the route",
            "before the worker has counted its nets the line says the route starts, not 0 of 0");
        let organic = |status: JobStatus| line_text(LineKey::Job { status, organic: true, hand: HandLine::default() });
        assert_eq!(organic(JobStatus { total: 0, routed: 0, current: None, ..status }).unwrap().line, "Organic: Starting the route",
            "an organic job says so from its first moment");
        assert_eq!(organic(status).unwrap().line, "Organic: Routing SCL: 4 of 9 nets", "an organic job's grid pass says it is organic");
        assert_eq!(organic(JobStatus { phase: JobPhase::Relax { step: 7 }, current: None, ..status }).unwrap().line,
            "Relaxing the copper, step 7: 4 of 9 nets routed", "the relax says what it does");
        let placing = |round, routed, failed| job(JobStatus { phase: JobPhase::Seed { round }, current: None, routed, failed, ..status }).unwrap();
        assert_eq!(placing(0, 0, 0).line, "Placing 9 parts", "a place job's first seeding counts the parts in scope");
        assert_eq!(placing(2, 8, 1).line, "Making room for the tracks, round 2: 8 of 9 parts placed, 1 without room",
            "a demand round counts the parts placed and those without room");
        assert_eq!(placing(1, 9, 0).buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(), [LineButton::Stop, LineButton::Discard],
            "a place job stops and discards as a route does");
        use fab_elec_route::progress::CompareStep;
        let phase = JobPhase::Compare { seated: CompareStep::Group(2), plain: CompareStep::Round(1), of: 7 };
        let comparing = job(JobStatus { phase, ..status }).unwrap();
        assert!(comparing.line.contains("with seats: matching group 3 of 7") && comparing.line.contains("without seats: demand round 1"));
        let matching = |group, of, first| job(JobStatus { phase: JobPhase::Match { group, of, first }, current: None, ..status }).unwrap().line;
        assert_eq!(matching(2, 7, BlockId(4)), "Matching repeated blocks: keys.keys (3 of 7)",
            "conform names the group it matches by its first copy, index dropped, and counts from one");
        assert_eq!(matching(0, 2, BlockId(5)), "Matching repeated blocks: snare.hiss (1 of 2)", "a copy's path without an index is its name");
        assert_eq!(matching(0, 1, BlockId(9)), "Matching repeated blocks (1 of 1)", "a group whose block is unknown is still counted");
        let seed = JobStatus { phase: JobPhase::Seed { round: 1 }, current: None, ..status };
        let hand = |hand: HandLine| line_text(LineKey::Job { status: seed, organic: false, hand }).unwrap().note;
        assert_eq!(placing(1, 9, 0).note, "", "a place that takes no hand-drawn copper off adds no note");
        assert_eq!(hand(HandLine::of(&[HandCopper { comp: CompId(7), tracks: 3, vias: 0 }])), "Removes 3 hand-drawn tracks from U1; Discard keeps them",
            "the hand-drawn tracks a place takes off are counted against their part");
        assert_eq!(hand(HandLine::of(&[HandCopper { comp: CompId(7), tracks: 1, vias: 0 }])), "Removes 1 hand-drawn track from U1; Discard keeps it",
            "one hand-drawn track is singular");
        assert_eq!(hand(HandLine::of(&[HandCopper { comp: CompId(7), tracks: 2, vias: 1 }, HandCopper { comp: CompId(9), tracks: 1, vias: 0 }])),
            "Removes 3 hand-drawn tracks and 1 via from 2 parts; Discard keeps them", "several parts are counted, vias apart");

        let design = ledctl();
        let board = design.units.iter().find_map(|u| u.board.as_deref()).expect("ledctl has a board");
        let track = &board.tracks[0];
        let via = &board.vias[0];
        let unrouted = {
            let mut nets = vec![track.net, via.net];
            nets.sort_unstable();
            nets.dedup();
            nets
        };
        let selected = [Sel::Item(board.unit, track.id), Sel::Item(board.unit, via.id), Sel::Item(board.unit, track.id)];
        assert_eq!(selected_unrouted(board, &selected, &unrouted), unrouted, "selected tracks and vias give their nets, each once, sorted");
        assert_eq!(selected_unrouted(board, &selected, &[]), Vec::<NetId>::new(), "routed nets are not offered");
        let part = &board.placed[0];
        let pad_nets: Vec<NetId> = {
            let mut nets: Vec<NetId> = part.pad_nets.iter().flatten().copied().collect();
            nets.sort_unstable();
            nets.dedup();
            nets
        };
        assert_eq!(selected_unrouted(board, &[Sel::Item(board.unit, part.id)], &pad_nets), pad_nets, "a selected part gives its pads' nets");
    }

    #[test]
    fn a_finished_route_names_the_nets_it_left_open_until_the_board_changes() {
        let name_of = |n: NetId| [(NetId(3), "SCL"), (NetId(5), "GND"), (NetId(6), "VBUS"), (NetId(8), "LED1")].iter()
            .find(|(id, _)| *id == n).map(|(_, name)| (*name).to_owned());
        let key = |left| LineKey::Idle { unrouted: 6, selected: 0, left };
        let four = [NetId(3), NetId(5), NetId(6), NetId(8)];
        let why = [(NetId(3), "no path. Blocked by GND; try clearing their routes and routing again".to_owned())];
        assert_eq!(line_text(key(Some(JobId(4))), &four, &why, name_of, |_| None, |_| None).unwrap().note, "Still open after the last route: SCL, GND, VBUS and 1 more",
            "the open nets are named, the first three and a count of the rest");
        assert_eq!(line_text(key(Some(JobId(4))), &four[..1], &[], name_of, |_| None, |_| None).unwrap().note, "Still open after the last route: SCL", "one open net");
        assert_eq!(line_text(key(Some(JobId(4))), &four[..1], &[], name_of, |_| None, |_| None).unwrap().buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(),
            [LineButton::RouteAll, LineButton::Reroute], "the nets a route left open can be re-routed");
        assert_eq!(line_text(LineKey::Idle { unrouted: 6, selected: 1, left: Some(JobId(4)) }, &four[..1], &[], name_of, |_| None, |_| None).unwrap()
            .buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(), [LineButton::RouteAll, LineButton::Reroute, LineButton::RouteSelected],
            "Route selected, which comes and goes with the selection, comes last, so Re-route never moves with it");
        assert!(line_text(key(None), &four, &why, name_of, |_| None, |_| None).unwrap().buttons.iter().all(|(b, _)| *b != LineButton::Reroute),
            "without nets a route left open there is nothing to re-route");
        assert_eq!(line_text(key(Some(JobId(4))), &four[..1], &why, name_of, |_| None, |_| None).unwrap().note,
            "SCL: no path. Blocked by GND; try clearing their routes and routing again", "one open net with its reason says why and what to try");
        assert_eq!(line_text(key(None), &four, &why, name_of, |_| None, |_| None).unwrap().note, "", "no finished job, no note");
        assert_eq!(line_text(key(Some(JobId(4))), &[], &why, name_of, |_| None, |_| None).unwrap().note, "", "a job that left nothing open adds no note");

        let design = ledctl();
        let board = design.units.iter().find_map(|u| u.board.as_deref()).expect("ledctl has a board");
        let conn = connect(board, &[]);
        let open: Vec<NetId> = conn.unrouted.iter().copied().take(2).collect();
        let routed = (0..board.nets.len() as u32).map(NetId).find(|n| conn.unrouted.binary_search(n).is_err());
        assert_eq!((open.len(), routed.is_some()), (2, true), "fixture setup: ledctl has open and routed nets");
        let mut asked = open.clone();
        asked.extend(routed);
        // the job's reason for the first open net: no corridor from one of its pads to another
        let pads: Vec<(Sel, String)> = board.placed.iter()
            .flat_map(|p| p.pad_nets.iter().enumerate().filter(|(_, net)| **net == Some(open[0]))
                .map(move |(i, _)| (Sel::Feature(FeatureRef::Pad(CompRef { board: board.unit, comp: p.comp }, PadIx(i as u16))),
                    format!("{} pad {}", p.refdes, p.footprint.pads[i].no))))
            .take(2).collect();
        assert_eq!(pads.len(), 2, "fixture setup: the open net has two pads");
        let no_room = Reason::NoRoom { at: P::ZERO, layer: LayerId::F_CU, need: Measure::Len(500_000), have: Measure::Len(0),
            between: [pads[0].0.clone(), pads[1].0.clone()], by: Vec::new() };
        let why_first = format!("no path for its 0.5 mm track from {} to {}; try clearing the routes in its way and routing again, or moving a part",
            pads[0].1, pads[1].1);
        assert_eq!(why_open(board, &no_room), why_first, "no corridor names the pads it could not join, by reference and pad");
        let narrow = Reason::NoRoom { at: P::ZERO, layer: LayerId::F_CU, need: Measure::Len(500_000), have: Measure::Len(120_000),
            between: [pads[0].0.clone(), pads[1].0.clone()], by: Vec::new() };
        assert_eq!(why_open(board, &narrow), format!("no path. No room for its 0.5 mm track between {} and {} (0.12 mm free); try moving one of them",
            pads[0].1, pads[1].1), "a narrow corridor says how much room there is");
        assert_eq!(why_open(board, &Reason::Terminal { at: P::ZERO, with: Some(pads[0].0.clone()) }),
            format!("no path. {} has no way out; try moving its part", pads[0].1), "a pad nothing reaches is named by reference and pad");
        // A drag's cut-back chip: the cut track's net and what it needed room past, a track by its net.
        let (cut, other) = (&board.tracks[0], board.tracks.iter().find(|t| t.net != board.tracks[0].net).expect("fixture setup: two nets' tracks"));
        let name = |net: NetId| board.nets[net.ix()].name.clone();
        let cut_by = |sel: Sel| Reason::NoRoom { at: P::ZERO, layer: LayerId::F_CU, need: Measure::Len(150_000), have: Measure::Len(0),
            between: [Sel::Item(board.unit, cut.id), sel], by: Vec::new() };
        // A net walled in by kept copper: what is in its way, by net, and on which layer.
        let walled = Reason::NoRoom { at: P::ZERO, layer: LayerId::B_CU, need: Measure::Len(500_000), have: Measure::Len(0),
            between: [pads[0].0.clone(), pads[1].0.clone()], by: vec![Sel::Item(board.unit, other.id), Sel::Item(board.unit, cut.id)] };
        assert_eq!(why_open(board, &walled), format!("no path for its 0.5 mm track from {} to {}: kept copper in the way on B.Cu: the {} track, the {} track; try Re-route, which may move it, or moving a part",
            pads[0].1, pads[1].1, name(other.net), name(cut.net)), "a net walled in by kept copper names that copper by net, and its layer");
        let one = [(cut.net, cut_by(Sel::Item(board.unit, other.id)))];
        assert_eq!(cut_words(board, &one, 0).as_deref(), Some(format!("{} cut back: 0.15 mm needed past the {} track", name(cut.net), name(other.net)).as_str()),
            "a cut names its net and the track it needed room past by that track's net");
        let two = [one[0].clone(), (other.net, cut_by(pads[0].0.clone()))];
        assert_eq!(cut_words(board, &two, 0).as_deref(), Some(format!("2 tracks cut back; {}: 0.15 mm needed past the {} track", name(cut.net), name(other.net)).as_str()),
            "several cuts give their count and the first");
        assert_eq!(cut_words(board, &one, 3).as_deref(), Some(format!("pushed 3 tracks; {} cut back: 0.15 mm needed past the {} track", name(cut.net), name(other.net)).as_str()),
            "a shove that also cut says how many tracks it pushed first");
        assert_eq!(cut_words(board, &[], 2), None, "a drag that cuts nothing has no chip: pushes need nothing of the person");
        let unjoined = |track, host| Reason::Unjoined {
            at: P::mm(1.25, -2.5), track: Sel::Item(board.unit, track), host: Sel::Item(board.unit, host),
        };
        let junctions = [(other.net, unjoined(other.id, cut.id)), (cut.net, unjoined(cut.id, other.id))];
        assert_eq!(why_open(board, &junctions[0].1), format!(
            "junction opened. The end of the {} track at (1.25 mm, -2.5 mm) is no longer on the {} track; try reconnecting it or routing the net again",
            name(other.net), name(cut.net)), "a lost junction names the end, its host, its point and how to reconnect it");
        assert_eq!(cut_words(board, &junctions[..1], 1),
            Some(format!("pushed 1 track; {} junction opened: a track end no longer meets the track it joined", name(other.net))),
            "a single opened junction follows the pushes and names its net");
        assert_eq!(cut_words(board, &junctions, 0),
            Some(format!("2 junctions opened; {}: a track end no longer meets the track it joined", name(other.net))),
            "several opened junctions give their count and the first");
        assert_eq!(cut_words(board, &[one[0].clone(), junctions[0].clone()], 0),
            Some(format!("{} cut back: 0.15 mm needed past the {} track; {} junction opened: a track end no longer meets the track it joined",
                name(cut.net), name(other.net), name(other.net))), "cuts and opened junctions are counted separately");
        assert_eq!(unpushed_words(board, one[0].0, &one[0].1), format!("{} track not pushed: 0.15 mm needed past the {} track", name(cut.net), name(other.net)),
            "a dragged segment that walks names the track it could not push, as a cut does");
        // The tracks a drag pushes: replaced with another path, on none of the held items.
        let held = board.placed.iter().find(|p| board.tracks.iter().any(|t| t.ends.iter().any(|e| e.item() == Some(p.id)))).expect("fixture setup: a part with a track");
        let own = board.tracks.iter().find(|t| t.ends.iter().any(|e| e.item() == Some(held.id))).expect("fixture setup: its track");
        let neighbour = board.tracks.iter().find(|t| !t.ends.iter().any(|e| e.item() == Some(held.id))).expect("fixture setup: another track");
        let moved = |t: &fab_elec_board::Track| {
            let mut t = t.clone();
            t.path.segs.push(fab_elec_core::Seg::Line(t.path.end()));
            BoardOp::Replace(Item::Track(t))
        };
        let same = BoardOp::Replace(Item::Track(neighbour.clone()));
        assert_eq!(pushed(board, &[moved(own), moved(neighbour), same], &[held.id]), [(neighbour.id, ItemKind::Track)],
            "a held item's own track is stretched, not pushed, and a replace that keeps the path (an end anchored again) pushes nothing");
        let via = board.vias.first().expect("fixture setup: ledctl has a via");
        let mut shifted = via.clone();
        shifted.at = via.at.add(P::new(1, 0));
        let ops = [BoardOp::Replace(Item::Via(shifted)), moved(neighbour), BoardOp::Replace(Item::Via(via.clone()))];
        assert_eq!(pushed(board, &ops, &[held.id]), [(via.id, ItemKind::Via), (neighbour.id, ItemKind::Track)],
            "a via replaced at another place appears in op order, and a via replaced in place pushes nothing");
        assert!(pushed(board, &ops[..1], &[via.id]).is_empty(), "the held via stays out of its own pushes");
        let moves = [BoardOp::Move { item: via.id, by: P::new(1, 0) }, BoardOp::Move { item: via.id, by: P::ZERO }];
        assert_eq!(pushed(board, &moves, &[held.id]), [(via.id, ItemKind::Via)],
            "a via the wave moves (a Move, as pushed and riding vias are sent) is pushed, and a zero move pushes nothing");
        let job = RouteJobState {
            status: JobStatus {
                job: JobId(4), unit: board.unit, phase: JobPhase::Nets, current: None, routed: 1, failed: 2,
                total: 3, attempt: 0, retry: None, blocked_by: None,
            },
            current_name: None,
            end: Some(JobEnd::Finished),
            scope: Arc::new(RouteScope::Nets(asked)),
            method: Method::Interactive,
            open: Arc::new(vec![(open[0], no_room)]),
        };
        assert_eq!(left_open(&job, false, board, &conn),
            Some(LeftOpen { job: JobId(4), generation: board.generation, nets: open.clone(), why: vec![(open[0], why_first)] }),
            "the finished job's open nets are its scope's nets still in more than one cluster, with the reasons the job gave");
        assert_eq!(left_open(&job, true, board, &conn), None, "not while its last preview waits to commit");
        let behind = Connectivity { generation: board.generation.wrapping_sub(1), ..conn.clone() };
        assert_eq!(left_open(&job, false, board, &behind), None, "not before connectivity has caught up with the committed board");
        let stopped = RouteJobState { end: Some(JobEnd::Stopped), ..job.clone() };
        assert_eq!(left_open(&stopped, false, board, &conn), None, "a stopped job names nothing: the person ended it");
        let running = RouteJobState { end: None, ..job };
        assert_eq!(left_open(&running, false, board, &conn), None, "a running job names nothing yet");

        let left = LeftOpen { job: JobId(4), generation: board.generation, nets: open.clone(), why: Vec::new() };
        assert_eq!(still_open(&left, board.generation, &conn), open, "the nets still unrouted are named");
        let joined = Connectivity { unrouted: conn.unrouted.iter().copied().filter(|net| *net != open[0]).collect(), ..conn.clone() };
        assert_eq!(still_open(&left, board.generation, &joined), open[1..].to_vec(),
            "a net the counted connectivity has joined since (a pour refilled after the commit) is not named: the names agree with the count");
        assert_eq!(still_open(&left, board.generation.wrapping_add(1), &conn), Vec::<NetId>::new(), "a board changed since names nothing");
    }
}
