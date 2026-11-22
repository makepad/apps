//! Jack ring dimensions, thread standards and shared material choices.

use crate::csg::Csg;
use crate::params::{self as part_params, choices, field, ParamDesc, ParamTable, PartParams};
use fab_materials::{Colour, Family, FinishId, FinishStandard, MaterialId, Processes};
use std::sync::OnceLock;

choices! {
    /// ISO 261 metric threads and the 3/8-inch UNEF jack thread.
    ThreadStd {
        Custom => "Custom",
        M2x04 => "M2 x 0.4",
        M2x025 => "M2 x 0.25",
        M25x045 => "M2.5 x 0.45",
        M25x035 => "M2.5 x 0.35",
        M3x05 => "M3 x 0.5",
        M3x035 => "M3 x 0.35",
        M35x06 => "M3.5 x 0.6",
        M35x035 => "M3.5 x 0.35",
        M4x07 => "M4 x 0.7",
        M4x05 => "M4 x 0.5",
        M5x08 => "M5 x 0.8",
        M5x05 => "M5 x 0.5",
        M6x1 => "M6 x 1",
        M6x075 => "M6 x 0.75",
        M6x05 => "M6 x 0.5",
        M7x1 => "M7 x 1",
        M7x075 => "M7 x 0.75",
        M7x05 => "M7 x 0.5",
        M8x125 => "M8 x 1.25",
        M8x1 => "M8 x 1",
        M8x075 => "M8 x 0.75",
        M8x05 => "M8 x 0.5",
        M9x125 => "M9 x 1.25",
        M9x1 => "M9 x 1",
        M9x075 => "M9 x 0.75",
        M9x05 => "M9 x 0.5",
        M10x15 => "M10 x 1.5",
        M10x125 => "M10 x 1.25",
        M10x1 => "M10 x 1",
        M10x075 => "M10 x 0.75",
        M11x15 => "M11 x 1.5",
        M11x125 => "M11 x 1.25",
        M11x1 => "M11 x 1",
        M11x075 => "M11 x 0.75",
        M12x175 => "M12 x 1.75",
        M12x15 => "M12 x 1.5",
        M12x125 => "M12 x 1.25",
        M12x1 => "M12 x 1",
        M12x075 => "M12 x 0.75",
        M13x175 => "M13 x 1.75",
        M13x15 => "M13 x 1.5",
        M13x1 => "M13 x 1",
        M14x2 => "M14 x 2",
        M14x15 => "M14 x 1.5",
        M14x125 => "M14 x 1.25",
        M14x1 => "M14 x 1",
        UNEF38x32 => "3/8\"-32 UNEF",
    }
}
choices! { ThreadMode {
    Modelled => "Modelled", Cosmetic => "Cosmetic (plain minor bore)", Clearance => "Clearance (slides over)",
} }
choices! { EdgeStyle { Sharp => "Sharp", Chamfer => "Chamfer", Round => "Round" } }
choices! { TopStyle { Flat => "Flat", Dome => "Dome", Step => "Step" } }
choices! { Grip {
    None => "None", Flats => "Spanner flats", Hex => "Hex", StraightKnurl => "Straight knurl",
    DiamondKnurl => "Diamond knurl", Flutes => "Flutes",
} }
choices! { Anodize {
    Clear => "Clear", Black => "Black", Red => "Red", Blue => "Blue", Teal => "Teal", Gold => "Gold",
    Purple => "Purple", Green => "Green", Orange => "Orange", Rose => "Rose", Champagne => "Champagne",
    Titanium => "Titanium grey",
} }

/// A bushing thread's major diameter and pitch, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadData {
    pub name: &'static str,
    pub d: f64,
    pub pitch: f64,
    pub note: &'static str,
}

