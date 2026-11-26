//! Generated parameter editing: what the part panel's parameter controls and
//! the view's shape handles are built from, and the drag math they share.
//!
//! The app fills [`ParamState::parts`] from the model's parameter model (one
//! [`PartParamsView`] per plate part, everything in printer millimetres on
//! the plate, Z up). Nothing here is per part or per feature: a control and a
//! handle come from a parameter's [`ParamKind`] and [`ParamHandle`] alone.
//!
//! Edits go out as [`ParamAction`]s: `Preview` while a control or a handle is
//! dragged (the app coalesces them: only the newest is ever evaluated, and
//! nothing is committed), `Commit` once (one undo step), `Cancel` to drop a
//! preview.
//! A step that does not build is capped at a value that did and flashes its control red.

use std::sync::Arc;

pub type V3 = [f64; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ParamKind {
    #[default]
    Length,
    Radius,
    Diameter,
    Angle,
    Count,
    Number,
    Ratio,
    Bool,
    Choice,
    Point,
    Profile,
    /// A whole number that picks one of many (a pattern's seed): its row reseeds too.
    Seed,
    /// A grown pattern's progress (its value: which pattern of the part's paint): the row
    /// counts its steps live and pauses or resumes it.
    Growth,
}

impl ParamKind {
    pub fn is_numeric(self) -> bool {
        matches!(self, ParamKind::Length | ParamKind::Radius | ParamKind::Diameter | ParamKind::Angle | ParamKind::Count | ParamKind::Number | ParamKind::Ratio | ParamKind::Seed)
    }
    pub fn is_length(self) -> bool {
        matches!(self, ParamKind::Length | ParamKind::Radius | ParamKind::Diameter)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    Num(f64),
    Bool(bool),
    Choice(usize),
    Point(Vec<f64>),
    Profile(Vec<[f64; 2]>),
}

impl Default for ParamValue {
    fn default() -> Self {
        ParamValue::Num(0.0)
    }
}

impl ParamValue {
    pub fn num(&self) -> Option<f64> {
        match self {
            ParamValue::Num(x) => Some(*x),
            _ => None,
        }
    }
}

/// What one drag has learned about how far a parameter goes: the values that built and the ones
/// that did not. A step that does not build is held at the highest value that did build on its side.
#[derive(Clone, Debug, PartialEq)]
pub struct DragCap {
    key: String,
    lo: Option<f64>,
    hi: Option<f64>,
    fail_lo: Option<f64>,
    fail_hi: Option<f64>,
    last: ParamValue,
}

impl DragCap {
    /// A drag of `key` starting at the committed `start` (it built: it is committed).
    pub fn new(key: &str, start: &ParamValue) -> DragCap {
        DragCap { key: key.to_string(), lo: start.num(), hi: start.num(), fail_lo: None, fail_hi: None, last: start.clone() }
    }

    pub fn key(&self) -> &str { &self.key }

    /// `value` built.
    pub fn built(&mut self, value: &ParamValue) {
        if let (Some(lo), Some(hi), ParamValue::Num(x)) = (self.lo, self.hi, value) {
            self.lo = Some(lo.min(*x));
            self.hi = Some(hi.max(*x));
            if self.fail_hi.is_some_and(|f| *x >= f) { self.fail_hi = None; }
            if self.fail_lo.is_some_and(|f| *x <= f) { self.fail_lo = None; }
        } else {
            // A changed kind keeps only the last value that built.
            self.lo = None;
            self.hi = None;
            self.fail_lo = None;
            self.fail_hi = None;
        }
        self.last = value.clone();
    }

    /// `value` did not build: remembers it, returns the value to hold at (`hold_for`).
    pub fn failed(&mut self, value: &ParamValue) -> ParamValue {
        if let (Some(lo), Some(hi), ParamValue::Num(x)) = (self.lo, self.hi, value) {
            if *x > hi {
                self.fail_hi = Some(self.fail_hi.map_or(*x, |f| f.min(*x)));
            } else if *x < lo {
                self.fail_lo = Some(self.fail_lo.map_or(*x, |f| f.max(*x)));
            }
        }
        self.hold_for(value)
    }

    /// The next number between a build and a failure, until no display step fits between them.
    pub fn probe(&self, decimals: usize) -> Option<ParamValue> {
        let (Some(lo), Some(hi)) = (self.lo, self.hi) else { return None };
        let scale = 10.0_f64.powi(decimals as i32);
        let round_to = |x: f64| (x * scale).round() / scale;
        if let Some(f) = self.fail_hi {
            let mid = round_to((hi + f) / 2.0);
            if mid > hi && mid < f { return Some(ParamValue::Num(mid)); }
        }
        if let Some(g) = self.fail_lo {
            let mid = round_to((g + lo) / 2.0);
            if mid > g && mid < lo { return Some(ParamValue::Num(mid)); }
        }
        None
    }

    /// `value` is already known not to build (at or beyond a value that failed): the value to
    /// hold at. `None` when it has to be tried.
    pub fn blocked(&self, value: &ParamValue) -> Option<ParamValue> {
        if let (Some(lo), Some(hi), ParamValue::Num(x)) = (self.lo, self.hi, value) {
            if self.fail_hi.is_some_and(|f| *x >= f) { return Some(ParamValue::Num(hi)); }
            if self.fail_lo.is_some_and(|f| *x <= f) { return Some(ParamValue::Num(lo)); }
        }
        None
    }

    /// `value` is known to build (inside the span of values that did).
    pub fn knows(&self, value: &ParamValue) -> bool {
        if let (Some(lo), Some(hi), ParamValue::Num(x)) = (self.lo, self.hi, value) {
            lo <= *x && *x <= hi
        } else {
            *value == self.last
        }
    }

    /// What to commit on release: the asked value and, if it needs searching, a value that built.
    pub fn commit_plan(&self, value: &ParamValue, decimals: usize) -> (ParamValue, Option<ParamValue>) {
        if self.knows(value) { return (value.clone(), None); }
        if let Some(hold) = self.blocked(value) {
            if self.probe(decimals).is_none() { return (hold, None); }
            let wall = match (self.fail_hi, value) {
                (Some(f), ParamValue::Num(x)) if *x >= f => f,
                _ => self.fail_lo.unwrap(),
            };
            return (ParamValue::Num(wall), Some(hold));
        }
        (value.clone(), Some(self.hold_for(value)))
    }

    /// Where `value` is held when it does not build.
    pub fn hold_for(&self, value: &ParamValue) -> ParamValue {
        if let (Some(lo), Some(hi), ParamValue::Num(x)) = (self.lo, self.hi, value) {
            if *x > hi { return ParamValue::Num(hi); }
            if *x < lo { return ParamValue::Num(lo); }
        }
        self.last.clone()
    }
}

/// A handle in printer millimetres on the plate (see the model's anchors). `rate` is how far
/// the handle moves (mm, degrees for an angle) per unit of value.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamHandle {
    Linear { from: V3, to: V3, rate: f64 },
    Radius { center: V3, axis: V3, radius: f64, rate: f64 },
    Angle { center: V3, axis: V3, from: V3, radius: f64, angle: f64, rate: f64 },
    Point { at: V3, axes: Vec<V3> },
    Profile { origin: V3, u: V3, v: V3, points: Vec<[f64; 2]> },
    Count { at: V3, axis: V3 },
}

impl ParamHandle {
    /// The handle as it stands at value `shown`, from the one built for `built`: what a drag
    /// or a typed value shows every frame, before the part is rebuilt with it (the inverse of
    /// [`drag_value`]).
    pub fn at_value(&self, built: &ParamValue, shown: &ParamValue) -> ParamHandle {
        let mut h = self.clone();
        match (&mut h, built, shown) {
            (ParamHandle::Linear { from, to, rate }, ParamValue::Num(a), ParamValue::Num(b)) => {
                let d = sub(*to, *from);
                let d = if length(d) > 1e-9 { unit(d) } else { [0.0, 0.0, 1.0] };
                *to = add(*to, mul(d, (b - a) * *rate));
            }
            (ParamHandle::Radius { radius, rate, .. }, ParamValue::Num(a), ParamValue::Num(b)) => *radius = (*radius + (b - a) * *rate).max(0.0),
            (ParamHandle::Angle { angle, rate, .. }, ParamValue::Num(a), ParamValue::Num(b)) => *angle += (b - a) * *rate,
            (ParamHandle::Point { at, axes }, ParamValue::Point(a), ParamValue::Point(b)) => {
                for (k, axis) in axes.iter().enumerate() {
                    if let (Some(x), Some(y)) = (a.get(k), b.get(k)) {
                        *at = add(*at, mul(*axis, y - x));
                    }
                }
            }
            (ParamHandle::Profile { points, .. }, _, ParamValue::Profile(b)) => *points = b.clone(),
            _ => {}
        }
        h
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParamView {
    pub key: String,
    pub label: String,
    pub group: String,
    pub kind: ParamKind,
    pub unit: String,
    pub value: ParamValue,
    /// The expression the field follows, as written in its script.
    pub formula: Option<String>,
    /// The row can store a formula that reads declared parameters.
    pub links: bool,
    /// How far a drag reaches; where nothing was declared it scales the drag and limits nothing.
    pub min: f64,
    pub max: f64,
    /// The bounds a value is held to (declared by the script or the builder); a side with none
    /// takes any value.
    pub limits: [Option<f64>; 2],
    pub step: f64,
    pub choices: Vec<String>,
    pub declared: bool,
    pub derived: bool,
    pub detail: bool,
    pub hint: String,
    /// The objects it shapes (a small one's box is a faint hint on hover).
    pub objects: Vec<String>,
    pub handle: Option<ParamHandle>,
    /// The value's bytes in the program.
    pub site: Option<std::ops::Range<usize>>,
    /// The print setting a region's row writes (`m.settings`): one not written (an empty
    /// `site`) shows the part's own value.
    pub setting: Option<&'static str>,
}

/// One object of a part with its box on the plate (highlights, edge snapping, picking a feature).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectBox {
    pub object: String,
    pub min: V3,
    pub max: V3,
    /// Its own origin on the plate: where its `at` places it, what it turns and scales about.
    pub origin: V3,
}

/// A cage's wireframe on the plate: its points, its faces' edges by point index, and each
/// point's row key (a point being dragged moves with its handle; `None`: no row).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wire {
    pub object: String,
    pub points: Vec<V3>,
    pub edges: Vec<[u32; 2]>,
    pub keys: Vec<Option<String>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartParamsView {
    pub object: String,
    /// A line of exact properties at the head of the panel (empty: none).
    pub readout: String,
    pub params: Vec<ParamView>,
    pub boxes: Vec<ObjectBox>,
    /// Its cages, drawn over the surface while the part is selected.
    pub wires: Vec<Wire>,
}

impl PartParamsView {
    pub fn param(&self, key: &str) -> Option<&ParamView> {
        self.params.iter().find(|p| p.key == key)
    }
    pub fn object_box(&self, object: &str) -> Option<&ObjectBox> {
        self.boxes.iter().find(|b| b.object == object)
    }
    /// `object`'s box when it is a small feature, worth a hint in the view: one that spans most
    /// of the part along two axes or more would only frame the part and cover it.
    pub fn feature_box(&self, object: &str) -> Option<&ObjectBox> {
        let b = self.object_box(object)?;
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for o in &self.boxes {
            for k in 0..3 {
                lo[k] = lo[k].min(o.min[k]);
                hi[k] = hi[k].max(o.max[k]);
            }
        }
        let spanned = (0..3).filter(|&k| b.max[k] - b.min[k] >= 0.6 * (hi[k] - lo[k])).count();
        (spanned < 2).then_some(b)
    }
}

/// A value held: what the part panel shows, with an optional refusal flash.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamCap {
    pub key: String,
    pub hold: ParamValue,
    pub flash: bool,
    pub serial: u64,
    /// `Cx::time_now()` when it happened.
    pub at: f64,
}

/// What a part's panel says of a grown feature (`m.grow`): how far it has come, what it waits for
/// or why it stopped. A grown part looks done whatever its run does, so the panel says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrowLine {
    pub object: String,
    pub part: String,
    /// Empty: nothing to say any more.
    pub text: String,
}

