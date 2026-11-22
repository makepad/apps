use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};

const RW: Perm = Perm { read: true, write: true, exec: false };
const RX: Perm = Perm { read: true, write: false, exec: true };

#[test]
fn memory_maps_aliases_and_faults() {
    let mut mem = Memory::new();
    let ram = mem.store(16);
    mem.map(0x1000, ram, 0, 16, RW);
    mem.map(0x8000, ram, 4, 8, RX);

    assert_eq!(mem.write(0x1004, Width::W, 0x4433_2211, 0), Ok(()), "a word store in the data mapping");
    assert_eq!(mem.read(0x8000, Width::W, Kind::Fetch, 0), Ok(0x4433_2211), "the alias shows the same store");
    assert_eq!(mem.read(0x1005, Width::H, Kind::Load, 0), Ok(0x3322), "unaligned halfword, little-endian");
    assert_eq!(mem.read(0x1007, Width::B, Kind::Load, 0), Ok(0x44), "byte read zero-extends");

    assert_eq!(mem.read(0x1000, Width::W, Kind::Fetch, 0), Err(()), "fetch needs exec");
    assert_eq!(mem.write(0x8000, Width::B, 0, 0), Err(()), "store needs write");
    assert_eq!(mem.read(0x100e, Width::W, Kind::Load, 0), Err(()), "an access crossing the mapping's end faults");
    assert_eq!(mem.read(0x2000, Width::B, Kind::Load, 0), Err(()), "unmapped address faults");

    let before = mem.bytes(ram).to_vec();
    assert_eq!(mem.write(0x100e, Width::W, 0xffff_ffff, 0), Err(()), "a store crossing the end faults");
    assert_eq!(mem.bytes(ram), before.as_slice(), "a faulting store changes no byte");

    mem.bytes_mut(ram)[15] = 0x5a;
    assert_eq!(mem.read(0x100f, Width::B, Kind::Load, 0), Ok(0x5a), "bytes_mut writes the store the bus reads");

    let overlap = std::panic::catch_unwind(move || mem.map(0x1008, ram, 0, 4, RW));
    assert!(overlap.is_err(), "an overlapping mapping panics");
}
