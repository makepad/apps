//! Knob dimensions, picker choices, file parameters and build measurements.

use crate::csg::Csg;
use crate::params::choices;
use crate::params::{self as part_params, field, ParamTable, PartParams};
use fab_materials::{Colour, Family, FinishId, Material, MaterialId, Processes};
use std::sync::OnceLock;

choices! { LobeStyle {
    Sine => "Sine", Scallop => "Scallop", Polygon => "Polygon", Star => "Star", Crenel => "Crenel",
} }
choices! { GripStyle {
    None => "None", Flutes => "Flutes", Knurl => "Diamond knurl", Rings => "Rings", Scallops => "Scallops",
} }
choices! { ScallopBottom { Ball => "Ball", Square => "Square" } }
choices! { ScallopTop {
    Open => "Open (through the top edge)", Ramp => "Ramp (undercut)", Ball => "Ball (undercut)",
} }
choices! { EdgeStyle { Fillet => "Fillet", Chamfer => "Chamfer" } }
choices! { FluteTop { Open => "Open", Fade => "Fade (undercut)" } }
choices! { TopStyle { Flat => "Flat", Dish => "Dish", Dome => "Dome", CapInsert => "Cap insert" } }
choices! { Indicator {
    None => "None", Groove => "Top groove", Ridge => "Raised line", Dot => "Dot",
    SideNotch => "Side notch", GrooveAndNotch => "Groove + notch", Line => "Pointer line",
} }
choices! { LineMode { Inlay => "Inlay (two-shot)", Raised => "Raised", Engraved => "Engraved" } }
choices! { Construction { Cored => "Cored (moulded)", Solid => "Solid (cast)" } }
choices! { BoreType {
    D => "D shaft", DoubleD => "Double D", Round => "Round", Spline => "Knurled spline",
    SplitKnurled => "Split knurled", Hex => "Hex", Square => "Square",
} }

/// Dimensions and manufacturing choices for a knob.
/// The knob axis is z, the bottom face is z = 0 and the indicator points along +x.
#[derive(Clone, Debug, PartialEq)]
pub struct KnobParams {
    /// Part name in saved parameter files.
    pub name: String,

    /// Largest body diameter at the lobe crests, mm.
    pub diameter: f64,
    /// Body height, mm.
    pub height: f64,
    /// Lobe count; zero gives a round body.
    pub lobes: i32,
    /// Stored [`LobeStyle`] index.
    pub lobe_style: i32,
    /// Lobe crest to valley depth, mm.
    pub lobe_depth: f64,
    /// Unitless sharpness, scallop width or corner radius control, 0..1.
    pub lobe_shape: f64,
    /// Twist from bottom to top, degrees.
    pub twist: f64,
    /// Top diameter divided by bottom diameter.
    pub taper: f64,
    /// Barrel bulge or negative waist depth, mm.
    pub bulge: f64,

    /// Stored [`GripStyle`] index.
    pub grip_style: i32,
    /// Flute, knurl, scallop or ring count.
    pub ridges: i32,
    /// Grip groove depth, mm.
    pub ridge_depth: f64,
    /// Grip start as a fraction of body height.
    pub grip_from: f64,
    /// Grip end as a fraction of body height.
    pub grip_to: f64,
    /// Knurl angle, degrees.
    pub knurl_angle: f64,
    /// Scallop width at the surface, mm.
    pub scallop_width: f64,
    /// Stored [`ScallopBottom`] index.
    pub scallop_bottom: i32,
    /// Stored [`ScallopTop`] index.
    pub scallop_top: i32,
    /// Scallop ramp angle from the pull axis, degrees.
    pub scallop_ramp: f64,
    /// Scallop rotation, degrees; zero puts the indicator on a land.
    pub scallop_phase: f64,
    /// Leave out the scallop nearest the indicator.
    pub scallop_skip: bool,

    /// Stored [`EdgeStyle`] index for the body top edge.
    pub edge_style: i32,
    /// Top edge fillet radius or chamfer size, mm.
    pub top_edge: f64,
    /// Bottom edge chamfer, mm.
    pub bottom_edge: f64,
    /// Stored [`TopStyle`] index.
    pub top_style: i32,
    /// Dish diameter as a fraction of the top diameter.
    pub dish_diameter: f64,
    /// Dish depth, mm.
    pub dish_depth: f64,
    /// Dome height above the body, mm.
    pub dome_height: f64,
    /// Flat rim between the top edge and cap recess, mm.
    pub rim_width: f64,
    /// Chamfer on the rim's inner edge, mm.
    pub rim_chamfer: f64,
    /// Cap insert thickness, mm.
    pub cap_thickness: f64,
    /// Cap top below the rim top, mm; zero is flush.
    pub cap_recess: f64,
    /// Cap crown height, mm; negative makes a dish.
    pub cap_crown: f64,
    /// Cap top edge fillet radius, mm.
    pub cap_edge: f64,
    /// Recess wall and cap side draft, degrees.
    pub cap_draft: f64,
    /// Cap insert colour in sRGB, 0..1 per channel.
    pub cap_colour: Colour,

