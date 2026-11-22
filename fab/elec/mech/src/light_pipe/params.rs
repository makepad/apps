//! Light pipe dimensions, picker choices, file parameters and build measurements.

use crate::csg::Csg;
pub use crate::draft::DraftZone;
use crate::params::choices;
use crate::params::{self as part_params, field, ParamTable, PartParams};
use fab_materials::{Clarity, Colour, FinishId, MaterialId, Processes};
use std::sync::OnceLock;

choices! {
    /// A generic LED body size. When the pipe is bound to a board, the LED part's body data
    /// replaces this table; board binding comes later.
    LedKind {
        Rgb1010 => "1010 RGB",
        Rgb2020 => "2020 RGB",
        Rgb5050 => "5050 RGB",
        Rgb3535 => "3535 RGB",
        Rgb3228ReverseMount => "3228 RGB reverse mount",
        Chip0603 => "0603 chip",
        Chip0805 => "0805 chip",
        Custom => "Custom",
    }
}
choices! { TipTop { Flat => "Flat", Dome => "Dome", Lens => "Lens" } }
choices! { FrostMode {
    None => "None", Top => "Top face", TopAndSides => "Top + tip sides", WholeTip => "Whole tip",
} }
choices! { BodyShape { Round => "Round", Square => "Square" } }
choices! { PocketShape { Rect => "Rectangular", Round => "Round" } }
choices! { EntryFace { Flat => "Flat", ConvexLens => "Convex lens", ConcaveDimple => "Concave dimple" } }
choices! { LegStyle {
    None => "None", HookOut => "Hook outward", HookIn => "Hook inward", SplitPin => "Split pin",
    PressPeg => "Press-fit pegs", HeatStake => "Heat-stake pins", LocatingPeg => "Locating pegs",
    HookAndPeg => "Hook + peg",
} }
choices! { Parting { ShoulderTop => "Shoulder top", ShoulderBottom => "Shoulder bottom" } }
choices! { Mount { Pcb => "PCB", PanelBack => "Panel, from behind", PanelFront => "Panel, from the front" } }
choices! { PanelHead { Flange => "Flange", Countersunk => "Countersunk" } }

/// Dimensions and manufacturing choices for a light pipe.
#[derive(Clone, Debug, PartialEq)]
pub struct PipeParams {
    pub name: String,

    /// PCB thickness, mm.
    pub pcb_thickness: f64,
    /// Stored [`LedKind`] index.
    pub led_type: i32,
    /// Custom LED footprint along x, mm.
    pub led_w: f64,
    /// Custom LED footprint along y, mm.
    pub led_d: f64,
    /// Custom LED top above the PCB, mm.
    pub led_h: f64,
    /// PCB top to the panel's back face, mm.
    pub led_to_panel: f64,
    /// Panel thickness, mm.
    pub panel_thickness: f64,
    /// Panel hole diameter, mm.
    pub hole_diameter: f64,

    /// Radial clearance in the panel hole at its front face, mm.
    pub tip_clearance: f64,
    /// Apex above the panel front face, mm; zero is flush, negative is recessed.
    pub tip_protrusion: f64,
    /// Stored [`TipTop`] index.
    pub tip_top: i32,
    /// Dome or lens height, mm.
    pub tip_dome: f64,
    /// Flat top's edge chamfer, mm.
    pub tip_chamfer: f64,

    /// Stored [`FrostMode`] index; frosting changes the surface, not the solid.
    pub frost_mode: i32,
    pub frost_finish: FinishId,
    /// Texture depth for the drawing note, um; zero uses the finish grade.
    pub frost_depth: f64,

    /// Shoulder diameter at the parting line, mm.
    pub shoulder_diameter: f64,
    /// Shoulder and bridge thickness, mm.
    pub shoulder_height: f64,
    /// Shoulder top to the panel's back face, mm.
    pub shoulder_gap: f64,
    /// Chamfer on the edge away from the parting line, mm.
    pub shoulder_chamfer: f64,

