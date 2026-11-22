//! Fab's resin slicer.
//!
//! A masked resin printer cures each layer at once: a screen under the vat lights the layer's
//! cross-section through its pixels, and the part grows downward from the build plate it hangs
//! from. So the slice of a plate is a stack of images, one per layer, instead of toolpaths:
//! - [`printer`]: the printer's profile (screen, height, exposures, peel), read from the same
//!   settings model as the filament printers, with a simulated printer for the tests;
//! - [`grid`]: the screen's pixels on the bed and the layers' heights;
//! - [`coverage`]: each pixel's covered share (the anti-aliasing), from a distance or from
//!   outlines;
//! - [`section`]: a part's cross-section on the pixels from its distance field or its mesh,
//!   behind one query;
//! - [`spans`]: a layer as runs of covered pixels per row, the one format everything reads;
//! - [`image`]: the plate's layers, one on demand or all of them in parallel;
//! - [`parts`]: a layer's cured pixels as parts, what each stands on below, the holes they close
//!   in and the features lit too dimly to cure;
//! - [`island`]: the points an island's shape gets;
//! - [`points`]: the support points, from one walk up the layers;
//! - [`tri`]: a mesh's signed distance, for the mesh sections' queries;
//! - [`solid`]: the supports' solids (balls' hulls, cut cones, the pad), each with its exact
//!   distance and a mesh;
//! - [`clear`]: whether a strut clears the parts, one exact march for fields and meshes alike;
//! - [`load`]: what the supports carry as the print peels off the film, and what holds it;
//! - [`supports`]: the heads, pillars, bridges, bases and pad on the points, and their section;
//! - [`cups`]: the pockets that seal against the film as it peels;
//! - [`rules`]: the rules a print keeps as it hangs from the plate, and the one check of them;
//! - [`estimate`]: the print's time and its resin;
//! - [`export`]: the layers written out, an image per layer as the screen shows it, and a
//!   manifest of the job;
//! - [`archive`]: the layers written as the one archive a printer reads from a USB stick;
//! - [`calibration`]: the orientation test, an F that says whether prints come out mirrored;
//! - [`link`]: the exported stack sent to a printer, and its job started, held and followed.

pub mod archive;
pub mod calibration;
pub mod clear;
pub mod coverage;
pub mod cups;
pub mod estimate;
pub mod export;
pub mod grid;
pub mod image;
pub mod island;
pub mod link;
pub mod load;
pub mod parts;
pub mod points;
pub mod printer;
pub mod rules;
pub mod section;
pub mod solid;
pub mod spans;
pub mod supports;
pub mod tri;

#[cfg(test)]
mod point_tests;
#[cfg(test)]
mod rule_tests;
#[cfg(test)]
mod support_tests;
#[cfg(test)]
mod tests;