    /// Add a flange at the bottom.
    pub skirt: bool,
    /// Skirt diameter, or size across polygon flats, mm.
    pub skirt_diameter: f64,
    /// Skirt height, mm.
    pub skirt_height: f64,
    /// Skirt top edge fillet or chamfer size, mm.
    pub skirt_edge: f64,
    /// Cove radius between the body and skirt top, mm.
    pub skirt_blend: f64,
    /// Skirt taper narrowing toward the top, degrees.
    pub skirt_taper: f64,
    /// Skirt polygon flat count; zero gives a round skirt.
    pub skirt_facets: i32,
    /// Skirt facet corner radius, mm.
    pub skirt_facet_round: f64,
    /// Stored [`EdgeStyle`] index for the skirt top edge.
    pub skirt_edge_style: i32,
    /// Skirt chamfer angle from the horizontal, degrees.
    pub skirt_chamfer_angle: f64,
    /// Stored [`FluteTop`] index.
    pub flute_top: i32,
    /// Groove indicator wall draft, degrees.
    pub ind_draft: f64,

    /// Stored [`Indicator`] index.
    pub indicator: i32,
    /// Indicator width, mm.
    pub ind_width: f64,
    /// Indicator depth or raised height, mm.
    pub ind_depth: f64,
    /// Groove or ridge start, or dot position, as a fraction of the top radius.
    pub ind_start: f64,
    /// Side notch depth, mm.
    pub notch_depth: f64,
    /// Stored [`LineMode`] index.
    pub line_mode: i32,
    /// Pointer line width at its inner end, mm.
    pub line_width_start: f64,
    /// Pointer line width at the edge and down the side, mm.
    pub line_width_end: f64,
    /// Inlaid or engraved line groove depth, mm.
    pub line_depth: f64,
    /// Raised line height above the surface, mm.
    pub line_height: f64,
    /// Side strip bottom above z = 0, mm; zero extends to the bottom or skirt.
    pub line_bottom: f64,
    /// Pointer strip and wedge wall draft, degrees.
    pub line_draft: f64,
    /// Pointer line colour in sRGB, 0..1 per channel.
    pub line_colour: Colour,

    /// Stored [`BoreType`] index.
    pub bore_type: i32,
    /// Nominal shaft diameter, mm.
    pub shaft_diameter: f64,
    /// D flat to the far side, or distance across opposed flats, mm.
    pub shaft_flat: f64,
    /// Spline or knurl tooth count.
    pub spline_teeth: i32,
    /// Radial bore clearance, mm; negative gives a press fit.
    pub clearance: f64,
    /// Bore depth above the bottom face, mm.
    pub bore_depth: f64,
    /// Bore flat angle relative to the indicator, degrees.
    pub bore_angle: f64,
    /// Bore opening lead-in size, mm.
    pub lead_in: f64,
    /// Bore draft, degrees; the bore widens toward its opening.
    pub bore_draft: f64,
    /// Key width across a split shaft's slot, mm.
    pub key_width: f64,
    /// Key depth into a split shaft's slot, mm.
    pub key_depth: f64,
    /// Add a radial set screw hole and a web through the coring.
    pub set_screw: bool,
    /// Set screw hole diameter, mm.
    pub screw_diameter: f64,
    /// Set screw centre above z = 0, mm.
    pub screw_height: f64,
    /// Set screw angle from the bore flat, degrees.
    pub screw_angle: f64,
    /// Bore crush rib count.
    pub crush_ribs: i32,
    /// Bore crush rib height, mm.
    pub crush_rib_height: f64,

    /// Stored [`Construction`] index.
    pub construction: i32,
    /// Cored body wall thickness, mm.
    pub wall: f64,
    /// Top skin thickness, mm.
    pub top_thickness: f64,
    /// Inside draft on the cavity walls, boss and ribs, degrees.
    pub draft: f64,
    /// Boss wall thickness around the bore, mm.
    pub boss_wall: f64,
    /// Boss bottom above z = 0, mm.
    pub boss_recess: f64,
    /// Supporting rib count.
    pub ribs: i32,
    /// Supporting rib thickness, mm.
    pub rib_thickness: f64,
    /// Rib bottom above z = 0, mm.
    pub rib_recess: f64,
    /// Supporting rib rotation, degrees.
    pub rib_angle: f64,

    /// Material for mass, manufacturing rules and appearance.
    pub material: MaterialId,
    /// Surface finish for roughness, texture and draft rules.
    pub finish: FinishId,
    /// Body colour in sRGB, including translucency from 0 to 0.999.
    pub colour: Colour,
    /// Points around a circular section.
    pub segments: i32,
}

