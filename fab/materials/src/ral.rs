//! RAL Classic codes, names, screen colours and nearest-colour search.
//!
//! RAL publishes no official sRGB values: the screen colours here are approximations
//! for previews and swatches, not for colour matching.
//!
//! Sources: the numbers and English names are RAL gGmbH's RAL Classic collection. The sRGB
//! values match the hex column of Wikipedia's "List of RAL colours" (CC BY-SA 4.0; checked
//! row by row on 15 colours across the groups, 2026-11-22), which names no source for them.

use crate::{Colour, Srgb};
use fab_colour::lab::{delta_e2000, srgb_to_lab, CieLab};
use std::sync::OnceLock;

/// The colour families in number order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RalGroup {
    YellowBeige,
    Orange,
    Red,
    Violet,
    Blue,
    Green,
    Grey,
    Brown,
    WhiteBlack,
}

impl RalGroup {
    pub fn name(self) -> &'static str {
        match self {
            Self::YellowBeige => "Yellow & beige",
            Self::Orange => "Orange",
            Self::Red => "Red",
            Self::Violet => "Violet",
            Self::Blue => "Blue",
            Self::Green => "Green",
            Self::Grey => "Grey",
            Self::Brown => "Brown",
            Self::WhiteBlack => "White & black",
        }
    }
}

/// The pigment effect; luminous shades are fluorescent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RalEffect {
    Solid,
    Pearl,
    Metallic,
    Luminous,
}

/// A standard colour and its screen approximation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RalColour {
    pub number: u16,
    pub name: &'static str,
    pub group: RalGroup,
    pub srgb: Srgb,
    pub effect: RalEffect,
    pub popular: bool,
}

impl RalColour {
    pub fn code(&self) -> String {
        format!("RAL {}", self.number)
    }

    pub fn colour(&self) -> Colour {
        Colour {
            srgb: self.srgb,
            translucency: 0.0,
            ral: Some(self.number),
        }
    }
}

const fn row(
    number: u16,
    name: &'static str,
    hex: u32,
    effect: RalEffect,
    popular: bool,
) -> RalColour {
    let group = match number / 1000 - 1 {
        0 => RalGroup::YellowBeige,
        1 => RalGroup::Orange,
        2 => RalGroup::Red,
        3 => RalGroup::Violet,
        4 => RalGroup::Blue,
        5 => RalGroup::Green,
        6 => RalGroup::Grey,
        7 => RalGroup::Brown,
        8 => RalGroup::WhiteBlack,
        _ => panic!("invalid RAL group"),
    };
    RalColour {
        number,
        name,
        group,
        srgb: Colour::hex(hex).srgb,
        effect,
        popular,
    }
}

use RalEffect::{Luminous, Metallic, Pearl, Solid};

