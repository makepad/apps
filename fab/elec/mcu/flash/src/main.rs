use fab_elec_mcu_flash::{esp_rom::{self, Entry, EspRegion, EspWrite, Exit, Grant, Layout, Waiver}, identify::{identify, Board}, rate::Rate, stk500, tether, watch};
use makepad_serial::line::{Line, Step, Traced, TtyLine};
use std::{env, fs, io, path::{Path, PathBuf}, process::ExitCode, time::{Duration, Instant}};

const USAGE: &str = "Usage:\n  fab-elec-mcu-flash identify <port> [--trace]\n  fab-elec-mcu-flash listen <port> [--baud 115200] [--ms 2000] [--reset none|dtr|en]\n  fab-elec-mcu-flash watch <port> [--baud 117647] [--reset dtr|usj|none] [--ms N]\n  fab-elec-mcu-flash tether <port> --picture <image> [--picture <image> ...] [--hold-ms 2000] [--baud 117647] [--reset dtr|usj|none] [--ms N]\n  fab-elec-mcu-flash tether <port> --rate [--phase-ms 10000] [--baud 117647] [--reset dtr|usj|none] [--ms N]\n  fab-elec-mcu-flash flash <port> <image.bin> --signature 1e950f [--baud 115200]\n  fab-elec-mcu-flash read <port> <out.bin> --bytes N\n  fab-elec-mcu-flash esp-info <port> [--no-reset] [--stay] [--trace]\n  fab-elec-mcu-flash esp-md5 <port> <offset> <size> --flash-size <bytes> [--no-reset] [--stay] [--trace]\n  fab-elec-mcu-flash esp-spi1-probe <port> --flash-size <bytes> [--no-reset] [--trace]\n  fab-elec-mcu-flash esp-read <port> --flash-size <bytes> --layout low|high --out <dir> [--no-reset] [--trace]\n  fab-elec-mcu-flash esp-boot <port> [--trace]\n  fab-elec-mcu-flash esp-read-reg <port> <address> [<address> ...] [--no-reset] [--trace]\n  fab-elec-mcu-flash esp-clear-force-download <port> --chip-id <n> [--no-reset] [--trace]\n  fab-elec-mcu-flash esp-write <port> --chip-id <n> --flash-size <bytes> --grant \"<device>:<said by>\" --no-backup \"<reason>\" --at <offset> <file> [--at <offset> <file> ...] [--trace]\nESP numbers accept decimal or 0x hex. --no-reset connects to a chip already in the ROM loader without the download reset.\nesp-spi1-probe, esp-read, esp-read-reg and esp-clear-force-download always stay in the ROM loader. esp-read writes flash.bin, flash.sha256 and report.txt only after ROM MD5 verification, and refuses an existing flash.bin.\n--remote, anywhere, in place of <port>: the device of a devhub serial claim (`devhub serial claim <device> -- <command> --remote`), through MAKEPAD_SERIAL_SOCKET, named MAKEPAD_SERIAL_DEVICE.";

/// How `listen` resets the board first: not at all, an Arduino's DTR pulse,
/// or an ESP board's EN pulse (RTS) with IO0 released (a normal boot, whose
/// ROM prints its banner).
#[derive(Clone, Copy)]
enum Reset {
    None,
    Dtr,
    En,
}

enum Command {
    Identify { port: String, trace: bool },
    Listen { port: String, baud: u32, ms: u64, reset: Reset },
    Watch { port: String, baud: u32, ms: Option<u64>, reset: watch::Reset },
    Tether { port: String, baud: u32, ms: Option<u64>, reset: watch::Reset, mode: TetherMode },
    Flash { port: String, image: String, signature: [u8; 3], baud: Option<u32> },
    Read { port: String, output: String, bytes: usize },
    Esp { port: String, trace: bool, action: EspAction },
}

enum TetherMode {
    Pictures { pictures: Vec<String>, hold_ms: u64 },
    Rate { phase_ms: u64 },
}

