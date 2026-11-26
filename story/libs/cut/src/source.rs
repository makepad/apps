//! A small lexical view of a cut's Splash: where the final `Flow{...}`
//! is, where a `let id = Type{...}` constructor is, and where each field of
//! an object literal starts and ends. Strings and comments are skipped;
//! nothing is evaluated. The patchers use these spans to change exactly one
//! value and leave every other byte as it was written.

/// One entry of an object literal at its own depth: `name: value`, or a
/// bare membership entry (`intro,` in `Flow{...}`) with no name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: Option<String>,
    /// Where the entry starts (its name, or the bare value).
    pub start: usize,
    /// Where its value starts and ends (exclusive, trailing space trimmed).
    pub value: (usize, usize),
}

pub fn ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

pub fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The index after a string or comment starting at `index`, or `None` when
/// none starts there. An unterminated one runs to the end.
pub fn skip_literal(bytes: &[u8], index: usize) -> Option<usize> {
    match bytes[index] {
        b'"' => {
            let mut at = index + 1;
            while at < bytes.len() {
                match bytes[at] {
                    b'\\' => at += 2,
                    b'"' => return Some(at + 1),
                    _ => at += 1,
                }
            }
            Some(bytes.len())
        }
        b'/' if bytes.get(index + 1) == Some(&b'/') => {
            Some(bytes[index..].iter().position(|&byte| byte == b'\n').map_or(bytes.len(), |at| index + at))
        }
        b'/' if bytes.get(index + 1) == Some(&b'*') => {
            let rest = &bytes[index + 2..];
            Some(rest.windows(2).position(|pair| pair == b"*/").map_or(bytes.len(), |at| index + 2 + at + 2))
        }
        _ => None,
    }
}

/// The index of the bracket closing the one at `open` (`{`, `[` or `(`).
pub fn matching(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Top-level word starts (outside every bracket, string and comment).
pub fn top_level_words(source: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut words = Vec::new();
    let (mut depth, mut index) = (0usize, 0usize);
    while index < bytes.len() {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            byte if depth == 0 && ident_start(byte) && (index == 0 || !(ident_byte(bytes[index - 1]) || bytes[index - 1] == b'.')) => {
                words.push(index);
                while index < bytes.len() && ident_byte(bytes[index]) {
                    index += 1;
                }
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    words
}

pub fn word_at(source: &str, at: usize) -> &str {
    let end = source[at..].bytes().position(|byte| !ident_byte(byte)).map_or(source.len(), |len| at + len);
    &source[at..end]
}

pub fn skip_space(source: &str, mut at: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        match (at < bytes.len()).then(|| skip_literal(bytes, at)).flatten() {
            Some(next) if bytes[at] == b'/' => at = next,
            _ => return at,
        }
    }
}

/// `(open, close)` of the final top-level `Flow{...}` (or `mod.flow.Flow{`).
pub fn final_flow(source: &str) -> Option<(usize, usize)> {
    let mut found = None;
    for at in top_level_words(source) {
        let word = word_at(source, at);
        let head_end = if word == "Flow" {
            at + 4
        } else if word == "mod" && source[at..].starts_with("mod.flow.Flow") {
            at + "mod.flow.Flow".len()
        } else {
            continue;
        };
        let open = skip_space(source, head_end);
        if source.as_bytes().get(open) == Some(&b'{') {
            if let Some(close) = matching(source, open) {
                found = Some((open, close));
            }
        }
    }
    found
}

/// `(open, close)` of the object literal node `id` is constructed with:
/// its unique top-level `let id = Type{...}`.
pub fn constructor(source: &str, id: &str) -> Result<(usize, usize), String> {
    let mut found = None;
    for at in top_level_words(source) {
        if word_at(source, at) != "let" {
            continue;
        }
        let name = skip_space(source, at + 3);
        if word_at(source, name) != id {
            continue;
        }
        let eq = skip_space(source, name + id.len());
        if source.as_bytes().get(eq) != Some(&b'=') || source.as_bytes().get(eq + 1) == Some(&b'=') {
            continue;
        }
        let head = skip_space(source, eq + 1);
        // `Type{` or `mod.cut.Type{`: a plain path, then the object.
        let path_end = source[head..].bytes().position(|byte| !(ident_byte(byte) || byte == b'.')).map_or(source.len(), |len| head + len);
        let open = skip_space(source, path_end);
        if path_end == head || source.as_bytes().get(open) != Some(&b'{') {
            return Err(format!("`{id}` is not constructed as `let {id} = Type{{...}}`; edit its source"));
        }
        let close = matching(source, open).ok_or_else(|| format!("`{id}`'s constructor is not closed"))?;
        if found.replace((open, close)).is_some() {
            return Err(format!("`{id}` is bound more than once; edit its source"));
        }
    }
    found.ok_or_else(|| format!("no `let {id} = ...` in the cut"))
}

/// The entries of the object literal `(open, close)`, at its own depth.
pub fn fields(source: &str, (open, close): (usize, usize)) -> Vec<Field> {
    let bytes = source.as_bytes();
    let mut fields = Vec::new();
    let mut at = open + 1;
    loop {
        at = skip_space(source, at);
        while at < close && bytes[at] == b',' {
            at = skip_space(source, at + 1);
        }
        if at >= close {
            break;
        }
        let start = at;
        // `name:` (not `name::`) names a field; anything else is a bare entry.
        let mut name = None;
        if ident_start(bytes[at]) {
            let word = word_at(source, at);
            let colon = skip_space(source, at + word.len());
            if bytes.get(colon) == Some(&b':') && bytes.get(colon + 1) != Some(&b':') {
                name = Some(word.to_string());
                at = skip_space(source, colon + 1);
            }
        }
        let value_start = at;
        let end = value_end(source, value_start, close);
        fields.push(Field { name, start, value: (value_start, end) });
        if end <= at {
            // Nothing consumed: step over the stray byte.
            at += 1;
        } else {
            at = end;
        }
    }
    fields
}

/// Where the value starting at `start` ends: at depth 0, a `,`, the
/// object's close, or whitespace followed by the next entry's word.
fn value_end(source: &str, start: usize, close: usize) -> usize {
    let bytes = source.as_bytes();
    let (mut depth, mut index, mut last) = (0usize, start, start);
    while index < close {
        if let Some(next) = skip_literal(bytes, index) {
            if bytes[index] == b'"' {
                last = next;
            }
            index = next;
            continue;
        }
        let byte = bytes[index];
        match byte {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => return last,
            _ if depth == 0 && byte.is_ascii_whitespace() => {
                let next = skip_space(source, index);
                if next >= close || bytes[next] == b',' || ident_start(bytes[next]) || bytes[next] == b'"' || bytes[next] == b'@' {
                    return last;
                }
                index = next;
                continue;
            }
            _ => {}
        }
        index += 1;
        if !byte.is_ascii_whitespace() {
            last = index;
        }
    }
    last
}

/// The written text of `field` in the object `(open, close)`.
pub fn field_text<'a>(source: &'a str, object: (usize, usize), field: &str) -> Option<&'a str> {
    fields(source, object).into_iter().find(|entry| entry.name.as_deref() == Some(field)).map(|entry| &source[entry.value.0..entry.value.1])
}

