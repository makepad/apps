//! Build-time assets and ordered draws on the reference panel.
use fab_elec_mcu::{compile, glyph_atlas, glyph_atlas_size, render_glyph_atlas, Compiled, Profile};
use fab_elec_mcu_rt as rt;
use rt::{format::*, module::{self, LoadError}, ports, sched::{Device, DrawRefusal, Fault, Hal, Role}};
use makepad_zune_png::{PngEncoder, makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions}};

const P: Profile = Profile {
    id: 7, pins: 0, max_entry_ops: 50_000, call_ops: None, ram_words: 4096, buffer_bytes: 0, eeprom: 0, eeprom_write_ns: 0, f64: false,
    op_ns: 10, clock_hz: 1_000_000, analog: &rt::analog::NONE, pwm: &rt::pwm::NONE, serial: &rt::serial::NONE,
    motion: None, touch: false, audio: None,
    display: Some(rt::DisplayProfile { width: 8, height: 8 }),
};

#[derive(Default)]
struct PanelHal { faults: Vec<Fault> }

impl Hal for PanelHal {
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!("this profile has no EEPROM") }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!("this profile has no EEPROM") }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!("this fixture shifts nothing") }
    fn configure(&mut self, _: u64, _: u32, _: Role) { panic!("display program has no pins"); }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { panic!("display program has no serial"); }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { panic!("display program never writes pins"); }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { panic!("display program has no PWM"); }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { panic!("display program never reads pins"); }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!("this fixture declares no analog input") }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { false }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { [2, 0, 0, 0, 0, 1] }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, _: u64, fault: Fault) { self.faults.push(fault); }
}

fn png(width: usize, height: usize, rgba: &[[u8; 4]]) -> Vec<u8> {
    let bytes: Vec<u8> = rgba.iter().flatten().copied().collect();
    let mut encoded = Vec::new();
    PngEncoder::new(&bytes, EncoderOptions::new(width, height, ColorSpace::RGBA, BitDepth::Eight))
        .encode(&mut encoded).expect("fixture PNG encodes");
    encoded
}

fn var(c: &Compiled, device: &Device<'_>, name: &str) -> u32 {
    let v = c.lowered.state_vars.iter().find(|v| v.name == name).expect("named state variable");
    device.state()[v.offset as usize]
}

fn error(source: &str, profile: &Profile, assets: &[(&str, &[u8])]) -> String {
    match compile(source, profile, &[], assets) {
        Ok(_) => panic!("invalid display input compiled: {source}"),
        Err(errors) => errors.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("\n"),
    }
}

