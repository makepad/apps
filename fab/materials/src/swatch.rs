//! Named generic swatches for plastic parts, translucent ones included.

use crate::ral::{find_ral, RalColour};
use crate::{Colour, Srgb};

/// A named colour in a palette family.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Swatch {
    pub name: &'static str,
    pub family: &'static str,
    pub colour: Colour,
    /// How much lighter and more saturated a coating colour reads over copper, which reflects back
    /// through it (1 for no change, and for every colour that is not a coating).
    pub lift: f32,
    /// The finish id a coating colour comes in by default (solder mask gloss or matte).
    pub finish: Option<&'static str>,
    /// Render roughness for translucent print swatches; other families keep the part's roughness.
    pub roughness: Option<f32>,
}

const fn entry(name: &'static str, family: &'static str, hex: u32, translucency: f32) -> Swatch {
    Swatch {
        name,
        family,
        colour: Colour::hex(hex).with_translucency(translucency),
        lift: 1.0,
        finish: None,
        roughness: None,
    }
}

const fn translucent(name: &'static str, hex: u32, translucency: f32, roughness: f32) -> Swatch {
    Swatch { roughness: Some(roughness), ..entry(name, "Translucent", hex, translucency) }
}

const fn mask(name: &'static str, hex: u32, translucency: f32, lift: f32, finish: &'static str) -> Swatch {
    Swatch { lift, finish: Some(finish), ..entry(name, "Solder mask", hex, translucency) }
}

const fn anodising(name: &'static str, srgb: Srgb) -> Swatch {
    Swatch { colour: Colour::srgb(srgb), ..entry(name, "Anodising", 0, 0.0) }
}

