//! The Circuit Library lists parts to arm or drag onto the sheet. Section membership and search
//! ranks are cached. The portal draws one widget per visible row.

pub mod rank;
pub mod symbol;

#[cfg(test)]
mod tests;

use crate::ElecLive;
use crate::req_list::ReqDraft;
use fab_elec_bom::prices::{Library as PriceBook, Prices};
use fab_elec_bom::shortlist::{PriceCoverage, ShortlistContext, ShortlistEntry};
use fab_elec_design::Design;
use makepad_widgets_core::tip::TipWidgetExt;
use fab_elec_circuit::Circuit;
use fab_elec_lang::{Catalog, CatalogEntry, CatalogKind};
use fab_elec_parts::Lifecycle;
use makepad_widgets_core::{view::ViewAction as RowAction, *};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use symbol::{Mark, SymbolIcon, SymbolIconWidgetRefExt};

/// Internal drag item identifying a catalog definition, encoded as `"<lib>\n<name>"`.
pub const LIBRARY_ENTRY: LiveId = live_id!(fab_elec_library_entry);

/// A definition's identity within the project's catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryKey {
    /// The library name; an empty string names the project.
    pub lib: String,
    /// The definition's name within that library.
    pub name: String,
}

/// The Library and the sheet asking each other, as plain actions.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryAction {
    ChooseParts,
    /// A click on an entry: the sheet carries it from the pointer's next move and places it where
    /// the next click lands.
    Arm(EntryKey),
    /// The ordered shortcuts to keep on this machine after a pin or a change of recency.
    Remember { pinned: Vec<EntryKey>, recent: Vec<EntryKey> },
    /// The side and fold state the user chose for the Library float.
    Place { left: bool, folded: bool },
}

/// Reads a definition's identity from an internal Library drag.
pub fn library_drag_entry(items: &[DragItem]) -> Option<EntryKey> {
    items.iter().find_map(|item| {
        let DragItem::String { value, internal_id: Some(LIBRARY_ENTRY) } = item else {
            return None;
        };
        let (lib, name) = value.split_once('\n')?;
        if name.contains('\n') {
            return None;
        }
        Some(EntryKey { lib: lib.into(), name: name.into() })
    })
}

/// Encodes a definition's identity for a Library drag.
pub fn library_drag_item(key: &EntryKey) -> DragItem {
    DragItem::String {
        value: format!("{}\n{}", key.lib, key.name),
        internal_id: Some(LIBRARY_ENTRY),
    }
}

fn entry_key(entry: &CatalogEntry) -> EntryKey {
    EntryKey { lib: entry.lib.clone(), name: entry.name.clone() }
}

