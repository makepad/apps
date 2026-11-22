//! `FabProgressCard`: a job the person waits on ([`ProgressCard`]: another plate on its way to
//! the bed), as a card high in the 3D view over the plate still on it, which dims. (High: the
//! plate's parts sit in the middle of the view, and the card leaves them to be seen and turned.
//! Over a slice it ends above the layer slider, whose tag says the shown layer's height.)
//! The card has a title, the job's phase (its stage on the job board, as the worker reports
//! it), a bar of how far the job has got (it never runs back) with how long it has run, and a
//! Cancel when the job can be cancelled: the job stops, and the card says what that leaves
//! while it does. A job that fails says why on the card until it is closed.
//!
//! Nothing else waits for it: the dim takes no input, so the plate under it still turns and
//! zooms, and the panels, tabs and menus work as ever. A quick job never shows the card: it
//! comes once the job has run [`SHOW_AFTER`] seconds, or at once when it takes over from a card
//! on show (another plate clicked while one loads). The card's size is fixed for a job, so
//! its words changing, or a pointer over its button, moves nothing.
//!
//! Main-loop cost: with no card, none (its overlay draws empty, so a redraw has somewhere to
//! land). With one, on `Event::Signal` it compares the board's sequence number and redraws only
//! when that moved; while the job runs, a one-second timer moves its elapsed time.

use crate::api::*;
use makepad_widgets_core::jobs::{board, now, JobEnd, JobId, JobSnapshot};
use makepad_widgets_core::*;

/// How long a job runs before its card shows (s): a plate switched back to in a moment never
/// flashes one.
pub const SHOW_AFTER: f64 = 0.3;

/// How far down the view the card sits: a quarter of the way.
const HIGH: f64 = 0.25;

