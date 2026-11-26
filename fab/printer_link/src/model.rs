use makepad_strict_json::{self as json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NozzleSide { Left, Right }

impl NozzleSide {
    pub fn label(self) -> &'static str {
        match self { Self::Left => "Left", Self::Right => "Right" }
    }
}

/// A reported head id, interpreted using the report's [`PrinterState::numbered_heads`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudNozzleId(pub u8);
/// Slicer: left = 1, right = 2; zero is automatic, not a side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlicerNozzleId(pub u8);
/// Preview/toolhead: left = 0, right = 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewNozzleId(pub u8);

impl CloudNozzleId {
    pub fn side(self) -> Option<NozzleSide> {
        match self.0 { 0 => Some(NozzleSide::Right), 1 => Some(NozzleSide::Left), _ => None }
    }
}
impl SlicerNozzleId {
    pub fn side(self) -> Option<NozzleSide> {
        match self.0 { 1 => Some(NozzleSide::Left), 2 => Some(NozzleSide::Right), _ => None }
    }
}
impl PreviewNozzleId {
    pub fn side(self) -> Option<NozzleSide> {
        match self.0 { 0 => Some(NozzleSide::Left), 1 => Some(NozzleSide::Right), _ => None }
    }
}

/// sRGB components, with alpha unchanged. Convert RGB before perceptual comparison.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour(pub [f32; 4]);

