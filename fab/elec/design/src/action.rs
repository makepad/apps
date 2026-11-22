//! The action catalog (contract C13, ARCHITECTURE 7.1): every operation of the suite is one
//! `ActionDef`, and from that one entry come the UI command, the palette item, the keymap target
//! and the AI tool, so an operation cannot exist for one and not the others.
//!
//! Entries are static data a lane declares next to its code and registers once at start-up
//! (`register`); the catalog is the union of every registration, sorted by name. Fields and
//! inputs are described in the terms the tools speak (Splash values), never JSON.
//!
//! Cost: `register` is O(n log n) in the catalog size and runs a few times at start-up;
//! it also checks O(returns text) per entry in debug builds only.
//! `catalog` is one atomic load and a short lock, O(1); `find` is a binary search. The lock is
//! held only for a pointer copy (the UI thread may call `catalog` every frame).

use std::sync::Mutex;

/// One operation.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct ActionDef {
    /// The tool name, dotted by area: `elec.route`, `elec.part.add`.
    pub name: &'static str,
    /// The command label shown in menus and the palette.
    pub label: &'static str,
    /// The palette and menu group: `Board`, `Schematic`, `Checks`.
    pub group: &'static str,
    /// One or two sentences: what it does (the reply's shape is `returns`).
    pub help: &'static str,
    /// The reply's fields, one per line: `name: kind  meaning` (meaning optional, after two
    /// spaces). A field nested in an object or a list of objects is indented two more spaces
    /// under it. `[kind]` is a list of that kind; a list's reply carries `total` and, when more
    /// remains, `next`. Kinds: `bool`, `int`, `num` with an optional unit (`num s`, `num V`,
    /// `num Hz`, `num %`), `text`, `choice a|b|c`, `id <what>`, `site`, `point`, `points`,
    /// `layer`, `layers`, `splash`, and `{..}` for an object. Empty: the reply is `ok`.
    pub returns: &'static str,
    /// Its parameters.
    pub fields: &'static [FieldDef],
    /// What it acts on, taken from the selection or the pointer when invoked from the UI.
    pub inputs: &'static [InputDef],
    pub risk: Risk,
    /// Default key binding as a key-chord string (`"Ctrl+Shift+R"`, `"X"`); empty for none.
    pub keys: &'static str,
}

/// The field an AI edit's answer gains once every check has run on its commit: what the edit
/// changed for the checks, by stable finding id. Every edit of what the checks read answers with
/// it (`ActionDef::answers_checks`); its help shows these lines after the action's own.
pub const CHECKS_RETURNS: &str = "checks: {..}  what the edit changed for the checks, once every check ran on it
  new: [{..}]  the findings it added, errors first, as elec.check.list lists them
  gone: int  how many findings it cleared
  more: text  when it added more than a page: how many more, elec.check.list lists them
  current: bool  false when the plate's electronics loaded again before the checks ran
  why: text  with current: false";

