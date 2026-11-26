//! The demo runner: a slide's `demo:` steps played on its hosted app, live.
//!
//! The steps are app_run [`Call`]s (checked when the document loads). One
//! app_run [`Runner`] on the real clock plays them, polled from the UI
//! thread on the apps' beat; [`HostedDriver`] is its [`Driver`] over a
//! [`Slot`]: input goes to the child as the presenter's would, and the
//! semantic tree arrives through the app-host mirror. Until it arrives the
//! runner returns pending; thereafter waits inspect the current tree, even
//! when an idle child has no changes to publish. Nothing blocks or sleeps.

use super::apps::{Life, Slot, SlotView};
use makepad_story_app_run::doc::RunOutput;
use makepad_story_app_run::driver::{Driver, Mods, Mouse, Row};
use makepad_story_deck::playback::{PausedBy, Phase, Playback, FPS, WAIT_NOTE_AFTER};
use makepad_story_deck::{DemoStep, DeckDoc, Step};
use makepad_studio_protocol::{KeyEvent, MouseButton, TextInputEvent};
use makepad_widgets::*;
use super::playbar::DemoInput;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// How long a press ripples on the synthetic cursor.
pub const RIPPLE_SECONDS: f64 = 0.45;

fn modifiers(mods: Mods) -> KeyModifiers {
    KeyModifiers { shift: mods.shift, control: mods.ctrl, alt: mods.alt, logo: mods.cmd }
}

/// The runner's view of a hosted app: window points in, the child's input
/// out, the semantic tree from the app-host mirror.
pub struct HostedDriver<'a> {
    pub slot: &'a mut Slot,
    // Child messages cannot advance during this synchronous runner poll.
    // Copy the mirror once; all reads in the poll see the same update.
    trees: Option<Vec<(u64, a11y_tree::mirror::Tree)>>,
}

impl HostedDriver<'_> {
    fn trees(&mut self) -> &[(u64, a11y_tree::mirror::Tree)] {
        self.trees.get_or_insert_with(|| self.slot.hosted.remote_ui().trees())
    }
}

impl Driver for HostedDriver<'_> {
    fn ready(&mut self) -> bool {
        let ui = self.slot.hosted.remote_ui();
        if !self.slot.semantics_enabled {
            ui.enable(a11y_tree::consumer::HOST);
            self.slot.semantics_enabled = true;
        }
        self.trees().iter().any(|(_, tree)| !tree.root.is_none())
    }

    fn snapshot(&mut self) -> Result<Vec<Row>, String> {
        let trees = self.trees();
        if trees.is_empty() {
            return Err("the app has not published its semantic tree yet".into());
        }
        let mut rows = Vec::new();
        for (_, tree) in trees {
            tree.walk(|node, _| rows.push(Row::from_node(node)));
        }
        Ok(rows)
    }

    fn mouse(&mut self, kind: Mouse, x: f64, y: f64) -> Result<(), String> {
        let at = dvec2(x, y);
        let time = Cx::time_now();
        let mods = KeyModifiers::default();
        let (hosted, deck) = (&mut self.slot.hosted, &mut self.slot.extra);
        match kind {
            Mouse::Move if deck.pressed => hosted.mouse_move(at, time, &mods),
            Mouse::Move => hosted.hover_move(at, time, &mods),
            Mouse::Down(button) => {
                deck.pressed = true;
                deck.pressed_at = Some(Cx::monotonic_now());
                hosted.mouse_down(at, button_of(button), time, &mods);
            }
            Mouse::Up(button) => {
                deck.pressed = false;
                hosted.mouse_up(Some(at), button_of(button), time, &mods, false);
            }
            Mouse::Scroll(dx, dy) => hosted.scroll(at, dvec2(dx, dy), true, makepad_studio_protocol::ScrollPhase::None, time, &mods),
        }
        Ok(())
    }

    fn key(&mut self, down: bool, code: &str, mods: Mods) -> Result<(), String> {
        let key_code = makepad_studio_protocol::KeyCode::from_name(code).ok_or_else(|| format!("`{code}` is not a key name"))?;
        let event = KeyEvent { key_code, is_repeat: false, modifiers: modifiers(mods), time: Cx::time_now() };
        if down {
            self.slot.hosted.key_down(event);
        } else {
            self.slot.hosted.key_up(event);
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        self.slot.hosted.text_input(TextInputEvent { input: text.to_string(), ..Default::default() });
        Ok(())
    }

    fn drop_file(&mut self, _: &Path, _: f64, _: f64) -> Result<(), String> {
        Err("a hosted app takes no dropped files".into())
    }

    fn resize(&mut self, _: f64, _: f64) -> Result<(), String> {
        Err("a hosted app keeps the size its slide declares".into())
    }

    fn settled(&mut self) -> Result<(bool, Vec<String>), String> {
        if self.slot.settled(Cx::monotonic_now()) {
            Ok((true, Vec::new()))
        } else {
            Ok((false, vec!["the picture is still changing".into()]))
        }
    }

    fn advance_frames(&mut self, _: u64) -> Result<(), String> {
        Err("a hosted app runs on the real clock".into())
    }
}

