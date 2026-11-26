//! One native GPU frame per slide, using the player's renderer. A preview
//! owns only its current slide engine and releases it before the next slide.
use super::render::SlideRenderer;
use makepad_story_deck::{DeckDoc, SlideCut};
use makepad_story_edit_render::{CutJob, PreviewStatus};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::sync::Arc;

pub struct Preview {
    doc: Arc<DeckDoc>,
    index: usize,
    renderer: Option<SlideRenderer>,
    frames: Vec<Json>,
    diagnostics: Vec<Json>,
}

impl Preview {
    pub fn new(doc: Arc<DeckDoc>) -> Self {
        Self { doc, index: 0, renderer: None, frames: Vec::new(), diagnostics: Vec::new() }
    }

    pub fn done(&self) -> bool { self.index == self.doc.slides.len() }

    pub fn event(&mut self, cx: &mut Cx, event: &Event) {
        if let Some(renderer) = &mut self.renderer { renderer.handle_event(cx, event); }
    }

    fn diagnostic(&mut self, message: String, severity: &str) {
        let slide = &self.doc.slides[self.index];
        self.diagnostics.push(json::obj(vec![("path", json::s(slide.graphic.clone().unwrap_or_else(|| self.doc.tree.as_ref().map_or("deck.splash", |tree| tree.document.as_str()).into()))), ("slide", json::s(slide.name.clone())), ("line", Json::Int(if slide.graphic.is_some() { 1 } else { slide.line as i64 })), ("severity", json::s(severity)), ("message", json::s(message))]));
    }

    pub fn draw(&mut self, cx: &mut Cx2d) {
        if self.done() { return; }
        if self.renderer.is_none() {
            let SlideCut::Inline(cut) = &self.doc.slides[self.index].cut else {
                self.diagnostic("the slide has no loaded tree cut".into(), "error");
                self.index += 1;
                return;
            };
            let doc = Arc::new((**cut).clone());
            if doc.nodes.iter().any(|node| matches!(node.kind, makepad_story_cut::CutNodeKind::AppView { .. } | makepad_story_cut::CutNodeKind::Camera(_) | makepad_story_cut::CutNodeKind::Code(_))) {
                self.diagnostic("This frame has live inputs; app, camera and code captures must also be checked while presenting.".into(), "warning");
            }
            self.renderer = Some(SlideRenderer::new(cx, CutJob { doc, marks: Default::default(), resolver: Box::new(crate::cut::store::LazyStoreMedia::new()), audio: false }));
        }
        let renderer = self.renderer.as_mut().unwrap();
        renderer.draw(cx);
        let result = match renderer.status() {
            PreviewStatus::Failed(error) => Some(Err(error)),
            PreviewStatus::Idle if renderer.exact() => Some(Ok(())),
            _ => None,
        };
        if let Some(result) = result {
            if let Err(error) = &result { self.diagnostic(error.clone(), "error"); }
            let slide = &self.doc.slides[self.index];
            self.frames.push(json::obj(vec![("slide", json::s(slide.name.clone())), ("path", json::s(slide.graphic.clone().unwrap_or_else(|| self.doc.tree.as_ref().map_or("deck.splash", |tree| tree.document.as_str()).into()))), ("rendered", Json::Bool(result.is_ok()))]));
            self.renderer = None;
            self.index += 1;
        }
    }

    pub fn result(self) -> Json {
        let clean = !self.diagnostics.iter().any(|diag| diag.get("severity").and_then(Json::as_str) == Some("error"));
        json::obj(vec![("frames", Json::Arr(self.frames)), ("diagnostics", Json::Arr(self.diagnostics)), ("clean", Json::Bool(clean))])
    }
}
