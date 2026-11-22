//! Named connections (user, 11-17: "just use the bench probes", "with a named connection"): a
//! bench probe shows on the sheet and on the board as a schematic net flag, not as a scope. A
//! short stub leaves the probed pin (a connection dot on the pin) and ends in a flag, the probe's
//! tag ([`ProbeTag`], as its Bench and Scope lanes wear it): its name in its lane's hue. It reads
//! as "this wire goes to the bench probe called LED0"; the signal itself is in the Scope.
//!
//! The flag is a design element: where it sits is the probe's `at` option in the bench source (a
//! pin of its net, `"<refdes>.<pad>"`; without one [`anchor_pin`] picks), so it is saved with the
//! probe, follows the pin when the part or wire moves, renames with the lane, and goes with the
//! probe. On the board the same pin is a pad.
//!
//! On the board a probe is a ring of its hue on its pad, test-point sized (user, 11-18: "these
//! probe points for the scope shouldnt be 'over' the board"), and its callout sits in the canvas
//! margin outside the board's outline, joined to the ring by a hairline ([`margin_callouts`]: on
//! the side nearest the pad, stacked so callouts never meet each other or the board). Where the
//! margin has no room the callout shows beside its ring while the ring is hovered. The ring's size
//! follows zoom sublinearly ([`flag_scale`]): the square root of the zoom against the pane's
//! reference, between a floor and a ceiling.
//!
//! Input ([`ProbeFlags::handle_event`], before the pane's own): the pointer over a flag is a hand;
//! a click asks for the probe's lane in the Scope ([`OpenInScope`]); a drag carries the flag along
//! its net: it rides the pointer, the pane draws a ghost flag at the pin of the net it would land
//! on, and the release is [`FlagInput::Moved`] (the pane writes `BenchOp::SetProbeAt`, one undo
//! step). A probe dragged over a net draws a ghost flag there before the drop.
//!
//! On a sheet the flags are tags instead (user, 11-18: "a little rectangle with a label 'behind'
//! the schematic", "or in some way routed somewhere out of the way"): a small rectangle in the
//! probe's hue with its name, in world units like the sheet's own net names, at the free spot
//! nearest its pin ([`place_tags`]: never on a symbol, a pin, a wire or a label), joined to the pin
//! by a thin leader. The tag and its leader draw behind the drawing ([`ProbeFlags::draw_behind`]),
//! its name with the drawing's text ([`ProbeFlags::draw_labels`]); zoomed out past readable text the
//! tag is a chip of its hue. A part under or near the pointer comes before a tag
//! ([`ProbeFlags::handle_event_yielding`]): only a press on the tag itself opens its lane.
//!
//! A flag is a small scope of its net (the approved design, V9): its name, the take's wave across
//! it with the play head, and the value at the instant shown ([`FlagScope`]); the instant itself
//! shows only in the recorder. Without a take, or for a net the run does not trace, a flag is its
//! name alone.
//!
//! Cost on the UI thread: per draw O(flags) stubs and tags (tag text layouts are cached by the tag
//! shader) and, per flag, its wave's columns kept between frames ([`ColumnCache`]: a frame where
//! only the play head moved reads nothing, a growing take reduces only its new columns) and one
//! value lookup (O(log samples)); per pointer event one pass over the shown flags. A pane redraws
//! for its flags only when what they show moved (a column of the play head, a value's text). A
//! sheet's placing is O(tags x (obstacles + spots x nearby obstacles)), done by the pane once per
//! layout.

use super::tag::{DrawProbeTag, LaneSide, ProbeTag, TagKind, TagState, TagStyle};
use crate::canvas::Camera2d;
use crate::instruments::look::InstrumentLook;
use crate::trace::colcache::ColumnCache;
use crate::trace::cursor::eng;
use crate::trace::store::value_at;
use crate::ElecLive;
use fab_elec_core::ids::NetRef;
use makepad_widgets_core::*;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    // The stub from the probed pin (a connection dot on it) to its flag.
    mod.draw.DrawProbeStub = set_type_default() do #(DrawProbeStub::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        from: vec2(0.0)
        to: vec2(0.0)
        pixel: fn() {
            let p = self.pos * self.rect_size
            let dpi = max(self.draw_pass.dpi_factor, 0.5)
            let ab = self.to - self.from
            let t = clamp(dot(p - self.from, ab) / max(dot(ab, ab), 0.0001), 0.0, 1.0)
            let line = clamp(0.5 + (0.6 - length(p - self.from - ab * t)) * dpi, 0.0, 1.0)
            let dot = clamp(0.5 + (2.5 - length(p - self.from)) * dpi, 0.0, 1.0)
            let a = max(line, dot) * self.color.a
            return vec4(self.color.rgb * a, a)
        }
    }

    // A probe's tag, callout or ring: a rounded box on its ground, tinted with its hue, its
    // outline in the hue only where `outline` is above 0 (a ring). `radius` is in `Sdf2d.box`
    // units (the sheet's tokens, through `ProbeFlags::radius`), at most a pill.
    mod.draw.DrawProbeFlag = set_type_default() do #(DrawProbeFlag::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        ground: #0000
        fill: 0.2
        radius: 0.0
        outline: 1.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let r = min(self.radius, min(self.rect_size.x, self.rect_size.y) * 0.5)
            let inset = max(self.outline, 1.0) * 0.5
            sdf.box(inset, inset, self.rect_size.x - 2.0 * inset, self.rect_size.y - 2.0 * inset, r)
            sdf.fill_keep(self.ground)
            sdf.fill_keep(vec4(self.color.rgb, self.color.a * self.fill))
            if self.outline > 0.0 { sdf.stroke(self.color, self.outline) }
            return sdf.result
        }
    }

    mod.widgets.ProbeFlags = set_type_default() do #(ProbeFlags::script_api(vm)) {
        draw_stub: mod.draw.DrawProbeStub {}
        draw_flag: mod.draw.DrawProbeFlag {}
        draw_tag: mod.draw.DrawProbeTag {}
        draw_wave: mod.draw.DrawColor {}
        // the sheet's corners; Fab sets its tokens on its instance (fab.radius / fab.radius_lg)
        radius: theme.corner_radius
        radius_lg: theme.container_corner_radius
        draw_text: mod.draw.DrawText {
            text_style: theme.font_regular {font_size: 10}
        }
        draw_code: mod.draw.DrawText {
            text_style: theme.font_code {font_size: 9}
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawProbeStub {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    #[live]
    pub from: Vec2f,
    #[live]
    pub to: Vec2f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawProbeFlag {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub color: Vec4f,
    /// The ground under the tint (clear: the drawing shows through).
    #[live]
    pub ground: Vec4f,
    /// The fill's opacity against the outline's.
    #[live]
    pub fill: f32,
    /// The corner radius, points (0: a quarter of the height, at most 3).
    #[live]
    pub radius: f32,
    /// The outline's width in the hue, points (0: none).
    #[live]
    pub outline: f32,
}

/// A flag as a scope at scale 1, points (the design's 84 x 26): its name and, at the right, its
/// value at the play head in a row, the take's wave under them.
pub const SCOPE_FLAG: DVec2 = DVec2 { x: 84.0, y: 26.0 };
/// A sheet's flag scope, in its net names' heights (as wide as its name needs, at least).
pub const SCOPE_TAG: [f64; 2] = [8.0, 2.6];
/// The columns a flag's wave takes.
const WAVE_COLS: u32 = 48;
/// A canvas's floating tool strip in one column, points: callouts stay clear of it before the
/// strip has been drawn once (after that, of the strip as drawn, one column or two).
const STRIP_W: f64 = 56.0;

/// A flag's scope ([`ProbeFlags::scope`]): the take's wave of its net as column spans in `[0, 1]`
/// of the flag's width and of its value range, the play head's place across it, and the value at
/// the instant shown (`-457 mV`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlagScope {
    /// Per column with samples: its left edge and right edge (0 to 1 across), and its least and
    /// greatest value (0 at the range's bottom, 1 at its top).
    pub spans: Vec<[f32; 4]>,
    pub head: Option<f32>,
    pub value: String,
}

/// A flag net's trace row and its wave's columns, kept between frames.
#[derive(Default)]
struct FlagWave {
    /// The sim index generation the row was found in, and the row (None: the run does not trace
    /// the net).
    row: Option<(u64, Option<usize>)>,
    cache: ColumnCache,
    used: bool,
}

/// What a pane's flags show: the instant (to the millisecond its tag reads) and the record they
/// read (its generation and length), as last drawn; a change asks the pane to draw again.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ShownKey {
    t: Option<i64>,
    record: Option<(u64, usize)>,
}

