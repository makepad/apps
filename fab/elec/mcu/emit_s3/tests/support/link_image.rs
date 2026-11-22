use fab_elec_mcu::{compile, Compiled};
use fab_elec_mcu_emit_s3::{build_image, chip, Board, BoardTime, Image, SlotOptions, VOICEBOX};
use fab_elec_mcu_rt::module;
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Profile, TimeProfile};

pub const MAC: [u8; 6] = [2, 0x13, 0x80, 0xff, 0, 0x42];

pub fn profile() -> S3Profile {
    S3Profile {
        flash_bytes: VOICEBOX.flash_bytes as usize, handoff_cpu: CpuClock::Xtal { div: 2 },
        handoff_mapped_pages: 64, entry: 0x4200_0408, seed: 53, mac: MAC, pvt: fab_elec_mcu_s3::NO_PVT,
        time: TimeProfile {
            systimer_first_long: true, rc_slow_hz: (136_000, 1), wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000, swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2, mwdt_rephase_on_feed: false, mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true, usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0, input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true, handoff_out: [0; 2], handoff_enable: [0; 2],
            handoff_pin: [0; 45], handoff_io_mux: [1 << 12; 45], handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(), handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(), handoff_usb_int_raw: 8, handoff_usb_int_ena: 0,
        },
    }
}

pub fn compiled(source: &str) -> Compiled {
    compile(source, &chip::profile(), &[], &[]).unwrap_or_else(|e| panic!("{e:?}\n{source}"))
}

pub fn image(c: &Compiled) -> Image {
    let p = profile();
    let m = module::load(&c.module, &chip::profile()).unwrap();
    let board = Board { layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None, radio: None,
        time: BoardTime { rc_slow_hz: p.time.rc_slow_hz, wdt_delay_sel: p.time.wdt_delay_sel } };
    build_image(&m, &board, &SlotOptions::default()).unwrap()
}
