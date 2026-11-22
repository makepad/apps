//! Read script values as owned circuit values.

use makepad_script::numeric::NumericValue;
use makepad_script::*;

const MAX_DEPTH: usize = 64;

/// An index into the native circuit records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// A component.
    Comp(usize),
    /// A component and its pin.
    Pin(usize, u16),
    /// A net.
    Net(usize),
    /// A bus.
    Bus(usize),
    /// A part.
    Part(usize),
    /// An instance.
    Inst(usize),
    /// A port.
    Port(usize),
    /// A pad.
    Pad(usize),
    /// A footprint.
    Fp(usize),
    /// A graphic.
    Gfx(usize),
    /// A symbol.
    Sym(usize),
}

/// A value read back from the VM.
#[derive(Clone, Debug)]
pub enum V {
    /// No value.
    Nil,
    /// A boolean.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string.
    Str(String),
    /// An identifier written as `@name`.
    Id(String),
    /// A colour in 0xRRGGBBAA form.
    Color(u32),
    /// A list of values.
    Arr(Vec<V>),
    /// An object with fields and optional keyless entries.
    Obj(Obj),
    /// An opaque native record.
    Handle(Handle),
    /// A build-scoped handle captured from a different circuit's records.
    Stale,
    /// A function (blocks keep the VM value itself).
    Fn,
    /// An unsupported value or a depth-limit message.
    Other(String),
}

/// An object's fields, keyless entries and prototype name.
#[derive(Clone, Debug)]
pub struct Obj {
    /// Fields in definition order, with inherited defaults after own fields.
    pub fields: Vec<(String, V)>,
    /// The object's own keyless entries.
    pub items: Vec<V>,
    /// The nearest named prototype, when present.
    pub proto: Option<String>,
}

impl Obj {
    /// Look up a field by name.
    pub fn get(&self, key: &str) -> Option<&V> {
        self.fields.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }
}

impl V {
    /// Read a number without conversion.
    pub fn num(&self) -> Option<f64> {
        match self {
            Self::Num(value) => Some(*value),
            _ => None,
        }
    }

    /// Read a string without conversion.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    /// Read an identifier's text.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Id(value) => Some(value),
            _ => None,
        }
    }

    /// Read the text of a string or identifier.
    pub fn text_or_id(&self) -> Option<&str> {
        match self {
            Self::Str(value) | Self::Id(value) => Some(value),
            _ => None,
        }
    }

    /// Read a boolean without conversion.
    pub fn bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Borrow a list's values.
    pub fn list(&self) -> Option<&[V]> {
        match self {
            Self::Arr(values) => Some(values),
            _ => None,
        }
    }

    /// Borrow an object.
    pub fn obj(&self) -> Option<&Obj> {
        match self {
            Self::Obj(value) => Some(value),
            _ => None,
        }
    }

    /// Read a native record handle.
    pub fn handle(&self) -> Option<Handle> {
        match self {
            Self::Handle(value) => Some(*value),
            _ => None,
        }
    }

    /// Describe a value for a diagnostic message.
    pub fn describe(&self) -> String {
        match self {
            Self::Nil => "nothing".into(),
            Self::Bool(value) => format!("{value}"),
            Self::Num(_) => "a number".into(),
            Self::Str(value) => format!("\"{value}\""),
            Self::Id(value) => format!("@{value}"),
            Self::Color(value) => format!("#{value:08x}"),
            Self::Arr(values) => format!("a list of {}", values.len()),
            Self::Obj(value) => match &value.proto {
                Some(proto) => format!("a {proto}"),
                None => "an object".into(),
            },
            Self::Handle(value) => match value {
                Handle::Comp(_) => "a component",
                Handle::Pin(_, _) => "a pin",
                Handle::Net(_) => "a net",
                Handle::Bus(_) => "a bus",
                Handle::Part(_) => "a part",
                Handle::Inst(_) => "an instance",
                Handle::Port(_) => "a port",
                Handle::Pad(_) => "a pad",
                Handle::Fp(_) => "a footprint",
                Handle::Gfx(_) => "a graphic",
                Handle::Sym(_) => "a symbol",
            }.into(),
            Self::Fn => "a fn".into(),
            Self::Stale => "a handle of another circuit (in a bench function, reach the circuit through c: c.net(\"VIN\"), c.dut.r)".into(),
            Self::Other(value) => value.clone(),
        }
    }
}

/// An identifier whose text can be read back.
pub fn lut_id(name: &str) -> LiveId {
    LiveId::from_str_with_lut(name).unwrap_or_else(|_| LiveId::from_str(name))
}

fn id_string(id: LiveId) -> String {
    id.as_string(|text| text.map(str::to_string)).unwrap_or_else(|| format!("{id}"))
}