const THREAD_DATA: [ThreadData; 48] = [
    ThreadData {
        name: ThreadStd::NAMES[0],
        d: 11.0,
        pitch: 1.0,
        note: "",
    },
    ThreadData {
        name: ThreadStd::NAMES[1],
        d: 2.0,
        pitch: 0.4,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[2],
        d: 2.0,
        pitch: 0.25,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[3],
        d: 2.5,
        pitch: 0.45,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[4],
        d: 2.5,
        pitch: 0.35,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[5],
        d: 3.0,
        pitch: 0.5,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[6],
        d: 3.0,
        pitch: 0.35,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[7],
        d: 3.5,
        pitch: 0.6,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[8],
        d: 3.5,
        pitch: 0.35,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[9],
        d: 4.0,
        pitch: 0.7,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[10],
        d: 4.0,
        pitch: 0.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[11],
        d: 5.0,
        pitch: 0.8,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[12],
        d: 5.0,
        pitch: 0.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[13],
        d: 6.0,
        pitch: 1.0,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[14],
        d: 6.0,
        pitch: 0.75,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[15],
        d: 6.0,
        pitch: 0.5,
        note: "ISO fine — 3.5 mm jacks",
    },
    ThreadData {
        name: ThreadStd::NAMES[16],
        d: 7.0,
        pitch: 1.0,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[17],
        d: 7.0,
        pitch: 0.75,
        note: "ISO fine — 9 mm pots, toggle and push switches",
    },
    ThreadData {
        name: ThreadStd::NAMES[18],
        d: 7.0,
        pitch: 0.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[19],
        d: 8.0,
        pitch: 1.25,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[20],
        d: 8.0,
        pitch: 1.0,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[21],
        d: 8.0,
        pitch: 0.75,
        note: "ISO fine — mini jack and pot bushings",
    },
    ThreadData {
        name: ThreadStd::NAMES[22],
        d: 8.0,
        pitch: 0.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[23],
        d: 9.0,
        pitch: 1.25,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[24],
        d: 9.0,
        pitch: 1.0,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[25],
        d: 9.0,
        pitch: 0.75,
        note: "ISO fine — 16 mm pots, slim 1/4\" jacks",
    },
    ThreadData {
        name: ThreadStd::NAMES[26],
        d: 9.0,
        pitch: 0.5,
        note: "ISO fine — fine-thread jack and pot bushings",
    },
    ThreadData {
        name: ThreadStd::NAMES[27],
        d: 10.0,
        pitch: 1.5,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[28],
        d: 10.0,
        pitch: 1.25,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[29],
        d: 10.0,
        pitch: 1.0,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[30],
        d: 10.0,
        pitch: 0.75,
        note: "ISO fine — pot and jack bushings",
    },
    ThreadData {
        name: ThreadStd::NAMES[31],
        d: 11.0,
        pitch: 1.5,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[32],
        d: 11.0,
        pitch: 1.25,
        note: "ISO fine — 1/4\" jack",
    },
    ThreadData {
        name: ThreadStd::NAMES[33],
        d: 11.0,
        pitch: 1.0,
        note: "ISO fine — 1/4\" jack bushings",
    },
    ThreadData {
        name: ThreadStd::NAMES[34],
        d: 11.0,
        pitch: 0.75,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[35],
        d: 12.0,
        pitch: 1.75,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[36],
        d: 12.0,
        pitch: 1.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[37],
        d: 12.0,
        pitch: 1.25,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[38],
        d: 12.0,
        pitch: 1.0,
        note: "ISO fine — 1/4\" jacks",
    },
    ThreadData {
        name: ThreadStd::NAMES[39],
        d: 12.0,
        pitch: 0.75,
        note: "ISO fine — large 1/4\" jack and XLR-combo bushings",
    },
    ThreadData {
        name: ThreadStd::NAMES[40],
        d: 13.0,
        pitch: 1.75,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[41],
        d: 13.0,
        pitch: 1.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[42],
        d: 13.0,
        pitch: 1.0,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[43],
        d: 14.0,
        pitch: 2.0,
        note: "ISO coarse",
    },
    ThreadData {
        name: ThreadStd::NAMES[44],
        d: 14.0,
        pitch: 1.5,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[45],
        d: 14.0,
        pitch: 1.25,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[46],
        d: 14.0,
        pitch: 1.0,
        note: "ISO fine",
    },
    ThreadData {
        name: ThreadStd::NAMES[47],
        d: 9.525,
        pitch: 25.4 / 32.0,
        note: "1/4\" jacks (3/8\"-32, 0.375\" panel hole)",
    },
];

const THREAD_NOTES: [&str; THREAD_DATA.len()] = {
    let mut notes = [""; THREAD_DATA.len()];
    let mut i = 0;
    while i < notes.len() {
        notes[i] = THREAD_DATA[i].note;
        i += 1;
    }
    notes[17] = "ISO fine — 9 mm pots & switches";
    notes[21] = "ISO fine — mini jack & pot bushings";
    notes[25] = "ISO fine — 16 mm pots & slim 1/4\" jacks";
    notes[26] = "ISO fine — fine jack & pot bushings";
    notes[30] = "ISO fine — pot & jack bushings";
    notes[39] = "ISO fine — large 1/4\" & XLR-combo bushings";
    notes[47] = "1/4\" jacks";
    notes
};

