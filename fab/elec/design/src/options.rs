//! A board builder's override record as plain leaves: what a script's `board("name", {..})` sets.

/// One leaf of an override record: a number (a script length, in metres: `7.9mm` is 0.0079), an
/// id (`@micro`, without the `@`) or a flag.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Num(f64),
    Id(String),
    Bool(bool),
}

/// A refused leaf: its dotted key (`usb.height`) and what is wrong with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub key: String,
    pub message: String,
}
