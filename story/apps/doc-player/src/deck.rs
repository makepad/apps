//! A presentation played by itself: the deck's own clock and slide changes
//! (`makepad_story_deck::Deck`), each slide's cut drawn by its own engine,
//! the slide being left holding its last picture while the change mixes the
//! two with the deck's transition preset (`SlideMixer`), as Stage's DECK
//! player draws them. Instead of a presenter's key, every stop (a beat, a
//! slide's end) holds `dwell` seconds and then the deck goes on.
//!
//! Slides that host live apps, cameras or code panes show what their cut
//! draws without them (their placeholders): a player runs no apps.

use crate::picture::Picture;
use makepad_story_deck::{Deck, DeckDoc, Loop, SlideCut};
use makepad_story_edit_render::compose::Painted;
use makepad_story_edit_render::mixer::PresetState;
use makepad_story_edit_render::{CutJob, LocalMedia, SlideMixer};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;

/// The longest frame step a transition effect is driven with (Stage's).
const MAX_STEP: f64 = 0.25;

pub struct DeckRun {
    doc: Arc<DeckDoc>,
    deck: Deck,
    media: LocalMedia,
    locked: bool,
    dwell: f64,
    looping: bool,
    /// The shown slide's engine, by slide.
    pictures: HashMap<usize, Picture>,
    /// The slide being left: its last picture, and its engine kept so that
    /// picture stays whole while the change runs.
    outgoing: Option<(usize, Texture, Option<Picture>)>,
    mixer: SlideMixer,
    /// The mix recorded and painting, for the deck time it was asked at.
    mix_recorded: Option<f64>,
    shown_mix: Option<(f64, Texture)>,
    /// The deck time the clock was last advanced to (the schedule runs once
    /// per time), and the last draw's, for the transition's step.
    scheduled: Option<f64>,
    last_now: Option<f64>,
    /// Since when the shown slide stands at a stop.
    holding_since: Option<f64>,
    /// When the shown slide's last beat was released (or it was entered).
    released_at: f64,
    /// Played past the last slide's end.
    pub finished: bool,
}

