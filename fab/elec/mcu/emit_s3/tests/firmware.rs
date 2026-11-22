//! The document entry point agrees with direct image building and attributes refusals.

use fab_elec_mcu::compile_with;
use fab_elec_mcu_emit_s3::{build_image, firmware, flash_regions, Board, BoardTime, SlotOptions, Wiring,
    VOICEBOX, VOICEBOX_I2C, VOICEBOX_MOTION};
use fab_elec_mcu_rt::{bundle, module};
use makepad_zune_png::{PngEncoder, makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions}};

const TIME: BoardTime = BoardTime { rc_slow_hz: (136_000, 1), wdt_delay_sel: 0 };
const WIRING: Wiring = Wiring {
    i2c: Some(VOICEBOX_I2C), motion: Some(VOICEBOX_MOTION), panel: None, touch: None, audio: None,
};

#[test]
fn a_long_handler_builds_and_names_its_wait_in_the_notes() {
    let source = r#"
var sum: i32 = 0
let step = every(ms(20), fn() {
    for i in 0..1000000 { sum = (sum + i) & 65535 }
})
"#;
    let wiring = Wiring { i2c: None, motion: None, panel: None, touch: None, audio: None };
    let built = firmware(source, &[], &wiring, TIME, None, &[])
        .expect("a handler above 20 ms builds when the RWDT covers its bound");
    let notes: Vec<_> = built.notes.iter().filter(|n| n.contains("the handler `step`")).collect();
    assert_eq!(notes.len(), 1, "the long handler gets exactly one source-named note");
    let time = notes[0].strip_prefix("the handler `step` can run for up to ")
        .and_then(|s| s.strip_suffix(" ms; others wait that long (above 20 ms)"))
        .and_then(|s| s.parse::<f64>().ok()).expect("the note reports the handler's time in ms");
    assert!(time > 20.0, "the loop exceeds 20 ms at the image's CPU clock: {time}");
    // An image carries a handler's cost in one word: a cost past it is refused at the source,
    // never as a module that does not load.
    let past = source.replace("for i in 0..1000000 { sum = (sum + i) & 65535 }",
        "for i in 0..1000000 { for j in 0..1000000 { sum = (sum + j) & 65535 } }");
    let refused = firmware(&past, &[], &wiring, TIME, None, &[]).err().expect("a handler past the cost word is refused");
    assert!(refused.iter().any(|d| d.message.contains("`step` may run") && d.span.is_some()),
        "the refusal names the handler at its source: {refused:?}");
}

// motion.rs's program(), with its period fixed at 10 ms.
const SOURCE: &str = r#"
let data = words(48)
var count: i32 = 0
var outside: i32 = 0
var before: i32 = 1
var bad: i32 = 1
var scale_a: i32 = 0
var scale_g: i32 = 0
let peek = every(100000, fn() { outside = accel(0) })
let imu = motion(10000, fn() {
    if count < 8 {
        let at = count * 6
        data[at] = accel(0)
        data[at + 1] = accel(1)
        data[at + 2] = accel(2)
        data[at + 3] = gyro(0)
        data[at + 4] = gyro(1)
        data[at + 5] = gyro(2)
        count += 1
    }
})
fn setup() {
    before = accel(0) | gyro(2)
    bad = gyro(3) | accel(-1)
    scale_a = ACCEL_LSB_PER_G
    scale_g = GYRO_LSB_PER_DPS
}
"#;

