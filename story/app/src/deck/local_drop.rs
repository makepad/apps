//! A file dragged in from the OS: onto the slide list it becomes a slide after
//! the one shown (or at the drop place), onto the picture a layer of the
//! shown slide. The library lists decks only, so this is how media comes in:
//! the file is put in the media library by reference (the VJ import, the
//! same alias per path), then added to the deck like any library media.
use super::*;
use makepad_asset_connection as asset_connection;
use super::super::document_tree::{self, Operation};
use makepad_asset_importer::media_import::{self, MediaClass};
use std::sync::Mutex;

/// Where a dropped file goes in the deck, decided at the drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// A slide at this index.
    Slide(usize),
    /// A layer of this slide.
    Layer(usize),
}

/// A dropped file in the library: its revision, or why not.
struct Landed {
    result: Result<(AssetRevisionRef, MediaClass, String, Option<f64>), String>,
    place: Place,
}

static LANDED: Mutex<Vec<Landed>> = Mutex::new(Vec::new());

/// The cut that shows a library media file under `path` of the deck's tree.
fn cut_of(class: MediaClass, path: String, seconds: Option<f64>) -> Option<NewCut> {
    match class {
        MediaClass::Image => Some(NewCut::ImagePath { path, fit: ImageFit::Contain }),
        MediaClass::Video => Some(NewCut::VideoPath { path, play: None, seconds, audio: false }),
        MediaClass::Audio => None,
    }
}

/// Publish `file` by reference (its alias is its path: dropping it again finds it)
/// and say which revision it is.
fn import(file: &media_import::MediaFile) -> Result<(AssetRevisionRef, Option<f64>), String> {
    let origin = asset_connection::origin();
    let mut connection = None;
    let client = asset_connection::connected(&mut connection, &origin)?;
    let root = file.path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    if let media_import::FileOutcome::Failed(error) = media_import::import_file(client, file, &media_import::personal_rights(&root)) {
        let hint = if error == "unusable image" { " (PNG or JPEG, 256 to 4096 pixels each way)" } else { "" };
        return Err(format!("{}: {error}{hint}", file.stem));
    }
    let alias = media_import::alias_for(&file.path).ok_or("no alias for this path")?;
    let head = client.resolve_alias(&alias).map_err(|error| error.to_string())?;
    let reference = AssetRevisionRef { asset_id: head.asset_id, revision: head.head_revision };
    let seconds = client.resolve_ref(&reference).ok().map(|manifest| manifest.metrics.media_millis).filter(|millis| *millis > 0).map(|millis| millis as f64 / 1000.0);
    Ok((reference, seconds))
}

impl DeckPage {
    /// The drag and drop events of files from the OS, while a deck is open.
    pub(super) fn handle_file_drop(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event) {
        let files = |items: &[DragItem]| -> Vec<std::path::PathBuf> {
            items.iter().filter_map(|item| match item {
                DragItem::FilePath { path, internal_id: None } => Some(std::path::PathBuf::from(path)),
                _ => None,
            }).collect()
        };
        match event {
            Event::Drag(drag) if !files(&drag.items).is_empty() => {
                let target = self.drop_target(cx, ui, drag.abs);
                self.drop_hint(cx, ui, if matches!(target, DropTarget::List) { Some(drag.abs) } else { None });
                if !matches!(target, DropTarget::Nowhere) {
                    let mut handled = drag.handled.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !*handled {
                        *handled = true;
                        *drag.response.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = DragResponse::Copy;
                    }
                }
            }
            Event::DragEnd => self.drop_hint(cx, ui, None),
            Event::Drop(drop) => {
                let dropped = files(&drop.items);
                if dropped.is_empty() {
                    return;
                }
                self.drop_hint(cx, ui, None);
                let target = self.drop_target(cx, ui, drop.abs);
                if matches!(target, DropTarget::Nowhere) {
                    return;
                }
                {
                    let mut handled = drop.handled.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if *handled {
                        return;
                    }
                    *handled = true;
                }
                for path in dropped {
                    if let Err(error) = self.add_dropped(cx, ui, &path, drop.abs, target) {
                        self.load_error = error;
                    }
                }
            }
            _ => {}
        }
    }

