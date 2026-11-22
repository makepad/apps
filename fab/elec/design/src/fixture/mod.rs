//! Rust fixtures and their shared parts and footprints.
//! The ledctl board is built from its documents in `fab-elec-fixtures`;
//! sbc, devboard and the controller's probe are still Rust builders.

pub mod build;
pub mod devboard;
pub mod fp;
pub mod parts;
pub mod probe;
pub mod sbc;

pub use devboard::devboard;
pub use sbc::sbc;
