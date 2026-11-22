//! Display window dimensions, picker choices, file parameters and build measurements.

use crate::csg::{Csg, V2};
use crate::params::choices;
use crate::params::{self as part_params, field, ParamDesc, ParamTable, PartParams};
use fab_materials::{Clarity, Colour, FinishId, MaterialId};
use std::sync::OnceLock;

choices! {
    /// A generic display size or LED ring under the window.
    Screen {
        Custom => "Custom",
        Round128 => "1.28\" round 240x240",
        Round071 => "0.71\" round 160x160",
        Round210 => "2.1\" round 480x480",
        Square154 => "1.54\" square 240x240",
        Oled096 => "0.96\" OLED 128x64",
        Oled130 => "1.3\" OLED 128x64",
        Oled242 => "2.42\" OLED 128x64",
        LedRing25 => "LED ring 25 mm, encoder",
    }
}
choices! { Outline { Round => "Round", Rect => "Rectangle" } }
choices! { EdgeStyle { None => "Square", Chamfer => "Chamfer", Round => "Rounded" } }
choices! { Viewport { None => "None (all printed)", Round => "Round", Rect => "Rectangle" } }
choices! { HoleType {
    None => "None", Round => "Round", Notched2 => "Two notches", Notched1 => "One notch", DFlat => "D flat",
} }
choices! { NotchEnd { Square => "Square", Round => "Round" } }
choices! { SegShape { Slot => "Slot", RoundedRect => "Rounded rect", Wedge => "Wedge", Dot => "Dot" } }
choices! { SegRound { None => "None (sharp)", Inner => "Inner end", Outer => "Outer end", Both => "Both ends" } }
choices! { SegInk { Clear => "Clear (tinted sheet)", Diffuser => "Diffuser ink" } }
choices! { MountHoles { None => "None", Two => "Two", Four => "Four" } }

/// Dimensions and manufacturing choices for a display window.
/// The back face is z = 0, +z faces the viewer and +y points up the display.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowParams {
    pub name: String,

    /// Stored [`Screen`] index.
    pub screen: i32,
    /// Outline beyond the display glass per side, mm; applied when a screen is picked.
    pub border: f64,
    /// Air below the window stack and above the display or LED board, mm; view only.
    pub screen_gap: f64,

    /// Stored [`Outline`] index.
    pub outline: i32,
    /// Round outline diameter, mm.
    pub diameter: f64,
    /// Rectangular outline width, mm.
    pub width: f64,
    /// Rectangular outline height, mm.
    pub height: f64,
    /// Rectangular outline corner radius, mm.
    pub corner: f64,
    /// Sheet thickness, mm.
    pub thickness: f64,
    /// Stored [`EdgeStyle`] index.
    pub edge: i32,
    /// Front edge chamfer or radius, mm.
    pub edge_size: f64,

    /// Stored [`Viewport`] index.
    pub viewport: i32,
    /// Round viewport diameter, mm.
    pub view_d: f64,
    /// Rectangular viewport width, mm.
    pub view_w: f64,
    /// Rectangular viewport height, mm.
    pub view_h: f64,
    /// Rectangular viewport corner radius, mm.
    pub view_corner: f64,
    /// Viewport centre offset along x, mm.
    pub view_x: f64,
    /// Viewport centre offset along y, mm.
    pub view_y: f64,
    /// Fade the print into the viewport with dots that shrink inward.
    pub dither: bool,
    /// Fade band inward from the viewport edge, mm.
    pub dither_width: f64,
    /// Dot spacing, mm.
    pub dither_pitch: f64,
    /// Smallest printed dot and narrowest clear gap between dots, mm.
    pub dither_min: f64,
    /// Edge dot diameter beyond the pitch, percent, to merge into the print.
    pub dither_overlap: f64,
    /// Unitless falloff: coverage is (1 - t)^(2^curve); zero is linear.
    pub dither_curve: f64,

    /// Stored [`HoleType`] index.
    pub hole: i32,
    /// Centre hole diameter, mm.
    pub hole_d: f64,
    /// Width across a notch, mm.
    pub notch_width: f64,
    /// Notch tip-to-tip span, mm; one notch uses twice the centre-to-tip distance.
    pub notch_span: f64,
    /// Stored [`NotchEnd`] index.
    pub notch_end: i32,
    /// First notch angle clockwise from +y, degrees.
    pub notch_angle: f64,
    /// D-flat distance from the centre, mm.
    pub flat_dist: f64,
    /// Centre hole offset along x, mm.
    pub hole_x: f64,
    /// Centre hole offset along y, mm.
    pub hole_y: f64,

    /// Number of light segments; zero disables them.
    pub seg_count: i32,
    /// Diameter through the segment centres, mm.
    pub seg_ring_d: f64,
    /// Radial segment length, mm.
    pub seg_length: f64,
    /// Tangential segment width, mm; a wedge uses its width at the ring diameter.
    pub seg_width: f64,
    /// Stored [`SegShape`] index.
    pub seg_shape: i32,
    /// Segment corner radius, mm.
    pub seg_corner: f64,
    /// Stored [`SegRound`] index; chooses which wedge ends are rounded.
    pub seg_round: i32,
    /// First segment angle clockwise from +y, degrees.
    pub seg_start: f64,
    /// Segment arc, degrees; below 360 puts the first and last segments at its ends.
    pub seg_span: f64,
    /// Stored [`SegInk`] index.
    pub seg_ink: i32,

    /// Print an opaque mask on the back face.
    pub print: bool,
    /// Print colour in sRGB, including translucency.
    pub print_colour: Colour,
    /// Print inset from the outline, mm.
    pub print_inset: f64,
    /// Modelled ink thickness, mm; thinner ink breaks STEP booleans.
    pub print_thickness: f64,

    /// Add an adhesive ring along the back outline.
    pub adhesive: bool,
    /// Adhesive ring width, mm.
    pub adhesive_width: f64,
    /// Adhesive inset from the outline, mm.
    pub adhesive_inset: f64,
    /// Adhesive thickness, mm.
    pub adhesive_thickness: f64,

    /// Stored [`MountHoles`] index.
    pub mount_holes: i32,
    /// Mounting hole diameter, mm.
    pub mount_d: f64,
    /// Mounting hole centre inset from the edge, mm.
    pub mount_inset: f64,

    pub material: MaterialId,
    /// Front face finish: gloss or a matte grade for anti-glare.
    pub finish: FinishId,
    /// Display tint in sRGB, including translucency.
    pub colour: Colour,
    /// Points around a circle.
    pub segments: i32,
}