// Registers the panel and the templates it instantiates for visible rows.
script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.SymbolIconBase = #(SymbolIcon::register_widget(vm))
    mod.widgets.SymbolIcon = set_type_default() do mod.widgets.SymbolIconBase{
        width: 18 height: 12
        draw_svg +: {color: theme.color_text}
    }
    mod.widgets.LibraryPanelBase = #(LibraryPanel::register_widget(vm))
    mod.widgets.LibraryPanel = set_type_default() do mod.widgets.LibraryPanelBase{
        width: Fill
        height: Fill
        flow: Down
        padding: Inset{left: 8 right: 4 top: 0 bottom: 0}
        spacing: 4
        grab_key_focus: false
        // No ground of its own: the host's rounded float shows through (v10).

        drag_tip: DragChip{
            draw_bg +: {color: theme.color_outset border_color: theme.color_bevel_outset_2}
            label +: {
                draw_text +: {
                    color: theme.color_text
                    text_style: theme.font_bold{font_size: 9}
                }
            }
        }

        lib_choose_parts_tip := Tip{
            width: Fit height: Fit
            lib_choose_parts := Button{text: "Choose parts" enabled: false}
        }

        // Fab's density: the property kit's row height and body type (`fab.font_size_ui`), a
        // 22-point section row like Fab's panel headers, the detail in Fab's small dim type.
        // v10: Fab's search well: 20 high, the inset ground, the control radius, no border.
        search := TextInput{
            width: Fill
            height: 20
            margin: 0
            text: ""
            empty_text: "Search parts  (name, value, footprint)"
            draw_bg +: {
                color: theme.color_inset
                color_hover: theme.color_inset
                color_focus: theme.color_inset
                color_down: theme.color_inset
                color_empty: theme.color_inset
                color_2: vec4(-1.0, -1.0, -1.0, -1.0)
                border_size: 0.0
                border_radius: min(theme.corner_radius 2.5)
                border_color: theme.color_bevel_inset_2
                border_color_hover: theme.color_bevel_inset_2
                border_color_focus: theme.color_focus
                border_color_down: theme.color_bevel_inset_2
                border_color_empty: theme.color_bevel_inset_2
                border_color_2: vec4(-1.0, -1.0, -1.0, -1.0)
            }
            draw_text +: {
                color: theme.color_text
                color_hover: theme.color_text
                color_focus: theme.color_text
                color_down: theme.color_text
                color_empty: theme.color_text_meta
                color_empty_hover: theme.color_text_meta
                color_empty_focus: theme.color_text_meta
                text_style: theme.font_regular{font_size: theme.prop_font_size}
            }
        }
        entries := PortalList{
            width: Fill
            height: Fill
            flow: Down
            drag_scrolling: false
            grab_key_focus: false
            selectable: false
            padding: Inset{right: 8}
            scan: [@Header, @Row, @Detail, @Empty]

            // A section keeps its count visible when folded.
            Header := View{
                width: Fill
                height: 22
                flow: Right
                align: Align{x: 0.0 y: 0.5}
                padding: Inset{left: 4 right: 4}
                spacing: 3
                cursor: MouseCursor.Hand
                grab_key_focus: false
                show_bg: true
                draw_bg +: {
                    hover: instance(0.0)
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, min(theme.corner_radius 2.5))
                        sdf.fill(vec4(theme.color_outset_hover.xyz, theme.color_outset_hover.w * self.hover * 0.5))
                        return sdf.result
                    }
                }
                animator: Animator{
                    hover: {
                        default: @off
                        off: AnimatorState{
                            from: {all: Forward {duration: 0.1}}
                            apply: {draw_bg: {hover: 0.0}}
                        }
                        on: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 1.0}}
                        }
                    }
                }
                tri := Label{
                    width: 12
                    height: Fit
                    padding: 0
                    align: Align{x: 0.5}
                    draw_text +: {
                        color: theme.color_on_surface_variant
                        text_style: theme.font_regular{font_size: 7.5}
                    }
                }
                mark := mod.widgets.SymbolIcon{width: 14 height: 14 draw_svg +: {color: theme.color_on_surface_variant}}
                title := Label{
                    width: Fill
                    height: Fit
                    padding: 0
                    max_lines: 1
                    text_overflow: Ellipsis
                    draw_text +: {
                        color: theme.color_text
                        text_style: theme.font_bold{font_size: 9}
                    }
                }
                count := Label{
                    width: Fit height: Fit padding: 0
                    align: Align{x: 1.0}
                    draw_text +: {
                        color: theme.color_on_surface_variant
                        text_style: theme.font_regular{font_size: 7.5}
                    }
                }
            }
            // The name and its detail share the space between the symbol and the pin target.
            // Transparent on the float; hover is a rounded wash (v10: no square surfaces).
            Row := View{
                width: Fill
                height: 34
                flow: Right
                align: Align{y: 0.5}
                padding: Inset{left: 19 right: 2}
                spacing: 4
                cursor: MouseCursor.Hand
                grab_key_focus: false
                show_bg: true
                draw_bg +: {
                    hover: instance(0.0)
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, min(theme.corner_radius 2.5))
                        sdf.fill(vec4(theme.color_outset_hover.xyz, theme.color_outset_hover.w * self.hover))
                        return sdf.result
                    }
                }
                animator: Animator{
                    hover: {
                        default: @off
                        off: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 0.0}}
                        }
                        on: AnimatorState{
                            from: {all: Snap}
                            apply: {draw_bg: {hover: 1.0}}
                        }
                    }
                }
                labels := View{
                    width: Fill height: Fill flow: Down align: Align{y: 0.5}
                    first := View{
                        width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                        symbol := mod.widgets.SymbolIcon{}
                        title := Label{
                            width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                            draw_text +: {color: theme.color_text text_style: theme.font_regular{font_size: theme.prop_font_size}}
                        }
                        status := Label{
                            width: Fit height: Fit padding: 0 max_lines: 1
                            draw_text +: {color: theme.color_text text_style: theme.font_regular{font_size: 7.5}}
                        }
                        star := View{
                            width: 14 height: 14 flow: Overlay
                            outline := mod.widgets.SymbolIcon{width: Fill height: Fill draw_svg +: {color: theme.color_on_surface_variant}}
                            filled := mod.widgets.SymbolIcon{width: Fill height: Fill draw_svg +: {color: theme.color_warning}}
                        }
                    }
                    meta := Label{
                        width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                        draw_text +: {color: theme.color_on_surface_variant text_style: theme.font_regular{font_size: 7.5}}
                    }
                }
            }
            Detail := View{
                width: Fill
                height: Fit
                flow: Down
                spacing: 2
                padding: Inset{left: 19 top: 2 bottom: 6}
                summary := Label{
                    width: Fill
                    height: Fit
                    padding: 0
                    draw_text +: {
                        color: theme.color_text
                        text_style: theme.font_regular{font_size: 7.5}
                    }
                }
                pins := Label{
                    width: Fill
                    height: Fit
                    padding: 0
                    draw_text +: {
                        color: theme.color_on_surface_variant
                        text_style: theme.font_regular{font_size: 7.5}
                    }
                }
            }
            Empty := Label{
                width: Fill
                height: Fit
                padding: Inset{left: 4 top: 4}
                text: "Parts the project defines or imports appear here"
                draw_text +: {
                    color: theme.color_on_surface_variant
                    text_style: theme.font_regular{font_size: 7.5}
                }
            }
        }
        // v9's preview: the hovered part's symbol in a rounded well, its name and summary beside
        // it; the hint while no part is hovered.
        preview := View{
            width: Fill
            height: Fit
            flow: Right
            spacing: 8
            padding: Inset{top: 4 bottom: 4}
            well := View{
                width: 72
                height: 60
                padding: 6
                show_bg: true
                draw_bg +: {
                    pixel: fn() {
                        let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                        sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, min(theme.container_corner_radius 3.5))
                        sdf.fill(theme.color_inset)
                        return sdf.result
                    }
                }
                big := mod.widgets.SymbolIcon{width: Fill height: Fill draw_svg +: {color: theme.color_text}}
            }
            hint := Label{
                width: Fill
                height: Fit
                padding: Inset{top: 2}
                text: "Hover a part to see its symbol. Drag it onto the sheet, or click it and then click the sheet."
                draw_text +: {
                    color: theme.color_on_surface_variant
                    text_style: theme.font_regular{font_size: 7.5}
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Section {
    Pinned, Recent, Board, Passives, Parts, Blocks,
}

impl Section {
    const ALL: [Self; 6] = [Self::Pinned, Self::Recent, Self::Board, Self::Passives, Self::Parts, Self::Blocks];

    fn title(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::Recent => "Recent",
            Self::Board => "On this board",
            Self::Passives => "Passives",
            Self::Parts => "Parts",
            Self::Blocks => "Blocks",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Header(Section),
    Entry { index: usize, section: Option<Section>, pinned: bool },
    Detail,
    Empty,
}

pub const RECENT_COUNT: usize = 8;

/// Rows in display order. One pass over ranked matches and remembered keys. A remembered key is
/// resolved only against offered entries, so a missing library can reappear on the next rebuild.
fn rows_from_matches(
    entries: &[CatalogEntry], ranked: &[ShortlistEntry], query: &str, pinned: &[EntryKey], recent: &[EntryKey],
    used: &HashSet<String>, folded: &HashSet<Section>,
) -> (Vec<Row>, [String; Section::ALL.len()]) {
    let mut counts: [String; Section::ALL.len()] = Default::default();
    let pins: HashSet<_> = pinned.iter().map(|key| (key.lib.as_str(), key.name.as_str())).collect();
    let row = |index: usize, section| {
        let entry = &entries[index];
        Row::Entry { index, section, pinned: pins.contains(&(entry.lib.as_str(), entry.name.as_str())) }
    };
    if !query.trim().is_empty() {
        return (ranked.iter().map(|r| row(r.index, None)).collect(), counts);
    }
    if ranked.is_empty() {
        return (vec![Row::Empty], counts);
    }
    let mut by_key = HashMap::new();
    let mut sections: [Vec<usize>; Section::ALL.len()] = Default::default();
    for matched in ranked {
        let index = matched.index;
        let entry = &entries[index];
        let key = (entry.lib.as_str(), entry.name.as_str());
        by_key.insert(key, index);
        if pins.contains(&key) { sections[Section::Pinned as usize].push(index); }
        if entry.part.as_ref().is_some_and(|part| used.contains(&part.name)) {
            sections[Section::Board as usize].push(index);
        }
        let section = match entry.kind {
            CatalogKind::Passive => Section::Passives,
            CatalogKind::Part => Section::Parts,
            CatalogKind::Block => Section::Blocks,
        };
        sections[section as usize].push(index);
    }
    let lookup = |key: &EntryKey| by_key.get(&(key.lib.as_str(), key.name.as_str())).copied();
    sections[Section::Recent as usize].extend(recent.iter().take(RECENT_COUNT).filter_map(lookup));
    let mut rows = Vec::new();
    for (section, indices) in Section::ALL.into_iter().zip(sections) {
        if !indices.is_empty() {
            counts[section as usize] = indices.len().to_string();
            rows.push(Row::Header(section));
            if !folded.contains(&section) {
                rows.extend(indices.into_iter().map(|index| row(index, Some(section))));
            }
        }
    }
    (rows, counts)
}

/// The package code and one catalog fact beside a part name. Scans the footprint name and defaults
/// when the catalog changes. A block's defaults are circuit parameters, so it shows its pin count.
fn entry_detail(entry: &CatalogEntry) -> String {
    let pins = || match entry.pins.len() {
        1 => "1 pin".to_string(),
        n => format!("{n} pins"),
    };
    if entry.kind == CatalogKind::Block { return pins() }
    let package = entry.part.as_ref().and_then(|p| p.footprint.as_ref())
        .and_then(|fp| package_code(&fp.name));
    let fact = ["tol", "dielectric", "color", "value"].into_iter().find_map(|key| {
        let (_, text) = entry.defaults.iter().find(|(name, _)| name == key)?;
        let value = default_text(text);
        if value.is_empty() || value.contains(['[', '{', '(', '\n']) { return None }
        Some(match value.strip_suffix('%').filter(|_| key == "tol") {
            Some(n) => format!("{} %", n.trim_end()),
            None => quantity_text(value).unwrap_or_else(|| value.to_string()),
        })
    }).unwrap_or_else(pins);
    match package {
        Some(code) => format!("{code}, {fact}"),
        None => fact,
    }
}

/// The person's badges for an entry's part: `approved`, then `NRND` or `obsolete`. Empty for a
/// block and for a part that states neither. O(1).
fn entry_status_badges(entry: &CatalogEntry) -> String {
    let Some(part) = &entry.part else { return String::new() };
    let mut out = Vec::new();
    if part.approved {
        out.push("approved");
    }
    match part.lifecycle {
        Some(Lifecycle::Nrnd) => out.push("NRND"),
        Some(Lifecycle::Obsolete) => out.push("obsolete"),
        _ => {}
    }
    out.join(" · ")
}

#[derive(Default)]
struct RowText {
    badges: String,
    meta: String,
    detail: String,
}

const IMPORT_NOTE: &str = "This part is in the local import library but not in this project, so it cannot be placed yet. Choose parts can still pick it.";

fn row_text(entry: &CatalogEntry, row: &ShortlistEntry, placeable: bool) -> RowText {
    let badges = entry_status_badges(entry);
    let base = entry_detail(entry);
    let tier = row.tier.ui_label();
    let (meta, full) = if entry.kind == CatalogKind::Block {
        let label = format!("{tier} · {base}");
        (label.clone(), label)
    } else {
        let (price, dated) = match &row.price {
            PriceCoverage::Unpriced => ("unpriced".into(), "unpriced".into()),
            PriceCoverage::Priced { unit_micros, qty_break, currency, source, date } => {
                let amount = fab_elec_bom::Figure { micros: i128::from(*unit_micros), currency: currency.clone(),
                    source: source.clone(), date: date.clone() }.display();
                let short = format!("{amount} @{qty_break}");
                (short.clone(), format!("{short} · {source} · {date}"))
            }
        };
        let alts = row.alternates.len();
        let mut full = format!("{tier} · {dated} · {base}");
        if let Some(fp) = &row.footprint { full.push_str(&format!(" · {fp}")); }
        full.push_str(&format!(" · +{alts} alt"));
        (format!("{tier} · {price} · {base} · +{alts} alt"), full)
    };
    let mut lines = Vec::new();
    if !badges.is_empty() { lines.push(badges.as_str()); }
    lines.push(&full);
    if !entry.summary.is_empty() { lines.push(&entry.summary); }
    if !placeable { lines.push(IMPORT_NOTE); }
    RowText { detail: lines.join("\n"), badges, meta }
}

/// A cache-only entry has no placement template in the project's circuit VM.
fn can_place(index: usize, base_count: usize) -> bool { index < base_count }

/// The first four-digit package token, borrowed from the footprint name.
fn package_code(footprint: &str) -> Option<&str> {
    footprint.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|s| s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()))
}

/// A catalog default's display text without surrounding quotes or a symbol prefix.
fn default_text(text: &str) -> &str {
    text.trim().trim_matches('"').trim_start_matches('@')
}

/// A unit literal ("10kohm", "100nF") as the sheet writes values ("10k", "100nF"), else None.
fn quantity_text(literal: &str) -> Option<String> {
    let split = literal.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, suffix) = literal.split_at(split);
    let (unit, mul, div) = fab_elec_lang::units::qty_of(suffix)?;
    let n: f64 = number.parse().ok()?;
    Some(fab_elec_lang::units::value_text(&fab_elec_circuit::Quantity { si: n * mul / div, unit }))
}

