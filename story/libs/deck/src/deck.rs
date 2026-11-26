//! Where a deck is: which slide shows, since when, and the slide
//! change under way. Pure state on a clock the host passes in; the player
//! draws what it says.

use crate::{loop_wrap, Loop, Slide, Transition, DeckDoc};

/// The longest slide change a key press starts: a press must read as
/// instant, so a document's longer transition is shortened to this.
pub const MAX_CHANGE_SECONDS: f64 = 0.35;

/// The longest change into another deck (a link or its return): a jump
/// between talks may take the slide's own, slower transition.
pub const MAX_JUMP_SECONDS: f64 = 1.5;

/// [`Incoming::from`] of a change that comes from another deck: the
/// player holds the left deck's last picture as the outgoing frame.
pub const OUTSIDE: usize = usize::MAX;

/// How long an early Next takes to carry a playing segment to its stop:
/// the story runs on, faster, and arrives there (no cut).
pub const RUSH_SECONDS: f64 = 0.25;

/// Story seconds within which Previous counts the story as being at a beat
/// (or at the start) already.
const PREVIOUS_EPSILON: f64 = 0.05;

/// How far short of the cut's end a seek stops.
pub const SEEK_END_MARGIN: f64 = 0.001;

/// The segment running toward `beat` (the first beat not released), or the
/// play-out when every beat is released. Going back starts at a hold point.
#[derive(Clone, Debug, PartialEq)]
struct StoryClock {
    beat: usize,
    started_at: f64,
    started_time: f64,
    /// Story seconds per second: 1, or faster while an early Next carries the
    /// segment to its hold.
    speed: f64,
    /// The cut's first pass ended and the story sits at its end (the settled
    /// final frame) whatever the slide's loop: the end stop an arrow brought
    /// it to. Only a scrub, Play or an arrow moves the story off it. Live apps and
    /// ambient motion keep running.
    to_end: bool,
}

impl StoryClock {
    fn new(now: f64) -> Self {
        Self { beat: 0, started_at: now, started_time: 0.0, speed: 1.0, to_end: false }
    }

    fn at(beat: usize, now: f64, time: f64) -> Self {
        Self { beat, started_at: now, started_time: time, speed: 1.0, to_end: false }
    }

    /// The end stop of a slide with `beats` beats and a cut of `duration`.
    fn at_end(beats: usize, now: f64, duration: f64) -> Self {
        Self { beat: beats, started_at: now, started_time: duration, speed: 1.0, to_end: true }
    }

    /// Carried to the end stop from `time`, arriving in [`RUSH_SECONDS`] (never slower than playing).
    fn rush_to_end(beats: usize, now: f64, time: f64, duration: f64) -> Self {
        Self { beat: beats, started_at: now, started_time: time, speed: ((duration - time) / RUSH_SECONDS).max(1.0), to_end: true }
    }

    fn time(&self, slide: &Slide, now: f64) -> f64 {
        let t = self.started_time + (now - self.started_at).max(0.0) * self.speed;
        if let Some(beat) = slide.beats.get(self.beat) {
            return t.min(*beat);
        }
        let Some(duration) = slide.cut.duration() else { return t };
        if self.to_end { return t.min(duration); }
        if !slide.beats.is_empty() {
            // Finish the first pass continuously before starting `play`.
            if t < duration { return t; }
            if slide.play() == Loop::Hold { return duration; }
            return loop_wrap(&slide.play(), duration, t - duration);
        }
        loop_wrap(&slide.play(), duration, t)
    }
}

