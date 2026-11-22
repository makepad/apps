//! The case shared by Mechanics and the Device editors. The pool resolves the plate and
//! design once. The UI binds placed indices to scene elements and updates their device poses
//! when the inputs, element IDs or assembly position change.

use fab::api::{AppState, PartGlow, PartPress, PartTurn, PlatePart, PoseSetId, ShellAction};
use fab::model::{state::Pose, ElementId, PoseSet};
use fab_elec_bench::{audio::output_jacks, ValueIndex};
use fab_elec_core::CompRef;
use fab_elec_design::Design;
use fab_elec_mech::bridge::bare_board;
use fab_elec_mech::{assembly_plan, board_base, board_mech_with, case_binding, case_poses, AssemblyPlan, CaseBinding, StepKind};
use fab_elec_view::ElecLive;
use fab_model::eval::Evaluated;
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, ToUIReceiver};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

type Affine = [[f64; 4]; 3];
const APPROACH_MM: f64 = 30.0;
const ASSEMBLY_SECONDS: f64 = 2.5;
/// The share of a step over which its parts set off, one after another.
const STAGGER: f64 = 0.5;
/// One build step's move when the Device tab steps through the plan.
const STEP_SECONDS: f64 = 1.2;
/// How far Kit lays a bought board clear of the printed parts.
const KIT_GAP_MM: f64 = 10.0;

struct ClosedPart {
    part: usize,
    step: Option<usize>,
    to_board: Affine,
    /// A print placement and its glued bodies turn about the same point on approach.
    centre: [f64; 3],
    binding: CaseBinding,
    /// When it sets off within its step and how long it travels, both as shares of the step
    /// ([`stagger`]); 0 and 1 for a part alone in its step.
    start: f64,
    span: f64,
}

pub(super) struct ClosedCase {
    plate_from_board: Affine,
    plan: AssemblyPlan,
    parts: Vec<ClosedPart>,
    /// The board occupies its own step and stays visible when the case is hidden.
    board: Option<ClosedPart>,
    /// The board's outline in its own millimetres (x and y, min and max): what the board layer
    /// draws through its holder's linked frame.
    board_outline: Option<([f64; 2], [f64; 2])>,
    pub(super) output: Option<CompRef>,
}

impl ClosedCase {
    fn placements(&self) -> impl Iterator<Item = &ClosedPart> {
        self.parts.iter().chain(self.board.iter())
    }

    pub(super) fn bindings(&self) -> impl Iterator<Item = CaseBinding> + '_ {
        self.parts.iter().map(|part| part.binding)
    }
}

fn closed_case(ev: &Evaluated, design: &Design) -> Option<ClosedCase> {
    design.units.iter().find_map(|unit| {
        let board = unit.board.as_deref()?;
        let poses = case_poses(ev, &unit.name);
        let plate_from_board = affine_inverse(&board_base(&poses)?.to_board);
        let plan = assembly_plan(&poses);
        let steps: HashMap<_, _> = plan.steps.iter().enumerate()
            .flat_map(|(k, step)| step.parts.iter().map(move |&part| (part, k))).collect();
        let group_steps: HashMap<_, _> = poses.iter().filter_map(|p| {
            Some((ev.placed_part(p.part).body_of.as_deref()?, *steps.get(&p.part)?))
        }).collect();
        let mech = board_mech_with(board, &unit.circuit, &board.drills());
        let mut centres = HashMap::new();
        let mut placement = |part, step, to_board, binding| {
            let placed = ev.placed_part(part);
            let centre = match placed.body_of.as_deref() {
                Some(group) => *centres.entry(group).or_insert_with(|| placed.bounds().center()),
                None => placed.bounds().center(),
            };
            ClosedPart { part, step, to_board, centre, binding, start: 0.0, span: 1.0 }
        };
        let mut parts: Vec<ClosedPart> = poses.iter().map(|p| {
            // A glued body follows its host even when only the host is named by the plan.
            let step = steps.get(&p.part).or_else(|| group_steps.get(ev.placed_part(p.part).body_of.as_deref()?)).copied();
            placement(p.part, step, p.to_board, case_binding(p, &mech))
        }).collect();
        stagger(&mut parts, |part| ev.placed_part(part).body_of.clone());

        // Match the board layer's first linked placement. Inserts keep their named frame in the
        // excluded placements. Both printed and excluded indices are present in PlateInfo.
        let bare = bare_board(ev, &unit.name);
        let board_step = plan.steps.iter().position(|s| s.kind == StepKind::Board)?;
        let outline = board.outline_bbox();
        let mm = |v: i32| f64::from(v) / 1_000_000.0;
        let board_outline = (!outline.is_empty()).then(|| ([mm(outline.min.x), mm(outline.min.y)], [mm(outline.max.x), mm(outline.max.y)]));
        let board = (0..ev.plate.parts.len() + ev.plate.excluded.len()).find_map(|i| {
            let (key, plate_from_frame) = crate::plate_edit::linked_place(ev, i)?;
            (key.strip_prefix("board:") == Some(unit.name.as_str())).then_some(())?;
            let to_board = bare.filter(|(part, _)| *part == i).map(|(_, frame)| frame)
                .or_else(|| crate::paint_edit::inverse(&plate_from_frame))?;
            Some(placement(i, Some(board_step), to_board, CaseBinding::None))
        });
        let output = output_jacks(&unit.circuit).first().map(|jack| CompRef { board: unit.id, comp: jack.comp });
        Some(ClosedCase { plate_from_board, plan, parts, board, board_outline, output })
    })
}

