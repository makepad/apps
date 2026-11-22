//! The imported part's Splash file, in the shape agreed with the part language: one `e.define` named
//! `"<supplier>:<number>"` with the std footprint helpers (`f.smd`, `f.thru`, `f.footprint`,
//! `f.ic_symbol` or `f.two_terminal`), the pin-to-pad map, the body by its `.glb`, and string fields
//! that say where the part came from and which converter read it.
//! The file calls its guarded factory so loading it registers the name for `e.part("<supplier>:<number>", {})`.
//!
//! A converter fills a [`PartOut`]; [`write_part`] turns it into text. The same `PartOut` gives the same
//! bytes: numbers are written to 0.1 µm (lengths) or 0.001° (angles) with trailing zeros dropped, and
//! everything is in the order the converter gave it. Nothing here reads a clock or the environment.

use std::fmt::Write;

/// Where a pin is drawn on an IC box symbol.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// A symbol pin: its name (the pad map's key) and its kind id (`in`, `passive`, `power_in`, ..).
#[derive(Clone, PartialEq, Debug)]
pub struct PinOut {
    pub name: String,
    pub kind: &'static str,
    pub side: Side,
}

#[derive(Clone, PartialEq, Debug)]
pub enum SymbolOut {
    /// The standard two-terminal symbol (passives); the pins' sides are ignored.
    TwoTerminal,
    /// The standard IC box, pins on the sides they name.
    Box,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PadShapeOut {
    Rect,
    RoundRect,
    Oval,
    Circle,
}

impl PadShapeOut {
    fn id(self) -> &'static str {
        match self {
            Self::Rect => "rect",
            Self::RoundRect => "roundrect",
            Self::Oval => "oval",
            Self::Circle => "circle",
        }
    }
}

/// A length, or a `[w, h]` pair (a slot, an oval land).
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Size {
    Round(f64),
    Wh(f64, f64),
}

#[derive(Clone, PartialEq, Debug)]
pub enum PadKindOut {
    /// A surface land of `size` (mm).
    Smd { size: [f64; 2] },
    /// A hole: the land `d` and the `drill`; `square` makes a `[w, h]` land a rectangle.
    Thru { d: Size, drill: Size, square: bool, plated: bool },
}

/// One pad, mm and degrees, in the footprint's frame (Y up, origin at the footprint's origin).
#[derive(Clone, PartialEq, Debug)]
pub struct PadOut {
    pub number: String,
    pub at: [f64; 2],
    pub kind: PadKindOut,
    /// The land's shape (an SMD pad's; `None` keeps the helper's default).
    pub shape: Option<PadShapeOut>,
    pub rot: f64,
    /// Corner radius over the short side, for `RoundRect` only (0..0.5).
    pub ratio: Option<f64>,
    /// A custom land (at least three points, a simple polygon), anchored by `shape`.
    pub outline: Vec<[f64; 2]>,
}

/// A drawn line on a footprint layer (`"F.SilkS"`, `"F.Fab"`, ..), mm.
#[derive(Clone, PartialEq, Debug)]
pub struct StrokeOut {
    pub layer: String,
    pub points: Vec<[f64; 2]>,
    pub w: f64,
}

/// The body: its mesh file (beside the part file), already placed in the body frame, and its height.
#[derive(Clone, PartialEq, Debug)]
pub struct BodyOut {
    pub mesh: String,
    pub height: f64,
    /// Where the mesh sits in the footprint (mm), and its turn `[rx, ry, rz]` in degrees, applied
    /// as `Rz * Ry * Rx`: zero for a mesh already placed in the body frame.
    pub offset: [f64; 3],
    pub rotation: [f64; 3],
}

#[derive(Clone, PartialEq, Debug)]
pub struct PartOut {
    /// The supplier label (the library folder): `"example"`.
    pub supplier: String,
    /// The supplier's part number: `"C123456"`.
    pub number: String,
    /// A part kind id (`resistor`, `ic`, ..).
    pub kind: String,
    pub prefix: String,
    pub symbol: SymbolOut,
    pub pins: Vec<PinOut>,
    pub pads: Vec<PadOut>,
    /// The footprint's body box `[[x0, y0], [x1, y1]]`, mm.
    pub courtyard: [[f64; 2]; 2],
    pub thru: bool,
    pub graphics: Vec<StrokeOut>,
    pub body: Option<BodyOut>,
    /// Each pin's pads, in pin order.
    pub pad_map: Vec<(String, Vec<String>)>,
    /// String fields in the order written (`supplier`, `supplier_part`, `mpn`, ..).
    pub fields: Vec<(String, String)>,
    /// One comment line said above the part (what was left out or corrected).
    pub note: String,
}

impl PartOut {
    /// The part's name in the library: `"<supplier>:<number>"`.
    pub fn name(&self) -> String {
        format!("{}:{}", self.supplier, self.number)
    }

    /// The Splash identifier the part's fn is bound to: lower case, anything but letters and digits as `_`.
    pub fn binding(&self) -> String {
        let mut id: String = format!("{}_{}", self.supplier, self.number)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
            .collect();
        if id.starts_with(|c: char| c.is_ascii_digit()) {
            id.insert(0, '_');
        }
        id
    }
}

