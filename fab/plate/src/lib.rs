//! The printable plate: what the model hands to everything downstream of it.
//!
//! A [`Plate`] is a list of [`Part`]s, each a closed triangle mesh in printer
//! coordinates: millimetres, Z up, the bed's front-left corner at the origin,
//! X to the right and Y to the back. The model document evaluates into one;
//! the bed view checks it against a [`machine::Machine`], the exporters write
//! it as STL or 3MF, and the slicer reads it through
//! [`contour::ContourSource`].
//!
//! Every part carries its display colour and, once a job maps the print's colours onto the
//! printer ([`PrintColor`]), the job's tool and nozzle that colour landed on, so a dual-nozzle
//! check and a multi-material 3MF come from the same data.

pub mod contour;
pub mod machine;
pub mod melt;
pub mod paint;
pub mod settings;
pub mod stl;
pub mod threemf;

pub use settings::*;

/// An indexed triangle mesh in millimetres. Triangles wind counter-clockwise
/// seen from outside (outward normals by the right-hand rule).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartMesh {
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

/// An axis-aligned box in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Bounds {
    pub const EMPTY: Bounds = Bounds {
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
    };
    pub fn is_empty(&self) -> bool {
        (0..3).any(|d| self.min[d] > self.max[d])
    }
    /// The box grown to take `p`. A zero bound is +0.0 (`+ 0.0`), whichever sign `p`'s zero has:
    /// `min` and `max` tie +0.0 and -0.0 by order, so a box would otherwise differ by machine.
    pub fn add_point(&mut self, p: [f64; 3]) {
        for d in 0..3 {
            self.min[d] = self.min[d].min(p[d]) + 0.0;
            self.max[d] = self.max[d].max(p[d]) + 0.0;
        }
    }
    pub fn union(&self, other: &Bounds) -> Bounds {
        let mut out = *self;
        if !other.is_empty() {
            out.add_point(other.min);
            out.add_point(other.max);
        }
        out
    }
    pub fn size(&self) -> [f64; 3] {
        if self.is_empty() {
            return [0.0; 3];
        }
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
    pub fn center(&self) -> [f64; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }
}

impl PartMesh {
    /// The mesh's box, its zero bounds +0.0 as [`Bounds::add_point`] makes them (once, after the
    /// loop: a vectorised min picks a tie's sign by its width).
    pub fn bounds(&self) -> Bounds {
        let mut b = Bounds::EMPTY;
        for p in &self.positions {
            for d in 0..3 {
                b.min[d] = b.min[d].min(p[d]);
                b.max[d] = b.max[d].max(p[d]);
            }
        }
        Bounds { min: b.min.map(|v| v + 0.0), max: b.max.map(|v| v + 0.0) }
    }
    pub fn translate(&mut self, by: [f64; 3]) {
        for p in &mut self.positions {
            for d in 0..3 {
                p[d] += by[d];
            }
        }
    }
    /// Signed volume in mm³ (positive for an outward-wound closed mesh).
    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for t in &self.triangles {
            let a = self.positions[t[0] as usize];
            let b = self.positions[t[1] as usize];
            let c = self.positions[t[2] as usize];
            v += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]);
        }
        v / 6.0
    }
    /// Edges used by other than exactly two triangles, or used twice in the
    /// same direction: zero for a closed, consistently wound (printable) mesh.
    pub fn open_edges(&self) -> usize {
        use std::collections::HashMap;
        let mut edges: HashMap<(u32, u32), i32> = HashMap::with_capacity(self.triangles.len() * 3);
        for t in &self.triangles {
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                if a == b {
                    continue;
                }
                let (key, dir) = if a < b { ((a, b), 1) } else { ((b, a), -1) };
                *edges.entry(key).or_insert(0) += dir;
            }
        }
        edges.values().filter(|&&n| n != 0).count()
    }
}

/// A part's mesh at print tolerance, made when a slice asks for it (it can take a while).
#[derive(Clone)]
pub struct PrintMesh(pub std::sync::Arc<dyn Fn() -> Result<PartMesh, String> + Send + Sync>);

impl std::fmt::Debug for PrintMesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrintMesh")
    }
}