/// Dimensions for a thread standard.
pub fn thread_data(standard: ThreadStd) -> &'static ThreadData {
    &THREAD_DATA[standard.index() as usize]
}

/// A threaded jack nut. z = 0 rests on the panel; +z points out of the panel.
#[derive(Clone, Debug, PartialEq)]
pub struct RingParams {
    pub name: String,
    /// Stored [`ThreadStd`] index.
    pub thread_std: i32,
    pub thread_d: f64,
    pub pitch: f64,
    /// Stored [`ThreadMode`] index.
    pub thread_mode: i32,
    /// Radial clearance over the basic thread profile, mm.
    pub clearance: f64,
    /// 45 degree countersink at both faces, measured from the minor radius, mm.
    pub lead_in: f64,
    pub diameter: f64,
    pub height: f64,
    /// Stored [`EdgeStyle`] index.
    pub top_edge: i32,
    /// Chamfer width across the face, or round radius, mm.
    pub top_edge_size: f64,
    /// Chamfer angle from the top face, degrees.
    pub top_angle: f64,
    /// Stored [`EdgeStyle`] index.
    pub bottom_edge: i32,
    pub bottom_edge_size: f64,
    /// Stored [`TopStyle`] index.
    pub top_style: i32,
    pub dome_height: f64,
    pub step_diameter: f64,
    pub step_height: f64,
    pub step_edge: f64,
    /// Stored [`Grip`] index.
    pub grip: i32,
    pub across_flats: f64,
    pub ribs: i32,
    pub grip_depth: f64,
    /// Diamond knurl helix angle from the axis, degrees.
    pub knurl_angle: f64,
    /// Grip band bounds as fractions of the side height.
    pub grip_from: f64,
    pub grip_to: f64,
    pub slots: i32,
    pub slot_width: f64,
    pub slot_depth: f64,
    pub flange: bool,
    pub flange_diameter: f64,
    pub flange_thickness: f64,
    pub flange_edge: f64,
    pub panel_thickness: f64,
    /// Thread length beyond the jack's front face, mm.
    pub bushing_length: f64,
    pub material: MaterialId,
    /// Stored [`Anodize`] index; used only for anodised aluminium.
    pub anodize: i32,
    pub finish: FinishId,
    /// sRGB colour; metals remain opaque.
    pub colour: Colour,
    pub segments: i32,
}

impl Default for RingParams {
    fn default() -> Self {
        Self {
            name: "ring".to_owned(),
            thread_std: ThreadStd::M11x1.index(),
            thread_d: 11.0,
            pitch: 1.0,
            thread_mode: ThreadMode::Modelled.index(),
            clearance: 0.05,
            lead_in: 0.6,
            diameter: 16.0,
            height: 3.5,
            top_edge: EdgeStyle::Chamfer.index(),
            top_edge_size: 0.8,
            top_angle: 45.0,
            bottom_edge: EdgeStyle::Chamfer.index(),
            bottom_edge_size: 0.2,
            top_style: TopStyle::Flat.index(),
            dome_height: 0.8,
            step_diameter: 13.0,
            step_height: 1.0,
            step_edge: 0.3,
            grip: Grip::None.index(),
            across_flats: 14.0,
            ribs: 36,
            grip_depth: 0.3,
            knurl_angle: 30.0,
            grip_from: 0.12,
            grip_to: 0.88,
            slots: 0,
            slot_width: 1.2,
            slot_depth: 0.8,
            flange: false,
            flange_diameter: 19.0,
            flange_thickness: 0.8,
            flange_edge: 0.3,
            panel_thickness: 2.0,
            bushing_length: 7.0,
            material: MaterialId::lookup("brass").expect("brass is in the material table"),
            anodize: Anodize::Black.index(),
            finish: FinishId::lookup("metal_polish").expect("metal polish is in the finish table"),
            colour: Colour::srgb([0.948, 0.861, 0.674]),
            segments: 192,
        }
    }
}

