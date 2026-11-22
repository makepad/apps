//! The support removal check: what the planner will print as supports, as loose rigid pieces
//! already cut free of the part. Whether a piece can physically leave the part, shown as a
//! verdict; and the pieces as bodies for the Mechanics sim, to shake and pull out. It says
//! nothing about how hard they are to break free: only what happens once they are.
//!
//! Everything is in the plate's frame, in mm, from the plan's own support geometry, never from
//! its paths.
//!
//! The same slice says what of a part prints apart ([`apart`]): the pieces Mechanics moves a field
//! part as, each with its mass and hulls from its outlines and drawn and met as the part's field
//! within its mask.

pub mod apart;
pub mod bodies;
pub mod grid;
pub mod mesh;
pub mod pieces;
pub mod solid;
pub mod truth;
pub mod verdict;