impl PartialEq for PrintMesh {
    fn eq(&self, other: &PrintMesh) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Where a part made from (or resting by) a distance field rests ([`Part::field_floor`]): the z
/// of the lowest solid point its field's proof found, None when it found none (the part then
/// rests on its box's bottom). Proven when the layout placed the part, or pending: a part that
/// cannot set its object's rest is read for its own gap only, so its proof waits for an analysis
/// to ask ([`Part::prove_floor`]) and runs once, on the thread that asks, for every copy.
#[derive(Clone)]
pub struct FieldFloor {
    z: std::sync::Arc<FloorProof>,
    /// Added to the proof's z: where the layout dropped the part, and every move since.
    offset: f64,
    /// Whether the part, when it rests above the plate, stands on printed material
    /// ([`Held`]); None: it is judged on its own, as floating.
    held: Option<std::sync::Arc<dyn Held>>,
}

/// Whether a part above the plate is held up: it rests on printed material that is itself
/// held, down to the plate, by a real contact (its first layer over that material, within a
/// layer, by a printable area). The layout proves it from the parts' fields when an analysis
/// asks, once for every copy.
pub trait Held: Send + Sync {
    /// Whether it is held, proving it first.
    fn held(&self) -> bool;
    /// Whether it is held: None while it is pending. Never runs a proof.
    fn known(&self) -> Option<bool>;
}

struct FloorProof {
    z: std::sync::OnceLock<Option<f64>>,
    prove: Option<Box<dyn Fn() -> Option<f64> + Send + Sync>>,
}

impl FieldFloor {
    /// A proven floor at `z`.
    pub fn proven(z: f64) -> Self {
        FieldFloor { z: std::sync::Arc::new(FloorProof { z: std::sync::OnceLock::from(Some(z)), prove: None }), offset: 0.0, held: None }
    }

    /// A floor `prove` finds when an analysis asks for it.
    pub fn pending(prove: impl Fn() -> Option<f64> + Send + Sync + 'static) -> Self {
        FieldFloor { z: std::sync::Arc::new(FloorProof { z: std::sync::OnceLock::new(), prove: Some(Box::new(prove)) }), offset: 0.0, held: None }
    }

    /// The floor, proving it first if it is pending.
    pub fn z(&self) -> Option<f64> {
        let p = &self.z;
        p.z.get_or_init(|| p.prove.as_ref().and_then(|prove| prove())).map(|z| z + self.offset)
    }

    /// The floor if it is proven: None while it is pending, Some(None) when the proof found no
    /// solid point. Never runs a proof.
    pub fn known(&self) -> Option<Option<f64>> {
        self.z.z.get().map(|z| z.map(|z| z + self.offset))
    }

    /// Moved up by `dz`, with the part.
    pub fn raise(&mut self, dz: f64) {
        self.offset += dz;
    }

    /// Judged by `held` when it rests above the plate.
    pub fn held_by(&mut self, held: std::sync::Arc<dyn Held>) {
        self.held = Some(held);
    }
}

impl std::fmt::Debug for FieldFloor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.known() {
            Some(z) => write!(f, "FieldFloor({z:?})"),
            None => write!(f, "FieldFloor(pending, offset {})", self.offset),
        }
    }
}

impl PartialEq for FieldFloor {
    fn eq(&self, other: &FieldFloor) -> bool {
        match (self.known(), other.known()) {
            (Some(a), Some(b)) => a == b,
            _ => std::sync::Arc::ptr_eq(&self.z, &other.z) && self.offset == other.offset,
        }
    }
}

/// This records how light passes through a part that carries it from a source to one of
/// its faces, such as a light pipe or a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Optics {
    /// This is the share of the source's flux that leaves the part's exit face, from 0 to 1.
    pub throughput: f64,
    /// This says whether the throughput is an estimate or a measurement.
    pub source: OpticsSource,
}

/// This identifies how a part's optical throughput was determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpticsSource {
    /// The throughput is an estimate.
    Estimate,
    /// The throughput is a measurement.
    Measured,
}

/// This records space on a board that this part keeps clear of other parts.
#[derive(Clone, Debug, PartialEq)]
pub struct Keepout {
    /// This names the board by its unit name.
    pub board: String,
    /// This names the thing keeping the space clear, with one record per thing.
    pub name: String,
    /// This is the outline in the board's own frame, in kernel units, with the board's
    /// x and y coordinates.
    pub area: Vec<[f64; 2]>,
    /// This is the occupied band `[low, high]`, in kernel units, measured up from the
    /// board's top surface.
    pub z: [f64; 2],
}

