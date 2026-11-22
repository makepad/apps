//! The Bench editor: everything outside the running board (V9, user 11-18). What powers it and
//! what drives it, the board with every connection to the bench, what its outputs drive, the
//! probes (each a lane in the Scope), and its front panel (the keys and knobs generated from its
//! parts). The page is a grid of up to three columns that wraps as the area narrows.
//!
//! A supply's or a source's value opens a menu of values (the usual rails for a voltage, a 1-2-5
//! ladder for the rest); a pick is one bench edit (`BenchOp::SetSupply` / `SetSource`) that
//! re-records the take like any value edit. A probe's ⋯ removes it; "+ Add probe" lists the nets.
//! POWER's "+ Add" puts a supply on a net, SOURCES' a source or load of a kind on a net (against
//! ground); a card's ⋯ takes its supply or source off (`BenchOp::Add*` / `Remove*`).
//! EXPECTATIONS' Check bench asks the app for the counted check (`CheckBench`: the person's
//! `elec.bench.run`, a run of its own judged at every point) and Stop check stops it; each row
//! then shows its verdict on that run under the live one, until the design moves on.
//!
//! Cost: the page is laid out once per width, theme, bench, jack loads or control count (O(supplies,
//! sources, probes) labels); a frame draws its tiles, glyphs and labels, and the front panel's own
//! draw; an open menu draws only its visible rows. A pointer move over the page is one search of
//! its buttons.

use super::{
    bench::{bench_time_per_div, new_card, remove_probe, BenchProbe},
    controls_panel::ControlsPanel,
    drive_lane::{DriveLane, GeneratorAction},
    logic_view::ShowInScope,
    look::InstrumentLook,
    scope::ScopeSettings,
    semantics,
    sources::{BenchOut, BenchView, UnitState},
};
use crate::{
    canvas::looks::Rgba,
    live::{
        probe::click_probe_op,
        sim::{write_run_settings, SimAction},
    },
    snapshot::{next_tag, ElecLive, ViewAction},
    trace::cursor::eng,
};
use fab_elec_bench::expect::{Bound, CheckState, Expect, Outcome, Probe, Verdict};
use fab_elec_bench::judge::Judged;
use fab_elec_bench::{audio::output_jacks, bench::SourceKind, BenchReport, SimCmd};
use fab_elec_circuit::{Circuit, NetKind};
use fab_elec_core::ids::NetRef;
use fab_elec_design::{BenchOp, DesignOp, ProbeQuantity};
use fab_elec_lang::sys_sim::AudioLoad;
use fab_elec_parts::PartKind;
use makepad_widgets_core::{makepad_draw::text::layouter::LaidoutText, *};
use makepad_widgets_pickers::combo_box::ComboBoxWidgetRefExt;
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    set_type_default() do #(DrawSetupBox::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        frame: #0000
        radius: 5.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.radius)
            sdf.fill_keep(self.color)
            sdf.stroke(self.frame, 1.0)
            return sdf.result
        }
    }

    // A bench card's glyph in the design's strokes (a 70 x 54 drawing scaled to the rect): a
    // supply (kind 0), a source (1), a load (2).
    set_type_default() do #(DrawBenchIcon::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        kind: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let s = min(self.rect_size.x / 70.0, self.rect_size.y / 54.0)
            let c = self.rect_size * 0.5
            let w = 2.2 * s
            if self.kind < 0.5 {
                sdf.box(c.x - 23.0 * s, c.y - 9.0 * s, 46.0 * s, 18.0 * s, 4.5 * s)
                sdf.stroke(self.color, w)
                sdf.move_to(c.x - 8.0 * s, c.y)
                sdf.line_to(c.x + 8.0 * s, c.y)
                sdf.stroke(self.color, w)
                sdf.move_to(c.x - 30.0 * s, c.y)
                sdf.line_to(c.x - 23.0 * s, c.y)
                sdf.stroke(self.color, w)
                return sdf.result
            }
            if self.kind < 1.5 {
                sdf.box(c.x - 22.0 * s, c.y - 16.0 * s, 44.0 * s, 32.0 * s, 1.0 * s)
                sdf.stroke(self.color, w)
                sdf.move_to(c.x - 14.0 * s, c.y)
                sdf.line_to(c.x - 10.5 * s, c.y - 7.4 * s)
                sdf.line_to(c.x - 7.0 * s, c.y - 10.5 * s)
                sdf.line_to(c.x - 3.5 * s, c.y - 7.4 * s)
                sdf.line_to(c.x, c.y)
                sdf.line_to(c.x + 3.5 * s, c.y + 7.4 * s)
                sdf.line_to(c.x + 7.0 * s, c.y + 10.5 * s)
                sdf.line_to(c.x + 10.5 * s, c.y + 7.4 * s)
                sdf.line_to(c.x + 14.0 * s, c.y)
                sdf.stroke(self.color, w)
                return sdf.result
            }
            sdf.box(c.x - 16.0 * s, c.y - 12.0 * s, 32.0 * s, 24.0 * s, 1.0 * s)
            sdf.stroke(self.color, w)
            sdf.move_to(c.x - 30.0 * s, c.y - 5.0 * s)
            sdf.line_to(c.x - 16.0 * s, c.y - 5.0 * s)
            sdf.stroke(self.color, w)
            sdf.move_to(c.x - 30.0 * s, c.y + 5.0 * s)
            sdf.line_to(c.x - 16.0 * s, c.y + 5.0 * s)
            sdf.stroke(self.color, w)
            sdf.circle(c.x + 2.0 * s, c.y, 6.0 * s)
            sdf.stroke(self.color, w)
            return sdf.result
        }
    }

    // An output jack's load: a preset, or a resistance typed in
    mod.widgets.BenchJackLoad = ComboBox{
        width: Fill height: 22 margin: 0 padding: Inset{left: 6 right: 16}
        accepts_typed: true
        // the value in the code font, as every combo's value (v10)
        input +: {draw_text +: {text_style: theme.font_code{font_size: theme.prop_font_size}}}
        draw_bg +: {
            border_size: 0.0 border_radius: theme.corner_radius
            color: theme.prop_field color_hover: theme.prop_field_hover color_focus: theme.prop_field
        }
    }

    // The page's inks and faces default to the theme tokens Fab's own are made of; the shell sets
    // Fab's on its instance.
    mod.widgets.BenchSetup = set_type_default() do #(BenchSetup::register_widget(vm)) {
        width: Fill height: Fill
        draw_text +: {text_style: theme.font_regular {font_size: theme.prop_font_size}}
        draw_small +: {text_style: theme.font_regular {font_size: 7.5}}
        draw_code +: {text_style: theme.font_code {font_size: theme.prop_font_size}}
        draw_big +: {text_style: theme.font_regular {font_size: 10.5}}
        color_ground: theme.color_surface_container
        color_card: theme.prop_field
        color_hover: theme.prop_field_hover
        color_text: theme.color_text
        color_dim: theme.color_text_meta
        color_muted: theme.color_text_meta
        color_accent: theme.color_focus
        color_in: #5cc8e6
        color_out: #e8ecf2
        color_probes: theme.color_warning
        // the sheet's corners; Fab sets its tokens on its instance (fab.radius / fab.radius_lg),
        // and the page hands them to its fields and its controls panel
        radius: theme.corner_radius
        radius_lg: theme.container_corner_radius
        color_popover: theme.color_surface_container_high
        color_popover_border: theme.color_bevel_outset_2
        color_menu_hover: theme.color_bg_highlight
        generator: mod.widgets.DriveLane {}
        controls_panel: mod.widgets.ControlsPanel {}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawSetupBox {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    frame: Vec4f,
    #[live]
    radius: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawBenchIcon {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    kind: f32,
}

/// The page's margin, and the gap between its columns and between its rows.
const PAD: f64 = 12.0;
/// The page's top inset: the intro's first line sits beside the area's notch, past it by
/// [`NOTCH_INDENT`], and the rest under it (v10.2).
const TOP: f64 = 10.0;
/// How far in the intro's first line starts: past the notch and its fillet.
const NOTCH_INDENT: f64 = crate::canvas::tools::AREA_NOTCH.x + 8.0 - PAD;
const GAP: f64 = 14.0;
/// A grid column is never narrower than this: the page shows as many as fit, up to three.
const COLUMN_MIN: f64 = 240.0;
/// Inside a card: its padding across and down, and the gap after it.
const CARD_X: f64 = 10.0;
const CARD_Y: f64 = 8.0;
const CARD_GAP: f64 = 12.0;
/// A card's glyph.
const ICON: DVec2 = DVec2 { x: 44.0, y: 34.0 };
/// A field's row (`prop_row_height`), a probe's row, a chip's height.
const ROW: f64 = 18.0;
/// An output jack's load field's height.
const JACK_FIELD: f64 = 22.0;
/// A load field's least width beside its jack chip: its longest preset, `Line in (10 kΩ) ▾`.
const JACK_FIELD_MIN: f64 = 128.0;
const PROBE_ROW: f64 = 24.0;
const CHIP_H: f64 = 20.0;
/// A menu's row and its padding inside the popover.
const MENU_ROW: f64 = 20.0;
const MENU_PAD: f64 = 4.0;

const SUPPLY: f32 = 0.0;
const SOURCE: f32 = 1.0;
const LOAD: f32 = 2.0;

const INTRO: &str = "The bench is everything outside the board: what powers it, what drives it, what is plugged into it, what you press and turn, and where the probes sit. No graphs here: every probe is a lane in the Scope.";

fn color(c: Rgba) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3])
}

fn rgba(c: Vec4f) -> Rgba {
    [c.x, c.y, c.z, c.w]
}

fn tint(base: Rgba, ink: Rgba, amount: f32) -> Rgba {
    [0, 1, 2, 3].map(|i| base[i] + (ink[i] - base[i]) * amount)
}

fn alpha(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], c[3] * a]
}

fn at(rect: Rect, origin: DVec2) -> Rect {
    Rect { pos: origin + rect.pos, ..rect }
}

fn fit_at(pos: DVec2, width: f64) -> Walk {
    Walk::new(Size::Fixed(width), Size::fit()).with_abs_pos(pos)
}

/// The page's inks and faces, as the widget's colours give them.
#[derive(Clone, Copy)]
struct Inks {
    warn: Rgba,
    ground: Rgba,
    card: Rgba,
    hover: Rgba,
    text: Rgba,
    dim: Rgba,
    muted: Rgba,
    accent: Rgba,
    chip_in: Rgba,
    chip_out: Rgba,
    chip_probes: Rgba,
    ok: Rgba,
    popover: Rgba,
    popover_border: Rgba,
    menu_hover: Rgba,
}

/// The page's type: body text (11 px), small captions and shelf titles (10 px), code (11 px),
/// and the board's name (14 px).
#[derive(Clone, Copy, PartialEq)]
enum Font {
    Body,
    Small,
    Code,
    Big,
}

struct Paint<'a> {
    ink: Inks,
    /// The sheet's corners (the widget's `radius`, `radius_lg`): a control's, and a card's.
    radius: f32,
    radius_lg: f32,
    panel: &'a mut DrawSetupBox,
    icon: &'a mut DrawBenchIcon,
    fonts: [&'a mut DrawText; 4],
}

impl Paint<'_> {
    /// A pill's corner for a height, where the sheet rounds at all (the kit draws twice the value).
    fn pill(&self, height: f64) -> f32 {
        if self.radius > 0.0 { (height * 0.25) as f32 } else { 0.0 }
    }

    fn font(&self, font: Font) -> &DrawText {
        self.fonts[font as usize]
    }

    fn rounded(&mut self, cx: &mut Cx2d, rect: Rect, fill: Rgba, frame: Rgba, radius: f32) {
        self.panel.color = color(fill);
        self.panel.frame = color(frame);
        self.panel.radius = radius;
        self.panel.draw_abs(cx, rect);
    }

    fn width(&self, cx: &mut Cx, font: Font, text: &str) -> f64 {
        let draw = self.font(font);
        let layout = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
        f64::from(layout.size_in_lpxs.width * draw.font_scale)
    }

    fn height(&self, cx: &mut Cx, font: Font) -> f64 {
        let draw = self.font(font);
        let layout = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), "Ag");
        f64::from(layout.size_in_lpxs.height * draw.font_scale)
    }
}

