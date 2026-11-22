//! Pure catalog search, with stable exact, prefix and substring ranks.

use super::{default_text, package_code, quantity_text};
use fab_elec_lang::{CatalogEntry, CatalogKind};
use fab_elec_parts::PartKind;

/// Matching catalog indices, best first; equal scores keep catalog order.
/// Every query word must match a field, and the weakest word sets the rank.
pub fn rank(entries: &[CatalogEntry], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    let offered = entries.iter().enumerate().filter(|(_, entry)| {
        !entry.part.as_ref().is_some_and(|part| part.kind == PartKind::PowerFlag)
    });
    if words.is_empty() {
        return offered.map(|(index, _)| index).collect();
    }
    // Three buckets keep ties stable without a sort over the catalog.
    let mut ranked: [Vec<usize>; 3] = Default::default();
    for (index, entry) in offered {
        let kind = match entry.kind {
            CatalogKind::Passive => "passive",
            CatalogKind::Part => "part",
            CatalogKind::Block => "block",
        };
        let mut fields: Vec<_> = [entry.name.as_str(), entry.title.as_str(), kind]
            .into_iter()
            .chain(entry.pins.iter().map(|pin| pin.name.as_str()))
            .map(str::to_lowercase)
            .collect();
        if let Some(fp) = entry.part.as_ref().and_then(|part| part.footprint.as_ref()) {
            fields.push(fp.name.to_lowercase());
            if let Some(code) = package_code(&fp.name) {
                fields.push(code.to_string());
            }
        }
        for (_, text) in &entry.defaults {
            let value = default_text(text);
            fields.push(value.to_lowercase());
            if let Some(sheet) = quantity_text(value) {
                fields.push(sheet.to_lowercase());
            }
        }
        let score = words.iter().try_fold(0, |worst, &word| {
            let best = fields.iter().filter_map(|field| {
                if field.as_str() == word {
                    Some(0)
                } else if field.starts_with(word) {
                    Some(1)
                } else if field.contains(word) {
                    Some(2)
                } else {
                    None
                }
            }).min()?;
            Some(worst.max(best))
        });
        if let Some(score) = score {
            ranked[score].push(index);
        }
    }
    ranked.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_lang::CatalogPin;
    use fab_elec_parts::PinKind;

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