impl Default for WindowParams {
    fn default() -> Self {
        Self {
            name: "window".to_owned(),
            screen: Screen::Round128.index(),
            border: 2.0,
            screen_gap: 0.0,
            outline: Outline::Round.index(),
            diameter: 40.0,
            width: 40.0,
            height: 30.0,
            corner: 2.0,
            thickness: 1.5,
            edge: EdgeStyle::Chamfer.index(),
            edge_size: 0.3,
            viewport: Viewport::Round.index(),
            view_d: 33.0,
            view_w: 24.0,
            view_h: 13.0,
            view_corner: 0.5,
            view_x: 0.0,
            view_y: 0.0,
            dither: false,
            dither_width: 2.0,
            dither_pitch: 0.35,
            dither_min: 0.08,
            dither_overlap: 10.0,
            dither_curve: 0.0,
            hole: HoleType::None.index(),
            hole_d: 7.0,
            notch_width: 2.0,
            notch_span: 9.0,
            notch_end: NotchEnd::Square.index(),
            notch_angle: 0.0,
            flat_dist: 2.75,
            hole_x: 0.0,
            hole_y: 0.0,
            seg_count: 0,
            seg_ring_d: 20.0,
            seg_length: 2.4,
            seg_width: 1.4,
            seg_shape: SegShape::RoundedRect.index(),
            seg_corner: 0.5,
            seg_round: SegRound::None.index(),
            seg_start: 0.0,
            seg_span: 360.0,
            seg_ink: SegInk::Clear.index(),
            print: true,
            print_colour: Colour::srgb([0.05, 0.05, 0.06]),
            print_inset: 0.0,
            print_thickness: 0.05,
            adhesive: true,
            adhesive_width: 1.3,
            adhesive_inset: 0.0,
            adhesive_thickness: 0.1,
            mount_holes: MountHoles::None.index(),
            mount_d: 2.2,
            mount_inset: 3.0,
            material: MaterialId::lookup("pmma").expect("PMMA is in the material table"),
            finish: FinishId::lookup("spi_a2").expect("SPI A-2 is in the finish table"),
            colour: Colour::srgb([0.227, 0.239, 0.267]).with_translucency(0.6),
            segments: 192,
        }
    }
}

impl WindowParams {
    pub fn screen(&self) -> Screen { Screen::from_index(self.screen) }
    pub fn outline(&self) -> Outline { Outline::from_index(self.outline) }
    pub fn edge(&self) -> EdgeStyle { EdgeStyle::from_index(self.edge) }
    pub fn viewport(&self) -> Viewport { Viewport::from_index(self.viewport) }
    pub fn hole(&self) -> HoleType { HoleType::from_index(self.hole) }
    pub fn notch_end(&self) -> NotchEnd { NotchEnd::from_index(self.notch_end) }
    pub fn seg_shape(&self) -> SegShape { SegShape::from_index(self.seg_shape) }
    pub fn seg_round(&self) -> SegRound { SegRound::from_index(self.seg_round) }
    pub fn seg_ink(&self) -> SegInk { SegInk::from_index(self.seg_ink) }
    pub fn mount_holes(&self) -> MountHoles { MountHoles::from_index(self.mount_holes) }
}

