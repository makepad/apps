//! A bench's drive of a stubbed part's pin: the stimulus a bench names (`fab-elec-bench` lowers
//! it) and a design op can change (the duty knob), so both crates take it from here.

/// What a stub does with one pin.
#[derive(Clone, Debug, PartialEq)]
pub enum Drive {
    /// `duty` is the fraction of the period the pin is HIGH.
    Pwm { freq: f64, duty: f64 },
    /// A static output, HIGH when true.
    Level(bool),
    /// An input with its pull.
    Input(Pull),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Pull {
    None,
    Up,
    Down,
}

// Canonical tags are fixed: new variants of either type go last.
crate::canon_enum!(Drive { 0 => Pwm { freq, duty }, 1 => Level(l), 2 => Input(p) });
crate::canon_tags!(Pull);
