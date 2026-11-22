//! Colour from a few filaments: a picture or a textured surface printed with the filaments loaded
//! (four, nine, any set), by placing them side by side at the scale of a bead and by stacking a
//! thin layer of one over another so the one under it shows through.
//!
//! The core knows no tiles and no layers in Z:
//! - [`oklab`]: linear light and the perceptual space colours are compared in.
//! - [`lab`]: CIE L*a*b* and CIEDE2000, the difference standard colour codes are matched by.
//! - [`optics`]: a filament's look as a layer of a thickness (Kubelka-Munk), and one layer over
//!   what lies under it.
//! - [`palette`]: a [`palette::FilamentSet`] (named, first guess or measured) and the inks it
//!   prints: every stack of its filaments a few levels deep over a backing.
//! - [`hull`], [`gamut`]: the colours the inks mix to side by side (their hull in linear light),
//!   mapping a wanted colour into it, and its volume in OKLab with what an extra filament adds.
//! - [`dither`]: choosing an ink per cell over any surface's cells (a [`dither::Cells`] graph),
//!   the error carried to the later cells, under the print's run and switch rules.
//!
//! Wrappers make cells of a surface: [`tile`] a flat picture printed face down, [`swatch`] the
//! calibration print every ink of a set is measured from, and [`read`] reads a photo of it back
//! into the set's measurements. [`image`] reads PNG, JPEG, WebP, GIF and BMP (the app's one decoder);
//! [`view`] shows a picture as it looks from a viewing distance and how far it looks from
//! another.

pub mod dither;
pub mod gamut;
pub mod hull;
pub mod image;
pub mod lab;
pub mod oklab;
pub mod optics;
pub mod palette;
pub mod read;
pub mod spectrum;
pub mod swatch;
pub mod tile;
pub mod view;

pub use dither::{dither, Cells, Rules};
pub use image::{decode_rgba, Image};
pub use palette::{Backing, Filament, FilamentSet, Ink, Palette, StackModel};
pub use tile::{Face, Tile, TileSpec};
