//! The vocabulary: every part a model program builds (`m.box({..})`,
//! `m.subtract(body, bolts)`) and every value type it writes (sketch shapes,
//! path segments, handles), with their fields.
//!
//! Numeric ranges are editor slider hints, not authoring limits.
//! One table serves the lowering (which fields are lengths, angles, counts),
//! the value sites (kind, range and step of each literal), the guide the AI
//! reads and the editor's Add menu. Lengths are metres and angles radians, as
//! the VM holds them (`150mm`, `90°`); a host shows them in its own unit.

/// What a field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Length,
    Radius,
    Diameter,
    Angle,
    Count,
    Number,
    /// A factor (shown in percent).
    Ratio,
    Bool,
    /// One of these names (`@xz`).
    Choice(&'static [&'static str]),
    /// [x y z] lengths.
    Point,
    /// [u v] lengths.
    Point2,
    /// [[u v] ...] lengths.
    Points2,
    /// [[x y z] ...] lengths.
    Points,
    /// [[i j k l] ...]: faces, each its points' indices.
    Faces,
    /// [x y z] angles.
    Rotation,
    /// A number (uniform) or [x y z].
    Scale,
    /// A sketch: `Sketch{shapes}`, one shape, or a list of shapes.
    Sketch,
    /// A list of path segments.
    Segments,
    Solid,
    /// One solid or a list of them.
    Solids,
    Text,
    /// This records optical throughput and whether it is estimated or measured.
    Optics,
    /// These records name the spaces on boards that a solid keeps clear of other parts.
    Keepouts,
    /// `@all`, `{top: true ...}` or a list of those.
    Edges,
    /// `[Section{sketch at} ...]`.
    Sections,
    /// `[LoftRing{at u v points crease} ...]`.
    Rings,
    /// `Helix{radius pitch turns}`.
    Helix,
    /// [[min] [max]] lengths.
    Bounds,
    /// A distance function `fn(p) { ... }`.
    Field,
    /// [r g b], each 0..1.
    Color,
    /// `Material{..}`.
    Material,
    /// `[Material{..} ...]`.
    Materials,
    /// `Splat{..}`.
    Splat,
    /// `Place{at rotate scale}`.
    Place,
    /// `[Place{..} ...]` or `[[x y z] ...]`.
    Places,
    /// A surface pattern: `Knurl{..}`, `Hex{..}`, `Noise{..}` or `Pattern{..}`.
    Pattern,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Length => "length",
            Kind::Radius => "radius",
            Kind::Diameter => "diameter",
            Kind::Angle => "angle",
            Kind::Count => "count",
            Kind::Number => "number",
            Kind::Ratio => "ratio",
            Kind::Bool => "bool",
            Kind::Choice(_) => "choice",
            Kind::Point | Kind::Point2 => "point",
            Kind::Points2 | Kind::Points => "points",
            Kind::Faces => "faces",
            Kind::Rotation => "rotation",
            Kind::Scale => "scale",
            Kind::Sketch => "sketch",
            Kind::Segments => "segments",
            Kind::Solid => "solid",
            Kind::Solids => "solids",
            Kind::Text => "text",
            Kind::Optics => "optics",
            Kind::Keepouts => "keepouts",
            Kind::Edges => "edges",
            Kind::Sections => "sections",
            Kind::Rings => "rings",
            Kind::Helix => "helix",
            Kind::Bounds => "bounds",
            Kind::Field => "field",
            Kind::Color => "color",
            Kind::Material => "material",
            Kind::Materials => "materials",
            Kind::Splat => "splat",
            Kind::Place => "place",
            Kind::Places => "places",
            Kind::Pattern => "pattern",
        }
    }

    /// Lengths (each coordinate of a point is one).
    pub fn is_length(self) -> bool {
        matches!(self, Kind::Length | Kind::Radius | Kind::Diameter | Kind::Point | Kind::Point2 | Kind::Points2 | Kind::Points | Kind::Bounds)
    }

    pub fn is_angle(self) -> bool {
        matches!(self, Kind::Angle | Kind::Rotation)
    }
}

/// One field of a prototype.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// Soft range and step in the VM's units (metres, radians); 0..0: none.
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub help: &'static str,
}

/// Which group a prototype is listed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Primitive,
    /// Solids from sketches (extrude, revolve, sweep, loft).
    Generator,
    /// Sketches and their parts (not solids).
    Sketch,
    /// Handles in the view for a parameter (not solids).
    Handle,
    Shape,
    Segment,
    Combine,
    Modify,
    Mechanical,
    Text,
    Field,
    /// Materials, splat looks and placements (values, not solids).
    Look,
    /// Joints between two parts (not solids: they change the parts they join).
    Joint,
    /// Free interlocking links printed together.
    Weave,
    /// Exact bodies of the B-rep engine (`m.brep.box(..)`).
    Brep,
    /// Cuts that lighten a part and still print: rows of holes, pockets.
    Lighten,
    /// Where a force study holds and loads the part (`m.fixed`, `m.force`; values, not solids).
    Study,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::Primitive => "primitive",
            Group::Generator => "generator",
            Group::Sketch => "sketch",
            Group::Shape => "shape",
            Group::Segment => "segment",
            Group::Combine => "combine",
            Group::Modify => "modify",
            Group::Mechanical => "mechanical",
            Group::Text => "text",
            Group::Field => "field",
            Group::Handle => "handle",
            Group::Look => "look",
            Group::Joint => "joint",
            Group::Weave => "weave",
            Group::Brep => "exact",
            Group::Lighten => "lighten",
            Group::Study => "study",
        }
    }

    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Group::Brep => &["brep"],
            _ => &[],
        }
    }
}

/// One prototype.
#[derive(Clone, Copy, Debug)]
pub struct Proto {
    pub name: &'static str,
    /// The builder call that makes this part (`box` for `m.box(..)`); empty for value types.
    pub call: &'static str,
    /// The fields the call takes in order before its options object
    /// (`m.subtract(a, b)`, `m.fillet(source, radius, {edges})`).
    pub args: &'static [&'static str],
    pub group: Group,
    pub help: &'static str,
    pub fields: &'static [Field],
    /// A complete example (Splash), for the guide and the editor's Add menu.
    pub example: &'static str,
}

impl Proto {
    /// How a program calls it, from `m`.
    pub fn path(&self) -> String {
        if self.group == Group::Brep { format!("brep.{}", self.call) } else { self.call.to_string() }
    }

    pub fn field(&self, key: &str) -> Option<&'static Field> {
        self.fields.iter().find(|f| f.key == key).or_else(|| if self.is_solid() { PLACEMENT.iter().find(|f| f.key == key) } else { None })
    }

    /// Builds a solid (takes `at`, `rotate`, `scale`).
    pub fn is_solid(&self) -> bool {
        !matches!(self.group, Group::Sketch | Group::Shape | Group::Segment | Group::Handle | Group::Look | Group::Joint | Group::Study)
    }
}

const MM: f64 = 0.001;
/// The faces a picture prints its picture on.
pub const PICTURE_FACES: &[&str] = &["down", "up"];
/// A Turing pattern's presets (the host's pattern module grows them).
pub const TURING_PRESETS: &[&str] = &["spots", "stripes", "labyrinth", "coral", "mitosis", "dots"];
/// A Lichtenberg figure's presets (the host's pattern module grows them).
pub const LICHTENBERG_PRESETS: &[&str] = &["tree", "circuit", "frost"];
/// Scales' presets (the host's pattern module grows them).
pub const SCALES_PRESETS: &[&str] = &["spiral", "even"];
/// A slime mould's presets (the host's pattern module grows them).
pub const PHYSARUM_PRESETS: &[&str] = &["network", "trunks", "lace"];
/// Flow lines' presets (the host's pattern module grows them).
pub const FLOW_PRESETS: &[&str] = &["streaks", "lines", "contours"];
/// The protos a Mix's field may be instead of a fn: patterns the host grows on the part.
pub const GROWN: &[&str] = &["Turing", "Lichtenberg", "Scales", "Physarum", "Flow"];
const DEG: f64 = std::f64::consts::PI / 180.0;

const fn f(key: &'static str, label: &'static str, kind: Kind, required: bool, min: f64, max: f64, step: f64, help: &'static str) -> Field {
    Field { key, label, kind, required, min, max, step, help }
}
const fn o(key: &'static str, label: &'static str, kind: Kind, required: bool, help: &'static str) -> Field {
    Field { key, label, kind, required, min: 0.0, max: 0.0, step: 0.0, help }
}

/// Fields every solid takes.
pub const PLACEMENT: &[Field] = &[
    o("link", "Host key", Kind::Text, false, "opaque host key attached by m.link(part, key)"),
    o("label", "Label", Kind::Text, false, "the name a person reads for this body where its object lists several bodies: object([lid, pipe], name: \"Lid\") lists a pipe with label: \"Light pipe D1\" so"),
    o("optics", "Optics", Kind::Optics, false, "{throughput: 0..1 source: @estimate | @measured}: the share of source flux leaving the exit face; source defaults to @estimate"),
    o("keepouts", "Board keepouts", Kind::Keepouts, false, "[{board: \"unit name\" name: \"thing\" area: [[x, y], ..] z: [low, high]}]: space kept clear of other parts, in the board's own frame and above its top surface"),
    o("frame", "Host frame", Kind::Place, false, "Frame{name at rotate}: a named frame the host reads, in the part's own frame (it moves with the part's placement)"),
    o("off", "Bypass", Kind::Bool, false, "omit this operand from its boolean; a bypassed boolean returns its first operand (default: false)"),
    o("at", "Position", Kind::Point, false, "moves the result: [x y z]"),
    f("rotate", "Rotation", Kind::Rotation, false, -2.0 * std::f64::consts::PI, 2.0 * std::f64::consts::PI, DEG, "turns it about X, then Y, then Z: [90° 0 0]"),
    f("scale", "Scale", Kind::Scale, false, 0.01, 100.0, 0.01, "a factor, or [x y z]"),
];

const PLANES: &[&str] = &["xz", "xy", "yz"];
const PLANE: Field = o("plane", "Plane", Kind::Choice(PLANES), false, "the sketch plane: @xz the ground (default, extrudes up +Y), @xy the front (+Z), @yz the side (-X)");
const SKETCH: Field = o("sketch", "Sketch", Kind::Sketch, true, "a Sketch, one shape or a list of shapes");
const SOURCE: Field = o("source", "Solid", Kind::Solid, true, "the solid it changes");
const EDGES: Field = o("edges", "Edges", Kind::Edges, false, "@all (default), {top: true}, {dir: @y convex: true}, ..., on an exact body m.edge(b.top, b.front) (the edge those two faces share; {near: [x y z]} one piece of it, {chain: true} with the edges running on from it), or a list of those");
const SEGMENTS: Field = f("segments", "Segments", Kind::Count, false, 3.0, 256.0, 1.0, "facets round the circumference");
const CELL: Field = f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution (default: 80 cells over the bounds)");
const AT2: Field = o("at", "Centre", Kind::Point2, false, "[u v] in the sketch");

const MODES: &[&str] = &["in_place", "snap"];
/// A hinge's or slider's motor in Mechanics' sim.
pub const MOTORS: &[&str] = &["none", "position", "speed"];
const JOINT_MODE: Field = o("mode", "Assembly", Kind::Choice(MODES), false, "@in_place (default): printed assembled, with clearances; @snap: printed apart and snapped together (later)");
const JOINT_AXIS: Field = o("axis", "Axis", Kind::Point, false, "[x y z] direction; default the print up (+Y): pins along the print Z print best");
const JOINT_CLEARANCE: Field = f("clearance", "Clearance", Kind::Length, false, 0.05 * MM, 2.0 * MM, 0.05 * MM, "the gap round the moving part; default from the nozzle (0.35mm for 0.4)");

const WEAVE_BAR: Field = f("bar", "Bar radius", Kind::Radius, false, 0.3 * MM, 3.0 * MM, 0.05 * MM, "bar radius; default from the printer (0.8mm for a 0.4 nozzle)");
const WEAVE_SIZE: Field = f("size", "Half size", Kind::Length, false, 2.0 * MM, 40.0 * MM, 0.1 * MM, "the link's half size (centreline); default the smallest that keeps the clearances. A list [3mm 5mm 3mm] grades the sizes evenly from one end to the other (a chain: along its path; mail: across its region's x)");
const WEAVE_SIZE_Y: Field = f("size_y", "Half size y", Kind::Length, false, 2.0 * MM, 40.0 * MM, 0.1 * MM, "mail: the links' half size across the region's y, one length or a graded list; default size. Interlocked mail keeps one width per column and one depth per row, so sizes grade along x and along y, not in rings");
const WEAVE_CLEARANCE: Field = f("clearance", "Clearance", Kind::Length, false, 0.1 * MM, 1.5 * MM, 0.05 * MM, "in-layer gap between links; default from the printer (0.35mm)");
const WEAVE_CLEARANCE_Z: Field = f("clearance_z", "Layer gap", Kind::Length, false, 0.1 * MM, 1.5 * MM, 0.05 * MM, "gap across layers; default two layers (0.4mm)");
const WEAVE_LEG: Field = f("leg", "Leg angle", Kind::Angle, false, 40.0 * DEG, 80.0 * DEG, DEG, "how steeply the legs rise to the bridges (default 45deg)");
const WEAVE_SIDES: Field = f("sides", "Sides", Kind::Count, false, 6.0, 32.0, 1.0, "facets round each bar (display mesh); default 12");

/// The module's calls that build no part: they answer values (`m.metric`, `m.heat_set`, `m.screw`) or the
/// part they were given, tagged (`m.link`, `m.optics`, `m.keepouts`).
pub const VALUE_CALLS: &[&str] = &["metric", "heat_set", "screw", "link", "optics", "keepouts", "host_call", "edge"];

