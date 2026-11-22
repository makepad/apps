//! The ATmega328P's GPIO: the one table its numberings come from. Each port pin has its data sheet
//! name (the part's pin name, `PD6`), its port and bit, the microsplash runtime's pin number (what
//! a module and the watch stream name) and its row in the machine ([`crate::machine`]).
//!
//! Source: Microchip DS40002061B, "Pin Configurations" and the "Port B/C/D" alternate functions:
//! PB6 and PB7 serve XTAL1 and XTAL2 with the board's crystal, and PC6 is RESET with the default
//! RSTDISBL fuse, so the board-facing package model exposes the remaining 20 GPIO. The runtime
//! numbers them as the Arduino Uno's D0..D13 and A0..A5 do: PD0..PD7 are 0..7, PB0..PB5 are
//! 8..13 and PC0..PC5 are 14..19. The analog-only ADC6/ADC7 are 20/21, as on the Nano.

/// One GPIO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChipPin {
    /// The port pin's data sheet name.
    pub name: &'static str,
    /// Its port, `B`, `C` or `D`.
    pub port: char,
    /// Its bit in the port's PIN, DDR and PORT registers.
    pub bit: u8,
    /// The runtime's pin number.
    pub number: u32,
}

const fn pin(name: &'static str, port: char, bit: u8, number: u32) -> ChipPin {
    ChipPin { name, port, bit, number }
}

/// The GPIO by machine row: port B, then C, then D.
pub const PINS: [ChipPin; 20] = [
    pin("PB0", 'B', 0, 8), pin("PB1", 'B', 1, 9), pin("PB2", 'B', 2, 10),
    pin("PB3", 'B', 3, 11), pin("PB4", 'B', 4, 12), pin("PB5", 'B', 5, 13),
    pin("PC0", 'C', 0, 14), pin("PC1", 'C', 1, 15), pin("PC2", 'C', 2, 16),
    pin("PC3", 'C', 3, 17), pin("PC4", 'C', 4, 18), pin("PC5", 'C', 5, 19),
    pin("PD0", 'D', 0, 0), pin("PD1", 'D', 1, 1), pin("PD2", 'D', 2, 2), pin("PD3", 'D', 3, 3),
    pin("PD4", 'D', 4, 4), pin("PD5", 'D', 5, 5), pin("PD6", 'D', 6, 6), pin("PD7", 'D', 7, 7),
];

/// An analog-only package pin (DS40002061B §24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalogPin {
    pub name: &'static str,
    pub number: u32,
    pub channel: u8,
    pub row: usize,
}

pub const ANALOG_ONLY: [AnalogPin; 2] = [
    AnalogPin { name: "ADC6", number: 20, channel: 6, row: 20 },
    AnalogPin { name: "ADC7", number: 21, channel: 7, row: 21 },
];

/// The package names by machine row: the 20 GPIO, then ADC6/ADC7.
pub const NAMES: [&str; 22] = {
    let mut names = [""; 22];
    let mut row = 0;
    while row < PINS.len() {
        names[row] = PINS[row].name;
        row += 1;
    }
    names[20] = ANALOG_ONLY[0].name;
    names[21] = ANALOG_ONLY[1].name;
    names
};

/// The machine row of the runtime's pin `number`.
pub fn row(number: u32) -> Option<usize> {
    PINS.iter().position(|p| p.number == number)
        .or_else(|| ANALOG_ONLY.iter().find(|p| p.number == number).map(|p| p.row))
}

/// The GPIO the runtime numbers `number`.
pub fn by_number(number: u32) -> Option<&'static ChipPin> {
    PINS.iter().find(|p| p.number == number)
}

/// ADC channel of a runtime pin number (DS40002061B §24, ADMUX).
pub fn adc_channel(number: u32) -> Option<u8> {
    by_number(number).filter(|p| p.port == 'C').map(|p| p.bit)
        .or_else(|| ANALOG_ONLY.iter().find(|p| p.number == number).map(|p| p.channel))
}

/// Machine row of an external ADC channel, 0..7.
pub fn adc_row(channel: u8) -> usize {
    match channel {
        0..=5 => row(14 + u32::from(channel)).unwrap(),
        6..=7 => ANALOG_ONLY[usize::from(channel - 6)].row,
        _ => panic!("the ATmega328P has no external ADC channel {channel}"),
    }
}

/// The GPIO named `name`, with its machine row.
pub fn by_name(name: &str) -> Option<(usize, &'static ChipPin)> {
    PINS.iter().enumerate().find(|(_, p)| p.name == name)
}

/// The data-space address of a port's PIN register; its DDR and PORT registers follow it.
pub fn pin_register(port: char) -> u16 {
    match port {
        'B' => 0x23,
        'C' => 0x26,
        'D' => 0x29,
        _ => panic!("the ATmega328P has no GPIO port {port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_numbering_is_one_to_one_and_names_its_port_bit() {
        for (r, p) in PINS.iter().enumerate() {
            assert_eq!(p.name, format!("P{}{}", p.port, p.bit), "row {r}: the name is its port and bit");
            assert_eq!(row(p.number), Some(r), "{}: its number leads back to its row", p.name);
            assert_eq!(by_name(p.name).map(|(k, _)| k), Some(r), "{}: its name leads back to its row", p.name);
        }
        let mut numbers: Vec<u32> = PINS.iter().map(|p| p.number).collect();
        numbers.sort_unstable();
        assert_eq!(numbers, (0..20).collect::<Vec<_>>(), "the runtime numbers the 20 GPIO 0..19");
        assert_eq!(by_number(6).map(|p| p.name), Some("PD6"));
        assert_eq!(row(6), Some(18), "PD6 is runtime pin 6 and machine row 18");
        for channel in 0..8 {
            let number = 14 + u32::from(channel);
            assert_eq!(adc_channel(number), Some(channel), "runtime analog pin maps to channel");
            assert_eq!(row(number), Some(adc_row(channel)), "channel maps back to machine row");
        }
        for p in ANALOG_ONLY {
            assert_eq!(NAMES[p.row], p.name, "analog-only package name at its row");
            assert_eq!(by_number(p.number), None, "analog-only pins are not GPIO");
        }
        assert_eq!(adc_channel(6), None, "PD6 has no ADC channel");
        assert_eq!(row(22), None, "no pin after ADC7");
    }
}