    /// Grow the body, shoulder and hole spacing to fit the LED; sliders give minimums.
    pub fit_to_led: bool,
    /// Stored [`BodyShape`] index.
    pub body_shape: i32,
    /// Diameter at the shoulder, or width across square flats, mm.
    pub body_diameter: f64,
    /// Taper per side, degrees; the body narrows toward the PCB.
    pub body_taper: f64,
    /// Square body's corner radius, mm.
    pub body_corner: f64,
    /// Radial foot flange on the PCB, mm; an undercut against the shoulder.
    pub foot_flange: f64,
    /// Foot flange height, mm.
    pub foot_height: f64,

    /// Stored [`PocketShape`] index.
    pub pocket_shape: i32,
    /// Clearance around the LED at the pocket ceiling, mm.
    pub pocket_clearance: f64,
    /// LED top to the light-entry face, mm.
    pub air_gap: f64,
    /// Stored [`EntryFace`] index.
    pub entry_face: i32,
    /// Entry lens sag, mm.
    pub lens_sag: f64,

    /// Stored [`Mount`] index.
    pub mount: i32,
    /// Stored [`PanelHead`] index, for insertion from the front.
    pub panel_head: i32,
    /// Radial head overhang beyond the panel hole, mm.
    pub head_overhang: f64,
    /// Flange thickness on the panel front face, mm.
    pub flange_thickness: f64,
    /// Included angle of the panel hole's countersink, degrees.
    pub countersink_angle: f64,
    /// Number of ribs in the panel hole or press-fit peg's PCB hole; zero disables them.
    pub crush_ribs: i32,
    /// Radial rib crest beyond the hole wall at mid-hole, mm.
    pub rib_interference: f64,
    /// Rib width at its crest, mm.
    pub rib_width: f64,
    /// Plus/minus PCB-to-panel tolerance, mm; a panel-held foot clears the LED at the minimum.
    pub stackup_tol: f64,
    /// Radial peg clearance in the PCB hole at the PCB top, mm.
    pub peg_clearance: f64,
    /// Peg tip below the PCB bottom, mm; the heat stake length, or negative for a blind peg.
    pub peg_below: f64,

    /// Stored [`LegStyle`] index.
    pub leg_style: i32,
    /// PCB hole spacing centre to centre along x, mm.
    pub hole_spacing: f64,
    /// PCB hole diameter, mm.
    pub pcb_hole: f64,
    /// Leg corner clearance to the hole wall, mm.
    pub hole_clearance: f64,
    /// Leg width along y at the catch, mm.
    pub leg_width: f64,
    /// Leg thickness along x, the bending direction, mm.
    pub leg_thickness: f64,
    /// Barb overhang beyond the leg face, mm.
    pub hook_overhang: f64,
    /// Insertion ramp angle from the insertion axis, degrees.
    pub lead_in: f64,
    /// Retaining angle, degrees; 90 is permanent, 30..45 allows removal.
    pub catch_angle: f64,
    /// Catch face below the PCB bottom, mm, for thickness tolerance.
    pub retention_gap: f64,
    /// Straight band at the barb tip, mm.
    pub hook_land: f64,
    /// Split pin slot width, mm.
    pub slot_width: f64,
    /// Split pin slot length from the leg tip upward, mm.
    pub slot_length: f64,
    /// Bridge material beside the leg or shut-off window, mm.
    pub bridge_rail: f64,

    pub material: MaterialId,
    /// Polish on the clear, unfrosted faces.
    pub finish: FinishId,
    /// Stored [`Parting`] index.
    pub parting: i32,
    /// Outside draft per side on the tip, shoulder, bridge and legs, degrees.
    pub draft: f64,
    /// Pocket and slot core draft per side, degrees.
    pub inner_draft: f64,
    /// Draft on faces the pass-through cores slide along, degrees.
    pub shutoff_draft: f64,
    /// Minimum wall thickness, mm.
    pub min_wall: f64,

    /// Display colour in sRGB, including translucency.
    pub colour: Colour,
    /// Points around a circular section.
    pub segments: i32,
}