/// The source file and 1-based line and column of an instruction.
pub fn loc_of_ip(vm: &ScriptVm, ip: ScriptIp) -> Option<(String, u32, u32)> {
    let loc = vm.bx.code_ref().ip_to_loc(ip)?;
    Some((loc.file().to_string(), loc.line(), loc.col() + 1))
}

fn text_of(vm: &ScriptVm, value: ScriptValue) -> Option<String> {
    if let Some(id) = value.as_id() {
        return Some(id_string(id));
    }
    if let Some(text) = value.as_string() {
        return Some(vm.bx.heap_ref().string(text).to_string());
    }
    value.as_inline_string(str::to_string)
}

/// Read a named argument, including inherited defaults.
pub fn arg(vm: &ScriptVm, args: ScriptObject, key: LiveId) -> ScriptValue {
    field(vm, args, key)
}

/// A field of an object; nil when it has none (the heap answers a missing key with an error value).
pub fn field(vm: &ScriptVm, obj: ScriptObject, key: LiveId) -> ScriptValue {
    let v = vm.bx.heap_ref().value(obj, key.into(), NoTrap);
    if v.is_err() { NIL } else { v }
}

/// Find an already reported error in a value's own fields and items, without following prototypes.
/// Like `Reader`, the scan stops at functions and native handles, so it visits only the plain values
/// a native reads: a call costs at most one more pass over its own arguments, never a walk of a
/// block's or a component's fields.
pub fn reported_error(vm: &ScriptVm, value: ScriptValue) -> Option<ScriptValue> {
    fn scan(vm: &ScriptVm, value: ScriptValue, depth: usize) -> Option<ScriptValue> {
        if value.is_err() {
            return Some(value);
        }
        if depth >= MAX_DEPTH {
            return None;
        }
        let heap = vm.bx.heap_ref();
        if let Some(object) = value.as_object() {
            let handle = field(vm, object, id_lut!(__proto)).as_id().is_some() && field(vm, object, id_lut!(ix)).as_number().is_some();
            if handle || heap.is_fn(object) {
                return None;
            }
            for entry in heap.map_entries_ordered(object) {
                if let Some(error) = scan(vm, entry.value, depth + 1) {
                    return Some(error);
                }
            }
            for index in 0..heap.vec_len(object) {
                let entry = heap.vec_key_value(object, index, NoTrap);
                if let Some(error) = scan(vm, entry.value, depth + 1) {
                    return Some(error);
                }
            }
        } else if let Some(array) = value.as_array() {
            for index in 0..heap.array_len(array) {
                let value = heap.array_index(array, index, NoTrap);
                if let Some(error) = scan(vm, value, depth + 1) {
                    return Some(error);
                }
            }
        }
        None
    }

    scan(vm, value, 0)
}

/// Reads VM values into owned values with bounded nesting.
pub struct Reader<'a, 'b> {
    /// The VM holding the values being read.
    pub vm: &'a mut ScriptVm<'b>,
}

impl<'a, 'b> Reader<'a, 'b> {
    /// Read values from this VM.
    pub fn new(vm: &'a mut ScriptVm<'b>) -> Self {
        Self { vm }
    }

    /// Read a value, its fields and its list entries.
    pub fn read(&mut self, value: ScriptValue) -> V {
        self.read_depth(value, 0)
    }

    fn read_depth(&mut self, value: ScriptValue, depth: usize) -> V {
        if value.is_nil() {
            return V::Nil;
        }
        if let Some(value) = value.as_bool() {
            return V::Bool(value);
        }
        if let Some(value) = value.as_color() {
            return V::Color(value);
        }
        if let Some(value) = value.as_number() {
            return V::Num(value);
        }
        if let Some(value) = value.as_id() {
            return V::Id(id_string(value));
        }
        if value.as_pod().is_some() {
            let nums = |values: &[f32]| V::Arr(values.iter().map(|value| V::Num(*value as f64)).collect());
            match NumericValue::from_script_value_heap(self.vm.heap(), value, self.vm.trap_ip()) {
                NumericValue::Vec2(value) => return nums(&[value.x, value.y]),
                NumericValue::Vec3(value) => return nums(&[value.x, value.y, value.z]),
                NumericValue::Vec4(value) => return nums(&[value.x, value.y, value.z, value.w]),
                _ => {}
            }
        }
        if value.is_string_like() {
            if let Some(text) = text_of(self.vm, value) {
                return V::Str(text);
            }
        }
        if depth >= MAX_DEPTH {
            return V::Other("nesting too deep".into());
        }
        if let Some(array) = value.as_array() {
            let len = self.vm.bx.heap_ref().array_len(array);
            return V::Arr((0..len).map(|index| {
                let value = self.vm.bx.heap_ref().array_index(array, index, NoTrap);
                self.read_depth(value, depth + 1)
            }).collect());
        }
        if let Some(object) = value.as_object() {
            if self.vm.bx.heap_ref().is_fn(object) {
                return V::Fn;
            }
            if let Some(handle) = self.read_handle(object) {
                return handle;
            }
            return self.read_object(object, depth);
        }
        V::Other(format!("{:?}", value.value_type()))
    }