/// One printable object on the plate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// The model object this part came from (stable across rebuilds).
    pub object: String,
    /// Display name (defaults to the object name).
    pub name: String,
    /// This records how the part carries light, or is `None` when the solid states no optics.
    pub optics: Option<Optics>,
    /// The host key `m.link` tagged its solid with, or `None`: a light pipe's is the port of the
    /// LED it carries, so a host shows the pipe lit as that LED is. The plate never reads it.
    pub link: Option<String>,
    /// These are the spaces on boards this part keeps clear of other parts, empty by default.
    pub keepouts: Vec<Keepout>,
    /// The job's tool (1-based) the part's print colour maps to. The model leaves it at 1;
    /// a job sets it from the colour's printer position, never from the project.
    pub filament: u32,
    /// Display colour, linear RGBA 0..1.
    pub color: [f32; 4],
    /// The nozzle (index into the machine's nozzles) of that position, set with `filament`;
    /// `None` until a job mapped it.
    pub nozzle: Option<usize>,
    pub mesh: PartMesh,
    /// A part made from a distance field (the slicer and the views take the field): its box
    /// in printer mm. Its `mesh` is then empty unless one was asked for (Mesh shading, an
    /// export).
    pub field_bounds: Option<Bounds>,
    /// A field part, or one resting by a field: the z of the lowest solid point its field's
    /// proof found, the point it rests on (its box may reach lower, by what the proof left open).
    /// A part with an analytic solid otherwise: its box's bottom, where that solid judges what it
    /// stands on ([`Part::known_held`]). None: it rests on its box's bottom ([`Part::floor`]).
    pub field_floor: Option<FieldFloor>,
    /// Per-part print setting overrides (process key, value), e.g. an
    /// experimental mode for this part only. Empty = the plate's settings.
    pub print: Vec<(String, String)>,
    /// An exact part's finer mesh for the slicer; `mesh` is then its display mesh.
    pub print_mesh: Option<PrintMesh>,
    /// Its paint read by position (a model part's script paints it): the slicer asks it per
    /// layer, the views have their own bake of the same program.
    pub paint: Option<SpatialPaint>,
    /// The placed object this part is one body of, when that object prints as several
    /// (stable across rebuilds): the slicer glues the layers of the parts that share it
    /// ([`Plate::groups`]). None: the part prints on its own.
    pub body_of: Option<String>,
    /// The parts ([`Part::object`]) this one prints fused with by design: a bond's part and
    /// each part it bonds, a braked hinge's turning part and its shoe. Their bodies may take
    /// the same space; that is no overlap.
    pub fused: Vec<String>,
}

/// One colour of the print: what parts and paint name, and where it prints on each printer
/// it was mapped for. Identity is the name, never the colour value: two whites in two
/// materials are two print colours.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintColor {
    pub name: String,
    /// The design colour (linear RGB) when it is set apart from the material's (an override);
    /// `None`: the colour of the material it maps to.
    pub rgb: Option<[f32; 3]>,
    /// The colour shown when it has no override and no mapped material reports one.
    pub fallback: [f32; 3],
    /// The colour the design asks for without an override (the albedo of a painted material
    /// that prints as it): what picks its tray. `None`: any colour of the material type.
    pub design: Option<[f32; 3]>,
    /// Per printer (its name): the nozzle side (`left`, `right`) and tray (`A2`, `Ext`).
    pub at: Vec<PrintColorAt>,
    /// The material type it prints in ("TPU", "PLA"); `None`: the job's own material.
    pub material: Option<String>,
    /// Not in the project: made for a part or painted material that names no colour.
    pub implicit: bool,
}

/// A flexible filament type ("TPU", "TPU-AMS", "TPE"): it needs a feed path of its own, and a
/// part of it bends.
pub fn flexible(material: &str) -> bool {
    let m = material.trim().to_ascii_uppercase();
    m.starts_with("TPU") || m.starts_with("TPE") || m.starts_with("FLEX")
}

/// Where a print colour prints on one printer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintColorAt {
    pub printer: String,
    /// The toolhead's word (its name in ASCII lowercase).
    pub side: String,
    pub tray: String,
}

