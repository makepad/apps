//! The self-drawing LED controller schematic. Click a part to focus and fly to it; Escape
//! clears focus and fits. T toggles the theme, A replays assembly, F fits; wheel/pinch zoom
//! and space-drag, middle/right drag or two-finger scrolling pan. Value drags and edits,
//! wire drags, X and Delete, and part moves go through the electronics host: a preview
//! evaluates the edit, and release commits it as a text splice of the fixture's documents.
//! Escape cancels a preview. Cmd/Ctrl+Z undoes; Shift+Cmd/Ctrl+Z redoes.
//! The bench simulates the fixture live: nets coloured, currents flowing and parts glowing.

pub use makepad_widgets;

use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_board::Factories;
use fab_elec_design::DesignOp;
use fab_elec_host::{Docs, Driver, Engine};
use fab_elec_simhost::SimHost;
use fab_elec_view::{
    canvas::draw::CanvasLive,
    explorer::view::{ExplorerView, Sel},
    library::LibraryPanel,
    live::LiveRows,
    DesignSnapshot, ViewAction,
};
use makepad_widgets::*;
use std::sync::{atomic::AtomicU64, Arc};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    startup() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.title: "Circuit explorer"
                window.inner_size: vec2(1400, 900)
                body +: {
                    flow: Right
                    library := LibraryPanel { width: 240 height: Fill }
                    explorer := ExplorerView { width: Fill height: Fill }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    started: bool,
    /// The shared electronics host and its document-backed engine strand.
    #[rust]
    driver: Option<Driver<()>>,
    #[rust(LiveRows::with_off([0.0; 4]))]
    items: LiveRows,
    #[rust]
    selected: Option<u32>,
    /// The fixture's live bench and row-texture publisher.
    #[rust]
    sim: SimHost,
    /// The shown snapshot; the bench reads its committed design.
    #[rust]
    snapshot: Option<Arc<DesignSnapshot>>,
    /// A frame to pump bench commands or let views read a publication.
    #[rust]
    next_frame: NextFrame,
}

impl App {
    /// Keep at most one host frame pending.
    fn request_frame(&mut self, cx: &mut Cx) {
        if !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
    }

    fn bind_items(&mut self, cx: &mut Cx, explorer: &mut ExplorerView) {
        let live = CanvasLive::off(cx).with_items(self.items.texture(cx).clone());
        explorer.set_live(cx, live);
    }

    fn select(&mut self, selected: Option<(u32, &str)>) -> bool {
        let row = selected.map(|(row, _)| row);
        if self.selected == row { return false; }
        if let Some(previous) = self.selected {
            self.item_channel(previous, 0, 0.0);
        }
        self.selected = row;
        if let Some((row, path)) = selected {
            self.item_channel(row, 0, 1.0);
            log!("sheetview: selected {} (row {})", path, row);
        } else {
            log!("sheetview: selected none");
        }
        true
    }

