//! The Library's monochrome schematic symbols and section marks.

use super::Section;
use crate::explorer::glyphs::{family_prims, symbol_prims, Family, Fill, Prim, SYMBOL_W};
use fab_elec_parts::{PartDef, PartKind};
use makepad_widgets_core::*;
use std::{f32::consts::{FRAC_PI_2, PI, TAU}, fmt::Write};

/// The same one-pin marker and standard two-pin symbols as on the sheet.
fn family(part: &PartDef) -> Option<Family> {
    let family = match part.kind {
        PartKind::Resistor | PartKind::Fuse => Family::Resistor,
        PartKind::Capacitor => Family::Capacitor,
        PartKind::Inductor => Family::Inductor,
        PartKind::Diode => Family::Diode,
        PartKind::Led => Family::Led,
        PartKind::Switch => Family::Switch,
        PartKind::Crystal => Family::Crystal,
        PartKind::PowerFlag => return (part.pins.len() == 1).then_some(Family::Flag),
        _ => return None,
    };
    if part.pins.len() != 2 { return None }
    let (_, ports) = fab_elec_explore::adapt::symbol_geometry(part, false);
    (ports[0].at == [-2, 0] && ports[1].at == [2, 0]).then_some(family)
}

fn on_arc(c: [f32; 2], r: f32, angle: f32) -> [f32; 2] {
    [c[0] + r * angle.cos(), c[1] + r * angle.sin()]
}

/// The unpressed symbol's extents on the sheet, including extrema inside an arc's sweep.
fn bounds(prims: &[Prim]) -> [f32; 4] {
    let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    let mut include = |[x, y]: [f32; 2]| {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    };
    for &prim in prims {
        match prim {
            Prim::Line { a, b, .. } => { include(a); include(b); }
            Prim::Box { c: [x, y], half: [w, h], .. } => {
                include([x - w, y - h]);
                include([x + w, y + h]);
            }
            Prim::Arc { c, r, start, sweep, .. } => {
                include(on_arc(c, r, start));
                include(on_arc(c, r, start + sweep));
                for i in 0..4 {
                    let angle = i as f32 * FRAC_PI_2;
                    if ((angle - start) * sweep.signum()).rem_euclid(TAU) <= sweep.abs() {
                        include(on_arc(c, r, angle));
                    }
                }
            }
        }
    }
    b
}

/// The first symbol unit as a tintable SVG. Linear in the unit's graphics and pins.
pub fn symbol_svg(part: &PartDef) -> Option<String> {
    let unit = part.symbol.units.first()?;
    let prims = match family(part) {
        Some(family) => family_prims(family),
        None => symbol_prims(unit, |_| Default::default()).0,
    };
    if prims.is_empty() { return None }
    let [left, bottom, right, top] = bounds(&prims);
    // Stroke widths on the sheet are in points. At row size one nominal stroke is about
    // one point; a larger preview scales the whole drawing with its walk.
    let point = ((right - left) / 18.0).max((top - bottom) / 12.0).max(f32::EPSILON);
    let stroke = |w| w * point / SYMBOL_W;
    let margin = 1.5 * point;
    let mut svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}">"#,
        left - margin, -top - margin, right - left + 2.0 * margin, top - bottom + 2.0 * margin);
    let fill = |f| if f == Fill::Ink { "#fff" } else { "none" };
    for prim in prims {
        match prim {
            Prim::Line { a, b, w, butt, .. } => {
                let cap = if butt { "butt" } else { "round" };
                write!(svg, r##"<path d="M{} {}L{} {}" fill="none" stroke="#fff" stroke-width="{}" stroke-linecap="{cap}"/>"##,
                    a[0], -a[1], b[0], -b[1], stroke(w)).unwrap();
            }
            Prim::Box { c, half, corner, w, fill: f } => {
                write!(svg, r##"<rect x="{}" y="{}" width="{}" height="{}" rx="{corner}" fill="{}" stroke="#fff" stroke-width="{}"/>"##,
                    c[0] - half[0], -c[1] - half[1], 2.0 * half[0], 2.0 * half[1], fill(f), stroke(w)).unwrap();
            }
            Prim::Arc { c, r, sweep, w, fill: f, .. } if sweep.abs() >= TAU => {
                write!(svg, r##"<circle cx="{}" cy="{}" r="{r}" fill="{}" stroke="#fff" stroke-width="{}"/>"##,
                    c[0], -c[1], fill(f), stroke(w)).unwrap();
            }
            Prim::Arc { c, r, start, sweep, w, .. } => {
                let a = on_arc(c, r, start);
                let b = on_arc(c, r, start + sweep);
                write!(svg, r##"<path d="M{} {}A{r} {r} 0 {} {} {} {}" fill="none" stroke="#fff" stroke-width="{}" stroke-linecap="round"/>"##,
                    a[0], -a[1], u8::from(sweep.abs() > PI), u8::from(sweep < 0.0), b[0], -b[1], stroke(w)).unwrap();
            }
        }
    }
    svg.push_str("</svg>");
    Some(svg)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mark {
    Section(Section),
    Star { pinned: bool },
}