/// The smallest a flag draws, against its size at the pane's reference zoom: still readable when
/// the drawing is zoomed far out.
pub const SCALE_MIN: f64 = 0.75;
/// The largest a flag draws: zoomed far in it stops growing, so it never hides the drawing.
pub const SCALE_MAX: f64 = 1.5;
/// The flag text's size at scale 1, points.
pub const FLAG_FONT: f64 = 10.0;
/// A probe ring's radius on the board at scale 1, points: a test point's.
const RING: f64 = 4.5;

/// A flag's scale at `ppu` points per world unit, for a pane whose flags are their own size at
/// `reference` points per unit: the square root of the ratio, between [`SCALE_MIN`] and
/// [`SCALE_MAX`] (not screen-fixed, not linear).
pub fn flag_scale(ppu: f64, reference: f64) -> f64 {
    if !(ppu > 0.0 && reference > 0.0) { return 1.0; }
    (ppu / reference).sqrt().clamp(SCALE_MIN, SCALE_MAX)
}

/// The room a callout keeps from the board, the pane's edges and other callouts, points.
pub const CALLOUT_GAP: f64 = 6.0;

/// A side of the board a callout sits beyond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side { Top, Bottom, Left, Right }

/// Where each callout goes around the board (`board`, on screen) inside the pane `clip`, each
/// `(point, size)` its probe's point and its callout's size: beyond the side of the board nearest
/// the point that has room for it, [`CALLOUT_GAP`] off the board, and along that side as near
/// opposite its point as the others let it, so callouts never meet each other, the board or a
/// `keep_out` rectangle (the pane's own floats). None where it fits nowhere: that callout shows
/// only while its point is hovered. O(callouts log callouts).
pub fn margin_callouts(points: &[(DVec2, DVec2)], board: Rect, clip: Rect, keep_out: &[Rect]) -> Vec<Option<Rect>> {
    let g = CALLOUT_GAP;
    let (b0, b1) = (board.pos, board.pos + board.size);
    let (c0, c1) = (clip.pos + dvec2(g, g), clip.pos + clip.size - dvec2(g, g));
    // the depth each side's margin offers, and the stretch along it
    let room = |side: Side| match side {
        Side::Top => b0.y - g - c0.y,
        Side::Bottom => c1.y - (b1.y + g),
        Side::Left => b0.x - g - c0.x,
        Side::Right => c1.x - (b1.x + g),
    };
    let depth = |side: Side, size: DVec2| if matches!(side, Side::Top | Side::Bottom) { size.y } else { size.x };
    let mut sides: Vec<(Side, usize)> = Vec::with_capacity(points.len());
    let mut out = vec![None; points.len()];
    for (i, &(at, size)) in points.iter().enumerate() {
        let mut order = [(Side::Top, (at.y - b0.y).abs()), (Side::Bottom, (b1.y - at.y).abs()), (Side::Left, (at.x - b0.x).abs()), (Side::Right, (b1.x - at.x).abs())];
        order.sort_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((side, _)) = order.iter().find(|(side, _)| room(*side) >= depth(*side, size)) {
            sides.push((*side, i));
        }
    }
    for side in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
        let across = matches!(side, Side::Top | Side::Bottom);
        // along the side: x for top and bottom, y for left and right
        let (lo, hi) = if across { (c0.x, c1.x) } else { (c0.y, c1.y) };
        let mut row: Vec<(usize, f64, f64)> = sides.iter().filter(|(s, _)| *s == side).map(|&(_, i)| {
            let (at, size) = points[i];
            let len = if across { size.x } else { size.y };
            (i, if across { at.x } else { at.y } - len * 0.5, len)
        }).collect();
        row.sort_by(|a, b| a.1.total_cmp(&b.1));
        // forward: each after the last; then back: the last within the stretch, each before the next
        let mut end = lo - g;
        for (_, start, len) in &mut row {
            *start = start.max(end + g).max(lo);
            end = *start + *len;
        }
        let mut next = hi + g;
        for (_, start, len) in row.iter_mut().rev() {
            *start = start.min(next - g - *len);
            next = *start;
        }
        for (i, start, len) in row {
            if start < lo || start + len > hi {
                continue;
            }
            let size = points[i].1;
            let pos = match side {
                Side::Top => dvec2(start, b0.y - g - size.y),
                Side::Bottom => dvec2(start, b1.y + g),
                Side::Left => dvec2(b0.x - g - size.x, start),
                Side::Right => dvec2(b1.x + g, start),
            };
            let rect = Rect { pos, size };
            if !keep_out.iter().any(|k| rect_meets(k, &rect)) {
                out[i] = Some(rect);
            }
        }
    }
    out
}

