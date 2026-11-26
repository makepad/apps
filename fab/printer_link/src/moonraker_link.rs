//! Moonraker link worker; the simulator uses the same steps and transfer thread.
use crate::job::{heads_fit, JobStage, JobStatus, PrintJob, PrintOptions, Tracker};
use crate::link::{drive, publish, spawn_transfer, Command, CommandState, ConnectionStatus, Driven, SessionLease, Shared, Snapshot, Storage, Transfer};
use crate::model::{AmsUnit, CloudNozzleId, Colour, Extruder, LoadedSlot, PrinterState, Temperature, Tray, TrayId};
use crate::moonraker::{self, ByteStream, Client, Error, Heater, StartParams, Status, StatusSocket};
use crate::moonraker_sim::SimMoonraker;
use crate::secrets::{Access, PrinterConfig, Secret};
use crate::safety::ValidatedJob;
use makepad_strict_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

type Stream = Box<dyn ByteStream + Send>;
const LOST: ConnectionStatus = ConnectionStatus::Offline("lost the printer");
const NOT_CONNECTED: &str = "the printer is not connected";

fn lost(error: &Error) -> Option<ConnectionStatus> {
    match error {
        Error::Unauthorized => Some(ConnectionStatus::WrongApiKey),
        Error::Io | Error::Protocol(_) => Some(LOST),
        _ => None,
    }
}
fn explanation(error: &Error) -> String {
    match error {
        Error::Refused(message) => moonraker::refusal(message).unwrap_or(message).to_string(),
        _ => error.to_string(),
    }
}
fn config_number(key: &str, value: &str) -> Result<f64, String> {
    value.trim().parse::<f64>().ok().filter(|n| n.is_finite())
        .ok_or_else(|| format!("the file's {key} is not a finite number"))
}
fn config_numbers(job: &ValidatedJob, key: &str) -> Result<Vec<f64>, String> {
    job.config_value(key).map(|value| value.split(',').map(|v| config_number(key, v)).collect()).unwrap_or_else(|| Ok(Vec::new()))
}
fn config_strings(job: &ValidatedJob, key: &str) -> Result<Vec<String>, String> {
    job.config_value(key).map(|value| value.split(',').map(|v| {
        let v = v.trim();
        moonraker::safe_list_value(v).map_err(|_| format!("the file's {key} has an invalid string"))?;
        Ok(v.to_string())
    }).collect()).unwrap_or_else(|| Ok(Vec::new()))
}
fn per_filament<T: Clone>(mut values: Vec<T>, count: usize, key: &str) -> Result<Vec<T>, String> {
    if values.len() == 1 { values.resize(count, values[0].clone()); }
    if !values.is_empty() && values.len() != count {
        return Err(format!("the file's {key} does not match its filaments"));
    }
    Ok(values)
}

// Heads correspond to the used filaments, in the same order as `used`.
fn start_params(job: &ValidatedJob, used: &[usize], heads: &[u32], options: &PrintOptions) -> Result<StartParams, String> {
    let facts = job.facts();
    let count = facts.filaments.len();
    if used.len() != heads.len() || used.iter().any(|&i| i >= count) {
        return Err("the file's used filaments do not match their heads".into());
    }
    let numeric_filaments = |key| per_filament(config_numbers(job, key)?, count, key);
    let outer_wall_speed_list = config_numbers(job, "outer_wall_speed")?;
    let outer_wall_acceleration_list = config_numbers(job, "outer_wall_acceleration")?;
    let line_width = job.config_value("line_width").map(|value| -> Result<f64, String> {
        let width = if let Some(percent) = value.trim().strip_suffix('%') {
            let nozzle = job.config_value("nozzle_diameter").and_then(|v| v.split(',').next())
                .ok_or_else(|| "the file's line_width needs nozzle_diameter".to_string())?;
            config_number("line_width", percent)? * config_number("nozzle_diameter", nozzle)? / 100.0
        } else { config_number("line_width", value)? };
        if !width.is_finite() { return Err("the file's line_width is not a finite number".into()); }
        Ok(width)
    }).transpose()?;
    let filament_volume_type = per_filament(config_strings(job, "filament_volume_type")?, count, "filament_volume_type")?;
    if filament_volume_type.iter().any(|v| !matches!(v.as_str(), "standard" | "high_flow")) {
        return Err("the file's filament_volume_type must be standard or high_flow".into());
    }
    let print_sequence = job.config_value("print_sequence").map(|value| -> Result<String, String> {
        moonraker::safe_list_value(value).map_err(|_| "the file's print_sequence has an invalid string".to_string())?;
        Ok(value.trim().to_string())
    }).transpose()?;
    let filament_types: Vec<_> = facts.filaments.iter().map(|f| f.kind.clone()).collect();
    for value in &filament_types {
        moonraker::safe_list_value(value).map_err(|_| "the file's filament_type has an invalid string".to_string())?;
    }
    let mut flow_calibrate_extruders = if options.flow_calibration { heads.to_vec() } else { Vec::new() };
    flow_calibrate_extruders.sort_unstable();
    flow_calibrate_extruders.dedup();
    Ok(StartParams {
        bed_level: Some(true), flow_calibrate: Some(options.flow_calibration), time_lapse_camera: Some(options.timelapse),
        map_table: used.iter().zip(heads).map(|(&i, &head)| (i as u32, head)).collect(),
        flow_calibrate_extruders, line_width,
        layer_height: job.config_value("layer_height").map(|v| config_number("layer_height", v)).transpose()?,
        outer_wall_speed: outer_wall_speed_list.first().copied(),
        nozzle_diameters: facts.nozzle_diameters.clone(), nozzle_temp: numeric_filaments("nozzle_temperature")?,
        filament_types, filament_flow_ratio: numeric_filaments("filament_flow_ratio")?,
        filament_diameter: numeric_filaments("filament_diameter")?,
        filament_max_volumetric_speed: numeric_filaments("filament_max_volumetric_speed")?,
        filament_used_g: facts.used_g.clone(), filament_used_mm: facts.used_mm.clone(),
        outer_wall_acceleration: outer_wall_acceleration_list.first().copied(),
        filament_volume_type, print_sequence, outer_wall_speed_list, outer_wall_acceleration_list,
        process_flow_support: config_strings(job, "process_flow_support")?,
        slicer: Some("Fab".into()), slicer_version: Some(env!("CARGO_PKG_VERSION").into()),
    })
}

fn temperature(heater: &Heater) -> Temperature {
    Temperature { current: Some(heater.temperature), target: Some(heater.target) }
}

/// Assumed print_task_config shape: lists indexed by physical head (0–3).
/// This still needs checking on a printer. Missing or malformed entries stay unknown.
fn head(config: &Value, h: u8) -> Tray {
    let entry = |key| config.get(key).and_then(Value::as_arr).and_then(|a| a.get(h as usize));
    Tray {
        id: Some(TrayId { ams: h, slot: 0 }),
        present: entry("filament_exist").and_then(Value::as_bool),
        material: entry("filament_type").and_then(Value::as_str).map(str::to_string),
        colour: entry("filament_color_rgba").and_then(Value::as_str).and_then(Colour::parse),
        ..Default::default()
    }
}
fn state(status: &Status, firmware: &str) -> PrinterState {
    let mut state = PrinterState {
        numbered_heads: true,
        gcode_state: Some(if firmware != "ready" { "OFFLINE" } else { match status.state.as_str() {
            "standby" => "IDLE", "cancelled" => "FAILED", "printing" => "RUNNING", "paused" => "PAUSE",
            "complete" => "FINISH", "error" => "FAILED", _ => "OFFLINE",
        }}.into()),
        job_name: Some(status.filename.clone()), progress: Some(status.progress * 100.0),
        layer: u32::try_from(status.current_layer).ok(), total_layers: u32::try_from(status.total_layer).ok(),
        bed: temperature(&status.bed), storage: Some(true),
        active_extruder: status.active_extruder.strip_prefix("extruder").and_then(|n| if n.is_empty() { Some(0) } else { n.parse::<u8>().ok() }).filter(|h| (*h as usize) < status.extruders.len()).map(CloudNozzleId),
        details: vec![("Firmware".into(), firmware.into())],
        ..Default::default()
    };
    if firmware != "ready" && !status.firmware_message.is_empty() { state.errors.push(("Firmware".into(), status.firmware_message.clone())); }
    if status.state == "error" { state.errors.push(("Print".into(), status.message.clone())); }
    for (h, heater) in status.extruders.iter().enumerate() {
        let Ok(h) = u8::try_from(h) else { continue };
        let tray = head(&status.print_task_config, h);
        state.extruders.push(Extruder {
            id: Some(CloudNozzleId(h)), temperature: temperature(heater),
            loaded: (tray.present == Some(true)).then_some(LoadedSlot::Tray(TrayId { ams: h, slot: 0 })),
            ..Default::default()
        });
        state.ams.push(AmsUnit { id: Some(h), extruder: Some(CloudNozzleId(h)), trays: vec![tray], ..Default::default() });
    }
    state
}