/// Owns the device pose set and live bindings for all consumers together.
#[derive(Default)]
pub struct DeviceAssembly {
    shown: bool,
    asked: Option<([u8; 32], Arc<Design>)>,
    // Each request owns its receiver. A superseded worker can never publish into a new request.
    pending: Option<(TaskHandle<()>, ToUIReceiver<Option<Arc<ClosedCase>>>)>,
    case: Option<Arc<ClosedCase>>,
    bound_from: Option<(u64, u64)>,
    /// In the order of ClosedCase::placements, including a slot for a missing scene element.
    elements: Vec<Option<ElementId>>,
    /// Where Kit lays each placement out (a bought board in its base), as a move in the plate frame;
    /// in placement order, from the plate the scene draws.
    kits: Vec<Option<Affine>>,
    held: Option<Arc<Vec<CompRef>>>,
    turned: Option<(Arc<Vec<(CompRef, f64)>>, Arc<ValueIndex>)>,
    position: f64,
    frame: Option<(NextFrame, f64)>,
}

impl DeviceAssembly {
    pub(super) fn case(&self) -> Option<&ClosedCase> {
        self.case.as_deref()
    }

    pub(super) fn resolving(&self) -> bool {
        self.pending.is_some()
    }

    /// Called each event after stage pane routing. Closed has priority over the editors' target.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState, plate: Option<&Arc<Evaluated>>, device_shown: bool, closed: bool) {
        if !(device_shown || closed) {
            if self.shown {
                self.clear(cx, state);
                self.frame = None;
                self.shown = false;
            }
            return;
        }
        let target = if closed || !state.device.kit { 1.0 } else { 0.0 };
        let snap = closed || !self.shown;
        self.shown = true;
        let Some((ev, live)) = plate.zip(cx.get_global_ref::<ElecLive>()) else {
            if self.asked.is_some() {
                self.clear(cx, state);
            }
            self.position = target;
            self.frame = None;
            return;
        };
        let design = live.snap.design.clone();
        self.resolve(cx, state, ev, &design);
        let Some(case) = self.case.clone() else {
            self.position = target;
            self.frame = None;
            return;
        };
        let steps = case.plan.steps.len();
        let stepping = !closed && state.device.step.is_some() && steps > 0;
        let target = if closed { 1.0 } else { assembly_target(state.device.step, state.device.kit, steps) };
        let seconds = if stepping { STEP_SECONDS * steps as f64 } else { ASSEMBLY_SECONDS };
        let moved = self.advance(cx, event, target, seconds, snap);
        state.device.position = self.position;
        let from = (state.plate.revision, state.scene_revision);
        let mut rebound = false;
        if self.bound_from != Some(from) {
            self.bound_from = Some(from);
            let ids = case.placements().map(|part| state.plate.parts.get(part.part).and_then(|p| p.element));
            if !self.elements.iter().copied().eq(ids) {
                self.bind(cx, state, &case);
                rebound = true;
            }
            let kits = kit_moves(&case, &state.plate.parts, (state.plate.bed_min, state.plate.bed_max));
            if kits != self.kits {
                self.kits = kits;
                rebound = true;
            }
        }
        let sim = &cx.get_global::<ElecLive>().sim;
        let same_held = self.held.as_ref().is_some_and(|h| Arc::ptr_eq(h, &sim.held));
        let same_turns = self.turned.as_ref().is_some_and(|(t, i)| Arc::ptr_eq(t, &sim.turned) && Arc::ptr_eq(i, &sim.index));
        if !moved && !rebound && same_held && same_turns {
            return;
        }
        let set = state.pose_set_mut(PoseSetId::DEVICE);
        let before = set.revision();
        pose(&case, &self.elements, &self.kits, self.position, &sim.held, &sim.turned, sim.index.knobs(), set);
        self.held = Some(sim.held.clone());
        self.turned = Some((sim.turned.clone(), sim.index.clone()));
        if before != set.revision() {
            cx.redraw_all();
        }
    }

    /// Moves the position toward `target`, the whole way (0 to 1) in `seconds`.
    fn advance(&mut self, cx: &mut Cx, event: &Event, target: f64, seconds: f64, snap: bool) -> bool {
        let before = self.position;
        if snap || before == target {
            self.position = target;
            self.frame = None;
        } else {
            if let Some((frame, _)) = self.frame {
                if !matches!(event, Event::NextFrame(ne) if ne.set.contains(&frame)) {
                    return false;
                }
            }
            let now = Cx::monotonic_now();
            if let Some((_, last)) = self.frame {
                let distance = (now - last) / seconds;
                self.position += (target - before).clamp(-distance, distance);
            }
            self.frame = (self.position != target).then(|| (cx.new_next_frame(), now));
        }
        self.position != before
    }

    fn resolve(&mut self, cx: &mut Cx, state: &mut AppState, ev: &Arc<Evaluated>, design: &Arc<Design>) {
        if self.asked.as_ref().is_none_or(|(content, was)| *content != ev.content || !Arc::ptr_eq(was, design)) {
            self.clear(cx, state);
            let rx = ToUIReceiver::default();
            let (tx, plate, snapshot) = (rx.sender(), ev.clone(), design.clone());
            self.asked = Some((ev.content, design.clone()));
            match cx.task_pool().submit_named(Class::Background, "fab device assembly", move || {
                let _ = tx.send(closed_case(&plate, &snapshot).map(Arc::new));
            }) {
                Ok(task) => self.pending = Some((task, rx)),
                Err(error) => log!("device assembly: {error}"),
            }
        }
        if let Some(case) = self.pending.as_ref().and_then(|(_, rx)| rx.try_recv().ok()) {
            self.pending = None;
            self.case = case;
        }
    }

    fn bind(&mut self, cx: &mut Cx, state: &mut AppState, case: &ClosedCase) {
        self.elements.clear();
        let (mut presses, mut turns, mut glows) = (Vec::new(), Vec::new(), Vec::new());
        for part in case.placements() {
            let element = state.plate.parts.get(part.part).and_then(|p| p.element);
            self.elements.push(element);
            let Some(element) = element else { continue };
            match part.binding {
                CaseBinding::Key { comp, .. } => presses.push(PartPress { element, comp }),
                CaseBinding::Knob { comp, .. } => turns.push(PartTurn { element, comp }),
                CaseBinding::Pipe { comp } => glows.push(PartGlow {
                    element, comp, seat: metres(&affine_mul(&case.plate_from_board, &part.to_board)),
                }),
                CaseBinding::None => {}
            }
        }
        let (parts, board) = self.elements.split_at(case.parts.len());
        let members = parts.iter().flatten().copied().collect::<std::collections::HashSet<_>>();
        state.pose_set_mut(PoseSetId::DEVICE).retain(|e| members.contains(&e) || board.contains(&Some(e)));
        if members != *state.device.case {
            state.device.case = Arc::new(members);
        }
        state.part_presses = presses.into();
        state.part_turns = turns.into();
        state.device.steps = case.plan.steps.iter().map(|s| step_name(&s.kind)).collect::<Vec<_>>().into();
        cx.action(ShellAction::SetPartGlows(glows.into()));
        cx.redraw_all();
    }

    fn clear(&mut self, cx: &mut Cx, state: &mut AppState) {
        if let Some((task, _)) = self.pending.take() {
            task.cancel();
        }
        self.asked = None;
        self.case = None;
        self.held = None;
        self.turned = None;
        self.elements.clear();
        self.kits.clear();
        if self.bound_from.take().is_some() {
            state.device.case = Arc::default();
            state.part_presses = Arc::new([]);
            state.part_turns = Arc::new([]);
            state.device.steps = Arc::default();
            state.pose_set_mut(PoseSetId::DEVICE).clear();
            cx.action(ShellAction::SetPartGlows(Arc::new([])));
            cx.redraw_all();
        }
    }
}

