//! The scrubber's filmstrip: the graphic as small frames along its time,
//! drawn by the edit's own renderer (the graphic alone,
//! `clip_films::mini_edit`, with the desk's texts served in place of the
//! library's), one engine per revision retargeted from frame to frame, read
//! back small and laid side by side in one texture.
//!
//! A revision's strip is made once the desk has rested on it for a moment
//! (not while it plays), and kept: the last few strips are cached by
//! revision and tile count, so undo and redo show theirs at once.
//!
//! Main-loop cost: nothing while the strip is made for the shown revision;
//! otherwise one small frame render at a time, with the scrubber drawn
//! again until the strip is whole.

use super::panel_sweep::Served;
use super::super::clip_films::SharedResolver;
use makepad_story_edit_render::engine::EngineOptions;
use makepad_story_edit_render::{CutJob, CutThumbnail};
use makepad_story_cut::CutDoc;
use makepad_widgets::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// How long the desk rests on a revision before its strip is made.
const SETTLE: f64 = 0.5;
/// Strips kept (revision and tile count).
const KEEP: usize = 6;
/// A tile's width in pixels, at most.
const TILE_MAX: u32 = 192;

/// What a strip is made from: the graphic alone as an edit, where its
/// clip starts in that edit, its length, and the desk's texts by asset id.
pub struct FilmSource {
    /// Equal keys, equal strips (the desk's revision and the edit's).
    pub key: u64,
    pub doc: Arc<CutDoc>,
    pub start: f64,
    pub duration: f64,
    pub texts: Arc<HashMap<String, String>>,
    /// The graphic's node and its document as the desk loaded it for this
    /// revision: drawn as it is (the strip never loads the text again).
    pub graphic: (String, Arc<makepad_motion::MotionDoc>),
}

struct Strip {
    key: u64,
    tiles: usize,
    tile: (u32, u32),
    pixels: Vec<u32>,
    texture: Texture,
    whole: bool,
}

struct Making {
    thumb: CutThumbnail,
    /// The tile being drawn.
    at: usize,
}

#[derive(Default)]
pub struct Filmstrip {
    source: Option<FilmSource>,
    /// When the source last changed.
    since: f64,
    strips: VecDeque<Strip>,
    making: Option<Making>,
    /// The tile count and width wanted (the scrubber's size).
    want: (usize, u32),
    failed: Option<u64>,
    timer: Timer,
}

impl Filmstrip {
    /// What the strip shows; `None` clears it (no graphic placed).
    pub fn set_source(&mut self, cx: &mut Cx, source: Option<FilmSource>) {
        if source.as_ref().map(|s| s.key) == self.source.as_ref().map(|s| s.key) {
            return;
        }
        self.source = source;
        self.since = Cx::time_now();
        self.making = None;
        self.timer = cx.start_timeout(SETTLE + 0.05);
    }

