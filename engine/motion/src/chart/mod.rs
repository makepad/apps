//! `Chart{type: @bar …}`: data-driven charts built from ordinary layers.
//!
//! The loader expands a chart into a `Group` of `Rect`/`Ellipse`/`Path`/
//! `Text` layers with their animations (bars growing in turn, lines drawing
//! on, slices sweeping round, count-up labels, datasets morphing into the
//! next), so everything a layer can do (spans, effects, inspect, fit)
//! works on charts, and every frame stays a pure function of time.
//!
//! Types: `@bar` (grouped, stacked, horizontal, sorted races), `@line`,
//! `@area`, `@donut`, `@pie`, `@scatter`, `@bubble`, `@network`
//! (a deterministic force layout), `@treemap`, `@map` (lon/lat dots and a
//! continent choropleth), `@kpi` (cards), `@counter` (one big number) and
//! `@timeline`. Data comes inline, or as CSV/JSON text (also through an
//! input the host fills from a library file: `csv: in(@data)`); further
//! datasets (`next`, or `split: "year"` over a long table) morph in at
//! `switch` times. `land` times each element: seconds, `beat(n)`,
//! `word(@vo, i)` / `word(@vo, "text")`, or `{beat: 8 every: 1}` /
//! `{voice: @vo said: @label}`.
//!
//! Every text layer a chart makes is named `<role> · <text>` (`title`,
//! `label`, `value`, `tick`, `legend`, `callout`, …), which is how
//! [`chart_checks`] finds them.

mod axis;
mod bar;
mod board;
pub(crate) mod build;
mod check;
mod data;
mod drum;
mod gantt;
mod kpi;
mod line;
mod meter;
mod network;
mod points;
mod probs;
mod radial;
pub(crate) mod scale;
mod timeline;
mod treemap;
mod world;

pub use check::{chart_checks, ChartIssue, ChartSeverity};

use crate::obj;
use build::*;
use makepad_script::*;
pub(crate) use data::{Datum, Table};
use scale::{text_box, NumFmt, Shown};

pub(crate) const CHART_TYPES: &[&str] = &["bar", "line", "area", "donut", "pie", "scatter", "bubble", "network", "treemap", "map", "kpi", "counter", "timeline", "scoreboard", "versus", "sticker", "meter", "gantt", "axis", "probs"];
pub(crate) const FACES: &[&str] = &["led", "roll", "plain", "led3d", "drum"];
pub(crate) const SHAPES: &[&str] = &["tag", "burst", "circle", "plate"];
pub(crate) const METERS: &[&str] = &["thermometer", "gauge", "bar"];
pub(crate) const THEMES: &[&str] = &["stage", "paper", "neon", "mono", "benchmark"];
/// The roles of a chart's text layers (their names' first word).
pub(crate) const TEXT_ROLES: &[&str] = &["title", "subtitle", "legend", "tick", "label", "value", "callout", "center", "period", "source", "caption", "delta"];

/// What a chart needs from the loader: the document's size, located
/// diagnostics, and constant values (inputs resolved).
pub(crate) trait ChartHost {
    fn doc_size(&self) -> [f64; 2];
    fn error(&mut self, field: &str, message: String, hint: Option<&str>);
    fn warning(&mut self, field: &str, message: String, hint: Option<&str>);
    fn number(&mut self, field: &str, value: ScriptValue) -> Option<f64>;
    fn vec2(&mut self, field: &str, value: ScriptValue) -> Option<[f64; 2]>;
    fn color(&mut self, field: &str, value: ScriptValue) -> Option<u32>;
    /// A string, or a text input's value (CSV/JSON from a library file).
    fn text(&mut self, field: &str, value: ScriptValue) -> Option<String>;
    /// `in(@x)` as the input's value (a number, text, colour or bool);
    /// any other value as it is.
    fn resolve(&mut self, field: &str, value: ScriptValue) -> ScriptValue;
}

/// Colours and faces.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub ink: u32,
    pub muted: u32,
    pub grid: u32,
    pub neutral: u32,
    pub card: u32,
    pub accent: u32,
    pub positive: u32,
    pub negative: u32,
    pub land: u32,
    pub series: Vec<u32>,
    pub font: String,
    pub title_font: String,
}

impl Theme {
    /// The motion guide's palettes (MOTION-LIBRARY §2.3): Stage's dark
    /// ground, the editorial paper, a neon night, and a neutral one that
    /// lets `highlight` carry the story.
    pub fn named(name: &str) -> Theme {
        let inter = || "Inter".to_string();
        match name {
            "paper" => Theme {
                ink: 0x1b1d22ff, muted: 0x5d6570ff, grid: 0x1b1d221f, neutral: 0xc9c2b3ff, card: 0x1b1d220d, accent: 0xd9480fff,
                positive: 0x1f8a4cff, negative: 0xc92a2aff, land: 0xddd6c8ff,
                series: vec![0xd9480fff, 0x4c55c9ff, 0x1f8a6cff, 0xc28a00ff, 0x2a7fb8ff, 0xc2417aff, 0x6b7280ff, 0x7a4fc4ff],
                font: inter(), title_font: "Playfair Display".into(),
            },
            "neon" => Theme {
                ink: 0xeaf6ffff, muted: 0x8fa3b8ff, grid: 0x7fdcff22, neutral: 0x26324aff, card: 0x7fdcff12, accent: 0x00e5ffff,
                positive: 0x7cff6bff, negative: 0xff4d8dff, land: 0x16213aff,
                series: vec![0x00e5ffff, 0xff2bd6ff, 0xb6ff00ff, 0xffd400ff, 0x7c4dffff, 0xff8a3dff, 0x4dffc3ff, 0xffffffff],
                font: inter(), title_font: inter(),
            },
            // WB1's reference board: a dark scoreboard, sodium orange and
            // acid green, condensed display type (REF-Ghdj3BOjgh4 §7).
            "benchmark" => Theme {
                ink: 0xf1ead6ff, muted: 0x9a9483ff, grid: 0xf1ead61c, neutral: 0x3a3c34ff, card: 0x14150fff, accent: 0xff7a1aff,
                positive: 0xa6ff3dff, negative: 0xff4d4dff, land: 0x22241cff,
                series: vec![0xff7a1aff, 0xa6ff3dff, 0xf1ead6ff, 0xffd23fff, 0x5ad1ffff, 0xff8fb1ff],
                font: inter(), title_font: "Anton".into(),
            },
            "mono" => Theme {
                series: vec![0xa6b1bdff, 0x6b7684ff, 0x4b5563ff, 0x8e9aa7ff, 0x3a4250ff, 0xc5ccd4ff],
                ..Theme::named("stage")
            },
            _ => Theme {
                ink: 0xf4f7faff, muted: 0x8e9aa7ff, grid: 0xffffff1a, neutral: 0x3a4250ff, card: 0xffffff0f, accent: 0xff5c39ff,
                positive: 0x35c05fff, negative: 0xff6b6bff, land: 0x222a36ff,
                series: vec![0xff5c39ff, 0x7c83f5ff, 0x2fb894ff, 0xffcf3fff, 0x5ad1ffff, 0xff8fb1ff, 0xa6b1bdff, 0xc792eaff],
                font: inter(), title_font: inter(),
            },
        }
    }

