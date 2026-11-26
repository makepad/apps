//! Makepad Story: video cutting (CUT) and presentation decks (DECK) on the
//! product shell (`makepad-app-shell`): the window's pages and header, its
//! one chat, and the AI control API on the shell's tool host.
//!
//! [`StoryApp`] owns the window: the two pages ([`cut`], [`deck`]), the
//! chat docked beside them, the output window ([`output`]: DECK's picture
//! on a second screen, and present mode's), the sound the pages play through
//! ([`sound`]) and the generation studio the video maker drives
//! ([`flow_host`]). The VJ effects and the kinetic type kits are optional
//! content packs (feature `packs`); without them Story draws on Motion
//! alone.
//!
//! The CUT page's Web lane has no grabs yet: they record through a
//! browser root, which Story does not have.

mod cut;
/// The DECK page: decks of looping cuts.
mod deck;
mod document_tree;
mod flow_host;
mod output;
mod sound;
mod still_cache;
mod structure;
mod ui;
/// File > Export > Web page…: a document as one wasm page (wasm-export).
mod web_page_export;

use makepad_app_shell::chat::ChatDock;
use makepad_app_shell::pages::{self, Pages, ProductPage};
use makepad_app_shell::pump_pace::{self, PumpPace};
use makepad_app_shell::theme_choice::ThemePicker;
use makepad_flow_studio::FlowStudio;
use makepad_widgets::*;
use output::StoryOutput;
use sound::StorySound;

use cut::VideoCut;
use deck::DeckPage;
use web_page_export::WebPageExport;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    startup() do #(StoryApp::script_component(vm)){
        ui: mod.story.Ui{}
    }
}

/// The page shown: CUT or DECK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoryPage {
    Cut,
    Deck,
}

impl ProductPage for StoryPage {
    fn tab(self) -> LiveId {
        match self {
            StoryPage::Cut => live_id!(tab_cut),
            StoryPage::Deck => live_id!(tab_deck),
        }
    }

    fn id(self) -> LiveId {
        match self {
            StoryPage::Cut => live_id!(cut_page),
            StoryPage::Deck => live_id!(deck_page),
        }
    }
}

impl StoryPage {
    /// The order the pages build in: DECK first (the window opens on it),
    /// CUT after the first frame.
    const BUILD_ORDER: &'static [StoryPage] = &[StoryPage::Deck, StoryPage::Cut];
}

#[derive(Script, ScriptHook)]
pub struct StoryApp {
    #[live]
    ui: WidgetRef,
    /// The CUT page: the timeline editor over one cut.
    #[rust]
    video_cut: VideoCut,
    /// The DECK page: a deck of looping cuts.
    #[rust]
    deck: DeckPage,
    /// File > Export > Web page…
    #[rust]
    web_page: WebPageExport,
    /// The window's one chat (the shell's dock; its tools are the control API).
    #[rust]
    chat: ChatDock,
    /// The generation studio the video maker fires presets through (no page).
    #[rust]
    flow_studio: FlowStudio,
    /// The output window: DECK's picture on another screen.
    #[rust]
    output: StoryOutput,
    /// The caption's theme picker (the shell's).
    #[rust]
    theme: ThemePicker,
    /// Present mode's output window: where it goes and whether present mode
    /// opened it.
    #[rust]
    present_output: Option<deck::PresentOutput>,
    /// The pages' sound: one deck playing the preview feed.
    #[rust]
    sound: Option<StorySound>,
    /// The asset store the workers use: attached, or hosted here.
    #[rust]
    asset_host: Option<makepad_asset_connection::host::AssetHost>,
    #[rust]
    timer: Timer,
    /// The pump timer's rate: full while pumps find work, an idle check after.
    #[rust]
    pump_pace: PumpPace,
    #[rust(StoryPage::Deck)]
    page: StoryPage,
    /// The window's pages and which are built: the first before the first
    /// frame, the other after it.
    #[rust]
    pages: Pages<StoryPage>,
    #[rust]
    closing: bool,
}

