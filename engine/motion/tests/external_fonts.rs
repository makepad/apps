//! External fonts (`makepad_doc_fonts`): a document names Archivo, IBM
//! Plex Mono or Cormorant like a bundled font; installed, the family draws;
//! not installed, its text still lays out and draws in the default face,
//! and the session says so once per family (no error, and nothing after it
//! indexes an empty layout). One test: it points `MAKEPAD_FONTS_DIR` at its
//! own folders, and the notices are the process's.

use makepad_motion::*;
use std::path::PathBuf;

fn load(source: &str) -> MotionDoc {
    MotionDoc::load(source).unwrap_or_else(|d| panic!("{}", d.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n")))
}

fn fonts_of(doc: &MotionDoc) -> Vec<String> {
    doc.eval(0.5)
        .items
        .iter()
        .map(|item| match &item.kind {
            ItemKind::Text { font, .. } => font.clone(),
            ItemKind::Shader { font, .. } => font.clone().unwrap_or_else(|| "(kit)".into()),
            other => panic!("{other:?}"),
        })
        .collect()
}

const DOC: &str = r#"use mod.motion.*; Motion{size: vec2(1920, 1080) duration: 1.0 layers: [
    Text{text: "A" font: Font{family: "Archivo" weight: 800} wdth: 70}
    Text{text: "B" font: Font{family: "ibm plex mono" italic: true}}
    Text{text: "C" font: Font{family: "Cormorant"}}
    KineticText{preset: "318_letter_flip" text: "D" font: Font{family: "Archivo" italic: true}}
    KineticText{preset: "318_letter_flip" text: "E"}
]}"#;

/// Words laid out letter by letter in each external family, as the plates do
/// (`layout_word`, `text_layout` under it), and a Draw2D's text and measure.
const MEASURED: &str = r#"use mod.motion.*; use mod.motion.modules.words.*
let now = layout_word("NOW", Font{family: "Archivo" weight: 900}, 120)
let hi = layout_word("Hi", Font{family: "Cormorant"}, 40)
let mono = layout_word("Mono", Font{family: "IBM Plex Mono" italic: true}, 40)
Motion{size: vec2(640, 360) duration: 1.0 layers: [
    Text{text: now.letters[2].ch font: Font{family: "Archivo"}}
    Text{text: hi.letters[1].ch font: Font{family: "Cormorant"}}
    Text{text: mono.letters[3].ch font: Font{family: "IBM Plex Mono"}}
    Draw2D{draw: fn(c, t, edit) { c.text("A", 10, 40, {px: 30 font: Font{family: "Archivo"}}) c.text("B", 10, 80, {px: 30 font: Font{family: "Cormorant"}}) c.measure("A", {font: Font{family: "Archivo"}}) }}
]}"#;