/// Moves an armed or dragged part to the front of the eight recent shortcuts.
fn touch_recent(recent: &mut Vec<EntryKey>, key: EntryKey) -> bool {
    if recent.first() == Some(&key) {
        return false;
    }
    recent.retain(|old| old != &key);
    recent.insert(0, key);
    recent.truncate(RECENT_COUNT);
    true
}

/// The schematic's unit, once its sheet rows are available.
fn shown_circuit<'a>(live: &'a ElecLive, previous: Option<&Circuit>) -> Option<&'a Arc<Circuit>> {
    let snap = &live.snap;
    let unit = &snap.live().units[live.sheet_unit(previous.map(|c| (c.unit, c.name.as_str())))?];
    snap.of(&snap.sheets, unit.id)?;
    Some(&unit.circuit)
}

/// A searchable parts tree with a virtualized row for each offer.
#[derive(Script, ScriptHook, Widget)]
pub struct LibraryPanel {
    #[deref]
    view: View,
    #[live]
    drag_tip: ScriptObjectRef,
    #[rust]
    tip: Option<WidgetRef>,
    #[rust]
    catalog: Option<Arc<Catalog>>,
    #[rust]
    host_catalog: bool,
    #[rust]
    base_catalog: Option<Arc<Catalog>>,
    #[rust]
    imported: Arc<Vec<CatalogEntry>>,
    #[rust]
    imported_ids: BTreeSet<String>,
    #[rust]
    price_book: Option<Arc<PriceBook>>,
    #[rust]
    design: Option<Arc<Design>>,
    #[rust]
    used_parts: BTreeSet<String>,
    #[rust]
    draft: Option<ReqDraft>,
    #[rust]
    circuit: Option<Arc<Circuit>>,
    #[rust]
    used: HashSet<String>,
    #[rust]
    query: String,
    #[rust]
    pinned: Vec<EntryKey>,
    #[rust]
    recent: Vec<EntryKey>,
    #[rust]
    folded: HashSet<Section>,
    #[rust(vec![Row::Empty])]
    rows: Vec<Row>,
    #[rust]
    section_counts: [String; Section::ALL.len()],
    #[rust]
    row_details: Vec<RowText>,
    #[rust]
    selected: Option<usize>,
    #[rust]
    selected_in: Option<Section>,
    #[rust]
    detail: String,
    #[rust]
    pins: String,
    /// The current targets and star hit areas of drawn rows.
    #[rust]
    visible_rows: HashMap<WidgetUid, (Row, Area)>,
    #[rust]
    press: Option<(Row, bool, Vec2d)>,
    /// The entry under the pointer, and the one the preview last showed.
    #[rust]
    hovered: Option<usize>,
    #[rust]
    previewed: Option<Option<usize>>,
}

