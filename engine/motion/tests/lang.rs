//! A render of a document in another language: the document writes each
//! text in the languages it is shown in and picks by `lang()`; a render
//! for no language (the presenter's) is the document as written.
use makepad_motion::*;
use std::sync::Arc;

const DOC: &str = r#"use mod.motion.*
let tr = fn(t) { if lang() == "zh" { t.zh ?? t.en } else { t.en } }
let TITLE = tr({en: "Motion, in Splash." zh: "用 Splash 做动效。"})
Motion{size: vec2(1920, 1080) duration: 2.0 layers: [
    Text{name: "title" text: TITLE size: 96 at: vec2(960, 300) align: @center}
    Draw2D{name: "chrome" size: vec2(1920, 1080) draw: fn(c, t, edit) {
        c.text(tr({en: "Shape and paths" zh: "形状与路径"}), 100, 900, {px: 24})
        c.text(tr({en: "Only in English"}), 100, 950, {px: 24})
    }}
]}
"#;

fn load(lang: Option<&str>) -> MotionDoc {
    let context = MotionContext { lang: lang.map(Arc::<str>::from), ..Default::default() };
    MotionDoc::load_with(DOC, &[], context).unwrap_or_else(|d| panic!("{}", d.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")))
}

/// Every text the frame draws: Text items and Draw2D text runs.
fn texts(frame: &MotionFrame) -> Vec<String> {
    let mut out = Vec::new();
    for item in &frame.items {
        match &item.kind {
            ItemKind::Text { text, .. } => out.push(text.clone()),
            ItemKind::Draw2d { picture, .. } => {
                for op in &picture.list.ops {
                    if let makepad_motion::draw2d::Op::Text(t) = op {
                        out.push(t.text.clone());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[test]
fn a_render_for_a_language_draws_the_texts_the_document_gives_in_it() {
    let zh = texts(&load(Some("zh")).eval(1.0));
    assert_eq!(zh, ["用 Splash 做动效。", "形状与路径", "Only in English"]);
}

#[test]
fn a_render_for_no_language_is_the_document_as_written() {
    let written = texts(&load(None).eval(1.0));
    assert_eq!(written, ["Motion, in Splash.", "Shape and paths", "Only in English"]);
    assert_eq!(texts(&load(Some("en")).eval(1.0)), written);
}