/// Replace the poses of the bound placements in place. A missing entry means the part is at rest.
#[allow(clippy::too_many_arguments)]
fn pose(case: &ClosedCase, elements: &[Option<ElementId>], kits: &[Option<Affine>], position: f64, held: &[CompRef], turned: &[(CompRef, f64)], starts: &[(CompRef, f64)], set: &mut PoseSet) {
    if position <= 0.0 {
        set.clear();
        for (&element, kit) in elements.iter().zip(kits) {
            if let (Some(kit), Some(element)) = (kit, element) {
                set.set(element, metres(kit));
            }
        }
        return;
    }
    let progress = position.min(1.0) * case.plan.steps.len() as f64;
    for (i, (p, &element)) in case.placements().zip(elements).enumerate() {
        let Some(element) = element else { continue };
        let kit = kits.get(i).copied().flatten();
        // Parts without a planned step stay at rest until the completed case.
        let t = if position >= 1.0 { 1.0 } else { p.step.map_or(0.0, |k| (progress - k as f64 - p.start) / p.span) };
        if t <= 0.0 {
            match kit {
                Some(kit) => set.set(element, metres(&kit)),
                None => set.remove(element),
            }
            continue;
        }
        let mut to_board = p.to_board;
        match p.binding {
            CaseBinding::Key { comp, travel } if held.contains(&comp) => to_board[2][3] -= travel,
            CaseBinding::Knob { comp, axis } => {
                if let Some(&(_, place)) = turned.iter().chain(starts).find(|(c, _)| *c == comp) {
                    let angle = -(place - 0.5) * fab_elec_parts::SHAFT_TURN_DEG.to_radians();
                    to_board = affine_mul(&turn_about_z(axis, angle), &to_board);
                }
            }
            _ => {}
        }
        let mut seated = affine_mul(&case.plate_from_board, &to_board);
        if let Some(step) = p.step.filter(|_| t < 1.0) {
            let from = case.plan.steps[step].from;
            for (r, row) in seated.iter_mut().enumerate() {
                let direction = (0..3).map(|k| case.plate_from_board[r][k] * from[k]).sum::<f64>();
                row[3] += APPROACH_MM * direction * (2.0 * (1.0 - t)).min(1.0);
            }
            if t < 0.5 {
                seated = match kit {
                    // the same turn and carry from where Kit shows it
                    Some(kit) => affine_mul(&from_rest(&affine_mul(&seated, &affine_inverse(&kit)), affine_point(&kit, p.centre), t * 2.0), &kit),
                    None => from_rest(&seated, p.centre, t * 2.0),
                };
            }
        }
        set.set(element, metres(&seated));
    }
}