#[derive(Clone)]
enum Target { Configured(Arc<PrinterConfig>), Simulated(SimMoonraker) }
impl Target {
    fn credentials(&self) -> Result<(&str, Option<Secret>), Error> {
        crate::entries::CREDENTIAL.enter();
        match self {
            Self::Simulated(_) => Ok(("memory", None)),
            Self::Configured(config) => match &config.access {
                Access::Moonraker { api_key, .. } => Ok((&config.host, api_key.as_ref().map(|k| Secret::new(k.bytes().to_vec())))),
                _ => Err(Error::Protocol("printer configuration changed protocol")),
            },
        }
    }
    fn stream(&self, nonblocking: bool, stop: &AtomicBool) -> Result<Stream, Error> {
        if stop.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        match self {
            Self::Simulated(sim) => Ok(Box::new(sim.stream())),
            Self::Configured(config) => {
                let Access::Moonraker { port, .. } = &config.access else { return Err(Error::Protocol("printer configuration changed protocol")); };
                // Unit tests cannot enter a socket or name-resolution path.
                #[cfg(test)]
                { let _ = (port, nonblocking); Err(Error::Protocol("network unavailable in tests")) }
                #[cfg(not(test))]
                {
                    let stream = std::net::TcpStream::connect((config.host.as_str(), *port))?;
                    if stop.load(Ordering::Acquire) { return Err(Error::Cancelled); }
                    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                    stream.set_write_timeout(Some(Duration::from_millis(200)))?;
                    stream.set_nonblocking(nonblocking)?;
                    Ok(Box::new(stream))
                }
            }
        }
    }
    fn client(&self, stop: &Arc<AtomicBool>) -> Result<Client<Stream>, Error> {
        crate::entries::MOONRAKER_HTTP.enter();
        let (host, key) = self.credentials()?;
        let mut client = Client::new(self.stream(false, stop)?, host, key)?;
        client.set_stop(stop.clone());
        Ok(client)
    }
    fn socket(&self, stop: &Arc<AtomicBool>, heads: usize) -> Result<StatusSocket<Stream>, Error> {
        crate::entries::MOONRAKER_WEBSOCKET.enter();
        let (host, key) = self.credentials()?;
        StatusSocket::connect_stoppable(self.stream(true, stop)?, host, key, heads, Some(stop))
    }
}

