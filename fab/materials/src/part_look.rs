//! Resolve a part mesh file's material factors and placement to Fab's material rows.
//! A part file's per-material override, when present, wins over this table; this table wins over
//! the unspecified body. This is pure classification, with no I/O or electronics dependencies.
//! Cost: a few comparisons per file material, run where the caller builds a body upload, never
//! per frame. Material row ids are looked up once and retained for subsequent resolutions.

use crate::{Colour, MaterialId, Surface};
use fab_colour::{lab::srgb_to_lab, oklab::to_srgb};
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PartClass {
    Ic, Transistor, Diode, Led, Resistor, Capacitor, Inductor, Crystal, Connector, Switch, Fuse, Other,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// Material bounds in body millimetres, Z up from the board surface.
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    /// Bounds of the whole mesh in the same frame.
    pub body_lo: [f32; 3],
    pub body_hi: [f32; 3],
    /// The material's triangle area divided by the mesh's, 0..1.
    pub area_share: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileMaterial<'a> {
    pub name: &'a str,
    /// Linear RGB from the file.
    pub base: [f32; 3],
    pub alpha: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub place: Place,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rule { Marking, TopCoat, Lens, Metal, Body, Fallback }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    pub surface: Surface,
    pub rule: Rule,
}

/// Separates metal greys from darker body colours.
const METAL_L_MIN: f32 = 60.0;
/// Separates metal greys from white ceramic and plastic.
const METAL_L_MAX: f32 = 90.0;
/// Separates neutral metal greys and whites from coloured surfaces.
const NEUTRAL_CHROMA_MAX: f32 = 12.0;
/// Separates saturated gold from neutral surfaces.
const GOLD_CHROMA_MIN: f32 = 25.0;
/// Separates gold from redder colours in Lab hue degrees.
const GOLD_HUE_MIN: f32 = 60.0;
/// Separates gold from greener colours in Lab hue degrees.
const GOLD_HUE_MAX: f32 = 95.0;
/// Separates gold from dark brown surfaces.
const GOLD_L_MIN: f32 = 50.0;
/// Separates pin yellow from gold with unequal red and green channels.
const PIN_YELLOW_RG_SPREAD: f32 = 0.08;
/// Separates pin yellow from darker colours in its red and green channels.
const PIN_YELLOW_RG_MIN: f32 = 0.45;
/// Separates pin yellow from colours with a larger blue share.
const PIN_YELLOW_B_SHARE: f32 = 0.55;
/// Separates white ceramic and plastic from grey surfaces.
const WHITE_L_MIN: f32 = 90.0;
/// Separates dark moulded bodies from lighter surfaces.
const DARK_L_MAX: f32 = 40.0;
/// Separates dark neutral bodies from saturated colours.
const DARK_CHROMA_MAX: f32 = 15.0;
/// Separates markings and coatings from solid material volumes, in mm.
const THIN_MM: f32 = 0.03;
/// Separates materials touching the seating plane from raised materials, in mm.
const SEAT_MM: f32 = 0.04;
/// Separates pins and tabs through the board from surfaces above it, in mm.
const BELOW_MM: f32 = -0.05;
/// Separates materials reaching the body's XY rim from inset materials, in mm.
const RIM_MM: f32 = 0.05;
/// Separates surfaces reaching the body's top from lower pieces, in mm.
const TOP_MM: f32 = 0.05;
/// Separates shells and cans spanning the body from smaller contacts.
const SPAN_MIN: f32 = 0.6;
/// Separates square-footprint cans from long chip parts.
const CAN_ASPECT_MAX: f32 = 1.2;
/// Separates resistor top coats from short top markings along X.
const TOP_COAT_X_MIN: f32 = 0.5;
/// Separates transparent LED lenses from opaque or diffused pieces.
const LENS_ALPHA_MAX: f32 = 0.95;
/// Separates a file's metallic hint from its dielectric factors.
const METALLIC_MIN: f32 = 0.35;
/// Separates corroborated metal evidence from a single ambiguous hint.
const METAL_SIGNALS_MIN: usize = 2;
/// Separates pale substrates and housings from other body colours.
const PALE_L_MIN: f32 = 80.0;