    pub fn color(&self, i: usize) -> u32 {
        self.series[i % self.series.len().max(1)]
    }

    /// Ink that reads on `fill`.
    pub fn ink_on(&self, fill: u32) -> u32 {
        if luminance(fill) > 0.4 { 0x111318ff } else { 0xffffffff }
    }
}

/// A datum picked by index or by its label.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Pick {
    Index(usize),
    Label(String),
}

/// A callout: `annotate: [[3, "record"], {label: "Q4" text: "…" set: 1 at: 4.0}]`.
#[derive(Clone, Debug)]
pub(crate) struct Note {
    pub pick: Pick,
    pub text: String,
    pub set: usize,
    pub at: Option<ScriptValue>,
    /// `@plate` (default), or a sticker: `@tag`, `@burst`, `@circle`.
    pub style: Option<String>,
    /// A sticker's small line above its text.
    pub sub: Option<String>,
}

/// A Gantt review: a "today" line sweeping from `from` to `to` (axis
/// units) starting at `at`, over `length` seconds.
#[derive(Clone, Debug)]
pub(crate) struct Review {
    pub from: f64,
    pub to: f64,
    pub at: ScriptValue,
    pub length: f64,
    pub label: String,
}

/// A wave of activation through a network: from these nodes, at `at`,
/// `hop` seconds per edge, along the links or against them.
#[derive(Clone, Debug)]
pub(crate) struct Activation {
    pub from: Vec<Pick>,
    pub at: ScriptValue,
    pub hop: f64,
    pub backward: bool,
}

/// When each element lands.
#[derive(Clone, Debug)]
pub(crate) enum Land {
    Auto,
    List(Vec<ScriptValue>),
    Beats { first: f64, every: f64 },
    Said { voice: String, what: String },
}

/// A chart being built: its resolved fields, and the layers so far.
pub(crate) struct Chart<'c, 'a, 'b> {
    /// The document's VM the layers are made in.
    pub b: &'c B<'a, 'b>,
    pub kind: String,
    pub theme: String,
    /// Boards' number face (`@led @roll @plain @led3d`).
    pub face: Option<String>,
    /// Stickers' shape, and callouts' (`@tag @burst @circle @plate`);
    /// a meter's style (`@thermometer @gauge @bar`).
    pub shape: Option<String>,
    /// A clock track (`playhead("reel")`, `progress("reel")`) a counter or
    /// meter reads instead of its data.
    pub clock: Option<ScriptValue>,
    /// A layer that rides a line's head (`rider:`), turned to its tangent
    /// with `rider_rotate`, lifted by `rider_offset`.
    pub rider: Option<(ScriptValue, bool, [f64; 2])>,
    pub review: Option<Review>,
    /// `y_scale: @log` / `x_scale: @log`.
    pub log_y: bool,
    pub log_x: bool,
    /// `@axis`: decades (or units) visible around the marker; 0: all.
    pub window: f64,
    pub activate: Vec<Activation>,
    pub header_text: Option<String>,
    pub top_k: Option<usize>,
    pub overflow: Option<String>,
    pub w: f64,
    pub h: f64,
    pub th: Theme,
    pub colors_given: bool,
    pub ls: f64,
    pub fmt: NumFmt,
    pub start: ScriptValue,
    pub stagger: f64,
    pub grow: f64,
    pub ease: Option<ScriptValue>,
    pub land: Land,
    pub axes: Option<bool>,
    pub grid: Option<bool>,
    pub values: bool,
    pub labels: Option<bool>,
    pub legend: Option<bool>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub source: Option<String>,
    pub highlight: Vec<Pick>,
    pub sets: Vec<Table>,
    pub switch: Vec<ScriptValue>,
    pub links: Vec<(Pick, Pick, f64)>,
    pub notes: Vec<Note>,
    pub layout: String,
    pub thickness: f64,
    pub max: Option<f64>,
    pub min: Option<f64>,
    pub horizontal: bool,
    pub stacked: bool,
    pub sort: bool,
    pub top: Option<usize>,
    pub smooth: bool,
    pub dots: Option<bool>,
    pub center: Option<String>,
    pub x_title: Option<String>,
    pub y_title: Option<String>,
    pub root: Option<Pick>,
    pub payoff: Option<ScriptValue>,
    pub regions: Vec<(String, f64)>,
    /// The layers, bottom first.
    pub out: Vec<ScriptValue>,
    /// The plot's box `[x0, y0, x1, y1]` inside the chart, after the header.
    pub plot: [f64; 4],
    /// Each element's label, in the order the kind indexes them.
    pub keys: Vec<String>,
    /// Callout anchors: `[set][element]`.
    pub marks: Vec<Vec<Option<[f64; 2]>>>,
    /// The marks sit above a value label rather than on the datum (no ring).
    pub lifted: bool,
    /// Boxes callouts keep clear of (a donut's disc), besides the texts.
    pub avoid: Vec<[f64; 4]>,
    pub warnings: Vec<(String, String)>,
}

