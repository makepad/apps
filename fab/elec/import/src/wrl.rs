//! VRML97 faces, with shared node identities and located refusals for geometry we cannot interpret.
//!
//! Parsing is a forward scan. Nodes live in a flat arena so USE does not copy subtrees, and dropping
//! a document cannot recurse. Walking costs the expanded instances and their faces, in addition to
//! the source size; coordinate and normal arrays are decoded once even when several shapes use them.

use std::collections::HashMap;
use std::rc::Rc;

use crate::mesh::srgb_to_linear;
use crate::{Affine, Issue, Material, MeshBuilder, MeshRead, Normals};

const MAX_DEPTH: usize = 256;

/// Reads a VRML 2.0 (VRML97) file's faces with their materials and transforms, in the file's units.
pub fn read_wrl(bytes: &[u8]) -> Result<MeshRead, Issue> {
    let first = bytes.split(|&b| b == b'\n' || b == b'\r').next().unwrap_or_default();
    if first.starts_with(b"#VRML V1.0") {
        return Err(Issue::new(1, "#VRML", "VRML 1.0 is not read"));
    }
    if !first.starts_with(b"#VRML V2.0") {
        return Err(Issue::new(1, "#VRML", "not a VRML 2.0 file"));
    }
    // Keywords and numbers are ASCII; a vendor's comment or name in another encoding must not refuse the file.
    let text = String::from_utf8_lossy(bytes);
    let source: &str = &text;
    let (tokens, end_line) = tokenize(source);
    let mut parser = Parser {
        tokens, at: 0, end_line, nodes: Vec::new(), definitions: HashMap::new(),
        warnings: Warnings::default(), material_count: 0,
    };
    let mut roots = Vec::new();
    while parser.at < parser.tokens.len() {
        match parser.ident() {
            Some("PROTO" | "EXTERNPROTO") => parser.prototype()?,
            Some("ROUTE") => parser.route()?,
            _ => {
                if let Some(id) = parser.node(1, true)? {
                    roots.push(id);
                }
            }
        }
    }
    let mut reader = Reader {
        nodes: &parser.nodes, builder: MeshBuilder::new(), materials: HashMap::new(),
        arrays: HashMap::new(), default_material: None, domain: 0,
    };
    reader.walk(&roots)?;
    if reader.builder.degenerate > 0 {
        parser.warnings.push(Issue::new(0, "IndexedFaceSet",
            format!("{} faces with no area are left out", reader.builder.degenerate)));
    }
    if reader.builder.crossing > 0 {
        parser.warnings.push(Issue::new(0, "IndexedFaceSet",
            format!("{} faces cross themselves; filled as fans", reader.builder.crossing)));
    }
    let mesh = reader.builder.finish();
    if mesh.is_empty() {
        parser.warnings.push(Issue::new(1, "VRML", "the file has no faces"));
    }
    Ok(MeshRead { mesh, warnings: parser.warnings.finish() })
}

enum Kind<'a> {
    Ident(&'a str),
    Number(f64),
    Text(String),
    Punct(u8),
    Invalid(&'a str, &'static str),
}

struct Token<'a> {
    kind: Kind<'a>,
    line: u32,
}

fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b',')
}