impl ActionDef {
    /// Check the declared names and scalar kinds before resolving any design objects.
    pub fn check_args<K: AsRef<str>, V: AsRef<str>>(&self, args: &[(K, V)]) -> Result<(), fab_elec_core::Diag> {
        let error = |name: &str, message: &str| fab_elec_core::Diag::error(format!("{name}: {message}"), vec![]);
        for (name, _) in args {
            let name = name.as_ref();
            if !self.fields.iter().any(|field| field.name == name) {
                return Err(error(name, "unknown field"));
            }
        }
        for (index, (name, _)) in args.iter().enumerate() {
            let name = name.as_ref();
            if args[..index].iter().any(|(other, _)| other.as_ref() == name) {
                return Err(error(name, "duplicate field"));
            }
        }
        for field in self.fields {
            if field.required && !args.iter().any(|(name, _)| name.as_ref() == field.name) {
                return Err(error(field.name, "required field is missing"));
            }
        }
        for (name, value) in args {
            let name = name.as_ref();
            let text = value.as_ref().trim();
            let field = self.fields.iter().find(|field| field.name == name).expect("field names checked");
            match field.kind {
                FieldKind::Bool if !matches!(text, "true" | "false") => return Err(error(name, "expected true or false")),
                FieldKind::Int { min, max } => {
                    if !text.replace('_', "").parse::<i64>().is_ok_and(|n| (min..=max).contains(&n)) {
                        return Err(error(name, &format!("expected an integer from {min} to {max}")));
                    }
                }
                FieldKind::Choice(list) if !list.contains(&text.strip_prefix('@').unwrap_or(text)) => {
                    return Err(error(name, &format!("expected {}", list.join("|"))));
                }
                FieldKind::Num { .. } if text != "inf" && !starts_with_number(text) => return Err(error(name, "expected a number")),
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether the AI's call of this action answers once every check has run on its commit, with
    /// `checks` (`CHECKS_RETURNS`): every edit of what the checks read. The bench's and the Scope's
    /// edits change what the bench runs and records, which no check reads. O(1).
    pub fn answers_checks(&self) -> bool {
        self.risk == Risk::Edit && !matches!(self.group, "Bench" | "Scope")
    }

    /// The AI tool's help and reply shape; menus and the palette use `help` alone.
    pub fn tool_description(&self) -> String {
        match (self.returns, self.answers_checks()) {
            ("", false) => self.help.to_owned(),
            ("", true) => format!("{}\n\nReturns:\n{CHECKS_RETURNS}", self.help),
            (returns, false) => format!("{}\n\nReturns:\n{returns}", self.help),
            (returns, true) => format!("{}\n\nReturns:\n{returns}\n{CHECKS_RETURNS}", self.help),
        }
    }
}

/// The action interprets the suffix after the signed decimal and its optional exponent.
fn starts_with_number(text: &str) -> bool {
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let mut rest = unsigned.trim_start_matches(|c: char| c.is_ascii_digit());
    let mut has_digits = rest.len() < unsigned.len();
    if let Some(fraction) = rest.strip_prefix('.') {
        rest = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
        has_digits |= rest.len() < fraction.len();
    }
    if let Some(exponent) = rest.strip_prefix(['e', 'E']) {
        let digits = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
        return has_digits && digits.as_bytes().first().is_some_and(u8::is_ascii_digit);
    }
    has_digits
}

/// Check a reply shape in O(text length), allocating only when reporting an error.
pub fn check_returns(text: &str) -> Result<(), String> {
    let mut previous_indent = 0;
    let mut previous_object = false;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let error = |reason: &str| format!("line {line_number}: {reason}");
        let indent = line.bytes().take_while(|&b| b == b' ').count();
        if indent % 2 != 0 {
            return Err(error("indent must be a multiple of two spaces"));
        }
        if index == 0 && indent != 0 {
            return Err(error("the first line must not be indented"));
        }
        if indent > previous_indent {
            if indent - previous_indent != 2 {
                return Err(error("indent may increase by only two spaces"));
            }
            if !previous_object {
                return Err(error("a child requires a parent of kind {..} or [{..}]"));
            }
        }
        let (name, rest) = line[indent..].split_once(": ")
            .ok_or_else(|| error("expected a field name followed by ': '"))?;
        let mut name_bytes = name.bytes();
        if !name_bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
            || !name_bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(error("field name must match [a-z_][a-z0-9_]*"));
        }
        let kind = if let Some((kind, meaning)) = rest.split_once("  ") {
            if meaning.trim().is_empty() {
                return Err(error("meaning after two spaces must not be empty"));
            }
            kind
        } else {
            rest
        };
        check_return_kind(kind).map_err(error)?;
        previous_indent = indent;
        previous_object = matches!(kind, "{..}" | "[{..}]");
    }
    Ok(())
}

fn check_return_kind(mut kind: &str) -> Result<(), &'static str> {
    // Peel list brackets without recursion or a stack proportional to the nesting depth.
    let depth = kind.bytes().take_while(|&b| b == b'[').count();
    kind = &kind[depth..];
    for _ in 0..depth {
        kind = kind.strip_suffix(']').ok_or("a list kind needs a matching closing ']'")?;
    }
    match kind {
        "bool" | "int" | "num" | "text" | "site" | "point" | "points" | "layer"
        | "layers" | "splash" | "{..}" => Ok(()),
        "choice" => Err("choice needs at least one alternative"),
        "id" => Err("id needs a word"),
        _ => {
            if let Some(choices) = kind.strip_prefix("choice ") {
                if choices.split('|').all(|choice| !choice.is_empty()
                    && choice.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
                {
                    return Ok(());
                }
                return Err("choice needs alternatives matching [a-z0-9_]+ separated by '|'");
            }
            if let Some(word) = kind.strip_prefix("id ") {
                if !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Ok(());
                }
                return Err("id needs one word");
            }
            if let Some(unit) = kind.strip_prefix("num ") {
                if !unit.is_empty() && !unit.chars().any(|c| c.is_whitespace() || matches!(c, '[' | ']')) {
                    return Ok(());
                }
                return Err("num's unit must be one token; separate a meaning with two spaces");
            }
            Err("unknown kind or invalid kind syntax; separate a meaning with two spaces")
        }
    }
}

