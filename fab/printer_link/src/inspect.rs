use crate::model::{CloudNozzleId, LoadedSlot, NozzleSide, Temperature, Tray, TrayId};
use crate::Snapshot;
use makepad_strict_json::{obj, s, Value};

fn number(n: Option<f64>) -> Value { n.map(Value::F64).unwrap_or(Value::Null) }
fn text(t: Option<&str>) -> Value { t.map(s).unwrap_or(Value::Null) }
fn side(n: Option<NozzleSide>) -> Value { text(n.map(NozzleSide::label)) }
fn id(t: TrayId) -> Value { obj(vec![("ams_id", Value::Int(t.ams as i64)), ("slot", Value::Int(t.slot as i64))]) }
fn loaded(t: Option<LoadedSlot>) -> Value {
    match t { Some(LoadedSlot::Tray(t)) => id(t), Some(LoadedSlot::NoneOrExternal) => s("none_or_external"), None => Value::Null }
}
fn temperature(t: Temperature) -> Value { obj(vec![("current", number(t.current)), ("target", number(t.target))]) }
fn tray(t: &Tray) -> Value {
    obj(vec![
        ("id", t.id.map(id).unwrap_or(Value::Null)),
        ("present", t.present.map(Value::Bool).unwrap_or(Value::Null)),
        ("type", text(t.material.as_deref())),
        ("rgba", t.colour.map(|c| Value::Arr(c.0.into_iter().map(|v| Value::F64(v as f64)).collect())).unwrap_or(Value::Null)),
        ("remain_percent", number(t.remain)),
        ("tray_info_idx", text(t.info_idx.as_deref())),
        ("nozzle_temp_min", number(t.nozzle_temp_min)), ("nozzle_temp_max", number(t.nozzle_temp_max)),
    ])
}

impl Snapshot {
    /// Explicit allowlist shared by chat and external agents; no transport/config data.
    /// `colours`: the print colours on the trays, as the app maps them.
    pub fn inspect(&self, colours: Value) -> Value {
        let state = &self.state;
        obj(vec![
            ("name", s(&self.name)), ("model", s(&self.model)), ("status", s(self.status.label())),
            ("virtual_printer", Value::Bool(self.virtual_printer)),
            ("serial_suffix", text(self.serial_suffix.as_deref())),
            ("gcode_state", text(state.gcode_state.as_deref())), ("progress", number(state.progress)),
            ("layer", state.layer.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)),
            ("total_layers", state.total_layers.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)),
            ("bed", temperature(state.bed)), ("chamber", temperature(state.chamber)),
            ("extruders", Value::Arr(state.extruders.iter().map(|e| obj(vec![
                ("side", side(e.id.and_then(CloudNozzleId::side))), ("temperature", temperature(e.temperature)),
                ("loaded", loaded(e.loaded)), ("target", loaded(e.target)),
            ])).collect())),
            ("nozzles", Value::Arr(state.nozzles.iter().map(|n| obj(vec![
                ("side", side(n.id.and_then(CloudNozzleId::side))), ("type", text(n.kind.as_deref())), ("diameter", number(n.diameter)),
            ])).collect())),
            ("ams", Value::Arr(state.ams.iter().map(|a| obj(vec![
                ("id", a.id.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)),
                ("type", a.kind.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)),
                ("side", side(a.extruder.and_then(CloudNozzleId::side))),
                ("humidity", number(a.humidity)), ("humidity_percent", number(a.humidity_percent)), ("temperature", number(a.temperature)),
                ("trays", Value::Arr(a.trays.iter().map(tray).collect())),
            ])).collect())),
            ("external", Value::Arr(state.trays().filter(|(_, t)| t.id.is_some_and(|t| t.ams >= 254)).map(|(n, t)| obj(vec![("side", side(n)), ("tray", tray(t))])).collect())),
            ("hms", Value::Arr(state.hms.iter().map(|h| s(h.code())).collect())),
            ("print_error", state.print_error.map(|n| s(format!("{n:08X}"))).unwrap_or(Value::Null)),
            ("stage", text(state.stage_name().as_deref())),
            ("remaining_minutes", number(state.remaining_minutes)),
            ("storage", state.storage.map(Value::Bool).unwrap_or(Value::Null)),
            ("camera", Value::Bool(state.camera)),
            ("printing_supported", Value::Bool(crate::printing_supported())),
            ("job", self.job.as_ref().map(|j| obj(vec![("name", s(&j.name)), ("file", s(&j.file)), ("stage", s(j.stage.label()))])).unwrap_or(Value::Null)),
            ("command", self.command.as_ref().map(|(c, st)| obj(vec![("command", s(*c)), ("state", s(match st {
                crate::CommandState::Pending => "pending".to_string(), crate::CommandState::Done => "done".into(), crate::CommandState::Failed(e) => format!("failed: {e}"),
            }))])).unwrap_or(Value::Null)),
            ("storage_files", match &self.storage_files {
                Some(Ok(files)) => Value::Arr(files.iter().map(|f| obj(vec![("name", s(&f.name)), ("size", f.size.map(|n| Value::Int(n as i64)).unwrap_or(Value::Null)), ("dir", Value::Bool(f.dir))])).collect()),
                Some(Err(e)) => obj(vec![("error", s(e))]),
                None => Value::Null,
            }),
            ("colours", colours),
        ])
    }
}
