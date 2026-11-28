//! The shared app's mixer. Only resolved library colours and numbers enter an install.
use super::ThemeSnapshot;
use crate::tweak::{ThemeInstall, ThemeVal};
use makepad_widgets::theme_tokens::{
    held_pairs, random_weights, reads_on, set_weight, to_relative, Appearance, BlendCache,
    BlendError, BlendTheme, Scheme, ThemeBlend, TokenValue, WeightMode, RELATIVE_TOTAL,
};
use std::sync::Arc;

pub const EQ_SETTLE: f64 = 0.15;
const SAME_WEIGHT: f64 = 1e-9;

#[derive(Clone, Debug, PartialEq)]
pub struct LabRow {
    pub theme: BlendTheme,
    pub label: String,
    pub weight: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readability {
    pub measured: usize,
    pub failures: Vec<String>,
    pub margin: f64,
    pub tightest: String,
}

impl std::fmt::Display for Readability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.measured == 0 { return f.write_str("no mix to measure") }
        match self.failures.first() {
            Some(worst) => write!(f, "{} of {} pairs short · {worst}", self.failures.len(), self.measured),
            None => write!(f, "{} pairs read · closest {}", self.measured, self.tightest),
        }
    }
}

pub enum LabOutcome {
    Nothing,
    Mix(ThemeInstall),
    RestoreEntry,
}

pub struct ThemeMix {
    cache: Arc<BlendCache>,
    available: Vec<BlendTheme>,
    entry_theme: Option<BlendTheme>,
    appearance: Appearance,
    rows: Vec<LabRow>,
    anchor: usize,
    mode: WeightMode,
    applied: Option<(Appearance, Vec<f64>)>,
    blended: Option<ThemeBlend>,
    installed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MixSpec {
    Weights(Vec<(String, f64)>),
    Surprise { seed: u64 },
}

#[derive(Clone, Debug)]
pub(crate) struct MixApplied {
    pub appearance: Appearance,
    pub mode: WeightMode,
    pub weights: Vec<(String, f64, f64)>,
    pub readability: Readability,
    pub install: ThemeInstall,
}

pub(crate) fn parse_blend_theme(id: &str) -> Option<BlendTheme> {
    BlendTheme::all().into_iter().find(|t| match t {
        BlendTheme::Base(s) => s.theme_name() == id,
        BlendTheme::Sheet(s) => *s == id,
    })
}

impl MixSpec {
    /// Check the local catalogue before asking the app for its sheets.
    pub(crate) fn appearance(&self, mut group: Option<Appearance>) -> Result<Option<Appearance>, String> {
        if let Self::Weights(weights) = self {
            let mut seen = std::collections::HashSet::new();
            let mut sum = 0.;
            for (id, weight) in weights {
                if !seen.insert(id) { return Err(format!("duplicate theme {id}")) }
                let theme = parse_blend_theme(id).ok_or_else(|| format!("unknown theme {id}"))?;
                if group.is_some_and(|g| g != theme.appearance()) {
                    return Err("all weights must have the same appearance".into())
                }
                group = Some(theme.appearance());
                if !weight.is_finite() || *weight < 0. { return Err(format!("invalid weight for {id}")) }
                sum += weight;
            }
            if !(sum > 0. && sum.is_finite()) { return Err("weights must have a finite positive sum".into()) }
        }
        Ok(group)
    }
}

impl ThemeMix {
    pub(crate) fn apply_spec(cache: Arc<BlendCache>, snap: &ThemeSnapshot, appearance: Option<Appearance>,
        mode: WeightMode, spec: MixSpec) -> Result<MixApplied, String> {
        let mut lab = Self::enter(cache, snap);
        if let Some(group) = spec.appearance(appearance)? { lab.set_appearance(group); }
        match spec {
            MixSpec::Weights(weights) => {
                for row in &mut lab.rows { row.weight = 0.; }
                for (id, weight) in weights {
                    let row = lab.rows.iter_mut().find(|r| r.theme.name() == id)
                        .ok_or_else(|| format!("theme {id} is unavailable in this share"))?;
                    row.weight = weight;
                }
                lab.set_mode(mode);
            }
            MixSpec::Surprise { seed } => { lab.set_mode(mode); lab.randomize(seed); }
        }
        lab.entry_theme = None;
        lab.applied = None;
        let LabOutcome::Mix(install) = lab.evaluate_apply().map_err(|e| e.to_string())?
            else { return Err("the mix did not produce an install".into()) };
        let weights = lab.rows.iter().zip(lab.displayed_shares()).filter(|(r, _)| r.weight > 0.)
            .map(|(r, share)| (r.theme.name(), r.weight, share)).collect();
        Ok(MixApplied { appearance: lab.appearance, mode, weights, readability: lab.readability(), install })
    }