impl Default for PipeParams {
    fn default() -> Self {
        Self {
            name: "pipe".to_owned(),
            pcb_thickness: 1.6,
            led_type: LedKind::Rgb2020.index(),
            led_w: 2.2,
            led_d: 2.0,
            led_h: 0.84,
            led_to_panel: 10.0,
            panel_thickness: 2.0,
            hole_diameter: 3.0,
            tip_clearance: 0.1,
            tip_protrusion: 0.5,
            tip_top: TipTop::Lens.index(),
            tip_dome: 0.35,
            tip_chamfer: 0.15,
            frost_mode: FrostMode::Top.index(),
            frost_finish: FinishId::lookup("vdi_24").expect("VDI 24 is in the finish table"),
            frost_depth: 0.0,
            shoulder_diameter: 6.4,
            shoulder_height: 1.2,
            shoulder_gap: 0.2,
            shoulder_chamfer: 0.4,
            fit_to_led: true,
            body_shape: BodyShape::Round.index(),
            body_diameter: 5.4,
            body_taper: 1.0,
            body_corner: 0.5,
            foot_flange: 0.0,
            foot_height: 0.6,
            pocket_shape: PocketShape::Rect.index(),
            pocket_clearance: 0.15,
            air_gap: 0.2,
            entry_face: EntryFace::Flat.index(),
            lens_sag: 0.15,
            mount: Mount::Pcb.index(),
            panel_head: PanelHead::Flange.index(),
            head_overhang: 0.5,
            flange_thickness: 0.5,
            countersink_angle: 90.0,
            crush_ribs: 3,
            rib_interference: 0.08,
            rib_width: 0.4,
            stackup_tol: 0.2,
            peg_clearance: 0.05,
            peg_below: 0.4,
            leg_style: LegStyle::HookOut.index(),
            hole_spacing: 10.0,
            pcb_hole: 2.4,
            hole_clearance: 0.1,
            leg_width: 1.2,
            leg_thickness: 1.0,
            hook_overhang: 0.7,
            lead_in: 30.0,
            catch_angle: 90.0,
            retention_gap: 0.1,
            hook_land: 0.3,
            slot_width: 0.7,
            slot_length: 3.5,
            bridge_rail: 0.6,
            material: MaterialId::lookup("pc").expect("PC is in the material table"),
            finish: FinishId::lookup("spi_a2").expect("SPI A-2 is in the finish table"),
            parting: Parting::ShoulderTop.index(),
            draft: 1.0,
            inner_draft: 1.0,
            shutoff_draft: 3.0,
            min_wall: 0.8,
            colour: Colour::srgb([0.913, 0.941, 0.949]).with_translucency(0.85),
            segments: 192,
        }
    }
}

impl PipeParams {
    pub fn led_kind(&self) -> LedKind { LedKind::from_index(self.led_type) }
    pub fn tip_top(&self) -> TipTop { TipTop::from_index(self.tip_top) }
    pub fn frost_mode(&self) -> FrostMode { FrostMode::from_index(self.frost_mode) }
    pub fn body_shape(&self) -> BodyShape { BodyShape::from_index(self.body_shape) }
    pub fn pocket_shape(&self) -> PocketShape { PocketShape::from_index(self.pocket_shape) }
    pub fn entry_face(&self) -> EntryFace { EntryFace::from_index(self.entry_face) }
    pub fn leg_style(&self) -> LegStyle { LegStyle::from_index(self.leg_style) }
    pub fn parting(&self) -> Parting { Parting::from_index(self.parting) }
    pub fn mount(&self) -> Mount { Mount::from_index(self.mount) }
    pub fn panel_head(&self) -> PanelHead { PanelHead::from_index(self.panel_head) }
}

impl PartParams for PipeParams {
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn colour(&self) -> Colour { self.colour }
    fn set_colour(&mut self, colour: Colour) { self.colour = colour; }
}

/// An LED footprint and the height of its emitting top above the PCB top face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LedPackage {
    pub name: &'static str,
    /// Footprint along x, mm.
    pub w: f64,
    /// Footprint along y, mm.
    pub d: f64,
    /// Emitting top above the PCB top face, mm.
    pub h: f64,
    /// Sits in a PCB cut-out from below and shines upward.
    pub reverse: bool,
    pub note: &'static str,
}

