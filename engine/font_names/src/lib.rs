//! The names a document can use for its font families.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FamilyName {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
}

// Declaration order resolves ambiguous aliases, with the bundled families first.
macro_rules! families {
    ($($id:ident => $name:literal, $aliases:expr;)*) => {
        $(pub const $id: FamilyName = FamilyName { name: $name, aliases: &$aliases };)*
        pub const FAMILIES: &[FamilyName] = &[$($id),*];
    };
}

families! {
    INTER => "Inter", ["sans", "sans-serif", "default"];
    ROBOTO_FLEX => "Roboto Flex", ["roboto"];
    IBM_PLEX_SANS => "IBM Plex Sans", ["plex", "ibmplex"];
    NOTO_SANS => "Noto Sans", [];
    JETBRAINS_MONO => "JetBrains Mono", ["mono", "monospace"];
    LIBERATION_MONO => "Liberation Mono", [];
    NEW_COMPUTER_MODERN => "New Computer Modern", ["serif", "roman", "newcm", "computermodern"];
    LXGW_WENKAI => "LXGW WenKai", ["wenkai", "handwritten", "script"];
    PLAYFAIR_DISPLAY => "Playfair Display", ["display-serif", "playfair"];
    CORMORANT_GARAMOND => "Cormorant Garamond", ["editorial", "cormorant", "garamond"];
    ANTON => "Anton", ["condensed", "display-condensed", "anton-regular"];
    STIX_TWO_MATH => "STIX Two Math", ["math", "stix", "stix-two-math", "stixtwomath"];
    LEXEND_ZETTA => "Lexend Zetta", ["wide", "expanded", "extended", "display-wide", "lexend", "zetta"];
    ARCHIVO => "Archivo", [];
    IBM_PLEX_MONO => "IBM Plex Mono", ["plex mono"];
    CORMORANT => "Cormorant", [];
    MONTSERRAT => "Montserrat", [];
    BEBAS_NEUE => "Bebas Neue", ["bebas"];
    POPPINS => "Poppins", [];
    OSWALD => "Oswald", [];
    LEAGUE_SPARTAN => "League Spartan", ["spartan"];
    OPEN_SANS => "Open Sans", ["opensans"];
}

fn key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

/// The family a name means: its name spelled exactly, else its name or an
/// alias without case, spaces or dashes (the bundled families first: a name
/// a bundled family answers to keeps meaning it, so `cormorant` stays
/// Cormorant Garamond while `"Cormorant"` spelled so is the downloadable
/// family).
pub fn family(name: &str) -> Option<&'static FamilyName> {
    if let Some(exact) = FAMILIES.iter().find(|f| f.name == name.trim()) {
        return Some(exact);
    }
    let wanted = key(name);
    FAMILIES.iter().find(|f| key(f.name) == wanted || f.aliases.iter().any(|a| key(a) == wanted))
}
