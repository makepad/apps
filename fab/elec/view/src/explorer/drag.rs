//! A drag owns a retained copy and clocks, never a second store of part positions.
//! Only Hint previews change the design; pointer samples change uniforms. Scene publication,
//! frame indexing and copying happen at gesture/layout boundaries, not at input rate.

use super::{layout_job::DragAnchor, scene::{Scene, Text, NO_ITEM}, view::{draw_text, Templates, UpTurn}};
use crate::{
    canvas::{
        chips::{Chip, ChipAnchor, ChipKind, ChipLayer},
        draw::{BoxInst, CanvasLayer, CanvasLive, CanvasUniforms, Common, DrawCanvasArc, DrawCanvasBox, DrawCanvasLine},
        looks::{ease_out, MOVE_SECONDS},
        Camera2d, Look, Tokens,
    },
    ViewAction,
};
use crate::snapshot::next_tag;
use fab_elec_circuit::{Circuit, CircuitOp, SheetHint, SheetHints};
use fab_elec_core::{BlockId, BoardId, SchPt, Sel};
use fab_elec_design::DesignOp;
use fab_elec_explore::{plan::GroupOrientation, sheet::{self, Mover}, Opts, SheetLayout};
use makepad_widgets_core::*;
use std::{collections::{HashMap, HashSet}, sync::Arc, time::Instant};

/// The frames a drop may not land in: every placed frame except the hint's block, its ancestors
/// and carried blocks. A sample scans them: a few hundred rectangles even on a large sheet.
#[derive(Default)]
struct Regions(Vec<[i32; 4]>);

impl Regions {
    fn new(sheet: &SheetLayout, circuit: &Circuit, block: u32, carried: &[bool]) -> Self {
        let mut ancestors = HashSet::new();
        let mut parent = Some(block);
        while let Some(b) = parent {
            if !ancestors.insert(b) { break; }
            parent = circuit.tree.get(b as usize).and_then(|b| b.parent).map(|b| b.0);
        }
        Self(sheet.frames.iter().enumerate()
            .filter(|(b, frame)| frame.placed && !ancestors.contains(&(*b as u32)) && carried.get(*b) != Some(&true))
            .map(|(_, frame)| frame.rect)
            .collect())
    }

    fn contains(&self, at: [f64; 2]) -> bool {
        self.0.iter().any(|r| at[0] >= r[0] as f64 && at[0] <= r[2] as f64 && at[1] >= r[1] as f64 && at[1] <= r[3] as f64)
    }
}

fn carried_blocks(circuit: &Circuit, block: u32) -> Vec<bool> {
    (0..circuit.tree.len()).map(|n| {
        let mut parent = Some(n as u32);
        for _ in 0..circuit.tree.len() {
            let Some(b) = parent else { break };
            if b == block { return true; }
            parent = circuit.tree.get(b as usize).and_then(|b| b.parent).map(|b| b.0);
        }
        false
    }).collect()
}

#[derive(Default)]
struct Measure {
    samples: u64,
    ui_sum: f64,
    ui_max: f64,
    layouts: u64,
    worker_sum: f64,
    worker_max: f64,
}

impl Measure {
    fn report(&self, refdes: &str) {
        log!("fab.explorer: drag {}: {} samples, ui mean {:.2} us max {:.2} us, {} layouts, worker mean {:.2} ms max {:.2} ms",
            refdes, self.samples, self.ui_sum / self.samples.max(1) as f64, self.ui_max,
            self.layouts, self.worker_sum / self.layouts.max(1) as f64, self.worker_max);
    }
}

#[derive(Clone, Copy)]
enum Return {
    /// Keep the visible copy steady until the host's committed layout is available.
    Waiting { gen: Option<u64> },
    Flying { start: f64, offset: [f32; 2] },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Grip { Part(u32), Block(u32) }

/// Another head in the grip's block, with its offset from the grip in hint coordinates (Y down).
struct OtherHead {
    path: String,
    hint: SheetHint,
    offset: [i64; 2],
}

/// Turn or reflect the carried heads about the grip in one pass, without allocating.
fn orient_carried(grip: &mut SheetHint, also: &mut [OtherHead], orient: GroupOrientation) {
    orient.apply(grip, [0, 0]);
    for head in also {
        head.offset = orient.apply(&mut head.hint, head.offset);
    }
}

/// Place the grip at the local pointer target and write one preview hint per head.
fn carried_parts(path: &str, grip: &SheetHint, also: &[OtherHead], [x, y]: [i32; 2]) -> Option<Vec<(String, SheetHint)>> {
    let mut parts = Vec::with_capacity(1 + also.len());
    parts.push((path.to_owned(), SheetHint { at: Some(SchPt::new(x, y)), ..grip.clone() }));
    for head in also {
        let x = i32::try_from(i64::from(x) + head.offset[0]).ok()?;
        let y = i32::try_from(i64::from(y) + head.offset[1]).ok()?;
        parts.push((head.path.clone(), SheetHint { at: Some(SchPt::new(x, y)), ..head.hint.clone() }));
    }
    Some(parts)
}

struct Moving {
    grip: Grip,
    row: u32,
    block: u32,
    unit: BoardId,
    path: String,
    refdes: String,
    label: String,
    block_name: String,
    hint: SheetHint,
    /// The group's distinct source paths, shared with each layout request.
    group: Option<Arc<HashSet<String>>>,
    also: Vec<OtherHead>,
    /// A keyboard move follows the pointer until its next click.
    click_to_drop: bool,
    /// How R turns the part (`PartPlace::turn_step`: quarters, halves, or 0 when its turn follows
    /// its host or chain); 0 for a block or a selection.
    turn_step: u8,
    turned: bool,
    /// The grip pose of the laid-out group. It identifies the orientation of the retained rows.
    group_layout_pose: Option<(u8, bool)>,
    start: [f64; 2],
    origin: [i32; 2],
    current: [i32; 2],
    target: [i32; 2],
    legal: [i32; 2],
    content: [i32; 4],
    regions: Regions,
    refused: bool,
    last_preview: Option<[i32; 2]>,
    /// The host sample a release commits, including a replacement with drop hints.
    last_tag: Option<u64>,
    /// The complete sample shared by the preview and its release.
    last_hints: Option<Arc<SheetHints>>,
    /// Released where it was last previewed: the drop's other hints are on the layout worker
    /// (`Drag::dropped`), and the copy holds still until they are in.
    committing: bool,
    chip_at: Option<([i32; 2], bool)>,
    /// The bottom-left corner of what the copy carries (its rows and its rail stubs and tags),
    /// relative to the dragged origin: the note chip hangs below it, clear of pins and tags.
    below: [f64; 2],
    members: Vec<bool>,
    blocks: Vec<bool>,
    frame: [f64; 4],
    boxes: CanvasLayer,
    lines: CanvasLayer,
    arcs: CanvasLayer,
    ghost: CanvasLayer,
    texts: Vec<Text>,
    returning: Option<Return>,
    measure: Measure,
}

impl Moving {
    fn content(sheet: &SheetLayout, block: u32) -> Option<[i32; 4]> {
        let frame = sheet.frames.get(block as usize).filter(|f| f.placed)?;
        let pad = Opts::default().pad;
        let [x0, y0, x1, y1] = frame.rect;
        Some([x0.saturating_add(pad).min(x1), y0.saturating_add(pad).min(y1),
            x1.saturating_sub(pad).max(x0.saturating_add(pad).min(x1)),
            y1.saturating_sub(pad).max(y0.saturating_add(pad).min(y1))])
    }