/// Resolve the selected package, subtracting PCB thickness for a reverse mount.
pub fn led_package(p: &PipeParams) -> LedPackage {
    let kind = p.led_kind();
    let (w, d, h, reverse, note) = match kind {
        LedKind::Rgb1010 => (1.0, 1.0, 1.0, false, "1.0 x 1.0, max 1.0 tall"),
        LedKind::Rgb2020 => (2.2, 2.0, 0.84, false, "2.2 x 2.0 x 0.84"),
        LedKind::Rgb5050 => (5.0, 5.0, 1.6, false, "5.0 x 5.0 x 1.6"),
        LedKind::Rgb3535 => (3.5, 3.5, 1.45, false, "3.5 x 3.5 x 1.45"),
        LedKind::Rgb3228ReverseMount => (3.2, 2.8, 1.78, true, "3.2 x 2.8 x 1.78, reverse mount"),
        LedKind::Chip0603 => (1.6, 0.8, 0.8, false, "1.6 x 0.8 x 0.8"),
        LedKind::Chip0805 => (2.0, 1.25, 0.8, false, "2.0 x 1.25 x 0.8"),
        LedKind::Custom => (p.led_w, p.led_d, p.led_h, false, "custom"),
    };
    LedPackage {
        name: LedKind::NAMES[kind.index() as usize],
        w,
        d,
        h: if reverse { (h - p.pcb_thickness).max(0.0) } else { h },
        reverse,
        note,
    }
}

const FROST_ALIASES: &[(&str, &str)] = &[
    ("0", "vdi_18"), ("1", "vdi_24"), ("2", "vdi_30"), ("3", "vdi_36"),
];

fn legacy_key(p: &mut PipeParams, key: &str, value: &str) -> bool {
    if key == "roughness" {
        return true;
    }
    if key != "frost_grade" {
        return false;
    }
    const NAMES: &[&str] = &[
        "Light (VDI 18)", "Medium (VDI 24)", "Heavy (VDI 30)", "Coarse (VDI 36)",
    ];
    let id = FROST_ALIASES.iter().zip(NAMES)
        .find(|((index, _), name)| value == *index || value == **name)
        .and_then(|((_, id), _)| FinishId::lookup(id))
        .or_else(|| FinishId::lookup(value));
    if let Some(id) = id {
        p.frost_finish = id;
        true
    } else {
        false
    }
}

