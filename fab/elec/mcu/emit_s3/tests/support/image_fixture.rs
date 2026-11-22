use fab_elec_mcu_emit32::xtensa::{assemble, enc, Asm};
use fab_elec_mcu_emit_s3::{Image, SLOT_BASE, VOICEBOX};
use makepad_core_util::sha256::sha256;

// Append guest instructions at init's final jump, then enter the normal
// dispatcher. Update the boot record so the image still passes verification.
pub fn before_dispatch(image: &mut Image, prepare: impl Fn(&mut Asm, &mut Vec<u32>)) -> Vec<u32> {
    let slot = &image.slot;
    let prefix = (slot.bytes.len() as u32 + 3) & !3;
    let init_jump = slot.map.runtime.iter().find(|f| f.name == "init").unwrap().end - SLOT_BASE - 3;
    let mut stops = Vec::new();
    let mut code = assemble(&mut Asm::new(8, 11), |a| {
        stops.clear(); a.base.reserve(prefix); a.literal_table(); prepare(a, &mut stops);
        a.j(slot.symbols.dispatch as i32 - (SLOT_BASE + a.base.offset()) as i32 - 4);
    });
    code[..slot.bytes.len()].copy_from_slice(&slot.bytes);
    let jump = enc::j(prefix as i32 - init_jump as i32 - 4).to_le_bytes();
    code[init_jump as usize..init_jump as usize + 3].copy_from_slice(&jump[..3]);
    image.record.length = code.len() as u32; image.record.sha256 = sha256(&code);
    let record = image.record.bytes(); let at = VOICEBOX.records[0] as usize;
    image.flash[at..at + record.len()].copy_from_slice(&record);
    image.flash.truncate(VOICEBOX.slots[0] as usize); image.flash.extend_from_slice(&code);
    image.slot.bytes = code;
    stops
}