fn chroma(lab: [f32; 3]) -> f32 { lab[1].hypot(lab[2]) }
fn hue(lab: [f32; 3]) -> f32 { lab[2].atan2(lab[1]).to_degrees() }
fn metal_grey(l: f32, c: f32) -> bool {
    (METAL_L_MIN..=METAL_L_MAX).contains(&l) && c <= NEUTRAL_CHROMA_MAX
}
fn gold(l: f32, c: f32, h: f32) -> bool {
    c >= GOLD_CHROMA_MIN && (GOLD_HUE_MIN..=GOLD_HUE_MAX).contains(&h) && l >= GOLD_L_MIN
}
fn pin_yellow([r, g, b]: [f32; 3]) -> bool {
    (r - g).abs() <= PIN_YELLOW_RG_SPREAD && r >= PIN_YELLOW_RG_MIN && g >= PIN_YELLOW_RG_MIN
        && b <= PIN_YELLOW_B_SHARE * r.min(g)
}
fn white(l: f32, c: f32) -> bool { l > WHITE_L_MIN && c <= NEUTRAL_CHROMA_MAX }
fn dark(l: f32, c: f32) -> bool { l < DARK_L_MAX && c <= DARK_CHROMA_MAX }
fn extents(lo: [f32; 3], hi: [f32; 3]) -> [f32; 3] { [0, 1, 2].map(|i| hi[i] - lo[i]) }
fn thin(ext: [f32; 3]) -> bool { ext[0].min(ext[1]).min(ext[2]) < THIN_MM }
fn at_seat(p: &Place) -> bool { p.lo[2] < p.body_lo[2] + SEAT_MM }
fn below(p: &Place) -> bool { p.lo[2] < BELOW_MM }
fn at_rim(p: &Place) -> bool {
    (0..2).any(|i| p.lo[i] <= p.body_lo[i] + RIM_MM || p.hi[i] >= p.body_hi[i] - RIM_MM)
}
fn on_top(p: &Place) -> bool { p.lo[2] >= p.body_hi[2] - TOP_MM }
fn reaches_top(p: &Place) -> bool { p.hi[2] >= p.body_hi[2] - TOP_MM }
fn extent_share(ext: [f32; 3], body_ext: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| if body_ext[i] > 0.0 { ext[i] / body_ext[i] } else { 0.0 })
}
fn spans(share: [f32; 3]) -> bool { share[0] >= SPAN_MIN && share[1] >= SPAN_MIN }
fn can_shaped(p: &Place) -> bool {
    let ext = extents(p.body_lo, p.body_hi);
    ext[0].max(ext[1]) / ext[0].min(ext[1]) <= CAN_ASPECT_MAX
}
fn has_token(name: &str, tokens: &[&str]) -> bool {
    name.split(|c: char| !c.is_alphanumeric()).any(|token| tokens.contains(&token))
}
fn name_gold(name: &str) -> bool { has_token(name, &["gold", "au"]) }
fn name_metal(name: &str) -> bool {
    has_token(name, &["metal", "met", "pin", "pins", "lead", "leads", "tin", "silver", "nickel",
        "steel", "shield", "terminal", "contact", "plating"])
}
fn name_marking(name: &str) -> bool { has_token(name, &["label", "marking", "text", "logo"]) }
fn name_lens(name: &str) -> bool { has_token(name, &["lens", "glass", "clear", "dome"]) }

fn row(id: &str) -> MaterialId {
    MaterialId::lookup(id).unwrap_or_else(|| panic!("part look material row is missing: {id}"))
}

struct Rows {
    tin_lead: MaterialId,
    gold_contact: MaterialId,
    nickel_brass: MaterialId,
    aluminium_can: MaterialId,
    epoxy_mold: MaterialId,
    ceramic_mlcc: MaterialId,
    alumina: MaterialId,
    thick_film_top: MaterialId,
    lcp_housing: MaterialId,
    pbt_housing: MaterialId,
    silk_epoxy: MaterialId,
    silicone_lens: MaterialId,
    epoxy_lens_diffused: MaterialId,
    unspecified_body: MaterialId,
}

static ROWS: LazyLock<Rows> = LazyLock::new(|| Rows {
    tin_lead: row("tin_lead"),
    gold_contact: row("gold_contact"),
    nickel_brass: row("nickel_brass"),
    aluminium_can: row("aluminium_can"),
    epoxy_mold: row("epoxy_mold"),
    ceramic_mlcc: row("ceramic_mlcc"),
    alumina: row("alumina"),
    thick_film_top: row("thick_film_top"),
    lcp_housing: row("lcp_housing"),
    pbt_housing: row("pbt_housing"),
    silk_epoxy: row("silk_epoxy"),
    silicone_lens: row("silicone_lens"),
    epoxy_lens_diffused: row("epoxy_lens_diffused"),
    unspecified_body: row("unspecified_body"),
});

