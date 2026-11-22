use fab_elec_mcu_rt::{MotionProfile, Profile};

use crate::{image::refusal, Refusal};

/// QMI8658C on the board's shared I2C bus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion { pub addr: u8 }

/// facts-parts B09: schematic's explicit address label; R01: shared SDA/SCL.
pub const VOICEBOX_MOTION: Motion = Motion { addr: 0x6b };

// QMI8658C Rev 0.6, lines 1123–1128 and 1159–1168: +/-2 g, +/-256 dps.
// Sensitivities: lines 545–551 and 583–594. Integral scales avoid division
// in the runtime; full-scale codes are shared with the generated prelude.
pub(crate) const ACCEL_FS: u8 = 0;
pub(crate) const GYRO_FS: u8 = 4;
pub const MOTION_PROFILE: MotionProfile = MotionProfile {
    accel_lsb_per_g: 16_384 >> ACCEL_FS,
    gyro_lsb_per_dps: 2_048 >> GYRO_FS,
};

impl Motion {
    pub fn profile(&self) -> Profile {
        Profile { motion: Some(MOTION_PROFILE), ..crate::chip::profile() }
    }
}

/// Lowest common normal-mode ODR at least 1/period. Quarter-Hz arithmetic
/// preserves 31.25 and 62.5 (datasheet lines 1133–1141, 1172–1180).
pub(crate) fn odr(period_us: u32) -> Result<u8, Refusal> {
    (0..=8).rev().find(|code| u64::from(period_us) * (32_000u64 >> *code) >= 4_000_000)
        .ok_or_else(|| refusal("motion period is below 125 us: QMI8658C maximum ODR is 8000 Hz (lines 1133, 1172)"))
}

/// Dispatcher's motion timer and scratch. Samples are committed only after
/// the complete locked burst. No motion declaration means no allocation.
#[derive(Clone, Debug)]
pub struct MotionSram {
    pub timer: u32,
    pub initialized: u32,
    pub sample: u32,
    pub status: u32,
}
