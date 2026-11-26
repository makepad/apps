//! A scene's function slots: places that name a function as a value
//! (`creature: unicorn_builder`), and the functions of the same signature
//! (the same parameters) that could take its place (`dragon_builder`). A
//! swap is changing that one name, like dropping another synth into a
//! slot; the scene's agent gets the slots and their alternatives, and the
//! strip offers each alternative as a variant.
//!
//! Main-loop cost: read when asked for (a scan of each file for the names
//! of its functions), never per frame.

use super::session::MotionSession;
use std::ops::Range;

/// A place that names a function, and what could take its place.
#[derive(Clone, Debug)]
pub struct Slot {
    pub doc: usize,
    pub range: Range<usize>,
    /// The field (or `let`) it is written under.
    pub what: String,
    pub current: String,
    pub alternatives: Vec<String>,
}

fn ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The scene's function slots, in the document and its modules (the
/// built-in modules are not the scene's). A slot in the document can name
/// its own functions and those of the modules it imports (`shapes.dragon`);
/// one in a module, that module's own. The parameters are the only link.
pub fn slots(session: &MotionSession) -> Vec<Slot> {
    let modules: Vec<(usize, &str, &makepad_motion::schema::code::CodeOutline, &str)> = session.modules.iter().enumerate().filter(|(_, m)| m.id != "builtin").map(|(k, m)| (k + 1, m.name.as_str(), &m.code, m.source.as_str())).collect();
    let files = std::iter::once((0, "", &session.code, session.source.as_str())).chain(modules.iter().copied());
    // A function's name as a file writes it, and its parameters (constants
    // and accessors, written in capitals, are not slots).
    let callable = |name: &str| !name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    // A name some function takes as a parameter is, where it is written, as
    // likely that parameter as the function (`at: at`, `fill: color`).
    let all_params: std::collections::HashSet<&str> = std::iter::once(&session.code).chain(modules.iter().map(|m| m.2)).flat_map(|c| c.fns.iter().flat_map(|(_, p)| p.iter().map(|x| x.as_str()))).collect();
    let mut out = Vec::new();
    for (doc, _, code, text) in files {
        let mut table: Vec<(String, &Vec<String>)> = code.fns.iter().filter(|(n, _)| callable(n)).map(|(n, p)| (n.clone(), p)).collect();
        if doc == 0 {
            for (_, module, mcode, _) in &modules {
                table.extend(mcode.fns.iter().filter(|(n, _)| callable(n)).map(|(n, p)| (format!("{module}.{n}"), p)));
            }
        }
        let bytes = text.as_bytes();
        for (name, params) in &table {
            if params.is_empty() || all_params.contains(name.rsplit('.').next().unwrap_or(name)) {
                continue;
            }
            let alternatives: Vec<String> = table.iter().filter(|(n, p)| n != name && *p == *params).map(|(n, _)| n.clone()).collect();
            // Too many of the same shape (`fn(t)`): not a kind of thing.
            if alternatives.is_empty() || alternatives.len() > MAX_ALTERNATIVES {
                continue;
            }
            let mut from = 0;
            while let Some(at) = text[from..].find(name.as_str()).map(|i| from + i) {
                from = at + name.len();
                let end = at + name.len();
                if (at > 0 && (ident_char(bytes[at - 1]) || bytes[at - 1] == b'.')) || (end < bytes.len() && ident_char(bytes[end])) {
                    continue;
                }
                // Named as a field's value, not called or read into, not in
                // a comment.
                let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
                if text[line_start..at].contains("//") {
                    continue;
                }
                let after = text[end..].trim_start();
                if after.starts_with('(') || after.starts_with('.') {
                    continue;
                }
                let Some(field) = text[..at].trim_end().strip_suffix(':') else { continue };
                let what = field.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("").to_string();
                if what.is_empty() {
                    continue;
                }
                out.push(Slot { doc, range: at..end, what, current: name.clone(), alternatives: alternatives.clone() });
            }
        }
    }
    out
}

/// A function with more alternatives than this is a common shape, not a
/// kind of thing a slot picks between.
const MAX_ALTERNATIVES: usize = 16;

/// The slots as the model reads them (each field and name once).
pub fn listing(slots: &[Slot]) -> String {
    let mut seen = std::collections::HashSet::new();
    slots.iter().filter(|s| seen.insert((s.what.as_str(), s.current.as_str()))).map(|s| format!("{}: {} (could be: {})\n", s.what, s.current, s.alternatives.join(", "))).collect()
}
