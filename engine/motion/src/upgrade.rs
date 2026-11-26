//! Bringing a document written before vocabulary modules up to date: the
//! `use` lines it needs and its names from `mod.scene`, `mod.post` and
//! `mod.kernel` qualified (`scene.Mesh{..}`, `post.Glow{..}`,
//! `kernel.input(..)`), as the repository's documents were migrated. A
//! document already up to date is left as it is. The names each vocabulary
//! has are read from the modules a document VM installs, not from a list.
//!
//! What a document means does not change: only where its names come from is
//! written down. The library upgrade tool (`makepad-library-upgrade`) runs
//! this over a person's library.

use std::collections::{BTreeSet, HashSet};

/// The names each vocabulary has, as a document VM installs them (by id:
/// a module's keys are ids, some without their text).
pub struct Vocabularies {
    motion: HashSet<u64>,
    scene: HashSet<u64>,
    scene_only: HashSet<u64>,
    post: HashSet<u64>,
}

const KERNEL_FNS: &[&str] = &["input", "output", "emit_buffer", "param", "record", "table"];
const KERNEL_LAYOUTS: &[&str] = &["Sprite", "Segment", "Contact", "Event", "Rigid", "Body"];
/// A Scene3D's fields that Motion's own (2D) layer reads; one with any
/// other field is a full 3D scene, whose code is scene code.
const SCENE_KEEP: &[&str] = &["name", "at", "x", "y", "scale", "rotate", "anchor", "opacity", "blend", "span", "blur", "effects", "visible", "locked", "size", "textures", "videos", "layers"];
const SCENE_SIMPLE: &[&str] = &["model", "camera", "lights", "nodes", "materials", "background", "exposure"];

fn id(name: &str) -> u64 {
    makepad_script::LiveId::from_str(name).0
}

impl Vocabularies {
    /// The installed modules' names (a document VM is made to read them).
    pub fn installed() -> Vocabularies {
        let mut ev = crate::value::evaluate(&crate::model::DocSource::file("use mod.motion.*\nMotion{size: vec2(1, 1) duration: 1.0}"), crate::MotionContext::default().signals, 0.0, &[], &crate::modules::NoModules, None).unwrap_or_else(|diags| panic!("an empty document: {diags:?}"));
        let names = |ev: &mut crate::value::Evaluated, module: &str| -> HashSet<u64> {
            ev.with_vm(|vm| {
                use makepad_script::*;
                // the vocabulary as a document gets it: a lazy module is made first
                vm.expand_lazy_key(LiveId::from_str(module));
                let mut out = HashSet::new();
                let Some(m) = vm.bx.heap_ref().value(vm.modules(), LiveId::from_str(module).into(), NoTrap).as_object() else { return out };
                let data = vm.bx.heap_ref();
                data.object_map_iter_ordered(m, |k, _| {
                    if let Some(id) = k.as_id() {
                        out.insert(id.0);
                    }
                });
                for kv in data.vec_ref(m) {
                    if let Some(id) = kv.key.as_id() {
                        out.insert(id.0);
                    }
                }
                out
            })
        };
        let mut motion = names(&mut ev, "motion");
        motion.insert(id("in"));
        motion.insert(id("import"));
        let scene = names(&mut ev, "scene");
        let (forms, shared, post, kernel) = (names(&mut ev, "forms"), names(&mut ev, "shared"), names(&mut ev, "post"), names(&mut ev, "kernel"));
        let core: HashSet<u64> = ["math", "pod", "std"].iter().flat_map(|m| names(&mut ev, m)).collect();
        let scene_only = scene.iter().filter(|n| !motion.contains(*n) && !forms.contains(*n) && !shared.contains(*n) && !core.contains(*n) && !kernel.contains(*n)).copied().collect();
        Vocabularies { motion, scene, scene_only, post }
    }

    fn post_name(&self, w: &str) -> bool {
        w.starts_with(|c: char| c.is_ascii_uppercase()) && w != "BloomDown13" && w != "BloomTent" && self.post.contains(&id(w))
    }

    fn scene_camel(&self, w: &str) -> bool {
        w.starts_with(|c: char| c.is_ascii_uppercase()) && self.scene_only.contains(&id(w))
    }