/// Generic swatches, grouped by family.
pub static SWATCHES: &[Swatch] = &[
    entry("Black", "Neutrals", 0x1A1A1C, 0.0),
    entry("Jet", "Neutrals", 0x0E0E10, 0.0),
    entry("Graphite", "Neutrals", 0x43454A, 0.0),
    entry("Slate", "Neutrals", 0x5A6068, 0.0),
    entry("Ash", "Neutrals", 0x8A8D91, 0.0),
    entry("Light grey", "Neutrals", 0x9EA1A4, 0.0),
    entry("Silver grey", "Neutrals", 0xC3C5C7, 0.0),
    entry("Off-white", "Neutrals", 0xE8E6E0, 0.0),
    entry("White", "Neutrals", 0xF4F3EF, 0.0),
    entry("Cream", "Neutrals", 0xE8DCBC, 0.0),
    entry("Bone", "Neutrals", 0xDDD3BD, 0.0),
    entry("Beige", "Neutrals", 0xCDBB9B, 0.0),
    entry("Signal red", "Reds & oranges", 0xD02F2A, 0.0),
    entry("Red", "Reds & oranges", 0xC81E1E, 0.0),
    entry("Cherry", "Reds & oranges", 0x9B1B30, 0.0),
    entry("Crimson", "Reds & oranges", 0x7A1525, 0.0),
    entry("Coral", "Reds & oranges", 0xF2645A, 0.0),
    entry("Salmon", "Reds & oranges", 0xF08A75, 0.0),
    entry("Orange", "Reds & oranges", 0xF2731A, 0.0),
    entry("Tangerine", "Reds & oranges", 0xFF8C1A, 0.0),
    entry("Burnt orange", "Reds & oranges", 0xCC5500, 0.0),
    entry("Amber", "Yellows", 0xF5A623, 0.0),
    entry("Harvest gold", "Yellows", 0xDA9A2B, 0.0),
    entry("Mustard", "Yellows", 0xD4A017, 0.0),
    entry("Yellow", "Yellows", 0xF2CC26, 0.0),
    entry("Lemon", "Yellows", 0xF5E050, 0.0),
    entry("Cream yellow", "Yellows", 0xF3E5AB, 0.0),
    entry("Lime", "Greens", 0x9CCB3B, 0.0),
    entry("Green", "Greens", 0x2E9E4F, 0.0),
    entry("Forest", "Greens", 0x1F4D2E, 0.0),
    entry("Avocado", "Greens", 0x8A8B3A, 0.0),
    entry("Olive", "Greens", 0x6B7038, 0.0),
    entry("Mint", "Greens", 0x8CD1B3, 0.0),
    entry("Sage", "Greens", 0x9CAF88, 0.0),
    entry("Teal", "Blues", 0x0D8085, 0.0),
    entry("Petrol", "Blues", 0x15505A, 0.0),
    entry("Aqua", "Blues", 0x4FB8B0, 0.0),
    entry("Cyan", "Blues", 0x7FD9E6, 0.0),
    entry("Sky", "Blues", 0x6BB6E8, 0.0),
    entry("Blue", "Blues", 0x1A5CBF, 0.0),
    entry("Cobalt", "Blues", 0x0047AB, 0.0),
    entry("Royal", "Blues", 0x2747A8, 0.0),
    entry("Navy", "Blues", 0x1A2440, 0.0),
    entry("Steel blue", "Blues", 0x4A6782, 0.0),
    entry("Magenta", "Purples & pinks", 0xE6007E, 0.0),
    entry("Hot pink", "Purples & pinks", 0xFF3F9A, 0.0),
    entry("Pink", "Purples & pinks", 0xF2A0C0, 0.0),
    entry("Rose", "Purples & pinks", 0xD96C8C, 0.0),
    entry("Purple", "Purples & pinks", 0x6B3D99, 0.0),
    entry("Violet", "Purples & pinks", 0x8A5CD1, 0.0),
    entry("Lavender", "Purples & pinks", 0xB9A6E0, 0.0),
    entry("Plum", "Purples & pinks", 0x5E2750, 0.0),
    entry("Pastel pink", "Pastels", 0xF7C6D0, 0.0),
    entry("Peach", "Pastels", 0xF9C9A8, 0.0),
    entry("Butter", "Pastels", 0xF6E7A1, 0.0),
    entry("Pistachio", "Pastels", 0xC8E0B0, 0.0),
    entry("Seafoam", "Pastels", 0xA9DDD3, 0.0),
    entry("Baby blue", "Pastels", 0xAFCFEF, 0.0),
    entry("Lilac", "Pastels", 0xCDB9E5, 0.0),
    entry("Dove", "Pastels", 0xD6D3CF, 0.0),
    entry("Chocolate", "Earth", 0x4A2E1E, 0.0),
    entry("Brown", "Earth", 0x6B4428, 0.0),
    entry("Rust", "Earth", 0xA64520, 0.0),
    entry("Terracotta", "Earth", 0xC0603E, 0.0),
    entry("Tan", "Earth", 0xB08A60, 0.0),
    entry("Khaki", "Earth", 0xB8AE7E, 0.0),
    entry("Sand", "Earth", 0xD9C49C, 0.0),
    entry("Clay", "Earth", 0x9C7A64, 0.0),
    translucent("Clear", 0xE9F0F2, 0.85, 0.0),
    translucent("Frosted", 0xE4E9EC, 0.55, 0.6),
    translucent("Smoke", 0x3A3D44, 0.6, 0.0),
    translucent("Tinted red", 0xD8263A, 0.6, 0.0),
    translucent("Tinted amber", 0xF0A020, 0.6, 0.0),
    translucent("Tinted green", 0x3FB860, 0.6, 0.0),
    translucent("Tinted blue", 0x2F7FE0, 0.6, 0.0),
    translucent("Tinted purple", 0x8A4FD0, 0.6, 0.0),
    translucent("Glow white", 0xF6F4EC, 0.4, 0.6),
    translucent("Ice blue", 0xBFE3F5, 0.7, 0.0),
    mask("Mask green", 0x1E6B3A, 0.55, 1.6, "mask_gloss"),
    mask("Mask red", 0xB0201A, 0.45, 1.7, "mask_gloss"),
    mask("Mask blue", 0x1F3F8F, 0.45, 1.6, "mask_gloss"),
    mask("Mask black", 0x141414, 0.05, 1.1, "mask_matte"),
    mask("Mask white", 0xEDEDE6, 0.15, 1.05, "mask_gloss"),
    mask("Mask yellow", 0xD9B21F, 0.35, 1.3, "mask_gloss"),
    mask("Mask purple", 0x4B2A7A, 0.45, 1.6, "mask_gloss"),
    mask("Mask matte green", 0x24563A, 0.45, 1.4, "mask_matte"),
    entry("Silk white", "Silkscreen", 0xF0F0EA, 0.0),
    entry("Silk black", "Silkscreen", 0x1A1A1A, 0.0),
    entry("Silk yellow", "Silkscreen", 0xE8D23A, 0.0),
    entry("Laser marking", "Component bodies", 0x4A4A4C, 0.0),
    anodising("Anodising clear", [0.91, 0.92, 0.93]),
    anodising("Anodising black", [0.07, 0.07, 0.08]),
    anodising("Anodising red", [0.72, 0.07, 0.1]),
    anodising("Anodising blue", [0.1, 0.3, 0.74]),
    anodising("Anodising teal", [0.07, 0.43, 0.43]),
    anodising("Anodising gold", [0.86, 0.67, 0.28]),
    anodising("Anodising purple", [0.44, 0.18, 0.62]),
    anodising("Anodising green", [0.18, 0.54, 0.24]),
    anodising("Anodising orange", [0.95, 0.44, 0.09]),
    anodising("Anodising rose", [0.88, 0.56, 0.62]),
    anodising("Anodising champagne", [0.85, 0.76, 0.58]),
    anodising("Anodising titanium", [0.42, 0.43, 0.45]),
];

