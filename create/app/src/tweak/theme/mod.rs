//! The shared app's resolved theme and the colour names offered in its property editors.
pub mod store;
pub(crate) mod builder;
pub(in crate::tweak) mod combinations;
pub(crate) mod lab;
pub(in crate::tweak) mod view;

use builder::{BuildLock, BuildOutcome, BuildRow, BuilderParams, SeedSlot, Suggestion, TbDrop, ThemeBuilderState};
use super::{RequestId, ThemeInstall, ThemeVal};
use super::props::{AppliedEdit, RowBinding, RowKind, SectionKind, StructKind, view::PropLine};
use std::{path::PathBuf, sync::Arc};
use makepad_widgets::theme_tokens::{Appearance, BlendCache, BlendError, BlendTheme, ThemeResolver, WeightMode};
use makepad_strict_json::Value;
use makepad_widgets::{makepad_script::colorhex::hex_bytes_to_u32, BaseTheme};
use std::{collections::HashMap, rc::Rc};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SheetInfo {
    pub id: String,
    pub label: String,
    pub base: String,
    pub dark: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ThemeToken {
    pub name: String,
    pub value: String,
    pub color: Option<u32>,
    pub site: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ThemeSnapshot {
    pub base: String,
    pub sheet: Option<String>,
    pub has_mix: bool,
    pub has_pins: bool,
    pub edits: Vec<(String, String)>,
    pub sheets: Vec<SheetInfo>,
    pub tokens: Vec<ThemeToken>,
    token_index: HashMap<String, usize>,
    edit_index: HashMap<String, usize>,
    row_order: Vec<usize>,
}

#[derive(Clone, Default)]
pub(in crate::tweak) struct ThemePalette {
    by_rgba: HashMap<u32, Rc<str>>,
    entries: Vec<(String, [f32; 4])>,
}

fn hex(text: &str) -> Option<u32> {
    let digits = text.strip_prefix('#')?;
    if !matches!(digits.len(), 3 | 6 | 8) { return None }
    hex_bytes_to_u32(digits.as_bytes()).ok()
}

pub(super) fn scalar(text: &str) -> Option<ThemeVal> {
    hex(text).map(ThemeVal::Color).or_else(|| text.parse::<f64>().ok().filter(|n| n.is_finite()).map(ThemeVal::Num))
}

pub(super) fn literal(val: ThemeVal) -> String {
    match val { ThemeVal::Color(c) => format!("#x{c:08X}"), ThemeVal::Num(n) => format!("{n:?}") }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeSection { Color, Spacing, Font }

impl ThemeSnapshot {
    pub(crate) fn ordered_tokens(&self) -> impl Iterator<Item = &ThemeToken> {
        self.row_order.iter().map(|&i| &self.tokens[i])
    }

    pub(crate) fn token_section(token: &ThemeToken) -> ThemeSection {
        if token.color.is_some() { ThemeSection::Color }
        else if token.name.starts_with("font") { ThemeSection::Font }
        else { ThemeSection::Spacing }
    }

    pub fn parse(json: &str) -> Option<Self> { Self::from_value(&super::parse_json(json).ok()?) }

    pub(super) fn from_value(v: &Value) -> Option<Self> {
        let base = BaseTheme::parse(v.get("base")?.as_str()?)?;
        let sheet = match v.get("sheet")? {
            Value::Null => None,
            id => Some(id.as_str()?.to_owned()),
        };
        let mut snap = Self { base: base.id().into(), sheet,
            has_mix: v.get("has_mix")?.as_bool()?, has_pins: v.get("has_pins")?.as_bool()?, ..Self::default() };
        for edit in v.get("edits")?.as_arr()? {
            let [key, literal] = edit.as_arr()? else { return None };
            let name = key.as_str()?;
            snap.edit_index.insert(name.to_owned(), snap.edits.len());
            snap.edits.push((name.to_owned(), literal.as_str()?.into()));
        }
        for sheet in v.get("sheets")?.as_arr()? {
            let base = BaseTheme::parse(sheet.get("base")?.as_str()?)?;
            let dark = match sheet.get("appearance")?.as_str()? {
                "dark" => true, "light" => false, _ => return None,
            };
            snap.sheets.push(SheetInfo { id: sheet.get("id")?.as_str()?.into(), label: sheet.get("label")?.as_str().filter(|s| !s.is_empty())?.into(),
                base: base.id().into(), dark });
        }
        for token in v.get("values")?.as_arr()? {
            let name = token.get("name")?.as_str()?;
            if !makepad_town_core::literal::is_identifier(name) || snap.token_index.insert(name.into(), snap.tokens.len()).is_some() {
                return None
            }
            let value = token.get("value")?.as_str()?;
            let color = match scalar(value)? { ThemeVal::Color(c) => Some(c), ThemeVal::Num(_) => None };
            snap.tokens.push(ThemeToken { name: name.into(), value: value.into(), color, site: token.get("site")?.as_str()?.into() });
        }
        snap.row_order = (0..snap.tokens.len()).collect();
        snap.row_order.sort_unstable_by(|&a, &b| snap.tokens[a].name.cmp(&snap.tokens[b].name));
        Some(snap)
    }

    pub fn label(&self) -> String {
        self.sheet.as_ref().and_then(|id| self.sheets.iter().find(|s| &s.id == id)).map(|s| s.label.clone())
            .unwrap_or_else(|| BaseTheme::parse(&self.base).map_or("", BaseTheme::label).into())
    }

    pub fn patch_token(&mut self, name: &str, new_value: &str) -> bool {
        let Some(&index) = self.token_index.get(name) else { return false };
        let token = &mut self.tokens[index];
        if token.value == new_value { return false }
        let literal = literal(match scalar(new_value) {
            Some(ThemeVal::Color(c)) if token.color.is_some() => {
                token.color = Some(c);
                ThemeVal::Color(c)
            }
            Some(val @ ThemeVal::Num(_)) if token.color.is_none() => val,
            _ => return false,
        });
        token.value = new_value.into();
        let index = self.edit_index.entry(name.into()).or_insert(self.edits.len());
        if *index == self.edits.len() { self.edits.push((name.into(), literal)); }
        else { self.edits[*index].1 = literal; }
        true
    }

    pub(in crate::tweak) fn palette(&self) -> ThemePalette {
        let mut palette = ThemePalette::default();
        for token in &self.tokens {
            if let Some(c) = token.color.filter(|_| token.name.starts_with("color")) {
                if c & 0xff != 0 { palette.by_rgba.entry(c).or_insert_with(|| Rc::from(token.name.as_str())); }
                palette.entries.push((token.name.clone(), c.to_be_bytes().map(|n| f32::from(n) / 255.0)));
            }
        }
        palette
    }
}

impl ThemePalette {
    pub fn match_hex(&self, text: &str) -> Option<Rc<str>> {
        if self.is_empty() { return None }
        self.by_rgba.get(&hex(text.trim())?).cloned()
    }
    pub fn entries(&self) -> Vec<(String, [f32; 4])> { self.entries.clone() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_indexes_resolved_colours_and_keeps_the_first_matching_token() {
        let json = r##"{"base":"light","sheet":"paper","has_mix":false,"has_pins":true,"edits":[["size","2.0"]],
            "sheets":[{"id":"paper","label":"Paper","base":"light","appearance":"light"}],"values":[
            {"name":"color_one","value":"#abc","site":"sheet:1"},{"name":"color_two","value":"#aabbccff","site":""},
            {"name":"color_clear","value":"#00000000","site":""},{"name":"shadow","value":"#123456ff","site":""},{"name":"size","value":"2","site":"base:3"}]}"##;
        let mut snap = ThemeSnapshot::parse(json).expect("snapshot accepts the app's catalogue and resolved values");
        assert!(snap.label() == "Paper" && !snap.sheets[0].dark && snap.has_pins && !snap.has_mix, "sheet metadata is preserved");
        let names = snap.palette();
        assert_eq!(names.match_hex("#ABC").as_deref(), Some("color_one"), "the first matching token wins across hex spellings");
        assert!(names.entries().len() == 3 && names.match_hex("#123456").is_none(), "the strip lists only named theme colours");
        assert!(names.match_hex("#00000000").is_none(), "fully transparent colours do not auto-match a theme chip");
        assert!(names.entries().iter().any(|(n, c)| n == "color_clear" && c[3] == 0.0), "the palette strip keeps transparent tokens");
        assert!(!snap.patch_token("size", "#abc") && !snap.patch_token("size", "NaN"), "invalid deltas cannot corrupt numeric tokens");
        assert!(snap.patch_token("color_one", "#00000080") && snap.patch_token("size", "3"), "edits update indexed tokens");
        assert_eq!(snap.palette().match_hex("#aabbcc").as_deref(), Some("color_two"), "matching follows a changed token");
        assert_eq!(snap.palette().match_hex("#00000080").as_deref(), Some("color_one"), "non-zero translucent colours match a chip");
        assert_eq!(snap.edits, [("size".into(), "3.0".into()), ("color_one".into(), "#x00000080".into())], "the snapshot retains typed edits");
        assert_eq!(resolve_preset(&snap, "dark").unwrap().install.base, "dark", "base presets resolve without a sheet");
        assert!(resolve_preset(&snap, "unavailable").is_err(), "a preset absent from the shared catalogue is refused");
        assert!(ThemeSnapshot::parse(&json.replace("aabbccff", "gg")).is_none(), "malformed snapshots fail as a whole");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum ThemePreset {
    Base(BaseTheme),
    Sheet { id: String, label: String, base: String },
}

impl ThemePreset {
    fn install(&self) -> ThemeInstall {
        let (label, base, sheet) = match self {
            Self::Base(b) => (b.label(), b.id(), None),
            Self::Sheet { id, label, base } => (label.as_str(), base.as_str(), Some(id.clone())),
        };
        ThemeInstall { label: label.into(), base: base.into(), sheet, globals: vec![], overrides: vec![] }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum ThemeChoice { Builtin(ThemePreset), Saved(String) }

impl ThemeChoice {
    pub fn label(&self) -> &str {
        match self {
            Self::Builtin(ThemePreset::Base(b)) => b.label(),
            Self::Builtin(ThemePreset::Sheet { label, .. }) | Self::Saved(label) => label,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum ThemeConfirm { Replace(String), Delete(String), DropStanding(String) }

pub(in crate::tweak) enum ThemeEntry {
    Preset { label: String, base: String, sheet: Option<String> },
    Saved(store::SavedTheme),
    Snapshot(ThemeInstall),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallSource { Picker, Mix, Build, Restore }

pub(crate) struct InstallMeta {
    pub saved: Option<store::SavedTheme>,
    pub source: InstallSource,
}

pub(crate) struct PendingInstall {
    pub install: ThemeInstall,
    pub meta: InstallMeta,
}

pub(crate) fn snapshot_for_save(snap: &ThemeSnapshot, name: &str) -> Result<store::SavedTheme, store::StoreError> {
    store::snapshot_from(snap, name)
}

impl PendingInstall {
    fn picker(install: ThemeInstall, saved: Option<store::SavedTheme>) -> Self {
        Self { install, meta: InstallMeta { saved, source: InstallSource::Picker } }
    }

    pub(crate) fn saved(saved: store::SavedTheme, sheets: &[SheetInfo]) -> Self {
        Self::picker(saved.to_install_for(sheets), Some(saved))
    }
}

pub(crate) fn resolve_preset(snap: &ThemeSnapshot, preset: &str) -> Result<PendingInstall, String> {
    let choice = if let Some(base) = BaseTheme::parse(preset) { ThemePreset::Base(base) }
    else if let Some(s) = snap.sheets.iter().find(|s| s.id == preset) {
        ThemePreset::Sheet { id: s.id.clone(), label: s.label.clone(), base: s.base.clone() }
    } else { return Err(format!("unknown theme preset {preset}")) };
    Ok(PendingInstall::picker(choice.install(), None))
}

pub(crate) fn resolve_saved(dir: &std::path::Path, snap: &ThemeSnapshot, name: &str) -> Result<PendingInstall, store::StoreError> {
    Ok(PendingInstall::saved(store::load_in(dir, name)?, &snap.sheets))
}

impl ThemeEntry {
    fn restore(&self, sheets: &[SheetInfo]) -> PendingInstall {
        let (install, saved) = match self {
            Self::Preset { label, base, sheet } => (ThemeInstall { label: label.clone(), base: base.clone(),
                sheet: sheet.clone(), globals: vec![], overrides: vec![] }, None),
            Self::Saved(saved) => (saved.to_install_for(sheets), Some(saved.clone())),
            Self::Snapshot(install) => (install.clone(), None),
        };
        PendingInstall { install, meta: InstallMeta { saved, source: InstallSource::Restore } }
    }
}

#[derive(Default)]
pub(crate) struct ThemeMixerCache {
    resolver: Option<ThemeResolver>,
    pub ready: Option<Arc<BlendCache>>,
}

impl ThemeMixerCache {
    #[cfg(test)]
    pub(crate) fn is_preparing(&self) -> bool { self.resolver.is_some() }

    pub(crate) fn prepare(&mut self) {
        if self.ready.is_none() && self.resolver.is_none() {
            self.resolver = Some(ThemeResolver::new(BlendTheme::all()));
        }
    }

    pub fn step(&mut self) {
        if let Some(resolver) = self.resolver.as_mut() {
            resolver.step();
            if resolver.is_done() {
                self.ready = self.resolver.take().map(|r| Arc::new(r.into_cache()));
            }
        }
    }
}

pub(in crate::tweak) enum MixAction {
    Fold, Appearance(Appearance), Mode(WeightMode), Surprise, Weight { index: usize, value: f64, ended: bool }, Reset(usize),
}

pub(in crate::tweak) enum BuildAction {
    Fold, Roll, Count(usize), Adjust(bool), Chip(usize), Text(usize), Lock(usize),
    Touch(usize), Color { slot: usize, rgba: u32, ended: bool }, Drop(usize, TbDrop),
    Slider { row: BuildRow, value: f64, ended: bool }, Reset(BuildRow),
}

pub(in crate::tweak) enum ThemeAction {
    Bake,
    Build(BuildAction),
    Mix(MixAction), PickChoice(usize), SaveAs(String), DeleteSaved, ToggleSection(u8), Filter(String), Name(String),
}

pub(in crate::tweak) struct ThemeTab {
    pub snapshot: Option<ThemeSnapshot>,
    pub palette: ThemePalette,
    pub themes_dir: PathBuf,
    pub saved_names: Vec<String>,
    pub worn: Option<store::SavedTheme>,
    pub last_preset_idx: usize,
    pub entries: Vec<ThemeChoice>,
    pub confirm: Option<ThemeConfirm>,
    pub msg: String,
    pub name_text: String,
    pub collapsed: [bool; 3],
    pub filter: String,
    pub refusals: HashMap<String, String>,
    pub changed_originals: HashMap<String, (bool, String)>,
    pub inflight_get: Option<RequestId>,
    pub inflight_install: Option<RequestId>,
    pub waiting_install: Option<PendingInstall>,
    pub last_own_install: Option<RequestId>,
    pub background_requested: bool,
    pub status: String,
    pub installing: Option<InstallMeta>,
    pub eq_open: bool,
    pub eq_lab: Option<lab::ThemeMix>,
    pub eq_entry: Option<ThemeEntry>,
    pub eq_stands: bool,
    pub eq_reanchor_pending: bool,
    pub eq_apply_due: bool,
    pub eq_apply_at_once: bool,
    eq_installed_at: f64,
    eq_seed: u64,
    pub eq_reading: String,
    eq_mode: WeightMode,
    pub tb_open: bool,
    tb_state: Option<ThemeBuilderState>,
    tb_entry: Option<ThemeEntry>,
    pub tb_stands: bool,
    pub tb_reanchor_pending: bool,
    pub tb_apply_due: bool,
    tb_apply_at_once: bool,
    pub tb_moving: bool,
    tb_moved_at: f64,
    tb_installed_at: f64,
    tb_seed: u64,
    tb_seed_slot: SeedSlot,
    tb_locks: [bool; 10],
    tb_adjust: bool,
    tb_own_schemes: Vec<Vec<u32>>,
    tb_suggestions: Vec<Suggestion>,
    tb_grown_for: Option<(SeedSlot, u32)>,
    tb_chosen: Option<Suggestion>,
    tb_suggest_due: bool,
    tb_chips_changed: bool,
    tb_strip_rewind: bool,
    tb_text_labels_for: usize,
    tb_reading: String,
    tb_color_opened: Option<usize>,
    tb_color_carried: Option<usize>,
    tb_color_target: Option<usize>,
    tb_color_insert: Option<usize>,
    // Applied precedes the broadcast containing the new token values.
    pub awaiting_snapshot: bool,
    save_after_snapshot: Option<String>,
}

impl Default for ThemeTab {
    fn default() -> Self {
        Self {
            snapshot: None, palette: Default::default(), themes_dir: PathBuf::new(), saved_names: vec![], worn: None,
            last_preset_idx: 0, entries: vec![], confirm: None, msg: String::new(), name_text: String::new(),
            collapsed: [false; 3], filter: String::new(), refusals: HashMap::new(), changed_originals: HashMap::new(),
            inflight_get: None, inflight_install: None, waiting_install: None, last_own_install: None,
            background_requested: false, status: String::new(), installing: None,
            eq_open: false, eq_lab: None, eq_entry: None, eq_stands: false, eq_reanchor_pending: false,
            eq_apply_due: false, eq_apply_at_once: false, eq_installed_at: f64::NEG_INFINITY, eq_seed: 0,
            tb_open: false, tb_state: None, tb_entry: None, tb_stands: false, tb_reanchor_pending: false,
            tb_apply_due: false, tb_apply_at_once: false, tb_moving: false, tb_moved_at: f64::NEG_INFINITY,
            tb_installed_at: f64::NEG_INFINITY, tb_seed: 0, tb_seed_slot: SeedSlot::Primary, tb_locks: [false; 10],
            tb_adjust: true, tb_own_schemes: vec![], tb_suggestions: vec![], tb_chosen: None, tb_grown_for: None,
            tb_suggest_due: false, tb_chips_changed: false, tb_strip_rewind: false, tb_text_labels_for: 0,
            tb_reading: String::new(), tb_color_opened: None, tb_color_carried: None, tb_color_target: None, tb_color_insert: None,
            eq_reading: String::new(), eq_mode: WeightMode::Absolute, awaiting_snapshot: false, save_after_snapshot: None,
        }
    }
}

impl ThemeTab {
    fn capture_entry(&self) -> Option<ThemeEntry> {
        let snap = self.snapshot.as_ref()?;
        let resolved = || ThemeEntry::Snapshot(store::resolved_snapshot(snap, &snap.label()).to_install_for(&snap.sheets));
        Some(if !snap.edits.is_empty() {
            resolved()
        } else if let Some(saved) = &self.worn {
            ThemeEntry::Saved(saved.clone())
        } else if snap.has_mix || snap.has_pins {
            resolved()
        } else {
            ThemeEntry::Preset { label: snap.label(), base: snap.base.clone(), sheet: snap.sheet.clone() }
        })
    }

    fn restore_entry(&self) -> Option<PendingInstall> {
        Some(self.eq_entry.as_ref()?.restore(&self.snapshot.as_ref()?.sheets))
    }

    pub fn inflight_is_mix(&self) -> bool {
        self.installing.as_ref().is_some_and(|meta| meta.source == InstallSource::Mix)
    }

    pub fn inflight_is_build(&self) -> bool {
        self.installing.as_ref().is_some_and(|meta| meta.source == InstallSource::Build)
    }

    pub fn on_external_install(&mut self) {
        self.stand_down();
        // A peer supersedes even a restore queued by a tool we already closed.
        self.waiting_install.take_if(|p| p.meta.source != InstallSource::Picker);
    }

    fn stand_down(&mut self) {
        self.tb_stands = false;
        self.tb_apply_due = false;
        self.tb_apply_at_once = false;
        self.tb_moving = false;
        self.tb_suggest_due = false;
        self.tb_entry = None;
        self.tb_reanchor_pending = self.tb_open;
        self.eq_stands = false;
        self.eq_apply_due = false;
        self.eq_apply_at_once = false;
        self.eq_entry = None;
        self.eq_reanchor_pending = self.eq_open;
        self.save_after_snapshot = None;
    }

    pub fn received_snapshot(&mut self) {
        self.awaiting_snapshot = false;
        // A local picker change can still be queued behind the previous install's refresh.
        if self.eq_open && self.eq_reanchor_pending && self.inflight_install.is_none() {
            self.eq_entry = self.capture_entry();
            self.eq_reanchor_pending = false;
            if let (Some(lab), Some(snap)) = (&mut self.eq_lab, &self.snapshot) {
                lab.mark_stood_down(snap);
                self.eq_reading = lab.readability().to_string();
            }
        }
        if self.inflight_install.is_none() {
            if self.tb_open && self.tb_reanchor_pending {
                self.tb_reanchor_pending = false;
                self.anchor_builder();
            }
            if let Some(text) = self.save_after_snapshot.take() {
                if let Err(e) = self.save_as(&text) { self.msg = e.to_string(); }
            }
        }
    }

    pub fn enter_prepared(&mut self, cache: &ThemeMixerCache) {
        if self.eq_lab.is_some() || !self.eq_open || self.eq_reanchor_pending { return }
        if let (Some(cache), Some(snap)) = (&cache.ready, &self.snapshot) {
            let mut lab = lab::ThemeMix::enter(cache.clone(), snap);
            lab.set_mode(self.eq_mode);
            self.eq_reading = lab.readability().to_string();
            self.eq_lab = Some(lab);
        }
    }

    pub fn mix_action(&mut self, action: MixAction, cache: &mut ThemeMixerCache) -> Option<PendingInstall> {
        if matches!(action, MixAction::Fold) {
            if self.snapshot.is_none() { return None }
            self.confirm = None;
            self.msg.clear();
            if self.eq_open {
                return self.close_mix()
            }
            let restore = if self.tb_open { self.close_build() } else { None };
            self.eq_open = true;
            self.eq_installed_at = f64::NEG_INFINITY;
            self.eq_reanchor_pending = restore.is_some() || self.inflight_install.is_some() || self.awaiting_snapshot;
            self.eq_entry = self.capture_entry();
            self.eq_reading = "Preparing theme mixer…".into();
            cache.prepare();
            self.enter_prepared(cache);
            return restore
        }
        if self.eq_reanchor_pending { return None }
        let lab = self.eq_lab.as_mut()?;
        let immediate = match action {
            MixAction::Appearance(a) => { lab.set_appearance(a); true }
            MixAction::Mode(mode) => { self.eq_mode = mode; lab.set_mode(mode); true }
            MixAction::Surprise => { self.eq_seed = lab::next_mix_seed(self.eq_seed); lab.randomize(self.eq_seed); true }
            MixAction::Weight { index, value, ended } => { lab.set_weight(index, value); ended }
            MixAction::Reset(i) => { lab.clear_weight(i); true }
            MixAction::Fold => unreachable!(),
        };
        self.confirm = None;
        self.eq_apply_due = true;
        self.eq_apply_at_once |= immediate;
        None
    }

    fn close_mix(&mut self) -> Option<PendingInstall> {
        let restore = (self.eq_stands || self.inflight_is_mix()).then(|| self.restore_entry()).flatten();
        self.eq_open = false;
        self.stand_down();
        self.eq_lab = None;
        self.eq_reading.clear();
        restore.and_then(|p| self.offer_install(p))
    }

    pub fn settle_mix(&mut self, now: f64) -> Option<PendingInstall> {
        if !self.eq_apply_due || self.inflight_install.is_some() || self.eq_reanchor_pending
            || !self.eq_apply_at_once && now - self.eq_installed_at < lab::EQ_SETTLE { return None }
        self.eq_apply_due = false;
        self.eq_apply_at_once = false;
        let lab = self.eq_lab.as_mut()?;
        let outcome = match lab.evaluate_apply() {
            Ok(outcome) => outcome,
            Err(BlendError::NoWeight) => {
                lab.reset();
                self.eq_apply_due = true;
                self.eq_apply_at_once = true;
                self.msg = "an empty mix is no mix — back to the theme it started from".into();
                return None
            }
            Err(error) => { self.msg = error.to_string(); return None }
        };
        self.eq_reading = lab.readability().to_string();
        let next = match outcome {
            lab::LabOutcome::Nothing => return None,
            lab::LabOutcome::Mix(install) => PendingInstall { install, meta: InstallMeta { saved: None, source: InstallSource::Mix } },
            lab::LabOutcome::RestoreEntry => self.restore_entry()?,
        };
        self.eq_installed_at = now;
        Some(next)
    }

    fn tb_locked(&self, which: BuildLock) -> bool {
        BuildLock::ALL
            .iter()
            .position(|at| *at == which)
            .is_some_and(|at| self.tb_locks[at])
    }

    /// `now` with every locked row put back where `was` had it.
    ///
    /// One place, for the two gestures that move rows nobody has a hand on:
    /// the die, which rolls a whole theme, and a palette off the carousel,
    /// which moves the two background rows onto its own page. A lock is a
    /// promise about those and about nothing else -- a drag, an arrow key or
    /// a click on the row's name never comes through here, so a locked row
    /// still answers the person who locked it.
    fn tb_locked_kept(&self, was: &BuilderParams, now: BuilderParams) -> BuilderParams {
        let mut now = self.tb_colors_kept(was, now);
        for (index, which) in BuildLock::ALL.iter().enumerate() {
            if !self.tb_locks[index] {
                continue;
            }
            now = match which {
                BuildLock::Row(row) => row.moved(now, row.shown(was)),
                BuildLock::TextColor => BuilderParams { text_color: was.text_color, ..now },
                // Done above, before anything that follows the Surf colour
                // has had a chance to look at it.
                BuildLock::Palette => now,
            };
        }
        now
    }

    /// `now` with the colour squares put back where `was` had them, if their
    /// lock is on: how many there are, and which colours stand on them.
    ///
    /// Its own call because it has to happen FIRST. The two Surface rows
    /// follow the Surf colour whenever that colour changes, and with this
    /// lock on it did not change, so the rows have nothing to follow --
    /// which is only true if they are asked after the palette has been put
    /// back and not before.
    ///
    /// What is put back is the recipe the four colours are read out of --
    /// the favourite, the harmony and the seeds a chip or an edit named --
    /// together with the count and the slot the carousel is grown from. That
    /// is the whole of the palette as the settings store it. Where the
    /// recipe alone is not enough the four are written down outright: an
    /// untouched house palette names no colours at all and reads its fourth
    /// square off the page, so a roll that moved the page would move that
    /// square out from under a lock that promised it would not. Held
    /// colours are colours somebody meant to keep, and keeping them is worth
    /// writing them down for.
    ///
    /// The slots a count below four DERIVES are a step of the primary on the
    /// page in force, and the page still moves under a roll. Those slots
    /// have no square -- the row shows only the colours that were chosen --
    /// so what the lock holds is every colour the person can see and every
    /// colour they could get back by putting the count up again.
    fn tb_colors_kept(&self, was: &BuilderParams, now: BuilderParams) -> BuilderParams {
        if !self.tb_locked(BuildLock::Palette) {
            return now;
        }
        let kept = BuilderParams {
            favourite: was.favourite,
            harmony: was.harmony,
            seeds: was.seeds,
            color_count: was.color_count,
            seed_slot: was.seed_slot,
            ..now
        };
        if kept.chosen_palette() == was.chosen_palette() {
            return kept;
        }
        kept.with_palette(was.chosen_palette())
    }

    /// The two Surface rows moved onto the Surf colour these settings carry,
    /// so that the page the theme builds IS that colour and the person tunes
    /// AWAY from it rather than towards it.
    ///
    /// The colour becomes the two numbers in `theme_builder::surface_sliders`,
    /// which says exactly how and is the inverse of the page the sliders
    /// ask for: the saturation row is a share of the colour's own saturation
    /// and goes to all of it, and the lightness row is the `L*` axis run
    /// backwards from the colour's own lightness, inside the appearance in
    /// force. What the page finally wears may still be a step off, because a
    /// page that cannot be told from an accent still moves clear of it; that
    /// rule is untouched and these rows show what was ASKED for.
    ///
    /// A locked row is left exactly where it is, which is the whole of what
    /// a lock on either of these two rows means.
    fn tb_follow_surface(&self, params: BuilderParams) -> BuilderParams {
        let surface = params.palette()[SeedSlot::Surface.index()];
        let (saturation, lightness) = builder::surface_sliders(surface, params.dark());
        let mut params = params;
        if !self.tb_locked(BuildLock::Row(BuildRow::Saturation)) {
            params = BuilderParams { saturation, ..params };
        }
        if !self.tb_locked(BuildLock::Row(BuildRow::Lightness)) {
            params = BuilderParams { lightness, ..params };
        }
        params
    }

    fn tb_surface_followed(&self, was: &BuilderParams, now: BuilderParams) -> BuilderParams {
        let at = SeedSlot::Surface.index();
        if was.palette()[at] | 0xFF == now.palette()[at] | 0xFF {
            return now;
        }
        self.tb_follow_surface(now)
    }

    pub fn install_failed(&mut self, meta: InstallMeta, why: String) {
        self.msg = why;
        self.save_after_snapshot = None;
        if meta.source != InstallSource::Picker {
            if let Some(lab) = &mut self.eq_lab { lab.install_failed(self.eq_stands); }
            if let Some(state) = &mut self.tb_state { state.install_failed(self.tb_stands); }
        }
        if (self.eq_reanchor_pending || self.tb_reanchor_pending) && !self.awaiting_snapshot { self.received_snapshot(); }
    }

    pub fn themes_dir(&self) -> PathBuf {
        if self.themes_dir.as_os_str().is_empty() { store::themes_dir() } else { self.themes_dir.clone() }
    }

    pub fn refresh_saved(&mut self) {
        match store::list_in(&self.themes_dir()) {
            Ok(names) => self.saved_names = names,
            Err(e) => { self.saved_names.clear(); self.msg = e.to_string(); }
        }
    }

    pub fn choice_list(&self) -> Vec<ThemeChoice> {
        let mut entries: Vec<_> = BaseTheme::ALL.into_iter().map(|b| ThemeChoice::Builtin(ThemePreset::Base(b))).collect();
        if let Some(snap) = &self.snapshot {
            entries.extend(snap.sheets.iter().map(|s| ThemeChoice::Builtin(ThemePreset::Sheet {
                id: s.id.clone(), label: s.label.clone(), base: s.base.clone(),
            })));
        }
        entries.extend(self.saved_names.iter().filter(|name| !self.snapshot.as_ref().is_some_and(|s|
            s.sheets.iter().any(|sh| sh.id.replace('-', "_").eq_ignore_ascii_case(name)))).cloned().map(ThemeChoice::Saved));
        entries
    }

    pub(in crate::tweak) fn board_theme(&self, index: usize) -> Result<ThemeInstall, String> {
        let snapshot = self.snapshot.as_ref();
        if index == self.selected_choice_index() {
            if let Some(snapshot) = snapshot {
                return Ok(store::resolved_snapshot(snapshot, &snapshot.label()).to_install());
            }
        }
        match self.entries.get(index) {
            Some(ThemeChoice::Builtin(preset)) => Ok(preset.install()),
            Some(ThemeChoice::Saved(name)) => resolve_saved(&self.themes_dir(), snapshot.unwrap_or(&ThemeSnapshot::default()), name)
                .map(|pending| pending.install).map_err(|e| e.to_string()),
            None => Err("Choose a theme to bake.".into()),
        }
    }

    pub fn selected_choice_index(&self) -> usize {
        if let Some(worn) = &self.worn {
            if let Some(i) = self.entries.iter().position(|e| matches!(e, ThemeChoice::Saved(n) if n == &worn.name)) { return i }
        }
        self.snapshot.as_ref().and_then(|s| self.entries.iter().position(|e| match e {
            ThemeChoice::Builtin(ThemePreset::Sheet { id, .. }) => s.sheet.as_ref() == Some(id),
            ThemeChoice::Builtin(ThemePreset::Base(b)) => s.sheet.is_none() && s.base == b.id(),
            _ => false,
        })).unwrap_or(self.last_preset_idx)
    }

    pub fn mark_changed<'a>(&mut self, history: impl Iterator<Item = &'a AppliedEdit>) {
        self.changed_originals.clear();
        for edit in history.filter(|e| e.path == "theme") {
            if edit.prop == "install" { self.changed_originals.clear(); continue }
            let (changed, old) = self.changed_originals.entry(edit.prop.clone()).or_insert_with(|| (false, edit.old.clone()));
            *changed = *old != edit.new;
        }
    }

    pub fn lines(&self) -> Vec<PropLine> {
        let Some(snap) = &self.snapshot else { return Vec::new() };
        let filter = self.filter.trim().to_ascii_lowercase();
        let path: Rc<str> = Rc::from("theme");
        let mut groups: [Vec<&ThemeToken>; 3] = Default::default();
        for t in snap.ordered_tokens() {
            if filter.is_empty() || t.name.to_ascii_lowercase().contains(&filter) {
                groups[ThemeSnapshot::token_section(t) as usize].push(t);
            }
        }
        let mut lines = Vec::new();
        for (i, (group, title)) in groups.into_iter().zip(["Colours", "Spacing & sizes", "Font sizes"]).enumerate() {
            if group.is_empty() { continue }
            lines.push(PropLine::Header { title: title.into(), count: group.len().to_string(), open: Some((super::props::view::HeaderId::Theme(i as u8), !self.collapsed[i])) });
            if self.collapsed[i] { continue }
            for t in group {
                let change = self.changed_originals.get(&t.name);
                let row = RowBinding { prop: t.name.clone(), value: t.value.clone(), kind: if i == 0 { RowKind::Color } else { RowKind::Num },
                    section: [SectionKind::Style, SectionKind::Layout, SectionKind::Text][i], set: true, quoted: false,
                    changed: change.is_some_and(|c| c.0), original: change.map(|c| c.1.clone()),
                    level: None, struct_kind: StructKind::None, comp_vals: vec![] };
                lines.push(PropLine::Prop { path: path.clone(), row, tag: None, theme_match: None, refusal: self.refusals.get(&t.name).cloned() });
            }
        }
        lines
    }

    pub fn pick(&mut self, index: usize) -> Result<Option<PendingInstall>, store::StoreError> {
        let confirm = self.confirm.take();
        self.msg.clear();
        let Some(choice) = self.entries.get(index) else { return Ok(None) };
        let building = self.tb_open && (self.tb_stands || self.inflight_is_build());
        if building || self.eq_open && (self.eq_stands || self.inflight_is_mix()) {
            let drop = ThemeConfirm::DropStanding(choice.label().into());
            if confirm != Some(drop.clone()) {
                let tool = if building { "built theme" } else { "mix" };
                self.msg = format!("{} drops the {tool} — pick again to switch", choice.label());
                self.confirm = Some(drop);
                return Ok(None)
            }
        }
        let pending = match choice {
            ThemeChoice::Builtin(p) => PendingInstall::picker(p.install(), None),
            ThemeChoice::Saved(name) => resolve_saved(&self.themes_dir(), self.snapshot.as_ref().unwrap_or(&ThemeSnapshot::default()), name)?,
        };
        if self.eq_open || self.tb_open { self.on_external_install(); }
        Ok(self.offer_install(pending))
    }

    pub fn offer_install(&mut self, install: PendingInstall) -> Option<PendingInstall> {
        if self.inflight_install.is_some() { self.waiting_install = Some(install); None } else { Some(install) }
    }

    pub fn install_settled(&mut self, req: RequestId) -> Option<PendingInstall> {
        self.inflight_install.take_if(|r| *r == req)?;
        self.waiting_install.take()
    }

    pub fn save_as(&mut self, text: &str) -> Result<(), store::StoreError> {
        if self.eq_open && (self.inflight_is_mix() || self.awaiting_snapshot)
            || self.tb_open && (self.inflight_is_build() || self.awaiting_snapshot) {
            self.save_after_snapshot = Some(text.into());
            return Ok(())
        }
        let name = store::normalize_name(text).ok_or_else(|| store::StoreError::InvalidName(text.into()))?;
        let Some(snap) = &self.snapshot else { self.msg = self.status.clone(); return Ok(()) };
        let saved = snapshot_for_save(snap, &name)?;
        self.name_text.clone_from(&name);
        let dir = self.themes_dir();
        if self.confirm.take() == Some(ThemeConfirm::Replace(name.clone())) {
            store::save_replacing_in(&dir, &saved)?;
        } else {
            match store::save_in(&dir, &saved) {
                Err(store::StoreError::Exists(_)) => {
                    self.msg = format!("{name} exists — save again to replace it");
                    self.confirm = Some(ThemeConfirm::Replace(name));
                    return Ok(())
                }
                result => { result?; }
            }
        }
        self.msg = format!("saved {name}");
        self.promote_saved(saved);
        self.refresh_saved();
        Ok(())
    }

    pub(crate) fn sync_ai_installed_saved(&mut self, saved: store::SavedTheme) {
        self.name_text.clone_from(&saved.name);
        self.worn = Some(saved);
    }

    pub(crate) fn sync_ai_deleted(&mut self, name: &str) {
        if self.worn.as_ref().is_some_and(|s| s.name == name) {
            self.worn = None;
            self.name_text.clear();
        }
        self.refresh_saved();
    }

    pub(crate) fn sync_ai_saved(&mut self, saved: store::SavedTheme, on_this_share: bool) {
        self.refresh_saved();
        if on_this_share && self.inflight_install.is_none() && !self.awaiting_snapshot { self.promote_saved(saved); }
    }

    fn promote_saved(&mut self, saved: store::SavedTheme) {
        if self.eq_open && self.eq_stands {
            self.eq_entry = Some(ThemeEntry::Saved(saved.clone()));
            self.eq_stands = false;
            self.eq_apply_due = false;
            self.eq_apply_at_once = false;
            if let Some(lab) = &mut self.eq_lab { lab.promote_standing(); }
        }
        if self.tb_open && self.tb_stands {
            self.tb_entry = Some(ThemeEntry::Saved(saved.clone()));
            self.tb_stands = false;
            self.tb_apply_due = false;
            self.tb_apply_at_once = false;
            self.tb_moving = false;
            if let Some(state) = &mut self.tb_state { state.promote_standing(); }
        }
        self.sync_ai_installed_saved(saved);
    }

    pub fn delete_saved(&mut self) -> Result<Option<usize>, store::StoreError> {
        let Some(ThemeChoice::Saved(name)) = self.entries.get(self.selected_choice_index()) else { return Ok(None) };
        if self.confirm.take() != Some(ThemeConfirm::Delete(name.clone())) {
            self.confirm = Some(ThemeConfirm::Delete(name.clone()));
            self.msg = format!("delete {name}? press delete again");
            return Ok(None)
        }
        store::delete_in(&self.themes_dir(), name)?;
        self.worn = None;
        self.name_text.clear();
        self.msg = format!("deleted {name}");
        self.refresh_saved();
        Ok(Some(self.last_preset_idx))
    }
}

impl ThemeTab {
    fn anchor_builder(&mut self) {
        let Some(snap) = &self.snapshot else { return };
        let dark = snap.sheet.as_ref().and_then(|id| snap.sheets.iter().find(|s| s.id == *id))
            .map_or(snap.base == "dark", |s| s.dark);
        self.tb_entry = self.capture_entry();
        if let Some(state) = &mut self.tb_state { state.mark_stood_down(dark); }
        else { self.tb_state = Some(ThemeBuilderState::enter(dark)); }
        self.tb_seed_slot = SeedSlot::Primary;
        self.tb_chosen = None;
        self.tb_grown_for = None;
        self.regrow_suggestions();
        self.update_build_reading();
    }

    fn regrow_suggestions(&mut self) {
        let Some(state) = &self.tb_state else { return };
        let params = state.params();
        let slot = self.tb_seed_slot;
        let seed = slot.seed_of(&params);
        self.tb_suggestions = if params.color_count == 1 {
            Vec::new()
        } else {
            let mode = if self.tb_adjust { builder::Schemes::RoundThePick } else { builder::Schemes::AsWritten };
            builder::all_suggestions_for(params.color_count, slot, seed, params.dark(), &self.tb_own_schemes, mode)
        };
        self.tb_chosen = self.tb_suggestions.iter().find(|s| s.worn_by(&params)).cloned();
        self.tb_strip_rewind |= self.tb_grown_for != Some((slot, seed));
        self.tb_grown_for = Some((slot, seed));
        self.tb_chips_changed = true;
        self.tb_suggest_due = false;
    }

    fn update_build_reading(&mut self) {
        let Some(state) = &self.tb_state else { return };
        self.tb_reading = state.readability().to_string();
        if let Some(slot) = state.crowding() {
            let color = ["primary", "secondary", "tertiary", "surface"][slot.index()];
            self.tb_reading.push_str(&format!(" · the {color} stands too close to the page, which cannot clear it and your other colours at once"));
        }
    }

    fn close_build(&mut self) -> Option<PendingInstall> {
        let restore = if self.tb_stands || self.inflight_is_build() {
            self.tb_entry.as_ref().zip(self.snapshot.as_ref()).map(|(entry, snap)| entry.restore(&snap.sheets))
        } else { None };
        self.tb_open = false;
        self.stand_down();
        self.tb_state = None;
        self.tb_reading.clear();
        self.tb_suggestions.clear();
        self.tb_own_schemes.clear();
        self.tb_chosen = None;
        restore.and_then(|p| self.offer_install(p))
    }

    pub fn build_action(&mut self, action: BuildAction) -> Option<PendingInstall> {
        if matches!(action, BuildAction::Fold) {
            self.snapshot.as_ref()?;
            self.confirm = None;
            self.msg.clear();
            if self.tb_open { return self.close_build() }
            let restore = if self.eq_open { self.close_mix() } else { None };
            self.tb_open = true;
            self.tb_installed_at = f64::NEG_INFINITY;
            self.tb_moved_at = f64::NEG_INFINITY;
            self.tb_reanchor_pending = restore.is_some() || self.inflight_install.is_some() || self.awaiting_snapshot;
            let (schemes, skipped) = store::read_palettes_in(&self.themes_dir());
            self.tb_own_schemes = schemes;
            if skipped != 0 { self.msg = format!("Skipped {skipped} invalid palette lines"); }
            if !self.tb_reanchor_pending { self.anchor_builder(); }
            return restore
        }
        if self.tb_reanchor_pending { return None }
        let was = self.tb_state.as_ref()?.params();
        let mut moving = false;
        let mut now = match action {
            BuildAction::Fold => unreachable!(),
            BuildAction::Lock(i) => { self.tb_locks[i] = !self.tb_locks[i]; return None }
            BuildAction::Adjust(on) => {
                if self.tb_adjust != on { self.tb_adjust = on; self.regrow_suggestions(); }
                return None
            }
            BuildAction::Chip(i) => {
                let offer = self.tb_suggestions.get(i)?.clone();
                let now = self.tb_surface_followed(&was, self.tb_colors_kept(&was, offer.params(was)));
                self.tb_chosen = Some(offer);
                self.tb_chips_changed = true;
                self.tb_locked_kept(&was, now)
            }
            BuildAction::Roll => {
                self.tb_seed = lab::next_mix_seed(self.tb_seed);
                let (params, words) = builder::rolled_from(&was, self.tb_seed);
                let kept = self.tb_locked_kept(&was, params);
                if !self.tb_locked(BuildLock::Palette) { self.tb_seed_slot = SeedSlot::Primary; }
                self.tb_suggest_due = true;
                words.on(kept, &was, self.tb_locked(BuildLock::TextColor), self.tb_locked(BuildLock::Row(BuildRow::TextTint)))
            }
            BuildAction::Count(count) => {
                if count == was.color_count { return None }
                if !self.tb_seed_slot.chosen_at(count) { self.tb_seed_slot = SeedSlot::Primary; }
                self.tb_grown_for = None;
                self.tb_suggest_due = true;
                BuilderParams { color_count: count, text_color: was.text_color.filter(|s| s.chosen_at(count)), ..was }
            }
            BuildAction::Text(i) => {
                let text_color = builder::tb_text_of_entry_at(i, was.color_count);
                let text_tint = if text_color.is_some() && was.text_tint == 0. { builder::TB_FIRST_TINT } else { was.text_tint };
                BuilderParams { text_color, text_tint, ..was }
            }
            BuildAction::Slider { row, value, ended } => {
                if !row.enabled(&was) { return None }
                moving = !ended;
                row.moved(was, value)
            }
            BuildAction::Reset(row) => row.house(was),
            BuildAction::Touch(i) => {
                self.tb_color_opened = Some(i);
                if self.tb_seed_slot == SeedSlot::ALL[i] { return None }
                self.tb_seed_slot = SeedSlot::ALL[i];
                was
            }
            BuildAction::Color { slot, rgba, ended } => {
                moving = !ended;
                self.tb_seed_slot = SeedSlot::ALL[slot];
                let mut palette = was.chosen_palette();
                if palette[slot] == rgba { was } else {
                    palette[slot] = rgba;
                    self.tb_surface_followed(&was, was.with_palette(palette))
                }
            }
            BuildAction::Drop(slot, drop) => {
                let shown = builder::tb_shown_slots(was.color_count);
                let from = shown.iter().position(|i| *i == slot)?;
                let palette = builder::tb_dropped_among(was.chosen_palette(), &shown, from, drop);
                self.tb_seed_slot = SeedSlot::ALL[shown[builder::tb_landed_at(shown.len(), from, drop)]];
                self.tb_surface_followed(&was, was.with_palette(palette))
            }
        };
        now.seed_slot = self.tb_seed_slot;
        self.tb_suggest_due |= was.seed_slot != now.seed_slot || now.seed_slot.seed_of(&now) != was.seed_slot.seed_of(&was);
        self.confirm = None;
        let state = self.tb_state.as_mut()?;
        if moving { state.set_moving(now); } else { state.set(now); }
        self.tb_apply_due = true;
        self.tb_apply_at_once |= !moving;
        self.tb_moving |= moving;
        None
    }

    pub fn settle_build(&mut self, now: f64) -> Option<PendingInstall> {
        if std::mem::take(&mut self.tb_moving) { self.tb_moved_at = now; }
        if !self.tb_apply_due || self.inflight_install.is_some() || self.tb_reanchor_pending { return None }
        if !self.tb_apply_at_once && (now - self.tb_installed_at < lab::EQ_SETTLE || now - self.tb_moved_at < builder::TB_QUIET) {
            return None
        }
        self.tb_apply_due = false;
        self.tb_apply_at_once = false;
        if self.tb_suggest_due { self.regrow_suggestions(); }
        let outcome = self.tb_state.as_mut()?.evaluate_apply();
        self.update_build_reading();
        let next = match outcome {
            BuildOutcome::Nothing => return None,
            BuildOutcome::Theme(install) => PendingInstall { install, meta: InstallMeta { saved: None, source: InstallSource::Build } },
            BuildOutcome::RestoreEntry => self.tb_entry.as_ref()?.restore(&self.snapshot.as_ref()?.sheets),
        };
        self.tb_installed_at = now;
        Some(next)
    }
}