    fn clamp(&self, at: [i32; 2]) -> [i32; 2] {
        [at[0].clamp(self.content[0], self.content[2]), at[1].clamp(self.content[1], self.content[3])]
    }

    fn offset(&self, now: f64) -> [f32; 2] {
        if let Some(Return::Flying { start, offset }) = self.returning {
            let remain = (1.0 - ease_out((now - start) / MOVE_SECONDS)) as f32;
            return offset.map(|v| v * remain);
        }
        std::array::from_fn(|i| (self.target[i] as f64 - self.current[i] as f64) as f32)
    }

    fn copy_visible(&self) -> bool {
        self.group_layout_pose.is_none_or(|pose| pose == (self.hint.turn, self.hint.mirror))
    }

    fn preview(&mut self, at: [i32; 2], sheet: &SheetLayout) -> Option<ViewAction> {
        let local = sheet::to_local(sheet, self.block, at)?;
        let parts = carried_parts(&self.path, &self.hint, &self.also, local)?;
        let ops = parts.iter().map(|(path, hint)| self.hint_op(path, hint.clone())).collect();
        self.last_preview = Some(at);
        self.last_hints = Some(Arc::new(SheetHints { parts, ..Default::default() }));
        let tag = next_tag();
        self.last_tag = Some(tag);
        Some(ViewAction::Preview { notes: vec![], tag, place: vec![], ops, label: self.label.clone() })
    }

    fn hint_op(&self, path: &str, hint: SheetHint) -> DesignOp {
        DesignOp::Circuit { unit: self.unit, op: CircuitOp::Hint { path: path.to_owned(), hint: Some(hint) } }
    }

    fn preview_orientation(&mut self, cx: &mut Cx, uid: WidgetUid, sheet: &SheetLayout, chips: &mut ChipLayer, now: f64) {
        self.turned = true;
        self.last_preview = None;
        if !self.refused || self.group.is_some() {
            if let Some(action) = self.preview(self.legal, sheet) { cx.widget_action(uid, action); }
        }
        self.chip_at = None;
        self.chip(chips, now);
    }

    fn chip(&mut self, chips: &mut ChipLayer, now: f64) {
        let key = (self.target, self.refused);
        if self.chip_at == Some(key) { return; }
        self.chip_at = Some(key);
        let mut text = if self.refused { "Another block: a code change".into() }
            else if self.turn_step > 0 { format!("Move in {} · R turns", self.block_name) }
            else if self.group.is_some() { format!("{} in {}", self.label, self.block_name) }
            else { format!("Move in {}", self.block_name) };
        if self.click_to_drop { text.push_str(" · click to drop"); }
        chips.set(vec![Chip {
            id: 0,
            kind: if self.refused { ChipKind::Refused } else { ChipKind::Info },
            text,
            anchor: ChipAnchor::World {
                at: [self.target[0] as f64 + self.below[0], self.target[1] as f64 + self.below[1]],
                dx: 0.0, dy: 6.0,
            },
            align: [0.0, 0.0],
        }], now);
    }

    fn belongs(&self, scene: &Scene, item: f32) -> bool {
        item >= 0.0 && (scene.part_node(item as u32)
            .is_some_and(|n| self.members.get(n as usize) == Some(&true))
            || (!self.blocks.is_empty() && scene.block_node(item as u32)
                .is_some_and(|b| self.blocks.get(b as usize) == Some(&true))))
    }

