//! Script values read out of the VM into plain Rust, so the run file's
//! fields and the natives' arguments are checked without holding the heap.

use makepad_script::*;

/// One script value. `Id` is an `@name` (escaped or plain id), kept apart
/// from `Str` because the run file uses both (`cursor: @synthetic`,
/// `app: "notes"`).
#[derive(Clone, Debug, PartialEq)]
pub enum Sv {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
    Id(String),
    Arr(Vec<Sv>),
    Obj(Vec<(String, Sv)>),
    Fn,
    /// Anything else (an error value, a pod, a handle), by its type name.
    Other(String),
}

impl Sv {
    pub fn kind(&self) -> &'static str {
        match self {
            Sv::Nil => "nil",
            Sv::Bool(_) => "a bool",
            Sv::Num(_) => "a number",
            Sv::Str(_) => "a string",
            Sv::Id(_) => "an @id",
            Sv::Arr(_) => "an array",
            Sv::Obj(_) => "an object",
            Sv::Fn => "a function",
            Sv::Other(_) => "an unsupported value",
        }
    }

    pub fn field(&self, name: &str) -> Option<&Sv> {
        match self {
            Sv::Obj(fields) => fields.iter().find(|(key, _)| key == name).map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Sv::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }
}

/// Nesting deeper than this reads as `Other`: run files and call options
/// are flat, and a cyclic object must not recurse forever.
const MAX_DEPTH: usize = 16;

pub fn read(vm: &ScriptVm, value: ScriptValue) -> Sv {
    read_depth(vm, value, 0)
}

fn id_string(id: LiveId) -> String {
    id.as_string(|s| s.map(str::to_string)).unwrap_or_else(|| format!("{id}"))
}

fn read_depth(vm: &ScriptVm, value: ScriptValue, depth: usize) -> Sv {
    if value.is_nil() {
        return Sv::Nil;
    }
    if let Some(b) = value.as_bool() {
        return Sv::Bool(b);
    }
    if let Some(n) = value.as_number() {
        return Sv::Num(n);
    }
    if let Some(id) = value.as_id() {
        return Sv::Id(id_string(id));
    }
    if value.is_string_like() {
        let heap = vm.heap();
        if let Some(s) = value.as_string() {
            return Sv::Str(heap.string(s).to_string());
        }
        if let Some(s) = value.as_inline_string(|s| s.to_string()) {
            return Sv::Str(s);
        }
    }
    if depth >= MAX_DEPTH {
        return Sv::Other("nesting".into());
    }
    if let Some(array) = value.as_array() {
        let heap = vm.heap();
        let items = (0..heap.array_len(array))
            .map(|index| read_depth(vm, heap.array_index(array, index, NoTrap), depth + 1))
            .collect();
        return Sv::Arr(items);
    }
    if let Some(object) = value.as_object() {
        let heap = vm.heap();
        if heap.is_fn(object) {
            return Sv::Fn;
        }
        let mut fields = Vec::new();
        let mut items = Vec::new();
        heap.map_entries(object).iter().for_each(|e| {
            fields.push((key_string(vm, e.key), e.value));
        });
        for index in 0..heap.vec_len(object) {
            let entry = heap.vec_key_value(object, index, NoTrap);
            if entry.key.is_nil() {
                items.push(entry.value);
            } else {
                fields.push((key_string(vm, entry.key), entry.value));
            }
        }
        // An object written as a list (`[a, b]` stored in the vec part)
        // reads as an array.
        if fields.is_empty() && !items.is_empty() {
            return Sv::Arr(items.into_iter().map(|v| read_depth(vm, v, depth + 1)).collect());
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        return Sv::Obj(fields.into_iter().map(|(k, v)| (k, read_depth(vm, v, depth + 1))).collect());
    }
    Sv::Other(format!("{:?}", value.value_type()))
}

fn key_string(vm: &ScriptVm, key: ScriptValue) -> String {
    match read_depth(vm, key, MAX_DEPTH) {
        Sv::Id(s) | Sv::Str(s) => s,
        Sv::Num(n) => format!("{n}"),
        other => format!("{other:?}"),
    }
}

/// `file:line:col: message` from a VM error, as (line, col, message).
pub fn split_error(error: &str) -> (u32, u32, String) {
    let mut parts = error.splitn(4, ':');
    let (_, line, col, rest) = (parts.next(), parts.next(), parts.next(), parts.next());
    match (line.and_then(|l| l.trim().parse().ok()), col.and_then(|c| c.trim().parse().ok()), rest) {
        (Some(line), Some(col), Some(rest)) => (line, col, strip_origin(rest.trim())),
        _ => (1, 1, strip_origin(error)),
    }
}

/// Drop the VM's `(platform/script/src/...rs:NN)` origin suffix: it names
/// the interpreter's source, not the author's.
fn strip_origin(message: &str) -> String {
    match message.rfind(" (platform/script/") {
        Some(at) if message.ends_with(')') => message[..at].to_string(),
        _ => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vm_error_splits_into_its_location_and_message() {
        assert_eq!(
            split_error("run.splash:7:13: no getter for field x on type id (platform/script/src/native.rs:327)"),
            (7, 13, "no getter for field x on type id".to_string())
        );
        assert_eq!(split_error("plain"), (1, 1, "plain".to_string()));
    }
}
