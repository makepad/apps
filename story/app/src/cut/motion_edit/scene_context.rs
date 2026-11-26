//! What the one chat is told of the graphic desk (its target's context and
//! suggestions), and the editor panel's progress while it is made. The chat
//! itself is the aichat panel (`chat.rs`): a change to the scene or its
//! editor is `scene_ask`, `patch_motion` or `editor_*`, a request for
//! variants is `variants`, and a question is answered from what is read
//! here and from `read_motion` / `graphic_source`.
//!
//! Main-loop cost: the context is built ten times a second at most while
//! the chat shows; the panel slot is a string compare per pump.

use super::super::{chat_context, VideoCut};
use makepad_ai_services::Suggestion;
use makepad_studio_api::services::MOTION;
use makepad_strict_json::obj;
use makepad_widgets::*;
use makepad_editor_panel::EditorPanelWidgetRefExt;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.ScenePanelMaking = View{width: Fill height: Fill flow: Down visible: false padding: Inset{left: 12 right: 12 top: 10 bottom: 10} spacing: 10
        Label{width: Fill text: "EDITOR" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 8}}}
        m_panel_progress := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 9}}}
    }
}

/// The panel slot's state.
#[derive(Default)]
pub struct Slot {
    /// What the slot shows (text, still making), and why the last make did
    /// not start.
    progress: Option<(String, bool)>,
    error: Option<String>,
}

/// Whether the open graphic has an editor panel (`None`: no graphic open).
fn has_editor(page: &VideoCut) -> Option<bool> {
    page.motion.desk.session.as_ref().map(|s| super::editor_panel::body_of(&s.source, &s.code).is_some())
}

/// The desk's suggestions: what the person could ask for now.
pub(crate) fn suggestions(page: &VideoCut) -> Vec<Suggestion> {
    let mut chips = Vec::new();
    if has_editor(page) == Some(false) {
        chips.push(Suggestion { label: "Make the editor".into(), app: MOTION.into(), tool: "editor_make".into(), prefill: obj(Vec::new()).to_json() });
    }
    chips
}

/// The code line at `byte` of `text`: its number (1-based) and itself, short.
fn line_at(text: &str, byte: usize) -> (usize, String) {
    let byte = byte.min(text.len());
    let start = text[..byte].rfind('\n').map_or(0, |n| n + 1);
    let end = text[byte..].find('\n').map_or(text.len(), |n| byte + n);
    let line = text[start..end].trim();
    let short: String = line.chars().take(110).collect();
    (text[..start].matches('\n').count() + 1, if short.len() < line.len() { format!("{short}…") } else { short })
}