fn tokenize(source: &str) -> (Vec<Token<'_>>, u32) {
    let bytes = source.as_bytes();
    let (mut at, mut line) = (0, 1u32);
    let mut out = Vec::new();
    while let Some(&b) = bytes.get(at) {
        if space(b) {
            if b == b'\n' || (b == b'\r' && bytes.get(at + 1) != Some(&b'\n')) {
                line = line.saturating_add(1);
            }
            at += 1;
        } else if b == b'#' {
            while bytes.get(at).is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
                at += 1;
            }
        } else if matches!(b, b'{' | b'}' | b'[' | b']') {
            out.push(Token { kind: Kind::Punct(b), line });
            at += 1;
        } else if b == b'"' {
            let start_line = line;
            at += 1;
            let mut start = at;
            let mut text = String::new();
            loop {
                match bytes.get(at) {
                    None => {
                        out.push(Token { kind: Kind::Invalid("string", "unterminated string"), line: start_line });
                        return (out, line);
                    }
                    Some(b'"') => {
                        text.push_str(&source[start..at]);
                        at += 1;
                        break;
                    }
                    Some(b'\\') if matches!(bytes.get(at + 1), Some(b'"' | b'\\')) => {
                        text.push_str(&source[start..at]);
                        at += 1;
                        // The escape guard established this byte and its UTF-8 boundary.
                        text.push(bytes[at] as char);
                        at += 1;
                        start = at;
                    }
                    Some(&b) => {
                        if b == b'\n' || (b == b'\r' && bytes.get(at + 1) != Some(&b'\n')) {
                            line = line.saturating_add(1);
                        }
                        at += 1;
                    }
                }
            }
            out.push(Token { kind: Kind::Text(text), line: start_line });
        } else {
            let start = at;
            while bytes.get(at).is_some_and(|&b| !space(b) && !matches!(b, b'{' | b'}' | b'[' | b']' | b'#' | b'"')) {
                at += 1;
            }
            let word = &source[start..at];
            let kind = match number(word) {
                Some(n) if n.is_finite() => Kind::Number(n),
                Some(_) => Kind::Invalid(word, "number is not finite"),
                None => Kind::Ident(word),
            };
            out.push(Token { kind, line });
        }
    }
    (out, line)
}

/// Recognises numeric spelling before parsing, leaving all other words as identifiers.
fn number(word: &str) -> Option<f64> {
    let bytes = word.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    if bytes.get(at..at + 2).is_some_and(|s| s == b"0x" || s == b"0X") {
        at += 2;
        let digits = bytes.get(at..)?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let mut n = 0.0;
        for &b in digits {
            let d = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                _ => b - b'A' + 10,
            };
            n = n * 16.0 + f64::from(d);
        }
        return Some(if bytes.first() == Some(&b'-') { -n } else { n });
    }
    let mut digits = false;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
        digits = true;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            digits = true;
        }
    }
    if !digits {
        return None;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    if at != bytes.len() {
        return None;
    }
    Some(match word.parse::<f64>() { Ok(n) => n, Err(_) => f64::INFINITY })
}

enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    Node(Option<usize>),
}

struct Field<'a> {
    name: &'a str,
    key: String,
    line: u32,
    values: Vec<Value>,
}

impl Field<'_> {
    fn issue(&self, message: impl Into<String>) -> Issue {
        Issue::new(self.line, &self.key, message)
    }

    fn number(&self, at: usize) -> Result<f64, Issue> {
        match self.values.get(at) {
            Some(Value::Number(n)) => Ok(*n),
            _ => Err(self.issue("expected a number")),
        }
    }

    fn node(&self) -> Result<Option<usize>, Issue> {
        match self.values.as_slice() {
            [Value::Node(id)] => Ok(*id),
            _ => Err(self.issue("expected one node or NULL")),
        }
    }
}

struct Node<'a> {
    kind: &'a str,
    name: Option<&'a str>,
    line: u32,
    fields: Vec<Field<'a>>,
    material_number: usize,
}

impl Node<'_> {
    fn field(&self, name: &str) -> Option<&Field<'_>> {
        self.fields.iter().rev().find(|f| f.name == name)
    }

    fn numbers<const N: usize>(&self, name: &str, default: [f64; N]) -> Result<[f64; N], Issue> {
        let Some(f) = self.field(name) else { return Ok(default) };
        if f.values.len() != N {
            return Err(f.issue(format!("expected {N} numbers")));
        }
        let mut out = [0.0; N];
        for (i, n) in out.iter_mut().enumerate() {
            *n = f.number(i)?;
        }
        Ok(out)
    }

    fn boolean(&self, name: &str, default: bool) -> Result<bool, Issue> {
        let Some(f) = self.field(name) else { return Ok(default) };
        match f.values.as_slice() {
            [Value::Bool(b)] => Ok(*b),
            _ => Err(f.issue("expected TRUE or FALSE")),
        }
    }

    fn child(&self, name: &str) -> Result<Option<usize>, Issue> {
        self.field(name).map_or(Ok(None), Field::node)
    }
}

