use fab_elec_mcu_emu::Pull;
use fab_elec_mcu_parts::{model, BoardDesc, NetDesc, PartDesc, PinRef};

// ES8311 datasheet lines 303-307: CE low.
pub const CODEC: u8 = 0x18;
pub const AMP_EN: u32 = 8;

pub fn board() -> BoardDesc {
    let chip = |gpio: u32| PinRef::Chip(format!("GPIO{gpio}").into());
    let part = |part: u16, pin: &str| PinRef::Part { part, pin: pin.into() };
    let mut nets: Vec<NetDesc> = [(14, "SCL", Pull::Up), (15, "SDA", Pull::Up), (4, "MCLK", Pull::None),
        (5, "SCLK", Pull::None), (6, "LRCK", Pull::None), (7, "DSDIN", Pull::None), (9, "ASDOUT", Pull::None)]
        .into_iter().map(|(gpio, pin, pull)| NetDesc { name: pin.into(), rise_ps: None, pull,
            pins: vec![chip(gpio), part(0, pin)] }).collect();
    for (out, input) in [("OUTP", "INP"), ("OUTN", "INN")] {
        nets.push(NetDesc { name: out.into(), rise_ps: None, pull: Pull::None, pins: vec![part(0, out), part(1, input)] });
    }
    nets.push(NetDesc { name: "PA_CTRL".into(), rise_ps: None, pull: Pull::None, pins: vec![chip(AMP_EN), part(1, "EN")] });
    let codec = model("ES8311", &[("addr".into(), f64::from(CODEC))]).expect("a CE-low codec");
    let amp = model("AMP", &[("enable".into(), 1.0), ("gain_db".into(), 0.0), ("start_ms".into(), 0.0)]).expect("a unity amplifier");
    BoardDesc { seed: 0x8311, parts: vec![PartDesc { refdes: "U1".into(), kind: codec },
        PartDesc { refdes: "U2".into(), kind: amp }], nets }
}
