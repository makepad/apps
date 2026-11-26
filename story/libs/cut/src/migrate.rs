//! The EDIT -> CUT rename of a stored cut document (schema v21 renamed the
//! kind; this renames the Splash names a document written before it uses).
//!
//! Token-level and exact: only the renamed names change, and never inside a
//! string or a comment.
//!
//! - `mod.edit` -> `mod.cut` (the module, and `mod.edit.edit(` -> `mod.cut.cut(`)
//! - `EditOutput` -> `CutOutput`
//! - `@edit` -> `@cut` (the flow kind and the nested-cut reference)
//! - the nested-cut reference function `edit(id, rev)` -> `cut(id, rev)`

/// `source` with the renamed names rewritten, or `None` when it uses none.
pub fn rename_edit_to_cut(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut changed = false;
    let mut i = 0;
    // The last token that was an identifier or punctuation, for `mod . edit`.
    let mut prev_mod_dot = false;
    let mut prev_dot = false;
    let mut prev_at = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            let end = source[i..].find('\n').map_or(source.len(), |n| i + n);
            out.push_str(&source[i..end]);
            i = end;
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let end = source[i + 2..].find("*/").map_or(source.len(), |n| i + 2 + n + 2);
            out.push_str(&source[i..end]);
            i = end;
        } else if c == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(bytes.len());
            out.push_str(&source[i..end]);
            i = end;
            prev_mod_dot = false;
            prev_dot = false;
            prev_at = false;
        } else if c == b'_' || c.is_ascii_alphabetic() {
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b'_' || bytes[j].is_ascii_alphanumeric()) {
                j += 1;
            }
            let word = &source[i..j];
            let call = bytes.get(j) == Some(&b'(');
            let renamed = match word {
                "edit" if prev_mod_dot || prev_at => Some("cut"),
                // `mod.cut.edit(` was `mod.edit.edit(`: its module is already renamed above.
                "edit" if prev_dot && out.ends_with("mod.cut.") => Some("cut"),
                "edit" if call && !prev_dot => Some("cut"),
                "EditOutput" => Some("CutOutput"),
                _ => None,
            };
            match renamed {
                Some(new) => {
                    out.push_str(new);
                    changed = true;
                }
                None => out.push_str(word),
            }
            prev_mod_dot = false;
            prev_at = false;
            prev_dot = false;
            // Remember `mod` for the `.` that follows.
            if word == "mod" {
                prev_mod_dot = false;
                let after = &source[j..];
                if after.starts_with('.') {
                    // Consume the dot here so the next word sees `prev_mod_dot`.
                    out.push('.');
                    i = j + 1;
                    prev_mod_dot = true;
                    continue;
                }
            }
            i = j;
        } else {
            let ch = source[i..].chars().next().unwrap();
            out.push(ch);
            prev_at = ch == '@';
            prev_dot = ch == '.';
            prev_mod_dot = false;
            i += ch.len_utf8();
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::rename_edit_to_cut as rename;

    #[test]
    fn renames_only_the_renamed_names() {
        let old = "use mod.flow.*\nuse mod.edit.*\n// mod.edit stays in a comment\nlet note = \"@edit and EditOutput stay in a string\"\nlet out = EditOutput{frame: 1}\nlet src = mod.edit.edit(\"a\", \"b\")\nlet x = edit(\"a\", \"b\")\nFlow{kind: @edit path: \"edit.p1.cut\" a: edit.p1}\n";
        let new = rename(old).unwrap();
        assert_eq!(
            new,
            "use mod.flow.*\nuse mod.cut.*\n// mod.edit stays in a comment\nlet note = \"@edit and EditOutput stay in a string\"\nlet out = CutOutput{frame: 1}\nlet src = mod.cut.cut(\"a\", \"b\")\nlet x = cut(\"a\", \"b\")\nFlow{kind: @cut path: \"edit.p1.cut\" a: edit.p1}\n"
        );
        assert_eq!(rename(&new), None);
    }
}