// The two drill operations take the same dimensions. An exact hole also accepts a face.
macro_rules! hole_fields {
    ($direction:literal $(, $on:expr)?) => { &[
        SOURCE,
        o("kind", "Kind", Kind::Choice(&["simple", "counterbore", "countersink", "tapped"]), false, "hole shape"),
        f("diameter", "Diameter", Kind::Diameter, true, 0.3 * MM, 200.0 * MM, 0.1 * MM, ""),
        f("depth", "Depth", Kind::Length, false, 0.1 * MM, 1000.0 * MM, 0.5 * MM, "default: through"),
        o("at", "Where", Kind::Points, true, "[x y z] on the surface, or a list of them"),
        o("dir", "Direction", Kind::Point, false, $direction),
        f("bore_diameter", "Bore diameter", Kind::Diameter, false, 0.5 * MM, 300.0 * MM, 0.1 * MM, "counterbore"),
        f("bore_depth", "Bore depth", Kind::Length, false, 0.1 * MM, 300.0 * MM, 0.1 * MM, "counterbore"),
        f("sink_diameter", "Sink diameter", Kind::Diameter, false, 0.5 * MM, 300.0 * MM, 0.1 * MM, "countersink"),
        f("angle", "Sink angle", Kind::Angle, false, 60.0 * DEG, 120.0 * DEG, 1.0 * DEG, "countersink, default 90°"),
        f("pitch", "Pitch", Kind::Length, false, 0.2 * MM, 8.0 * MM, 0.05 * MM, "tapped"),
        f("clearance", "Clearance", Kind::Length, false, 0.0, 1.0 * MM, 0.05 * MM, "tapped, default 0.15mm"),
        $($on,)?
    ] };
}

