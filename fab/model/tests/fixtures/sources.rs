// Extract raw literals (any # count) and escaped literals, including local let sources.
// Each literal is consumed whole so quotes in a Splash body cannot become Rust sources.
pub fn literals(text: &str) -> Vec<(usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"//") { i += text[i..].find('\n').unwrap_or(b.len() - i); continue; }
        if b[i..].starts_with(b"/*") {
            i += 2; let mut depth = 1;
            while depth > 0 && i < b.len() {
                if b[i..].starts_with(b"/*") { depth += 1; i += 2; }
                else if b[i..].starts_with(b"*/") { depth -= 1; i += 2; } else { i += 1; }
            }
            continue;
        }
        let start = i;
        // Character literals can contain a quote; lifetimes have no closing apostrophe.
        if b[i] == b'\'' && i + 2 < b.len() {
            let end = if b[i + 1] == b'\\' { i + 3 } else { i + 1 + text[i + 1..].chars().next().unwrap().len_utf8() };
            if b.get(end) == Some(&b'\'') { i = end + 1; continue; }
        }
        if b[i] == b'r' {
            let mut q = i + 1;
            while q < b.len() && b[q] == b'#' { q += 1; }
            if q < b.len() && b[q] == b'"' {
                let end = format!("\"{}", "#".repeat(q - i - 1));
                let len = text[q + 1..].find(&end).expect("closed Rust raw literal");
                out.push((text[..start].bytes().filter(|&c| c == b'\n').count() + 1, text[q + 1..q + 1 + len].into()));
                i = q + 1 + len + end.len(); continue;
            }
        }
        if b[i] == b'"' {
            i += 1; let mut value = String::new();
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    value.push(match b[i] { b'n' => '\n', b'r' => '\r', b't' => '\t', b'0' => '\0', c => c as char }); i += 1;
                } else { let c = text[i..].chars().next().unwrap(); value.push(c); i += c.len_utf8(); }
            }
            i += 1;
            out.push((text[..start].bytes().filter(|&c| c == b'\n').count() + 1, value)); continue;
        }
        i += 1;
    }
    out
}


// Preserve the weave regression documents after app/tests/weave.rs was folded
// into mechanics.rs; the 120 mm sheet is the 181-loop / 5,068-segment case.
pub const WEAVE_MAIL: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet mail = m.mail(Rect{size: [40mm 40mm]})\nobject(mail)\n";
pub const WEAVE_LARGE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet mail = m.mail(Rect{size: [120mm 120mm]}, {bar: 0.8mm})\nobject(mail)\n";

// The mixed sheet/box mechanics document from the original census, before the
// UI test consolidation replaced it with MAIL + OTHER.
pub const MECHANICS_SHEET: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet sheet = m.mail(Rect{size: [40mm 30mm]}, {size: 6mm bar: 0.8mm})\nobject(sheet, name: \"sheet\")\nlet blk = m.box({size: [16mm 16mm 16mm] at: [50mm 0 0]})\nobject(blk, name: \"blk\")\n";