    pub fn enter(cache: Arc<BlendCache>, snap: &ThemeSnapshot) -> Self {
        let available = cache.themes().filter(|theme| match theme {
            BlendTheme::Base(_) => true,
            BlendTheme::Sheet(id) => snap.sheets.iter().any(|s| s.id == *id),
        }).collect();
        let mut lab = Self { cache, available, entry_theme: None, appearance: Appearance::Dark,
            rows: Vec::new(), anchor: 0, mode: WeightMode::Absolute, applied: None, blended: None,
            installed: false };
        lab.mark_stood_down(snap);
        lab
    }

    pub fn mark_stood_down(&mut self, snap: &ThemeSnapshot) {
        self.entry_theme = self.available.iter().copied().find(|theme| match theme {
            BlendTheme::Sheet(id) => snap.sheet.as_deref() == Some(*id),
            BlendTheme::Base(base) => snap.sheet.is_none() && snap.base == base.theme_name(),
        });
        let dark = snap.sheet.as_ref().and_then(|id| snap.sheets.iter().find(|s| &s.id == id))
            .map_or(snap.base == Scheme::Dark.theme_name(), |s| s.dark);
        self.show(if dark { Appearance::Dark } else { Appearance::Light });
        self.installed = false;
        self.blended = None;
        self.applied = Some((self.appearance, self.weights()));
    }

    // The app kept its previous theme. Let a later gesture retry these weights.
    pub fn install_failed(&mut self, stands: bool) {
        self.installed = stands;
        self.applied = None;
        self.blended = None;
    }

    pub fn promote_standing(&mut self) {
        self.installed = false;
        self.entry_theme = None;
    }

    fn show(&mut self, appearance: Appearance) {
        self.appearance = appearance;
        self.rows = self.available.iter().copied().filter(|t| t.appearance() == appearance)
            .map(|theme| LabRow { theme, label: theme.label(), weight: 0.0 }).collect();
        self.anchor = self.rows.iter().position(|r| Some(r.theme) == self.entry_theme).unwrap_or(0);
        self.reset();
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        if appearance != self.appearance { self.show(appearance); }
    }

    fn mix(&self) -> Vec<(BlendTheme, f64)> {
        self.rows.iter().map(|r| (r.theme, r.weight)).collect()
    }

    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    pub fn rows(&self) -> &[LabRow] {
        &self.rows
    }

    pub fn set_weight(&mut self, index: usize, weight: f64) {
        let mut weights = self.weights();
        set_weight(self.mode, &mut weights, self.anchor, index, weight);
        self.take_weights(weights);
    }

    pub fn clear_weight(&mut self, index: usize) {
        self.set_weight(index, 0.0);
    }

    pub fn mode(&self) -> WeightMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: WeightMode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        if mode == WeightMode::Relative {
            let weights = to_relative(&self.weights(), self.anchor);
            self.take_weights(weights);
        }
    }

    pub fn randomize(&mut self, seed: u64) {
        let drawn = random_weights(seed, self.rows.len());
        // The draw sums to one, and both modes want a slider's worth of
        // number; relative mode additionally requires the total, so scaling
        // to it is the one answer that serves both.
        let weights = to_relative(&drawn, self.anchor);
        self.take_weights(weights);
    }

    pub fn reset(&mut self) {
        let anchor = self.anchor;
        for (index, row) in self.rows.iter_mut().enumerate() {
            row.weight = if index == anchor { RELATIVE_TOTAL } else { 0.0 };
        }
    }

    pub fn is_dirty(&self) -> bool {
        let Some((appearance, weights)) = &self.applied else {
            return true;
        };
        *appearance != self.appearance
            || weights.len() != self.rows.len()
            || weights
                .iter()
                .zip(self.rows.iter())
                .any(|(was, row)| (was - row.weight).abs() >= SAME_WEIGHT)
    }

    fn weights(&self) -> Vec<f64> {
        self.rows.iter().map(|row| row.weight).collect()
    }

    fn take_weights(&mut self, weights: Vec<f64>) {
        for (row, weight) in self.rows.iter_mut().zip(weights) {
            row.weight = weight;
        }
    }