/// The shared inspector, serialization and sweep table, in display order.
pub fn param_table() -> &'static ParamTable<PipeParams> {
    static TABLE: OnceLock<ParamTable<PipeParams>> = OnceLock::new();
    TABLE.get_or_init(|| {
        macro_rules! row {
            ($builder:ident, $key:literal, $label:literal, $group:expr, $field:ident $(, $arg:expr)* $(,)?) => {{
                let (get, set) = field!(PipeParams, $field);
                part_params::$builder($key, $label, $group, get, set $(, $arg)*)
            }};
        }
        let mut descs = Vec::new();
        let mut g = "Stack-up";
        descs.push(row!(enumeration, "led_type", "LED", g, led_type, LedKind::NAMES)
            .tip("Package under the pipe; sets the pocket. Custom: enter the size below."));
        descs.push(row!(float, "led_w", "LED size x", g, led_w, 0.4, 8.0, "%.2f mm"));
        descs.push(row!(float, "led_d", "LED size y", g, led_d, 0.4, 8.0, "%.2f mm"));
        descs.push(row!(float, "led_h", "LED height", g, led_h, 0.1, 4.0, "%.2f mm")
            .tip("Top of the package above the PCB."));
        descs.push(row!(float, "pcb_thickness", "PCB thickness", g, pcb_thickness, 0.4, 3.2, "%.2f mm"));
        descs.push(row!(float, "led_to_panel", "PCB to panel", g, led_to_panel, 3.0, 30.0, "%.2f mm")
            .tip("PCB top face to the panel's back face."));
        descs.push(row!(float, "panel_thickness", "Panel thickness", g, panel_thickness, 0.5, 6.0, "%.2f mm"));
        descs.push(row!(float, "hole_diameter", "Panel hole", g, hole_diameter, 1.0, 10.0, "%.2f mm"));
        g = "Tip";
        descs.push(row!(float, "tip_clearance", "Clearance in hole", g, tip_clearance, 0.0, 0.5, "%.3f mm")
            .tip("Radial, at the panel front face (the tip is drafted, so it is tighter at the back face)."));
        descs.push(row!(float, "tip_protrusion", "Proud of panel", g, tip_protrusion, -1.0, 3.0, "%.2f mm")
            .tip("Apex above the panel front face. 0 = flush."));
        descs.push(row!(enumeration, "tip_top", "Tip top", g, tip_top, TipTop::NAMES));
        descs.push(row!(float, "tip_dome", "Dome / lens height", g, tip_dome, 0.05, 3.0, "%.2f mm"));
        descs.push(row!(float, "tip_chamfer", "Edge chamfer", g, tip_chamfer, 0.0, 0.6, "%.2f mm"));
        g = "Frosting";
        descs.push(row!(enumeration, "frost_mode", "Frosted", g, frost_mode, FrostMode::NAMES)
            .tip("A diffusing finish on the light exit (EDM / blasted texture in the mould). Surface finish only: the solid is unchanged."));
        descs.push(row!(finish, "frost_finish", "Finish", g, frost_finish,
            Some(|f| f.frosted && f.processes.contains(Processes::INJECTION)), FROST_ALIASES)
            .tip("The frosted faces' finish: VDI 3400 EDM texture, SPI B-3..D blast or a grain. Sets their render roughness, texture and the draft textured sides need."));
        descs.push(row!(float, "frost_depth", "Texture depth", g, frost_depth, 0.0, 100.0, "%.0f um")
            .tip("Optional, for the drawing note (0 = per grade)."));
        g = "Shoulder";
        descs.push(row!(float, "shoulder_diameter", "Diameter", g, shoulder_diameter, 1.5, 16.0, "%.2f mm")
            .tip("At the parting line."));
        descs.push(row!(float, "shoulder_height", "Height", g, shoulder_height, 0.4, 5.0, "%.2f mm")
            .tip("Also the bridge to the legs."));
        descs.push(row!(float, "shoulder_gap", "Gap to panel", g, shoulder_gap, 0.0, 3.0, "%.2f mm")
            .tip("Shoulder top face to the panel's back face. 0 = touching."));
        descs.push(row!(float, "shoulder_chamfer", "Chamfer", g, shoulder_chamfer, 0.0, 2.0, "%.2f mm")
            .tip("45 degree chamfer on the edge away from the parting line."));
        g = "Body";
        descs.push(row!(boolean, "fit_led", "Fit to the LED", g, fit_to_led)
            .tip("Grow the body, shoulder and hole spacing as the LED needs (the sliders are then minimums)."));
        descs.push(row!(enumeration, "body_shape", "Shape", g, body_shape, BodyShape::NAMES));
        descs.push(row!(float, "body_diameter", "Diameter", g, body_diameter, 1.0, 14.0, "%.2f mm")
            .tip("At the shoulder; across the flats when square."));
        descs.push(row!(float, "body_taper", "Taper", g, body_taper, 0.0, 6.0, "%.2f deg")
            .tip("Per side, narrower toward the LED. This is the body's draft."));
        descs.push(row!(float, "body_corner", "Corner radius", g, body_corner, 0.05, 3.0, "%.2f mm"));
        descs.push(row!(float, "foot_flange", "Foot flange", g, foot_flange, 0.0, 3.0, "%.2f mm")
            .tip("Radial ring on the PCB. Its top face is an undercut for a straight-pull mould."));
        descs.push(row!(float, "foot_height", "Flange height", g, foot_height, 0.2, 3.0, "%.2f mm"));
        g = "LED pocket";
        descs.push(row!(enumeration, "pocket_shape", "Pocket", g, pocket_shape, PocketShape::NAMES));
        descs.push(row!(float, "pocket_clearance", "Clearance", g, pocket_clearance, 0.03, 1.0, "%.2f mm")
            .tip("Around the LED (placement tolerance ~0.1 mm), at the pocket ceiling."));
        descs.push(row!(float, "air_gap", "Air gap", g, air_gap, 0.02, 2.0, "%.2f mm")
            .tip("LED top to the light-entry face. Keep it under ~1.3 mm (0.05\") to limit light bleed."));
        descs.push(row!(enumeration, "entry_face", "Entry face", g, entry_face, EntryFace::NAMES));
        descs.push(row!(float, "lens_sag", "Lens sag", g, lens_sag, 0.02, 1.0, "%.2f mm"));
        g = "Mount";
        descs.push(row!(enumeration, "mount", "Held by", g, mount, Mount::NAMES)
            .tip("PCB: legs or pegs through PCB holes. Panel: pressed into the panel hole on crush ribs, from behind (the shoulder stops on the back face) or from the front (a head on the front face); the foot then stops short of the LED."));
        descs.push(row!(enumeration, "leg_style", "PCB fixing", g, leg_style, LegStyle::NAMES)
            .tip("Hooks snap under the board (a pass-through core from the upper half forms the catch: no side action). Press-fit pegs hold on crush ribs, heat-stake pins are melted into a head under the board, locating pegs only locate (glue, or the panel holds the pipe). Hook + peg: one hook, one peg, so it only fits one way round."));
        descs.push(row!(enumeration, "panel_head", "Head", g, panel_head, PanelHead::NAMES)
            .tip("From the front: a flange on the panel's front face, or a countersunk head flush with it."));
        descs.push(row!(float, "head_overhang", "Head overhang", g, head_overhang, 0.1, 3.0, "%.2f mm")
            .tip("Radial, beyond the panel hole: the flange, or the countersink's top edge."));
        descs.push(row!(float, "flange_thickness", "Flange thickness", g, flange_thickness, 0.2, 3.0, "%.2f mm")
            .tip("On the panel's front face."));
        descs.push(row!(float, "countersink_angle", "Countersink", g, countersink_angle, 60.0, 120.0, "%.0f deg")
            .tip("Included angle of the panel hole's countersink (90 is the common tool)."));
        descs.push(row!(int, "crush_ribs", "Crush ribs", g, crush_ribs, 0, 6)
            .tip("Ribs that crush against the hole wall and hold the part (panel hole, or the PCB hole for press-fit pegs)."));
        descs.push(row!(float, "rib_interference", "Rib interference", g, rib_interference, 0.0, 0.3, "%.3f mm")
            .tip("Radial, rib crest past the hole wall at mid-hole. ~0.05-0.1 mm in PC; less in PMMA (brittle)."));
        descs.push(row!(float, "rib_width", "Rib width", g, rib_width, 0.15, 1.5, "%.2f mm")
            .tip("At the crest (wider toward its root: draft)."));
        descs.push(row!(float, "stackup_tol", "Stack-up tolerance", g, stackup_tol, 0.0, 1.0, "%.2f mm")
            .tip("+/- on PCB to panel. A panel-held pipe's foot must clear the LED at the minimum."));
        descs.push(row!(float, "peg_clearance", "Peg clearance", g, peg_clearance, 0.0, 0.4, "%.3f mm")
            .tip("Radial, peg to the PCB hole at the PCB top."));
        descs.push(row!(float, "peg_below", "Peg below PCB", g, peg_below, -2.0, 6.0, "%.2f mm")
            .tip("Peg tip below the PCB's bottom face. Heat stake: the stake that melts into the head (~1.5x the peg diameter). Negative: a blind peg that stops inside the hole."));
        g = "Snap legs";
        descs.push(row!(float, "hole_spacing", "Hole spacing", g, hole_spacing, 3.0, 40.0, "%.2f mm")
            .tip("Centre to centre, along x."));
        descs.push(row!(float, "pcb_hole", "PCB hole", g, pcb_hole, 0.8, 5.0, "%.2f mm"));
        descs.push(row!(float, "hole_clearance", "Hole clearance", g, hole_clearance, 0.0, 0.4, "%.2f mm")
            .tip("Leg corners to the hole wall."));
        descs.push(row!(float, "leg_width", "Leg width", g, leg_width, 0.4, 4.0, "%.2f mm")
            .tip("Along y, at the catch (wider toward the root: draft)."));
        descs.push(row!(float, "leg_thickness", "Leg thickness", g, leg_thickness, 0.3, 4.0, "%.2f mm")
            .tip("In the bending direction (x)."));
        descs.push(row!(float, "hook_overhang", "Hook overhang", g, hook_overhang, 0.1, 2.0, "%.2f mm")
            .tip("Barb beyond the leg face."));
        descs.push(row!(float, "lead_in", "Lead-in angle", g, lead_in, 10.0, 60.0, "%.0f deg")
            .tip("Insertion ramp, from the insertion axis."));
        descs.push(row!(float, "catch_angle", "Retaining angle", g, catch_angle, 20.0, 90.0, "%.0f deg")
            .tip("90 = permanent; 30-45 lets the pipe be pulled out again."));
        descs.push(row!(float, "retention_gap", "Retention gap", g, retention_gap, 0.0, 0.5, "%.2f mm")
            .tip("Catch face below the PCB's bottom face (thickness tolerance)."));
        descs.push(row!(float, "hook_land", "Hook land", g, hook_land, 0.0, 1.0, "%.2f mm"));
        descs.push(row!(float, "slot_width", "Slot width", g, slot_width, 0.2, 2.0, "%.2f mm")
            .tip("Split pin: the prongs close into it."));
        descs.push(row!(float, "slot_length", "Slot length", g, slot_length, 0.5, 15.0, "%.2f mm")
            .tip("From the leg tip up: the prongs' flexing length."));
        descs.push(row!(float, "bridge_rail", "Bridge rail", g, bridge_rail, 0.3, 3.0, "%.2f mm")
            .tip("Bridge material beside the leg or the shut-off window."));
        g = "Moulding";
        descs.push(row!(material, "material", "Material", g, material,
            Some(|m| m.family.is_moulding() && m.optics.clarity == Clarity::Clear), None, &[("0", "pc"), ("1", "pmma")])
            .tip("Clear grades: the snap legs' strain limit (PC 4 %, PMMA 2 %), refractive index, density, min wall."));
        descs.push(row!(finish, "finish", "Clear finish", g, finish, Some(|f| f.clear_ok), &[])
            .tip("Polish of the clear (unfrosted) faces: SPI A-1..A-3 keep the pipe optically clear."));
        descs.push(row!(enumeration, "parting", "Parting line", g, parting, Parting::NAMES));
        descs.push(row!(float, "draft", "Draft", g, draft, 0.0, 5.0, "%.2f deg")
            .tip("Outside faces: tip, shoulder, bridge, legs. 1-3 deg for polished light guides, 2+ for textured."));
        descs.push(row!(float, "inner_draft", "Core draft", g, inner_draft, 0.0, 5.0, "%.2f deg")
            .tip("LED pocket and slot."));
        descs.push(row!(float, "shutoff_draft", "Shut-off draft", g, shutoff_draft, 0.0, 7.0, "%.2f deg")
            .tip("Faces the pass-through cores slide along (3 deg or more)."));
        descs.push(row!(float, "min_wall", "Min wall", g, min_wall, 0.3, 3.0, "%.2f mm"));
        g = "Look";
        descs.push(row!(colour, "color", "Colour", g, colour));
        descs.push(part_params::float("translucency", "Translucency", g,
            |p: &PipeParams| f64::from(p.colour.translucency), |p, value| p.colour.translucency = value as f32,
            0.0, 0.999, "%.3f")
            .tip("Clear / frosted / tinted (display only)."));
        descs.push(row!(int, "segments", "Segments", g, segments, 48, 720).tip("Points around."));
        ParamTable { descs, header: "# Light pipe", after_set: None, legacy: Some(legacy_key) }
    })
}