    /// The settle timer ran out, or a frame is being made: the host draws
    /// the scrubber again.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.timer.is_event(event).is_some() {
            return true;
        }
        match &mut self.making {
            Some(making) => {
                making.thumb.handle_event(cx, event);
                true
            }
            None => false,
        }
    }

    /// The strip for the scrubber's `strip` rect (with `playing`, nothing
    /// new starts): its texture, whole or filling in. From the host's draw.
    pub fn draw(&mut self, cx: &mut Cx2d, strip: Rect, playing: bool) -> Option<Texture> {
        let source = self.source.as_ref()?;
        let key = source.key;
        let aspect = source.doc.output.width.max(1) as f64 / source.doc.output.height.max(1) as f64;
        let tiles = ((strip.size.x / (strip.size.y * aspect).max(1.0)).round() as usize).clamp(1, 64);
        let width = ((strip.size.x / tiles as f64 * cx.current_dpi_factor()).ceil() as u32).clamp(8, TILE_MAX);
        if self.making.is_some() && self.want != (tiles, width) {
            // Resized while being made: that strip goes.
            self.making = None;
            if let Some(k) = self.strips.iter().position(|s| !s.whole) {
                self.strips.remove(k);
            }
        }
        self.want = (tiles, width);
        if let Some(k) = self.strips.iter().position(|s| s.key == key && s.tiles == tiles) {
            // The newest first.
            let found = self.strips.remove(k).expect("strip");
            self.strips.push_front(found);
        } else if self.making.is_none() && !playing && self.failed != Some(key) && Cx::time_now() - self.since >= SETTLE {
            makepad_value_pickers::panel::host::timed("film.start", || self.start(cx, 0));
        }
        makepad_value_pickers::panel::host::timed("film.step", || self.step(cx));
        // This revision's strip, or until it is made the last one shown at
        // this size (its tiles give way to the new ones as they come).
        self.strips.iter().find(|s| s.key == key && s.tiles == tiles).or_else(|| self.strips.iter().find(|s| s.tiles == tiles)).map(|s| s.texture.clone())
    }

    /// Whether frames are still being made (the host draws again).
    pub fn busy(&self) -> bool {
        self.making.is_some()
    }

    fn time_of(&self, tile: usize) -> f64 {
        let (tiles, _) = self.want;
        let source = self.source.as_ref().expect("source");
        source.start + source.duration * (tile as f64 + 0.5) / tiles as f64
    }

    fn start(&mut self, cx: &mut Cx2d, at: usize) {
        let Some(source) = &self.source else { return };
        let (tiles, width) = self.want;
        let served = Served { inner: SharedResolver::new(Box::new(super::super::store::LazyStoreMedia::new())), texts: source.texts.clone() };
        let job = CutJob { doc: source.doc.clone(), marks: HashMap::new(), resolver: Box::new(served), audio: false };
        let t = self.time_of(at);
        let mut thumb = CutThumbnail::with_options(cx, job, t, width, EngineOptions { max_loads: 1, budget: None });
        thumb.give_graphic(&source.graphic.0, (*source.graphic.1).clone());
        self.making = Some(Making { thumb, at });
        let height = ((width as f64) * source.doc.output.height.max(1) as f64 / source.doc.output.width.max(1) as f64).round().max(1.0) as u32;
        // Starts as the last strip of this size (each tile replaced as it is
        // drawn), else dark.
        let pixels = self.strips.iter().find(|s| s.tiles == tiles && s.tile == (width, height)).map(|s| s.pixels.clone()).unwrap_or_else(|| vec![0xff10_1216u32; tiles * width as usize * height as usize]);
        let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: tiles * width as usize, height: height as usize, data: Some(pixels.clone()), updated: TextureUpdated::Full }).label(cx, "edit/filmstrip");
        self.strips.push_front(Strip { key: source.key, tiles, tile: (width, height), pixels, texture, whole: false });
        while self.strips.len() > KEEP {
            self.strips.pop_back();
        }
    }

    fn step(&mut self, cx: &mut Cx2d) {
        let Some(making) = &mut self.making else { return };
        makepad_value_pickers::panel::host::timed("film.thumb_draw", || making.thumb.draw(cx));
        let Some(result) = making.thumb.result().cloned() else { return };
        let at = making.at;
        let key = self.source.as_ref().map_or(0, |s| s.key);
        match result {
            Ok(thumb) => {
                let Some(strip) = self.strips.front_mut().filter(|s| s.key == key && !s.whole) else {
                    self.making = None;
                    return;
                };
                let (tw, th) = strip.tile;
                let row = strip.tiles * tw as usize;
                for y in 0..(th.min(thumb.height)) as usize {
                    for x in 0..(tw.min(thumb.width)) as usize {
                        let p = &thumb.rgba[(y * thumb.width as usize + x) * 4..][..4];
                        strip.pixels[y * row + at * tw as usize + x] = 0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32;
                    }
                }
                strip.texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width: row, height: th as usize, data: Some(strip.pixels.clone()), updated: TextureUpdated::Full }).label(cx, "edit/filmstrip");
                if at + 1 < strip.tiles {
                    let t = self.time_of(at + 1);
                    let making = self.making.as_mut().expect("making");
                    if making.thumb.retarget(t) {
                        making.at = at + 1;
                    } else {
                        self.making = None;
                    }
                } else {
                    strip.whole = true;
                    self.making = None;
                }
            }
            Err(error) => {
                log!("filmstrip: frame {at} of {}: {error}", self.want.0);
                self.failed = Some(key);
                self.making = None;
                if let Some(k) = self.strips.iter().position(|s| s.key == key && !s.whole) {
                    self.strips.remove(k);
                }
            }
        }
    }
}