/// The parts one step puts on set off one after another across the board (by their seat's x,
/// then y), over the step's first [`STAGGER`], and each arrives by the step's end. A glued body
/// (`group_of`) goes with its host. Once per case resolve: O(parts) per step.
fn stagger(parts: &mut [ClosedPart], group_of: impl Fn(usize) -> Option<String>) {
    let keys: Vec<String> = parts.iter().map(|p| group_of(p.part).unwrap_or_else(|| format!("#{}", p.part))).collect();
    let mut movers: HashMap<usize, Vec<(&str, [f64; 2])>> = HashMap::new();
    for (p, key) in parts.iter().zip(&keys) {
        let Some(step) = p.step else { continue };
        let list = movers.entry(step).or_default();
        if !list.iter().any(|(k, _)| *k == key.as_str()) {
            list.push((key.as_str(), [p.to_board[0][3], p.to_board[1][3]]));
        }
    }
    for list in movers.values_mut() {
        list.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]).then(a.1[1].total_cmp(&b.1[1])));
    }
    let starts: Vec<Option<(f64, f64)>> = parts.iter().zip(&keys).map(|(p, key)| {
        let list = movers.get(&p.step?)?;
        let at = list.iter().position(|(k, _)| *k == key.as_str())?;
        (list.len() > 1).then(|| (STAGGER * at as f64 / (list.len() - 1) as f64, 1.0 - STAGGER))
    }).collect();
    for (p, timing) in parts.iter_mut().zip(starts) {
        if let Some((start, span)) = timing {
            (p.start, p.span) = (start, span);
        }
    }
}

/// Where the assembly stands for the Device tab's mode: Kit at 0, assembled at 1, stopped at a
/// build step with the plan's steps up to and including it put on.
fn assembly_target(step: Option<usize>, kit: bool, steps: usize) -> f64 {
    match step {
        Some(k) if steps > 0 => (k + 1).min(steps) as f64 / steps as f64,
        _ if kit => 0.0,
        _ => 1.0,
    }
}

/// A build step as the step controls name it.
fn step_name(kind: &StepKind) -> String {
    match kind {
        StepKind::Base => "Base".into(),
        StepKind::Shell(role) => role.clone(),
        StepKind::Board => "Board".into(),
        StepKind::Keys => "Keys".into(),
        StepKind::Lid => "Lid".into(),
        StepKind::Knobs => "Knobs".into(),
    }
}

/// Where Kit lays each placement out, in [`ClosedCase::placements`] order, as a move in the plate
/// frame, from the plate the scene draws: the printed parts' bounds, and the board where the board
/// layer draws it at rest (its outline through its holder's linked frame). A board resting in its
/// base is laid out ([`kit_move`]); every other placement stays where it prints. O(parts), once
/// per plate revision.
fn kit_moves(case: &ClosedCase, parts: &[PlatePart], bed: ([f64; 2], [f64; 2])) -> Vec<Option<Affine>> {
    let bounds = |part: usize| parts.get(part).map(|p| (p.min_mm, p.max_mm));
    let printed: Vec<_> = case.parts.iter().filter_map(|p| bounds(p.part)).collect();
    let base: Vec<_> = case.plan.steps.iter().filter(|s| s.kind == StepKind::Base)
        .flat_map(|s| s.parts.iter().filter_map(|&part| bounds(part))).collect();
    let board = board_rest(case, parts).and_then(|(part, rest)| Some((part, kit_move(rest, &base, &printed, bed)?)));
    case.placements().map(|p| board.filter(|(part, _)| *part == p.part).map(|(_, kit)| kit)).collect()
}

