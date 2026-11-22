//! Screen-sized gesture labels, with retained geometry and fades keyed by chip id.

use super::{
    camera::Camera2d,
    draw::{
        BoxInst, BoxTemplate, CanvasLayer, CanvasLive, CanvasUniforms, Common, DrawCanvasBox,
        DrawCanvasLine, LineInst, LineTemplate,
    },
    looks::{ease_out, Look, Tokens, STATE_SECONDS},
};
use makepad_widgets_core::*;
use std::collections::HashMap;

const FONT_SIZE: f32 = 11.0;
const PAD_X: f64 = 6.0;
const PAD_Y: f64 = 3.0;
/// Gesture notes (Info, Refused) ride beside the dragged part: smaller than a value chip.
const NOTE_FONT_SIZE: f32 = 9.0;
const NOTE_PAD_X: f64 = 4.0;
const NOTE_PAD_Y: f64 = 2.0;
const CHEVRON_SPACE: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipKind {
    Info,
    Value,
    Refused,
    Trail,
}

impl ChipKind {
    fn look(self) -> Look {
        match self {
            Self::Info => Look::Preview,
            Self::Value => Look::Focus,
            Self::Refused => Look::Violation,
            Self::Trail => Look::Committed,
        }
    }

    fn ink(self, tokens: &Tokens) -> [f32; 4] {
        if self == Self::Refused { tokens.violation } else { tokens.ink }
    }

    fn note(self) -> bool {
        matches!(self, Self::Info | Self::Refused)
    }

    fn font_size(self) -> f32 {
        if self.note() { NOTE_FONT_SIZE } else { FONT_SIZE }
    }

    fn pad(self) -> [f64; 2] {
        if self.note() { [NOTE_PAD_X, NOTE_PAD_Y] } else { [PAD_X, PAD_Y] }
    }

