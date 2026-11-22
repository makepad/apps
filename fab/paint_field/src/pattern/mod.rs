//! Colour patterns a simulation grows on the part itself, as a [`crate::Mix`]'s field.
//!
//! - [`Spec`]: what a script asks for (`Turing{preset: @spots scale: 5mm seed: 1 steps: 3000}`,
//!   `Lichtenberg{preset: @tree scale: 6mm}`): a model and its preset, the pattern's scale (the
//!   distance from one spot, stripe or branch to the next), its seed and how many steps it
//!   grows.
//! - [`band`]: the cells it grows in, a thin band round the part's surface on a lattice picked
//!   from the scale (not the print's resolution), so spots and stripes follow the skin. A depth
//!   past that grows it through the part.
//! - [`rd`]: the models stepped over the band (Gray-Scott; an activator and its inhibitor),
//!   on a coarse lattice first so the pattern forms at once, then on the fine one; and
//!   [`dla`], walkers that grow a Lichtenberg figure's tree over it; [`scales`], the skin
//!   tiled round seed points (a sunflower's spiral, or thrown at random); [`physarum`],
//!   agents that crawl over it laying a trail and following it into a slime mould's network;
//!   and [`flow`], lines drawn evenly along a swirling flow over it. Both go over the skin as a
//!   surface ([`skin`]).
//! - [`live`]: the simulations running in the process, one per (part shape, spec but its
//!   steps): a step count raised carries on, lowered replays, and a finished state is kept for
//!   every sampler. Views follow a running one at display rate; the print takes the state at
//!   exactly `steps`.
//! - [`Grid`]: a state sampled by position (model millimetres), what the views and the slicer
//!   read: its place in the mix's materials, 0..1.

pub mod band;
pub mod dla;
pub mod flow;
pub mod live;
pub mod physarum;
pub mod rd;
pub mod scales;
pub mod skin;
pub mod veins;

use crate::program::P3;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

/// A reaction-diffusion model and its rates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Model {
    /// Gray-Scott: `u + 2v -> 3v`, `u` fed at `feed` and `v` removed at `feed + kill`. Spots,
    /// stripes, mazes and dividing cells by the two rates.
    GrayScott { feed: f64, kill: f64 },
    /// An activator that makes itself and its inhibitor, which spreads faster
    /// (Gierer-Meinhardt): spots at the wavelength the two spreads set. Its production
    /// saturates at `saturation` (0: none), which rounds the spots off and, past about 0.1,
    /// holds the even state.
    Activator { saturation: f64 },
    /// Diffusion-limited aggregation: a Lichtenberg figure, a tree grown from `roots` points by
    /// walkers that hit a cell beside it with the chance `stick`, the cell joining at its `hits`th
    /// hit ([`dla`]): more hits, fewer and straighter branches; less sticking, denser ones.
    Aggregation { stick: f64, hits: u32, roots: u32 },
    /// Scales: the skin tiled by the seeds nearest it, the seeds set as a sunflower's
    /// florets round the upright axis (`spiral`) or at random; each outlined `edge` times the
    /// scale wide, `fill` of them through ([`scales`]).
    Scales { spiral: bool, edge: f64, fill: f64 },
    /// A slime mould: agents that read the trail ahead and `sense` degrees either way, turn
    /// `turn` degrees toward the strongest, step and lay trail ([`physarum`]).
    Physarum { sense: f64, turn: f64, spread: f64 },
    /// Flow lines: lines a scale apart along a flow over the skin, the curl of a noise `swirl`
    /// scales across (or round its `contour`s), each `width` of the scale wide, at most `length`
    /// scales long, `taper` of its width narrowing to its ends ([`flow`]).
    Flow { contour: bool, length: f64, width: f64, swirl: f64, taper: f64 },
}

/// A named starting point: its model and how it is stepped (a Turing pattern's calibrated by
/// `the_scale_is_the_measured_wavelength`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    /// The script's name for its family (`Turing`, `Lichtenberg`, `Scales`, `Physarum`, `Flow`).
    pub kind: &'static str,
    pub name: &'static str,
    pub model: Model,
    /// The first species' diffusion on the fine lattice (cells² per step), the second's
    /// `ratio` times it.
    pub du: f64,
    pub ratio: f64,
    /// The step (the model's own time).
    pub dt: f64,
    /// The pattern's wavelength on the fine lattice at `du` (cells): the lattice's pitch is
    /// the scale over it.
    pub cells: f64,
    /// Steps on the coarse lattice (twice the pitch) before the fine one takes over.
    pub coarse: u32,
    /// The steps a script that gives none grows.
    pub steps: u32,
    /// The second species' level that reads as the second material (0..1 on a place), and
    /// the first's. Aggregation, scales, a slime mould and flow lines: 0 and 1 (their places are
    /// their own, [`dla::places`], [`scales::places`], [`physarum::places`], [`flow::places`]).
    pub lo: f64,
    pub hi: f64,
}