impl Default for KnobParams {
    fn default() -> Self {
        Self {
            name: "knob".to_owned(),
            diameter: 20.0,
            height: 15.0,
            lobes: 5,
            lobe_style: LobeStyle::Sine.index(),
            lobe_depth: 1.5,
            lobe_shape: 0.5,
            twist: 0.0,
            taper: 0.94,
            bulge: 0.0,
            grip_style: GripStyle::None.index(),
            ridges: 36,
            ridge_depth: 0.35,
            grip_from: 0.10,
            grip_to: 0.80,
            knurl_angle: 30.0,
            scallop_width: 2.4,
            scallop_bottom: ScallopBottom::Ball.index(),
            scallop_top: ScallopTop::Open.index(),
            scallop_ramp: 30.0,
            scallop_phase: 0.0,
            scallop_skip: false,
            edge_style: EdgeStyle::Fillet.index(),
            top_edge: 1.5,
            bottom_edge: 0.4,
            top_style: TopStyle::Dish.index(),
            dish_diameter: 0.7,
            dish_depth: 0.8,
            dome_height: 2.0,
            rim_width: 1.2,
            rim_chamfer: 0.4,
            cap_thickness: 1.5,
            cap_recess: 0.2,
            cap_crown: 0.0,
            cap_edge: 0.3,
            cap_draft: 1.0,
            cap_colour: Colour::srgb([0xB4 as f32 / 255.0, 0xB3 as f32 / 255.0, 0xAD as f32 / 255.0]),
            skirt: false,
            skirt_diameter: 26.0,
            skirt_height: 2.0,
            skirt_edge: 0.6,
            skirt_blend: 1.5,
            skirt_taper: 0.0,
            skirt_facets: 0,
            skirt_facet_round: 0.5,
            skirt_edge_style: EdgeStyle::Fillet.index(),
            skirt_chamfer_angle: 45.0,
            flute_top: FluteTop::Open.index(),
            ind_draft: 3.0,
            indicator: Indicator::Groove.index(),
            ind_width: 1.0,
            ind_depth: 0.8,
            ind_start: 0.45,
            notch_depth: 0.8,
            line_mode: LineMode::Inlay.index(),
            line_width_start: 0.4,
            line_width_end: 1.0,
            line_depth: 0.6,
            line_height: 0.3,
            line_bottom: 0.0,
            line_draft: 0.5,
            line_colour: Colour::srgb([0.95, 0.95, 0.93]),
            bore_type: BoreType::D.index(),
            shaft_diameter: 6.0,
            shaft_flat: 4.5,
            spline_teeth: 18,
            clearance: 0.08,
            bore_depth: 11.0,
            bore_angle: 0.0,
            lead_in: 0.4,
            bore_draft: 0.0,
            key_width: 0.8,
            key_depth: 2.5,
            set_screw: false,
            screw_diameter: 2.5,
            screw_height: 5.0,
            screw_angle: 0.0,
            crush_ribs: 0,
            crush_rib_height: 0.15,
            construction: Construction::Cored.index(),
            wall: 1.6,
            top_thickness: 1.8,
            draft: 1.0,
            boss_wall: 1.5,
            boss_recess: 0.5,
            ribs: 4,
            rib_thickness: 1.0,
            rib_recess: 2.0,
            rib_angle: 45.0,
            material: MaterialId::lookup("abs").expect("ABS is in the material table"),
            finish: FinishId::lookup("vdi_27").expect("VDI 27 is in the finish table"),
            colour: Colour::srgb([0.10, 0.10, 0.11]),
            segments: 360,
        }
    }
}

impl KnobParams {
    pub fn lobe_style(&self) -> LobeStyle { LobeStyle::from_index(self.lobe_style) }
    pub fn grip_style(&self) -> GripStyle { GripStyle::from_index(self.grip_style) }
    pub fn scallop_bottom(&self) -> ScallopBottom { ScallopBottom::from_index(self.scallop_bottom) }
    pub fn scallop_top(&self) -> ScallopTop { ScallopTop::from_index(self.scallop_top) }
    pub fn edge_style(&self) -> EdgeStyle { EdgeStyle::from_index(self.edge_style) }
    pub fn skirt_edge_style(&self) -> EdgeStyle { EdgeStyle::from_index(self.skirt_edge_style) }
    pub fn flute_top(&self) -> FluteTop { FluteTop::from_index(self.flute_top) }
    pub fn top_style(&self) -> TopStyle { TopStyle::from_index(self.top_style) }
    pub fn indicator(&self) -> Indicator { Indicator::from_index(self.indicator) }
    pub fn line_mode(&self) -> LineMode { LineMode::from_index(self.line_mode) }
    pub fn bore_type(&self) -> BoreType { BoreType::from_index(self.bore_type) }
    pub fn construction(&self) -> Construction { Construction::from_index(self.construction) }

    /// Whether the body is cored for moulding rather than solid for casting.
    pub fn hollow(&self) -> bool { self.construction == Construction::Cored.index() }
}

impl PartParams for KnobParams {
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn colour(&self) -> Colour { self.colour }
    fn set_colour(&mut self, colour: Colour) { self.colour = colour; }
}

/// A shaft geometry and its semicolon-separated parameter assignments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShaftStandard {
    pub name: &'static str,
    pub settings: &'static str,
}