enum EspAction {
    Info { entry: Entry, leave: Exit },
    Boot,
    Md5 { entry: Entry, offset: u32, size: u32, flash_size: u32, leave: Exit },
    Spi1Probe { entry: Entry, flash_size: u32 },
    ReadReg { entry: Entry, addresses: Vec<u32> },
    ClearForceDownload { entry: Entry, chip_id: u32 },
    Read { entry: Entry, flash_size: u32, layout: Layout, output: String },
    Write {
        chip_id: u32,
        flash_size: u32,
        grant: Grant,
        waiver: Waiver,
        files: Vec<(u32, String)>,
    },
}

fn number(text: &str) -> Option<u32> {
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn parse_esp(command: &str, port: String, args: &[String]) -> Option<Command> {
    let (mut trace, mut stay, mut no_reset) = (false, false, false);
    let (mut chip_id, mut flash_size, mut grant, mut waiver) = (None, None, None, None);
    let (mut layout, mut output) = (None, None);
    let mut files = Vec::new();
    let mut at = 0;
    let region = if command == "esp-md5" {
        at = 2;
        Some((number(args.first()?)?, number(args.get(1)?)?))
    } else { None };
    let mut addresses = Vec::new();
    if command == "esp-read-reg" {
        while let Some(address) = args.get(at).and_then(|text| number(text)) {
            addresses.push(address);
            at += 1;
        }
    }
    while at < args.len() {
        match args[at].as_str() {
            "--trace" if !trace => { trace = true; at += 1; }
            "--stay" if !stay && (command == "esp-info" || command == "esp-md5") => { stay = true; at += 1; }
            "--no-reset" if !no_reset && matches!(command, "esp-info" | "esp-md5" | "esp-spi1-probe" | "esp-read" | "esp-read-reg" | "esp-clear-force-download") => {
                no_reset = true;
                at += 1;
            }
            "--flash-size" if matches!(command, "esp-md5" | "esp-write" | "esp-spi1-probe" | "esp-read") && flash_size.is_none() => {
                flash_size = Some(number(args.get(at + 1)?)?);
                at += 2;
            }
            "--layout" if command == "esp-read" && layout.is_none() => {
                layout = Some(match args.get(at + 1)?.as_str() {
                    "low" => Layout::Low,
                    "high" => Layout::High,
                    _ => return None,
                });
                at += 2;
            }
            "--out" if command == "esp-read" && output.is_none() => {
                output = Some(args.get(at + 1)?.clone());
                at += 2;
            }
            "--chip-id" if matches!(command, "esp-write" | "esp-clear-force-download") && chip_id.is_none() => {
                chip_id = Some(number(args.get(at + 1)?)?);
                at += 2;
            }
            "--grant" if command == "esp-write" && grant.is_none() => {
                let (device, said_by) = args.get(at + 1)?.split_once(':')?;
                if device.trim().is_empty() || said_by.trim().is_empty() { return None; }
                grant = Some(Grant { device: device.into(), said_by: said_by.into() });
                at += 2;
            }
            "--no-backup" if command == "esp-write" && waiver.is_none() => {
                let reason = args.get(at + 1)?.clone();
                if reason.trim().is_empty() { return None; }
                waiver = Some(Waiver { reason });
                at += 2;
            }
            "--at" if command == "esp-write" => {
                files.push((number(args.get(at + 1)?)?, args.get(at + 2)?.clone()));
                at += 3;
            }
            _ => return None,
        }
    }
    let leave = if stay { Exit::Stay } else { Exit::Boot };
    let entry = if no_reset { Entry::NoReset } else { Entry::Reset };
    let action = match command {
        "esp-info" => EspAction::Info { entry, leave },
        "esp-boot" => EspAction::Boot,
        "esp-md5" => {
            let (offset, size) = region?;
            EspAction::Md5 { entry, offset, size, flash_size: flash_size?, leave }
        }
        "esp-spi1-probe" => EspAction::Spi1Probe { entry, flash_size: flash_size? },
        "esp-read-reg" if !addresses.is_empty() => EspAction::ReadReg { entry, addresses },
        "esp-clear-force-download" => EspAction::ClearForceDownload { entry, chip_id: chip_id? },
        "esp-read" => EspAction::Read { entry, flash_size: flash_size?, layout: layout?, output: output? },
        "esp-write" if !files.is_empty() => EspAction::Write {
            chip_id: chip_id?, flash_size: flash_size?, grant: grant?, waiver: waiver?, files,
        },
        _ => return None,
    };
    Some(Command::Esp { port, trace, action })
}

fn signature(text: &str) -> Option<[u8; 3]> {
    if text.len() != 6 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut result = [0; 3];
    for (i, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(result)
}

/// The line a command runs over: a local port, or (`--remote`, under `devhub serial claim`) the
/// claimed device on whichever PC it is plugged into, through the claim's socket.
enum Port {
    Tty(TtyLine),
    #[cfg(unix)]
    Remote(makepad_serial::remote::RemoteLine<std::os::unix::net::UnixStream>),
}

fn open(port: &str, baud: u32, remote: Option<&Path>) -> Result<Port, String> {
    let Some(socket) = remote else {
        return TtyLine::open(port, baud).map(Port::Tty).map_err(|err| format!("{port}: {err}"));
    };
    #[cfg(unix)]
    {
        use makepad_serial::remote::{Mode, RemoteLine};
        let stream = std::os::unix::net::UnixStream::connect(socket)
            .map_err(|err| format!("{port}: the claim's socket {} does not answer: {err}", socket.display()))?;
        RemoteLine::open(stream, port, Mode::Serial, baud).map(Port::Remote).map_err(|err| format!("{port}: {err}"))
    }
    #[cfg(not(unix))]
    Err(format!("{port}: --remote needs a Unix socket ({} on this OS)", socket.display()))
}

macro_rules! each_port {
    ($port:expr, $line:ident => $body:expr) => {
        match $port {
            Port::Tty($line) => $body,
            #[cfg(unix)]
            Port::Remote($line) => $body,
        }
    };
}

impl Line for Port {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> { each_port!(self, line => line.write_all(bytes)) }
    fn read_byte(&mut self, timeout: Duration) -> io::Result<Option<u8>> { each_port!(self, line => line.read_byte(timeout)) }
    fn set_dtr(&mut self, on: bool) -> io::Result<()> { each_port!(self, line => line.set_dtr(on)) }
    fn set_rts(&mut self, on: bool) -> io::Result<()> { each_port!(self, line => line.set_rts(on)) }
    fn set_baud(&mut self, baud: u32) -> io::Result<()> { each_port!(self, line => line.set_baud(baud)) }
    fn drain_input(&mut self) -> io::Result<()> { each_port!(self, line => line.drain_input()) }
    fn sequence(&mut self, steps: &[Step]) -> Result<(), (usize, io::Error)> { each_port!(self, line => line.sequence(steps)) }
    fn reopen(&mut self) -> io::Result<()> { each_port!(self, line => line.reopen()) }
    fn sleep(&mut self, d: Duration) { each_port!(self, line => line.sleep(d)) }
    fn now(&self) -> Instant { each_port!(self, line => line.now()) }
}

/// The command, and the claim's socket when `--remote` stands in for its port: the port's name
/// is then the claimed device's label (`MAKEPAD_SERIAL_DEVICE`).
fn parse() -> Option<(Command, Option<PathBuf>)> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let remote = match args.iter().position(|arg| arg == "--remote") {
        Some(at) => {
            args.remove(at);
            let socket = env::var_os("MAKEPAD_SERIAL_SOCKET").filter(|socket| !socket.is_empty())?;
            let label = env::var("MAKEPAD_SERIAL_DEVICE").unwrap_or_else(|_| "the claimed device".into());
            args.insert(1.min(args.len()), label);
            Some(PathBuf::from(socket))
        }
        None => None,
    };
    parse_command(&args).map(|command| (command, remote))
}

fn parse_command(args: &[String]) -> Option<Command> {
    let command = args.first()?.as_str();
    let port = args.get(1)?.clone();
    match command {
        "esp-info" | "esp-md5" | "esp-write" | "esp-boot" | "esp-spi1-probe" | "esp-read" | "esp-read-reg" | "esp-clear-force-download" => parse_esp(command, port, &args[2..]),
        "identify" if args.len() == 2 => Some(Command::Identify { port, trace: false }),
        "identify" if args.len() == 3 && args[2] == "--trace" => Some(Command::Identify { port, trace: true }),
        "listen" => {
            let (mut baud, mut ms, mut reset) = (115200, 2000, Reset::None);
            let mut options = args[2..].chunks_exact(2);
            for option in &mut options {
                match option[0].as_str() {
                    "--baud" => {
                        baud = option[1].parse::<u32>().ok()?;
                        if !matches!(baud, 57600 | 115200) { return None; }
                    }
                    "--ms" => ms = option[1].parse::<u64>().ok()?,
                    "--reset" => reset = match option[1].as_str() { "none" => Reset::None, "dtr" => Reset::Dtr, "en" => Reset::En, _ => return None },
                    _ => return None,
                }
            }
            if !options.remainder().is_empty() { return None; }
            Some(Command::Listen { port, baud, ms, reset })
        }
        "watch" => {
            let (mut baud, mut ms, mut reset) = (117647, None, watch::Reset::Dtr);
            let mut options = args[2..].chunks_exact(2);
            for option in &mut options {
                match option[0].as_str() {
                    "--baud" => {
                        baud = option[1].parse::<u32>().ok()?;
                        if baud == 0 { return None; }
                    }
                    "--ms" => ms = Some(option[1].parse::<u64>().ok()?),
                    "--reset" => reset = match option[1].as_str() {
                        "dtr" => watch::Reset::Dtr,
                        "usj" => watch::Reset::Usj,
                        "none" => watch::Reset::None,
                        _ => return None,
                    },
                    _ => return None,
                }
            }
            if !options.remainder().is_empty() { return None; }
            Some(Command::Watch { port, baud, ms, reset })
        }
        "tether" => {
            let (mut baud, mut ms, mut reset) = (117647, None, watch::Reset::Dtr);
            let (mut pictures, mut hold_ms, mut rate, mut phase_ms) = (Vec::new(), None, false, None);
            let mut options = args[2..].iter();
            while let Some(option) = options.next() {
                match option.as_str() {
                    "--baud" => {
                        baud = options.next()?.parse::<u32>().ok()?;
                        if baud == 0 { return None; }
                    }
                    "--ms" => ms = Some(options.next()?.parse::<u64>().ok()?),
                    "--reset" => reset = match options.next()?.as_str() {
                        "dtr" => watch::Reset::Dtr,
                        "usj" => watch::Reset::Usj,
                        "none" => watch::Reset::None,
                        _ => return None,
                    },
                    "--picture" => {
                        let picture = options.next()?;
                        if picture == "--rate" { return None; }
                        pictures.push(picture.clone());
                    }
                    "--hold-ms" => hold_ms = Some(options.next()?.parse().ok()?),
                    "--rate" if !rate => rate = true,
                    "--phase-ms" => phase_ms = Some(options.next()?.parse().ok()?),
                    _ => return None,
                }
            }
            let mode = match (rate, pictures.len()) {
                (true, 0) if hold_ms.is_none() => TetherMode::Rate { phase_ms: phase_ms.unwrap_or(10_000) },
                (false, 1..=256) if phase_ms.is_none() => TetherMode::Pictures { pictures, hold_ms: hold_ms.unwrap_or(2000) },
                _ => return None,
            };
            Some(Command::Tether { port, baud, ms, reset, mode })
        }
        "flash" => {
            let image = args.get(2)?.clone();
            let mut sig = None;
            let mut baud = None;
            let mut options = args[3..].chunks_exact(2);
            for option in &mut options {
                match option[0].as_str() {
                    "--signature" if sig.is_none() => sig = Some(signature(&option[1])?),
                    "--baud" if baud.is_none() => {
                        let value = option[1].parse::<u32>().ok()?;
                        if !matches!(value, 57600 | 115200) { return None; }
                        baud = Some(value);
                    }
                    _ => return None,
                }
            }
            if !options.remainder().is_empty() { return None; }
            Some(Command::Flash { port, image, signature: sig?, baud })
        }
        "read" if args.len() == 5 && args[3] == "--bytes" => {
            Some(Command::Read { port, output: args[2].clone(), bytes: args[4].parse().ok()? })
        }
        _ => None,
    }
}

fn run_esp<L: Line>(line: &mut L, action: EspAction) -> Result<(), String> {
    match action {
        EspAction::Info { entry, leave } => { esp_rom::esp_info(line, entry, leave).map_err(|err| err.to_string())?; }
        EspAction::Boot => { esp_rom::esp_boot(line).map_err(|err| err.to_string())?; }
        EspAction::Md5 { entry, offset, size, flash_size, leave } => {
            esp_rom::esp_md5(line, entry, offset, size, flash_size, leave).map_err(|err| err.to_string())?;
        }
        EspAction::Spi1Probe { entry, flash_size } => {
            esp_rom::esp_spi1_probe(line, entry, flash_size, &mut std::io::stdout().lock()).map_err(|err| err.to_string())?;
        }
        EspAction::ReadReg { entry, addresses } => {
            esp_rom::esp_read_reg(line, entry, &addresses).map_err(|err| err.to_string())?;
        }
        EspAction::ClearForceDownload { entry, chip_id } => {
            esp_rom::esp_clear_force_download(line, entry, chip_id).map_err(|err| err.to_string())?;
        }
        EspAction::Read { entry, flash_size, layout, output } => {
            esp_rom::esp_read(line, entry, flash_size, layout, Path::new(&output)).map_err(|err| err.to_string())?;
        }
        EspAction::Write { chip_id, flash_size, grant, waiver, files } => {
            let mut regions = Vec::with_capacity(files.len());
            for (offset, path) in files {
                let bytes = fs::read(&path).map_err(|err| format!("cannot read {path}: {err}"))?;
                regions.push(EspRegion { offset, bytes });
            }
            let plan = EspWrite { regions, flash_size, expect_chip_id: chip_id, grant, waiver };
            esp_rom::esp_write(line, &plan, &mut |_| {}).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn run(command: Command, remote: Option<&Path>) -> Result<(), String> {
    match command {
        Command::Esp { port, trace, action } => {
            // Open with the existing transport baud. TRM Table 33.3-1:
            // USB Serial/JTAG accepts but ignores SET_LINE_CODING.
            let mut line = open(&port, 115200, remote)?;
            let result = if trace { run_esp(&mut Traced::new(line), action) } else { run_esp(&mut line, action) };
            result.map_err(|err| format!("{port}: {err}"))?;
        }
        Command::Identify { port, trace } => {
            let line = open(&port, 115200, remote)?;
            let board = if trace { identify(&mut Traced::new(line)) } else { identify(&mut { line }) };
            println!("{port}: {board}");
        }
        Command::Listen { port, baud, ms, reset } => {
            use Step::*;
            let mut line = open(&port, baud, remote)?;
            let io = |err: std::io::Error| format!("{port}: {err}");
            match reset {
                Reset::None => {}
                Reset::Dtr => {
                    line.sequence(&[
                        Dtr(false), Rts(false), Wait(Duration::from_millis(250)), Dtr(true), Rts(true),
                    ]).map_err(|(_, err)| io(err))?;
                }
                Reset::En => {
                    line.sequence(&[Dtr(false), Rts(true), Wait(Duration::from_millis(100)), Rts(false)])
                        .map_err(|(_, err)| io(err))?;
                }
            }
            let (start, total) = (Instant::now(), Duration::from_millis(ms));
            let mut got = Vec::new();
            while let Some(left) = total.checked_sub(start.elapsed()) {
                match line.read_byte(left).map_err(io)? {
                    Some(byte) => got.push(byte),
                    None => break,
                }
            }
            // Printable ASCII as is, everything else escaped: the board's
            // bytes cannot drive the terminal.
            let mut text = String::new();
            for byte in &got {
                match *byte {
                    b'\n' => text.push('\n'),
                    0x20..=0x7e => text.push(*byte as char),
                    other => text.push_str(&format!("\\x{other:02x}")),
                }
            }
            println!("{port}: {} bytes at {baud} in {ms} ms\n{text}", got.len());
        }
        Command::Watch { port, baud, ms, reset } => {
            // Without --ms it streams until the process is stopped.
            let mut line = open(&port, baud, remote)?;
            watch::watch_until(&mut line, reset, ms.map(Duration::from_millis), &mut std::io::stdout().lock(), || false)
                .map_err(|err| format!("{port}: {err}"))?;
        }
        Command::Tether { port, baud, ms, reset, mode } => {
            let mut peer: Box<dyn watch::Peer> = match mode {
                TetherMode::Pictures { pictures, hold_ms } => {
                    // Each picture is decoded and centred on the handheld's panel before the port opens.
                    let (width, height) = tether::PANEL;
                    let mut panel = Vec::new();
                    for picture in &pictures {
                        let bytes = fs::read(picture).map_err(|err| format!("cannot read picture: {err}"))?;
                        let (w, h, rgba) = makepad_image_decode::decode_rgba(&bytes)
                            .map_err(|err| format!("cannot decode a picture: {err:?}"))?;
                        panel.push(tether::centred(&rgba, w, h, width, height));
                        println!("picture {} {w}x{h} on the {width}x{height} panel", panel.len() - 1);
                    }
                    Box::new(tether::Pictures::new(&panel, Duration::from_millis(hold_ms)))
                }
                TetherMode::Rate { phase_ms } => Box::new(Rate::new(Duration::from_millis(phase_ms))),
            };
            let mut line = open(&port, baud, remote)?;
            watch::tether(&mut line, reset, ms.map(Duration::from_millis), &mut std::io::stdout().lock(), peer.as_mut())
                .map_err(|err| format!("{port}: {err}"))?;
        }
        Command::Flash { port, image, signature, baud } => {
            let mut line = open(&port, 115200, remote)?;
            let boot = match identify(&mut line) {
                Board::Stk500(boot) => boot,
                board => return Err(format!("{port}: cannot flash {board}")),
            };
            if boot.signature != signature {
                let err = stk500::Error::WrongChip { want: signature, got: boot.signature };
                return Err(format!("{port}: {err}"));
            }
            let baud = match baud { Some(baud) => baud, None => boot.baud };
            let image = fs::read(&image).map_err(|err| format!("cannot read image: {err}"))?;
            let pages = stk500::write_flash(&mut line, baud, signature, &image)
                .map_err(|err| format!("{port}: {err}"))?;
            println!("{port}: flashed {} bytes in {pages} pages, verified", image.len());
        }
        Command::Read { port, output, bytes } => {
            let mut line = open(&port, 115200, remote)?;
            let boot = match identify(&mut line) {
                Board::Stk500(boot) => boot,
                board => return Err(format!("{port}: cannot read flash from {board}")),
            };
            let image = stk500::read_flash(&mut line, boot.baud, bytes)
                .map_err(|err| format!("{port}: {err}"))?;
            fs::write(output, &image).map_err(|err| format!("cannot write output: {err}"))?;
            println!("{port}: read {} bytes", image.len());
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let Some((command, remote)) = parse() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(command, remote.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Port names and OS errors cannot inject extra diagnostic lines.
            eprintln!("{}", err.replace(['\r', '\n'], " "));
            ExitCode::from(1)
        }
    }
}