/// A parameter of an action.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct FieldDef {
    pub name: &'static str,
    pub kind: FieldKind,
    pub help: &'static str,
    pub required: bool,
    /// The default as a Splash literal (`"0.25mm"`, `"@auto"`); empty for none.
    pub default: &'static str,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum FieldKind {
    Bool,
    Int { min: i64, max: i64 },
    /// A number with an optional unit suffix (`"mm"`, `"V"`, `"ohm"`; empty for a plain number).
    Num { unit: &'static str },
    Text,
    /// One of these symbols (`@auto`, `@grid`, ...), written without the `@`.
    Choice(&'static [&'static str]),
    /// One selection id of this kind.
    Sel(SelKind),
    /// A list of selection ids of this kind.
    Sels(SelKind),
    /// A point or a list of points in a unit's frame.
    Point,
    Points,
    /// A layer name or a set of layer names.
    Layer,
    Layers,
    /// A Splash statement or expression (span edits, block calls).
    Splash,
}

/// The kinds of thing an action can take as input.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum SelKind {
    Any,
    Unit,
    Comp,
    Pin,
    Net,
    Block,
    Item,
    Link,
    Feature,
    Source,
}

/// What an action acts on when invoked from the UI.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct InputDef {
    pub name: &'static str,
    pub kind: InputKind,
    pub help: &'static str,
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum InputKind {
    /// The current selection, of this kind; `many` takes all selected.
    Selection { kind: SelKind, many: bool },
    /// The pointer position in the view's unit frame.
    Pointer,
    /// A dragged region.
    Region,
}

/// What running an action can do, for confirmation policy and the AI's permissions.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Risk {
    /// Reads or shows; changes nothing.
    Read,
    /// Proposes a preview; nothing changes until it is committed.
    Preview,
    /// Changes the bench's or an instrument's state (play, pause, a capture), never the design; not an undo step.
    Run,
    /// Edits the design (one undo step).
    Edit,
    /// Writes files outside the project or talks to a device or a service.
    External,
}

static CATALOG: Mutex<&'static [ActionDef]> = Mutex::new(&[]);

/// Add a lane's actions. An entry whose name is already registered replaces it (so a module
/// that registers again, as after a reload, changes its entries in place).
pub fn register(defs: &'static [ActionDef]) {
    #[cfg(debug_assertions)]
    for d in defs {
        let result = check_returns(d.returns);
        debug_assert!(result.is_ok(), "{} returns: {}", d.name, result.unwrap_err());
    }
    let mut cat = CATALOG.lock().unwrap_or_else(|e| e.into_inner());
    let mut all: Vec<ActionDef> = cat.iter().filter(|a| !defs.iter().any(|d| d.name == a.name)).copied().collect();
    let mut seen: Vec<&str> = Vec::new();
    for d in defs.iter().rev() {
        if !seen.contains(&d.name) {
            seen.push(d.name);
            all.push(*d);
        }
    }
    all.sort_by(|a, b| a.name.cmp(b.name));
    // the catalog only grows and is registered a few times per run: the old slice is left alive
    // because callers may hold it
    *cat = Box::leak(all.into_boxed_slice());
}

