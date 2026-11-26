//! The media layers: Image, Video, read from their components.

use super::{At, Reader};
use crate::layers::{kind, Field, Image, Video};
use crate::model::*;
use crate::obj::Look;

impl Image {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let src = source(c, at, &self.src, "Image", true);
        let size = self.size.size(c, at, "size");
        let fit = self.fit.pick(c, at, "fit", Fit::Fit);
        if src == Media::Backdrop && size.is_none() {
            c.err(at, "size", "an `@backdrop` Image needs its `size` (the lens)".into(), Some("`Image{src: @backdrop size: vec2(300, 300) zoom: 2 focus: in(@target)}`"));
        }
        let zoom = self.zoom.prop(c, at, "zoom", 1.0);
        let focus = self.focus.opt(c, at, "focus");
        let mesh = c.mesh_grid(at, self.mesh.look(c));
        LayerKind::Image { src, size, fit, zoom, focus, mesh }
    }
}

impl Video {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let src = source(c, at, &self.src, "Video", false);
        let size = self.size.size(c, at, "size");
        let fit = self.fit.pick(c, at, "fit", Fit::Fit);
        let source_in = self.r#in.time(c, at, "in").unwrap_or(Time::seconds(0.0));
        let speed = match self.speed.look(c) {
            None => 1.0,
            Some(value) => match value.num() {
                Some(s) if s > 0.0 => s,
                _ => {
                    c.err(at, "speed", format!("`speed` is a positive number, not {}", value.kind()), None);
                    1.0
                }
            },
        };
        LayerKind::Video { src, size, fit, source_in, speed }
    }
}

/// A media layer's `src`, which it needs (`@backdrop` for an Image: the
/// frame under it).
fn source(c: &mut Reader, at: &At, src: &Field<kind::Media>, layer: &str, backdrop: bool) -> Media {
    match src.look(c) {
        Some(Look::Id(id)) if id == "backdrop" && backdrop => Media::Backdrop,
        Some(_) => src.media(c, at, "src", false).unwrap_or(Media::Empty),
        None => {
            c.err(at, "src", format!("an {layer} needs `src: asset(id, rev)` or `src: in(@name)`"), None);
            Media::Empty
        }
    }
}