/// Over a slice, the room between the card and the layer slider's top (px).
const OVER_SLIDER: f64 = 16.0;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabProgressCardBase = #(FabProgressCard::register_widget(vm))
    mod.widgets.FabProgressCard = set_type_default() do mod.widgets.FabProgressCardBase{
        width: Fill
        height: Fill
        flow: Overlay
        // The plate under the card dims (a solid view: a plain view's background draws nothing);
        // the dim has no cursor, so it takes no input.
        dim := SolidView{
            width: Fill
            height: Fill
            draw_bg +: { color: #x00000059 }
        }
        // (over a slice its height ends above the layer slider, and the card sits at its foot)
        middle := View{
            width: Fill
            height: Fill
            align: Align{x: 0.5 y: #(HIGH)}
            card := View{
                width: 380
                height: Fit
                flow: Down
                spacing: 10
                padding: Inset{left: 18 right: 18 top: 16 bottom: 14}
                cursor: MouseCursor.Default
                show_bg: true
                draw_bg +: {
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                        sdf.fill_keep(fab.color_popover)
                        sdf.stroke(fab.color_popover_border, 1.0)
                        return sdf.result
                    }
                }
                title := mod.widgets.FabTitleLabel{ width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis }
                phase := mod.widgets.FabLabelDim{ width: Fill height: 34 text: "" max_lines: 2 text_overflow: TextOverflow.Ellipsis flow: Right{wrap: true} }
                bar_row := View{
                    width: Fill
                    height: 16
                    flow: Right
                    spacing: 10
                    align: Align{y: 0.5}
                    bar := mod.widgets.FabProgress{ width: Fill height: 6 }
                    elapsed := mod.widgets.FabLabelDim{ width: 72 text: "" align: Align{x: 1.0} }
                }
                buttons := View{
                    width: Fill
                    height: Fit
                    flow: Right
                    align: Align{x: 1.0 y: 0.5}
                    act := mod.widgets.FabButton{ width: 96 text: "Cancel" }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabProgressCard {
    #[deref]
    view: View,
    #[rust]
    seen_seq: u64,
    #[rust]
    tick: Timer,
    /// The job last shown and the furthest its bar got (a bar never runs back).
    #[rust]
    shown: Option<(JobId, f32)>,
}

/// What the card shows of its job.
enum Shows {
    /// Not yet (a quick job never shows): the card comes in this many seconds.
    Soon(f64),
    Running(JobSnapshot),
    Failed(String),
    /// Ended (done or cancelled): the app takes the card down when it has what the job made.
    Ended,
}

/// What the card shows of its job; `on_show`: a card is up (this job's, or the one it takes
/// over from).
fn shows(card: &ProgressCard, on_show: bool) -> Shows {
    let Some(job) = board().get(card.job) else { return Shows::Ended };
    match &job.end {
        JobEnd::Running => {
            let wait = job.started + SHOW_AFTER - now();
            if wait > 0.0 && !on_show { Shows::Soon(wait) } else { Shows::Running(job) }
        }
        JobEnd::Failed { reason, .. } => Shows::Failed(reason.clone()),
        JobEnd::Done { .. } | JobEnd::Cancelled => Shows::Ended,
    }
}

/// A job's stage as a sentence's start: "Building the wheels 6/8".
fn sentence(words: &str) -> String {
    let mut c = words.chars();
    c.next().map_or(String::new(), |first| first.to_uppercase().chain(c).collect())
}

impl Widget for FabProgressCard {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let card = scope.data.get::<AppState>().and_then(|s| s.progress_card.clone());
        // (the app redraws the card when it sets or clears it)
        if self.tick.is_event(event).is_some() {
            self.tick = Timer::empty();
            if card.is_some() {
                self.view.redraw(cx);
            }
        }
        if let (Event::Signal, Some(_)) = (event, &card) {
            let seq = board().seq();
            if seq != self.seen_seq {
                self.seen_seq = seq;
                self.view.redraw(cx);
            }
        }
        // Only a card on show takes input.
        let Some(card) = card else { return };
        let failed = match shows(&card, self.shown.is_some()) {
            Shows::Running(_) => false,
            Shows::Failed(_) => true,
            Shows::Soon(_) | Shows::Ended => return,
        };
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if self.view.button(cx, ids!(act)).clicked(actions) {
            if failed {
                cx.action(ShellAction::CloseProgressCard);
            } else {
                board().cancel(card.job);
            }
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let state = scope.data.get::<AppState>();
        let card = state.and_then(|s| s.progress_card.clone());
        // (the stage's slice view, with its layer slider)
        let slice = state.is_some_and(|s| s.print.play.active || s.print.monitor_holds());
        let on_show = self.card(cx, card);
        // Drawn either way, empty when there is no card: its redraw then has an area to draw.
        self.view.view(cx, ids!(dim)).set_visible(cx, on_show);
        let middle = self.view.view(cx, ids!(middle));
        middle.set_visible(cx, on_show);
        if let Some(mut middle) = middle.borrow_mut() {
            (middle.walk.height, middle.layout.align.y) = if slice { (Size::Fixed(crate::ui::stage::SLIDER_TOP - OVER_SLIDER), 1.0) } else { (Size::fill(), HIGH) };
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl FabProgressCard {
    /// Dresses the card for `card`'s job; false: no card shows (yet).
    fn card(&mut self, cx: &mut Cx2d, card: Option<ProgressCard>) -> bool {
        let Some(card) = card else {
            self.shown = None;
            return false;
        };
        // (title, phase, bar, elapsed, the button's words and whether it is pressable; none: no button)
        let (title, phase, fraction, elapsed, act) = match shows(&card, self.shown.is_some()) {
            Shows::Soon(wait) => {
                if self.tick.is_empty() {
                    self.tick = cx.start_timeout(wait + 0.01);
                }
                return false;
            }
            Shows::Ended => {
                self.shown = None;
                return false;
            }
            Shows::Running(job) => {
                if self.tick.is_empty() {
                    self.tick = cx.start_timeout(1.0);
                }
                let furthest = match (self.shown, job.fraction) {
                    (Some((id, f)), Some(now)) if id == card.job => f.max(now),
                    (Some((id, f)), None) if id == card.job => f,
                    (_, f) => f.unwrap_or(0.0),
                };
                self.shown = Some((card.job, furthest));
                let secs = job.elapsed(now());
                let elapsed = if secs < 60.0 { format!("{:.0}% · {:.0} s", furthest * 100.0, secs.floor()) } else { format!("{:.0}% · {}:{:02}", furthest * 100.0, secs as u64 / 60, secs as u64 % 60) };
                let (phase, act) = if job.cancelling {
                    (sentence(&format!("cancelling: {}", card.cancel_keeps)), Some(("Cancelling", false)))
                } else {
                    let words = job.stage_words();
                    (if words.is_empty() { "Starting".to_string() } else { sentence(&words) }, job.cancellable.then_some(("Cancel", true)))
                };
                (card.title.clone(), phase, Some(furthest), elapsed, act)
            }
            Shows::Failed(reason) => (card.failed.clone(), sentence(&reason), None, String::new(), Some(("Close", true))),
        };
        let v = &self.view;
        v.label(cx, ids!(title)).set_text(cx, &title);
        v.label(cx, ids!(phase)).set_text(cx, &phase);
        v.label(cx, ids!(elapsed)).set_text(cx, &elapsed);
        let bar = v.view(cx, ids!(bar));
        bar.set_visible(cx, fraction.is_some());
        if let Some(f) = fraction {
            crate::ui::widgets::set_progress(cx, &bar, f);
        }
        let button = v.button(cx, ids!(act));
        button.set_visible(cx, act.is_some());
        if let Some((words, enabled)) = act {
            button.set_text(cx, words);
            button.set_enabled(cx, enabled);
        }
        true
    }
}