impl PartParams for WindowParams {
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn colour(&self) -> Colour { self.colour }
    fn set_colour(&mut self, colour: Colour) { self.colour = colour; }
}

/// Display glass, active area and module board dimensions.
/// When the window is bound to a display part, the part's mech data replaces this
/// table; the binding comes later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenData {
    pub name: &'static str,
    /// Resolution and dimensional notes; estimates are marked est.
    pub note: &'static str,
    pub round: bool,
    /// Glass width or round diameter, mm.
    pub glass_w: f64,
    /// Glass height, mm; zero for a round panel.
    pub glass_h: f64,
    /// Active width or round diameter, mm.
    pub active_w: f64,
    /// Active height, mm; zero for a round panel.
    pub active_h: f64,
    /// Active centre relative to the glass centre along x, mm.
    pub active_x: f64,
    /// Active centre relative to the glass centre along y, mm.
    pub active_y: f64,
    /// Panel thickness, mm.
    pub glass_t: f64,
    /// Board width or round diameter, mm; zero for a bare panel.
    pub pcb_w: f64,
    /// Board height, mm; zero for a round board or bare panel.
    pub pcb_h: f64,
    /// Board thickness, mm; zero for a bare panel.
    pub pcb_t: f64,
    /// Board centre relative to the glass centre along y, mm.
    pub pcb_y: f64,
}

/// Dimensions for the selected generic display or LED ring.
pub fn screen_data(screen: Screen) -> &'static ScreenData {
    static TABLE: [ScreenData; 9] = [
        ScreenData {
            name: Screen::NAMES[0],
            note: "",
            round: true,
            glass_w: 0.0, glass_h: 0.0, active_w: 0.0, active_h: 0.0,
            active_x: 0.0, active_y: 0.0, glass_t: 0.0,
            pcb_w: 0.0, pcb_h: 0.0, pcb_t: 0.0, pcb_y: 0.0,
        },
        ScreenData {
            name: Screen::NAMES[1],
            note: "240x240 IPS; active 32.4 mm (glass and board est.)",
            round: true,
            glass_w: 35.6, glass_h: 0.0, active_w: 32.4, active_h: 0.0,
            active_x: 0.0, active_y: 0.0, glass_t: 1.6,
            pcb_w: 39.0, pcb_h: 0.0, pcb_t: 1.6, pcb_y: 0.0,
        },
        ScreenData {
            name: Screen::NAMES[2],
            note: "160x160; active ~18.0 mm (est.)",
            round: true,
            glass_w: 20.9, glass_h: 0.0, active_w: 18.0, active_h: 0.0,
            active_x: 0.0, active_y: 0.0, glass_t: 1.5,
            pcb_w: 0.0, pcb_h: 0.0, pcb_t: 0.0, pcb_y: 0.0,
        },
        ScreenData {
            name: Screen::NAMES[3],
            note: "480x480 IPS, RGB; active 53.28 mm (glass est.)",
            round: true,
            glass_w: 56.6, glass_h: 0.0, active_w: 53.28, active_h: 0.0,
            active_x: 0.0, active_y: 0.0, glass_t: 2.3,
            pcb_w: 0.0, pcb_h: 0.0, pcb_t: 0.0, pcb_y: 0.0,
        },
        ScreenData {
            name: Screen::NAMES[4],
            note: "240x240 IPS; active 27.72 mm square (offset est.)",
            round: false,
            glass_w: 31.52, glass_h: 33.72, active_w: 27.72, active_h: 27.72,
            active_x: 0.0, active_y: 1.0, glass_t: 1.6,
            pcb_w: 0.0, pcb_h: 0.0, pcb_t: 0.0, pcb_y: 0.0,
        },
        ScreenData {
            name: Screen::NAMES[5],
            note: "128x64; active 21.74 x 10.86 (board est.)",
            round: false,
            glass_w: 26.7, glass_h: 19.26, active_w: 21.744, active_h: 10.864,
            active_x: 0.0, active_y: 1.55, glass_t: 1.26,
            pcb_w: 27.3, pcb_h: 27.8, pcb_t: 1.2, pcb_y: -2.8,
        },
        ScreenData {
            name: Screen::NAMES[6],
            note: "128x64; active 29.42 x 14.70 (offset, board est.)",
            round: false,
            glass_w: 34.5, glass_h: 23.0, active_w: 29.42, active_h: 14.7,
            active_x: 0.0, active_y: 1.6, glass_t: 1.45,
            pcb_w: 35.4, pcb_h: 33.5, pcb_t: 1.2, pcb_y: -3.3,
        },
        ScreenData {
            name: Screen::NAMES[7],
            note: "128x64; active 55.01 x 27.49 (offset est.)",
            round: false,
            glass_w: 60.5, glass_h: 37.0, active_w: 55.01, active_h: 27.49,
            active_x: 0.0, active_y: 2.0, glass_t: 2.0,
            pcb_w: 0.0, pcb_h: 0.0, pcb_t: 0.0, pcb_y: 0.0,
        },
        ScreenData {
            name: Screen::NAMES[8],
            note: "LED ring board around an M7 encoder bushing",
            round: true,
            glass_w: 0.0, glass_h: 0.0, active_w: 0.0, active_h: 0.0,
            active_x: 0.0, active_y: 0.0, glass_t: 0.0,
            pcb_w: 25.0, pcb_h: 0.0, pcb_t: 1.6, pcb_y: 0.0,
        },
    ];
    &TABLE[screen.index() as usize]
}

