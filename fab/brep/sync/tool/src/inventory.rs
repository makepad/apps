use crate::strip::Source;
use crate::table::InventorySpec;
use relax_lex::lexer::T;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

#[derive(Default)]
pub struct Inventory {
    pub i1: BTreeMap<String, usize>,
    pub i2: BTreeMap<String, Vec<String>>,
    pub shim_helpers: BTreeSet<String>,
    pub forbid: usize,
    pub generated: Vec<(String, bool)>,
}

fn hit(counts: &mut BTreeMap<String, usize>, kind: &str) {
    *counts.entry(kind.into()).or_default() += 1;
}

pub(crate) fn matches(s: &Source<'_>, i: usize, words: &[&str]) -> bool {
    words.iter().enumerate().all(|(j, &word)| s.text(i + j) == word)
}

fn string_value(token: &str) -> Option<&str> {
    let bare = if let Some(raw) = token.strip_prefix('r') {
        let (hashes, _) = raw.split_once('"')?;
        if !hashes.bytes().all(|b| b == b'#') { return None; }
        raw.strip_prefix(hashes)?.strip_suffix(hashes)?
    } else { token };
    bare.strip_prefix('"')?.strip_suffix('"')
}

fn error_forms(s: &Source<'_>, first: usize, end: usize) -> BTreeSet<String> {
    let mut forms = BTreeSet::new();
    if s.text(first) == "transparent" && first + 1 == end {
        forms.insert("transparent".into());
        return forms;
    }
    let Some(mut text) = string_value(s.text(first)) else {
        forms.insert(format!("unclassified argument {}", s.text(first)));
        return forms;
    };
    while let Some((_, rest)) = text.split_once('{') {
        if let Some(tail) = rest.strip_prefix('{') { text = tail; continue; }
        let Some((capture, tail)) = rest.split_once('}') else {
            forms.insert("unclassified format".into()); break;
        };
        let (field, format) = capture.split_once(':').unwrap_or((capture, ""));
        let kind = if field.is_empty() { "implicit" } else if field.bytes().all(|c| c.is_ascii_digit()) {
            "positional"
        } else if field.chars().all(|c| c.is_alphanumeric() || c == '_') { "inline" } else { "unclassified capture" };
        forms.insert(kind.into());
        if format == "?" { forms.insert("debug".into()); }
        else if !format.is_empty() { forms.insert(format!("format {format}")); }
        text = tail;
    }
    if forms.is_empty() { forms.insert("plain".into()); }
    if s.kind(first + 1) == T::Comma && first + 2 < end {
        forms.insert("extra arguments".into());
        for i in first + 2..end {
            if s.kind(i) == T::Dot && !matches!(s.kind(i - 1),
                T::Ident | T::RawIdent | T::Int | T::CloseParen | T::CloseBrace | T::CloseBracket) {
                forms.insert("field shorthand".into());
            }
        }
    } else if first + 1 < end && s.kind(first + 1) != T::Comma {
        forms.insert("unclassified arguments".into());
    }
    forms
}

fn serde_helpers(s: &Source<'_>, open: usize, inv: &mut Inventory, spec: &InventorySpec, test: bool) -> Result<(), String> {
    let mut j = open + 1;
    let end = s.after(open) - 1;
    while j < end {
        if !s.kind(j).is_ident_like() { return Err(s.error(j, "I1: unclassified serde helper")); }
        let name = s.text(j);
        hit(&mut inv.i1, &format!("serde.{name}"));
        if test && !spec.serde_shim.iter().any(|k| k == name) {
            inv.shim_helpers.insert(format!("{name} {}:{}", s.path, s.line(j)));
        }
        j += 1;
        while j < end && s.kind(j) != T::Comma { j = s.after(j); }
        j += 1;
    }
    Ok(())
}

fn derive_error(s: &Source<'_>, i: usize, inv: &mut Inventory) -> Result<(), String> {
    hit(&mut inv.i1, "thiserror.derive");
    let mut j = s.after(i + 1);
    while s.kind(j) == T::Pound && s.kind(j + 1) == T::OpenBracket { j = s.after(j + 1); }
    j = s.visibility(j);
    if !matches!(s.kind(j), T::KwEnum | T::KwStruct) { return Err(s.error(i, "I1: unclassified Error derive target")); }
    if s.kind(j + 2) == T::Lt { hit(&mut inv.i1, "thiserror.generic"); }
    while !matches!(s.kind(j), T::OpenBrace | T::OpenParen | T::Semi | T::Eof) { j += 1; }
    for k in j..s.after(j) {
        if s.text(k) == "source" && s.kind(k + 1) == T::Colon { hit(&mut inv.i1, "thiserror.source field"); }
    }
    Ok(())
}