/// The board's placement and its plate-frame bounds (mm) where the board layer draws it at rest:
/// its outline through its holder's linked frame, at its bottom face.
fn board_rest(case: &ClosedCase, parts: &[PlatePart]) -> Option<(usize, ([f64; 3], [f64; 3]))> {
    let (board, (lo, hi)) = case.board.as_ref().zip(case.board_outline)?;
    let frame = parts.get(board.part)?.linked.as_ref()?.plate_from_frame;
    let corners = [[lo[0], lo[1]], [hi[0], lo[1]], [lo[0], hi[1]], [hi[0], hi[1]]].map(|[x, y]| affine_point(&frame, [x, y, 0.0]));
    let rest = corners.iter().fold(([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]), |(lo, hi), c| {
        (std::array::from_fn(|d| lo[d].min(c[d])), std::array::from_fn(|d| hi[d].max(c[d])))
    });
    Some((board.part, rest))
}

/// Kit lays a bought board that rests in its base (its rest overlaps the base) out flat on the
/// bed beside the base, `KIT_GAP_MM` clear: in front (-y), else right, left or behind, the first
/// side on the bed and clear of every printed part; with none, in front of the bed. None when it
/// already rests outside its base. Bounds are plate-frame millimetres.
fn kit_move(board: ([f64; 3], [f64; 3]), base: &[([f64; 3], [f64; 3])], printed: &[([f64; 3], [f64; 3])], bed: ([f64; 2], [f64; 2])) -> Option<Affine> {
    let (min, max) = board;
    let overlaps = |by: [f64; 2], (lo, hi): &([f64; 3], [f64; 3])| (0..2).all(|d| min[d] + by[d] < hi[d] && lo[d] < max[d] + by[d]);
    if !base.iter().any(|b| overlaps([0.0; 2], b)) {
        return None;
    }
    let lo = |d: usize| base.iter().map(|(l, _)| l[d]).fold(f64::INFINITY, f64::min);
    let hi = |d: usize| base.iter().map(|(_, h)| h[d]).fold(f64::NEG_INFINITY, f64::max);
    let sides = [
        [0.0, lo(1) - KIT_GAP_MM - max[1]],
        [hi(0) + KIT_GAP_MM - min[0], 0.0],
        [lo(0) - KIT_GAP_MM - max[0], 0.0],
        [0.0, hi(1) + KIT_GAP_MM - min[1]],
    ];
    let clear = |by: &[f64; 2]| !printed.iter().any(|p| overlaps(*by, p));
    let on_bed = |by: &[f64; 2]| (0..2).all(|d| bed.0[d] <= min[d] + by[d] && max[d] + by[d] <= bed.1[d]);
    let by = sides.into_iter().find(|by| on_bed(by) && clear(by)).unwrap_or([0.0, bed.0[1] - KIT_GAP_MM - max[1]]);
    // flat on the bed: its bottom face at the plate's floor
    Some([[1.0, 0.0, 0.0, by[0]], [0.0, 1.0, 0.0, by[1]], [0.0, 0.0, 1.0, -min[2]]])
}

fn affine_point(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

fn metres(m: &Affine) -> Pose {
    m.map(|[x, y, z, mm]| [x as f32, y as f32, z as f32, (mm * 0.001) as f32])
}

/// Slerp from the print orientation and carry the part's centre in a straight line to the
/// approach point. Retain any scale in the seated transform as the part turns.
fn from_rest(end: &Affine, centre: [f64; 3], t: f64) -> Affine {
    let column = |c: usize| vec3f(end[0][c] as f32, end[1][c] as f32, end[2][c] as f32);
    let rotation = Quat::look_rotation(column(2), column(1));
    let turn = Mat3f::from_quat(Quat::from_slerp(Quat::default(), rotation, t as f32));
    let unturn = Mat3f::from_quat(rotation).transpose();
    let axes = [vec3f(1., 0., 0.), vec3f(0., 1., 0.), vec3f(0., 0., 1.)];
    let mut m = [[0.0; 4]; 3];
    for (c, axis) in axes.into_iter().enumerate() {
        let v = turn.mul_vec3(axis + (unturn.mul_vec3(column(c)) - axis) * t as f32);
        [m[0][c], m[1][c], m[2][c]] = [v.x as f64, v.y as f64, v.z as f64];
    }
    for r in 0..3 {
        let rotated = |a: &Affine| (0..3).map(|c| a[r][c] * centre[c]).sum::<f64>();
        m[r][3] = centre[r] + t * (rotated(end) + end[r][3] - centre[r]) - rotated(&m);
    }
    m
}

fn affine_mul(a: &Affine, b: &Affine) -> Affine {
    std::array::from_fn(|r| std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c] + if c == 3 { a[r][3] } else { 0.0 }))
}