/// The point of `rect`'s edge nearest `at`: where a callout's leader meets it.
fn edge_toward(rect: Rect, at: DVec2) -> DVec2 {
    dvec2(at.x.clamp(rect.pos.x, rect.pos.x + rect.size.x), at.y.clamp(rect.pos.y, rect.pos.y + rect.size.y))
}

/// How far from its pin's own place a sheet tag looks for a free spot, in half tag heights each way.
const TAG_REACH: i32 = 16;

/// Where tags sit on a sheet, in order, each `(anchor, [w, h])` in world units: at the free spot
/// nearest the pin's own place (just past the pin, along it), `gap` clear of every `obstacles` box
/// (the drawing: bodies, pins, wires, labels), of every `others` box and of the tags placed before
/// it, searched in half tag heights out to [`TAG_REACH`] of them; with none free, the own place.
/// World boxes `[x0, y0, x1, y1]`, one per tag. O(tags x (obstacles + spots x nearby obstacles)):
/// one pass over the obstacles per tag, then about a thousand spots against the few near it.
pub fn place_tags(tags: &[(Anchor, [f64; 2])], obstacles: &[[f64; 4]], others: &[[f64; 4]], gap: f64) -> Vec<[f64; 4]> {
    let mut spots: Vec<(i32, i32)> = (-TAG_REACH..=TAG_REACH).flat_map(|i| (-TAG_REACH..=TAG_REACH).map(move |j| (i, j))).collect();
    spots.sort_by_key(|&(i, j)| (i * i + j * j, j.abs()));
    let mut placed: Vec<[f64; 4]> = Vec::with_capacity(tags.len());
    let mut near = Vec::new();
    for &(anchor, [w, h]) in tags {
        let [ox, oy] = anchor.out;
        let along = gap + 0.5 * (w * ox.abs() + h * oy.abs());
        let own = [anchor.at[0] + ox * along, anchor.at[1] + oy * along];
        let step = h * 0.5;
        let reach = TAG_REACH as f64 * step + w + h + gap;
        let area = [own[0] - reach, own[1] - reach, own[0] + reach, own[1] + reach];
        near.clear();
        near.extend(obstacles.iter().chain(others).chain(&placed).filter(|o| boxes_meet(o, &area))
            .map(|o| [o[0] - gap, o[1] - gap, o[2] + gap, o[3] + gap]));
        let rect_at = |c: [f64; 2]| [c[0] - w * 0.5, c[1] - h * 0.5, c[0] + w * 0.5, c[1] + h * 0.5];
        let rect = spots.iter().map(|&(i, j)| rect_at([own[0] + i as f64 * step, own[1] + j as f64 * step]))
            .find(|rect| !near.iter().any(|o| boxes_meet(o, rect)))
            .unwrap_or_else(|| rect_at(own));
        placed.push(rect);
    }
    placed
}

fn boxes_meet(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// Where the pointer is for a press or a hover (a mouse press or move, a touch's first point);
/// None for other events.
pub fn pointer_abs(event: &Event) -> Option<DVec2> {
    match event {
        Event::MouseDown(e) => Some(e.abs),
        Event::MouseMove(e) => Some(e.abs),
        Event::TouchUpdate(e) => e.touches.first().map(|touch| touch.abs),
        _ => None,
    }
}

/// A probe point in a pane's world units: where the stub starts, the outward direction (a unit
/// vector in world axes, y up), and the probed part's box `[x0, y0, x1, y1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub at: [f64; 2],
    pub out: [f64; 2],
    pub body: Option<[f64; 4]>,
}

impl Anchor {
    /// A pin end at `at` pointing `dir` (out of its part, any length), of a part whose box is
    /// `body`. A pin of no length points away from the box's centre (right for the centre).
    pub fn pin(at: [f64; 2], dir: [f64; 2], body: Option<[f64; 4]>) -> Anchor {
        let unit = |d: [f64; 2]| {
            let len = d[0].hypot(d[1]);
            (len > 1e-12 && len.is_finite()).then(|| [d[0] / len, d[1] / len])
        };
        let away = body.and_then(|b| unit([at[0] - (b[0] + b[2]) * 0.5, at[1] - (b[1] + b[3]) * 0.5]));
        Anchor { at, out: unit(dir).or(away).unwrap_or([1.0, 0.0]), body }
    }

    /// A wire from `a` to `b` with no pin to anchor at: its middle, the flag above it.
    pub fn wire(a: [f64; 2], b: [f64; 2]) -> Anchor {
        Anchor { at: [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5], out: [0.0, 1.0], body: None }
    }
}

/// A pin a probe of its net could anchor at, as a pane names it: its part's reference, the pin's
/// number, and how many pins its part has.
#[derive(Clone, Copy, Debug)]
pub struct PinSpot<'a> {
    pub refdes: &'a str,
    pub number: &'a str,
    pub part_pins: usize,
}

impl PinSpot<'_> {
    /// The name a probe's `at` option gives this pin: `"<refdes>.<number>"`.
    pub fn name(&self) -> String {
        format!("{}.{}", self.refdes, self.number)
    }
}

/// Which of the net's pins (`spots`) a probe placed or labelled `name` anchors its flag at: the
/// pin the name names ("<refdes>.<pin>", as a probe's `at` and `probe_label` write it), else a pin
/// of a part the name names ("D1 PD6": D1's pin), else the pin of the part with the fewest pins (a
/// discrete part before the MCU), the first such. None without spots. O(spots x name words), no
/// allocation.
pub fn anchor_pin(name: &str, spots: &[PinSpot]) -> Option<usize> {
    named_spot(name, spots).or_else(|| spots.iter().enumerate().min_by_key(|(i, s)| (s.part_pins, *i)).map(|(i, _)| i))
}

/// The pin of `spots` that `name` itself names ([`anchor_pin`]'s first two rules: the pin, else a
/// pin of the part); None when it names neither and the flag takes the net's default place.
pub fn named_spot(name: &str, spots: &[PinSpot]) -> Option<usize> {
    let head = name.lines().next().unwrap_or("");
    if let Some((refdes, number)) = head.split_once('.') {
        if let Some(i) = spots.iter().position(|s| s.refdes == refdes && s.number == number) {
            return Some(i);
        }
    }
    let mut words = head.split(|c: char| c.is_whitespace() || c == '.' || c == ',').filter(|w| !w.is_empty());
    words.find_map(|word| spots.iter().position(|s| s.refdes == word))
}