/// A region of a part's paint: a support channel (`m.support_block`, `m.support_enforce` or
/// `m.no_trees`), or where a region's print settings hold (`m.settings`).
#[derive(Clone)]
pub struct SupportRegion {
    /// At a printer point: negative inside the region, and its size a lower bound of the distance
    /// (printer mm) to the region's edge.
    pub at: std::sync::Arc<dyn Fn([f64; 3]) -> f64 + Send + Sync>,
    /// The printer box it holds in (mm); `None`: it may hold anywhere.
    pub bounds: Option<([f64; 3], [f64; 3])>,
}

/// A region of a part with print settings of its own (`m.settings`): where it holds, and the
/// [`REGION_SETTINGS`] it sets, as checked values ([`checked_value`]: "100%", "gyroid", "4").
#[derive(Clone)]
pub struct SettingsRegion {
    pub region: SupportRegion,
    pub values: Vec<(String, String)>,
}

/// What a part's paint holds at a printer point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintSample {
    /// The colour slot (0: the part's own); in a blend the nearer one.
    pub slot: u8,
    /// A lower bound of the distance (printer mm) to the nearest point whose slot differs; 0 in
    /// a blend (it may change anywhere).
    pub dist: f64,
    /// A blend of two: the other slot and its share (over 0, at most one half), which the print
    /// dithers.
    pub mix: Option<(u8, f32)>,
}

/// A part's paint by position: the colour slot at a printer point (0: the part's own), where
/// the support channels hold, the colours the slots show and print in, and a key (equal keys
/// paint the same).
#[derive(Clone)]
pub struct SpatialPaint {
    /// What the paint holds at a printer point.
    pub sample_at: std::sync::Arc<dyn Fn([f64; 3]) -> PaintSample + Send + Sync>,
    /// Where supports never go.
    pub support_block: Option<SupportRegion>,
    /// Where supports always go.
    pub support_enforce: Option<SupportRegion>,
    /// Where no tree support stands or grows (`m.no_trees`).
    pub no_trees: Option<SupportRegion>,
    /// The regions with print settings of their own (`m.settings`), in the script's order: where
    /// two overlap, the later one's value of a key wins.
    pub settings: Vec<SettingsRegion>,
    pub colors: Vec<paint::PaintColor>,
    /// What the paint is to the slice: 0 when it holds only no-trees areas.
    pub key: u64,
    /// What its no-trees areas are, which only the supports read: 0 for none.
    pub no_trees_key: u64,
    /// What its support channels and regions' print settings are where they stand on the
    /// printer, whatever its colours: equal keys read the same regions; 0 for none.
    pub regions_key: u64,
    /// The paint the closures sample, for [`SpatialPaint::changed`].
    pub source: std::sync::Arc<dyn PaintChange>,
}

/// The paint behind a [`SpatialPaint`], compared with an older one.
pub trait PaintChange: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    /// The printer heights (mm) outside which `old` samples the same slot and
    /// distance: empty when the two paint alike, `None` anywhere.
    fn changed(&self, old: &dyn PaintChange) -> Option<Vec<(f64, f64)>>;
}

impl SpatialPaint {
    /// The heights where [`SpatialPaint::sample_at`] can answer differently from `old`'s
    /// (printer mm): empty when the two paint alike, `None` anywhere.
    pub fn changed(&self, old: &SpatialPaint) -> Option<Vec<(f64, f64)>> {
        if self.key == old.key {
            return Some(Vec::new());
        }
        self.source.changed(&*old.source)
    }
    /// The paint state per slot for the slicer (0: prints as the part, N: tool N), the part
    /// printing in tool `own`.
    pub fn states(&self, own: u32) -> Vec<u32> {
        paint::PartPaint { colors: self.colors.clone(), tri: Vec::new() }.states(own)
    }
    /// Whether any slot prints in another tool than `own`.
    pub fn colours(&self, own: u32) -> bool {
        self.states(own).iter().any(|&s| s != 0)
    }
}

