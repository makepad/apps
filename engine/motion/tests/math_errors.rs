//! A formula with a command the typesetter does not know is an error that
//! names it (it would otherwise show as its own text); set symbols work.
use makepad_motion::*;

#[test]
fn unknown_tex_commands_are_named_and_set_symbols_typeset() {
    let err = MotionDoc::load(r#"use mod.motion.*
Motion{duration: 1.0 layers: [Math{tex: "A \\capx B" size: 48 color: #fff}]}"#).err().expect("an unknown command is an error");
    let text = err.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n");
    assert!(text.contains("\\capx") && text.contains("does not know"), "{text}");
    let doc = MotionDoc::load(r#"use mod.motion.*
Motion{duration: 1.0 layers: [Math{tex: "A \\cap B \\cup C \\wedge D \\vee E" size: 48 color: #fff}]}"#).unwrap_or_else(|d| panic!("{d:?}"));
    assert!(doc.warnings.is_empty());
}
