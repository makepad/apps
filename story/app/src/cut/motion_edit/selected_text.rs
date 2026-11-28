//! Text picked in the picture, editable at the top of the panel column:
//! its words where they come from (the pick's origin: a lyric word in the
//! song's timing module, a listing line's string, a label's literal), typed
//! over live (the code edited as it goes, one undo step per typing), and
//! the font, size and colour the element reads, as panel controls. It is
//! there whatever the generated panel has.
//!
//! A sung word shows its whole line (the word picked selected): typing
//! rewrites the line's words in the song's timing (`sung_line`: the words
//! still there keep their times), one undo step per typing. "One word"
//! edits the picked word alone.
//!
//! Main-loop cost: read when the selection changes; a keystroke is one
//! splice.

use makepad_edit_values::ValueKind;
use super::super::VideoCut;
use makepad_motion::paths;
use makepad_edit_values::Edit;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    /** What is picked in the picture, as a compact card over the panel:
     * its words (typed over live), the font and colours it reads, and an
     * × that lets go of the selection. The panel's modules stay below.
     * Its colour and font controls hold a drag and write it once (a hand). */
    mod.widgets.SceneSelectedText = mod.widgets.PanelHand{width: Fill height: Fit flow: Down visible: false
        margin: Inset{left: 6 right: 6 top: 6 bottom: 2}
        RoundedView{width: Fill height: Fit flow: Down padding: Inset{left: 10 right: 6 top: 6 bottom: 8} spacing: 4
            draw_bg +: {color: stage.row border_color: stage.accent_a66 border_size: 1.0 border_radius: 6.0}
            View{width: Fill height: 22 flow: Right spacing: 6 align: Align{y: 0.5}
                Label{width: Fit text: "Selected" draw_text +: {color: stage.accent text_style: theme.font_bold{font_size: 8}}}
                t_from := Label{width: Fill max_lines: 1 text_overflow: Ellipsis text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7.5}}}
                t_mode := ButtonFlatter{visible: false text: "One word"}
                Tip{text: "Let go of the selection" t_clear := StageButton{width: 20 height: 20 margin: 0 padding: 0 text: "×" draw_text +: {text_style: theme.font_regular{font_size: 10}}}}
            }
            // (The picked word of a sung line stays marked while unfocused.)
            t_words := TextInput{width: Fill height: Fit is_multiline: true submit_on_enter: true margin: Inset{right: 4} padding: Inset{left: 8 right: 8 top: 4 bottom: 4} empty_text: "the words"
                draw_bg +: {color: stage.well color_hover: stage.well color_focus: stage.well border_color: stage.line_faint border_size: 1.0 border_radius: 4.0}
                draw_selection +: {color: theme.color_selection_focus}
                draw_text +: {text_style: theme.font_regular{font_size: 9}}}
            t_style := Splash{width: Fill height: Fit}
        }
    }
}

/// The text being edited: its document, its bytes now, the gesture its
/// typing joins, and the selection it is of.
#[derive(Default)]
pub struct SelectedText {
    of: Option<u64>,
    at: Option<(usize, std::ops::Range<usize>)>,
    gesture: u64,
    typing: bool,
    /// What its bytes say now (an edit lands only where they still do),
    /// and the words as picked (put back when an edit breaks the scene).
    expect: String,
    original: String,
    /// A finished edit waiting for its load: (revision, document, range).
    verify: Option<(u64, usize, std::ops::Range<usize>)>,
    /// The literal is a raw string (`r#"…"#`: its hashes).
    raw: Option<usize>,
    /// A sung line being edited (`at` is the inside of its word list),
    /// and as the last typing wrote it (the line once the scene runs it).
    line: Option<super::sung_line::Line>,
    written: Option<super::sung_line::Line>,
    /// A sung word edits its word alone, not its line.
    one_word: bool,
    /// A sung word asked for from outside the picture:
    /// (document, bytes of its `w` string).
    asked: Option<(usize, std::ops::Range<usize>)>,
}

