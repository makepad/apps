//! The library explorer pages share: the lane list, the search/grid/list
//! tools, the tag and order head, and the card list (`CutMediaList`), with
//! one controller (`LibraryBrowser`) for the state and the store worker. CUT
//! and DECK both host it: a page lists the lanes it wants, decorates the rows
//! (badges, used ranges, drafts), and decides what a click means; the
//! explorer's widgets are found under the page's own root.

use super::store::{CutStore, StoreJob, StoreReply};
use makepad_library_browser::lanes::{LibLane, LibRow, Thumb};
use makepad_asset_data::AssetRevisionRef;
use makepad_widgets::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use crate::still_cache::{Demand, Spec, Size, Stills};

/// Thumbnails uploaded as textures per pump.
const UPLOADS_PER_PUMP: usize = 2;
/// Replies read per pump.
const REPLIES_PER_PUMP: usize = 8;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.StageLane = StageButton{width: Fill height: 28 margin: 0 padding: Inset{left: 12 right: 8} align: Align{x: 0.0 y: 0.5}
        icon_walk: Walk{width: 14 height: 14 margin: Inset{right: 8}}
        draw_icon +: {color: stage.faint}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    mod.widgets.StageLaneCount = Label{width: Fit text: "" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 8}}}
    // A sidebar lane: the button, its selected bar, its count.
    mod.widgets.StageLaneRow = View{width: Fill height: 28 flow: Overlay
        lane := mod.widgets.StageLane{}
        View{width: Fit height: Fill padding: Inset{left: 1} align: Align{y: 0.5}
            bar := SolidView{visible: false width: 2 height: 16 draw_bg +: {color: stage.accent}}}
        View{width: Fill height: Fill padding: Inset{right: 8} align: Align{x: 1.0 y: 0.5}
            count := mod.widgets.StageLaneCount{}}
    }

    let Toggle = StageIconButton{width: 24 height: 22 margin: 0 icon_walk: Walk{width: 11 height: Fit}}

    // The explorer's search and grid/list toggles.
    mod.widgets.StageLibraryTools = View{width: Fit height: Fit flow: Right spacing: 6 align: Align{y: 0.5}
        Tip{text: "Grid" lib_grid := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/edit/grid.svg")}}}
        Tip{text: "List" lib_list_mode := Toggle{draw_icon +: {svg: crate_resource("self:resources/icons/list.svg")}}}
        Tip{text: "Search this lane as you type" RoundedView{width: 180 height: 22 flow: Overlay show_bg: true
            draw_bg +: {color: stage.well border_color: stage.line border_size: 1.0 border_radius: 2.5}
            View{width: Fit height: Fill padding: Inset{left: 7} align: Align{y: 0.5}
                Icon{icon_walk: Walk{width: 10 height: Fit} draw_icon +: {color: stage.muted svg: crate_resource("self:resources/icons/ui/search.svg")}}}
            lib_search := TextInput{width: Fill height: 22 margin: 0
                padding: Inset{left: 22 right: 6 top: 3 bottom: 3} empty_text: "Search Video"
                draw_bg +: {color: #0000 color_hover: #0000 color_focus: #0000 border_size: 0.0}
                draw_text +: {text_style: theme.font_regular{font_size: 9}}}}}
    }

    // The explorer's head: the lane's caption, the tag filter, the order.
    mod.widgets.StageLibraryHead = View{width: Fill height: 28 flow: Right spacing: 6 padding: Inset{left: 8 right: 8} align: Align{y: 0.5}
        lib_caption := Label{width: Fill max_lines: 1 text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
        Tip{text: "Only items with this tag" lib_tag := DropDown{width: 96 height: 20 margin: 0 labels: ["All tags"]
            draw_text +: {text_style: theme.font_regular{font_size: 8}}}}
        Tip{text: "Newest first, or by name" lib_sort := DropDown{width: 72 height: 20 margin: 0 labels: ["Newest" "A–Z"]
            draw_text +: {text_style: theme.font_regular{font_size: 8}}}}}

    // DECK's search: the one control the Library has besides "+ New deck";
    // it fills the rest of that row.
    mod.widgets.StageDeckSearch = Tip{width: Fill text: "Search the decks as you type" RoundedView{width: Fill height: 22 flow: Overlay show_bg: true
        draw_bg +: {color: stage.well border_color: stage.line border_size: 1.0 border_radius: 2.5}
        View{width: Fit height: Fill padding: Inset{left: 7} align: Align{y: 0.5}
            Icon{icon_walk: Walk{width: 10 height: Fit} draw_icon +: {color: stage.muted svg: crate_resource("self:resources/icons/ui/search.svg")}}}
        lib_search := TextInput{width: Fill height: 22 margin: 0
            padding: Inset{left: 22 right: 6 top: 3 bottom: 3} empty_text: "Search Decks"
            draw_bg +: {color: #0000 color_hover: #0000 color_focus: #0000 border_size: 0.0}
            draw_text +: {text_style: theme.font_regular{font_size: 9}}}}}
}

/// What a page's lane list holds: the store's lanes, and the pages' own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageLane {
    Store(LibLane),
    /// Web grabs (WB1): the store's videos tagged `web-grab`, and the
    /// grabs under way.
    Web,
    Effects,
    Transitions,
}

/// A lane's name in the search field's hint and the captions.
pub fn lane_name(lane: Option<PageLane>) -> &'static str {
    match lane {
        Some(PageLane::Store(lane)) => match lane {
            LibLane::Video => "Video",
            LibLane::Image => "Images",
            LibLane::Audio => "Audio",
            LibLane::Cuts => "Cuts",
            LibLane::AppRuns => "App runs",
            LibLane::Decks => "Decks",
            LibLane::Scores => "Scores",
            LibLane::Samples => "Samples",
            LibLane::SoundFonts => "SoundFonts",
            LibLane::Voices => "Voices",
        },
        Some(PageLane::Web) => "web grabs",
        Some(PageLane::Effects) => "Effects",
        Some(PageLane::Transitions) => "Transitions",
        None => "",
    }
}

/// The explorer's state and its one code path, for every page that
/// browses the library: the lane, the query, the rows the store answered,
/// their thumbnails, the selection, the search/tag/order/grid state, the
/// store worker. A page hosts it, decorates the rows it lists (badges,
/// used ranges, drafts) and decides what a click means; `root` scopes the
/// explorer's widgets to the page's own (two pages can both have one).
#[derive(Default)]
pub struct LibraryBrowser {
    root: LiveId,
    still_specs: HashMap<(AssetRevisionRef, Option<(AssetRevisionRef, String)>), Option<Arc<Spec>>>,
    still_pending: HashSet<(AssetRevisionRef, Option<(AssetRevisionRef, String)>)>,
    still_ids: HashMap<(AssetRevisionRef, Option<(AssetRevisionRef, String)>), usize>,
    still_seen: u64,

    pub store: Option<CutStore>,
    /// Jobs a full queue held back, retried each pump.
    held: VecDeque<StoreJob>,
    pub lane: Option<PageLane>,
    pub search: String,
    pub query_serial: u64,
    pub rows: Vec<LibRow>,
    pub list_selected: Option<usize>,
    pub status: String,
    pub uploads: VecDeque<(AssetRevisionRef, Thumb)>,
    pub thumbs: HashMap<String, (Thumb, Texture)>,
    /// The tag filter, the lane's tags, A–Z order.
    pub tag: Option<String>,
    pub tags: Vec<(String, u64)>,
    pub az: bool,
    pub list_mode: bool,
    /// Items per library lane.
    pub counts: HashMap<LibLane, u64>,
    /// The selected card's item (asset id, `draft:<id>` or a preset), and
    /// the index last painted for it: the selection follows its item.
    pub list_key: Option<String>,
    pub list_painted: Option<usize>,
    /// The lane the sidebar was last painted for.
    pub painted_lane: Option<PageLane>,
}

/// What `pump` found.
#[derive(Default)]
pub struct Pumped {
    /// The lane whose rows just arrived.
    pub rows_of: Option<LibLane>,
    /// Thumbnails were uploaded as textures.
    pub uploaded: bool,
    /// Replies that are the page's (a document opened or saved, ...).
    pub others: Vec<StoreReply>,
}

impl LibraryBrowser {
    // A shared-file edit changes the tree head even when deck.splash's own
    // revision is unchanged. Relative paths also belong to this identity.
    fn still_key(row: &LibRow) -> (AssetRevisionRef, Option<(AssetRevisionRef, String)>) {
        (row.reference, row.tree.as_ref().map(|tree| (tree.reference, tree.path.clone())))
    }

    pub fn card_aspect(&self, row: &LibRow) -> Option<f64> {
        self.still_specs.get(&Self::still_key(row))?.as_ref().map(|spec| spec.cut.output.width as f64 / spec.cut.output.height.max(1) as f64)
    }

    pub fn live_name(&self, row: &LibRow) -> Option<&str> {
        self.still_specs.get(&Self::still_key(row))?.as_ref()?.live_name.as_deref()
    }

    /// The cards drawn now (list index, width) and the list's DPI.
    pub fn visible_cards(&self, cx: &mut Cx, ui: &WidgetRef) -> (f64, Vec<(usize, f64)>) {
        let list = self.list(cx, ui);
        let dpi = cx.get_dpi_factor_of(&list.area());
        let visible = list.borrow::<makepad_library_browser::list::CutMediaList>().map(|list| {
            let (_, drawn) = list.wanted();
            drawn.map(|index| (index, list.card_width(index))).collect::<Vec<_>>()
        }).unwrap_or_default();
        (dpi, visible)
    }

    /// The library rows' stills; `first`: the list index of the first row
    /// (cards before it are the page's own, such as folder decks).
    pub fn still_demands(&mut self, cx: &mut Cx, ui: &WidgetRef, first: usize) -> Vec<Demand> {
        let (dpi, visible) = self.visible_cards(cx, ui);
        let mut demands = Vec::new();
        for (index, width) in visible {
            let Some(row) = index.checked_sub(first).and_then(|index| self.rows.get(index)) else { continue };
            let kind = match self.lane { Some(PageLane::Store(LibLane::Decks)) => makepad_asset_data::AssetKind::Deck, Some(PageLane::Store(LibLane::Cuts)) => makepad_asset_data::AssetKind::Cut, _ => continue };
            let reference = row.reference;
            let key = Self::still_key(row);
            if let Some(Some(spec)) = self.still_specs.get(&key) {
                let next = usize::MAX - self.still_ids.len();
                let id = *self.still_ids.entry(key).or_insert(next);
                demands.push(Demand { id, size: Size::new(width, dpi, spec.cut.output.width as f64 / spec.cut.output.height.max(1) as f64), spec: spec.clone() });
            } else if !self.still_specs.contains_key(&key) && self.still_pending.insert(key) {
                let tree = row.tree.clone();
                self.send(StoreJob::CardSource { reference, kind, tree });
            }
        }
        demands
    }

    pub fn accept_stills(&mut self, stills: &Stills) -> bool {
        if self.still_seen == stills.revision { return false; }
        self.still_seen = stills.revision;
        let mut changed = false;
        for row in &self.rows {
            let Some(id) = self.still_ids.get(&Self::still_key(row)) else { continue };
            if let (Some(thumb), Some(texture)) = (stills.thumb(*id), stills.texture(*id)) {
                self.thumbs.insert(row.reference.asset_id.to_string(), (thumb, texture));
                changed = true;
            }
        }
        changed
    }

    /// Start the explorer of the page whose widgets sit under `root`.
    pub fn attach(&mut self, cx: &mut Cx, root: LiveId) {
        self.root = root;
        self.store = Some(CutStore::new(cx));
    }

    /// A widget of the explorer: `id` under the page's root.
    pub fn widget(&self, cx: &mut Cx, ui: &WidgetRef, id: LiveId) -> WidgetRef {
        ui.widget(cx, &[self.root, id])
    }

    /// The card list.
    pub fn list(&self, cx: &mut Cx, ui: &WidgetRef) -> WidgetRef {
        self.widget(cx, ui, live_id!(lib_list))
    }

    /// A job for the asset worker; held until its queue has room.
    pub fn send(&mut self, job: StoreJob) {
        self.held.push_back(job);
        self.flush_held();
    }

    pub fn flush_held(&mut self) {
        let Some(store) = &self.store else { return };
        while let Some(job) = self.held.pop_front() {
            if let Err(job) = store.try_send(job) {
                self.held.push_front(job);
                break;
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(store) = &mut self.store {
            store.stop();
        }
    }

    /// Show `lane`; false when it already is shown.
    pub fn set_lane(&mut self, lane: PageLane) -> bool {
        if self.lane == Some(lane) {
            return false;
        }
        self.lane = Some(lane);
        self.list_selected = None;
        self.tag = None;
        self.tags.clear();
        self.query();
        true
    }

    /// Ask the store for the lane's items again.
    pub fn query(&mut self) {
        self.rows.clear();
        match self.lane {
            Some(PageLane::Store(lane)) => self.ask(lane, self.tag.clone(), format!("Loading {}…", lane.label().to_lowercase())),
            // Finished grabs are the store's videos tagged `web-grab`.
            Some(PageLane::Web) => self.ask(LibLane::Video, Some("web-grab".into()), "Loading web grabs…".into()),
            Some(PageLane::Effects) => self.status = "Drag an effect onto a clip, or choose it and click the clip".into(),
            Some(PageLane::Transitions) => self.status = "Drag a transition onto a cut between clips · cmd-T adds a crossfade".into(),
            None => {}
        }
    }

    fn ask(&mut self, lane: LibLane, tag: Option<String>, status: String) {
        self.query_serial += 1;
        if let Some(store) = &self.store {
            store.set_query_serial(self.query_serial);
        }
        self.status = status;
        self.send(StoreJob::Query { serial: self.query_serial, lane, text: self.search.clone(), tag, az: self.az });
    }

    /// The worker's replies (a handful per pump) and a couple of thumbnail
    /// uploads. Rows and thumbnails are the explorer's; the rest is returned.
    pub fn pump(&mut self, cx: &mut Cx) -> Pumped {
        self.flush_held();
        let mut pumped = Pumped::default();
        for _ in 0..REPLIES_PER_PUMP {
            let Some(reply) = self.store.as_ref().and_then(|store| store.try_recv()) else { break };
            match reply {
                StoreReply::Rows { serial, lane, result } => {
                    if serial != self.query_serial {
                        continue;
                    }
                    match result {
                        Ok((rows, tags)) => {
                            self.tags = tags;
                            self.status = match rows.len() {
                                0 => format!("No {} in the library", lane.label().to_lowercase()),
                                n => format!("{n} items"),
                            };
                            self.rows = rows;
                            pumped.rows_of = Some(lane);
                        }
                        Err(error) if error == "cancelled" => {}
                        Err(error) => {
                            self.rows.clear();
                            self.status = format!("The library did not answer: {error}");
                            pumped.rows_of = Some(lane);
                        }
                    }
                }
                StoreReply::Thumb { reference, thumb } => self.uploads.push_back((reference, thumb)),
                StoreReply::CardSource { reference, tree, result } => {
                    let key = (reference, tree.as_ref().map(|tree| (tree.reference, tree.path.clone())));
                    self.still_pending.remove(&key);
                    match result {
                        Ok(spec) => {
                            if let Some(name) = spec.as_ref().and_then(|spec| spec.live_name.as_ref()) {
                                for row in self.rows.iter_mut().filter(|row| Self::still_key(row) == key) { row.detail = format!("App · {name}"); }
                                self.thumbs.remove(&reference.asset_id.to_string());
                            }
                            self.still_specs.insert(key, spec.map(Arc::new));
                        }
                        Err(error) => { log!("library still: {reference:?}: {error}"); self.still_specs.insert(key, None); }
                    }
                    pumped.uploaded = true;
                }
                other => pumped.others.push(other),
            }
        }
        for _ in 0..UPLOADS_PER_PUMP {
            let Some((reference, thumb)) = self.uploads.pop_front() else { break };
            // A black picture reads as a broken file: the card keeps its kind glyph.
            if super::is_black(&thumb) {
                continue;
            }
            let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                width: thumb.width, height: thumb.height, data: Some((*thumb.pixels).clone()), updated: TextureUpdated::Full,
            }).label(cx, "edit/film");
            self.thumbs.insert(reference.asset_id.to_string(), (thumb, texture));
            pumped.uploaded = true;
        }
        pumped
    }

    /// The explorer's own controls: the lane buttons of `lanes`, grid/list,
    /// tag, order, search. True when the page must paint the library again.
    pub fn handle_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions, lanes: &[(LiveId, PageLane)]) -> bool {
        let mut changed = false;
        for (id, lane) in lanes {
            if ui.button(cx, &[*id, live_id!(lane)]).clicked(actions) {
                changed |= self.set_lane(*lane);
            }
        }
        for (id, list) in [(live_id!(lib_grid), false), (live_id!(lib_list_mode), true)] {
            if self.widget(cx, ui, id).as_button().clicked(actions) {
                self.list_mode = list;
                changed = true;
            }
        }
        if let Some(item) = self.widget(cx, ui, live_id!(lib_tag)).as_drop_down().changed(actions) {
            self.tag = (item > 0).then(|| self.tags.get(item - 1).map(|(tag, _)| tag.clone())).flatten();
            self.list_selected = None;
            self.query();
            changed = true;
        }
        if let Some(item) = self.widget(cx, ui, live_id!(lib_sort)).as_drop_down().changed(actions) {
            self.az = item == 1;
            self.list_selected = None;
            self.query();
            changed = true;
        }
        changed |= self.handle_search(cx, ui, actions);
        changed
    }

    /// Only the search field (DECK's Library has no other control here).
    pub fn handle_search(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) -> bool {
        let Some(text) = self.widget(cx, ui, live_id!(lib_search)).as_text_input().changed(actions) else { return false };
        self.search = text;
        self.list_selected = None;
        self.query();
        true
    }

    /// Whether a search is typed.
    pub fn searching(&self) -> bool {
        !self.search.trim().is_empty()
    }

    /// Paint the explorer's chrome: the lane buttons of `lanes` (selected
    /// wash, bar, count), the toggles, the search hint, the tag and order
    /// drop-downs and the caption (`used`: how many listed items the open
    /// document uses).
    pub fn paint_chrome(&mut self, cx: &mut Cx, ui: &WidgetRef, lanes: &[(LiveId, PageLane)], compact: bool, used: usize) {
        let p = makepad_look::palette::get(cx);
        self.painted_lane = self.lane;
        for (id, on) in [(live_id!(lib_grid), !self.list_mode), (live_id!(lib_list_mode), self.list_mode)] {
            let mut button = self.widget(cx, ui, id);
            let (bg, hover, icon) = if on { (p.accent_wash, p.accent_wash, p.accent) } else { (p.control, p.control_hover, p.label) };
            let (bg, hover, icon) = (Vec4f::from_u32(bg), Vec4f::from_u32(hover), Vec4f::from_u32(icon));
            script_apply_eval!(cx, button, {
                draw_bg +: {color: #(bg) color_focus: #(bg) color_hover: #(hover)}
                draw_icon +: {color: #(icon)}
            });
        }
        for (id, lane) in lanes {
            let selected = self.lane == Some(*lane);
            let mut button = ui.widget(cx, &[*id, live_id!(lane)]);
            let bg = Vec4f::from_u32(if selected { p.accent_wash } else { p.panel });
            let hover = Vec4f::from_u32(if selected { p.accent_wash } else { p.control_hover });
            let ink = Vec4f::from_u32(if selected { p.text_hi } else { p.label });
            let icon = Vec4f::from_u32(if selected { p.accent } else { p.faint });
            script_apply_eval!(cx, button, {
                draw_bg +: {color: #(bg) color_focus: #(bg) color_hover: #(hover) border_size: 0.0}
                draw_text +: {color: #(ink) color_focus: #(ink) color_hover: #(ink)}
                draw_icon +: {color: #(icon) color_focus: #(icon) color_hover: #(icon)}
            });
            ui.widget(cx, &[*id, live_id!(bar)]).set_visible(cx, selected);
            let count = match lane {
                PageLane::Store(lane) => self.counts.get(lane).map(|count| count.to_string()).unwrap_or_default(),
                _ => String::new(),
            };
            ui.label(cx, &[*id, live_id!(count)]).set_text(cx, if compact { "" } else { &count });
        }
        let lane_name = lane_name(self.lane);
        self.widget(cx, ui, live_id!(lib_search)).as_text_input().set_empty_text(cx, format!("Search {lane_name}"));
        let caption = match self.lane {
            Some(PageLane::Store(lane)) if !self.rows.is_empty() => {
                let noun = match (lane, self.rows.len() == 1) {
                    (LibLane::Video, false) => "videos", (LibLane::Video, true) => "video",
                    (LibLane::Image, false) => "images", (LibLane::Image, true) => "image",
                    (LibLane::Audio, false) => "sounds", (LibLane::Audio, true) => "sound",
                    (LibLane::Cuts, false) => "edits", (LibLane::Cuts, true) => "edit",
                    (LibLane::AppRuns, false) => "app runs", (LibLane::AppRuns, true) => "app run",
                    (LibLane::Scores, false) => "scores", (LibLane::Scores, true) => "score",
                    (LibLane::Decks, false) => "decks", (LibLane::Decks, true) => "deck",
                    (LibLane::Samples | LibLane::SoundFonts | LibLane::Voices, _) => "items",
                };
                if self.search.trim().is_empty() {
                    if used > 0 { format!("{} {noun} · {used} used", self.rows.len()) } else { format!("{} {noun}", self.rows.len()) }
                } else {
                    format!("Search: {} results in {lane_name}", self.rows.len())
                }
            }
            _ => String::new(),
        };
        self.widget(cx, ui, live_id!(lib_caption)).as_label().set_text(cx, &caption);
    }

    /// The head's tag labels and order, for the store's lanes.
    pub fn paint_head(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let store = matches!(self.lane, Some(PageLane::Store(_) | PageLane::Web));
        self.widget(cx, ui, live_id!(lib_head)).set_visible(cx, store);
        let mut labels = vec!["All tags".to_string()];
        labels.extend(self.tags.iter().map(|(tag, count)| format!("{tag} ({count})")));
        let picked = self.tag.as_ref().and_then(|tag| self.tags.iter().position(|(t, _)| t == tag)).map_or(0, |index| index + 1);
        let tag = self.widget(cx, ui, live_id!(lib_tag)).as_drop_down();
        tag.set_labels(cx, labels);
        tag.set_selected_item(cx, picked);
        self.widget(cx, ui, live_id!(lib_sort)).as_drop_down().set_selected_item(cx, usize::from(self.az));
    }
}