/// Fit the viewport to the active area with 0.3 mm bleed per side and the outline
/// to the glass plus the border, or apply the LED ring preset. Custom changes nothing.
pub fn apply_screen(p: &mut WindowParams) {
    let screen = p.screen();
    if screen == Screen::Custom {
        return;
    }
    let d = screen_data(screen);
    if screen == Screen::LedRing25 {
        p.outline = Outline::Round.index();
        p.diameter = 25.0;
        p.thickness = 0.5;
        p.edge = EdgeStyle::None.index();
        p.viewport = Viewport::None.index();
        p.hole = HoleType::Notched2.index();
        p.hole_d = 7.0;
        p.notch_width = 2.0;
        p.notch_span = 9.0;
        p.notch_angle = 0.0;
        p.hole_x = 0.0;
        p.hole_y = 0.0;
        p.seg_count = 24;
        p.seg_ring_d = 18.4;
        p.seg_length = 2.8;
        p.seg_width = 1.2;
        p.seg_shape = SegShape::Wedge.index();
        p.seg_corner = 0.45;
        p.seg_start = 0.0;
        p.seg_span = 360.0;
        p.print = true;
        p.adhesive = true;
        p.adhesive_width = 1.3;
        p.adhesive_inset = 0.0;
        p.material = MaterialId::lookup("pc").expect("PC is in the material table");
        p.finish = FinishId::lookup("spi_b1").expect("SPI B-1 is in the finish table");
        p.mount_holes = MountHoles::None.index();
        return;
    }
    let bleed = 0.3;
    p.seg_count = 0;
    p.hole = HoleType::None.index();
    p.view_x = d.active_x;
    p.view_y = d.active_y;
    if d.round {
        p.outline = Outline::Round.index();
        p.diameter = d.glass_w + 2.0 * p.border;
        p.viewport = Viewport::Round.index();
        p.view_d = d.active_w + 2.0 * bleed;
    } else {
        p.outline = Outline::Rect.index();
        p.width = d.glass_w + 2.0 * p.border;
        p.height = d.glass_h + 2.0 * p.border;
        p.viewport = Viewport::Rect.index();
        p.view_w = d.active_w + 2.0 * bleed;
        p.view_h = d.active_h + 2.0 * bleed;
        p.view_corner = 0.3;
    }
}

fn after_set(p: &mut WindowParams, d: &ParamDesc<WindowParams>) {
    if d.key == "screen"
        || (d.key == "border" && !matches!(p.screen(), Screen::Custom | Screen::LedRing25))
    {
        apply_screen(p);
    }
}