    fn copy(&mut self, scene: &mut Scene, templates: &Templates, tokens: &Tokens) {
        let common = |c: Common| Common {
            item: self.row as f32, look: Look::Preview.code(),
            pose_a: [0.0, 0.0, 0.0, 1.0], pose_b: [0.0, 0.0, 0.0, 1.0], span: [0.0; 2], ..c
        };
        let mut boxes = Vec::new();
        let mut lines = Vec::new();
        let mut arcs = Vec::new();
        for row in scene.boxes.iter().filter(|i| self.belongs(scene, i.c.item)) {
            templates.boxes.append(&mut boxes, &BoxInst { c: common(row.c), ..*row });
        }
        for row in scene.lines.iter().filter(|i| self.belongs(scene, i.c.item)) {
            templates.line.append(&mut lines, &crate::canvas::draw::LineInst {
                c: common(row.c), follow: [0.0; 2], ..*row
            });
        }
        for row in scene.arcs.iter().filter(|i| self.belongs(scene, i.c.item)) {
            templates.arc.append(&mut arcs, &crate::canvas::draw::ArcInst { c: common(row.c), ..*row });
        }
        self.texts.clear();
        self.texts.extend(scene.texts.iter().filter(|t| t.owner
            .is_some_and(|n| self.members.get(n as usize) == Some(&true))
            || (!self.blocks.is_empty() && t.item != NO_ITEM && scene.block_node(t.item)
                .is_some_and(|b| self.blocks.get(b as usize) == Some(&true)))).cloned());
        self.boxes.set(templates.boxes.slots(), boxes);
        self.lines.set(templates.line.slots(), lines);
        self.arcs.set(templates.arc.slots(), arcs);
        let mut ghost = Vec::new();
        let mut outline = |body: [f64; 4], corner| {
            templates.boxes.append(&mut ghost, &BoxInst {
                c: Common { item: self.row as f32, look: Look::Ghost.code(), ..Common::at([0.0; 3], tokens.selection) },
                rect: [((body[0] + body[2]) * 0.5) as f32, ((body[1] + body[3]) * 0.5) as f32,
                    ((body[2] - body[0]) * 0.5) as f32, ((body[3] - body[1]) * 0.5) as f32],
                corner, stroke: [1.0, 0.0], fill: [0.0; 4],
            });
        };
        match self.grip {
            Grip::Part(_) if self.group.is_some() => {
                for (node, &member) in self.members.iter().enumerate() {
                    if member {
                        if let Some(body) = scene.part_box(node as u32) { outline(body, 0.4); }
                    }
                }
            }
            Grip::Part(node) => if let Some(body) = scene.part_box(node) { outline(body, 0.4); },
            Grip::Block(_) => outline(self.frame, 1.0),
        }
        self.ghost.set(templates.boxes.slots(), ghost);
        scene.follow(&self.members);
        self.below = self.carried(scene);
        self.chip_at = None;
    }

    /// The bottom-left corner of the copy's rows, its short attached strokes (rail stubs, which
    /// `Scene::follow` marks) and the rail tags at their ends, relative to the dragged origin.
    fn carried(&self, scene: &Scene) -> [f64; 2] {
        /// The longest attached stroke counted as a rail stub, and the room its tag takes.
        const STUB: f64 = 3.0;
        const TAG: f64 = 1.5;
        let mut low = [f64::INFINITY; 2];
        let mut grow = |x: f64, y: f64| low = [low[0].min(x), low[1].min(y)];
        for row in &scene.lines {
            let [ax, ay, bx, by] = row.seg.map(f64::from);
            if self.belongs(scene, row.c.item) {
                grow(ax, ay);
                grow(bx, by);
            } else if row.follow != [0.0; 2] && (bx - ax).hypot(by - ay) <= STUB {
                grow(ax.min(bx) - TAG, ay.min(by) - TAG);
            }
        }
        for row in scene.boxes.iter().filter(|i| self.belongs(scene, i.c.item)) {
            grow((row.rect[0] - row.rect[2]) as f64, (row.rect[1] - row.rect[3]) as f64);
        }
        for row in scene.arcs.iter().filter(|i| self.belongs(scene, i.c.item)) {
            grow((row.circle[0] - row.circle[2]) as f64, (row.circle[1] - row.circle[2]) as f64);
        }
        for text in &self.texts {
            grow(text.at[0], text.at[1] - text.size);
        }
        if !low[0].is_finite() {
            return [0.0; 2];
        }
        [low[0] - self.current[0] as f64, low[1] - self.current[1] as f64]
    }
}

/// What a drag's release asks of the view.
pub(super) enum Finish {
    /// Nothing ended.
    None,
    /// The drag ended: publish the sheet's connection rows once.
    Done,
    /// Committing: lay out the drop's other hints (`LayoutJobs::drop_hints`), then `Drag::dropped`.
    Drop { path: String, hint: Arc<SheetHints>, token: u64 },
}

#[derive(Default)]
pub(super) struct Drag {
    moving: Option<Moving>,
    /// Names the release whose drop hints are on the worker.
    token: u64,
    /// The last dragged part's block and its origin when that drag began: relayouts keep it
    /// there (`Keep`), so the block grows where the part goes and a drop or a cancel lands
    /// without moving the rest of the sheet.
    anchor: Option<(u32, [i32; 2])>,
    rides: bool,
    pinned: bool,
    pinned_at: [f64; 2],
    chips: ChipLayer,
    /// Keep the final/cancel layout's reflow animated even after release clears the copy.
    finish_motion: bool,
    reveal_logged: bool,
}

/// Removing a drop's retained hints lets automatic placement take over in one undoable edit.
pub(super) fn unpin_ops(circuit: &Circuit, unit: BoardId, node: u32, block: bool) -> Vec<DesignOp> {
    let Some(comp) = circuit.comps.get(node as usize) else { return Vec::new() };
    let mut pinned: HashSet<&str> = circuit.hints.parts.iter().map(|(path, _)| path.as_str()).collect();
    let parts = if block { circuit.comps.as_slice() } else { std::slice::from_ref(comp) };
    parts.iter()
        .filter(|part| part.block == comp.block && pinned.remove(part.path.as_str()))
        .map(|part| DesignOp::Circuit {
            unit, op: CircuitOp::Hint { path: part.path.clone(), hint: None },
        })
        .collect()
}

/// Release a block's hint, optionally including every hinted block and part it carries.
pub(super) fn unpin_block_ops(circuit: &Circuit, unit: BoardId, block: u32, all: bool) -> Vec<DesignOp> {
    if block == 0 { return Vec::new(); }
    let Some(target) = circuit.tree.get(block as usize) else { return Vec::new() };
    let mut pinned: HashSet<&str> = circuit.hints.parts.iter().map(|(path, _)| path.as_str()).collect();
    let mut ops = Vec::new();
    let mut remove = |path: &str| {
        if pinned.remove(path) {
            ops.push(DesignOp::Circuit {
                unit, op: CircuitOp::Hint { path: path.to_owned(), hint: None },
            });
        }
    };
    remove(&target.path);
    if all {
        let blocks = carried_blocks(circuit, block);
        for (b, child) in circuit.tree.iter().enumerate() {
            if b as u32 != block && blocks[b] { remove(&child.path); }
        }
        for part in &circuit.comps {
            if blocks.get(part.block.0 as usize) == Some(&true) { remove(&part.path); }
        }
    }
    ops
}

impl Drag {
    pub fn busy(&self) -> bool { self.moving.is_some() || self.rides }
    pub fn anchor(&self) -> Option<DragAnchor> {
        let (block, origin) = self.anchor?;
        let heads = self.moving.as_ref().filter(|m| m.returning.is_none()).and_then(|m| m.group.clone());
        Some(DragAnchor { block, origin, heads })
    }
    /// A Library carry's block and its origin: like a dragged part's, later layouts (the
    /// previews, then the commit's or the cancel's) keep that block where it was.
    pub fn keep(&mut self, anchor: (u32, [i32; 2])) { self.anchor = Some(anchor); }
    pub fn rides(&self) -> bool { self.rides }
    pub fn held(&self) -> bool { self.moving.as_ref().is_some_and(|m| m.returning.is_none() && !m.committing) }
    pub fn click_to_drop(&self) -> bool {
        self.held() && self.moving.as_ref().is_some_and(|m| m.click_to_drop)
    }
    /// The part a drag moves while its hint is live in the design: relayouts let it lead.
    pub fn lead(&self) -> Option<String> {
        self.moving.as_ref().filter(|m| m.returning.is_none() && m.last_preview.is_some()).map(|m| m.path.clone())
    }
    pub fn reflow(&self) -> bool { self.moving.is_some() || self.finish_motion }
    pub fn member(&self, node: u32) -> bool {
        self.moving.as_ref().is_some_and(|m| m.members.get(node as usize) == Some(&true))
    }