/// The parameter editing state the panel, the handles and the app share.
#[derive(Clone, Debug, Default)]
pub struct ParamState {
    /// One per plate part (same order), from the newest plate (a preview's while dragging).
    pub parts: Vec<Arc<PartParamsView>>,
    /// What each part's panel says of its grown features.
    pub grow_lines: Vec<GrowLine>,
    /// The parameter under the pointer (a panel row or a handle): its handle lights up.
    pub hover: Option<String>,
    /// The object whose features the handles show (`None`: the part's declared parameters).
    pub focus: Option<String>,
    /// The value a drag is at (shown before the preview catches up).
    pub dragging: Option<(String, ParamValue)>,
    /// A panel number is pressed or being typed in, before it has set a value.
    pub input_active: bool,
    /// Values set but not yet in a committed plate: every control shows these from the moment
    /// they are set until the rebuilt plate carries them (the display never goes back).
    pub pending: Vec<(String, ParamValue)>,
    /// The newest step that was refused and held (the panel applies each one once, by `serial`).
    pub capped: Option<ParamCap>,
    pub cap_serial: u64,
    /// The part (object) being rebuilt.
    pub rebuilding: Option<Rebuilding>,
    /// The jobs building the model (oldest first; the app's model host keeps them): the
    /// view shows the running one's stage ("Rebuilding… fillets 3/7", "Opening … · meshing").
    pub build_jobs: Vec<makepad_widgets_core::jobs::JobId>,
    /// The tool bodies a parameter cuts or intersects with, at its live value.
    pub ghost: Option<Arc<GhostView>>,
    /// The areas the scripts mark (`m.no_trees`, `m.settings`), as the view shows them: built
    /// off the UI thread after each model build.
    pub areas: Arc<Vec<MarkedArea>>,
    /// Distance-field parts at a dragged value (the program compiled, nothing
    /// meshed): the SDF view draws these until the committed plate has them.
    pub sdf_live: Vec<crate::api::LiveSdf>,
    /// A part's mass and balance while the panel's row for it is open or the AI read it: the
    /// panel's figures and the view's centre-of-mass marker, never shown otherwise.
    pub mass: Option<MassShown>,
    /// A command's value at a pick (Round, Chamfer) while it lasts: its part's handles give way to
    /// its handle.
    pub command: Option<CommandView>,
}

