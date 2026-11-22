//! Fab's scan: a headset capture session read into posed views ([`session`]), what is
//! reconstructed from them fitted to the object's model ([`fit`]), and the scan as it shows while
//! it is captured ([`live`]), with the detail its pictures give ([`detail`], [`refine`]).

pub mod colour;
pub mod detail;
pub mod fit;
pub mod live;
pub mod refine;
pub mod session;