    pub fn inside_reveal(&mut self) {
        if !self.reveal_logged {
            log!("fab.explorer: drag is off inside a reveal");
            self.reveal_logged = true;
        }
    }

    pub fn show_rides(&mut self, node: u32, host: u32, port: u32, circuit: &Circuit, scene: &Scene, now: f64) {
        if self.busy() { return; }
        let Some(host) = circuit.comps.get(host as usize) else { return };
        let Some(body) = scene.part_box(node) else { return };
        let offset = scene.offset(node, now);
        let pin = host.part.pins.get(port as usize).map_or("?", |p| p.name.as_str());
        self.pinned = false;
        self.chips.set(vec![Chip {
            id: 0, kind: ChipKind::Info, text: format!("Rides with {} {}", host.refdes, pin),
            anchor: ChipAnchor::World { at: [body[2] + offset[0] + 1.0, body[3] + offset[1] + 1.0], dx: 0.0, dy: 0.0 },
            align: [0.0, 1.0],
        }], now);
        self.rides = true;
    }

    pub fn release_rides(&mut self, now: f64) {
        if self.rides {
            self.rides = false;
            self.chips.set(Vec::new(), now);
        }
    }

    /// The hover chip of a pinned part or block: how to hand it back to automatic placement. It is an
    /// overlay, so a hover never changes the layout, and it never blocks a drag.
    pub fn show_pinned(&mut self, anchor: [f64; 2], now: f64) {
        if self.busy() { return; }
        if self.pinned && self.pinned_at == anchor { return; }
        self.chips.set(vec![Chip {
            id: 0, kind: ChipKind::Info, text: "Pinned · U: auto-place".into(),
            anchor: ChipAnchor::World { at: anchor, dx: 0.0, dy: 0.0 },
            align: [0.0, 1.0],
        }], now);
        self.pinned = true;
        self.pinned_at = anchor;
    }

    pub fn release_pinned(&mut self, now: f64) {
        if !self.pinned { return; }
        self.pinned = false;
        self.chips.set(Vec::new(), now);
    }

