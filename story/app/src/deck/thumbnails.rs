//! Passive shared stills and visible-card demand. This type owns no engine.
use makepad_story_deck::DeckDoc;
use makepad_widgets::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use super::super::still_cache::Size;

pub type SharedThumbnails = Rc<RefCell<Thumbnails>>;

#[derive(Default)]
pub struct Thumbnails {
    pub doc: Option<Rc<DeckDoc>>,
    pub current: usize,
    pub visible: Vec<usize>,
    pub sizes: HashMap<usize, Size>,
    pub presenter_size: Option<Size>,
    pub sorter_visible: bool,
    pub presenter_visible: bool,
    pub textures: HashMap<usize, Texture>,
    pub presenter_textures: HashMap<usize, Texture>,
    pub revision: u64,
}

/// Live inputs never instantiate a thumbnail renderer.
pub fn live_label(slide: &makepad_story_deck::Slide) -> Option<String> {
    let mut names = slide.apps.iter().map(|app| app.app.clone()).collect::<Vec<_>>();
    if let makepad_story_deck::SlideCut::Inline(cut) = &slide.cut {
        for node in &cut.nodes {
            let name = match &node.kind {
                makepad_story_cut::CutNodeKind::Camera(_) => Some("Camera"),
                makepad_story_cut::CutNodeKind::Code(_) => Some("Source"),
                makepad_story_cut::CutNodeKind::AppView { app, .. } if names.is_empty() => Some(app.as_str()),
                _ => None,
            };
            if let Some(name) = name { if !names.iter().any(|n| n == name) { names.push(name.to_string()); } }
        }
    }
    (!names.is_empty()).then(|| names.join(", "))
}
