//! Stimulus for the controller documents in `fab-elec-fixtures`.

use crate::bench::{BenchSpec, Stub};
use crate::input::{Drive, Pull};

/// MCU stimulus for `fixtures/ledctl.ckt.splash`, with duty measured HIGH.
/// The LEDs sink into PA0..PA7: a LOW pin lights its LED.
pub fn ledctl_bench(pwm_hz: f64, duty: [f64; 8]) -> BenchSpec {
    let mut stub: Vec<_> = duty.into_iter().enumerate().map(|(i, duty)| {
        let drive = if duty == 0.0 {
            Drive::Level(false)
        } else if duty == 1.0 {
            Drive::Level(true)
        } else {
            Drive::Pwm { freq: pwm_hz, duty }
        };
        (format!("PA{i}"), drive)
    }).collect();
    stub.extend([
        ("PB7", Pull::Up), ("NRST", Pull::Up), ("PA13", Pull::Up),
        ("PA14", Pull::Down), ("BOOT0", Pull::None),
    ].map(|(name, pull)| (name.to_owned(), Drive::Input(pull))));
    BenchSpec {
        stubs: vec![Stub { path: "mcu.u".to_owned(), pins: stub, rail: None }],
        pressed: Vec::new(),
        turned: Vec::new(),
        supplies: Vec::new(),
        sources: Vec::new(),
        probes: Vec::new(),
        firmware: Vec::new(),
        nets: Vec::new(),
    }
}