    fn read_handle(&self, object: ScriptObject) -> Option<V> {
        let proto = arg(self.vm, object, id_lut!(__proto)).as_id()?;
        let ix = arg(self.vm, object, id_lut!(ix)).as_number()? as usize;
        let handle = match proto {
            id if id == id_lut!(Comp) => Handle::Comp(ix),
            id if id == id_lut!(Pin) => Handle::Pin(ix, arg(self.vm, object, id_lut!(pin)).as_number()? as u16),
            id if id == id_lut!(Net) => Handle::Net(ix),
            id if id == id_lut!(Bus) => Handle::Bus(ix),
            id if id == id_lut!(Part) => Handle::Part(ix),
            id if id == id_lut!(Inst) => Handle::Inst(ix),
            id if id == id_lut!(Port) => Handle::Port(ix),
            id if id == id_lut!(Pad) => Handle::Pad(ix),
            id if id == id_lut!(Fp) => Handle::Fp(ix),
            id if id == id_lut!(Gfx) => Handle::Gfx(ix),
            id if id == id_lut!(Sym) => Handle::Sym(ix),
            _ => return None,
        };
        let build = field(self.vm, object, id_lut!(build));
        if !build.is_nil() {
            let module = self.vm.bx.heap_ref().module(id_lut!(elec));
            if build != field(self.vm, module, id_lut!(__build)) {
                return Some(V::Stale);
            }
        }
        Some(V::Handle(handle))
    }

    fn read_object(&mut self, object: ScriptObject, depth: usize) -> V {
        let mut chain = Vec::new();
        let mut at = Some(object);
        while let Some(object) = at {
            if chain.len() > MAX_DEPTH {
                break;
            }
            chain.push(object);
            at = self.vm.bx.heap_ref().proto(object).as_object();
        }
        let mut proto = None;
        let mut raw: Vec<(String, ScriptValue)> = Vec::new();
        let mut items = Vec::new();
        for (level, object) in chain.iter().enumerate() {
            let heap = self.vm.heap();
            let mut fields: Vec<(String, ScriptValue)> = heap.map_entries_ordered(*object)
                .into_iter().map(|entry| (key_string(self.vm, entry.key), entry.value)).collect();
            for index in 0..heap.vec_len(*object) {
                let entry = heap.vec_key_value(*object, index, NoTrap);
                if entry.key.is_nil() {
                    if level == 0 {
                        items.push(entry.value);
                    }
                } else {
                    fields.push((key_string(self.vm, entry.key), entry.value));
                }
            }
            for (key, value) in fields {
                if key == "__proto" {
                    if proto.is_none() {
                        proto = value.as_id().map(id_string);
                    }
                    continue;
                }
                // A named prototype's inherited fields are not settings.
                if level > 0 && proto.is_some() {
                    continue;
                }
                if !raw.iter().any(|(name, _)| *name == key) {
                    raw.push((key, value));
                }
            }
        }
        if raw.is_empty() && proto.is_none() && !items.is_empty() {
            return V::Arr(items.into_iter().map(|value| self.read_depth(value, depth + 1)).collect());
        }
        let fields = raw.into_iter().map(|(key, value)| (key, self.read_depth(value, depth + 1))).collect();
        let items = items.into_iter().map(|value| self.read_depth(value, depth + 1)).collect();
        V::Obj(Obj { fields, items, proto })
    }
}