impl LibraryPanel {
    /// The preview shows the hovered entry: its symbol, title and summary (or detail), else the
    /// hint. Written only when the hovered entry changes.
    fn update_preview(&mut self, cx: &mut Cx) {
        if self.previewed == Some(self.hovered) {
            return;
        }
        self.previewed = Some(self.hovered);
        let entry = self.hovered.and_then(|index| self.catalog.as_ref()?.entries.get(index));
        if let Some(mut icon) = self.view.widget(cx, ids!(preview.well.big)).borrow_mut::<SymbolIcon>() {
            match entry.and_then(|entry| Some((entry.part.as_ref()?, entry.hash))) {
                Some((part, hash)) => icon.set_part(part, hash),
                None => icon.set_mark(Mark::Section(Section::Blocks), false),
            }
        }
        let text = match (entry, self.hovered) {
            (Some(entry), Some(index)) => {
                let about = &self.row_details[index].detail;
                format!("{}\n{about}", entry.title)
            }
            _ => "Hover a part to see its symbol. Drag it onto the sheet, or click it and then click the sheet.".to_string(),
        };
        self.view.label(cx, ids!(preview.hint)).set_text(cx, &text);
    }

    /// Feeds the catalog from a host and stops following `ElecLive`, including for `None`.
    pub fn set_catalog(&mut self, cx: &mut Cx, catalog: Option<Arc<Catalog>>) {
        let imported = if self.host_catalog { self.imported.clone() } else { Arc::default() };
        self.host_catalog = true;
        self.update_source(cx, catalog, imported, None, None, None);
    }