/// What the person sees in the desk, for the model: the moment, the word
/// sung, what draws there, the node selected in the structure tree, the
/// things selected in the picture (their paths and the code lines that
/// drew them) and the scene's history.
pub(crate) fn context(page: &VideoCut) -> String {
    let mut out = String::new();
    let Some(session) = page.motion.desk.session.as_ref() else { return out };
    let t = session.playhead;
    out.push_str(&format!("The moment: {t:.2} s of {:.2} s{}.\n", session.duration(), if session.playing.is_some() { " (playing)" } else { "" }));
    // The sung word: the film's timing at the edit's time of this moment,
    // else the timing a module of the scene writes (song seconds).
    let start = session.node.as_deref().and_then(|n| page.projection.block(n)).map(|b| b.start);
    let sung = match (&page.cut_panel.timing, start) {
        (Some(timing), Some(start)) => chat_context::sung_at(&timing.words, start + t),
        _ => session.modules.iter().find_map(|m| super::super::cut_panel::SongTiming::parse(&m.source)).and_then(|timing| chat_context::sung_at(&timing.words, t)),
    };
    if let Some(sung) = sung {
        out.push_str(&sung);
        out.push('\n');
    }
    // What draws there: the top-level code that draws most.
    let mut drawing: Vec<(f32, String)> = Vec::new();
    for (file, amounts) in super::desk::activity_now(page) {
        let (code, prefix) = match file {
            makepad_motion::schema::CodeFile::Doc => (&session.code, String::new()),
            makepad_motion::schema::CodeFile::Module(key) => match session.modules.iter().find(|m| m.key == *key) {
                Some(m) => (&m.code, format!("{}.", m.name)),
                None => continue,
            },
        };
        for (region, amount) in code.regions.iter().zip(amounts) {
            if region.parent.is_none() && *amount > 0.0 && !region.name.contains("Motion") {
                drawing.push((*amount, format!("{prefix}{}", region.name)));
            }
        }
    }
    drawing.sort_by(|a, b| b.0.total_cmp(&a.0));
    if !drawing.is_empty() {
        out.push_str(&format!("Drawing now (most first): {}.\n", drawing.iter().take(8).map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(node) = &page.motion.structure.selected {
        out.push_str(&format!("Selected in the structure tree: `{node}`.\n"));
    }
    // The picture's selection: each thing, its paths, its words, and the
    // code lines that drew it (innermost first).
    let (_, things) = super::desk::selection(page);
    let names: Vec<String> = std::iter::once("doc".to_string()).chain(session.modules.iter().map(|m| m.name.clone())).collect();
    for s in things.iter().take(6) {
        out.push_str(&format!("Selected in the picture: {}", s.label));
        if let Some(words) = &s.words {
            out.push_str(&format!(", its words “{words}”"));
        }
        if !s.paths.is_empty() {
            out.push_str(&format!(" (paths: {})", s.paths.join(", ")));
        }
        out.push('\n');
        if let Some(said) = &s.origin_said {
            out.push_str(&format!("  its text: {said}\n"));
        }
        for (file, byte) in s.code.iter().take(4) {
            let doc = match file {
                makepad_motion::schema::CodeFile::Doc => Some(0),
                makepad_motion::schema::CodeFile::Module(key) => session.modules.iter().position(|m| m.key == *key).map(|i| i + 1),
            };
            let Some(doc) = doc else { continue };
            let text = if doc == 0 { &session.source } else { &session.modules[doc - 1].source };
            let (line, written) = line_at(text, *byte);
            out.push_str(&format!("  drawn at {} line {line}: {written}\n", names[doc]));
        }
    }
    if things.is_empty() {
        out.push_str("Nothing is selected in the picture.\n");
    }
    out.push_str(&super::history::context(page));
    out
}

pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    // The panel's own Make (no panel yet): a new panel, made now, its
    // progress in the slot. (A remake is asked in the chat.)
    let panel = desk_ref.widget(cx, ids!(m_editor_panel)).as_editor_panel();
    if panel.make_clicked(actions) {
        if let Err(error) = super::deep_make::start(page, cx, "") {
            page.motion.slot.error = Some(error);
        }
    }
}

pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    // The panel slot while there is no panel: made, or why not.
    let has_panel = has_editor(page) == Some(true);
    // (A remake shows its progress over the panel it replaces.)
    let making = super::deep_make::progress(page).filter(|p| p.1).or_else(|| super::variants::panel_progress(page).filter(|p| p.1));
    let progress = if page.motion.desk.session.is_none() {
        None
    } else if has_panel {
        making
    } else {
        making.or_else(|| super::deep_make::progress(page)).or_else(|| super::variants::panel_progress(page)).or_else(|| Some((page.motion.slot.error.clone().unwrap_or_else(|| "This scene has no editor yet: ask the chat to make one.".into()), false)))
    };
    if progress.as_ref().is_some_and(|p| p.1) {
        // The seconds and dots move while it is made.
        cx.new_next_frame();
    }
    if progress != page.motion.slot.progress {
        desk_ref.widget(cx, ids!(m_panel_making)).set_visible(cx, progress.is_some());
        if let Some((text, _)) = &progress {
            desk_ref.widget(cx, ids!(m_panel_progress)).set_text(cx, text);
        }
        page.motion.slot.progress = progress;
    }
}
