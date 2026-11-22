use fab_elec_mcu_emit_avr::image::blink_entry;
use fab_elec_mcu_emit_avr::{build, PinDef, Program, Role, TimerDef};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::env::args_os().nth(1).ok_or("expected an output directory")?;
    let program = Program {
        image_id: 1, state_init: vec![0], pins: vec![PinDef { pin: 13, role: Role::Output }],
        timers: vec![TimerDef { entry: 0, period_ticks: 500_000, repeat: true }],
        entries: vec![&blink_entry], ..Program::default()
    };
    let image = build(&program)?;
    let destination = std::path::PathBuf::from(destination);
    std::fs::create_dir_all(&destination)?;
    std::fs::write(destination.join("blink.bin"), image.bytes())?;
    println!("blink: {} bytes; RAM {:?}", image.words.len() * 2, image.layout);
    Ok(())
}