    fn drop_target(&self, cx: &Cx, ui: &WidgetRef, abs: Vec2d) -> DropTarget {
        let over_list = self.nav == Nav::Slides && ui.widget(cx, ids!(deck_sorter)).borrow::<DeckSorter>().is_some_and(|sorter| sorter.contains(cx, abs));
        if over_list {
            DropTarget::List
        } else if ui.widget(cx, ids!(deck_nav_tabs)).area().rect(cx).contains(abs) {
            DropTarget::Tabs
        } else if ui.widget(cx, ids!(deck_view)).area().rect(cx).contains(abs) {
            DropTarget::Picture
        } else {
            DropTarget::Nowhere
        }
    }

    fn add_dropped(&mut self, cx: &mut Cx, ui: &WidgetRef, file: &std::path::Path, abs: Vec2d, target: DropTarget) -> Result<(), String> {
        if self.session.as_ref().and_then(|session| session.tree.as_ref()).is_none() {
            return Err("Make or open a deck first (+ New deck)".into());
        }
        let found = media_import::scan_paths(&[file.to_path_buf()]);
        let Some(media) = found.files.into_iter().find(|media| media.class != MediaClass::Audio) else {
            return Err(found.skipped.first().map_or("Only PNG or JPEG images and MP4 videos are dropped on a deck".into(), |skipped| format!("{}: {}", file.display(), skipped.why)));
        };
        let current = self.doc.as_ref().and_then(|doc| ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()).filter(|index| *index < doc.slides.len()));
        let place = match target {
            DropTarget::List | DropTarget::Tabs => {
                let slides = self.doc.as_ref().map_or(0, |doc| doc.slides.len());
                let slot = if target == DropTarget::List { ui.widget(cx, ids!(deck_sorter)).borrow::<DeckSorter>().and_then(|sorter| sorter.drop_slot(cx, abs)) } else { current.map(|index| index + 1) };
                Place::Slide(slot.unwrap_or(slides).min(slides))
            }
            DropTarget::Picture => Place::Layer(current.ok_or("There is no slide to put it on")?),
            DropTarget::Nowhere => return Ok(()),
        };
        self.load_error = format!("Adding {} to the library…", media.stem);
        let options = makepad_widgets::makepad_platform::thread::ThreadOptions { name: Some(Arc::from("deck-drop")), stack_size: None, priority: CxThreadPriority::Utility };
        let handle = cx.thread_spawner().spawn_worker(options, move || {
            let result = import(&media).map(|(reference, seconds)| (reference, media.class, media.stem.clone(), seconds));
            LANDED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(Landed { result, place });
            makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
        }).map_err(|error| format!("import worker unavailable: {error:?}"))?;
        handle.detach();
        Ok(())
    }

    /// Every pump: the files that reached the library become slides or layers.
    pub(super) fn pump_dropped(&mut self, cx: &mut Cx) {
        let landed = std::mem::take(&mut *LANDED.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for Landed { result, place } in landed {
            self.load_error.clear();
            let placed = result.and_then(|(reference, class, title, seconds)| {
                let tree = self.session.as_ref().and_then(|session| session.tree.as_ref()).ok_or("The deck was closed before the file was added")?;
                let path = document_tree::media_stem(tree, &title, "media");
                let cut = cut_of(class, path.clone(), seconds).ok_or("Only images and videos are dropped on a deck")?;
                let op = match place {
                    Place::Layer(index) => DeckOp::AddLayer { index, layer: cut },
                    Place::Slide(slot) => {
                        let slot = slot.min(self.doc.as_ref().map_or(0, |doc| doc.slides.len()));
                        let name = self.fresh_slide_name();
                        self.select = Some(slot);
                        if slot == 0 { DeckOp::AddSlideFirst { name, cut } } else { DeckOp::AddSlide { after: Some(slot - 1), name, cut } }
                    }
                };
                self.pending.push_back(Operation::Import { reference, path, op });
                Ok::<(), String>(())
            });
            if let Err(error) = placed {
                self.load_error = error;
            }
            cx.redraw_all();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DropTarget {
    List,
    Tabs,
    Picture,
    Nowhere,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_image_or_video_becomes_a_cut_and_sound_does_not() {
        assert_eq!(cut_of(MediaClass::Image, "media/a.png".into(), None), Some(NewCut::ImagePath { path: "media/a.png".into(), fit: ImageFit::Contain }));
        assert_eq!(cut_of(MediaClass::Video, "media/a.mp4".into(), Some(3.0)), Some(NewCut::VideoPath { path: "media/a.mp4".into(), play: None, seconds: Some(3.0), audio: false }));
        assert_eq!(cut_of(MediaClass::Audio, "media/a.wav".into(), None), None);
    }
}
