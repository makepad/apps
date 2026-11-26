//! Reading positions in a deck's source. Splash gives each object
//! the place it was written; these helpers find the extent of what is
//! written there, skipping strings and comments.

/// The end of the string or comment starting at `index`, when one does.
fn skip_literal(bytes: &[u8], index: usize) -> Option<usize> {
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
        b'/' if bytes.get(index + 1) == Some(&b'/') => Some(bytes[index..].iter().position(|b| *b == b'\n').map_or(bytes.len(), |n| index + n)),
        b'/' if bytes.get(index + 1) == Some(&b'*') => {
            let mut at = index + 2;
            while at + 1 < bytes.len() {
                if bytes[at] == b'*' && bytes[at + 1] == b'/' {
                    return Some(at + 2);
                }
                at += 1;
            }
            Some(bytes.len())
        }
        _ => None,
    }
}

/// The first `needle` at or after `from`, outside strings and comments.
pub fn next_code_char(source: &str, from: usize, needle: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
        } else if bytes[index] == needle {
            return Some(index);
        } else {
            index += 1;
        }
    }
    None
}

/// The `}` closing the `{` at `open`.
pub fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let (mut depth, mut index) = (0usize, open);
    while index < bytes.len() {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' => depth += 1,
            b'}' => {
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

fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// 1-based line and column of a byte offset.
pub fn line_col(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    let line = before.matches('\n').count() as u32 + 1;
    let col = before.rsplit('\n').next().map_or(0, |line| line.chars().count()) as u32 + 1;
    (line, col)
}

/// The byte offset of a 0-based script row and column.
pub fn offset_of(source: &str, row: u32, col: u32) -> Option<usize> {
    let mut offset = 0usize;
    for (index, text) in source.split_inclusive('\n').enumerate() {
        if index == row as usize {
            let byte_col = text.char_indices().nth(col as usize).map(|(at, _)| at).unwrap_or(text.len());
            return Some(offset + byte_col);
        }
        offset += text.len();
    }
    None
}

/// Where `name:` is written directly inside the braces `span` (an object
/// literal, `{` to just past `}`): 1-based line and column.
pub fn field_in(source: &str, span: (usize, usize), name: &str) -> Option<(u32, u32)> {
    let bytes = source.as_bytes();
    let (mut depth, mut index) = (0usize, span.0);
    while index < span.1.min(bytes.len()) {
        if let Some(next) = skip_literal(bytes, index) {
            index = next;
            continue;
        }
        match bytes[index] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            b if depth == 1 && ident_byte(b) && (index == 0 || !ident_byte(bytes[index - 1])) => {
                let mut end = index;
                while end < bytes.len() && ident_byte(bytes[end]) {
                    end += 1;
                }
                if &source[index..end] == name && source[end..].trim_start().starts_with(':') {
                    return Some(line_col(source, index));
                }
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}
