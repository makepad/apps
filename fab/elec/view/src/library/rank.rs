//! The Library's catalog fields, ranked by the same offline ranker as the parts action.
use super::{default_text, package_code, quantity_text};
use fab_elec_bom::prices::Prices;
use fab_elec_bom::shortlist::{rank_candidates, Candidate, RankMode, ShortlistContext, ShortlistEntry, ShortlistQuery};
use fab_elec_lang::{CatalogEntry, CatalogKind};
pub use fab_elec_parts::lifecycle_rank;

pub fn rank_with_context(entries: &[CatalogEntry], query: &str, ctx: &ShortlistContext<'_>, prices: &Prices) -> Vec<ShortlistEntry> {
    let tokens: Vec<_> = entries.iter().map(|entry| {
        let mut extra: Vec<_> = entry.pins.iter().map(|pin| pin.name.clone()).collect();
        if let Some(code) = entry.part.as_ref().and_then(|p| p.footprint.as_ref()).and_then(|fp| package_code(&fp.name)) {
            extra.push(code.into());
        }
        for (_, literal) in &entry.defaults {
            let value = default_text(literal);
            extra.push(value.into());
            extra.extend(quantity_text(value));
        }
        extra
    }).collect();
    let candidates: Vec<_> = entries.iter().zip(&tokens).map(|(e, extra)| Candidate {
        lib: &e.lib, name: &e.name, title: &e.title, stem: &e.stem, summary: &e.summary,
        kind_label: match e.kind { CatalogKind::Passive => "passive", CatalogKind::Part => "part", CatalogKind::Block => "block" },
        extra_tokens: extra, part: e.part.as_deref(),
    }).collect();
    rank_candidates(&candidates, ctx, prices, &ShortlistQuery { text: query, kind: None, limit: 0, mode: RankMode::Search })
}