    /// Resolve the pressed part and its selection once. The retained copy and wire endpoints
    /// share a membership mask until release. The returned row anchors the moving pins and glows.
    pub fn begin(&mut self, node: u32, selected: &[(u32, String)], from: [f64; 2], click_to_drop: bool, circuit: &Circuit, unit: BoardId,
        sheet: &SheetLayout, scene: &mut Scene, templates: &Templates, tokens: &Tokens, now: f64) -> Option<u32>
    {
        if self.busy() { return None; }
        let group = selected.len() > 1;
        let mover = |n: u32| sheet.movers.get(n as usize).copied().unwrap_or_default();
        let head = |n| match mover(n) {
            Mover::Itself => Some(n),
            Mover::Head(h) => Some(h),
            Mover::Rides { .. } => None,
        };
        let mut heads = HashSet::new();
        let mut ordered = Vec::new();
        for (n, path) in selected {
            if &circuit.comps.get(*n as usize)?.path != path { return None; }
            if let Some(h) = head(*n) {
                if heads.insert(h) { ordered.push(h); }
            }
        }
        let node = match mover(node) {
            Mover::Itself => node,
            Mover::Head(head) => head,
            Mover::Rides { host, port } => {
                if let Some(h) = head(host).filter(|h| heads.contains(h)).or_else(|| ordered.first().copied()) { h }
                else {
                    self.show_rides(node, host, port, circuit, scene, now);
                    return None;
                }
            }
        };
        let part = sheet.parts.get(node as usize).filter(|p| p.placed)?;
        let comp = circuit.comps.get(node as usize)?;
        let row = scene.part_item(node)?;
        let content = Moving::content(sheet, part.block)?;
        let path = circuit.tree.get(part.block as usize).map_or("", |b| b.path.as_str());
        let block_name = if path.is_empty() { circuit.name.as_str() } else { path.rsplit('.').next().unwrap_or(path) };
        let stored: HashMap<_, _> = circuit.hints.parts.iter().map(|(p, hint)| (p.as_str(), hint)).collect();
        let hint_for = |path: &str, place: &sheet::PartPlace| {
            let hint = stored.get(path).copied().cloned().unwrap_or_default();
            SheetHint { turn: place.turn, mirror: place.mirror, ..hint }
        };
        let mut also = Vec::new();
        for h in ordered.into_iter().filter(|&h| h != node) {
            let other = sheet.parts.get(h as usize).filter(|p| p.placed)?;
            if other.block != part.block {
                self.pinned = false;
                self.rides = true;
                self.chips.set(vec![Chip {
                    id: 0, kind: ChipKind::Refused, text: "Parts in several blocks: move them one at a time".into(),
                    anchor: ChipAnchor::World { at: from, dx: 0.0, dy: 6.0 }, align: [0.0, 0.0],
                }], now);
                return None;
            }
            let c = circuit.comps.get(h as usize)?;
            also.push(OtherHead {
                path: c.path.clone(), hint: hint_for(&c.path, other),
                offset: [i64::from(other.at[0]) - i64::from(part.at[0]), i64::from(part.at[1]) - i64::from(other.at[1])],
            });
        }
        heads.insert(node);
        let members: Vec<_> = (0..sheet.parts.len()).map(|n| match mover(n as u32) {
            Mover::Itself => heads.contains(&(n as u32)),
            Mover::Head(h) => heads.contains(&h),
            Mover::Rides { host, .. } => group && head(host).is_some_and(|h| heads.contains(&h)),
        }).collect();
        let label = if group {
            let count = selected.iter().filter(|(n, _)| members.get(*n as usize) == Some(&true)).count();
            if count == 1 { "Move 1 part".into() } else { format!("Move {count} parts") }
        } else { format!("Move {}", comp.refdes) };
        let group_paths = group.then(|| Arc::new(std::iter::once(comp.path.clone())
            .chain(also.iter().map(|h| h.path.clone())).collect()));
        self.anchor = sheet::block_origin(sheet, part.block).map(|origin| (part.block, origin));
        let mut moving = Moving {
            grip: Grip::Part(node), row, block: part.block, unit, path: comp.path.clone(), refdes: comp.refdes.clone(),
            label, block_name: block_name.into(), hint: hint_for(&comp.path, part), group: group_paths, also, click_to_drop,
            turn_step: if group { 0 } else { part.turn_step }, turned: false,
            group_layout_pose: group.then_some((part.turn, part.mirror)),
            start: from, origin: part.at, current: part.at, target: part.at, legal: part.at,
            content, regions: Regions::new(sheet, circuit, part.block, &[]),
            refused: false, last_preview: None, last_tag: None, last_hints: None, committing: false, chip_at: None, below: [0.0; 2],
            members, blocks: Vec::new(), frame: [0.0; 4],
            boxes: CanvasLayer::default(), lines: CanvasLayer::default(), arcs: CanvasLayer::default(),
            ghost: CanvasLayer::default(), texts: Vec::new(), returning: None, measure: Measure::default(),
        };
        moving.copy(scene, templates, tokens);
        self.pinned = false;
        moving.chip(&mut self.chips, now);
        self.moving = Some(moving);
        Some(row)
    }

    /// Returns true when sheet connection rows changed and need one publication.
    pub fn begin_block(&mut self, block: u32, from: [f64; 2], circuit: &Circuit, unit: BoardId,
        sheet: &SheetLayout, scene: &mut Scene, templates: &Templates, tokens: &Tokens, now: f64) -> bool
    {
        if self.busy() { return false; }
        if sheet.block_movable.get(block as usize) != Some(&true) { return false; }
        let Some(child) = circuit.tree.get(block as usize) else { return false };
        let Some(parent) = child.parent.map(|b| b.0) else { return false };
        let Some(frame) = sheet.frames.get(block as usize).filter(|f| f.placed) else { return false };
        let Some(origin) = sheet::block_origin(sheet, block) else { return false };
        let Some(row) = scene.item_of(&Sel::Block(unit, BlockId(block))) else { return false };
        let Some(content) = Moving::content(sheet, parent) else { return false };
        self.anchor = sheet::block_origin(sheet, parent).map(|origin| (parent, origin));
        let path = circuit.tree.get(parent as usize).map_or("", |b| b.path.as_str());
        let block_name = if path.is_empty() { circuit.name.as_str() } else { path.rsplit('.').next().unwrap_or(path) };
        let name = child.path.rsplit('.').next().unwrap_or(&child.path);
        let hint = circuit.hints.parts.iter().rev().find(|(path, _)| path == &child.path)
            .map(|(_, hint)| hint.clone()).unwrap_or_default();
        let blocks = carried_blocks(circuit, block);
        let mut moving = Moving {
            grip: Grip::Block(block), row, block: parent, unit, path: child.path.clone(), refdes: name.into(),
            label: format!("Move {name}"), block_name: block_name.into(), hint, group: None, also: Vec::new(), click_to_drop: false, turn_step: 0, turned: false,
            group_layout_pose: None,
            start: from, origin, current: origin, target: origin, legal: origin,
            content, regions: Regions::new(sheet, circuit, parent, &blocks),
            refused: false, last_preview: None, last_tag: None, last_hints: None, committing: false, chip_at: None, below: [0.0; 2],
            members: sheet.parts.iter().map(|part| blocks.get(part.block as usize) == Some(&true)).collect(),
            blocks, frame: frame.rect.map(f64::from),
            boxes: CanvasLayer::default(), lines: CanvasLayer::default(), arcs: CanvasLayer::default(),
            ghost: CanvasLayer::default(), texts: Vec::new(), returning: None, measure: Measure::default(),
        };
        moving.copy(scene, templates, tokens);
        self.pinned = false;
        moving.chip(&mut self.chips, now);
        self.moving = Some(moving);
        true
    }

    /// R turns the group about its grip in O(heads), or steps a lone part's own turn.
    pub fn turn(&mut self, cx: &mut Cx, uid: WidgetUid, back: bool, sheet: &SheetLayout, now: f64) {
        if !self.held() { return; }
        let Some(m) = &mut self.moving else { return };
        if m.group.is_some() {
            orient_carried(&mut m.hint, &mut m.also, GroupOrientation::Turn { counter_clockwise: back });
        } else {
            if m.turn_step == 0 { return; }
            // R steps clockwise and Shift-R counter-clockwise, as at rest and in a group carry.
            m.hint.turn = (m.hint.turn + if back { m.turn_step } else { 4 - m.turn_step }) % 4;
        }
        m.preview_orientation(cx, uid, sheet, &mut self.chips, now);
    }