/// The pin of `pins` (world points) nearest `at`: where a flag dragged along its net lands.
pub fn nearest_pin(pins: impl IntoIterator<Item = [f64; 2]>, at: [f64; 2]) -> Option<usize> {
    pins.into_iter().enumerate()
        .map(|(i, p)| (i, (p[0] - at[0]).hypot(p[1] - at[1])))
        .filter(|(_, d)| d.is_finite())
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// One flag for [`ProbeFlags::draw`]: its identity and where it anchors.
#[derive(Clone, Debug)]
pub struct Flag<'a> {
    /// The probed net's lowered name (the bench channel's target).
    pub net: &'a str,
    pub label: &'a str,
    /// Its probe's hue (`PeekGesture::hue_of`).
    pub hue: usize,
    pub anchor: Anchor,
    /// Where a drop would put a flag (a probe carried over a net, or a flag carried along its
    /// net), drawn before the drop.
    pub ghost: bool,
}

/// The person asks to see a net's lane in the Scope tab, by its net: a probe's flag clicked, or a
/// wire clicked on the sheet or the board (which probes it first when it has no probe).
#[derive(Clone, Debug, PartialEq)]
pub struct OpenInScope(pub String);

/// What the flags did with an event.
#[derive(Clone, Debug, PartialEq)]
pub enum FlagInput {
    /// Not a flag's: the pane handles it.
    None,
    /// A flag has the pointer (hovered, pressed or carried): the pane keeps its hands off.
    Taken,
    /// The flag of this net was clicked: show its lane in the Scope (posted as [`OpenInScope`]).
    Open(String),
    /// The flag of this net was carried and let go at `abs` (absolute): the pane places it at the
    /// net's pin nearest there.
    Moved { net: String, abs: DVec2 },
}

struct Shown {
    net: String,
    area: Area,
    /// On the board, the probe's ring (its callout is `area`).
    ring: Area,
}

/// A sheet tag's name, drawn over the drawing where [`ProbeFlags::draw_behind`] put its tag, and,
/// for a flag scope, its value (at the bottom right; the name then sits at the top left).
struct SheetLabel {
    title: String,
    rect: Rect,
    color: Vec4f,
    points: f64,
    value: Option<String>,
}

/// A pane's `#[live]` flag layer, registered as `mod.widgets.ProbeFlags` (after `live::tag`): the
/// pane hands it the probes with their anchors each draw, and its events before its own.
#[derive(Script, ScriptHook)]
pub struct ProbeFlags {
    #[live]
    draw_stub: DrawProbeStub,
    #[live]
    draw_flag: DrawProbeFlag,
    #[live]
    draw_tag: DrawProbeTag,
    /// A flag scope's wave, ground and play head.
    #[live]
    draw_wave: DrawColor,
    #[live]
    draw_text: DrawText,
    /// Values, in the code face.
    #[live]
    draw_code: DrawText,
    /// Per flag net its wave's row and columns ([`FlagWave`]); a net no flag shows is dropped.
    /// A tag's corner and a scope flag's (a float's), in `Sdf2d.box` units: the sheet's tokens.
    #[live]
    radius: f64,
    #[live]
    radius_lg: f64,
    #[rust]
    waves: HashMap<String, FlagWave>,
    /// What the flags showed as last drawn ([`ShownKey`]).
    #[rust]
    drawn_key: ShownKey,
    /// The flags of the last draw that take input (not ghosts), in draw order; their areas carry
    /// over by net, so a press keeps its flag while it moves.
    #[rust]
    shown: Vec<Shown>,
    #[rust]
    hover: Option<String>,
    /// The flag pressed and where, and whether it has moved off (a carry).
    #[rust]
    pressed: Option<(String, DVec2, bool)>,
    /// The flag carried and the pointer.
    #[rust]
    carried: Option<(String, DVec2)>,
    /// A sheet's tag names from the layer behind the drawing, for the layer over it.
    #[rust]
    labels: Vec<SheetLabel>,
    /// A sheet's carried flag (its label and hue) at the pointer, drawn with the names.
    #[rust]
    riding: Option<(String, Vec4f, DVec2)>,
}

/// The instant the flags show (the one reviewed, else the one heard) and the record's axis
/// `(t0, t1)`: the take, or while Continuous the stretch its ring keeps. None without a record.
/// The status line's word for a hovered net (user: "hover shows the voltage in the status line"):
/// `KICK 108 mV`, its value at the recorder's play head, while a record holds the net. O(log n).
pub fn net_value_line(cx: &Cx, net: NetRef, name: &str) -> Option<String> {
    let live = cx.get_global_ref::<ElecLive>()?;
    let store = live.sim.store.as_ref().filter(|s| s.generation == live.sim.index.generation && !s.is_empty())?;
    let t = instant(live)?.0?;
    let k = live.sim.index.net(net)?;
    let v = value_at(store, k, t).filter(|v| v.is_finite())?;
    Some(format!("{name} {}", eng(f64::from(v), "V", 3)))
}

fn instant(live: &ElecLive) -> Option<(Option<f64>, (f64, f64))> {
    let sim = &live.sim;
    let axis = if sim.status.continuous {
        let (first, last) = sim.record;
        (first.is_finite() && last.is_finite() && last > first).then_some((first, last))?
    } else {
        (0.0, sim.status.take.filter(|end| end.is_finite() && *end > 0.0)?)
    };
    // the recorder's play head: a scrub, else what plays, else where the run is (as the overview)
    Some((Some(sim.scrub.or(sim.heard.map(|(t, _)| t)).unwrap_or(sim.t)).filter(|t| t.is_finite()), axis))
}

/// A board callout's name and value size, points: a short name and a value with its sign and
/// prefixed unit fit the design's width side by side.
const CALLOUT_FONT: f64 = 8.0;

/// A sheet scope tag's name and value size for net names `points` big.
fn sheet_scope_font(points: f64) -> f64 {
    (points * 0.8).min(10.0)
}

/// The height of a flag scope's top row (its name and value, `font` points): its wave starts
/// under it.
fn scope_text_row(font: f64) -> f64 {
    font * 1.25 + 2.0
}

/// A colour with its alpha scaled.
fn faded(c: [f32; 4], a: f32) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3] * a)
}