/// Every registered action, sorted by name.
pub fn catalog() -> &'static [ActionDef] {
    *CATALOG.lock().unwrap_or_else(|e| e.into_inner())
}

/// The action named `name`.
pub fn find(name: &str) -> Option<&'static ActionDef> {
    let cat = catalog();
    cat.binary_search_by(|a| a.name.cmp(name)).ok().map(|i| &cat[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    static A: [ActionDef; 2] = [
        ActionDef { name: "elec.test.b", label: "B", group: "Test", help: "b", returns: "", fields: &[], inputs: &[], risk: Risk::Read, keys: "" },
        ActionDef {
            name: "elec.test.a",
            label: "A",
            group: "Test",
            help: "a",
            returns: "",
            fields: &[FieldDef { name: "w", kind: FieldKind::Num { unit: "mm" }, help: "width", required: false, default: "0.25mm" }],
            inputs: &[InputDef { name: "net", kind: InputKind::Selection { kind: SelKind::Net, many: false }, help: "the net" }],
            risk: Risk::Edit,
            keys: "W",
        },
    ];
    static B: [ActionDef; 1] = [ActionDef { name: "elec.test.a", label: "A2", group: "Test", help: "a again", returns: "", fields: &[], inputs: &[], risk: Risk::Preview, keys: "" }];

    #[test]
    fn reply_shapes_and_tool_description() {
        let examples = [
            "",
            "sources: [{..}]  every source, the simulator first
  id: id source
  side: choice sim|real|replay
  name: text
  state: choice closed|open|busy|error
  access: text  `ok`, or why it cannot be opened (permissions, in use, unplugged)
  channels: [{..}]
    id: int
    kind: choice volt|amp|logic
    range: [num]  the input ranges it offers, smallest first
    bits: int
  rates: [num Hz]  the sample rates it offers; the simulator gives its max step as `1/step`
  depth: int  samples per channel per capture; none for the simulator
  trigger: [choice edge|level|pulse|pattern|serial]  each with `hw` or `sw` after it
  outputs: [text]  generator and digital outputs, empty when none
  cannot: [text]  what this source cannot do that the tools could ask for",
            "probe: id probe
lane: int  the scope lane it draws in
unit: text
net: text  the resolved net (or branch for an `i` on a source or inductor)
site: site  the statement written",
            "source: id source
rate: num Hz  the real rate (the simulator: 1 / max step)
depth: int
span: [num s]  from, to, relative to the trigger
ranges: [num V]  per channel, the real input range
step: num V  per channel, the level step (bits over range; the simulator: none)
trigger: {..}
  kind: choice edge|level|pulse|none
  on: text
  level: num V
  slope: choice rise|fall|either
  how: choice hw|sw
  blind: num %  the share of time the source cannot see (re-arm, readout); 0 for the simulator
notes: [text]  every place the plan differs from the ask, and why",
            "a: [[num]]\nb: bool\nc: point\nd: points\ne: layer\nf: layers\ng: splash\n_h2: [id source]\ni: choice one",
        ];
        for text in examples {
            assert_eq!(check_returns(text), Ok(()), "valid reply shape: {text}");
        }
        assert_eq!(check_returns(CHECKS_RETURNS), Ok(()), "the shared checks field is a valid reply shape");
        for (text, line) in [
            ("a int", 1),
            ("a: int\nb: nmu s", 2),
            ("a: {..}\n   b: int", 2),
            ("a: int\n  b: int", 2),
            ("a: choice", 1),
            ("a: int\nb: int meaning", 2),
            ("  a: int", 1),
            ("a: {..}\n    b: int", 2),
            ("a: int\nb: [num", 2),
            ("a: num s]", 1),
            ("a: choice one||two", 1),
            ("a: id", 1),
            ("a: num V s", 1),
            ("a: int  ", 1),
            ("A: int", 1),
        ] {
            let error = check_returns(text).expect_err(text);
            assert!(error.starts_with(&format!("line {line}: ")), "wrong error line for {text}: {error}");
        }
        let mut entry = A[0];
        assert_eq!(entry.tool_description(), "b", "empty returns keeps just the help");
        entry.returns = examples[2];
        assert_eq!(entry.tool_description(), format!("b\n\nReturns:\n{}", examples[2]), "reply lines are preserved");
        let edit = ActionDef { returns: examples[2], ..A[1] };
        assert_eq!(edit.tool_description(), format!("a\n\nReturns:\n{}\n{CHECKS_RETURNS}", examples[2]), "an edit lists checks after its own reply lines");
        let bench = ActionDef { group: "Bench", ..edit };
        assert_eq!(bench.tool_description(), format!("a\n\nReturns:\n{}", examples[2]), "a bench edit lists only its own reply lines");
    }

    #[test]
    fn register_find_and_replace() {
        register(&A);
        let names: Vec<&str> = catalog().iter().map(|a| a.name).filter(|n| n.starts_with("elec.test.")).collect();
        assert_eq!(names, ["elec.test.a", "elec.test.b"], "sorted by name");
        assert_eq!(find("elec.test.a").map(|a| a.fields.len()), Some(1), "find returns the entry");
        assert!(find("elec.test.none").is_none(), "unknown names are absent");
        register(&B);
        assert_eq!(find("elec.test.a").map(|a| a.label), Some("A2"), "a later registration replaces by name");
        assert_eq!(catalog().iter().filter(|a| a.name == "elec.test.a").count(), 1, "names stay unique");
        assert!(find("elec.test.b").is_some(), "other entries are kept");
    }

    #[test]
    fn argument_checks_report_the_first_problem_by_field() {
        let action = ActionDef {
            fields: &[
                FieldDef { name: "enabled", kind: FieldKind::Bool, required: true, default: "", help: "" },
                FieldDef { name: "count", kind: FieldKind::Int { min: -20, max: 20 }, required: false, default: "", help: "" },
                FieldDef { name: "mode", kind: FieldKind::Choice(&["a", "b", "c"]), required: false, default: "", help: "" },
                FieldDef { name: "width", kind: FieldKind::Num { unit: "mm" }, required: false, default: "", help: "" },
            ],
            ..A[0]
        };
        for (args, expected) in [
            (&[("count", "0"), ("count", "1"), ("other", "")][..], "other: unknown field"),
            (&[("count", "0"), ("count", "1")], "count: duplicate field"),
            (&[("count", "wrong")], "enabled: required field is missing"),
            (&[("enabled", "yes")], "enabled: expected true or false"),
            (&[("enabled", "true"), ("count", "21")], "count: expected an integer from -20 to 20"),
            (&[("enabled", "true"), ("mode", "@d")], "mode: expected a|b|c"),
            (&[("enabled", "true"), ("width", "1e+")], "width: expected a number"),
        ] {
            let diag = action.check_args(args).expect_err(expected);
            assert_eq!(diag.message, expected, "the diagnostic identifies the argument error");
            assert!(diag.sel.is_empty(), "argument error has no design selection: {expected}");
        }
        for (flag, count, mode, width) in [("true", "-20", "@a", "-.5e+2mm"), ("false", "2_0", "b", "+12k")] {
            action.check_args(&[("enabled", flag), ("count", count), ("mode", mode), ("width", width)])
                .expect("both booleans, inclusive bounds, choice spellings and suffixed decimals are accepted");
        }
        action.check_args(&[("enabled", "true"), ("width", "inf")]).expect("numeric fields accept inf");
    }
}