/// The shared inspector, serialization and sweep table, in display order.
pub fn param_table() -> &'static ParamTable<WindowParams> {
    static TABLE: OnceLock<ParamTable<WindowParams>> = OnceLock::new();
    TABLE.get_or_init(|| {
        macro_rules! row {
            ($builder:ident, $key:literal, $label:literal, $group:expr, $field:ident $(, $arg:expr)* $(,)?) => {{
                let (get, set) = field!(WindowParams, $field);
                part_params::$builder($key, $label, $group, get, set $(, $arg)*)
            }};
        }
        let mut descs = Vec::new();
        let mut g = "Display";
        descs.push(row!(enumeration, "screen", "Display", g, screen, Screen::NAMES)
            .tip("The display under the window. Choosing one sets the viewport over its active area and the outline around its glass (sizes marked est. are typical: measure yours). Custom leaves everything as is."));
        descs.push(row!(float, "border", "Border", g, border, 0.0, 10.0, "%.2f mm")
            .tip("Window beyond the display glass, per side (applied with the display)."));
        descs.push(row!(float, "screen_gap", "Display gap", g, screen_gap, 0.0, 10.0, "%.2f mm")
            .tip("Air between the bottom of the window (adhesive) and the display glass or LED board in the 3D view and the raytrace: a bezel or spacer that holds the window above the screen. Display only, never exported."));
        g = "Outline";
        descs.push(row!(enumeration, "outline", "Shape", g, outline, Outline::NAMES));
        descs.push(row!(float, "diameter", "Diameter", g, diameter, 5.0, 120.0, "%.2f mm"));
        descs.push(row!(float, "width", "Width", g, width, 5.0, 150.0, "%.2f mm"));
        descs.push(row!(float, "height", "Height", g, height, 5.0, 150.0, "%.2f mm"));
        descs.push(row!(float, "corner", "Corner radius", g, corner, 0.0, 20.0, "%.2f mm"));
        descs.push(row!(float, "thickness", "Thickness", g, thickness, 0.2, 6.0, "%.2f mm")
            .tip("Sheet: PC 0.25-1 mm films and plates, cast PMMA 1, 1.5, 2, 3 mm ..."));
        descs.push(row!(enumeration, "edge", "Front edge", g, edge, EdgeStyle::NAMES)
            .tip("Chamfer / round the front edge (CNC or a diamond-polished edge; a laser leaves it square)."));
        descs.push(row!(float, "edge_size", "Edge size", g, edge_size, 0.05, 3.0, "%.2f mm"));
        g = "Viewport";
        descs.push(row!(enumeration, "viewport", "Viewport", g, viewport, Viewport::NAMES)
            .tip("The clear area in the print, over the display's active area."));
        descs.push(row!(float, "view_d", "Diameter", g, view_d, 2.0, 110.0, "%.2f mm"));
        descs.push(row!(float, "view_w", "Width", g, view_w, 2.0, 140.0, "%.2f mm"));
        descs.push(row!(float, "view_h", "Height", g, view_h, 2.0, 140.0, "%.2f mm"));
        descs.push(row!(float, "view_corner", "Corner radius", g, view_corner, 0.0, 20.0, "%.2f mm"));
        descs.push(row!(float, "view_x", "Offset x", g, view_x, -30.0, 30.0, "%.2f mm"));
        descs.push(row!(float, "view_y", "Offset y", g, view_y, -30.0, 30.0, "%.2f mm")
            .tip("Active areas usually sit away from the FPC side (+y)."));
        descs.push(row!(boolean, "dither", "Dithered edge", g, dither)
            .tip("Fade the print into the viewport with a halftone: rows of dots following its outline, the first row centred on the edge, shrinking inward."));
        descs.push(row!(float, "dither_width", "Fade width", g, dither_width, 0.2, 10.0, "%.2f mm")
            .tip("From the viewport's edge to where the last dots vanish."));
        descs.push(row!(float, "dither_pitch", "Dot pitch", g, dither_pitch, 0.1, 2.0, "%.2f mm")
            .tip("Dot spacing (screen print: 0.3 mm and up; UV inkjet finer)."));
        descs.push(row!(float, "dither_overlap", "Edge overlap", g, dither_overlap, 0.0, 100.0, "%.0f %%")
            .tip("The first dots are the pitch plus this much across: they overlap each other and the print, so the mask breaks up into dots instead of stopping at a line."));
        descs.push(row!(float, "dither_curve", "Falloff curve", g, dither_curve, -3.0, 3.0, "%.2f")
            .tip("How the coverage falls across the fade: 0 linear; positive opens up quickly near the edge, negative stays dense longer and drops off late."));
        descs.push(row!(float, "dither_min", "Smallest dot", g, dither_min, 0.02, 0.5, "%.2f mm")
            .tip("Dots below this are dropped, and clear gaps between dots narrower than this close (screen print holds ~0.1 mm)."));
        g = "Centre hole";
        descs.push(row!(enumeration, "hole", "Hole", g, hole, HoleType::NAMES)
            .tip("For an encoder / pot bushing through a ring display. Notches key the window on the bushing's flats or tabs."));
        descs.push(row!(float, "hole_d", "Diameter", g, hole_d, 1.0, 30.0, "%.2f mm")
            .tip("M7 encoder bushing: 7 mm; M9 pot: 9 mm."));
        descs.push(row!(float, "notch_width", "Notch width", g, notch_width, 0.3, 6.0, "%.2f mm"));
        descs.push(row!(float, "notch_span", "Notch span", g, notch_span, 1.0, 36.0, "%.2f mm")
            .tip("Tip to tip (one notch: twice centre to tip)."));
        descs.push(row!(enumeration, "notch_end", "Notch ends", g, notch_end, NotchEnd::NAMES)
            .tip("Round: semicircular tips (the span still runs to the tips)."));
        descs.push(row!(float, "notch_angle", "Notch angle", g, notch_angle, -180.0, 180.0, "%.0f deg")
            .tip("First notch / the flat, clockwise from +y."));
        descs.push(row!(float, "flat_dist", "Flat distance", g, flat_dist, -10.0, 15.0, "%.2f mm")
            .tip("D flat: centre to the flat."));
        descs.push(row!(float, "hole_x", "Offset x", g, hole_x, -40.0, 40.0, "%.2f mm"));
        descs.push(row!(float, "hole_y", "Offset y", g, hole_y, -40.0, 40.0, "%.2f mm"));
        g = "Light segments";
        descs.push(row!(int, "seg_count", "Segments", g, seg_count, 0, 96)
            .tip("Clear windows in the print around a ring (LED ring, level meter). 0 = none."));
        descs.push(row!(float, "seg_ring_d", "Ring diameter", g, seg_ring_d, 2.0, 110.0, "%.2f mm")
            .tip("Through the segment centres: over the LEDs."));
        descs.push(row!(float, "seg_length", "Length", g, seg_length, 0.2, 20.0, "%.2f mm")
            .tip("Radial."));
        descs.push(row!(float, "seg_width", "Width", g, seg_width, 0.2, 20.0, "%.2f mm")
            .tip("Tangential (a wedge: at the ring diameter; a dot: its diameter)."));
        descs.push(row!(enumeration, "seg_shape", "Shape", g, seg_shape, SegShape::NAMES));
        descs.push(row!(float, "seg_corner", "Corner radius", g, seg_corner, 0.0, 5.0, "%.2f mm")
            .tip("A wedge: the rounded end's corners; large enough makes the end a full round."));
        descs.push(row!(enumeration, "seg_round", "Rounded end", g, seg_round, SegRound::NAMES)
            .tip("A wedge: round its inner (toward the hole) or outer end, or both."));
        descs.push(row!(float, "seg_start", "Start angle", g, seg_start, -180.0, 180.0, "%.1f deg")
            .tip("The first segment, clockwise from +y."));
        descs.push(row!(float, "seg_span", "Arc", g, seg_span, 10.0, 360.0, "%.0f deg")
            .tip("360 = all round; less puts the first and last segment at the arc's ends (a 270 deg pot scale)."));
        descs.push(row!(enumeration, "seg_ink", "Fill", g, seg_ink, SegInk::NAMES)
            .tip("Diffuser: a white translucent ink in the windows (dead-front look, hides the LEDs)."));
        g = "Print";
        descs.push(row!(boolean, "print", "Back print", g, print)
            .tip("An opaque mask printed on the back face: everything but the viewport, the segments and the holes."));
        descs.push(row!(colour, "print_color", "Colour", g, print_colour));
        descs.push(row!(float, "print_inset", "Inset from edge", g, print_inset, 0.0, 5.0, "%.2f mm")
            .tip("Leave a clear rim (0 = to the edge)."));
        descs.push(row!(float, "print_thickness", "Ink thickness", g, print_thickness, 0.05, 0.3, "%.3f mm")
            .tip("Model only: screen print is 10-20 um, modelled at 0.05 mm or more so the STEP booleans hold."));
        g = "Adhesive";
        descs.push(row!(boolean, "adhesive", "Adhesive ring", g, adhesive)
            .tip("Die-cut double-sided tape on the back, along the outline."));
        descs.push(row!(float, "adhesive_width", "Width", g, adhesive_width, 0.3, 15.0, "%.2f mm"));
        descs.push(row!(float, "adhesive_inset", "Inset", g, adhesive_inset, 0.0, 10.0, "%.2f mm")
            .tip("From the outline."));
        descs.push(row!(float, "adhesive_thickness", "Thickness", g, adhesive_thickness, 0.03, 1.0, "%.2f mm")
            .tip("Tape: 0.05 .. 0.13; foam 0.4+."));
        g = "Mounting";
        descs.push(row!(enumeration, "mount_holes", "Mounting holes", g, mount_holes, MountHoles::NAMES)
            .tip("Screw holes: round windows on the x axis (two) or diagonals (four), rectangles at the sides / corners."));
        descs.push(row!(float, "mount_d", "Diameter", g, mount_d, 0.8, 8.0, "%.2f mm")
            .tip("M2 clearance 2.2, M2.5 2.7, M3 3.2."));
        descs.push(row!(float, "mount_inset", "Inset", g, mount_inset, 0.8, 15.0, "%.2f mm")
            .tip("Edge to the hole centre."));
        g = "Material";
        descs.push(row!(material, "material", "Material", g, material,
            Some(|m| m.family.is_moulding() && m.optics.clarity == Clarity::Clear), None, &[])
            .tip("Clear sheet. Cast PMMA laser cuts with flame-polished edges; PC (tougher, for thin films and anything that gets knocked) is CNC- or die-cut: a laser chars it."));
        descs.push(row!(finish, "finish", "Front finish", g, finish, None, &[])
            .tip("Gloss (SPI A) or a fine matte for anti-glare (SPI B-1 / B-2 hard-coated sheet)."));
        descs.push(row!(colour, "color", "Tint", g, colour));
        descs.push(part_params::float("translucency", "Translucency", g,
            |p: &WindowParams| f64::from(p.colour.translucency), |p, value| p.colour.translucency = value as f32,
            0.0, 0.999, "%.3f")
            .tip("Clear / smoked / tinted (display only)."));
        descs.push(row!(int, "segments", "Segments", g, segments, 48, 720)
            .tip("Points around a circle."));
        ParamTable { descs, header: "# Display window", after_set: Some(after_set), legacy: None }
    })
}

