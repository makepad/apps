//! Motion presentation data and time sampling.

pub mod plain;
pub use plain::{PlainDoc, PlainFrame};
pub mod frame;
pub mod geometry;
pub mod model;
pub mod timing;
pub mod placement;
pub mod sample;
mod text;
pub use text::{is_cjk, unit_range_iter, unit_ranges};

pub use frame::{Affine, Local, PlateFrame, TextUnit};
pub use makepad_live_id::LiveId;
pub use makepad_tween::named::{Curve, Ease};
pub use model::*;
pub use sample::{bezier_at, key_handles, sample_track};

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

/// The share of a document's width inside its side margins (6% each
/// side): what `wrap: @safe` and `fit: @safe` mean, and the width
/// `inspect` holds text to.
pub const SAFE_WIDTH: f64 = 0.88;