struct Label {
    rect: Rect,
    layout: Rc<LaidoutText>,
    font: Font,
    ink: Rgba,
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Scope(usize),
    /// A probe's menu: remove it.
    More(usize),
    /// A supply's or a source's value (an index in the page's fields): its menu of values.
    Field(usize),
    /// The menu of nets a probe can go on.
    AddProbe,
    /// The menu of nets a supply can go on.
    AddSupply,
    /// The menu of the kinds of source, then of the nets one can drive.
    AddSource,
    /// A supply's or a source's card menu (an index in the page's things on the bench): remove it.
    CardMore(usize),
    /// EXPECTATIONS' Check bench, or Stop check while one runs.
    Check,
}

/// What a value field edits: the supply on a net, or the source or load between two nets.
#[derive(Clone, PartialEq)]
enum Stim {
    Supply { net: String },
    Source { a: String, b: String },
}

/// An editable value on the page: what it belongs to, its option, its value and unit, and the
/// words naming it in an undo step.
#[derive(Clone)]
struct Field {
    stim: Stim,
    key: &'static str,
    value: Option<f64>,
    unit: &'static str,
    what: String,
}

/// What a menu row does.
#[derive(Clone)]
enum Pick {
    Value(usize, f64),
    Remove(usize),
    AddProbe(String),
    AddSupply(String),
    /// A kind of source picked (its bench call): the nets come next.
    SourceKind(&'static str),
    AddSource(&'static str, String),
    /// Take a supply or a source off (an index in the page's things on the bench).
    RemoveStim(usize),
}

/// The kinds of source "+ Add" offers: their names, bench calls and first options (a start the
/// value menus then change).
pub const SOURCE_KINDS: [(&str, &str, &[(&str, f64)]); 4] = [
    ("DC voltage", "vsrc", &[("dc", 1.0)]),
    ("DC current", "isrc", &[("dc", 1e-3)]),
    ("Voltage pulse", "pulse", &[("v", 3.3), ("width", 1e-3), ("every", 10e-3)]),
    ("Load", "load", &[("r", 10e3)]),
];

/// A supply's first voltage when "+ Add" puts one on (the voltage menu changes it).
pub const SUPPLY_V: f64 = 5.0;

struct MenuItem {
    text: String,
    pick: Pick,
    current: bool,
}

/// An open menu: what it offers, the button it hangs from (relative to the page's area), its
/// scroll, the row under the pointer, and where it drew (relative to the area, for hits).
struct Menu {
    anchor: Rect,
    items: Vec<MenuItem>,
    scroll: f64,
    hovered: Option<usize>,
    pressed: Option<usize>,
    rect: Rect,
}

impl Menu {
    fn height(&self) -> f64 {
        self.items.len() as f64 * MENU_ROW + 2.0 * MENU_PAD
    }

    /// The row at `pos` (relative to the area).
    fn row_at(&self, pos: DVec2) -> Option<usize> {
        if !self.rect.contains(pos) {
            return None;
        }
        let y = pos.y - self.rect.pos.y - MENU_PAD + self.scroll;
        (y >= 0.0).then(|| (y / MENU_ROW) as usize).filter(|&i| i < self.items.len())
    }
}

/// Two values the same up to rounding (a written literal against its parsed value).
fn same_value(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-30)
}

/// The values a field's menu offers: for a voltage the usual rails, for the rest a 1-2-5 ladder two
/// decades either side of the value; the value itself always, and zero where the option allows it.
fn ladder(field: &Field) -> Vec<f64> {
    let zero = matches!(field.key, "ramp" | "at" | "low" | "dc" | "i");
    let mut values: Vec<f64> = if field.unit == "V" {
        vec![1.2, 1.8, 2.5, 3.3, 5.0, 9.0, 12.0, 15.0, 24.0]
    } else {
        let fallback = match field.unit {
            "A" => 0.1,
            "s" => 1e-3,
            _ => 1e3,
        };
        let centre = field.value.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(fallback);
        let decade = centre.log10().floor() as i32;
        (decade - 2..=decade + 2).flat_map(|e| [1.0, 2.0, 5.0].map(|m| m * 10f64.powi(e))).collect()
    };
    values.extend(field.value.filter(|v| v.is_finite()));
    if zero {
        values.push(0.0);
    }
    values.sort_by(f64::total_cmp);
    values.dedup_by(|a, b| same_value(*a, *b));
    values
}

/// How a button shows: flat, a face only under the pointer.
#[derive(Clone, Copy, PartialEq)]
enum Face {
    Flat,
}

struct Button {
    rect: Rect,
    choice: Choice,
    key: u64,
    name: String,
    face: Face,
}

struct Tile {
    rect: Rect,
    fill: Rgba,
    frame: Rgba,
    radius: f32,
}

/// One column of the page: its tiles, glyphs, labels and buttons, with positions relative to the
/// column (independent of scrolling).
#[derive(Default)]
struct Column {
    width: f64,
    height: f64,
    tiles: Vec<Tile>,
    icons: Vec<(Rect, f32)>,
    labels: Vec<Label>,
    buttons: Vec<Button>,
}

/// A chip: a connection's words in code type, outlined in its colour.
struct Chip {
    text: String,
    ink: Rgba,
}

impl Column {
    fn new(width: f64) -> Self {
        Self { width, ..Default::default() }
    }

    /// Text at `pos`, wrapped at `width`: its height.
    fn label(&mut self, cx: &mut Cx, p: &Paint, font: Font, text: &str, pos: DVec2, width: f64, centred: bool, ink: Rgba) -> f64 {
        let draw = p.font(font);
        let width = width.max(1.0);
        let align = Align { x: if centred { 0.5 } else { 0.0 }, y: 0.0 };
        let layout = draw.layout(cx, 0.0, 0.0, Some((width / f64::from(draw.font_scale)) as f32), true, align, text);
        let height = f64::from(layout.size_in_lpxs.height * draw.font_scale);
        self.labels.push(Label { rect: Rect { pos, size: dvec2(width, height) }, layout, font, ink });
        height
    }

    /// One line of text at `pos`, never wrapped (cut at the column's edge): its size.
    fn line(&mut self, cx: &mut Cx, p: &Paint, font: Font, text: &str, pos: DVec2, ink: Rgba) -> DVec2 {
        let draw = p.font(font);
        let layout = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
        let size = dvec2(f64::from(layout.size_in_lpxs.width * draw.font_scale), f64::from(layout.size_in_lpxs.height * draw.font_scale));
        let width = size.x.min(self.width - pos.x).max(0.0);
        self.labels.push(Label { rect: Rect { pos, size: dvec2(width, size.y) }, layout, font, ink });
        size
    }

    fn note(&mut self, cx: &mut Cx, p: &Paint, text: &str) {
        let h = self.label(cx, p, Font::Body, text, dvec2(0.0, self.height), self.width, false, p.ink.dim);
        self.height += h + CARD_GAP;
    }

    /// A note whose first line starts `indent` in, beside the area's notch, and the rest under
    /// it; when not one word fits beside the notch, all of it under. O(words) text layouts.
    fn note_past(&mut self, cx: &mut Cx, p: &Paint, text: &str, indent: f64) {
        let room = self.width - indent;
        let mut first = 0;
        for end in text.match_indices(' ').map(|(i, _)| i).chain(std::iter::once(text.len())) {
            if p.width(cx, Font::Body, &text[..end]) > room {
                break;
            }
            first = end;
        }
        if first == 0 {
            self.height += crate::canvas::tools::AREA_NOTCH.y + 6.0 - TOP;
            return self.note(cx, p, text);
        }
        self.height += self.label(cx, p, Font::Body, &text[..first], dvec2(indent, self.height), room, false, p.ink.dim);
        let rest = text[first..].trim_start();
        if !rest.is_empty() {
            self.height += self.label(cx, p, Font::Body, rest, dvec2(0.0, self.height), self.width, false, p.ink.dim);
        }
        self.height += CARD_GAP;
    }

    /// A shelf's head: its title in small capitals, then what it holds in words, and its add
    /// button at the right.
    fn shelf(&mut self, cx: &mut Cx, p: &Paint, title: &str, hint: &str, add: Option<(&str, Choice)>) {
        // one flow, as the design's section heads: the title, what it holds, then its add
        // button, each wrapping to a line of its own when the column has no room for it
        let y = self.height;
        let size = self.line(cx, p, Font::Small, title, dvec2(0.0, y + 1.0), p.ink.dim);
        let line_h = size.y.max(p.height(cx, Font::Body));
        let (mut x, mut row_y, mut bottom) = (size.x + 14.0, y, y + line_h);
        if !hint.is_empty() {
            let hint_w = p.width(cx, Font::Body, hint);
            if x + hint_w > self.width {
                (x, row_y) = (0.0, bottom + 2.0);
            }
            let h = self.label(cx, p, Font::Body, hint, dvec2(x, row_y), self.width - x, false, p.ink.muted);
            bottom = bottom.max(row_y + h);
            x += hint_w.min(self.width - x) + 8.0;
        }
        if let Some((text, choice)) = add {
            let w = p.width(cx, Font::Body, text) + 12.0;
            if x + w > self.width {
                (x, row_y) = (0.0, bottom + 2.0);
            }
            let by = row_y + (line_h - ROW) * 0.5;
            let name = format!("{title} {text}");
            let key = semantics::key("bench add", &name, 0);
            self.button(cx, p, text, p.ink.dim, Face::Flat, (choice, key, name), dvec2(x, by), w);
            bottom = bottom.max(by + ROW);
        }
        self.height = bottom + 8.0;
    }

    fn tile(&mut self, rect: Rect, fill: Rgba, frame: Rgba, radius: f32) {
        self.tiles.push(Tile { rect, fill, frame, radius });
    }

    /// A chip at `pos`: its width.
    fn chip(&mut self, cx: &mut Cx, p: &Paint, chip: &Chip, pos: DVec2, width: Option<f64>, fill: Rgba) -> f64 {
        let w = width.unwrap_or_else(|| p.width(cx, Font::Code, &chip.text) + 16.0);
        self.tile(Rect { pos, size: dvec2(w, CHIP_H) }, fill, alpha(chip.ink, 0.45), p.pill(CHIP_H));
        let th = p.height(cx, Font::Code);
        // cut at the chip's own edge
        let saved = std::mem::replace(&mut self.width, pos.x + w - 8.0);
        self.line(cx, p, Font::Code, &chip.text, pos + dvec2(8.0, (CHIP_H - th) * 0.5), chip.ink);
        self.width = saved;
        w
    }