    /// X mirrors the group left-right and Y top-bottom about the grip, in O(heads).
    pub fn mirror(&mut self, cx: &mut Cx, uid: WidgetUid, flip_vertical: bool, sheet: &SheetLayout, now: f64) {
        if !self.held() { return; }
        let Some(m) = self.moving.as_mut().filter(|m| m.group.is_some()) else { return };
        orient_carried(&mut m.hint, &mut m.also, GroupOrientation::Mirror { flip_vertical });
        m.preview_orientation(cx, uid, sheet, &mut self.chips, now);
    }

    pub fn sample(&mut self, cx: &mut Cx, uid: WidgetUid, at: [f64; 2], sheet: &SheetLayout, now: f64) {
        let start = Instant::now();
        let Some(m) = &mut self.moving else { return };
        if m.returning.is_some() || m.committing { return; }
        let raw = std::array::from_fn(|i| (m.origin[i] as f64 + (at[i] - m.start[i]).round()) as i32);
        let inside = at[0] >= m.content[0] as f64 && at[0] <= m.content[2] as f64
            && at[1] >= m.content[1] as f64 && at[1] <= m.content[3] as f64;
        // Empty room takes the part and its block grows to hold it; another block refuses it.
        m.refused = !inside && m.regions.contains(at);
        m.target = raw;
        m.legal = if m.refused { m.clamp(raw) } else { m.target };
        m.chip(&mut self.chips, now);
        if !m.refused && m.last_preview != Some(m.legal)
            && (m.legal != m.origin || m.last_preview.is_some() || (m.group.is_some() && m.turned))
        {
            if let Some(action) = m.preview(m.legal, sheet) { cx.widget_action(uid, action); }
        }
        let us = start.elapsed().as_secs_f64() * 1.0e6;
        m.measure.samples += 1;
        m.measure.ui_sum += us;
        m.measure.ui_max = m.measure.ui_max.max(us);
    }

    pub fn worker(&mut self, ms: f64) {
        if let Some(m) = self.moving.as_mut().filter(|m| m.returning.is_none()) {
            m.measure.layouts += 1;
            m.measure.worker_sum += ms;
            m.measure.worker_max = m.measure.worker_max.max(ms);
        }
    }

    pub fn design_updated(&mut self, gen: u64) {
        if let Some(m) = &mut self.moving {
            if let Some(Return::Waiting { gen: waiting }) = &mut m.returning {
                *waiting = Some(gen);
            }
        }
    }

    pub fn layout(&mut self, gen: u64, sheet: &SheetLayout, circuit: &Circuit, now: f64) {
        self.finish_motion = false;
        let Some(m) = &mut self.moving else { return };
        match m.grip {
            Grip::Part(node) => if let Some(part) = sheet.parts.get(node as usize) {
                m.current = part.at;
                if !m.turned && m.group.is_none() { m.hint.turn = part.turn; }
                if m.group.is_some() && m.turned {
                    let drawn = (part.turn, part.mirror);
                    // A chain can derive its drawn pose. Use the layout's source grip to identify
                    // the rigid transform even when an earlier preview arrives between key presses.
                    m.group_layout_pose = m.returning.is_none().then(|| {
                        if part.turn_step != 0 { return drawn; }
                        circuit.hints.parts.iter().rev().find(|(path, _)| path == &m.path)
                            .map_or(drawn, |(_, hint)| (hint.turn, hint.mirror))
                    });
                }
            },
            Grip::Block(block) => {
                if let Some(origin) = sheet::block_origin(sheet, block) { m.current = origin; }
                if let Some(frame) = sheet.frames.get(block as usize).filter(|f| f.placed) {
                    m.frame = frame.rect.map(f64::from);
                }
            }
        }
        if let Some(content) = Moving::content(sheet, m.block) { m.content = content; }
        m.regions = Regions::new(sheet, circuit, m.block, &m.blocks);
        m.legal = if m.refused { m.clamp(m.target) } else { m.target };
        if matches!(m.returning, Some(Return::Waiting { gen: Some(g) }) if g == gen) {
            let offset = m.offset(now);
            m.returning = Some(Return::Flying { start: now, offset });
        }
    }

    pub fn rebuild(&mut self, scene: &mut Scene, templates: &Templates, tokens: &Tokens) {
        if let Some(m) = &mut self.moving { m.copy(scene, templates, tokens); }
    }

    /// The release: a drop where the part was last previewed commits it (after its other hints
    /// are in, `Finish::Drop`); a refused one commits the nearest legal place; anything else
    /// cancels.
    pub fn finish(&mut self, cx: &mut Cx, uid: WidgetUid, scene: &mut Scene, sheet: &SheetLayout, now: f64) -> Finish {
        self.rides = false;
        self.pinned = false;
        self.chips.set(Vec::new(), now);
        let Some(mut m) = self.moving.take() else { return Finish::None };
        if m.returning.is_some() || m.committing {
            self.moving = Some(m);
            return Finish::None;
        }
        let at = if m.refused { m.legal } else { m.target };
        if m.refused && m.last_preview != Some(at) {
            if let Some(action) = m.preview(at, sheet) { cx.widget_action(uid, action); }
        }
        if let Some(hint) = m.last_hints.clone().filter(|_| (at != m.origin || m.turned) && m.last_preview == Some(at)) {
            m.committing = true;
            self.token += 1;
            let path = m.path.clone();
            self.moving = Some(m);
            return Finish::Drop { path, hint, token: self.token };
        }
        cx.widget_action(uid, ViewAction::CancelPreview);
        self.end(m, scene);
        Finish::Done
    }