#[derive(Clone)]
struct HttpStorage {
    target: Target,
    stop: Arc<AtomicBool>,
    #[cfg(test)]
    hooks: Option<Arc<UploadHooks>>,
}
impl HttpStorage {
    fn new(target: Target, stop: Arc<AtomicBool>) -> Self {
        Self { target, stop, #[cfg(test)] hooks: None }
    }
}
#[cfg(test)]
struct UploadHooks {
    ready: std::sync::mpsc::Sender<()>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    done: std::sync::mpsc::Sender<()>,
}
impl Storage for HttpStorage {
    fn upload(&self, file: String, job: crate::safety::ValidatedJob) -> Option<Arc<Transfer>> {
        let storage = self.clone();
        spawn_transfer("", move |_, transfer| {
            let result = storage.target.client(&storage.stop).and_then(|mut client| {
                client.upload_with_receipt(&file, job.bytes(), &transfer.cancel, |sent, _| {
                    transfer.sent.store(sent as u64, Ordering::Relaxed);
                    #[cfg(test)]
                    if sent == 0 {
                        if let Some(hooks) = &storage.hooks {
                            hooks.ready.send(()).unwrap();
                            hooks.resume.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                        }
                    }
                })
            });
            *transfer.stored.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            #[cfg(test)]
            if let Some(hooks) = &storage.hooks { hooks.done.send(()).unwrap(); }
        })
    }
    fn list(&self) -> Option<Arc<Transfer>> {
        let storage = self.clone();
        spawn_transfer("", move |_, transfer| {
            let result = storage.target.client(&storage.stop).and_then(|mut client| client.list()).map_err(|e| explanation(&e));
            *transfer.listing.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        })
    }
}

// Each exchange owns a connection. A silent reply occupies only its request thread.
struct Task<T> {
    result: std::sync::mpsc::Receiver<Result<T, Error>>,
    cached: Option<Result<T, Error>>,
}
impl<T: Send + 'static> Task<T> {
    fn spawn(work: impl FnOnce() -> Result<T, Error> + Send + 'static) -> Self {
        let (sender, result) = std::sync::mpsc::sync_channel(1);
        let spawned = spawn_transfer("", move |_, _| { let _ = sender.send(work()); });
        Self { result, cached: spawned.is_none().then_some(Err(Error::Protocol("cannot start the request"))) }
    }
    fn take(&mut self) -> Option<Result<T, Error>> {
        self.cached.take().or_else(|| match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(Error::Io)),
        })
    }
    #[cfg(test)]
    fn wait(&mut self) {
        if self.cached.is_none() { self.cached = Some(self.result.recv_timeout(Duration::from_secs(5)).expect("request thread completed")); }
    }
}
struct Live {
    stop: Arc<AtomicBool>,
    socket: Option<StatusSocket<Stream>>,
    opening: Option<Task<StatusSocket<Stream>>>,
    info: Option<Task<moonraker::ServerInfo>>,
    firmware: String,
    probing: bool,
    next_info: Instant,
}
impl Live {
    /// The status socket opens for `heads` heads, the printer profile's.
    fn connect(storage: &HttpStorage, heads: usize) -> Result<Self, ConnectionStatus> {
        let target = storage.target.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let opening_stop = stop.clone();
        let opening = Task::spawn(move || target.socket(&opening_stop, heads));
        let mut live = Self { stop, socket: None, opening: Some(opening), info: None, firmware: String::new(), probing: true, next_info: Instant::now() };
        live.ask_info(storage);
        Ok(live)
    }
    fn ask_info(&mut self, storage: &HttpStorage) {
        let target = storage.target.clone(); let stop = self.stop.clone();
        self.info = Some(Task::spawn(move || target.client(&stop)?.server_info()));
    }
    fn fresh(&self) -> bool {
        self.firmware == "ready" && self.socket.as_ref().is_some_and(|s| s.fresh() && s.status().firmware == "ready")
    }
    /// Watch `heads` heads from now on (a socket still opening is told once it opens).
    fn watch_heads(&mut self, heads: usize) -> Result<(), ConnectionStatus> {
        match &mut self.socket {
            Some(socket) => socket.set_heads(heads).map_err(|e| lost(&e).unwrap_or(LOST)),
            None => Ok(()),
        }
    }
    fn poll(&mut self, storage: &HttpStorage, now: Instant) -> Result<bool, ConnectionStatus> {
        let mut changed = false;
        if let Some(result) = self.opening.as_mut().and_then(Task::take) {
            self.opening = None;
            self.socket = Some(result.map_err(|e| lost(&e).unwrap_or(LOST))?);
        }
        if let Some(result) = self.info.as_mut().and_then(Task::take) {
            self.info = None;
            self.next_info = now + Duration::from_secs(1);
            self.firmware = result.map_err(|e| lost(&e).unwrap_or(LOST))?.klippy_state;
            if self.firmware == "ready" {
                self.probing = false;
                if let Some(socket) = &mut self.socket {
                    if !socket.status().firmware.is_empty() && socket.status().firmware != "ready" { socket.resubscribe().map_err(|e| lost(&e).unwrap_or(LOST))?; }
                }
            }
            changed = true;
        }
        if let Some(socket) = &mut self.socket {
            let reported = socket.poll().map_err(|e| lost(&e).unwrap_or(LOST))?.is_some();
            changed |= reported;
            if socket.is_closed() { return Err(LOST); }
            let firmware = &socket.status().firmware;
            if reported && !firmware.is_empty() {
                self.firmware = firmware.clone();
                if firmware == "disconnected" { self.probing = true; }
                else if firmware == "ready" && socket.fresh() { self.probing = false; }
            }
        }
        if self.probing && self.info.is_none() && now >= self.next_info { self.ask_info(storage); }
        Ok(changed)
    }
}
impl Drop for Live {
    fn drop(&mut self) { self.stop.store(true, Ordering::Release); }
}
enum Operation { Control(&'static str), Start(String), Delete(String) }
impl Operation {
    fn name(&self) -> &'static str {
        match self { Self::Control(name) => name, Self::Start(_) => "print", Self::Delete(_) => "delete" }
    }
}
struct Request { id: u64, operation: Operation, task: Task<Vec<crate::ftps::Entry>> }
struct Worker {
    snapshot: Snapshot,
    storage: Option<HttpStorage>,
    tracker: Option<Tracker>,
    printing: Option<PrintJob>,
    upload: Option<Arc<Transfer>>,
    pending_start: Option<String>,
    requests: Vec<Request>,
    request_id: u64,
    shown_request: u64,
    cleanup: Vec<String>,
    next_cleanup: Option<Instant>,
    plate_check: bool,
    backoff: u64,
    changed: bool,
}
impl Worker {
    fn new(snapshot: Snapshot) -> Self {
        Self { snapshot, storage: None, tracker: None, printing: None, upload: None, pending_start: None,
            requests: Vec::new(), request_id: 0, shown_request: 0, cleanup: Vec::new(), next_cleanup: None, plate_check: false, backoff: 1, changed: false }
    }
    fn show_job(&mut self) {
        if let Some(t) = &self.tracker {
            self.snapshot.job = Some(t.status.clone());
            if !t.status.stage.active() && self.upload.is_none() && !self.requests.iter().any(|r| matches!(r.operation, Operation::Start(_))) {
                self.tracker = None;
                self.printing = None;
                self.pending_start = None;
            }
        }
        let state = Arc::make_mut(&mut self.snapshot.state);
        state.details.retain(|(key, _)| key != "Waiting");
        for request in &self.requests { state.details.push(("Waiting".into(), format!("{}: waiting for the printer", request.operation.name()))); }
        if self.upload.is_some() { state.details.push(("Waiting".into(), "upload: waiting for the printer".into())); }
    }
    fn preflight(&self, print: &PrintJob) -> Option<String> {
        if self.plate_check && !print.options.plate_clear { return Some("Confirm the plate is clear first".into()); }
        heads_fit(&self.snapshot.state, print.job.facts(), print)
    }
    fn print(&mut self, print: PrintJob) {
        self.snapshot.job_sends += 1;
        if matches!(self.snapshot.command, Some(("print", _))) { self.snapshot.command = None; }
        let pending = if self.tracker.as_ref().is_some_and(|t| t.status.stage.active()) || self.requests.iter().any(|r| matches!(r.operation, Operation::Start(_))) { Some("a job is already on its way") }
            else if self.upload.is_some() || !self.cleanup.is_empty() { Some("the cancelled upload is still closing: press it again in a moment") } else { None };
        if let Some(why) = pending { self.snapshot.command = Some(("print", CommandState::Failed(why.into()))); return; }
        let why = if self.snapshot.status != ConnectionStatus::Connected { Some(NOT_CONNECTED.into()) }
            else { self.preflight(&print) };
        if let Some(why) = why {
            self.snapshot.job = Some(JobStatus { name: print.job.name().into(), file: String::new(), stage: JobStage::Failed(why) });
            return;
        }
        if print.options.plate_clear { self.plate_check = false; }
        self.snapshot.command = None;
        let file = crate::job::plain_file_name(print.job.name());
        let mut tracker = Tracker::new(&print, file.clone());
        self.upload = self.storage.as_ref().and_then(|storage| storage.upload(file, print.job.clone()));
        if self.upload.is_none() { tracker.fail("cannot start the upload".into()); }
        self.printing = Some(print);
        self.tracker = Some(tracker);
    }
    fn request(&mut self, operation: Operation) {
        self.request_id += 1;
        if matches!(operation, Operation::Control(_)) { self.shown_request = self.request_id; }
        let storage = self.storage.as_ref().unwrap().clone();
        let name = operation.name();
        let argument = match &operation { Operation::Start(line) | Operation::Delete(line) => Some(line.clone()), _ => None };
        let task = Task::spawn(move || {
            let mut client = storage.target.client(&storage.stop)?;
            match name {
                "pause" => client.pause()?, "resume" => client.resume()?, "stop" => client.cancel()?,
                "check_storage" => return client.list(),
                "print" => client.script(argument.as_deref().unwrap())?,
                "delete" => client.delete(argument.as_deref().unwrap())?,
                _ => return Err(Error::Protocol("unknown request")),
            }
            Ok(Vec::new())
        });
        self.requests.push(Request { id: self.request_id, operation, task });
        self.changed = true;
    }
    fn command(&mut self, command: Command, connected: bool) {
        if let Command::Print(print) = command { self.print(print); return; }
        let name = match command {
            Command::Pause => "pause", Command::Resume => "resume", Command::Stop => "stop",
            Command::ClearError => "clean_print_error", Command::CheckStorage => "check_storage", Command::Print(_) => unreachable!(),
        };
        if matches!(command, Command::Stop) {
            if let Some(t) = &mut self.tracker {
                if self.upload.is_some() || self.pending_start.is_some() {
                    if let Some(upload) = &self.upload { upload.cancel.store(true, Ordering::Release); }
                    if let Some(file) = self.pending_start.take() { self.cleanup.push(file); }
                    t.cancel();
                } else { t.stop_requested(); }
            }
            // Stop always gets its own connection, including while another exchange waits.
            if self.storage.is_some() { self.request(Operation::Control(name)); self.snapshot.command = Some((name, CommandState::Pending)); return; }
        }
        if matches!(command, Command::ClearError) {
            self.snapshot.command = Some((name, CommandState::Failed("clear the error on the printer's screen".into()))); return;
        }
        if !connected {
            if matches!(command, Command::CheckStorage) { self.snapshot.storage_files = Some(Err(NOT_CONNECTED.into())); self.changed = true; }
            self.snapshot.command = Some((name, CommandState::Failed(NOT_CONNECTED.into()))); return;
        }
        self.request(Operation::Control(name));
        self.snapshot.command = Some((name, CommandState::Pending));
    }
    fn requests(&mut self, now: Instant) -> Result<(), ConnectionStatus> {
        let mut outcome = Ok(());
        let mut index = 0;
        while index < self.requests.len() {
            let Some(result) = self.requests[index].task.take() else { index += 1; continue; };
            let request = self.requests.remove(index);
            self.changed = true;
            match request.operation {
                Operation::Start(_) => {
                    if let Some(t) = &mut self.tracker {
                        if t.status.stage.active() {
                            match &result {
                                Ok(_) => { t.sent(0, now); t.status.stage = JobStage::Accepted; }
                                Err(Error::Refused(_)) => t.fail(explanation(result.as_ref().unwrap_err())),
                                Err(_) => { t.status.stage = JobStage::StartReplyLost; outcome = Err(LOST); }
                            }
                        }
                    }
                }
                Operation::Delete(file) => {
                    if result.is_ok() { self.cleanup.retain(|f| f != &file); }
                    else { self.next_cleanup = Some(now + Duration::from_secs(1)); }
                }
                Operation::Control(name) => {
                    if name == "check_storage" { self.snapshot.storage_files = Some(result.as_ref().cloned().map_err(explanation)); }
                    if request.id == self.shown_request && self.snapshot.command.as_ref().is_some_and(|(shown, _)| *shown == name) {
                        self.snapshot.command = Some((name, match &result { Ok(_) => CommandState::Done, Err(e) => CommandState::Failed(explanation(e)) }));
                    }
                    if name == "stop" && result.is_err() { if let Some(t) = &mut self.tracker { t.stop_refused(); } }
                }
            }
        }
        if self.next_cleanup.is_none_or(|next| now >= next) {
            let files: Vec<_> = self.cleanup.iter().filter(|file| !self.requests.iter().any(|r| matches!(&r.operation, Operation::Delete(p) if p == *file))).cloned().collect();
            for file in files { self.request(Operation::Delete(file)); }
        }
        outcome
    }
    fn transfers(&mut self) {
        let Some(upload) = self.upload.clone() else { return; };
        let Some(t) = &mut self.tracker else { return; };
        t.uploading(upload.sent.load(Ordering::Relaxed));
        let Some(result) = upload.stored.lock().unwrap_or_else(|e| e.into_inner()).take() else { return; };
        self.upload = None;
        if !matches!(t.status.stage, JobStage::Uploading { .. }) {
            self.cleanup.push(result.unwrap_or_else(|_| t.status.file.clone()));
            return;
        }
        match result {
            Ok(file) => { t.status.file = file.clone(); t.uploaded(Ok(())); self.pending_start = Some(file); }
            Err(e) => t.fail(explanation(&e)),
        }
    }
    fn start(&mut self, live: &Live) {
        if !live.fresh() { return; }
        let Some(file) = self.pending_start.take() else { return; };
        let Some(print) = &self.printing else { return; };
        if let Some(why) = self.preflight(print) { self.tracker.as_mut().unwrap().fail(format!("uploaded, not started: {why}")); return; }
        let heads: Vec<_> = print.used.iter().map(|&i| u32::from(print.trays[i].unwrap().ams)).collect();
        let line = start_params(&print.job, &print.used, &heads, &print.options)
            .and_then(|params| moonraker::start_line(&file, &params).map_err(|e| explanation(&e)));
        match line {
            Ok(line) => { self.plate_check = true; self.request(Operation::Start(line)); }
            Err(why) => self.tracker.as_mut().unwrap().fail(format!("uploaded, not started: {why}")),
        }
    }

