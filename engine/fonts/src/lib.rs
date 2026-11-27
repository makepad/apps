//! Document fonts: ONE table of the font families a document can name
//! (`font: Font{family: "Archivo"}`), bundled or downloaded, each with the files that draw
//! it, the aliases it answers to and what it is for. Every consumer reads
//! this table: the loader (a name is a family or an error), the drawing
//! side's text styles, the measuring functions, the 3D type builders, the
//! bundlers.
//!
//! A bundled family's files are crate resources (`makepad_widgets:resources/
//! Inter.ttf`, `makepad_doc_fonts:resources/fonts/…`: the names `crate_resource`
//! takes), resolved through the roots their crates register ([`register_root`];
//! this crate's own is known from the start). A host that must hear of every
//! root (Scene3D trusts the font folders) asks with [`observe_roots`]. A downloaded family (Stage's Resources: a
//! resumable download, the pinned size and SHA-256 checked before the file is
//! kept, the licence beside it) lands in the AI Hub's weights folder under
//! `fonts/<family>/`, where every host finds it the same way.
//!
//! A family is variable files (a `wght` range, `wdth` and the other axes read
//! through `axes`/`wdth`/`wght`/`slnt`) or a static set (one file per weight):
//! [`pick`] chooses the file for a weight and italic. A document naming a
//! family that is not installed draws and measures its text in the default
//! face ([`drawn_family`]), said once a session ([`missing_notices`]).
//! Nothing here downloads: this is the table and where the files are.

use makepad_doc_font_names as names;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// What a file of a family is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontFileRole {
    /// A face: the `wght` values it covers (one value for a static file,
    /// the axis range for a variable one) and whether it is the italic.
    Face { weight: [f64; 2], italic: bool },
    /// The licence text, kept beside the faces.
    Licence,
}

/// Where a file comes from.
#[derive(Clone, Copy, Debug)]
pub enum Place {
    /// A crate resource, `crate:path` as `crate_resource` names it.
    Resource(&'static str),
    /// A pinned download.
    Download {
        /// Where it lands under the weights folder, '/'-separated
        /// (`fonts/<family>/<file>`): also its id in install records.
        cache_as: &'static str,
        /// The URL it is fetched from, verbatim (a release tag or commit, so
        /// the bytes never move under the pinned hash).
        url: &'static str,
        bytes: u64,
        sha256: &'static str,
    },
}

/// One file of a family.
#[derive(Clone, Copy, Debug)]
pub struct FontFile {
    pub place: Place,
    pub role: FontFileRole,
}

impl FontFile {
    /// Where a download lands (`fonts/<family>/<file>`); empty for a resource.
    pub fn cache_as(&self) -> &'static str {
        match self.place {
            Place::Download { cache_as, .. } => cache_as,
            Place::Resource(_) => "",
        }
    }
}

/// A font family a document can name.
#[derive(Debug)]
pub struct Family {
    /// The name a document writes (`font: Font{family: "Archivo"}`); compared without
    /// case, spaces or dashes.
    pub name: &'static str,
    /// Other names for it.
    pub aliases: &'static [&'static str],
    /// Bundled with Makepad (else downloaded: Stage's Resources).
    pub bundled: bool,
    /// The text style's ascender shift (the face's own metrics sit a little
    /// off for some of the bundled sans faces).
    pub ascent: f32,
    /// Whether Inter stands behind it for the arrows and symbols it lacks.
    pub symbols: bool,
    /// What it is for (the guide's font list).
    pub role: &'static str,
    pub license: &'static str,
    /// The licence text as the family publishes it (downloaded families).
    pub license_text: &'static str,
    /// Where the files come from, in words (downloaded families).
    pub origin: &'static str,
    pub files: &'static [FontFile],
}

const OFL: &str = "SIL Open Font License 1.1";

macro_rules! face {
    ($res:expr, $weight:expr, $italic:expr) => {
        FontFile { place: Place::Resource($res), role: FontFileRole::Face { weight: [$weight.0, $weight.1], italic: $italic } }
    };
}

/// A file of google/fonts' `ofl/` tree at a pinned commit.
macro_rules! google_fonts {
    ($path:literal) => {
        concat!("https://raw.githubusercontent.com/google/fonts/9710da1eacb3be272583c3224dcb70f9da6eadbb/ofl/", $path)
    };
}
/// A file of IBM Plex Mono's release tag.
macro_rules! plex_mono {
    ($($path:literal),+) => {
        concat!("https://raw.githubusercontent.com/IBM/plex/%40ibm/plex-mono%401.1.0/packages/plex-mono/", $($path),+)
    };
}
macro_rules! plex_face {
    ($file:literal, $bytes:literal, $sha:literal, $weight:literal, $italic:literal) => {
        FontFile {
            place: Place::Download {
                cache_as: concat!("fonts/ibm-plex-mono/", $file),
                url: plex_mono!("fonts/complete/ttf/", $file),
                bytes: $bytes,
                sha256: $sha,
            },
            role: FontFileRole::Face { weight: [$weight, $weight], italic: $italic },
        }
    };
}