    /// A card for a thing on the bench: its glyph, its name, its values as caption and value, and
    /// the chip of what it connects to (beside, or under the values when the column is narrow).
    fn device(&mut self, cx: &mut Cx, p: &Paint, icon: f32, title: &str, fields: &[(&str, String, Option<usize>)], chip: Option<Chip>, more: Option<usize>) {
        let top = self.height;
        let card = self.tiles.len();
        self.tile(Rect::default(), p.ink.card, p.ink.card, p.radius_lg);
        let body_x = CARD_X + ICON.x + CARD_X;
        let chip_w = chip.as_ref().map_or(0.0, |c| p.width(cx, Font::Code, &c.text) + 16.0);
        let beside = chip.is_some() && self.width - body_x - CARD_X - chip_w - CARD_X >= 110.0;
        let body_end = self.width - CARD_X - if beside { chip_w + CARD_X } else { 0.0 };
        // the card's menu at its top right, the title cut short of it
        let more_w = p.width(cx, Font::Body, "⋯") + 12.0;
        if let Some(index) = more {
            let name = format!("More for {title}");
            let key = semantics::key("bench card", &name, 0);
            self.button(cx, p, "⋯", p.ink.dim, Face::Flat, (Choice::CardMore(index), key, name), dvec2(self.width - 4.0 - more_w, top + 4.0), more_w);
        }
        let title_end = body_end.min(self.width - 4.0 - more_w - 4.0);
        let mut y = top + CARD_Y;
        y += self.label(cx, p, Font::Body, title, dvec2(body_x, y), title_end - body_x, false, p.ink.text) + 4.0;
        // the values flow in rows and wrap
        let (small_h, body_h) = (p.height(cx, Font::Small), p.height(cx, Font::Body));
        let mut x = body_x;
        let mut rows = 0;
        for (caption, value, edit) in fields {
            // an editable value carries its menu's caret
            let value = if edit.is_some() { format!("{value} ▾") } else { value.clone() };
            let value_w = p.width(cx, Font::Body, &value);
            let w = p.width(cx, Font::Small, caption) + 5.0 + value_w;
            if rows == 0 || (x > body_x && x + w > body_end) {
                if rows > 0 {
                    y += ROW;
                }
                x = body_x;
                rows += 1;
            }
            let cw = self.line(cx, p, Font::Small, caption, dvec2(x, y + (ROW - small_h) * 0.5 + 1.0), p.ink.dim).x;
            let value_x = x + cw + 5.0;
            self.line(cx, p, Font::Body, &value, dvec2(value_x, y + (ROW - body_h) * 0.5), p.ink.text);
            if let Some(index) = *edit {
                let name = format!("{title} {caption}");
                let key = semantics::key("bench field", &name, 0);
                self.buttons.push(Button { rect: rect(value_x - 4.0, y, value_w + 8.0, ROW), choice: Choice::Field(index), key, name, face: Face::Flat });
            }
            x += w + 14.0;
        }
        if rows > 0 {
            y += ROW;
        }
        if let Some(chip) = chip.as_ref().filter(|_| !beside) {
            y += 4.0;
            self.chip(cx, p, chip, dvec2(body_x, y), None, alpha(chip.ink, 0.0));
            y += CHIP_H;
            self.icons.push((Rect { pos: dvec2(CARD_X, top + CARD_Y), size: ICON }, icon));
        } else {
            let h = (y + CARD_Y - top).max(ICON.y + 2.0 * CARD_Y);
            y = top + h - CARD_Y;
            self.icons.push((Rect { pos: dvec2(CARD_X, top + (h - ICON.y) * 0.5), size: ICON }, icon));
            if let Some(chip) = &chip {
                let chip_y = (top + (h - CHIP_H) * 0.5).max(top + 4.0 + ROW + 2.0).min(top + h - 4.0 - CHIP_H);
                self.chip(cx, p, chip, dvec2(self.width - CARD_X - chip_w, chip_y), None, alpha(chip.ink, 0.0));
            }
        }
        let bottom = y + CARD_Y;
        self.tiles[card].rect = Rect { pos: dvec2(0.0, top), size: dvec2(self.width, bottom - top) };
        self.height = bottom + CARD_GAP;
    }

    /// The board: its name and what it is, then every connection to the bench as a chip.
    fn board(&mut self, cx: &mut Cx, p: &Paint, title: &str, subtitle: &str, chips: &[Chip]) {
        let top = self.height;
        let card = self.tiles.len();
        let green = p.ink.ok;
        self.tile(Rect::default(), tint(p.ink.ground, green, 0.16), tint(p.ink.ground, green, 0.5), p.radius_lg);
        let inner = self.width - 24.0;
        let mut y = top + 12.0;
        y += self.label(cx, p, Font::Big, title, dvec2(12.0, y), inner, true, p.ink.text);
        if !subtitle.is_empty() {
            y += self.label(cx, p, Font::Body, subtitle, dvec2(12.0, y + 1.0), inner, true, p.ink.dim) + 1.0;
        }
        y += 6.0;
        for chip in chips {
            y += 6.0;
            self.chip(cx, p, chip, dvec2(12.0, y), Some(inner), [0.0, 0.0, 0.0, 0.25]);
            y += CHIP_H;
        }
        let bottom = y + 12.0;
        self.tiles[card].rect = Rect { pos: dvec2(0.0, top), size: dvec2(self.width, bottom - top) };
        self.height = bottom + CARD_GAP;
    }

    fn button(&mut self, cx: &mut Cx, p: &Paint, text: &str, ink: Rgba, face: Face, (choice, key, name): (Choice, u64, String), pos: DVec2, width: f64) {
        let h = p.height(cx, Font::Body);
        self.label(cx, p, Font::Body, text, pos + dvec2(4.0, (ROW - h) * 0.5), width - 8.0, true, ink);
        self.buttons.push(Button { rect: Rect { pos, size: dvec2(width, ROW) }, choice, key, name, face });
    }

    /// What each output jack drives, a row a jack: `J3 output`, its load's field under it
    /// (`BenchSetup::jack_fields`: a preset or a typed resistance) and the chip of the jack it
    /// takes from at the right (under the field when the column is narrow). The fields' rects
    /// from the column's corner, in `jacks`' order.
    fn loads(&mut self, cx: &mut Cx, p: &Paint, jacks: &[(String, String)]) -> Vec<Rect> {
        self.shelf(cx, p, "LOADS", "what is plugged into each output", None);
        let top = self.height;
        let card = self.tiles.len();
        self.tile(Rect::default(), p.ink.card, p.ink.card, p.radius_lg);
        let body_x = CARD_X + ICON.x + CARD_X;
        let inner = self.width - body_x - CARD_X;
        let mut y = top + CARD_Y;
        if jacks.is_empty() {
            y += self.label(cx, p, Font::Body, "The board has no output jack", dvec2(body_x, y), inner, false, p.ink.dim);
        }
        let mut fields = Vec::with_capacity(jacks.len());
        for (i, (refdes, _)) in jacks.iter().enumerate() {
            if i > 0 {
                y += 6.0;
            }
            let title = format!("{refdes} output");
            let chip = Chip { text: format!("← {refdes}"), ink: p.ink.chip_in };
            let chip_w = p.width(cx, Font::Code, &chip.text) + 16.0;
            // the chip beside the field, keeping the field room for its longest preset; in a
            // narrow column beside the title; under the field when neither fits
            let beside_field = inner - chip_w - 8.0 >= JACK_FIELD_MIN;
            let beside_title = !beside_field && p.width(cx, Font::Body, &title) + 8.0 + chip_w <= inner;
            let body_h = p.height(cx, Font::Body);
            let title_h = body_h.max(if beside_title { CHIP_H } else { 0.0 });
            self.label(cx, p, Font::Body, &title, dvec2(body_x, y + (title_h - body_h) * 0.5), inner, false, p.ink.text);
            let field_y = y + title_h + 3.0;
            let field_w = if beside_field { inner - chip_w - 8.0 } else { inner };
            fields.push(Rect { pos: dvec2(body_x, field_y), size: dvec2(field_w.max(1.0), JACK_FIELD) });
            let mut h = title_h + 3.0 + JACK_FIELD;
            let chip_x = self.width - CARD_X - chip_w;
            if beside_field {
                self.chip(cx, p, &chip, dvec2(chip_x, field_y + (JACK_FIELD - CHIP_H) * 0.5), None, alpha(chip.ink, 0.0));
            } else if beside_title {
                self.chip(cx, p, &chip, dvec2(chip_x, y + (title_h - CHIP_H) * 0.5), None, alpha(chip.ink, 0.0));
            } else {
                self.chip(cx, p, &chip, dvec2(body_x, y + h + 4.0), None, alpha(chip.ink, 0.0));
                h += 4.0 + CHIP_H;
            }
            y += h;
        }
        let h = (y + CARD_Y - top).max(ICON.y + 2.0 * CARD_Y);
        self.icons.push((Rect { pos: dvec2(CARD_X, top + (h - ICON.y) * 0.5), size: ICON }, LOAD));
        self.tiles[card].rect = Rect { pos: dvec2(0.0, top), size: dvec2(self.width, h) };
        self.height = top + h + CARD_GAP;
        fields
    }

    /// Every probe as a row: its lane's hue, its name, what it reads, and Scope and remove.
    fn probes(&mut self, cx: &mut Cx, p: &Paint, out: &BenchOut, lanes: &[BenchProbe], look: &InstrumentLook) {
        self.shelf(cx, p, "PROBES", "each one is a lane in the Scope", Some(("+ Add probe", Choice::AddProbe)));
        if out.probes.is_empty() {
            self.note(cx, p, "Drag the probe onto a net, or hold P and press one");
        }
        let mut lanes = lanes.iter().enumerate();
        let (body_h, code_h) = (p.height(cx, Font::Body), p.height(cx, Font::Code));
        for (index, probe) in out.probes.iter().enumerate() {
            let top = self.height;
            let supported = probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none();
            let lane = if supported { lanes.next() } else { None };
            self.tile(rect(0.0, top, self.width, PROBE_ROW), p.ink.card, p.ink.card, p.radius_lg);
            if let Some((channel, _)) = lane {
                let hue = look.channel(channel);
                self.tile(rect(3.0, top + 5.0, 3.0, PROBE_ROW - 10.0), hue, hue, p.pill(3.0));
            }
            let identity = probe.of.to_string();
            let (label, detail) = match lane {
                Some((_, lane)) => (lane.label.clone(), format!("net {} · V", lane.net)),
                None => (probe.label.clone(), identity.clone()),
            };
            let remove_w = p.width(cx, Font::Body, "⋯") + 12.0;
            let scope_w = if supported { p.width(cx, Font::Body, "Scope") + 16.0 } else { 0.0 };
            let bx = self.width - 4.0 - remove_w - scope_w;
            let by = top + (PROBE_ROW - ROW) * 0.5;
            // the name and what it reads, cut short of the buttons
            let saved = std::mem::replace(&mut self.width, bx - 4.0);
            let name_w = self.line(cx, p, Font::Body, &label, dvec2(10.0, top + (PROBE_ROW - body_h) * 0.5), p.ink.text).x;
            self.line(cx, p, Font::Code, &detail, dvec2(10.0 + name_w + 8.0, top + (PROBE_ROW - code_h) * 0.5), p.ink.dim);
            self.width = saved;
            if supported {
                let key = semantics::key("open probe", &identity, 0);
                self.button(cx, p, "Scope", p.ink.dim, Face::Flat, (Choice::Scope(index), key, format!("Scope {label}")), dvec2(bx, by), scope_w);
            }
            let key = semantics::key("probe menu", &identity, 0);
            self.button(cx, p, "⋯", p.ink.dim, Face::Flat, (Choice::More(index), key, format!("More for {label}")), dvec2(bx + scope_w, by), remove_w);
            self.height = top + PROBE_ROW + 4.0;
        }
        self.height += CARD_GAP - 4.0;
    }