#[test]
fn motion_document_matches_the_direct_image() {
    let firmware = firmware(SOURCE, &[], &WIRING, TIME, None, &[]).expect("motion firmware builds");
    let profile = WIRING.profile();
    let c = compile_with(SOURCE, &profile, &[], &[], &[]).expect("motion source compiles");
    let m = module::load(&c.module, &profile).expect("motion module verifies");
    let board = Board {
        layout: VOICEBOX, time: TIME, panel: None,
        i2c: Some(VOICEBOX_I2C), motion: Some(VOICEBOX_MOTION), touch: None, radio: None,
    };
    let image = build_image(&m, &board, &SlotOptions {
        sites: c.sites.iter().map(|s| (s.line, s.col)).collect(), marks: c.marks.clone(),
        ..SlotOptions::default()
    }).expect("motion image links directly");
    assert!(firmware.flash == image.flash, "the document path preserves every image byte");
    assert_eq!(firmware.image_id, image.slot.image_id, "the Hello id is the linked slot's");
    assert_eq!(firmware.debug.image_id, firmware.image_id, "the debug table names the same image");
    let [(head_at, head), (slot_at, slot)] = flash_regions(&firmware.flash)[..] else { panic!("an image without files writes the head and slot A") };
    assert_eq!((head_at, head.len()), (0, 0x3000), "the head runs from zero through both record sectors");
    assert!(head[0x2000..].iter().all(|&b| b == 0xff), "the head writes R1 erased, so no older record outranks R0");
    assert_eq!(&head[0x1000..0x1004], &image.record.bytes()[..4], "the head carries this image's R0");
    assert_eq!((slot_at, slot), (VOICEBOX.slots[0], &image.slot.bytes[..]), "slot A holds exactly the linked slot");

    // The firmware's folder: its pictures ride in bundle A, its other files are named in a note.
    let rgb = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    let mut logo = Vec::new();
    PngEncoder::new(&rgb, EncoderOptions::new(2, 2, ColorSpace::RGB, BitDepth::Eight)).encode(&mut logo).expect("a 2x2 PNG encodes");
    let with_files = fab_elec_mcu_emit_s3::firmware(SOURCE, &[], &WIRING, TIME, None, &[("Logo.png", &logo[..]), ("notes.txt", &b"plain text"[..])])
        .expect("the firmware builds with its folder's files");
    assert!(with_files.notes.iter().any(|note| note.contains("not shipped") && note.contains("notes.txt")),
        "a file that is no picture is named in the notes: {:?}", with_files.notes);
    let regions = flash_regions(&with_files.flash);
    let &(bundle_at, bytes) = regions.last().expect("regions");
    assert_eq!((regions.len(), bundle_at), (3, VOICEBOX.bundles[0]), "Flash to writes bundle A after the head and slot A");
    let read = bundle::read(bytes).expect("bundle A reads");
    let logo = read.find("logo").expect("the picture is filed under its normalized name");
    assert_eq!((read.len(), logo.a, logo.b, &logo.data[..2]), (1, 2, 2, &0xf800u16.to_le_bytes()[..]),
        "only the picture is bundled, at its size, red first in RGB565");
}

#[test]
fn source_and_constant_errors_keep_their_attribution() {
    let source = "fn setup() { missing() }";
    let errors = firmware(source, &[], &WIRING, TIME, None, &[]).expect_err("unknown function is refused");
    assert!(!errors.is_empty(), "source errors yield diagnostics");
    for error in errors {
        let span = error.span.expect("source error has a document span");
        assert!(span.start < span.end && span.end <= source.len(), "source span is inside the document");
        assert!(error.param.is_none(), "source error is not a bench parameter");
    }
    let source = "let count = 1\nfn setup() {}";
    let errors = firmware(source, &[("absent", 2.0)], &WIRING, TIME, None, &[]).expect_err("unknown bench constant is refused");
    assert_eq!(errors.len(), 1, "one unknown constant gives one diagnostic");
    assert_eq!(errors[0].param.as_deref(), Some("absent"), "unknown constant is named");
    assert!(errors[0].span.is_none(), "an undeclared constant has no source span");
    let errors = firmware(source, &[("count", 1.5)], &WIRING, TIME, None, &[]).expect_err("fractional i32 constant is refused");
    assert_eq!(errors.len(), 1, "one invalid constant gives one diagnostic");
    assert_eq!(errors[0].param.as_deref(), Some("count"), "invalid constant is named");
    let span = errors[0].span.clone().expect("declared constant has a source span");
    assert!(span.start < span.end && span.end <= source.len(), "constant span is inside the document");
}

#[test]
fn image_refusal_points_at_the_source_statement() {
    let source = "let data = output(33)\nlet clock = output(34)\nfn setup() {\n    shift_out(data, clock, 1, 8)\n}";
    let profile = WIRING.profile();
    let c = compile_with(source, &profile, &[], &[], &[]).expect("shift source passes the frontend");
    let m = module::load(&c.module, &profile).expect("shift module verifies");
    let board = Board {
        layout: VOICEBOX, time: TIME, panel: None,
        i2c: WIRING.i2c, motion: WIRING.motion, touch: None, radio: None,
    };
    let refusal = build_image(&m, &board, &WIRING.options()).err().expect("S3 image builder refuses shift_out");
    let errors = firmware(source, &[], &WIRING, TIME, None, &[]).expect_err("image refusal becomes a diagnostic");
    assert_eq!(errors.len(), 1, "one image refusal gives one diagnostic");
    assert_eq!(errors[0].message, refusal.message, "the builder's refusal is preserved");
    let span = errors[0].span.clone().expect("refused instruction has a statement span");
    assert!(span.start < span.end && span.end <= source.len(), "refusal span is inside the source");
    assert!(source[span].contains("shift_out(data, clock, 1, 8)"), "refusal identifies the shift statement");
    assert!(errors[0].param.is_none(), "image refusal is not a bench parameter");
}