impl RingParams {
    pub fn thread_std(&self) -> ThreadStd {
        ThreadStd::from_index(self.thread_std)
    }
    pub fn thread_mode(&self) -> ThreadMode {
        ThreadMode::from_index(self.thread_mode)
    }
    pub fn top_edge(&self) -> EdgeStyle {
        EdgeStyle::from_index(self.top_edge)
    }
    pub fn bottom_edge(&self) -> EdgeStyle {
        EdgeStyle::from_index(self.bottom_edge)
    }
    pub fn top_style(&self) -> TopStyle {
        TopStyle::from_index(self.top_style)
    }
    pub fn grip(&self) -> Grip {
        Grip::from_index(self.grip)
    }
    pub fn anodize(&self) -> Anodize {
        Anodize::from_index(self.anodize)
    }
}

impl PartParams for RingParams {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
    fn colour(&self) -> Colour {
        self.colour
    }
    fn set_colour(&mut self, colour: Colour) {
        self.colour = colour.with_translucency(0.0);
    }
}

/// ISO 68-1 internal minor diameter, mm.
pub fn minor_diameter(p: &RingParams) -> f64 {
    p.thread_d - 1.0825 * p.pitch
}

/// Thread designation, including rounded dimensions for a custom thread.
pub fn thread_text(p: &RingParams) -> String {
    if p.thread_std() != ThreadStd::Custom {
        return thread_data(p.thread_std()).name.to_owned();
    }
    format!(
        "M{} x {}",
        general((p.thread_d * 1000.0).round() / 1000.0),
        general((p.pitch * 10000.0).round() / 10000.0)
    )
}