#[derive(Default)]
struct Warnings {
    entries: Vec<(Issue, Option<usize>)>,
    keys: HashMap<String, usize>,
}

impl Warnings {
    fn push(&mut self, issue: Issue) -> usize {
        let at = self.entries.len();
        self.entries.push((issue, None));
        at
    }

    fn note(&mut self, key: String, issue: Issue, counted: bool) {
        if let Some(&at) = self.keys.get(&key) {
            if let Some(count) = &mut self.entries[at].1 {
                *count += 1;
            }
        } else {
            self.keys.insert(key, self.entries.len());
            self.entries.push((issue, counted.then_some(1)));
        }
    }

    fn finish(self) -> Vec<Issue> {
        self.entries.into_iter().map(|(mut issue, count)| {
            if let Some(count) = count {
                let plural = if count == 1 { "" } else { "s" };
                issue.message.push_str(&format!(" ({count} occurrence{plural})"));
            }
            issue
        }).collect()
    }
}

fn omitted(kind: &str) -> bool {
    matches!(kind, "IndexedLineSet" | "PointSet" | "Box" | "Cone" | "Cylinder" | "Sphere"
        | "ElevationGrid" | "Extrusion" | "Text")
}

fn fields(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "Transform" => &["translation", "rotation", "scale", "scaleOrientation", "center", "children",
            "bboxCenter", "bboxSize"],
        "Group" => &["children", "bboxCenter", "bboxSize"],
        "Anchor" => &["children", "description", "parameter", "url", "bboxCenter", "bboxSize"],
        "Collision" => &["children", "collide", "proxy", "bboxCenter", "bboxSize"],
        "Billboard" => &["children", "axisOfRotation", "bboxCenter", "bboxSize"],
        "Switch" => &["choice", "whichChoice"],
        "LOD" => &["level", "center", "range"],
        "Inline" => &["url", "bboxCenter", "bboxSize"],
        "Shape" => &["appearance", "geometry"],
        "Appearance" => &["material", "texture", "textureTransform"],
        "Material" => &["diffuseColor", "emissiveColor", "transparency", "shininess", "specularColor", "ambientIntensity"],
        "IndexedFaceSet" => &["coord", "coordIndex", "normal", "normalIndex", "normalPerVertex", "ccw", "solid",
            "convex", "creaseAngle", "color", "colorIndex", "colorPerVertex", "texCoord", "texCoordIndex"],
        "Coordinate" => &["point"],
        "Normal" => &["vector"],
        _ => return None,
    })
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    at: usize,
    end_line: u32,
    nodes: Vec<Node<'a>>,
    definitions: HashMap<&'a str, usize>,
    warnings: Warnings,
    material_count: usize,
}

impl<'a> Parser<'a> {
    fn line(&self) -> u32 {
        self.tokens.get(self.at).map_or(self.end_line, |t| t.line)
    }

