//! Placement foundations: a board snapshot with movable and fixed pins, and incremental
//! wirelength and ratsnest-crossing costs. The legaliser and constructive placer build on
//! this model and use the board's shared placement checks for candidate poses.

mod cluster;
pub mod congest;
mod cost;
mod free;
pub(crate) mod job;
mod legal;
mod problem;
mod seed;

pub use cluster::{clusters, identical_instances, Clusters, Instance, Pull};
pub use cost::{Cost, CostTerms};
pub use free::{rooms_overlap, Free};
pub use fab_elec_board::edge_parts_of as edge_parts;
pub use legal::{legalise, LegalOpts, LegalReport};
pub use problem::{BodyShape, Movable, Obstacle, PNet, PinRef, PlaceProblem, Pose};
pub use seed::{seed, seed_watched, ConformRow, ConformStatus, DemandPasses, RoundScore, SeatBlocker, SeatCandidate, SeatGuard, SeatTrial, SeedOpts, SeedReport, SeedRound, SeedWatch};

#[cfg(test)]
mod tests;