impl ProbeFlags {
    /// What the flags would show now ([`ShownKey`]).
    fn key(cx: &Cx) -> ShownKey {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return ShownKey::default() };
        let t = instant(live).and_then(|(t, _)| t).map(|t| (t * 1e3).round() as i64);
        let record = live.sim.store.as_ref().map(|store| (store.generation, store.time.len()));
        ShownKey { t, record }
    }

    /// `net`'s scope at the instant shown ([`FlagScope`]): None without a record, or for a net the
    /// run does not trace. Its wave's columns are kept between calls.
    fn scope(&mut self, cx: &Cx, net: &str) -> Option<FlagScope> {
        let live = cx.get_global_ref::<ElecLive>()?;
        let store = live.sim.store.as_ref().filter(|s| s.generation == live.sim.index.generation && !s.is_empty())?;
        let (t, (t0, t1)) = instant(live)?;
        let index = &live.sim.index;
        let wave = self.waves.entry(net.to_owned()).or_default();
        wave.used = true;
        if wave.row.map(|(generation, _)| generation) != Some(index.generation) {
            let row = live.snap.live().units.iter()
                .find_map(|unit| unit.circuit.net(net).map(|n| NetRef { board: unit.id, net: n.id }))
                .and_then(|n| index.net(n));
            wave.row = Some((index.generation, row));
        }
        let k = wave.row.and_then(|(_, row)| row)?;
        let cols = wave.cache.columns(store, k, t0, t1, WAVE_COLS);
        let shown = || cols.cols_data.iter().filter(|c| c.lo.is_finite() && c.hi.is_finite());
        let (lo, hi) = shown().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), c| (lo.min(c.lo), hi.max(c.hi)));
        // a flat line sits mid-height
        let (lo, span) = if hi - lo > 1e-6 { (lo, hi - lo) } else { (lo - 0.5, 1.0) };
        let n = WAVE_COLS as f32;
        let spans = shown().map(|c| {
            let x = c.x0.floor().clamp(0.0, n - 1.0) / n;
            [x, x + 1.0 / n, (c.lo - lo) / span, (c.hi - lo) / span]
        }).collect();
        let head = t.map(|t| ((t - t0) / (t1 - t0)) as f32).filter(|x| (0.0..=1.0).contains(x));
        let value = t.and_then(|t| value_at(store, k, t)).filter(|v| v.is_finite()).map(|v| eng(f64::from(v), "V", 3)).unwrap_or_default();
        Some(FlagScope { spans, head, value })
    }

    /// Starts a draw of the flags' scopes: a net no flag of the last draw showed lets its wave go.
    fn begin_scopes(&mut self, cx: &Cx) {
        self.waves.retain(|_, wave| std::mem::take(&mut wave.used));
        self.drawn_key = Self::key(cx);
    }

    /// A flag scope's ground, outline, wave and play head in `rect`, in `hue`: the wave under the
    /// row of its name and value, `font` points ([`scope_text_row`]).
    fn draw_scope_body(&mut self, cx: &mut Cx2d, look: &InstrumentLook, rect: Rect, hue: Vec4f, scope: &FlagScope, state: TagState, font: f64) {
        self.draw_flag.color = match state {
            TagState::Hover => vec4(hue.x + (1.0 - hue.x) * 0.3, hue.y + (1.0 - hue.y) * 0.3, hue.z + (1.0 - hue.z) * 0.3, hue.w),
            _ => hue,
        };
        self.draw_flag.ground = faded(look.canvas.block_fill, 0.94);
        self.draw_flag.fill = 0.08;
        self.draw_flag.radius = self.radius_lg as f32;
        self.draw_flag.outline = 0.0;
        self.draw_flag.draw_abs(cx, rect);
        self.draw_flag.ground = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_flag.radius = self.radius as f32;
        self.draw_flag.outline = 1.0;
        let row = scope_text_row(font);
        let inner = Rect { pos: rect.pos + dvec2(4.0, row), size: dvec2((rect.size.x - 8.0).max(0.0), (rect.size.y - row - 3.0).max(0.0)) };
        let px = 1.0 / cx.current_dpi_factor().max(0.5);
        self.draw_wave.color = vec4(hue.x, hue.y, hue.z, hue.w * 0.8);
        for [x0, x1, lo, hi] in &scope.spans {
            let left = inner.pos.x + f64::from(*x0) * inner.size.x;
            let right = inner.pos.x + f64::from(*x1) * inner.size.x;
            let top = inner.pos.y + inner.size.y * (1.0 - f64::from(*hi));
            let bottom = inner.pos.y + inner.size.y * (1.0 - f64::from(*lo));
            self.draw_wave.draw_abs(cx, Rect { pos: dvec2(left, top), size: dvec2((right - left).max(px), (bottom - top).max(px)) });
        }
        if let Some(head) = scope.head {
            self.draw_wave.color = faded(look.canvas.ok, 1.0);
            let x = inner.pos.x + f64::from(head) * inner.size.x;
            self.draw_wave.draw_abs(cx, Rect { pos: dvec2(x, inner.pos.y), size: dvec2(px.max(1.0), inner.size.y) });
        }
    }

    /// A flag scope's top row, `font` points, over its wave: the name at the left in `hue` and the
    /// value at the play head at the right. A name too long for the row beside its value is cut
    /// short with an ellipsis; the value is always whole.
    fn draw_scope_text(&mut self, cx: &mut Cx2d, look: &InstrumentLook, rect: Rect, title: &str, hue: Vec4f, value: &str, font: f64) {
        self.draw_code.text_style.font_size = font as f32;
        let width = |this: &mut Self, cx: &mut Cx2d, text: &str| {
            let size = this.draw_code.layout(cx, 0.0, 0.0, None, false, Align::default(), text).size_in_lpxs;
            f64::from(size.width * this.draw_code.font_scale)
        };
        let value_w = if value.is_empty() { 0.0 } else { width(self, cx, value) };
        let room = (rect.size.x - 10.0 - if value.is_empty() { 0.0 } else { value_w + 4.0 }).max(0.0);
        let mut name = title.to_owned();
        if width(self, cx, &name) > room {
            let mut chars: Vec<char> = title.chars().collect();
            while !chars.is_empty() {
                chars.pop();
                name = chars.iter().collect::<String>() + "…";
                if width(self, cx, &name) <= room {
                    break;
                }
            }
        }
        self.draw_code.color = hue;
        self.draw_code.draw_abs(cx, rect.pos + dvec2(5.0, 1.0), &name);
        if !value.is_empty() {
            self.draw_code.color = faded(look.text, 1.0);
            self.draw_code.draw_abs(cx, rect.pos + dvec2(rect.size.x - value_w - 5.0, 1.0), value);
        }
    }

    /// Whether the pointer is over a flag.
    pub fn hovered(&self) -> bool { self.hover.is_some() }

    /// The flag being carried along its net and the pointer (absolute): the pane draws the ghost
    /// of where it would land.
    pub fn carried(&self) -> Option<(&str, DVec2)> {
        self.carried.as_ref().map(|(net, abs)| (net.as_str(), *abs))
    }

    /// The pane shows no flags any more: nothing is hovered, pressed or carried. Twice is harmless.
    pub fn release(&mut self) {
        self.shown.clear();
        self.hover = None;
        self.pressed = None;
        self.carried = None;
        self.labels.clear();
        self.riding = None;
    }

    /// [`Self::handle_event`] for a pane whose drawing comes before its tags: with `yields` (the
    /// pane has a part under or near the pointer), a press or a hover there is the pane's even over
    /// a tag, unless a tag is pressed or carried already. The tags then take no hits, so none
    /// captures that press.
    pub fn handle_event_yielding(&mut self, cx: &mut Cx, event: &Event, pane: Area, yields: bool) -> FlagInput {
        let pointer = matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::TouchUpdate(_));
        if yields && pointer && self.pressed.is_none() && self.carried.is_none() {
            if self.hover.take().is_some() {
                pane.redraw(cx);
            }
            return FlagInput::None;
        }
        self.handle_event(cx, event, pane)
    }

    /// Before the pane's own handling: a press on a flag is the flag's, so the canvas under it
    /// never selects through it. `pane` is redrawn when the hover or a carry moves.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, pane: Area) -> FlagInput {
        if matches!(event, Event::Signal | Event::NextFrame(_)) && !self.shown.is_empty() && Self::key(cx) != self.drawn_key {
            pane.redraw(cx);
        }
        let mut input = FlagInput::None;
        let mut hover = self.hover.clone();
        for (shown, area) in self.shown.iter().flat_map(|shown| [(shown, shown.area), (shown, shown.ring)]) {
            match event.hits(cx, area) {
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                    cx.set_cursor(MouseCursor::Hand);
                    hover = Some(shown.net.clone());
                    input = FlagInput::Taken;
                }
                Hit::FingerHoverOut(_) => {
                    if hover.as_deref() == Some(shown.net.as_str()) { hover = None; }
                }
                Hit::FingerDown(e) if e.is_primary_hit() => {
                    self.pressed = Some((shown.net.clone(), e.abs, false));
                    input = FlagInput::Taken;
                }
                Hit::FingerMove(e) => {
                    if let Some((net, start, moved)) = &mut self.pressed {
                        if *net == shown.net && (*moved || (e.abs - *start).length() > 4.0) {
                            *moved = true;
                            self.carried = Some((net.clone(), e.abs));
                            cx.set_cursor(MouseCursor::Grabbing);
                            pane.redraw(cx);
                        }
                    }
                    input = FlagInput::Taken;
                }
                Hit::FingerLongPress(_) => input = FlagInput::Taken,
                Hit::FingerUp(e) => {
                    let pressed = self.pressed.take().filter(|(net, ..)| *net == shown.net);
                    self.carried = None;
                    input = match pressed {
                        Some((net, _, true)) => {
                            pane.redraw(cx);
                            FlagInput::Moved { net, abs: e.abs }
                        }
                        Some((net, _, false)) if e.is_over && e.is_primary_hit() => {
                            cx.action(OpenInScope(net.clone()));
                            FlagInput::Open(net)
                        }
                        _ => FlagInput::Taken,
                    };
                }
                _ => {}
            }
        }
        if hover != self.hover {
            self.hover = hover;
            pane.redraw(cx);
        }
        input
    }

    /// Draw a board's `flags` in the pane `clip` through `cam`: each probe a ring of its hue on its
    /// pad (sized by [`flag_scale`] of the zoom against `reference`), and its callout (its scope
    /// while there is a record of its net, else its name) in the margin outside the board's
    /// outline `board` (a world box), joined to the ring by a hairline ([`margin_callouts`]); a
    /// callout with no room shows beside its ring while the ring is hovered. The carried flag rides
    /// the pointer. Callouts keep clear of the area's notch and of the pane's tool strip `strip`
    /// as last drawn (empty before its first frame).
    pub fn draw(&mut self, cx: &mut Cx2d, cam: &Camera2d, clip: Rect, reference: f64, flags: &[Flag], board: [f64; 4], strip: Rect) {
        self.draw_flag.radius = self.radius as f32;
        let mut kept = std::mem::take(&mut self.shown);
        self.begin_scopes(cx);
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        cx.push_clip_rect(clip);
        if flags.is_empty() {
            cx.pop_clip_rect();
            return;
        }
        let ring = RING * (flag_scale(cam.px_per_unit, reference) * 16.0).round() / 16.0;
        self.draw_text.text_style.font_size = FLAG_FONT as f32;
        let rest = TagStyle { hue: vec4(1.0, 1.0, 1.0, 1.0), kind: TagKind::Volt, side: LaneSide::Sim, state: TagState::Rest };
        // A probe's callout is a scope of its net while there is a record of it; a ghost is a ring.
        let scopes: Vec<Option<FlagScope>> = flags.iter().map(|flag| if flag.ghost { None } else { self.scope(cx, flag.net) }).collect();
        let sizes: Vec<DVec2> = flags.iter().zip(&scopes).map(|(flag, scope)| match scope {
            Some(_) => SCOPE_FLAG,
            None => {
                let tag = ProbeTag::from_label(flag.label, "");
                tag.layout(tag.measure(cx, &mut self.draw_tag, &self.draw_text), f64::MAX, rest).size
            }
        }).collect();
        let points: Vec<DVec2> = flags.iter().map(|flag| cam.to_screen(flag.anchor.at)).collect();
        // the callouts around the board, clear of the pane's tool strip, one column or two, with
        // a gap all round, and of the area's notch above it
        let shown: Vec<usize> = (0..flags.len()).filter(|&i| !flags[i].ghost).collect();
        let outline = cam.rect_to_screen(board);
        let strip = if strip.size.x > 0.0 && strip.size.y > 0.0 {
            Rect { pos: dvec2(clip.pos.x, strip.pos.y - CALLOUT_GAP), size: dvec2(strip.pos.x + strip.size.x + CALLOUT_GAP - clip.pos.x, strip.size.y + 2.0 * CALLOUT_GAP) }
        } else {
            Rect { pos: clip.pos, size: dvec2(STRIP_W, clip.size.y) }
        };
        let keep_out = [strip, Rect { pos: clip.pos, size: crate::canvas::tools::AREA_NOTCH }];
        let spots: Vec<(DVec2, DVec2)> = shown.iter().map(|&i| (points[i], sizes[i])).collect();
        let mut callouts: Vec<Option<Rect>> = vec![None; flags.len()];
        for (&i, rect) in shown.iter().zip(margin_callouts(&spots, outline, clip, &keep_out)) {
            callouts[i] = rect;
        }
        for (i, flag) in flags.iter().enumerate() {
            let carried = self.carried.as_ref().filter(|(net, _)| !flag.ghost && net == flag.net).map(|(_, abs)| *abs);
            let hovered = !flag.ghost && self.hover.as_deref() == Some(flag.net);
            if let Some(abs) = carried {
                // the carried probe's callout rides the pointer
                callouts[i] = Some(Rect { pos: abs + dvec2(12.0, -sizes[i].y * 0.5), size: sizes[i] });
            } else if callouts[i].is_none() && hovered {
                // no room in the margin: beside its ring while hovered, within the pane
                let pos = points[i] + dvec2(ring + CALLOUT_GAP, -sizes[i].y * 0.5);
                let pos = dvec2(pos.x.min(clip.pos.x + clip.size.x - sizes[i].x - CALLOUT_GAP).max(clip.pos.x), pos.y.clamp(clip.pos.y, (clip.pos.y + clip.size.y - sizes[i].y).max(clip.pos.y)));
                callouts[i] = Some(Rect { pos, size: sizes[i] });
            }
        }
        // In layers (leaders, rings, then callouts), so probes apart never interleave their batches.
        for (i, flag) in flags.iter().enumerate() {
            let Some(rect) = callouts[i] else { continue };
            let hue = look.channel(flag.hue);
            let at = points[i];
            let end = edge_toward(rect, at);
            let lo = dvec2(at.x.min(end.x), at.y.min(end.y)) - dvec2(4.0, 4.0);
            let hi = dvec2(at.x.max(end.x), at.y.max(end.y)) + dvec2(4.0, 4.0);
            self.draw_stub.color = vec4(hue[0], hue[1], hue[2], if flag.ghost { 0.3 } else { 0.6 });
            self.draw_stub.from = vec2((at.x - lo.x) as f32, (at.y - lo.y) as f32);
            self.draw_stub.to = vec2((end.x - lo.x) as f32, (end.y - lo.y) as f32);
            self.draw_stub.draw_abs(cx, Rect { pos: lo, size: hi - lo });
        }
        let mut rings = Vec::with_capacity(flags.len());
        for (i, flag) in flags.iter().enumerate() {
            let hue = look.channel(flag.hue);
            let hovered = !flag.ghost && self.hover.as_deref() == Some(flag.net);
            let r = if hovered { ring + 1.0 } else { ring };
            let rect = Rect { pos: points[i] - dvec2(r, r), size: dvec2(2.0 * r, 2.0 * r) };
            self.draw_flag.color = vec4(hue[0], hue[1], hue[2], if flag.ghost { 0.45 } else { 1.0 });
            self.draw_flag.ground = vec4(0.0, 0.0, 0.0, 0.0);
            self.draw_flag.fill = if hovered { 0.35 } else { 0.0 };
            self.draw_flag.radius = r as f32;
            self.draw_flag.outline = 1.5;
            self.draw_flag.draw_abs(cx, rect);
            rings.push(rect);
        }
        self.draw_flag.radius = self.radius as f32;
        self.draw_flag.outline = 1.0;
        for (i, flag) in flags.iter().enumerate() {
            let hue = look.channel(flag.hue);
            let hue = vec4(hue[0], hue[1], hue[2], 1.0);
            let hovered = !flag.ghost && self.hover.as_deref() == Some(flag.net);
            if let Some(rect) = callouts[i] {
                match &scopes[i] {
                    Some(scope) => {
                        self.draw_scope_body(cx, look, rect, hue, scope, if hovered { TagState::Hover } else { TagState::Rest }, CALLOUT_FONT);
                        let title = ProbeTag::from_label(flag.label, "").title;
                        self.draw_scope_text(cx, look, rect, title, hue, &scope.value, CALLOUT_FONT);
                    }
                    None => self.draw_name_pill(cx, look, rect, ProbeTag::from_label(flag.label, "").title, hue, flag.ghost),
                }
            }
            if !flag.ghost {
                let (area, ring_area) = kept.iter().position(|shown| shown.net == flag.net)
                    .map_or((Area::Empty, Area::Empty), |i| { let shown = kept.swap_remove(i); (shown.area, shown.ring) });
                let (mut area, mut ring_area) = (area, ring_area);
                if let Some(rect) = callouts[i] {
                    cx.add_rect_area(&mut area, rect);
                } else {
                    area = Area::Empty;
                }
                // the ring takes the pointer a few points round it, as a pad's hit does
                let hit = Rect { pos: rings[i].pos - dvec2(3.0, 3.0), size: rings[i].size + dvec2(6.0, 6.0) };
                cx.add_rect_area(&mut ring_area, hit);
                self.shown.push(Shown { net: flag.net.to_owned(), area, ring: ring_area });
            }
        }
        cx.pop_clip_rect();
    }

    /// A probe's name on a pill of its hue in `rect` (a callout before there is a record).
    fn draw_name_pill(&mut self, cx: &mut Cx2d, look: &InstrumentLook, rect: Rect, title: &str, hue: Vec4f, ghost: bool) {
        self.draw_flag.color = vec4(hue.x, hue.y, hue.z, if ghost { 0.45 } else { 1.0 });
        self.draw_flag.ground = faded(look.canvas.block_fill, 0.92);
        self.draw_flag.fill = 0.16;
        self.draw_flag.radius = (rect.size.y * 0.5) as f32;
        self.draw_flag.outline = 0.0;
        self.draw_flag.draw_abs(cx, rect);
        self.draw_flag.ground = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_flag.radius = self.radius as f32;
        self.draw_flag.outline = 1.0;
        self.draw_code.text_style.font_size = 9.0;
        self.draw_code.color = hue;
        let size = self.draw_code.layout(cx, 0.0, 0.0, None, false, Align::default(), title).size_in_lpxs;
        let size = dvec2(f64::from(size.width * self.draw_code.font_scale), f64::from(size.height * self.draw_code.font_scale));
        self.draw_code.draw_abs(cx, rect.pos + (rect.size - size) * 0.5, title);
    }
}