/// A command's one number at a pick (Round, Chamfer): dragged at a handle there or typed in its
/// box, shown as a parameter of the part it is on while the command lasts. Its key
/// (`<instance>/@round`) is no site of the script: the app writes each value through the
/// command's own change.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandView {
    /// The printed object it is on (`instance/object`).
    pub object: String,
    pub param: ParamView,
    /// What the value does, in words by the handle.
    pub says: String,
    /// Moves when a command starts: its box opens once.
    pub rev: u64,
}

/// The centre-of-mass marker the view shows: the AI's read, or the panel's while its Mass and
/// balance row is open on the selected part; nothing otherwise, and nothing while weighing.
pub fn mass_marked(state: &crate::api::AppState) -> Option<&MassFigures> {
    let m = state.params.mass.as_ref()?;
    let selected = crate::ui::parts::active_part(state).and_then(|i| state.params.part(i)).map(|pp| pp.object.as_str());
    (m.ai || m.panel && selected == Some(m.object.as_str())).then_some(())?;
    m.figures.as_ref()
}

/// What `fab.measure {what: "mass"}` answered for a part, as the panel and the view show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MassShown {
    pub object: String,
    /// The Part panel's Mass and balance row is open on it.
    pub panel: bool,
    /// The AI's read points at it (until the model changes, the person's next line or the row
    /// closing).
    pub ai: bool,
    /// None while it is being weighed.
    pub figures: Option<MassFigures>,
}

/// A weighed part, in printer mm (the view's frame) and as the panel's rows say it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MassFigures {
    /// The panel's rows: a label and its value.
    pub rows: Vec<(String, String)>,
    pub com: [f64; 3],
    /// Straight under the centre of mass, on the base it stands on as modelled.
    pub foot: [f64; 3],
    pub base: Vec<[f64; 3]>,
    pub stands: bool,
}

/// An area a script marks: its solid (`instance/solid`), what it is, and its bodies on the
/// plate, one per part of the script, with the rows of its size and place they were built at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkedArea {
    pub object: String,
    pub kind: AreaKind,
    /// It follows a feature (`m.offset(feature, grow)`): its shape is the feature's, so it has no
    /// gizmo; Grow is what sizes it.
    pub follows: bool,
    pub bodies: Vec<GhostBody>,
    pub rows: Vec<AreaRow>,
}

/// One number row of a marked area's own (a size's or a position's coordinate): its key, the
/// value its bodies were built at, and its handle there.
#[derive(Clone, Debug, PartialEq)]
pub struct AreaRow {
    pub key: String,
    pub built: f64,
    pub handle: ParamHandle,
}

impl MarkedArea {
    /// Its bodies as its rows stand now (a value dragged, typed or set and not built yet): each
    /// size stretched along its handle about the handle's foot, then the whole moved by its
    /// position's, this frame, before the plate and its slice catch up.
    pub fn shown_bodies(&self, params: &ParamState) -> Vec<GhostBody> {
        let mut bodies = self.bodies.clone();
        let mut shift = [0.0; 3];
        for row in &self.rows {
            let Some(now) = params.shown_number(&row.key) else { continue };
            let d = now - row.built;
            if d.abs() < 1e-12 {
                continue;
            }
            match &row.handle {
                ParamHandle::Linear { from, to, rate } => {
                    let axis = sub(*to, *from);
                    let len = length(axis);
                    if len < 1e-9 {
                        continue;
                    }
                    let (u, k) = (mul(axis, 1.0 / len), d * rate / len);
                    for p in bodies.iter_mut().flat_map(|b| b.positions.iter_mut()) {
                        *p = add(*p, mul(u, dot(sub(*p, *from), u) * k));
                    }
                }
                ParamHandle::Point { axes, .. } => {
                    for a in axes {
                        shift = add(shift, mul(*a, d));
                    }
                }
                _ => {}
            }
        }
        if length(shift) > 0.0 {
            for p in bodies.iter_mut().flat_map(|b| b.positions.iter_mut()) {
                *p = add(*p, shift);
            }
        }
        bodies
    }
}