    /// Every expectation as a row: its name, what it measures and its bounds, and its verdict on
    /// the run as the judge last published it (`ExpectLine`); under the head the counted check's
    /// button and what it found, and in a row its verdict on the check's run.
    fn expects(&mut self, cx: &mut Cx, p: &Paint, shelf: &ExpectShelf) {
        let button = shelf.button.map(|b| (if b == CheckBench::Stop { "Stop check" } else { "Check bench" }, Choice::Check));
        self.shelf(cx, p, "EXPECTATIONS", "judged on the run as it plays", button);
        if !shelf.note.is_empty() {
            self.height += self.label(cx, p, Font::Body, &shelf.note, dvec2(0.0, self.height), self.width, false, p.ink.dim) + 8.0;
        }
        let lines = &shelf.lines;
        if lines.is_empty() {
            self.note(cx, p, "None yet: the bench states them with b.expect");
            return;
        }
        let (body_h, code_h) = (p.height(cx, Font::Body), p.height(cx, Font::Code));
        let tone_ink = |tone: Tone| match tone {
            Tone::Pending => p.ink.dim,
            Tone::Pass => p.ink.ok,
            Tone::Fail => p.ink.warn,
        };
        for line in lines {
            // the name and the verdict on the first line, what it judges under them, then the
            // check's verdict
            let top = self.height;
            let h = PROBE_ROW + code_h + 4.0 + if line.check.is_some() { code_h + 2.0 } else { 0.0 };
            self.tile(rect(0.0, top, self.width, h), p.ink.card, p.ink.card, p.radius_lg);
            let ink = tone_ink(line.tone);
            let chip = Chip { text: line.state.clone(), ink };
            let chip_w = (p.width(cx, Font::Code, &chip.text) + 16.0).min(self.width * 0.6);
            let chip_x = self.width - 4.0 - chip_w;
            self.chip(cx, p, &chip, dvec2(chip_x, top + (PROBE_ROW - CHIP_H) * 0.5), Some(chip_w), alpha(ink, 0.0));
            let saved = std::mem::replace(&mut self.width, chip_x - 4.0);
            self.line(cx, p, Font::Body, &line.name, dvec2(10.0, top + (PROBE_ROW - body_h) * 0.5), p.ink.text);
            self.width = saved - 6.0;
            self.line(cx, p, Font::Code, &line.what, dvec2(10.0, top + PROBE_ROW - 2.0), p.ink.dim);
            if let Some((text, tone)) = &line.check {
                self.line(cx, p, Font::Code, text, dvec2(10.0, top + PROBE_ROW + code_h), tone_ink(*tone));
            }
            self.width = saved;
            self.height = top + h + 4.0;
        }
        self.height += CARD_GAP - 4.0;
    }