impl AppMain for StoryApp {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        // The extra style sheets join the catalogue, and the theme picked
        // last time goes on, before the first theme load.
        makepad_widgets_sheets::register();
        makepad_app_shell::theme_choice::install_saved(vm);
        #[cfg(feature = "tweaker")]
        makepad_widgets_tweaker::link(vm);
        makepad_widgets::script_mod(vm);
        makepad_app_aichat::script_mod(vm);
        makepad_flowgraph::script_mod(vm);
        // Inline value controls in code views (Motion's Source view).
        makepad_code_values::script_mod(vm);
        makepad_live_frame::script_mod(vm);
        makepad_stage_ui::script_mod(vm);
        makepad_flow_ui::theme::script_mod(vm);
        makepad_value_pickers::script_mod(vm);
        makepad_stage_motion_pickers::script_mod(vm);
        structure::script_mod(vm);
        makepad_flow_ui::script_mod(vm);
        makepad_stage_ui::live_grid::script_mod(vm);
        // The pages' effect picker: the installed effect pack's, none here.
        makepad_content_packs::picker::script_mod(vm);
        makepad_app_shell::theme_choice::script_mod(vm);
        // The graphic desk's editor panels: every panel isolate gets the
        // panel widgets and `scene`.
        makepad_editor_panel::register(vm, makepad_stage_motion_pickers::motion_doc(), &[makepad_stage_motion_pickers::isolate_mod]);
        cut::register(vm);
        deck::register(vm);
        // The output window's overlay is DECK's render in another language.
        output::script_mod(vm);
        flow_host::install_host();
        ui::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // F10 and the bridge's chat requests are the one chat's, not the
        // window's own overlay's.
        self.chat.handle_event(event);
        self.take_chat_requests(cx);
        match self.page {
            StoryPage::Cut => self.video_cut.handle_event(cx, &self.ui, event),
            StoryPage::Deck => {
                self.deck.handle_event(cx, &self.ui, event, self.output.window_id().filter(|_| self.output.is_open()));
                self.video_cut.suspend_deck_layout(self.deck.own_window() || self.deck.presenting().is_some());
            }
        }
        match event {
            Event::Startup => self.startup(cx),
            // A registered output callback plays only on a device the app
            // chose: the system's default, again whenever the list changes.
            Event::AudioDevices(devices) if self.sound.is_some() => cx.use_audio_outputs(&devices.default_output()),
            Event::WindowCloseRequested(request) if self.ui.window(cx, ids!(main_window)).window_id() == Some(request.window_id) => {
                // The window closes once the workers have stopped.
                request.accept_close.set(false);
                self.begin_close(cx);
            }
            Event::QuitRequested(request) => {
                request.handle();
                self.begin_close(cx);
            }
            Event::Shutdown => {
                self.output.close(cx);
                // The folder watchers' threads end with their watchers.
                self.video_cut.close_folder();
                self.deck.close_folder();
                cut::folder_docs::stop_cards();
                // the kernel pool (motion, effects, render kernels) ends with the app: under
                // `makepad run` the app's domain waits for every thread it started
                makepad_script_compute_sched::sched::ThreadExecutor::stop_shared();
                // the AI hub's LAN discovery listener (the provider menu looked for the fleet)
                // ends with the app too
                makepad_ai_hub::discovery::stop_listener();
                self.video_cut.stop();
                self.deck.stop(cx, &self.ui);
                self.flow_studio.stop();
                if let Some(host) = &mut self.asset_host {
                    host.stop();
                }
            }
            _ => {}
        }
        // Input, a signal or an action: the pump is at full rate again.
        if !matches!(event, Event::Draw(_) | Event::NextFrame(_) | Event::Timer(_)) {
            if let Some(interval) = self.pump_pace.woken() {
                self.set_pump_interval(cx, interval);
            }
        }
        if matches!(event, Event::Signal) || self.timer.is_event(event).is_some() {
            self.pump(cx);
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
        self.output.handle_event(cx, event);
        // Every window has drawn: the frame views on screen show what the
        // producers rendered in this draw.
        if let Event::Draw(_) = event {
            makepad_live_frame::refresh_shown_frames(cx);
        }
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
        if matches!(event, Event::LiveEdit | Event::ScriptReapply) {
            self.set_page(cx, self.page);
        }

    }
}