/// All 216 RAL Classic colours, sorted by number.
pub static RAL_CLASSIC: [RalColour; 216] = [
    row(1000, "Green beige", 0xCDBA88, Solid, false),
    row(1001, "Beige", 0xD0B084, Solid, false),
    row(1002, "Sand yellow", 0xD2AA6D, Solid, false),
    row(1003, "Signal yellow", 0xF9A800, Solid, true),
    row(1004, "Golden yellow", 0xE49E00, Solid, false),
    row(1005, "Honey yellow", 0xCB8E00, Solid, false),
    row(1006, "Maize yellow", 0xE29000, Solid, false),
    row(1007, "Daffodil yellow", 0xE88C00, Solid, false),
    row(1011, "Brown beige", 0xAF804F, Solid, false),
    row(1012, "Lemon yellow", 0xDDAF27, Solid, false),
    row(1013, "Oyster white", 0xE3D9C6, Solid, true),
    row(1014, "Ivory", 0xDDC49A, Solid, false),
    row(1015, "Light ivory", 0xE6D2B5, Solid, true),
    row(1016, "Sulfur yellow", 0xF1DD38, Solid, false),
    row(1017, "Saffron yellow", 0xF6A950, Solid, false),
    row(1018, "Zinc yellow", 0xFACA30, Solid, true),
    row(1019, "Grey beige", 0xA48F7A, Solid, false),
    row(1020, "Olive yellow", 0xA08F65, Solid, false),
    row(1021, "Colza yellow", 0xF6B600, Solid, true),
    row(1023, "Traffic yellow", 0xF7B500, Solid, true),
    row(1024, "Ochre yellow", 0xBA8F4C, Solid, false),
    row(1026, "Luminous yellow", 0xFFFF00, Luminous, false),
    row(1027, "Curry", 0xA77F0E, Solid, false),
    row(1028, "Melon yellow", 0xFF9B00, Solid, false),
    row(1032, "Broom yellow", 0xE2A300, Solid, false),
    row(1033, "Dahlia yellow", 0xF99A1C, Solid, false),
    row(1034, "Pastel yellow", 0xEB9C52, Solid, false),
    row(1035, "Pearl beige", 0x908370, Pearl, false),
    row(1036, "Pearl gold", 0x80643F, Pearl, false),
    row(1037, "Sun yellow", 0xF09200, Solid, false),
    row(2000, "Yellow orange", 0xDD7907, Solid, false),
    row(2001, "Red orange", 0xBE4E20, Solid, false),
    row(2002, "Vermilion", 0xC63927, Solid, false),
    row(2003, "Pastel orange", 0xFA842B, Solid, false),
    row(2004, "Pure orange", 0xE75B12, Solid, true),
    row(2005, "Luminous orange", 0xFF2300, Luminous, false),
    row(2007, "Luminous bright orange", 0xFFA421, Luminous, false),
    row(2008, "Bright red orange", 0xF3752C, Solid, false),
    row(2009, "Traffic orange", 0xE15501, Solid, true),
    row(2010, "Signal orange", 0xD4652F, Solid, false),
    row(2011, "Deep orange", 0xEC7C25, Solid, false),
    row(2012, "Salmon orange", 0xDB6A50, Solid, false),
    row(2013, "Pearl orange", 0x954527, Pearl, false),
    row(2017, "RAL orange", 0xFA4402, Solid, false),
    row(3000, "Flame red", 0xAB2524, Solid, true),
    row(3001, "Signal red", 0xA02128, Solid, false),
    row(3002, "Carmine red", 0xA1232B, Solid, false),
    row(3003, "Ruby red", 0x8D1D2C, Solid, false),
    row(3004, "Purple red", 0x701F29, Solid, false),
    row(3005, "Wine red", 0x5E2028, Solid, false),
    row(3007, "Black red", 0x402225, Solid, false),
    row(3009, "Oxide red", 0x703731, Solid, false),
    row(3011, "Brown red", 0x7E292C, Solid, false),
    row(3012, "Beige red", 0xCB8D73, Solid, false),
    row(3013, "Tomato red", 0x9C322E, Solid, false),
    row(3014, "Antique pink", 0xD47479, Solid, false),
    row(3015, "Light pink", 0xE1A6AD, Solid, false),
    row(3016, "Coral red", 0xAC4034, Solid, false),
    row(3017, "Rose", 0xD3545F, Solid, false),
    row(3018, "Strawberry red", 0xD14152, Solid, false),
    row(3020, "Traffic red", 0xC1121C, Solid, true),
    row(3022, "Salmon pink", 0xD56D56, Solid, false),
    row(3024, "Luminous red", 0xF70000, Luminous, false),
    row(3026, "Luminous bright red", 0xFF0000, Luminous, false),
    row(3027, "Raspberry red", 0xB42041, Solid, false),
    row(3028, "Pure red", 0xE72512, Solid, false),
    row(3031, "Orient red", 0xAC323B, Solid, false),
    row(3032, "Pearl ruby red", 0x711521, Pearl, false),
    row(3033, "Pearl pink", 0xB24C43, Pearl, false),
    row(4001, "Red lilac", 0x8A5A83, Solid, false),
    row(4002, "Red violet", 0x933D50, Solid, false),
    row(4003, "Heather violet", 0xD15B8F, Solid, false),
    row(4004, "Claret violet", 0x691639, Solid, false),
    row(4005, "Blue lilac", 0x83639D, Solid, false),
    row(4006, "Traffic purple", 0x992572, Solid, false),
    row(4007, "Purple violet", 0x4A203B, Solid, false),
    row(4008, "Signal violet", 0x904684, Solid, false),
    row(4009, "Pastel violet", 0xA38995, Solid, false),
    row(4010, "Telemagenta", 0xC63678, Solid, false),
    row(4011, "Pearl violet", 0x8773A1, Pearl, false),
    row(4012, "Pearl blackberry", 0x6B6880, Pearl, false),
    row(5000, "Violet blue", 0x384C70, Solid, false),
    row(5001, "Green blue", 0x1F4764, Solid, false),
    row(5002, "Ultramarine blue", 0x2B2C7C, Solid, true),
    row(5003, "Sapphire blue", 0x2A3756, Solid, false),
    row(5004, "Black blue", 0x1D1F2A, Solid, false),
    row(5005, "Signal blue", 0x154889, Solid, false),
    row(5007, "Brilliant blue", 0x41678D, Solid, false),
    row(5008, "Grey blue", 0x313C48, Solid, false),
    row(5009, "Azure blue", 0x2E5978, Solid, false),
    row(5010, "Gentian blue", 0x13447C, Solid, true),
    row(5011, "Steel blue", 0x232C3F, Solid, false),
    row(5012, "Light blue", 0x3481B8, Solid, true),
    row(5013, "Cobalt blue", 0x232D53, Solid, false),
    row(5014, "Pigeon blue", 0x6C7C98, Solid, false),
    row(5015, "Sky blue", 0x2874B2, Solid, true),
    row(5017, "Traffic blue", 0x0E518D, Solid, true),
    row(5018, "Turquoise blue", 0x21888F, Solid, false),
    row(5019, "Capri blue", 0x1A5784, Solid, false),
    row(5020, "Ocean blue", 0x0B4151, Solid, false),
    row(5021, "Water blue", 0x07737A, Solid, false),
    row(5022, "Night blue", 0x2F2A5A, Solid, false),
    row(5023, "Distant blue", 0x4D668E, Solid, false),
    row(5024, "Pastel blue", 0x6A93B0, Solid, false),
    row(5025, "Pearl gentian blue", 0x296478, Pearl, false),
    row(5026, "Pearl night blue", 0x102C54, Pearl, false),
    row(6000, "Patina green", 0x327662, Solid, false),
    row(6001, "Emerald green", 0x28713E, Solid, false),
    row(6002, "Leaf green", 0x276235, Solid, false),
    row(6003, "Olive green", 0x4B573E, Solid, false),
    row(6004, "Blue green", 0x0E4243, Solid, false),
    row(6005, "Moss green", 0x0F4336, Solid, true),
    row(6006, "Grey olive", 0x40433B, Solid, false),
    row(6007, "Bottle green", 0x283424, Solid, false),
    row(6008, "Brown green", 0x35382E, Solid, false),
    row(6009, "Fir green", 0x26392F, Solid, false),
    row(6010, "Grass green", 0x3E753B, Solid, false),
    row(6011, "Reseda green", 0x68825B, Solid, false),
    row(6012, "Black green", 0x31403D, Solid, false),
    row(6013, "Reed green", 0x797C5A, Solid, false),
    row(6014, "Yellow olive", 0x444337, Solid, false),
    row(6015, "Black olive", 0x3D403A, Solid, false),
    row(6016, "Turquoise green", 0x026A52, Solid, false),
    row(6017, "May green", 0x468641, Solid, false),
    row(6018, "Yellow green", 0x48A43F, Solid, true),
    row(6019, "Pastel green", 0xB7D9B1, Solid, false),
    row(6020, "Chrome green", 0x354733, Solid, false),
    row(6021, "Pale green", 0x86A47C, Solid, false),
    row(6022, "Olive drab", 0x3E3C32, Solid, false),
    row(6024, "Traffic green", 0x008754, Solid, true),
    row(6025, "Fern green", 0x53753C, Solid, false),
    row(6026, "Opal green", 0x005D52, Solid, false),
    row(6027, "Light green", 0x81C0BB, Solid, false),
    row(6028, "Pine green", 0x2D5546, Solid, false),
    row(6029, "Mint green", 0x007243, Solid, true),
    row(6032, "Signal green", 0x0F8558, Solid, false),
    row(6033, "Mint turquoise", 0x478A84, Solid, false),
    row(6034, "Pastel turquoise", 0x7FB0B2, Solid, false),
    row(6035, "Pearl green", 0x1B542C, Pearl, false),
    row(6036, "Pearl opal green", 0x005D4C, Pearl, false),
    row(6037, "Pure green", 0x25E712, Solid, false),
    row(6038, "Luminous green", 0x00F700, Luminous, false),
    row(6039, "Fibrous green", 0xB3C43E, Solid, false),
    row(7000, "Squirrel grey", 0x7E8B92, Solid, false),
    row(7001, "Silver grey", 0x8F999F, Solid, true),
    row(7002, "Olive grey", 0x817F68, Solid, false),
    row(7003, "Moss grey", 0x7A7B6D, Solid, false),
    row(7004, "Signal grey", 0x9EA0A1, Solid, true),
    row(7005, "Mouse grey", 0x6B716F, Solid, false),
    row(7006, "Beige grey", 0x756F61, Solid, false),
    row(7008, "Khaki grey", 0x746643, Solid, false),
    row(7009, "Green grey", 0x5B6259, Solid, false),
    row(7010, "Tarpaulin grey", 0x575D57, Solid, false),
    row(7011, "Iron grey", 0x555D61, Solid, false),
    row(7012, "Basalt grey", 0x596163, Solid, false),
    row(7013, "Brown grey", 0x555548, Solid, false),
    row(7015, "Slate grey", 0x51565C, Solid, true),
    row(7016, "Anthracite grey", 0x373F43, Solid, true),
    row(7021, "Black grey", 0x2E3234, Solid, true),
    row(7022, "Umbra grey", 0x4B4D46, Solid, false),
    row(7023, "Concrete grey", 0x818479, Solid, false),
    row(7024, "Graphite grey", 0x474A50, Solid, true),
    row(7026, "Granite grey", 0x374447, Solid, false),
    row(7030, "Stone grey", 0x939388, Solid, false),
    row(7031, "Blue grey", 0x5D6970, Solid, false),
    row(7032, "Pebble grey", 0xB9B9A8, Solid, true),
    row(7033, "Cement grey", 0x818979, Solid, false),
    row(7034, "Yellow grey", 0x939176, Solid, false),
    row(7035, "Light grey", 0xCBD0CC, Solid, true),
    row(7036, "Platinum grey", 0x9A9697, Solid, false),
    row(7037, "Dusty grey", 0x7C7F7E, Solid, false),
    row(7038, "Agate grey", 0xB4B8B0, Solid, false),
    row(7039, "Quartz grey", 0x6B695F, Solid, false),
    row(7040, "Window grey", 0x9DA3A6, Solid, true),
    row(7042, "Traffic grey A", 0x8F9695, Solid, false),
    row(7043, "Traffic grey B", 0x4E5451, Solid, false),
    row(7044, "Silk grey", 0xBDBDB2, Solid, false),
    row(7045, "Telegrey 1", 0x91969A, Solid, false),
    row(7046, "Telegrey 2", 0x82898E, Solid, false),
    row(7047, "Telegrey 4", 0xCFD0CF, Solid, false),
    row(7048, "Pearl mouse grey", 0x888175, Pearl, false),
    row(8000, "Green brown", 0x887142, Solid, false),
    row(8001, "Ochre brown", 0x9C6B30, Solid, false),
    row(8002, "Signal brown", 0x7B5141, Solid, false),
    row(8003, "Clay brown", 0x80542F, Solid, false),
    row(8004, "Copper brown", 0x8F4E35, Solid, false),
    row(8007, "Fawn brown", 0x6F4A2F, Solid, false),
    row(8008, "Olive brown", 0x6F4F28, Solid, false),
    row(8011, "Nut brown", 0x5A3A29, Solid, false),
    row(8012, "Red brown", 0x673831, Solid, false),
    row(8014, "Sepia brown", 0x49392D, Solid, false),
    row(8015, "Chestnut brown", 0x633A34, Solid, false),
    row(8016, "Mahogany brown", 0x4C2F26, Solid, false),
    row(8017, "Chocolate brown", 0x44322D, Solid, true),
    row(8019, "Grey brown", 0x3F3A3A, Solid, false),
    row(8022, "Black brown", 0x211F20, Solid, false),
    row(8023, "Orange brown", 0xA65E2F, Solid, false),
    row(8024, "Beige brown", 0x79553C, Solid, false),
    row(8025, "Pale brown", 0x755C49, Solid, false),
    row(8028, "Terra brown", 0x4E3B2B, Solid, false),
    row(8029, "Pearl copper", 0x773C27, Pearl, false),
    row(9001, "Cream", 0xEFEBDC, Solid, false),
    row(9002, "Grey white", 0xDDDED4, Solid, false),
    row(9003, "Signal white", 0xF4F8F4, Solid, true),
    row(9004, "Signal black", 0x2E3032, Solid, false),
    row(9005, "Jet black", 0x0A0A0D, Solid, true),
    row(9006, "White aluminium", 0xA5A8A6, Metallic, true),
    row(9007, "Grey aluminium", 0x8F8F8C, Metallic, false),
    row(9010, "Pure white", 0xF7F9EF, Solid, true),
    row(9011, "Graphite black", 0x1C1C1C, Solid, false),
    row(9012, "Cleanroom white", 0xFFFDE6, Solid, false),
    row(9016, "Traffic white", 0xF7FBF5, Solid, true),
    row(9017, "Traffic black", 0x2A2D2F, Solid, true),
    row(9018, "Papyrus white", 0xCFD3CD, Solid, false),
    row(9022, "Pearl light grey", 0x858583, Pearl, false),
    row(9023, "Pearl dark grey", 0x7E8182, Pearl, false),
];

