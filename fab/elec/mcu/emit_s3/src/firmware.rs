//! A firmware document and circuit wiring to a confirmed S3 flash image.

use std::{fmt, sync::Arc};

use crate::{build_image, debug_table, radio::stand_in::stand_in, Board, BoardTime, Radio,
    SlotOptions, Wiring, CPU_HZ, VOICEBOX};
use fab_elec_mcu::{compile_with, Diagnostic};
use fab_elec_mcu_emu::FwDebug;
use fab_elec_mcu_island::Island;
use fab_elec_mcu_rt::{module, ports};
use fab_elec_mcu_s3::{rom, VirtualFns};

/// A built image and what stepping, the observations and the code view need of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Firmware {
    /// The board flash image from address zero through the last linked byte in slot A.
    pub flash: Vec<u8>,
    /// The linked island and the simulator's foreign functions for this slot.
    pub radio: Option<SimRadio>,
    /// The id its Hello frame carries: the linked slot's image id.
    pub image_id: u32,
    /// The image's table for a co-simulated device: its step map (None, named in `notes`, when
    /// the emitter's map does not hold together), statements, handlers, variables and runtime
    /// routines, in absolute byte addresses. A handler is its module entry number (module order,
    /// setup included). Its `source_rev` is the host's to set.
    pub debug: FwDebug,
    /// The compiler's notes, rounded speaker queues, handlers above 20 ms, and any unusable step map.
    pub notes: Vec<String>,
    /// The pins that are ever analog, by the chip's pin names (`GPIO{n}`): the inputs the source
    /// declares `analog(..)`. The firmware uses every other pin digitally.
    pub analog: Vec<String>,
    /// Each pin the source declares, by the chip's pin name (`GPIO{n}`), and its role.
    pub pins: Vec<(String, ports::PinRole)>,
    /// Each network connection the source declares, in order (its number): the receive slots its
    /// host end may fill (`rt::net::Host::new`). Empty: the image has no network ports.
    pub net_slots: Vec<u16>,
}

/// The island bytes and foreign table for a single linked voicebox image.
#[derive(Clone)]
pub struct SimRadio {
    /// Linked bytes at the voicebox island's flash offset.
    pub island: Arc<Vec<u8>>,
    table: Arc<VirtualFns>,
    function_count: usize,
}

impl SimRadio {
    /// The pack and ROM functions to install together on the machine.
    pub fn table(&self) -> Arc<VirtualFns> { Arc::clone(&self.table) }

    /// Complete machine flash for this firmware's slot and island.
    pub fn flash(&self, slot_flash: &[u8]) -> Vec<u8> {
        flash_with_island(slot_flash, &self.island)
    }
}

impl fmt::Debug for SimRadio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimRadio").field("island_bytes", &self.island.len())
            .field("function_count", &self.function_count).finish()
    }
}

impl PartialEq for SimRadio {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.table, &other.table) && self.island == other.island
    }
}

/// Places the island after erased NOR padding. Both slices must fit the voicebox layout.
/// An absent island uses the short slot image.
pub fn flash_with_island(slot_flash: &[u8], island: &[u8]) -> Vec<u8> {
    if island.is_empty() { return slot_flash.to_vec(); }
    let at = VOICEBOX.island as usize;
    assert!(slot_flash.len() <= at && island.len() <= (VOICEBOX.island_pages * crate::PAGE) as usize);
    let mut flash = Vec::with_capacity(at + island.len());
    flash.extend_from_slice(slot_flash);
    flash.resize(at, 0xff);
    flash.extend_from_slice(island);
    flash
}

/// The flash writes that put an image (`Firmware::flash`) on a board, by offset (boot-stage.md,
/// Flash layout): the head, from zero through both record sectors, slot A, and bundle A when R0
/// names one. The head carries R0 and an erased R1, so no record an older image left in R1
/// outranks this one (a record's higher `seq` wins). The padding past the records, slot B,
/// bundle B, the island and the stores keep what the board holds; a radio image's island is its
/// own write (`island_region`).
pub fn flash_regions(flash: &[u8]) -> Vec<(u32, &[u8])> {
    let head = VOICEBOX.records.iter().max().map_or(0, |&at| at + crate::SECTOR) as usize;
    let r0 = &flash[VOICEBOX.records[0] as usize..];
    let length = u32::from_le_bytes(r0[16..20].try_into().expect("R0's length word")) as usize;
    let slot = VOICEBOX.slots[0];
    let mut regions = vec![(0, &flash[..head]), (slot, &flash[slot as usize..slot as usize + length])];
    if u16::from_le_bytes([r0[10], r0[11]]) != 0 {
        let bundle = VOICEBOX.bundles[0];
        regions.push((bundle, &flash[bundle as usize..]));
    }
    regions
}