fn button_of(button: u8) -> MouseButton {
    match button {
        1 => MouseButton::SECONDARY,
        2 => MouseButton::MIDDLE,
        _ => MouseButton::PRIMARY,
    }
}

/// An app owns its setup and playback progress, including across beat changes.
pub type DemoKey = (usize, String);

/// The demos of a deck's apps, each at the step it reached.
pub struct Demo {
    slides: HashMap<DemoKey, Playback>,
    /// Setup steps run on every launch of their app.
    setups: HashMap<DemoKey, Playback>,
    /// Slides whose app the presenter used by hand: its state is no longer
    /// where the steps left it, so a seek there starts from a relaunch.
    touched: HashSet<DemoKey>,
    /// A segment left halfway through a call must restart cleanly, rather
    /// than re-applying a toggle whose pointer-up may already have landed.
    interrupted: HashSet<DemoKey>,
}

impl Default for Demo {
    fn default() -> Demo {
        Demo { slides: HashMap::new(), setups: HashMap::new(), touched: HashSet::new(), interrupted: HashSet::new() }
    }
}

impl Demo {
    /// A render-only edit must not replay setup clicks in a surviving child.
    pub fn remap(&mut self, before: &DeckDoc, after: &DeckDoc) {
        let mut keys = HashMap::new();
        for (index, slide) in before.slides.iter().enumerate() {
            let Some(next) = after.slides.iter().position(|new| new.name == slide.name) else { continue };
            let new = &after.slides[next];
            for app in &slide.apps {
                let Some(next_app) = new.apps.iter().find(|candidate| candidate.name == app.name) else { continue };
                // Source locations change when text above a slide is edited;
                // only the actual calls determine whether playback changed.
                let same_setup = slide.setup_for(app).iter().map(|step| &step.step).eq(new.setup_for(next_app).iter().map(|step| &step.step));
                let same_demo = slide.demo_for(app).iter().map(|step| &step.step).eq(new.demo_for(next_app).iter().map(|step| &step.step));
                keys.insert((index, app.name.clone()), ((next, app.name.clone()), same_setup, same_demo));
            }
        }
        self.setups = std::mem::take(&mut self.setups).into_iter().filter_map(|(key, progress)| {
            let (key, same, _) = keys.get(&key)?;
            same.then(|| (key.clone(), progress))
        }).collect();
        self.touched = std::mem::take(&mut self.touched).into_iter().filter_map(|key| Some(keys.get(&key)?.0.clone())).collect();
        self.interrupted = std::mem::take(&mut self.interrupted).into_iter().filter_map(|key| Some(keys.get(&key)?.0.clone())).collect();
        self.slides = std::mem::take(&mut self.slides).into_iter().filter_map(|(key, progress)| {
            let (key, _, same) = keys.get(&key)?;
            if !same { self.interrupted.insert(key.clone()); }
            same.then(|| (key.clone(), progress))
        }).collect();
    }

    /// What the play bar shows of the slide's demo: where it is and what it
    /// is doing. None: the slide has no demo.
    pub fn playbar(&self, key: &DemoKey, steps: usize) -> Option<DemoInput> {
        if steps == 0 {
            return None;
        }
        let progress = self.slides.get(key);
        Some(DemoInput {
            step: progress.map_or(0, |progress| progress.step.min(steps)),
            total: steps,
            playing: progress.is_some_and(|progress| progress.phase == Phase::Running),
            seeking: progress.and_then(Playback::seeking).map(|to| to.min(steps)),
        })
    }