    fn draw(&self, cx: &mut Cx2d, p: &mut Paint, origin: DVec2, clip: Rect, hovered: Option<u64>) {
        for tile in &self.tiles {
            let r = at(tile.rect, origin);
            if clip.intersects(r) {
                p.rounded(cx, r, tile.fill, tile.frame, tile.radius);
            }
        }
        for b in &self.buttons {
            let r = at(b.rect, origin);
            if !clip.intersects(r) {
                continue;
            }
            let face = match b.face {
                _ if hovered == Some(b.key) => Some(p.ink.hover),
                _ => None,
            };
            if let Some(face) = face {
                p.rounded(cx, r, face, face, p.radius);
            }
        }
        for &(rect, kind) in &self.icons {
            let r = at(rect, origin);
            if clip.intersects(r) {
                p.icon.color = color(p.ink.accent);
                p.icon.kind = kind;
                p.icon.draw_abs(cx, r);
            }
        }
        for label in &self.labels {
            let r = at(label.rect, origin);
            if !clip.intersects(r) || r.size.x <= 0.0 {
                continue;
            }
            let draw = &mut *p.fonts[label.font as usize];
            draw.color = color(label.ink);
            cx.push_clip_rect(r);
            draw.draw_walk_laidout(cx, Walk::abs_rect(r), &label.layout);
            cx.pop_clip_rect();
        }
    }
}

/// A source's name and its values: caption, value, unit, and the option a menu edits (None for a
/// value no option holds alone, as a load step's two currents).
type SourceField = (&'static str, f64, &'static str, Option<&'static str>);

fn source_fields(kind: &SourceKind) -> (&'static str, Vec<SourceField>) {
    match *kind {
        SourceKind::Vdc(v) => ("DC voltage", vec![("Voltage", v, "V", Some("dc"))]),
        SourceKind::Idc(i) => ("DC current", vec![("Current", i, "A", Some("dc"))]),
        SourceKind::LoadR(r) => ("Load", vec![("Resistance", r, "Ω", Some("r"))]),
        SourceKind::LoadI(i) => ("Load", vec![("Current", i, "A", Some("i"))]),
        SourceKind::Pulse { low, high, at, width, every, edge } => {
            let mut fields = vec![
                ("Low", low, "V", Some("low")),
                ("High", high, "V", Some("v")),
                ("At", at, "s", Some("at")),
                ("Width", width, "s", Some("width")),
                ("Edge", edge, "s", Some("edge")),
            ];
            if let Some(period) = every {
                fields.push(("Every", period, "s", Some("every")));
            }
            ("Voltage pulse", fields)
        }
        SourceKind::LoadStep { from, to, at, rise } => {
            ("Load step", vec![("From", from, "A", None), ("To", to, "A", None), ("At", at, "s", Some("at")), ("Rise", rise, "s", Some("rise"))])
        }
    }
}

/// The loads an output jack's field lists, in its order, with their words; a typed resistance is
/// any other.
const JACK_PRESETS: [(AudioLoad, &str, &str); 3] = [
    (AudioLoad::Open, "Open (nothing plugged in)", "Open"),
    (AudioLoad::Phones, "Headphones (32 Ω)", "Headphones"),
    (AudioLoad::Line, "Line in (10 kΩ)", "Line in"),
];

/// What a jack's field says when a typed load does not read.
const JACK_HINT: &str = "Type a resistance (600, 2.2k, 47 Ω) or open, headphones, line";

/// An output jack's load field (`mod.widgets.BenchJackLoad`), named for its jack, listing the
/// presets, its corners the page's `radius`.
fn new_jack_field(cx: &mut Cx, refdes: &str, radius: f64) -> WidgetRef {
    let label = format!("Load of {refdes}");
    let widget = cx.with_vm(|vm| {
        let value = script_eval!(vm, { mod.widgets.BenchJackLoad{a11y: {label: #(label)} draw_bg +: {border_radius: #(radius)}} });
        WidgetRef::script_from_value(vm, value)
    });
    widget.as_combo_box().set_labels(cx, JACK_PRESETS.iter().map(|preset| preset.1.to_owned()).collect());
    widget
}

/// What each output jack drives: the jacks' own loads by reference (the project's `sim`
/// statement, or the host's without a project) and the load of the rest.
#[derive(Clone, PartialEq)]
struct JackLoads {
    rest: AudioLoad,
    own: Vec<(String, AudioLoad)>,
}

impl Default for JackLoads {
    fn default() -> Self {
        Self { rest: AudioLoad::Line, own: Vec::new() }
    }
}

impl JackLoads {
    fn of(&self, refdes: &str) -> AudioLoad {
        self.own.iter().find(|(jack, _)| jack == refdes).map_or(self.rest, |&(_, load)| load)
    }
}

/// A load as its field shows it (`long`) or as a chip names it: a preset's words, a typed
/// resistance's value.
fn load_text(load: AudioLoad, long: bool) -> String {
    match JACK_PRESETS.iter().find(|preset| preset.0 == load) {
        Some(&(_, words, word)) => if long { words } else { word }.to_owned(),
        None => load.ohms().map_or_else(String::new, |r| supply_value(r, "Ω")),
    }
}

/// A load typed into a jack's field: a preset's word (`open`, `headphones`, `line`), or a
/// resistance with an optional prefix and unit (`600`, `600 Ω`, `2.2k`, `2k2`, `47R`, `1 Mohm`);
/// None when it does not read as one, or is not a positive, finite resistance.
fn parse_load(text: &str) -> Option<AudioLoad> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let lower = t.to_lowercase();
    for (load, words) in [(AudioLoad::Open, &["open"][..]), (AudioLoad::Phones, &["head", "phone"]), (AudioLoad::Line, &["line"])] {
        if words.iter().any(|w| lower.starts_with(w)) {
            return Some(load);
        }
    }
    let t = ["Ω", "ohms", "ohm", "Ohms", "Ohm"].iter().find_map(|unit| t.strip_suffix(unit)).unwrap_or(&t);
    // a prefix at the end (2.2k) or in place of the decimal point (2k2, 4R7)
    let scale = |c: char| match c {
        'm' => Some(1e-3),
        'R' | 'r' => Some(1.0),
        'k' | 'K' => Some(1e3),
        'M' => Some(1e6),
        _ => None,
    };
    let r = match t.char_indices().find(|&(_, c)| scale(c).is_some()) {
        Some((i, c)) => {
            let (whole, rest) = (&t[..i], &t[i + c.len_utf8()..]);
            let n: f64 = if rest.is_empty() { whole.parse().ok()? } else { format!("{whole}.{rest}").parse().ok()? };
            n * scale(c)?
        }
        None => t.parse().ok()?,
    };
    (r.is_finite() && r > 0.0).then_some(AudioLoad::Ohms(r))
}

fn supply_value(value: f64, unit: &str) -> String {
    let text = eng(value, unit, 3);
    if let Some((n, units)) = text.split_once(' ') {
        if n.contains('.') && !n.contains('e') {
            return format!("{} {units}", n.trim_end_matches('0').trim_end_matches('.'));
        }
    }
    text
}

/// `n thing` or `n things`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The page laid out at one width: the intro, the grid's three items (power and sources, the
/// board, loads and probes) and the front panel's head. The grid shows as many columns as fit at
/// [`COLUMN_MIN`], up to three, in rows as tall as their tallest item.
struct SetupPage {
    width: f64,
    dark: bool,
    columns: usize,
    intro: Column,
    left: Column,
    board: Column,
    right: Column,
    front: Column,
    /// The page's editable values, by the index their buttons carry.
    fields: Vec<Field>,
    /// The supplies and sources on the bench, by the index their card menus carry.
    stims: Vec<Stim>,
    /// The board's output jacks: each one's reference, its load and its field's rect from the
    /// right column's corner (the loads card's rows).
    jacks: Vec<(String, AudioLoad, Rect)>,
}

impl SetupPage {
    fn new(cx: &mut Cx, p: &Paint, width: f64, dark: bool, bench: Option<(&BenchOut, Option<&Circuit>)>, (controls, loads, project): (Option<(usize, usize)>, &JackLoads, &str), expects: &ExpectShelf) -> Self {
        let inner = (width - 2.0 * PAD).max(1.0);
        let columns = (((inner + GAP) / (COLUMN_MIN + GAP)).floor() as usize).clamp(1, 3);
        let column = ((inner - (columns - 1) as f64 * GAP) / columns as f64).max(1.0);
        let mut page = Self {
            width,
            dark,
            columns,
            intro: Column::new(inner),
            left: Column::new(column),
            board: Column::new(column),
            right: Column::new(column),
            front: Column::new(inner),
            fields: Vec::new(),
            stims: Vec::new(),
            jacks: Vec::new(),
        };
        let Some((out, circuit)) = bench else {
            page.intro.note_past(cx, p, "No bench yet: the board has no bench file, or it did not build.", NOTCH_INDENT);
            return page;
        };
        if let UnitState::Failed(diags) = &out.state {
            let reason = diags.first().map_or("The bench did not build.", |diag| diag.message.as_str());
            page.intro.note_past(cx, p, &format!("{}: {reason}", out.name), NOTCH_INDENT);
            return page;
        }
        page.intro.note_past(cx, p, INTRO, NOTCH_INDENT);
        let net = |r: NetRef| {
            circuit
                .filter(|c| c.unit == r.board)
                .and_then(|c| c.nets.get(r.net.ix()).filter(|n| n.id == r.net).or_else(|| c.nets.iter().find(|n| n.id == r.net)))
        };
        let net_name = |r: NetRef| net(r).map_or("Unknown net", |n| n.name.as_str());
        // a net with a connector on it is named by the connector too: `J1 VBUS`
        let jack = |r: NetRef| {
            let name = net_name(r);
            let connector = circuit.filter(|c| c.unit == r.board).and_then(|c| {
                c.comps.iter().find(|comp| comp.part.kind == PartKind::Connector && comp.conns.iter().any(|&(_, n)| n == r.net))
            });
            match connector {
                Some(comp) => format!("{} {name}", comp.refdes),
                None => name.to_owned(),
            }
        };
        let mut chips = Vec::new();
        page.left.shelf(cx, p, "POWER", "", Some(("+ Add", Choice::AddSupply)));
        for supply in &out.spec.supplies {
            let to = jack(supply.net);
            let title = format!("{} supply", net_name(supply.net));
            let stim = Stim::Supply { net: net_name(supply.net).to_owned() };
            let mut edit = |key, value, unit, what: &str| {
                page.fields.push(Field { stim: stim.clone(), key, value, unit, what: format!("{title} {what}") });
                Some(page.fields.len() - 1)
            };
            let mut fields = vec![("Voltage", format!("{:.2} V", supply.v), edit("v", Some(supply.v), "V", "voltage"))];
            let limit = supply.limit.map_or_else(|| "none".to_owned(), |limit| supply_value(limit, "A"));
            fields.push(("Limit", limit, edit("limit", supply.limit, "A", "limit")));
            if let Some(ramp) = supply.ramp {
                fields.push(("Ramp", supply_value(ramp, "s"), edit("ramp", Some(ramp), "s", "ramp")));
            }
            page.stims.push(stim.clone());
            let more = Some(page.stims.len() - 1);
            page.left.device(cx, p, SUPPLY, &title, &fields, Some(Chip { text: format!("→ {to}"), ink: p.ink.chip_in }), more);
            chips.push(Chip { text: format!("{to} ← {title}"), ink: p.ink.chip_in });
        }
        if out.spec.supplies.is_empty() {
            page.left.note(cx, p, "No supply on the bench");
        }
        page.left.shelf(cx, p, "SOURCES", "", Some(("+ Add", Choice::AddSource)));
        for source in &out.spec.sources {
            let a = jack(source.a);
            let (title, values) = source_fields(&source.kind);
            let stim = Stim::Source { a: net_name(source.a).to_owned(), b: net_name(source.b).to_owned() };
            let fields: Vec<_> = values
                .into_iter()
                .map(|(caption, value, unit, key)| {
                    let edit = key.map(|key| {
                        let what = format!("{title} {} {}", net_name(source.a), caption.to_lowercase());
                        page.fields.push(Field { stim: stim.clone(), key, value: Some(value), unit, what });
                        page.fields.len() - 1
                    });
                    (caption, eng(value, unit, 3), edit)
                })
                .collect();
            // the return is named only when it is not ground
            let to = match net(source.b).filter(|n| n.kind != NetKind::Ground) {
                Some(b) => format!("→ {a} – {}", b.name),
                None => format!("→ {a}"),
            };
            page.stims.push(stim.clone());
            let more = Some(page.stims.len() - 1);
            page.left.device(cx, p, SOURCE, title, &fields, Some(Chip { text: to, ink: p.ink.chip_in }), more);
            chips.push(Chip { text: format!("{a} ← {title}"), ink: p.ink.chip_in });
        }
        if out.spec.sources.is_empty() && out.spec.stubs.is_empty() {
            page.left.note(cx, p, "No source drives the board");
        }
        let (lanes, _) = BenchProbe::from_bench(&out.probes);
        // the board's output jacks, each with its load: one chip when they drive the same
        let jacks: Vec<(String, String)> = circuit.map(|c| output_jacks(c).into_iter().map(|jack| {
            let name = c.comps.get(jack.comp.ix()).map_or_else(String::new, |comp| comp.path.clone());
            (jack.refdes, name)
        }).collect()).unwrap_or_default();
        let jack_loads: Vec<AudioLoad> = jacks.iter().map(|(refdes, _)| loads.of(refdes)).collect();
        match jack_loads.split_first() {
            Some((&first, rest)) if rest.iter().all(|&l| l == first) => {
                let to = if rest.is_empty() { jacks[0].0.clone() } else { "Output jacks".to_owned() };
                chips.push(Chip { text: format!("{to} → {}", load_text(first, false)), ink: p.ink.chip_out });
            }
            Some(_) => {
                for ((refdes, _), &load) in jacks.iter().zip(&jack_loads) {
                    chips.push(Chip { text: format!("{refdes} → {}", load_text(load, false)), ink: p.ink.chip_out });
                }
            }
            None => {}
        }
        chips.push(Chip { text: format!("{} → Scope", count(out.probes.len(), "probe", "probes")), ink: p.ink.chip_probes });
        if let Some((keys, knobs)) = controls.filter(|&(keys, knobs)| keys + knobs > 0) {
            let parts: Vec<String> = [(keys, "key", "keys"), (knobs, "knob", "knobs")].into_iter().filter(|c| c.0 > 0).map(|(n, one, many)| count(n, one, many)).collect();
            chips.push(Chip { text: format!("{} ← Front panel", parts.join(" · ")), ink: p.ink.chip_in });
        }
        let title = circuit.map_or(out.name.as_str(), |c| c.name.as_str());
        // what is open, then the board's size
        let mut subtitle: Vec<String> = Vec::new();
        if !project.is_empty() {
            subtitle.push(project.to_owned());
        }
        if let Some(c) = circuit {
            subtitle.extend([count(c.comps.len(), "part", "parts"), count(c.nets.len(), "net", "nets")]);
        }
        let subtitle = subtitle.join(" · ");
        page.board.shelf(cx, p, "THE BOARD", "", None);
        page.board.board(cx, p, title, &subtitle, &chips);
        let fields = page.right.loads(cx, p, &jacks);
        page.jacks = jacks.into_iter().zip(jack_loads).zip(fields).map(|(((refdes, _), load), rect)| (refdes, load, rect)).collect();
        page.right.probes(cx, p, out, &lanes, InstrumentLook::for_theme(dark));
        page.right.expects(cx, p, expects);
        page.front.shelf(cx, p, "FRONT PANEL", "the board's keys and knobs, generated from its parts; also docked under the other tabs", None);
        page
    }

    /// Where grid item `i` (0 power and sources, 1 the board, 2 loads and probes) sits: its column's
    /// left edge from the page's origin.
    fn item_x(&self, i: usize) -> f64 {
        (i % self.columns) as f64 * (self.left.width + GAP)
    }
}

/// The bench setup page in the Electronics workspace.
#[derive(Script, WidgetRegister, WidgetRef, WidgetSet)]
pub struct BenchSetup {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_box: DrawSetupBox,
    #[live]
    draw_icon: DrawBenchIcon,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    #[live]
    draw_code: DrawText,
    #[live]
    draw_big: DrawText,
    #[live]
    color_ground: Vec4f,
    #[live]
    color_card: Vec4f,
    #[live]
    color_hover: Vec4f,
    #[live]
    color_text: Vec4f,
    #[live]
    color_dim: Vec4f,
    #[live]
    color_muted: Vec4f,
    #[live]
    color_accent: Vec4f,
    #[live]
    color_in: Vec4f,
    #[live]
    color_out: Vec4f,
    #[live]
    color_probes: Vec4f,
    #[live]
    radius: f32,
    #[live]
    radius_lg: f32,
    #[live]
    color_popover: Vec4f,
    #[live]
    color_popover_border: Vec4f,
    #[live]
    color_menu_hover: Vec4f,
    #[live]
    generator: ScriptObjectRef,
    #[live]
    controls_panel: WidgetRef,
    #[rust]
    followed: Option<Arc<Vec<BenchView>>>,
    #[rust]
    row: Option<usize>,
    #[rust]
    drives: Vec<(String, WidgetRef)>,
    #[rust]
    page: Option<SetupPage>,
    /// What each output jack drives, as the page was laid out with.
    #[rust]
    loads: JackLoads,
    /// The output jacks' load fields, in the page's jack order: (reference, field, the load it
    /// was last set to show).
    #[rust]
    jack_fields: Vec<(String, WidgetRef, Option<AudioLoad>)>,
    #[rust]
    area: Area,
    /// Where the grid's three columns drew, from the area's corner: their buttons' origins.
    #[rust]
    offsets: [DVec2; 3],
    /// The menu a value, a probe's ⋯ or an add button opened.
    #[rust]
    menu: Option<Menu>,
    #[rust]
    scroll_y: f64,
    #[rust]
    scroll_max: f64,
    #[rust]
    pressed: Option<u64>,
    /// The button under the pointer: its face shows.
    #[rust]
    hovered: Option<u64>,
    /// The front panel's keys and knobs the page was laid out with.
    #[rust]
    controls: Option<(usize, usize)>,
    /// The name of what is open (`ElecLive::title`), under the board's name.
    #[rust]
    project: String,
    /// The expectations' shelf as last laid out, and the judge's publication and the check
    /// (`ElecLive::check`) it shows.
    #[rust]
    expects: ExpectShelf,
    #[rust]
    judged_seen: u64,
    #[rust]
    check_seen: Option<Arc<BenchCheck>>,
    /// The time the running check had reached when the shelf was laid out.
    #[rust]
    reached_seen: f64,
}

impl ScriptHook for BenchSetup {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.page = None;
            // made again from the reloaded template with the page
            self.jack_fields.clear();
            for (_, widget) in &mut self.drives {
                widget.script_apply(vm, apply, scope, self.generator.as_object().into());
            }
        }
    }
}

impl BenchSetup {
    fn bench(&self) -> Option<&BenchOut> {
        let out = &self.followed.as_ref()?.get(self.row?)?.out;
        (!matches!(out.state, UnitState::Failed(_))).then_some(out)
    }

    fn follow(&mut self, cx: &mut Cx) {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        // what the jacks drive: the project's picks, else the host's (a line input by default);
        // O(jacks) a signal, a copy only when they change
        let (rest, own) = match live.snap.docs.as_ref() {
            Some(docs) => (docs.sim.audio_load, &docs.sim.jack_loads),
            None => (live.sim.status.jack_load, &*live.sim.jack_loads),
        };
        let rest = rest.unwrap_or(AudioLoad::Line);
        let loads = (rest != self.loads.rest || *own != self.loads.own).then(|| JackLoads { rest, own: own.clone() });
        let row = crate::snapshot::bench_index(&live.snap.benches, live.chosen_bench.as_deref());
        let snap = (self.row != row || self.followed.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &live.snap.benches))).then(|| live.snap.clone());
        // the expectations' verdicts as the run publishes them, and the counted check's as the
        // app puts it up: O(expectations) per publication, a new layout only when a row's words
        // change
        let check = live.check.clone();
        let check_new = match (&check, &self.check_seen) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (a, b) => a.is_some() != b.is_some(),
        };
        // a running check's progress: one load a signal, the note laid out again as it moves
        let reached = check.as_ref().filter(|c| c.report.is_none()).map(|c| c.reached.get());
        let progressed = reached.is_some_and(|t| t != self.reached_seen);
        let judged = live.sim.judged.lock().ok().filter(|j| j.version != self.judged_seen || snap.is_some() || check_new || progressed).map(|j| j.clone());
        let lines = judged.map(|judged| {
            let benches = snap.as_ref().map_or_else(|| self.followed.clone(), |s| Some(s.benches.clone()));
            let out = row.and_then(|i| benches.as_ref()?.get(i)).map(|v| &v.out).filter(|out| !matches!(out.state, UnitState::Failed(_)));
            (judged.version, out.map_or_else(ExpectShelf::default, |out| expect_shelf(out, out.circuit(live.snap.live()), &judged, check.as_deref())))
        });
        if lines.is_some() {
            self.check_seen = check;
            self.reached_seen = reached.unwrap_or(0.0);
        }
        if self.project != live.title {
            self.project.clone_from(&live.title);
            self.page = None;
            self.area.redraw(cx);
        }
        if let Some(loads) = loads {
            self.loads = loads;
            self.page = None;
            self.area.redraw(cx);
        }
        if let Some((version, lines)) = lines {
            self.judged_seen = version;
            if lines != self.expects {
                self.expects = lines;
                self.page = None;
                self.area.redraw(cx);
            }
        }
        let Some(snap) = snap else { return };
        let benches = snap.benches.clone();
        let view = row.map(|i| &benches[i]).filter(|v| !matches!(v.out.state, UnitState::Failed(_)));
        let same = self.bench().map(|b| &b.name) == view.map(|v| &v.out.name);
        let mut old: HashMap<_, _> = self.drives.drain(..).collect();
        if !same {
            self.scroll_y = 0.0;
        }
        if let Some(view) = view {
            let circuit = view.out.circuit(snap.live());
            let window = ScopeSettings { time_per_div: bench_time_per_div(view.out.stop), ..Default::default() }.window();
            let mut seen = HashSet::with_capacity(view.out.spec.stubs.len());
            for stub in &view.out.spec.stubs {
                if !seen.insert(stub.path.as_str()) {
                    continue;
                }
                let reusable = if same { old.remove(&stub.path) } else { None };
                let widget = reusable.unwrap_or_else(|| new_card(cx, &self.generator));
                let title = circuit.and_then(|c| c.comp(&stub.path)).map_or(&stub.path, |c| &c.refdes).clone();
                if let Some(mut card) = widget.borrow_mut::<DriveLane>() {
                    if card.set_source(cx, view, stub, title) {
                        cx.widget_action(self.uid, ViewAction::CancelPreview);
                    }
                    card.set_window(cx, window);
                }
                self.drives.push((stub.path.clone(), widget));
            }
        }
        for widget in old.into_values() {
            if let Some(mut card) = widget.borrow_mut::<DriveLane>() {
                if card.cancel_drag(cx) {
                    cx.widget_action(self.uid, ViewAction::CancelPreview);
                }
            }
        }
        self.followed = Some(benches);
        self.row = row;
        self.page = None;
        self.pressed = None;
        self.hovered = None;
        self.menu = None;
        cx.widget_tree_mark_dirty(self.uid);
        self.area.redraw(cx);
    }

