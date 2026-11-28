//! Offline catalog ranking shared by the Library and the parts shortlist action.
use crate::prices::{Prices, Sourcing};
use crate::sheet::alternate_key;
use fab_elec_parts::{lifecycle_rank, Lifecycle, PartDef, PartKind};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum SourceTier { Project, Pinned, Used, Imported, Std, Library }

impl SourceTier {
    pub fn name(self) -> &'static str {
        match self {
            Self::Project => "project", Self::Pinned => "pinned", Self::Used => "used",
            Self::Imported => "imported", Self::Std => "std", Self::Library => "library",
        }
    }

    pub fn ui_label(self) -> &'static str {
        if self == Self::Used { "on board" } else { self.name() }
    }
}

pub struct Candidate<'a> {
    pub lib: &'a str,
    pub name: &'a str,
    pub title: &'a str,
    pub stem: &'a str,
    pub summary: &'a str,
    pub kind_label: &'a str,
    pub extra_tokens: &'a [String],
    pub part: Option<&'a PartDef>,
}

pub struct ShortlistContext<'a> {
    pub pinned: &'a [(String, String)],
    pub used_parts: &'a BTreeSet<String>,
    pub imported_ids: &'a BTreeSet<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RankMode { Shortlist, Search }

pub struct ShortlistQuery<'a> {
    pub text: &'a str,
    pub kind: Option<PartKind>,
    pub limit: usize,
    pub mode: RankMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PriceCoverage {
    Priced { unit_micros: i64, qty_break: i64, currency: String, source: String, date: String },
    Unpriced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortlistEntry {
    pub index: usize,
    pub lib: String,
    pub name: String,
    pub stem: String,
    pub tier: SourceTier,
    pub approved: bool,
    pub lifecycle: Option<Lifecycle>,
    pub footprint: Option<String>,
    pub mpn: Option<String>,
    pub alternates: Vec<Sourcing>,
    pub price: PriceCoverage,
    pub score: u32,
}

pub fn sourcing_of_part(part: &PartDef) -> (Sourcing, Vec<Sourcing>) {
    let field = |key| part.fields.get(key).unwrap_or_default();
    let mut primary = Sourcing {
        mpn: field("mpn").into(), supplier: field("supplier").into(), supplier_part: field("supplier_part").into(),
    };
    if primary.supplier.is_empty() && primary.supplier_part.is_empty() && !field("lcsc").is_empty() {
        primary.supplier = "LCSC".into();
        primary.supplier_part = field("lcsc").into();
    }
    let mut alternates = BTreeMap::<i64, Sourcing>::new();
    for (key, value) in &part.fields.0 {
        let Some((number, key)) = alternate_key(key) else { continue };
        match key {
            "mpn" => alternates.entry(number).or_default().mpn.clone_from(value),
            "supplier" => alternates.entry(number).or_default().supplier.clone_from(value),
            "supplier_part" => alternates.entry(number).or_default().supplier_part.clone_from(value),
            _ => {}
        }
    }
    (primary, alternates.into_values().collect())
}

fn match_score(fields: &[String], words: &[&str], mode: RankMode) -> Option<u32> {
    let mut score = 0;
    for &word in words {
        let best = fields.iter().map(|field| {
            if field == word { 3 } else if field.starts_with(word) { 2 }
            else if field.contains(word) { 1 } else { 0 }
        }).max().unwrap_or(0);
        match mode {
            RankMode::Search if best == 0 => return None,
            RankMode::Search => score = score.max(3 - best),
            RankMode::Shortlist => score += best,
        }
    }
    (mode == RankMode::Search || words.is_empty() || score > 0).then_some(score)
}

/// Rank catalog entries in O(N log N) for fixed query and field sizes. Search ties retain
/// catalog order; a block shortlist breaks ties by library and definition name.
pub fn rank_candidates(candidates: &[Candidate<'_>], ctx: &ShortlistContext<'_>, prices: &Prices, query: &ShortlistQuery<'_>) -> Vec<ShortlistEntry> {
    let pins: HashSet<_> = ctx.pinned.iter().map(|(lib, name)| (lib.as_str(), name.as_str())).collect();
    let text = query.text.to_lowercase();
    let words: Vec<_> = match query.mode {
        RankMode::Search => text.split_whitespace().collect(),
        RankMode::Shortlist => text.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect(),
    };
    let mut rows = Vec::new();
    for (index, c) in candidates.iter().enumerate() {
        if c.part.is_some_and(|p| p.kind == PartKind::PowerFlag)
            || query.kind.as_ref().is_some_and(|k| !c.part.is_some_and(|p| p.kind == *k)) { continue }
        let (primary, alternates) = c.part.map(sourcing_of_part).unwrap_or_default();
        let approved = c.part.is_some_and(|p| p.approved);
        let lifecycle = c.part.and_then(|p| p.lifecycle);
        let footprint = c.part.and_then(|p| p.footprint.as_ref()).map(|f| &f.name);
        let score = if words.is_empty() { 0 } else {
            let mut fields: Vec<_> = [c.name, c.title, c.stem, c.lib, c.summary, c.kind_label]
                .into_iter().chain(c.extra_tokens.iter().map(String::as_str)).map(str::to_lowercase).collect();
            if let Some(part) = c.part {
                fields.extend([Some(part.kind.name()), approved.then_some("approved"), lifecycle.map(Lifecycle::name),
                    footprint.map(String::as_str), Some(&primary.mpn), Some(&primary.supplier_part),
                    part.fields.get("description"), part.fields.get("value"), part.fields.get("role")]
                    .into_iter().flatten().map(str::to_lowercase));
            }
            let Some(score) = match_score(&fields, &words, query.mode) else { continue };
            score
        };
        let tier = if c.lib.is_empty() { SourceTier::Project }
            else if pins.contains(&(c.lib, c.name)) { SourceTier::Pinned }
            else if c.part.is_some_and(|p| ctx.used_parts.contains(&p.name)) { SourceTier::Used }
            else if c.lib == "imported" || ctx.imported_ids.contains(c.name) { SourceTier::Imported }
            else if c.lib == "std" { SourceTier::Std } else { SourceTier::Library };
        let price = prices.resolve(&primary).and_then(|entry| {
            let (&qty_break, &unit_micros) = entry.breaks.first_key_value()?;
            Some(PriceCoverage::Priced { qty_break, unit_micros, currency: entry.currency.clone(),
                source: entry.source.name.clone(), date: entry.source.date.clone() })
        }).unwrap_or(PriceCoverage::Unpriced);
        rows.push(ShortlistEntry { index, lib: c.lib.into(), name: c.name.into(), stem: c.stem.into(),
            tier, approved, lifecycle, footprint: footprint.cloned(), mpn: (!primary.mpn.is_empty()).then_some(primary.mpn),
            alternates, price, score });
    }
    let preference = |e: &ShortlistEntry| (e.tier, !e.approved, lifecycle_rank(e.lifecycle));
    let unpriced = |e: &ShortlistEntry| matches!(e.price, PriceCoverage::Unpriced);
    match query.mode {
        RankMode::Shortlist => rows.sort_by(|a, b| {
            (preference(a), Reverse(a.score), unpriced(a), &a.lib, &a.name)
                .cmp(&(preference(b), Reverse(b.score), unpriced(b), &b.lib, &b.name))
        }),
        RankMode::Search => rows.sort_by_key(|e| (e.score, preference(e), unpriced(e), e.index)),
    }
    if query.limit > 0 { rows.truncate(query.limit); }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prices::{Entry, PriceKey, Source};

    fn part(name: &str) -> PartDef {
        PartDef { name: name.into(), kind: PartKind::Regulator, symbol: Default::default(), footprint: None,
            body: None, pins: vec![], fields: Default::default(), emitter: None, gpio: None, lifecycle: None, approved: false }
    }

    fn candidate<'a>(lib: &'a str, p: &'a PartDef) -> Candidate<'a> {
        Candidate { lib, name: &p.name, title: &p.name, stem: "u", kind_label: "part", summary: "", extra_tokens: &[], part: Some(p) }
    }

    fn query(text: &str) -> ShortlistQuery<'_> {
        ShortlistQuery { text, mode: RankMode::Shortlist, kind: None, limit: 0 }
    }

    #[test]
    fn shortlist_orders_my_libraries_first_and_approved_active_within_tier() {
        let tiers = [SourceTier::Project, SourceTier::Pinned, SourceTier::Used, SourceTier::Imported, SourceTier::Std, SourceTier::Library];
        let mut parts = Vec::new();
        let mut pinned = Vec::new();
        let mut used = BTreeSet::new();
        let mut imported = BTreeSet::new();
        for tier in tiers {
            for (i, (approved, state)) in [(true, Some(Lifecycle::Active)), (true, None), (true, Some(Lifecycle::Nrnd)),
                (true, Some(Lifecycle::Obsolete)), (false, Some(Lifecycle::Active))].into_iter().enumerate() {
                let mut p = part(&format!("{}-{i}", tier.name()));
                p.approved = approved;
                p.lifecycle = state;
                let lib = match tier { SourceTier::Project => "", SourceTier::Std => "std", _ => "lib" };
                if tier == SourceTier::Pinned { pinned.push((lib.into(), p.name.clone())); }
                if tier <= SourceTier::Used { used.insert(p.name.clone()); }
                if tier <= SourceTier::Imported { imported.insert(p.name.clone()); }
                parts.push((lib, p, tier));
            }
        }
        let mut candidates = parts.iter().map(|(lib, p, _)| candidate(lib, p)).collect::<Vec<_>>();
        let ctx = ShortlistContext { pinned: &pinned, used_parts: &used, imported_ids: &imported };
        let expected: Vec<_> = parts.iter().map(|(_, p, tier)| (p.name.clone(), *tier)).collect();
        for _ in 0..2 {
            let ranked = rank_candidates(&candidates, &ctx, &Prices::default(), &query(""));
            assert_eq!(ranked.into_iter().map(|r| (r.name, r.tier)).collect::<Vec<_>>(), expected,
                "tier, approval and lifecycle must determine shortlist order independently of catalog enumeration");
            candidates.reverse();
        }
    }

    #[test]
    fn shortlist_resolves_offline_price_coverage_and_alternates_without_a_board() {
        let mut p = part("reg");
        p.fields.0 = [("mpn", "M"), ("lcsc", "C123"), ("alt.1.mpn", "ALT")].map(|(k,v)| (k.into(), v.into())).into();
        let mut other = p.clone();
        other.name = "unpriced".into();
        other.fields.0 = [("mpn", "OTHER"), ("alt.1.mpn", "M")].map(|(k,v)| (k.into(), v.into())).into();
        let key = PriceKey::Mpn("M".into());
        let entry = Entry { key: key.clone(), currency: "USD".into(), breaks: [(10, 420000), (100, 320000)].into(),
            source: Source { name: "book".into(), date: "2026-11-20".into(), url: None }, moq: 1, multiple: 1, assembly_class: None };
        let prices = Prices([(key, entry)].into());
        let empty = BTreeSet::new();
        let ctx = ShortlistContext { pinned: &[], used_parts: &empty, imported_ids: &empty };
        let rows = rank_candidates(&[candidate("", &other), candidate("", &p)], &ctx, &prices, &query(""));
        assert_eq!(rows[0].price, PriceCoverage::Priced { unit_micros: 420000, qty_break: 10, currency: "USD".into(),
            source: "book".into(), date: "2026-11-20".into() }, "the primary gets its offline price at the lowest break");
        assert_eq!(rows[0].alternates, vec![Sourcing { mpn: "ALT".into(), ..Default::default() }], "alternate sourcing is retained without a board");
        assert_eq!(rows[1].price, PriceCoverage::Unpriced, "a priced alternate must not supply its primary's price");
        let (primary, _) = sourcing_of_part(&p);
        assert_eq!((primary.supplier.as_str(), primary.supplier_part.as_str()), ("LCSC", "C123"), "an lcsc field identifies the primary supplier");
    }

    #[test]
    fn shortlist_matches_natural_language_block_roles_by_token_overlap() {
        let mut ldo = part("reg33");
        ldo.fields.0.push(("description".into(), "3.3V LDO".into()));
        let mut switch = part("switch");
        switch.kind = PartKind::Switch;
        let empty = BTreeSet::new();
        let ctx = ShortlistContext { pinned: &[], used_parts: &empty, imported_ids: &empty };
        let rows = rank_candidates(&[candidate("std", &switch), candidate("std", &ldo)], &ctx, &Prices::default(), &query("5V to 3V3 LDO"));
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["reg33"], "a partial role match finds the LDO and excludes an unrelated part");
    }
}