    fn ident(&self) -> Option<&'a str> {
        match self.tokens.get(self.at)?.kind {
            Kind::Ident(s) => Some(s),
            _ => None,
        }
    }

    fn error(&self, field: &str, message: &str) -> Issue {
        if let Some(Token { kind: Kind::Invalid(token, reason), line }) = self.tokens.get(self.at) {
            Issue::new(*line, if field == "node" { *token } else { field }, *reason)
        } else {
            Issue::new(self.line(), field, message)
        }
    }

    fn word(&mut self, field: &str) -> Result<&'a str, Issue> {
        let s = self.ident().ok_or_else(|| self.error(field, "expected an identifier"))?;
        self.at += 1;
        Ok(s)
    }

    fn punct(&self, p: u8) -> bool {
        matches!(self.tokens.get(self.at), Some(Token { kind: Kind::Punct(b), .. }) if *b == p)
    }

    fn expect(&mut self, p: u8, field: &str) -> Result<(), Issue> {
        if !self.punct(p) {
            return Err(self.error(field, &format!("expected '{}'", p as char)));
        }
        self.at += 1;
        Ok(())
    }

    fn note_node(&mut self, kind: &str, line: u32, report: bool) -> bool {
        let known = fields(kind).is_some();
        if report {
            if omitted(kind) || !known {
                let message = if omitted(kind) { "geometry is left out" } else { "node is left out" };
                self.warnings.note(format!("node:{kind}"), Issue::new(line, kind, format!("{kind} {message}")), true);
            } else if kind == "LOD" {
                self.warnings.note("LOD".into(), Issue::new(line, kind, "LOD: only the first level is read"), false);
            }
        }
        report && known
    }

    fn node(&mut self, depth: usize, report: bool) -> Result<Option<usize>, Issue> {
        let line = self.line();
        let mut kind = self.word("node")?;
        if depth > MAX_DEPTH {
            return Err(Issue::new(line, kind, "nesting deeper than 256 nodes"));
        }
        if kind == "NULL" {
            return Ok(None);
        }
        if kind == "USE" {
            let name = self.word("USE")?;
            let id = *self.definitions.get(name)
                .ok_or_else(|| Issue::new(line, "USE", format!("undefined USE {name}")))?;
            let node_kind = self.nodes[id].kind;
            self.note_node(node_kind, line, report);
            if report && node_kind == "Inline" {
                let message = inline_message(&self.nodes[id]);
                self.warnings.push(Issue::new(line, "Inline", message));
            }
            return Ok(Some(id));
        }
        let name = if kind == "DEF" {
            let name = self.word("DEF")?;
            kind = self.word("DEF")?;
            Some(name)
        } else {
            None
        };
        self.expect(b'{', kind)?;
        let report_fields = self.note_node(kind, line, report);
        let inline_warning = (report && kind == "Inline")
            .then(|| self.warnings.push(Issue::new(line, "Inline", "Inline is left out")));
        if kind == "Material" {
            self.material_count += 1;
        }
        let id = self.nodes.len();
        self.nodes.push(Node { kind, name, line, fields: Vec::new(), material_number: self.material_count });
        if let Some(name) = name {
            self.definitions.insert(name, id);
        }
        if fields(kind).is_none() {
            self.balanced_body(b'{', kind, depth)?;
            return Ok(Some(id));
        }
        let mut node_fields = Vec::new();
        while !self.punct(b'}') {
            if self.at == self.tokens.len() {
                return Err(Issue::new(self.line(), kind, "expected '}'"));
            }
            let field_line = self.line();
            let field_name = self.word(kind)?;
            let key = format!("{kind}.{field_name}");
            let known = fields(kind).is_some_and(|names| names.contains(&field_name));
            if report_fields && !known {
                self.warnings.note(format!("field:{key}"),
                    Issue::new(field_line, &key, "unknown field is left out"), true);
            }
            let texture = kind == "Appearance" && field_name == "texture";
            let color = kind == "IndexedFaceSet" && matches!(field_name, "color" | "colorIndex");
            if report_fields && texture && self.ident() != Some("NULL") {
                self.warnings.note("texture".into(),
                    Issue::new(field_line, &key, "textures are left out; the colours are kept"), false);
            }
            if report_fields && color {
                self.warnings.note("color".into(), Issue::new(field_line, &key, "vertex colours are left out"), false);
            }
            let ignored = texture || color || (kind == "Appearance" && field_name == "textureTransform")
                || (kind == "IndexedFaceSet" && field_name == "texCoord");
            let values = if self.ident() == Some("IS") {
                self.at += 1;
                self.word(&key)?;
                Vec::new()
            } else {
                self.value(depth, report_fields && known && !ignored, &key)?
            };
            node_fields.push(Field { name: field_name, key, line: field_line, values });
        }
        self.at += 1;
        self.nodes[id].fields = node_fields;
        if report_fields && matches!(kind, "Coordinate" | "Normal") {
            let name = if kind == "Coordinate" { "point" } else { "vector" };
            if let Some(field) = self.nodes[id].field(name) {
                if field.values.len() % 3 != 0 {
                    return Err(field.issue("expected a multiple of 3 numbers"));
                }
                for i in 0..field.values.len() {
                    field.number(i)?;
                }
            }
        }
        if let Some(at) = inline_warning {
            self.warnings.entries[at].0.message = inline_message(&self.nodes[id]);
        }
        Ok(Some(id))
    }

    fn scalar(&mut self, key: &str) -> Result<Option<Value>, Issue> {
        let Some(token) = self.tokens.get(self.at) else { return Ok(None) };
        let value = match &token.kind {
            Kind::Number(n) => Value::Number(*n),
            Kind::Text(s) => Value::Text(s.clone()),
            Kind::Ident("TRUE") => Value::Bool(true),
            Kind::Ident("FALSE") => Value::Bool(false),
            Kind::Invalid(_, _) => return Err(self.error(key, "invalid token")),
            _ => return Ok(None),
        };
        self.at += 1;
        Ok(Some(value))
    }

    fn value(&mut self, depth: usize, report: bool, key: &str) -> Result<Vec<Value>, Issue> {
        let mut values = Vec::new();
        if self.punct(b'[') {
            self.at += 1;
            while !self.punct(b']') {
                if self.at == self.tokens.len() || self.punct(b'}') {
                    return Err(Issue::new(self.line(), key, "expected ']'"));
                }
                if let Some(v) = self.scalar(key)? {
                    values.push(v);
                } else {
                    values.push(Value::Node(self.node(depth + 1, report)?));
                }
            }
            self.at += 1;
        } else if let Some(v) = self.scalar(key)? {
            values.push(v);
            while let Some(v) = self.scalar(key)? {
                values.push(v);
            }
        } else if self.ident().is_some() {
            values.push(Value::Node(self.node(depth + 1, report)?));
        } else {
            return Err(Issue::new(self.line(), key, "expected a value"));
        }
        Ok(values)
    }

    /// Skipped declarations still check delimiters and depth, without interpreting interface types.
    fn balanced(&mut self, open: u8, key: &str) -> Result<(), Issue> {
        self.expect(open, key)?;
        self.balanced_body(open, key, 0)
    }

    fn balanced_body(&mut self, open: u8, key: &str, depth: usize) -> Result<(), Issue> {
        let mut stack = vec![if open == b'[' { b']' } else { b'}' }];
        let mut nodes = depth;
        while let Some(&close) = stack.last() {
            let token = self.tokens.get(self.at)
                .ok_or_else(|| Issue::new(self.line(), key, format!("expected '{}'", close as char)))?;
            match token.kind {
                Kind::Punct(p @ (b'{' | b'[')) => {
                    nodes += usize::from(p == b'{');
                    if nodes > MAX_DEPTH {
                        return Err(Issue::new(token.line, key, "nesting deeper than 256 nodes"));
                    }
                    stack.push(if p == b'{' { b'}' } else { b']' });
                }
                Kind::Punct(p @ (b'}' | b']')) => {
                    if p != close {
                        return Err(Issue::new(token.line, key, format!("expected '{}'", close as char)));
                    }
                    nodes -= usize::from(p == b'}' && stack.len() > 1);
                    stack.pop();
                }
                Kind::Invalid(_, _) => return Err(self.error(key, "invalid token")),
                _ => {}
            }
            self.at += 1;
        }
        Ok(())
    }

    fn prototype(&mut self) -> Result<(), Issue> {
        let line = self.line();
        let keyword = self.word("PROTO")?;
        let name = self.word(keyword)?;
        self.warnings.push(Issue::new(line, keyword, format!("PROTO {name} is left out")));
        self.balanced(b'[', keyword)?;
        if keyword == "PROTO" {
            self.balanced(b'{', keyword)
        } else if self.punct(b'[') {
            self.balanced(b'[', keyword)
        } else if matches!(self.tokens.get(self.at), Some(Token { kind: Kind::Text(_), .. })) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.error(keyword, "expected a URL string or list"))
        }
    }

    fn route(&mut self) -> Result<(), Issue> {
        self.at += 1;
        self.word("ROUTE")?;
        if self.ident() != Some("TO") {
            return Err(self.error("ROUTE", "expected TO"));
        }
        self.at += 1;
        self.word("ROUTE")?;
        Ok(())
    }
}