pub fn rank(entries: &[CatalogEntry], query: &str) -> Vec<usize> {
    let empty = Default::default();
    let ctx = ShortlistContext { pinned: &[], used_parts: &empty, imported_ids: &empty };
    rank_with_context(entries, query, &ctx, &Prices::default()).into_iter().map(|r| r.index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_lang::CatalogPin;
    use fab_elec_parts::{Lifecycle, PartDef, PartKind, PinKind};
    use std::sync::Arc;

    #[test]
    fn ranking_prefers_approved_and_active_over_unapproved_nrnd_and_obsolete() {
        let entry = |name: &str, approved: bool, lifecycle: Option<Lifecycle>| CatalogEntry {
            lib: String::new(), name: name.into(), kind: CatalogKind::Part, title: name.into(), summary: String::new(),
            pins: Vec::new(), stmt: String::new(), stem: String::new(), defaults: Vec::new(), hash: 0,
            part: Some(Arc::new(PartDef {
                name: name.into(), kind: PartKind::Ic, symbol: Default::default(), footprint: None, body: None,
                pins: Vec::new(), fields: Default::default(), emitter: None, gpio: None, lifecycle, approved,
            })),
        };
        // Catalog order is the reverse of the preferred order, so only preference can sort them.
        let entries = vec![
            entry("mcu_obsolete", false, Some(Lifecycle::Obsolete)),
            entry("mcu_nrnd", false, Some(Lifecycle::Nrnd)),
            entry("mcu_plain", false, None),
            entry("mcu_active", false, Some(Lifecycle::Active)),
            entry("mcu_approved", true, None),
            entry("mcu_approved_active", true, Some(Lifecycle::Active)),
        ];
        assert_eq!(rank(&entries, "mcu"), vec![5, 4, 3, 2, 1, 0],
            "approved+active < approved < active < unstated < NRND < obsolete among equal matches");
        assert_eq!(rank(&entries, ""), vec![5, 4, 3, 2, 1, 0], "the whole catalog is in preferred order too");
        assert_eq!(rank(&entries, "approved"), vec![5, 4], "approved is searchable");
        assert_eq!(rank(&entries, "active"), vec![5, 3], "active is searchable, approved first");
        assert_eq!(rank(&entries, "nrnd"), vec![1], "nrnd is searchable");
        assert_eq!(rank(&entries, "obsolete"), vec![0], "obsolete is searchable");
        let mut exact = entries.clone();
        exact.push(entry("mcu", false, Some(Lifecycle::Obsolete)));
        assert_eq!(rank(&exact, "mcu"), vec![6, 5, 4, 3, 2, 1, 0], "an exact match still beats preferred prefix matches");
        let mut ties = entries.clone();
        ties.push(entry("mcu_twin", true, Some(Lifecycle::Active)));
        assert_eq!(rank(&ties, "mcu")[..2], [5, 6], "equal preference keeps catalog order");
        let unmarked = vec![entry("b", false, None), entry("a", false, None), entry("c", false, None)];
        assert_eq!(rank(&unmarked, ""), vec![0, 1, 2], "unmarked parts keep catalog order");
    }

    #[test]
    fn library_search_ranks_exact_then_prefix_then_contains() {
        let entry = |name: &str, title: &str, kind, pins: &[&str]| CatalogEntry {
            lib: String::new(),
            name: name.into(),
            kind,
            title: title.into(),
            summary: String::new(),
            pins: pins.iter().map(|name| CatalogPin {
                name: (*name).into(),
                kind: PinKind::Passive,
            }).collect(),
            stmt: String::new(),
            stem: String::new(),
            defaults: Vec::new(),
            part: None,
            hash: 0,
        };
        let entries = vec![
            entry("connector", "Socket", CatalogKind::Part, &["preCLKpost", "power"]),
            entry("oscillator", "CLK source", CatalogKind::Block, &["power"]),
            entry("clk", "Clock", CatalogKind::Passive, &["power"]),
            entry("clk_helper", "Buffer", CatalogKind::Part, &["power"]),
        ];
        assert_eq!(rank(&entries, "  ClK  "), vec![2, 1, 3, 0],
            "exact name beats prefix title and contained pin; case, trim and stable ties");
        assert_eq!(rank(&entries, "clk\tPOWER"), vec![2, 1, 3, 0],
            "query words may match different fields");
        assert_eq!(rank(&entries, "clk pow"), vec![1, 2, 3, 0],
            "the worst word sets the score, preserving catalog order on ties");
        assert_eq!(rank(&entries, "clk block"), vec![1],
            "kind participates in multi-word matches");
        assert_eq!(rank(&entries, "part"), vec![0, 3],
            "kind matches keep catalog order");
        assert_eq!(rank(&entries, ""), vec![0, 1, 2, 3],
            "an empty query returns the whole catalog in order");
        assert_eq!(rank(&entries, " \n\t "), vec![0, 1, 2, 3],
            "whitespace alone is an empty query");
        assert!(rank(&entries, "missing").is_empty(), "no matching field yields no results");
        assert!(rank(&entries, "clk missing").is_empty(), "every query word must match");
    }

    #[test]
    fn library_search_finds_values_footprints_and_pins() {
        use fab_elec_lang::{defs_catalog, std_libs, ProjectDefs, Sources};

        let libs = std_libs();
        let mut sources = Sources::new();
        sources.add("search.blocks.splash", "let e = mod.elec\nlet catalog = fn() {[ fn(a, b) { e.r(10000ohm, a, b) } ]}\n")
            .expect("the project catalog document has a valid name");
        let catalog = defs_catalog(&libs, &ProjectDefs::new(&sources, &libs));
        let entries = &catalog.entries;
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        let finds = |query: &str, name: &str| rank(entries, query).into_iter().any(|i| entries[i].name == name);
        for (query, name, property) in [
            ("10k", "r 10kohm 0402", "the sheet value finds the standard resistor"),
            ("10k", "r 10000ohm", "sheet notation finds a value written without a prefix"),
            ("10000ohm", "r 10000ohm", "the source literal finds it too"),
            ("0603", "led @green 0603", "the package code finds the standard LED"),
            ("r_0402", "r 10kohm 0402", "the full footprint name finds the standard resistor"),
            ("green", "led @green 0603", "a default's text without its @ finds the LED"),
            ("cc1", "USB_C_POWER", "a pin name alone still finds the USB-C receptacle"),
        ] {
            assert!(finds(query, name), "{property}: {query:?} should find {name:?} in {names:?}");
        }
    }
}