/// Common shaft geometries, in picker order.
pub fn shaft_standards() -> &'static [ShaftStandard] {
    static TABLE: [ShaftStandard; 14] = [
        ShaftStandard {
            name: "6 mm D-shaft",
            settings: "bore_type=D shaft; shaft_diameter=6; shaft_flat=4.5; set_screw=false",
        },
        ShaftStandard {
            name: "6 mm knurled shaft, 18 teeth",
            settings: "bore_type=Knurled spline; shaft_diameter=6; spline_teeth=18; set_screw=false",
        },
        ShaftStandard {
            name: "6 mm knurled shaft, 20 teeth",
            settings: "bore_type=Knurled spline; shaft_diameter=6; spline_teeth=20; set_screw=false",
        },
        ShaftStandard {
            name: "6 mm knurled shaft, 24 teeth",
            settings: "bore_type=Knurled spline; shaft_diameter=6; spline_teeth=24; set_screw=false",
        },
        ShaftStandard {
            name: "6 mm split knurled shaft, 18 teeth",
            settings: "bore_type=Split knurled; shaft_diameter=6; spline_teeth=18; key_width=0.8; key_depth=2.5; set_screw=false",
        },
        ShaftStandard {
            name: "6 mm round shaft, M3 set screw",
            settings: "bore_type=Round; shaft_diameter=6; set_screw=true; screw_diameter=2.5",
        },
        ShaftStandard {
            name: "1/4\" (6.35 mm) D-shaft",
            settings: "bore_type=D shaft; shaft_diameter=6.35; shaft_flat=5.6; set_screw=false",
        },
        ShaftStandard {
            name: "1/4\" (6.35 mm) round shaft, M3 set screw",
            settings: "bore_type=Round; shaft_diameter=6.35; set_screw=true; screw_diameter=2.5",
        },
        ShaftStandard {
            name: "1/8\" (3.175 mm) round shaft, M2 set screw",
            settings: "bore_type=Round; shaft_diameter=3.175; set_screw=true; screw_diameter=1.6",
        },
        ShaftStandard {
            name: "4 mm D-shaft",
            settings: "bore_type=D shaft; shaft_diameter=4; shaft_flat=3.5; set_screw=false",
        },
        ShaftStandard {
            name: "3 mm D-shaft",
            settings: "bore_type=D shaft; shaft_diameter=3; shaft_flat=2.5; set_screw=false",
        },
        ShaftStandard {
            name: "6 mm double D-shaft",
            settings: "bore_type=Double D; shaft_diameter=6; shaft_flat=4; set_screw=false",
        },
        ShaftStandard {
            name: "1/4\" hex shaft (6.35 mm across flats)",
            settings: "bore_type=Hex; shaft_diameter=6.35; shaft_flat=6.35; set_screw=false",
        },
        ShaftStandard {
            name: "4 mm square shaft (across flats)",
            settings: "bore_type=Square; shaft_diameter=5.66; shaft_flat=4; set_screw=false",
        },
    ];
    &TABLE
}

/// Apply a shaft's settings through the same table used by saved parameter files.
pub fn apply_shaft(p: &mut KnobParams, standard: &ShaftStandard) -> Result<(), String> {
    param_table().from_text(&standard.settings.replace(';', "\n"), p, None)
}

fn mould_or_print(m: &Material) -> bool {
    m.family.is_moulding()
        && (m.processes.intersects(Processes::INJECTION | Processes::FDM | Processes::SLA | Processes::SLS)
            || m.family == Family::Glass)
}

fn legacy_key(p: &mut KnobParams, key: &str, value: &str) -> bool {
    match key {
        "hollow" => {
            p.construction = if matches!(value, "false" | "0" | "no") {
                Construction::Solid.index()
            } else {
                Construction::Cored.index()
            };
            true
        }
        "roughness" => {
            // Use the file parser's numeric-prefix rules without imposing new limits.
            let d = part_params::float("roughness", "Roughness", "Look",
                |p: &KnobParams| f64::from(p.finish.get().render_roughness()),
                |p, value| param_table().legacy_roughness(p, value),
                f64::NEG_INFINITY, f64::INFINITY, "%g");
            param_table().set_text(p, &d, value)
        }
        _ => false,
    }
}