/// A colour by its four-digit number.
pub fn ral(number: u16) -> Option<&'static RalColour> {
    RAL_CLASSIC
        .binary_search_by_key(&number, |colour| colour.number)
        .ok()
        .map(|index| &RAL_CLASSIC[index])
}

/// A four-digit code, optionally prefixed by RAL and a separator.
pub fn ral_by_code(code: &str) -> Option<&'static RalColour> {
    let bytes = code.as_bytes();
    let is_space = |byte: u8| matches!(byte, b' ' | b'\t'..=b'\r');
    let mut i = 0;
    while i < bytes.len() && is_space(bytes[i]) {
        i += 1;
    }
    if bytes
        .get(i..i + 3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"ral"))
    {
        i += 3;
    }
    let mut number = 0;
    let mut digits = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => {
                digits += 1;
                if digits > 4 {
                    return None;
                }
                number = number * 10 + u16::from(bytes[i] - b'0');
            }
            b' ' | b'-' | b'_' | b':' => {
                if digits != 0 {
                    break;
                }
            }
            _ => return None,
        }
        i += 1;
    }
    while i < bytes.len() && is_space(bytes[i]) {
        i += 1;
    }
    if i == bytes.len() && digits == 4 {
        ral(number)
    } else {
        None
    }
}