impl StoryApp {
    fn startup(&mut self, cx: &mut Cx) {
        // Motion documents import Splash modules from the library.
        cut::store::LibraryModules::install();
        // The field registry the structure panel reads (a load of every
        // layer type), built off the UI thread before a layer is shown.
        std::thread::spawn(|| {
            makepad_motion::field_registry::motion_types();
        });
        let mut args = std::env::args().skip(1).peekable();
        while let Some(arg) = args.next() {
            if arg == "--window-size" || arg.starts_with("--window-size=") {
                let text = match arg.strip_prefix("--window-size=") {
                    Some(text) => Some(text.to_string()),
                    None => args.next_if(|next| !next.starts_with("--")),
                };
                match text.as_deref().map(parse_window_size) {
                    Some(Ok(size)) => self.ui.window(cx, ids!(main_window)).resize(cx, size),
                    Some(Err(error)) => eprintln!("story: --window-size: {error}; the window keeps its default size"),
                    None => eprintln!("story: --window-size needs <width>x<height>; the window keeps its default size"),
                }
            }
        }
        self.asset_host = Some(makepad_asset_connection::host::AssetHost::start());
        match StorySound::open(cx) {
            Ok(sound) => self.sound = sound,
            Err(error) => log!("story: no sound: {error}"),
        }
        // The page shown first is the only one built before the first frame.
        self.pages = Pages::new(StoryPage::BUILD_ORDER.to_vec());
        let first = StoryPage::BUILD_ORDER[0];
        self.ensure_page(cx, first);
        self.timer = cx.start_interval(pump_pace::FAST);
        self.set_page(cx, first);
    }

    fn set_pump_interval(&mut self, cx: &mut Cx, interval: f64) {
        cx.stop_timer(self.timer);
        self.timer = cx.start_interval(interval);
    }

    /// One pump, and the pump clock's rate after it: a pump that asked for
    /// no frame, sent no action and found no web page export running or the
    /// window closing found nothing to do.
    fn pump(&mut self, cx: &mut Cx) {
        let before = (cx.frame_requests(), cx.new_actions.len());
        self.pump_once(cx);
        let busy = (cx.frame_requests(), cx.new_actions.len()) != before || self.closing || self.web_page_export_running();
        if let Some(interval) = self.pump_pace.pumped(busy) {
            self.set_pump_interval(cx, interval);
        }
    }

    fn pump_once(&mut self, cx: &mut Cx) {
        // The store is up (hosted here) or known to be served elsewhere.
        if let Some(note) = self.asset_host.as_mut().and_then(makepad_asset_connection::host::AssetHost::take_resolved) {
            trace!("story.assets", "asset store: {note}");
        }
        if let Some(sound) = &mut self.sound {
            sound.pump();
        }
        self.ui.level_meter(cx, ids!(utility_master_meter)).feed(cx, sound::take_peak() as f64);
        self.pump_web_page_export(cx);
        // A theme reload puts the open picker's markup back: its lit row again.
        self.theme.sync(cx, &self.ui);
        // Each page's pump writes its widgets: it runs once the page is built.
        if self.pages.is_built(StoryPage::Cut) {
            self.pump_video_cut(cx);
        }
        if self.pages.is_built(StoryPage::Deck) {
            self.video_cut.suspend_deck_layout(self.deck.own_window() || self.deck.presenting().is_some());
            if self.page == StoryPage::Deck {
                self.video_cut.apply_deck_layout(cx, &self.ui);
                let pixels = self.output.window_id().filter(|id| self.output.is_open() && cx.windows.is_valid(*id)).map(|id| {
                    let geom = &cx.windows[id].window_geom;
                    ((geom.inner_size.x * geom.dpi_factor).round().max(1.0) as u32, (geom.inner_size.y * geom.dpi_factor).round().max(1.0) as u32)
                });
                self.deck.set_output_pixels(cx, &self.ui, pixels);
            }
            self.deck.pump(cx, &self.ui, self.page == StoryPage::Deck);
            self.pump_present(cx);
        }
        // The control API reaches both pages: it starts when both are built.
        if self.pages.all_built() {
            self.pump_studio_api(cx);
        }
        // The chat's turn (the video maker pumps it on CUT's pump; DECK's
        // window pumps it here until CUT is built).
        if !self.pages.is_built(StoryPage::Cut) {
            self.pump_chat(cx);
        }
        // The generation studio: no page, its API's jobs and probes.
        self.flow_studio.pump(cx, &self.ui, false, &makepad_flow_studio::NoLiveRoots);
        // The other page is built after the first frame.
        if makepad_widgets::makepad_platform::first_present_seen() && !self.closing {
            if let Some(page) = self.pages.next_to_build() {
                self.ensure_page(cx, page);
            }
        }
        if self.closing && self.flow_studio.stopped() && self.output.window_id().is_none() {
            cx.quit();
        }
    }