/// How a print settings box's size or position row reads on the plate. The box is written in
/// its part's frame (a script's, Y up), but its rows read as the part stands on the plate: Z
/// up, X and Y the plate's, each the way the plate's runs. `label` is the row's name there,
/// `sign` turns the written value into the shown one (and back), `order` sorts the three
/// rows of a field X, Y, Z. A part turned so that none of its axes lines up with the plate's
/// keeps its own names, its up (the script's Y) named height.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateAxis {
    pub label: String,
    pub sign: f64,
    pub order: usize,
}

/// How near (the cosine) a part's axis must run to one of the plate's to take its name.
const ALIGNED: f64 = 0.9;

/// A box's size or position row as it reads on the plate ([`PlateAxis`]); none for a row that
/// is neither (a setting, a count) or has no handle to tell its direction.
pub fn plate_axis(p: &ParamView) -> Option<PlateAxis> {
    let (base, own) = ["X", "Y", "Z"].iter().enumerate().find_map(|(k, a)| Some((p.label.strip_suffix(&format!(" {a}"))?, k)))?;
    let (dir, signed) = match p.handle.as_ref()? {
        ParamHandle::Linear { from, to, .. } => (sub(*to, *from), false),
        ParamHandle::Point { axes, .. } => (axes.iter().fold([0.0; 3], |a, b| add(a, *b)), true),
        _ => return None,
    };
    let len = length(dir);
    if len < 1e-9 {
        return None;
    }
    let j = (0..3).max_by(|&a, &b| dir[a].abs().total_cmp(&dir[b].abs()))?;
    if dir[j].abs() / len >= ALIGNED {
        // (a size is a length, whichever way its handle points)
        let sign = if signed && dir[j] < 0.0 { -1.0 } else { 1.0 };
        return Some(PlateAxis { label: format!("{base} {}", ["X", "Y", "Z"][j]), sign, order: j });
    }
    let label = if own == 1 { format!("{base} height") } else { p.label.clone() };
    Some(PlateAxis { label, sign: 1.0, order: own })
}

/// What a marked area does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AreaKind {
    /// The trees keep out of it (`m.no_trees`).
    #[default]
    NoTrees,
    /// It prints with settings of its own (`m.settings`).
    Settings,
}

/// A tool body at a live value, plate mm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GhostBody {
    /// Cut away (red) or intersected (blue).
    pub subtract: bool,
    pub positions: Vec<V3>,
    pub triangles: Vec<[u32; 3]>,
    /// Its outline: folds and open edges.
    pub edges: Vec<[u32; 2]>,
}

/// The ghost of a parameter's tool bodies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GhostView {
    pub key: String,
    pub bodies: Vec<GhostBody>,
    /// It fades from this time (`Cx::time_now`): the rebuilt part has the cut now.
    pub fade_from: Option<f64>,
}

/// A part whose shape is being recalculated.
#[derive(Clone, Debug, PartialEq)]
pub struct Rebuilding {
    pub object: String,
    /// When it started (`Cx::time_now`), for the indicator's animation.
    pub since: f64,
}

impl ParamState {
    pub fn part(&self, index: usize) -> Option<&Arc<PartParamsView>> {
        self.parts.get(index)
    }

    /// Part `pp`'s parameter `key`, or the command's when it is on that part.
    pub fn param_of<'a>(&'a self, pp: &'a PartParamsView, key: &str) -> Option<&'a ParamView> {
        match &self.command {
            Some(c) if c.param.key == key && c.object == pp.object => Some(&c.param),
            _ => pp.param(key),
        }
    }

    /// A value is being edited, from its press or first keystroke until its rebuild is in.
    pub fn is_editing(&self) -> bool {
        self.input_active || self.dragging.is_some() || !self.pending.is_empty() || self.rebuilding.is_some()
    }

    /// A number row's value as it shows (a drag, a value set and not built, else the newest
    /// plate's): none for a row it does not have or no number.
    pub fn shown_number(&self, key: &str) -> Option<f64> {
        let p = self.parts.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key == key)?;
        match self.shown_value(p) {
            ParamValue::Num(x) => Some(x),
            _ => None,
        }
    }

    /// What a parameter shows: a drag in progress, else a value set and not yet built, else
    /// the model's.
    pub fn shown_value(&self, p: &ParamView) -> ParamValue {
        if let Some((k, v)) = &self.dragging {
            if *k == p.key {
                return v.clone();
            }
        }
        self.pending.iter().rev().find(|(k, _)| *k == p.key).map(|(_, v)| v.clone()).unwrap_or_else(|| p.value.clone())
    }

    /// A part's parameters as they show: each value set but not yet built (a drag, a commit
    /// rebuilding) with its handle moved to it ([`ParamHandle::at_value`]). What the headset
    /// draws and grabs, so a held handle stays with the hand before the rebuild is in.
    pub fn shown_part(&self, pp: &PartParamsView) -> PartParamsView {
        let mut out = pp.clone();
        for p in &mut out.params {
            let v = self.shown_value(p);
            if v != p.value {
                p.handle = p.handle.as_ref().map(|h| h.at_value(&p.value, &v));
                p.value = v;
            }
        }
        out
    }

    /// Remember a value set (newest wins).
    pub fn set_pending(&mut self, key: &str, value: ParamValue) {
        self.pending.retain(|(k, _)| k != key);
        self.pending.push((key.to_string(), value));
    }

    /// Hold a drag at a value that built, flashing only when a pointer step was refused.
    pub fn cap(&mut self, key: &str, hold: ParamValue, now: f64, flash: bool) {
        if let Some((k, value)) = &mut self.dragging {
            if k.as_str() == key { *value = hold.clone(); }
        }
        self.set_pending(key, hold.clone());
        self.cap_serial += 1;
        self.capped = Some(ParamCap { key: key.to_string(), hold, flash, serial: self.cap_serial, at: now });
    }

    /// A commit was refused: flash the control without changing dragging or pending values.
    pub fn refuse(&mut self, key: &str, hold: ParamValue, now: f64) {
        self.cap_serial += 1;
        self.capped = Some(ParamCap { key: key.to_string(), hold, flash: true, serial: self.cap_serial, at: now });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamAction {
    Hover(Option<String>),
    /// The panel's Mass and balance row opened (weigh `object`) or closed.
    Weigh { object: String, open: bool },
    /// Show the handles of `object`'s features (`None`: back to the part's own parameters).
    Focus(Option<String>),
    /// A live value while dragging: evaluated, not committed.
    /// A step that does not build is capped at a value that did and flashes the control red.
    Preview { object: String, key: String, value: ParamValue },
    /// The edit: one undo step.
    /// A refused commit flashes the control at the held value without committing it.
    Commit { object: String, key: String, value: ParamValue },
    /// A typed formula and its pending value in the row's units.
    Link { object: String, key: String, code: String, value: f64 },
    /// A drag was cancelled: back to the committed plate.
    Cancel { object: String },
    /// The command at a pick ends (Esc): its handle goes, its value as committed.
    EndCommand,
    /// A feature of part `object` (`feature`, its `instance/solid`) moved, turned or scaled by
    /// the view's gizmo, from where the drag began: its placement in its script, shown live like
    /// a value while `done` is false, one undo step when let go.
    Place { object: String, feature: String, gesture: PlaceGesture, done: bool },
}

/// What the view's gizmo does to a feature, on the plate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaceGesture {
    /// Moved by this much (mm).
    Move(V3),
    /// Turned about the vertical through its origin (degrees, counter-clockwise from above).
    Turn(f64),
    /// Scaled about its origin.
    Scale(f64),
}