#[test]
fn display_and_images_reference() {
    let rgba = [
        [255, 0, 0, 0], [0, 255, 0, 255], [0, 0, 255, 127],
        [250, 130, 65, 255], [80, 90, 100, 1], [255, 255, 255, 255],
        [10, 20, 30, 255], [40, 50, 60, 255], [70, 80, 90, 0],
    ];
    let small_rgba = [[255, 255, 0, 255], [0, 255, 255, 0], [255, 0, 255, 255], [0, 0, 0, 255]];
    let logo_png = png(3, 3, &rgba);
    let badge_png = png(2, 2, &small_rgba);
    // Reversed input order also checks numbering by normalized name.
    let assets = [("LoGo.png", logo_png.as_slice()), ("Badge!.png", badge_png.as_slice())];
    let source = "\
let screen = display()
var white: i32 = 0
var red: i32 = 0
var width: i32 = 0
var height: i32 = 0
fn setup() {
    white = rgb(255, 255, 255)
    red = rgb(255, 0, 0)
    width = logo_w
    height = logo_h
    fill_rect(screen, 0, 0, display_w, display_h, white)
    draw_image(screen, logo, logo_w, logo_h, 0, 0)
    draw_image(screen, badge_, badge__w, badge__h, 2, 2)
    present(screen, logo, 1, 1, 2, 2, 4, 0)
    draw_image(screen, logo, logo_w, logo_h, -2, 4)
    present(screen, logo, 0, 0, 0, 2, 1, 1)
    draw_image(screen, logo, logo_w, logo_h, 2147483647, 0)
    fill_rect(screen, -2147483648, 0, 2147483647, 2, 0)
}
let refused = after(ms(1), fn() {
    draw_image(screen, logo, logo_w, logo_h, 1, 0)
    present(screen, logo, 3, 0, 2, 2, 0, 0)
    draw_image(screen, logo, logo_w, logo_h, -1, 0)
    fill_rect(screen, 0, 1, 2, 2, 0)
    fill_rect(screen, 0, 0, -2, 2, 0)
    present(-1, logo, 0, 0, -2, 2, 0, 0)
    fill_rect(9, 0, 0, -2, 2, 0)
})
let continued = after(ms(2), fn() {
    present(screen, logo, 0, 0, 2, 2, 3, 4)
    fill_rect(screen, 6, 6, 2, 2, 65567)
})
";
    let c = compile(source, &P, &[], &assets).unwrap_or_else(|e| panic!("display program compiles: {e:?}"));
    let module = module::load(&c.module, &P).expect("display module verifies");
    assert_eq!(module.images().len(), 2, "both assets are carried");
    assert_eq!(module.images().iter().map(|i| (i.width, i.height)).collect::<Vec<_>>(), [(2, 2), (4, 4)], "normalized name order and even padding");
    let rgb565 = |p: &[u8; 4]| (u16::from(p[0]) / 8) * 2048 + (u16::from(p[1]) / 4) * 32 + u16::from(p[2]) / 8;
    let original: Vec<u16> = rgba.iter().map(rgb565).collect();
    let padded: Vec<u16> = [0, 1, 2, 2, 3, 4, 5, 5, 6, 7, 8, 8, 6, 7, 8, 8].map(|i| original[i]).to_vec();
    let badge: Vec<u16> = small_rgba.iter().map(rgb565).collect();
    for (image, expected) in module.images().iter().zip([&badge, &padded]) {
        let actual: Vec<u16> = (0..image.height).flat_map(|y| (0..image.width).map(move |x| image.pixel(x, y).expect("in-range image pixel"))).collect();
        assert_eq!(&actual, expected, "RGB565 truncation ignores alpha and repeats the last column and row");
        assert_eq!(image.pixel(image.width, 0), None, "pixel accessor refuses coordinates outside the image");
    }
    let image_dir = (0..c.module[H_SECTIONS] as usize).map(|k| HEADER_WORDS + k * DIRECTORY_WORDS)
        .find(|at| c.module[*at] == SEC_IMAGES).expect("IMAGES directory row");
    let image_start = c.module[image_dir + 1] as usize;
    assert_eq!(&c.module[image_start..image_start + 5], &[2, 2, 2, 0xFFE0_07FF, 0xF81F_0000], "IMAGES count, dimensions and high-half-first RGB565 packing");
    let mut device = Device::new(module, &P);
    let mut hal = PanelHal::default();
    assert_eq!(device.framebuffer(), Some([0u16; 64].as_slice()), "reference panel starts at fixed zero fill");
    device.boot(&mut hal).expect("setup runs");
    assert_eq!(var(&c, &device, "white"), 0xFFFF, "rgb white is RGB565 white");
    assert_eq!(var(&c, &device, "red"), 0xF800, "rgb red is RGB565 red");
    assert_eq!((var(&c, &device, "width"), var(&c, &device, "height")), (4, 4), "generated image size constants are padded");
    let mut expected = vec![0xFFFF; 64];
    // Independent destination-to-source mapping, including clipping at the left edge.
    let paint = |frame: &mut [u16], pixels: &[u16], stride: usize, src: [usize; 4], dest: [i32; 2]| {
        for (y, row) in frame.chunks_mut(8).enumerate() {
            for (x, pixel) in row.iter_mut().enumerate() {
                let (dx, dy) = (x as i32 - dest[0], y as i32 - dest[1]);
                if dx >= 0 && dy >= 0 && dx < src[2] as i32 && dy < src[3] as i32 {
                    *pixel = pixels[(src[1] + dy as usize) * stride + src[0] + dx as usize];
                }
            }
        }
    };
    paint(&mut expected, &padded, 4, [0, 0, 4, 4], [0, 0]);
    paint(&mut expected, &badge, 2, [0, 0, 2, 2], [2, 2]);
    paint(&mut expected, &padded, 4, [1, 1, 2, 2], [4, 0]);
    paint(&mut expected, &padded, 4, [0, 0, 4, 4], [-2, 4]);
    assert_eq!(device.framebuffer(), Some(expected.as_slice()), "ordered image overlap, odd source origin and even clipped window");
    assert!(hal.faults.is_empty(), "valid and empty clipped windows never fault: {:?}", hal.faults);
    device.step(&mut hal).expect("refusal handler runs");
    assert_eq!(device.framebuffer(), Some(expected.as_slice()), "every refused draw leaves the entire framebuffer unchanged");
    let fault = |port, refusal, needle: &str| {
        let at = source.find(needle).expect("refused call is in source");
        let site = c.sites.iter().position(|s| s.start as usize == at).expect("refused call retains its own source site") as u32;
        Fault::Draw { port, site, refusal }
    };
    assert_eq!(hal.faults, [
        fault(ports::PRESENT, DrawRefusal::OddWindow, "draw_image(screen, logo, logo_w, logo_h, 1, 0)"),
        fault(ports::PRESENT, DrawRefusal::SourceOutside, "present(screen, logo, 3, 0, 2, 2, 0, 0)"),
        fault(ports::PRESENT, DrawRefusal::OddWindow, "draw_image(screen, logo, logo_w, logo_h, -1, 0)"),
        fault(ports::FILL_RECT, DrawRefusal::OddWindow, "fill_rect(screen, 0, 1, 2, 2, 0)"),
        fault(ports::FILL_RECT, DrawRefusal::SourceOutside, "fill_rect(screen, 0, 0, -2, 2, 0)"),
        fault(ports::PRESENT, DrawRefusal::Handle, "present(-1, logo, 0, 0, -2, 2, 0, 0)"),
        fault(ports::FILL_RECT, DrawRefusal::Handle, "fill_rect(9, 0, 0, -2, 2, 0)"),
    ], "draw refusal priority, kind, port and user call site");
    device.step(&mut hal).expect("continuation handler runs");
    paint(&mut expected, &[31; 4], 2, [0, 0, 2, 2], [6, 6]);
    assert_eq!(device.framebuffer(), Some(expected.as_slice()), "a later draw in the faulting handler lands with only the color's low 16 bits");
    assert_eq!(hal.faults.len(), 8, "continuation handler faults exactly once");
    assert_eq!(hal.faults[7], fault(ports::PRESENT, DrawRefusal::OddWindow, "present(screen, logo, 0, 0, 2, 2, 3, 4)"), "continued handler retains the refusal's call site");

    let no_display = Profile { display: None, ..P };
    assert!(matches!(module::load(&c.module, &no_display), Err(LoadError::DisplayUnsupported { .. })), "display declaration requires the profile's display");
    assert!(error("let screen = display()\nfn setup() {}", &no_display, &[]).contains("board with a display"), "compile reports unsupported display declarations");
    let odd = Profile { display: Some(rt::DisplayProfile { width: 7, height: 8 }), ..P };
    assert!(matches!(module::load(&c.module, &odd), Err(LoadError::DisplaySize { .. })), "loader refuses odd profile dimensions");
    assert!(error("let a = display()\nlet b = display()\nfn setup() {}", &P, &[]).contains("at most one display"), "v1 refuses a second display declaration");
    for (first, second) in [("LoGo.png", "logo.jpg"), ("logo.png", "logo_w.png"), ("logo_h.png", "logo.png")] {
        let message = error("fn setup() {}", &P, &[(first, &logo_png), (second, &badge_png)]);
        assert!(message.contains(first) && message.contains(second), "image and dimension name collisions identify both files: {message}");
    }
    assert!(error("fn setup() {}", &P, &[("1logo.png", &logo_png)]).contains("1logo.png"), "invalid image name identifies its file");
    assert!(error("fn setup() {}", &P, &[("broken.png", b"not an image")]).contains("broken.png"), "decode error identifies its file");
    let shadow = compile("let logo = 37\nvar result: i32 = 0\nfn setup() { result = logo }", &P, &[], &assets)
        .unwrap_or_else(|e| panic!("program may shadow an image: {e:?}"));
    let module = module::load(&shadow.module, &P).expect("shadowed module verifies");
    assert_eq!(module.images().len(), 2, "unused and shadowed images remain in the module in v1");
    let mut shadow_device = Device::new(module, &P);
    shadow_device.boot(&mut hal).expect("shadowed setup runs");
    assert_eq!(var(&shadow, &shadow_device, "result"), 37, "program item wins over the generated image name");
    assert_eq!(shadow_device.framebuffer(), None, "available but undeclared display has no framebuffer");

    let mut bad = c.module.clone();
    bad[image_start + 1] = 3;
    assert!(matches!(module::load(&bad, &P), Err(LoadError::ImageSize { .. })), "loader refuses odd image dimensions");
    bad[image_start + 1] = 0;
    assert!(matches!(module::load(&bad, &P), Err(LoadError::ImageSize { .. })), "loader refuses zero image dimensions");
    let mut bad = c.module.clone();
    bad[image_dir + 2] -= 1;
    assert!(matches!(module::load(&bad, &P), Err(LoadError::SectionLength { kind: SEC_IMAGES, .. })), "loader refuses truncated image pixels");
    let decl_dir = (0..c.module[H_SECTIONS] as usize).map(|k| HEADER_WORDS + k * DIRECTORY_WORDS)
        .find(|at| c.module[*at] == SEC_DECLS).expect("DECLS directory row");
    let mut image_decl = c.module[decl_dir + 1] as usize;
    while c.module[image_decl] != ports::FORM_IMAGE { image_decl += DECL_HEADER_WORDS + 2 * c.module[image_decl + 3] as usize; }
    let mut bad = c.module.clone();
    bad[image_decl + DECL_HEADER_WORDS + 1] = 99;
    assert!(matches!(module::load(&bad, &P), Err(LoadError::ImageIndex { .. })), "loader refuses declarations of nonexistent images");
    let mut bad = c.module.clone();
    bad[image_decl + DECL_HEADER_WORDS + 3] += 2;
    assert!(matches!(module::load(&bad, &P), Err(LoadError::ImageDimensions { .. })), "loader requires declaration sizes to equal the carried image");
    let mut bad = c.module.clone();
    bad[image_decl + 1] = 99;
    assert!(matches!(module::load(&bad, &P), Err(LoadError::DeclHandle { .. })), "image handles must be buffer ordinals");
}