/// Every font family a document can name: the bundled ones first (a name a
/// bundled family answers to keeps meaning it, so existing Splash means what
/// it did), then the downloadable ones.
pub const FAMILIES: &[Family] = &[
    Family {
        name: names::INTER.name,
        aliases: names::INTER.aliases,
        bundled: true,
        ascent: -0.1,
        symbols: false,
        role: "sans, weights 100–900: the default; UI, captions, headlines",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_widgets:resources/Inter.ttf", (100.0, 900.0), false)],
    },
    Family {
        name: names::ROBOTO_FLEX.name,
        aliases: names::ROBOTO_FLEX.aliases,
        bundled: true,
        ascent: -0.1,
        symbols: true,
        role: "sans, weights 100–1000 with `stretch` 25–151: condensed or extended display type",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_widgets:resources/RobotoFlex.ttf", (100.0, 1000.0), false)],
    },
    Family {
        name: names::IBM_PLEX_SANS.name,
        aliases: names::IBM_PLEX_SANS.aliases,
        bundled: true,
        ascent: -0.1,
        symbols: true,
        role: "humanist sans, 400/600 and italics: editorial, technical",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[
            face!("makepad_widgets:resources/IBMPlexSans-Text.ttf", (400.0, 400.0), false),
            face!("makepad_widgets:resources/IBMPlexSans-SemiBold.ttf", (600.0, 600.0), false),
            face!("makepad_widgets:resources/IBMPlexSans-Italic.ttf", (400.0, 400.0), true),
            face!("makepad_widgets:resources/IBMPlexSans-BoldItalic.ttf", (600.0, 600.0), true),
        ],
    },
    Family {
        name: names::NOTO_SANS.name,
        aliases: names::NOTO_SANS.aliases,
        bundled: true,
        ascent: -0.1,
        symbols: true,
        role: "neutral sans, 400: long text, many scripts",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_widgets:resources/NotoSans-Regular.ttf", (400.0, 400.0), false)],
    },
    Family {
        name: names::JETBRAINS_MONO.name,
        aliases: names::JETBRAINS_MONO.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "monospace, 100–800: code, data, typewriter",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_widgets:resources/jetbrains_mono_variable.ttf", (100.0, 800.0), false)],
    },
    Family {
        name: names::LIBERATION_MONO.name,
        aliases: names::LIBERATION_MONO.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "monospace, 400: plain terminal look",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_widgets:resources/LiberationMono-Regular.ttf", (400.0, 400.0), false)],
    },
    Family {
        name: names::NEW_COMPUTER_MODERN.name,
        aliases: names::NEW_COMPUTER_MODERN.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: false,
        role: "serif (Computer Modern roman), 400: classic, academic",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_widgets:resources/NewCMMath-Regular.otf", (400.0, 400.0), false)],
    },
    Family {
        name: names::LXGW_WENKAI.name,
        aliases: names::LXGW_WENKAI.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "brush-drawn humanist letters, 400/700 (and CJK): warm, handwritten display",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[
            face!("makepad_widgets:resources/LXGWWenKaiRegular.ttf", (400.0, 400.0), false),
            face!("makepad_widgets:resources/LXGWWenKaiBold.ttf", (700.0, 700.0), false),
        ],
    },
    Family {
        name: names::PLAYFAIR_DISPLAY.name,
        aliases: names::PLAYFAIR_DISPLAY.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "high-contrast display serif, 400–900 and italic: hero titles, luxury, editorial headlines",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[
            face!("makepad_doc_fonts:resources/fonts/playfair-display/PlayfairDisplay-Variable.ttf", (400.0, 900.0), false),
            face!("makepad_doc_fonts:resources/fonts/playfair-display/PlayfairDisplay-Italic-Variable.ttf", (400.0, 900.0), true),
        ],
    },
    Family {
        name: names::CORMORANT_GARAMOND.name,
        aliases: names::CORMORANT_GARAMOND.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "refined Garamond serif, 300–700 (no italic): quotes, literary and elegant text",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_doc_fonts:resources/fonts/cormorant-garamond/CormorantGaramond-Variable.ttf", (300.0, 700.0), false)],
    },
    Family {
        name: names::ANTON.name,
        aliases: names::ANTON.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "heavy condensed display sans, one weight: loud all-caps punches, posters, sports",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_doc_fonts:resources/fonts/anton/Anton-Regular.ttf", (400.0, 400.0), false)],
    },
    Family {
        name: names::STIX_TWO_MATH.name,
        aliases: names::STIX_TWO_MATH.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "math font (OpenType MATH table), 400: the face the Math layer typesets formulas in",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_doc_fonts:resources/fonts/stix-two-math/STIXTwoMath-Regular.otf", (400.0, 400.0), false)],
    },
    Family {
        name: names::LEXEND_ZETTA.name,
        aliases: names::LEXEND_ZETTA.aliases,
        bundled: true,
        ascent: 0.0,
        symbols: true,
        role: "very wide (expanded) geometric grotesk, 100–900: logo slams and stings, wide all-caps titles",
        license: OFL,
        license_text: "",
        origin: "",
        files: &[face!("makepad_doc_fonts:resources/fonts/lexend-zetta/LexendZetta-Variable.ttf", (100.0, 900.0), false)],
    },
    Family {
        name: names::ARCHIVO.name,
        aliases: names::ARCHIVO.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "grotesque sans, variable weight 100–900 and width (wdth) 62–125, with italics: condensed to wide headlines, sports, editorial",
        license: OFL,
        license_text: include_str!("licences/archivo-OFL.txt"),
        origin: "Omnibus-Type's Archivo as google/fonts ships it (ofl/archivo, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/archivo/Archivo-Variable.ttf", url: google_fonts!("archivo/Archivo%5Bwdth,wght%5D.ttf"), bytes: 658_596, sha256: "0e094a7d3c7c4c25cf1310c4b30014f1dae9332220b1c2c88f4fa996f0b05053" },
                role: FontFileRole::Face { weight: [100.0, 900.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/archivo/Archivo-Italic-Variable.ttf", url: google_fonts!("archivo/Archivo-Italic%5Bwdth,wght%5D.ttf"), bytes: 741_368, sha256: "305d13b4eb80e62f7d517f78d7c045250d977552a420f2dbed906f314e761305" },
                role: FontFileRole::Face { weight: [100.0, 900.0], italic: true },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/archivo/OFL.txt", url: google_fonts!("archivo/OFL.txt"), bytes: 4_388, sha256: "108b4e57c9c796d3d38d0428ca7ee39de47ad93187302718d9b2d8864b9b716b" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::IBM_PLEX_MONO.name,
        aliases: names::IBM_PLEX_MONO.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "monospace, static weights 100–700 (and Text 450) with italics: code, data, technical captions",
        license: OFL,
        license_text: include_str!("licences/ibm-plex-mono-OFL.txt"),
        origin: "IBM Plex Mono, IBM/plex release @ibm/plex-mono@1.1.0 (the release tag's TrueType files)",
        files: &[
            plex_face!("IBMPlexMono-Thin.ttf", 158_412, "34ce19c385afdd31726866c4797314f78ae59de41da04e898e4b3a04fc709ecd", 100.0, false),
            plex_face!("IBMPlexMono-ThinItalic.ttf", 168_188, "059d9f9bdd35a26bbdfd8e68ccc18a4a5fe4f9af22cbc80509206936583f122c", 100.0, true),
            plex_face!("IBMPlexMono-ExtraLight.ttf", 156_344, "9c84b764bfc85441f53ce5d261c369156b0612a02837f1483ae525916c846486", 200.0, false),
            plex_face!("IBMPlexMono-ExtraLightItalic.ttf", 166_752, "2c168787c187535d0d42e2150e10841887e0f94bddc0ebd0ee936520621ca854", 200.0, true),
            plex_face!("IBMPlexMono-Light.ttf", 155_564, "f2a7e41a2bb183a1ba82b415eb176ac2dd81d2ca9fc8d2a2c23e5d413b89540e", 300.0, false),
            plex_face!("IBMPlexMono-LightItalic.ttf", 165_404, "14c3e18514d64a95b82cacf8a6d77a173fadff92c90aed9905faf9a71fa83876", 300.0, true),
            plex_face!("IBMPlexMono-Regular.ttf", 155_940, "fe11304a5fe956d5744e9b6a246cc83d90425245e75a62230044966ca96a7f50", 400.0, false),
            plex_face!("IBMPlexMono-Italic.ttf", 165_880, "8ebe04c8c6cc82f0be19896ddc61d9935cdd0f027b0173c1945b8d247d7dfc2a", 400.0, true),
            plex_face!("IBMPlexMono-Text.ttf", 155_748, "650b37d83353821b19000dc8db573e27290aa82bb3b5e7366613eaa7260ca0fe", 450.0, false),
            plex_face!("IBMPlexMono-TextItalic.ttf", 165_040, "fd037a88a0f0b29b95db086ee50450a69ac3a7cbb752ed286fca23d65711bc9c", 450.0, true),
            plex_face!("IBMPlexMono-Medium.ttf", 156_996, "0bede3debdea8488bbb927f8f0650d915073209734a67fe8cd5a3320b572511c", 500.0, false),
            plex_face!("IBMPlexMono-MediumItalic.ttf", 165_820, "71bd1f5f16fa0d10b101e050c67db3a2276f274e59cccfb3e9f9af3fc007a5a3", 500.0, true),
            plex_face!("IBMPlexMono-SemiBold.ttf", 157_552, "c9417148ce13f8fa7d2d5c9180bbc141f72aa0d814ffeb280f6904dc2b1bbd7a", 600.0, false),
            plex_face!("IBMPlexMono-SemiBoldItalic.ttf", 166_200, "7b4b32e3b8beb4fda5605a619671e61c27efc98f64fdc078ce225556f40aa8c5", 600.0, true),
            plex_face!("IBMPlexMono-Bold.ttf", 157_924, "ca403c56931baef307d20ba64b69acb71abcad61f75e66414661d57484b690ec", 700.0, false),
            plex_face!("IBMPlexMono-BoldItalic.ttf", 166_328, "0e45a5a540992163229d2a29662553f313fab391757ca2ab3dc8f4e0d9be0979", 700.0, true),
            FontFile {
                place: Place::Download { cache_as: "fonts/ibm-plex-mono/LICENSE.txt", url: plex_mono!("LICENSE.txt"), bytes: 4_456, sha256: "7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::CORMORANT.name,
        aliases: names::CORMORANT.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "display Garamond serif, variable weight 300–700 with italics: elegant titles, quotes, luxury",
        license: OFL,
        license_text: include_str!("licences/cormorant-OFL.txt"),
        origin: "Christian Thalmann's Cormorant (CatharsisFonts) as google/fonts ships it (ofl/cormorant, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/cormorant/Cormorant-Variable.ttf", url: google_fonts!("cormorant/Cormorant%5Bwght%5D.ttf"), bytes: 572_892, sha256: "8f12cb21f05b61649192eaff13eeeb1b5619bc524feeae672fb916974259a076" },
                role: FontFileRole::Face { weight: [300.0, 700.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/cormorant/Cormorant-Italic-Variable.ttf", url: google_fonts!("cormorant/Cormorant-Italic%5Bwght%5D.ttf"), bytes: 350_096, sha256: "2c4e1c43fa126b51a84160815b9264af442c6c531d13bfd3c1723703cd489dd2" },
                role: FontFileRole::Face { weight: [300.0, 700.0], italic: true },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/cormorant/OFL.txt", url: google_fonts!("cormorant/OFL.txt"), bytes: 4_387, sha256: "60700d351cac4650c51f3f9db318d2a420f8b45052dba2715eb5fec41f0f6956" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::MONTSERRAT.name,
        aliases: names::MONTSERRAT.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "geometric sans, variable weight 100–900, with italics: bold titles, posters, headlines",
        license: OFL,
        license_text: include_str!("licences/montserrat-OFL.txt"),
        origin: "Julieta Ulanovsky's Montserrat as google/fonts ships it (ofl/montserrat, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/montserrat/Montserrat-Variable.ttf", url: google_fonts!("montserrat/Montserrat%5Bwght%5D.ttf"), bytes: 744_936, sha256: "0f7b311b2f3279e4eef9b2f968bcdbab6e28f4daeb1f049f4f278a902bcd82f7" },
                role: FontFileRole::Face { weight: [100.0, 900.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/montserrat/Montserrat-Italic-Variable.ttf", url: google_fonts!("montserrat/Montserrat-Italic%5Bwght%5D.ttf"), bytes: 762_376, sha256: "51607f316bc020e59f03cbf51543eecffbea501c0b31d73e5b82927c5cca442c" },
                role: FontFileRole::Face { weight: [100.0, 900.0], italic: true },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/montserrat/OFL.txt", url: google_fonts!("montserrat/OFL.txt"), bytes: 4_400, sha256: "8b7141c03fa4f8d44e6345d5d4931709290f0f67875e452e95ac1fd3a027802e" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::BEBAS_NEUE.name,
        aliases: names::BEBAS_NEUE.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "tall condensed all-caps display sans, one weight: headlines, posters, big type",
        license: OFL,
        license_text: include_str!("licences/bebas-neue-OFL.txt"),
        origin: "Dharma Type's Bebas Neue as google/fonts ships it (ofl/bebasneue, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/bebas-neue/BebasNeue-Regular.ttf", url: google_fonts!("bebasneue/BebasNeue-Regular.ttf"), bytes: 61_400, sha256: "08e4623805102d819f58601e46e345648846075e363b2ceb23313c2d1c83ec73" },
                role: FontFileRole::Face { weight: [400.0, 400.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/bebas-neue/OFL.txt", url: google_fonts!("bebasneue/OFL.txt"), bytes: 4_337, sha256: "72082f6cb4d04be2ecf7cc7d9e1e7d73787f0af8a5a278a47cade70c16b78341" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::POPPINS.name,
        aliases: names::POPPINS.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "geometric sans, static weights 400–900 (with an italic): friendly bold titles, labels",
        license: OFL,
        license_text: include_str!("licences/poppins-OFL.txt"),
        origin: "Indian Type Foundry's Poppins as google/fonts ships it (ofl/poppins, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-Regular.ttf", url: google_fonts!("poppins/Poppins-Regular.ttf"), bytes: 160_316, sha256: "7e65201e9b79159e2300267cc885e16c8dcef2424cdfa09a29bfb0980a94a7ba" },
                role: FontFileRole::Face { weight: [400.0, 400.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-Italic.ttf", url: google_fonts!("poppins/Poppins-Italic.ttf"), bytes: 184_544, sha256: "4fa76ae75b40f926420514044722cb97f32186cafd3b38263cc34dad7174d46d" },
                role: FontFileRole::Face { weight: [400.0, 400.0], italic: true },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-Medium.ttf", url: google_fonts!("poppins/Poppins-Medium.ttf"), bytes: 158_576, sha256: "90373e7d838d32468438fc3e152dca0bdb12edcab99ea639f158790b1ba1fd05" },
                role: FontFileRole::Face { weight: [500.0, 500.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-SemiBold.ttf", url: google_fonts!("poppins/Poppins-SemiBold.ttf"), bytes: 157_312, sha256: "d3bf1bdaf0550e83da9ac0b1d1d9fe6db086835a83aa28578e609a394b9a0286" },
                role: FontFileRole::Face { weight: [600.0, 600.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-Bold.ttf", url: google_fonts!("poppins/Poppins-Bold.ttf"), bytes: 155_996, sha256: "983676516167748b74de6f4771fb384c664fd913acb8b471122ecacf5da5ea6c" },
                role: FontFileRole::Face { weight: [700.0, 700.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-ExtraBold.ttf", url: google_fonts!("poppins/Poppins-ExtraBold.ttf"), bytes: 154_836, sha256: "f2ab17c1a63a0ecc12c2461848fc8a469395e3cd2d641803e889c643d9f958e1" },
                role: FontFileRole::Face { weight: [800.0, 800.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/Poppins-Black.ttf", url: google_fonts!("poppins/Poppins-Black.ttf"), bytes: 153_440, sha256: "d82aaaf98a9283f9a8edd24e51173337d8eaf09e25cd3d98831f8ec8461748a1" },
                role: FontFileRole::Face { weight: [900.0, 900.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/poppins/OFL.txt", url: google_fonts!("poppins/OFL.txt"), bytes: 4_385, sha256: "6be04893d770899a015649c7aa3b582f871b272f8747a92b78b17c3e5c8b2573" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::OSWALD.name,
        aliases: names::OSWALD.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "condensed gothic sans, variable weight 200–700: tall headlines, sports, signage",
        license: OFL,
        license_text: include_str!("licences/oswald-OFL.txt"),
        origin: "Vernon Adams' Oswald as google/fonts ships it (ofl/oswald, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/oswald/Oswald-Variable.ttf", url: google_fonts!("oswald/Oswald%5Bwght%5D.ttf"), bytes: 172_088, sha256: "5b38c246e255a12f5712d640d56bcced0472466fc68983d2d0410ec0457c2817" },
                role: FontFileRole::Face { weight: [200.0, 700.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/oswald/OFL.txt", url: google_fonts!("oswald/OFL.txt"), bytes: 4_483, sha256: "0fd731a904b729a4e02eaf5e8ebd06783edd9abe400e8882760160230675b652" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::LEAGUE_SPARTAN.name,
        aliases: names::LEAGUE_SPARTAN.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "geometric sans, variable weight 100–900: heavy display titles, logos",
        license: OFL,
        license_text: include_str!("licences/league-spartan-OFL.txt"),
        origin: "The League of Moveable Type's League Spartan as google/fonts ships it (ofl/leaguespartan, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/league-spartan/LeagueSpartan-Variable.ttf", url: google_fonts!("leaguespartan/LeagueSpartan%5Bwght%5D.ttf"), bytes: 95_116, sha256: "2dbb6290b39ab7c48a40b18f74ca59ef48a69a015c3ea0542703f0c6ce51d617" },
                role: FontFileRole::Face { weight: [100.0, 900.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/league-spartan/OFL.txt", url: google_fonts!("leaguespartan/OFL.txt"), bytes: 4_402, sha256: "41c9980cacc325ccd1afa6553bcae038cc6c74fa519aff60bacb99e3854292ce" },
                role: FontFileRole::Licence,
            },
        ],
    },
    Family {
        name: names::OPEN_SANS.name,
        aliases: names::OPEN_SANS.aliases,
        bundled: false,
        ascent: 0.0,
        symbols: true,
        role: "humanist sans, variable weight 300–800 and width 75–100, with italics: clear body copy and calm titles",
        license: OFL,
        license_text: include_str!("licences/open-sans-OFL.txt"),
        origin: "Steve Matteson's Open Sans as google/fonts ships it (ofl/opensans, pinned commit)",
        files: &[
            FontFile {
                place: Place::Download { cache_as: "fonts/open-sans/OpenSans-Variable.ttf", url: google_fonts!("opensans/OpenSans%5Bwdth,wght%5D.ttf"), bytes: 532_636, sha256: "36643644f318a812aab2d2ed3bb98f8cf0872527f835fe9398d95fe6b9adb878" },
                role: FontFileRole::Face { weight: [300.0, 800.0], italic: false },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/open-sans/OpenSans-Italic-Variable.ttf", url: google_fonts!("opensans/OpenSans-Italic%5Bwdth,wght%5D.ttf"), bytes: 583_992, sha256: "fe269381e992f32e135801740998544d6235061e37c93ec067ad2be3edd5b17b" },
                role: FontFileRole::Face { weight: [300.0, 800.0], italic: true },
            },
            FontFile {
                place: Place::Download { cache_as: "fonts/open-sans/OFL.txt", url: google_fonts!("opensans/OFL.txt"), bytes: 4_389, sha256: "fbbbcfef55318de350562559b671360de6d597112ecc5c73881b05092db89602" },
                role: FontFileRole::Licence,
            },
        ],
    },
];

/// The catalog entry for the document family or alias `name`.
pub fn family(name: &str) -> Option<&'static Family> {
    let canonical = names::family(name)?.name;
    FAMILIES.iter().find(|family| family.name == canonical)
}

/// The bundled families' names.
pub fn bundled() -> impl Iterator<Item = &'static str> {
    FAMILIES.iter().filter(|f| f.bundled).map(|f| f.name)
}

/// How many downloadable families there are.
pub const fn downloadable_count() -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < FAMILIES.len() {
        if !FAMILIES[index].bundled {
            count += 1;
        }
        index += 1;
    }
    count
}

/// The downloadable families (Stage's Resources, in table order).
pub fn downloadable() -> impl Iterator<Item = &'static Family> {
    FAMILIES.iter().filter(|f| !f.bundled)
}

/// The directory a crate's resources are read from, by crate name: the
/// crate `makepad_widgets` of a font named `makepad_widgets:resources/Inter.ttf`.
fn roots() -> &'static RwLock<HashMap<String, PathBuf>> {
    static ROOTS: std::sync::OnceLock<RwLock<HashMap<String, PathBuf>>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| {
        // This crate's own resources, where the crate is, as `crate_resource`
        // finds any crate's.
        // The tests have no widget library to ask.
        RwLock::new(HashMap::from([
            ("makepad_doc_fonts".to_string(), PathBuf::from(DIR)),
            #[cfg(any(test, feature = "dev-roots"))]
            ("makepad_widgets".to_string(), PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../widgets")),
        ]))
    })
}

/// This crate's directory: its `resources/fonts` are the bundled display faces.
pub const DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Told of a crate root: its crate name and directory.
pub type RootObserver = fn(&str, &Path);

static OBSERVERS: RwLock<Vec<RootObserver>> = RwLock::new(Vec::new());

/// Hear of every crate root: each one known now, at once, and each one a
/// host registers later, before it is used.
pub fn observe_roots(observer: RootObserver) {
    let known: Vec<(String, PathBuf)> = roots().read().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    OBSERVERS.write().unwrap_or_else(|e| e.into_inner()).push(observer);
    for (name, dir) in &known {
        observer(name, dir);
    }
}

/// Say where a crate's resources are (`makepad_widgets` is the widget
/// library's directory: `makepad_widgets::widgets_dir()`). A host calls it
/// once, before documents load.
pub fn register_root(crate_name: &str, dir: impl Into<PathBuf>) {
    let dir = dir.into();
    for observer in OBSERVERS.read().unwrap_or_else(|e| e.into_inner()).iter() {
        observer(crate_name, &dir);
    }
    roots().write().unwrap_or_else(|e| e.into_inner()).insert(crate_name.to_string(), dir);
}

/// Where a crate's resources are, as the host said.
pub fn root_of(crate_name: &str) -> Option<PathBuf> {
    roots().read().unwrap_or_else(|e| e.into_inner()).get(crate_name).cloned()
}

/// The path of a resource file (`crate:path`), or the path as the browser
/// player keys it (`crate/path`) when its crate has no registered root. In
/// the browser a crate's resources are the page's `<crate>/<path>` (a root
/// there names the build machine's folder).
pub fn resource_path(resource: &str) -> PathBuf {
    let (krate, rel) = resource.split_once(':').unwrap_or(("", resource));
    #[cfg(target_arch = "wasm32")]
    return PathBuf::from(krate).join(rel);
    #[cfg(not(target_arch = "wasm32"))]
    match roots().read().unwrap_or_else(|e| e.into_inner()).get(krate) {
        Some(root) => root.join(rel),
        None => PathBuf::from(krate).join(rel),
    }
}

/// The AI Hub's weights folder: `MAKEPAD_ASSET_AI_CACHE`, else the Makepad
/// home's `weights/` (`MAKEPAD_HOME`, else `~/.makepad`), as the installer
/// (`makepad_ai_hub::home`) resolves it.
pub fn weights_root() -> PathBuf {
    let set = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty()).map(PathBuf::from);
    set("MAKEPAD_ASSET_AI_CACHE").unwrap_or_else(|| {
        set("MAKEPAD_HOME")
            .or_else(|| set("USERPROFILE").or_else(|| set("HOME")).map(|home| home.join(".makepad")))
            .unwrap_or_else(|| std::env::temp_dir().join(".makepad"))
            .join("weights")
    })
}

/// Where the installer puts a downloaded file.
pub fn install_path(file: &FontFile) -> PathBuf {
    let mut path = weights_root();
    path.extend(file.cache_as().split('/'));
    path
}

/// A file on this machine. A bundled one is where its crate puts it. A
/// downloaded one is under `MAKEPAD_FONTS_DIR` when that is set (a folder
/// laid out like the cache's `fonts/`, taken on trust: a test's or an
/// operator's own copies), else the installer's file at its pinned size (a
/// shorter one is a download that stopped). In the browser: the page's
/// `fonts/...` file, when it has one.
pub fn local_path(file: &FontFile) -> Option<PathBuf> {
    let Place::Download { cache_as, bytes, .. } = file.place else {
        let Place::Resource(resource) = file.place else { return None };
        return Some(resource_path(resource));
    };
    // The page's `fonts/…` (the cache's layout), when it serves the file.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = bytes;
        let path = PathBuf::from(cache_as);
        makepad_asset_data::source::is_file(&path).then_some(path)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = std::env::var_os("MAKEPAD_FONTS_DIR").filter(|dir| !dir.is_empty()) {
            let mut path = PathBuf::from(dir);
            path.extend(cache_as.trim_start_matches("fonts/").split('/'));
            return path.is_file().then_some(path);
        }
        let path = install_path(file);
        std::fs::metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() == bytes).then_some(path)
    }
}

/// Every face of the family is on this machine (a bundled family always is:
/// its files come with the build).
pub fn installed(family: &Family) -> bool {
    family.bundled || faces(family).all(|file| local_path(file).is_some())
}

fn faces(family: &Family) -> impl Iterator<Item = &FontFile> {
    family.files.iter().filter(|f| matches!(f.role, FontFileRole::Face { .. }))
}

/// The face for a weight and italic: the italic files when asked for and
/// the family has them (else the roman ones), then the file whose weight
/// range is nearest (a variable file covers its whole range).
pub fn pick(family: &Family, weight: f64, italic: bool) -> Option<&FontFile> {
    let has_italic = faces(family).any(|f| matches!(f.role, FontFileRole::Face { italic: true, .. }));
    let want_italic = italic && has_italic;
    let distance = |f: &FontFile| match f.role {
        FontFileRole::Face { weight: [lo, hi], .. } => if weight < lo { lo - weight } else if weight > hi { weight - hi } else { 0.0 },
        FontFileRole::Licence => f64::MAX,
    };
    faces(family)
        .filter(|f| matches!(f.role, FontFileRole::Face { italic: i, .. } if i == want_italic))
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
}

/// The weight `family` draws at when `weight` is asked for: exactly
/// `weight` on a variable file (its `wght` axis takes any value in range);
/// a static file's own weight (the nearest face [`pick`] chose).
pub fn drawn_weight(family: &Family, weight: f64, italic: bool) -> f64 {
    let weight = weight.clamp(1.0, 1000.0);
    match pick(family, weight, italic) {
        Some(FontFile { role: FontFileRole::Face { weight: [lo, hi], .. }, .. }) if lo == hi => *lo,
        _ => weight,
    }
}

/// Whether a face file is variable on `wght` (it covers a range).
pub fn variable(file: &FontFile) -> bool {
    matches!(file.role, FontFileRole::Face { weight: [lo, hi], .. } if hi > lo)
}

/// A host's font bytes by file path, consulted before the file system: a
/// host without this machine's files (the browser player) hands over the
/// bytes it fetched. Installed once, at startup; `None` from it reads the
/// file as before.
pub type FontReader = dyn Fn(&Path) -> Option<std::sync::Arc<[u8]>> + Send + Sync;

static FONT_READER: std::sync::OnceLock<Box<FontReader>> = std::sync::OnceLock::new();

/// Install the host's font reader (once; a later call is ignored: false).
/// A Scene3D's Text3D reads its font files through it too ([`host_font`]).
pub fn set_font_reader(reader: Box<FontReader>) -> bool {
    FONT_READER.set(reader).is_ok()
}

/// The host's bytes of a font file, when it installed a reader and has them.
pub fn host_font(path: &Path) -> Option<std::sync::Arc<[u8]>> {
    FONT_READER.get().and_then(|reader| reader(path))
}

/// A font file's bytes: the host's reader, else the file (read through
/// `makepad_asset_data::source`: in the browser, the page's file).
pub fn read_font(path: &Path) -> Result<std::sync::Arc<[u8]>, String> {
    if let Some(bytes) = host_font(path) {
        return Ok(bytes);
    }
    makepad_asset_data::source::read(path).map(Into::into).map_err(|e| {
        let hint = if path.is_relative() { " (the host has not said where that crate's resources are: makepad_doc_fonts::register_root)" } else { "" };
        format!("{}: {e}{hint}", path.display())
    })
}

/// The family's own name, when its files are on this machine. `None` for a
/// name that is no family, or a downloadable family that is not installed
/// (whose text [`drawn_family`] draws in the default face).
pub fn document_font(name: &str) -> Option<&'static str> {
    let f = family(name)?;
    installed(f).then_some(f.name)
}

/// The bundled family text is drawn in when its own family is not installed.
pub const DEFAULT_FAMILY: &str = "Inter";

/// Downloadable families found installed, until [`installs_changed`] (the
/// draw asks for every text of every frame).
static FOUND: RwLock<Vec<&'static str>> = RwLock::new(Vec::new());

/// A host installed or removed font files (Stage's Resources): installed
/// families are looked for again.
pub fn installs_changed() {
    FOUND.write().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Downloadable families found missing this session, each said once.
static MISSING: RwLock<Vec<&'static str>> = RwLock::new(Vec::new());

/// The family a document font name draws and measures in: the named one
/// when its files are here, else (a downloadable family not installed) the
/// default family, so its text still lays out and draws; the first time a
/// session meets each missing family it says so ([`missing_notices`], and
/// on stderr). A missing family is looked for again at every ask: one
/// installed meanwhile (here or by another app) draws in its own face from
/// then on. `None` for a name that is no family.
pub fn drawn_family(name: &str) -> Option<&'static Family> {
    let f = family(name)?;
    if f.bundled || FOUND.read().unwrap_or_else(|e| e.into_inner()).contains(&f.name) {
        return Some(f);
    }
    if installed(f) {
        FOUND.write().unwrap_or_else(|e| e.into_inner()).push(f.name);
        return Some(f);
    }
    let first = {
        let mut missing = MISSING.write().unwrap_or_else(|e| e.into_inner());
        let first = !missing.contains(&f.name);
        if first {
            missing.push(f.name);
        }
        first
    };
    if first {
        eprintln!("{}", missing_notice(f.name));
    }
    family(DEFAULT_FAMILY)
}

fn missing_notice(name: &str) -> String {
    format!("font {name} is not installed: drawn with the default face; install it in Resources for the intended look")
}

/// This session's notices of fonts drawn in the default face: one per
/// missing family, in the order met.
pub fn missing_notices() -> Vec<String> {
    MISSING.read().unwrap_or_else(|e| e.into_inner()).iter().map(|name| missing_notice(name)).collect()
}

/// The file a document font reads from, by name, weight and italic: the
/// face of the family it draws in ([`drawn_family`]: the default family's
/// for one not installed). `None` for a name that is no family.
pub fn font_file(name: &str, weight: f64, italic: bool) -> Option<PathBuf> {
    local_path(pick(drawn_family(name)?, weight, italic)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_resolve_by_name_and_alias() {
        assert!(FAMILIES.iter().map(|f| (f.name, f.aliases))
            .eq(names::FAMILIES.iter().map(|f| (f.name, f.aliases))),
            "each document family has its catalog entry in lookup order");
        assert_eq!(family("archivo").map(|f| f.name), Some("Archivo"));
        assert_eq!(family("IBM-Plex Mono").map(|f| f.name), Some("IBM Plex Mono"));
        assert_eq!(family("plex mono").map(|f| f.name), Some("IBM Plex Mono"));
        assert_eq!(family("Cormorant").map(|f| f.name), Some("Cormorant"));
        // `cormorant` was the bundled Garamond's alias before: it still is.
        assert_eq!(family("cormorant").map(|f| f.name), Some("Cormorant Garamond"));
        assert_eq!(family("CORMORANT").map(|f| f.name), Some("Cormorant Garamond"));
        assert_eq!(document_font("cormorant"), Some("Cormorant Garamond"));
        assert_eq!(family("Cormorant Garamond").map(|f| f.name), Some("Cormorant Garamond"));
        assert_eq!(family("Inter").map(|f| f.name), Some("Inter"));
        // The names the bundled fonts answer to.
        for (name, want) in [
            ("inter", "Inter"), ("IBM-Plex Sans", "IBM Plex Sans"), ("monospace", "JetBrains Mono"), ("serif", "New Computer Modern"),
            ("handwritten", "LXGW WenKai"), ("display-serif", "Playfair Display"), ("editorial", "Cormorant Garamond"),
            ("condensed", "Anton"), ("anton", "Anton"), ("wide", "Lexend Zetta"), ("Lexend Zetta", "Lexend Zetta"), ("sans-serif", "Inter"),
        ] {
            assert_eq!(family(name).map(|f| f.name), Some(want), "{name}");
        }
        assert!(family("Comic Sans").is_none());
    }

    #[test]
    fn a_static_family_picks_the_nearest_weight_and_its_italic() {
        let plex = family("IBM Plex Mono").unwrap();
        let name = |w: f64, i: bool| pick(plex, w, i).unwrap().cache_as().rsplit('/').next().unwrap();
        assert_eq!(name(400.0, false), "IBMPlexMono-Regular.ttf");
        assert_eq!(name(700.0, true), "IBMPlexMono-BoldItalic.ttf");
        assert_eq!(name(640.0, false), "IBMPlexMono-SemiBold.ttf");
        assert_eq!(name(900.0, false), "IBMPlexMono-Bold.ttf");
        assert_eq!(name(50.0, true), "IBMPlexMono-ThinItalic.ttf");
        assert_eq!(name(450.0, false), "IBMPlexMono-Text.ttf");
    }

    #[test]
    fn a_variable_family_covers_its_range_with_one_file() {
        let archivo = family("Archivo").unwrap();
        for w in [100.0, 450.0, 900.0, 1000.0] {
            assert_eq!(pick(archivo, w, false).unwrap().cache_as(), "fonts/archivo/Archivo-Variable.ttf");
            assert_eq!(pick(archivo, w, true).unwrap().cache_as(), "fonts/archivo/Archivo-Italic-Variable.ttf");
        }
    }

    #[test]
    fn every_family_is_pinned_with_its_licence_under_its_own_folder() {
        for f in downloadable() {
            assert!(f.license_text.contains("SIL Open Font License"), "{}", f.name);
            assert_eq!(f.files.iter().filter(|x| x.role == FontFileRole::Licence).count(), 1, "{}", f.name);
            let folder = format!("fonts/{}/", f.name.to_lowercase().replace(' ', "-"));
            for file in f.files {
                let Place::Download { cache_as, url, bytes, sha256 } = file.place else { panic!("{} is not a download", f.name) };
                assert!(cache_as.starts_with(&folder), "{cache_as}");
                assert_eq!(sha256.len(), 64);
                assert!(sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(url.starts_with("https://raw.githubusercontent.com/"), "{url}");
                assert!(bytes > 1000);
            }
            assert!(pick(f, 400.0, false).is_some() && pick(f, 400.0, true).is_some());
        }
    }
}


/// What text a face must be able to show, beyond what a build tool sees it
/// draw: `@all` (every glyph: text whose content is open), named sets
/// (`"latin"`: Basic Latin and Latin-1, `"digits"`: numbers and their
/// punctuation), code point ranges (`"U+0020-U+007E"`) or literal
/// characters (`"chars:ÄÖÜ"`), comma-separated. A document declares it
/// on a Text (`glyphs: @all`) or a Draw2D text's options; a number Text
/// declares `digits` itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CoverageDecl {
    /// The document font name as written (`"Roboto Flex"`).
    pub family: String,
    pub spec: String,
}

static RECORD_COVERAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn coverage_queue() -> &'static (std::sync::mpsc::Sender<CoverageDecl>, std::sync::Mutex<std::sync::mpsc::Receiver<CoverageDecl>>) {
    static Q: std::sync::OnceLock<(std::sync::mpsc::Sender<CoverageDecl>, std::sync::Mutex<std::sync::mpsc::Receiver<CoverageDecl>>)> = std::sync::OnceLock::new();
    Q.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, std::sync::Mutex::new(rx))
    })
}

/// Keeps (or stops keeping) the coverage documents declare from now on,
/// on any thread (a build tool's analysis).
pub fn record_coverage(on: bool) {
    RECORD_COVERAGE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The coverage declared since the last call.
pub fn take_coverage() -> Vec<CoverageDecl> {
    let Ok(rx) = coverage_queue().1.lock() else { return Vec::new() };
    rx.try_iter().collect()
}

/// A document declared that `family` must show `spec`.
pub fn declare_coverage(family: &str, spec: &str) {
    if RECORD_COVERAGE.load(std::sync::atomic::Ordering::Relaxed) {
        coverage_queue().0.send(CoverageDecl { family: family.to_string(), spec: spec.to_string() }).ok();
    }
}

/// Whether `spec` is a coverage this module understands (see
/// [`CoverageDecl`]); the message says what is wrong.
pub fn check_coverage(spec: &str) -> Result<(), String> {
    for part in spec.split(',').map(str::trim) {
        let ok = match part {
            "all" | "latin" | "digits" => true,
            p if p.starts_with("chars:") => true,
            p => p.strip_prefix("U+").and_then(|r| r.split_once('-')).is_some_and(|(a, b)| {
                u32::from_str_radix(a, 16).is_ok() && u32::from_str_radix(b.trim_start_matches("U+"), 16).is_ok()
            }),
        };
        if !ok {
            return Err(format!("`{part}` is not a glyph coverage: use @all, \"latin\", \"digits\", \"U+0020-U+007E\" or \"chars:…\" (comma-separated)"));
        }
    }
    Ok(())
}