    /// Every button of the grid with its column's origin (from the area's corner).
    fn buttons(&self) -> impl Iterator<Item = (&Button, DVec2)> {
        self.page.iter().flat_map(move |page| {
            [(&page.left, self.offsets[0]), (&page.board, self.offsets[1]), (&page.right, self.offsets[2])]
                .into_iter()
                .flat_map(|(column, origin)| column.buttons.iter().map(move |b| (b, origin)))
        })
    }

    fn button_at(&self, cx: &Cx, abs: DVec2) -> Option<(&Button, DVec2)> {
        let pos = abs - self.area.rect(cx).pos;
        self.buttons().find(|(b, origin)| at(b.rect, *origin).contains(pos))
    }

    /// A button pressed: a choice made at once, or its menu opened under `anchor` (the button's
    /// rect from the area's corner).
    fn press(&mut self, cx: &mut Cx, choice: Choice, anchor: Rect) {
        let Some(out) = self.bench() else { return };
        let items = match choice {
            Choice::Scope(i) => {
                cx.action(ShowInScope(out.probes[i].of.target.clone()));
                return;
            }
            Choice::Check => {
                if let Some(press) = self.expects.button {
                    cx.action(press);
                }
                return;
            }
            Choice::More(i) => vec![MenuItem { text: "Remove probe".into(), pick: Pick::Remove(i), current: false }],
            Choice::Field(i) => {
                let Some(field) = self.page.as_ref().and_then(|page| page.fields.get(i)) else { return };
                ladder(field)
                    .into_iter()
                    .map(|v| MenuItem {
                        text: supply_value(v, field.unit),
                        pick: Pick::Value(i, v),
                        current: field.value.is_some_and(|c| same_value(c, v)),
                    })
                    .collect()
            }
            Choice::AddProbe => {
                let probed: HashSet<&str> = out.probes.iter().filter(|p| p.of.quantity == ProbeQuantity::V && p.of.reference.is_none()).map(|p| p.of.target.as_str()).collect();
                self.net_items(cx, |net| !probed.contains(net), |net| Pick::AddProbe(net.to_owned()))
            }
            Choice::AddSupply => {
                let supplied: HashSet<String> = self.page.iter().flat_map(|page| &page.stims).filter_map(|stim| match stim {
                    Stim::Supply { net } => Some(net.clone()),
                    Stim::Source { .. } => None,
                }).collect();
                self.net_items(cx, |net| !supplied.contains(net), |net| Pick::AddSupply(net.to_owned()))
            }
            Choice::AddSource => SOURCE_KINDS.iter().map(|&(text, kind, _)| MenuItem { text: text.into(), pick: Pick::SourceKind(kind), current: false }).collect(),
            Choice::CardMore(i) => {
                let text = match self.page.as_ref().and_then(|page| page.stims.get(i)) {
                    Some(Stim::Supply { .. }) => "Remove supply",
                    Some(Stim::Source { .. }) => "Remove source",
                    None => return,
                };
                vec![MenuItem { text: text.into(), pick: Pick::RemoveStim(i), current: false }]
            }
        };
        if items.is_empty() {
            return;
        }
        self.menu = Some(Menu { anchor, items, scroll: 0.0, hovered: None, pressed: None, rect: Rect::default() });
        self.hovered = None;
        self.area.redraw(cx);
    }

    /// The run board's nets other than ground that `keep` keeps, by name, as menu rows.
    fn net_items(&self, cx: &Cx, keep: impl Fn(&str) -> bool, pick: impl Fn(&str) -> Pick) -> Vec<MenuItem> {
        let (Some(out), Some(live)) = (self.bench(), cx.get_global_ref::<ElecLive>()) else { return Vec::new() };
        let snap = live.snap.clone();
        let Some(circuit) = out.circuit(snap.live()) else { return Vec::new() };
        let mut nets: Vec<&str> = circuit.nets.iter().filter(|n| n.kind != NetKind::Ground).map(|n| n.name.as_str()).filter(|n| keep(n)).collect();
        nets.sort_unstable();
        nets.dedup();
        nets.into_iter().map(|net| MenuItem { text: net.to_owned(), pick: pick(net), current: false }).collect()
    }

    /// The run board's ground net's name: a source's return.
    fn ground(&self, cx: &Cx) -> Option<String> {
        let out = self.bench()?;
        let live = cx.get_global_ref::<ElecLive>()?;
        let snap = live.snap.clone();
        let circuit = out.circuit(snap.live())?;
        circuit.nets.iter().find(|n| n.kind == NetKind::Ground).map(|n| n.name.clone())
    }

    /// A menu row picked: the menu closes and its edit goes out.
    fn pick(&mut self, cx: &mut Cx, pick: Pick) {
        let anchor = self.menu.take().map(|menu| menu.anchor);
        self.area.redraw(cx);
        // a kind of source picked: the nets it can drive, in a menu at the same place
        if let Pick::SourceKind(kind) = pick {
            let items = self.net_items(cx, |_| true, |net| Pick::AddSource(kind, net.to_owned()));
            if let (Some(anchor), false) = (anchor, items.is_empty()) {
                self.menu = Some(Menu { anchor, items, scroll: 0.0, hovered: None, pressed: None, rect: Rect::default() });
            }
            return;
        }
        let ground = self.ground(cx);
        let Some(out) = self.bench() else { return };
        let action = match pick {
            Pick::Value(i, value) => {
                let Some(field) = self.page.as_ref().and_then(|page| page.fields.get(i)) else { return };
                if field.value.is_some_and(|c| same_value(c, value)) {
                    return;
                }
                let key = field.key.to_owned();
                let op = match &field.stim {
                    Stim::Supply { net } => BenchOp::SetSupply { net: net.clone(), key, value },
                    Stim::Source { a, b } => BenchOp::SetSource { a: a.clone(), b: b.clone(), key, value },
                };
                ViewAction::Ops {
                    place: vec![],
                    label: format!("{} {}", field.what, supply_value(value, field.unit)),
                    ops: vec![DesignOp::Bench { bench: out.name.clone(), op }],
                }
            }
            Pick::Remove(i) => {
                let Some(probe) = out.probes.get(i) else { return };
                if probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none() {
                    remove_probe(cx, self.uid, Some((&out.name, &out.probes)), probe.of.target.clone());
                    return;
                }
                ViewAction::Ops {
                    place: vec![],
                    label: format!("Remove probe {}", probe.label),
                    ops: vec![DesignOp::Bench { bench: out.name.clone(), op: BenchOp::RemoveProbe { of: probe.of.clone() } }],
                }
            }
            Pick::AddProbe(net) => {
                let Some(action) = click_probe_op(&out.name, &out.probes, &net) else { return };
                action
            }
            Pick::AddSupply(net) => ViewAction::Ops {
                place: vec![],
                label: format!("Add a supply on {net}"),
                ops: vec![DesignOp::Bench { bench: out.name.clone(), op: BenchOp::AddSupply { net, v: SUPPLY_V } }],
            },
            Pick::AddSource(kind, a) => {
                let Some(b) = ground else { return };
                let Some(&(name, _, opts)) = SOURCE_KINDS.iter().find(|(_, k, _)| *k == kind) else { return };
                let opts = opts.iter().map(|&(key, value)| (key.to_owned(), value)).collect();
                ViewAction::Ops {
                    place: vec![],
                    label: format!("Add a {} on {a}", name.to_lowercase()),
                    ops: vec![DesignOp::Bench { bench: out.name.clone(), op: BenchOp::AddSource { kind: kind.to_owned(), a, b, opts } }],
                }
            }
            Pick::RemoveStim(i) => {
                let Some(stim) = self.page.as_ref().and_then(|page| page.stims.get(i)) else { return };
                let (op, label) = match stim {
                    Stim::Supply { net } => (BenchOp::RemoveSupply { net: net.clone() }, format!("Remove the {net} supply")),
                    Stim::Source { a, b } => (BenchOp::RemoveSource { a: a.clone(), b: b.clone() }, format!("Remove the source on {a}")),
                };
                ViewAction::Ops { place: vec![], label, ops: vec![DesignOp::Bench { bench: out.name.clone(), op }] }
            }
            Pick::SourceKind(_) => return,
        };
        cx.widget_action(self.uid, action);
    }

    /// Events while a menu is open: its rows take the pointer; a press anywhere else closes it.
    fn handle_menu(&mut self, cx: &mut Cx, event: &Event) {
        let corner = self.area.rect(cx).pos;
        if let Event::MouseDown(e) = event {
            let inside = self.menu.as_ref().is_some_and(|menu| menu.rect.contains(e.abs - corner));
            if !inside {
                self.menu = None;
                self.area.redraw(cx);
                return;
            }
        }
        let Some(menu) = self.menu.as_mut() else { return };
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let over = menu.row_at(e.abs - corner);
                cx.set_cursor(if over.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                if over != menu.hovered {
                    menu.hovered = over;
                    self.area.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if menu.hovered.take().is_some() {
                    self.area.redraw(cx);
                }
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                menu.pressed = menu.row_at(e.abs - corner);
            }
            Hit::FingerUp(e) => {
                let pressed = menu.pressed.take();
                if let Some(row) = menu.row_at(e.abs - corner).filter(|&row| Some(row) == pressed) {
                    let pick = menu.items[row].pick.clone();
                    self.pick(cx, pick);
                }
            }
            _ => {}
        }
    }