/// A radio image's island (`SimRadio::island`) as board writes, in order: its body, then its header
/// sectors (`Header::body_offset`), at the layout's island offset, apart from the slots so an app
/// update never moves it. The header goes last, so a write cut short leaves no valid header (the
/// header sectors erased) or the board's old one, which this image's slot refuses: the boot never
/// starts a half-written island. Err when the bytes carry no island header.
pub fn island_regions(island: &[u8]) -> Result<Vec<(u32, &[u8])>, String> {
    let header = fab_elec_mcu_island::Header::decode(island).map_err(|err| format!("the radio island: {err}"))?;
    let body = (header.body_offset() as usize).min(island.len());
    let mut regions = Vec::with_capacity(2);
    if body < island.len() { regions.push((VOICEBOX.island + body as u32, &island[body..])); }
    regions.push((VOICEBOX.island, &island[..body]));
    Ok(regions)
}

/// Builds `source` for the circuit's S3 wiring and measured board time, at CPU_HZ, with the bench's
/// `consts` replacing the values of the document's top-level `let`s of those names.
/// Supplying a linked island enables the voicebox station and binds its simulator table. `files`
/// are the firmware folder's files by name; its pictures go into the slot's file bundle.
pub fn firmware(
    source: &str, consts: &[(&str, f64)], wiring: &Wiring, time: BoardTime, island: Option<&Island>,
    files: &[(&str, &[u8])],
) -> Result<Firmware, Vec<Diagnostic>> {
    let profile = wiring.profile();
    let c = compile_with(source, &profile, &[], consts, files)
        .map_err(|e| e.diagnostics(source.len()))?;
    let m = module::load(&c.module, &profile)
        .map_err(|e| vec![Diagnostic::new(None, format!("internal compiler error: the verified module does not load: {e}"))])?;
    let board = Board {
        layout: VOICEBOX, time, panel: wiring.panel, i2c: wiring.i2c,
        motion: wiring.motion, touch: wiring.touch, radio: island.map(|island| Radio::voicebox(island.binding())),
    };
    let options = SlotOptions {
        sites: c.sites.iter().map(|s| (s.line, s.col)).collect(),
        marks: c.marks.clone(),
        image_names: c.images.clone(),
        ..wiring.options()
    };
    let image = build_image(&m, &board, &options)
        .map_err(|r| vec![Diagnostic::new(c.code_span(r.entry, r.code, source.len()), r.message)])?;
    let radio = match (island, &board.radio, &image.slot.layout.radio) {
        (Some(island), Some(profile), Some(sram)) => {
            let mut parts = stand_in(&profile.island, sram)
                .map_err(|e| vec![Diagnostic::new(None, format!("radio stand-in: {e}"))])?;
            let (range, functions) = rom::rows();
            parts.ranges.push(range);
            parts.functions.extend(functions);
            let function_count = parts.functions.len();
            let table = parts.table()
                .map_err(|e| vec![Diagnostic::new(None, format!("radio foreign table: {e}"))])?;
            Some(SimRadio { island: Arc::new(island.image.clone()), table: Arc::new(table), function_count })
        }
        (None, None, None) => None,
        _ => return Err(vec![Diagnostic::new(None, "radio stand-in: the slot's radio layout does not match its profile")]),
    };
    let (debug, note) = debug_table(&c, source.len(), &image.slot);
    let mut notes = c.notes;
    notes.extend(image.slot.layout.speaker.and_then(|s| s.rounding_note));
    for handler in &image.slot.handlers {
        // Compare exactly before formatting; widen before multiplying the u64 bound.
        if u128::from(handler.max_insns) * 1000 > u128::from(CPU_HZ) * 20 {
            let name = &debug.handlers[handler.id as usize].name;
            let ms = handler.max_insns as f64 * 1000.0 / f64::from(CPU_HZ);
            notes.push(format!("the handler `{name}` can run for up to {ms:.1} ms; others wait that long (above 20 ms)"));
        }
    }
    if let Some(note) = note { notes.push(note); }
    let pins: Vec<(String, ports::PinRole)> = m.decls().iter()
        .filter_map(|d| ports::pin_role(d.form, &d.args).map(|(pin, role)| (format!("GPIO{pin}"), role))).collect();
    let analog = pins.iter().filter(|(_, r)| *r == ports::PinRole::Analog).map(|(n, _)| n.clone()).collect();
    let net_slots = image.slot.layout.net.as_ref().map_or_else(Vec::new, |net| net.connections.iter()
        .map(|c| u16::try_from(c.slot_count).expect("a connection's slots fit its SRAM")).collect());
    Ok(Firmware { flash: image.flash, radio, image_id: image.slot.image_id, debug, notes, analog, pins, net_slots })
}