    fn inset(self) -> f64 {
        self.pad()[0] + if self == Self::Value { CHEVRON_SPACE } else { 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChipAnchor {
    /// A world point, with a screen-point offset (positive dy is down).
    World { at: [f64; 2], dx: f64, dy: f64 },
    /// Logical screen points, y down, in the same coordinates as Camera2d::to_screen.
    Screen { at: [f64; 2] },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    /// Stable identity, unique within this pane's chips.
    pub id: u32,
    pub kind: ChipKind,
    pub text: String,
    pub anchor: ChipAnchor,
    /// 0..1 per axis: the anchor is at the left/top (0), centre (0.5), or right/bottom (1).
    pub align: [f64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fade {
    start: f64,
    from: f64,
    to: f64,
}

impl Fade {
    fn alpha(self, now: f64) -> f64 {
        self.from + (self.to - self.from) * ease_out((now - self.start) / STATE_SECONDS)
    }

    fn animating(self, now: f64) -> bool {
        self.from != self.to && now < self.start + STATE_SECONDS
    }
}

struct Templates {
    line: LineTemplate,
    boxes: BoxTemplate,
}

impl Templates {
    fn capture(cx: &Cx, line: &DrawCanvasLine, boxes: &DrawCanvasBox) -> Option<Self> {
        Some(Self {
            line: LineTemplate::capture(cx, line)?,
            boxes: BoxTemplate::capture(cx, boxes)?,
        })
    }
}

/// The chips of one pane: set them when they change, place and draw them each frame.
/// Fade endpoints live in the retained rows; only the motion uniform advances each frame.
#[derive(Default)]
pub struct ChipLayer {
    chips: Vec<Chip>,
    fades: Vec<Fade>,
    sizes: Vec<[f64; 2]>,
    placed: Vec<(u32, [f64; 4])>,
    visible: Vec<bool>,
    drawn: Vec<(u32, [f64; 4])>,
    camera: Option<Camera2d>,
    /// The clip the chips were last placed in: they are placed again when it changes.
    clip: [f64; 4],
    tokens: Option<Tokens>,
    dirty: bool,
    templates: Option<Templates>,
    capture_attempted: bool,
    boxes: CanvasLayer,
    lines: CanvasLayer,
    box_rows: Vec<f32>,
    line_rows: Vec<f32>,
}

impl ChipLayer {
    /// Replace the chips. Kept ids keep their fade; new and removed ids fade over 120 ms.
    /// Reintroducing an outgoing id reverses its fade from its current opacity.
    pub fn set(&mut self, mut chips: Vec<Chip>, now: f64) {
        if chips.iter().eq(self.chips.iter().zip(&self.fades)
            .filter(|(_, fade)| fade.to == 1.0).map(|(chip, _)| chip))
        {
            return;
        }
        let mut previous: Vec<_> = self.chips.drain(..).zip(self.fades.drain(..))
            .map(Some).collect();
        let mut by_id: HashMap<_, _> = previous.iter().enumerate()
            .filter_map(|(i, entry)| entry.as_ref().map(|(chip, _)| (chip.id, i)))
            .collect();
        for chip in &chips {
            let fade = by_id.remove(&chip.id).and_then(|i| previous[i].take())
                .map_or(Fade { start: now, from: 0.0, to: 1.0 }, |(_, fade)| {
                    if fade.to == 1.0 {
                        fade
                    } else {
                        Fade { start: now, from: fade.alpha(now), to: 1.0 }
                    }
                });
            self.fades.push(fade);
        }
        // Preserve the order of outgoing chips, and never restart an existing fade-out.
        for (chip, fade) in previous.into_iter().flatten() {
            let alpha = fade.alpha(now);
            if alpha <= 0.0 {
                continue;
            }
            chips.push(chip);
            self.fades.push(if fade.to == 0.0 {
                fade
            } else {
                Fade { start: now, from: alpha, to: 0.0 }
            });
        }
        self.chips = chips;
        self.dirty = true;
    }

    /// True while any chip is fading; the view only needs animation frames then.
    pub fn animating(&self, now: f64) -> bool {
        self.fades.iter().any(|fade| fade.animating(now))
    }

    /// The topmost chip whose last-drawn screen box contains `at`, for trail hit testing.
    /// Clipped and fully transparent chips do not receive hits.
    pub fn hit(&self, at: [f64; 2]) -> Option<u32> {
        self.drawn.iter().rev().find_map(|&(id, rect)| {
            (at[0] >= rect[0] && at[0] <= rect[2] && at[1] >= rect[1] && at[1] <= rect[3])
                .then_some(id)
        })
    }

    /// Draw rounded boxes, Value chevrons, and 11-point text (9-point for gesture notes) in
    /// logical screen coordinates.
    /// World anchors follow the camera, while padding, corners and type keep their size.
    pub fn draw(
        &mut self,
        cx: &mut Cx2d,
        cam: &Camera2d,
        clip: [f32; 4],
        tokens: &Tokens,
        live: &CanvasLive,
        line: &mut DrawCanvasLine,
        boxes: &mut DrawCanvasBox,
        text: &mut DrawText,
        now: f64,
    ) {
        self.drawn.clear();
        if !self.capture_attempted {
            self.capture_attempted = true;
            // Capture itself logs the first failure; do not retry or log every frame.
            self.templates = Templates::capture(cx, line, boxes);
        }
        let Some(templates) = &self.templates else { return };
        let old_font_size = text.text_style.font_size;
        let old_font_scale = text.font_scale;
        let old_color = text.color;
        text.font_scale = 1.0;

        if self.dirty {
            // At the first frame alpha can still be zero; reserve hit storage now so
            // subsequent fade frames do not allocate when chips become hittable.
            self.drawn.reserve(self.chips.len());
            self.sizes.clear();
            for chip in &self.chips {
                text.text_style.font_size = chip.kind.font_size();
                let size = text.layout(cx, 0.0, 0.0, None, false, Align::default(), &chip.text)
                    .size_in_lpxs;
                self.sizes.push([size.width as f64, size.height as f64]);
            }
        }
        let mut rebuild = self.dirty || self.tokens.as_ref() != Some(tokens);
        let moved = self.camera.is_none_or(|old| {
            old.center != cam.center || old.px_per_unit != cam.px_per_unit || old.view != cam.view
        }) && self.chips.iter().any(|chip| matches!(chip.anchor, ChipAnchor::World { .. }));
        let clip = clip.map(f64::from);
        let clip_valid = clip[0] < clip[2] && clip[1] < clip[3];
        if self.dirty || moved || self.clip != clip {
            let placed = place(&self.chips, cam, &self.sizes, clip_valid.then_some(clip));
            self.clip = clip;
            // A camera change with only offscreen world chips does not republish rows.
            rebuild |= placed.iter().zip(&self.placed).zip(&self.visible)
                .any(|((new, old), visible)| *visible && new != old);
            self.placed = placed;
        }
        self.camera = Some(*cam);
        self.visible.resize(self.chips.len(), false);
        for (i, (_, rect)) in self.placed.iter().enumerate() {
            let fade = self.fades[i];
            // Include the outline. Incoming alpha-zero rows must already
            // be present so their entire fade can run on uniforms alone.
            let visible = clip_valid && (fade.to > 0.0 || fade.animating(now))
                && rect[2] + 6.0 >= clip[0] && rect[0] - 6.0 <= clip[2]
                && rect[3] + 6.0 >= clip[1] && rect[1] - 6.0 <= clip[3];
            rebuild |= self.visible[i] != visible;
            self.visible[i] = visible;
        }

        if rebuild {
            self.box_rows.clear();
            self.line_rows.clear();
            for (i, (_, rect)) in self.placed.iter().enumerate() {
                if !self.visible[i] {
                    continue;
                }
                let chip = &self.chips[i];
                let fade = self.fades[i];
                let half_w = ((rect[2] - rect[0]) * 0.5) as f32;
                let half_h = ((rect[3] - rect[1]) * 0.5) as f32;
                let mut c = Common::at([
                    ((rect[0] + rect[2]) * 0.5) as f32,
                    -((rect[1] + rect[3]) * 0.5) as f32,
                    0.0,
                ], chip.kind.ink(tokens));
                c.look = chip.kind.look().code();
                c.pose_a[3] = fade.from as f32;
                c.pose_b[3] = fade.to as f32;
                c.span = [fade.start as f32, STATE_SECONDS as f32];
                templates.boxes.append(&mut self.box_rows, &BoxInst {
                    c,
                    rect: [0.0, 0.0, half_w, half_h],
                    corner: 4.0,
                    stroke: [1.0, 0.0],
                    fill: tokens.part_fill,
                });
                if chip.kind == ChipKind::Value {
                    for direction in [-1.0f32, 1.0] {
                        let tip = direction * (half_w - chip.kind.pad()[0] as f32 - 1.0);
                        for dy in [-3.0, 3.0] {
                            templates.line.append(&mut self.line_rows, &LineInst {
                                c,
                                seg: [tip, 0.0, tip - direction * 3.0, dy],
                                width: [1.0, 0.0],
                                ..LineInst::default()
                            });
                        }
                    }
                }
            }
            // CanvasLayer owns its publication. Keep the scratch vectors' capacity and
            // copy only when publishing changed geometry, never for a fade-only frame.
            self.boxes.set(templates.boxes.slots(), self.box_rows.clone());
            self.lines.set(templates.line.slots(), self.line_rows.clone());
            self.tokens = Some(*tokens);
            self.dirty = false;
        }
        let uniforms = CanvasUniforms {
            drag_item: -1.0, drag_offset: [0.0; 2],
            cam: [0.0, 0.0, 1.0, cam.dpi as f32],
            motion_t: now as f32,
            hover_item: -1.0,
            hover_mix: 0.0,
            layer_alpha: 1.0,
            time: now as f32,
            live_time: now as f32,
            light: tokens.canvas[0] > 0.5,
            clip: clip.map(|value| value as f32),
        };
        uniforms.with(tokens, |values| {
            self.boxes.draw(cx, &mut boxes.draw_vars, values, live);
            self.lines.draw(cx, &mut line.draw_vars, values, live);
        });
        if clip_valid {
            cx.push_clip_rect(Rect {
                pos: dvec2(clip[0], clip[1]),
                size: dvec2(clip[2] - clip[0], clip[3] - clip[1]),
            });
            for (i, &(id, rect)) in self.placed.iter().enumerate() {
                let alpha = self.fades[i].alpha(now) as f32;
                if !self.visible[i] || alpha <= 0.0 {
                    continue;
                }
                let chip = &self.chips[i];
                let ink = chip.kind.ink(tokens);
                text.color = vec4(ink[0], ink[1], ink[2], ink[3] * alpha);
                text.text_style.font_size = chip.kind.font_size();
                text.draw_abs(cx, dvec2(rect[0] + chip.kind.inset(), rect[1] + chip.kind.pad()[1]), &chip.text);
                let hit = [rect[0].max(clip[0]), rect[1].max(clip[1]),
                    rect[2].min(clip[2]), rect[3].min(clip[3])];
                if hit[0] < hit[2] && hit[1] < hit[3] {
                    self.drawn.push((id, hit));
                }
            }
            cx.pop_clip_rect();
        }
        text.text_style.font_size = old_font_size;
        text.font_scale = old_font_scale;
        text.color = old_color;
    }
}

/// Text sizes exclude padding and chevrons; rectangles are [left, top, right, bottom].
/// A chip whose anchor lies inside `clip` (the pane) stays inside it: on an axis where it would
/// cross an edge it goes to the other side of its anchor (a world chip's offset mirrored) when
/// that crosses less, then it is shifted inside, its start kept when it is longer than the pane.
/// A chip anchored outside the pane is placed as it is (and culled). O(chips).
/// Called only when content, the clip or its screen placement changes, not for fade-only frames.
fn place(chips: &[Chip], cam: &Camera2d, sizes: &[[f64; 2]], clip: Option<[f64; 4]>) -> Vec<(u32, [f64; 4])> {
    chips.iter().zip(sizes).map(|(chip, size)| {
        let (anchor, offset) = match chip.anchor {
            ChipAnchor::World { at, dx, dy } => {
                let screen = cam.to_screen(at);
                ([screen.x, screen.y], [dx, dy])
            }
            ChipAnchor::Screen { at } => (at, [0.0, 0.0]),
        };
        let extent = [size[0] + 2.0 * chip.kind.inset(), size[1] + 2.0 * chip.kind.pad()[1]];
        let inside = clip.filter(|c| (c[0]..=c[2]).contains(&anchor[0]) && (c[1]..=c[3]).contains(&anchor[1]));
        let start = |axis: usize| {
            let (at, len, align) = (anchor[axis], extent[axis], chip.align[axis]);
            let here = at + offset[axis] - len * align;
            let Some(c) = inside else { return here };
            let (lo, hi) = (c[axis], c[axis + 2]);
            let crossing = |start: f64| (lo - start).max(0.0) + (start + len - hi).max(0.0);
            let mirrored = at - offset[axis] - len * (1.0 - align);
            let start = if crossing(mirrored) < crossing(here) { mirrored } else { here };
            start.min(hi - len).max(lo)
        };
        let (left, top) = (start(0), start(1));
        (chip.id, [left, top, left + extent[0], top + extent[1]])
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_fade_and_hit_by_id() {
        let trail = Chip {
            id: 7,
            kind: ChipKind::Trail,
            text: "Previous focus".into(),
            anchor: ChipAnchor::Screen { at: [120.0, 65.0] },
            align: [0.0, 0.0],
        };
        let value = Chip {
            id: 42,
            kind: ChipKind::Value,
            text: "10k".into(),
            anchor: ChipAnchor::World { at: [4.0, 5.0], dx: 10.0, dy: -6.0 },
            align: [0.5, 1.0],
        };
        let cam = Camera2d {
            view: Rect { pos: dvec2(100.0, 50.0), size: dvec2(200.0, 100.0) },
            px_per_unit: 2.0,
            ..Camera2d::default()
        };
        let mut layer = ChipLayer::default();
        layer.set(vec![trail, value.clone()], 0.0);
        assert!(layer.animating(0.05), "new ids fade in");
        assert!(!layer.animating(0.2), "fade-in settles within 120 ms");
        let kept_fade = layer.fades[1];
        let sizes = [[40.0, 12.0], [30.0, 12.0]];
        let pane = Some([100.0, 50.0, 300.0, 150.0]);
        layer.drawn = place(&layer.chips, &cam, &sizes, pane);
        assert_eq!(layer.drawn, vec![(7, [120.0, 65.0, 172.0, 83.0]),
            (42, [187.0, 66.0, 249.0, 84.0])], "padding, offsets and alignment use screen points");
        assert_eq!(layer.hit([125.0, 70.0]), Some(7), "screen anchor hits by id");
        assert_eq!(layer.hit([218.0, 75.0]), Some(42), "world anchor hits by id");
        assert_eq!(layer.hit([0.0, 0.0]), None, "empty space misses");
        let zoomed = place(&layer.chips, &Camera2d { px_per_unit: 4.0, ..cam }, &sizes, pane);
        assert_eq!(zoomed[0], layer.drawn[0], "screen chips ignore camera zoom");
        assert_eq!(zoomed[1].1, [195.0, 56.0, 257.0, 74.0], "world chips move without scaling");
        // A note beside the pointer near the pane's right and bottom edges goes to the pointer's
        // other side; one wider than the pane keeps its start inside; one whose anchor is outside
        // the pane is left where it is.
        let note = |text: &str, at: [f64; 2]| Chip {
            id: 9, kind: ChipKind::Refused, text: text.into(), anchor: ChipAnchor::World { at, dx: 14.0, dy: 14.0 }, align: [0.0, 0.0],
        };
        let corner = [45.0, -22.0];
        let pointer = cam.to_screen(corner);
        let edge = place(&[note("gap", corner)], &cam, &[[60.0, 10.0]], pane)[0].1;
        assert_eq!(edge, [pointer.x - 14.0 - 68.0, pointer.y - 14.0 - 14.0, pointer.x - 14.0, pointer.y - 14.0],
            "near the right and bottom edges a note flips to the pointer's left and above it");
        assert!(edge[0] >= 100.0 && edge[1] >= 50.0 && edge[2] <= 300.0 && edge[3] <= 150.0, "the flipped note is inside the pane: {edge:?}");
        let wide = place(&[note("wide", corner)], &cam, &[[400.0, 10.0]], pane);
        assert_eq!(wide[0].1[0], 100.0, "a note wider than the pane keeps its start on the pane's left edge");
        let away = place(&[note("gap", [200.0, -22.0])], &cam, &[[60.0, 10.0]], pane);
        assert_eq!(away[0].1[0], cam.to_screen([200.0, -22.0]).x + 14.0, "a note anchored outside the pane is not pulled in");
        assert_eq!(place(&[note("gap", corner)], &cam, &[[60.0, 10.0]], None)[0].1[0], pointer.x + 14.0,
            "without a pane the note sits where it is anchored");
        layer.set(vec![value.clone()], 1.0);
        assert_eq!(layer.fades[0], kept_fade, "a retained id keeps its fade despite reordering");
        assert!(layer.animating(1.05), "the removed id fades out");
        layer.set(vec![value], 1.06);
        assert!(!layer.animating(1.2), "repeated set does not restart the outgoing fade");
        assert_eq!(layer.fades[1].alpha(1.2), 0.0, "removed chip becomes transparent");
        assert_eq!(layer.hit([125.0, 70.0]), Some(7), "hit describes the last drawn frame");
    }
}