fn dielectric(material: MaterialId, srgb: [f32; 3], alpha: f32, rule: Rule) -> Resolved {
    let mut surface = Surface::new(material);
    let row = material.get();
    let translucency = if row.translucency_allowed() { 1.0 - alpha } else { surface.colour.translucency };
    surface.colour = Colour::srgb(row.clamp_colour(srgb)).with_translucency(translucency);
    Resolved { surface, rule }
}

/// Resolve the first matching rule, retaining file colour only for dielectric rows.
pub fn resolve(m: &FileMaterial<'_>, class: PartClass) -> Resolved {
    let rows = &*ROWS;
    let srgb = m.base.map(to_srgb);
    let lab = srgb_to_lab(srgb);
    let (l, c, h) = (lab[0], chroma(lab), hue(lab));
    let metal_grey = metal_grey(l, c);
    let pin_yellow = pin_yellow(srgb);
    let gold = !pin_yellow && gold(l, c, h);
    let white = white(l, c);
    let dark = dark(l, c);
    let p = &m.place;
    let ext = extents(p.lo, p.hi);
    let body_ext = extents(p.body_lo, p.body_hi);
    let share = extent_share(ext, body_ext);
    let thin = thin(ext);
    let at_seat = at_seat(p);
    let below = below(p);
    let name = m.name.to_lowercase();
    let name_gold = name_gold(&name);
    let housing = matches!(class, PartClass::Connector | PartClass::Switch);

    if class == PartClass::Resistor && thin && on_top(p) && dark && share[0] >= TOP_COAT_X_MIN {
        return dielectric(rows.thick_film_top, srgb, m.alpha, Rule::TopCoat);
    }
    if name_marking(&name) || (thin && !at_seat) {
        return dielectric(rows.silk_epoxy, srgb, m.alpha, Rule::Marking);
    }
    if class == PartClass::Led
        && (name_lens(&name) || m.alpha < LENS_ALPHA_MAX || ((white || metal_grey) && reaches_top(p)))
    {
        let material = if m.alpha < LENS_ALPHA_MAX { rows.silicone_lens } else { rows.epoxy_lens_diffused };
        return dielectric(material, srgb, m.alpha, Rule::Lens);
    }
    if class == PartClass::Capacitor && !can_shaped(p) && !thin && at_seat && reaches_top(p) {
        let axis = if body_ext[0] >= body_ext[1] { 0 } else { 1 };
        if p.lo[axis] > p.body_lo[axis] + RIM_MM && p.hi[axis] < p.body_hi[axis] - RIM_MM {
            return dielectric(rows.ceramic_mlcc, srgb, m.alpha, Rule::Body);
        }
    }
    let signals = [name_metal(&name) || name_gold, m.metallic >= METALLIC_MIN,
        metal_grey || gold || pin_yellow, (at_seat && at_rim(p)) || below];
    if signals.into_iter().filter(|signal| *signal).count() >= METAL_SIGNALS_MIN {
        let material = if gold || name_gold {
            if class == PartClass::Connector || name_gold { rows.gold_contact } else { rows.tin_lead }
        } else if housing && (below || spans(share)) {
            rows.nickel_brass
        } else if class == PartClass::Capacitor && spans(share) && can_shaped(p) {
            rows.aluminium_can
        } else if class == PartClass::Crystal && spans(share) {
            rows.nickel_brass
        } else {
            rows.tin_lead
        };
        return Resolved { surface: Surface::new(material), rule: Rule::Metal };
    }
    let material = if dark {
        Some(if housing { rows.lcp_housing } else { rows.epoxy_mold })
    } else if white || l >= PALE_L_MIN {
        match class {
            PartClass::Resistor => Some(rows.alumina),
            PartClass::Capacitor => Some(rows.ceramic_mlcc),
            PartClass::Led | PartClass::Connector | PartClass::Switch => Some(rows.pbt_housing),
            _ => None,
        }
    } else if housing {
        Some(rows.pbt_housing)
    } else if class == PartClass::Capacitor {
        Some(rows.ceramic_mlcc)
    } else {
        None
    };
    match material {
        Some(material) => dielectric(material, srgb, m.alpha, Rule::Body),
        None => dielectric(rows.unspecified_body, srgb, m.alpha, Rule::Fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_colour::oklab::to_linear;

    #[test]
    fn supplier_part_materials() {
        let file = |srgb: [f32; 3], metallic, lo, hi, (body_lo, body_hi)| FileMaterial {
            name: "1", base: srgb.map(to_linear), alpha: 1.0, metallic, roughness: 0.5,
            place: Place { lo, hi, body_lo, body_hi, area_share: 0.5 },
        };
        let connector = ([-4.47, -5.05, -0.85], [4.47, 2.85, 3.25]);
        let shell_box = (connector.0, [4.47, 2.25, 3.25]);
        let qfn = ([-3.51, -3.51, 0.0], [3.51, 3.51, 0.87]);
        let resistor = ([-0.81, -0.41, 0.0], [0.81, 0.41, 0.45]);
        let chip_capacitor = ([-0.50, -0.25, 0.0], [0.50, 0.25, 0.50]);
        let chip_capacitor_0603 = ([-0.80, -0.40, 0.0], [0.80, 0.40, 0.80]);
        let electrolytic = ([-2.15, -2.15, 0.0], [2.15, 2.15, 5.40]);
        let pin_header = ([-3.17, -1.30, -2.50], [3.17, 1.30, 5.60]);
        let led = ([-0.81, -0.40, 0.0], [0.81, 0.40, 0.71]);
        let led_0603 = ([-0.81, -0.41, 0.0], [0.81, 0.41, 0.56]);
        let sot = ([-1.25, -1.45, 0.0], [1.25, 1.45, 1.05]);
        let generic = ([0.0; 3], [2.0, 2.0, 1.0]);
        let shell = file([0.85; 3], 1.0, shell_box.0, shell_box.1, shell_box);
        let housing = file([0.18; 3], 0.0, [-4.17, -4.75, -0.60], [4.17, 2.25, 2.95], connector);
        let lens = file([0.925; 3], 0.0, [-0.55, -0.40, 0.01], [0.55, 0.40, 0.71], led);
        let ceramic_body = file([0.59, 0.46, 0.0], 0.0, [-0.28, -0.25, 0.01], [0.28, 0.25, 0.49], chip_capacitor);
        let cases = [
            ("connector shell", shell, PartClass::Connector, Some("nickel_brass"), Rule::Metal),
            ("connector contacts", file([0.95, 0.76, 0.18], 1.0, [-3.42, -4.30, 0.0], [3.42, 2.85, 1.96], connector), PartClass::Connector, Some("gold_contact"), Rule::Metal),
            ("connector housing", housing, PartClass::Connector, Some("lcp_housing"), Rule::Body),
            ("connector top text", file([0.0; 3], 0.0, [-1.49, -4.22, 3.25], [1.75, -3.21, 3.25], connector), PartClass::Connector, Some("silk_epoxy"), Rule::Marking),
            ("pin header pins through the board", file([0.50, 0.50, 0.25], 0.0, [-2.74, -0.20, -2.50], [2.74, 0.20, 5.60], pin_header), PartClass::Connector, Some("nickel_brass"), Rule::Metal),
            ("QFN body", file([0.25; 3], 0.0, [-3.5, -3.5, 0.02], [3.5, 3.5, 0.87], qfn), PartClass::Ic, Some("epoxy_mold"), Rule::Body),
            ("QFN pins", file([1.0, 1.0, 0.0], 1.0, qfn.0, [3.51, 3.51, 0.22], qfn), PartClass::Ic, Some("tin_lead"), Rule::Metal),
            ("resistor body", file([1.0; 3], 0.0, [-0.81, -0.40, 0.0], [0.81, 0.41, 0.45], resistor), PartClass::Resistor, Some("alumina"), Rule::Body),
            ("resistor ends", file([0.75; 3], 1.0, [-0.80, -0.40, 0.01], [0.80, 0.41, 0.44], resistor), PartClass::Resistor, Some("tin_lead"), Rule::Metal),
            ("resistor top coat", file([0.25; 3], 0.0, [-0.54, -0.40, 0.44], [0.54, 0.41, 0.45], resistor), PartClass::Resistor, Some("thick_film_top"), Rule::TopCoat),
            ("resistor side text", file([0.0; 3], 0.0, [-0.74, -0.41, 0.24], [-0.24, -0.40, 0.39], resistor), PartClass::Resistor, Some("silk_epoxy"), Rule::Marking),
            ("chip capacitor ceramic body", ceramic_body, PartClass::Capacitor, Some("ceramic_mlcc"), Rule::Body),
            ("chip capacitor terminations", file([0.86; 3], 0.0, chip_capacitor.0, chip_capacitor.1, chip_capacitor), PartClass::Capacitor, Some("tin_lead"), Rule::Metal),
            ("0603 chip capacitor ceramic body", file([0.59, 0.46, 0.0], 0.0, [-0.55, -0.40, 0.0], [0.55, 0.40, 0.80], chip_capacitor_0603), PartClass::Capacitor, Some("ceramic_mlcc"), Rule::Body),
            ("electrolytic can", file([0.86; 3], 0.0, electrolytic.0, electrolytic.1, electrolytic), PartClass::Capacitor, Some("aluminium_can"), Rule::Metal),
            ("LED ends", file([0.83, 0.67, 0.13], 1.0, [-0.81, -0.35, 0.0], [0.81, 0.35, 0.32], led), PartClass::Led, Some("tin_lead"), Rule::Metal),
            ("LED terminals at the seat", file([0.76, 0.76, 0.0], 0.0, led_0603.0, [0.81, 0.41, 0.20], led_0603), PartClass::Led, Some("tin_lead"), Rule::Metal),
            ("LED lens", lens, PartClass::Led, Some("epoxy_lens_diffused"), Rule::Lens),
            ("LED body", file([1.0; 3], 0.0, [-0.80, -0.40, 0.01], [0.80, 0.40, 0.31], led), PartClass::Led, Some("pbt_housing"), Rule::Body),
            ("SOT-23 leads", file([0.77; 3], 1.0, [-1.25, -1.15, 0.0], [1.25, 1.15, 0.60], sot), PartClass::Transistor, Some("tin_lead"), Rule::Metal),
            ("SOT-23 body", file([0.25; 3], 0.0, [-0.65, -1.45, 0.05], [0.65, 1.45, 1.05], sot), PartClass::Transistor, Some("epoxy_mold"), Rule::Body),
            ("one signal only: grey plastic", file([0.75; 3], 0.0, [0.0, 0.0, 0.5], [1.0; 3], generic), PartClass::Other, None, Rule::Fallback),
            ("one signal only: metallic factor", file([0.30, 0.20, 0.60], 0.9, [0.0, 0.0, 0.5], [1.0; 3], generic), PartClass::Ic, None, Rule::Fallback),
            ("named pin", FileMaterial { name: "PIN-01", ..file([0.6; 3], 1.0, [0.0; 3], [2.0, 0.3, 0.5], generic) }, PartClass::Other, Some("tin_lead"), Rule::Metal),
        ];
        for (name, material, class, expected, rule) in cases {
            let resolved = resolve(&material, class);
            if let Some(expected) = expected {
                assert_eq!(resolved.surface.material.name(), expected, "{name}: material row");
            } else {
                assert!(resolved.surface.material.get().f0.is_none(), "{name}: one signal cannot select any metal row");
            }
            assert_eq!(resolved.rule, rule, "{name}: winning rule");
        }
        let resolved = resolve(&housing, PartClass::Connector).surface;
        assert_eq!(resolved.colour, Colour::srgb(resolved.material.get().clamp_colour(housing.base.map(to_srgb))),
            "connector housing: dielectric keeps the clamped file colour");
        let resolved = resolve(&ceramic_body, PartClass::Capacitor).surface;
        assert_eq!(resolved.colour, Colour::srgb(resolved.material.get().clamp_colour(ceramic_body.base.map(to_srgb))),
            "chip capacitor ceramic body: dielectric keeps the clamped file colour");
        assert_eq!(resolve(&shell, PartClass::Connector).surface, Surface::new(ROWS.nickel_brass),
            "connector shell: metal keeps the row's colour and finish");
        let translucent = FileMaterial { alpha: 0.5, ..lens };
        let resolved = resolve(&translucent, PartClass::Led);
        assert_eq!((resolved.surface.material.name(), resolved.rule), ("silicone_lens", Rule::Lens),
            "translucent LED lens: silicone row and lens rule");
        assert_eq!(resolved.surface.colour, Colour::srgb(ROWS.silicone_lens.get().clamp_colour(translucent.base.map(to_srgb))).with_translucency(0.5),
            "translucent LED lens: file colour and translucency 0.5");
    }
}
