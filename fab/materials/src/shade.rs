//! The values a shader reads for a surface and for a board's layered look. The realtime viewport
//! and the tracer's scene export both call these functions, so the two pictures agree.

use crate::swatch::{Swatch, SWATCHES};
use crate::{Colour, FinishId, MaterialId, Srgb, Surface};
use fab_colour::lab::{delta_e2000, srgb_to_lab};
use fab_colour::oklab::to_linear;

/// One surface's shading values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shade {
    /// Linear base colour: a dielectric's albedo, a metal's reflectance.
    pub base: [f32; 3],
    /// GGX perceptual roughness.
    pub roughness: f32,
    /// 1 for a metal, 0 for a dielectric.
    pub metal: f32,
    /// Linear reflectance at normal incidence: a metal's own, a dielectric's from its index.
    pub f0: [f32; 3],
    /// 0 opaque, 1 clear.
    pub translucency: f32,
}

/// The material a scene body without a surface is drawn in.
pub fn unspecified() -> Surface {
    Surface::new(MaterialId::lookup("unspecified_body").expect("the unspecified body row is in the table"))
}

/// A surface's shading values; they read the same roughness and reflectance as its [`Surface::medium`].
pub fn shade(surface: &Surface) -> Shade {
    let medium = surface.medium();
    match medium.f0 {
        Some(f0) => Shade { base: f0, roughness: medium.roughness, metal: 1.0, f0, translucency: 0.0 },
        None => {
            let f0 = ((medium.ior - 1.0) / (medium.ior + 1.0)).powi(2);
            Shade {
                base: surface.colour.srgb.map(to_linear),
                roughness: medium.roughness,
                metal: 0.0,
                f0: [f0; 3],
                translucency: surface.colour.translucency.clamp(0.0, 1.0),
            }
        }
    }
}

/// A scene body's shading values: its surface's, or the unspecified body's when it has none.
pub fn shade_body(surface: Option<&Surface>) -> Shade {
    match surface {
        Some(surface) => shade(surface),
        None => shade(&unspecified()),
    }
}

/// A thin coating over the board (solder mask, legend ink).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoatLook {
    /// Linear colour of the coat.
    pub colour: [f32; 3],
    /// 0 opaque, 1 clear: how much of what lies under it shows through.
    pub translucency: f32,
    /// How much lighter and more saturated the coat reads over copper (1: no change).
    pub lift: f32,
    /// GGX perceptual roughness of the coat's surface.
    pub roughness: f32,
    /// Linear reflectance of the coat's surface at normal incidence.
    pub f0: f32,
    /// Mean free path of the coat's bulk scattering, mm; 0 for none.
    pub scatter_mm: f32,
    /// Thickness over copper and over laminate, mm.
    pub thickness_mm: [f32; 2],
}

/// The layered board's look: mask over copper and laminate, exposed copper in its finish, legend on
/// top, and the laminate at the edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardLook {
    pub mask: CoatLook,
    pub silk: CoatLook,
    /// Exposed copper and plated barrels in the board's surface finish.
    pub copper: Shade,
    pub laminate: Shade,
}

/// The solder mask colour preset nearest a picked colour (CIEDE2000): its translucency, lift and
/// default finish apply to the picked colour.
pub fn mask_preset(srgb: Srgb) -> &'static Swatch {
    let lab = srgb_to_lab(srgb);
    SWATCHES
        .iter()
        .filter(|s| s.family == "Solder mask")
        .min_by(|a, b| delta_e2000(lab, srgb_to_lab(a.colour.srgb)).total_cmp(&delta_e2000(lab, srgb_to_lab(b.colour.srgb))))
        .expect("the swatch table has solder mask colours")
}