impl DeckRun {
    pub fn new(cx: &mut Cx, doc: Arc<DeckDoc>, media: LocalMedia, locked: bool, dwell: f64, looping: bool, slide: usize, now: f64) -> Self {
        let mut deck = Deck::new(doc.slides.len(), now);
        if slide > 0 {
            deck.enter_from_outside(slide, makepad_story_deck::Transition { seconds: 0.0, effect: String::new(), dials: Vec::new() }, now);
        }
        Self {
            doc, deck, media, locked, dwell, looping, pictures: HashMap::new(), outgoing: None, mixer: SlideMixer::new(cx),
            mix_recorded: None, shown_mix: None, scheduled: None, last_now: None, holding_since: None, released_at: now, finished: false,
        }
    }

    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        for picture in self.pictures.values_mut() {
            picture.handle_event(cx, event);
        }
        self.mixer.handle_event(cx, event);
    }

    pub fn current(&self) -> usize {
        self.deck.current()
    }

    /// Advance the deck's clock to `now`: a stop held `dwell` seconds goes
    /// on (Next), past the last slide the deck ends (or starts over).
    fn schedule(&mut self, now: f64) {
        if self.scheduled == Some(now) {
            return;
        }
        self.scheduled = Some(now);
        self.deck.tick(now);
        if self.deck.incoming().is_some() {
            self.holding_since = None;
            return;
        }
        if self.deck.incoming().is_none() && self.outgoing.is_some() {
            self.outgoing = None;
            self.shown_mix = None;
        }
        let doc = self.doc.clone();
        let current = self.deck.current();
        let Some(slide) = doc.slides.get(current) else { return };
        let due = if self.deck.holding(&doc, now) {
            let since = *self.holding_since.get_or_insert(now);
            now - since >= self.dwell
        } else {
            self.holding_since = None;
            // A slide whose story loops never holds: it goes on after its
            // first pass from the last beat (or the start), and the dwell.
            let rest = slide.cut.duration().unwrap_or(0.0) - slide.beats.last().copied().unwrap_or(0.0);
            slide.play() != Loop::Hold && self.deck.beat() >= slide.beats.len() && now - self.released_at >= rest.max(0.0) + self.dwell
        };
        if !due {
            return;
        }
        self.holding_since = None;
        if self.deck.next_leaves(&doc, now) && current + 1 >= doc.slides.len() {
            if !self.looping {
                self.finished = true;
                return;
            }
            self.deck.first(&doc, now);
        } else {
            let beat = self.deck.beat();
            self.deck.next(&doc, now);
            if self.deck.beat() != beat {
                self.released_at = now;
            }
        }
        if self.deck.current() != current {
            self.released_at = now;
            let picture = self.pictures.remove(&current);
            let last = picture.as_ref().and_then(|picture| picture.shown().map(|(_, texture)| texture.clone()));
            self.outgoing = last.map(|texture| (current, texture, picture));
            self.mix_recorded = None;
        }
    }

    /// The picture of deck time `now`: `Ok(Some(_))` once it is whole
    /// (locked), else the last whole picture.
    pub fn draw(&mut self, cx: &mut Cx2d, now: f64) -> Result<Option<Texture>, String> {
        self.schedule(now);
        let step = self.last_now.map_or(1.0 / 60.0, |last| (now - last).clamp(1.0 / 240.0, MAX_STEP));
        let doc = self.doc.clone();
        let current = self.deck.current();
        if !self.pictures.contains_key(&current) {
            let SlideCut::Inline(cut) = &doc.slides[current].cut else { return Err(format!("slide {current} has no cut")) };
            let job = CutJob { doc: Arc::new((**cut).clone()), marks: HashMap::new(), resolver: Box::new(self.media.clone()), audio: false };
            self.pictures.insert(current, Picture::new(cx, job, self.locked));
        }
        let picture = self.pictures.get_mut(&current).expect("the shown slide's engine");
        let t = self.deck.slide_time(&doc, current, now);
        let live = self.deck.live_time(current, now);
        let n = picture.frame_of(t);
        let Some(b) = picture.draw(cx, n, Some(live))? else { return Ok(None) };
        let Some(incoming) = self.deck.incoming().cloned() else {
            self.last_now = Some(now);
            return Ok(Some(b));
        };
        let Some((_, a, _)) = self.outgoing.as_ref().filter(|(from, _, _)| *from == incoming.from) else {
            self.last_now = Some(now);
            return Ok(Some(b));
        };
        let a = a.clone();
        let effect = incoming.transition.effect.clone();
        self.mixer.prepare(cx, &effect);
        // Locked time mixes with the preset itself, never the dissolve that
        // stands in while it builds.
        if self.locked && self.mixer.state(&effect) == PresetState::Building {
            return Ok(None);
        }
        if let Some(asked) = self.mix_recorded {
            match self.mixer.painted(cx) {
                Painted::Whole => {
                    self.mix_recorded = None;
                    self.shown_mix = self.mixer.output().cloned().map(|texture| (asked, texture));
                }
                Painted::Painting if self.mixer.stale(cx) => self.mix_recorded = None,
                Painted::Painting => {
                    self.mixer.reattach(cx);
                    return Ok(if self.locked { None } else { self.shown_mix.as_ref().map(|(_, texture)| texture.clone()) });
                }
            }
        }
        if let Some((asked, texture)) = &self.shown_mix {
            if *asked == now {
                self.last_now = Some(now);
                return Ok(Some(texture.clone()));
            }
        }
        let progress = self.deck.progress(now).unwrap_or(1.0);
        let dial = |name: &str| incoming.transition.dial(name).unwrap_or(-1.0);
        let dials = [dial("p1"), dial("p2"), dial("p3")];
        self.mixer.hold_output(self.shown_mix.as_ref().map(|(_, texture)| texture.clone()));
        self.mixer.mix(cx, &a, &b, &effect, progress, dials, doc.output, now - incoming.started, step);
        self.mix_recorded = Some(now);
        if !self.locked {
            self.last_now = Some(now);
        }
        Ok(if self.locked { None } else { self.shown_mix.as_ref().map(|(_, texture)| texture.clone()).or(Some(a)) })
    }
}