/// A case-insensitive official English name.
pub fn ral_by_name(name: &str) -> Option<&'static RalColour> {
    RAL_CLASSIC
        .iter()
        .find(|colour| colour.name.eq_ignore_ascii_case(name))
}

/// A standard colour code or official English name.
pub fn find_ral(code_or_name: &str) -> Option<&'static RalColour> {
    ral_by_code(code_or_name).or_else(|| ral_by_name(code_or_name))
}

/// The nearest colour and its CIEDE2000 distance; optionally skip effect pigments.
pub fn nearest_ral(srgb: Srgb, solid_only: bool) -> (&'static RalColour, f32) {
    static LAB: OnceLock<[CieLab; 216]> = OnceLock::new();
    let labs = LAB.get_or_init(|| std::array::from_fn(|i| srgb_to_lab(RAL_CLASSIC[i].srgb)));
    let query = srgb_to_lab(srgb);
    let mut best = &RAL_CLASSIC[0];
    let mut best_distance = 1e30;
    for (colour, &lab) in RAL_CLASSIC.iter().zip(labs) {
        if solid_only && colour.effect != RalEffect::Solid {
            continue;
        }
        let distance = delta_e2000(query, lab);
        if distance < best_distance {
            best = colour;
            best_distance = distance;
        }
    }
    (best, best_distance)
}

