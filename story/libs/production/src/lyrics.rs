//! The Lyrics step: one answer from the model with the song's title, the
//! music model's brief and the section-tagged lyric sheet (or, when the
//! person gave the lyrics, the title and brief for them).

/// The lyric writer's instructions.
pub const SYSTEM: &str = "\
You write songs for lyric videos. A music model sings exactly the lyric sheet you write, from a one-line \
music brief. Answer in exactly this form and nothing else (no code fences, no commentary):

TITLE: <the song's title, 1-5 words>
MUSIC: <one line for the music model: genre and style, tempo as `<n> BPM`, key, the lead voice, the arrangement>
[Verse]
<lines>

[Chorus]
<lines>
...

Rules:
- Section tags in square brackets on their own line: [Intro], [Verse], [Pre-Chorus], [Chorus], [Bridge], [Outro]. \
Each sung section has 2-4 lines; a tag may stand alone for an instrumental part ([Intro], [Break], [Outro]).
- Lines are short and singable: 4-9 words, concrete images, one idea a line, natural stress. The chorus repeats \
word for word each time it comes back; write it out in full every time.
- Plain words only: no stage directions, no parentheses, no backing-vocal echoes, no ad-libs, no numbers as digits.
- Nothing about real people, brands, products or real-world conflicts.
- The MUSIC line asks for one clear lead vocal with clear diction and upfront vocals (the words are timed from it), \
no backing vocals or harmonies, and fits the requested style.";

/// What the writer is asked.
pub fn request(ask: &str, seconds: f64, given: Option<&str>) -> String {
    match given {
        Some(sheet) => format!(
            "The video: {ask}\nThe song runs about {seconds:.0} seconds. The person wrote the lyrics; keep them exactly, \
             answer with TITLE and MUSIC for them and then the sheet unchanged:\n\n{}",
            sheet.trim()
        ),
        None => {
            // The music model takes about six seconds a sung line with its
            // gaps, after an intro of about ten: lines for the whole length,
            // so the song ends where its lyrics do.
            let lines = (((seconds - 10.0) / 6.0).round() as usize).clamp(4, 40);
            format!(
                "The video: {ask}\nWrite the song for it: {seconds:.0} seconds, {lines} sung lines in all (no more: \
                 the music model sings about one line every six seconds), a short intro, in the style the video asks for."
            )
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub title: String,
    pub caption: String,
    pub sheet: String,
}

/// Read the writer's answer. The sheet must have at least one tagged block
/// with sung lines; a `TITLE`/`MUSIC` line anywhere before it is taken.
pub fn parse(answer: &str) -> Result<Written, String> {
    let mut title = String::new();
    let mut caption = String::new();
    let mut sheet: Vec<String> = Vec::new();
    for raw in answer.lines() {
        let line = raw.trim().trim_start_matches(['*', '#']).trim();
        if line.starts_with("```") {
            continue;
        }
        if sheet.is_empty() {
            if let Some(rest) = strip_key(line, "TITLE") {
                title = rest.trim_matches(['"', '*', ' ']).to_string();
                continue;
            }
            if let Some(rest) = strip_key(line, "MUSIC") {
                caption = rest.to_string();
                continue;
            }
            if !(line.starts_with('[') && line.ends_with(']')) {
                continue;
            }
        }
        if line.is_empty() {
            if sheet.last().is_some_and(|last| !last.is_empty()) {
                sheet.push(String::new());
            }
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if sheet.last().is_some_and(|last| !last.is_empty()) {
                sheet.push(String::new());
            }
            sheet.push(line.to_string());
            continue;
        }
        // A sung line: words as written, without stage directions in parentheses.
        let mut words = String::new();
        let mut depth = 0;
        for c in line.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth = (depth - 1).max(0),
                _ if depth == 0 => words.push(c),
                _ => {}
            }
        }
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        if !words.is_empty() {
            sheet.push(words);
        }
    }
    while sheet.last().is_some_and(String::is_empty) {
        sheet.pop();
    }
    let sung = sheet.iter().filter(|line| !line.is_empty() && !line.starts_with('[')).count();
    if sung < 2 {
        return Err("the answer has no lyric sheet: `[Verse]` and its lines".into());
    }
    if caption.is_empty() {
        return Err("the answer has no `MUSIC:` line".into());
    }
    if title.is_empty() {
        title = sheet.iter().find(|line| !line.is_empty() && !line.starts_with('[')).cloned().unwrap_or_default();
    }
    Ok(Written { title, caption, sheet: sheet.join("\n") })
}

fn strip_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key).or_else(|| line.strip_prefix(&key.to_lowercase()))?;
    let rest = rest.trim_start_matches(['*', ' ']);
    rest.strip_prefix(':').map(|rest| rest.trim().trim_start_matches('*').trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_reads_as_title_brief_and_sheet() {
        let answer = "Here you go.\n**TITLE:** Neon Tide\nMUSIC: synthwave, 104 BPM, A minor, female lead vocal, clear diction, upfront vocals\n\n[Intro]\n[Verse]\nthe city hums (softly) below\nwe ride the neon tide\n\n\n[Chorus]\nhold on, hold on\nthe night is ours\n```";
        let written = parse(answer).expect("reads");
        assert_eq!(written.title, "Neon Tide");
        assert!(written.caption.starts_with("synthwave, 104 BPM"));
        assert_eq!(written.sheet, "[Intro]\n\n[Verse]\nthe city hums below\nwe ride the neon tide\n\n[Chorus]\nhold on, hold on\nthe night is ours");
        assert!(parse("TITLE: x\n[Verse]\nonly one line").is_err());
    }
}