// ===========================================================================
// Formatting and snapping
// ===========================================================================

/// Decimals to show for a value of `kind` with `step`.
pub fn precision(kind: ParamKind, step: f64) -> usize {
    match kind {
        ParamKind::Count | ParamKind::Seed | ParamKind::Growth | ParamKind::Bool | ParamKind::Choice => 0,
        // a rate in thousandths (a pattern's feed and kill)
        ParamKind::Number if step > 0.0 && step < 0.01 => 3,
        ParamKind::Angle => {
            if step < 1.0 {
                1
            } else {
                0
            }
        }
        ParamKind::Ratio => 2,
        _ => {
            if step < 0.1 {
                2
            } else {
                1
            }
        }
    }
}

pub fn format_value(p: &ParamView, value: &ParamValue) -> String {
    let prec = precision(p.kind, p.step);
    let unit = if p.unit.is_empty() { String::new() } else if p.unit == "°" { "°".into() } else { format!(" {}", p.unit) };
    match value {
        ParamValue::Num(x) => format!("{x:.prec$}{unit}"),
        ParamValue::Bool(b) => (if *b { "on" } else { "off" }).into(),
        ParamValue::Choice(i) => p.choices.get(*i).cloned().unwrap_or_default(),
        ParamValue::Point(c) => c.iter().map(|x| format!("{x:.prec$}")).collect::<Vec<_>>().join(", ") + &unit,
        ParamValue::Profile(pts) => format!("{} points", pts.len()),
    }
}

/// Round decimal numbers with extra fractional digits, leaving names and other text alone.
pub fn round_numbers(text: &str, decimals: usize) -> String {
    let bytes = text.as_bytes();
    let joined = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
        if bytes.get(i) != Some(&b'.') { continue; }
        i += 1;
        let fraction = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
        if i - fraction <= decimals
            || text[..start].chars().next_back().is_some_and(joined)
            || text[i..].chars().next().is_some_and(joined)
        {
            continue;
        }
        if let Ok(x) = text[start..i].parse::<f64>() {
            out.push_str(&text[from..start]);
            out.push_str(&format!("{:.*}", decimals, x));
            from = i;
        }
    }
    out.push_str(&text[from..]);
    out
}

/// Decimals kept in a build failure shown where no parameter's own precision applies (the code
/// panel, a tool's answer): a kernel error carries every digit of its floats.
pub const ERROR_DECIMALS: usize = 3;

/// The reason for a build failure, without its wrapper, source location, solid or shape.
pub fn failure_reason(text: &str) -> String {
    let mut text = text.trim();
    text = text.strip_prefix("the project would not build, nothing committed: ").unwrap_or(text);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((location, rest)) = text.split_once(": ") {
        let mut fields = location.rsplitn(3, ':');
        if let (Some(column), Some(line), Some(file)) = (fields.next(), fields.next(), fields.next()) {
            if !file.is_empty() && digits(line) && digits(column) { text = rest; }
        }
    }
    if let Some((solid, rest)) = text.split_once(": ") {
        if solid.contains(" (") && solid.ends_with(')') { text = rest; }
    }
    if let Some(shape) = text.strip_prefix("shape ") {
        if let Some((index, rest)) = shape.split_once(": ") {
            if digits(index) { text = rest; }
        }
    }
    text.trim().to_string()
}

/// The held value and the build failure, at the parameter's displayed precision.
pub fn cap_message(p: &ParamView, hold: &ParamValue, failure: &str) -> String {
    format!("{} capped at {}: {}", p.label, format_value(p, hold), round_numbers(&failure_reason(failure), precision(p.kind, p.step)))
}

/// The round-value increment a drag snaps to: whole millimetres for lengths over 10 mm,
/// halves below, 15° (5° under 30°) for angles, whole counts. `fine` (Shift) drops to the step.
pub fn snap_step(p: &ParamView, value: f64, fine: bool) -> f64 {
    if fine {
        return if matches!(p.kind, ParamKind::Count | ParamKind::Seed) { 1.0 } else { p.step.max(1e-6) };
    }
    match p.kind {
        ParamKind::Count | ParamKind::Seed => 1.0,
        // a small rate moves by its own step
        ParamKind::Number if p.step > 0.0 && p.step < 0.1 => p.step,
        ParamKind::Angle => {
            if value.abs() < 30.0 {
                5.0
            } else {
                15.0
            }
        }
        ParamKind::Ratio => 0.05,
        _ => {
            if value.abs() >= 10.0 {
                1.0
            } else if value.abs() >= 2.0 {
                0.5
            } else {
                0.1
            }
        }
    }
}

pub fn snap(value: f64, inc: f64) -> f64 {
    if inc > 0.0 {
        (value / inc).round() * inc
    } else {
        value
    }
}

pub fn clamp_value(p: &ParamView, x: f64) -> f64 {
    let x = if matches!(p.kind, ParamKind::Count | ParamKind::Seed) { x.round() } else { x };
    if p.max > p.min {
        x.clamp(p.min, p.max)
    } else {
        x
    }
}