fn inline_message(node: &Node<'_>) -> String {
    let url = node.field("url").and_then(|f| f.values.iter().find_map(|v| {
        if let Value::Text(s) = v { Some(s.as_str()) } else { None }
    })).unwrap_or("");
    format!("Inline {url} is left out")
}

struct Reader<'a> {
    nodes: &'a [Node<'a>],
    builder: MeshBuilder,
    materials: HashMap<usize, usize>,
    arrays: HashMap<usize, Rc<[[f32; 3]]>>,
    default_material: Option<usize>,
    domain: u32,
}

impl Reader<'_> {
    fn walk(&mut self, roots: &[usize]) -> Result<(), Issue> {
        let mut stack: Vec<_> = roots.iter().rev().map(|&id| (id, Affine::IDENTITY, 1)).collect();
        while let Some((id, parent, depth)) = stack.pop() {
            let node = &self.nodes[id];
            if depth > MAX_DEPTH {
                return Err(Issue::new(node.line, node.kind, "nesting deeper than 256 nodes"));
            }
            let (children, transform, choice) = match node.kind {
                "Transform" => ("children", transform(node, &parent)?, None),
                "Group" | "Anchor" | "Collision" | "Billboard" => ("children", parent, None),
                "Switch" => {
                    let [choice] = node.numbers("whichChoice", [-1.0])?;
                    if choice.fract() != 0.0 || choice < -1.0 {
                        let line = node.field("whichChoice").map_or(node.line, |f| f.line);
                        return Err(Issue::new(line, "Switch.whichChoice", "expected an integer at least -1"));
                    }
                    if choice < 0.0 { continue }
                    ("choice", parent, Some(choice as usize))
                }
                "LOD" => ("level", parent, Some(0)),
                "Shape" => {
                    self.shape(id, &parent)?;
                    continue;
                }
                _ => continue,
            };
            if let Some(field) = node.field(children) {
                for (i, value) in field.values.iter().enumerate().rev() {
                    let Value::Node(child) = value else { return Err(field.issue("expected nodes")) };
                    if choice.is_none_or(|choice| choice == i) {
                        if let Some(child) = child {
                            stack.push((*child, transform, depth + 1));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn shape(&mut self, id: usize, transform: &Affine) -> Result<(), Issue> {
        let node = &self.nodes[id];
        let Some(geometry) = node.child("geometry")? else { return Ok(()) };
        if self.nodes[geometry].kind != "IndexedFaceSet" {
            return Ok(());
        }
        let appearance = node.child("appearance")?;
        let material = if let Some(appearance) = appearance {
            let appearance = &self.nodes[appearance];
            if appearance.kind != "Appearance" {
                return Err(node.field("appearance").map_or_else(
                    || Issue::new(node.line, "Shape.appearance", "expected Appearance"),
                    |f| f.issue("expected Appearance")));
            }
            appearance.child("material")?
        } else {
            None
        };
        let material = self.material(material)?;
        self.faces(geometry, material, transform)
    }

    fn material(&mut self, id: Option<usize>) -> Result<usize, Issue> {
        let Some(id) = id else {
            return Ok(*self.default_material.get_or_insert_with(|| self.builder.add_material(Material::named("default"))));
        };
        if let Some(&material) = self.materials.get(&id) {
            return Ok(material);
        }
        let node = &self.nodes[id];
        if node.kind != "Material" {
            return Err(Issue::new(node.line, "Appearance.material", "expected Material"));
        }
        let diffuse = node.numbers("diffuseColor", [0.8; 3])?;
        let emissive = node.numbers("emissiveColor", [0.0; 3])?;
        let [transparency] = node.numbers("transparency", [0.0])?;
        let [shininess] = node.numbers("shininess", [0.2])?;
        let specular = node.numbers("specularColor", [0.0; 3])?;
        node.numbers("ambientIntensity", [0.2])?;
        let factor = |v: f64| v.clamp(0.0, 1.0) as f32;
        let colour = |v: f64| srgb_to_linear(factor(v));
        let base = diffuse.map(colour);
        let specular = node.field("specularColor").map(|_| specular.map(colour));
        let name = node.name.map(str::to_owned).unwrap_or_else(|| format!("material {}", node.material_number));
        let material = self.builder.add_material(Material {
            name, base_color: [base[0], base[1], base[2], factor(1.0 - transparency)],
            specular, emissive: emissive.map(colour), roughness: factor(1.0 - shininess), metallic: 0.0,
        });
        self.materials.insert(id, material);
        Ok(material)
    }

    fn array(&mut self, id: usize, kind: &str, name: &str, owner: &Field<'_>) -> Result<Rc<[[f32; 3]]>, Issue> {
        let node = &self.nodes[id];
        if node.kind != kind {
            return Err(owner.issue(format!("expected {kind}")));
        }
        if let Some(array) = self.arrays.get(&id) {
            return Ok(Rc::clone(array));
        }
        let mut out = Vec::new();
        if let Some(field) = node.field(name) {
            if field.values.len() % 3 != 0 {
                return Err(field.issue("expected a multiple of 3 numbers"));
            }
            out.reserve(field.values.len() / 3);
            for start in (0..field.values.len()).step_by(3) {
                let mut triple = [0.0; 3];
                for (k, value) in triple.iter_mut().enumerate() {
                    *value = field.number(start + k)? as f32;
                    if !value.is_finite() {
                        return Err(field.issue("number is outside the mesh's finite range"));
                    }
                }
                out.push(triple);
            }
        }
        let out: Rc<[[f32; 3]]> = out.into();
        self.arrays.insert(id, Rc::clone(&out));
        Ok(out)
    }

    fn faces(&mut self, id: usize, material: usize, transform: &Affine) -> Result<(), Issue> {
        let node = &self.nodes[id];
        let domain = self.domain;
        self.domain = self.domain.checked_add(1)
            .ok_or_else(|| Issue::new(node.line, "IndexedFaceSet", "too many normal domains"))?;
        let Some(indices) = node.field("coordIndex") else { return Ok(()) };
        let points = if let Some(coord) = node.field("coord") {
            if let Some(id) = coord.node()? { self.array(id, "Coordinate", "point", coord)? } else { Rc::from([]) }
        } else {
            Rc::from([])
        };
        let normal_field = node.field("normal");
        let normals = if let Some(field) = normal_field {
            if let Some(id) = field.node()? { Some(self.array(id, "Normal", "vector", field)?) } else { None }
        } else {
            None
        };
        let normal_indices = node.field("normalIndex");
        let per_vertex = node.boolean("normalPerVertex", true)?;
        let reverse = !node.boolean("ccw", true)? ^ (transform.det() < 0.0);
        let [angle] = node.numbers("creaseAngle", [0.0])?;
        let angle = angle.clamp(0.0, std::f64::consts::PI) as f32;
        let mut corners = Vec::new();
        let mut given = Vec::new();
        let mut face = 0;
        for k in 0..indices.values.len() {
            let raw = indices.number(k)?;
            if raw == -1.0 {
                if normals.is_some() && per_vertex {
                    if let Some(field) = normal_indices {
                        if field.values.get(k).is_none() {
                            return Err(field.issue("normalIndex is shorter than needed"));
                        }
                        if field.number(k)? != -1.0 {
                            return Err(field.issue("normalIndex must have the same face separators as coordIndex"));
                        }
                    }
                }
                if !corners.is_empty() {
                    self.polygon(material, &mut corners, &mut given, normals.is_some(), reverse, domain, angle);
                }
                face += 1;
                continue;
            }
            let point_index = index(raw, points.len(), indices, "points")?;
            let point = transform.point(points[point_index]);
            if !point.iter().all(|v| v.is_finite()) {
                return Err(indices.issue("transformed point is outside the mesh's finite range"));
            }
            corners.push(point);
            if let Some(normals) = &normals {
                let field = normal_indices.or(normal_field)
                    .ok_or_else(|| indices.issue("missing normal field"))?;
                let raw = if let Some(indices) = normal_indices {
                    let at = if per_vertex { k } else { face };
                    if indices.values.get(at).is_none() {
                        return Err(indices.issue("normalIndex is shorter than needed"));
                    }
                    indices.number(at)?
                } else if per_vertex {
                    raw
                } else {
                    face as f64
                };
                let normal = index(raw, normals.len(), field, "vectors")?;
                given.push(transform.normal(normals[normal]));
            }
        }
        if !corners.is_empty() {
            self.polygon(material, &mut corners, &mut given, normals.is_some(), reverse, domain, angle);
        }
        Ok(())
    }

    fn polygon(&mut self, material: usize, corners: &mut Vec<[f32; 3]>, given: &mut Vec<[f32; 3]>,
        has_normals: bool, reverse: bool, domain: u32, angle: f32)
    {
        if reverse {
            corners.reverse();
            given.reverse();
        }
        let normals = if has_normals { Normals::Given(given) } else { Normals::Crease { domain, angle } };
        self.builder.polygon(material, corners, normals);
        corners.clear();
        given.clear();
    }
}

fn index(raw: f64, len: usize, field: &Field<'_>, what: &str) -> Result<usize, Issue> {
    if raw < 0.0 || raw.fract() != 0.0 || raw >= len as f64 {
        return Err(field.issue(format!("index {raw} is outside the {len} {what}")));
    }
    let index = raw as usize;
    if index >= len {
        return Err(field.issue(format!("index {raw} is outside the {len} {what}")));
    }
    Ok(index)
}

fn transform(node: &Node<'_>, parent: &Affine) -> Result<Affine, Issue> {
    let translation = node.numbers("translation", [0.0; 3])?;
    let center = node.numbers("center", [0.0; 3])?;
    let [x, y, z, angle] = node.numbers("rotation", [0.0, 0.0, 1.0, 0.0])?;
    let [sx, sy, sz, sa] = node.numbers("scaleOrientation", [0.0, 0.0, 1.0, 0.0])?;
    let scale = node.numbers("scale", [1.0; 3])?;
    let local = Affine::translate(translation)
        .mul(&Affine::translate(center))
        .mul(&rotation([x, y, z], angle))
        .mul(&rotation([sx, sy, sz], sa))
        .mul(&Affine::scale(scale))
        .mul(&rotation([sx, sy, sz], -sa))
        .mul(&Affine::translate(center.map(|v| -v)));
    let result = parent.mul(&local);
    if !result.m.iter().flatten().all(|v| v.is_finite()) || !result.det().is_finite() {
        return Err(Issue::new(node.line, "Transform", "transform is not finite"));
    }
    Ok(result)
}

/// Rescaling the axis avoids overflow or underflow in its squared length without changing rotation.
fn rotation(axis: [f64; 3], angle: f64) -> Affine {
    let largest = axis.iter().fold(0.0f64, |largest, v| largest.max(v.abs()));
    let axis = if largest > 0.0 { axis.map(|v| v / largest) } else { axis };
    Affine::rotate(axis, angle)
}