/// The largest factor by which the linear part of an affine map stretches a length (its
/// largest singular value): a distance `d` before the map is at most `d * stretch` after it.
pub fn stretch(m: &[[f64; 4]; 3]) -> f64 {
    // power iteration on MᵀM from a start no row annihilates
    let mtm: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|r| m[r][i] * m[r][j]).sum()));
    let mut v = [0.577, 0.578, 0.579];
    let mut l = 0.0;
    for _ in 0..64 {
        let w: [f64; 3] = std::array::from_fn(|i| (0..3).map(|j| mtm[i][j] * v[j]).sum());
        let n = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt();
        if n <= 0.0 {
            return 0.0;
        }
        v = w.map(|x| x / n);
        l = n;
    }
    // a hair over, so a distance bound divided by it stays a bound
    l.sqrt() * (1.0 + 1e-6)
}

impl std::fmt::Debug for SpatialPaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SpatialPaint({} colours, key {:016x})", self.colors.len(), self.key)
    }
}

impl PartialEq for SpatialPaint {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.no_trees_key == other.no_trees_key
    }
}

impl Part {
    /// Its box: the field's for a field part, else its mesh's.
    pub fn bounds(&self) -> Bounds {
        self.field_bounds.unwrap_or_else(|| self.mesh.bounds())
    }

    /// The nozzles it prints with once a job mapped its colours ([`Plate::map_colors`]): its
    /// own colour's and each painted colour's, each once, in order.
    pub fn nozzles(&self) -> Vec<usize> {
        let painted = self.paint.iter().flat_map(|sp| sp.colors.iter().filter_map(|c| c.nozzle));
        let mut nozzles: Vec<usize> = self.nozzle.into_iter().chain(painted).collect();
        nozzles.sort_unstable();
        nozzles.dedup();
        nozzles
    }

    /// The z it rests on: a field part's lowest found solid point, else its box's bottom. The
    /// layout drops a part by it. A pending floor is proven first, on this thread.
    pub fn floor(&self) -> f64 {
        self.field_floor.as_ref().and_then(FieldFloor::z).unwrap_or_else(|| self.bounds().min[2])
    }

    /// Whether it stands on printed material when it rests above the plate: None while that is
    /// pending, false when it floats or is judged on its own. Never runs a proof.
    pub fn known_held(&self) -> Option<bool> {
        match self.field_floor.as_ref().and_then(|f| f.held.as_ref()) {
            Some(h) => h.known(),
            None => Some(false),
        }
    }

    /// [`Part::floor`] if it is known: None while its floor is pending. The plate check tests it.
    pub fn known_floor(&self) -> Option<f64> {
        match &self.field_floor {
            Some(f) => f.known().map(|z| z.unwrap_or_else(|| self.bounds().min[2])),
            None => Some(self.bounds().min[2]),
        }
    }

    /// Prove a pending floor, and whether it is held (an analysis that reports the part's gap, on
    /// its own thread).
    pub fn prove_floor(&self) {
        if let Some(f) = &self.field_floor {
            f.z();
            if let Some(h) = &f.held {
                h.held();
            }
        }
    }
}

/// A part's render mesh: per-corner normals, printer frame, millimetres.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// A part's `place(...)` call as it reads, so a direct edit (move, turn, scale,
/// filament) changes that call's arguments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartHandles {
    /// The solid's id (its `place(...)` takes it).
    pub part: Option<String>,
    pub place: Option<String>,
    pub at: Option<[f64; 2]>,
    /// Degrees about X, Y, Z of that call.
    pub rotate: [f64; 3],
    /// Its factors along the printer's X, Y, Z of the unturned part (all equal: uniform).
    pub scale: [f64; 3],
    pub drop: bool,
    /// Kept on the plate but out of the views (`hidden: true`).
    pub hidden: bool,
}

/// A placement kept on the bed but left out of slicing and printing.
/// Its object id and place handle retain the instance/link identity.
#[derive(Clone, Debug, PartialEq)]
pub struct ExcludedPart {
    pub part: Part,
    /// The host key `m.link` tagged the emitted solid with (`"board:ledctl"`): the host that
    /// draws this part resolves it. The plate never reads it.
    pub link: Option<String>,
    /// Its named frame (`frame: Frame{name at rotate}`) in the model frame, row-major, as the
    /// model's object info gives it: a host that draws this part by its `link` places its own
    /// geometry at `model_to_plate x frame`. The plate never reads it.
    pub frame: Option<(String, [[f64; 4]; 3])>,
    /// Set when this part rides in a printed one and the slice is checked against it.
    pub insert: Option<Insert>,
    pub render: RenderMesh,
    pub handles: PartHandles,
    pub model_to_plate: [[f64; 4]; 3],
    pub paint: Option<std::sync::Arc<paint::PartPaint>>,
}