fn coat(material: &str, colour: Colour, finish: Option<FinishId>, lift: f32) -> CoatLook {
    let m = MaterialId::lookup(material).expect("board coating rows are in the table");
    let mut surface = Surface::new(m);
    surface.colour = colour;
    if let Some(finish) = finish {
        surface.finish = finish;
    }
    let shade = shade(&surface);
    let medium = surface.medium();
    let coat = m.get().coat_um;
    CoatLook {
        colour: shade.base,
        translucency: shade.translucency,
        lift,
        roughness: shade.roughness,
        f0: shade.f0[0],
        scatter_mm: medium.scatter_distance_mm,
        thickness_mm: [coat.lo / 1000.0, coat.hi / 1000.0],
    }
}

/// The look of a board from its copper finish and laminate rows and its mask and legend colours.
pub fn board_look(copper: MaterialId, laminate: MaterialId, mask: Srgb, silk: Srgb) -> BoardLook {
    let preset = mask_preset(mask);
    let mask_finish = preset.finish.and_then(FinishId::lookup);
    BoardLook {
        mask: coat("lpi_mask", Colour::srgb(mask).with_translucency(preset.colour.translucency), mask_finish, preset.lift),
        silk: coat("silk_epoxy", Colour::srgb(silk), None, 1.0),
        copper: shade(&Surface::new(copper)),
        laminate: shade(&Surface::new(laminate)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metals, dielectrics, the unspecified body and a board's layered look resolve from the table.
    #[test]
    fn shading_values() {
        let id = |name| MaterialId::lookup(name).unwrap();
        let cu = shade(&Surface::new(id("cu_bare")));
        assert_eq!((cu.metal, cu.base, cu.f0), (1.0, [0.955, 0.638, 0.538], [0.955, 0.638, 0.538]), "a metal shades by its f0");
        assert!((cu.roughness - 0.30).abs() < 1e-6 && cu.translucency == 0.0, "a metal is as rough as its row, and opaque");
        let pc = shade(&Surface::new(id("pc")));
        assert!(pc.metal == 0.0 && (pc.f0[0] - 0.0513).abs() < 1e-3, "a dielectric's f0 comes from its index: {:?}", pc.f0);
        assert!(pc.translucency > 0.8, "clear polycarbonate is clear");
        assert_eq!(shade_body(None), shade(&unspecified()), "no surface is the unspecified body");
        let none = shade_body(None);
        assert!(none.metal == 0.0 && none.translucency == 0.0 && none.roughness > 0.2, "the unspecified body is an opaque dielectric");

        let green = Colour::hex(0x1E6B3A).srgb;
        assert_eq!(mask_preset(green).name, "Mask green", "a preset colour finds itself");
        assert_eq!(mask_preset([0.05, 0.05, 0.05]).name, "Mask black", "a near-black mask is the black preset");
        let look = board_look(id("enig"), id("fr4"), green, Colour::hex(0xF0F0EA).srgb);
        assert!((look.mask.lift - 1.6).abs() < 1e-6 && (look.mask.translucency - 0.55).abs() < 1e-6, "the mask takes its preset's lift and translucency");
        assert!((look.mask.roughness - 0.18).abs() < 0.01 && (look.mask.f0 - 0.043).abs() < 1e-3, "gloss mask: roughness and f0");
        let [over_copper, over_laminate] = look.mask.thickness_mm;
        assert!((look.mask.scatter_mm - 0.08).abs() < 1e-6 && (over_copper - 0.02).abs() < 1e-6 && (over_laminate - 0.03).abs() < 1e-6, "mask bulk and thickness: {:?}", look.mask.thickness_mm);
        assert_eq!(look.copper.metal, 1.0, "exposed copper is metal");
        assert!(look.laminate.metal == 0.0 && (look.laminate.translucency - 0.35).abs() < 1e-6, "FR-4 is a translucent dielectric");
        assert!(look.silk.translucency == 0.0 && (look.silk.roughness - 0.65).abs() < 1e-6, "legend ink is opaque and as rough as its row");
        let black = board_look(id("enig"), id("fr4"), Colour::hex(0x141414).srgb, [1.0; 3]);
        assert!((black.mask.roughness - 0.45).abs() < 0.01, "a black mask comes matte");
    }
}