    /// Restores both shortcut lists and rebuilds the rows once, keeping keys for absent libraries;
    /// lists it already holds change nothing (O(lists) to compare).
    pub fn set_remembered(&mut self, cx: &mut Cx, pinned: Vec<EntryKey>, recent: Vec<EntryKey>) {
        if self.pinned == pinned && self.recent == recent {
            return;
        }
        self.pinned = pinned;
        self.recent = recent;
        self.rebuild(cx);
    }

    fn shortcuts_changed(&mut self, cx: &mut Cx) {
        cx.action(LibraryAction::Remember { pinned: self.pinned.clone(), recent: self.recent.clone() });
        self.rebuild(cx);
    }

    /// The selected definition, if it is still present in the catalog.
    pub fn selected(&self) -> Option<EntryKey> {
        self.catalog.as_ref()?.entries.get(self.selected?).map(entry_key)
    }

    fn sync_choose(&mut self, cx: &mut Cx) {
        let state = cx.get_global_ref::<ReqDraft>();
        if state.is_some() && self.draft.as_ref() == state { return }
        let draft = state.cloned().unwrap_or_default();
        if self.draft.as_ref() == Some(&draft) { return }
        let working = draft.view.as_ref().is_some_and(|v| !v.done);
        let note = if draft.slot == Some("parts") { draft.view.as_ref().map(|v| v.text.as_str()) }
            else if working { Some("Another stage is being drafted") } else { None };
        self.view.button(cx, ids!(lib_choose_parts_tip.lib_choose_parts)).set_enabled(cx, !working && draft.admit.is_ok());
        self.view.tip(cx, ids!(lib_choose_parts_tip)).set_text(note.or(draft.admit.err()).unwrap_or("Choose parts"));
        self.draft = Some(draft);
    }