impl ProbeFlags {
    /// A sheet's tags, behind its drawing (before its symbols and wires): `flags[i]`'s tag at the
    /// world box `rects[i]` ([`place_tags`]) with a thin leader from its pin, through `cam`, in the
    /// pane `clip`; the tags take input. Their names, `points` big, are kept for
    /// [`Self::draw_labels`], over the drawing; under `min_points` a tag is a chip of its hue with
    /// no name. The carried flag rides the pointer, drawn with the names.
    pub fn draw_behind(&mut self, cx: &mut Cx2d, cam: &Camera2d, clip: Rect, flags: &[Flag], rects: &[[f64; 4]], points: f64, min_points: f64) {
        self.draw_flag.radius = self.radius as f32;
        let mut kept = std::mem::take(&mut self.shown);
        self.labels.clear();
        self.riding = None;
        if flags.is_empty() { return; }
        self.begin_scopes(cx);
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        let named = points >= min_points;
        let mut placed = Vec::with_capacity(flags.len());
        for (flag, world) in flags.iter().zip(rects) {
            let hue = look.channel(flag.hue);
            let hue = vec4(hue[0], hue[1], hue[2], if flag.ghost { 0.5 } else { 1.0 });
            match &self.carried {
                Some((net, abs)) if !flag.ghost && net == flag.net => {
                    self.riding = Some((flag.label.to_owned(), hue, *abs));
                    continue;
                }
                _ => {}
            }
            // A chip stays a few points each way however far out the zoom is.
            let mut rect = cam.rect_to_screen(*world);
            let small = dvec2((3.0 - rect.size.x).max(0.0), (2.0 - rect.size.y).max(0.0));
            rect = Rect { pos: rect.pos - small * 0.5, size: rect.size + small };
            let at = cam.to_screen(flag.anchor.at);
            if clip.contains(at) || rect_meets(&rect, &clip) {
                placed.push((flag, hue, at, rect));
            }
        }
        cx.push_clip_rect(clip);
        // In layers (leaders, then tags), so tags apart never interleave their batches.
        for &(_, hue, at, rect) in &placed {
            let end = dvec2(at.x.clamp(rect.pos.x, rect.pos.x + rect.size.x), at.y.clamp(rect.pos.y, rect.pos.y + rect.size.y));
            let lo = dvec2(at.x.min(end.x), at.y.min(end.y)) - dvec2(4.0, 4.0);
            let hi = dvec2(at.x.max(end.x), at.y.max(end.y)) + dvec2(4.0, 4.0);
            self.draw_stub.color = vec4(hue.x, hue.y, hue.z, hue.w * 0.7);
            self.draw_stub.from = vec2((at.x - lo.x) as f32, (at.y - lo.y) as f32);
            self.draw_stub.to = vec2((end.x - lo.x) as f32, (end.y - lo.y) as f32);
            self.draw_stub.draw_abs(cx, Rect { pos: lo, size: hi - lo });
        }
        for &(flag, hue, _, rect) in &placed {
            let hovered = !flag.ghost && self.hover.as_deref() == Some(flag.net);
            // named and with a record of its net, a tag is a scope of it
            let scope = if named && !flag.ghost { self.scope(cx, flag.net) } else { None };
            match &scope {
                Some(scope) => {
                    self.draw_scope_body(cx, look, rect, hue, scope, if hovered { TagState::Hover } else { TagState::Rest }, sheet_scope_font(points));
                }
                None => {
                    self.draw_flag.color = hue;
                    self.draw_flag.fill = match (named, hovered) { (false, _) => 0.9, (true, true) => 0.34, (true, false) => 0.2 };
                    self.draw_flag.radius = (rect.size.y * 0.5) as f32;
                    self.draw_flag.outline = 0.0;
                    self.draw_flag.draw_abs(cx, rect);
                    self.draw_flag.radius = self.radius as f32;
                    self.draw_flag.outline = 1.0;
                }
            }
            if named {
                let title = ProbeTag::from_label(flag.label, "").title.to_owned();
                self.labels.push(SheetLabel { title, rect, color: hue, points, value: scope.map(|scope| scope.value) });
            }
            if !flag.ghost {
                let mut area = kept.iter().position(|shown| shown.net == flag.net).map_or(Area::Empty, |i| kept.swap_remove(i).area);
                cx.add_rect_area(&mut area, rect);
                self.shown.push(Shown { net: flag.net.to_owned(), area, ring: Area::Empty });
            }
        }
        cx.pop_clip_rect();
    }

