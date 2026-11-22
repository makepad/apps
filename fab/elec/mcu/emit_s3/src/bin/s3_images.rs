//! Writes the S3 board's check images into a directory. `check.bin` is the 240 MHz port's analog
//! check (`SlotOptions::analog_check`: the analog reads after init's raise, the settings, a clock check at 240 MHz)
//! over an empty program: the flash
//! from address 0, with the regions the board flasher writes one by one printed beside it.
use fab_elec_mcu::compile_with;
use fab_elec_mcu_emit_s3::{build_image, chip, Board, BoardTime, SlotOptions, VOICEBOX};
use fab_elec_mcu_rt::module;
use makepad_core_util::sha256::sha256;

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let destination = std::path::PathBuf::from(std::env::args_os().nth(1).ok_or("expected an output directory")?);
    let profile = chip::profile();
    let c = compile_with("fn setup() {}", &profile, &[], &[], &[]).map_err(|e| format!("{e:?}"))?;
    let m = module::load(&c.module, &profile).map_err(|e| format!("{e:?}"))?;
    // The RWDT hold follows the nominal RC slow clock every S3 image of this board assumes
    // (fab-elec-fw's s3_handoff), until the board measures it.
    let board = Board { layout: VOICEBOX, time: BoardTime { rc_slow_hz: (136_000, 1), wdt_delay_sel: 0 },
        panel: None, i2c: None, motion: None, touch: None, radio: None };
    let image = build_image(&m, &board, &SlotOptions { analog_check: true, ..SlotOptions::default() }).map_err(|r| r.message)?;
    std::fs::create_dir_all(&destination)?;
    std::fs::write(destination.join("check.bin"), &image.flash)?;
    let (record, slot) = (VOICEBOX.records[0] as usize, VOICEBOX.slots[0] as usize);
    println!("check.bin: {} bytes, sha256 {}", image.flash.len(), hex(&sha256(&image.flash)));
    println!("  boot stage 0x0..{record:#x}, sha256 {}", hex(&sha256(&image.flash[..record])));
    println!("  record R0 {record:#x}..{slot:#x}, sha256 {}", hex(&sha256(&image.flash[record..slot])));
    println!("  slot 0 {slot:#x}..{:#x}, sha256 {}", image.flash.len(), hex(&sha256(&image.flash[slot..])));
    println!("  image id {:#010x}, RWDT hold {:?}", image.slot.image_id, image.record.wdt);
    println!("  board-unverified: {:?}", image.slot.symbols.board_unverified);
    Ok(())
}