    /// Commit the released heads and every part that gave way in one step. Returns the tag
    /// whose layout ends the drag.
    pub fn dropped(&mut self, cx: &mut Cx, uid: WidgetUid, token: u64, drop: Vec<(String, SheetHint)>, scene: &mut Scene) -> Option<u64> {
        if token != self.token || !self.moving.as_ref().is_some_and(|m| m.committing) { return None; }
        let mut m = self.moving.take()?;
        if !drop.is_empty() {
            let heads = m.last_hints.as_ref().expect("a committing drag has previewed");
            let mut ops: Vec<_> = heads.parts.iter().map(|(path, hint)| m.hint_op(path, hint.clone())).collect();
            ops.extend(drop.into_iter().map(|(path, hint)| m.hint_op(&path, hint)));
            let tag = next_tag();
            m.last_tag = Some(tag);
            cx.widget_action(uid, ViewAction::Preview { notes: vec![], tag, place: vec![], ops, label: m.label.clone() });
        }
        let tag = m.last_tag.expect("a committing drag has previewed");
        cx.widget_action(uid, ViewAction::CommitPreview);
        self.end(m, scene);
        Some(tag)
    }

    fn end(&mut self, m: Moving, scene: &mut Scene) {
        m.measure.report(&m.refdes);
        self.finish_motion = true;
        scene.follow(&[]);
    }

    pub fn cancel(&mut self, cx: &mut Cx, uid: WidgetUid, now: f64) {
        self.rides = false;
        self.pinned = false;
        self.chips.set(Vec::new(), now);
        let Some(m) = &mut self.moving else { return };
        if m.returning.is_some() || m.committing { return; }
        m.measure.report(&m.refdes);
        // Until the first preview the sheet is still at its base, so there is no layout to wait for.
        m.returning = Some(if m.last_tag.is_none() { Return::Flying { start: now, offset: m.offset(now) } }
            else { Return::Waiting { gen: None } });
        cx.widget_action(uid, ViewAction::CancelPreview);
    }

    /// End the return with one connection-layer publication, not a rebuild per frame.
    pub fn settle(&mut self, scene: &mut Scene, now: f64) -> bool {
        if self.moving.as_ref().is_some_and(|m| matches!(m.returning,
            Some(Return::Flying { start, .. }) if now >= start + MOVE_SECONDS))
        {
            self.moving = None;
            scene.follow(&[]);
            return true;
        }
        false
    }

    pub fn offset(&self, now: f64) -> [f32; 2] {
        self.moving.as_ref().filter(|m| m.copy_visible()).map_or([0.0; 2], |m| m.offset(now))
    }

    pub fn animating(&self, now: f64) -> bool {
        self.moving.as_ref().is_some_and(|m| m.returning.is_none()
            || matches!(m.returning, Some(Return::Flying { start, .. }) if now < start + MOVE_SECONDS))
            || self.chips.animating(now)
    }