/// A slide change under way. Each slide keeps its own story and live clocks,
/// including any hold on the slide being left.
#[derive(Clone, Debug, PartialEq)]
pub struct Incoming {
    /// The slide being left.
    pub from: usize,
    /// When the slide being left was entered (its clock keeps running).
    pub from_entered_at: f64,
    from_story: StoryClock,
    /// When the change began.
    pub started: f64,
    pub transition: Transition,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deck {
    slides: usize,
    current: usize,
    entered_at: f64,
    story: StoryClock,
    incoming: Option<Incoming>,
    black: bool,
    /// The play bar's loop: a slide that holds its last frame starts over
    /// instead (a viewing choice of the editor, never kept by a slide).
    looped: bool,
}

impl Deck {
    pub fn new(slides: usize, now: f64) -> Deck {
        Deck { slides, current: 0, entered_at: now, story: StoryClock::new(now), incoming: None, black: false, looped: false }
    }

    pub fn current(&self) -> usize {
        self.current
    }

    pub fn incoming(&self) -> Option<&Incoming> {
        self.incoming.as_ref()
    }

    /// When the shown slide was entered.
    pub fn entered_at(&self) -> f64 {
        self.entered_at
    }

    /// Reload the same slide at `current` (clamped), retaining both clocks
    /// and its beat segment, with no transition under way.
    pub fn resume(&mut self, current: usize, previous: &Deck) {
        self.current = current.min(self.slides.saturating_sub(1));
        self.entered_at = previous.entered_at;
        self.story = previous.story.clone();
        self.incoming = None;
    }

    /// The first beat not released, or the beat count during play-out.
    pub fn beat(&self) -> usize {
        self.story.beat
    }

    /// Seconds since slide entry, independent of holds, loops and loading.
    pub fn live_time(&self, index: usize, now: f64) -> f64 {
        let entered = if index == self.current {
            self.entered_at
        } else if let Some(incoming) = self.incoming.as_ref().filter(|incoming| incoming.from == index) {
            incoming.from_entered_at
        } else {
            return 0.0;
        };
        (now - entered).max(0.0)
    }

    /// A pending beat has been reached; the picture can sleep unless its
    /// contents read the live clock or have other work in flight.
    pub fn holding(&self, doc: &DeckDoc, now: f64) -> bool {
        let Some(slide) = doc.slides.get(self.current) else { return true };
        if self.paused() { return true; }
        if self.story.to_end && slide.cut.duration().is_some_and(|duration| self.story.time(slide, now) >= duration) { return true; }
        if let Some(beat) = slide.beats.get(self.story.beat) {
            return self.story.time(slide, now) >= *beat;
        }
        !self.looped && slide.play() == Loop::Hold && slide.cut.duration().is_some_and(|duration| self.story.time(slide, now) >= duration)
    }

    /// The shown slide's story is stopped where it is (the play bar's
    /// pause, or a scrub in progress). Entering a slide plays it again.
    pub fn paused(&self) -> bool {
        self.story.speed == 0.0
    }

    pub fn looped(&self) -> bool {
        self.looped
    }

    pub fn set_looped(&mut self, looped: bool) {
        self.looped = looped;
    }

    /// Stop the shown slide's story at its time now.
    pub fn pause(&mut self, doc: &DeckDoc, now: f64) {
        let time = self.slide_time(doc, self.current, now);
        self.story = StoryClock { beat: self.story.beat, started_at: now, started_time: time, speed: 0.0, to_end: self.story.to_end };
    }

    /// Run the shown slide's story on from its time now.
    pub fn play(&mut self, doc: &DeckDoc, now: f64) {
        let time = self.slide_time(doc, self.current, now);
        // Played from the end stop, a looping slide loops again.
        let to_end = self.story.to_end && doc.slides.get(self.current).and_then(|slide| slide.cut.duration()).is_some_and(|duration| time < duration);
        self.story = StoryClock { beat: self.story.beat, started_at: now, started_time: time, speed: 1.0, to_end };
    }