impl Mark {
    fn svg(self) -> String {
        let path = match self {
            Self::Section(Section::Pinned) => "M6 2h6l-1 2v3l2 2v1H4V9l2-2V4ZM8.5 10v4",
            Self::Section(Section::Recent) => "M14 8a6 6 0 1 1-12 0 6 6 0 1 1 12 0ZM8 4.5V8l3 1.5",
            Self::Section(Section::Board) => "M2 3h12v10H2ZM4 6h3l2 3h3M4 10h2",
            Self::Section(Section::Passives) => "M1 8h2l2-4 2 8 2-8 2 8 2-4h2",
            Self::Section(Section::Parts) => "M4 4h8v8H4ZM6 2v2m4-2v2M6 12v2m4-2v2M2 6h2m-2 4h2m8-4h2m-2 4h2",
            Self::Section(Section::Blocks) => "M3 3h10v10H3ZM1 6h2m-2 4h2m10-2h2",
            Self::Star { .. } => "M8 1.5 L10 5.6 14.5 6.3 11.25 9.5 12 14 8 11.9 4 14 4.75 9.5 1.5 6.3 6 5.6 Z",
        };
        let fill = if matches!(self, Self::Star { pinned: true }) { "#fff" } else { "none" };
        format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="{path}" fill="{fill}" stroke="#fff" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>"##)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageKey {
    Part(u64),
    Mark(Mark),
}

/// One retained SVG fitted to its walk, from a Library row to a larger preview.
#[derive(Script, ScriptHook, Widget)]
pub struct SymbolIcon {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[rust]
    area: Area,
    #[live]
    draw_svg: DrawSvg,
    #[rust]
    key: Option<ImageKey>,
    #[rust]
    hidden_ink: bool,
}

impl SymbolIcon {
    /// Binds a catalog part before drawing. An unchanged content hash costs one comparison.
    pub fn set_part(&mut self, part: &PartDef, hash: u64) {
        if self.key != Some(ImageKey::Part(hash)) {
            self.load(ImageKey::Part(hash), symbol_svg(part).as_deref());
        }
    }

    pub(super) fn set_mark(&mut self, mark: Mark, shown: bool) {
        if self.key != Some(ImageKey::Mark(mark)) {
            self.load(ImageKey::Mark(mark), Some(&mark.svg()));
        }
        self.hidden_ink = !shown;
    }

    fn load(&mut self, key: ImageKey, svg: Option<&str>) {
        self.key = Some(key);
        self.hidden_ink = false;
        if let Some(svg) = svg {
            self.draw_svg.load_from_str(svg);
            // DrawSvg normally fits just the geometry. Keep the viewBox padding for stroke caps.
            if let Some(doc) = &self.draw_svg.svg_doc {
                let (w, h) = doc.logical_size();
                self.draw_svg.content_bounds = (0.0, 0.0, w, h);
                self.draw_svg.content_size = dvec2(w as f64, h as f64);
            }
        } else {
            self.draw_svg.svg_doc = None;
        }
    }
}

impl Widget for SymbolIcon {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        if !self.hidden_ink {
            self.draw_svg.draw_abs(cx, rect);
        }
        DrawStep::done()
    }
}