type Imports<'a> = BTreeMap<&'a str, Vec<(&'a str, Range<usize>)>>;

// Track the standard module bound by each use tree in its lexical scope.
// A local CAD module called net, for example, supplies no std::net capability.
fn imports<'a>(s: &Source<'a>, cuts: &[Range<usize>], tests: &[Range<usize>]) -> Imports<'a> {
    let mut bindings = Imports::new();
    for i in 0..s.toks.len() {
        if s.kind(i) != T::KwUse || s.in_macro(i) { continue; }
        let pos = s.toks[i].lo as usize;
        if [cuts, tests].iter().any(|ranges| ranges.get(ranges.partition_point(|r| r.end <= pos)).is_some_and(|r| r.contains(&pos))) { continue; }
        let mut j = i + 1;
        while j < s.toks.len() && !matches!(s.kind(j), T::Semi | T::Eof) {
            let member = s.text(j);
            if s.std_member(j) && matches!(member, "fs" | "net" | "process" | "env" | "io" | "thread") {
                let binding = if s.kind(j + 1) == T::KwAs { Some(s.text(j + 2)) }
                    else if s.kind(j + 1) != T::PathSep { Some(member) }
                    else if s.kind(j + 2) == T::OpenBrace {
                        (j + 3..s.after(j + 2)).find(|&p| s.kind(p) == T::KwSelfValue)
                            .map(|p| if s.kind(p + 1) == T::KwAs { s.text(p + 2) } else { member })
                    } else { None };
                if let Some(name) = binding { bindings.entry(name).or_default().push((member, s.enclosure(i))); }
            }
            j += 1;
        }
    }
    bindings
}

fn capability(s: &Source<'_>, i: usize, imports: &Imports<'_>) -> Option<&'static str> {
    match s.kind(i) {
        T::KwUnsafe => Some("unsafe"),
        T::KwExtern => Some("extern"),
        T::KwStatic if s.kind(i + 1) == T::KwMut => Some("static mut"),
        T::Pound if s.kind(i + 1) == T::OpenBracket && s.text(i + 2) == "link" => Some("#[link"),
        T::Ident | T::RawIdent => {
            let name = s.text(i).trim_start_matches("r#");
            let imported = if s.kind(i + 1) == T::PathSep && (i == 0 || s.kind(i - 1) != T::PathSep) {
                imports.get(name).and_then(|bindings| bindings.iter().rev().find(|(_, scope)| scope.contains(&i))).map(|(n, _)| *n)
            } else { None };
            match imported.unwrap_or(name) {
            "fs" | "net" | "process" | "env" | "io" => {
                if !s.std_member(i) && imported.is_none() { return None; }
                match imported.unwrap_or(name) {
                    "fs" => Some("std::fs"), "net" => Some("std::net"), "process" => Some("std::process"),
                    "env" => Some("std::env"), _ => Some("std::io"),
                }
            }
            "thread" if (s.std_member(i) || imported.is_some()) && s.kind(i + 1) == T::PathSep && matches!(s.text(i + 2), "spawn" | "scope" | "Builder") => Some("std::thread"),
            "spawn" | "scope" | "Builder" if s.thread_import(i) => Some("std::thread"),
            "Instant" => Some("Instant"), "SystemTime" => Some("SystemTime"),
            "thread_local" if s.kind(i + 1) == T::Not => Some("thread_local!"),
            "println" if s.kind(i + 1) == T::Not => Some("println!"),
            "eprintln" if s.kind(i + 1) == T::Not => Some("eprintln!"),
            _ => None,
            }
        }
        _ => None,
    }
}

pub(crate) fn capabilities(s: &Source<'_>) -> BTreeMap<String, Vec<String>> {
    let imports = imports(s, &[], &[]);
    let mut kinds = BTreeMap::new();
    for i in 0..s.toks.len() {
        if let Some(k) = capability(s, i, &imports) {
            kinds.entry(k.into()).or_insert_with(Vec::new).push(format!("{}:{}", s.path, s.line(i)));
        }
    }
    kinds
}