/// Dimensions and fit results of the built part; lengths are in mm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PipeReport {
    pub led_top: f64,
    pub entry_z: f64,
    pub shoulder_bottom: f64,
    pub shoulder_top: f64,
    pub panel_front: f64,
    pub apex: f64,
    pub parting_z: f64,
    pub catch_z: f64,
    pub leg_bottom: f64,
    pub tip_diameter: f64,
    pub tip_clearance_back: f64,
    pub pocket_wall: f64,
    /// Body diameter after growing to fit the LED.
    pub body_diameter: f64,
    pub shoulder_diameter: f64,
    pub hole_spacing: f64,
    pub legs: bool,
    /// Per cantilever: a whole leg, or one prong of a split pin.
    pub leg_length: f64,
    pub leg_thickness: f64,
    pub deflection: f64,
    pub strain: f64,
    pub strain_limit: f64,
    pub engagement: f64,
    pub allowed_deflection: f64,
    /// Half extent of the part's footprint along x.
    pub extent_x: f64,
    /// Half extent of the part's footprint along y.
    pub extent_y: f64,
    /// Leg width after fitting the PCB hole.
    pub leg_width_built: f64,
    pub leg_thickness_built: f64,
    pub hook_overhang_built: f64,
    pub slot_width_built: f64,
    /// Free space beside the deflected leg and barb in the hole, at least zero.
    pub hole_margin: f64,
    pub panel_held: bool,
    pub pcb_holes: bool,
    pub pegs: bool,
    /// Minimum foot-to-LED gap across the panel mount's stack-up tolerance.
    pub foot_gap_min: f64,
    /// Maximum foot-to-LED gap across the panel mount's stack-up tolerance.
    pub foot_gap_max: f64,
    pub ribs: i32,
    /// Radial interference at mid-hole.
    pub rib_interference: f64,
    /// Rib length bearing in the hole.
    pub rib_engaged: f64,
    /// Peg diameter at the PCB top.
    pub peg_diameter: f64,
    /// Diameter of the dome formed by melting the stake's protrusion.
    pub stake_head_diameter: f64,
    /// Changes made to fit the holes.
    pub fit_notes: Vec<String>,
}