    pub fn readability(&self) -> Readability {
        let mut out = Readability::default();
        let fresh;
        let blend = match &self.blended {
            // The resolved themes cannot change while the lab is open --
            // `enter` is the only thing that fills them, and entering twice
            // does nothing -- so the weights not having moved is the whole of
            // what keeps the blend from the last apply true.
            Some(blend) if !self.is_dirty() => blend,
            _ => match self.cache.blend(&self.mix()) {
                Ok(blend) => {
                    fresh = blend;
                    &fresh
                }
                Err(_) => return out,
            },
        };
        let mut margin: Option<f64> = None;
        let mut failures: Vec<(f64, String)> = Vec::new();
        for (ground, ink, need) in held_pairs() {
            let (Some(g), Some(i)) = (blend.color(ground), blend.color(ink)) else {
                continue;
            };
            // The ink is laid over its ground before it is measured: most of
            // a theme's text carries an alpha, and white at 65 percent on a
            // mid grey is not white.
            let stands = reads_on(g | 0xFF, i);
            let line = format!("{ink} on {ground} = {stands:.2}, wants {need:.1}");
            out.measured += 1;
            if margin.is_none_or(|best| stands - need < best) {
                margin = Some(stands - need);
                out.tightest = line.clone();
            }
            if stands < *need {
                failures.push((stands - need, line));
            }
        }
        // Worst first, so that a panel with room for one line shows the pair
        // that is furthest under rather than the pair the table names first.
        // Stable, so that equally bad pairs keep the table's order and the
        // line does not change under a redraw.
        failures.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out.failures = failures.into_iter().map(|(_, line)| line).collect();
        out.margin = margin.unwrap_or(0.0);
        out
    }

    pub fn displayed_shares(&self) -> Vec<f64> {
        let weights = self.weights();
        match self.mode {
            WeightMode::Relative => shares_that_add_up(&weights, RELATIVE_TOTAL),
            WeightMode::Absolute => {
                let mut shares: Vec<_> = weights.iter().map(|w| w.max(0.0).round()).collect();
                for i in rows_that_would_print_as_out(&weights, &shares) { shares[i] = 1.0; }
                shares
            }
        }
    }

    pub fn evaluate_apply(&mut self) -> Result<LabOutcome, BlendError> {
        if !self.is_dirty() { return Ok(LabOutcome::Nothing) }
        let at_entry = self.entry_theme.is_some_and(|entry| self.rows.iter().any(|r| r.theme == entry)
            && self.rows.iter().all(|row| {
                let want = if row.theme == entry { RELATIVE_TOTAL } else { 0.0 };
                (row.weight - want).abs() < SAME_WEIGHT
            }));
        let outcome = if at_entry {
            self.blended = None;
            if std::mem::take(&mut self.installed) { LabOutcome::RestoreEntry } else { LabOutcome::Nothing }
        } else {
            let blend = self.cache.blend(&self.mix())?;
            let outcome = if self.installed && self.blended.as_ref() == Some(&blend) {
                LabOutcome::Nothing
            } else {
                let overrides = blend.overrides.iter().map(|(name, val)| {
                    let val = match val {
                        TokenValue::Color(c) => ThemeVal::Color(*c),
                        TokenValue::Num(n) => ThemeVal::Num(*n),
                        TokenValue::Raw(_) => unreachable!("BlendCache resolves only colours and numbers"),
                    };
                    (name.clone(), val)
                }).collect();
                self.installed = true;
                LabOutcome::Mix(ThemeInstall { label: "mix".into(), base: blend.base.theme_name().into(),
                    sheet: None, globals: vec![], overrides })
            };
            self.blended = Some(blend);
            outcome
        };
        self.applied = Some((self.appearance, self.weights()));
        Ok(outcome)
    }
}

pub fn next_mix_seed(seed: u64) -> u64 {
    seed.wrapping_add(0x9E37_79B9_7F4A_7C15) & (i64::MAX as u64)
}