    /// Put the shown slide's story at `time` seconds (inside the cut): the
    /// segment running toward the first beat not before it, or the play-out
    /// past the last. Playing runs on from there; not playing, it stays.
    /// A slide change under way ends. The beat index follows, so Next from
    /// here releases the beat the story stands at.
    pub fn seek(&mut self, doc: &DeckDoc, time: f64, playing: bool, now: f64) {
        let Some(slide) = doc.slides.get(self.current) else { return };
        // Just short of the end: at the end itself a looping cut wraps to its start.
        let end = slide.cut.duration().map_or(0.0, |duration| (duration - SEEK_END_MARGIN).max(0.0));
        let time = time.clamp(0.0, end);
        let beat = slide.beats.iter().position(|beat| *beat >= time).unwrap_or(slide.beats.len());
        self.story = StoryClock { beat, started_at: now, started_time: time, speed: if playing { 1.0 } else { 0.0 }, to_end: false };
        self.incoming = None;
    }

    /// With the loop on, a slide that holds its last frame starts over from
    /// its beginning (the first beat is held again).
    pub fn wrap(&mut self, doc: &DeckDoc, now: f64) {
        if !self.looped || self.paused() { return; }
        let Some(slide) = doc.slides.get(self.current) else { return };
        if self.story.beat < slide.beats.len() || slide.play() != Loop::Hold { return; }
        // Sitting at the end stop (an arrow put it there) it stays.
        if !self.story.to_end && slide.cut.duration().is_some_and(|duration| self.story.time(slide, now) >= duration) {
            self.story = StoryClock::new(now);
        }
    }

    /// What Next will play toward once the shown stop is left: the following
    /// beat, the end of the cut, or the next slide at its entry time.
    pub fn next_preview(&self, doc: &DeckDoc, now: f64) -> Option<(usize, f64)> {
        let slide = doc.slides.get(self.current)?;
        if self.story.beat < slide.beats.len() {
            let t = slide.beats.get(self.story.beat + 1).copied()
                .or_else(|| slide.cut.duration()).unwrap_or(slide.beats[self.story.beat]);
            return Some((self.current, t));
        }
        if let Some(duration) = slide.cut.duration().filter(|_| !self.sat_at_end(slide, now)) {
            return Some((self.current, duration));
        }
        let next = doc.slides.get(self.current + 1)?;
        Some((self.current + 1, StoryClock::new(0.0).time(next, 0.0)))
    }

    pub fn black(&self) -> bool {
        self.black
    }

    /// Next would leave the shown slide: its end stop is reached (a slide
    /// without a cut leaves once every beat is released).
    pub fn next_leaves(&self, doc: &DeckDoc, now: f64) -> bool {
        doc.slides.get(self.current).is_none_or(|slide| self.story.beat >= slide.beats.len() && (slide.cut.duration().is_none() || self.sat_at_end(slide, now)))
    }

    /// The shown story stands at the end stop of its cut: sitting there, or
    /// (a slide that holds) played out to it.
    fn sat_at_end(&self, slide: &Slide, now: f64) -> bool {
        self.story.beat >= slide.beats.len() && slide.cut.duration().is_some_and(|duration| {
            self.story.time(slide, now) >= duration && (self.story.to_end || slide.play() == Loop::Hold)
        })
    }

    /// Enter slide `to` (clamped) of this deck from a picture of another deck
    /// (a link, or the return from one), through `transition` (at most
    /// [`MAX_JUMP_SECONDS`]). The slide left is [`OUTSIDE`].
    pub fn enter_from_outside(&mut self, to: usize, mut transition: Transition, now: f64) {
        self.current = to.min(self.slides.saturating_sub(1));
        self.entered_at = now;
        self.story = StoryClock::new(now);
        self.black = false;
        transition.seconds = transition.seconds.clamp(0.0, MAX_JUMP_SECONDS);
        self.incoming = (transition.seconds > 0.0).then(|| Incoming { from: OUTSIDE, from_entered_at: now, from_story: StoryClock::new(now), started: now, transition });
    }

    pub fn toggle_black(&mut self) {
        self.black = !self.black;
    }