    /// The stop an arrow goes to (see `Playback::arrow_stop`).
    pub fn arrow_stop(&self, key: &DemoKey, steps: usize, forward: bool) -> Option<usize> {
        match self.slides.get(key) {
            Some(progress) => progress.arrow_stop(steps, forward),
            None => Playback::fresh(0).arrow_stop(steps, forward),
        }
    }

    /// Play, pause or restart (`restart`: the caller has relaunched the app
    /// first). Playing needs the launch the progress belongs to.
    pub fn toggle(&mut self, key: &DemoKey, steps: usize, generation: u64, restart: bool) {
        self.slides.entry(key.clone()).or_insert_with(|| Playback::fresh(generation)).toggle(steps, generation, restart);
    }

    /// Whether getting to the start of step `to` takes a relaunch.
    pub fn needs_relaunch(&self, key: &DemoKey, to: usize) -> bool {
        self.interrupted.contains(key) || self.touched.contains(key) || self.slides.get(key).is_some_and(|progress| progress.needs_relaunch(to))
    }

    /// The app was launched again: no hand-made state is left in it.
    pub fn relaunched(&mut self, key: &DemoKey) {
        self.touched.remove(key);
        self.interrupted.remove(key);
    }

    /// Whether the demo has begun and not ended: playing, or paused.
    pub fn unfinished(&self, key: &DemoKey) -> bool {
        self.slides.get(key).is_some_and(|progress| matches!(progress.phase, Phase::Running | Phase::Paused))
    }

    pub fn paused(&self, key: &DemoKey) -> bool {
        self.slides.get(key).is_some_and(|progress| progress.phase == Phase::Paused)
    }

    /// Whether the demo is playing (a replay counts).
    pub fn running(&self, key: &DemoKey) -> bool {
        self.slides.get(key).is_some_and(|progress| progress.phase == Phase::Running)
    }

    /// Go to the start of step `to` (see `Playback::seek`).
    pub fn seek(&mut self, key: &DemoKey, to: usize, steps: usize, generation: u64, then_run: bool) {
        self.slides.entry(key.clone()).or_insert_with(|| Playback::fresh(generation)).seek(to, steps, generation, then_run);
    }

    /// The step a fast replay is on its way to, while it runs.
    pub fn seeking(&self, key: &DemoKey) -> Option<usize> {
        self.slides.get(key)?.seeking()
    }

    /// The slide's demo ran all its steps.
    pub fn finished(&self, key: &DemoKey) -> bool {
        self.slides.get(key).is_some_and(|progress| progress.phase == Phase::Done)
    }

    pub fn interrupted(&self, key: &DemoKey) -> bool {
        self.interrupted.contains(key)
    }

    /// Leaving the slide stops its demo; the step it reached is kept.
    pub fn leave(&mut self, key: &DemoKey) {
        if let Some(progress) = self.slides.get_mut(key) {
            if progress.begun || progress.seeking().is_some() {
                self.interrupted.insert(key.clone());
            }
            progress.leave();
        }
    }

    /// The presenter used the app: a running demo stops after its step.
    pub fn presenter_input(&mut self, key: &DemoKey) {
        self.touched.insert(key.clone());
        if let Some(progress) = self.slides.get_mut(key) {
            progress.presenter_input();
        }
    }

    /// The runner's pointer on the slide's demo (app points), while it
    /// plays. A replay moves it at once, so it is not shown.
    pub fn cursor(&self, key: &DemoKey) -> Option<(f64, f64)> {
        let progress = self.slides.get(key).filter(|progress| progress.seeking().is_none())?;
        progress.cursor()
    }

