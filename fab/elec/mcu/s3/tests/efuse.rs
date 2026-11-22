use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emu::bus::{Kind, Width};
use fab_elec_mcu_emu::Machine;
use fab_elec_mcu_s3::{S3Machine, StopDetail};

mod support;
use support::*;

#[test]
fn factory_mac_and_pvt_shadows_are_read_only_profile_owned_and_rewindable() {
    // TRM v1.8 Table 4.3-3 p.408, §5.3.3 p.422 (boot autoload),
    // Registers 5.18/5.19 p.436; IDF v6.1.1 efuse_reg.h:674–720 (MAC), :775–1017 (PVT words).
    let mut p = Program::new();
    p.load(1, 0x6000_7044); let before = p.pc(); p.emit(enc::l32i(6, 1, 0));
    p.emit(enc::l32i(7, 1, 4));
    for (n, offset) in [0x50u32, 0x54, 0x58, 0x6c].into_iter().enumerate() { p.emit(enc::l32i(8 + n as u8, 1, offset - 0x44)); }
    let fault = p.emit(enc::s32i(6, 1, 0));
    let image = p.finish();
    // The PVT fields' bits per word (BLK_VERSION_MINOR; K_RTC_LDO, K_DIG_LDO, V_RTC_DBIAS20's low bits; its high
    // bits, V_DIG_DBIAS20, DIG_DBIAS_HVT; BLK_VERSION_MAJOR); the profile's other bits do not reach the chip.
    let masks = [0x0700_0000u32, 0xffff_e000, 0x0000_ffff, 0x0000_0003];
    for (seed, mac, pvt) in [(19, [2, 0x13, 0x80, 0xff, 0, 0x42], [u32::MAX; 4]), (71, [2, 0xfe, 0xdc, 0xba, 0x98, 0x76], [0x0400_0000, 0x4440_2000, 0xd420, 1])] {
        let mut profile = profile(seed); profile.mac = mac; profile.pvt = pvt;
        let mut m = S3Machine::new(image.clone(), &profile);
        at_pc(&mut m, before); m.mark();
        for replay in 0..2 {
            if replay != 0 { m.rewind(); }
            assert_stop(&mut m, fault, "eFuse writes are not modelled; factory MAC registers are read-only",
                Some(StopDetail { addr: 0x6000_7044, width: Width::W, kind: Kind::Store, mmu_entry: None }));
            assert_eq!(m.cpu().ar[6], u32::from_be_bytes(mac[2..].try_into().unwrap()), "MAC low word");
            assert_eq!(m.cpu().ar[7] & 0xffff, u32::from(u16::from_be_bytes([mac[0], mac[1]])), "MAC high field");
            for k in 0..4 {
                assert_eq!(m.cpu().ar[8 + k] & masks[k], pvt[k] & masks[k], "PVT word {k}'s fields from the profile");
            }
            let mut notes = Vec::new(); m.take_notes(&mut notes);
            assert_eq!(notes.len(), 1, "the omitted fields' assumption is visible once per snapshot");
            assert!(notes[0].what.contains("eFuse fields beside the factory MAC and the PVT fields"));
        }
    }
}