impl Chart<'_, '_, '_> {
    pub fn ease(&self, default: &str) -> ScriptValue {
        self.ease.unwrap_or_else(|| id(default))
    }

    /// The element count the land times are for.
    fn count(&self) -> usize {
        self.keys.len().max(self.sets.first().map_or(0, |t| t.rows.len()))
    }

    /// When element `i` of the first dataset lands.
    pub fn land(&self, i: usize) -> ScriptValue {
        let b = self.b;
        let auto = |i: usize| shift(b, self.start, self.grow + self.stagger * i as f64);
        match &self.land {
            Land::Auto => auto(i),
            Land::List(times) => match times.get(i) {
                Some(t) => *t,
                None if times.is_empty() => auto(i),
                None => shift(b, times[times.len() - 1], self.stagger * (i + 1 - times.len()) as f64),
            },
            Land::Beats { first, every } => beat(b, first + every * i as f64),
            Land::Said { voice, what } => {
                let datum = self.sets.first().and_then(|t| {
                    let key = self.keys.get(i)?;
                    t.rows.iter().find(|d| &d.label == key)
                }).or_else(|| self.sets.first().and_then(|t| t.rows.get(i)));
                let said = datum.map(|d| match what.as_str() {
                    "text" => d.text.clone().unwrap_or_default(),
                    "value" => self.fmt.shown(d.value.abs(), 0.0, &[d.value]).text(d.value),
                    _ => d.label.clone(),
                }).unwrap_or_default();
                if said.trim().is_empty() {
                    auto(i)
                } else {
                    let w = word(b, voice, s(b, &said));
                    shift(b, w, self.grow * 0.35)
                }
            }
        }
    }

    /// When element `i` starts growing.
    pub fn enter(&self, i: usize) -> ScriptValue {
        shift(self.b, self.land(i), -self.grow)
    }

    /// When element `i` reaches its value in dataset `k`.
    pub fn set_time(&self, k: usize, i: usize) -> ScriptValue {
        if k == 0 {
            self.land(i)
        } else {
            let step = if self.sort { 0.0 } else { self.stagger * 0.3 };
            shift(self.b, self.switch[k - 1], self.grow + step * i as f64)
        }
    }

    /// When dataset `k` starts arriving.
    pub fn set_start(&self, k: usize) -> ScriptValue {
        if k == 0 { self.start } else { self.switch[k - 1] }
    }

    pub fn highlighted(&self, i: usize) -> bool {
        self.highlight.iter().any(|p| match p {
            Pick::Index(k) => *k == i,
            Pick::Label(l) => self.keys.get(i) == Some(l),
        })
    }

    pub fn index_of(&self, pick: &Pick) -> Option<usize> {
        match pick {
            Pick::Index(i) => Some(*i),
            Pick::Label(l) => self.keys.iter().position(|k| k == l).or_else(|| self.keys.iter().position(|k| k.eq_ignore_ascii_case(l))),
        }
    }

    /// A text layer named `<role> · <text>`.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&self, role: &str, text: &str, at: [f64; 2], align: &str, valign: &str, size: f64, weight: f64, ink: u32, appear: ScriptValue, fit: Option<f64>) -> ScriptValue {
        let b = self.b;
        let mut fields = vec![
            ("name", s(b, &format!("{role} · {text}"))), ("text", s(b, text)), ("font", font_face(b, &self.th.font, weight)),  ("size", num(size / scale::PX)),
            ("color", color(ink)), ("at", v2(b, at[0], at[1])), ("align", id(align)), ("valign", id(valign)), ("opacity", fade_in(b, appear, 0.3)),
        ];
        if let Some(fit) = fit {
            fields.push(("fit", num(fit.max(1.0))));
        }
        layer(b, "Text", fields)
    }

    /// A count-up Text: 0 to `values[0]` by `times[0]`, then on to each
    /// next value at its time. `at` is a position or a track.
    #[allow(clippy::too_many_arguments)]
    pub fn number(&self, role: &str, values: &[f64], times: &[ScriptValue], shown: &Shown, at: ScriptValue, align: &str, valign: &str, size: f64, weight: f64, ink: u32, fit: Option<f64>) -> ScriptValue {
        let b = self.b;
        let ease = self.ease("ease_out_cubic");
        let from = shift(b, times[0], -self.grow);
        let number = if values.len() == 1 {
            tween(b, num(0.0), num(values[0] / shown.div), from, times[0], ease)
        } else {
            let mut points = vec![(from, num(0.0))];
            for (k, (v, t)) in values.iter().zip(times).enumerate() {
                // A later dataset starts moving at its switch.
                if k > 0 {
                    points.push((shift(b, *t, -self.grow), num(values[k - 1] / shown.div)));
                }
                points.push((*t, num(v / shown.div)));
            }
            keys(b, points, ease)
        };
        let last = values.last().copied().unwrap_or(0.0);
        let fade_end = shift(b, from, 0.25);
        let mut fields = vec![
            ("name", s(b, &format!("{role} · {}", shown.text(last)))), ("number", number), ("decimals", num(shown.decimals as f64)), ("grouping", boolean(true)),
            ("prefix", s(b, &shown.prefix)), ("suffix", s(b, &shown.suffix)), ("tabular", boolean(true)), ("font", font_face(b, &self.th.font, weight)), 
            ("size", num(size / scale::PX)), ("color", color(ink)), ("at", at), ("align", id(align)), ("valign", id(valign)),
            ("opacity", tween(b, num(0.0), num(1.0), from, fade_end, id("ease_out_cubic"))),
        ];
        if let Some(fit) = fit {
            fields.push(("fit", num(fit.max(1.0))));
        }
        if let Some(time) = &shown.time {
            fields.push(("format", id(time)));
            if let Some(total) = shown.total {
                fields.push(("total", num(total)));
            }
            // `overflow: @wall`: a huge escalating number tiles the frame.
            if time == "escalate" && self.overflow.as_deref() == Some("wall") {
                fields.push(("overflow", id("wall")));
            }
        }
        layer(b, "Text", fields)
    }

    /// A hairline (gridline, axis, leader) as a Rect.
    pub fn rule(&self, at: [f64; 2], size: [f64; 2], fill: u32, appear: ScriptValue) -> ScriptValue {
        let b = self.b;
        layer(b, "Rect", vec![("at", v2(b, at[0], at[1])), ("size", v2(b, size[0], size[1])), ("fill", color(fill)), ("opacity", fade_in(b, appear, 0.4))])
    }

    /// Title, subtitle, legend and source: laid out at the top and bottom
    /// of the chart's box; the plot is what is left.
    pub fn header(&mut self, legend: &[(String, u32)]) {
        let b = self.b;
        let (w, ls) = (self.w, self.ls);
        let mut y = 0.0;
        let appear = shift(b, self.start, 0.1);
        if let Some(title) = self.title.clone() {
            let size = ls * 1.55;
            let t = self.text("title", &title, [0.0, y], "left", "top", size, 700.0, self.th.ink, appear, Some(w));
            set(b, t, "font", font_face(b, &self.th.title_font, 700.0));
            set(b, t, "tracking", num(-0.5));
            self.out.push(t);
            y += size * 1.35;
        }
        if let Some(sub) = self.subtitle.clone() {
            let t = self.text("subtitle", &sub, [0.0, y], "left", "top", ls * 0.95, 400.0, self.th.muted, shift(b, appear, 0.1), Some(w));
            self.out.push(t);
            y += ls * 1.45;
        }
        let show_legend = self.legend.unwrap_or(legend.len() > 1);
        if show_legend && !legend.is_empty() {
            y += ls * 0.35;
            let sw = ls * 0.72;
            let size = ls * 0.9;
            let mut x = 0.0;
            for (k, (name, fill)) in legend.iter().enumerate() {
                let item_w = sw + ls * 0.45 + scale::text_width(name, size, 500.0) + ls * 1.4;
                if x > 0.0 && x + item_w > w {
                    x = 0.0;
                    y += ls * 1.5;
                }
                let t = shift(b, appear, 0.15 + 0.06 * k as f64);
                self.out.push(layer(b, "Rect", vec![
                    ("name", s(b, &format!("swatch {name}"))), ("at", v2(b, x, y + (size * 1.21 - sw) * 0.5)), ("size", v2(b, sw, sw)), ("radius", num(sw * 0.22)),
                    ("fill", color(*fill)), ("opacity", fade_in(b, t, 0.3)),
                ]));
                let label = self.text("legend", name, [x + sw + ls * 0.45, y], "left", "top", size, 500.0, self.th.ink, t, Some((w - x - sw - ls * 0.45).max(10.0)));
                self.out.push(label);
                x += item_w;
            }
            y += ls * 1.6;
        }
        if y > 0.0 {
            y += ls * 0.6;
        }
        let mut bottom = self.h;
        if let Some(source) = self.source.clone() {
            let size = ls * 0.7;
            let t = self.text("source", &source, [0.0, self.h], "left", "bottom", size, 400.0, self.th.muted, appear, Some(w));
            self.out.push(t);
            bottom -= size * 1.21 + ls * 0.6;
        }
        self.plot = [0.0, y, w, bottom.max(y + 10.0)];
    }

    /// The boxes of the chart's static texts so far (`[x0, y0, x1, y1]`),
    /// estimated: what a callout must not cover.
    fn static_text_boxes(&self) -> Vec<[f64; 4]> {
        let b = self.b;
        let pair = |v: ScriptValue| -> Option<[f64; 2]> {
            let p = items_of(b, v).filter(|p| p.len() == 2)?;
            Some([number_of(b, p[0])?, number_of(b, p[1])?])
        };
        self.out.iter().filter_map(|layer| {
            if type_of(b, *layer) != Some("Text") {
                return None;
            }
            // A moving text counts where it comes to rest.
            let rest = |v: ScriptValue| -> Option<[f64; 2]> {
                if let Some(p) = items_of(b, v) {
                    if p.len() == 2 {
                        return Some([number_of(b, p[0])?, number_of(b, p[1])?]);
                    }
                    return None;
                }
                v.as_object()?;
                let points = field_of(b, v, "points");
                match points.or_else(|| field_of(b, v, "to")) {
                    Some(list) if points.is_some() => {
                        let last = *items_of(b, list)?.last()?;
                        let last = items_of(b, last).filter(|l| l.len() >= 2)?;
                        pair(last[1])
                    }
                    Some(p) => pair(p),
                    None => None,
                }
            };
            let at = rest(field_of(b, *layer, "at")?)?;
            let name_value = field_of(b, *layer, "name")?;
            let name = b.with(|vm| obj::string(vm, name_value))?;
            let text = name.split_once(" · ").map_or(name.clone(), |(_, t)| t.to_string());
            let size = number_of(b, field_of(b, *layer, "size")?)? * scale::PX;
            let weight = field_of(b, *layer, "font").and_then(|f| f.as_object()).and_then(|f| b.with(|vm| obj::opt(vm, f, "weight").and_then(|w| obj::num(vm, w)))).unwrap_or(400.0);
            let fit = field_of(b, *layer, "fit").and_then(|v| number_of(b, v));
            let [w, h] = text_box(&text, size, weight, None, fit, 1.0);
            let align = field_of(b, *layer, "align").and_then(obj::ident);
            let valign = field_of(b, *layer, "valign").and_then(obj::ident);
            let af = match align.as_deref() { Some("center") => 0.5, Some("right") => 1.0, _ => 0.0 };
            let vf = match valign.as_deref() { Some("center") => 0.5, Some("bottom") => 1.0, _ => 0.0 };
            Some([at[0] - w * af, at[1] - h * vf, at[0] - w * af + w, at[1] - h * vf + h])
        }).collect()
    }

    /// Callouts from `annotate`: a plate with a leader to the datum's
    /// anchor, placed above, below or beside it, wherever it covers no
    /// other text and stays inside the chart's box.
    pub fn callouts(&mut self) {
        let b = self.b;
        let notes = std::mem::take(&mut self.notes);
        let mut taken = self.static_text_boxes();
        taken.extend(self.avoid.iter().copied());
        for note in &notes {
            let Some(i) = self.index_of(&note.pick) else {
                self.warnings.push(("annotate".into(), format!("the callout \"{}\" names {:?}, which is not an element of this chart", note.text, note.pick)));
                continue;
            };
            let k = note.set.min(self.marks.len().saturating_sub(1));
            let Some(Some(p)) = self.marks.get(k).and_then(|m| m.get(i)).copied() else {
                self.warnings.push(("annotate".into(), format!("the callout \"{}\" points at element {i}, and the chart has {}", note.text, self.marks.first().map_or(0, Vec::len))));
                continue;
            };
            let ls = self.ls;
            let t = note.at.unwrap_or_else(|| shift(b, self.set_time(k, i), 0.35));
            // A sticker callout (`style: @tag | @burst | @circle`) or a plate.
            let style = note.style.clone().or_else(|| self.shape.clone().filter(|_| self.kind != "sticker")).filter(|s| s != "plate");
            let sticker_size = ls * 1.1;
            let (bw, bh) = match &style {
                Some(shape) => {
                    let (_, [w, h]) = board::sticker(self, shape, &note.text, note.sub.as_deref(), [0.0, 0.0], sticker_size, 0, num(0.0), 0.0);
                    (w, h)
                }
                None => {
                    let [tw, th] = text_box(&note.text, ls, 700.0, None, Some(self.w * 0.45), 1.0);
                    (tw + 32.0, th + 16.0)
                }
            };
            let lead = ls * 1.3;
            // Candidate boxes: above, below, right, left, and the diagonals.
            let clamp_x = |x0: f64| x0.clamp(2.0, (self.w - bw - 2.0).max(2.0));
            let clamp_y = |y0: f64| y0.clamp(2.0, (self.h - bh - 2.0).max(2.0));
            let candidates = [
                [clamp_x(p[0] - bw * 0.5), p[1] - lead - bh],
                [clamp_x(p[0] - bw * 0.5), p[1] + lead],
                [p[0] + lead, clamp_y(p[1] - bh * 0.5)],
                [p[0] - lead - bw, clamp_y(p[1] - bh * 0.5)],
                [p[0] + lead * 0.7, p[1] - lead * 0.7 - bh],
                [p[0] - lead * 0.7 - bw, p[1] - lead * 0.7 - bh],
            ];
            let inside = |b: [f64; 2]| b[0] >= 0.0 && b[1] >= 0.0 && b[0] + bw <= self.w && b[1] + bh <= self.h;
            let covered = |b: [f64; 2]| taken.iter().map(|o| ((b[0] + bw).min(o[2]) - b[0].max(o[0])).max(0.0) * ((b[1] + bh).min(o[3]) - b[1].max(o[1])).max(0.0)).sum::<f64>();
            let best = candidates.iter().copied().filter(|b| inside(*b)).min_by(|a, b| covered(*a).total_cmp(&covered(*b))).unwrap_or([clamp_x(p[0] - bw * 0.5), clamp_y(p[1] - lead - bh)]);
            taken.push([best[0], best[1], best[0] + bw, best[1] + bh]);
            let centre = [best[0] + bw * 0.5, best[1] + bh * 0.5];
            let edge = [p[0].clamp(best[0], best[0] + bw), p[1].clamp(best[1], best[1] + bh)];
            let dir = { let d = [edge[0] - p[0], edge[1] - p[1]]; let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6); [d[0] / l, d[1] / l] };
            if !self.lifted {
                let ring = layer(b, "Ellipse", vec![
                    ("name", s(b, "callout ring")), ("at", v2(b, p[0] - ls * 0.45, p[1] - ls * 0.45)), ("size", v2(b, ls * 0.9, ls * 0.9)), ("stroke", color(self.th.accent)),
                    ("width", num(3.0)), ("scale", tween(b, num(0.0), num(1.0), shift(b, t, -0.3), shift(b, t, 0.05), id("ease_out_back"))),
                ]);
                self.out.push(ring);
            }
            let start = if self.lifted { p } else { [p[0] + dir[0] * ls * 0.45, p[1] + dir[1] * ls * 0.45] };
            let leader = layer(b, "Path", vec![
                ("name", s(b, "callout leader")), ("svg", s(b, &format!("M {:.2} {:.2} L {:.2} {:.2}", start[0], start[1], edge[0], edge[1]))),
                ("stroke", color(self.th.accent)), ("width", num(2.5)), ("cap", id("round")),
                ("trim_end", tween(b, num(0.0), num(1.0), shift(b, t, -0.2), t, id("ease_out_cubic"))),
            ]);
            self.out.push(leader);
            if let Some(shape) = &style {
                let (group, _) = board::sticker(self, shape, &note.text, note.sub.as_deref(), centre, sticker_size, self.th.accent, shift(b, t, 0.25), if dir[0] >= 0.0 { -5.0 } else { 5.0 });
                self.out.push(group);
                continue;
            }
            let label = self.text("callout", &note.text, centre, "center", "center", ls, 700.0, self.th.ink_on(self.th.accent), shift(b, t, 0.2), Some(self.w * 0.45));
            set(b, label, "plate", color(self.th.accent));
            set(b, label, "plate_radius", num(8.0));
            set(b, label, "x", tween(b, num(dir[0] * -10.0), num(0.0), shift(b, t, -0.1), shift(b, t, 0.3), id("ease_out_cubic")));
            set(b, label, "y", tween(b, num(dir[1] * -10.0), num(0.0), shift(b, t, -0.1), shift(b, t, 0.3), id("ease_out_cubic")));
            self.out.push(label);
        }
    }

    /// The layout's anchors for dataset `k`, element `i`.
    pub fn mark(&mut self, k: usize, i: usize, p: [f64; 2]) {
        while self.marks.len() <= k {
            self.marks.push(Vec::new());
        }
        let row = &mut self.marks[k];
        while row.len() <= i {
            row.push(None);
        }
        row[i] = Some(p);
    }
}

