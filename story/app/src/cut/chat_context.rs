//! What the chat is told beside the person's words on the CUT page: the
//! words sung at the playhead. (The picture is the shell's
//! `makepad_app_shell::chat::picture`.)


/// The words sung at `t` (edit seconds) and their line, from the song's
/// timing (`words`: word, start, end): the word being sung, or the last
/// one sung and the next.
pub fn sung_at(words: &[(String, f64, f64)], t: f64) -> Option<String> {
    if words.is_empty() {
        return None;
    }
    let now = words.iter().position(|(_, a, b)| t >= *a && t < *b);
    let near = now.or_else(|| words.iter().rposition(|(_, a, _)| *a <= t))?;
    // The line: the words around it with no pause longer than 0.9 s.
    let (mut a, mut b) = (near, near);
    while a > 0 && near - a < 5 && words[a].1 - words[a - 1].2 < 0.9 {
        a -= 1;
    }
    while b + 1 < words.len() && b - near < 5 && words[b + 1].1 - words[b].2 < 0.9 {
        b += 1;
    }
    let line = words[a..=b].iter().map(|(w, ..)| w.as_str()).collect::<Vec<_>>().join(" ");
    let (w, s, e) = &words[near];
    Some(match now {
        Some(_) => format!("Sung now: “{w}” ({s:.2}–{e:.2} s), in “…{line}…”."),
        None if t - e > 2.0 => format!("No word sung now (the last, “{w}”, ended at {e:.2} s)."),
        None => format!("Between words: “{w}” ended at {e:.2} s, in “…{line}…”."),
    })
}