/// Replace the value of `field` in the object `(open, close)` with `value`,
/// or add `field: value` before the object closes. Only those bytes change.
pub fn set_field(source: &str, object: (usize, usize), field: &str, value: &str) -> String {
    let entries = fields(source, object);
    let mut written = source.to_string();
    if let Some(entry) = entries.iter().find(|entry| entry.name.as_deref() == Some(field)) {
        written.replace_range(entry.value.0..entry.value.1, value);
        return written;
    }
    let (open, close) = object;
    let line_start = source[..close].rfind('\n').map_or(0, |at| at + 1);
    let multiline = source[open..close].contains('\n');
    if multiline && line_start > open && source[line_start..close].bytes().all(|byte| byte == b' ' || byte == b'\t') {
        // A closing brace on its own line: a new line above it, indented
        // like the entries.
        let indent = entries.last().map(|entry| {
            let at = source[..entry.start].rfind('\n').map_or(0, |at| at + 1);
            source[at..entry.start].to_string()
        }).filter(|indent| indent.bytes().all(|byte| byte == b' ' || byte == b'\t')).unwrap_or_else(|| format!("{}    ", &source[line_start..close]));
        written.insert_str(line_start, &format!("{indent}{field}: {value}\n"));
    } else {
        let before = source[..close].trim_end();
        let at = before.len();
        let gap = if source[open + 1..at].trim().is_empty() { "" } else { " " };
        written.replace_range(at..close, &format!("{gap}{field}: {value}"));
    }
    written
}