    /// `demo: step 2/8 click @increment_button [waiting for @increment_button]`.
    pub fn status(&self, key: &DemoKey, steps: &[DemoStep], now: f64) -> Option<String> {
        let progress = self.slides.get(key)?;
        if steps.is_empty() {
            return None;
        }
        let total = steps.len();
        let at = progress.step.min(total - 1);
        let step = &steps[at];
        let text = match &progress.phase {
            Phase::Idle => return None,
            Phase::Done => format!("demo: finished {total} steps"),
            Phase::Paused => format!("demo: {} before step {}/{total} {}", paused_text(progress).to_lowercase(), at + 1, step.describe()),
            Phase::Failed(why) => format!("demo: step {}/{total} {} failed: {why}", at + 1, step.describe()),
            Phase::Running if progress.seeking().is_some() => format!("demo: seeking to step {}/{total}", progress.seeking().unwrap_or(0).min(total) + 1),
            Phase::Running => {
                let mut text = format!("demo: step {}/{total} {}", at + 1, step.describe());
                if progress.begun && now - progress.begun_at > WAIT_NOTE_AFTER {
                    if let Step::Call(call) = &step.step {
                        let targets: Vec<String> = call.targets().iter().map(|target| target.describe()).collect();
                        if !targets.is_empty() {
                            text.push_str(&format!(" [waiting for {}]", targets.join(", ")));
                        }
                    }
                }
                text
            }
        };
        Some(text)
    }

    /// Run the slide's `setup:` steps as far as they go now, on the slide's
    /// app: after its current-slide launch,
    /// before the first beat and before the demo. A launch that is another
    /// than the last one's starts them over. True while they are not done;
    /// a failed setup is logged and does not hold the slide back.
    pub fn poll_setup(&mut self, key: &DemoKey, steps: &[DemoStep], slot: &mut Slot, dpi: f64, now: f64) -> bool {
        if steps.is_empty() {
            return false;
        }
        let generation = slot.generation;
        let progress = self.setups.entry(key.clone()).or_insert_with(|| {
            let mut fresh = Playback::fresh(generation);
            fresh.toggle(steps.len(), generation, false);
            fresh
        });
        if progress.generation != generation {
            *progress = Playback::fresh(generation);
            progress.toggle(steps.len(), generation, false);
        }
        match &slot.life {
            Life::Missing(why) => progress.fail(why.clone()),
            Life::Exited(why) => progress.fail(format!("{} ended: {why}", slot.decl.app)),
            _ => {}
        }
        if progress.phase == Phase::Running && slot.hosted.has_frame() {
            let size = slot.size();
            let output = RunOutput { width: (size.x * dpi) as u32, height: (size.y * dpi) as u32, dpi_scale: dpi, fps: FPS };
            progress.drive(steps, &mut HostedDriver { slot: &mut *slot, trees: None }, output, 1.0, now);
        }
        if let Phase::Failed(why) = &progress.phase {
            if progress.step < steps.len() {
                log!("deck app {key:?}: setup step {}/{} {} failed: {why}", progress.step + 1, steps.len(), steps[progress.step].describe());
                progress.step = steps.len();
            }
        }
        progress.phase == Phase::Running
    }

    /// Run this app's demo as far as it goes now, on the apps' beat.
    pub fn poll(&mut self, key: &DemoKey, steps: &[DemoStep], slot: &mut Slot, dpi: f64, now: f64) {
        let Some(progress) = self.slides.get_mut(key) else { return };
        if progress.phase != Phase::Running {
            return;
        }
        match &slot.life {
            Life::Missing(why) => return progress.fail(why.clone()),
            Life::Exited(why) => return progress.fail(format!("{} ended: {why}", slot.decl.app)),
            _ => {}
        }
        if !slot.hosted.has_frame() {
            return;
        }
        let size = slot.size();
        let output = RunOutput { width: (size.x * dpi) as u32, height: (size.y * dpi) as u32, dpi_scale: dpi, fps: FPS };
        let before = progress.phase.clone();
        progress.drive(steps, &mut HostedDriver { slot: &mut *slot, trees: None }, output, 1.0, now);
        if progress.phase != before {
            match &progress.phase {
                Phase::Done => log!("deck app {key:?}: demo finished {} steps", steps.len()),
                Phase::Failed(why) => log!("deck app {key:?}: demo step {}/{} failed: {why}", progress.step + 1, steps.len()),
                _ => {}
            }
        }
    }
}

/// What a stopped demo says about itself.
fn paused_text(progress: &Playback) -> &'static str {
    match progress.paused_by() {
        PausedBy::Script => "Paused (script)",
        PausedBy::You => "Paused (you)",
        PausedBy::Transport => "Paused",
    }
}