impl Colour {
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 8 || !text.is_ascii() { return None; }
        let n = u32::from_str_radix(text, 16).ok()?;
        Some(Self([(n >> 24) as u8 as f32 / 255.0, (n >> 16) as u8 as f32 / 255.0, (n >> 8) as u8 as f32 / 255.0, n as u8 as f32 / 255.0]))
    }

    pub fn linear(self) -> [f32; 4] {
        let [r, g, b, a] = self.0;
        let convert = |x: f32| if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
        [convert(r), convert(g), convert(b), a]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayId { pub ams: u8, pub slot: u8 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadedSlot {
    Tray(TrayId),
    /// The protocol uses the same sentinels for an external path and no tray.
    NoneOrExternal,
}

fn loaded(v: Option<&Value>) -> Option<LoadedSlot> {
    let n = u16::try_from(integer(v)?).ok()?;
    Some(match n {
        0xffff | 0xfeff => LoadedSlot::NoneOrExternal,
        _ => LoadedSlot::Tray(TrayId { ams: (n >> 8) as u8, slot: n as u8 }),
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Temperature { pub current: Option<f64>, pub target: Option<f64> }

fn packed(v: Option<&Value>) -> Temperature {
    let n = integer(v).and_then(|n| u32::try_from(n).ok());
    Temperature { current: n.map(|n| (n & 0xffff) as f64), target: n.map(|n| (n >> 16) as f64) }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extruder {
    pub id: Option<CloudNozzleId>,
    pub temperature: Temperature,
    pub loaded: Option<LoadedSlot>,
    pub target: Option<LoadedSlot>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nozzle {
    pub id: Option<CloudNozzleId>,
    pub kind: Option<String>,
    pub diameter: Option<f64>,
}

/// Whether a reported nozzle kind is abrasion-resistant; unknown kinds stay unknown.
pub fn hardened(kind: &str) -> Option<bool> {
    let lower = kind.to_ascii_lowercase();
    if lower.contains("hardened") || lower.contains("tungsten") { return Some(true); }
    if lower.contains("stainless") || lower.contains("brass") { return Some(false); }
    let code = kind.as_bytes();
    if code.len() == 4 && code[0] == b'H' && code[1].is_ascii_uppercase() && code[2..].iter().all(u8::is_ascii_digit) { return Some(true); }
    None
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tray {
    pub id: Option<TrayId>,
    pub present: Option<bool>,
    pub material: Option<String>,
    pub colour: Option<Colour>,
    pub info_idx: Option<String>,
    pub remain: Option<f64>,
    pub nozzle_temp_min: Option<f64>,
    pub nozzle_temp_max: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AmsUnit {
    pub id: Option<u8>,
    pub kind: Option<u8>,
    pub extruder: Option<CloudNozzleId>,
    /// A switcher input is not an extruder id; do not guess its nozzle side.
    pub switcher_input: Option<u8>,
    pub humidity: Option<f64>,
    pub humidity_percent: Option<f64>,
    pub temperature: Option<f64>,
    pub trays: Vec<Tray>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmsCode {
    pub module: u8,
    pub instance: u8,
    pub part: u8,
    pub reserved: u8,
    pub severity: u16,
    pub message: u16,
}

impl HmsCode {
    pub fn code(&self) -> String {
        format!("HMS_{:02X}{:02X}_{:02X}{:02X}_{:04X}_{:04X}", self.module, self.instance, self.part, self.reserved, self.severity, self.message)
    }
}

/// How hard the fans run, as the printer reports them (levels, 15 is full).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fans {
    pub part: Option<u32>,
    pub heatbreak: Option<u32>,
    pub aux: Option<u32>,
    pub chamber: Option<u32>,
}

/// Only telemetry fields are published. No raw report, address or full serial: [`Self::details`]
/// is the rest of the report with who and where the printer is taken out ([`private_key`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrinterState {
    /// The heads are numbered in the machine's order from 0, each with one spool,
    /// `TrayId { ams: head, slot: 0 }`. False: the first family's two sides.
    pub numbered_heads: bool,
    pub gcode_state: Option<String>,
    pub progress: Option<f64>,
    pub layer: Option<u32>,
    pub total_layers: Option<u32>,
    pub bed: Temperature,
    pub chamber: Temperature,
    pub extruders: Vec<Extruder>,
    pub nozzles: Vec<Nozzle>,
    pub ams: Vec<AmsUnit>,
    pub external: Vec<Tray>,
    pub hms: Vec<HmsCode>,
    pub print_error: Option<u32>,
    /// Minutes left in the running job.
    pub remaining_minutes: Option<f64>,
    /// The printer's current stage id (`stg_cur`; -1 = none).
    pub stage: Option<i64>,
    /// The running or last job's name as the printer reports it.
    pub job_name: Option<String>,
    /// How far the printer has read the job file before it starts (0..100).
    pub prepare_percent: Option<f64>,
    /// Removable storage present (`sdcard`, or storage bits of `aux`).
    pub storage: Option<bool>,
    /// The printer offers its RTSPS live view.
    pub camera: bool,
    pub fans: Fans,
    /// The print speed setting (`spd_lvl`) and its percent of normal (`spd_mag`).
    pub speed_level: Option<u32>,
    pub speed_percent: Option<f64>,
    /// Wi-Fi strength as reported ("-52dBm").
    pub wifi: Option<String>,
    /// The extruder printing now (`device.extruder.state`).
    pub active_extruder: Option<CloudNozzleId>,
    /// A virtual printer's job clock, in the seconds a preview's timeline counts (a real
    /// printer reports none).
    pub print_seconds: Option<f64>,
    /// The job seconds that clock runs per second: 0 while it is held or the job has ended.
    pub print_rate: Option<f64>,
    /// The firmware updater's state (`upgrade_state.status`).
    pub firmware: Option<String>,
    /// What else the printer says went wrong: its other error fields, (field, value).
    pub errors: Vec<(String, String)>,
    /// The rest of the report, flattened to (path, value), for the raw details.
    pub details: Vec<(String, String)>,
}

/// Fields shown by name elsewhere, or that only frame the report.
const SHOWN: &[&str] = &[
    "msg", "sequence_id", "command", "gcode_state", "mc_percent", "layer_num", "total_layer_num", "bed_temper", "bed_target_temper",
    "mc_remaining_time", "stg_cur", "subtask_name", "gcode_file_prepare_percent", "print_error", "hms", "cooling_fan_speed",
    "heatbreak_fan_speed", "big_fan1_speed", "big_fan2_speed", "spd_lvl", "spd_mag", "wifi_signal", "fail_reason", "mc_print_error_code",
    "mc_err", "ap_err", "err",
];

/// A field that says who or where the printer is, its account or a tag's identity: never published.
fn private_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(key.as_str(), "sn" | "ip" | "mac" | "net" | "ipcam" | "key" | "ssid")
        || ["serial", "passw", "access", "token", "secret", "uid", "url", "addr", "user", "_sn", "_id"].iter().any(|word| key.contains(word))
}

/// A value that is an address, whatever its field is called.
fn private_value(text: &str) -> bool {
    text.contains("://") || text.contains('@') || {
        let parts: Vec<&str> = text.split('.').collect();
        parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }
}

fn flatten(v: &Value, path: &mut String, out: &mut Vec<(String, String)>) {
    let mut under = |name: &str, field: &Value, path: &mut String| {
        let len = path.len();
        if len > 0 { path.push('.'); }
        path.push_str(name);
        flatten(field, path, out);
        path.truncate(len);
    };
    match v {
        Value::Obj(fields) => {
            for (key, field) in fields {
                if !private_key(key) && !(path.is_empty() && SHOWN.contains(&key.as_str())) {
                    under(key, field, path);
                }
            }
        }
        Value::Arr(items) => {
            for (i, item) in items.iter().enumerate() {
                under(&i.to_string(), item, path);
            }
        }
        Value::Str(s) if s.is_empty() || private_value(s) => {}
        Value::Str(s) => out.push((path.clone(), s.clone())),
        Value::Int(n) => out.push((path.clone(), n.to_string())),
        Value::F64(n) => out.push((path.clone(), n.to_string())),
        Value::Bool(b) => out.push((path.clone(), b.to_string())),
        Value::Null => {}
    }
}

/// A printer's answer to one of our commands (`print.command` echoed with our sequence id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub command: String,
    pub sequence: u64,
    /// What the printer said became of it: `Some(true)` it took it, `Some(false)` it refused it,
    /// None an echo that says neither, which is not a success.
    pub ok: Option<bool>,
    pub reason: Option<String>,
}

impl Ack {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn redact(&mut self, redact: impl Fn(&str) -> String) {
        if let Some(reason) = &mut self.reason { *reason = redact(reason); }
    }
}

fn says_success(text: &str) -> bool { text.eq_ignore_ascii_case("success") || text.eq_ignore_ascii_case("ok") }

/// What an error field says, if it says there is an error: zero, in any form, says none.
fn error_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Str(s) => Some(s.trim()).filter(|s| !s.is_empty() && !says_success(s) && s.parse::<f64>().ok() != Some(0.0)).map(str::to_string),
        Value::Int(n) if *n != 0 => Some(n.to_string()),
        Value::F64(n) if *n != 0.0 => Some(n.to_string()),
        _ => None,
    }
}

/// The fields whose values an answer's log line shows: whose answer it is and what it says became
/// of the command. Any other field shows only its name and kind, so what a message carries (a
/// file's name, a path, an account) stays out of the log.
const VERDICT_FIELDS: [&str; 15] = ["command", "sequence_id", "result", "reason", "fail_reason", "errno", "err_code", "error", "err", "code", "message", "msg", "status", "state", "ret"];

/// The shape of a message from the printer that is not a status report, as one line for the log,
/// so a real print shows what real answers look like. It covers every such message, whichever
/// module it comes in and whether or not [`ack`] can read it, because those are the ones to see.
pub fn answer_shape(payload: &[u8], redact: impl Fn(&str) -> String) -> Option<String> {
    let message = json::parse(payload).ok()?;
    let Value::Obj(modules) = &message else { return None };
    let answers = |module: &Value| matches!(module, Value::Obj(_)) && match module.get("command") {
        Some(command) => command.as_str() != Some("push_status"),
        None => module.get("sequence_id").is_some(),
    };
    modules.iter().any(|(_, module)| answers(module)).then(|| fields_shape(modules, &redact))
}

fn fields_shape(fields: &[(String, Value)], redact: &impl Fn(&str) -> String) -> String {
    // Decoded and without control characters first: only then is every spelling of a secret the secret.
    let clean = |text: &str| redact(&text.chars().filter(|c| !c.is_control()).collect::<String>());
    let shapes: Vec<String> = fields.iter().map(|(key, value)| {
        let (name, shown) = (clean(key), VERDICT_FIELDS.contains(&key.as_str()));
        match value {
            Value::Obj(inner) => format!("{name}{{{}}}", fields_shape(inner, redact)),
            Value::Arr(items) => format!("{name}:arr[{}]", items.len()),
            Value::Null => format!("{name}:null"),
            Value::Str(text) if shown => format!("{name}={:?}", clean(text).chars().take(120).collect::<String>()),
            Value::Str(_) => format!("{name}:str"),
            Value::Bool(flag) if shown => format!("{name}={flag}"),
            Value::Bool(_) => format!("{name}:bool"),
            Value::Int(number) if shown => format!("{name}={}", clean(&number.to_string())),
            Value::Int(_) => format!("{name}:int"),
            Value::F64(number) if shown => format!("{name}={}", clean(&number.to_string())),
            Value::F64(_) => format!("{name}:float"),
        }
    }).collect();
    shapes.join(" ")
}

/// The acknowledgement in a report payload, if it is one.
pub fn ack(payload: &[u8]) -> Option<Ack> {
    let report = json::parse(payload).ok()?;
    let (_, module) = match &report { Value::Obj(fields) => fields.iter().find(|(k, _)| matches!(k.as_str(), "print" | "system"))?, _ => return None };
    let command = module.get("command")?.as_str()?;
    if command == "push_status" { return None; }
    let sequence = u64::try_from(integer(module.get("sequence_id"))?).ok()?;
    let result = module.get("result").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty());
    let said = error_text(module.get("reason"));
    let error = ["fail_reason", "errno", "err_code"].iter().find_map(|k| error_text(module.get(k)));
    let ok = match (&error, result) {
        // An error code outweighs a result, whatever the result says.
        (Some(_), _) => Some(false),
        (None, Some(result)) => Some(says_success(result)),
        (None, None) if said.is_some() => Some(false),
        (None, None) => module.get("reason").and_then(Value::as_str).filter(|r| says_success(r.trim())).map(|_| true),
    };
    // Free text from the printer goes to logs and the UI as one clean line.
    let reason = said.or(error).map(|r| r.chars().filter(|c| !c.is_control()).collect());
    Some(Ack { command: command.to_string(), sequence, ok, reason })
}

/// What the printer is doing, from its stage id.
pub fn stage_name(stage: i64) -> Option<String> {
    Some(match stage {
        -1 | 255 => return None,
        0 => "Printing".into(),
        1 => "Auto bed levelling".into(),
        2 => "Heating the bed".into(),
        3 => "Vibration compensation".into(),
        4 => "Changing filament".into(),
        5 => "Paused (filament change)".into(),
        6 => "Paused (filament ran out)".into(),
        7 => "Heating the nozzle".into(),
        8 => "Flow calibration".into(),
        9 => "Scanning the bed".into(),
        10 => "Inspecting the first layer".into(),
        11 => "Identifying the build plate".into(),
        12 => "Calibrating the lidar".into(),
        13 => "Homing".into(),
        14 => "Cleaning the nozzle".into(),
        16 => "Paused".into(),
        17 => "Paused (front cover)".into(),
        19 => "Calibrating extrusion".into(),
        20 => "Paused (nozzle temperature)".into(),
        21 => "Paused (bed temperature)".into(),
        22 => "Unloading filament".into(),
        24 => "Loading filament".into(),
        29 => "Cooling the chamber".into(),
        39 => "Nozzle offset calibration".into(),
        n => format!("Stage {n}"),
    })
}

pub(crate) fn number(v: Option<&Value>) -> Option<f64> {
    let n = match v? { Value::Int(n) => *n as f64, Value::F64(n) => *n, Value::Str(s) => s.parse().ok()?, _ => return None };
    n.is_finite().then_some(n)
}

fn integer(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Int(n) => Some(*n),
        Value::Str(s) => s.parse().ok(),
        Value::F64(n) if n.is_finite() && n.fract() == 0.0 && *n >= i64::MIN as f64 && *n < i64::MAX as f64 => Some(*n as i64),
        _ => None,
    }
}
fn byte(v: Option<&Value>) -> Option<u8> { u8::try_from(integer(v)?).ok() }
fn uint(v: Option<&Value>) -> Option<u32> { u32::try_from(integer(v)?).ok() }
fn string(v: Option<&Value>) -> Option<String> { v?.as_str().map(str::to_string) }
fn array(v: Option<&Value>) -> &[Value] { v.and_then(Value::as_arr).unwrap_or(&[]) }
fn field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> { keys.iter().try_fold(v, |v, k| v.get(k)) }
fn hex(v: Option<&Value>) -> Option<u64> {
    match v? { Value::Str(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).ok(), v => v.as_u64() }
}

fn tray(v: &Value, id: Option<TrayId>, present: Option<bool>) -> Tray {
    Tray {
        id, present, material: string(v.get("tray_type")), colour: v.get("tray_color").and_then(Value::as_str).and_then(Colour::parse),
        info_idx: string(v.get("tray_info_idx")), remain: number(v.get("remain")).filter(|v| *v >= 0.0),
        nozzle_temp_min: number(v.get("nozzle_temp_min")), nozzle_temp_max: number(v.get("nozzle_temp_max")),
    }
}

impl PrinterState {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn redact(&mut self, redact: impl Fn(&str) -> String) {
        let clean = |text: &mut Option<String>| { if let Some(s) = text { *s = redact(s); } };
        clean(&mut self.gcode_state);
        clean(&mut self.job_name);
        for nozzle in &mut self.nozzles { clean(&mut nozzle.kind); }
        for tray in self.ams.iter_mut().flat_map(|a| &mut a.trays).chain(&mut self.external) {
            clean(&mut tray.material);
            clean(&mut tray.info_idx);
        }
        clean(&mut self.wifi);
        clean(&mut self.firmware);
        for (_, value) in self.errors.iter_mut().chain(&mut self.details) {
            *value = redact(value);
        }
    }

    fn from_print(p: &Value) -> Self {
        let get = |keys: &[&str]| field(p, keys);
        let bed = Temperature {
            current: number(p.get("bed_temper")).or_else(|| number(get(&["device", "bed", "info", "temp"]))).or_else(|| number(get(&["device", "bed_temp"]))),
            target: number(p.get("bed_target_temper")),
        };
        let bits = get(&["ams", "tray_exist_bits"]);
        let ams = array(get(&["ams", "ams"])).iter().map(|a| {
            let id = byte(a.get("id"));
            let info = hex(a.get("info"));
            let attached = info.map(|i| ((i >> 8) & 15) as u8);
            let trays = array(a.get("tray")).iter().map(|t| {
                let slot = byte(t.get("id"));
                let tray_id = id.zip(slot).map(|(ams, slot)| TrayId { ams, slot });
                let global = tray_id.and_then(|t| match (t.ams, t.slot) {
                    (0..=3, 0..=3) => Some(t.ams as usize * 4 + t.slot as usize),
                    (128..=253, 0) => Some(16 + (t.ams - 128) as usize),
                    _ => None,
                });
                tray(t, tray_id, global.and_then(|i| hex_bit(bits, i)))
            }).collect();
            AmsUnit {
                id, kind: info.map(|i| (i & 15) as u8),
                extruder: attached.filter(|i| *i != 14).map(CloudNozzleId),
                switcher_input: info.filter(|_| attached == Some(14)).map(|i| ((i >> 24) & 15) as u8),
                humidity: number(a.get("humidity")), humidity_percent: number(a.get("humidity_raw")), temperature: number(a.get("temp")), trays,
            }
        }).collect();
        Self {
            gcode_state: string(p.get("gcode_state")), progress: number(p.get("mc_percent")),
            layer: uint(p.get("layer_num")).or_else(|| uint(get(&["3D", "layer_num"]))),
            total_layers: uint(p.get("total_layer_num")).or_else(|| uint(get(&["3D", "total_layer_num"]))),
            bed, chamber: Temperature { current: number(get(&["device", "ctc", "info", "temp"])), target: None },
            extruders: array(get(&["device", "extruder", "info"])).iter().map(|v| Extruder {
                id: byte(v.get("id")).map(CloudNozzleId), temperature: packed(v.get("temp")), loaded: loaded(v.get("snow")), target: loaded(v.get("star")),
            }).collect(),
            nozzles: array(get(&["device", "nozzle", "info"])).iter().map(|v| Nozzle {
                id: byte(v.get("id")).map(CloudNozzleId), kind: string(v.get("type")), diameter: number(v.get("diameter")),
            }).collect(),
            ams,
            external: array(p.get("vir_slot")).iter().map(|v| {
                let id = byte(v.get("id")).filter(|n| matches!(*n, 254 | 255));
                let present = v.get("tray_type").and_then(Value::as_str).map(|s| !s.is_empty());
                tray(v, id.map(|id| TrayId { ams: id, slot: id }), present)
            }).collect(),
            hms: array(p.get("hms")).iter().filter_map(|v| {
                let attr = uint(v.get("attr"))?;
                let code = uint(v.get("code"))?;
                Some(HmsCode { module: (attr >> 24) as u8, instance: (attr >> 16) as u8, part: (attr >> 8) as u8, reserved: attr as u8, severity: (code >> 16) as u16, message: code as u16 })
            }).collect(),
            print_error: uint(p.get("print_error")),
            remaining_minutes: number(p.get("mc_remaining_time")),
            stage: integer(p.get("stg_cur")),
            job_name: string(p.get("subtask_name")).filter(|s| !s.is_empty()),
            prepare_percent: number(p.get("gcode_file_prepare_percent")),
            storage: match p.get("sdcard") {
                Some(Value::Bool(b)) => Some(*b),
                Some(v) => integer(Some(v)).map(|n| n != 0),
                None => None,
            }.map(|card| card || hex(p.get("aux")).is_some_and(|a| (a >> 12) & 3 != 0))
                .or_else(|| hex(p.get("aux")).map(|a| (a >> 12) & 3 != 0)),
            camera: get(&["ipcam", "rtsp_url"]).and_then(Value::as_str).is_some_and(|u| u.starts_with("rtsps://")),
            fans: Fans {
                part: uint(p.get("cooling_fan_speed")), heatbreak: uint(p.get("heatbreak_fan_speed")),
                aux: uint(p.get("big_fan1_speed")), chamber: uint(p.get("big_fan2_speed")),
            },
            speed_level: uint(p.get("spd_lvl")), speed_percent: number(p.get("spd_mag")),
            wifi: string(p.get("wifi_signal")).filter(|s| !s.is_empty()),
            // Bits 4-7 of the state: the current extruder.
            active_extruder: integer(get(&["device", "extruder", "state"])).map(|s| ((s >> 4) & 15) as u8).filter(|n| *n < 2).map(CloudNozzleId),
            firmware: string(get(&["upgrade_state", "status"])).filter(|s| !s.is_empty()),
            errors: ["mc_print_error_code", "mc_err", "ap_err", "err", "fail_reason"].iter().filter_map(|key| {
                let value = match p.get(key)? {
                    Value::Str(s) if !s.is_empty() && s != "0" => s.clone(),
                    Value::Int(n) if *n != 0 => n.to_string(),
                    _ => return None,
                };
                Some((key.to_string(), value))
            }).collect(),
            details: {
                let mut details = Vec::new();
                flatten(p, &mut String::new(), &mut details);
                details
            },
            ..Default::default()
        }
    }

    /// The machine head corresponding to a reported id.
    pub fn head_of(&self, id: CloudNozzleId) -> Option<usize> {
        if self.numbered_heads { return Some(usize::from(id.0)); }
        match id.side() { Some(NozzleSide::Left) => Some(0), Some(NozzleSide::Right) => Some(1), None => None }
    }

    pub fn head_name(&self, id: CloudNozzleId) -> Option<String> {
        if self.numbered_heads { return Some(format!("Tool {}", u16::from(id.0) + 1)); }
        id.side().map(|side| format!("{} nozzle", side.label()))
    }

    /// Reported heads in display order: the first family's left then right, or ascending ids.
    pub fn heads(&self) -> Vec<&Extruder> {
        if !self.numbered_heads {
            return [CloudNozzleId(1), CloudNozzleId(0)].into_iter()
                .filter_map(|id| self.extruders.iter().find(|e| e.id == Some(id))).collect();
        }
        let mut heads: Vec<_> = self.extruders.iter().filter(|e| e.id.is_some()).collect();
        heads.sort_by_key(|e| e.id.map(|id| id.0));
        heads
    }

    pub fn trays(&self) -> impl Iterator<Item = (Option<NozzleSide>, &Tray)> {
        self.ams.iter().flat_map(|a| a.trays.iter().map(move |t| (a.extruder.and_then(CloudNozzleId::side), t)))
            .chain(self.external.iter().map(|t| (t.id.and_then(|id| match id.ams { 254 => Some(NozzleSide::Left), 255 => Some(NozzleSide::Right), _ => None }), t)))
    }

    pub fn tray(&self, id: TrayId) -> Option<&Tray> { self.trays().find(|(_, t)| t.id == Some(id)).map(|(_, t)| t) }

    /// The printer takes a new job (idle, finished or failed).
    pub fn ready_for_job(&self) -> bool {
        matches!(self.gcode_state.as_deref(), Some("IDLE" | "FINISH" | "FAILED"))
    }

    pub fn busy(&self) -> bool {
        matches!(self.gcode_state.as_deref(), Some("PREPARE" | "RUNNING" | "PAUSE" | "SLICING" | "INIT"))
    }

    /// A fatal or serious HMS entry is active.
    pub fn serious_hms(&self) -> bool { self.hms.iter().any(|h| matches!(h.severity, 1 | 2)) }

    pub fn stage_name(&self) -> Option<String> { self.stage.and_then(stage_name) }
}

// Read the requested nibble directly: large AMS ids must not truncate a bitmask.
fn hex_bit(v: Option<&Value>, bit: usize) -> Option<bool> {
    match v? {
        Value::Str(s) => {
            let s = s.strip_prefix("0x").unwrap_or(s);
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) { return None; }
            let digit = s.as_bytes().iter().rev().nth(bit / 4).copied().unwrap_or(b'0');
            Some((digit as char).to_digit(16)? & (1 << (bit % 4)) != 0)
        }
        v => v.as_u64().map(|n| bit < 64 && n & (1u64 << bit) != 0),
    }
}

pub fn merge(target: &mut Value, delta: Value) {
    match (target, delta) {
        (Value::Obj(base), Value::Obj(update)) => {
            for (key, value) in update {
                if let Some((_, old)) = base.iter_mut().find(|(k, _)| *k == key) { merge(old, value); }
                else { base.push((key, value)); }
            }
        }
        (target, value) => *target = value,
    }
}

/// Worker-owned merge state. Intentionally has no Debug or raw-report accessor.
#[derive(Default)]
pub struct Reports { print: Option<Value> }

impl Reports {
    pub fn apply(&mut self, payload: &[u8]) -> Result<Option<PrinterState>, &'static str> {
        let report = json::parse(payload).map_err(|_| "invalid printer report")?;
        let Some(p) = report.get("print") else { return Ok(None) };
        if !matches!(p, Value::Obj(_)) { return Err("invalid print report"); }
        if p.get("command").and_then(Value::as_str).is_some_and(|c| c != "push_status") { return Ok(None); }
        match integer(p.get("msg")) {
            Some(1) => merge(self.print.as_mut().ok_or("delta received before a full report")?, p.clone()),
            None | Some(0) => self.print = Some(p.clone()),
            _ => return Err("unknown printer report mode"),
        }
        Ok(self.print.as_ref().map(PrinterState::from_print))
    }

    /// Everything so far as one full report: what a printer says when asked for everything.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn full_report(&self) -> Option<String> {
        let mut print = self.print.clone()?;
        merge(&mut print, json::obj(vec![("command", json::s("push_status")), ("msg", Value::Int(0))]));
        Some(json::obj(vec![("print", print)]).to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_head_numberings_keep_machine_and_display_order() {
        let mut state = PrinterState {
            extruders: [0, 1].map(|h| Extruder { id: Some(CloudNozzleId(h)), ..Default::default() }).to_vec(),
            ..Default::default()
        };
        assert!(!state.numbered_heads);
        assert_eq!(state.head_of(CloudNozzleId(0)), Some(1));
        assert_eq!(state.head_of(CloudNozzleId(1)), Some(0));
        assert_eq!(state.head_name(CloudNozzleId(0)).as_deref(), Some("Right nozzle"));
        assert_eq!(state.head_name(CloudNozzleId(1)).as_deref(), Some("Left nozzle"));
        assert_eq!(state.head_of(CloudNozzleId(2)), None);
        assert_eq!(state.head_name(CloudNozzleId(2)), None);
        assert_eq!(state.heads().iter().map(|e| e.id).collect::<Vec<_>>(), [Some(CloudNozzleId(1)), Some(CloudNozzleId(0))]);
        state.numbered_heads = true;
        state.extruders = [3, 1, 0, 2].map(|h| Extruder { id: Some(CloudNozzleId(h)), ..Default::default() }).to_vec();
        for h in 0..4 {
            assert_eq!(state.head_of(CloudNozzleId(h)), Some(usize::from(h)));
            assert_eq!(state.head_name(CloudNozzleId(h)), Some(format!("Tool {}", h + 1)));
        }
        assert_eq!(state.heads().iter().map(|e| e.id).collect::<Vec<_>>(), (0..4).map(|h| Some(CloudNozzleId(h))).collect::<Vec<_>>());
        assert_eq!(state.head_of(CloudNozzleId(255)), Some(255));
        assert_eq!(state.head_name(CloudNozzleId(255)).as_deref(), Some("Tool 256"));
    }
}