/// The shared inspector, serialization and sweep table, in display order.
pub fn param_table() -> &'static ParamTable<KnobParams> {
    static TABLE: OnceLock<ParamTable<KnobParams>> = OnceLock::new();
    TABLE.get_or_init(|| {
        macro_rules! row {
            ($builder:ident, $key:literal, $label:literal, $group:expr, $field:ident $(, $arg:expr)* $(,)?) => {{
                let (get, set) = field!(KnobParams, $field);
                part_params::$builder($key, $label, $group, get, set $(, $arg)*)
            }};
        }
        let mut descs = Vec::new();
        let mut g = "Body";
        descs.push(row!(float, "diameter", "Diameter", g, diameter, 6.0, 60.0, "%.1f mm")
            .tip("Largest body diameter (lobe crests)."));
        descs.push(row!(float, "height", "Height", g, height, 4.0, 40.0, "%.1f mm"));
        descs.push(row!(int, "lobes", "Lobes", g, lobes, 0, 16)
            .tip("0 = round body."));
        descs.push(row!(enumeration, "lobe_style", "Lobe style", g, lobe_style, LobeStyle::NAMES));
        descs.push(row!(float, "lobe_depth", "Lobe depth", g, lobe_depth, 0.0, 8.0, "%.2f mm")
            .tip("Crest to valley (not used by Polygon)."));
        descs.push(row!(float, "lobe_shape", "Lobe shape", g, lobe_shape, 0.0, 1.0, "%.2f")
            .tip("Sine/Star: valley sharpness. Scallop: cut width. Polygon: corner radius. Crenel: flank softness."));
        descs.push(row!(float, "twist", "Twist", g, twist, -180.0, 180.0, "%.0f deg"));
        descs.push(row!(float, "taper", "Taper", g, taper, 0.6, 1.2, "%.3f")
            .tip("Top diameter / bottom diameter."));
        descs.push(row!(float, "bulge", "Bulge", g, bulge, -3.0, 3.0, "%.2f mm")
            .tip("Barrel (+) or waist (-)."));
        g = "Grip";
        descs.push(row!(enumeration, "grip_style", "Grip", g, grip_style, GripStyle::NAMES));
        descs.push(row!(int, "ridges", "Ridges", g, ridges, 4, 160)
            .tip("Flutes / knurl / scallop count around, or ring count."));
        descs.push(row!(float, "ridge_depth", "Ridge depth", g, ridge_depth, 0.0, 3.0, "%.2f mm")
            .tip("Scallops: depth at the centre (at most half the width)."));
        descs.push(row!(float, "grip_from", "Grip from", g, grip_from, 0.0, 1.0, "%.2f")
            .tip("Fraction of the body height. Scallops: where the bottom end starts."));
        descs.push(row!(float, "grip_to", "Grip to", g, grip_to, 0.0, 1.0, "%.2f")
            .tip("Scallops: where the top end stops (kept below the top edge)."));
        descs.push(row!(float, "knurl_angle", "Knurl angle", g, knurl_angle, 10.0, 60.0, "%.0f deg"));
        descs.push(row!(enumeration, "flute_top", "Flute top", g, flute_top, FluteTop::NAMES)
            .tip("Open: flutes run out through the top edge, so the cavity half releases them. Fade: closed tops (an undercut)."));
        descs.push(row!(float, "scallop_width", "Scallop width", g, scallop_width, 0.6, 8.0, "%.2f mm")
            .tip("Across the scallop at the surface."));
        descs.push(row!(enumeration, "scallop_bottom", "Scallop bottom", g, scallop_bottom, ScallopBottom::NAMES)
            .tip("Bottom end. Ball: rounded (ball-end cutter). Square: a flat step. Both face up and release with the cavity (pulled along +z)."));
        descs.push(row!(enumeration, "scallop_top", "Scallop top", g, scallop_top, ScallopTop::NAMES)
            .tip("Top end. Open: the groove runs out through the top edge into air, the only top end that releases from a cavity pulled along +z. Ramp (floor rising to the surface at Scallop ramp) and Ball (closed, rounded) both overhang the groove: undercuts for a straight pull (side action, a down-pulled outside, or printing)."));
        descs.push(row!(float, "scallop_ramp", "Scallop ramp", g, scallop_ramp, 3.0, 75.0, "%.0f deg")
            .tip("Ramp top: the floor's angle to the axis; the ramp ends at Grip to."));
        descs.push(row!(float, "scallop_phase", "Scallop phase", g, scallop_phase, -180.0, 180.0, "%.1f deg")
            .tip("Turns the scallops. 0 = the indicator in the middle of a land; half a pitch = a scallop under it."));
        descs.push(row!(boolean, "scallop_skip", "Skip under indicator", g, scallop_skip)
            .tip("Leave out the scallop nearest the indicator (a wide land for the line)."));
        g = "Top & edges";
        descs.push(row!(enumeration, "edge_style", "Top edge", g, edge_style, EdgeStyle::NAMES));
        descs.push(row!(float, "top_edge", "Top edge size", g, top_edge, 0.0, 8.0, "%.2f mm"));
        descs.push(row!(float, "bottom_edge", "Bottom chamfer", g, bottom_edge, 0.0, 3.0, "%.2f mm"));
        descs.push(row!(enumeration, "top_style", "Top", g, top_style, TopStyle::NAMES));
        descs.push(row!(float, "dish_diameter", "Dish diameter", g, dish_diameter, 0.2, 1.0, "%.2f")
            .tip("Fraction of the flat top."));
        descs.push(row!(float, "dish_depth", "Dish depth", g, dish_depth, 0.1, 4.0, "%.2f mm"));
        descs.push(row!(float, "dome_height", "Dome height", g, dome_height, 0.2, 8.0, "%.2f mm"));
        descs.push(row!(float, "rim_width", "Rim width", g, rim_width, 0.4, 6.0, "%.2f mm")
            .tip("Cap insert: flat rim between the top edge and the recess."));
        descs.push(row!(float, "rim_chamfer", "Rim chamfer", g, rim_chamfer, 0.0, 2.0, "%.2f mm")
            .tip("Chamfer on the rim's inner edge, into the recess."));
        descs.push(row!(float, "cap_thickness", "Cap thickness", g, cap_thickness, 0.6, 5.0, "%.2f mm"));
        descs.push(row!(float, "cap_recess", "Cap recess", g, cap_recess, 0.0, 2.0, "%.2f mm")
            .tip("Cap top below the rim top (0 = flush)."));
        descs.push(row!(float, "cap_crown", "Cap crown", g, cap_crown, -1.5, 2.0, "%.2f mm")
            .tip("Cap top: + domed, - dished, 0 flat."));
        descs.push(row!(float, "cap_edge", "Cap edge fillet", g, cap_edge, 0.0, 1.5, "%.2f mm"));
        descs.push(row!(float, "cap_draft", "Recess draft", g, cap_draft, 0.0, 5.0, "%.1f deg")
            .tip("Recess wall and cap side, wider at the top."));
        descs.push(row!(colour, "cap_color", "Cap colour", g, cap_colour));
        g = "Skirt";
        descs.push(row!(boolean, "skirt", "Skirt", g, skirt)
            .tip("Round flange at the bottom."));
        descs.push(row!(float, "skirt_diameter", "Skirt diameter", g, skirt_diameter, 8.0, 80.0, "%.1f mm"));
        descs.push(row!(float, "skirt_height", "Skirt height", g, skirt_height, 0.5, 10.0, "%.2f mm"));
        descs.push(row!(float, "skirt_edge", "Skirt edge", g, skirt_edge, 0.0, 3.0, "%.2f mm"));
        descs.push(row!(float, "skirt_blend", "Skirt blend", g, skirt_blend, 0.0, 6.0, "%.2f mm")
            .tip("Concave fillet from the body into the skirt; lobes and grip run out inside it."));
        descs.push(row!(float, "skirt_taper", "Skirt taper", g, skirt_taper, 0.0, 30.0, "%.1f deg")
            .tip("The skirt narrows toward its top; also its draft for the cavity pull."));
        descs.push(row!(int, "skirt_facets", "Skirt facets", g, skirt_facets, 0, 32)
            .tip("0 = round; else flats (diameter across the flats)."));
        descs.push(row!(float, "skirt_facet_round", "Facet corners", g, skirt_facet_round, 0.0, 5.0, "%.2f mm")
            .tip("Corner radius of a faceted skirt."));
        descs.push(row!(enumeration, "skirt_edge_style", "Skirt edge style", g, skirt_edge_style, EdgeStyle::NAMES));
        descs.push(row!(float, "skirt_chamfer_angle", "Skirt chamfer angle", g, skirt_chamfer_angle, 10.0, 80.0, "%.0f deg")
            .tip("From the horizontal: low = a wide flat bevel, high = a steep facet. Skirt edge sets its width."));
        g = "Indicator";
        descs.push(row!(enumeration, "indicator", "Indicator", g, indicator, Indicator::NAMES)
            .tip("Points along +x."));
        descs.push(row!(float, "ind_width", "Width", g, ind_width, 0.3, 4.0, "%.2f mm"));
        descs.push(row!(float, "ind_depth", "Depth / height", g, ind_depth, 0.1, 3.0, "%.2f mm"));
        descs.push(row!(float, "ind_draft", "Groove draft", g, ind_draft, 0.0, 10.0, "%.1f deg")
            .tip("Draft on the groove indicator's side and end walls."));
        descs.push(row!(float, "ind_start", "Start", g, ind_start, 0.0, 0.95, "%.2f")
            .tip("Groove start as a fraction of the top radius; dot position."));
        descs.push(row!(float, "notch_depth", "Notch depth", g, notch_depth, 0.1, 3.0, "%.2f mm"));
        descs.push(row!(enumeration, "line_mode", "Line", g, line_mode, LineMode::NAMES)
            .tip("Inlay: a separate flush body (two-shot) in a groove. Raised: a separate body standing proud. Engraved: a groove for paint."));
        descs.push(row!(float, "line_width_start", "Width at start", g, line_width_start, 0.1, 4.0, "%.2f mm")
            .tip("Line width at its inner end on the top."));
        descs.push(row!(float, "line_width_end", "Width at edge", g, line_width_end, 0.2, 4.0, "%.2f mm")
            .tip("Line width at the top edge and down the side."));
        descs.push(row!(float, "line_depth", "Line depth", g, line_depth, 0.1, 2.0, "%.2f mm")
            .tip("Inlay / engraved groove depth."));
        descs.push(row!(float, "line_height", "Line height", g, line_height, 0.05, 1.5, "%.2f mm")
            .tip("Raised: proud of the surface."));
        descs.push(row!(float, "line_bottom", "Line bottom", g, line_bottom, 0.0, 40.0, "%.1f mm")
            .tip("Height where the side strip ends. 0 = full (down to the bottom or skirt)."));
        descs.push(row!(float, "line_draft", "Line draft", g, line_draft, 0.0, 3.0, "%.2f deg")
            .tip("Walls of the strip (narrower toward the bottom) and the wedge."));
        descs.push(row!(colour, "line_color", "Line colour", g, line_colour));
        g = "Shaft";
        descs.push(row!(enumeration, "bore_type", "Bore", g, bore_type, BoreType::NAMES));
        descs.push(row!(float, "shaft_diameter", "Shaft diameter", g, shaft_diameter, 2.0, 12.0, "%.2f mm"));
        descs.push(row!(float, "shaft_flat", "Across flat(s)", g, shaft_flat, 1.0, 12.0, "%.2f mm")
            .tip("D: flat to the far side. Double D, hex, square: flat to flat."));
        descs.push(row!(int, "spline_teeth", "Spline teeth", g, spline_teeth, 6, 40));
        descs.push(row!(float, "key_width", "Slot key width", g, key_width, 0.3, 2.0, "%.2f mm")
            .tip("Key across the bore that sits in a split shaft's slot."));
        descs.push(row!(float, "key_depth", "Slot key depth", g, key_depth, 0.5, 8.0, "%.2f mm"));
        descs.push(row!(float, "clearance", "Clearance", g, clearance, -0.2, 0.4, "%.3f mm")
            .tip("Radial. Negative = press fit."));
        descs.push(row!(float, "bore_depth", "Bore depth", g, bore_depth, 2.0, 30.0, "%.1f mm"));
        descs.push(row!(float, "bore_angle", "Flat angle", g, bore_angle, -180.0, 180.0, "%.0f deg")
            .tip("Flat direction relative to the indicator."));
        descs.push(row!(float, "lead_in", "Lead-in", g, lead_in, 0.0, 1.5, "%.2f mm"));
        descs.push(row!(float, "bore_draft", "Bore draft", g, bore_draft, 0.0, 2.0, "%.2f deg")
            .tip("Bore wider at the opening so the core pin releases. 0 = straight."));
        descs.push(row!(boolean, "set_screw", "Set screw", g, set_screw)
            .tip("Radial hole for a grub screw; a solid web bridges the coring."));
        descs.push(row!(float, "screw_diameter", "Screw hole", g, screw_diameter, 1.2, 5.0, "%.2f mm")
            .tip("Tap drill: M2 1.6, M2.5 2.05, M3 2.5. Self-tapping: about the screw size minus 0.3."));
        descs.push(row!(float, "screw_height", "Screw height", g, screw_height, 1.0, 30.0, "%.1f mm"));
        descs.push(row!(float, "screw_angle", "Screw angle", g, screw_angle, -180.0, 180.0, "%.0f deg")
            .tip("From the flat (0 = bears on the D flat)."));
        descs.push(row!(int, "crush_ribs", "Crush ribs", g, crush_ribs, 0, 8));
        descs.push(row!(float, "crush_height", "Crush rib height", g, crush_rib_height, 0.02, 0.5, "%.2f mm"));
        g = "Moulding";
        descs.push(row!(enumeration, "construction", "Construction", g, construction, Construction::NAMES)
            .tip("Cored: carved out for injection moulding (even walls, a boss round the bore, ribs).\nSolid: a full body for resin casting in a silicone mould (PU / epoxy) or machining: no walls to keep even, no sink."));
        descs.push(row!(float, "wall", "Wall", g, wall, 0.6, 5.0, "%.2f mm"));
        descs.push(row!(float, "top_thickness", "Top thickness", g, top_thickness, 0.6, 6.0, "%.2f mm"));
        descs.push(row!(float, "draft", "Draft", g, draft, 0.0, 5.0, "%.1f deg")
            .tip("Inside draft: cavity walls, boss and ribs. The outside draft is the body taper."));
        descs.push(row!(float, "boss_wall", "Boss wall", g, boss_wall, 0.6, 5.0, "%.2f mm"));
        descs.push(row!(float, "boss_recess", "Boss recess", g, boss_recess, 0.0, 15.0, "%.2f mm"));
        descs.push(row!(int, "ribs", "Ribs", g, ribs, 0, 12));
        descs.push(row!(float, "rib_thickness", "Rib thickness", g, rib_thickness, 0.4, 3.0, "%.2f mm"));
        descs.push(row!(float, "rib_recess", "Rib recess", g, rib_recess, 0.0, 20.0, "%.2f mm"));
        descs.push(row!(float, "rib_angle", "Rib angle", g, rib_angle, 0.0, 180.0, "%.0f deg"));
        g = "Look";
        descs.push(row!(material, "material", "Material", g, material, Some(mould_or_print), None, &[])
            .tip("Plastic: mass, wall / draft rules, colours it comes in (conductive grades: dark only)."));
        descs.push(row!(finish, "finish", "Finish", g, finish, None, &[])
            .tip("Mould polish (SPI), EDM texture (VDI 3400), grain, or as printed / machined: render roughness, texture and draft."));
        descs.push(row!(colour, "color", "Colour", g, colour));
        descs.push(part_params::float("translucency", "Translucency", g,
            |p: &KnobParams| f64::from(p.colour.translucency), |p, value| p.colour.translucency = value as f32,
            0.0, 0.999, "%.3f")
            .tip("Clear / frosted / tinted plastics (PC, PMMA, translucent ABS)."));
        descs.push(row!(int, "segments", "Segments", g, segments, 64, 1440)
            .tip("Points around. Raise for fine grip ridges."));
        ParamTable { descs, header: "# Knob", after_set: None, legacy: Some(legacy_key) }
    })
}