    fn scene_lower(&self, w: &str) -> bool {
        w.starts_with(|c: char| c.is_ascii_lowercase()) && (w.starts_with("kit_") || !["kits_loaded", "crowd_lay", "crowd_move", "screen_crop"].contains(&w)) && self.scene_only.contains(&id(w))
    }

    /// Whether `name` is one of `vocabulary`'s (`motion`, `scene`, `post`).
    pub fn has(&self, vocabulary: &str, name: &str) -> bool {
        match vocabulary {
            "motion" => self.motion.contains(&id(name)),
            "scene" => self.scene.contains(&id(name)),
            _ => self.post.contains(&id(name)),
        }
    }
}

/// What a document is, for its main vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A Motion document or a library module: `use mod.motion.*`.
    Motion,
    /// A scene-only file: `use mod.scene.*`.
    Scene,
}

/// The kind of a library document's text: an edit (`use mod.flow`) or a
/// text that is neither is not a Motion document (None). `module`: the
/// asset is tagged a library module (then Motion unless it is a scene).
pub fn kind_of(text: &str, module: bool) -> Option<Kind> {
    if text.contains("use mod.flow") {
        return None;
    }
    let code = text;
    if has_object(code, "Motion") {
        return Some(Kind::Motion);
    }
    if has_object(code, "Scene3D") || has_object(code, "Seq3D") {
        return Some(if module { Kind::Motion } else { Kind::Scene });
    }
    if module || text.contains("use mod.motion") {
        return Some(Kind::Motion);
    }
    None
}

/// `text` brought up to date as a document of `kind`, or None when it is
/// already (nothing to change).
pub fn namespaces(text: &str, kind: Kind, vocab: &Vocabularies) -> Option<String> {
    let original = text;
    let scene_only = kind == Kind::Scene;
    let ranges = if scene_only { Vec::new() } else { scene_ranges(original) };
    // A scene's `music(@feature, lo, hi)` / `voice(..)` (a value that
    // follows the audio) is `follow_music` / `follow_voice`; `music(@m)`
    // stays the track.
    let followed = follow_audio(text, &ranges, scene_only);
    let text = followed.as_str();
    let ranges = if scene_only { Vec::new() } else { scene_ranges(text) };
    let (edits, need) = analyse(text, &ranges, scene_only, vocab);
    let mut out = text.to_string();
    for (pos, q) in edits.iter().rev() {
        out.insert_str(*pos, &format!("{q}."));
    }
    let out = add_uses(&out, if scene_only { "scene" } else { "motion" }, &need);
    (out != original).then_some(out)
}