/// `in(@x)` inside a data list, replaced by the input's value.
fn inputs_in(b: &B, value: ScriptValue, host: &mut dyn ChartHost) -> ScriptValue {
    if type_of(b, value) == Some("In") {
        return host.resolve("data", value);
    }
    if let Some(items) = value.as_array().and_then(|_| items_of(b, value)) {
        let items: Vec<ScriptValue> = items.into_iter().map(|item| inputs_in(b, item, host)).collect();
        return list(b, items);
    }
    let Some(o) = value.as_object() else { return value };
    if b.with(|vm| obj::is_fn(vm, value)) {
        return value;
    }
    if let Some(items) = items_of(b, value) {
        let items: Vec<ScriptValue> = items.into_iter().map(|item| inputs_in(b, item, host)).collect();
        return list(b, items);
    }
    let copy = b.with(|vm| obj::copy_without(vm, o, &[]));
    for (k, v, _) in b.with(|vm| obj::fields(vm, copy)) {
        let new = inputs_in(b, v, host);
        if new != v {
            b.with(|vm| obj::set(vm, copy, &k, new));
        }
    }
    copy.into()
}

/// A `Chart{…}` as the `Group` it draws as, made in the VM of `b`.
/// `None` after an error (the host has it).
pub(crate) fn expand(b: &B, o: ScriptObject, host: &mut dyn ChartHost) -> Option<ScriptValue> {
    let doc = host.doc_size();
    let get = |field: &str| b.with(|vm| obj::opt(vm, o, field));
    let ident = |field: &str| get(field).and_then(|v| text_of(b, v));
    let kind = match get("type").and_then(obj::ident) {
        Some(t) if CHART_TYPES.contains(&t.as_str()) => t,
        Some(t) => {
            let near = makepad_script::suggest::nearest(&t, CHART_TYPES.iter().copied(), 1).first().map(|n| format!(" Did you mean @{n}?")).unwrap_or_default();
            host.error("type", format!("`@{t}` is not a chart type.{near}"), Some(&format!("chart types: @{}", CHART_TYPES.join(", @"))));
            return None;
        }
        None => {
            host.error("type", format!("a Chart names its `type: @bar` (or @{})", CHART_TYPES[1..].join(", @")), None);
            return None;
        }
    };
    let split = match get("split") {
        Some(v) => host.text("split", v),
        None => None,
    };
    let is_list = |v: ScriptValue| items_of(b, v).is_some();
    let sets = match (get("data"), get("csv"), get("json")) {
        (Some(d), None, None) if is_list(d) => {
            let d = inputs_in(b, d, host);
            data::tables_of(b, d, split.as_deref())
        }
        (Some(t), None, None) => data::tables_of_text(b, &host.text("data", t)?, split.as_deref()),
        (None, Some(t), None) => data::tables_of_text(b, &host.text("csv", t)?, split.as_deref()),
        (None, None, Some(t)) => data::tables_of_text(b, &host.text("json", t)?, split.as_deref()),
        // A meter or counter on a clock needs no data.
        (None, None, None) if get("clock").is_some() => {
            let zero = list(b, vec![num(0.0)]);
            data::tables_of(b, zero, split.as_deref())
        }
        _ => {
            host.error("data", "a Chart has one of `data: […]`, `csv: \"…\"` or `json: \"…\"`".into(), Some("`data: [[\"Q1\", 12], [\"Q2\", 18]]`, or `csv: in(@data)` for a library file"));
            return None;
        }
    };
    let mut sets = match sets {
        Ok(sets) => sets,
        Err(message) => {
            host.error(if split.is_some() { "split" } else { "data" }, message, None);
            return None;
        }
    };
    if let Some(next) = get("next").and_then(|v| items_of(b, v)) {
        for (k, set) in next.into_iter().enumerate() {
            let tables = if is_list(set) {
                data::tables_of(b, set, None)
            } else {
                match host.text("next", set) {
                    Some(t) => data::tables_of_text(b, &t, None),
                    None => continue,
                }
            };
            match tables {
                Ok(mut t) => sets.append(&mut t),
                Err(message) => host.error("next", format!("dataset {}: {message}", k + 1), None),
            }
        }
    }
    if let Some(names) = get("series").and_then(|v| items_of(b, v)) {
        let names: Vec<String> = names.into_iter().filter_map(|n| text_of(b, n)).collect();
        for set in &mut sets {
            set.series = names.clone();
        }
    }
    if sets.iter().all(|t| t.rows.is_empty()) {
        host.error("data", "the chart has no data".into(), None);
        return None;
    }
    let given = |field: &str| get(field).filter(|v| !v.is_nil());
    let num = |host: &mut dyn ChartHost, field: &str, default: f64| match given(field) {
        None => default,
        Some(value) => host.number(field, value).unwrap_or(default),
    };
    let opt_num = |host: &mut dyn ChartHost, field: &str| match given(field) {
        None => None,
        Some(value) => host.number(field, value),
    };
    let flag = |field: &str| get(field).and_then(|v| v.as_bool());
    let text = |host: &mut dyn ChartHost, field: &str| match given(field) {
        None => None,
        Some(value) => host.text(field, value),
    };
    let size = match get("size") {
        Some(value) => host.vec2("size", value).unwrap_or([doc[0] * crate::SAFE_WIDTH, doc[0] * 0.45]),
        // Landscape: a wide band; portrait (9:16): a tall block.
        None if doc[1] > doc[0] * 1.2 => [doc[0] * 0.82, (doc[1] * 0.5).min(doc[0] * 1.2)],
        None => [doc[0] * crate::SAFE_WIDTH, (doc[1] * 0.8).min(doc[0] * 0.5)],
    };
    let theme_name = ident("theme").unwrap_or_else(|| "stage".into());
    if !THEMES.contains(&theme_name.as_str()) {
        host.warning("theme", format!("`@{theme_name}` is not a chart theme; using @stage"), Some("themes: @stage (dark), @paper (editorial light), @neon, @mono (neutral, `highlight` in the accent)"));
    }
    let mut th = Theme::named(&theme_name);
    let mut colors: Vec<u32> = Vec::new();
    match get("colors") {
        Some(v) if is_list(v) => {
            for item in items_of(b, v).unwrap_or_default() {
                if let Some(c) = host.color("colors", item) {
                    colors.push(c);
                }
            }
        }
        Some(other) => host.error("colors", format!("`colors` is a list of colours, not {}", kind_of(b, other)), None),
        None => {}
    }
    if let Some(c) = get("color") {
        if let Some(c) = host.color("color", c) {
            colors.insert(0, c);
        }
    }
    let colors_given = !colors.is_empty();
    if colors_given {
        th.series = colors;
    }
    if let Some(v) = get("accent") {
        if let Some(c) = host.color("accent", v) {
            th.accent = c;
        }
    }
    if let Some(v) = get("label_color") {
        if let Some(c) = host.color("label_color", v) {
            th.ink = c;
        }
    }
    // `font`/`title_font` are Font values; the chart's roles set their
    // own weights, so the family is what a Font gives it.
    if let Some(v) = given("font") {
        match b.with(|vm| crate::font_spec::font_spec(vm, v)) {
            Ok(face) => {
                th.font = face.family.to_string();
                th.title_font = th.font.clone();
            }
            Err(error) => host.error("font", error, None),
        }
    }
    if let Some(v) = given("title_font") {
        match b.with(|vm| crate::font_spec::font_spec(vm, v)) {
            Ok(face) => th.title_font = face.family.to_string(),
            Err(error) => host.error("title_font", error, None),
        }
    }
    // `label_size` is a Text size (points); the layout works in its em.
    let ls = num(host, "label_size", (size[1].min(size[0] * 0.75) * 0.05).clamp(20.0, 60.0) / scale::PX) * scale::PX;
    let fmt_kind = ident("format").unwrap_or_else(|| "plain".into());
    if !["plain", "compact", "currency", "percent", "timecode", "mmss", "hhmmss", "seconds", "escalate"].contains(&fmt_kind.as_str()) {
        host.error("format", format!("`@{fmt_kind}` is not a number format"), Some("@plain (12,400), @compact (12.4K), @currency ($12.4K), @percent (42%), or a time: @mmss (45:10), @hhmmss, @timecode, @seconds, @escalate (999,999 → 12.4 million → 1.0e42)"));
    }
    let fmt = NumFmt {
        total: opt_num(host, "total"),
        kind: fmt_kind,
        decimals: opt_num(host, "decimals").map(|d| d.clamp(0.0, 6.0) as u32),
        prefix: text(host, "prefix").unwrap_or_default(),
        suffix: text(host, "suffix").unwrap_or_default(),
        currency: text(host, "currency").unwrap_or_else(|| "$".into()),
    };
    let field = |v: ScriptValue, name: &str| field_of(b, v, name);
    let number = |v: Option<ScriptValue>| v.and_then(|v| number_of(b, v));
    let string = |v: Option<ScriptValue>| v.and_then(|v| b.with(|vm| obj::string(vm, v)));
    let is_record = |v: ScriptValue| v.as_object().is_some() && !is_list(v) && !b.with(|vm| obj::is_fn(vm, v));
    let land = match get("land") {
        None => Land::Auto,
        Some(v) if v.is_nil() => Land::Auto,
        Some(v) if is_list(v) => Land::List(items_of(b, v).unwrap_or_default()),
        Some(v) if is_record(v) && field(v, "time").is_none() => {
            if let Some(first) = number(field(v, "beat")) {
                Land::Beats { first, every: number(field(v, "every")).unwrap_or(1.0) }
            } else if let Some(voice) = field(v, "voice").and_then(obj::ident) {
                match field(v, "said") {
                    Some(w) if obj::ident(w).is_some_and(|w| ["label", "value", "text"].contains(&w.as_str())) => Land::Said { voice, what: obj::ident(w).unwrap() },
                    Some(w) if is_list(w) => Land::List(items_of(b, w).unwrap_or_default().into_iter().map(|p| match b.with(|vm| obj::string(vm, p)) {
                        Some(t) => word(b, &voice, s(b, &t)),
                        None => word(b, &voice, p),
                    }).collect()),
                    _ => {
                        host.error("land", "`land: {voice: @vo said: @label}` (or @value, @text, or a list of phrases or word indices)".into(), None);
                        Land::Auto
                    }
                }
            } else {
                host.error("land", "`land` is a list of times, `{beat: 8 every: 1}` or `{voice: @vo said: @label}`".into(), None);
                Land::Auto
            }
        }
        Some(t) => Land::List(vec![t]),
    };
    let pick = |v: ScriptValue| -> Option<Pick> {
        if let Some(n) = v.as_number() {
            return (n >= 0.0).then_some(Pick::Index(n as usize));
        }
        text_of(b, v).map(Pick::Label)
    };
    let highlight: Vec<Pick> = match get("highlight") {
        Some(v) if is_list(v) => items_of(b, v).unwrap_or_default().into_iter().filter_map(pick).collect(),
        Some(v) => pick(v).into_iter().collect(),
        None => Vec::new(),
    };
    let links: Vec<(Pick, Pick, f64)> = match get("links").and_then(|v| items_of(b, v)) {
        Some(items) => items.into_iter().filter_map(|l| {
            if let Some(p) = items_of(b, l) {
                if p.len() >= 2 {
                    return Some((pick(p[0])?, pick(p[1])?, p.get(2).and_then(|w| number_of(b, *w)).unwrap_or(1.0)));
                }
                return None;
            }
            if is_record(l) {
                return Some((pick(field(l, "from").or(field(l, "source"))?)?, pick(field(l, "to").or(field(l, "target"))?)?, number(field(l, "weight")).unwrap_or(1.0)));
            }
            None
        }).collect(),
        None => Vec::new(),
    };
    let notes: Vec<Note> = match get("annotate") {
        Some(v) if is_list(v) => items_of(b, v).unwrap_or_default().into_iter().filter_map(|a| {
            if let Some(p) = items_of(b, a) {
                if p.len() == 2 {
                    return Some(Note { pick: pick(p[0])?, text: string(Some(p[1]))?, set: 0, at: None, style: None, sub: None });
                }
                return None;
            }
            if is_record(a) {
                return Some(Note {
                    pick: pick(field(a, "index").or(field(a, "label"))?)?,
                    text: string(field(a, "text"))?,
                    set: number(field(a, "set")).unwrap_or(0.0).max(0.0) as usize,
                    at: field(a, "at"),
                    style: field(a, "style").and_then(obj::ident),
                    sub: string(field(a, "sub")),
                });
            }
            None
        }).collect(),
        Some(other) => {
            host.error("annotate", format!("`annotate` is a list of `[index, \"text\"]` or `{{label: \"Q4\" text: \"…\"}}`, not {}", kind_of(b, other)), None);
            Vec::new()
        }
        None => Vec::new(),
    };
    let regions: Vec<(String, f64)> = match get("regions").filter(|v| is_record(*v)) {
        Some(r) => b.with(|vm| obj::fields(vm, r.as_object().unwrap())).into_iter().filter_map(|(k, v, _)| Some((k, number_of(b, v)?))).collect(),
        None => Vec::new(),
    };
    let start = get("start").unwrap_or(0.2.into());
    let stagger = num(host, "stagger", match kind.as_str() {
        "line" | "area" => 0.18,
        "timeline" => 0.6,
        "kpi" => 0.15,
        _ => 0.12,
    });
    let grow = num(host, "grow", 0.8);
    let orient = ident("orient").unwrap_or_default();
    let mode = ident("mode").unwrap_or_default();
    let ident_of = |host: &mut dyn ChartHost, field: &str, allowed: &[&str]| -> Option<String> {
        let value = ident(field)?;
        if allowed.contains(&value.as_str()) {
            Some(value)
        } else {
            host.error(field, format!("`@{value}` is not a `{field}`"), Some(&format!("{field}: @{}", allowed.join(" | @"))));
            None
        }
    };
    let face = ident_of(host, "face", FACES);
    let shape = if kind == "meter" { ident_of(host, "style", METERS) } else { ident_of(host, "shape", SHAPES).or(ident_of(host, "callout", SHAPES)) };
    let clock = get("clock");
    let review = match get("review") {
        Some(r) if is_record(r) => Some(Review {
            from: number(field(r, "from")).unwrap_or(f64::NAN),
            to: number(field(r, "to")).unwrap_or(f64::NAN),
            at: field(r, "at").unwrap_or(f64::NAN.into()),
            length: number(field(r, "length")).unwrap_or(3.0),
            label: string(field(r, "label")).unwrap_or_else(|| "TODAY".into()),
        }),
        Some(other) => {
            host.error("review", format!("`review` is `{{from: 0 to: 9 at: 2.0 length: 3.0 label: \"TODAY\"}}`, not {}", kind_of(b, other)), None);
            None
        }
        None => None,
    };
    let activate: Vec<Activation> = match get("activate") {
        Some(v) if is_list(v) => items_of(b, v).unwrap_or_default().into_iter().filter_map(|a| {
            if !is_record(a) {
                return None;
            }
            let from = match field(a, "from") {
                Some(l) if is_list(l) => items_of(b, l).unwrap_or_default().into_iter().filter_map(pick).collect(),
                Some(v) => pick(v).into_iter().collect(),
                None => Vec::new(),
            };
            Some(Activation {
                from,
                at: field(a, "at").unwrap_or(1.0.into()),
                hop: number(field(a, "hop")).unwrap_or(0.35),
                backward: field(a, "direction").and_then(obj::ident).as_deref() == Some("backward"),
            })
        }).collect(),
        Some(other) => {
            host.error("activate", format!("`activate` is a list of `{{from: \"input\" at: 2.0 hop: 0.3 direction: @forward}}`, not {}", kind_of(b, other)), None);
            Vec::new()
        }
        None => Vec::new(),
    };
    let rider = get("rider").map(|layer| {
        let offset = get("rider_offset").and_then(|v| host.vec2("rider_offset", v)).unwrap_or([0.0, 0.0]);
        (layer, flag("rider_rotate").unwrap_or(false), offset)
    });
    let mut c = Chart {
        b,
        kind: kind.clone(),
        theme: theme_name.clone(),
        face,
        shape,
        clock,
        rider,
        review,
        log_y: ident("y_scale").as_deref() == Some("log"),
        log_x: ident("x_scale").as_deref() == Some("log"),
        window: num(host, "window", 0.0),
        activate,
        header_text: text(host, "header"),
        top_k: opt_num(host, "top_k").map(|k| k.max(1.0) as usize),
        overflow: ident("overflow"),
        w: size[0],
        h: size[1],
        th,
        colors_given,
        ls,
        fmt,
        start,
        stagger,
        grow,
        ease: get("ease"),
        land,
        axes: flag("axes"),
        grid: flag("grid"),
        values: flag("values").unwrap_or(true),
        labels: flag("labels"),
        legend: flag("legend"),
        title: text(host, "title"),
        subtitle: text(host, "subtitle"),
        source: text(host, "source"),
        highlight,
        sets,
        switch: Vec::new(),
        links,
        notes,
        layout: ident("layout").unwrap_or_else(|| "force".into()),
        thickness: num(host, "thickness", (size[1] * 0.008).max(3.0)),
        max: opt_num(host, "max"),
        min: opt_num(host, "min"),
        horizontal: orient == "horizontal" || orient == "h",
        stacked: mode == "stacked" || mode == "stack",
        sort: flag("sort").unwrap_or(false),
        top: opt_num(host, "top").map(|n| n.max(1.0) as usize),
        smooth: flag("smooth").unwrap_or(false),
        dots: flag("dots"),
        center: text(host, "center"),
        x_title: text(host, "x_title"),
        y_title: text(host, "y_title"),
        root: get("root").and_then(pick),
        payoff: get("payoff"),
        regions,
        out: Vec::new(),
        plot: [0.0, 0.0, size[0], size[1]],
        keys: Vec::new(),
        marks: Vec::new(),
        lifted: false,
        avoid: Vec::new(),
        warnings: Vec::new(),
    };
    if !orient.is_empty() && !["horizontal", "h", "vertical", "v"].contains(&orient.as_str()) {
        host.error("orient", format!("`orient` is @vertical or @horizontal, not @{orient}"), None);
    }
    if !mode.is_empty() && !["stacked", "stack", "grouped", "group"].contains(&mode.as_str()) {
        host.error("mode", format!("`mode` is @grouped or @stacked, not @{mode}"), None);
    }
    c.keys = c.sets[0].rows.iter().map(|d| d.label.clone()).collect();
    // Datasets after the first arrive at `switch` times, or `every`
    // seconds after the first has landed.
    let every = num(host, "every", 2.0);
    let extra = c.sets.len() - 1;
    match get("switch") {
        Some(times) if is_list(times) => c.switch = items_of(b, times).unwrap_or_default(),
        Some(t) => c.switch = vec![t],
        None => {}
    }
    if c.switch.len() < extra {
        if get("switch").is_some() {
            host.error("switch", format!("{extra} datasets follow the first, and `switch: […]` gives {} times", c.switch.len()), Some("one time per dataset after the first, or leave `switch` out and set `every: 2.0`"));
            return None;
        }
        let n = c.count();
        let last = c.land(n.saturating_sub(1));
        c.switch = (0..extra).map(|k| shift(b, last, every * (k + 1) as f64 - if c.sort { c.grow * 0.5 } else { 0.0 })).collect();
    }
    let result = match kind.as_str() {
        "bar" => bar::build(&mut c),
        "line" | "area" => line::build(&mut c),
        "donut" | "pie" => radial::build(&mut c),
        "scatter" | "bubble" => points::scatter(&mut c),
        "map" => points::map(&mut c),
        "network" => network::build(&mut c),
        "treemap" => treemap::build(&mut c),
        "kpi" | "counter" => kpi::build(&mut c),
        "timeline" => timeline::build(&mut c),
        "scoreboard" => board::scoreboard(&mut c),
        "versus" => board::versus(&mut c),
        "sticker" => board::stickers(&mut c),
        "meter" => meter::build(&mut c),
        "gantt" => gantt::build(&mut c),
        "axis" => axis::build(&mut c),
        "probs" => probs::build(&mut c),
        _ => Err(format!("`@{kind}` is not a chart type")),
    };
    if let Err(message) = result {
        host.error("data", message, None);
        return None;
    }
    c.callouts();
    for (field, message) in std::mem::take(&mut c.warnings) {
        host.warning(&field, message, None);
    }
    // The Group: the chart's own common fields, its box and the layers.
    let group = b.with(|vm| crate::layers::make(vm, "Group", &[]));
    for (k, v, _) in b.with(|vm| obj::fields(vm, o)) {
        if crate::layers::is_common(&k) {
            b.with(|vm| obj::set(vm, group, &k, v));
        }
    }
    if get("at").is_none() {
        // Centred in the safe area by default: the frame's middle, or in a
        // portrait frame the band the Reels UI leaves free (y 14%..65%).
        let y = if doc[1] > doc[0] * 1.2 { doc[1] * 0.14 + (doc[1] * 0.51 - c.h) * 0.5 } else { (doc[1] - c.h) * 0.5 };
        let at = v2(b, ((doc[0] - c.w) * 0.5).max(0.0), y.max(0.0));
        b.with(|vm| obj::set(vm, group, "at", at));
    }
    let size = v2(b, c.w, c.h);
    let layers = list(b, std::mem::take(&mut c.out));
    b.with(|vm| {
        obj::set(vm, group, "size", size);
        obj::set(vm, group, "layers", layers);
        let at = vm.bx.heap_ref().made_at(o);
        vm.bx.heap_mut().set_made_at(group, at);
    });
    Some(group.into())
}
