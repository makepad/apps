//! The board shaders' instance rows, written on a worker in exactly the layout the compiled
//! shaders read (captured once on the UI thread), so a batch uploads with no conversion.
//!
//! Every board shader starts, after the DrawQuad base, with this common block. Live values
//! come from the host's row textures; instances carry only static indices (-1 means none).
//!
//! | Common field | Lanes | Meaning |
//! |---|---|---|
//! | pose_a | 4 | Start x, y, CCW radians, alpha |
//! | pose_b | 4 | End x, y, CCW radians, alpha |
//! | span | 2 | Motion-clock start, duration |
//! | item | 1 | Hover id, -1 none |
//! | look | 1 | The `Look` code |
//! | ink | 4 | Straight RGBA; a = 0 takes the layer's ink uniform |
//! | live_net | 1 | Net row index, -1 none |
//! | live_seg | 1 | Segment row index, -1 none |
//! | live_part | 1 | Part row index, -1 none |
//!
//! Then the shape:
//!
//! | Shader | Shape fields |
//! |---|---|
//! | `DrawBoardStroke` | seg (ax, ay, bx, by), sag (signed sagitta, + bulges left of a->b), width, along0 |
//! | `DrawBoardPad` | rect (cx, cy, half_w, half_h), rot, corner, hole (cx, cy, half_len, radius), hole_rot |
//! | `DrawBoardTri` | tri_ab (ax, ay, bx, by), tri_c (cx, cy) |
//!
//! And last, in every shader, `net` (1 lane): the board's net index (`NetId`), -1 none, so a
//! pane lights a whole net with one uniform. It is part of `Common`; it sits after the shape so
//! the shape lanes keep their places.
//!
//! Units are millimetres in the board frame as f32 (world Y up), which keeps 60 nm of
//! precision over a metre of board.

use fab_elec_core::{Nm, P};
use makepad_widgets_core::*;

/// The instance layout of `DrawBoardStroke`, `DrawBoardPad` and `DrawBoardTri` (the fields, their
/// order and their types in the tables above). Bumped with any change to them. A sibling shader
/// that draws the same retained rows (a 3D decal pass) inherits the board shader
/// (`..mod.draw.DrawBoardStroke`, its Rust struct `#[deref]`s `DrawBoardStroke` and adds no
/// instance field), asserts this version at compile time, and compares its captured template with
/// the batch's (`StrokeTemplate: PartialEq`) before drawing, so a change breaks loudly.
/// 1: a `live: vec4` slot. 2: static row indices `live_net`, `live_seg`, `live_part`. 3: `net`.
pub const LAYOUT_VERSION: u32 = 3;

/// The board common block (module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Common {
    pub pose_a: [f32; 4],
    pub pose_b: [f32; 4],
    pub span: [f32; 2],
    pub item: f32,
    pub look: f32,
    pub ink: [f32; 4],
    pub live_net: f32,
    pub live_seg: f32,
    pub live_part: f32,
    pub net: f32,
}

impl Common {
    /// At rest at the identity pose with alpha 1, committed, the layer's ink, live off.
    pub fn rest(item: f32) -> Common {
        Common {
            pose_a: [0.0, 0.0, 0.0, 1.0],
            pose_b: [0.0, 0.0, 0.0, 1.0],
            span: [0.0, 0.0],
            item,
            look: 0.0,
            ink: [0.0; 4],
            live_net: -1.0,
            live_seg: -1.0,
            live_part: -1.0,
            net: -1.0,
        }
    }
}

/// A track segment or arc, or a graphic stroke, with round caps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeInst {
    pub c: Common,
    /// ax, ay, bx, by.
    pub seg: [f32; 4],
    /// Signed sagitta: 0 straight; > 0 an arc bulging this far to the left of a->b.
    pub sag: f32,
    pub width: f32,
    /// The world arc length at the stroke's start along its track (live flow runs on through
    /// corners).
    pub along0: f32,
}