/// The string literal holding byte `at` of a Splash text: its contents'
/// bytes (between the quotes) and its raw-string hashes (None: a plain
/// string, escaped). Comments are skipped.
fn literal_at(text: &str, at: usize) -> Option<(std::ops::Range<usize>, Option<usize>)> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'r' if (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) && matches!(b.get(i + 1), Some(b'"') | Some(b'#')) => {
                let mut hashes = 0;
                let mut j = i + 1;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let start = j + 1;
                let close: String = std::iter::once('"').chain(std::iter::repeat('#').take(hashes)).collect();
                let end = text[start..].find(&close).map(|k| start + k)?;
                if at >= start && at <= end {
                    return Some((start..end, Some(hashes)));
                }
                i = end + close.len();
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                if at >= start && at <= j {
                    return Some((start..j.min(b.len()), None));
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// A string's words as a Splash string's inside (between its quotes).
fn escaped(words: &str) -> String {
    words.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn unescaped(text: &str) -> String {
    text.replace("\\n", "\n").replace("\\\"", "\"").replace("\\\\", "\\")
}

pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    verify(page, cx, ui);
    let asked = page.motion.selected_text.asked.take();
    if asked.is_some() {
        page.motion.selected_text.typing = false;
    }
    let (rev, things) = super::desk::selection(page);
    // While typing the words stay where they are being typed (a new pick
    // of the same text says where it was before the typing).
    if asked.is_none() && (page.motion.selected_text.of == Some(rev) || page.motion.selected_text.typing) {
        return;
    }
    // A text with an origin: the first picked (or the sung word asked for).
    let asked = asked.map(|origin| (origin, "Sung word".to_string(), Vec::new(), Some("sung word".to_string())));
    let picked = asked.or_else(|| things.iter().find_map(|s| Some((s.origin.clone()?, s.label.clone(), s.paths.clone(), s.origin_said.clone()))));
    page.motion.selected_text.of = Some(rev);
    page.motion.selected_text.line = None;
    page.motion.selected_text.written = None;
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let pane = desk_ref.widget(cx, ids!(m_selected_text));
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let Some(((doc, range), label, picked_paths, said)) = picked else {
        page.motion.selected_text.at = None;
        pane.set_visible(cx, false);
        return;
    };
    let text = if doc == 0 { Some(session.source.as_str()) } else { session.modules.get(doc - 1).map(|m| m.source.as_str()) };
    let Some(mut words) = text.and_then(|t| t.get(range.clone())) else {
        pane.set_visible(cx, false);
        return;
    };
    // The whole string literal the drawn words are in (a typing-on shows
    // part of it), between its quotes.
    let drawn = words.trim_start_matches(['r', '#', '"']).trim_end_matches(['#', '"']).to_string();
    // (Inside its quotes: past a raw string's `r#"` too.)
    let lead = words.len() - words.trim_start_matches(['r', '#', '"']).len();
    let Some((range, raw)) = text.and_then(|t| literal_at(t, range.start + lead)) else {
        pane.set_visible(cx, false);
        return;
    };
    words = text.and_then(|t| t.get(range.clone())).unwrap_or_default();
    page.motion.selected_text.raw = raw;
    let from = if doc == 0 { "the graphic".to_string() } else { session.modules.get(doc - 1).map_or(String::new(), |m| format!("the {} module", m.name)) };
    let line = text.map_or(0, |t| t[..range.start].matches('\n').count() + 1);
    let shown = if raw.is_some() { words.to_string() } else { unescaped(words) };
    let part = if drawn.is_empty() || drawn == shown { String::new() } else { format!(" (drawn now: \"{drawn}\")") };
    // A sung word says what it edits (the song's lyrics, not the drawing).
    let sung = said.as_ref().is_some_and(|s| s.starts_with("sung word"));
    let from_said = match said.filter(|s| s.starts_with("sung word ")) {
        Some(said) => said,
        None => format!("{label} · {from}, line {line}{part}"),
    };
    pane.widget(cx, ids!(t_from)).set_text(cx, &from_said);
    pane.widget(cx, ids!(t_mode)).set_visible(cx, sung);
    pane.widget(cx, ids!(t_words)).set_text(cx, &shown);
    page.motion.selected_text.at = Some((doc, range.clone()));
    page.motion.selected_text.expect = words.to_string();
    page.motion.selected_text.original = words.to_string();
    // A sung word: its whole line, the word selected.
    let line = if sung && !page.motion.selected_text.one_word {
        text.and_then(|t| {
            let (a, b) = super::desk::enclosing_list(t, &range)?;
            let line = super::sung_line::Line::parse(&t[a + 1..b])?;
            // Which of its words: the records before it.
            let k = line.recs.iter().position(|r| t[a + 1..b].find(&r.text).is_some_and(|o| a + 1 + o + r.text.len() > range.start)).unwrap_or(0);
            Some((a + 1..b, line, k))
        })
    } else {
        None
    };
    pane.widget(cx, ids!(t_mode)).set_text(cx, if line.is_some() { "One word" } else { "Whole line" });
    if let (Some((inner, line, k)), Some(t)) = (line, text) {
        let words = line.words();
        let shown = words.join(" ");
        let from: usize = words[..k].iter().map(|w| w.len() + 1).sum();
        let input = pane.text_input(cx, ids!(t_words));
        input.set_text(cx, &shown);
        use makepad_widgets::makepad_draw::text::selection::{Cursor, Selection};
        input.set_selection(cx, Selection { anchor: Cursor { index: from, prefer_next_row: false }, cursor: Cursor { index: from + words[k].len(), prefer_next_row: false } });
        page.motion.selected_text.at = Some((doc, inner.clone()));
        page.motion.selected_text.expect = t[inner.clone()].to_string();
        page.motion.selected_text.original = t[inner].to_string();
        page.motion.selected_text.raw = None;
        page.motion.selected_text.line = Some(line);
    }
    page.motion.selected_text.gesture += 1;
    page.motion.selected_text.typing = false;
    // Its font, size and colour as the element reads them.
    let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    // Its font and sizes a row each; the colours it reads side by side (a
    // swatch and a name each, the picker under the one opened), only the
    // ones this element reads, not the scene's whole palette.
    let (mut controls, mut colours) = (Vec::new(), Vec::new());
    for p in picked_paths.iter().take(24) {
        let Ok(site) = paths::resolve(&files, p) else { continue };
        let leaf = p.rsplit(['.', '[']).next().unwrap_or(p).to_lowercase();
        let (list, control) = match site.kind {
            ValueKind::Font => (&mut controls, format!("PanelFont{{path: \"{p}\"}}")),
            // A third of the row each, so one alone keeps its swatch form.
            ValueKind::Colour => (&mut colours, format!("PanelColor{{path: \"{p}\" width: 140}}")),
            ValueKind::Number | ValueKind::Length if leaf.contains("size") || leaf.contains("weight") || leaf.contains("tracking") => (&mut controls, format!("PanelSlider{{path: \"{p}\"}}")),
            _ => continue,
        };
        if !list.contains(&control) {
            list.push(control);
        }
    }
    for row in colours.chunks(3) {
        controls.push(format!("PanelRow{{{}}}", row.join(" ")));
    }
    let body = if controls.is_empty() { String::new() } else { format!("flow: Down width: Fill spacing: 2\n{}", controls.join("\n")) };
    let splash = pane.widget(cx, ids!(t_style)).as_splash();
    splash.set_grants(HostGrants::CLIPBOARD);
    splash.set_text(cx, &body);
    pane.set_visible(cx, true);
}

pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    // × lets go of the picture's selection (the card goes with it).
    if desk_ref.button(cx, ids!(t_clear)).clicked(actions) {
        super::panel_time::clear_selection_filter();
        desk_ref.widget(cx, ids!(m_selected_text)).set_visible(cx, false);
        page.motion.selected_text.at = None;
        return;
    }
    if desk_ref.button(cx, ids!(t_mode)).clicked(actions) && !page.motion.selected_text.typing {
        // The same pick again, its word alone or its line.
        page.motion.selected_text.one_word = !page.motion.selected_text.one_word;
        page.motion.selected_text.of = None;
        return;
    }
    let input = desk_ref.text_input(cx, ids!(t_words));
    let done = input.returned(actions).is_some() || input.key_focus_lost(actions);
    let changed = input.changed(actions);
    if changed.is_none() && !(done && page.motion.selected_text.typing) {
        return;
    }
    let Some((doc, range)) = page.motion.selected_text.at.clone() else { return };
    // Where the bytes still say what was read or written there; when the
    // code around moved (another edit, a panel written in), the same
    // literal nearest where it was; else it is gone.
    // (The text as typing left it: typing is held, written when it ends.)
    let text = super::desk::text_now(page, doc).unwrap_or_default();
    let expect = page.motion.selected_text.expect.clone();
    let range = if text.get(range.clone()) == Some(expect.as_str()) {
        range
    } else {
        let found = if page.motion.selected_text.line.is_some() {
            text.match_indices(&expect).map(|(i, _)| i..i + expect.len()).min_by_key(|r| r.start.abs_diff(range.start))
        } else {
            let quoted = format!("\"{expect}\"");
            text.match_indices(&quoted).map(|(i, _)| i + 1..i + 1 + expect.len()).min_by_key(|r| r.start.abs_diff(range.start))
        };
        match found {
            Some(r) => r,
            None => {
                page.motion.selected_text.at = None;
                page.motion.selected_text.typing = false;
                desk_ref.widget(cx, ids!(t_from)).set_text(cx, "The text is no longer in the code: pick it again");
                return;
            }
        }
    };
    let typed = changed.unwrap_or_else(|| input.text());
    let words = if let Some(line) = &page.motion.selected_text.line {
        // The line's words in the song's timing, rewritten from the line
        // as picked (each keystroke from the same start). A scene naming
        // the words by place (`sung(LINE, 4)`) keeps each record where it
        // is; else words added or taken out are records added or taken out.
        let typed_words: Vec<&str> = typed.split_whitespace().collect();
        let by_place = page.motion.desk.session.as_ref().is_some_and(names_words_by_place);
        match if by_place { line.rewrite_in_place(&typed_words) } else { line.rewrite(&typed_words) } {
            Some(inner) => inner,
            None => {
                desk_ref.widget(cx, ids!(t_from)).set_text(cx, "A sung line keeps at least one word");
                return;
            }
        }
    } else { match page.motion.selected_text.raw {
        // A raw string takes its words as they are, all but its own end.
        Some(hashes) => {
            let close: String = std::iter::once('"').chain(std::iter::repeat('#').take(hashes)).collect();
            if typed.contains(&close) {
                desk_ref.widget(cx, ids!(t_from)).set_text(cx, &format!("This string cannot hold {close}"));
                return;
            }
            typed
        }
        None => escaped(&typed),
    } };
    let gesture = u64::MAX / 4 + page.motion.selected_text.gesture;
    page.motion.desk.author = Some("You · text".into());
    super::desk::code_edits(page, cx, doc, vec![Edit::splice(range.start..range.end, words.clone(), gesture, done)]);
    // Its bytes now (the next keystroke replaces these).
    let range = range.start..range.start + words.len();
    page.motion.selected_text.at = Some((doc, range.clone()));
    page.motion.selected_text.expect = words;
    page.motion.selected_text.typing = !done;
    if done {
        page.motion.selected_text.gesture += 1;
        let revision = page.motion.desk.session.as_ref().map_or(0, |s| s.revision);
        page.motion.selected_text.verify = Some((revision, doc, range));
        // The line as written: the next typing starts from it.
        page.motion.selected_text.written = page.motion.selected_text.line.as_ref().and_then(|_| super::sung_line::Line::parse(&page.motion.selected_text.expect));
    }
}

/// Whether a scene names sung words by their place in a line (`sung(LINE1,
/// 4)`, `.words[4]`) in its code or its modules: a word added before
/// one would move what it names.
fn names_words_by_place(session: &super::session::MotionSession) -> bool {
    let by_place = |text: &str| {
        text.match_indices("sung(").any(|(i, _)| {
            let args = &text[i + 5..];
            let args = &args[..args.find(')').unwrap_or(args.len())];
            args.rsplit(',').next().is_some_and(|last| { let t = last.trim(); !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) })
        }) || text.match_indices("words[").any(|(i, _)| text[i + 6..].starts_with(|c: char| c.is_ascii_digit()))
    };
    by_place(&session.source) || session.modules.iter().any(|m| !m.builtin() && by_place(&m.source))
}