    fn track(&mut self, report: Option<&Status>, now: Instant) {
        let Some(t) = &mut self.tracker else { return; };
        if t.status.stage == JobStage::StartReplyLost {
            let Some(report) = report else { return; };
            if report.filename == t.status.file && (matches!(report.state.as_str(), "printing" | "paused" | "complete" | "cancelled" | "error") || (report.is_active && report.file_path.ends_with(&t.status.file))) {
                t.sent(0, now); t.status.stage = JobStage::Accepted;
                if report.state == "standby" { return; }
                // Establish that this job ran, including a terminal first report.
                let mut running = (*self.snapshot.state).clone(); running.gcode_state = Some("RUNNING".into());
                let _ = t.advance(&running, now);
            } else if report.state == "standby" {
                t.fail(if report.filename == t.status.file { "the printer loaded the file but did not start it" } else { "the start did not reach the printer" }.into());
            } else { return; }
        }
        if let Some(report) = report {
            if report.filename == t.status.file && report.state == "cancelled" { t.cancel(); }
        }
        // A request without a reply has no wall-clock failure. Accepted jobs use Tracker's clock.
        if !self.requests.iter().any(|r| matches!(r.operation, Operation::Start(_))) {
            let _ = t.advance(&self.snapshot.state, now);
        }
    }
    fn step(&mut self, live: &mut Live, commands: Vec<Command>, now: Instant) -> Result<(), ConnectionStatus> {
        let mut outcome = self.requests(now);
        let polled = live.poll(self.storage.as_ref().unwrap(), now);
        match &polled {
            Ok(changed) => {
                if *changed {
                    if let Some(socket) = &live.socket {
                        self.snapshot.state = Arc::new(state(socket.status(), &live.firmware));
                        if matches!(socket.status().state.as_str(), "complete" | "cancelled" | "error") { self.plate_check = true; }
                    }
                    self.changed = true;
                }
                self.snapshot.status = if live.fresh() { self.backoff = 1; ConnectionStatus::Connected } else { ConnectionStatus::Offline("waiting for ready firmware and fresh status") };
            }
            Err(status) => { self.snapshot.status = status.clone(); outcome = Err(status.clone()); }
        }
        if let Err(status) = &outcome { self.snapshot.status = status.clone(); }
        self.track(if outcome.is_ok() && live.fresh() { live.socket.as_ref().map(StatusSocket::status) } else { None }, now);
        for command in commands { self.command(command, outcome.is_ok() && live.fresh()); }
        if outcome.is_ok() { self.start(live); }
        self.transfers();
        self.show_job();
        if live.info.is_some() { Arc::make_mut(&mut self.snapshot.state).details.push(("Waiting".into(), "server info: waiting for the printer".into())); }
        if live.opening.is_some() { Arc::make_mut(&mut self.snapshot.state).details.push(("Waiting".into(), "status connection: waiting for the printer".into())); }
        outcome
    }
}
impl Driven for Worker {
    fn snapshot(&mut self) -> &mut Snapshot { &mut self.snapshot }
    fn backoff(&mut self) -> &mut u64 { &mut self.backoff }
    fn take_changed(&mut self) -> bool { std::mem::take(&mut self.changed) }
    fn poll_interval(&self) -> Option<Duration> { Some(Duration::from_millis(20)) }
    fn offline(&mut self, commands: Vec<Command>) {
        let now = Instant::now();
        let _ = self.requests(now);
        self.track(None, now);
        for command in commands { self.command(command, false); }
        self.transfers();
        self.show_job();
    }
    fn cancel_transfer(&self) { if let Some(upload) = &self.upload { upload.cancel.store(true, Ordering::Release); } }
}
fn connected(shared: &Shared, worker: &mut Worker, mut live: Live) -> Result<(), ConnectionStatus> {
    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if let Some(HttpStorage { target: Target::Simulated(sim), .. }) = &worker.storage { sim.follow_reports(); }
        let before = (worker.snapshot.job.clone(), worker.snapshot.command.clone());
        live.watch_heads(shared.heads_of(&worker.snapshot.model))?;
        let outcome = worker.step(&mut live, shared.queued(), now);
        if worker.take_changed() || before != (worker.snapshot.job.clone(), worker.snapshot.command.clone()) { publish(shared, &worker.snapshot); }
        outcome?;
        // The memory stream returns immediately; a wake also interrupts this pacing wait.
        std::thread::park_timeout(Duration::from_millis(20));
    }
    Ok(())
}
fn lease_endpoint(host: &str, port: u16) -> Result<SessionLease, ConnectionStatus> {
    SessionLease::for_printer(format!("moonraker:{}:{port}", host.to_ascii_lowercase()))
}