    fn item_channel(&mut self, row: u32, channel: usize, value: f32) {
        let mut state = self.items.row(row as usize);
        if state[channel] != value {
            state[channel] = value;
            self.items.set(row as usize, state);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        // The sheet Fab wears standalone, before the theme loads, so the panels match Fab.
        fab::theme::wear_host_sheet(vm);
        makepad_widgets::script_mod(vm);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::canvas::draw::script_mod(vm);
        // The probe flags the explorer draws, with the probe tag they wear.
        fab_elec_view::live::tag::script_mod(vm);
        fab_elec_view::live::flags::script_mod(vm);
        fab_elec_view::explorer::view::script_mod(vm);
        fab_elec_view::library::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if !self.started && matches!(event, Event::Startup | Event::Draw(_)) {
            let explorer = self.ui.widget(cx, ids!(explorer));
            if let Some(mut explorer) = explorer.borrow_mut::<ExplorerView>() {
                explorer.set_bench(Some("ledctl".into()));
                self.bind_items(cx, &mut explorer);
                self.sim = SimHost::new();
                let dc = std::env::var("SHEETVIEW_BENCH").is_ok_and(|bench| bench == "dc");
                let duties = if dc { [0.0; 8] } else { [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8] };
                self.sim.set_bench(ledctl_bench(10e3, duties));
                log!("sheetview: bench {}", if dc { "dc" } else { "pwm" });
                let steps = Arc::new(AtomicU64::new(0));
                let logged = Arc::new(AtomicU64::new(0));
                let mut driver = Driver::new(cx, steps.clone(), logged.clone());
                driver.load(cx, None, move |_tx| {
                    let sources = fab_elec_fixtures::docs("ledctl").expect("the controller has stock documents");
                    let (design, docs) = Docs::from_evaluated(sources, fab_elec_fixtures::ledctl_eval());
                    Engine::new(design, Arc::new(Factories::new()), steps, logged, "fixture".into()).with_docs(docs)
                });
                self.driver = Some(driver);
                self.request_frame(cx);
                self.started = true;
            };
        }
        if let Some(driver) = &mut self.driver {
            if let Some(snap) = driver.event(cx, &mut |_, _, _| false) {
                self.snapshot = Some(snap.clone());
                let library = self.ui.widget(cx, ids!(library));
                if let Some(mut library) = library.borrow_mut::<LibraryPanel>() {
                    library.set_catalog(cx, snap.catalog.clone());
                };
                let unit = snap.design.units[0].id;
                if let Some(sheet) = snap.of(&snap.sheets, unit) {
                    let count = sheet.value.count;
                    if self.selected.is_some_and(|row| row >= count) {
                        self.select(None);
                    }
                    self.items.resize(count as usize);
                }
                let explorer = self.ui.widget(cx, ids!(explorer));
                if let Some(mut explorer) = explorer.borrow_mut::<ExplorerView>() {
                    self.bind_items(cx, &mut explorer);
                };
                self.ui.redraw(cx);
            }
        }
        let tick = self.next_frame.is_event(event).is_some();
        if matches!(event, Event::Signal) || tick {
            if let Some(snapshot) = &self.snapshot {
                if self.sim.frame(cx, &snapshot.design, tick) {
                    self.request_frame(cx);
                }
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
        if let Event::KeyDown(key) = event {
            let m = key.modifiers;
            if key.key_code == KeyCode::KeyB && !key.is_repeat && !m.shift && !m.control && !m.alt && !m.logo
                && self.ui.window(cx, ids!(main_window)).window_id()
                    .is_some_and(|window| cx.get_window_id_of(&cx.key_focus()) == Some(window))
            {
                cx.start_dragging(vec![fab_elec_view::live::probe::probe_drag_item()]);
                log!("sheetview: probe drag (stands in for the bench)");
            }
            if key.key_code == KeyCode::KeyZ && (m.logo || m.control) && !key.is_repeat
                && !cx.key_focus_takes_text()
                && self.ui.window(cx, ids!(main_window)).window_id()
                    .is_some_and(|window| cx.get_window_id_of(&cx.key_focus()) == Some(window))
            {
                if let Some(driver) = &mut self.driver {
                    if m.shift {
                        driver.redo();
                        log!("fab.sheetview: redo");
                    } else {
                        driver.undo();
                        log!("fab.sheetview: undo");
                    }
                }
            }
        }
        let can_leave_reveal = matches!(event, Event::KeyDown(key) if key.key_code == KeyCode::Escape)
            || matches!(event, Event::MouseUp(_) | Event::TouchUpdate(_));
        if can_leave_reveal && self.selected.is_some() {
            let explorer = self.ui.widget(cx, ids!(explorer));
            if let Some(mut explorer) = explorer.borrow_mut::<ExplorerView>() {
                // Escape cancels a scrub first; an empty click also leaves the reveal.
                if explorer.focus().is_none() && self.select(None) {
                    self.bind_items(cx, &mut explorer);
                }
            };
        }
        if let Event::Actions(actions) = event {
            self.sim.handle_actions(actions);
            self.request_frame(cx);
            let explorer = self.ui.widget(cx, ids!(explorer));
            if let Some(mut explorer) = explorer.borrow_mut::<ExplorerView>() {
                for action in explorer.view_actions(actions) {
                    match &action {
                        ViewAction::Ops { ops, label, .. } => {
                            log!("fab.sheetview: ops {label}");
                            for op in ops {
                                if let DesignOp::Bench { op, .. } = op {
                                    log!("fab.sheetview: bench op {op:?} (not applied here)");
                                }
                            }
                        }
                        ViewAction::Preview { tag, label, .. } => {
                            log!("fab.sheetview: preview t{tag} {label}");
                        }
                        ViewAction::CommitPreview => {
                            log!("fab.sheetview: commit preview");
                        }
                        ViewAction::CancelPreview => {
                            log!("fab.sheetview: cancel preview");
                        }
                        _ => {}
                    }
                    let Some(driver) = &mut self.driver else { continue };
                    if let Some(ViewAction::Focus(focus)) = driver.act(action) {
                        if let Sel::Comp(comp) = focus.sel {
                            let Some(snap) = driver.snap().cloned() else { continue };
                            let unit = snap.design.units[0].id;
                            if comp.board != unit { continue; }
                            let Some(sheet) = snap.of(&snap.sheets, unit) else { continue };
                            let Some(row) = sheet.value.rows.comp(comp.comp.0) else { continue };
                            let Some(unit) = snap.live().unit(unit) else { continue };
                            let Some(comp) = unit.circuit.comps.get(comp.comp.0 as usize) else { continue };
                            if self.select(Some((row, &comp.path))) {
                                self.bind_items(cx, &mut explorer);
                            }
                        }
                    }
                }
            };
        }
    }
}