pub(crate) fn inventory(s: &Source<'_>, cuts: &[Range<usize>], test_ranges: &[Range<usize>], test_file: bool,
    table: &crate::table::Table, lib: bool, inv: &mut Inventory) -> Result<(), String> {
    let (mut cut, mut cfg) = (0, 0);
    let expected = format!("\"/{}\"", table.build.output);
    let imports = if test_file { BTreeMap::new() } else { imports(s, cuts, test_ranges) };
    let mut consumed_env = None;
    for i in 0..s.toks.len() {
        let pos = s.toks[i].lo as usize;
        while cut < cuts.len() && cuts[cut].end <= pos { cut += 1; }
        if cut < cuts.len() && cuts[cut].contains(&pos) { continue; }
        while cfg < test_ranges.len() && test_ranges[cfg].end <= pos { cfg += 1; }
        let test = test_file || cfg < test_ranges.len() && test_ranges[cfg].contains(&pos);
        if !test {
            if let Some(k) = capability(s, i, &imports) {
                inv.i2.entry(k.into()).or_default().push(format!("{}:{}", s.path, s.line(i)));
            }
        }
        if s.kind(i) == T::Pound {
            if lib && s.at_root(i) && matches(s, i, &["#", "!", "[", "forbid", "(", "unsafe_code", ")", "]"]) { inv.forbid += 1; }
            if s.kind(i + 1) != T::OpenBracket { continue; }
            let name = s.text(i + 2);
            if matches!(name, "from" | "source" | "backtrace") { hit(&mut inv.i1, &format!("thiserror.#[{name}]")); }
            if s.kind(i + 3) != T::OpenParen {
                if matches!(name, "serde" | "error") { return Err(s.error(i, "I1: unclassified serde or error attribute")); }
                continue;
            }
            let end = s.after(i + 3) - 1;
            match name {
                "serde" => serde_helpers(s, i + 3, inv, &table.inventory, test)?,
                "error" => {
                    hit(&mut inv.i1, "thiserror.error");
                    for form in error_forms(s, i + 4, end) { hit(&mut inv.i1, &format!("thiserror.{form}")); }
                }
                "derive" if (i + 4..end).any(|j| s.text(j) == "Error") => derive_error(s, i, inv)?,
                _ => {}
            }
        } else if matches!(s.kind(i), T::Ident | T::RawIdent) && s.kind(i + 1) == T::Not {
            let name = s.text(i).trim_start_matches("r#");
            let is_include = name == "include";
            let out_dir = name == "env" && matches!(s.kind(i + 2), T::OpenParen | T::OpenBrace | T::OpenBracket)
                && string_value(s.text(i + 3)) == Some("OUT_DIR");
            if !is_include && (!out_dir || consumed_env == Some(i)) { continue; }
            let known = s.path == table.build.consumer && matches(s, i,
                &["include", "!", "(", "concat", "!", "(", "env", "!", "(", "\"OUT_DIR\"", ")", ",", &expected, ")", ")"]);
            if known { consumed_env = Some(i + 6); }
            inv.generated.push((format!("{}:{} {}", s.path, s.line(i),
                s.src.lines().nth(s.line(i) - 1).unwrap_or("").trim()), known));
        }
    }
    Ok(())
}

impl Inventory {
    pub fn check(&self, spec: &InventorySpec) -> Result<(), String> {
        let new_i1: Vec<_> = self.i1.keys().filter(|k| !spec.i1.contains(k)).collect();
        let new_i2: Vec<_> = self.i2.keys().filter(|k| !spec.i2.contains(k)).collect();
        if new_i1.is_empty() && new_i2.is_empty() { return Ok(()); }
        Err(format!("inventory changed: I1 new {new_i1:?}, I2 new {new_i2:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tests::sample_rules;

    #[test]
    fn inventories_fail_on_a_new_kind() {
        let table = sample_rules();
        let s = Source::parse("src/lib.rs", br##"#![forbid(unsafe_code)]
// #![forbid(unsafe_code)] println!()
#[derive(thiserror::Error)] #[error("{source:?} {}", .source)]
struct E<T> { source: T }
#[serde(new_helper)] struct S;
mod fs; fn f() { fs::local(); std::net::TcpStream::connect(""); }"##, 2021).unwrap();
        let mut inv = Inventory::default();
        inventory(&s, &[], &[], false, &table, true, &mut inv).unwrap();
        assert_eq!(inv.forbid, 1);
        let error = inv.check(&table.inventory).unwrap_err();
        for k in ["serde.new_helper", "thiserror.source field", "thiserror.generic", "thiserror.field shorthand", "std::net"] {
            assert!(error.contains(k), "{error}");
        }
        assert!(!inv.i2.contains_key("println!") && !inv.i2.contains_key("std::fs"));
    }
}