/// A closed, counter-clockwise polygon, star-shaped about its local origin, in mm.
pub type Poly = Vec<V2>;

/// A polygon placed about a centre in window coordinates, in mm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placed {
    pub poly: Poly,
    pub cx: f64,
    pub cy: f64,
}

/// The 2D layout shared by the solids and laser export, in mm, with +y up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowLayout {
    /// The cut outline.
    pub outline: Poly,
    /// Centre hole and mounting hole cuts.
    pub holes: Vec<Placed>,
    /// Clear areas in the print: viewport and segments.
    pub openings: Vec<Placed>,
    /// Segments alone, for diffuser ink.
    pub segs: Vec<Placed>,
    /// Dithered edge dots printed inside the viewport.
    pub dots: Vec<Placed>,
    /// Print outer edge: the outline inset by the print inset.
    pub print_outer: Poly,
    pub adhesive_outer: Poly,
    pub adhesive_inner: Poly,
}

/// Dimensions and clearances of the built window; lengths and coordinates are in mm.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowReport {
    /// Outline extent along x.
    pub size_x: f64,
    /// Outline extent along y.
    pub size_y: f64,
    /// Sheet area, mm².
    pub area: f64,
    /// Built viewport width; zero if absent.
    pub view_w: f64,
    /// Built viewport height; zero if absent.
    pub view_h: f64,
    /// Centre-to-centre segment pitch along the ring.
    pub seg_pitch: f64,
    /// Clear gap between neighbouring segments.
    pub seg_gap: f64,
    /// Narrowest material between a hole and an edge or another hole.
    pub min_web: f64,
    /// Adhesive area, mm².
    pub adhesive_area: f64,
    /// Dithered edge dot count.
    pub dots: i32,
    /// Smallest dot diameter.
    pub dot_min: f64,
    /// Largest dot diameter.
    pub dot_max: f64,
    /// Print top face z coordinate (zero).
    pub print_top: f64,
    /// Adhesive bottom face z coordinate.
    pub adhesive_bottom: f64,
}