/// A value as the parameter takes it from a typed number or a box: a count whole, held to its
/// declared limits only. A drag's reach (`min`, `max`) limits a drag, never a typed number.
pub fn within_limits(p: &ParamView, x: f64) -> f64 {
    let x = if matches!(p.kind, ParamKind::Count | ParamKind::Seed) { x.round() } else { x };
    let [lo, hi] = p.limits;
    x.max(lo.unwrap_or(f64::NEG_INFINITY)).min(hi.unwrap_or(f64::INFINITY))
}

/// The formula sent with an editor's Ended action when its row accepts links.
pub(crate) fn typed_formula(p: &ParamView, actions: &makepad_widgets_core::Actions, uid: makepad_widgets_core::WidgetUid) -> Option<(f64, String)> {
    use makepad_widgets_core::{ValueInputAction, WidgetActionsApi};
    if !p.links { return None; }
    actions.filter_widget_actions_cast::<ValueInputAction>(uid).filter_map(|action| match action {
        ValueInputAction::Formula { value, script } => Some((value, script)),
        _ => None,
    }).last()
}

/// `p`'s `min` and `max` are only a drag's reach: its box takes a typed value past them.
pub fn reach_only(p: &ParamView) -> bool {
    p.max > p.min && p.limits != [Some(p.min), Some(p.max)]
}

/// The names a part's value boxes read: its declared parameters, as its script holds them
/// (metres, radians, a fraction), so `wall * 2` typed in a box means what it means in the script.
pub fn named_values(pp: &PartParamsView) -> Arc<[makepad_script::syntax::quantity::NamedValue]> {
    use makepad_script::syntax::quantity::{Measure, NamedValue};
    pp.params
        .iter()
        .filter(|p| p.declared)
        .filter_map(|p| {
            let x = p.value.num()?;
            let name = p.key.rsplit('/').next().unwrap_or(&p.key).to_string();
            let (value, measure) = match p.kind {
                ParamKind::Length | ParamKind::Radius | ParamKind::Diameter => (x / 1000.0, Measure::Length),
                ParamKind::Angle => (x.to_radians(), Measure::Angle),
                ParamKind::Ratio => (x, Measure::Fraction),
                ParamKind::Count | ParamKind::Number | ParamKind::Seed => (x, Measure::Plain),
                _ => return None,
            };
            Some(NamedValue { name, label: p.label.clone(), value, measure })
        })
        .collect()
}

/// The snap points a value passes between `a` and `b` (for haptic ticks).
pub fn ticks_between(a: f64, b: f64, inc: f64) -> usize {
    if inc <= 0.0 {
        return 0;
    }
    ((a / inc).floor() - (b / inc).floor()).abs() as usize
}

// ===========================================================================
// Vector helpers and the drag math (pure; the desktop and the headset share it)
// ===========================================================================

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn mul(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn length(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub fn unit(a: V3) -> V3 {
    let l = length(a);
    if l > 1e-12 {
        mul(a, 1.0 / l)
    } else {
        [0.0, 0.0, 1.0]
    }
}
pub fn perp(a: V3) -> V3 {
    let t = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    unit(cross(a, t))
}
/// `d` turned by `radians` about the unit `axis` (right-handed).
pub fn rotate(d: V3, axis: V3, radians: f64) -> V3 {
    let k = unit(axis);
    let (s, c) = radians.sin_cos();
    let kd = dot(k, d);
    let x = cross(k, d);
    std::array::from_fn(|i| d[i] * c + x[i] * s + k[i] * kd * (1.0 - c))
}

/// A pointer: a ray (desktop) or a hand point (headset: `dir` zero).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragPointer {
    pub origin: V3,
    pub dir: V3,
}

impl DragPointer {
    pub fn ray(origin: V3, dir: V3) -> DragPointer {
        DragPointer { origin, dir: unit(dir) }
    }
    pub fn point(at: V3) -> DragPointer {
        DragPointer { origin: at, dir: [0.0; 3] }
    }
    fn is_point(&self) -> bool {
        self.dir == [0.0; 3]
    }

    /// The parameter along the line `p + t*d` closest to the pointer.
    pub fn along(&self, p: V3, d: V3) -> Option<f64> {
        let d = unit(d);
        if self.is_point() {
            return Some(dot(sub(self.origin, p), d));
        }
        // Closest points between two lines.
        let w = sub(p, self.origin);
        let (a, b, c) = (dot(d, d), dot(d, self.dir), dot(self.dir, self.dir));
        let (e, f) = (dot(d, w), dot(self.dir, w));
        let den = a * c - b * b;
        if den.abs() < 1e-9 {
            return None;
        }
        Some((b * f - c * e) / den)
    }

    /// Where the pointer meets the plane through `p` with normal `n` (a hand point projects onto it).
    pub fn on_plane(&self, p: V3, n: V3) -> Option<V3> {
        let n = unit(n);
        if self.is_point() {
            return Some(sub(self.origin, mul(n, dot(sub(self.origin, p), n))));
        }
        let den = dot(self.dir, n);
        if den.abs() < 1e-6 {
            return None;
        }
        let t = dot(sub(p, self.origin), n) / den;
        (t > 0.0).then(|| add(self.origin, mul(self.dir, t)))
    }
}

/// The plane normal a point handle drags in: of the planes its axes span, the one that faces
/// `view` (towards the eye) most.
pub fn point_plane(axes: &[V3], view: V3) -> (usize, usize) {
    let n = axes.len().min(3);
    if n < 2 {
        return (0, 0);
    }
    let mut best = (0, 1, -1.0);
    for i in 0..n {
        for j in i + 1..n {
            let f = dot(unit(cross(axes[i], axes[j])), unit(view)).abs();
            if f > best.2 {
                best = (i, j, f);
            }
        }
    }
    (best.0, best.1)
}

/// Solve `d = a*u + b*v` in the least-squares sense.
pub fn solve2(d: V3, u: V3, v: V3) -> Option<(f64, f64)> {
    let (uu, uv, vv) = (dot(u, u), dot(u, v), dot(v, v));
    let (du, dv) = (dot(d, u), dot(d, v));
    let det = uu * vv - uv * uv;
    if det.abs() < 1e-12 {
        return None;
    }
    Some(((du * vv - dv * uv) / det, (dv * uu - du * uv) / det))
}

/// Which part of a handle a drag holds (a profile point's index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HandleGrip {
    pub point: usize,
}