/// A drawing note with the nearest code and, when approximate, its distance.
pub fn ral_note(srgb: Srgb) -> String {
    let (colour, distance) = nearest_ral(srgb, false);
    if distance < 0.05 {
        colour.code()
    } else {
        format!("~ {} (dE {:.1})", colour.code(), distance)
    }
}

/// The first colour whose channels each differ by less than 0.6/255.
pub fn exact_ral(srgb: Srgb) -> Option<&'static RalColour> {
    RAL_CLASSIC.iter().find(|colour| {
        srgb.iter()
            .zip(colour.srgb)
            .all(|(&a, b)| (a - b).abs() < 0.6 / 255.0)
    })
}

/// Rendering metalness for an exact metallic or pearl shade.
pub fn effect_metalness(srgb: Srgb) -> f32 {
    match exact_ral(srgb).map(|colour| colour.effect) {
        Some(RalEffect::Metallic) => 0.5,
        Some(RalEffect::Pearl) => 0.35,
        _ => 0.0,
    }
}

impl Colour {
    pub fn ral(number: u16) -> Option<Colour> {
        ral(number).map(RalColour::colour)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn collection_and_lookups() {
        assert_eq!(RAL_CLASSIC.len(), 216, "RAL Classic colour count");
        let mut numbers = HashSet::new();
        let mut names = HashSet::new();
        for (i, colour) in RAL_CLASSIC.iter().enumerate() {
            let number = colour.number;
            assert!(numbers.insert(number), "unique RAL number {number}");
            assert!(
                names.insert(colour.name.to_ascii_lowercase()),
                "unique name {}",
                colour.name
            );
            assert!(
                i == 0 || RAL_CLASSIC[i - 1].number < number,
                "number order at {number}"
            );
            assert!(
                (1000..=9999).contains(&number),
                "four-digit number {number}"
            );
            assert_eq!(colour.group as u16, number / 1000 - 1, "group of {number}");
            for code in [
                number.to_string(),
                colour.code(),
                format!("ral{number}"),
                format!("RAL-{number}"),
            ] {
                assert!(
                    ral_by_code(&code).is_some_and(|found| std::ptr::eq(found, colour)),
                    "code lookup {code}"
                );
            }
            for name in [
                colour.name.to_ascii_lowercase(),
                colour.name.to_ascii_uppercase(),
            ] {
                assert!(
                    ral_by_name(&name).is_some_and(|found| std::ptr::eq(found, colour)),
                    "name lookup {name}"
                );
            }
            let (nearest, distance) = nearest_ral(colour.srgb, false);
            assert!(
                std::ptr::eq(nearest, colour) && distance < 1e-3,
                "nearest of {number}: {} at {distance}",
                nearest.number
            );
            assert_eq!(
                Colour::ral(number),
                Some(colour.colour()),
                "Colour constructor for {number}"
            );
            assert_eq!(
                colour.colour().ral,
                Some(number),
                "standard code for {number}"
            );
            assert_eq!(
                colour.colour().translucency,
                0.0,
                "opaque standard colour {number}"
            );
        }
        for query in [
            "3020",
            "RAL 3020",
            "ral3020",
            "RAL-3020",
            " ral 3020 ",
            "\x0bRAL 3020 \x0b",
            "RAL_3020",
            "RAL:3020",
            "Traffic red",
            "TRAFFIC RED",
        ] {
            assert_eq!(
                find_ral(query).map(|colour| colour.number),
                Some(3020),
                "lookup {query:?}"
            );
        }
        for query in [
            "30201",
            "302",
            "abc",
            "RAL 3021",
            "RAL 30 20x",
            "",
            "RAL 3020-",
            "RAL 30 20",
            "ＲＡＬ 3020",
        ] {
            assert!(find_ral(query).is_none(), "no match for {query:?}");
        }
        assert!(Colour::ral(3021).is_none(), "unknown standard colour");
    }

    #[test]
    fn effects_and_popular_colours() {
        let expected = [
            (1026, Luminous),
            (1035, Pearl),
            (1036, Pearl),
            (2005, Luminous),
            (2007, Luminous),
            (2013, Pearl),
            (3024, Luminous),
            (3026, Luminous),
            (3032, Pearl),
            (3033, Pearl),
            (4011, Pearl),
            (4012, Pearl),
            (5025, Pearl),
            (5026, Pearl),
            (6035, Pearl),
            (6036, Pearl),
            (6038, Luminous),
            (7048, Pearl),
            (8029, Pearl),
            (9006, Metallic),
            (9007, Metallic),
            (9022, Pearl),
            (9023, Pearl),
        ];
        let actual: Vec<_> = RAL_CLASSIC
            .iter()
            .filter(|colour| colour.effect != Solid)
            .map(|colour| (colour.number, colour.effect))
            .collect();
        assert_eq!(actual, expected, "complete set of effect shades");
        assert_eq!(
            RAL_CLASSIC.iter().filter(|colour| colour.popular).count(),
            35,
            "popular colour count"
        );
        for number in [9005, 9016, 7016, 7035, 3020, 5015, 6018, 1023] {
            assert!(ral(number).unwrap().popular, "popular RAL {number}");
        }
        for (number, metalness) in [
            (9006, 0.5),
            (9007, 0.5),
            (1035, 0.35),
            (1036, 0.35),
            (9022, 0.35),
            (9023, 0.35),
            (1026, 0.0),
            (9005, 0.0),
        ] {
            let colour = ral(number).unwrap();
            assert_eq!(
                exact_ral(colour.srgb).map(|found| found.number),
                Some(number),
                "exact RAL {number}"
            );
            assert_eq!(
                effect_metalness(colour.srgb),
                metalness,
                "metalness of RAL {number}"
            );
        }
        let black = ral(9005).unwrap().srgb;
        assert_eq!(
            exact_ral(black.map(|channel| channel + 0.5 / 255.0)).map(|colour| colour.number),
            Some(9005),
            "exact match tolerates half a channel step"
        );
        for channel in 0..3 {
            let mut shifted = black;
            shifted[channel] += 0.7 / 255.0;
            assert!(
                exact_ral(shifted).is_none(),
                "exact match rejects channel {channel} outside tolerance"
            );
        }
        assert_eq!(
            effect_metalness([1.0, 0.0, 1.0]),
            0.0,
            "unmatched colour has no metalness"
        );
    }

    #[test]
    fn screen_colours_and_drawing_notes() {
        let rgb = |number| ral(number).unwrap().srgb;
        let lab = |number| srgb_to_lab(rgb(number));
        let red = rgb(3020);
        assert!(
            red[0] > 0.65 && red[1] < 0.15 && red[2] < 0.15,
            "RAL 3020 strong red"
        );
        let blue = rgb(5015);
        assert!(
            blue[2] > 0.6 && blue[2] > blue[0] * 2.0,
            "RAL 5015 sky blue"
        );
        let green = rgb(6018);
        assert!(
            green[1] > green[0] && green[1] > green[2] && green[1] > 0.55,
            "RAL 6018 yellow green"
        );
        assert!(lab(9005)[0] < 5.0, "RAL 9005 jet black");
        let grey = lab(7035);
        assert!(
            grey[0] > 80.0 && grey[1].hypot(grey[2]) < 5.0,
            "RAL 7035 light grey"
        );
        assert!(lab(1023)[2] > 60.0, "RAL 1023 yellow");
        let off_red = [0.76, 0.08, 0.12];
        let (nearest, _) = nearest_ral(off_red, true);
        assert_eq!(nearest.group, RalGroup::Red, "near-red maps to red");
        assert_eq!(nearest.effect, Solid, "solid-only match excludes effects");
        let (solid, _) = nearest_ral(rgb(1026), true);
        assert_eq!(
            solid.effect, Solid,
            "solid-only match excludes an exact luminous shade"
        );
        assert_eq!(ral_note(rgb(9005)), "RAL 9005", "exact drawing note");
        let (nearest, distance) = nearest_ral(off_red, false);
        assert!(
            distance >= 0.05,
            "off-red requires an approximate drawing note"
        );
        assert_eq!(
            ral_note(off_red),
            format!("~ RAL {} (dE {:.1})", nearest.number, distance),
            "approximate drawing note"
        );
    }
}