impl ExcludedPart {
    /// Its heat-set insert's hole on the plate (printer mm), when it is one.
    pub fn melt(&self) -> Option<melt::Melt> {
        self.insert.as_ref()?.melt.as_ref().map(|m| m.placed(&self.model_to_plate))
    }
}

/// A non-printed part riding on a printed one: `into` is that part's `Part::object`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Insert {
    pub into: String,
    /// Sideways bead clearance in millimetres.
    pub clearance: f64,
    /// Printed into its host: the slicer cuts its cavity from the host and the print pauses
    /// before the first layer over it, for a person to drop it in.
    pub embed: bool,
    /// Melted into its host after the print (a heat-set insert): its hole, in the model frame.
    /// The print never stops for it and nothing is cut for it but the hole its script made.
    pub melt: Option<melt::Melt>,
}

/// Everything on the build plate, in printer coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plate {
    /// Printable parts only; all slicer indices refer to this list.
    pub parts: Vec<Part>,
    pub excluded: Vec<ExcludedPart>,
    /// The project's global print settings (key, value), on top of the process profile and
    /// under each part's own [`Part::print`].
    pub settings: Vec<(String, String)>,
}

impl Plate {
    /// Inserts in excluded order, each with the index of its printed host.
    pub fn inserts(&self) -> Vec<(&Part, &Insert, usize)> {
        if !self.excluded.iter().any(|p| p.insert.is_some()) {
            return Vec::new();
        }
        let hosts: std::collections::HashMap<&str, usize> = self.parts.iter().enumerate().map(|(i, p)| (p.object.as_str(), i)).collect();
        self.excluded.iter().filter_map(|p| {
            let insert = p.insert.as_ref()?;
            Some((&p.part, insert, *hosts.get(insert.into.as_str())?))
        }).collect()
    }

    /// Put each part and each painted colour on the job's tool and nozzle of its print colour:
    /// `part_colors[i]` is placed part `i`'s colour (printed, then excluded), `tool_of` a
    /// colour's (1-based tool, nozzle index); a colour without one keeps tool 1 and no nozzle.
    pub fn map_colors(&mut self, part_colors: &[String], tool_of: &dyn Fn(&str) -> Option<(u32, usize)>) {
        let parts = self.parts.iter_mut().chain(self.excluded.iter_mut().map(|e| &mut e.part));
        for (part, name) in parts.zip(part_colors) {
            let (tool, nozzle) = tool_of(name).map_or((1, None), |(t, z)| (t, Some(z)));
            part.filament = tool;
            part.nozzle = nozzle;
            if let Some(sp) = &mut part.paint {
                for c in sp.colors.iter_mut() {
                    c.map(tool_of);
                }
            }
        }
    }

    /// The parts that print as one body: those that share a [`Part::body_of`], two or more,
    /// each group their indices in plate order (the object's own order of its bodies).
    pub fn groups(&self) -> Vec<Vec<usize>> {
        let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (i, p) in self.parts.iter().enumerate() {
            let Some(of) = p.body_of.as_deref() else { continue };
            match groups.iter_mut().find(|g| g.0 == of) {
                Some(g) => g.1.push(i),
                None => groups.push((of, vec![i])),
            }
        }
        groups.into_iter().map(|g| g.1).filter(|g| g.len() > 1).collect()
    }

    /// Editor order: printable parts first, then excluded placements.
    pub fn placed_parts(&self) -> impl Iterator<Item = &Part> {
        self.parts.iter().chain(self.excluded.iter().map(|p| &p.part))
    }
    pub fn placed_part(&self, i: usize) -> Option<&Part> {
        if i < self.parts.len() { self.parts.get(i) }
        else { self.excluded.get(i - self.parts.len()).map(|p| &p.part) }
    }