    /// The open menu drawn over the page, under its button (above it when there is more room
    /// there), held inside the area; only its visible rows are laid out.
    fn draw_menu(&mut self, cx: &mut Cx2d, view: Rect, ink: &Inks) {
        let Some(menu) = self.menu.as_mut() else { return };
        // a draw call of their own, so the menu covers what the page drew
        self.draw_box.new_draw_call(cx);
        self.draw_text.new_draw_call(cx);
        let widest = menu.items.iter().map(|item| {
            let layout = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &item.text);
            f64::from(layout.size_in_lpxs.width * self.draw_text.font_scale)
        }).fold(0.0f64, f64::max);
        let width = (widest + 32.0).max(menu.anchor.size.x).min(view.size.x - 8.0).max(1.0);
        let full = menu.height();
        let below = view.size.y - (menu.anchor.pos.y + menu.anchor.size.y + 2.0) - 4.0;
        let above = menu.anchor.pos.y - 2.0 - 4.0;
        let down = full <= below || below >= above;
        let height = full.min(if down { below } else { above }).max(MENU_ROW + 2.0 * MENU_PAD);
        let x = menu.anchor.pos.x.min(view.size.x - width - 4.0).max(4.0);
        let y = if down { menu.anchor.pos.y + menu.anchor.size.y + 2.0 } else { menu.anchor.pos.y - 2.0 - height };
        menu.rect = rect(x, y, width, height);
        menu.scroll = menu.scroll.clamp(0.0, (full - height).max(0.0));
        let r = at(menu.rect, view.pos);
        self.draw_box.color = color(ink.popover);
        self.draw_box.frame = color(ink.popover_border);
        self.draw_box.radius = self.radius_lg;
        self.draw_box.draw_abs(cx, r);
        cx.push_clip_rect(r);
        let first = (menu.scroll / MENU_ROW) as usize;
        let rows = (height / MENU_ROW).ceil() as usize + 1;
        let text_h = {
            let layout = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), "Ag");
            f64::from(layout.size_in_lpxs.height * self.draw_text.font_scale)
        };
        for (i, item) in menu.items.iter().enumerate().skip(first).take(rows) {
            let row = rect(r.pos.x + 4.0, r.pos.y + MENU_PAD + i as f64 * MENU_ROW - menu.scroll, r.size.x - 8.0, MENU_ROW);
            if menu.hovered == Some(i) {
                self.draw_box.color = color(ink.menu_hover);
                self.draw_box.frame = color(ink.menu_hover);
                self.draw_box.radius = self.radius;
                self.draw_box.draw_abs(cx, row);
            }
            let layout = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &item.text);
            self.draw_text.color = color(if item.current { ink.accent } else { ink.text });
            let text = rect(row.pos.x + 10.0, row.pos.y + (MENU_ROW - text_h) * 0.5, row.size.x - 14.0, text_h);
            self.draw_text.draw_walk_laidout(cx, Walk::abs_rect(text), &layout);
        }
        cx.pop_clip_rect();
    }

    /// A jack's load picked from its field's presets, or typed into it: saved with the project as
    /// its `sim` statement's `jack_loads` (one undoable edit), or sent to the host without a
    /// project; the page follows it. A typed entry that does not read says why and changes nothing.
    fn jack_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for (refdes, widget, _) in &self.jack_fields {
            let field = widget.as_combo_box();
            let load = match (field.selected(actions), field.typed(actions)) {
                (Some(i), _) => JACK_PRESETS.get(i).map(|preset| preset.0),
                (None, Some(text)) => match parse_load(&text) {
                    Some(load) => Some(load),
                    None => {
                        field.set_error(cx, Some(JACK_HINT));
                        None
                    }
                },
                (None, None) => None,
            };
            let Some(load) = load else { continue };
            field.set_text(cx, &load_text(load, true));
            let saved = write_run_settings(cx, |s| match s.jack_loads.binary_search_by(|(jack, _)| jack.as_str().cmp(refdes)) {
                Ok(i) => s.jack_loads[i].1 = load,
                Err(i) => s.jack_loads.insert(i, (refdes.clone(), load)),
            });
            if !saved {
                cx.action(SimAction::Cmd(SimCmd::SetJackLoadOf(refdes.clone(), Some(load))));
            }
        }
    }

    fn drive_actions(&self, cx: &mut Cx, actions: &Actions) {
        let Some(out) = self.bench() else { return };
        for (stub, widget) in &self.drives {
            for action in actions.filter_widget_actions_cast::<GeneratorAction>(widget.widget_uid()) {
                let action = match action {
                    GeneratorAction::SetDrive { pin, drive, field, preview } => {
                        let label = format!("{stub} {pin} {field}");
                        let ops = vec![DesignOp::Bench {
                            bench: out.name.clone(),
                            op: BenchOp::SetDrive { stub: stub.clone(), pin, drive },
                        }];
                        if preview {
                            ViewAction::Preview { notes: vec![], tag: next_tag(), place: vec![], ops, label }
                        } else {
                            ViewAction::Ops { place: vec![], ops, label }
                        }
                    }
                    GeneratorAction::CommitPreview => ViewAction::CommitPreview,
                    GeneratorAction::None => continue,
                };
                cx.widget_action(self.uid, action);
            }
        }
    }
}

impl WidgetNode for BenchSetup {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }

    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        if self.bench().is_some() {
            for (stub, widget) in &self.drives {
                visit(LiveId::from_str("generator:").bytes_append(stub.as_bytes()), widget.clone());
            }
            visit(id!(controls), self.controls_panel.clone());
            for (refdes, widget, _) in &self.jack_fields {
                visit(LiveId::from_str("jack load:").bytes_append(refdes.as_bytes()), widget.clone());
            }
        }
    }

    fn find_widgets_from_point(&self, cx: &Cx, point: DVec2, found: &mut dyn FnMut(&WidgetRef)) {
        if !self.area.clipped_rect(cx).contains(point) || self.bench().is_none() {
            return;
        }
        for (_, widget) in &self.drives {
            widget.find_widgets_from_point(cx, point, found);
        }
        for (_, widget, _) in &self.jack_fields {
            widget.find_widgets_from_point(cx, point, found);
        }
        self.controls_panel.find_widgets_from_point(cx, point, found);
    }
}

impl Widget for BenchSetup {
    fn clips_content(&self) -> bool {
        true
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.follow(cx);
        let has_bench = self.bench().is_some();
        cx.begin_turtle(walk, Layout::flow_overlay());
        let view = cx.turtle().rect();
        cx.push_clip_rect(view);
        let dark = base_theme(cx) != BaseTheme::Light;
        let look = InstrumentLook::for_theme(dark);
        let controls = self.controls_panel.borrow_mut::<ControlsPanel>().and_then(|mut panel| panel.counts(cx));
        let ink = Inks {
            ground: rgba(self.color_ground),
            card: rgba(self.color_card),
            hover: rgba(self.color_hover),
            text: rgba(self.color_text),
            dim: rgba(self.color_dim),
            muted: rgba(self.color_muted),
            accent: rgba(self.color_accent),
            chip_in: rgba(self.color_in),
            chip_out: rgba(self.color_out),
            chip_probes: rgba(self.color_probes),
            ok: look.canvas.ok,
            warn: look.canvas.warning,
            popover: rgba(self.color_popover),
            popover_border: rgba(self.color_popover_border),
            menu_hover: rgba(self.color_menu_hover),
        };
        let mut paint = Paint {
            ink,
            radius: self.radius,
            radius_lg: self.radius_lg,
            panel: &mut self.draw_box,
            icon: &mut self.draw_icon,
            fonts: [&mut self.draw_text, &mut self.draw_small, &mut self.draw_code, &mut self.draw_big],
        };
        let width = view.size.x;
        if self.controls != controls || self.page.as_ref().is_none_or(|page| page.width != width || page.dark != dark) {
            self.controls = controls;
            let snap = cx.get_global_ref::<ElecLive>().map(|live| live.snap.clone());
            let out = self.row.and_then(|i| self.followed.as_ref()?.get(i)).map(|v| &v.out);
            let bench = out.map(|out| (out, snap.as_ref().and_then(|s| out.circuit(s.live()))));
            let page = SetupPage::new(cx, &paint, width, dark, bench, (controls, &self.loads, &self.project), &self.expects);
            // a field a jack, kept by its reference across layouts, showing the jack's load
            let mut old = std::mem::take(&mut self.jack_fields);
            for (refdes, load, _) in &page.jacks {
                let (widget, shown) = match old.iter().position(|f| f.0 == *refdes) {
                    Some(i) => {
                        let (_, widget, shown) = old.swap_remove(i);
                        (widget, shown)
                    }
                    None => (new_jack_field(cx, refdes, f64::from(self.radius)), None),
                };
                if shown != Some(*load) {
                    widget.as_combo_box().set_text(cx, &load_text(*load, true));
                }
                self.jack_fields.push((refdes.clone(), widget, Some(*load)));
            }
            self.page = Some(page);
        }
        let page = self.page.as_ref().unwrap();
        let state = self.hovered;
        paint.rounded(cx, view, ink.ground, ink.ground, 0.0);
        // the intro's first line beside the area's notch, the rest under it
        let origin = view.pos + dvec2(PAD, TOP - self.scroll_y);
        page.intro.draw(cx, &mut paint, origin, view, state);
        let top = origin.y + page.intro.height;
        let mut bottom = top;
        if has_bench {
            // the grid: power and sources, the board, loads and probes, in rows of `columns`
            let (mut row_top, mut row_h) = (top, 0.0f64);
            for i in 0..3 {
                if i > 0 && i % page.columns == 0 {
                    row_top += row_h;
                    row_h = 0.0;
                }
                let at = dvec2(origin.x + page.item_x(i), row_top);
                self.offsets[i] = at - view.pos;
                let h = match i {
                    0 => {
                        page.left.draw(cx, &mut paint, at, view, state);
                        let mut h = page.left.height;
                        if !self.drives.is_empty() {
                            cx.begin_turtle(fit_at(at + dvec2(0.0, h), page.left.width), Layout::flow_down());
                            for (_, widget) in &self.drives {
                                widget.draw_walk_all(cx, scope, Walk::new(Size::Fixed(page.left.width), Size::fit()));
                            }
                            h += cx.end_turtle().size.y + CARD_GAP;
                        }
                        h
                    }
                    1 => {
                        page.board.draw(cx, &mut paint, at, view, state);
                        page.board.height
                    }
                    _ => {
                        page.right.draw(cx, &mut paint, at, view, state);
                        // the jacks' load fields on their rows
                        for ((_, _, rect), (_, widget, _)) in page.jacks.iter().zip(&self.jack_fields) {
                            widget.draw_walk_all(cx, scope, Walk::new(Size::Fixed(rect.size.x), Size::Fixed(rect.size.y)).with_abs_pos(at + rect.pos));
                        }
                        page.right.height
                    }
                };
                row_h = row_h.max(h);
            }
            bottom = row_top + row_h;
            page.front.draw(cx, &mut paint, dvec2(origin.x, bottom), view, state);
            bottom += page.front.height;
            // ControlsPanel's fit walk calls height_for and draw_in, and publishes its controls.
            self.controls_panel.draw_walk_all(cx, scope, fit_at(dvec2(origin.x, bottom), page.front.width));
            bottom += self.controls_panel.area().rect(cx).size.y;
        }
        let content_h = bottom - origin.y + TOP + PAD;
        self.scroll_max = (content_h - view.size.y).max(0.0);
        if self.scroll_y > self.scroll_max {
            self.scroll_y = self.scroll_max;
            self.area.redraw(cx);
        }
        if self.scroll_max > 0.0 && view.size.y > 8.0 {
            let track = view.size.y - 8.0;
            let h = (track * view.size.y / content_h).max(24.0).min(track);
            let y = view.pos.y + 4.0 + (track - h) * self.scroll_y / self.scroll_max;
            let thumb = rect(view.pos.x + width - 7.0, y, 4.0, h);
            let pill = paint.pill(4.0);
            paint.rounded(cx, thumb, look.text_tertiary, look.text_tertiary, pill);
        }
        self.draw_menu(cx, view, &ink);
        cx.pop_clip_rect();
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) {
            self.follow(cx);
        }
        if let Event::Scroll(scroll) = event {
            if !scroll.handled_y.get() {
                if let Hit::FingerScroll(e) = event.hits(cx, self.area) {
                    let pos = e.abs - self.area.rect(cx).pos;
                    // over an open menu the wheel scrolls its rows; elsewhere the page, and the
                    // menu closes
                    if let Some(menu) = self.menu.as_mut().filter(|m| m.rect.contains(pos)) {
                        menu.scroll = (menu.scroll + e.scroll.y).clamp(0.0, (menu.height() - menu.rect.size.y).max(0.0));
                        menu.hovered = menu.row_at(pos);
                        scroll.handled_y.set(true);
                        self.area.redraw(cx);
                        return;
                    }
                    if self.scroll_max > 0.0 && e.scroll.y != 0.0 {
                        self.menu = None;
                        self.scroll_y = (self.scroll_y + e.scroll.y).clamp(0.0, self.scroll_max);
                        scroll.handled_y.set(true);
                        self.area.redraw(cx);
                        return;
                    }
                }
            }
        }
        if self.bench().is_none() {
            return;
        }
        self.controls_panel.handle_event(cx, event, scope);
        for (_, widget) in &self.drives {
            widget.handle_event(cx, event, scope);
        }
        for (_, widget, _) in &self.jack_fields {
            widget.handle_event(cx, event, scope);
        }
        if let Event::Actions(actions) = event {
            self.drive_actions(cx, actions);
            self.jack_actions(cx, actions);
        }
        if self.menu.is_some() {
            self.handle_menu(cx, event);
            return;
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let over = self.button_at(cx, e.abs).map(|(b, _)| b.key);
                cx.set_cursor(if over.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                if over != self.hovered {
                    self.hovered = over;
                    self.area.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) if self.hovered.is_some() => {
                self.hovered = None;
                self.area.redraw(cx);
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                self.pressed = self.button_at(cx, e.abs).map(|(b, _)| b.key);
            }
            Hit::FingerUp(e) => {
                let pressed = self.pressed.take();
                if e.was_tap() && e.is_over {
                    let hit = self.button_at(cx, e.abs).filter(|(b, _)| Some(b.key) == pressed).map(|(b, origin)| (b.choice, at(b.rect, origin)));
                    if let Some((choice, anchor)) = hit {
                        self.press(cx, choice, anchor);
                    }
                }
            }
            _ => {}
        }
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Bench setup");
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let corner = self.area.rect(cx).pos;
        let clip = self.area.clipped_rect(cx);
        for (b, offset) in self.buttons() {
            let origin = corner + offset;
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                name: b.name.clone(),
                actions: a11y_tree::Actions::PRESS,
                ..Default::default()
            };
            semantics::bounds(&mut node, semantics::intersect(at(b.rect, origin), clip));
            out.push((b.key, None, node));
        }
        // an open menu's rows, each a menu item
        if let Some(menu) = &self.menu {
            for (i, item) in menu.items.iter().enumerate() {
                let mut node = a11y_tree::Node {
                    role: a11y_tree::Role::MenuItem,
                    name: item.text.clone(),
                    actions: a11y_tree::Actions::PRESS,
                    ..Default::default()
                };
                node.states.set(a11y_tree::States::SELECTED, item.current);
                let row = rect(menu.rect.pos.x, menu.rect.pos.y + MENU_PAD + i as f64 * MENU_ROW - menu.scroll, menu.rect.size.x, MENU_ROW);
                semantics::bounds(&mut node, semantics::intersect(at(row, corner), clip));
                out.push((semantics::key("bench menu", &item.text, i as u64), None, node));
            }
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions as A};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let row = self.menu.as_ref().and_then(|menu| {
            menu.items.iter().enumerate().find(|(i, item)| semantics::key("bench menu", &item.text, *i as u64) == key).map(|(_, item)| item.pick.clone())
        });
        if let Some(pick) = row {
            if request.action != A::PRESS {
                return ActionResult::Unsupported;
            }
            self.pick(cx, pick);
            return ActionResult::Handled;
        }
        let Some((choice, rect)) = self.buttons().find(|(b, _)| b.key == key).map(|(b, origin)| (b.choice, at(b.rect, origin))) else { return ActionResult::Unhandled };
        if request.action == A::PRESS {
            self.press(cx, choice, rect);
        } else if request.action == A::SCROLL_INTO_VIEW {
            let top = rect.pos.y;
            let below = top + rect.size.y - self.area.rect(cx).size.y;
            let dy = if top < 0.0 { top } else { below.max(0.0) };
            self.scroll_y = (self.scroll_y + dy).clamp(0.0, self.scroll_max);
            self.area.redraw(cx);
        } else {
            return ActionResult::Unsupported;
        }
        ActionResult::Handled
    }
}