/// The value a handle drag gives when the pointer is at `ptr`, relative to the handle as last
/// drawn (`now` = the value it was built for). `view` points towards the eye (for point planes).
/// `None` when the pointer is parallel to the drag. Numbers are unsnapped.
pub fn drag_value(handle: &ParamHandle, now: &ParamValue, grip: HandleGrip, ptr: &DragPointer, view: V3) -> Option<ParamValue> {
    match (handle, now) {
        (ParamHandle::Linear { from, to, rate }, ParamValue::Num(v)) => {
            let d = sub(*to, *from);
            let s = ptr.along(*to, if length(d) > 1e-9 { d } else { [0.0, 0.0, 1.0] })?;
            (rate.abs() > 1e-12).then(|| ParamValue::Num(v + s / rate))
        }
        (ParamHandle::Radius { center, axis, radius, rate }, ParamValue::Num(v)) => {
            let hit = ptr.on_plane(*center, *axis)?;
            let r = length(sub(hit, *center));
            (rate.abs() > 1e-12).then(|| ParamValue::Num(v + (r - radius) / rate))
        }
        (ParamHandle::Angle { center, axis, from, angle, rate, .. }, ParamValue::Num(v)) => {
            let hit = ptr.on_plane(*center, *axis)?;
            let d = sub(hit, *center);
            if length(d) < 1e-9 {
                return None;
            }
            let x = unit(*from);
            let y = cross(unit(*axis), x);
            let a = dot(d, y).atan2(dot(d, x)).to_degrees();
            let mut delta = a - angle;
            while delta > 180.0 {
                delta -= 360.0;
            }
            while delta < -180.0 {
                delta += 360.0;
            }
            (rate.abs() > 1e-12).then(|| ParamValue::Num(v + delta / rate))
        }
        (ParamHandle::Point { at, axes }, ParamValue::Point(c)) => {
            let mut out = c.clone();
            if axes.len() == 1 {
                let s = ptr.along(*at, axes[0])?;
                if let Some(x) = out.get_mut(0) {
                    *x += s / length(axes[0]).max(1e-12);
                }
                return Some(ParamValue::Point(out));
            }
            let (i, j) = point_plane(axes, view);
            let hit = ptr.on_plane(*at, cross(axes[i], axes[j]))?;
            let (a, b) = solve2(sub(hit, *at), axes[i], axes[j])?;
            if let Some(x) = out.get_mut(i) {
                *x += a;
            }
            if let Some(x) = out.get_mut(j) {
                *x += b;
            }
            Some(ParamValue::Point(out))
        }
        (ParamHandle::Profile { origin, u, v, .. }, ParamValue::Profile(pts)) => {
            let hit = ptr.on_plane(*origin, cross(*u, *v))?;
            let (a, b) = solve2(sub(hit, *origin), *u, *v)?;
            let mut out = pts.clone();
            *out.get_mut(grip.point)? = [a, b];
            Some(ParamValue::Profile(out))
        }
        _ => None,
    }
}

/// The plate point of profile point `i`.
pub fn profile_point(origin: V3, u: V3, v: V3, p: [f64; 2]) -> V3 {
    add(origin, add(mul(u, p[0]), mul(v, p[1])))
}

/// Where a handle is held: the arrow tip, a ring point facing `view`, the arc end, the point.
pub fn grip_point(handle: &ParamHandle, grip: HandleGrip, view: V3) -> V3 {
    match handle {
        ParamHandle::Linear { to, .. } => *to,
        ParamHandle::Radius { center, axis, radius, .. } => {
            // The ring point nearest the eye's side, so the grip is never behind the part.
            let side = sub(view, mul(unit(*axis), dot(view, unit(*axis))));
            let dir = if length(side) > 1e-6 { unit(side) } else { perp(*axis) };
            add(*center, mul(dir, *radius))
        }
        ParamHandle::Angle { center, axis, from, radius, angle, .. } => add(*center, mul(rotate(unit(*from), *axis, angle.to_radians()), radius.max(1.0))),
        ParamHandle::Point { at, .. } | ParamHandle::Count { at, .. } => *at,
        ParamHandle::Profile { origin, u, v, points } => points.get(grip.point).map(|p| profile_point(*origin, *u, *v, *p)).unwrap_or(*origin),
    }
}