#[test]
fn an_external_font_draws_once_installed_and_in_the_default_face_until_then() {
    let root = std::env::temp_dir().join(format!("motion_external_fonts_{}", std::process::id()));
    let (missing, installed) = (root.join("missing"), root.join("installed"));
    std::fs::create_dir_all(&missing).unwrap();
    let name = |path: Option<PathBuf>| path.unwrap().file_name().unwrap().to_string_lossy().to_string();

    // Not installed: the document loads with the names it wrote, its text
    // reads the default face's file, and words lay out with every letter;
    // no error, no warning about the font, and no frame error after it.
    std::env::set_var("MAKEPAD_FONTS_DIR", &missing);
    let doc = load(DOC);
    assert_eq!(fonts_of(&doc), ["Archivo", "IBM Plex Mono", "Cormorant", "Archivo", "(kit)"], "the names the document wrote");
    assert!(!doc.warnings.iter().any(|w| w.message.contains("font")), "{:?}", doc.warnings);
    assert_eq!(makepad_doc_fonts::drawn_family("Archivo").map(|f| f.name), Some(makepad_doc_fonts::DEFAULT_FAMILY));
    assert_eq!(name(makepad_doc_fonts::font_file("Archivo", 800.0, true)), "Inter.ttf");
    for _ in 0..2 {
        let doc = load(MEASURED);
        let frame = doc.eval(0.2);
        assert!(frame.errors.is_empty(), "no follow-on errors: {:?}", frame.errors);
        let texts: Vec<&str> = frame.items.iter().filter_map(|item| match &item.kind {
            ItemKind::Text { text, .. } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert_eq!(texts, ["W", "i", "o"], "a word in a missing font has all its letters");
    }
    // Said once per missing family, however often it is named, measured or drawn.
    let mut notices = makepad_doc_fonts::missing_notices();
    notices.sort();
    let said = |family: &str| format!("font {family} is not installed: drawn with the default face; install it in Resources for the intended look");
    assert_eq!(notices, [said("Archivo"), said("Cormorant"), said("IBM Plex Mono")]);
    // The bundled Garamond's old alias still means it, with no warning.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Text{text: "A" font: Font{family: "cormorant"}} Text{text: "B" font: Font{family: "Cormorant Garamond" italic: true}}]}"#);
    assert_eq!(fonts_of(&doc), ["Cormorant Garamond", "Cormorant Garamond"]);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    // A name that is neither bundled nor installable is an error naming the nearest font and both lists.
    let errors = MotionDoc::load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Text{text: "A" font: Font{family: "Archivio"}} KineticText{preset: "318_letter_flip" text: "B" font: Font{family: "Nope Sans"}}]}"#).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("`Archivo`")), "{errors:?}");
    assert!(errors.iter().any(|d| d.message.contains("Nope Sans") && d.message.contains("Bundled fonts")), "{errors:?}");

    // Installed (every face of the family present): the family draws, quietly.
    for (family, files) in [
        ("archivo", &["Archivo-Variable.ttf", "Archivo-Italic-Variable.ttf"][..]),
        ("cormorant", &["Cormorant-Variable.ttf", "Cormorant-Italic-Variable.ttf"][..]),
    ] {
        std::fs::create_dir_all(installed.join(family)).unwrap();
        for file in files {
            std::fs::write(installed.join(family).join(file), b"face").unwrap();
        }
    }
    let plex = makepad_doc_fonts::family("IBM Plex Mono").unwrap();
    std::fs::create_dir_all(installed.join("ibm-plex-mono")).unwrap();
    for file in plex.files.iter().filter(|f| matches!(f.role, makepad_doc_fonts::FontFileRole::Face { .. })) {
        std::fs::write(installed.join("ibm-plex-mono").join(file.cache_as().rsplit('/').next().unwrap()), b"face").unwrap();
    }
    std::env::set_var("MAKEPAD_FONTS_DIR", &installed);
    let doc = load(DOC);
    assert_eq!(fonts_of(&doc), ["Archivo", "IBM Plex Mono", "Cormorant", "Archivo", "(kit)"]);
    assert!(!doc.warnings.iter().any(|w| w.message.contains("font")), "{:?}", doc.warnings);
    // Installed, `cormorant` still draws the bundled Garamond.
    let doc = load(r#"use mod.motion.*; Motion{size: vec2(640, 360) duration: 1.0 layers: [Text{text: "A" font: Font{family: "cormorant"}}]}"#);
    assert_eq!(fonts_of(&doc), ["Cormorant Garamond"]);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    // The file a face reads: italic and weight pick it (Plex Mono is static).
    assert_eq!(name(makepad_doc_fonts::font_file("Archivo", 800.0, true)), "Archivo-Italic-Variable.ttf");
    assert_eq!(name(makepad_doc_fonts::font_file("IBM Plex Mono", 700.0, true)), "IBMPlexMono-BoldItalic.ttf");
    assert_eq!(name(makepad_doc_fonts::font_file("plex mono", 300.0, false)), "IBMPlexMono-Light.ttf");
    // Bundled fonts are unchanged.
    assert_eq!(name(makepad_doc_fonts::font_file("Cormorant Garamond", 400.0, false)), "CormorantGaramond-Variable.ttf");
    assert_eq!(name(makepad_doc_fonts::font_file("editorial", 400.0, false)), "CormorantGaramond-Variable.ttf");
    assert_eq!(name(makepad_doc_fonts::font_file("Inter", 400.0, false)), "Inter.ttf");
    assert_eq!(makepad_doc_fonts::missing_notices().len(), 3, "installed, nothing more is said");

    std::env::remove_var("MAKEPAD_FONTS_DIR");
    let _ = std::fs::remove_dir_all(&root);
}