    /// A worker input without the editor's display-only geometry: of the excluded parts only
    /// the embedded inserts of printed hosts stay (the slicer cuts their cavities), each without
    /// its render mesh.
    pub fn printable(&self) -> Self {
        let hosts: std::collections::HashSet<&str> = self.parts.iter().map(|p| p.object.as_str()).collect();
        let excluded = self.excluded.iter().filter(|e| e.insert.as_ref().is_some_and(|i| i.embed && hosts.contains(i.into.as_str())))
            .map(|e| ExcludedPart { part: e.part.clone(), link: None, frame: None, insert: e.insert.clone(), render: RenderMesh::default(), handles: PartHandles::default(), model_to_plate: e.model_to_plate, paint: None })
            .collect();
        Self { parts: self.parts.clone(), excluded, settings: self.settings.clone() }
    }

    /// The embedded inserts ([`Insert::embed`]) by printed host index: per host, its items in
    /// excluded order.
    pub fn embeds(&self) -> Vec<(usize, Vec<(&Part, &Insert)>)> {
        let mut out: Vec<(usize, Vec<(&Part, &Insert)>)> = Vec::new();
        for (part, insert, host) in self.inserts() {
            if !insert.embed { continue; }
            match out.iter_mut().find(|(h, _)| *h == host) {
                Some((_, items)) => items.push((part, insert)),
                None => out.push((host, vec![(part, insert)])),
            }
        }
        out
    }

    pub fn bounds(&self) -> Bounds {
        self.parts
            .iter()
            .fold(Bounds::EMPTY, |b, p| b.union(&p.bounds()))
    }
    pub fn triangle_count(&self) -> usize {
        self.parts.iter().map(|p| p.mesh.triangles.len()).sum()
    }
}

/// Shortest decimal text for a millimetre value at 10 nm resolution, for the
/// text formats (3MF, ASCII reports): `12`, `12.5`, `-0.00125`.
pub fn fmt_mm(v: f64) -> String {
    let v = if v.abs() < 5e-6 { 0.0 } else { v };
    let mut s = format!("{v:.5}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

#[cfg(test)]
pub(crate) fn test_cube(min: [f64; 3], size: f64) -> PartMesh {
    test_box(min, [size; 3])
}

#[cfg(test)]
pub(crate) fn test_box(min: [f64; 3], size: [f64; 3]) -> PartMesh {
    let mut positions = Vec::new();
    for i in 0..8u32 {
        positions.push([
            min[0] + size[0] * (i & 1) as f64,
            min[1] + size[1] * ((i >> 1) & 1) as f64,
            min[2] + size[2] * ((i >> 2) & 1) as f64,
        ]);
    }
    // Outward counter-clockwise faces of the unit cube indexed by bits (x, y, z).
    let triangles = vec![
        [0, 2, 1], [1, 2, 3], // z = 0 (down)
        [4, 5, 6], [5, 7, 6], // z = 1 (up)
        [0, 1, 4], [1, 5, 4], // y = 0 (front)
        [2, 6, 3], [3, 6, 7], // y = 1 (back)
        [0, 4, 2], [2, 4, 6], // x = 0 (left)
        [1, 3, 5], [3, 7, 5], // x = 1 (right)
    ];
    PartMesh { positions, triangles }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_is_closed_with_positive_volume() {
        let m = test_cube([0.0; 3], 10.0);
        assert_eq!(m.open_edges(), 0);
        assert!((m.volume() - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn the_bodies_of_one_object_are_a_group_in_plate_order() {
        let part = |of: Option<&str>| Part { body_of: of.map(str::to_string), object: "p".into(), name: "p".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: test_cube([0.0; 3], 1.0), field_bounds: None, field_floor: None, print: Vec::new(), print_mesh: None, paint: None, ..Default::default() };
        let parts = vec![part(Some("hair")), part(None), part(Some("vase")), part(Some("hair")), part(Some("hair"))];
        let plate = Plate { parts, ..Default::default() };
        assert_eq!(plate.groups(), vec![vec![0, 3, 4]], "a body alone is no group, a part of none prints alone");
    }

    #[test]
    fn flexible_types_are_flexible() {
        assert!(flexible("TPU 95A") && flexible("tpe") && flexible("Flex") && !flexible("PLA") && !flexible("PETG"));
    }

    #[test]
    fn mm_text_is_short() {
        assert_eq!(fmt_mm(12.0), "12");
        assert_eq!(fmt_mm(12.5), "12.5");
        assert_eq!(fmt_mm(-0.000001), "0");
        assert_eq!(fmt_mm(0.123456), "0.12346");
    }
}