    fn follow_live(&mut self, cx: &mut Cx) {
        self.sync_choose(cx);
        if self.host_catalog { return }
        if let Some(live) = cx.get_global_ref::<ElecLive>() {
            let catalog = live.snap.catalog.clone();
            let imported = live.imported_catalog.clone();
            let price_book = live.price_book.clone();
            let design = Some(live.snap.design.clone());
            let circuit = shown_circuit(live, self.circuit.as_deref()).cloned();
            self.update_source(cx, catalog, imported, price_book, design, circuit);
        }
    }

    fn update_source(&mut self, cx: &mut Cx, catalog: Option<Arc<Catalog>>, imported: Arc<Vec<CatalogEntry>>,
        price_book: Option<Arc<PriceBook>>, design: Option<Arc<Design>>, circuit: Option<Arc<Circuit>>) {
        let catalog_changed = self.base_catalog.as_ref().map(Arc::as_ptr) != catalog.as_ref().map(Arc::as_ptr)
            || !Arc::ptr_eq(&self.imported, &imported);
        let prices_changed = self.price_book.as_ref().map(Arc::as_ptr) != price_book.as_ref().map(Arc::as_ptr);
        self.price_book = price_book;
        let mut used_changed = false;
        if self.circuit.as_ref().map(Arc::as_ptr) != circuit.as_ref().map(Arc::as_ptr) {
            // e.define registers parts by unique name. Component and catalog VMs can construct
            // separate PartDef Arcs for that same name.
            let mut used = HashSet::new();
            if let Some(circuit) = &circuit {
                for comp in &circuit.comps {
                    if !used.contains(comp.part.name.as_str()) {
                        used.insert(comp.part.name.clone());
                    }
                }
            }
            self.circuit = circuit;
            used_changed = self.used != used;
            self.used = used;
        }
        if self.design.as_ref().map(Arc::as_ptr) != design.as_ref().map(Arc::as_ptr) {
            let used = design.iter().flat_map(|d| &d.units).flat_map(|u| &u.circuit.comps).map(|c| c.part.name.clone()).collect();
            used_changed |= self.used_parts != used;
            self.used_parts = used;
            self.design = design;
        }
        if catalog_changed {
            let selected = self.selected();
            self.base_catalog = catalog;
            self.imported = imported;
            self.imported_ids = self.imported.iter().map(|e| e.name.clone()).collect();
            self.catalog = if self.imported.is_empty() { self.base_catalog.clone() } else {
                let mut entries = self.base_catalog.as_ref().map(|c| c.entries.clone()).unwrap_or_default();
                let present: HashSet<_> = entries.iter().filter(|e| e.lib.is_empty() || e.lib == "imported").map(|e| e.name.clone()).collect();
                entries.extend(self.imported.iter().filter(|e| !present.contains(&e.name)).cloned());
                Some(Arc::new(Catalog { entries }))
            };
            self.hovered = None;
            self.selected = selected.and_then(|key| self.catalog.as_ref()?.entries.iter().position(|e| e.lib == key.lib && e.name == key.name));
        }
        if catalog_changed || used_changed || prices_changed { self.rebuild(cx); }
    }

    fn placeable(&self, index: usize) -> bool {
        can_place(index, self.base_catalog.as_ref().map_or(0, |c| c.entries.len()))
    }

