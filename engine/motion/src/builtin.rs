//! Built-in documents: what an edit's `Title` lowers to (EDITS.md §10.2:
//! "`Title` becomes a Graphic with the built-in \"title\" document").

/// The built-in documents by name: "title", and M4's app-run overlays
/// ("overlay/<id>", `crate::bind`).
pub fn source(name: &str) -> Option<&'static str> {
    match name {
        "title" => Some(TITLE),
        _ => crate::bind::tools::builtin(name),
    }
}

/// The edit's `Title`: its text centred on `anchor` (fractions of the frame)
/// at `font_size` pixels, white, as the compositor drew titles before
/// graphics existed. The document is 1 x 1 unit per output pixel: the
/// renderer draws it at the output's own size, so `size` is replaced by
/// the host with the output size (video_edit's `graphic::title_params`).
pub const TITLE: &str = r#"use mod.motion.*
Motion{
    label: "Title"
    size: in(@frame)
    duration: 86400.0
    inputs: {
        text: input(@text, "", "The title")
        frame: input(@vec2, vec2(1920, 1080), "The output size in pixels")
        position: input(@vec2, vec2(960, 540), "Where the title is centred, in pixels")
        font_size: input(@number, 48, "Size in output pixels")
    }
    layers: [
        Text{
            name: "title"
            text: in(@text)
            font: Font{family: "IBM Plex Sans" weight: 600} 
            size: in(@font_size)
            align: @center valign: @center
            at: in(@position)
            color: #ffffff
        }
    ]
}
"#;