    fn begin_close(&mut self, cx: &mut Cx) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.output.close(cx);
        self.video_cut.stop();
        self.deck.stop(cx, &self.ui);
        self.stop_studio_api();
        self.flow_studio.stop();
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(tab_cut)).clicked(actions) {
            self.set_page(cx, StoryPage::Cut);
        }
        if self.ui.button(cx, ids!(tab_deck)).clicked(actions) {
            self.set_page(cx, StoryPage::Deck);
        }
        if self.ui.button(cx, ids!(toggle_gen)).clicked(actions) || (self.page == StoryPage::Deck && self.ui.button(cx, ids!(deck_toggle_chat)).clicked(actions)) {
            self.toggle_chat(cx);
        }
        // The caption's master volume and mute (sound.rs: the device sink).
        if sound::handle_master_actions(cx, &self.ui, actions) {
            self.paint_navigation(cx);
        }
        self.chat_actions(cx, actions);
        if self.closing {
            return;
        }
        let menu = self.ui.menu_bar(cx, ids!(file_menu)).selected(actions);
        if self.ui.button(cx, ids!(open_output)).clicked(actions) || menu == Some(live_id!(output_window)) {
            self.sync_output_feed(cx);
            if let Err(error) = self.output.toggle(cx) {
                log!("story: output window: {error}");
            }
            self.paint_navigation(cx);
        }
        if menu == Some(live_id!(export_web_page)) {
            self.open_web_page_export(cx);
        }
        if self.ui.button(cx, ids!(theme_btn)).clicked(actions) {
            self.theme.open(cx, &self.ui);
        }
        if let Some(label) = self.theme.handle_actions(cx, &self.ui, actions) {
            log!("story: theme: {label}");
        }
        self.handle_web_page_export(cx, actions);
        self.video_cut.handle_actions(cx, &self.ui, actions);
        if self.page == StoryPage::Deck {
            self.deck.handle_actions(cx, &self.ui, actions, self.output.window_id().filter(|_| self.output.is_open()));
            self.video_cut.suspend_deck_layout(self.deck.own_window() || self.deck.presenting().is_some());
            if !self.deck.own_window() && self.deck.presenting().is_none() {
                self.video_cut.deck_layout_actions(cx, &self.ui, actions);
            }
        }
    }

    /// The header: the page tabs and the toggles that are on.
    fn paint_navigation(&self, cx: &mut Cx) {
        let palette = makepad_look::palette::get(cx);
        let gen_open = {
            let requests = cx.global::<makepad_widgets::ai_slot::AiSlotRequests>();
            requests.open.unwrap_or(requests.is_open)
        };
        let tabs: Vec<(LiveId, bool)> = StoryPage::BUILD_ORDER.iter().map(|page| (page.tab(), self.page == *page)).collect();
        let toggles = [(live_id!(toggle_gen), gen_open), (live_id!(open_output), self.output.is_open()), (live_id!(utility_mute), sound::muted()), (live_id!(theme_btn), false)];
        pages::paint_header(cx, &self.ui, &palette, &tabs, &toggles);
    }

    /// Present mode's asks of the output window: open it on the other
    /// display and make it fullscreen there once its native window exists,
    /// or put it back.
    fn pump_present(&mut self, cx: &mut Cx) {
        match self.deck.take_present_ask() {
            Some(deck::PresentAsk::OpenOutput { screen }) => {
                self.sync_output_feed(cx);
                let opened_here = !self.output.is_open();
                if opened_here {
                    if let Err(error) = self.output.toggle(cx) {
                        self.deck.present_failed(cx, &self.ui, &error);
                        return;
                    }
                }
                self.present_output = Some(deck::PresentOutput { screen, opened_here, placed: false });
            }
            Some(deck::PresentAsk::CloseOutput) => {
                if let Some(present) = self.present_output.take() {
                    if present.opened_here {
                        self.output.close(cx);
                    } else if let Some(id) = self.output.window_id() {
                        cx.push_unique_platform_op(CxOsOp::NormalizeWindow(id));
                    }
                }
            }
            None => {}
        }
        let Some(present) = &mut self.present_output else { return };
        if present.placed {
            return;
        }
        // The window is placed on the display once it is a native window.
        if let Some(id) = self.output.window_id().filter(|id| cx.windows.is_valid(*id) && cx.windows[*id].is_created) {
            cx.push_unique_platform_op(CxOsOp::RepositionWindow(id, present.screen.bounds.pos));
            cx.push_unique_platform_op(CxOsOp::FullscreenWindow(id));
            present.placed = true;
        }
    }

    /// The output window shows DECK's picture while DECK is the page.
    fn sync_output_feed(&mut self, cx: &mut Cx) {
        let deck = self.page == StoryPage::Deck && self.pages.is_built(StoryPage::Deck);
        let feed = if deck { self.deck.feed(cx, &self.ui) } else { None };
        let lane = deck.then(|| self.deck.output_lane());
        self.output.set_feed(cx, feed);
        self.output.set_deck_lane(cx, lane);
    }

    /// Build `page`'s widgets (once; a built page is left alone). The page
    /// does not become the shown one.
    fn ensure_page(&mut self, cx: &mut Cx, page: StoryPage) {
        if self.pages.is_built(page) {
            return;
        }
        self.pages.ensure(cx, &self.ui, page);
    }

    fn set_page(&mut self, cx: &mut Cx, page: StoryPage) {
        if !self.pages.has(page) {
            return;
        }
        // A page opened before its turn in the background build is built now.
        self.ensure_page(cx, page);
        let was_deck = self.page == StoryPage::Deck;
        self.page = page;
        if was_deck && page != StoryPage::Deck {
            self.deck.left(cx, &self.ui);
            self.video_cut.suspend_deck_layout(false);
        }
        // The output window shows the page's own picture on DECK.
        self.sync_output_feed(cx);
        self.pages.show(cx, &self.ui, page);
        self.paint_navigation(cx);
        self.ui.redraw(cx);
    }
}