/// Snap a linear drag's tip to the box edges of other objects: the snapped value when the
/// tip lands within `tol` mm (along the arrow) of a box face plane.
pub fn edge_snap(handle: &ParamHandle, now: f64, target: f64, boxes: &[ObjectBox], skip: &[String], tol: f64) -> Option<f64> {
    let ParamHandle::Linear { from, to, rate } = handle else { return None };
    if rate.abs() < 1e-12 {
        return None;
    }
    let d = unit(sub(*to, *from));
    let tip = add(*to, mul(d, (target - now) * rate));
    let mut best: Option<(f64, f64)> = None;
    for b in boxes.iter().filter(|b| !skip.contains(&b.object)) {
        for k in 0..3 {
            if d[k].abs() < 0.999 {
                continue;
            }
            for face in [b.min[k], b.max[k]] {
                let off = (face - tip[k]) * d[k].signum();
                if off.abs() <= tol && best.is_none_or(|(o, _)| off.abs() < o) {
                    best = Some((off.abs(), target + off / rate));
                }
            }
        }
    }
    best.map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box written in a script's frame (Y up) on a part standing as written: its Size Y is the
    /// plate's height (Z), its Size Z the plate's Y; its Position Z runs the plate's Y the other
    /// way, so it shows negated. On a part turned off the plate's axes its rows keep their own
    /// names, the script's up named height.
    #[test]
    fn a_box_row_reads_in_the_plate_axes() {
        let row = |label: &str, handle: ParamHandle| ParamView { label: label.into(), handle: Some(handle), ..Default::default() };
        let size = |to: V3| ParamHandle::Linear { from: [10.0, 10.0, 10.0], to, rate: 0.5 };
        let at = |axis: V3| ParamHandle::Point { at: [10.0, 10.0, 10.0], axes: vec![axis, [0.0; 3], [0.0; 3]] };
        let read = |p: ParamView| plate_axis(&p).map(|a| (a.label, a.sign, a.order));
        assert_eq!(read(row("Size X", size([15.0, 10.0, 10.0]))), Some(("Size X".into(), 1.0, 0)));
        assert_eq!(read(row("Size Y", size([10.0, 10.0, 15.0]))), Some(("Size Z".into(), 1.0, 2)));
        assert_eq!(read(row("Size Z", size([10.0, 5.0, 10.0]))), Some(("Size Y".into(), 1.0, 1)));
        assert_eq!(read(row("Position Z", at([0.0, -1.0, 0.0]))), Some(("Position Y".into(), -1.0, 1)));
        assert_eq!(read(row("Position Y", at([0.0, 0.0, 1.0]))), Some(("Position Z".into(), 1.0, 2)));
        // turned 45 degrees about the plate's X: Y and Z run between the plate's
        let h = std::f64::consts::FRAC_1_SQRT_2;
        assert_eq!(read(row("Size Y", size([10.0, 10.0 - 5.0 * h, 10.0 + 5.0 * h]))), Some(("Size height".into(), 1.0, 1)));
        assert_eq!(read(row("Size Z", size([10.0, 10.0 + 5.0 * h, 10.0 + 5.0 * h]))), Some(("Size Z".into(), 1.0, 2)));
        assert_eq!(read(row("Density", size([15.0, 10.0, 10.0]))), None, "a setting's row");
    }

    #[test]
    fn linear_drag_follows_a_ray_and_a_hand() {
        // An arrow along +Z at the top of a 70 mm part whose tip moves 1 mm per mm of value.
        let h = ParamHandle::Linear { from: [0.0, 0.0, 35.0], to: [0.0, 0.0, 70.0], rate: 1.0 };
        let now = ParamValue::Num(70.0);
        // A ray from the side at Z 80.
        let ray = DragPointer::ray([100.0, 0.0, 80.0], [-1.0, 0.0, 0.0]);
        assert_eq!(drag_value(&h, &now, HandleGrip::default(), &ray, [1.0, 0.0, 0.0]), Some(ParamValue::Num(80.0)));
        // A hand 4 mm below the tip; a centred size (rate 0.5) needs twice the value change.
        let hand = DragPointer::point([3.0, 2.0, 66.0]);
        assert_eq!(drag_value(&h, &now, HandleGrip::default(), &hand, [1.0, 0.0, 0.0]), Some(ParamValue::Num(66.0)));
        let half = ParamHandle::Linear { from: [0.0, 0.0, 35.0], to: [0.0, 0.0, 70.0], rate: 0.5 };
        assert_eq!(drag_value(&half, &now, HandleGrip::default(), &hand, [1.0, 0.0, 0.0]), Some(ParamValue::Num(62.0)));
    }

    #[test]
    fn rings_arcs_points_and_profiles() {
        let ring = ParamHandle::Radius { center: [0.0; 3], axis: [0.0, 0.0, 1.0], radius: 10.0, rate: 1.0 };
        let down = DragPointer::ray([12.0, 0.0, 50.0], [0.0, 0.0, -1.0]);
        assert_eq!(drag_value(&ring, &ParamValue::Num(10.0), HandleGrip::default(), &down, [0.0, 0.0, 1.0]), Some(ParamValue::Num(12.0)));
        let arc = ParamHandle::Angle { center: [0.0; 3], axis: [0.0, 0.0, 1.0], from: [1.0, 0.0, 0.0], radius: 10.0, angle: 0.0, rate: 1.0 };
        let at90 = DragPointer::ray([0.0, 5.0, 50.0], [0.0, 0.0, -1.0]);
        let ParamValue::Num(a) = drag_value(&arc, &ParamValue::Num(0.0), HandleGrip::default(), &at90, [0.0, 0.0, 1.0]).unwrap() else { panic!() };
        assert!((a - 90.0).abs() < 1e-9);
        let pt = ParamHandle::Point { at: [1.0, 2.0, 3.0], axes: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        // Looking down: the XY plane; Z keeps.
        let ParamValue::Point(c) = drag_value(&pt, &ParamValue::Point(vec![1.0, 2.0, 3.0]), HandleGrip::default(), &DragPointer::ray([4.0, 6.0, 9.0], [0.0, 0.0, -1.0]), [0.0, 0.0, 1.0]).unwrap() else { panic!() };
        assert_eq!(c, vec![4.0, 6.0, 3.0]);
        let prof = ParamHandle::Profile { origin: [0.0, 0.0, 5.0], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]] };
        let now = ParamValue::Profile(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
        let got = drag_value(&prof, &now, HandleGrip { point: 2 }, &DragPointer::ray([2.0, 3.0, 9.0], [0.0, 0.0, -1.0]), [0.0, 0.0, 1.0]);
        assert_eq!(got, Some(ParamValue::Profile(vec![[0.0, 0.0], [1.0, 0.0], [2.0, 3.0]])));
    }

    #[test]
    fn snapping_rounds_and_meets_edges() {
        let p = ParamView { kind: ParamKind::Length, step: 0.1, ..Default::default() };
        assert_eq!(snap(13.37, snap_step(&p, 13.37, false)), 13.0);
        assert!((snap(13.37, snap_step(&p, 13.37, true)) - 13.4).abs() < 1e-9);
        assert_eq!(ticks_between(9.6, 12.2, 1.0), 3);
        let h = ParamHandle::Linear { from: [0.0, 0.0, 0.0], to: [0.0, 0.0, 10.0], rate: 1.0 };
        let boxes = vec![ObjectBox { object: "lid".into(), min: [0.0, 0.0, 12.0], max: [5.0, 5.0, 14.0], origin: [2.5, 2.5, 13.0] }];
        assert_eq!(edge_snap(&h, 10.0, 11.7, &boxes, &[], 0.5), Some(12.0));
        assert_eq!(edge_snap(&h, 10.0, 11.0, &boxes, &[], 0.5), None);
        assert_eq!(edge_snap(&h, 10.0, 11.7, &boxes, &["lid".into()], 0.5), None);
    }
}