/// Displayed dimensions and mesh measurements; populated when the generator is available.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KnobReport {
    /// Overall body or skirt diameter, mm.
    pub diameter: f64,
    /// Overall height including the dome, cap crown and raised line, mm.
    pub height: f64,
    /// Shaft diameter plus diametral clearance, mm.
    pub bore: f64,
    /// Mass from the material density and mesh volume, g.
    pub mass_g: f64,
    /// Mesh volume, mm cubed; divide by 1000 for cm cubed.
    pub volume_mm3: f64,
    /// Mesh surface area, mm squared.
    pub area_mm2: f64,
    /// Mesh triangle count.
    pub triangles: usize,
    /// Build duration, milliseconds.
    pub build_ms: f64,
}

/// A separate cap or pointer line touching the main body without overlapping it.
#[derive(Clone, Debug, PartialEq)]
pub struct KnobBody {
    /// Body name: cap or line.
    pub name: String,
    /// Body solid as a CSG program.
    pub csg: Csg,
    /// Body colour in sRGB, 0..1 per channel.
    pub colour: Colour,
    /// Perceptual surface roughness, 0..1.
    pub roughness: f32,
}

impl Default for KnobBody {
    fn default() -> Self {
        Self { name: String::new(), csg: Csg::default(), colour: Colour::srgb([0.0; 3]), roughness: 0.45 }
    }
}