/// A finished edit, once the scene loaded it or could not: one that broke
/// the scene is put back (the words as picked), with why.
fn verify(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let Some((revision, doc, range)) = page.motion.selected_text.verify.clone() else { return };
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    if session.revision != revision {
        // Another edit came after it: that one is the scene now.
        page.motion.selected_text.verify = None;
        return;
    }
    if session.doc_of == revision && session.problems.is_empty() {
        // Kept: what is written is the line and the words from now on.
        page.motion.selected_text.verify = None;
        page.motion.selected_text.original = page.motion.selected_text.expect.clone();
        if let Some(written) = page.motion.selected_text.written.take() {
            let said = format!("sung line “{}” · song lyrics", written.words().join(" "));
            ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(t_from)).set_text(cx, &said);
            page.motion.selected_text.line = Some(written);
        }
        return;
    }
    let Some(problem) = session.problems.first().map(|p| format!("line {}:{}: {}", p.line, p.col, p.message)) else { return };
    page.motion.selected_text.verify = None;
    page.motion.selected_text.written = None;
    let original = page.motion.selected_text.original.clone();
    let gesture = u64::MAX / 4 + page.motion.selected_text.gesture;
    page.motion.selected_text.gesture += 1;
    page.motion.desk.author = Some("You · text".into());
    super::desk::code_edits(page, cx, doc, vec![Edit::splice(range.start..range.end, original.clone(), gesture, true)]);
    page.motion.selected_text.at = Some((doc, range.start..range.start + original.len()));
    page.motion.selected_text.expect = original.clone();
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let shown = match &page.motion.selected_text.line {
        Some(line) => line.words().join(" "),
        None if page.motion.selected_text.raw.is_some() => original.clone(),
        None => unescaped(&original),
    };
    desk_ref.widget(cx, ids!(t_words)).set_text(cx, &shown);
    desk_ref.widget(cx, ids!(t_from)).set_text(cx, &format!("Not kept: the scene does not run with it ({problem})"));
}
