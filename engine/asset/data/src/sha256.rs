//! The content contract names bytes by SHA-256 digest. The implementation is
//! the shared one in `makepad-core-util` (streaming, hardware accelerated,
//! with its FIPS vectors and hardware-parity tests); this module keeps the
//! `makepad_asset_data::sha256::*` paths every asset crate imports.

pub use makepad_core_util::sha256::*;