/// The Splash text of a part file that registers its definition when evaluated; its factory remains reusable.
pub fn write_part(p: &PartOut) -> String {
    let (name, id) = (quote(&p.name()), p.binding());
    let mut s = String::new();
    let _ = writeln!(s, "let __{id}_e = mod.elec");
    let _ = writeln!(s, "let __{id}_f = mod.elec.f");
    for line in p.note.lines() {
        let _ = writeln!(s, "// {line}");
    }
    let _ = writeln!(s, "let {id} = fn() {{");
    let _ = writeln!(s, "    let e = __{id}_e");
    let _ = writeln!(s, "    let f = __{id}_f");
    let _ = writeln!(s, "    let existing = e.lookup({name})");
    let _ = writeln!(s, "    if existing != nil {{ return existing }}");
    let _ = writeln!(s, "    return e.define({name}, {{");
    let _ = writeln!(s, "        kind: @{}", p.kind);
    let _ = writeln!(s, "        symbol: {}", symbol(p));
    let _ = writeln!(s, "        footprint: f.footprint({{");
    let _ = writeln!(s, "            pads: [");
    for pad in &p.pads {
        let _ = writeln!(s, "                {}", pad_call(pad));
    }
    let _ = writeln!(s, "            ]");
    let [a, b] = p.courtyard;
    let _ = writeln!(s, "            body: [{}, {}]", point(a), point(b));
    let _ = writeln!(s, "            mount: @{}", if p.thru { "thru" } else { "smd" });
    if !p.graphics.is_empty() {
        let _ = writeln!(s, "            graphics: [");
        for g in &p.graphics {
            let points: Vec<String> = g.points.iter().map(|q| point(*q)).collect();
            let _ = writeln!(s, "                f.stroke({}, [{}], {})", quote(&g.layer), points.join(", "), mm(g.w));
        }
        let _ = writeln!(s, "            ]");
    }
    let _ = writeln!(s, "        }})");
    if let Some(body) = &p.body {
        let _ = writeln!(
            s,
            "        body: {{mesh: {} offset: [{}, {}, {}] rotation: [{}, {}, {}] height: {}}}",
            quote(&body.mesh),
            mm(body.offset[0]), mm(body.offset[1]), mm(body.offset[2]),
            deg(body.rotation[0]), deg(body.rotation[1]), deg(body.rotation[2]),
            mm(body.height)
        );
    }
    let map: Vec<String> = p
        .pad_map
        .iter()
        .map(|(pin, pads)| match pads.as_slice() {
            [one] => format!("[{}, {}]", quote(pin), quote(one)),
            many => format!("[{}, [{}]]", quote(pin), many.iter().map(|n| quote(n)).collect::<Vec<_>>().join(", ")),
        })
        .collect();
    let _ = writeln!(s, "        pads: [{}]", map.join(", "));
    let _ = writeln!(s, "        prefix: {}", quote(&p.prefix));
    let fields: Vec<String> = p.fields.iter().map(|(k, v)| format!("{k}: {}", quote(v))).collect();
    let _ = writeln!(s, "        fields: {{{}}}", fields.join(" "));
    let _ = writeln!(s, "    }})");
    let _ = writeln!(s, "}}");
    let _ = writeln!(s, "{id}()");
    s
}

fn symbol(p: &PartOut) -> String {
    let pin = |q: &PinOut| format!("[{}, @{}]", quote(&q.name), q.kind);
    match p.symbol {
        SymbolOut::TwoTerminal => {
            format!("f.two_terminal([{}])", p.pins.iter().map(pin).collect::<Vec<_>>().join(", "))
        }
        SymbolOut::Box => {
            let mut sides = Vec::new();
            for (side, key) in [(Side::Left, "left"), (Side::Right, "right"), (Side::Top, "top"), (Side::Bottom, "bottom")] {
                let on: Vec<String> = p.pins.iter().filter(|q| q.side == side).map(pin).collect();
                if !on.is_empty() {
                    sides.push(format!("{key}: [{}]", on.join(", ")));
                }
            }
            format!("f.ic_symbol({}, {{{}}})", quote(&p.name()), sides.join(" "))
        }
    }
}

fn pad_call(pad: &PadOut) -> String {
    let mut opts = Vec::new();
    if let Some(shape) = pad.shape {
        opts.push(format!("shape: @{}", shape.id()));
    }
    opts.push(format!("rot: {}", deg(pad.rot)));
    if let Some(r) = pad.ratio {
        opts.push(format!("ratio: {}", num(r, 1e6)));
    }
    if !pad.outline.is_empty() {
        opts.push(format!("outline: [{}]", pad.outline.iter().map(|q| point(*q)).collect::<Vec<_>>().join(", ")));
    }
    let (n, at) = (quote(&pad.number), point(pad.at));
    match &pad.kind {
        PadKindOut::Smd { size } => format!("f.smd({n}, {at}, {}, {{{}}}),", point(*size), opts.join(" ")),
        PadKindOut::Thru { d, drill, square, plated } => {
            if *square {
                opts.push("square: true".into());
            }
            if !plated {
                opts.push("plated: false".into());
            }
            format!("f.thru({n}, {at}, {}, {}, {{{}}}),", size(*d), size(*drill), opts.join(" "))
        }
    }
}

fn size(s: Size) -> String {
    match s {
        Size::Round(d) => mm(d),
        Size::Wh(w, h) => point([w, h]),
    }
}

fn point(p: [f64; 2]) -> String {
    format!("[{}, {}]", mm(p[0]), mm(p[1]))
}

/// A length in mm to 0.1 µm.
fn mm(v: f64) -> String {
    format!("{}mm", num(v, 1e4))
}

/// An angle in degrees to 0.001°.
fn deg(v: f64) -> String {
    format!("{}deg", num(v, 1e3))
}

/// `v` rounded to 1/scale, trailing zeros dropped; never `-0`.
fn num(v: f64, scale: f64) -> String {
    let r = (v * scale).round() / scale;
    let r = if r == 0.0 { 0.0 } else { r };
    let digits = scale.log10().round() as usize;
    let text = format!("{r:.digits$}");
    let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.') } else { &text };
    text.to_string()
}

/// A Splash string literal.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