/// Build measurements and warnings. The solids are added with the generator.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PipeBuild {
    /// the solid as a CSG program; slot 0 the clear faces, slot 1 the frosted faces
    pub csg: Csg,
    pub report: PipeReport,
    pub zones: Vec<DraftZone>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Kind;
    use std::collections::HashSet;

    #[test]
    fn pipe_parameters() {
        let table = param_table();
        let defaults = PipeParams::default();
        let mut keys = HashSet::new();
        for d in &table.descs {
            assert!(keys.insert(d.key), "parameter key is unique: {}", d.key);
            if let Kind::Float { get, hard_lo, hard_hi, .. } = &d.kind {
                assert!((*hard_lo..=*hard_hi).contains(&get(&defaults)),
                    "default is in the hard range: {}", d.key);
            }
        }
        let text = table.to_text(&defaults, None);
        let mut p = PipeParams::default();
        table.from_text(&text, &mut p, None).expect("default parameters parse");
        assert_eq!(table.to_text(&p, None), text, "parameter text round trips");

        p.led_type = LedKind::Rgb3228ReverseMount.index();
        p.pcb_thickness = 1.6;
        let led = led_package(&p);
        assert!(led.reverse && (led.h - 0.18).abs() < 1e-6,
            "reverse mount height is measured above the PCB top");
        p.pcb_thickness = 2.0;
        assert_eq!(led_package(&p).h, 0.0, "reverse mount height cannot be negative");
        p.led_type = LedKind::Custom.index();
        (p.led_w, p.led_d, p.led_h) = (3.1, 1.7, 1.2);
        let led = led_package(&p);
        assert_eq!((led.w, led.d, led.h, led.reverse), (3.1, 1.7, 1.2, false),
            "custom LED uses all three dimensions above the PCB");

        for value in ["2", "Heavy (VDI 30)", "vdi_30"] {
            p.frost_finish = defaults.frost_finish;
            table.from_text(&format!("frost_grade = {value}"), &mut p, None)
                .expect("legacy frost grade parses by index, name or finish id");
            assert_eq!(p.frost_finish.name(), "vdi_30", "legacy frost grade selects VDI 30");
        }
        let before = p.clone();
        table.from_text("roughness = 0.3", &mut p, None).expect("legacy roughness is accepted");
        assert_eq!(p, before, "legacy roughness changes nothing");

        let material = table.material_desc().expect("material picker exists");
        let options = material.options();
        assert_eq!(options.first(), Some(&"pc"), "PC is the first clear material in table order");
        assert!(options.iter().all(|id| { let m = MaterialId::lookup(id).unwrap().get(); m.family.is_moulding() && m.optics.clarity == Clarity::Clear }),
            "material picker contains only clear materials");
        table.from_text("material = pmma", &mut p, None).expect("PMMA loads by text");
        assert_eq!(p.material.name(), "pmma", "material text selects PMMA");
        assert!(p.finish.get().clear_ok, "changing material keeps a clear finish");
        assert_eq!(p.finish, defaults.finish, "compatible clear finish is retained");
        for id in table.find("finish").unwrap().options() {
            assert!(FinishId::lookup(id).unwrap().get().clear_ok, "clear finish picker: {id}");
        }
        for id in table.find("frost_finish").unwrap().options() {
            let finish = FinishId::lookup(id).unwrap().get();
            assert!(finish.frosted && finish.processes.contains(Processes::INJECTION),
                "frost finish picker offers injection textures: {id}");
        }
    }
}