fn shares_that_add_up(weights: &[f64], total: f64) -> Vec<f64> {
    let mut shares: Vec<f64> = weights.iter().map(|weight| weight.max(0.0).floor()).collect();
    let floors: f64 = shares.iter().sum();
    let left = (total - floors).round();
    if left < 0.0 || left > shares.len() as f64 {
        shares = weights.iter().map(|weight| weight.max(0.0).round()).collect();
    } else {
        let mut order: Vec<usize> = (0..shares.len()).collect();
        order.sort_by(|a, b| {
            let fraction = |index: usize| weights[index].max(0.0) - shares[index];
            fraction(*b)
                .partial_cmp(&fraction(*a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        for index in order.into_iter().take(left as usize) {
            shares[index] += 1.0;
        }
    }
    // ...and then no row that is in the mix printing as out of it. The part
    // comes off the largest share there is, which is the one that can least
    // notice it: the column still adds to the total, and a part off the
    // biggest row is a smaller lie than a row claiming not to be there at
    // all. Where nothing can spare one -- a column already down to ones --
    // nothing is moved, because eight rows cannot each print 1 out of four.
    for silent in rows_that_would_print_as_out(weights, &shares) {
        let Some(largest) = largest_share_that_can_spare_a_part(&shares) else {
            break;
        };
        shares[largest] -= 1.0;
        shares[silent] = 1.0;
    }
    shares
}

fn rows_that_would_print_as_out(weights: &[f64], shares: &[f64]) -> Vec<usize> {
    (0..shares.len())
        .filter(|index| weights[*index] > 0.0 && shares[*index] <= 0.0)
        .collect()
}

fn largest_share_that_can_spare_a_part(shares: &[f64]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for index in 0..shares.len() {
        if shares[index] < 2.0 {
            continue;
        }
        match best {
            Some(had) if shares[had] >= shares[index] => {}
            _ => best = Some(index),
        }
    }
    best
}

#[cfg(test)]
pub(in crate::tweak) mod tests {
    use super::*;
    use makepad_widgets::theme_tokens::{mix_rgb, BlendValue, ThemeValues};
    use std::collections::BTreeMap;
    fn made_up(theme: BlendTheme, bg: u32, ink: u32, radius: f64) -> ThemeValues {
        let mut values = BTreeMap::new();
        for (key, rgba) in [
            ("color_bg_app", bg),
            ("color_surface", bg),
            ("color_surface_container", bg),
            ("color_on_surface", ink),
            ("color_on_surface_variant", ink),
            ("color_primary", 0xFF5C39FF),
        ] {
            values.insert(key.to_string(), BlendValue::Color(rgba));
        }
        values.insert("radius_m".to_string(), BlendValue::Num(radius));
        ThemeValues::new(theme, values)
    }
    pub fn bench() -> BlendCache {
        let mut cache = BlendCache::new();
        for (step, theme) in BlendTheme::all().into_iter().enumerate() {
            let shade = ((step as u32) % 24) * 3;
            let (bg, ink) = match theme.appearance() {
                Appearance::Dark => (0x101010FF + (shade << 24) + (shade << 16) + (shade << 8), 0xEEEEEEFF),
                Appearance::Light => (0xF0F0F0FF - (shade << 24) - (shade << 16) - (shade << 8), 0x101010FF),
            };
            cache.insert(made_up(theme, bg, ink, 2.0 + step as f64));
        }
        cache
    }
    fn unreadable(theme: BlendTheme) -> ThemeValues {
        let mut values = BTreeMap::new();
        for (key, rgba) in [
            ("color_surface", 0x000000FFu32),
            ("color_on_surface", 0x727272FF),
            ("color_surface_container_highest", 0x1A1A1AFF),
            ("color_on_surface_variant", 0x343434FF),
        ] {
            values.insert(key.to_string(), BlendValue::Color(rgba));
        }
        ThemeValues::new(theme, values)
    }

    #[test]
    fn apply_spec_mixes_absolute_relative_surprise_and_refuses_bad_inputs() {
        for seed in [0, i64::MAX as u64] {
            assert!(next_mix_seed(seed) <= i64::MAX as u64, "the next panel seed after {seed} fits a tool call");
        }
        let cache = Arc::new(bench());
        let mut snap = ThemeSnapshot { base: "dark".into(), ..Default::default() };
        snap.sheets.push(super::super::SheetInfo { id: "macos-dark".into(), label: "macOS dark".into(), base: "dark".into(), dark: true });
        let apply = |mode, weights: &[(&str, f64)]| ThemeMix::apply_spec(cache.clone(), &snap, None, mode,
            MixSpec::Weights(weights.iter().map(|(id, w)| (id.to_string(), *w)).collect()));
        let weights = [("dark", 50.), ("macos-dark", 50.)];
        let abs = apply(WeightMode::Absolute, &weights).unwrap();
        let rel = apply(WeightMode::Relative, &weights).unwrap();
        assert_eq!(abs.install, rel.install, "equal weights blend the same endpoints in both modes");
        assert_eq!(rel.weights.iter().map(|r| r.2).sum::<f64>(), 100., "relative shares sum to one hundred");
        assert!(rel.readability.measured > 0, "the installed blend has measured contrast");
        let only = apply(WeightMode::Relative, &[("macos-dark", 1.)]).unwrap();
        assert_eq!(only.weights, [("macos-dark".into(), 100., 100.)], "an omitted anchor contributes no weight");
        let surprise = || ThemeMix::apply_spec(cache.clone(), &snap, Some(Appearance::Light), WeightMode::Relative, MixSpec::Surprise { seed: 42 }).unwrap();
        let drawn = surprise();
        assert_eq!((drawn.appearance, drawn.mode), (Appearance::Light, WeightMode::Relative), "the result reports the requested group and mode");
        assert_eq!(drawn.weights, surprise().weights, "a seeded surprise repeats");
        assert!(drawn.weights.iter().all(|(id, _, _)| parse_blend_theme(id).unwrap().appearance() == Appearance::Light),
            "surprise stays within its appearance group");
        for bad in [vec![], vec![("dark", 0.)], vec![("dark", 1.), ("light", 1.)], vec![("absent", 1.)],
            vec![("dark", 1.), ("dark", 1.)], vec![("macos-light", 1.)]] {
            assert!(apply(WeightMode::Relative, &bad).is_err(), "invalid or unavailable weights {bad:?} are refused");
        }
    }

    #[test]
    fn mix_arithmetic_relative_total_seed_determinism_and_readability_order() {
        let mut cache = bench();
        let dark = BlendTheme::Base(Scheme::Dark);
        let sheet = BlendTheme::Sheet("macos-dark");
        let snap = ThemeSnapshot { base: "dark".into(), sheets: vec![super::super::SheetInfo {
            id: sheet.name(), label: sheet.label(), base: "dark".into(), dark: true,
        }], ..Default::default() };
        let left = cache.get(dark).unwrap();
        let right = cache.get(sheet).unwrap();
        let midpoint = (left.num("radius_m").unwrap() + right.num("radius_m").unwrap()) / 2.;
        let colour = mix_rgb(left.color("color_bg_app").unwrap(), right.color("color_bg_app").unwrap(), 0.5);
        for mode in [WeightMode::Absolute, WeightMode::Relative] {
            let mut lab = ThemeMix::enter(Arc::new(cache.clone()), &snap);
            assert_eq!(lab.rows().len(), 2, "only shared dark themes appear");
            lab.set_mode(mode);
            lab.set_weight(1, 50.);
            lab.set_weight(0, 50.);
            let LabOutcome::Mix(install) = lab.evaluate_apply().unwrap() else { panic!("the midpoint installs a mix") };
            assert!(install.overrides.contains(&("radius_m".into(), ThemeVal::Num(midpoint))) &&
                install.overrides.contains(&("color_bg_app".into(), ThemeVal::Color(colour))),
                "{mode:?} installs the numeric and colour midpoints of the cached endpoints");
            assert!(matches!(lab.evaluate_apply(), Ok(LabOutcome::Nothing)), "unchanged weights cost no install");
            lab.randomize(42);
            let weights = lab.weights();
            lab.randomize(42);
            assert_eq!(weights, lab.weights(), "a seed reproduces the same mix");
            if mode == WeightMode::Relative {
                for step in 0..200 {
                    lab.set_weight(step % 2, ((step * 37) % 130) as f64);
                    assert!((lab.weights().iter().sum::<f64>() - 100.).abs() < SAME_WEIGHT, "relative move {step} conserves the total");
                }
            } else {
                lab.clear_weight(0);
                lab.clear_weight(1);
                assert!(matches!(lab.evaluate_apply(), Err(BlendError::NoWeight)), "empty weights are refused");
            }
        }
        assert_eq!(shares_that_add_up(&[0.1, 37.45, 37.45, 25.], 100.), [1.,37.,37.,25.],
            "rounded relative shares total exactly 100 and every contributor prints at least one");
        cache.insert(unreadable(dark));
        let reading = ThemeMix::enter(Arc::new(cache), &snap).readability();
        assert!(reading.failures.len() == 4 && reading.tightest == reading.failures[0]
            && reading.tightest.starts_with("color_on_surface_variant on color_surface_container_highest")
            && reading.failures.last().unwrap().starts_with("color_on_surface on color_surface ="),
            "readability presents the worst failure first and the near miss last");
    }
}