/// `music(` / `voice(` with more than one argument in scene code, renamed.
fn follow_audio(text: &str, ranges: &[(usize, usize)], scene_only: bool) -> String {
    let toks: Vec<(Tk, usize, usize)> = tokens(text).into_iter().filter(|t| t.0 != Tk::Comment).collect();
    let b = text.as_bytes();
    let mut inserts = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        let w = &text[t.1..t.2];
        if t.0 != Tk::Ident || !(w == "music" || w == "voice") {
            continue;
        }
        if t.1 > 0 && (b[t.1 - 1] == b'.' || b[t.1 - 1] == b'@' || b[t.1 - 1].is_ascii_alphanumeric() || b[t.1 - 1] == b'_') {
            continue;
        }
        if !(scene_only || ranges.iter().any(|(a, z)| *a <= t.1 && t.1 < *z)) {
            continue;
        }
        if toks.get(k + 1).map(|n| &text[n.1..n.2]) != Some("(") {
            continue;
        }
        // Top-level commas inside the call: more than one argument.
        let mut depth = 0;
        let mut commas = 0;
        for n in &toks[k + 1..] {
            match &text[n.1..n.2] {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                "," if depth == 1 => commas += 1,
                _ => {}
            }
        }
        if commas > 0 {
            inserts.push(t.1);
        }
    }
    let mut out = text.to_string();
    for at in inserts.into_iter().rev() {
        out.insert_str(at, "follow_");
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tk {
    Ident,
    Str,
    Comment,
    Color,
    Symbol,
    Number,
    Punct,
}

fn tokens(s: &str) -> Vec<(Tk, usize, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    let ident0 = |c: u8| c.is_ascii_alphabetic() || c == b'_';
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < n {
        let c = b[i];
        if (c as char).is_ascii_whitespace() || c >= 0x80 && s[i..].chars().next().is_some_and(char::is_whitespace) {
            i += s[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        if s[i..].starts_with("//") {
            let j = s[i..].find('\n').map_or(n, |j| i + j);
            out.push((Tk::Comment, i, j));
            i = j;
            continue;
        }
        if s[i..].starts_with("/*") {
            let j = s[i + 2..].find("*/").map_or(n, |j| i + 2 + j + 2);
            out.push((Tk::Comment, i, j));
            i = j;
            continue;
        }
        if c == b'r' && (i == 0 || !ident(b[i - 1])) {
            let mut k = i + 1;
            while k < n && b[k] == b'#' {
                k += 1;
            }
            if k < n && b[k] == b'"' && k - i - 1 <= 2 {
                let end = format!("\"{}", "#".repeat(k - i - 1));
                let j = s[k + 1..].find(&end).map_or(n, |j| k + 1 + j + end.len());
                out.push((Tk::Str, i, j));
                i = j;
                continue;
            }
        }
        if c == b'"' {
            let mut j = i + 1;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(n);
            out.push((Tk::Str, i, end));
            i = j + 1;
            continue;
        }
        if c == b'#' {
            let mut j = i + 1;
            if j < n && (b[j] == b'x' || b[j] == b'X') {
                j += 1;
            }
            while j < n && ident(b[j]) {
                j += 1;
            }
            out.push((Tk::Color, i, j));
            i = j;
            continue;
        }
        if c == b'@' {
            let mut j = i + 1;
            while j < n && ident(b[j]) {
                j += 1;
            }
            out.push((Tk::Symbol, i, j));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let j = number_end(b, i);
            out.push((Tk::Number, i, j));
            i = j;
            continue;
        }
        if ident0(c) {
            let mut j = i + 1;
            while j < n && ident(b[j]) {
                j += 1;
            }
            out.push((Tk::Ident, i, j));
            i = j;
            continue;
        }
        let len = s[i..].chars().next().map_or(1, char::len_utf8);
        out.push((Tk::Punct, i, i + len));
        i += len;
    }
    out
}

/// `0x..`, or digits with `_`, an optional fraction and exponent.
fn number_end(b: &[u8], i: usize) -> usize {
    let n = b.len();
    if b[i] == b'0' && i + 1 < n && (b[i + 1] == b'x' || b[i + 1] == b'X') && i + 2 < n && (b[i + 2].is_ascii_hexdigit() || b[i + 2] == b'_') {
        let mut j = i + 2;
        while j < n && (b[j].is_ascii_hexdigit() || b[j] == b'_') {
            j += 1;
        }
        return j;
    }
    let mut j = i;
    while j < n && (b[j].is_ascii_digit() || b[j] == b'_') {
        j += 1;
    }
    if j + 1 < n && b[j] == b'.' && b[j + 1].is_ascii_digit() {
        j += 1;
        while j < n && (b[j].is_ascii_digit() || b[j] == b'_') {
            j += 1;
        }
    }
    if j < n && (b[j] == b'e' || b[j] == b'E') {
        let mut k = j + 1;
        if k < n && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        if k < n && b[k].is_ascii_digit() {
            while k < n && b[k].is_ascii_digit() {
                k += 1;
            }
            j = k;
        }
    }
    j
}

/// The names the file binds: let/var, fn params, for variables.
fn bound_names<'a>(s: &'a str, toks: &[(Tk, usize, usize)]) -> HashSet<&'a str> {
    let txt = |t: &(Tk, usize, usize)| &s[t.1..t.2];
    let mut names = HashSet::new();
    let n = toks.len();
    for (k, t) in toks.iter().enumerate() {
        if t.0 != Tk::Ident {
            continue;
        }
        let w = txt(t);
        if (w == "let" || w == "var") && k + 1 < n && toks[k + 1].0 == Tk::Ident {
            names.insert(txt(&toks[k + 1]));
        } else if w == "fn" && k + 1 < n && txt(&toks[k + 1]) == "(" {
            let mut d = 0;
            let mut j = k + 1;
            while j < n {
                let tj = txt(&toks[j]);
                if tj == "(" {
                    d += 1;
                } else if tj == ")" {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                } else if d == 1 && toks[j].0 == Tk::Ident {
                    let prev = txt(&toks[j - 1]);
                    if prev == "(" || prev == "," {
                        names.insert(tj);
                    }
                }
                j += 1;
            }
        } else if w == "for" {
            let mut j = k + 1;
            while j < n && txt(&toks[j]) != "in" && j < k + 8 {
                if toks[j].0 == Tk::Ident {
                    names.insert(txt(&toks[j]));
                }
                j += 1;
            }
        }
    }
    names
}

/// The qualifications to insert (position, vocabulary) and the
/// vocabularies the file then needs.
fn analyse(s: &str, scene_ranges: &[(usize, usize)], scene_only: bool, v: &Vocabularies) -> (Vec<(usize, &'static str)>, BTreeSet<&'static str>) {
    let toks = tokens(s);
    let code: Vec<(Tk, usize, usize)> = toks.into_iter().filter(|t| t.0 != Tk::Comment).collect();
    let txt = |t: &(Tk, usize, usize)| &s[t.1..t.2];
    let bound = bound_names(s, &code);
    let n = code.len();
    let mut in_use_line = HashSet::new();
    for (k, t) in code.iter().enumerate() {
        if t.0 == Tk::Ident && txt(t) == "use" {
            let mut j = k + 1;
            while j < n && (code[j].0 == Tk::Ident || matches!(txt(&code[j]), "." | "*")) && (j == k + 1 || code[j].1 == code[j - 1].2) {
                in_use_line.insert(j);
                j += 1;
            }
            in_use_line.insert(k);
        }
    }
    let in_scene = |pos: usize| scene_only || scene_ranges.iter().any(|(a, b)| *a <= pos && pos < *b);
    let mut edits = Vec::new();
    let mut need = BTreeSet::new();
    for k in 0..n {
        let t = &code[k];
        if t.0 != Tk::Ident || in_use_line.contains(&k) {
            continue;
        }
        let w = txt(t);
        let prev = if k > 0 { txt(&code[k - 1]) } else { "" };
        let nxt = if k + 1 < n { txt(&code[k + 1]) } else { "" };
        if prev == "." || matches!(prev, "let" | "var" | "fn" | "for") {
            continue;
        }
        if nxt == ":" && !(k + 2 < n && txt(&code[k + 2]) == ":") {
            continue;
        }
        let mut qual: Option<&'static str> = None;
        if KERNEL_FNS.contains(&w) && nxt == "(" {
            let nn = if k + 2 < n { txt(&code[k + 2]) } else { "" };
            if w == "input" && nn.starts_with('@') {
                continue;
            }
            qual = Some("kernel");
        } else if KERNEL_LAYOUTS.contains(&w) {
            if prev == "(" && k >= 2 && matches!(txt(&code[k - 2]), "output" | "input" | "emit_buffer") {
                qual = Some("kernel");
            } else if w == "Body" && nxt == "{" {
                qual = Some("scene");
            }
        } else if v.post_name(w) {
            qual = Some("post");
        } else if v.scene_camel(w) {
            qual = Some("scene");
        } else if v.scene_lower(w) && nxt == "(" {
            qual = Some("scene");
        } else if (w == "beat" || w == "bar") && nxt == "(" && in_scene(t.1) {
            qual = Some("scene");
        } else if w == "Crowd" && nxt == "{" && in_scene(t.1) {
            qual = Some("scene");
        }
        let Some(q) = qual else { continue };
        if q != "kernel" && bound.contains(w) {
            continue;
        }
        if scene_only && q == "scene" && v.has("scene", w) {
            continue;
        }
        edits.push((t.1, q));
        need.insert(q);
    }
    (edits, need)
}

/// The file's `use` statements: an existing `use mod.<main>.*` line is
/// extended, else one is put first.
fn add_uses(s: &str, main: &str, need: &BTreeSet<&str>) -> String {
    let order = ["scene", "post", "kernel"];
    let head = format!("use mod.{main}.*");
    let mut at = 0;
    for line in s.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if let Some(rest) = body.strip_prefix(&head) {
            let rest_trim = rest.trim_start_matches([' ', '\t']);
            if rest_trim.is_empty() || rest_trim.starts_with(';') {
                let end = at + body.len();
                let extra: String = order.iter().filter(|v| need.contains(**v) && **v != main && !s.contains(&format!("use mod.{v}"))).map(|v| format!("; use mod.{v}")).collect();
                return format!("{}{}{}", &s[..end], extra, &s[end..]);
            }
        }
        at += line.len();
    }
    let mut parts = vec![head];
    parts.extend(order.iter().filter(|v| need.contains(**v) && **v != main).map(|v| format!("use mod.{v}")));
    format!("{}\n{}", parts.join("; "), s)
}

fn skip(s: &[u8], i: usize) -> Option<usize> {
    if s[i..].starts_with(b"//") {
        return Some(s[i..].iter().position(|&c| c == b'\n').map_or(s.len(), |j| i + j));
    }
    if s[i..].starts_with(b"/*") {
        return Some(s[i + 2..].windows(2).position(|w| w == b"*/").map_or(s.len(), |j| i + 2 + j + 2));
    }
    if s[i] == b'"' {
        let mut j = i + 1;
        while j < s.len() && s[j] != b'"' {
            j += if s[j] == b'\\' { 2 } else { 1 };
        }
        return Some(j + 1);
    }
    None
}

fn match_close(s: &[u8], open: usize) -> usize {
    let mut d = 0i32;
    let mut i = open;
    while i < s.len() {
        if let Some(k) = skip(s, i) {
            i = k;
            continue;
        }
        match s[i] {
            b'{' | b'[' | b'(' => d += 1,
            b'}' | b']' | b')' => {
                d -= 1;
                if d == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len().saturating_sub(1)
}

fn top_fields(s: &[u8], open: usize, close: usize) -> Vec<String> {
    let mut names = Vec::new();
    let mut d = 0i32;
    let mut i = open + 1;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < close {
        if let Some(k) = skip(s, i) {
            i = k;
            continue;
        }
        let c = s[i];
        match c {
            b'{' | b'[' | b'(' => d += 1,
            b'}' | b']' | b')' => d -= 1,
            _ if d == 0 && (c.is_ascii_alphabetic() || c == b'_') && !(ident(s[i - 1]) || s[i - 1] == b'.') => {
                let mut j = i;
                while j < s.len() && ident(s[j]) {
                    j += 1;
                }
                let mut k = j;
                while k < s.len() && (s[k] == b' ' || s[k] == b'\t' || s[k] == b'\n' || s[k] == b'\r') {
                    k += 1;
                }
                if k < s.len() && s[k] == b':' && s.get(k + 1) != Some(&b':') {
                    names.push(String::from_utf8_lossy(&s[i..j]).into_owned());
                    i = k + 1;
                    continue;
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    names
}

/// The full 3D scenes' ranges (a `Scene3D{..}` with fields beyond Motion's
/// own layer's): their code is scene code.
fn scene_ranges(text: &str) -> Vec<(usize, usize)> {
    let s = text.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("Scene3D").map(|k| from + k) {
        from = at + 1;
        if at > 0 && (s[at - 1].is_ascii_alphanumeric() || s[at - 1] == b'_' || s[at - 1] == b'.') {
            continue;
        }
        let mut o = at + "Scene3D".len();
        while o < s.len() && (s[o] == b' ' || s[o] == b'\t' || s[o] == b'\n') {
            o += 1;
        }
        if o >= s.len() || s[o] != b'{' {
            continue;
        }
        let c = match_close(s, o);
        let fields = top_fields(s, o, c);
        if fields.iter().any(|f| !SCENE_KEEP.contains(&f.as_str()) && !SCENE_SIMPLE.contains(&f.as_str())) {
            out.push((at, c + 1));
        }
    }
    out
}

/// `Name {` written as a value (not a field or a path's tail).
fn has_object(code: &str, name: &str) -> bool {
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(at) = code[from..].find(name).map(|k| from + k) {
        from = at + 1;
        if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_' || b[at - 1] == b'.') {
            continue;
        }
        let after = &code[at + name.len()..];
        if after.trim_start().starts_with('{') && !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            return true;
        }
    }
    false
}
