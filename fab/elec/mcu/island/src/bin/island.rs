use fab_elec_mcu_island::{audit, import_map::parse_roots, link, parse_glue, Archive, ImportMap, IslandLayout, Region};
use std::{collections::BTreeMap, env, fs, io::{self, Write}, process::ExitCode};

fn number(value: &str) -> Result<u32, String> {
    let result = if let Some(hex) = value.strip_prefix("0x") {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(format!("invalid number {value}")); }
        u32::from_str_radix(hex, 16)
    } else {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) { return Err(format!("invalid number {value}")); }
        value.parse()
    };
    result.map_err(|_| format!("number out of range: {value}"))
}

fn region(value: &str) -> Result<Region, String> {
    let (start, bytes) = value.split_once(':').ok_or_else(|| format!("expected 0xADDR:BYTES, got {value}"))?;
    Ok(Region { start: number(start)?, bytes: number(bytes)? })
}

fn read_text(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

fn run() -> Result<bool, String> {
    let mut args = env::args().skip(1);
    let usage = "usage: island audit <roots> [--map <imports>] <archive.a>...\n       island link <roots> --map <imports> --glue <file> --mmu-page N --window 0xADDR:BYTES --window-exec 0xADDR [--flash-bytes BYTES] --out <image> --symbols <map> <archive.a>...\n       link defaults --flash-bytes to 1048576 (the voicebox island reservation)\n       glue file: glue <name> text <align> [entry <offset>] hex <bytes>\n                  glue <name> dram|rodata <align> hex <bytes>\n                  glue <name> dram|rodata <align> <size> zero\n                  reloc <earlier glue name> <word offset> <pack symbol, gate or glue name>";
    let command = args.next().ok_or(usage)?;
    if !matches!(command.as_str(), "audit" | "link") { return Err(usage.into()); }
    let roots_path = args.next().ok_or(usage)?;
    let roots_text = read_text(&roots_path)?;
    let roots = parse_roots(&roots_text).map_err(|e| format!("{roots_path}: {e}"))?;
    let mut options = BTreeMap::new();
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        if arg.starts_with('-') {
            if arg != "--map" && (command == "audit" || !matches!(arg.as_str(),
                "--glue" | "--mmu-page" | "--window" | "--window-exec" | "--flash-bytes" | "--out" | "--symbols"))
            { return Err(format!("unknown argument {arg}; {usage}")); }
            let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
            if options.insert(arg.clone(), value).is_some() { return Err(format!("duplicate {arg}")); }
        } else { paths.push(arg); }
    }
    if paths.is_empty() { return Err(usage.into()); }
    let map = options.get("--map").map(|path| {
        ImportMap::parse(&read_text(path)?).map_err(|e| format!("{path}: {e}"))
    }).transpose()?;
    let data = paths.iter().map(|path| fs::read(path).map_err(|e| format!("{path}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let archives: Vec<_> = paths.iter().zip(&data).map(|(name, bytes)| Archive { name, data: bytes }).collect();
    if command == "audit" {
        let result = audit(&archives, &roots, map.as_ref()).map_err(|e| e.to_string())?;
        write!(io::stdout().lock(), "{result}").map_err(|e| format!("report: {e}"))?;
        return Ok(result.passed());
    }
    let required = |name: &str| options.get(name).map(String::as_str).ok_or_else(|| format!("missing {name}; {usage}"));
    let map = map.as_ref().ok_or("link requires --map")?;
    let glue_path = required("--glue")?;
    let glue = parse_glue(&read_text(glue_path)?).map_err(|e| format!("{glue_path}: {e}"))?;
    let layout = IslandLayout {
        mmu_page: number(required("--mmu-page")?)?,
        flash_bytes: options.get("--flash-bytes").map(|v| number(v)).transpose()?.unwrap_or(1024 * 1024),
        window: region(required("--window")?)?,
        window_exec: number(required("--window-exec")?)?,
    };
    let out = required("--out")?;
    let symbols = required("--symbols")?;
    if out == symbols || paths.iter().any(|p| p == out || p == symbols)
        || [Some(roots_path.as_str()), options.get("--map").map(String::as_str), Some(glue_path)]
            .into_iter().flatten().any(|p| p == out || p == symbols)
    { return Err("output paths must differ from each other and all input paths".into()); }
    let result = link(&archives, &roots, map, &glue, &layout).map_err(|e| e.to_string())?;
    fs::write(out, &result.image).map_err(|e| format!("{out}: {e}"))?;
    fs::write(symbols, &result.map).map_err(|e| format!("{symbols}: {e}"))?;
    write!(io::stdout().lock(), "{}", result.size_report(&layout)).map_err(|e| format!("report: {e}"))?;
    Ok(true)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    }
}