    pub fn draw(&mut self, cx: &mut Cx2d, uniforms: &CanvasUniforms, cam: &Camera2d,
        tokens: &Tokens, live: &CanvasLive, boxes: &mut DrawCanvasBox, lines: &mut DrawCanvasLine,
        arcs: &mut DrawCanvasArc, text: &mut DrawText, up_list: &mut Option<DrawList2d>, now: f64)
    {
        if let Some(m) = self.moving.as_mut().filter(|m| m.copy_visible()) {
            let offset = m.offset(now);
            let copy = CanvasUniforms { drag_item: m.row as f32, drag_offset: offset, ..*uniforms };
            if m.refused && m.returning.is_none() {
                CanvasUniforms {
                    drag_offset: std::array::from_fn(|i| (m.legal[i] as f64 - m.current[i] as f64) as f32), ..copy
                }.with(tokens, |u| m.ghost.draw(cx, &mut boxes.draw_vars, u, live));
            }
            copy.with(tokens, |u| {
                m.boxes.draw(cx, &mut boxes.draw_vars, u, live);
                m.lines.draw(cx, &mut lines.draw_vars, u, live);
                m.arcs.draw(cx, &mut arcs.draw_vars, u, live);
            });
            cx.push_clip_rect(cam.view);
            for t in &m.texts {
                draw_text(cx, text, cam, t, [t.at[0] + offset[0] as f64, t.at[1] + offset[1] as f64], 1.0, None);
            }
            cx.pop_clip_rect();
            let turn = UpTurn::of(cam.view);
            turn.begin(cx, up_list);
            for t in &m.texts {
                draw_text(cx, text, cam, t, [t.at[0] + offset[0] as f64, t.at[1] + offset[1] as f64], 1.0, Some(turn));
            }
            turn.end(cx, up_list);
        }
        self.chips.draw(cx, cam, uniforms.clip, tokens, live, lines, boxes, text, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::SheetHints;

    #[test]
    fn carried_group_orients_each_hint_about_the_grip() {
        use GroupOrientation::{Mirror, Turn};
        let hint = |turn, mirror| SheetHint { turn, mirror, ..SheetHint::default() };
        let mut grip = hint(2, true);
        let mut also = [
            OtherHead { path: "a".into(), hint: hint(1, false), offset: [3, -5] },
            OtherHead { path: "b".into(), hint: hint(0, true), offset: [-4, 2] },
        ];
        let target = [10, 20];
        for (key, orient, poses) in [
            ("R", Turn { counter_clockwise: false }, [(10, 20, 1, true), (15, 23, 0, false), (8, 16, 3, true)]),
            ("X", Mirror { flip_vertical: false }, [(10, 20, 3, false), (5, 23, 0, true), (12, 16, 1, false)]),
            ("Y", Mirror { flip_vertical: true }, [(10, 20, 3, true), (5, 17, 2, false), (12, 24, 1, true)]),
            ("Shift-R", Turn { counter_clockwise: true }, [(10, 20, 0, true), (7, 25, 3, false), (14, 18, 2, true)]),
        ] {
            orient_carried(&mut grip, &mut also, orient);
            let parts = carried_parts("grip", &grip, &also, target).expect("all three carried heads fit on the sheet");
            let expected: Vec<_> = ["grip", "a", "b"].into_iter().zip(poses).map(|(path, (x, y, turn, mirror))|
                (path.to_owned(), SheetHint { at: Some(SchPt::new(x, y)), ..hint(turn, mirror) })).collect();
            assert_eq!(parts, expected, "{key} keeps the grip at the pointer and composes every head's position and pose");
        }
    }

    #[test]
    fn block_drags_carry_their_subtree_and_never_refuse_their_own_frame() {
        let d = fab_elec_fixtures::ledctl();
        let c = &d.units[0].circuit;
        let sheet = fab_elec_explore::layout(&d, 0, &SheetHints::default(), &Opts::default());
        let root = 0;
        let mcu = c.tree.iter().find(|block| block.path == "mcu").expect("fixture has an mcu block").id.0;
        let reg = c.tree.iter().find(|block| block.path == "reg").expect("fixture has a reg block").id.0;
        assert!(carried_blocks(c, root).iter().all(|&carried| carried),
            "carried: the root carries every block");
        let carried = carried_blocks(c, mcu);
        assert!(carried[mcu as usize] && carried.iter().filter(|&&carried| carried).count() == 1,
            "carried: a leaf block carries only itself");
        let parent = c.tree.iter().find(|block| block.id.0 != root
            && c.tree.iter().any(|child| child.parent == Some(block.id)));
        let (parent, property) = match parent {
            Some(parent) => (parent.id, "carried: a block carries its descendants"),
            None => (BlockId(root), "carried: a block carries its descendants (no nested blocks; checking the root's children)"),
        };
        let descendants = carried_blocks(c, parent.0);
        let children: Vec<_> = c.tree.iter().filter(|child| child.parent == Some(parent)).collect();
        assert!(descendants[parent.0 as usize] && !children.is_empty()
            && children.iter().all(|child| descendants[child.id.0 as usize]), "{property}");
        let regions = Regions::new(&sheet, c, root, &carried);
        let centre = |block: u32| {
            let [x0, y0, x1, y1] = sheet.frames[block as usize].rect.map(f64::from);
            [(x0 + x1) * 0.5, (y0 + y1) * 0.5]
        };
        assert!(sheet.frames[mcu as usize].placed && !regions.contains(centre(mcu)),
            "regions: a dragged block's own frame never refuses its drop");
        assert!(sheet.frames[reg as usize].placed && regions.contains(centre(reg)),
            "regions: a sibling block's frame still refuses");
    }

    #[test]
    fn unpin_ops_remove_exactly_the_pins_asked_for() {
        let d = fab_elec_fixtures::ledctl();
        let unit = d.units[0].id;
        let mut circuit = (*d.units[0].circuit).clone();
        let block = circuit.tree.iter().find(|block| block.path == "mcu").expect("fixture has an mcu block").id;
        let mcu: Vec<_> = circuit.comps.iter().enumerate()
            .filter(|(_, comp)| comp.block == block).map(|(node, _)| node as u32).collect();
        let other = circuit.comps.iter().position(|comp| comp.block != block).expect("fixture has another block") as u32;
        circuit.hints.parts.clear();
        for node in [mcu[1], other, mcu[0]] {
            circuit.hints.parts.push((circuit.comps[node as usize].path.clone(), SheetHint::default()));
        }
        let remove = |node: u32| DesignOp::Circuit {
            unit, op: CircuitOp::Hint { path: circuit.comps[node as usize].path.clone(), hint: None },
        };
        assert_eq!(unpin_ops(&circuit, unit, mcu[0], false), vec![remove(mcu[0])],
            "unpin part: a pinned part gives one op removing its hint");
        assert!(unpin_ops(&circuit, unit, mcu[2], false).is_empty(),
            "unpin part: an unpinned part gives none");
        assert_eq!(unpin_ops(&circuit, unit, mcu[2], true), vec![remove(mcu[0]), remove(mcu[1])],
            "unpin block: exactly the pinned parts of that block, in component order");
        for block in [false, true] {
            assert!(unpin_ops(&circuit, unit, circuit.comps.len() as u32, block).is_empty(),
                "unpin: a node out of range gives none");
        }
    }

    #[test]
    fn unpin_block_ops_remove_the_block_and_with_shift_its_contents() {
        let d = fab_elec_fixtures::ledctl();
        let unit = d.units[0].id;
        let mut circuit = (*d.units[0].circuit).clone();
        let mcu = circuit.tree.iter().find(|block| block.path == "mcu")
            .expect("unpin block: the fixture has an MCU block").id;
        let reg = circuit.tree.iter().find(|block| block.path == "reg")
            .expect("unpin block: the fixture has a regulator block").id;
        let inside = circuit.comps.iter().find(|comp| comp.block == mcu)
            .expect("unpin block: the MCU block contains a part").path.clone();
        let outside = circuit.comps.iter().find(|comp| comp.block != mcu)
            .expect("unpin block: the fixture has a part outside the MCU block").path.clone();
        circuit.hints.parts.clear();
        for path in [&outside, &inside, &circuit.tree[mcu.0 as usize].path] {
            circuit.hints.parts.push((path.clone(), SheetHint::default()));
        }
        let remove = |path: &str| DesignOp::Circuit {
            unit, op: CircuitOp::Hint { path: path.to_owned(), hint: None },
        };
        assert_eq!(unpin_block_ops(&circuit, unit, mcu.0, false), vec![remove("mcu")],
            "unpin block: U removes only the block's own hint, even without a position");
        assert_eq!(unpin_block_ops(&circuit, unit, mcu.0, true), vec![remove("mcu"), remove(&inside)],
            "unpin block: Shift+U removes the block then its hinted contents, excluding outside parts");
        assert!(unpin_block_ops(&circuit, unit, reg.0, false).is_empty(),
            "unpin block: a block without a hint entry gives none");
        for all in [false, true] {
            assert!(unpin_block_ops(&circuit, unit, 0, all).is_empty(),
                "unpin block: the root gives none");
            assert!(unpin_block_ops(&circuit, unit, circuit.tree.len() as u32, all).is_empty(),
                "unpin block: a block out of range gives none");
        }
    }
}