/// A case-insensitive name, with underscores accepted as spaces.
pub fn swatch(name: &str) -> Option<&'static Swatch> {
    let name = name.replace('_', " ");
    SWATCHES
        .iter()
        .find(|swatch| swatch.name.eq_ignore_ascii_case(&name))
}

/// A swatch name, or a standard colour code or name; see [`crate::lookup`].
pub fn colour_by_name(name: &str) -> Option<Colour> {
    swatch(name)
        .map(|swatch| swatch.colour)
        .or_else(|| find_ral(name).map(RalColour::colour))
}

/// An sRGB colour as #RRGGBB, rounded and clamped to byte channels.
pub fn hex_string(srgb: Srgb) -> String {
    let [r, g, b] = srgb.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8);
    format!("#{r:02X}{g:02X}{b:02X}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn named_colours() {
        let anodising: Vec<_> = SWATCHES.iter().filter(|s| s.family == "Anodising").collect();
        assert_eq!(anodising.len(), 12, "twelve anodising colours share the Anodising family");
        for (swatch, (name, rgb)) in anodising.iter().zip([
            ("clear", [0.91, 0.92, 0.93]), ("black", [0.07, 0.07, 0.08]),
            ("red", [0.72, 0.07, 0.10]), ("blue", [0.10, 0.30, 0.74]),
            ("teal", [0.07, 0.43, 0.43]), ("gold", [0.86, 0.67, 0.28]),
            ("purple", [0.44, 0.18, 0.62]), ("green", [0.18, 0.54, 0.24]),
            ("orange", [0.95, 0.44, 0.09]), ("rose", [0.88, 0.56, 0.62]),
            ("champagne", [0.85, 0.76, 0.58]), ("titanium", [0.42, 0.43, 0.45]),
        ]) {
            assert_eq!(swatch.name, format!("Anodising {name}"), "generic anodising swatch name: {name}");
            assert_eq!(swatch.colour, Colour::srgb(rgb), "anodising preserves exact sRGB and opacity: {name}");
            assert_eq!(colour_by_name(swatch.name), Some(swatch.colour), "anodising colour name resolves: {name}");
        }
        assert_eq!(
            colour_by_name("smoke").unwrap().translucency,
            0.6,
            "smoke translucency"
        );
        assert_eq!(
            swatch("tinted_red"),
            swatch("Tinted red"),
            "underscore swatch name"
        );
        assert_eq!(
            colour_by_name("TINTED_RED").unwrap().translucency,
            0.6,
            "case-insensitive translucent swatch"
        );
        for name in ["RAL 9005", "jet black"] {
            let colour = colour_by_name(name).unwrap();
            assert_eq!(colour.ral, Some(9005), "standard colour lookup {name}");
            assert_eq!(colour.translucency, 0.0, "opaque standard colour {name}");
        }
        assert!(
            colour_by_name("unknown colour").is_none(),
            "unknown colour is absent"
        );
        let mut names = HashSet::new();
        for swatch in SWATCHES {
            assert!(
                names.insert(swatch.name.to_ascii_lowercase()),
                "unique swatch name {}",
                swatch.name
            );
        }
        for swatch in SWATCHES {
            assert!(swatch.lift >= 1.0, "a coating reads no darker over copper: {}", swatch.name);
            assert_eq!(swatch.finish.is_some(), swatch.family == "Solder mask", "mask colours, and only they, name a finish: {}", swatch.name);
            assert_eq!(swatch.roughness.is_some(), swatch.family == "Translucent", "translucent colours, and only they, name a roughness: {}", swatch.name);
            if let Some(finish) = swatch.finish {
                assert!(crate::FinishId::lookup(finish).is_some(), "mask finish is in the table: {}", swatch.name);
            }
        }
        let green = swatch("mask_green").unwrap();
        assert_eq!((green.lift, green.finish, green.colour.translucency), (1.6, Some("mask_gloss"), 0.55), "stackup preset by id");
        assert_eq!(crate::lookup("mask_matte_green"), Some(crate::Lookup::Colour(swatch("Mask matte green").unwrap().colour)), "crate lookup reaches mask colours");
        let cream = colour_by_name("Cream").unwrap();
        assert_eq!(
            cream.ral, None,
            "swatch takes precedence over standard name"
        );
        assert_eq!(hex_string(cream.srgb), "#E8DCBC", "swatch hex round trip");
        assert_eq!(
            hex_string([-0.1, 0.5, 1.1]),
            "#0080FF",
            "hex channels clamp and round"
        );
    }
}