/// Main solid, measurements and additional bodies; empty until a knob is generated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KnobBuild {
    /// Main solid, named body when additional bodies exist.
    pub csg: Csg,
    pub report: KnobReport,
    /// Moulding or fit problems.
    pub warnings: Vec<String>,
    /// Adjustments made while building the requested shape.
    pub notes: Vec<String>,
    /// Separate cap and pointer line solids.
    pub bodies: Vec<KnobBody>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Kind;
    use fab_materials::material::MATERIALS;
    use std::collections::HashSet;

    #[test]
    fn knob_parameters() {
        let table = param_table();
        let defaults = KnobParams::default();
        let mut keys = HashSet::new();
        for d in &table.descs {
            assert!(keys.insert(d.key), "parameter key is unique: {}", d.key);
            if let Kind::Float { get, hard_lo, hard_hi, .. } = &d.kind {
                assert!((*hard_lo..=*hard_hi).contains(&get(&defaults)),
                    "default is in the hard range: {}", d.key);
            }
        }
        let text = table.to_text(&defaults, None);
        let mut p = KnobParams::default();
        table.from_text(&text, &mut p, None).expect("default parameters parse");
        assert_eq!(table.to_text(&p, None), text, "default parameter text round trips");

        let expected = [
            (BoreType::D, 6.0, Some(4.5), None, None, None),
            (BoreType::Spline, 6.0, None, Some(18), None, None),
            (BoreType::Spline, 6.0, None, Some(20), None, None),
            (BoreType::Spline, 6.0, None, Some(24), None, None),
            (BoreType::SplitKnurled, 6.0, None, Some(18), Some((0.8, 2.5)), None),
            (BoreType::Round, 6.0, None, None, None, Some(2.5)),
            (BoreType::D, 6.35, Some(5.6), None, None, None),
            (BoreType::Round, 6.35, None, None, None, Some(2.5)),
            (BoreType::Round, 3.175, None, None, None, Some(1.6)),
            (BoreType::D, 4.0, Some(3.5), None, None, None),
            (BoreType::D, 3.0, Some(2.5), None, None, None),
            (BoreType::DoubleD, 6.0, Some(4.0), None, None, None),
            (BoreType::Hex, 6.35, Some(6.35), None, None, None),
            (BoreType::Square, 5.66, Some(4.0), None, None, None),
        ];
        assert_eq!(shaft_standards().len(), expected.len(), "every standard shaft has a geometry check");
        for (standard, (bore, diameter, flat, teeth, key, screw)) in shaft_standards().iter().zip(expected) {
            let mut p = KnobParams {
                bore_type: BoreType::Square.index(), shaft_diameter: 12.0, shaft_flat: 12.0,
                spline_teeth: 40, key_width: 2.0, key_depth: 8.0,
                set_screw: screw.is_none(), screw_diameter: 5.0,
                ..KnobParams::default()
            };
            assert!(apply_shaft(&mut p, standard).is_ok(), "shaft settings apply: {}", standard.name);
            assert_eq!((p.bore_type(), p.shaft_diameter), (bore, diameter),
                "shaft bore type and diameter match its geometry: {}", standard.name);
            if let Some(flat) = flat {
                assert_eq!(p.shaft_flat, flat, "shaft flat dimension matches its geometry: {}", standard.name);
            }
            if let Some(teeth) = teeth {
                assert_eq!(p.spline_teeth, teeth, "shaft tooth count matches its geometry: {}", standard.name);
            }
            if let Some(key) = key {
                assert_eq!((p.key_width, p.key_depth), key, "split shaft key matches its slot: {}", standard.name);
            }
            assert_eq!(p.set_screw, screw.is_some(), "shaft sets or clears the set screw: {}", standard.name);
            if let Some(screw) = screw {
                assert_eq!(p.screw_diameter, screw, "shaft set screw uses its tap drill: {}", standard.name);
            }
            let text = table.to_text(&p, None);
            let mut loaded = KnobParams::default();
            table.from_text(&text, &mut loaded, None).expect("shaft parameters parse");
            assert_eq!(table.to_text(&loaded, None), text, "shaft parameter text round trips: {}", standard.name);
        }

        for value in ["false", "0", "no", "true", "1", "yes", "other"] {
            let solid = matches!(value, "false" | "0" | "no");
            let mut p = KnobParams {
                construction: if solid { Construction::Cored.index() } else { Construction::Solid.index() },
                ..KnobParams::default()
            };
            table.from_text(&format!("hollow = {value}"), &mut p, None).expect("legacy hollow parses");
            assert_eq!(p.construction(), if solid { Construction::Solid } else { Construction::Cored },
                "legacy hollow selects construction: {value}");
            assert_eq!(p.hollow(), !solid, "hollow accessor agrees with construction: {value}");
        }
        for (value, finish) in [("0.55", "vdi_27"), ("0.10", "spi_a2"), ("9e-1 old", "vdi_45")] {
            let mut p = KnobParams::default();
            table.from_text(&format!("roughness = {value}"), &mut p, None).expect("legacy roughness parses");
            assert_eq!(p.finish.name(), finish, "legacy roughness selects the nearest eligible finish: {value}");
        }

        let options = table.material_desc().expect("material picker exists").options();
        assert!(options.contains(&"abs"), "material picker includes ABS");
        for m in MATERIALS {
            let allowed = m.family.is_moulding()
                && (m.family == Family::Glass
                    || [Processes::INJECTION, Processes::FDM, Processes::SLA, Processes::SLS]
                        .iter().any(|process| m.processes.contains(*process)));
            assert_eq!(options.contains(&m.id), allowed,
                "material picker offers exactly moulding, printing and glass materials, never board rows: {}", m.id);
        }
    }
}