    fn rebuild(&mut self, cx: &mut Cx) {
        let entries = self.catalog.as_ref().map_or(&[][..], |catalog| catalog.entries.as_slice());
        let pinned = self.pinned.iter().map(|k| (k.lib.clone(), k.name.clone())).collect::<Vec<_>>();
        let ctx = ShortlistContext { pinned: &pinned, used_parts: &self.used_parts, imported_ids: &self.imported_ids };
        let empty = Prices::default();
        let prices = self.price_book.as_ref().map_or(&empty, |p| &p.prices);
        let matches = rank::rank_with_context(entries, &self.query, &ctx, prices);
        (self.rows, self.section_counts) = rows_from_matches(entries, &matches, &self.query, &self.pinned, &self.recent, &self.used, &self.folded);
        self.row_details.clear();
        self.row_details.resize_with(entries.len(), RowText::default);
        for row in matches {
            self.row_details[row.index] = row_text(&entries[row.index], &row, self.placeable(row.index));
        }
        self.previewed = None;
        self.update_detail();
        self.place_detail();
        self.press = None;
        self.visible_rows.clear();
        self.view.redraw(cx);
    }

    /// Inserts one detail below the clicked occurrence, or the first visible occurrence when
    /// its section went away. Called on a row rebuild or a change of selection.
    fn place_detail(&mut self) {
        let Some(selected) = self.selected else { return };
        let mut first = None;
        for (pos, row) in self.rows.iter().enumerate() {
            if let Row::Entry { index, section, .. } = row {
                if *index == selected {
                    first.get_or_insert(pos);
                    if *section == self.selected_in {
                        first = Some(pos);
                        break;
                    }
                }
            }
        }
        if let Some(pos) = first {
            self.rows.insert(pos + 1, Row::Detail);
        }
    }

    fn update_detail(&mut self) {
        self.detail.clear();
        self.pins.clear();
        let Some(index) = self.selected else { return };
        if let Some(entry) = self.catalog.as_ref().and_then(|catalog| catalog.entries.get(index)) {
            self.detail.clone_from(&self.row_details[index].detail);
            // Named, so a two-pin part does not read as a stray "1, 2".
            let names = entry.pins.iter().map(|pin| pin.name.as_str()).collect::<Vec<_>>().join(", ");
            self.pins = if names.is_empty() { names } else { format!("pins {names}") };
        }
    }

    fn click(&mut self, cx: &mut Cx, row: Row, star: bool) {
        match row {
            Row::Header(section) => {
                if !self.folded.remove(&section) {
                    self.folded.insert(section);
                }
                self.rebuild(cx);
            }
            Row::Entry { index, section, pinned } => {
                let key = entry_key(&self.catalog.as_ref().unwrap().entries[index]);
                if star {
                    if pinned {
                        self.pinned.retain(|old| old != &key);
                    } else {
                        self.pinned.push(key);
                    }
                    self.shortcuts_changed(cx);
                    return;
                }
                if self.placeable(index) { cx.action(LibraryAction::Arm(key.clone())); }
                let selection_changed = self.selected != Some(index) || self.selected_in != section;
                self.selected = Some(index);
                self.selected_in = section;
                if selection_changed {
                    self.update_detail();
                }
                if touch_recent(&mut self.recent, key) {
                    self.shortcuts_changed(cx);
                } else if selection_changed {
                    self.rows.retain(|row| *row != Row::Detail);
                    self.place_detail();
                    self.visible_rows.clear();
                    self.view.redraw(cx);
                }
            }
            _ => {}
        }
    }

    fn begin_drag(&mut self, cx: &mut Cx, index: usize) {
        if !self.placeable(index) { return }
        let Some(entry) = self.catalog.as_ref().and_then(|catalog| catalog.entries.get(index)) else {
            return;
        };
        let key = entry_key(entry);
        let item = library_drag_item(&key);
        let tip = self.tip.get_or_insert_with(|| {
            cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.drag_tip.as_object().into()))
        }).clone();
        tip.label(cx, ids!(label)).set_text(cx, &entry.title);
        cx.start_dragging_with(vec![item], tip, dvec2(10.0, 10.0));
        cx.set_cursor(MouseCursor::Grabbing);
        if touch_recent(&mut self.recent, key) {
            self.shortcuts_changed(cx);
        }
    }
}