/// Whether `text` is a plain literal the writer may replace: numbers,
/// strings, `@ids`, `true`/`false`/`nil` and arrays or objects of them. A
/// name, call or operator makes it an expression the author owns.
pub fn plain_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'"' {
            index = skip_literal(bytes, index).unwrap_or(bytes.len());
            continue;
        }
        if byte == b'@' {
            index += 1;
            while index < bytes.len() && ident_byte(bytes[index]) {
                index += 1;
            }
            continue;
        }
        if ident_start(byte) {
            let word = word_at(text, index);
            let after = skip_space(text, index + word.len());
            // An object key is fine; so are the three constants and an
            // exponent (`1e5`, reached only after a digit).
            let key = bytes.get(after) == Some(&b':');
            let exponent = index > 0 && bytes[index - 1].is_ascii_digit() && (byte == b'e' || byte == b'E');
            // The cut's own value constructors, as the page writes them.
            let constructor = bytes.get(after) == Some(&b'(') && matches!(word, "keys" | "line" | "asset" | "run" | "edit" | "marker" | "vec2" | "take" | "preset");
            if !(key || exponent || constructor || matches!(word, "true" | "false" | "nil")) {
                return false;
            }
            index += word.len().max(1);
            continue;
        }
        if !(byte.is_ascii_digit() || byte.is_ascii_whitespace() || b".-+[]{},:()".contains(&byte)) {
            return false;
        }
        index += 1;
    }
    true
}

/// A Splash string literal.
pub fn string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// A number as a cut writes it: whole numbers with `.0`, else the
/// shortest round trip, rounded to a microsecond grid first so drags do
/// not write float noise.
pub fn number(value: f64) -> String {
    let value = (value * 1e6).round() / 1e6;
    let value = if value == 0.0 { 0.0 } else { value };
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDIT: &str = "use mod.flow.*\nlet a = Clip{src: asset(\"x\", \"y\") in: 2.0 span: [0.0, 6.0] fit: @fill}\n// Flow{ in a comment\nlet s = \"Flow{\"\nFlow{\n    kind: @cut\n    label: \"L\"\n    output: {width: 1080 height: 1920 fps: 60}\n    duration: 17.0\n    a, s\n}\n";

    #[test]
    fn finds_the_final_flow_and_constructors_past_strings_and_comments() {
        let (open, close) = final_flow(EDIT).unwrap();
        assert!(EDIT[..open].ends_with("Flow"));
        assert_eq!(&EDIT[close..], "}\n");
        let object = constructor(EDIT, "a").unwrap();
        assert!(EDIT[object.0..=object.1].starts_with("{src:"));
        assert!(constructor(EDIT, "b").is_err());
    }

    #[test]
    fn fields_split_values_of_every_shape() {
        let object = constructor(EDIT, "a").unwrap();
        let names = fields(EDIT, object).into_iter().map(|field| (field.name, EDIT[field.value.0..field.value.1].to_string())).collect::<Vec<_>>();
        assert_eq!(names, vec![
            (Some("src".into()), "asset(\"x\", \"y\")".into()),
            (Some("in".into()), "2.0".into()),
            (Some("span".into()), "[0.0, 6.0]".into()),
            (Some("fit".into()), "@fill".into()),
        ]);
        let flow = final_flow(EDIT).unwrap();
        let entries = fields(EDIT, flow);
        assert_eq!(entries.iter().filter(|field| field.name.is_none()).map(|field| &EDIT[field.value.0..field.value.1]).collect::<Vec<_>>(), vec!["a", "s"]);
        assert_eq!(field_text(EDIT, flow, "output"), Some("{width: 1080 height: 1920 fps: 60}"));
    }

    #[test]
    fn set_field_replaces_or_inserts_and_touches_nothing_else() {
        let object = constructor(EDIT, "a").unwrap();
        let moved = set_field(EDIT, object, "span", "[1.5, 4.0]");
        assert_eq!(moved, EDIT.replace("span: [0.0, 6.0]", "span: [1.5, 4.0]"));
        let object = constructor(&moved, "a").unwrap();
        let tracked = set_field(&moved, object, "track", "2.0");
        assert!(tracked.contains("fit: @fill track: 2.0}"));
        let flow = final_flow(&tracked).unwrap();
        let longer = set_field(&tracked, flow, "duration", "20.0");
        assert!(longer.contains("    duration: 20.0\n"));
        let flow = final_flow(&longer).unwrap();
        let bpm = set_field(&longer, flow, "bpm", "128.0");
        assert!(bpm.contains("    a, s\n    bpm: 128.0\n}"), "{bpm}");
    }

    #[test]
    fn numbers_are_written_plainly() {
        assert_eq!(number(2.0), "2.0");
        assert_eq!(number(1.0000000001), "1.0");
        assert_eq!(number(-0.0), "0.0");
        assert_eq!(number(0.25), "0.25");
        assert_eq!(string_literal("a\"b"), "\"a\\\"b\"");
    }
}