/// Window solids, build measurements, warnings and notes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowBuild {
    /// The clear sheet with its edge treatment and holes.
    pub window: Csg,
    /// The back print mask and dither dots; empty when print is off.
    pub print: Csg,
    /// Diffuser ink in the printed light segments; empty when absent.
    pub ink: Csg,
    /// The adhesive ring below the print; empty when adhesive is off.
    pub adhesive: Csg,
    pub report: WindowReport,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Kind;
    use std::collections::HashSet;

    #[test]
    fn window_parameters() {
        let table = param_table();
        let defaults = WindowParams::default();
        let mut keys = HashSet::new();
        for d in &table.descs {
            assert!(keys.insert(d.key), "parameter key is unique: {}", d.key);
            if let Kind::Float { get, hard_lo, hard_hi, .. } = &d.kind {
                assert!((*hard_lo..=*hard_hi).contains(&get(&defaults)),
                    "default is in the hard range: {}", d.key);
            }
        }
        let text = table.to_text(&defaults, None);
        let mut p = WindowParams::default();
        table.from_text(&text, &mut p, None).expect("default parameters parse");
        assert_eq!(table.to_text(&p, None), text, "default parameter text round trips");

        for index in 1..Screen::NAMES.len() as i32 {
            let mut p = WindowParams {
                screen: index,
                outline: Outline::Rect.index(),
                viewport: Viewport::None.index(),
                hole: HoleType::Round.index(),
                seg_count: 7,
                view_x: 12.0,
                view_y: -12.0,
                ..WindowParams::default()
            };
            let mut from_text = p.clone();
            from_text.screen = Screen::Custom.index();
            let screen = p.screen();
            let d = screen_data(screen);
            apply_screen(&mut p);
            if screen == Screen::LedRing25 {
                assert_eq!((p.outline(), p.viewport()), (Outline::Round, Viewport::None),
                    "LED ring has a round outline without a viewport");
                assert_eq!((p.hole(), p.hole_d), (HoleType::Notched2, 7.0),
                    "LED ring has a notched 7 mm encoder hole");
                assert_eq!((p.notch_width, p.notch_span), (2.0, 9.0),
                    "LED ring notches are 2 mm wide with a 9 mm span");
                assert_eq!((p.seg_count, p.seg_shape()), (24, SegShape::Wedge),
                    "LED ring has 24 wedge segments");
                assert!(p.seg_ring_d + p.seg_length < p.diameter && p.diameter > d.active_w,
                    "LED ring segments fit within the window outline");
                assert_eq!((p.material.name(), p.finish.name()), ("pc", "spi_b1"),
                    "LED ring uses PC with SPI B-1 finish");
            } else {
                assert_eq!((p.hole(), p.seg_count), (HoleType::None, 0),
                    "display preset removes the centre hole and segments: {}", d.name);
                assert_eq!((p.view_x, p.view_y), (d.active_x, d.active_y),
                    "viewport follows the active area offset: {}", d.name);
                if d.round {
                    assert_eq!((p.outline(), p.viewport()), (Outline::Round, Viewport::Round),
                        "round display selects a round window and viewport: {}", d.name);
                    assert!(p.view_d > d.active_w
                        && p.view_d + 2.0 * p.view_x.hypot(p.view_y) < p.diameter,
                        "round viewport covers the active area and fits inside the outline: {}", d.name);
                    assert!((p.view_d - d.active_w - 0.6).abs() < 1e-5,
                        "round viewport has 0.3 mm bleed per side: {}", d.name);
                } else {
                    assert_eq!((p.outline(), p.viewport()), (Outline::Rect, Viewport::Rect),
                        "rectangular display selects a rectangular window and viewport: {}", d.name);
                    assert!(p.view_w > d.active_w && p.view_h > d.active_h
                        && p.view_w + 2.0 * p.view_x.abs() < p.width
                        && p.view_h + 2.0 * p.view_y.abs() < p.height,
                        "rectangular viewport covers the active area and fits inside the outline: {}", d.name);
                    assert!((p.view_w - d.active_w - 0.6).abs() < 1e-5
                        && (p.view_h - d.active_h - 0.6).abs() < 1e-5,
                        "rectangular viewport has 0.3 mm bleed per side: {}", d.name);
                }
            }

            table.from_text(&format!("screen = {}", Screen::NAMES[index as usize]), &mut from_text, None)
                .expect("screen name parses");
            assert_eq!(from_text, p, "setting screen by text applies its preset: {}", d.name);
            let text = table.to_text(&p, None);
            let mut loaded = WindowParams::default();
            table.from_text(&text, &mut loaded, None).expect("preset parameters parse");
            assert_eq!(table.to_text(&loaded, None), text,
                "preset parameter text round trips: {}", d.name);

            let before = p.clone();
            let border = table.find("border").expect("border parameter exists");
            table.set(&mut p, border, before.border + 1.0);
            if screen == Screen::LedRing25 {
                let mut expected = before;
                expected.border += 1.0;
                assert_eq!(p, expected, "border leaves the LED ring preset unchanged");
            } else if d.round {
                assert!((p.diameter - before.diameter - 2.0).abs() < 1e-5,
                    "border grows both sides of the round outline: {}", d.name);
            } else {
                assert!((p.width - before.width - 2.0).abs() < 1e-5
                    && (p.height - before.height - 2.0).abs() < 1e-5,
                    "border grows all sides of the rectangular outline: {}", d.name);
            }
        }

        let mut custom = WindowParams {
            screen: Screen::Custom.index(),
            diameter: 47.0,
            seg_count: 11,
            ..WindowParams::default()
        };
        let mut expected = custom.clone();
        apply_screen(&mut custom);
        assert_eq!(custom, expected, "custom screen leaves all parameters unchanged");
        table.from_text("screen = Custom\nborder = 4", &mut custom, None)
            .expect("custom screen and border parse");
        expected.border = 4.0;
        assert_eq!(custom, expected, "custom border does not apply a display preset");

        let options = table.material_desc().expect("material picker exists").options();
        assert!(!options.is_empty(), "material picker offers clear sheet materials");
        for id in options {
            let m = MaterialId::lookup(id).expect("picker material exists").get();
            assert!(m.family.is_moulding() && m.optics.clarity == Clarity::Clear,
                "material picker contains only clear moulding materials: {id}");
        }
    }
}