    /// Go to slide `to` (clamped), through the change the document names for
    /// that step, at most [`MAX_CHANGE_SECONDS`] long. Going to the slide that
    /// shows does nothing. A change under way is finished at once (the slide
    /// it was entering is the one left) and the new one is a cut, so presses
    /// in quick succession (a held key) move through the slides at key rate.
    pub fn go(&mut self, doc: &DeckDoc, to: usize, now: f64) {
        let to = to.min(self.slides.saturating_sub(1));
        if to == self.current || self.slides == 0 {
            return;
        }
        let mut transition = doc.transition_for(self.current, to).clone();
        transition.seconds = if self.incoming.is_some() { 0.0 } else { transition.seconds.min(MAX_CHANGE_SECONDS) };
        let from = self.current;
        let from_entered_at = self.entered_at;
        let from_story = self.story.clone();
        self.current = to;
        self.entered_at = now;
        self.story = StoryClock::new(now);
        self.black = false;
        self.incoming = (transition.seconds > 0.0).then_some(Incoming { from, from_entered_at, from_story, started: now, transition });
    }

    /// Next: the story goes to its next stop (see [`stops`]) and stands there;
    /// the press after it leaves that stop. A slide change under way ends at
    /// once. Before a beat, the story is carried on faster and arrives at the
    /// beat in [`RUSH_SECONDS`] (no cut), however far into the segment it is.
    /// At a beat's hold the beat is released and the story plays on toward the
    /// next stop (past the last beat a looping slide loops by itself). Before the
    /// end, the story is carried on faster and arrives at its settled final
    /// frame in [`RUSH_SECONDS`] (no cut), a looping one too (it loops again when played). At the end the next slide is entered at its start.
    pub fn next(&mut self, doc: &DeckDoc, now: f64) {
        if let Some(slide) = doc.slides.get(self.current) {
            let t = self.story.time(slide, now);
            if let Some(&beat) = slide.beats.get(self.story.beat) {
                // A change under way ends here; with no beat left, go() below
                // ends it and cuts to the following slide.
                self.incoming = None;
                if t < beat {
                    // A press while already being carried keeps the arrival time.
                    if self.story.speed <= 1.0 {
                        self.story = StoryClock { beat: self.story.beat, started_at: now, started_time: t, speed: (beat - t) / RUSH_SECONDS, to_end: false };
                    }
                    return;
                }
                // Released, the story plays on by itself; past the last beat a
                // looping slide loops on its own. Only an arrow sits it at its end.
                self.story = StoryClock::at(self.story.beat + 1, now, t);
                return;
            }
            if let Some(duration) = slide.cut.duration().filter(|_| !self.sat_at_end(slide, now)) {
                self.incoming = None;
                // A press while already being carried keeps the arrival time.
                if !(self.story.to_end && self.story.speed > 1.0) {
                    self.story = StoryClock::rush_to_end(slide.beats.len(), now, t, duration);
                }
                return;
            }
        }
        self.go(doc, self.current + 1, now);
    }

    /// Previous: back to the hold of the last beat before the story's time
    /// (wherever it stands: mid-segment, after a scrub, playing, in the
    /// play-out); at or before the first beat, to the start of the slide;
    /// at the start, to the end of the previous slide (its settled final frame,
    /// a looping slide's end of the first pass too).
    pub fn previous(&mut self, doc: &DeckDoc, now: f64) {
        if let Some(slide) = doc.slides.get(self.current) {
            let mut t = self.story.time(slide, now);
            let speed = if self.paused() { 0.0 } else { 1.0 };
            // In the play-out of a looping slide the time may have wrapped past the end.
            if let Some(last) = slide.beats.last().filter(|last| self.story.beat >= slide.beats.len() && t < **last) { t = *last + 2.0 * PREVIOUS_EPSILON; }
            if let Some(index) = slide.beats.iter().rposition(|beat| *beat < t - PREVIOUS_EPSILON) {
                // Not at the start itself: a beat at time 0 is the start.
                if slide.beats[index] > PREVIOUS_EPSILON {
                    self.incoming = None;
                    self.story = StoryClock { beat: index, started_at: now, started_time: slide.beats[index], speed, to_end: false };
                    return;
                }
            }
            if t > PREVIOUS_EPSILON {
                self.incoming = None;
                self.story = StoryClock { beat: 0, started_at: now, started_time: 0.0, speed, to_end: false };
                return;
            }
        }
        let from = self.current;
        self.go(doc, from.saturating_sub(1), now);
        // The slide before is entered at its end, its settled final frame.
        if self.current != from { self.settle_at_end(doc, now); }
    }