// `app_main!` emits the platform entry points; [`story_main`] decides
// between this window and a headless workflow run first, so the macro lives
// in a module and the window enters through its generated `main`.
mod entry {
    use super::*;
    app_main!(StoryApp);

    pub fn on_app_thread() {
        app_main();
    }
}

/// The node types a headless workflow run of Story evaluates with: the
/// built-ins and the recipe checks.
fn registries() -> Result<(std::sync::Arc<makepad_flow::document::Registry>, std::sync::Arc<makepad_flow::engine::providers::ExecutorRegistry>), String> {
    makepad_flow_studio::nodes::registries_with(|_, _| Ok(()))
}

/// `makepad-story`'s `main`: `--export-web` exports a document as a web page,
/// `--panel-effects` checks that every panel control changes the film, and
/// `--headless`/`--check` run a workflow package, all without a window;
/// otherwise the window opens (the HAL owns the main thread, the app runs on
/// its own).
pub fn story_main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = web_page_export::export_web_main(&args) { return code; }
    if let Some(code) = crate::cut::motion_edit::panel_effects::panel_effects_main(&args) { return code; }
    if makepad_flow_studio::headless::wants_headless(&args) {
        return makepad_flow_studio::headless::main(args, registries, "makepad-story");
    }
    makepad_widgets::makepad_platform::run_app(start_window);
    std::process::ExitCode::SUCCESS
}

fn start_window() {
    isolate_generate_request();
    entry::on_app_thread();
}

/// A headless Generate run (`STAGE_GENERATE_REQUEST`) keeps its draft and
/// storyboards beside the request unless told otherwise, so it never opens
/// or overwrites the person's own CUT draft.
fn isolate_generate_request() {
    let Some(request) = std::env::var_os("STAGE_GENERATE_REQUEST").filter(|path| !path.is_empty()) else { return };
    let dir = std::path::Path::new(&request).parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    for (key, name) in [("STAGE_EDIT_DRAFTS", ".drafts"), ("STAGE_STORYBOARDS", ".boards")] {
        if std::env::var_os(key).filter(|value| !value.is_empty()).is_none() {
            std::env::set_var(key, dir.join(name));
        }
    }
}

/// The window's side bounds for `--window-size`.
const WINDOW_SIDE: std::ops::RangeInclusive<f64> = 200.0..=4096.0;

/// A window's logical size from `<width>x<height>`, for example `900x600`.
fn parse_window_size(text: &str) -> Result<Vec2d, String> {
    let (width, height) = text
        .split_once(|c| c == 'x' || c == 'X')
        .ok_or_else(|| format!("`{text}` is not <width>x<height>"))?;
    let side = |name: &str, value: &str| -> Result<f64, String> {
        let number: f64 = value.trim().parse().map_err(|_| format!("the {name} `{value}` is not a number"))?;
        if !number.is_finite() || !WINDOW_SIDE.contains(&number) {
            return Err(format!("the {name} `{value}` is outside {}..{}", WINDOW_SIDE.start(), WINDOW_SIDE.end()));
        }
        Ok(number)
    };
    Ok(dvec2(side("width", width)?, side("height", height)?))
}