#[test]
fn surfaces_and_images_share_pixels() {
    let source = include_str!("support/surfaces.splash");
    let rgba: Vec<_> = (0..16).map(|k| [8 * (k + 16), 12 * k, 8 * (k ^ 7), 255]).collect();
    let tile = png(4, 4, &rgba);
    // Only the 48-byte surface and the word-rounded byte buffer consume RAM.
    let profile = Profile { buffer_bytes: 52, ..P };
    let c = compile(source, &profile, &[], &[("tile.png", &tile)]).expect("surface and read-only image operations compile");
    let m = module::load(&c.module, &profile).expect("read-only images fit beside the exact writable RAM budget");
    assert_eq!(m.buffers().iter().map(|b| (b.elem, b.len, b.writable)).collect::<Vec<_>>(),
        [(module::Elem::U16, 16, false), (module::Elem::U8, 1, true),
            (module::Elem::U16, 24, true), (module::Elem::U16, 16, false)],
        "generated image, byte buffer, surface and explicit image share buffer ordinals");
    assert_eq!(m.decls()[m.buffers()[3].decl as usize].handle, 3, "explicit image handle differs from its IMAGES index zero");
    let pixels: Vec<u16> = (0..16).map(|k| ((k + 16) << 11) | ((3 * k) << 5) | (k ^ 7)).collect();
    let packed: Vec<u32> = pixels.chunks_exact(2).map(|p| u32::from(p[0]) | (u32::from(p[1]) << 16)).collect();
    let mut surface: Vec<u16> = (0..24).map(|k| 0x8000 + k * 0x421).collect();
    surface[12..16].copy_from_slice(&pixels[4..8]);
    let mut device = Device::new(m, &profile);
    for boot in 0..2 {
        let mut hal = PanelHal::default();
        device.boot(&mut hal).expect("shared surface fixture boots");
        assert_eq!(var(&c, &device, "clean"), 0, "boot {boot}: writable surface clears before setup");
        for (name, expected) in [("from_image", pixels[5]), ("from_alias", pixels[15]), ("copied", pixels[6])] {
            assert_eq!(var(&c, &device, name), u32::from(expected), "boot {boot}: {name} loads the expected unsigned RGB565 pixel");
        }
        assert_eq!(device.buffers()[2], surface.chunks_exact(2).map(|p| u32::from(p[0]) | (u32::from(p[1]) << 16)).collect::<Vec<_>>(),
            "boot {boot}: the loop fills every surface element and copy replaces exactly one image row");
        let mut frame = vec![0; 64];
        assert_eq!(device.framebuffer(), Some(frame.as_slice()), "setup paints a known background");
        for phase in ["surface_first", "image_next", "clipped", "refused"] {
            let ran = device.step(&mut hal).expect("each present has its own handler");
            assert_eq!(c.lowered.entries[ran.entry as usize].name, phase, "present calls execute in timer order");
            match phase {
                "surface_first" => for y in 0..2 { for x in 0..2 { frame[y * 8 + x] = surface[(y + 1) * 6 + x + 1]; } },
                "image_next" => for y in 0..2 { for x in 0..2 { frame[y * 8 + x + 2] = pixels[(y + 1) * 4 + x + 1]; } },
                "clipped" => for y in 0..4 { for x in 0..4 { frame[(y + 2) * 8 + x] = surface[y * 6 + x + 2]; } },
                "refused" => (),
                _ => unreachable!(),
            }
            assert_eq!(device.framebuffer(), Some(frame.as_slice()), "boot {boot}, {phase}: panel equals the independent expected frame");
            for k in [0, 3] {
                assert_eq!(device.buffers()[k], packed, "boot {boot}, {phase}: image buffer {k} retains its packed pixels");
            }
            if phase != "refused" { assert!(hal.faults.is_empty(), "{phase}: valid surface and image presents never fault"); }
        }
        assert_eq!(hal.faults.iter().map(|fault| match fault {
            Fault::Draw { port, refusal, .. } => { assert_eq!(*port, ports::PRESENT, "surface refusal identifies present"); *refusal },
            other => panic!("unexpected surface fault: {other:?}"),
        }).collect::<Vec<_>>(), [DrawRefusal::SourceOutside; 4].into_iter().chain([DrawRefusal::OddWindow, DrawRefusal::Handle]).collect::<Vec<_>>(),
            "surface dimensions, signed coordinates, clipped alignment and display handles retain their refusal kinds");
    }
}