    /// Put the shown slide's story at its end stop (its settled final frame,
    /// whatever the slide's loop), when it has a cut.
    fn settle_at_end(&mut self, doc: &DeckDoc, now: f64) {
        let Some(slide) = doc.slides.get(self.current) else { return };
        if let Some(duration) = slide.cut.duration() {
            self.story = StoryClock::at_end(slide.beats.len(), now, duration);
        }
    }

    pub fn first(&mut self, doc: &DeckDoc, now: f64) {
        self.go(doc, 0, now);
    }

    pub fn last(&mut self, doc: &DeckDoc, now: f64) {
        self.go(doc, self.slides.saturating_sub(1), now);
    }

    /// Start the shown slide's clock again.
    pub fn restart(&mut self, now: f64) {
        self.entered_at = now;
        self.story = StoryClock::new(now);
        self.incoming = None;
    }

    /// Advance the clock. A change under way that has run its length ends.
    /// It never waits for the entered slide's picture: the player has the
    /// next slide's engine prepared, and otherwise holds the last picture
    /// until the entered slide's engine has its first frame.
    pub fn tick(&mut self, now: f64) {
        let Some(incoming) = &self.incoming else { return };
        if now - incoming.started >= incoming.transition.seconds {
            self.incoming = None;
        }
    }

    /// How far the change is (0 to 1, eased over the change's length), when one
    /// is under way.
    pub fn progress(&self, now: f64) -> Option<f64> {
        let incoming = self.incoming.as_ref()?;
        let p = ((now - incoming.started) / incoming.transition.seconds).clamp(0.0, 1.0);
        Some(p * p * (3.0 - 2.0 * p))
    }

    /// The story time of the shown slide or the one being left. Beats hold
    /// the first pass; `play` controls the clock afterwards. Other slides
    /// wait at their entry time.
    pub fn slide_time(&self, doc: &DeckDoc, index: usize, now: f64) -> f64 {
        let Some(slide) = doc.slides.get(index) else { return 0.0 };
        if index == self.current {
            self.story.time(slide, now)
        } else if let Some(incoming) = self.incoming.as_ref().filter(|incoming| incoming.from == index) {
            incoming.from_story.time(slide, now)
        } else {
            StoryClock::new(now).time(slide, now)
        }
    }

    /// The slides whose pictures are kept ready: the shown one, the slide
    /// being left, the previous one (so Previous is instant too) and the
    /// next (prepared). Every other slide is torn down. Sorted.
    pub fn keep(&self) -> Vec<usize> {
        let mut keep = vec![self.current];
        if self.current > 0 {
            keep.push(self.current - 1);
        }
        if self.current + 1 < self.slides {
            keep.push(self.current + 1);
        }
        if let Some(incoming) = self.incoming.as_ref().filter(|incoming| incoming.from != OUTSIDE) {
            keep.push(incoming.from);
        }
        keep.sort_unstable();
        keep.dedup();
        keep
    }
}

/// The stops of a slide, in story seconds: its start, every beat, and the end
/// of its cut (the settled final frame; the end of the first pass for a
/// looping slide). Equal neighbours count once; a slide without a cut has no
/// end stop. The arrows walk these: Right to the next, Left to the previous.
pub fn stops(slide: &Slide) -> Vec<f64> {
    let mut stops = vec![0.0];
    for at in slide.beats.iter().copied().chain(slide.cut.duration()) {
        if at > stops[stops.len() - 1] + PREVIOUS_EPSILON { stops.push(at); }
    }
    stops
}
