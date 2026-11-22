//! The pieces explorer maps share, app-neutral: Scope's code map and Fab's
//! circuit explorer both navigate a large flat world the same way.
//!
//! - `hop`: van Wijk and Nuij's smooth zoom-and-pan flight between two
//!   cameras, and its duration rule.
//! - `pan_fling`: the release velocity of a drag and the exponential coast.
//! - `wheel_zoom`: wheel ticks as short constant log-zoom glides.
//!
//! Pure math over `makepad_math`: no `Cx`, no frames, no widgets. Nothing here
//! runs unless a camera moves; each call is O(1) (the fling tracker keeps the
//! samples of the last 60 ms).

pub mod hop;
pub mod pan_fling;
pub mod wheel_zoom;