pub(crate) fn run(shared: Arc<Shared>, name: String) {
    let snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
    let mut worker = Worker::new(snapshot);
    drive(&shared, &mut worker, |worker| {
        let config = PrinterConfig::load(&name).map_err(ConnectionStatus::Offline)?;
        let Access::Moonraker { port, api_key } = &config.access else { return Err(ConnectionStatus::Offline("printer configuration changed protocol")); };
        worker.snapshot.name = api_key.as_ref().map_or_else(|| name.clone(), |key| key.redact(&name));
        worker.snapshot.model = config.model.clone();
        let _endpoint = lease_endpoint(&config.host, *port)?;
        worker.storage = Some(HttpStorage::new(Target::Configured(Arc::new(config)), shared.stop.clone()));
        publish(&shared, &worker.snapshot);
        let live = Live::connect(worker.storage.as_ref().unwrap(), shared.heads_of(&worker.snapshot.model))?;
        connected(&shared, worker, live)
    });
}
pub(crate) fn run_sim(shared: Arc<Shared>, path: std::path::PathBuf) {
    let snapshot = (**shared.slot.lock().unwrap_or_else(|e| e.into_inner())).clone();
    let mut worker = Worker::new(snapshot);
    worker.storage = Some(HttpStorage::new(Target::Simulated(SimMoonraker::from_reports(path)), shared.stop.clone()));
    drive(&shared, &mut worker, |worker| {
        let live = Live::connect(worker.storage.as_ref().unwrap(), shared.heads_of(&worker.snapshot.model))?;
        connected(&shared, worker, live)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::tests::heads_job;
    use crate::moonraker_sim::SIM_HEADS;
    use makepad_strict_json::{obj, s};
    use std::sync::mpsc::{channel, Receiver, Sender};

    #[test]
    fn moonraker_leases_the_host_and_effective_port() {
        let first = lease_endpoint("Printer.invalid", 7125).unwrap();
        let duplicate = || lease_endpoint("printer.invalid", 7125);
        let refused = ConnectionStatus::Offline("printer already linked in this process");
        assert_eq!(duplicate().err(), Some(refused.clone()));
        let _other_port = lease_endpoint("printer.invalid", 8125).unwrap();
        assert_eq!(duplicate().err(), Some(refused));
        drop(first);
        assert!(duplicate().is_ok());
    }

    struct FileSim {
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
        link: crate::PrinterLink,
        last: Arc<Snapshot>,
    }
    impl FileSim {
        fn new(tag: &str, report: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("fab-second-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("reports.jsonl");
            std::fs::write(&path, report).unwrap();
            let model = makepad_fab_settings::family::FAMILIES.iter().find(|f| f.flavor == makepad_fab_settings::family::Flavor::Named).expect("second family").models[0];
            let link = crate::PrinterLink::simulate_moonraker(path.clone(), model);
            // The app says how many heads the printer's profile has.
            link.set_heads(model, SIM_HEADS);
            let mut sim = Self { dir, path, link, last: Arc::default() };
            sim.wait_for(|s| s.status == ConnectionStatus::Connected);
            sim
        }
        fn append(&self, line: &str) {
            use std::io::Write;
            writeln!(std::fs::OpenOptions::new().append(true).open(&self.path).unwrap(), "{line}").unwrap();
        }
        fn wait_for(&mut self, done: impl Fn(&Snapshot) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(snapshot) = self.link.poll() { self.last = snapshot; }
                if done(&self.last) { return; }
                assert!(Instant::now() < deadline, "simulator did not reach the expected state: {:?}", self.last);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    impl Drop for FileSim {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); }
    }

    #[test]
    fn file_reports_update_state_layer_and_head_and_follow_rewrites() {
        let mut sim = FileSim::new("reports", concat!(
            "# report fixture\n\n@unknown\nnot json\n[]\n",
            "{\"print_stats\":{\"state\":\"printing\",\"info\":{\"current_layer\":2,\"total_layer\":12}},\"virtual_sdcard\":{\"progress\":0.25}}\n",
            "@head 2 0 PETG FF000080\n",
        ));
        sim.wait_for(|s| s.state.layer == Some(2));
        assert_eq!(sim.last.state.gcode_state.as_deref(), Some("RUNNING"));
        assert_eq!(sim.last.state.total_layers, Some(12));
        assert_eq!(sim.last.state.progress, Some(25.0));
        let tray = sim.last.state.tray(TrayId { ams: 2, slot: 0 }).unwrap();
        assert_eq!(tray.material.as_deref(), Some("PETG"));
        assert_eq!(tray.present, Some(false));
        assert_eq!(tray.colour, Colour::parse("FF000080"));
        sim.append(r#"{"print_stats":{"state":"paused","info":{"current_layer":3}}}"#);
        sim.wait_for(|s| s.state.layer == Some(3));
        assert_eq!(sim.last.state.gcode_state.as_deref(), Some("PAUSE"));
        assert_eq!(sim.last.state.total_layers, Some(12), "deltas retain other fields");
        std::fs::write(&sim.path, "{\"print_stats\":{\"state\":\"complete\"}}\n").unwrap();
        sim.wait_for(|s| s.state.gcode_state.as_deref() == Some("FINISH"));
    }

    /// A head count told for another printer model is not this printer's: it watches no head.
    #[test]
    fn heads_told_for_another_model_are_not_watched() {
        let mut sim = FileSim::new("other-model", "# ready\n");
        sim.wait_for(|s| s.state.extruders.len() == SIM_HEADS);
        sim.link.set_heads("Another printer", 6);
        sim.wait_for(|s| s.state.extruders.is_empty());
    }

    #[test]
    fn file_simulator_appends_each_accepted_start() {
        let mut sim = FileSim::new("starts", "# ready\n");
        for count in 1..=2 {
            let mut job = four_job(&[]);
            job.options.plate_clear = count > 1;
            sim.link.send(Command::Print(job));
            sim.wait_for(|s| s.job.as_ref().is_some_and(|j| j.stage == JobStage::Printing));
            let starts = std::fs::read_to_string(crate::sim::started_path(&sim.path)).unwrap();
            assert_eq!(starts.lines().collect::<Vec<_>>(), vec![full_start_line(); count]);
            sim.append(r#"{"print_stats":{"state":"complete"}}"#);
            sim.wait_for(|s| s.job.as_ref().is_some_and(|j| j.stage == JobStage::Finished));
            sim.append(r#"{"print_stats":{"state":"standby"}}"#);
            sim.wait_for(|s| s.state.gcode_state.as_deref() == Some("IDLE"));
        }
    }

    #[test]
    fn file_simulator_refuses_the_next_start_with_its_reason() {
        let mut sim = FileSim::new("refuse", "@refuse fixture refusal\n");
        sim.link.send(Command::Print(four_job(&[])));
        sim.wait_for(|s| s.job.as_ref().is_some_and(|j| matches!(&j.stage, JobStage::Failed(why) if why.contains("fixture refusal"))));
        assert!(!crate::sim::started_path(&sim.path).exists());
        let mut job = four_job(&[]);
        job.options.plate_clear = true;
        sim.link.send(Command::Print(job));
        sim.wait_for(|s| s.job.as_ref().is_some_and(|j| j.stage == JobStage::Printing));
        assert_eq!(std::fs::read_to_string(crate::sim::started_path(&sim.path)).unwrap().lines().count(), 1);
    }

    fn four_job(changes: &[(&str, Option<&str>)]) -> PrintJob {
        use crate::safety::{validate_gcode, MachineProfile};
        let area = crate::safety::parse_points("0x0,40x0,40x40,0x40");
        let profile = MachineProfile {
            flavor: makepad_fab_settings::family::Flavor::Named,
            printer_model: makepad_fab_settings::family::FAMILIES[1].models[0].into(), printable_area: area.clone(), printable_height: 40.0,
            extruder_areas: vec![area; 4], extruder_heights: vec![40.0; 4], extruder_offsets: vec![[0.0, 0.0]; 4],
            nozzle_diameters: vec![0.4; 4], filament_diameters: vec![1.75; 4], max_volumetric: vec![25.0; 4],
            filament_types: vec!["PLA".into(); 4], nozzle_temp_ranges: vec![[190.0, 240.0]; 4], print_temps: vec![[220.0; 2]; 4],
            templates: vec![("machine_start_gcode".into(), "G28\nG90\nM83\n".into())], prime_allowance: 5.0,
            ..Default::default()
        };
        let mut text = String::from("; EXECUTABLE_BLOCK_START\nG28\nG90\nM83\n");
        for i in 0..4 {
            let y = 5 + i * 5;
            text.push_str(&format!("T{i}\nG1 Z0.6 F1200\nG1 X5 Y{y} F3000\nG1 Z0.2\nG1 X15 E0.4 F600\n"));
        }
        text.push_str(&format!("; EXECUTABLE_BLOCK_END\n; filament used [mm] = 1016.1, 1681.25, 1073.41, 1910.55\n; filament used [g] = 3.23, 5.34, 3.41, 6.07\n; CONFIG_BLOCK_START\n; printer_model = {}\n; printable_area = 0x0,40x0,40x40,0x40\n; printable_height = 40\n; nozzle_diameter = 0.4,0.4,0.4,0.4\n; extruder_offset = 0x0,0x0,0x0,0x0\n; filament_map = 1,2,3,4\n; machine_start_gcode = G28\\nG90\\nM83\\n\n", profile.printer_model));
        for (key, value) in [
            ("line_width", "0.42"), ("layer_height", "0.2"), ("outer_wall_speed", "200,500"),
            ("nozzle_temperature", "220"), ("filament_flow_ratio", "0.98"), ("filament_diameter", "1.75"),
            ("filament_max_volumetric_speed", "15"), ("outer_wall_acceleration", "5000"),
            ("filament_volume_type", "standard"), ("print_sequence", "by layer"), ("process_flow_support", "standard,high_flow"),
        ] {
            let value = changes.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap_or(Some(value));
            if let Some(value) = value { text.push_str(&format!("; {key} = {value}\n")); }
        }
        text.push_str("; CONFIG_BLOCK_END\n");
        let job = validate_gcode(text.into_bytes(), &profile, "four.gcode").unwrap_or_else(|e| panic!("{}", e.message()));
        PrintJob { job, trays: (0..4).map(|h| Some(TrayId { ams: h, slot: 0 })).collect(), used: vec![0, 1, 2, 3], options: Default::default() }
    }
    fn full_start_line() -> String {
        concat!(
            r#"SDCARD_PRINT_FILE_WITH_PARAMETERS FILENAME="four.gcode" BED_LEVEL="1" FLOW_CALIBRATE="0" TIME_LAPSE_CAMERA="0" MAP_TABLE="[[0, 0], [1, 1], [2, 2], [3, 3]]""#,
            r#" LINE_WIDTH="0.42" LAYER_HEIGHT="0.2" OUTER_WALL_SPEED="200.0" NOZZLE_DIAMETER_LIST="[0.4, 0.4, 0.4, 0.4]" NOZZLE_TEMP="[220.0, 220.0, 220.0, 220.0]""#,
            r#" FILAMENT_TYPE="['PLA', 'PLA', 'PLA', 'PLA']" FILAMENT_FLOW_RATIO="[0.98, 0.98, 0.98, 0.98]" FILAMENT_DIAMETER="[1.75, 1.75, 1.75, 1.75]" FILAMENT_MAX_VOLUMETRIC_SPEED="[15.0, 15.0, 15.0, 15.0]""#,
            r#" FILAMENT_USED_G="[3.23, 5.34, 3.41, 6.07]" FILAMENT_USED_MM="[1016.1, 1681.25, 1073.41, 1910.55]" OUTER_WALL_ACCELERATION="5000.0" FILAMENT_VOLUME_TYPE="['standard', 'standard', 'standard', 'standard']" PRINT_SEQUENCE="by layer""#,
            r#" OUTER_WALL_SPEED_LIST="[200.0, 500.0]" OUTER_WALL_ACCELERATION_LIST="[5000.0]" PROCESS_FLOW_SUPPORT="['standard', 'high_flow']" SLICER="Fab" SLICER_VERSION=""#,
            env!("CARGO_PKG_VERSION"), r#"""#,
        ).into()
    }

    struct Rig {
        worker: Worker, sim: SimMoonraker, live: Live, now: Instant,
        ready: Receiver<()>, resume: Sender<()>, done: Receiver<()>,
    }
    impl Rig {
        fn new() -> Self {
            Self::with_sim(SimMoonraker::default(), SIM_HEADS)
        }
        /// On `sim`, watching `heads` heads.
        fn with_sim(sim: SimMoonraker, heads: usize) -> Self {
            let mut storage = HttpStorage::new(Target::Simulated(sim.clone()), Arc::new(AtomicBool::new(false)));
            let (ready_tx, ready) = channel(); let (resume, resume_rx) = channel(); let (done_tx, done) = channel();
            storage.hooks = Some(Arc::new(UploadHooks { ready: ready_tx, resume: std::sync::Mutex::new(resume_rx), done: done_tx }));
            let live = Live::connect(&storage, heads).unwrap();
            let mut worker = Worker::new(Snapshot::default()); worker.storage = Some(storage);
            let mut rig = Self { worker, sim, live, now: Instant::now(), ready, resume, done };
            rig.step(Vec::new()).unwrap();
            assert_eq!(rig.worker.snapshot.status, ConnectionStatus::Connected);
            rig
        }
        fn settle(&mut self) -> Result<(), ConnectionStatus> {
            for request in &mut self.worker.requests {
                let path = match &request.operation {
                    Operation::Start(_) => "/printer/gcode/script".to_string(),
                    Operation::Delete(file) => format!("/server/files/gcodes/{file}"),
                    Operation::Control(name) => match *name {
                        "pause" => "/printer/print/pause", "resume" => "/printer/print/resume",
                        "stop" => "/printer/print/cancel", "check_storage" => "/server/files/list?root=gcodes", _ => unreachable!(),
                    }.to_string(),
                };
                if !self.sim.held(&path) { request.task.wait(); }
            }
            let outcome = self.worker.requests(self.now);
            self.worker.show_job();
            outcome
        }
        fn step(&mut self, commands: Vec<Command>) -> Result<(), ConnectionStatus> {
            if let Some(opening) = &mut self.live.opening { opening.wait(); }
            if !self.sim.held("/server/info") { if let Some(info) = &mut self.live.info { info.wait(); } }
            self.worker.step(&mut self.live, commands, self.now)?;
            self.settle()
        }
        fn press(&mut self, command: Command) { self.step(vec![command]).unwrap(); }
        fn stage(&self) -> JobStage { self.worker.snapshot.job.as_ref().unwrap().stage.clone() }
        fn begin(&mut self) -> PrintJob {
            self.begin_job(four_job(&[]))
        }
        fn begin_job(&mut self, job: PrintJob) -> PrintJob {
            self.press(Command::Print(job.clone()));
            assert!(matches!(self.stage(), JobStage::Uploading { .. }));
            self.ready.recv_timeout(Duration::from_secs(5)).unwrap(); job
        }
        fn uploaded(&mut self) {
            self.resume.send(()).unwrap(); self.done.recv_timeout(Duration::from_secs(5)).unwrap();
            self.step(Vec::new()).unwrap();
        }
        fn printing(&mut self) {
            self.begin(); self.uploaded();
            assert_eq!(self.stage(), JobStage::Starting);
            self.step(Vec::new()).unwrap(); assert_eq!(self.stage(), JobStage::Accepted);
            self.step(Vec::new()).unwrap(); assert_eq!(self.stage(), JobStage::Printing);
        }
        fn advance(&mut self, seconds: f64) {
            self.now += Duration::from_secs_f64(seconds); self.sim.step(seconds); self.step(Vec::new()).unwrap();
        }
        fn reconnect(&mut self) {
            self.worker.snapshot.status = ConnectionStatus::Connecting;
            self.live = Live::connect(self.worker.storage.as_ref().unwrap(), SIM_HEADS).unwrap();
            self.step(Vec::new()).unwrap();
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.worker.storage.as_ref().unwrap().stop.store(true, Ordering::Release);
            let _ = self.resume.send(());
        }
    }

    #[test]
    fn cancelled_jobs_require_a_plate_check_even_after_restart() {
        let mut rig = Rig::new(); rig.printing(); rig.press(Command::Stop); rig.step(vec![]).unwrap();
        assert_eq!(rig.stage(), JobStage::Cancelled);
        let uploads = rig.sim.request_count("/server/files/upload");
        rig.press(Command::Print(heads_job()));
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.to_lowercase().contains("plate is clear")));
        assert_eq!(rig.sim.request_count("/server/files/upload"), uploads);
        rig.sim.restart(); rig.step(vec![]).unwrap();
        rig.sim.firmware("ready", "").unwrap(); rig.step(vec![]).unwrap();
        assert_eq!(rig.worker.snapshot.state.gcode_state.as_deref(), Some("IDLE"));
        rig.press(Command::Print(heads_job()));
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.to_lowercase().contains("plate is clear")));
        assert_eq!(rig.sim.request_count("/server/files/upload"), uploads);
        let mut job = heads_job(); job.options.plate_clear = true;
        rig.press(Command::Print(job)); rig.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        rig.uploaded(); rig.step(vec![]).unwrap();
        assert_eq!(rig.stage(), JobStage::Accepted); assert_eq!(rig.sim.started().len(), 2);
        // Another client's cancelled print is covered by the mapped terminal state too.
        let mut external = Rig::new();
        external.sim.update(obj(vec![("print_stats", obj(vec![("state", s("cancelled"))]))]));
        external.press(Command::Print(heads_job()));
        assert!(matches!(external.stage(), JobStage::Failed(why) if why.to_lowercase().contains("plate is clear")));
        assert_eq!(external.sim.request_count("/server/files/upload"), 0);
    }
    #[test]
    fn reconnect_waits_for_new_heads_before_starting() {
        let mut rig = Rig::new(); rig.begin(); rig.uploaded();
        rig.sim.drop_streams(); assert_eq!(rig.step(vec![]), Err(LOST));
        rig.sim.set_head(0, false, "PLA", "FFFFFFFF");
        rig.sim.hold_reply("printer.objects.subscribe"); rig.reconnect();
        assert_eq!(rig.stage(), JobStage::Starting); assert!(rig.sim.started().is_empty());
        rig.step(vec![]).unwrap(); assert!(rig.sim.started().is_empty());
        rig.sim.release_reply("printer.objects.subscribe"); rig.step(vec![]).unwrap();
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("head 1 is empty")));
        assert!(rig.sim.started().is_empty());
    }
    #[test]
    fn a_waiting_pause_leaves_stop_and_status_free() {
        let mut rig = Rig::new(); rig.printing();
        rig.sim.hold_reply("/printer/print/pause"); rig.press(Command::Pause);
        rig.sim.wait_requests("/printer/print/pause", 1);
        assert_eq!(rig.worker.snapshot.command, Some(("pause", CommandState::Pending)));
        rig.step(vec![]).unwrap(); assert_eq!(rig.stage(), JobStage::Paused);
        rig.press(Command::Stop); rig.step(vec![]).unwrap();
        assert_eq!(rig.worker.snapshot.command, Some(("stop", CommandState::Done)));
        assert_eq!(rig.stage(), JobStage::Cancelled);
        assert!(rig.worker.snapshot.state.details.iter().any(|(_, text)| text == "pause: waiting for the printer"));
        rig.sim.release_reply("/printer/print/pause"); rig.settle().unwrap();
        assert!(rig.worker.requests.is_empty());
        // Shutdown interrupts an unanswered exchange through the shared flag.
        rig.sim.hold_reply("/server/files/list?root=gcodes"); rig.press(Command::CheckStorage);
        rig.sim.wait_requests("/server/files/list?root=gcodes", 1);
        rig.worker.storage.as_ref().unwrap().stop.store(true, Ordering::Release);
        rig.worker.requests[0].task.wait();
        assert!(matches!(rig.worker.requests[0].task.take(), Some(Err(Error::Cancelled))));
        let mut pending = Rig::new(); pending.begin(); pending.uploaded();
        pending.sim.hold_reply("/printer/gcode/script"); pending.press(Command::CheckStorage);
        pending.sim.wait_requests("/printer/gcode/script", 1);
        pending.now += Duration::from_secs(1000); pending.step(vec![]).unwrap();
        assert_eq!(pending.stage(), JobStage::Starting, "an unanswered request never fails by a timer");
        pending.press(Command::Stop); assert_eq!(pending.worker.snapshot.command, Some(("stop", CommandState::Done)));
        pending.sim.release_reply("/printer/gcode/script"); pending.settle().unwrap();
        pending.sim.hold_reply("/server/info"); pending.reconnect();
        pending.sim.wait_requests("/server/info", 2);
        pending.press(Command::CheckStorage);
        assert_eq!(pending.worker.snapshot.command, Some(("check_storage", CommandState::Done)));
        pending.sim.release_reply("/server/info"); pending.step(vec![]).unwrap();
    }
    #[test]
    fn lost_start_replies_reconcile_once_without_resending() {
        for next in ["printing", "paused", "complete", "cancelled", "error", "standby", "reset"] {
            let mut rig = Rig::new(); rig.begin(); rig.uploaded();
            rig.sim.start_state(next); rig.sim.drop_reply("/printer/gcode/script");
            assert_eq!(rig.step(vec![]), Err(LOST));
            assert_eq!(rig.stage(), JobStage::StartReplyLost);
            assert!(rig.worker.plate_check);
            rig.reconnect();
            match next {
                "printing" => { assert_eq!(rig.stage(), JobStage::Printing); rig.advance(10.0); assert_eq!(rig.stage(), JobStage::Finished); }
                "paused" => assert_eq!(rig.stage(), JobStage::Paused),
                "complete" => assert_eq!(rig.stage(), JobStage::Finished),
                "cancelled" => assert_eq!(rig.stage(), JobStage::Cancelled),
                "error" => assert!(matches!(rig.stage(), JobStage::Failed(_))),
                "standby" => assert_eq!(rig.stage(), JobStage::Failed("the printer loaded the file but did not start it".into())),
                "reset" => assert_eq!(rig.stage(), JobStage::Failed("the start did not reach the printer".into())),
                _ => unreachable!(),
            }
            assert_eq!(rig.sim.request_count("/printer/gcode/script"), 1);
        }
        // A loaded file with an active reader also proves the lost start arrived.
        let mut rig = Rig::new(); rig.begin(); rig.uploaded();
        rig.sim.start_state("standby"); rig.sim.drop_reply("/printer/gcode/script");
        assert_eq!(rig.step(vec![]), Err(LOST));
        rig.sim.update(obj(vec![("virtual_sdcard", obj(vec![("is_active", Value::Bool(true)), ("file_path", s("gcodes/four.gcode"))]))]));
        rig.reconnect(); assert!(rig.stage().active()); assert_eq!(rig.sim.started().len(), 1);
        let mut restart = Rig::new(); restart.begin(); restart.uploaded();
        restart.sim.hold_reply("/printer/gcode/script"); restart.step(vec![]).unwrap();
        restart.sim.wait_requests("/printer/gcode/script", 1); restart.sim.restart();
        restart.sim.release_reply("/printer/gcode/script");
        assert_eq!(restart.settle(), Err(LOST)); assert_eq!(restart.stage(), JobStage::StartReplyLost);
        restart.sim.firmware("ready", "").unwrap(); restart.reconnect();
        assert_eq!(restart.stage(), JobStage::Failed("the start did not reach the printer".into()));
        assert_eq!(restart.sim.request_count("/printer/gcode/script"), 1);
    }
    #[test]
    fn firmware_readiness_requires_a_fresh_subscription() {
        let mut rig = Rig::new(); rig.begin(); rig.uploaded();
        let subscriptions = rig.sim.subscription_count();
        rig.sim.firmware("shutdown", "heater stopped").unwrap(); rig.step(vec![]).unwrap();
        assert_eq!(rig.worker.snapshot.state.gcode_state.as_deref(), Some("OFFLINE"));
        assert!(rig.worker.snapshot.state.errors.iter().any(|(_, why)| why == "heater stopped"));
        assert!(rig.sim.started().is_empty());
        rig.sim.hold_reply("printer.objects.subscribe"); rig.sim.firmware("ready", "").unwrap();
        rig.step(vec![]).unwrap(); assert!(rig.sim.started().is_empty());
        assert_eq!(rig.sim.subscription_count(), subscriptions + 1);
        rig.sim.release_reply("printer.objects.subscribe"); rig.step(vec![]).unwrap();
        assert_eq!(rig.stage(), JobStage::Accepted);
        // Disconnection drops subscriptions. Poll server info once per second, even without pushes.
        rig.sim.restart(); rig.step(vec![]).unwrap();
        rig.now += Duration::from_secs(1); rig.step(vec![]).unwrap(); rig.step(vec![]).unwrap();
        let infos = rig.sim.request_count("/server/info");
        rig.step(vec![]).unwrap(); assert_eq!(rig.sim.request_count("/server/info"), infos);
        rig.now += Duration::from_secs(1); rig.step(vec![]).unwrap(); rig.step(vec![]).unwrap();
        assert_eq!(rig.sim.request_count("/server/info"), infos + 1);
        let subscriptions = rig.sim.subscription_count();
        rig.sim.ready_without_notification(); rig.now += Duration::from_secs(1);
        rig.step(vec![]).unwrap(); rig.step(vec![]).unwrap();
        assert_eq!(rig.sim.subscription_count(), subscriptions + 1);
        assert_eq!(rig.worker.snapshot.status, ConnectionStatus::Connected);
    }
    #[test]
    fn accepted_jobs_time_out_without_a_status_change() {
        let mut rig = Rig::new(); rig.begin(); rig.uploaded(); rig.sim.start_state("standby");
        rig.step(vec![]).unwrap(); assert_eq!(rig.stage(), JobStage::Accepted);
        rig.step(vec![]).unwrap(); assert_eq!(rig.stage(), JobStage::Accepted);
        rig.now += Duration::from_secs(91); rig.step(vec![]).unwrap();
        assert_eq!(rig.stage(), JobStage::Failed("the printer did not start the job".into()));
    }
    #[test]
    fn stopping_a_stored_upload_deletes_it_before_the_next_job() {
        for held in [false, true] {
            let mut rig = Rig::new(); rig.begin();
            if held { rig.sim.hold_reply("/server/files/upload"); }
            rig.resume.send(()).unwrap(); rig.sim.wait_requests("/server/files/upload", 1);
            assert!(rig.sim.file("four.gcode").is_some());
            if !held { rig.done.recv_timeout(Duration::from_secs(5)).unwrap(); }
            rig.press(Command::Stop); assert_eq!(rig.stage(), JobStage::Cancelled);
            if held { rig.sim.release_reply("/server/files/upload"); rig.done.recv_timeout(Duration::from_secs(5)).unwrap(); }
            rig.step(vec![]).unwrap(); rig.step(vec![]).unwrap();
            assert!(rig.sim.file("four.gcode").is_none()); assert!(rig.sim.started().is_empty());
            assert_eq!(rig.stage(), JobStage::Cancelled);
            let mut client = rig.worker.storage.as_ref().unwrap().target.client(&Arc::new(AtomicBool::new(false))).unwrap();
            assert_eq!(client.delete("four.gcode"), Ok(()), "missing files count as cleaned");
            let count = rig.sim.request_count("/server/files/gcodes/four.gcode");
            for file in ["../four.gcode", "a/b.gcode", "a\\b.gcode", "a.gcode\n", "a.txt", "a\".gcode"] { assert!(client.delete(file).is_err()); }
            assert_eq!(rig.sim.request_count("/server/files/gcodes/four.gcode"), count);
        }
    }

    #[test]
    fn states_and_firmware_map_without_guessing_readiness() {
        let mut status = Status::new(SIM_HEADS);
        for (report, expected) in [("standby", "IDLE"), ("cancelled", "FAILED"), ("printing", "RUNNING"), ("paused", "PAUSE"), ("complete", "FINISH"), ("error", "FAILED"), ("future", "OFFLINE")] {
            status.state = report.into(); status.message = "check the print".into();
            let mapped = state(&status, "ready");
            assert_eq!(mapped.gcode_state.as_deref(), Some(expected), "{report}");
            assert_eq!(mapped.errors, if report == "error" { vec![("Print".into(), "check the print".into())] } else { vec![] });
            for firmware in ["startup", "shutdown", "error"] {
                let mapped = state(&status, firmware);
                assert_eq!(mapped.gcode_state.as_deref(), Some("OFFLINE"));
                assert!(!mapped.ready_for_job());
                assert_eq!(mapped.details, [("Firmware".into(), firmware.into())]);
            }
        }
    }
    /// A six-head printer is watched for no head until its profile's count comes, then for six,
    /// each with its own spool (A1 to F1).
    #[test]
    fn six_heads_are_watched_once_the_profile_says() {
        let mut rig = Rig::with_sim(SimMoonraker::with_heads(None, 6), 0);
        assert!(rig.worker.snapshot.state.extruders.is_empty());
        rig.live.watch_heads(6).unwrap();
        assert!((0..100).any(|_| { rig.step(Vec::new()).unwrap(); rig.worker.snapshot.state.extruders.len() == 6 }));
        let heaters: Vec<String> = rig.sim.subscribed().into_iter().filter(|o| o.starts_with("extruder")).collect();
        assert_eq!(heaters, (0..6).map(moonraker::heater_name).collect::<Vec<_>>());
        let state = &rig.worker.snapshot.state;
        for h in 0..6 {
            assert_eq!(state.extruders[h].id, Some(CloudNozzleId(h as u8)));
            assert_eq!(state.extruders[h].loaded, Some(LoadedSlot::Tray(TrayId { ams: h as u8, slot: 0 })));
            assert_eq!(state.ams[h].extruder, state.extruders[h].id);
        }
    }
    #[test]
    fn four_heads_map_temperatures_filament_colours_and_progress() {
        let mut rig = Rig::new();
        rig.sim.set_head(2, false, "PETG", "FF000080");
        rig.sim.update(obj(vec![
            ("extruder2", obj(vec![("temperature", Value::Int(210)), ("target", Value::Int(220))])),
            ("heater_bed", obj(vec![("temperature", Value::Int(55)), ("target", Value::Int(60))])),
            ("toolhead", obj(vec![("extruder", s("extruder2"))])),
            ("print_stats", obj(vec![("filename", s("four.gcode")), ("info", obj(vec![("current_layer", Value::Int(2)), ("total_layer", Value::Int(10))]))])),
            ("virtual_sdcard", obj(vec![("progress", Value::F64(0.25))])),
        ]));
        rig.step(Vec::new()).unwrap(); let state = &rig.worker.snapshot.state;
        assert!(state.numbered_heads);
        assert_eq!(state.extruders.len(), 4); assert_eq!(state.ams.len(), 4);
        for h in 0..4 {
            let id = TrayId { ams: h as u8, slot: 0 };
            assert_eq!(state.extruders[h].id, Some(CloudNozzleId(h as u8)));
            assert_eq!(state.ams[h].extruder, state.extruders[h].id);
            assert_eq!(state.tray(id).unwrap().material.as_deref(), Some(if h == 2 { "PETG" } else { "PLA" }));
            assert_eq!(state.extruders[h].loaded, if h == 2 { None } else { Some(LoadedSlot::Tray(id)) });
            assert_eq!(state.extruders[h].temperature.current, Some(if h == 2 { 210.0 } else { 20.0 }));
        }
        assert_eq!(state.extruders[2].temperature.target, Some(220.0));
        assert_eq!(state.ams[2].trays[0].colour, Colour::parse("FF000080"));
        assert_eq!(state.active_extruder, Some(CloudNozzleId(2)));
        assert_eq!(state.bed, Temperature { current: Some(55.0), target: Some(60.0) });
        assert_eq!((state.layer, state.total_layers, state.progress), (Some(2), Some(10), Some(25.0)));
        assert_eq!(state.job_name.as_deref(), Some("four.gcode")); assert_eq!(state.storage, Some(true));
    }
    #[test]
    fn malformed_head_lists_leave_fields_unknown() {
        for config in [Value::Null, s("bad"), obj(vec![]), obj(vec![
            ("filament_exist", Value::Arr(vec![s("true")])), ("filament_type", Value::Bool(true)),
            ("filament_color_rgba", Value::Arr(vec![s("not hex")])),
        ])] {
            let status = Status { print_task_config: config, ..Status::new(SIM_HEADS) };
            let mapped = state(&status, "ready");
            for h in 0..4 {
                let tray = &mapped.ams[h].trays[0];
                assert_eq!((tray.present, tray.material.as_ref(), tray.colour), (None, None, None));
                assert_eq!(mapped.extruders[h].loaded, None);
            }
        }
    }
    #[test]
    fn upload_start_and_completion_use_the_checked_bytes_and_tool_map() {
        let mut rig = Rig::new(); let job = rig.begin();
        assert_eq!(rig.stage(), JobStage::Uploading { sent: 0, total: job.job.bytes().len() as u64 });
        rig.uploaded(); assert_eq!(rig.stage(), JobStage::Starting);
        assert_eq!(rig.sim.file("four.gcode").unwrap(), job.job.bytes());
        assert!(rig.sim.started().is_empty());
        rig.step(Vec::new()).unwrap(); assert_eq!(rig.stage(), JobStage::Accepted);
        assert_eq!(rig.sim.started(), [full_start_line()]);
        rig.step(Vec::new()).unwrap(); assert_eq!(rig.stage(), JobStage::Printing);
        rig.advance(2.0); assert_eq!(rig.stage(), JobStage::Printing);
        rig.advance(8.0); assert_eq!(rig.stage(), JobStage::Finished);
        rig.press(Command::CheckStorage);
        assert_eq!(rig.worker.snapshot.storage_files, Some(Ok(vec![crate::ftps::Entry { name: "four.gcode".into(), size: Some(job.job.bytes().len() as u64), dir: false }])));
        assert_eq!(rig.sim.started().len(), 1);
    }
    #[test]
    fn start_facts_expand_preserve_order_and_omit_missing_values() {
        let job = four_job(&[("line_width", Some("105%")), ("process_flow_support", None)]);
        let params = start_params(&job.job, &job.used, &[0, 1, 2, 3], &job.options).unwrap();
        assert_eq!(moonraker::start_line("four.gcode", &params).unwrap(), full_start_line().replace(r#" PROCESS_FLOW_SUPPORT="['standard', 'high_flow']""#, ""));
        let options = PrintOptions { flow_calibration: true, ..Default::default() };
        let params = start_params(&job.job, &[0, 2], &[3, 1], &options).unwrap();
        let line = moonraker::start_line("four.gcode", &params).unwrap();
        assert!(line.contains(r#"FLOW_CALIBRATE="1" TIME_LAPSE_CAMERA="0" MAP_TABLE="[[0, 3], [2, 1]]" FLOW_CALIBRATE_EXTRUDERS="[1, 3]" LINE_WIDTH="0.42""#));
        assert_eq!(params.nozzle_temp, [220.0; 4], "facts stay indexed by job filament");
        let params = start_params(&job.job, &[0, 2], &[1, 1], &options).unwrap();
        assert_eq!(params.flow_calibrate_extruders, [1], "calibration heads never repeat");
        let job = four_job(&[
            ("nozzle_temperature", Some("210, 220, 230, 240")), ("filament_flow_ratio", Some("0.9, 1, 1.1, 1.2")),
            ("filament_diameter", Some("1.7, 1.75, 1.8, 1.85")), ("filament_max_volumetric_speed", Some("10, 15, 20, 25")),
            ("filament_volume_type", Some("standard, high_flow, standard, high_flow")),
        ]);
        let params = start_params(&job.job, &job.used, &[0, 1, 2, 3], &job.options).unwrap();
        assert_eq!(params.nozzle_temp, [210.0, 220.0, 230.0, 240.0]);
        assert_eq!(params.filament_flow_ratio, [0.9, 1.0, 1.1, 1.2]);
        assert_eq!(params.filament_diameter, [1.7, 1.75, 1.8, 1.85]);
        assert_eq!(params.filament_max_volumetric_speed, [10.0, 15.0, 20.0, 25.0]);
        assert_eq!(params.filament_volume_type, ["standard", "high_flow", "standard", "high_flow"]);
        let job = heads_job();
        let params = start_params(&job.job, &job.used, &[0, 1, 2, 3], &job.options).unwrap();
        let line = moonraker::start_line("four.gcode", &params).unwrap();
        assert_eq!(line, concat!(
            r#"SDCARD_PRINT_FILE_WITH_PARAMETERS FILENAME="four.gcode" BED_LEVEL="1" FLOW_CALIBRATE="0" TIME_LAPSE_CAMERA="0" MAP_TABLE="[[0, 0], [1, 1], [2, 2], [3, 3]]" NOZZLE_DIAMETER_LIST="[0.4, 0.4, 0.4, 0.4]" FILAMENT_TYPE="['PLA', 'PLA', 'PLA', 'PLA']" FILAMENT_USED_G="[0.01, 0.01, 0.01, 0.01]" FILAMENT_USED_MM="[0.4, 0.4, 0.4, 0.4]" SLICER="Fab" SLICER_VERSION=""#,
            env!("CARGO_PKG_VERSION"), r#"""#,
        ));
    }
    #[test]
    fn invalid_start_facts_fail_after_upload_without_starting() {
        for (key, value, reason) in [
            ("nozzle_temperature", "220,220,220", "does not match its filaments"),
            ("filament_flow_ratio", "1,1,1", "does not match its filaments"),
            ("filament_diameter", "1.75,1.75,1.75", "does not match its filaments"),
            ("filament_max_volumetric_speed", "15,15,15", "does not match its filaments"),
            ("filament_volume_type", "standard,standard,standard", "does not match its filaments"),
            ("filament_volume_type", "fast", "must be standard or high_flow"),
            ("nozzle_temperature", "abc", "is not a finite number"),
            ("nozzle_temperature", "NaN", "is not a finite number"),
            ("line_width", "inf", "is not a finite number"),
            ("layer_height", "0.2,0.3", "is not a finite number"),
            ("outer_wall_speed", "200,abc", "is not a finite number"),
            ("outer_wall_acceleration", "5000,inf", "is not a finite number"),
            ("print_sequence", "by \"layer\"", "has an invalid string"),
            ("print_sequence", "by layer,[x]%", "has an invalid string"),
            ("process_flow_support", "standard;high_flow", "has an invalid string"),
        ] {
            let mut rig = Rig::new();
            rig.begin_job(four_job(&[(key, Some(value))])); rig.uploaded(); rig.step(vec![]).unwrap();
            assert_eq!(rig.stage(), JobStage::Failed(format!("uploaded, not started: the file's {key} {reason}")), "{key}");
            assert!(rig.sim.file("four.gcode").is_some(), "{key}: upload completed");
            assert!(rig.sim.started().is_empty(), "{key}: nothing started");
            assert_eq!(rig.sim.request_count("/printer/gcode/script"), 0, "{key}: no start sent");
        }
    }
    #[test]
    fn a_refused_job_uploads_and_starts_nothing() {
        let mut rig = Rig::new(); rig.sim.set_head(2, true, "PETG", "FFFFFFFF");
        rig.press(Command::Print(heads_job()));
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("head 3 holds PETG")));
        assert!(rig.worker.upload.is_none()); assert!(rig.sim.file("four.gcode").is_none()); assert!(rig.sim.started().is_empty());
    }
    #[test]
    fn changed_heads_are_checked_again_before_starting() {
        let mut rig = Rig::new(); rig.begin();
        rig.sim.set_head(0, false, "PLA", "FFFFFFFF"); rig.uploaded();
        rig.step(Vec::new()).unwrap();
        assert!(matches!(rig.stage(), JobStage::Failed(why) if why.contains("head 1 is empty")));
        assert!(rig.sim.file("four.gcode").is_some()); assert!(rig.sim.started().is_empty());
    }
    #[test]
    fn start_refusal_explains_the_busy_code() {
        let mut rig = Rig::new(); rig.begin(); rig.uploaded(); rig.sim.refuse_next_start("531-16");
        rig.step(Vec::new()).unwrap();
        assert_eq!(rig.stage(), JobStage::Failed(moonraker::refusal("531-16").unwrap().into()));
        assert!(rig.sim.started().is_empty());
    }
    #[test]
    fn stop_during_upload_discards_the_file_and_start() {
        let mut rig = Rig::new(); rig.begin(); rig.press(Command::Stop);
        assert_eq!(rig.stage(), JobStage::Cancelled);
        rig.uploaded();
        assert_eq!(rig.stage(), JobStage::Cancelled);
        assert!(rig.sim.file("four.gcode").is_none()); assert!(rig.sim.started().is_empty());
        assert!(rig.worker.upload.is_none());
    }
    #[test]
    fn pause_resume_and_stop_follow_control_replies() {
        let mut rig = Rig::new(); rig.printing();
        for (command, name, stage) in [(Command::Pause, "pause", JobStage::Paused), (Command::Resume, "resume", JobStage::Printing), (Command::Stop, "stop", JobStage::Cancelled)] {
            rig.press(command); rig.step(Vec::new()).unwrap();
            assert_eq!(rig.worker.snapshot.command, Some((name, CommandState::Done))); assert_eq!(rig.stage(), stage);
        }
        rig.press(Command::ClearError);
        assert_eq!(rig.worker.snapshot.command, Some(("clean_print_error", CommandState::Failed("clear the error on the printer's screen".into()))));
    }
    #[test]
    fn api_key_mismatch_is_a_connection_status() {
        let storage = HttpStorage::new(Target::Simulated(SimMoonraker::new(Some("private-test-key".into()))), Arc::new(AtomicBool::new(false)));
        let mut live = Live::connect(&storage, SIM_HEADS).unwrap();
        live.opening.as_mut().unwrap().wait(); live.info.as_mut().unwrap().wait();
        assert_eq!(live.poll(&storage, Instant::now()), Err(ConnectionStatus::WrongApiKey));
        assert_eq!(ConnectionStatus::WrongApiKey.label(), "The printer wants an API key, or the key is wrong");
    }
    #[test]
    fn lost_streams_go_offline_then_follow_the_same_job_to_finished() {
        let mut rig = Rig::new(); rig.printing(); rig.advance(2.0);
        rig.sim.drop_streams(); assert_eq!(rig.step(Vec::new()), Err(LOST));
        assert_eq!(rig.worker.snapshot.status, LOST);
        rig.worker.offline(vec![Command::Pause]);
        assert_eq!(rig.worker.snapshot.command, Some(("pause", CommandState::Failed(NOT_CONNECTED.into()))));
        rig.reconnect(); assert_eq!(rig.stage(), JobStage::Printing);
        rig.advance(8.0); assert_eq!(rig.stage(), JobStage::Finished); assert_eq!(rig.sim.started().len(), 1);
        rig.sim.close(); assert_eq!(rig.step(Vec::new()), Err(LOST));
    }
    #[test]
    fn offline_prints_and_storage_are_answered_without_sending() {
        let mut rig = Rig::new(); rig.sim.drop_streams(); assert_eq!(rig.step(vec![Command::Print(heads_job()), Command::CheckStorage]), Err(LOST));
        assert_eq!(rig.stage(), JobStage::Failed(NOT_CONNECTED.into()));
        assert_eq!(rig.worker.snapshot.storage_files, Some(Err(NOT_CONNECTED.into())));
        rig.reconnect(); assert!(rig.sim.file("four.gcode").is_none()); assert!(rig.sim.started().is_empty());
    }
}