/// A rounded box (circle, rect, oval, rounded rect) with an optional stadium hole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PadInst {
    pub c: Common,
    /// cx, cy, half_w, half_h.
    pub rect: [f32; 4],
    /// CCW radians.
    pub rot: f32,
    pub corner: f32,
    /// cx, cy, half_len, radius; radius 0 means no hole.
    pub hole: [f32; 4],
    /// The hole's axis, CCW radians.
    pub hole_rot: f32,
}

/// One filled triangle (a, b, c in board mm), no anti-aliasing of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriInst {
    pub c: Common,
    /// ax, ay, bx, by.
    pub tri_ab: [f32; 4],
    /// cx, cy.
    pub tri_c: [f32; 2],
}

/// nm to the f32 millimetres the shaders take.
pub fn mm(v: Nm) -> f32 {
    (v as f64 / 1e6) as f32
}

/// A point in shader millimetres.
pub fn pt(p: P) -> [f32; 2] {
    [mm(p.x), mm(p.y)]
}

/// A shader's default row and where each named field sits in it.
#[derive(Clone, Debug, PartialEq)]
struct Template {
    data: Vec<f32>,
    offs: Vec<usize>,
}

const COMMON: [(LiveId, usize); 9] = [
    (live_id!(pose_a), 4),
    (live_id!(pose_b), 4),
    (live_id!(span), 2),
    (live_id!(item), 1),
    (live_id!(look), 1),
    (live_id!(ink), 4),
    (live_id!(live_net), 1),
    (live_id!(live_seg), 1),
    (live_id!(live_part), 1),
];
/// After the shape fields: the board net.
const TAIL: [(LiveId, usize); 1] = [(live_id!(net), 1)];
const STROKE: [(LiveId, usize); 4] = [(live_id!(seg), 4), (live_id!(sag), 1), (live_id!(width), 1), (live_id!(along0), 1)];
const PAD: [(LiveId, usize); 5] =
    [(live_id!(rect), 4), (live_id!(rot), 1), (live_id!(corner), 1), (live_id!(hole), 4), (live_id!(hole_rot), 1)];
const TRI: [(LiveId, usize); 2] = [(live_id!(tri_ab), 4), (live_id!(tri_c), 2)];

impl Template {
    /// Reads the compiled shader's instance mapping (as Scope's arc template does). None, with
    /// a log line, when the shader is not compiled or a field is missing or not an f32 lane.
    fn capture(cx: &Cx, vars: &DrawVars, what: &str, fields: &[(LiveId, usize)]) -> Option<Template> {
        let Some(shader) = vars.draw_shader_id else {
            log!("fab.board: the {what} shader is not compiled");
            return None;
        };
        let mapping = &cx.draw_shaders[shader.index].mapping.instances;
        let data = vars.as_slice().to_vec();
        if data.len() != mapping.total_slots || mapping.stride_bytes != data.len() * 4 {
            log!("fab.board: {what} instance layout {} slots, mapping {} slots / {} bytes", data.len(), mapping.total_slots, mapping.stride_bytes);
            return None;
        }
        let mut offs = Vec::with_capacity(fields.len());
        for &(id, slots) in fields {
            let found = mapping
                .inputs
                .iter()
                .find(|i| i.id == id && i.slots == slots && i.attr_format.is_f32_lane() && i.byte_offset == i.offset * 4);
            match found {
                Some(i) => offs.push(i.offset),
                None => {
                    log!("fab.board: {what} has no f32 input {id} of {slots} slots");
                    return None;
                }
            }
        }
        Some(Template { data, offs })
    }

    /// Fields laid out back to back from 0, in order: the tests' layout (no shader).
    fn packed(fields: &[(LiveId, usize)]) -> Template {
        let mut offs = Vec::with_capacity(fields.len());
        let mut at = 0;
        for &(_, slots) in fields {
            offs.push(at);
            at += slots;
        }
        Template { data: vec![0.0; at], offs }
    }