    /// The names of the tags [`Self::draw_behind`] placed, over the drawing, each centred in its
    /// tag (a scope's in its top row, with its value at the right), and the carried flag
    /// riding the pointer as its probe's tag, in the pane `clip`. The instant itself shows only in
    /// the recorder.
    pub fn draw_labels(&mut self, cx: &mut Cx2d, clip: Rect) {
        if self.labels.is_empty() && self.riding.is_none() { return; }
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        cx.push_clip_rect(clip);
        let labels = std::mem::take(&mut self.labels);
        for label in &labels {
            if let Some(value) = &label.value {
                self.draw_scope_text(cx, look, label.rect, &label.title, label.color, value, sheet_scope_font(label.points));
                continue;
            }
            self.draw_text.text_style.font_size = label.points as f32;
            self.draw_text.color = label.color;
            let size = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &label.title).size_in_lpxs;
            let pos = label.rect.pos + (label.rect.size - dvec2(size.width as f64, size.height as f64)) * 0.5;
            self.draw_text.draw_abs(cx, pos, &label.title);
        }
        self.labels = labels;
        if let Some((label, hue, abs)) = &self.riding {
            self.draw_text.text_style.font_size = FLAG_FONT as f32;
            let style = TagStyle { hue: *hue, kind: TagKind::Volt, side: LaneSide::Sim, state: TagState::Carried };
            let tag = ProbeTag::from_label(label, "");
            let size = tag.layout(tag.measure(cx, &mut self.draw_tag, &self.draw_text), f64::MAX, style).size;
            tag.draw(cx, &mut self.draw_tag, &mut self.draw_text, Rect { pos: *abs + dvec2(12.0, -size.y * 0.5), size }, style);
        }
        cx.pop_clip_rect();
    }
}

fn rect_meets(a: &Rect, b: &Rect) -> bool {
    a.pos.x < b.pos.x + b.size.x && b.pos.x < a.pos.x + a.size.x && a.pos.y < b.pos.y + b.size.y && b.pos.y < a.pos.y + a.size.y
}