/// A turn by `angle` radians (counter-clockwise seen from above) round the vertical axis at `at`.
fn turn_about_z(at: [f64; 2], angle: f64) -> Affine {
    let (sin, cos) = angle.sin_cos();
    [
        [cos, -sin, 0.0, at[0] - cos * at[0] + sin * at[1]],
        [sin, cos, 0.0, at[1] - sin * at[0] - cos * at[1]],
        [0.0, 0.0, 1.0, 0.0],
    ]
}

/// The inverse of a rigid map (its turn's transpose); the case frames are rigid.
fn affine_inverse(m: &Affine) -> Affine {
    let t = [m[0][3], m[1][3], m[2][3]];
    std::array::from_fn(|r| std::array::from_fn(|c| if c == 3 { -(m[0][r] * t[0] + m[1][r] * t[1] + m[2][r] * t[2]) } else { m[c][r] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_box() -> &'static (ClosedCase, Design, fab::api::PlateInfo) {
        static DEMO: std::sync::OnceLock<(ClosedCase, Design, fab::api::PlateInfo)> = std::sync::OnceLock::new();
        DEMO.get_or_init(load_tone_box)
    }

    fn load_tone_box() -> (ClosedCase, Design, fab::api::PlateInfo) {
        let demo = crate::demo_part::demo("tonebox").expect("the tone box demo");
        let mut model = demo.model().unwrap_or_else(|e| panic!("the demo loads: {e}"));
        model.set_env(crate::elec_host::doc_env(model.docs(), model.elec_assets()));
        let mut evaluator = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = model.evaluate(&mut evaluator, None).unwrap_or_else(|f| panic!("the case builds: {f}"));
        let design = crate::elec_host::docs_design(model.docs(), model.elec_assets(), None)
            .unwrap_or_else(|e| panic!("the demo's documents build: {e}"));
        let case = super::closed_case(&ev, &design).expect("the board's case is on the plate");
        // the plate the scene draws
        let plate = crate::plate_edit::plate_info(None, &ev, 0);
        (case, design, plate)
    }

    /// The tone box closes from its plate's data alone: its keycap presses its key, its knob turns
    /// its pot, its pipe shows its LED, and Listen hears its jack's left channel. Nothing here
    /// names the box's parts.
    #[test]
    fn the_tone_box_closes_with_its_key_knob_pipe_and_jack_from_its_data() {
        let (case, design, _) = tone_box();
        let unit = design.unit_by_name("tonebox").expect("the board unit");
        let mut bound: Vec<String> = case.parts.iter().filter_map(|p| match p.binding {
            super::CaseBinding::Key { comp, .. } => Some(format!("key {}", unit.circuit.comps[comp.comp.ix()].path)),
            super::CaseBinding::Knob { comp, .. } => Some(format!("knob {}", unit.circuit.comps[comp.comp.ix()].path)),
            super::CaseBinding::Pipe { comp } => Some(format!("pipe {}", unit.circuit.comps[comp.comp.ix()].path)),
            _ => None,
        }).collect();
        bound.sort();
        assert_eq!(bound, ["key key", "knob pitch", "pipe lamp.d"], "the keycap presses the key, the knob turns the pitch pot and the pipe shows the LED");
        // Listen hears the jack, found by its audio role: both channels against its sleeve
        let output = case.output.expect("the board's output jack, by its audio role");
        let jacks = fab_elec_bench::audio::output_jacks(&unit.circuit);
        let jack = jacks.iter().find(|j| j.comp == output.comp).expect("the output is an output jack");
        let name = |net: fab_elec_core::ids::NetId| unit.circuit.nets[net.ix()].name.clone();
        assert_eq!((output.board, jack.stereo()), (unit.id, true), "the jack is the board's stereo jack");
        assert_eq!(jack.channels().map(name).collect::<Vec<_>>(), ["OUT", "OUT"], "both channels are the tone's net");
        assert_eq!(unit.circuit.nets[jack.reference.ix()].kind, fab_elec_circuit::NetKind::Ground, "the sleeve is ground");
    }

    #[test]
    fn the_tone_box_moves_between_the_kit_and_its_live_closed_poses_in_plan_order() {
        let (case, _, _) = tone_box();
        let board = case.board.as_ref().expect("the excluded board has a linked placement");
        let id = |p: &ClosedPart| ElementId::from_index(p.part);
        let elements: Vec<_> = case.placements().map(|p| Some(id(p))).collect();
        let held: Vec<_> = case.bindings().filter_map(|b| match b { CaseBinding::Key { comp, .. } => Some(comp), _ => None }).collect();
        let turned: Vec<_> = case.bindings().filter_map(|b| match b { CaseBinding::Knob { comp, .. } => Some((comp, 0.75)), _ => None }).collect();
        let show = |position, set: &mut PoseSet| pose(case, &elements, &[], position, &held, &turned, &[], set);

        // Reconstruct the old closed transform directly in the board frame, including live input.
        let mut set = PoseSet::default();
        show(1.0, &mut set);
        for p in case.placements() {
            let mut frame = p.to_board;
            match p.binding {
                CaseBinding::Key { travel, .. } => frame[2][3] -= travel,
                CaseBinding::Knob { axis, .. } => {
                    frame = affine_mul(&turn_about_z(axis, -0.25 * fab_elec_parts::SHAFT_TURN_DEG.to_radians()), &frame);
                }
                _ => {}
            }
            assert_eq!(set.get(id(p)), Some(metres(&affine_mul(&case.plate_from_board, &frame))),
                "assembled placement {} has its closed transform, including key travel and knob turn", p.part);
        }
        let closed = set.clone();
        let lid = case.plan.steps.iter().position(|s| s.kind == StepKind::Lid).expect("the lid step");
        let steps = case.plan.steps.len() as f64;
        show((lid as f64 + 0.25) / steps, &mut set);
        for &part in &case.plan.steps[lid].parts {
            let element = ElementId::from_index(part);
            let half = set.get(element).expect("the lid is turning on its way to the approach point");
            let seat = closed.get(element).expect("the lid has a seat");
            for r in 0..3 {
                for c in 0..3 {
                    let twice: f32 = (0..3).map(|k| half[r][k] * half[k][c]).sum();
                    assert!((twice - seat[r][c]).abs() < 1e-5, "two halfway rotations give the lid's seated rotation");
                }
            }
        }
        show((lid as f64 + 0.75) / steps, &mut set);
        assert_eq!(set.get(id(board)), closed.get(id(board)), "the earlier board step is seated in the base");
        for &part in &case.plan.steps[lid].parts {
            let id = ElementId::from_index(part);
            let now = set.get(id).expect("the current lid step is moving");
            let seat = closed.get(id).expect("the lid has a seat");
            for r in 0..3 {
                let direction = (0..3).map(|c| case.plate_from_board[r][c] * case.plan.steps[lid].from[c]).sum::<f64>();
                assert!((f64::from(now[r][3] - seat[r][3]) - direction * APPROACH_MM * 0.0005).abs() < 1e-6,
                    "the lid and pipe approach their seats along the step direction, plate axis {r}");
                assert_eq!(now[r][..3], seat[r][..3], "the approaching lid has finished its rotation");
            }
        }
        let knob = case.parts.iter().find(|p| matches!(p.binding, CaseBinding::Knob { .. })).expect("the knob");
        assert!(!set.contains(id(knob)), "the later knob step is still at its print placement");
        let board_step = case.plan.steps.iter().position(|s| s.kind == StepKind::Board).expect("the board step");
        show(assembly_target(Some(board_step), false, case.plan.steps.len()), &mut set);
        assert_eq!(set.get(id(board)), closed.get(id(board)), "stopped at the board's build step, the board is in its base");
        assert!(!set.contains(id(knob)), "the later knob step waits at its print placement");
        assert_eq!(assembly_target(Some(99), true, case.plan.steps.len()), 1.0, "a step past the plan's end is the assembled device");
        show(0.0, &mut set);
        assert!(set.is_empty(), "kit leaves every case part and the board at their print placements");
    }

    #[test]
    fn the_parts_of_one_step_set_off_across_the_board_and_a_glued_body_goes_with_its_host() {
        let at = |part, step, x| ClosedPart {
            part, step, to_board: [[1.0, 0.0, 0.0, x], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            centre: [0.0; 3], binding: CaseBinding::None, start: 0.0, span: 1.0,
        };
        let mut parts = vec![at(0, Some(0), 0.0), at(1, Some(1), 30.0), at(2, Some(1), 10.0), at(3, Some(1), 20.0), at(4, Some(1), 40.0)];
        // part 4 is printed glued to part 1
        stagger(&mut parts, |part| (part == 1 || part == 4).then(|| "cap".to_string()));
        assert_eq!((parts[0].start, parts[0].span), (0.0, 1.0), "a part alone in its step travels the whole step");
        assert_eq!([parts[2].start, parts[3].start, parts[1].start], [0.0, STAGGER / 2.0, STAGGER], "the step's movers set off in order across the board");
        assert_eq!(parts[4].start, parts[1].start, "a glued body sets off with its host");
        assert!(parts[1..].iter().all(|p| p.start + p.span <= 1.0 + 1e-12), "every mover arrives by the step's end");
    }

    /// Kit reads the plate the scene draws: the tone box's board, which its plate seats in its
    /// base, lies out flat on the bed clear of every printed part; nothing else moves.
    #[test]
    fn kit_lays_the_tone_box_board_out_from_the_plate_the_scene_draws() {
        let (case, _, plate) = tone_box();
        let kits = kit_moves(case, &plate.parts, (plate.bed_min, plate.bed_max));
        let (part, (min, max)) = board_rest(case, &plate.parts).expect("the board layer draws the board in its holder's frame");
        let base: Vec<_> = case.plan.steps.iter().filter(|s| s.kind == StepKind::Base).flat_map(|s| s.parts.iter().map(|&i| (plate.parts[i].min_mm, plate.parts[i].max_mm))).collect();
        assert!(base.iter().any(|(lo, hi)| (0..2).all(|d| min[d] < hi[d] && lo[d] < max[d])), "the plate seats the board in its base: board {min:?}..{max:?}, base {base:?}");
        let moved: Vec<_> = case.placements().zip(&kits).filter(|(_, kit)| kit.is_some()).map(|(p, _)| p.part).collect();
        assert_eq!(moved, [part], "Kit moves the board alone");
        let kit = kits.iter().flatten().next().expect("the board's kit move");
        let (lo, hi) = (affine_point(kit, min), affine_point(kit, max));
        assert!(lo[2].abs() < 1e-9, "it lies flat on the bed, its bottom at {}", lo[2]);
        for (i, p) in plate.parts.iter().enumerate().filter(|(i, p)| p.printed && *i != part) {
            assert!(!(0..2).all(|d| lo[d] < p.max_mm[d] && p.min_mm[d] < hi[d]), "laid out at {lo:?}..{hi:?} it clears printed part {i} at {:?}..{:?}", p.min_mm, p.max_mm);
        }
    }

    #[test]
    fn kit_lays_a_board_in_its_base_out_in_front_and_its_step_carries_it_in() {
        let base = ([0.0, 0.0, 0.0], [100.0, 80.0, 20.0]);
        let keys = ([20.0, -80.0, 0.0], [80.0, -40.0, 10.0]);
        let lid = ([110.0, 0.0, 0.0], [200.0, 80.0, 10.0]);
        let (bed, small_bed) = (([-200.0, -200.0], [300.0, 300.0]), ([-10.0, -50.0], [210.0, 90.0]));
        let board = ([10.0, 10.0, 3.0], [90.0, 70.0, 5.0]);
        assert_eq!(kit_move(([150.0, 0.0, 0.0], [200.0, 40.0, 2.0]), &[base], &[base, keys], bed), None, "a board resting outside its base stays where it is placed");
        let kit = kit_move(board, &[base], &[base], bed).expect("a board placed in its base is laid out");
        assert_eq!([kit[0][3], kit[1][3], kit[2][3]], [0.0, -KIT_GAP_MM - 70.0, -3.0], "it lies clear in front of its base, flat on the bed");
        let kit = kit_move(board, &[base], &[base, keys], bed).expect("a board placed in its base is laid out");
        assert_eq!([kit[0][3], kit[1][3]], [100.0 + KIT_GAP_MM - 10.0, 0.0], "with printed parts in front of the base it lies to the right");
        let shifted = kit_move(board, &[base], &[base], small_bed).expect("laid out");
        assert_eq!([shifted[0][3], shifted[1][3]], [100.0 + KIT_GAP_MM - 10.0, 0.0], "where the front runs off the bed it lies on the bed to the right");
        let crowded = kit_move(board, &[base], &[base, lid], small_bed).expect("laid out");
        assert_eq!([crowded[0][3], crowded[1][3]], [0.0, -50.0 - KIT_GAP_MM - 70.0], "with no clear side on the bed it lies in front of the bed");

        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let board = ClosedPart { part: 0, step: Some(1), to_board: identity, centre: [50.0, 40.0, 4.0], binding: CaseBinding::None, start: 0.0, span: 1.0 };
        let step = |kind| fab_elec_mech::AssemblyStep { kind, parts: Vec::new(), from: [0.0, 0.0, 1.0] };
        let case = ClosedCase { plate_from_board: identity, plan: AssemblyPlan { steps: vec![step(StepKind::Base), step(StepKind::Board)] }, parts: Vec::new(), board: Some(board), board_outline: None, output: None };
        let element = ElementId::from_index(0);
        let mut set = PoseSet::default();
        let mut show = |position| { pose(&case, &[Some(element)], &[Some(kit)], position, &[], &[], &[], &mut set); set.get(element) };
        assert_eq!(show(0.0), Some(metres(&kit)), "Kit shows the board where it is laid out");
        assert_eq!(show(0.25), Some(metres(&kit)), "before its step it waits where it is laid out");
        let start = show(0.5 + 1e-9).expect("its step carries it");
        assert!((start[1][3] - metres(&kit)[1][3]).abs() < 1e-6, "its step sets off from where Kit showed it");
        assert_eq!(show(1.0), Some(metres(&identity)), "assembled, it sits in its base");
    }
}