pub const PRESETS: &[Preset] = &[
    Preset { kind: "Turing", name: "spots", model: Model::GrayScott { feed: 0.03, kill: 0.062 }, du: 0.14, ratio: 0.5, dt: 1.0, cells: 11.0, coarse: 1200, steps: 3000, lo: 0.05, hi: 0.3 },
    Preset { kind: "Turing", name: "stripes", model: Model::GrayScott { feed: 0.05, kill: 0.063 }, du: 0.125, ratio: 0.5, dt: 1.0, cells: 11.0, coarse: 1200, steps: 3000, lo: 0.05, hi: 0.3 },
    Preset { kind: "Turing", name: "labyrinth", model: Model::GrayScott { feed: 0.029, kill: 0.057 }, du: 0.122, ratio: 0.5, dt: 1.0, cells: 11.0, coarse: 1200, steps: 3000, lo: 0.05, hi: 0.3 },
    Preset { kind: "Turing", name: "coral", model: Model::GrayScott { feed: 0.0545, kill: 0.062 }, du: 0.123, ratio: 0.5, dt: 1.0, cells: 11.0, coarse: 1200, steps: 3000, lo: 0.05, hi: 0.3 },
    Preset { kind: "Turing", name: "mitosis", model: Model::GrayScott { feed: 0.0367, kill: 0.0649 }, du: 0.103, ratio: 0.5, dt: 1.0, cells: 11.0, coarse: 1200, steps: 3000, lo: 0.05, hi: 0.3 },
    Preset { kind: "Turing", name: "dots", model: Model::Activator { saturation: 0.0 }, du: 0.96, ratio: 20.0, dt: 0.0065, cells: 11.0, coarse: 900, steps: 2000, lo: 1.0, hi: 3.0 },
    Preset { kind: "Lichtenberg", name: "tree", model: Model::Aggregation { stick: 1.0, hits: 1, roots: 1 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 8.0, coarse: 0, steps: 1200, lo: 0.0, hi: 1.0 },
    Preset { kind: "Lichtenberg", name: "circuit", model: Model::Aggregation { stick: 1.0, hits: 4, roots: 1 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 8.0, coarse: 0, steps: 3000, lo: 0.0, hi: 1.0 },
    Preset { kind: "Lichtenberg", name: "frost", model: Model::Aggregation { stick: 0.15, hits: 1, roots: 1 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 8.0, coarse: 0, steps: 1200, lo: 0.0, hi: 1.0 },
    Preset { kind: "Scales", name: "spiral", model: Model::Scales { spiral: true, edge: 0.15, fill: 0.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 12.0, coarse: 0, steps: 60, lo: 0.0, hi: 1.0 },
    Preset { kind: "Scales", name: "even", model: Model::Scales { spiral: false, edge: 0.15, fill: 0.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 12.0, coarse: 0, steps: 60, lo: 0.0, hi: 1.0 },
    Preset { kind: "Physarum", name: "network", model: Model::Physarum { sense: 22.5, turn: 45.0, spread: 1.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 20.0, coarse: 0, steps: 800, lo: 0.0, hi: 1.0 },
    Preset { kind: "Physarum", name: "trunks", model: Model::Physarum { sense: 22.5, turn: 45.0, spread: 2.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 20.0, coarse: 0, steps: 800, lo: 0.0, hi: 1.0 },
    Preset { kind: "Physarum", name: "lace", model: Model::Physarum { sense: 22.5, turn: 45.0, spread: 0.35 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 20.0, coarse: 0, steps: 800, lo: 0.0, hi: 1.0 },
    Preset { kind: "Flow", name: "streaks", model: Model::Flow { contour: false, length: 4.0, width: 0.45, swirl: 5.0, taper: 1.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 8.0, coarse: 0, steps: 1000, lo: 0.0, hi: 1.0 },
    Preset { kind: "Flow", name: "lines", model: Model::Flow { contour: false, length: 60.0, width: 0.35, swirl: 6.0, taper: 0.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 8.0, coarse: 0, steps: 1000, lo: 0.0, hi: 1.0 },
    Preset { kind: "Flow", name: "contours", model: Model::Flow { contour: true, length: 60.0, width: 0.35, swirl: 6.0, taper: 0.0 }, du: 0.0, ratio: 0.0, dt: 1.0, cells: 8.0, coarse: 0, steps: 1000, lo: 0.0, hi: 1.0 },
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// The presets' names, for messages and the panel.
pub fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.name).collect()
}

/// The names of one family's presets (`Turing`, `Lichtenberg`, `Scales`, `Physarum`, `Flow`).
pub fn preset_names_of(kind: &str) -> Vec<&'static str> {
    PRESETS.iter().filter(|p| p.kind == kind).map(|p| p.name).collect()
}

/// What a pattern is: its preset (model and stepping), the model's rates, its scale, seed,
/// steps and depth.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub preset: &'static Preset,
    pub model: Model,
    /// From one spot, stripe, branch, scale, mesh of a network or flow line to the next (mm).
    pub scale: f64,
    pub seed: u32,
    /// The steps it grows: the print takes the state at exactly this many.
    pub steps: u32,
    /// How deep it grows in from the surface (mm): 0 on the skin; deeper, it grows through
    /// that depth (spots as balls, stripes as sheets).
    pub depth: f64,
}

/// The smallest and largest scale a pattern takes (mm).
pub const SCALE: (f64, f64) = (0.8, 200.0);
/// The most steps a pattern grows.
pub const MAX_STEPS: u32 = 200_000;
/// The most points a Lichtenberg figure grows from.
pub const MAX_ROOTS: u32 = 64;
/// The most hits a cell of a Lichtenberg figure takes to join it.
pub const MAX_HITS: u32 = 16;
/// The longest a flow line grows (scales).
pub const MAX_LENGTH: f64 = 200.0;

impl Spec {
    /// A spec from a script's `Turing{..}`, `Lichtenberg{..}`, `Scales{..}`, `Physarum{..}` or
    /// `Flow{..}`: the preset (`spots` when none), then any of its numbers (`scale` mm, `seed`,
    /// `steps`, `depth` mm, and the model's own: `feed`, `kill`, `saturation`, `stick`, `hits`,
    /// `roots`, `edge`, `fill`, `sense` and `turn` degrees, `length`, `width`, `swirl` and
    /// `taper`).
    pub fn new(preset_name: Option<&str>, values: &[(&str, f64)]) -> Result<Spec, String> {
        let name = preset_name.unwrap_or("spots");
        let preset = preset(name).ok_or_else(|| format!("no preset @{name}: one of {}", preset_names().iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(", ")))?;
        let mut s = Spec { preset, model: preset.model, scale: 5.0, seed: 1, steps: preset.steps, depth: 0.0 };
        for &(key, v) in values {
            if !v.is_finite() {
                return Err(format!("'{key}' is not a number"));
            }
            match (key, &mut s.model) {
                ("scale", _) if (SCALE.0..=SCALE.1).contains(&v) => s.scale = v,
                ("scale", _) => return Err(format!("'scale' is {}mm to {}mm, not {v}mm", SCALE.0, SCALE.1)),
                ("seed", _) if v >= 0.0 && v.fract() == 0.0 && v <= u32::MAX as f64 => s.seed = v as u32,
                ("seed", _) => return Err(format!("'seed' is a whole number, not {v}")),
                ("steps", _) if v >= 0.0 && v.fract() == 0.0 && v <= MAX_STEPS as f64 => s.steps = v as u32,
                ("steps", _) => return Err(format!("'steps' is a whole number up to {MAX_STEPS}, not {v}")),
                ("depth", _) if v >= 0.0 => s.depth = v,
                ("depth", _) => return Err(format!("'depth' is a length of 0 or more, not {v}mm")),
                ("feed", Model::GrayScott { feed, .. }) if (0.0..=0.12).contains(&v) => *feed = v,
                ("kill", Model::GrayScott { kill, .. }) if (0.0..=0.08).contains(&v) => *kill = v,
                ("feed", Model::GrayScott { .. }) => return Err(format!("'feed' is 0 to 0.12, not {v}")),
                ("kill", Model::GrayScott { .. }) => return Err(format!("'kill' is 0 to 0.08, not {v}")),
                ("saturation", Model::Activator { saturation }) if (0.0..=2.0).contains(&v) => *saturation = v,
                ("saturation", Model::Activator { .. }) => return Err(format!("'saturation' is 0 to 2, not {v}")),
                ("stick", Model::Aggregation { stick, .. }) if v > 0.0 && v <= 1.0 => *stick = v,
                ("stick", Model::Aggregation { .. }) => return Err(format!("'stick' is over 0 and up to 1, not {v}")),
                ("hits", Model::Aggregation { hits, .. }) if (1.0..=MAX_HITS as f64).contains(&v) && v.fract() == 0.0 => *hits = v as u32,
                ("hits", Model::Aggregation { .. }) => return Err(format!("'hits' is a whole number 1 to {MAX_HITS}, not {v}")),
                ("roots", Model::Aggregation { roots, .. }) if (1.0..=MAX_ROOTS as f64).contains(&v) && v.fract() == 0.0 => *roots = v as u32,
                ("roots", Model::Aggregation { .. }) => return Err(format!("'roots' is a whole number 1 to {MAX_ROOTS}, not {v}")),
                ("edge", Model::Scales { edge, .. }) if (0.0..=0.5).contains(&v) => *edge = v,
                ("edge", Model::Scales { .. }) => return Err(format!("'edge' is 0 to 0.5 of the scale, not {v}")),
                ("fill", Model::Scales { fill, .. }) if (0.0..=1.0).contains(&v) => *fill = v,
                ("fill", Model::Scales { .. }) => return Err(format!("'fill' is 0 to 1, not {v}")),
                ("sense", Model::Physarum { sense, .. }) if (5.0..=90.0).contains(&v) => *sense = v,
                ("sense", Model::Physarum { .. }) => return Err(format!("'sense' is 5 to 90 degrees, not {v}")),
                ("turn", Model::Physarum { turn, .. }) if (5.0..=90.0).contains(&v) => *turn = v,
                ("turn", Model::Physarum { .. }) => return Err(format!("'turn' is 5 to 90 degrees, not {v}")),
                ("spread", Model::Physarum { spread, .. }) if (0.1..=4.0).contains(&v) => *spread = v,
                ("spread", Model::Physarum { .. }) => return Err(format!("'spread' is 0.1 to 4, not {v}")),
                ("length", Model::Flow { length, .. }) if (1.0..=MAX_LENGTH).contains(&v) => *length = v,
                ("length", Model::Flow { .. }) => return Err(format!("'length' is 1 to {MAX_LENGTH} scales, not {v}")),
                ("width", Model::Flow { width, .. }) if (0.05..=0.5).contains(&v) => *width = v,
                ("width", Model::Flow { .. }) => return Err(format!("'width' is 0.05 to 0.5 of the scale, not {v}")),
                ("swirl", Model::Flow { swirl, .. }) if (1.0..=50.0).contains(&v) => *swirl = v,
                ("swirl", Model::Flow { .. }) => return Err(format!("'swirl' is 1 to 50 scales, not {v}")),
                ("taper", Model::Flow { taper, .. }) if (0.0..=1.0).contains(&v) => *taper = v,
                ("taper", Model::Flow { .. }) => return Err(format!("'taper' is 0 to 1, not {v}")),
                ("feed" | "kill" | "saturation" | "stick" | "hits" | "roots" | "edge" | "fill" | "sense" | "turn" | "spread" | "length" | "width" | "swirl" | "taper", _) => return Err(format!("@{name} takes {}, not '{key}'", match preset.model {
                    Model::GrayScott { .. } => "'feed' and 'kill'",
                    Model::Activator { .. } => "'saturation'",
                    Model::Aggregation { .. } => "'stick', 'hits' and 'roots'",
                    Model::Scales { .. } => "'edge' and 'fill'",
                    Model::Physarum { .. } => "'sense', 'turn' and 'spread'",
                    Model::Flow { .. } => "'length', 'width', 'swirl' and 'taper'",
                })),
                _ => return Err(format!("a {} pattern has no '{key}'", preset.kind)),
            }
        }
        Ok(s)
    }

    /// The fine lattice's pitch (mm).
    pub fn pitch(&self) -> f64 {
        self.scale / self.preset.cells
    }

    /// Who it is but for its steps (with the shape it grows on, a simulation's identity: a
    /// step count raised carries on from where it is).
    pub fn lineage(&self, shape: u64) -> u64 {
        let text = format!("{}|{:?}|{}|{}|{}|{shape}", self.preset.name, self.model, self.scale.to_bits(), self.seed, self.depth.to_bits());
        fnv(text.as_bytes())
    }
}

pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// splitmix64: a well-mixed word from any word.
pub(crate) fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A number in 0..1 from the seed, a salt and a value.
pub(crate) fn unit(seed: u32, salt: u64, x: u64) -> f64 {
    (mix(mix(seed as u64 ^ (salt << 32)) ^ x) >> 11) as f64 / (1u64 << 53) as f64
}

/// A lattice cell packed into one word (21 bits an axis, so a map hashes one word).
pub(crate) fn pack(c: [i32; 3]) -> u64 {
    const M: u64 = (1 << 21) - 1;
    ((c[0] as u64 & M) << 42) | ((c[1] as u64 & M) << 21) | (c[2] as u64 & M)
}

/// A hasher for packed cells: one multiply.
#[derive(Default, Clone, Copy)]
pub(crate) struct CellHasher(u64);

impl Hasher for CellHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ *b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_u64(&mut self, v: u64) {
        let x = (v ^ (v >> 31)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = x ^ (x >> 29);
    }
}

pub(crate) type CellMap<V> = HashMap<u64, V, BuildHasherDefault<CellHasher>>;

/// Samples per brick edge in a [`Layout`].
pub(crate) const BRICK: i32 = 8;

/// Where a level's cells sit for sampling: bricks of 8³ lattice points, each cell's slot in
/// them, and per brick near the cells how many bricks away the nearest one with cells is.
#[derive(Debug)]
pub struct Layout {
    /// The lattice pitch (mm).
    pub pitch: f64,
    bricks: CellMap<u32>,
    /// Per cell (the level's order), its brick's index times 512 plus its place in it.
    slots: Vec<u32>,
    /// Bricks within two of a brick with cells: 0 one of them, 1 or 2 how far.
    near: CellMap<u8>,
}

impl Layout {
    pub(crate) fn new(pitch: f64, cells: &[[i32; 3]]) -> Layout {
        let mut bricks: CellMap<u32> = CellMap::default();
        let mut slots = Vec::with_capacity(cells.len());
        for c in cells {
            let b = c.map(|v| v.div_euclid(BRICK));
            let n = bricks.len() as u32;
            let i = *bricks.entry(pack(b)).or_insert(n);
            let l = c.map(|v| v.rem_euclid(BRICK));
            slots.push(i * 512 + (l[0] + (l[1] + l[2] * BRICK) * BRICK) as u32);
        }
        let mut near: CellMap<u8> = CellMap::default();
        let full: Vec<[i32; 3]> = {
            let mut seen: CellMap<()> = CellMap::default();
            cells.iter().map(|c| c.map(|v| v.div_euclid(BRICK))).filter(|b| seen.insert(pack(*b), ()).is_none()).collect()
        };
        for b in &full {
            for dz in -2..=2i32 {
                for dy in -2..=2i32 {
                    for dx in -2..=2i32 {
                        let ring = dx.abs().max(dy.abs()).max(dz.abs()) as u8;
                        let e = near.entry(pack([b[0] + dx, b[1] + dy, b[2] + dz])).or_insert(ring);
                        *e = (*e).min(ring);
                    }
                }
            }
        }
        Layout { pitch, bricks, slots, near }
    }

    pub fn bricks(&self) -> usize {
        self.bricks.len()
    }
}

/// A pattern's state sampled by position: per lattice cell its place in the mix's materials
/// (0..1), read trilinearly; away from every cell (deep inside, or outside the part) the
/// first material.
#[derive(Clone)]
pub struct Grid {
    pub layout: Arc<Layout>,
    /// 512 per brick: the place, NaN where no cell is.
    values: Arc<Vec<f32>>,
    /// The steps it has grown.
    pub step: u32,
    /// The state at its spec's steps (what prints), not one on the way.
    pub done: bool,
    /// The step its pattern settled at on the way here, while it stays settled
    /// ([`live::SETTLED`]). A readout only: it grows to its steps all the same.
    pub settled: Option<u32>,
    /// What it is: its simulation's lineage, steps and lattice. Equal keys sample alike.
    pub key: u64,
}

impl std::fmt::Debug for Grid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Grid(step {}{} key {:016x})", self.step, if self.done { " done" } else { "" }, self.key)
    }
}

impl Grid {
    pub(crate) fn new(layout: Arc<Layout>, cell_values: &[f32], step: u32, done: bool, key: u64) -> Grid {
        let mut values = vec![f32::NAN; layout.bricks.len() * 512];
        for (slot, v) in layout.slots.iter().zip(cell_values) {
            values[*slot as usize] = *v;
        }
        Grid { layout, values: Arc::new(values), step, done, settled: None, key }
    }

    /// The place at `p` (model mm), 0..1: trilinear over the cells round it that it has, the
    /// first material (0) where it has none.
    pub fn value(&self, p: P3) -> f64 {
        let (sum, weight) = self.weighed(p);
        if weight > 1e-9 { sum / weight } else { 0.0 }
    }

    /// The trilinear read at `p` (model mm) over the cells there: the weighed sum of their
    /// values, and the weight they hold, 1 inside the band and down to 0 a cell past it.
    pub fn weighed(&self, p: P3) -> (f64, f64) {
        let h = self.layout.pitch;
        let q = p.map(|v| v / h);
        let i: [i32; 3] = q.map(|v| v.floor() as i32);
        let f: [f64; 3] = std::array::from_fn(|k| q[k] - i[k] as f64);
        let (b, l) = (i.map(|v| v.div_euclid(BRICK)), i.map(|v| v.rem_euclid(BRICK)));
        // the corners lie in at most eight bricks, mostly in one: each looked up once
        let mut bricks: [Option<Option<u32>>; 8] = [None; 8];
        let (mut sum, mut weight) = (0.0, 0.0);
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let c: [i32; 3] = std::array::from_fn(|k| l[k] + o[k] as i32);
            let carry = (0..3).fold(0, |m, k| m | ((c[k] == BRICK) as usize) << k);
            let brick = *bricks[carry].get_or_insert_with(|| {
                let at: [i32; 3] = std::array::from_fn(|k| b[k] + ((carry >> k) & 1) as i32);
                self.layout.bricks.get(&pack(at)).copied()
            });
            let Some(brick) = brick else { continue };
            let c = c.map(|v| v % BRICK);
            let v = self.values[brick as usize * 512 + (c[0] + (c[1] + c[2] * BRICK) * BRICK) as usize];
            if v.is_nan() {
                continue;
            }
            let w: f64 = (0..3).map(|k| if o[k] == 1 { f[k] } else { 1.0 - f[k] }).product();
            sum += w * v as f64;
            weight += w;
        }
        (sum, weight)
    }

    /// A lower bound of the distance from `p` (model mm) to the nearest cell: past it the
    /// place is the first material's.
    pub fn clear_distance(&self, p: P3) -> f64 {
        let edge = BRICK as f64 * self.layout.pitch;
        let b: [i32; 3] = p.map(|v| (v / edge).floor() as i32);
        match self.layout.near.get(&pack(b)) {
            None => 2.0 * edge,
            Some(0) => 0.0,
            Some(ring) => (*ring as f64 - 1.0) * edge,
        }
    }

    /// The bytes it holds.
    pub fn bytes(&self) -> usize {
        self.values.len() * 4 + self.layout.slots.len() * 4
    }
}

/// A mix's pattern: what was asked for and, once the part's surface has grown it, its state
/// (and the simulation still running it, while it is not done).
#[derive(Clone)]
pub struct Grown {
    pub spec: Spec,
    pub grid: Option<Arc<Grid>>,
    pub sim: Option<Arc<live::Sim>>,
}

impl Grown {
    pub fn new(spec: Spec) -> Grown {
        Grown { spec, grid: None, sim: None }
    }

    /// The place at `p` (model mm): 0..1, the first material before it has grown.
    pub fn value(&self, p: P3) -> f64 {
        self.grid.as_ref().map_or(0.0, |g| g.value(p))
    }

    /// Whether what it shows is what prints.
    pub fn done(&self) -> bool {
        self.grid.as_ref().is_some_and(|g| g.done)
    }
}

impl std::fmt::Debug for Grown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({} {:?} scale {} seed {} steps {} depth {} {:?})", self.spec.preset.kind, self.spec.preset.name, self.spec.model, self.spec.scale, self.spec.seed, self.spec.steps, self.spec.depth, self.grid)
    }
}

impl PartialEq for Grown {
    fn eq(&self, o: &Grown) -> bool {
        self.spec == o.spec && self.grid.as_ref().map(|g| g.key) == o.grid.as_ref().map(|g| g.key)
    }
}

#[cfg(test)]
mod tests;