pub(crate) fn key_string(vm: &ScriptVm, key: ScriptValue) -> String {
    if let Some(text) = text_of(vm, key) {
        return text;
    }
    if let Some(value) = key.as_number() {
        return format!("{value}");
    }
    format!("{key:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_values_fields_items_and_handles() {
        let mut host = ScriptVmHost::new((), ());
        let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
        vm.capture_errors();
        let source = r#"use mod.pod.*
let base = {left: 1 right: 2}
{
    number: 42
    short: "0402"
    long: "resistor"
    ident: @green
    flag: true
    nested: [1, [2, 3]]
    ordered: {left: 4 right: 5}
    items: {6, 7, 8}
    mixed: {name: "pair", 9, 10}
    inherited: base{left: 11}
    vectors: [vec2(1, 2), vec3(3, 4, 5), vec4(6, 7, 8, 9)]
    net: {__proto: @Net ix: 3}
    pin: {__proto: @Pin ix: 4 pin: 2}
    comp: {__proto: @Comp ix: 4}
}"#;
        let value = vm.eval(ScriptMod::new("", "", "read.splash", 1, 0, source, &[]));
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "the value program evaluates: {errors:?}");
        let value = Reader::new(&mut vm).read(value);
        let root = value.obj().expect("the result is an object");
        let field = |key| root.get(key).unwrap_or_else(|| panic!("the result has field {key}"));

        assert_eq!(field("number").num(), Some(42.0), "numbers retain their value");
        assert_eq!(field("short").text(), Some("0402"), "inline strings retain their text");
        assert_eq!(field("long").text(), Some("resistor"), "heap strings retain their text");
        assert_eq!(field("ident").id(), Some("green"), "identifiers retain their text");
        assert_eq!(field("ident").text(), None, "identifiers are not strings");
        assert_eq!(field("short").id(), None, "strings are not identifiers");
        assert_eq!(field("short").text_or_id(), Some("0402"), "text_or_id accepts strings");
        assert_eq!(field("ident").text_or_id(), Some("green"), "text_or_id accepts identifiers");
        assert_eq!(field("flag").bool(), Some(true), "booleans retain their value");

        let nested = field("nested").list().expect("the outer array is a list");
        assert_eq!(nested.len(), 2, "the outer list retains both entries");
        assert_eq!(nested[0].num(), Some(1.0), "the outer list retains its number");
        let inner = nested[1].list().expect("a nested array is a list");
        assert_eq!(inner.iter().map(V::num).collect::<Vec<_>>(), [Some(2.0), Some(3.0)], "nested list entries retain order");

        let ordered = field("ordered").obj().expect("keyed entries form an object");
        assert_eq!(ordered.fields.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(), ["left", "right"], "fields retain definition order");
        assert_eq!(ordered.get("left").and_then(V::num), Some(4.0), "fields retain their values");
        assert!(ordered.get("missing").is_none(), "a missing field returns none");
        let items = field("items").list().expect("a keyless object is a list");
        assert_eq!(items.iter().map(V::num).collect::<Vec<_>>(), [Some(6.0), Some(7.0), Some(8.0)], "keyless entries retain order");
        let mixed = field("mixed").obj().expect("mixed fields and items form an object");
        assert_eq!(mixed.get("name").and_then(V::text), Some("pair"), "a mixed object retains fields");
        assert_eq!(mixed.items.iter().map(V::num).collect::<Vec<_>>(), [Some(9.0), Some(10.0)], "a mixed object retains keyless entries");
        let inherited = field("inherited").obj().expect("a derived object is an object");
        assert_eq!(inherited.get("left").and_then(V::num), Some(11.0), "own fields override prototype fields");
        assert_eq!(inherited.get("right").and_then(V::num), Some(2.0), "prototype fields supply defaults");

        let vectors = field("vectors").list().expect("the vectors are in a list");
        assert_eq!(vectors.len(), 3, "all three vector sizes are present");
        for (vector, expected) in vectors.iter().zip([&[1.0, 2.0][..], &[3.0, 4.0, 5.0], &[6.0, 7.0, 8.0, 9.0]]) {
            let values = vector.list().expect("a vector reads as a list");
            assert_eq!(values.iter().map(V::num).collect::<Vec<_>>(), expected.iter().copied().map(Some).collect::<Vec<_>>(), "vector components retain order");
        }
        assert_eq!(field("net").handle(), Some(Handle::Net(3)), "a net object reads as its handle");
        assert!(field("net").obj().is_none(), "a handle is opaque");
        assert_eq!(field("pin").handle(), Some(Handle::Pin(4, 2)), "a pin handle retains both indices");
        assert_eq!(field("comp").handle(), Some(Handle::Comp(4)), "a component object reads as its handle");

        assert_eq!(field("number").describe(), "a number", "numbers have a type description");
        assert_eq!(field("short").describe(), "\"0402\"", "strings are described with quotes");
        assert_eq!(field("ident").describe(), "@green", "identifiers are described with an at sign");
        assert_eq!(field("items").describe(), "a list of 3", "list descriptions include their length");
        assert_eq!(field("ordered").describe(), "an object", "objects have an object description");
        assert_eq!(field("net").describe(), "a net", "net handles have a net description");
        assert_eq!(field("pin").describe(), "a pin", "pin handles have a pin description");
        assert_eq!(field("comp").describe(), "a component", "component handles have a component description");
    }
}