/// How an expectation stands on the run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tone {
    Pending,
    Pass,
    Fail,
}

/// One expectation's row on the Bench editor: its name, what it judges, and its verdict.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpectLine {
    pub name: String,
    /// `mean v(3V3) · 3.2 V to 3.4 V`
    pub what: String,
    /// `pending`, `ok · 3.30 V`, `fails · 0 V at 1.20 ms`
    pub state: String,
    pub tone: Tone,
    /// Its verdict on the counted check's run, when the shelf's last check is of these
    /// expectations: `check: ok · 3.30 V`, `check: fails · 0 V at 1.20 ms`, `check: not judged`.
    pub check: Option<(String, Tone)>,
}

/// The bench's counted check (EXPECTATIONS' Check bench, `elec.bench.run`) as the app last put it
/// up (`ElecLive::check`): a run of its own from power-on to `tstop`, every point judged
/// (`fab_elec_bench::bench_run`). The shelf shows it beside the live verdicts, never in their place.
#[derive(Clone, Debug, PartialEq)]
pub struct BenchCheck {
    /// The bench checked and its expectations, as they were when the check started.
    pub bench: String,
    pub expects: Vec<Expect>,
    pub tstop: f64,
    /// None while it runs.
    pub report: Option<BenchReport>,
    /// The design changed after the checked run was made from it.
    pub earlier: bool,
    /// How far the run has got while it runs.
    pub reached: Reached,
}

/// The time (s) a running check has reached, as its run publishes it after each slice and the
/// shelf reads it: one atomic store a slice, one load a signal. Equal for the same run's.
#[derive(Clone, Debug, Default)]
pub struct Reached(Arc<AtomicU64>);

impl Reached {
    pub fn set(&self, t: f64) {
        self.0.store(t.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

impl PartialEq for Reached {
    fn eq(&self, other: &Reached) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// EXPECTATIONS' button pressed: start a counted check of the bench, or stop the one that runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CheckBench {
    Start,
    Stop,
}

/// The EXPECTATIONS shelf: its rows, its button and what the last check says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExpectShelf {
    pub lines: Vec<ExpectLine>,
    /// What the button does: Check bench, or Stop check while one runs; None without expectations.
    pub button: Option<CheckBench>,
    /// Under the shelf's head: that a check runs, what it found, or that it ran an earlier design.
    pub note: String,
}

/// A verdict's words and tone: a live one (`counted` false) is pending until judged, a counted
/// run's is final (`CheckState::outcome`).
fn verdict(expect: &Expect, state: &CheckState, counted: bool) -> (String, Tone) {
    let unit = expect.unit();
    let value = |value: f64| if value.is_finite() { format!(" · {}", eng(value, unit, 3)) } else { String::new() };
    match state.outcome() {
        Outcome::Fail => {
            let (t, v) = state.failed.unwrap_or((state.at, state.value));
            (format!("fails{} at {}", value(v), eng(t, "s", 3)), Tone::Fail)
        }
        Outcome::Pass if counted || state.verdict == Verdict::Pass => (format!("ok{}", value(state.value)), Tone::Pass),
        _ if counted => ("not judged".to_owned(), Tone::Pending),
        _ => ("pending".to_owned(), Tone::Pending),
    }
}

/// The bench's expectations as rows, with the judge's verdicts when they are of these
/// expectations (else pending). O(expectations).
pub fn expect_lines(out: &BenchOut, circuit: Option<&Circuit>, judged: &Judged) -> Vec<ExpectLine> {
    let states = (judged.expects == out.expects).then_some(&judged.states);
    out.expects.iter().enumerate().map(|(i, expect)| {
        let what = format!("{} {} · {}", expect.measure_name(), expect_probe(&expect.probe, circuit), expect_bound(expect));
        let (state, tone) = match states.and_then(|states| states.get(i)) {
            Some(state) => verdict(expect, state, false),
            None => ("pending".to_owned(), Tone::Pending),
        };
        ExpectLine { name: expect.name.clone(), what, state, tone, check: None }
    }).collect()
}

/// The EXPECTATIONS shelf: the rows (`expect_lines`), each with its verdict on the counted
/// check's run when `check` is of this bench, these expectations and the design as it is, and the
/// check's button and line. O(expectations).
pub fn expect_shelf(out: &BenchOut, circuit: Option<&Circuit>, judged: &Judged, check: Option<&BenchCheck>) -> ExpectShelf {
    let mut lines = expect_lines(out, circuit, judged);
    let current = check.filter(|c| !c.earlier && c.bench == out.name && c.expects == out.expects);
    let report = current.and_then(|c| c.report.as_ref());
    if let Some(report) = report {
        for ((line, expect), state) in lines.iter_mut().zip(&out.expects).zip(&report.states) {
            let (text, tone) = verdict(expect, state, true);
            line.check = Some((format!("check: {text}"), tone));
        }
    }
    let running = check.is_some_and(|c| c.report.is_none());
    let button = (!out.expects.is_empty()).then_some(if running { CheckBench::Stop } else { CheckBench::Start });
    let note = match (check, report) {
        (None, _) => String::new(),
        (Some(c), _) if c.report.is_none() => match c.reached.get() {
            t if t > 0.0 => format!("Checking a run of its own: {} of {}…", eng(t, "s", 3), eng(c.tstop, "s", 3)),
            _ => format!("Checking a run of its own to {}…", eng(c.tstop, "s", 3)),
        },
        (Some(_), None) => "The last check ran an earlier design: Check bench runs it again".to_owned(),
        (Some(c), Some(r)) => {
            let head = if r.stopped {
                format!("Check stopped at {}", eng(r.t, "s", 3))
            } else if let Some(error) = &r.error {
                format!("Check failed at {}: {error}", eng(r.t, "s", 3))
            } else {
                format!("Checked to {}", eng(c.tstop, "s", 3))
            };
            let count = |o: Outcome| r.states.iter().filter(|s| s.outcome() == o).count();
            let counts: Vec<String> = [(count(Outcome::Pass), "pass"), (count(Outcome::Fail), "fail"), (count(Outcome::NotJudged), "not judged")]
                .into_iter().filter(|c| c.0 > 0).map(|(n, what)| format!("{n} {what}")).collect();
            let mut note = if counts.is_empty() { head } else { format!("{head}: {}", counts.join(" · ")) };
            if !r.rescues.is_empty() {
                note.push_str(&format!(" · {} steps only through the gmin ladder", r.rescues.len()));
            }
            note
        }
    };
    ExpectShelf { lines, button, note }
}

/// A bench probe as the bench writes it: `v(3V3)`, `v(A, B)`, `i(D1.A)`, `p(U1)`, named in the
/// bench's circuit. O(nets + parts).
pub fn expect_probe(probe: &Probe, circuit: Option<&Circuit>) -> String {
    let net = |r: NetRef| circuit.and_then(|c| c.nets.iter().find(|n| n.id == r.net)).map_or("?", |n| n.name.as_str()).to_owned();
    let comp = |id| circuit.and_then(|c| c.comps.iter().find(|c| c.id == id));
    match probe {
        Probe::V(n) => format!("v({})", net(*n)),
        Probe::VDiff(a, b) => format!("v({}, {})", net(*a), net(*b)),
        Probe::I(pin) => match comp(pin.comp) {
            Some(c) => format!("i({}.{})", c.refdes, c.part.pins.get(pin.pin.ix()).map_or("?", |p| p.name.as_str())),
            None => "i(?)".to_owned(),
        },
        Probe::P(r) => format!("p({})", comp(r.comp).map_or("?", |c| c.refdes.as_str())),
    }
}

/// An expectation's bounds in its unit: `3.2 V to 3.4 V`, `> 1 kHz`, `= "OK"`.
pub fn expect_bound(expect: &Expect) -> String {
    let unit = expect.unit();
    match &expect.bound {
        Bound::Range { min, max } => match (min.is_finite(), max.is_finite()) {
            (true, true) => format!("{} to {}", eng(*min, unit, 3), eng(*max, unit, 3)),
            (true, false) => format!("> {}", eng(*min, unit, 3)),
            (false, true) => format!("< {}", eng(*max, unit, 3)),
            (false, false) => "any".to_owned(),
        },
        Bound::TextEquals(text) => format!("= {text:?}"),
        Bound::TextContains(text) => format!("has {text:?}"),
    }
}