pub const PROTOS: &[Proto] = &[
    Proto { name: "Panel", call: "panel", args: &["base"], group: Group::Combine, help: "A body that takes additions and cuts from host-generated parts, in the model frame. Inherits its base's frame unless it declares its own.", fields: &[
        o("base", "Base", Kind::Solid, true, "the shell before claimable cuts or generated edits; generators query this uncut shell"),
        o("cuts", "Cuts", Kind::Solids, false, "base cuts; a generated part can take over cuts linked with m.link(cut, key). Unkeyed cuts always apply"),
        o("name", "Name", Kind::Text, false, "panel name; defaults to its let name"),
        o("pose", "Pose", Kind::Place, false, "a plain value passed unchanged to the host"),
    ], example: "let lid = m.panel(m.box({size: [40mm 2mm 40mm]}), {})" },
    Proto { name: "Generated", call: "generated", args: &["kind"], group: Group::Generator, help: "A part built by a host generator; empty until the host answers. Its options are generator parameters, including an optional panel part.", fields: &[
        o("kind", "Kind", Kind::Text, true, "the registered host generator"),
        o("params", "Parameters", Kind::Place, false, "plain generator parameters, with panel naming an m.panel part"),
    ], example: "m.generated(\"post\", {panel: lid at: [5mm 5mm]})" },
    Proto { name: "BrepBox", call: "box", args: &[], group: Group::Brep, help: "An exact box centred on the origin. Its faces: b.top, b.bottom, b.left, b.right, b.front, b.back.", fields: &[
        o("size", "Size", Kind::Point, true, "[x y z]"),
    ], example: "m.brep.box({size: [40mm 10mm 30mm]})" },
    Proto { name: "BrepSphere", call: "sphere", args: &[], group: Group::Brep, help: "An exact sphere centred on the origin. Its face: s.wall.", fields: &[
        f("radius", "Radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, ""),
    ], example: "m.brep.sphere({radius: 10mm})" },
    Proto { name: "BrepCylinder", call: "cylinder", args: &[], group: Group::Brep, help: "An exact cylinder along Y, centred; top_radius makes a cone. Its faces: c.wall, c.top, c.bottom.", fields: &[
        f("radius", "Radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.1 * MM, ""),
        f("height", "Height", Kind::Length, true, 0.1 * MM, 2000.0 * MM, 0.5 * MM, ""),
        f("top_radius", "Top radius", Kind::Radius, false, 0.0, 1000.0 * MM, 0.1 * MM, "default: the radius"),
    ], example: "m.brep.cylinder({radius: 5mm height: 20mm})" },
    Proto { name: "BrepTorus", call: "torus", args: &[], group: Group::Brep, help: "An exact ring in the XZ plane. Its face: t.wall.", fields: &[
        f("radius", "Ring radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, ""),
        f("tube", "Tube radius", Kind::Radius, true, 0.1 * MM, 500.0 * MM, 0.1 * MM, ""),
    ], example: "m.brep.torus({radius: 20mm tube: 3mm})" },
    Proto { name: "BrepExtrude", call: "extrude", args: &[], group: Group::Brep, help: "A sketch pulled into an exact solid along its plane's normal. Its faces: e.top, e.bottom, and e.side(i), the side pulled from the sketch's segment i (0 the first).", fields: &[
        SKETCH,
        PLANE,
        f("height", "Height", Kind::Length, true, 0.01 * MM, 2000.0 * MM, 0.5 * MM, ""),
        o("both", "Both ways", Kind::Bool, false, "centred on the plane"),
    ], example: "m.brep.extrude({sketch: Rect{size: [40mm 20mm]} height: 8mm})" },
    Proto { name: "BrepRevolve", call: "revolve", args: &[], group: Group::Brep, help: "A sketch turned about Y into an exact solid: u is the radius, v the height. Its faces: r.side(i) from the sketch's segment i, and r.start, r.end where a part turn stops.", fields: &[
        SKETCH,
        f("angle", "Angle", Kind::Angle, false, 1.0 * DEG, 360.0 * DEG, DEG, "default a full turn"),
    ], example: "m.brep.revolve({sketch: Rect{center: [20mm 10mm] size: [8mm 20mm]}})" },
    Proto { name: "BrepSweep", call: "sweep", args: &[], group: Group::Brep, help: "A sketch carried along a smooth path into an exact solid, sized along the way. Its faces: s.start, s.end, and s.side(0) for a sketch of one curve.", fields: &[
        SKETCH,
        o("path", "Path", Kind::Points, true, "[[x y z] ...]: the curve passes through them (two or more)"),
        f("scale", "Scale", Kind::Ratio, false, 0.01, 100.0, 0.05, "the sketch's size at each path point [1 0.8 0.3], or one number: the end's size, reached evenly"),
    ], example: "m.brep.sweep({sketch: Circle{radius: 1.5mm} path: [[0 0 0] [3mm 12mm 2mm] [10mm 20mm 0] [16mm, 24mm, -4mm] [20mm, 22mm, -8mm]] scale: [1 0.85 0.6 0.4 0.2]})" },
    Proto { name: "BrepLoft", call: "loft", args: &[], group: Group::Brep, help: "An exact solid through sections stacked along a plane's normal; each has the same number of curves. Its faces: l.start, l.end.", fields: &[
        o("sections", "Sections", Kind::Sections, true, "[Section{sketch at} ...]"),
        PLANE,
    ], example: "m.brep.loft({sections: [Section{sketch: Circle{radius: 15mm} at: 0} Section{sketch: Circle{radius: 8mm} at: 30mm}]})" },
    Proto { name: "BrepUnion", call: "union", args: &["a", "b"], group: Group::Brep, help: "Exact union of bodies. Both must be m.brep parts; m.union takes any parts through the mesh.", fields: &[
        o("a", "Solid", Kind::Solid, true, ""),
        o("b", "With", Kind::Solids, true, "one body or a list"),
    ], example: "m.brep.union(a, b)" },
    Proto { name: "BrepDifference", call: "subtract", args: &["a", "b"], group: Group::Brep, help: "Exact difference of bodies. Both must be m.brep parts; m.subtract takes any parts through the mesh.", fields: &[
        o("a", "Solid", Kind::Solid, true, ""),
        o("b", "Minus", Kind::Solids, true, "one body or a list"),
    ], example: "m.brep.subtract(a, holes)" },
    Proto { name: "BrepIntersect", call: "intersect", args: &["a", "b"], group: Group::Brep, help: "Exact intersection of bodies. Both must be m.brep parts.", fields: &[
        o("a", "Solid", Kind::Solid, true, ""),
        o("b", "With", Kind::Solids, true, "one body or a list"),
    ], example: "m.brep.intersect(a, b)" },
    Proto { name: "BrepFillet", call: "fillet", args: &["source", "radius"], group: Group::Brep, help: "Rounds exact edges of a body with a true circular surface. Its faces: r.face(m.edge(a, b)) along the edge of a and b, r.corner(a, b, c) where blends meet; the rest keep their names.", fields: &[
        SOURCE,
        f("radius", "Radius", Kind::Radius, true, 0.05 * MM, 100.0 * MM, 0.1 * MM, ""),
        EDGES,
    ], example: "m.brep.fillet(body, 2mm, {edges: m.edge(body.top, body.front)})" },
    Proto { name: "BrepChamfer", call: "chamfer", args: &["source", "size"], group: Group::Brep, help: "Bevels exact edges of a body by size back along each face. Its faces: c.face(m.edge(a, b)), c.corner(a, b, c); the rest keep their names.", fields: &[
        SOURCE,
        f("size", "Size", Kind::Length, true, 0.05 * MM, 100.0 * MM, 0.1 * MM, ""),
        EDGES,
    ], example: "m.brep.chamfer(body, 1mm, {edges: {bottom: true}})" },
    Proto { name: "BrepShell", call: "shell", args: &["source", "thickness"], group: Group::Brep, help: "Hollows a body with walls inward, leaving its flat top or bottom open, or the faces named. Its faces: s.inner() every inner one, s.inner(b.front) the one inside b.front; the rest keep their names.", fields: &[
        SOURCE,
        f("thickness", "Wall", Kind::Length, true, 0.1 * MM, 100.0 * MM, 0.1 * MM, ""),
        o("open", "Open", Kind::Choice(&["top", "bottom", "closed"]), false, "@top (default), @bottom, @closed, or faces by name: body.front or [body.top body.front]"),
    ], example: "m.brep.shell(body, 2mm, {open: @top})" },
    Proto { name: "BrepPull", call: "pull", args: &["source"], group: Group::Brep, help: "Moves a flat face of a body along its normal by `by`, as a press-pull: the face's outline (its holes too) swept out and joined on, or swept in and cut away below 0. The moved face keeps the face's names; the rest keep theirs; a result that is no closed solid is refused.", fields: &[
        SOURCE,
        o("face", "Face", Kind::Faces, true, "the flat face, by name: body.top"),
        f("by", "By", Kind::Length, true, -500.0 * MM, 500.0 * MM, 0.1 * MM, "out of the material; into it below 0"),
    ], example: "m.brep.pull(body, {face: body.top by: 5mm})" },
    Proto { name: "BrepHole", call: "hole", args: &["source"], group: Group::Brep, help: "Drills an exact body at each point, to depth or through. Use @simple, @counterbore or @countersink. With on, each point follows the named face along the drill direction. The narrow wall is h.wall and the bottom of a blind hole is h.floor.", fields: hole_fields!(
        "into the material, default inward from on or [0, -1, 0] without a face",
        o("on", "On", Kind::Faces, false, "the flat face it starts on, by name: body.top (it follows the face)")
    ), example: "m.brep.hole(body, {on: body.top at: [5mm 10mm 0] diameter: 4mm depth: 6mm})" },
    Proto { name: "BrepOffset", call: "offset", args: &["source", "distance"], group: Group::Brep, help: "Grows a body by distance along its normals; negative shrinks it.", fields: &[
        SOURCE,
        f("distance", "Distance", Kind::Length, true, -100.0 * MM, 100.0 * MM, 0.1 * MM, ""),
    ], example: "m.brep.offset(body, 1mm)" },
    Proto { name: "BrepMirror", call: "mirror", args: &["source"], group: Group::Brep, help: "The exact mirror image across a plane through center: @yz flips X, @xz flips Y, @xy flips Z; keep (default true) joins it with the original; keep: false gives the image alone, its faces named as its source's (img.top).", fields: &[
        SOURCE,
        o("plane", "Mirror plane", Kind::Choice(&["yz", "xz", "xy"]), false, ""),
        o("center", "Through", Kind::Point, false, "[x y z]"),
        o("keep", "Keep original", Kind::Bool, false, "join both (default true)"),
    ], example: "m.brep.mirror(m.brep.box({size: [10mm 10mm 10mm] at: [8mm 0 0]}), {plane: @yz})" },
    // ---- joints
    Proto { name: "Hinge", call: "hinge", args: &["a", "b"], group: Group::Joint, help: "A print-in-place hinge between two parts that overlap or touch at `at`: a gets the fork (two plates and a pin along `axis`), b the eye round the pin. Clearances, pin and plate sizes come from the printer; the cuts clear every angle, so it turns where the parts allow.", fields: &[
        o("a", "Fork part", Kind::Solid, true, "the part that gets the plates and the pin"),
        o("b", "Eye part", Kind::Solid, true, "the part that turns round the pin"),
        o("at", "Pin at", Kind::Point, true, "a point on the pin axis, where the parts meet: [x y z]"),
        JOINT_AXIS,
        f("lower", "Lower limit", Kind::Angle, false, -180.0 * DEG, 0.0, 5.0 * DEG, "how far b turns back (default: until the parts touch)"),
        f("upper", "Upper limit", Kind::Angle, false, 0.0, 180.0 * DEG, 5.0 * DEG, "how far b turns forward (default: until the parts touch)"),
        f("pin", "Pin radius", Kind::Radius, false, 0.4 * MM, 20.0 * MM, 0.1 * MM, "default from the parts' size"),
        f("plate", "Plate thickness", Kind::Length, false, 0.4 * MM, 20.0 * MM, 0.2 * MM, "each of the fork's two plates; default 4 layers or a fifth of the thickness"),
        JOINT_CLEARANCE,
        f("clearance_axial", "Axial clearance", Kind::Length, false, 0.05 * MM, 2.0 * MM, 0.05 * MM, "the gap between plates and eye along the pin; default two layers"),
        o("shoe", "Brake shoe", Kind::Solid, false, "a part of flexible filament (TPU) round b's eye: the hinge makes it two shoes in the eye, each with a tooth in a groove of the pin as it prints; turned off that angle, the pin presses the teeth in and the hinge holds its pose. Each brake has its own shoe part; the pin needs a radius of 2 mm or so"),
        f("hold", "Brake hold", Kind::Number, false, 1.0, 1000.0, 1.0, "the torque the brake holds once turned, N mm (default: its teeth run the eye's length)"),
        o("motor", "Motor", Kind::Choice(MOTORS), false, "a motor on the pin in Mechanics' sim: @position, a servo that turns b to the angle set there; @speed, turns b at the speed set there (default @none)"),
        f("torque", "Motor torque", Kind::Number, false, 1.0, 100000.0, 10.0, "the most the motor turns b with, N mm (default 150, a micro servo's)"),
        JOINT_MODE,
    ], example: "let elbow = m.hinge(upper, lower, {at: [0, 0, 40mm]})" },
    Proto { name: "BallJoint", call: "ball", args: &["a", "b"], group: Group::Joint, help: "A print-in-place ball and socket centred at `at`: a gets the socket (a shell with a 45° roof and an opening toward b), b the ball and a neck through the opening. b tilts within `cone` and twists freely. The ball prints standing on the bed: its centre 0.5 to 0.95 of its radius above it (0.71 needs no support), cut flat there in a socket open underneath; or on its neck, the axis pointing down. A ball over a socket floor would print in mid-air and is refused.", fields: &[
        o("a", "Socket part", Kind::Solid, true, "the part that holds the socket"),
        o("b", "Ball part", Kind::Solid, true, "the part on the ball"),
        o("at", "Centre", Kind::Point, true, "the ball's centre: [x y z]"),
        f("radius", "Ball radius", Kind::Radius, false, 1.5 * MM, 50.0 * MM, 0.5 * MM, "default from the parts' size"),
        f("neck", "Neck radius", Kind::Radius, false, 0.5 * MM, 30.0 * MM, 0.1 * MM, "default 0.38 x the ball radius"),
        o("axis", "Opening", Kind::Point, false, "[x y z] from the centre toward b; default toward b's centre"),
        f("cone", "Cone limit", Kind::Angle, false, 5.0 * DEG, 80.0 * DEG, 5.0 * DEG, "how far b tilts from the opening axis (default 35°, kept small enough that the ball stays captive)"),
        f("twist_lower", "Twist lower", Kind::Angle, false, -180.0 * DEG, 0.0, 5.0 * DEG, ""),
        f("twist_upper", "Twist upper", Kind::Angle, false, 0.0, 180.0 * DEG, 5.0 * DEG, ""),
        f("wall", "Socket wall", Kind::Length, false, 0.8 * MM, 20.0 * MM, 0.2 * MM, ""),
        JOINT_CLEARANCE,
        JOINT_MODE,
    ], example: "let shoulder = m.ball(torso, arm, {at: [12mm, 3.5mm, 0], radius: 5mm})" },
    Proto { name: "Slider", call: "slider", args: &["a", "b"], group: Group::Joint, help: "A print-in-place slider: b (the carriage) slides along `axis` in a (the rail), both standing on the bed. The carriage's neck comes up through a slot in the rail's top, and a chevron ridge on it (flanks no flatter than 45 degrees) runs in a matching groove, so it cannot lift out and both print without support. The groove stops short of the rail's ends (the end stops). Over the rail the carriage's underside climbs at 45 degrees from its neck; past the rail's sides it may stand on the bed.", fields: &[
        o("a", "Rail part", Kind::Solid, true, "the part with the groove, standing on the bed"),
        o("b", "Carriage part", Kind::Solid, true, "the part that slides, standing on the bed in the rail"),
        o("at", "On the way", Kind::Point, true, "a point on the way's centre line (its height does not matter): [x y z]"),
        o("axis", "Way", Kind::Point, true, "[x y z] the direction it slides, along the bed"),
        f("lower", "Back", Kind::Length, false, -1000.0 * MM, 0.0, 1.0 * MM, "how far the carriage slides back from where it prints (default: to the end stop)"),
        f("upper", "On", Kind::Length, false, 0.0, 1000.0 * MM, 1.0 * MM, "how far it slides on (default: to the end stop)"),
        f("width", "Neck width", Kind::Length, false, 2.0 * MM, 30.0 * MM, 0.5 * MM, "the carriage's neck through the slot; default from the rail's width"),
        f("depth", "Ridge depth", Kind::Length, false, 0.4 * MM, 10.0 * MM, 0.1 * MM, "how far the ridge reaches past the neck each side; default 0.3 of the rail's height"),
        JOINT_CLEARANCE,
        o("motor", "Motor", Kind::Choice(MOTORS), false, "a motor on the way in Mechanics' sim: @position, moves b to where it is set there; @speed, moves b at the speed set there (default @none)"),
        f("force", "Motor force", Kind::Number, false, 0.1, 10000.0, 1.0, "the most the motor pushes b with, N (default 10)"),
        JOINT_MODE,
    ], example: "let way = m.slider(track, block, {at: [30mm, 0, 0], axis: [1 0 0]})" },
    Proto { name: "Bearing", call: "bearing", args: &["a", "b"], group: Group::Joint, help: "A print-in-place bearing: b (the inner) turns all the way round in a (the outer), about the print's up through `at`, both standing on the bed. A chevron ridge round the inner runs in a matching groove of the outer (flanks no flatter than 45 degrees), so it cannot lift out and both print without support. Over the outer the inner's underside climbs at 45 degrees from its neck.", fields: &[
        o("a", "Outer part", Kind::Solid, true, "the part with the race, standing on the bed"),
        o("b", "Inner part", Kind::Solid, true, "the part that turns, standing on the bed in it"),
        o("at", "Axis at", Kind::Point, true, "a point on the turning axis (its height does not matter): [x y z]"),
        f("radius", "Race radius", Kind::Radius, false, 2.0 * MM, 200.0 * MM, 0.5 * MM, "the inner's radius at the bed and at the race's top; default from the inner's size"),
        f("depth", "Ridge depth", Kind::Length, false, 0.4 * MM, 10.0 * MM, 0.1 * MM, "how far the ridge reaches out past the radius; default 0.3 of the outer's height"),
        JOINT_CLEARANCE,
        JOINT_MODE,
    ], example: "let turntable = m.bearing(base, disc, {at: [0, 0, 0]})" },
    Proto { name: "Bond", call: "bond", args: &["to", "parts"], group: Group::Joint, help: "Bonds parts to `to` as they print: `to` is cut out of each of them exactly, so they print touching and fuse where they meet (a spine through a row of segments, an inlay). Bonded to a rigid part, a part is fixed to it. Bonded to a flexible part (its colour's material TPU, TPE or FLEX), each part bends to the next in the list about it where it runs free between them: leave it a free run there, a few millimetres long, and Mechanics bends it as stiff as its section.", fields: &[
        o("to", "Bonded to", Kind::Solid, true, "the part they are bonded to: a flexible spine, a core"),
        o("parts", "Parts", Kind::Solids, true, "the parts bonded to it, in their order along it"),
    ], example: "let back = m.bond(spine, [head body tail])" },
    Proto { name: "Chain", call: "chain", args: &["links"], group: Group::Joint, help: "Joins each link to the next with a hinge (or a ball): one call for a jointed chain. Each joint sits where the two links meet, midway between their centres, unless `at` is given per joint.", fields: &[
        o("links", "Links", Kind::Solids, true, "the links in order"),
        o("joint", "Joint", Kind::Choice(&["hinge", "ball"]), false, "@hinge (default) or @ball"),
        JOINT_AXIS,
        f("lower", "Lower limit", Kind::Angle, false, -180.0 * DEG, 0.0, 5.0 * DEG, ""),
        f("upper", "Upper limit", Kind::Angle, false, 0.0, 180.0 * DEG, 5.0 * DEG, ""),
        f("pin", "Pin radius", Kind::Radius, false, 0.4 * MM, 20.0 * MM, 0.1 * MM, ""),
        f("radius", "Ball radius", Kind::Radius, false, 1.5 * MM, 50.0 * MM, 0.5 * MM, "for @ball"),
        f("cone", "Cone limit", Kind::Angle, false, 5.0 * DEG, 80.0 * DEG, 5.0 * DEG, "for @ball"),
        JOINT_CLEARANCE,
        JOINT_MODE,
    ], example: "let links = for i in 0..8 { m.box({size: [12mm 6mm 5mm] at: [i * 12mm + 6mm 3mm 0]}) }\nm.chain(links)" },
    Proto { name: "Anchor", call: "anchor", args: &["part"], group: Group::Joint, help: "Where the piece is held in the sim: a hanging hole through `part` at `at` (it hangs on a pin there), held fixed with fixed: true, or by a hand with held: true.", fields: &[
        o("part", "Part", Kind::Solid, true, ""),
        o("at", "At", Kind::Point, true, "[x y z]"),
        f("hole", "Hole diameter", Kind::Diameter, false, 0.8 * MM, 20.0 * MM, 0.2 * MM, "default 2.4mm"),
        JOINT_AXIS,
        o("fixed", "Held fixed", Kind::Bool, false, "hold the part still instead of hanging it (no hole)"),
        o("held", "Held by hand", Kind::Bool, false, "a hand holds the part (no hole): where it printed, or a little over the bed when hands are all its script's joints; it stays there until a grab carries it, and where the hand lets go"),
    ], example: "m.anchor(links[0], {at: [3mm 3mm 0]})" },
    Proto { name: "String", call: "string", args: &["a", "b"], group: Group::Joint, help: "A string from a hole in part `a` to a hole in part `b`: taut at its length, slack when shorter, and it pulls nothing then. Strings run in Mechanics' sim, pass through parts and print as nothing: cut them to length.", fields: &[
        o("a", "From part", Kind::Solid, true, ""),
        o("b", "To part", Kind::Solid, true, ""),
        o("from", "From", Kind::Point, true, "the hole's centre in a: [x y z]"),
        o("to", "To", Kind::Point, true, "the hole's centre in b: [x y z]"),
        f("length", "Length", Kind::Length, true, 1.0 * MM, 2000.0 * MM, 1.0 * MM, "at least the path as printed: a string must not start stretched"),
    ], example: "m.string(control, puppet, {from: [0 3mm 0] to: [0 3mm 60mm] length: 120mm})" },
    // ---- weave
    Proto { name: "Links", call: "links", args: &["path"], group: Group::Weave, help: "A print-in-place chain of free interlocking links along a path on the bed: each link interlocks with the next and they hold by contact alone.", fields: &[
        o("path", "Path", Kind::Points, true, "[[x y z] ...] on the bed (the up coordinate is ignored), or a Shape outline (a loop)"),
        o("closed", "Closed", Kind::Bool, false, "a loop: the last link interlocks with the first; default true for a Shape"),
        WEAVE_BAR, WEAVE_SIZE, WEAVE_CLEARANCE, WEAVE_CLEARANCE_Z, WEAVE_LEG, WEAVE_SIDES,
    ], example: "let c = m.links([[0 0 0] [120mm 0 0]])" },
    Proto { name: "Mail", call: "mail", args: &["region"], group: Group::Weave, help: "Print-in-place chainmail filling a region of the bed (each link interlocked with four), or with fill: @volume a solid's volume: leaning links that all stand on the bed and interlock with two links in every row they cross, printed without support.", fields: &[
        o("region", "Region", Kind::Sketch, true, "a Shape or Sketch laid on the bed like an extrude's sketch, or an SDF solid: its footprint (or, with fill: @volume, its volume)"),
        o("fill", "Fill", Kind::Choice(&["footprint", "volume"]), false, "@footprint (default): one layer of links over the region's footprint; @volume: the solid's volume, links standing on the bed and leaning (an SDF solid region)"),
        f("lean", "Lean", Kind::Angle, false, 45.0 * DEG, 85.0 * DEG, DEG, "fill: @volume: how steeply the links lean up from the bed (default 60deg; 45deg is the steepest overhang printed without support)"),
        o("angle", "Angle", Kind::Angle, false, "turns the weave in the bed plane"),
        WEAVE_BAR, WEAVE_SIZE, WEAVE_SIZE_Y, WEAVE_CLEARANCE, WEAVE_CLEARANCE_Z, WEAVE_LEG, WEAVE_SIDES,
    ], example: "let mail = m.mail(Circle{radius: 40mm})" },
    Proto { name: "Drape", call: "drape", args: &["mail", "surface"], group: Group::Weave, help: "Chainmail draped over the top of a solid: each link moves onto the surface under it, tilts with it and rests clearance_z over it at its closest. Print the surface (or m.form(skin)) as the support under it.", fields: &[
        o("mail", "Mail", Kind::Solid, true, "a m.mail(...)"),
        o("surface", "Surface", Kind::Solid, true, "an SDF solid"),
    ], example: "let dome = m.sphere({radius: 50mm at: [0, -30mm, 0]})\nlet skin = m.drape(m.mail(Rect{size: [60mm 60mm]}), dome)" },
    Proto { name: "Form", call: "form", args: &["drape"], group: Group::Weave, help: "The printed support form under a drape: its surface cut to the links' footprint, from the bed up. Each link rests clearance_z over it, so the form breaks away; print it with the drape.", fields: &[
        o("drape", "Drape", Kind::Solid, true, "a m.drape(...)"),
        f("margin", "Margin", Kind::Length, false, 0.0, 20.0 * MM, 0.5 * MM, "how far the form reaches past the links' footprint; default 0"),
    ], example: "let dome = m.sphere({radius: 50mm at: [0, -30mm, 0]})\nlet skin = m.drape(m.mail(Rect{size: [60mm 60mm]}), dome)\nlet form = m.form(skin)" },
    // ---- primitives
    Proto { name: "Box", call: "box", args: &[], group: Group::Primitive, help: "A box centred on the origin, Y up. Taper widens the top (+Y) face along X and Z (negative narrows it); shift moves that face along X. Radius rounds every edge and cannot be combined with nonzero taper or shift.", fields: &[
        o("size", "Size", Kind::Point, true, "[x y z]"),
        f("radius", "Edge radius", Kind::Radius, false, 0.0, 50.0 * MM, 0.1 * MM, "rounds every edge"),
        o("taper", "Taper", Kind::Point2, false, "[x z]: added top width and depth; negative narrows the top (default [0 0])"),
        o("shift", "Shift", Kind::Length, false, "moves the top face along X (default 0)"),
    ], example: "m.box({size: [40mm 10mm 30mm] taper: [-4mm, -2mm] shift: 2mm})" },
    Proto { name: "Hull", call: "hull", args: &["parts"], group: Group::Primitive, help: "The convex hull of parts and points: the smallest convex solid containing them all, like shrink-wrap (two spheres make a capsule; a box and a cylinder a smooth transition). Parts are hulled from their mesh; points alone need four not in one plane.", fields: &[
        o("parts", "Parts", Kind::Solids, false, "one part or a list"),
        o("points", "Points", Kind::Points, false, "[[x y z] ...]"),
    ], example: "m.hull([m.sphere({radius: 4mm}) m.sphere({radius: 2mm at: [20mm 0 0]})])" },
    Proto { name: "Picture", call: "picture", args: &[], group: Group::Primitive, help: "A flat tile that prints a photo in full colour with the filaments loaded: side by side a bead apart, and stacked, the colour under a thin layer showing through it. Face down (the default) the first layer, on the plate, is the picture, mirrored so it reads right turned over; the layers past the second are the set's white (or the second again). Centred on the origin, the picture over x and z, its layers up y.", fields: &[
        o("image", "Image", Kind::Text, true, "a PNG or a JPEG beside the script"),
        f("width", "Width", Kind::Length, true, 5.0 * MM, 500.0 * MM, MM, "the tile across; its depth follows the picture's"),
        f("layers", "Layers", Kind::Count, false, 1.0, 64.0, 1.0, "default 4"),
        f("layer", "Layer", Kind::Length, false, 0.04 * MM, 0.6 * MM, 0.01 * MM, "each layer's height (default 0.16mm)"),
        o("face", "Face", Kind::Choice(PICTURE_FACES), false, "@down (default): the first layer is the picture; @up: the last"),
        f("min_run", "Shortest run", Kind::Count, false, 1.0, 16.0, 1.0, "the shortest run of one filament along a line, in beads (default 2)"),
        f("smooth", "Smooth", Kind::Ratio, false, 0.0, 1.0, 0.01, "how much fewer filament changes weigh against colour: fewer runs and retractions, a little more grain (default 0.3)"),
        o("set", "Filament set", Kind::Text, false, "a calibrated filament set by name; by default the loaded filaments and their calibration"),
        o("filaments", "Filaments", Kind::Materials, false, "[Material{..} ...]: a first guess before any calibration (not with set)"),
    ], example: "m.picture({image: \"photo.jpg\" width: 120mm})" },
    Proto { name: "ColourSwatch", call: "colour_swatch", args: &[], group: Group::Primitive, help: "The calibration print of a filament set: a patch of every two-layer stack of its filaments over the white and over the darkest, a white frame, a tone strip and four corner marks. Photographed face up, it measures the colours a picture prints in.", fields: &[
        o("size", "Size", Kind::Point2, true, "[x z]: the size the swatch of the set's filaments needs"),
        f("layers", "Layers", Kind::Count, false, 1.0, 64.0, 1.0, "the tiles' layers (default 4)"),
        f("layer", "Layer", Kind::Length, false, 0.04 * MM, 0.6 * MM, 0.01 * MM, "the tiles' layer height (default 0.16mm)"),
        o("face", "Face", Kind::Choice(PICTURE_FACES), false, "@down (default) or @up, as the tiles"),
        o("set", "Filament set", Kind::Text, false, "a filament set by name; by default the loaded filaments"),
        o("filaments", "Filaments", Kind::Materials, false, "[Material{..} ...] (not with set)"),
    ], example: "m.colour_swatch({size: [74mm 52mm]})" },
    Proto { name: "Sphere", call: "sphere", args: &[], group: Group::Primitive, help: "A sphere centred on the origin.", fields: &[
        f("radius", "Radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, ""),
        SEGMENTS,
        f("rings", "Rings", Kind::Count, false, 2.0, 256.0, 1.0, "facets from pole to pole"),
    ], example: "m.sphere({radius: 10mm})" },
    Proto { name: "Cylinder", call: "cylinder", args: &[], group: Group::Primitive, help: "A cylinder along Y, centred; top_radius makes a cone frustum.", fields: &[
        f("radius", "Radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.1 * MM, ""),
        f("height", "Height", Kind::Length, true, 0.1 * MM, 2000.0 * MM, 0.5 * MM, ""),
        f("top_radius", "Top radius", Kind::Radius, false, 0.0, 1000.0 * MM, 0.1 * MM, "default: the radius"),
        SEGMENTS,
    ], example: "m.cylinder({radius: 5mm height: 20mm})" },
    Proto { name: "Torus", call: "torus", args: &[], group: Group::Primitive, help: "A ring in the XZ plane.", fields: &[
        f("radius", "Ring radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, ""),
        f("tube", "Tube radius", Kind::Radius, true, 0.1 * MM, 500.0 * MM, 0.1 * MM, ""),
        SEGMENTS,
        f("sides", "Sides", Kind::Count, false, 3.0, 256.0, 1.0, "facets round the tube"),
    ], example: "m.torus({radius: 20mm tube: 3mm})" },
    Proto { name: "Ellipsoid", call: "ellipsoid", args: &[], group: Group::Primitive, help: "An ellipsoid centred on the origin with its radii along X, Y and Z: a distance-field solid (blends, carves and shells smoothly).", fields: &[
        o("radii", "Radii", Kind::Point, true, "[x y z]"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution"),
    ], example: "m.ellipsoid({radii: [20mm 25mm 20mm]})" },
    Proto { name: "Capsule", call: "capsule", args: &[], group: Group::Primitive, help: "A rod with round ends from a to b, radius thick (a limb, a finger, a lock): a distance-field solid (blends, carves and shells smoothly).", fields: &[
        o("a", "From", Kind::Point, true, "[x y z]: the middle of one end"),
        o("b", "To", Kind::Point, true, "[x y z]: the middle of the other end"),
        f("radius", "Radius", Kind::Radius, true, 0.05 * MM, 1000.0 * MM, 0.1 * MM, ""),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution"),
    ], example: "m.capsule({a: [0 10mm 0] b: [12mm 30mm 0] radius: 3mm})" },
    Proto { name: "Tube", call: "tube", args: &[], group: Group::Primitive, help: "A smooth tube through points, each [x y z r] with its radius there: a distance-field solid that blends into what it grows from (hair, tendrils, cables). `curves` puts many tubes in one part.", fields: &[
        o("points", "Points", Kind::Points, false, "[[x y z r] ...]: the curve passes through them, r its radius there"),
        o("curves", "Curves", Kind::Points, false, "[[[x y z r] ...] ...]: many tubes in one part (a head of hair: one per lock)"),
        f("blend", "Blend", Kind::Radius, false, 0.0, 100.0 * MM, 0.1 * MM, "a smooth fillet this wide where two curves meet (default 0: they just overlap)"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution"),
    ], example: "m.tube({points: [[0 0 0 3mm] [4mm 10mm 2mm 2.5mm] [-2mm, 18mm, 4mm, 1.8mm] [3mm, 25mm, -2mm, 0.8mm]]})" },
    Proto { name: "Cage", call: "cage", args: &[], group: Group::Primitive, help: "A smooth closed surface round a coarse cage of points and faces (Catmull-Clark subdivision, limit surface): it passes near the points, not through them. A mesh solid for sharp booleans, and a distance field inside Blend and Carve.", fields: &[
        o("points", "Points", Kind::Points, true, "[[x y z] ...]: the cage's corners"),
        o("faces", "Faces", Kind::Faces, true, "[[i j k l] ...]: each face's points by index, counter-clockwise seen from outside; quads mostly, every edge between two faces"),
        f("levels", "Levels", Kind::Count, false, 1.0, 6.0, 1.0, "subdivision steps (default: as many as the print tolerance needs)"),
    ], example: "m.cage({points: [[-8mm, 0, -8mm] [8mm, 0, -8mm] [8mm, 0, 8mm] [-8mm, 0, 8mm] [-8mm, 16mm, -8mm] [8mm, 16mm, -8mm] [8mm, 16mm, 8mm] [-8mm, 16mm, 8mm]] faces: [[0 1 2 3] [4 7 6 5] [0 4 5 1] [1 5 6 2] [2 6 7 3] [3 7 4 0]]})" },
    // ---- sketch generators
    Proto { name: "Extrude", call: "extrude", args: &[], group: Group::Generator, help: "A sketch pulled into a solid along its plane's normal; draft tapers the walls inward, both centres it on the plane.", fields: &[
        SKETCH, PLANE,
        f("height", "Height", Kind::Length, true, 0.01 * MM, 2000.0 * MM, 0.5 * MM, "how far it extrudes"),
        f("draft", "Draft", Kind::Angle, false, -60.0 * DEG, 60.0 * DEG, 0.5 * DEG, "side wall taper"),
        o("both", "Both sides", Kind::Bool, false, "centre on the sketch plane"),
    ], example: "m.extrude({sketch: Rect{size: [80mm 60mm] corner: 6mm} height: 8mm})" },
    Proto { name: "Revolve", call: "revolve", args: &[], group: Group::Generator, help: "A sketch turned about Y: sketch u is the radius (u >= 0), v the height; an angle under 360° leaves a wedge.", fields: &[
        SKETCH,
        f("angle", "Angle", Kind::Angle, false, 1.0 * DEG, 360.0 * DEG, 1.0 * DEG, "how far round (default 360°)"),
    ], example: "m.revolve({sketch: Polygon{points: [[0 0] [15mm 0] [15mm 40mm] [13mm 40mm] [13mm 2mm] [0 2mm]]}})" },
    Proto { name: "Sweep", call: "sweep", args: &[], group: Group::Generator, help: "A sketch (in its own u, v) carried along a path [[x y z] ...] or a Helix; twist turns it along the way, scale sizes the end.", fields: &[
        SKETCH,
        o("path", "Path", Kind::Points, false, "[[x y z] ...]"),
        o("helix", "Helix", Kind::Helix, false, "Helix{radius pitch turns} about +Y"),
        o("closed", "Closed", Kind::Bool, false, "the path is a loop"),
        f("twist", "Twist", Kind::Angle, false, -3600.0 * DEG, 3600.0 * DEG, 5.0 * DEG, "turn along the way"),
        f("scale", "End scale", Kind::Ratio, false, 0.01, 100.0, 0.05, "section size at the end"),
    ], example: "m.sweep({sketch: Circle{radius: 3mm} path: [[0 0 0] [0 20mm 0] [0 30mm 10mm] [0 30mm 30mm]]})" },
    Proto { name: "Loft", call: "loft", args: &[], group: Group::Generator, help: "A solid through cross-sections stacked along the plane's normal, in rising order; each section is one outline.", fields: &[
        o("sections", "Sections", Kind::Sections, true, "[Section{sketch at} ...]"),
        PLANE,
    ], example: "m.loft({sections: [Section{sketch: Rect{size: [30mm 30mm] corner: 4mm} at: 0} Section{sketch: Circle{radius: 10mm} at: 40mm}]})" },
    Proto { name: "RingLoft", call: "ring_loft", args: &[], group: Group::Generator, help: "A solid through rings with equal point counts, capped at both ends. Each ring is star-shaped about its origin and counter-clockwise seen from u x v; origins rise strictly along the first ring's normal. Rings may tilt.", fields: &[
        o("rings", "Rings", Kind::Rings, true, "[LoftRing{at u v points crease} ...]"),
        o("prism", "Prism", Kind::Bool, false, "marks exact prism surfaces; the mesh is unchanged (default false)"),
    ], example: "m.ring_loft({rings: [LoftRing{at: [0 0 0] u: [1 0 0] v: [0, 0, -1] points: [[-5mm, -5mm] [5mm, -5mm] [5mm 5mm] [-5mm, 5mm]]} LoftRing{at: [0 20mm 0] u: [1 0 0] v: [0, 0.6, -0.8] points: [[-3mm, -3mm] [3mm, -3mm] [3mm 3mm] [-3mm, 3mm]]}]})" },
    Proto { name: "Text", call: "text", args: &[], group: Group::Text, help: "Letters as a solid: size is the capital height, depth how far they extrude; join or cut it to emboss or engrave.", fields: &[
        o("text", "Text", Kind::Text, true, "the letters; \\n starts a line"),
        f("size", "Size", Kind::Length, true, 0.5 * MM, 500.0 * MM, 0.5 * MM, "capital height"),
        f("depth", "Depth", Kind::Length, true, 0.05 * MM, 100.0 * MM, 0.1 * MM, "extrusion"),
        f("spacing", "Spacing", Kind::Length, false, -5.0 * MM, 50.0 * MM, 0.1 * MM, "extra space between letters"),
        o("align", "Align", Kind::Choice(&["left", "center", "right"]), false, "horizontal alignment at u = 0"),
        o("font", "Font", Kind::Choice(&["sans", "mono"]), false, "the typeface"),
        PLANE,
    ], example: "m.text({text: \"FAB-01\" size: 6mm depth: 0.6mm align: @center})" },
    Proto { name: "Gear", call: "gear", args: &[], group: Group::Mechanical, help: "An involute spur gear; mesh_with sets it beside another gear of the same module, mesh_angle round it, with its teeth in the gaps.", fields: &[
        f("module", "Module", Kind::Length, true, 0.2 * MM, 20.0 * MM, 0.1 * MM, "pitch diameter / teeth"),
        f("teeth", "Teeth", Kind::Count, true, 6.0, 400.0, 1.0, ""),
        f("thickness", "Thickness", Kind::Length, true, 0.2 * MM, 500.0 * MM, 0.5 * MM, "face width"),
        f("pressure", "Pressure angle", Kind::Angle, false, 10.0 * DEG, 35.0 * DEG, 0.5 * DEG, "default 20°"),
        f("backlash", "Backlash", Kind::Length, false, 0.0, 2.0 * MM, 0.02 * MM, "play on the pitch circle (default 0.1mm)"),
        f("shift", "Profile shift", Kind::Number, false, -1.0, 1.0, 0.05, ""),
        f("bore", "Bore", Kind::Diameter, false, 0.0, 500.0 * MM, 0.1 * MM, "centre hole"),
        PLANE,
        o("mesh_with", "Mesh with", Kind::Solid, false, "a gear to sit beside"),
        f("mesh_angle", "Mesh direction", Kind::Angle, false, -360.0 * DEG, 360.0 * DEG, 5.0 * DEG, "where round the other gear"),
    ], example: "m.gear({module: 1.5mm teeth: 17 thickness: 6mm bore: 5mm})" },
    Proto { name: "Thread", call: "thread", args: &[], group: Group::Mechanical, help: "An ISO metric threaded rod along +Y; its ends run out to a plain round. clearance grows every radius and internal: true runs out to the major radius (a tool for a threaded hole, longer than the hole), left for left-handed.", fields: &[
        f("diameter", "Diameter", Kind::Diameter, true, 1.0 * MM, 100.0 * MM, 0.5 * MM, "major diameter"),
        f("pitch", "Pitch", Kind::Length, false, 0.2 * MM, 8.0 * MM, 0.05 * MM, "default ISO coarse"),
        f("length", "Length", Kind::Length, true, 0.5 * MM, 500.0 * MM, 0.5 * MM, ""),
        f("clearance", "Clearance", Kind::Length, false, -1.0 * MM, 1.0 * MM, 0.05 * MM, "added to the radius"),
        o("left", "Left-handed", Kind::Bool, false, ""),
        f("runout", "Run-out", Kind::Length, false, 0.0, 50.0 * MM, 0.1 * MM, "each end fades over this length (default one pitch; 0: cut flat)"),
        o("internal", "Hole tool", Kind::Bool, false, "ends run out to the major radius, so the hole's mouth takes the mating thread"),
    ], example: "m.thread({diameter: 8mm length: 20mm})" },
    // ---- modifiers
    Proto { name: "Fillet", call: "fillet", args: &["source", "radius"], group: Group::Modify, help: "Rounds edges of a solid: outside edges lose material, inside corners gain it.", fields: &[
        SOURCE,
        f("radius", "Radius", Kind::Radius, true, 0.05 * MM, 100.0 * MM, 0.1 * MM, ""),
        EDGES,
    ], example: "m.fillet(plate, 1mm, {edges: {top: true}})" },
    Proto { name: "Chamfer", call: "chamfer", args: &["source", "size"], group: Group::Modify, help: "Bevels edges of a solid by size back along each face.", fields: &[
        SOURCE,
        f("size", "Size", Kind::Length, true, 0.05 * MM, 100.0 * MM, 0.1 * MM, ""),
        EDGES,
    ], example: "m.chamfer(plate, 0.6mm, {edges: {bottom: true}})" },
    Proto { name: "Hole", call: "hole", args: &["source"], group: Group::Modify, help: "Drills holes into a solid from each point of at, along dir (into the material, default down), to depth or through: kind @simple, @counterbore, @countersink or @tapped.", fields: hole_fields!("into the material, default [0, -1, 0]"), example: "m.hole(plate, {kind: @counterbore diameter: 3.4mm bore_diameter: 6.5mm bore_depth: 3mm at: [[-32mm 8mm 22mm] [32mm 8mm 22mm]]})" },
    Proto { name: "HoleRow", call: "hole_row", args: &[], group: Group::Lighten, help: "A row of equal holes to cut away (m.subtract): count holes centred evenly from `from` to `to`, each `length` long along axis. A hole whose axis is level (across `up`, the print's up) gets a 45° roof: a teardrop prints without support.", fields: &[
        o("from", "First centre", Kind::Point, true, "the first hole's centre [x y z]"),
        o("to", "Last centre", Kind::Point, false, "the last hole's centre (default: from)"),
        f("count", "Count", Kind::Count, false, 1.0, 64.0, 1.0, "default 1"),
        f("diameter", "Diameter", Kind::Diameter, true, 0.3 * MM, 200.0 * MM, 0.1 * MM, ""),
        f("length", "Length", Kind::Length, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, "along the axis, centred on each centre: longer than the wall for a through hole"),
        o("axis", "Axis", Kind::Point, false, "[x y z] direction of the holes, default [0 1 0]"),
        o("up", "Print up", Kind::Point, false, "the print's up in the model frame, default [0 1 0]"),
        o("teardrop", "Teardrop", Kind::Bool, false, "a 45° roof on each hole (default: when the axis is level)"),
        SEGMENTS,
    ], example: "m.hole_row({from: [-20mm, 30mm, -20mm] to: [20mm, 30mm, -20mm] count: 3 diameter: 8mm length: 10mm axis: [0 0 1]})" },
    Proto { name: "Pocket", call: "pocket", args: &[], group: Group::Lighten, help: "A pocket to cut away (m.subtract): a rounded rectangle `size` (along, then across) opening at `center` on a face, `depth` into the material along axis. One whose axis is level (across `up`, the print's up) gets a 45° roof over its top edge, so it prints without support.", fields: &[
        o("center", "Opening centre", Kind::Point, true, "the middle of the pocket's opening on the face [x y z]"),
        o("size", "Size", Kind::Point2, true, "[along across]"),
        f("depth", "Depth", Kind::Length, true, 0.1 * MM, 1000.0 * MM, 0.1 * MM, "into the material from the face"),
        o("axis", "Axis", Kind::Point, true, "[x y z] into the material"),
        o("along", "Along", Kind::Point, false, "[x y z] direction of the size's first side, default level across the axis"),
        o("up", "Print up", Kind::Point, false, "the print's up in the model frame, default [0 1 0]"),
        f("corner", "Corner radius", Kind::Radius, false, 0.0, 50.0 * MM, 0.1 * MM, "rounds the pocket's corners and floor edges, default 1mm"),
        o("roof", "Roof", Kind::Bool, false, "a 45° roof over the top edge (default: when the axis is level)"),
    ], example: "m.pocket({center: [0, 30mm, -17mm] size: [40mm 20mm] depth: 4mm axis: [0, 0, -1] corner: 2mm})" },
    Proto { name: "Mirror", call: "mirror", args: &["source"], group: Group::Modify, help: "A mirror image across a plane through center: @yz flips X, @xz flips Y, @xy flips Z; keep (default true) joins it with the original.", fields: &[
        SOURCE,
        o("plane", "Mirror plane", Kind::Choice(&["yz", "xz", "xy"]), false, ""),
        o("center", "Through", Kind::Point, false, "[x y z]"),
        o("keep", "Keep original", Kind::Bool, false, "join both (default true)"),
    ], example: "m.mirror(arm, {plane: @yz})" },
    Proto { name: "Split", call: "split", args: &["source"], group: Group::Modify, help: "Splits a solid in two for printing at the plane through center facing normal (any angle), and gives the list [below above]: below behind the normal, above on its side. Alignment pins cross the cut, sized from the printer like the joints: pegs on below that go into holes in above, or with dowels: true holes in both and the pins as a third part. A cut with no room for a pin (thinner than about 4.5mm at the defaults) splits without, and the build's warnings say so with the pin radius that fits. On an exact (m.brep) part each half stays exact and its face on the cut is named cut (halves[0].cut).", fields: &[
        SOURCE,
        o("center", "Through", Kind::Point, true, "[x y z] a point of the cut's plane"),
        o("normal", "Facing", Kind::Point, false, "[x y z] direction, default [0 1 0] (a level cut)"),
        f("pins", "Pins", Kind::Count, false, 0.0, 12.0, 1.0, "how many, default 2; 0 for none"),
        f("pin", "Pin radius", Kind::Radius, false, 0.5 * MM, 10.0 * MM, 0.1 * MM, "default from the cut's room, up to 2.5mm"),
        f("depth", "Pin depth", Kind::Length, false, 1.0 * MM, 40.0 * MM, 0.5 * MM, "how far a pin reaches into each half, default twice its radius (less where a half is thin)"),
        o("dowels", "Dowels", Kind::Bool, false, "holes in both halves and the pins as a part of their own (default false: pegs on below)"),
        f("clearance", "Clearance", Kind::Length, false, 0.05 * MM, 1.0 * MM, 0.05 * MM, "a hole's radius over its pin's; default from the printer (0.35mm for a 0.4 nozzle)"),
    ], example: "let halves = m.split(body, {center: [0 20mm 0]})" },
    Proto { name: "Shell", call: "shell", args: &["source", "thickness"], group: Group::Modify, help: "Hollows a solid to walls of thickness, open at the @top (default), @bottom or @closed; the outside stays exact.", fields: &[
        SOURCE,
        f("thickness", "Wall", Kind::Length, true, 0.2 * MM, 50.0 * MM, 0.1 * MM, ""),
        o("open", "Open", Kind::Choice(&["top", "bottom", "closed"]), false, ""),
        o("side", "Wall side", Kind::Choice(&["inside", "both"]), false, "inside the surface (default), or straddling it (distance-field solids, open @closed)"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 5.0 * MM, 0.05 * MM, "meshing resolution, default thickness / 3"),
    ], example: "m.shell(box, 2mm)" },
    Proto { name: "Offset", call: "offset", args: &["source", "distance"], group: Group::Modify, help: "A solid's surface moved out by `distance` everywhere, or in when it is negative: its shape grown. A distance-field solid stays one field; any other is read through its mesh. It leaves the solid it reads as it is, which is what a print settings volume that follows a feature needs: m.settings(m.offset(arm, 1mm), {..}).", fields: &[
        SOURCE,
        f("distance", "Grow", Kind::Length, true, -50.0 * MM, 50.0 * MM, 0.1 * MM, "how far the surface moves out; negative moves it in"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 5.0 * MM, 0.05 * MM, "meshing resolution of a solid read through its mesh, default its largest side / 64"),
    ], example: "m.offset(arm, 1mm)" },
    // ---- combining
    Proto { name: "Union", call: "union", args: &["a", "b"], group: Group::Combine, help: "a joined with b (one part or a list).", fields: &[
        o("a", "A", Kind::Solid, true, ""), o("b", "B", Kind::Solids, true, ""),
    ], example: "m.union(base, [post_l post_r])" },
    Proto { name: "Difference", call: "subtract", args: &["a", "b"], group: Group::Combine, help: "a with b (one part or a list) cut away.", fields: &[
        o("a", "A", Kind::Solid, true, ""), o("b", "B", Kind::Solids, true, ""),
    ], example: "m.subtract(plate, holes)" },
    Proto { name: "Intersect", call: "intersect", args: &["a", "b"], group: Group::Combine, help: "What a and every b share.", fields: &[
        o("a", "A", Kind::Solid, true, ""), o("b", "B", Kind::Solids, true, ""),
    ], example: "m.intersect(block, ball)" },
    // ---- imports and smooth blends of distance-field solids
    Proto { name: "Import", call: "import", args: &["name"], group: Group::Primitive, help: "A mesh the host has (an imported or generated model), by its name: a solid for sharp booleans, and a distance field inside Blend and Carve.", fields: &[
        o("name", "Name", Kind::Text, true, "the mesh's name in the project"),
    ], example: "m.import(\"cat\")" },
    Proto { name: "Blend", call: "blend", args: &["of", "radius"], group: Group::Field, help: "Solids joined with a smooth fillet of `radius` where they meet, as one distance field meshed at `cell`: Sdf, Import, Box, Sphere, Cylinder, Torus, Ellipsoid, Capsule and other Blends and Carves (moved, turned, uniformly scaled).", fields: &[
        o("of", "Solids", Kind::Solids, true, "[a b ...]"),
        f("radius", "Blend", Kind::Radius, true, 0.0, 100.0 * MM, 0.1 * MM, "how wide the fillet is"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution (default: 80 cells over the bounds)"),
    ], example: "m.blend([figure ring], 2mm)" },
    Proto { name: "Carve", call: "carve", args: &["a", "b", "radius"], group: Group::Field, help: "a with every b cut away with a smooth edge of `radius`, as one distance field (the same solids as Blend).", fields: &[
        o("a", "A", Kind::Solid, true, ""), o("b", "B", Kind::Solids, true, ""),
        f("radius", "Blend", Kind::Radius, true, 0.0, 100.0 * MM, 0.1 * MM, "how soft the cut's edge is"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution"),
    ], example: "m.carve(figure, peg, 0.5mm)" },
    // ---- distance-field nodes: instances, bones, warps (each one distance field meshed at `cell`)
    Proto { name: "Instance", call: "instance", args: &["source", "places"], group: Group::Field, help: "Copies of a solid, one at each place: moved, turned and uniformly scaled; the source is built once.", fields: &[
        SOURCE,
        o("places", "Places", Kind::Places, true, "[Place{at rotate scale} ...] or [[x y z] ...]"),
        CELL,
    ], example: "m.instance(post, for x in [-20mm 0 20mm] { Place{at: [x 0 0]} })" },
    Proto { name: "Bone", call: "bone", args: &["source"], group: Group::Field, help: "A solid bound to a bone (named by its let) whose rest frame is `rest`; a renderer poses it at run time.", fields: &[
        SOURCE,
        o("rest", "Rest frame", Kind::Place, false, "Place{at rotate}: the bone's frame at rest (default: the origin)"),
        CELL,
    ], example: "m.bone(forearm, {rest: Place{at: [0 30mm 0]}})" },
    Proto { name: "Twist", call: "twist", args: &["source", "rate"], group: Group::Field, help: "A solid twisted about Y by `rate` per metre of height (place it to pick the axis).", fields: &[
        SOURCE,
        f("rate", "Twist", Kind::Number, true, -1000.0, 1000.0, 0.1, "angle per metre: 90° / 100mm"),
        CELL,
    ], example: "m.twist(bar, 90° / 40mm)" },
    Proto { name: "Bend", call: "bend", args: &["source", "rate"], group: Group::Field, help: "A solid bent in the XY plane about Z by `rate` per metre along X.", fields: &[
        SOURCE,
        f("rate", "Bend", Kind::Number, true, -1000.0, 1000.0, 0.1, "angle per metre: 45° / 100mm"),
        CELL,
    ], example: "m.bend(beam, 30° / 100mm)" },
    Proto { name: "Taper", call: "taper", args: &["source", "rate"], group: Group::Field, help: "A solid whose X and Z scale by 1 + rate * y (rate per metre of height).", fields: &[
        SOURCE,
        f("rate", "Taper", Kind::Number, true, -100.0, 100.0, 0.1, "scale change per metre: -0.5 / 100mm"),
        CELL,
    ], example: "m.taper(post, -0.4 / 60mm)" },
    Proto { name: "Fold", call: "fold", args: &["source", "axis"], group: Group::Field, help: "A solid's half on the positive side of `axis`, mirrored onto the other side: one symmetric field.", fields: &[
        SOURCE,
        o("axis", "Axis", Kind::Choice(&["x", "y", "z"]), true, "@x mirrors across the YZ plane"),
        CELL,
    ], example: "m.fold(half_face, @x)" },
    Proto { name: "Texture", call: "texture", args: &["source"], group: Group::Field, help: "A solid's surface displaced by a pattern (knurling, hexagons, noise or your own): `depth` deep where the pattern is 1, untouched where it is 0. @cylindrical wraps it round `axis` a whole number of times (no seam), @planar projects it along `axis`, @triplanar from all three axes blended by the surface's direction. top_angle and bottom_angle leave faces within that angle of flat untextured. The pattern also moves the surface the source has inside: texture one solid (or a carve), not a union of touching solids, whose shared faces it would cut open.", fields: &[
        SOURCE,
        o("pattern", "Pattern", Kind::Pattern, true, "Knurl{pitch angle kind flat}, Hex{size groove}, Noise{size} or Pattern{field cell slope}"),
        o("map", "Mapping", Kind::Choice(&["triplanar", "cylindrical", "planar"]), false, "how the pattern lies on the surface (default @triplanar)"),
        o("axis", "Axis", Kind::Choice(&["y", "x", "z"]), false, "the cylinder's axis or the plane's normal (default @y)"),
        f("depth", "Depth", Kind::Length, true, -10.0 * MM, 10.0 * MM, 0.05 * MM, "how deep the pattern cuts in; negative raises it"),
        f("radius", "Radius", Kind::Radius, false, 0.5 * MM, 1000.0 * MM, 0.1 * MM, "@cylindrical: the radius the pitch is measured on (default the solid's largest about the axis); the pattern fades out from twice the depth inside it to four times"),
        o("offset", "Offset", Kind::Point2, false, "[u v]: moves the pattern along the surface"),
        f("turn", "Turn", Kind::Angle, false, -180.0 * DEG, 180.0 * DEG, DEG, "turns the pattern in the surface"),
        f("top_angle", "Top angle", Kind::Angle, false, 0.0, 90.0 * DEG, DEG, "faces facing up within this angle stay untextured (default 0: off); at a sharp rim the smoothed direction turns over about `smooth`, so take 60deg or more to keep the face itself clean"),
        f("bottom_angle", "Bottom angle", Kind::Angle, false, 0.0, 90.0 * DEG, DEG, "faces facing down within this angle stay untextured (default 0: off; 60deg or more at a sharp rim)"),
        f("blend", "Blend", Kind::Angle, false, DEG, 60.0 * DEG, DEG, "the angles over which the texture fades in past top_angle and bottom_angle (default 15deg)"),
        f("smooth", "Smooth", Kind::Length, false, 0.1 * MM, 20.0 * MM, 0.1 * MM, "the angles and the blend read the surface's direction smoothed over this much (default twice the depth, at least 1mm)"),
        CELL,
    ], example: "m.texture(knob, {pattern: Knurl{pitch: 1mm angle: 30° kind: @diamond} map: @cylindrical depth: 0.4mm top_angle: 60° bottom_angle: 60°})" },
    Proto { name: "Voronoi", call: "voronoi", args: &["source"], group: Group::Field, help: "A solid's skin turned into a web of rounded struts: the faces of even cells spaced over the skin (`size` wide, `stretch` times taller along `axis`), `skin` deep, solid, if asked, in a band at the part's lowest point (`base`) and its highest (`rim`). Inside: a foam of struts along the same cells' edges (@foam), solid (@solid), or hollow (@none: the skin's underside then needs supports to print). @pointed tops end each hole in a point at the overhang angle, so the web prints without supports. Struts print from two line widths up.", fields: &[
        SOURCE,
        f("size", "Cell size", Kind::Length, true, 1.0 * MM, 500.0 * MM, 0.5 * MM, "how wide the cells are, seed to seed over the skin: a hole is this less a strut"),
        f("strut", "Strut", Kind::Length, true, 0.2 * MM, 50.0 * MM, 0.1 * MM, "how thick the struts are: two line widths or more to print"),
        f("skin", "Skin", Kind::Length, false, 0.2 * MM, 100.0 * MM, 0.1 * MM, "how deep the web reaches under the surface (default two struts)"),
        f("random", "Randomness", Kind::Number, false, 0.0, 0.7, 0.05, "how uneven the cells are, 0..0.7: 0 relaxes them towards a honeycomb (default 0.6)"),
        f("base", "Solid base", Kind::Length, false, 0.0, 100.0 * MM, 0.1 * MM, "a solid band this high from the part's lowest point along `axis`, the struts merging into it: what a part with a small foot stands on (default 0: none)"),
        f("rim", "Solid rim", Kind::Length, false, 0.0, 100.0 * MM, 0.1 * MM, "a solid band this deep from the part's highest point along `axis`, the holes ending in points under it (default 0: none)"),
        f("stretch", "Stretch", Kind::Number, false, 1.0, 4.0, 0.1, "the cells' height over their width along `axis`: taller cells give steeper hole tops (default 1.6)"),
        o("axis", "Axis", Kind::Choice(&["y", "x", "z"]), false, "the cells' tall axis and the print's up (default @y)"),
        o("core", "Core", Kind::Choice(&["foam", "solid", "none"]), false, "what fills the inside (default @foam)"),
        o("tops", "Hole tops", Kind::Choice(&["pointed", "round"]), false, "@pointed (default): each hole's top a point at the overhang angle; @round: as the cells make them"),
        f("overhang", "Overhang", Kind::Angle, false, 20.0 * DEG, 80.0 * DEG, DEG, "@pointed: the shallowest roof that prints, from the vertical (default 45°)"),
        f("round", "Round", Kind::Length, false, 0.0, 50.0 * MM, 0.1 * MM, "rounds the holes' corners and where the struts meet (default half a strut)"),
        f("seed", "Seed", Kind::Count, false, 0.0, 9999.0, 1.0, "another spread of the same kind of cells"),
        o("adapt", "Adapt", Kind::Choice(&["none", "stress"]), false, "@stress: thicker struts where the part's force study loads it (a solve of the solid part), up to `strut_max`; @none (default): all `strut`"),
        f("strut_max", "Thickest strut", Kind::Length, false, 0.2 * MM, 50.0 * MM, 0.1 * MM, "@stress: the struts' thickness where the stress is highest (default twice `strut`)"),
        CELL,
    ], example: "m.voronoi(cat, {size: 6mm strut: 1.5mm})" },
    Proto { name: "Grow", call: "grow", args: &["source"], group: Group::Field, help: "Material where the part's loads want it (topology optimisation): a solid's space, of which `mass` stays, grown by the force study's m.fixed and m.force. It shows from the first look and refines as it goes; a change to a load moves it from where it stands.", fields: &[
        SOURCE,
        f("mass", "Mass", Kind::Number, false, 0.05, 0.9, 0.01, "the share of the solid it keeps (default 0.3)"),
        f("member", "Thinnest member", Kind::Length, false, 0.5 * MM, 50.0 * MM, 0.1 * MM, "how thin its members may get (default 2 mm)"),
    ], example: "m.grow(space, {mass: 0.3 member: 2mm})" },
    Proto { name: "Veins", call: "veins", args: &["source", "pattern"], group: Group::Field, help: "A slime mould's veins raised on a solid's skin as rounded tubes: the mould (Physarum{..}) grows over the solid as a Mix grows it, and each vein is a tube as wide as the vein, its trunks up to 1.6 mm proud and its finest threads ridges 1 mm wide and 0.4 mm high. Only tubes that print without support rise: none on skin that leans past the printer's overhang angle, nor one whose own side would. Blend it into its solid (m.blend) and colour it (m.fill). Empty until the mould has grown.", fields: &[
        SOURCE,
        o("pattern", "Slime mould", Kind::Pattern, true, "Physarum{preset scale seed ..}: the mould whose veins it raises"),
    ], example: "m.veins(dish, Physarum{preset: @trunks scale: 10mm seed: 3})" },
    // ---- fields
    Proto { name: "Sdf", call: "sdf", args: &[], group: Group::Field, help: "A signed distance field: field is a Splash fn(p) returning the distance (negative inside), compiled as a kernel and meshed inside bounds at cell. sdf_* shapes and smooth_* blends are built in.", fields: &[
        o("field", "Field", Kind::Field, true, "fn(p) { return sdf_sphere(p, 10mm) }"),
        o("bounds", "Bounds", Kind::Bounds, true, "[[x y z] [x y z]]: a box the shape fits in"),
        f("cell", "Cell", Kind::Length, false, 0.05 * MM, 10.0 * MM, 0.05 * MM, "meshing resolution (default: 80 cells over the bounds)"),
    ], example: "m.sdf({field: fn(p) { return smooth_union(sdf_sphere(p, 10mm), sdf_box(p - vec3(12mm, 0, 0), vec3(30mm, 6mm, 6mm)), 4mm) } bounds: [[-12mm, -12mm, -12mm] [30mm 12mm 12mm]]})" },
    // ---- sketches and their shapes
    Proto { name: "Sketch", call: "", args: &[], group: Group::Sketch, help: "A 2D drawing: shapes nested into outlines with holes (a shape inside another is a hole).", fields: &[
        o("shapes", "Shapes", Kind::Sketch, true, "[Rect{..} Circle{..} ...]"),
    ], example: "Sketch{shapes: [Rect{size: [80mm 60mm] corner: 6mm} Circle{radius: 1.7mm at: [-32mm, -22mm]}]}" },
    Proto { name: "Rect", call: "", args: &[], group: Group::Shape, help: "A rectangle; corner rounds, chamfer cuts its corners.", fields: &[
        o("size", "Size", Kind::Point2, true, "[w h]"), AT2,
        f("corner", "Corner radius", Kind::Radius, false, 0.0, 500.0 * MM, 0.1 * MM, ""),
        f("chamfer", "Corner chamfer", Kind::Length, false, 0.0, 500.0 * MM, 0.1 * MM, ""),
    ], example: "Rect{size: [40mm 20mm] corner: 2mm}" },
    Proto { name: "Circle", call: "", args: &[], group: Group::Shape, help: "A circle.", fields: &[
        f("radius", "Radius", Kind::Radius, true, 0.05 * MM, 1000.0 * MM, 0.1 * MM, ""), AT2,
    ], example: "Circle{radius: 5mm}" },
    Proto { name: "Ellipse", call: "", args: &[], group: Group::Shape, help: "An ellipse.", fields: &[
        o("radii", "Radii", Kind::Point2, true, "[ru rv]"), AT2,
    ], example: "Ellipse{radii: [10mm 6mm]}" },
    Proto { name: "Slot", call: "", args: &[], group: Group::Shape, help: "A stadium round the segment from..to.", fields: &[
        o("from", "From", Kind::Point2, true, ""), o("to", "To", Kind::Point2, true, ""),
        f("radius", "Radius", Kind::Radius, true, 0.05 * MM, 500.0 * MM, 0.1 * MM, ""), AT2,
    ], example: "Slot{from: [-10mm 0] to: [10mm 0] radius: 2mm}" },
    Proto { name: "Regular", call: "", args: &[], group: Group::Shape, help: "A regular polygon of sides with this circumradius.", fields: &[
        f("sides", "Sides", Kind::Count, true, 3.0, 256.0, 1.0, ""),
        f("radius", "Radius", Kind::Radius, true, 0.05 * MM, 1000.0 * MM, 0.1 * MM, "circumradius"),
        f("angle", "Angle", Kind::Angle, false, -180.0 * DEG, 180.0 * DEG, 1.0 * DEG, ""), AT2,
    ], example: "Regular{sides: 6 radius: 5mm}" },
    Proto { name: "Polygon", call: "", args: &[], group: Group::Shape, help: "A polygon through points; fillet rounds every corner.", fields: &[
        o("points", "Points", Kind::Points2, true, "[[u v] ...]"),
        f("fillet", "Corner radius", Kind::Radius, false, 0.0, 500.0 * MM, 0.1 * MM, ""), AT2,
    ], example: "Polygon{points: [[0 0] [20mm 0] [0 20mm]]}" },
    Proto { name: "Offset2D", call: "", args: &[], group: Group::Shape, help: "Another shape moved out by d along its normals (negative d moves it in), corners mitred, round corners staying round: a clearance round a shape, or a wall's inner outline in the same sketch.", fields: &[
        o("of", "Shape", Kind::Sketch, true, "one shape: Rect, Circle, Slot, Regular, Polygon, Path or Ellipse"),
        f("d", "Distance", Kind::Length, true, -500.0 * MM, 500.0 * MM, 0.1 * MM, "outward; negative is inward"),
    ], example: "Offset2D{of: Rect{size: [40mm 20mm] corner: 2mm} d: 1.5mm}" },
    Proto { name: "SvgPath", call: "", args: &[], group: Group::Shape, help: "The outlines of SVG path data (a <path>'s d: M L H V C S Q T A Z), one SVG unit a millimetre unless width scales it; y is flipped so the drawing reads upright, its origin at `at`. Each closed subpath is an outline; one inside another is a hole. Curves are flattened within 0.01 mm.", fields: &[
        o("d", "Path data", Kind::Text, true, "\"M0 0 L20 0 L10 15 Z\""),
        f("width", "Width", Kind::Length, false, 0.1 * MM, 2000.0 * MM, 0.5 * MM, "scale the drawing to this width"), AT2,
    ], example: "SvgPath{d: \"M0 0 H20 V10 H0 Z M5 3 H15 V7 H5 Z\" width: 40mm}" },
    Proto { name: "Gear2D", call: "", args: &[], group: Group::Shape, help: "An involute spur gear's outline, a tooth on +u (turned by angle): m.extrude makes a spur gear, m.sweep with a twist a helical one, two sweeps twisted opposite ways a herringbone. No bore: put a Circle in the same sketch.", fields: &[
        f("module", "Module", Kind::Length, true, 0.2 * MM, 20.0 * MM, 0.1 * MM, "pitch diameter / teeth"),
        f("teeth", "Teeth", Kind::Count, true, 6.0, 400.0, 1.0, ""),
        f("pressure", "Pressure angle", Kind::Angle, false, 10.0 * DEG, 35.0 * DEG, 0.5 * DEG, "default 20°"),
        f("backlash", "Backlash", Kind::Length, false, 0.0, 2.0 * MM, 0.02 * MM, "play on the pitch circle (default 0.1mm)"),
        f("shift", "Profile shift", Kind::Number, false, -1.0, 1.0, 0.05, ""),
        f("angle", "Angle", Kind::Angle, false, -360.0 * DEG, 360.0 * DEG, 1.0 * DEG, "turns the teeth"), AT2,
    ], example: "Gear2D{module: 1.5mm teeth: 17}" },
    Proto { name: "Path", call: "", args: &[], group: Group::Shape, help: "A closed outline drawn from `from`. Write segments as children in order, named with := if wanted, or supply a segs list.", fields: &[
        o("from", "Start", Kind::Point2, false, "default [0 0]"),
        o("segs", "Segments", Kind::Segments, false, "[LineTo{to} ArcTo{to r} RoundCorner{r} ...]"), AT2,
    ], example: "Path{from: [0 0] base := LineTo{to: [30mm 0]} RoundCorner{r: 3mm} right := LineTo{to: [30mm 20mm]} ArcTo{to: [0 20mm] r: 15mm}}" },
    Proto { name: "LineTo", call: "", args: &[], group: Group::Segment, help: "A line to a point.", fields: &[o("to", "To", Kind::Point2, true, "")], example: "LineTo{to: [10mm 0]}" },
    Proto { name: "LineBy", call: "", args: &[], group: Group::Segment, help: "A line by an offset.", fields: &[o("by", "By", Kind::Point2, true, "")], example: "LineBy{by: [10mm 0]}" },
    Proto { name: "Horizontal", call: "", args: &[], group: Group::Segment, help: "Horizontally by d.", fields: &[f("d", "Distance", Kind::Length, true, -1000.0 * MM, 1000.0 * MM, 0.5 * MM, "")], example: "Horizontal{d: 10mm}" },
    Proto { name: "Vertical", call: "", args: &[], group: Group::Segment, help: "Vertically by d.", fields: &[f("d", "Distance", Kind::Length, true, -1000.0 * MM, 1000.0 * MM, 0.5 * MM, "")], example: "Vertical{d: 10mm}" },
    Proto { name: "Polar", call: "", args: &[], group: Group::Segment, help: "A line of length at angle (0 = +u, counter-clockwise).", fields: &[
        f("length", "Length", Kind::Length, true, 0.0, 1000.0 * MM, 0.5 * MM, ""), f("angle", "Angle", Kind::Angle, true, -360.0 * DEG, 360.0 * DEG, 1.0 * DEG, ""),
    ], example: "Polar{length: 10mm angle: 45°}" },
    Proto { name: "ArcTo", call: "", args: &[], group: Group::Segment, help: "An arc to `to` through a point, or of radius r (positive: counter-clockwise, the shorter arc; large: the longer).", fields: &[
        o("to", "To", Kind::Point2, true, ""), o("through", "Through", Kind::Point2, false, ""),
        f("r", "Radius", Kind::Radius, false, -1000.0 * MM, 1000.0 * MM, 0.1 * MM, ""), o("large", "Large arc", Kind::Bool, false, ""),
    ], example: "ArcTo{to: [0 20mm] r: 15mm}" },
    Proto { name: "TangentTo", call: "", args: &[], group: Group::Segment, help: "An arc to a point, tangent to the last segment.", fields: &[o("to", "To", Kind::Point2, true, "")], example: "TangentTo{to: [10mm 10mm]}" },
    Proto { name: "SplineTo", call: "", args: &[], group: Group::Segment, help: "A smooth curve through points, ending at the last.", fields: &[o("points", "Points", Kind::Points2, true, "")], example: "SplineTo{points: [[5mm 3mm] [10mm 0]]}" },
    Proto { name: "RoundCorner", call: "", args: &[], group: Group::Segment, help: "Rounds the corner at the current point.", fields: &[f("r", "Radius", Kind::Radius, true, 0.0, 500.0 * MM, 0.1 * MM, "")], example: "RoundCorner{r: 2mm}" },
    Proto { name: "CutCorner", call: "", args: &[], group: Group::Segment, help: "Cuts the corner at the current point back by d on each side.", fields: &[f("d", "Distance", Kind::Length, true, 0.0, 500.0 * MM, 0.1 * MM, "")], example: "CutCorner{d: 1mm}" },
    // ---- handles: where a parameter's drag handle sits (`handle(height, Arrow{..})`)
    Proto { name: "Arrow", call: "", args: &[], group: Group::Handle, help: "An arrow at `to` pointing away from `from`: dragging it moves the parameter so `to` follows.", fields: &[
        o("from", "From", Kind::Point, false, "default [0 0 0]"), o("to", "To", Kind::Point, true, "where the value shows (an expression of the param)"),
    ], example: "Arrow{from: [0 0 0] to: [0 height 0]}" },
    Proto { name: "Ring", call: "", args: &[], group: Group::Handle, help: "A ring of `radius` about `axis` through `center`: dragging it moves the parameter so the ring follows.", fields: &[
        o("center", "Centre", Kind::Point, false, ""), o("axis", "Axis", Kind::Point, false, "default [0 1 0]"), f("radius", "Radius", Kind::Radius, true, 0.0, 1000.0 * MM, 0.1 * MM, ""),
    ], example: "Ring{center: [0 20mm 0] radius: belly}" },
    Proto { name: "Stepper", call: "", args: &[], group: Group::Handle, help: "+ and - buttons at `at` along `axis`.", fields: &[
        o("at", "At", Kind::Point, true, ""), o("axis", "Axis", Kind::Point, false, "default [1 0 0]"),
    ], example: "Stepper{at: [20mm 0 0]}" },
    // ---- looks and placements
    Proto { name: "Material", call: "", args: &[], group: Group::Look, help: "A surface: colour, metalness, roughness, glow and splat look; give it to m.fill or m.stroke.", fields: &[
        o("albedo", "Colour", Kind::Color, false, "[r g b], 0..1"),
        f("filament", "Filament", Kind::Count, false, 1.0, 64.0, 1.0, "the filament this material prints in (Fab); unset: the loaded filament nearest its colour"),
        f("fade", "Fade", Kind::Ratio, false, 0.0, 1.0, 0.01, "transparency"),
        f("metal", "Metal", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("rough", "Rough", Kind::Ratio, false, 0.0, 1.0, 0.01, "default 0.5"),
        f("emissive", "Glow", Kind::Number, false, 0.0, 100.0, 0.1, "glow strength"),
        o("emissive_color", "Glow colour", Kind::Color, false, "[r g b]"),
        o("unlit", "Unlit", Kind::Bool, false, ""),
        o("colorize", "Colorize", Kind::Bool, false, "tinted by the instance colour"),
        o("splat", "Splat", Kind::Splat, false, "Splat{size opacity fluff ...}"),
    ], example: "Material{albedo: [0.62 0.64 0.68] metal: 1 rough: 0.3}" },
    Proto { name: "Flood", call: "", args: &[], group: Group::Look, help: "A smooth-region fill for m.fill: the part's surface round seed, spreading while neighbouring normals stay within angle, so it stops at creases and edges sharper than that; recomputed on the current shape.", fields: &[
        o("seed", "Seed", Kind::Point, true, "a point on the part's surface"),
        f("angle", "Angle", Kind::Angle, false, DEG, 180.0 * DEG, DEG, "the largest angle between neighbouring normals it crosses (default 30deg)"),
    ], example: "m.fill(Flood{seed: [0 24mm 21mm] angle: 30deg}, Material{albedo: [0.8 0.05 0.05]})" },
    Proto { name: "Mix", call: "", args: &[], group: Group::Look, help: "Materials through the part's volume by a field, for m.fill: field is a Splash fn(p) from a point (model frame) to a place in materials: 0 the first, 1 the second, a fraction between two a blend of them, which prints dithered (each bead one or the other) and reads as the blend. A whole number is one material, so floor(..) cuts sharp regions. noise.value, perlin, simplex, cells (Voronoi: cell_id, edges), fbm, ridged, turbulence, warp, ramp and bands are built in.", fields: &[
        o("materials", "Materials", Kind::Materials, true, "[Material{..} Material{..} ...]: two or more"),
        o("field", "Field", Kind::Field, true, "fn(p) { 0.5 + 0.5 * noise.fbm(p / 8mm, 1, 3) }, or a pattern grown on the part: Turing{preset: @spots scale: 5mm}, Lichtenberg{preset: @tree scale: 6mm}, Scales{preset: @spiral scale: 6mm}, Physarum{preset: @network scale: 8mm}, Flow{preset: @lines scale: 4mm}"),
        f("sharp", "Sharp", Kind::Ratio, false, 0.0, 1.0, 0.01, "how sharply a blend turns from one material to the next: 0 as the field gives it, 1 at once halfway (default 0)"),
        f("balance", "Balance", Kind::Number, false, -1.0, 1.0, 0.01, "added to every place: over 0 toward the later materials (default 0)"),
    ], example: "m.fill(vase, Mix{materials: [blue yellow] field: fn(p) { 0.5 + 0.5 * noise.fbm(p / 8mm, 1, 3) }})" },
    Proto { name: "Turing", call: "", args: &[], group: Group::Look, help: "A Turing pattern for a Mix's field: reaction-diffusion grown on the part's own surface, so spots and stripes follow its skin. Seeded, so the same every time, and the print takes its state at exactly steps; the views show it growing. 0..1 over the Mix's materials: sharp: 1 for two colours sharp, three or more for bands by level, balance to grow or shrink the second material.", fields: &[
        o("preset", "Preset", Kind::Choice(TURING_PRESETS), false, "@spots (default), @stripes, @labyrinth, @coral or @mitosis (Gray-Scott), or @dots (an activator and its inhibitor)"),
        f("scale", "Scale", Kind::Length, false, 0.8 * MM, 200.0 * MM, 0.1 * MM, "from one spot or stripe to the next (default 5mm)"),
        f("seed", "Seed", Kind::Count, false, 0.0, 99999.0, 1.0, "which pattern of its kind (default 1)"),
        f("steps", "Steps", Kind::Count, false, 0.0, 200000.0, 100.0, "how many steps it grows; the print takes the state at exactly this many (default 3000, @dots 2000)"),
        f("depth", "Depth", Kind::Length, false, 0.0, 200.0 * MM, 0.5 * MM, "how deep it grows from the surface: 0 the skin (default); deeper, through the part, spots as balls and stripes as sheets"),
        f("feed", "Feed", Kind::Number, false, 0.0, 0.12, 0.001, "Gray-Scott's feed rate (default the preset's)"),
        f("kill", "Kill", Kind::Number, false, 0.0, 0.08, 0.001, "Gray-Scott's kill rate (default the preset's)"),
        f("saturation", "Saturation", Kind::Number, false, 0.0, 2.0, 0.01, "@dots: how soon the activator saturates (default 0)"),
    ], example: "m.fill(lizard, Mix{materials: [teal dark] field: Turing{preset: @spots scale: 5mm seed: 2} sharp: 1})" },
    Proto { name: "Lichtenberg", call: "", args: &[], group: Group::Look, help: "A Lichtenberg figure for a Mix's field: a branching tree grown over the part's own surface by diffusion-limited aggregation, from the part's lowest point (and from more roots spread farthest apart), so its branches follow the skin. Seeded, so the same every time, and the print takes its state at exactly steps; the views show it growing. 0..1 over the Mix's materials, the tree's the second: sharp: 1 for two colours sharp, balance to thicken or thin the branches.", fields: &[
        o("preset", "Preset", Kind::Choice(LICHTENBERG_PRESETS), false, "@tree (default), @circuit (straight runs along the print's axes that turn at right angles) or @frost (denser branches)"),
        f("scale", "Scale", Kind::Length, false, 0.8 * MM, 200.0 * MM, 0.1 * MM, "from one branch to the next (default 5mm)"),
        f("seed", "Seed", Kind::Count, false, 0.0, 99999.0, 1.0, "which figure of its kind (default 1)"),
        f("steps", "Steps", Kind::Count, false, 0.0, 200000.0, 100.0, "how many steps it grows; the print takes the state at exactly this many (default 1200, @circuit 3000)"),
        f("depth", "Depth", Kind::Length, false, 0.0, 200.0 * MM, 0.5 * MM, "how deep it grows from the surface: 0 the skin (default); deeper, the tree branches through the part"),
        f("stick", "Stick", Kind::Ratio, false, 0.01, 1.0, 0.01, "the chance a walker beside the tree hits the cell it is on: less grows denser branches (default the preset's: 1, @frost 0.15)"),
        f("hits", "Hits", Kind::Count, false, 1.0, 16.0, 1.0, "the hits a cell takes to join the tree: more grows fewer, straighter branches, along the print's axes from about three (default the preset's: 1, @circuit 4)"),
        f("roots", "Roots", Kind::Count, false, 1.0, 64.0, 1.0, "how many points it grows from: the part's lowest point, then each next the farthest from those (default 1)"),
    ], example: "m.fill(vase, Mix{materials: [ivory blue] field: Lichtenberg{preset: @tree scale: 6mm seed: 2} sharp: 1})" },
    Proto { name: "Scales", call: "", args: &[], group: Group::Look, help: "Scales for a Mix's field: the part's skin tiled round seed points, each scale the skin nearest its seed (over the surface), grown out from the seeds till the scales meet. @spiral sets the seeds as a sunflower sets its florets, round the part's upright axis, so they run in crossing spirals; @even throws them at random. Seeded, so the same every time, and the print takes its state at exactly steps; the views show them growing. 0..1 over the Mix's materials, the outlines (and filled scales) the second: sharp: 1 for two colours sharp.", fields: &[
        o("preset", "Preset", Kind::Choice(SCALES_PRESETS), false, "@spiral (default: a sunflower's spiral round the upright axis, darts where it leaves the skin bare) or @even (thrown at random, evenly apart)"),
        f("scale", "Scale", Kind::Length, false, 0.8 * MM, 200.0 * MM, 0.1 * MM, "from one scale's middle to the next (default 5mm)"),
        f("seed", "Seed", Kind::Count, false, 0.0, 99999.0, 1.0, "which scales of their kind (default 1)"),
        f("steps", "Steps", Kind::Count, false, 0.0, 200000.0, 10.0, "how many steps they grow, a cell a step; the print takes the state at exactly this many (default 60, past where they meet)"),
        f("depth", "Depth", Kind::Length, false, 0.0, 200.0 * MM, 0.5 * MM, "how deep they grow from the surface: 0 the skin (default); deeper, through the part"),
        f("edge", "Edge", Kind::Ratio, false, 0.0, 0.5, 0.01, "each outline's width over the scale (default 0.15); 0 for none"),
        f("fill", "Fill", Kind::Ratio, false, 0.0, 1.0, 0.01, "the share of scales the second material through (default 0)"),
    ], example: "m.fill(egg, Mix{materials: [cream green] field: Scales{preset: @spiral scale: 6mm fill: 0.3} sharp: 1})" },
    Proto { name: "Physarum", call: "", args: &[], group: Group::Look, help: "A slime mould's network for a Mix's field: the mould spreads over the part's skin from the point on top of its middle (a dish's floor, a pebble's crown), its agents crawling out to its front and home again, each laying a trail and turning toward the strongest trail just ahead while the trail spreads and fades, so the traffic gathers onto veins: trunks near where it started that branch into fine threads, and lace at the front, each vein as wide as its trail is strong. Seeded, so the same every time, and the print takes its state at exactly steps; the views show it growing. 0..1 over the Mix's materials, the veins the second: sharp: 1 for two colours sharp.", fields: &[
        o("preset", "Preset", Kind::Choice(PHYSARUM_PRESETS), false, "@network (default: it reaches the far side of the part at its last step), @trunks (all over the part by half its steps, its veins thickened into trunks) or @lace (still spreading, its front a fan of lace a third of the way over the part)"),
        f("scale", "Scale", Kind::Length, false, 0.8 * MM, 200.0 * MM, 0.1 * MM, "about how far across the finest meshes are: a thread is about a fifth of it across, a trunk up to two thirds (default 5mm)"),
        f("seed", "Seed", Kind::Count, false, 0.0, 99999.0, 1.0, "which network of its kind (default 1)"),
        f("steps", "Steps", Kind::Count, false, 0.0, 200000.0, 10.0, "how many steps it grows, a cell a step; the print takes the state at exactly this many (default 800)"),
        f("depth", "Depth", Kind::Length, false, 0.0, 200.0 * MM, 0.5 * MM, "how deep it grows from the surface: 0 the skin (default); deeper, through the part"),
        f("sense", "Sense", Kind::Angle, false, 5.0 * DEG, 90.0 * DEG, DEG, "how far either way an agent looks for trail ahead of it (default 22.5deg)"),
        f("turn", "Turn", Kind::Angle, false, 5.0 * DEG, 90.0 * DEG, DEG, "how far an agent turns toward the trail it finds (default 45deg)"),
        f("spread", "Spread", Kind::Number, false, 0.1, 4.0, 0.05, "how far it has spread by its last step, as a share of the way to the part's far side: 1 just there (default the preset's: 1, @trunks 2, @lace 0.35)"),
    ], example: "m.fill(bowl, Mix{materials: [cream ochre] field: Physarum{preset: @network scale: 8mm seed: 2} sharp: 1})" },
    Proto { name: "Flow", call: "", args: &[], group: Group::Look, help: "Flow lines for a Mix's field: lines drawn a scale apart along a swirling flow over the part's skin (the curl of a noise laid into the surface), each stopping short of the others, as a field's streamlines are drawn evenly. Seeded, so the same every time, and the print takes its state at exactly steps; the views show the lines being drawn. 0..1 over the Mix's materials, the lines the second: sharp: 1 for two colours sharp.", fields: &[
        o("preset", "Preset", Kind::Choice(FLOW_PRESETS), false, "@streaks (default: short strokes that taper to their ends), @lines (long, even lines) or @contours (long lines round a noise's contours, as on a map)"),
        f("scale", "Scale", Kind::Length, false, 0.8 * MM, 200.0 * MM, 0.1 * MM, "from one line to the next (default 5mm)"),
        f("seed", "Seed", Kind::Count, false, 0.0, 99999.0, 1.0, "which flow and lines of their kind (default 1)"),
        f("steps", "Steps", Kind::Count, false, 0.0, 200000.0, 10.0, "how many steps it grows, a line or more a step; the print takes the state at exactly this many (default 1000, past where the lines run out on most parts)"),
        f("depth", "Depth", Kind::Length, false, 0.0, 200.0 * MM, 0.5 * MM, "how deep the lines go from the surface: 0 the skin (default); deeper, through the part"),
        f("length", "Length", Kind::Number, false, 1.0, 200.0, 1.0, "the longest a line goes, in scales (default the preset's: 4, @lines and @contours 60)"),
        f("width", "Width", Kind::Ratio, false, 0.05, 0.5, 0.01, "each line's width over the scale (default the preset's: 0.45, @lines and @contours 0.35)"),
        f("swirl", "Swirl", Kind::Number, false, 1.0, 50.0, 0.5, "how far across the flow's swirls are, in scales (default the preset's: 5, @lines and @contours 6)"),
        f("taper", "Taper", Kind::Ratio, false, 0.0, 1.0, 0.01, "how much of its width a line loses toward its ends: 1 to a point (default the preset's: 1, @lines and @contours 0)"),
    ], example: "m.fill(vase, Mix{materials: [ivory teal] field: Flow{preset: @lines scale: 4mm seed: 2} sharp: 1})" },
    // ---- where a force study holds and loads the part
    Proto { name: "Face", call: "", args: &[], group: Group::Study, help: "A face of the part for m.fixed, m.force, m.pressure or m.torque: the smooth surface round at, up to its edges (where it turns more than 30deg from one step to the next, or 40deg from the face at at); found again on the current shape after every edit.", fields: &[
        o("at", "At", Kind::Point, true, "a point on the face (model frame)"),
    ], example: "m.force(Face{at: [0 6mm 10mm]}, [0, -100, 0])" },
    Proto { name: "HoleWall", call: "", args: &[], group: Group::Study, help: "A hole's wall for m.fixed, m.force, m.pressure or m.torque: the cylinder through at (a bolt's, a pin's, a shaft's), its axis and radius fitted to the surface; found again on the current shape after every edit.", fields: &[
        o("at", "At", Kind::Point, true, "a point on the hole's wall (model frame)"),
    ], example: "m.torque(HoleWall{at: [2.3mm 3mm 0]}, 500)" },
    Proto { name: "Splat", call: "", args: &[], group: Group::Look, help: "A material's splat look: point size, opacity, fluff and jitters.", fields: &[
        f("size", "Size", Kind::Length, false, 0.0, 50.0 * MM, 0.1 * MM, ""),
        f("opacity", "Opacity", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("fluff", "Fluff", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_size", "Size jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_pos", "Position jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_rot", "Rotation jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_bump", "Bump jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_metal", "Metal jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_bright", "Brightness jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
        f("jitter_color", "Colour jitter", Kind::Ratio, false, 0.0, 1.0, 0.01, ""),
    ], example: "Splat{size: 1mm opacity: 0.9 fluff: 0.2}" },
    // ---- surface patterns (m.texture)
    Proto { name: "Knurl", call: "", args: &[], group: Group::Look, help: "Knurling: V grooves `pitch` apart round the surface, at `angle` to the axis; @diamond crosses two sets, @diagonal has one, @straight runs along the axis.", fields: &[
        f("pitch", "Pitch", Kind::Length, true, 0.2 * MM, 20.0 * MM, 0.05 * MM, "from one groove to the next, round the surface"),
        f("angle", "Angle", Kind::Angle, false, 5.0 * DEG, 85.0 * DEG, DEG, "the grooves' angle to the axis (default 30deg; @straight runs along it)"),
        o("kind", "Kind", Kind::Choice(&["diamond", "straight", "diagonal"]), false, "default @diamond"),
        f("flat", "Flat", Kind::Ratio, false, 0.0, 0.9, 0.01, "the flat top between grooves, a fraction of the pitch (default 0.2); the grooves are rounded at the bottom and the top"),
    ], example: "Knurl{pitch: 1mm angle: 30° kind: @diamond}" },
    Proto { name: "Hex", call: "", args: &[], group: Group::Look, help: "Hexagonal cells `size` across with grooves between them.", fields: &[
        f("size", "Size", Kind::Length, true, 0.5 * MM, 100.0 * MM, 0.1 * MM, "across the flats of a cell"),
        f("groove", "Groove", Kind::Ratio, false, 0.02, 0.5, 0.01, "each side's groove, a fraction of the size (default 0.15)"),
    ], example: "Hex{size: 4mm groove: 0.15}" },
    Proto { name: "Noise", call: "", args: &[], group: Group::Look, help: "A smooth random bumpy surface with bumps about `size` apart.", fields: &[
        f("size", "Size", Kind::Length, true, 0.2 * MM, 100.0 * MM, 0.1 * MM, "how far apart the bumps are"),
    ], example: "Noise{size: 2mm}" },
    Proto { name: "Pattern", call: "", args: &[], group: Group::Look, help: "Your own pattern: field is a Splash fn(c) from the point in cells (c.x and c.y along the surface, c.z the height above it) to 0..1 (0 the untouched surface, 1 the full depth); give it period 1 in c.x and c.y to wrap without a seam.", fields: &[
        o("field", "Field", Kind::Field, true, "fn(c) { return 0.5 + 0.5 * sin(c.x * TAU) }"),
        o("cell", "Cell", Kind::Point, false, "[u v w]: one cell's size (default [1mm 1mm 1mm])"),
        f("slope", "Slope", Kind::Number, false, 0.0, 1000.0, 0.1, "the most the value changes per cell (default: measured)"),
    ], example: "Pattern{field: fn(c) { return 0.5 + 0.5 * sin(c.x * TAU) } cell: [2mm 2mm 2mm] slope: 3.2}" },
    Proto { name: "Place", call: "", args: &[], group: Group::Look, help: "A placement: scaled (one number), turned about X, Y, Z, then moved.", fields: &[
        o("at", "Position", Kind::Point, false, "[x y z]"),
        f("rotate", "Rotation", Kind::Rotation, false, -2.0 * std::f64::consts::PI, 2.0 * std::f64::consts::PI, DEG, "[x y z]"),
        f("scale", "Scale", Kind::Ratio, false, 0.01, 100.0, 0.01, "uniform"),
    ], example: "Place{at: [20mm 0 0] rotate: [0 45° 0]}" },
    Proto { name: "Frame", call: "", args: &[], group: Group::Look, help: "A named frame a part carries for the host (its `frame:` field): that frame's origin and axes in the part's own frame, turned about X, Y, Z, then moved. It moves with the part's placement; the host reads it in the model frame.", fields: &[
        o("name", "Name", Kind::Text, true, "the host's name for it"),
        o("at", "Position", Kind::Point, false, "[x y z]"),
        f("rotate", "Rotation", Kind::Rotation, false, -2.0 * std::f64::consts::PI, 2.0 * std::f64::consts::PI, DEG, "[x y z]"),
    ], example: "Frame{name: \"datum\" at: [0 10mm 0] rotate: [-90° 0 0]}" },
    Proto { name: "Section", call: "", args: &[], group: Group::Sketch, help: "One section of a Loft: one outline at a distance along the plane's normal.", fields: &[
        SKETCH, f("at", "At", Kind::Length, true, -2000.0 * MM, 2000.0 * MM, 0.5 * MM, ""),
    ], example: "Section{sketch: Circle{radius: 10mm} at: 40mm}" },
    Proto { name: "LoftRing", call: "", args: &[], group: Group::Sketch, help: "One planar outline of a RingLoft in its own (u, v) frame. Its axes are unit and perpendicular; its points are star-shaped about at and counter-clockwise seen from u x v.", fields: &[
        o("at", "Origin", Kind::Point, true, "[x y z]"),
        o("u", "U axis", Kind::Point, true, "unit in-plane axis [x y z], plain numbers"),
        o("v", "V axis", Kind::Point, true, "unit in-plane axis [x y z], perpendicular to u, plain numbers"),
        o("points", "Points", Kind::Points2, true, "[[u v] ...]: outline about the ring's origin"),
        o("crease", "Crease", Kind::Bool, false, "sharp side edge at this ring for exact surfaces; the mesh is unchanged (default false)"),
    ], example: "LoftRing{at: [0 0 0] u: [1 0 0] v: [0, 0, -1] points: [[-5mm, -5mm] [5mm, -5mm] [5mm 5mm] [-5mm, 5mm]]}" },
    Proto { name: "Helix", call: "", args: &[], group: Group::Sketch, help: "A helix about +Y for a Sweep.", fields: &[
        f("radius", "Radius", Kind::Radius, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, ""),
        f("pitch", "Pitch", Kind::Length, true, 0.1 * MM, 1000.0 * MM, 0.5 * MM, "rise per turn"),
        f("turns", "Turns", Kind::Number, true, 0.1, 1000.0, 0.5, ""),
    ], example: "Helix{radius: 10mm pitch: 4mm turns: 5}" },
];

pub fn proto(name: &str) -> Option<&'static Proto> {
    PROTOS.iter().find(|p| p.name == name)
}

/// The part a builder call makes (`subtract` -> `Difference`).
pub fn of_call(call: &str) -> Option<&'static Proto> {
    PROTOS.iter().find(|p| !p.call.is_empty() && p.call == call)
}

/// What other CAD tools call a builder Fab has under another name or in another form.
const SYNONYMS: &[(&[&str], &str)] = &[
    (&["cut", "difference", "minus", "diff"], "m.subtract(a, b): a with b (one part or a list) cut away"),
    (&["join", "combine", "add", "merge", "fuse"], "m.union(a, b): a joined with b (one part or a list)"),
    (&["common", "intersection"], "m.intersect(a, b)"),
    (&["cone", "frustum"], "m.cylinder({radius height top_radius}), top_radius 0 for a point"),
    (&["cube", "block", "cuboid"], "m.box({size: [x y z]})"),
    (&["hollow"], "m.shell(part, thickness)"),
    (&["thicken"], "m.offset(part, distance), or m.shell(part, thickness, {side: @both})"),
    (&["pattern", "linear_pattern", "circular_pattern", "polar_pattern", "array"], "a for loop of parts, or m.instance(part, for i in .. { Place{at rotate} }), which joins the copies"),
    (&["move", "translate", "rotate", "copy", "transform", "place"], "the `at`, `rotate` and `scale` options of the builder that makes the part; m.instance(part, [Place{at rotate}]) places an existing part"),
    (&["split", "slice"], "m.intersect or m.subtract with a large m.box slab on one side of the cut"),
    (&["counterbore", "countersink", "tapped_hole", "hole_wizard", "drill"], "m.hole(part, {kind: @counterbore diameter at dir ..})"),
    (&["helix", "spring", "coil"], "m.sweep({sketch: Circle{radius: 1mm} helix: Helix{radius pitch turns}})"),
    (&["draft"], "m.extrude({.. draft}) for a drafted extrusion, m.taper(part, rate) for a solid"),
    (&["minkowski", "round", "inflate"], "m.offset(part, distance), which rounds every edge, or m.box({size radius})"),
    (&["pad", "linear_extrude", "prism"], "m.extrude({sketch height})"),
    (&["rotate_extrude", "lathe"], "m.revolve({sketch angle}) about Y, sketch u the radius"),
    (&["pocket"], "m.subtract(part, m.extrude({sketch height ..}))"),
    (&["circle", "rect", "rectangle", "polygon", "ellipse", "slot", "square"], "the value types Circle{}, Rect{}, Polygon{}, Ellipse{}, Slot{} inside a sketch: m.extrude({sketch: Circle{radius: 5mm} height: 2mm})"),
    (&["offset2d", "offset_curve", "offset_sketch"], "the sketch shape Offset2D{of: Rect{..} d: 1mm}"),
];

/// A hint for an `m.<name>` call that is no builder: what Fab calls it, or the nearest builder.
pub fn unknown_call_hint(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if let Some((_, fab)) = SYNONYMS.iter().find(|(names, _)| names.contains(&lower.as_str())) {
        return format!("Fab's is {fab}");
    }
    let near = PROTOS.iter().filter(|p| !p.call.is_empty()).map(|p| (edit_distance(&lower, p.call), p.call)).min();
    match near {
        Some((d, call)) if d <= 2 && d < lower.len() => format!("did you mean m.{call}? fab.guide {{section: \"vocabulary\"}} lists every builder"),
        _ => "fab.guide {section: \"names\"} has other CAD tools' words, {section: \"vocabulary\"} every builder".to_string(),
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + (ca != *cb) as usize).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}