impl Widget for LibraryPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) || matches!(event, Event::Actions(actions)
            if actions.iter().any(|a| a.downcast_ref::<crate::live::transport::BenchChosen>().is_some())) {
            self.follow_live(cx);
        }
        if matches!(event, Event::DragEnd | Event::FingerCancel(_)) {
            self.press = None;
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if self.view.button(cx, ids!(lib_choose_parts_tip.lib_choose_parts)).clicked(&actions) {
            cx.action(LibraryAction::ChooseParts);
        }
        if let Some(query) = self.view.text_input(cx, ids!(search)).changed(&actions) {
            if self.query != query {
                self.query = query;
                self.hovered = None;
                self.view.portal_list(cx, ids!(entries)).set_first_id_and_scroll(0, 0.0);
                self.rebuild(cx);
            }
        }
        for action in &actions {
            let Some(action) = action.as_widget_action() else { continue };
            let Some(&(row, star)) = self.visible_rows.get(&action.widget_uid) else { continue };
            match action.cast::<RowAction>() {
                RowAction::FingerDown(down) if down.device.is_primary_hit() => {
                    self.press = Some((row, star.is_valid(cx) && star.rect(cx).contains(down.abs), down.abs));
                }
                RowAction::FingerMove(moved) => {
                    if let (Row::Entry { index, .. }, Some((pressed, false, abs))) = (row, self.press) {
                        if row == pressed && (moved.abs - abs).length() > 4.0 {
                            self.press = None;
                            self.begin_drag(cx, index);
                        }
                    }
                }
                RowAction::FingerUp(up) => {
                    if let Some((pressed, on_star, _)) = self.press.take() {
                        let over_star = star.is_valid(cx) && star.rect(cx).contains(up.abs);
                        if row == pressed && up.is_over && up.was_tap() && on_star == over_star {
                            self.click(cx, row, on_star);
                        }
                    }
                }
                RowAction::FingerHoverIn(_) => {
                    if let Row::Entry { index, .. } = row { self.hovered = Some(index); }
                    self.view.redraw(cx);
                }
                RowAction::FingerHoverOut(_) => {
                    if matches!(row, Row::Entry { index, .. } if self.hovered == Some(index)) { self.hovered = None; }
                    self.view.redraw(cx);
                }
                _ => {}
            }
        }
        cx.extend_actions(actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // a panel made, or shown again, after the snapshot last changed catches up
        self.follow_live(cx);
        self.update_preview(cx);
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, self.rows.len());
            self.visible_rows.clear();
            while let Some(row_id) = list.next_visible_item(cx) {
                let Some(&row) = self.rows.get(row_id) else { continue };
                match row {
                    Row::Header(section) => {
                        let header = list.item(cx, row_id, id!(Header));
                        let chevron = if self.folded.contains(&section) { "▸" } else { "▾" };
                        header.label(cx, ids!(tri)).set_text(cx, chevron);
                        if let Some(mut icon) = header.symbol_icon(cx, ids!(mark)).borrow_mut() {
                            icon.set_mark(Mark::Section(section), true);
                        }
                        header.label(cx, ids!(title)).set_text(cx, section.title());
                        header.label(cx, ids!(count)).set_text(cx, &self.section_counts[section as usize]);
                        self.visible_rows.insert(header.widget_uid(), (row, Area::Empty));
                        header.draw_all(cx, scope);
                    }
                    Row::Entry { index, pinned, .. } => {
                        let entry = &self.catalog.as_ref().unwrap().entries[index];
                        let item = list.item(cx, row_id, id!(Row));
                        if let Some(mut icon) = item.symbol_icon(cx, ids!(labels.first.symbol)).borrow_mut() {
                            if let Some(part) = &entry.part {
                                icon.set_part(part, entry.hash);
                            } else {
                                icon.set_mark(Mark::Section(Section::Blocks), entry.kind == CatalogKind::Block);
                            }
                        }
                        if let Some(mut view) = item.borrow_mut::<View>() {
                            view.cursor = Some(if self.placeable(index) { MouseCursor::Hand } else { MouseCursor::Default });
                        }
                        item.label(cx, ids!(labels.first.status)).set_text(cx, &self.row_details[index].badges);
                        item.label(cx, ids!(labels.first.title)).set_text(cx, &entry.title);
                        item.label(cx, ids!(labels.meta)).set_text(cx, &self.row_details[index].meta);
                        let star = item.widget(cx, ids!(labels.first.star));
                        if let Some(mut icon) = star.symbol_icon(cx, ids!(outline)).borrow_mut() {
                            icon.set_mark(Mark::Star { pinned: false }, !pinned && cx.fingers.is_area_hovered(item.area()));
                        }
                        if let Some(mut icon) = star.symbol_icon(cx, ids!(filled)).borrow_mut() {
                            icon.set_mark(Mark::Star { pinned: true }, pinned);
                        }
                        item.draw_all(cx, scope);
                        self.visible_rows.insert(item.widget_uid(), (row, star.area()));
                    }
                    Row::Detail => {
                        let detail = list.item(cx, row_id, id!(Detail));
                        // An empty line takes no room: a part without a summary shows its pins only.
                        let summary = detail.label(cx, ids!(summary));
                        summary.set_visible(cx, !self.detail.is_empty());
                        summary.set_text(cx, &self.detail);
                        detail.label(cx, ids!(pins)).set_text(cx, &self.pins);
                        detail.draw_all(cx, scope);
                    }
                    Row::Empty => list.item(cx, row_id, id!(Empty)).draw_all(cx, scope),
                }
            }
        }
        DrawStep::done()
    }
}