    fn row<'a>(&self, out: &'a mut Vec<f32>) -> &'a mut [f32] {
        let start = out.len();
        out.extend_from_slice(&self.data);
        &mut out[start..]
    }
}

fn put(row: &mut [f32], at: usize, v: &[f32]) {
    row[at..at + v.len()].copy_from_slice(v);
}

fn put_common(row: &mut [f32], offs: &[usize], c: &Common) {
    put(row, offs[0], &c.pose_a);
    put(row, offs[1], &c.pose_b);
    put(row, offs[2], &c.span);
    row[offs[3]] = c.item;
    row[offs[4]] = c.look;
    put(row, offs[5], &c.ink);
    row[offs[6]] = c.live_net;
    row[offs[7]] = c.live_seg;
    row[offs[8]] = c.live_part;
    row[offs[offs.len() - 1]] = c.net;
}

fn fields(shape: &[(LiveId, usize)]) -> Vec<(LiveId, usize)> {
    COMMON.iter().chain(shape.iter()).chain(TAIL.iter()).copied().collect()
}

/// `DrawBoardStroke`'s row layout. Plain data: clone it to workers.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeTemplate(Template);

impl StrokeTemplate {
    pub fn capture(cx: &Cx, vars: &DrawVars) -> Option<Self> {
        Template::capture(cx, vars, "stroke", &fields(&STROKE)).map(Self)
    }
    /// Fields packed from offset 0 in declaration order, for tests (no shader).
    pub fn packed() -> Self {
        Self(Template::packed(&fields(&STROKE)))
    }
    pub fn slots(&self) -> usize {
        self.0.data.len()
    }
    pub fn append(&self, out: &mut Vec<f32>, i: &StrokeInst) {
        let offs = &self.0.offs;
        let row = self.0.row(out);
        put_common(row, offs, &i.c);
        put(row, offs[9], &i.seg);
        row[offs[10]] = i.sag;
        row[offs[11]] = i.width;
        row[offs[12]] = i.along0;
    }
}

/// `DrawBoardPad`'s row layout. Plain data: clone it to workers.
#[derive(Clone, Debug, PartialEq)]
pub struct PadTemplate(Template);

impl PadTemplate {
    pub fn capture(cx: &Cx, vars: &DrawVars) -> Option<Self> {
        Template::capture(cx, vars, "pad", &fields(&PAD)).map(Self)
    }
    /// Fields packed from offset 0 in declaration order, for tests (no shader).
    pub fn packed() -> Self {
        Self(Template::packed(&fields(&PAD)))
    }
    pub fn slots(&self) -> usize {
        self.0.data.len()
    }
    pub fn append(&self, out: &mut Vec<f32>, i: &PadInst) {
        let offs = &self.0.offs;
        let row = self.0.row(out);
        put_common(row, offs, &i.c);
        put(row, offs[9], &i.rect);
        row[offs[10]] = i.rot;
        row[offs[11]] = i.corner;
        put(row, offs[12], &i.hole);
        row[offs[13]] = i.hole_rot;
    }
}

/// `DrawBoardTri`'s row layout. Plain data: clone it to workers.
#[derive(Clone, Debug, PartialEq)]
pub struct TriTemplate(Template);

impl TriTemplate {
    pub fn capture(cx: &Cx, vars: &DrawVars) -> Option<Self> {
        Template::capture(cx, vars, "tri", &fields(&TRI)).map(Self)
    }
    /// Fields packed from offset 0 in declaration order, for tests (no shader).
    pub fn packed() -> Self {
        Self(Template::packed(&fields(&TRI)))
    }
    pub fn slots(&self) -> usize {
        self.0.data.len()
    }
    pub fn append(&self, out: &mut Vec<f32>, i: &TriInst) {
        let offs = &self.0.offs;
        let row = self.0.row(out);
        put_common(row, offs, &i.c);
        put(row, offs[9], &i.tri_ab);
        put(row, offs[10], &i.tri_c);
    }
}