// Six significant digits, using fixed notation for exponents -4 through 5.
fn general(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string().to_lowercase();
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific exponent");
    let exponent: i32 = exponent.parse().expect("integer exponent");
    if (-4..6).contains(&exponent) {
        let precision = (5 - exponent) as usize;
        let fixed = format!("{value:.precision$}");
        if precision == 0 {
            fixed
        } else {
            fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
    } else {
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        format!("{mantissa}e{exponent:+03}")
    }
}

/// An opaque type II anodising colour from the shared swatch table.
pub fn anodize_colour(anodize: Anodize) -> Colour {
    const NAMES: [&str; 12] = [
        "Anodising clear",
        "Anodising black",
        "Anodising red",
        "Anodising blue",
        "Anodising teal",
        "Anodising gold",
        "Anodising purple",
        "Anodising green",
        "Anodising orange",
        "Anodising rose",
        "Anodising champagne",
        "Anodising titanium",
    ];
    fab_materials::swatch::swatch(NAMES[anodize.index() as usize])
        .expect("anodising colour is in the swatch table")
        .colour
}

/// The metal's natural colour, or the selected anodising for aluminium.
pub fn ring_colour(p: &RingParams) -> Colour {
    if p.material.name() == "aluminium_anodised" {
        anodize_colour(p.anodize())
    } else {
        Colour::srgb(p.material.get().natural)
    }
}

/// Finish roughness, with the satin floor of black oxide.
pub fn surface_roughness(p: &RingParams) -> f32 {
    let roughness = match p.finish.get().id {
        "metal_polish" => 0.04,
        "turned" => 0.28,
        "cnc_0_8" => 0.36,
        "cnc_3_2" => 0.55,
        _ => p.finish.get().render_roughness(),
    };
    if p.material.name() == "steel_black_oxide" {
        roughness.max(0.35)
    } else {
        roughness
    }
}

/// Mass for the generated solid's volume in cubic millimetres.
pub fn ring_mass_grams(p: &RingParams, volume_mm3: f64) -> f32 {
    let density = if p.material.name() == "nickel_brass" {
        8.47_f32
    } else {
        p.material.get().density
    };
    (volume_mm3 * density as f64 / 1000.0) as f32
}

fn after_set(p: &mut RingParams, d: &ParamDesc<RingParams>) {
    if d.key == "thread_std" && p.thread_std() != ThreadStd::Custom {
        let thread = thread_data(p.thread_std());
        p.thread_d = thread.d;
        p.pitch = thread.pitch;
    } else if (d.key == "thread_d" || d.key == "pitch") && p.thread_std() != ThreadStd::Custom {
        let thread = thread_data(p.thread_std());
        if (thread.d - p.thread_d).abs() > 1e-3 || (thread.pitch - p.pitch).abs() > 1e-4 {
            p.thread_std = ThreadStd::Custom.index();
        }
    } else if d.key == "material" || d.key == "anodize" {
        p.colour = ring_colour(p);
    }
    p.colour.translucency = 0.0;
}

/// Fit measurements for the built ring, in millimetres except the thread count.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RingReport {
    pub outer_d: f64,
    pub top_z: f64,
    pub minor_d: f64,
    pub bore_d: f64,
    pub min_wall: f64,
    pub thread_length: f64,
    /// Number of turns engaging the bushing.
    pub engaged: f64,
    /// Bushing length beyond the panel.
    pub protrusion: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RingBuild {
    pub csg: Csg,
    pub report: RingReport,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

/// The shared inspector, serialization and sweep table, in display order.
pub fn param_table() -> &'static ParamTable<RingParams> {
    static TABLE: OnceLock<ParamTable<RingParams>> = OnceLock::new();
    TABLE.get_or_init(|| {
        macro_rules! row {
            ($builder:ident, $key:literal, $label:literal, $group:expr, $field:ident $(, $arg:expr)* $(,)?) => {{
                let (get, set) = field!(RingParams, $field);
                part_params::$builder($key, $label, $group, get, set $(, $arg)*)
            }};
        }
        let mut descs = Vec::new();
        let mut g = "Thread";
        descs.push(row!(enumeration, "thread_std", "Thread", g, thread_std, ThreadStd::NAMES)
            .notes(&THREAD_NOTES)
            .tip("The jack's bushing thread (ISO 261 coarse and fine pitches from M2 to M14, plus 3/8\"-32 UNEF). M11 x 1.25, M11 x 1 and 3/8\"-32 UNEF suit 1/4\" jack bushings. M6 x 0.5 suits 3.5 mm jacks."));
        descs.push(row!(float, "thread_d", "Major diameter", g, thread_d, 2.0, 20.0, "%.3f mm"));
        descs.push(row!(float, "pitch", "Pitch", g, pitch, 0.2, 2.0, "%.4f mm")
            .tip("Inch threads: 25.4 / TPI (32 TPI = 0.7938)."));
        descs.push(row!(enumeration, "thread_mode", "Bore", g, thread_mode, ThreadMode::NAMES)
            .tip("Modelled: the internal thread swept along its helix (ISO 68-1 basic profile), in the STL and the STEP.\nCosmetic: a plain bore at the minor diameter (tap it; CAD shows the thread as a note).\nClearance: a plain bore over the major diameter: a cover ring held by the jack's own nut or glue."));
        descs.push(row!(float, "clearance", "Clearance", g, clearance, 0.0, 0.4, "%.3f mm")
            .tip("Radial, over the basic profile. 0.03-0.08 turned and tapped; 0.15-0.2 for a printed or resin test ring."));
        descs.push(row!(float, "lead_in", "Lead-in", g, lead_in, 0.0, 2.0, "%.2f mm")
            .tip("45 deg countersink at both faces, from the minor diameter."));
        g = "Body";
        descs.push(row!(float, "diameter", "Diameter", g, diameter, 4.0, 40.0, "%.2f mm"));
        descs.push(row!(float, "height", "Height", g, height, 0.8, 15.0, "%.2f mm")
            .tip("Bottom face to the top (dome apex / step top)."));
        descs.push(row!(enumeration, "top_edge", "Top edge", g, top_edge, EdgeStyle::NAMES));
        descs.push(row!(float, "top_edge_size", "Top edge size", g, top_edge_size, 0.0, 6.0, "%.2f mm")
            .tip("Chamfer: width across the face. Round: radius."));
        descs.push(row!(float, "top_angle", "Bevel angle", g, top_angle, 10.0, 80.0, "%.0f deg")
            .tip("Chamfer angle from the top face: 45 = even, 30 = a nut's shallow chamfer, low = a wide flat bevel."));
        descs.push(row!(enumeration, "bottom_edge", "Bottom edge", g, bottom_edge, EdgeStyle::NAMES));
        descs.push(row!(float, "bottom_edge_size", "Bottom edge size", g, bottom_edge_size, 0.0, 3.0, "%.2f mm"));
        descs.push(row!(enumeration, "top_style", "Top", g, top_style, TopStyle::NAMES)
            .tip("Flat face, a spherical dome rising to the bore, or a raised boss round the bore."));
        descs.push(row!(float, "dome_height", "Dome height", g, dome_height, 0.1, 5.0, "%.2f mm")
            .tip("From the rim to the dome at the axis."));
        descs.push(row!(float, "step_diameter", "Step diameter", g, step_diameter, 3.0, 38.0, "%.2f mm"));
        descs.push(row!(float, "step_height", "Step height", g, step_height, 0.1, 6.0, "%.2f mm"));
        descs.push(row!(float, "step_edge", "Step edge", g, step_edge, 0.0, 2.0, "%.2f mm")
            .tip("Round on the boss's top edge."));
        g = "Wrench";
        descs.push(row!(enumeration, "grip", "Sides", g, grip, Grip::NAMES)
            .tip("Spanner flats (two), a hex, a straight or diamond knurl, flutes, or a plain round side (slots in the top or a pin spanner instead)."));
        descs.push(row!(float, "across_flats", "Across flats", g, across_flats, 3.5, 40.0, "%.2f mm")
            .tip("Flats / hex: spanner size."));
        descs.push(row!(int, "ribs", "Count", g, ribs, 6, 120).tip("Knurl teeth or flutes around."));
        descs.push(row!(float, "grip_depth", "Depth", g, grip_depth, 0.05, 2.0, "%.2f mm"));
        descs.push(row!(float, "knurl_angle", "Knurl angle", g, knurl_angle, 15.0, 60.0, "%.0f deg")
            .tip("Diamond knurl: helix angle from the axis (30 = DIN 82 RGE 30)."));
        descs.push(row!(float, "grip_from", "From", g, grip_from, 0.0, 1.0, "%.2f")
            .tip("Knurl / flutes band, fraction of the side height."));
        descs.push(row!(float, "grip_to", "To", g, grip_to, 0.0, 1.0, "%.2f")
            .tip("1 = runs out into the top edge."));
        descs.push(row!(int, "slots", "Top slots", g, slots, 0, 6)
            .tip("Spanner slots across the top face (a ring-nut spanner). 0 = none."));
        descs.push(row!(float, "slot_width", "Slot width", g, slot_width, 0.3, 4.0, "%.2f mm"));
        descs.push(row!(float, "slot_depth", "Slot depth", g, slot_depth, 0.1, 4.0, "%.2f mm").tip("From the top."));
        g = "Flange";
        descs.push(row!(boolean, "flange", "Flange", g, flange)
            .tip("A washer lip at the bottom, wider than the body."));
        descs.push(row!(float, "flange_diameter", "Flange diameter", g, flange_diameter, 6.0, 50.0, "%.2f mm"));
        descs.push(row!(float, "flange_thickness", "Flange thickness", g, flange_thickness, 0.2, 4.0, "%.2f mm"));
        descs.push(row!(float, "flange_edge", "Flange edge", g, flange_edge, 0.0, 2.0, "%.2f mm").tip("Round on its top edge."));
        g = "Fit";
        descs.push(row!(float, "panel_thickness", "Panel", g, panel_thickness, 0.5, 6.0, "%.2f mm")
            .tip("The panel the ring clamps (2 mm aluminium)."));
        descs.push(row!(float, "bushing_length", "Bushing length", g, bushing_length, 1.0, 20.0, "%.2f mm")
            .tip("Thread length out of the jack's front face: what is left beyond the panel is what the ring grips."));
        g = "Look";
        descs.push(row!(material, "material", "Metal", g, material,
            Some(|m| m.family == Family::Metal && m.processes.contains(Processes::CNC)), None, &[])
            .tip("Density (mass), natural colour and linear reflectance."));
        descs.push(row!(enumeration, "anodize", "Anodising", g, anodize, Anodize::NAMES)
            .tip("Aluminium: type II anodising, dyed."));
        descs.push(row!(finish, "finish", "Surface", g, finish,
            Some(|f| f.standard == FinishStandard::Machined), &[])
            .tip("Polished metal, turned, CNC fine finish for brushed, CNC standard for bead blasted."));
        descs.push(part_params::colour("color", "Colour", g,
            |p: &RingParams| p.colour, |p, colour| p.set_colour(colour)));
        descs.push(row!(int, "segments", "Segments", g, segments, 48, 720)
            .tip("Points around. Raised to fit the knurl / flutes and the thread's profile."));
        ParamTable { descs, header: "# Jack ring", after_set: Some(after_set), legacy: None }
    })
}