#[test]
fn image_writes_and_invalid_present_sources_are_refused() {
    let tile = png(2, 2, &[[248, 252, 248, 255]; 4]);
    let assets = [("tile.png", tile.as_slice())];
    let profile = Profile { buffer_bytes: 12, ..P };
    for body in ["tile[0] = 1", "copy(tile, 0, s, 0, 1)", "fill(tile, 0, 1, 0)"] {
        let source = format!("let s = surface(2, 2)\nfn setup() {{ {body} }}");
        let message = error(&source, &profile, &assets);
        assert!(message.contains("read-only") && message.contains("tile"), "{body}: compiler names the read-only image: {message}");
    }
    let message = error("let b = bytes(4)\nlet screen = display()\nfn setup() { present(screen, b, 0, 0, 2, 2, 0, 0) }", &profile, &[]);
    assert!(message.contains("present") && message.contains("U16") && message.contains("U8"), "present refuses the byte source by element kind: {message}");
    assert!(error("let s = surface(-2, -2)\nfn setup() {}", &profile, &[]).contains("DeclArguments"),
        "two negative dimensions cannot masquerade as a positive surface length");

    let c = compile("\
let screen = display()
let s = surface(2, 2)
let b = bytes(4)
var seed: i32 = 1
fn setup() {
    s[0] = seed
    copy(s, 0, tile, 0, 4)
    fill(s, 0, 4, 0)
    fill(b, 0, 4, 0)
    present(screen, s, 0, 0, 2, 2, 0, 0)
}
", &profile, &[], &assets).expect("mutation fixture is a valid surface program");
    let m = module::load(&c.module, &profile).expect("mutation fixture verifies before editing");
    let section = |kind| c.module[HEADER_WORDS..HEADER_WORDS + c.module[H_SECTIONS] as usize * DIRECTORY_WORDS]
        .chunks_exact(DIRECTORY_WORDS).find(|row| row[0] == kind).expect("fixture section exists")[1] as usize;
    let code = m.code();
    let entry = &m.entries()[m.entry(rt::SETUP).expect("fixture setup exists")];
    let mut pc = entry.body as usize + 1;
    let end = pc + code[entry.body as usize] as usize;
    let mut instructions = Vec::new();
    while pc < end {
        instructions.push(pc);
        pc += match code[pc] & OP_MASK {
            OP_CONST | OP_GET | OP_SET | OP_UN => 3,
            OP_LOAD | OP_STORE => 5,
            OP_PORT => 5 + code[pc + 3] as usize + code[pc + 4] as usize,
            op => panic!("linear mutation fixture has an unexpected opcode {op}"),
        };
    }
    assert_eq!(pc, end, "mutation offsets stay on instruction boundaries");
    let store = *instructions.iter().find(|&&at| code[at] & OP_MASK == OP_STORE).expect("fixture has a surface store");
    let dynamic = *instructions.iter().find(|&&at| code[at] == (OP_LOAD | REGION_STATE << SUB_SHIFT)).expect("fixture loads seed from state");
    let port = |wanted| *instructions.iter().find(|&&at| code[at] == OP_PORT && code[at + 1] == u32::from(wanted)).expect("fixture calls the requested port");
    let copy = port(ports::COPY);
    let fill = port(ports::FILL);
    let byte_fill = *instructions.iter().find(|&&at| at > fill && code[at] == OP_PORT && code[at + 1] == u32::from(ports::FILL)).expect("fixture fills a byte buffer");
    let present = port(ports::PRESENT);
    let base = section(SEC_CODE);
    let image_value = code[copy + 7];
    for (offset, value, cause) in [
        (base + store, OP_STORE | REGION_BUF_BASE << SUB_SHIFT, "store"),
        (base + copy + 5, image_value, "copy"),
        (base + fill + 5, image_value, "fill"),
        (base + present + 6, code[byte_fill + 5], "present-kind"),
        (base + present + 6, code[dynamic + 1], "present-dynamic"),
        (section(SEC_BUFS) + 3, 1, "image-writable"),
    ] {
        let mut bad = c.module.clone(); bad[offset] = value;
        let error = module::load(&bad, &profile).expect_err("edited module must be refused");
        let named = match cause {
            "store" => matches!(error, LoadError::BufferReadOnly { buffer: 0, .. }),
            "copy" => matches!(error, LoadError::BufferArgument { port, param: 0, .. } if port == u32::from(ports::COPY)),
            "fill" => matches!(error, LoadError::BufferArgument { port, param: 0, .. } if port == u32::from(ports::FILL)),
            "present-kind" | "present-dynamic" => matches!(error, LoadError::BufferArgument { port, param: 1, .. } if port == u32::from(ports::PRESENT)),
            "image-writable" => matches!(error, LoadError::BufferWritable { buffer: 0, writable: true, .. }),
            _ => unreachable!(),
        };
        assert!(named, "{cause}: loader reports its named refusal, got {error}");
    }
    let undeclared = compile("fn setup() { present(0, tile, 0, 0, 2, 2, 0, 0) }", &P, &[], &assets)
        .expect("source buffer is valid even without a display declaration");
    let mut device = Device::new(module::load(&undeclared.module, &P).expect("undeclared-display module verifies"), &P);
    let mut hal = PanelHal::default(); device.boot(&mut hal).expect("undeclared-display setup runs");
    assert!(matches!(hal.faults.as_slice(), [Fault::Draw { port: ports::PRESENT, refusal: DrawRefusal::Handle, .. }]),
        "present reports an undeclared display before touching panel memory");
}

#[test]
fn a_glyph_atlas_is_built_from_its_declaration_and_drawn_by_present() {
    // Cells of 4 x 6: an atlas of 16 columns by 14 rows of cells, Latin-1 from 32.
    let source = "\
let screen = display()
let glyphs = glyph_atlas(4, 6)
var width: i32 = 0
var height: i32 = 0
fn setup() {
    width = glyphs_w
    height = glyphs_h
    let c: i32 = 65 - 32
    present(screen, glyphs, c % 16 * 4, c / 16 * 6, 4, 6, 2, 2)
}
";
    let (w, h) = glyph_atlas_size(4, 6);
    assert_eq!((w, h), (64, 84), "sixteen cells to a row, 224 characters");
    let fresh = render_glyph_atlas(4, 6);
    assert_eq!(fresh, render_glyph_atlas(4, 6), "the same declaration renders the same pixels twice");
    assert_eq!(*glyph_atlas(4, 6), fresh, "the cached atlas is the rendered one");
    let cell = |c: usize| -> Vec<u16> {
        let (x, y) = ((c - 32) % 16 * 4, (c - 32) / 16 * 6);
        (0..6).flat_map(|row| fresh[(y + row) * 64 + x..(y + row) * 64 + x + 4].to_vec()).collect()
    };
    assert!(cell(32).iter().all(|&p| p == 0), "a space is a black cell");
    assert!(cell(65).iter().any(|&p| p != 0) && cell(233).iter().any(|&p| p != 0), "A and e-acute have glyphs");
    assert!(cell(127).iter().any(|&p| p == 0xFFFF), "character 127's cell is the box for characters outside the atlas");

    let picture = png(2, 2, &[[255, 0, 0, 255]; 4]);
    let c = compile(source, &P, &[], &[("logo.png", &picture)]).unwrap_or_else(|e| panic!("atlas program compiles: {e:?}"));
    assert_eq!(c.images, ["logo", "glyphs"], "an atlas follows the folder's pictures");
    let module = module::load(&c.module, &P).expect("atlas module verifies");
    let atlas = &module.images()[1];
    assert_eq!((atlas.width, atlas.height), (w, h), "the module carries the atlas at its size");
    let carried: Vec<u16> = (0..h).flat_map(|y| (0..w).map(move |x| atlas.pixel(x, y).expect("in-range atlas pixel"))).collect();
    assert_eq!(carried, fresh, "the module carries the rendered atlas");
    let mut device = Device::new(module, &P);
    let mut hal = PanelHal::default();
    device.boot(&mut hal).expect("setup runs");
    assert_eq!((var(&c, &device, "width"), var(&c, &device, "height")), (64, 84), "glyphs_w and glyphs_h are declared like a picture's");
    let mut expected = vec![0u16; 64];
    for (k, &p) in cell(65).iter().enumerate() { expected[(2 + k / 4) * 8 + 2 + k % 4] = p; }
    assert_eq!(device.framebuffer(), Some(expected.as_slice()), "present draws A's cell");
    assert!(hal.faults.is_empty(), "drawing a glyph never faults: {:?}", hal.faults);

    for (bad, why) in [("glyph_atlas(3, 6)", "an odd width"), ("glyph_atlas(4)", "a missing height"), ("glyph_atlas(4, 6.5)", "a fraction")] {
        let message = error(&format!("let glyphs = {bad}\nfn setup() {{}}"), &P, &[]);
        assert!(message.contains("even whole numbers"), "{why} is refused: {message}");
    }
    let message = error("let glyphs = glyph_atlas(4, 6)\nfn setup() {}", &P, &[("glyphs_w.png", &picture)]);
    assert!(message.contains("glyphs") && message.contains("glyphs_w"), "an atlas and a picture declaring the same names are both named: {message}");
}
