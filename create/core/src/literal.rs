//! Structural checks shared by all peers that replay named registers.

pub fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn after_string(bytes: &[u8], mut at: usize) -> Option<usize> {
    at += 1;
    while let Some(&b) = bytes.get(at) {
        at += 1;
        match b {
            b'"' => return Some(at),
            b'\\' => { bytes.get(at)?; at += 1; }
            _ => {}
        }
    }
    None
}

fn trivia(bytes: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let rest = &bytes[at..];
        if rest.first().is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        } else if rest.starts_with(b"//") {
            at += rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        } else if let Some(comment) = rest.strip_prefix(b"/*") {
            at += 4 + comment.windows(2).position(|end| end == b"*/")?;
        } else {
            return Some(at)
        }
    }
}

fn closer(byte: u8) -> Option<u8> {
    match byte { b'{' => Some(b'}'), b'[' => Some(b']'), b'(' => Some(b')'), _ => None }
}

/// Frozen under the log's format version. Changing what this accepts changes fold results,
/// so a change to the rule needs a new format version. Only the bytes determine validity.
pub fn is_literal(src: &str) -> bool {
    let bytes = src.as_bytes();
    let Some(start) = trivia(bytes, 0) else { return false };
    let Some(&first) = bytes.get(start) else { return false };
    let mut at = start;
    if first == b'"' {
        let Some(end) = after_string(bytes, at) else { return false };
        at = end;
    } else {
        while let Some(&b) = bytes.get(at) {
            if b.is_ascii_whitespace() || matches!(b, b'{' | b'}' | b'[' | b']' | b'(' | b')' | b'"')
                || bytes[at..].starts_with(b"//") || bytes[at..].starts_with(b"/*") { break }
            at += 1;
        }
        if at > start {
            let Some(next) = trivia(bytes, at) else { return false };
            if next == bytes.len() { return true }
            at = next;
        }
        if let Some(end) = bytes.get(at).and_then(|&b| closer(b)) {
            let mut ends = vec![end];
            at += 1;
            while !ends.is_empty() {
                let Some(next) = trivia(bytes, at) else { return false };
                let Some(&b) = bytes.get(next) else { return false };
                at = next + 1;
                if let Some(end) = closer(b) {
                    ends.push(end);
                } else if matches!(b, b'}' | b']' | b')') {
                    if ends.pop() != Some(b) { return false }
                } else if b == b'"' {
                    let Some(end) = after_string(bytes, next) else { return false };
                    at = end;
                }
            }
        } else {
            return false
        }
    }
    trivia(bytes, at) == Some(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::is_literal;

    #[test]
    fn one_balanced_value_depends_only_on_its_bytes() {
        for (source, expected) in [
            (r#""a \" ]""#, true), (" 12px ", true), ("#ff0", true), ("@id", true),
            ("foo.bar", true), ("Diagram{a:[(1)]}", true), ("path(1)", true),
            (r#"Sticky{text: "}"}"#, true), (r#"Sticky{text: "]"}"#, true),
            (r#"Sticky{text: "{"}"#, true), (r#"Sticky{text: "a \" } b"}"#, true),
            ("[1 // ]\n", false), ("[4 2]", true), ("{anything: /}", true), ("()", true), ("Name[]", true),
            ("/* [ */ [\"}\", // )\n()] // ]", true), ("word/*suffix*/", true),
            ("", false), ("// alone", false), ("[", false), ("}", false), ("([)]", false),
            ("[1]}", false), ("[] []", false), ("a b", false), ("A {}", true),
            ("A/*x*/{}", true), ("f // comment\n(1)", true), ("[]()", false),
            ("[] /*x*/ {}", false), ("\"a\" {}", false), ("\"a\"b", false), ("\"a\\", false),
            ("/*", false), ("a/*", false), ("[\"abc]", false), ("[/*]*/", false),
        ] {
            assert_eq!(is_literal(source), expected, "{source:?}");
        }
        std::thread::Builder::new().stack_size(64 * 1024).spawn(|| {
            let nested = "[".repeat(100_000) + &"]".repeat(100_000);
            assert!(is_literal(&nested));
        }).unwrap().join().unwrap();
    }
}
