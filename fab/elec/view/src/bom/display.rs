//! Prepare the strip, comparison table and row text on the pool with the estimate.
use fab_elec_bom::{cost::{Amount, CostRun, Purchase, WholeEstimate}, prices::Source, BomSheet};
use std::collections::{BTreeMap, BTreeSet};

pub struct PriceCell {
    pub label: String,
    pub old: bool,
}

fn source(s: &Source) -> String {
    let mut text = format!("{} · {}", s.name, s.date);
    if let Some(url) = &s.url { text.push_str(" · "); text.push_str(url); }
    text
}

fn values(amounts: &[Amount]) -> String {
    amounts.iter().map(Amount::display).collect::<Vec<_>>().join(" + ")
}

fn sources<'a>(amounts: impl Iterator<Item = &'a Amount>) -> String {
    amounts.flat_map(|a| &a.sources).collect::<BTreeSet<_>>().into_iter().map(source).collect::<Vec<_>>().join(" / ")
}

fn purchase(p: &Purchase, today: &str) -> PriceCell {
    let mut label = match (&p.unit, &p.bought, &p.used_per_board, p.ordered) {
        (Some(u), Some(b), Some(used), Some(n)) => format!("{} each   ·   Bought {n} for {}   ·   Used {} per board\n{}",
            u.display(), b.display(), used.display(), sources([u,b,used].into_iter())),
        _ => String::new(),
    };
    if !p.notes.is_empty() {
        if !label.is_empty() { label.push_str(" · "); }
        label.push_str(&p.notes.join(" "));
    }
    // Tone the price after its first anniversary; its original date and full value stay visible.
    let old = p.unit.as_ref().is_some_and(|u| u.sources.iter().any(|s| {
        let year = s.date.get(..4).and_then(|y| y.parse::<u32>().ok());
        year.zip(s.date.get(4..)).is_some_and(|(y, md)| format!("{:04}{md}", y + 1).as_str() <= today)
    }));
    PriceCell { label, old }
}

fn totals(r: &CostRun) -> String {
    let boards = if r.boards == 1 { "board" } else { "boards" };
    let bound = if r.lower_bound { " lower bound" } else { "" };
    if r.totals.is_empty() { return format!("{} {boards} · no priced charges",r.boards); }
    format!("{} {boards}{bound} · {} run · {} per board", r.boards, values(&r.totals), values(&r.per_board))
}

pub fn cost_text(estimate: Result<WholeEstimate,String>, grouped: &BomSheet, shown: &BomSheet, today: &str)
    -> (String, String, Vec<PriceCell>)
{
    let unavailable = |e: String| (format!("Estimate unavailable · {e}"),String::new(),Vec::new());
    let estimate = match estimate { Ok(e) => e, Err(e) => return unavailable(e) };
    let run = match estimate.selected { Ok(r) => r, Err(e) => return unavailable(e) };
    let parts = if run.parts.is_empty() { String::new() } else { format!(" · {}", values(&run.parts)) };
    let fab = run.fabrication.as_ref().map(|r| r.totals.iter().map(|a| a.display()).collect::<Vec<_>>().join(" + "))
        .unwrap_or_else(|e| e.clone());
    let assembly = run.assembly.as_ref().map(|a| a.total.display()).unwrap_or_else(|e| e.clone());
    let summary = format!("{}\nParts · {} of {} lines priced{}   |   Fabrication · {}   |   Assembly · {}\n{}\n{}",
        totals(&run),run.priced,run.fitted,parts,fab,assembly,sources(run.totals.iter()),estimate.excludes);
    let mut details = vec!["Quantity comparison".into()];
    for (q, r) in estimate.comparisons {
        details.push(match r {
            Ok(r) => format!("{} · {} of {} lines priced · {}", totals(&r),r.priced,r.fitted,sources(r.totals.iter())),
            Err(why) => format!("{q} boards · {why}"),
        });
    }
    if let Ok(f) = run.fabrication {
        details.push(format!("Fabrication · setup {} · {} each · {} · {}",f.setup.display(),f.each.display(),f.setup.source,f.setup.date));
        details.extend(f.surcharges.iter().map(|(n,a)| format!("{n} · {} · {} · {}", a.display(),a.source,a.date)));
    }
    if let Ok(a) = run.assembly {
        details.extend(a.charges.iter().map(|(n,a)| format!("{n} · {} · {}",a.display(),sources(std::iter::once(a)))));
        details.extend(a.assumptions);
    }
    if let Some(x) = run.exchange {
        details.push(format!("Exchange · 1 {} = {}.{:06} {} · {}",x.from,x.rate / 1_000_000,x.rate % 1_000_000,x.to,source(&x.source)));
    }
    if let Some(a) = run.extra { details.push(format!("Flat amount · {} · {}",a.display(),sources(std::iter::once(&a)))); }
    if !run.dnp.is_empty() { details.push(format!("DNP · {}",run.dnp.join(", "))); }

    // An expanded row still reports its purchasing line's order and price break.
    let mut owners = BTreeMap::new();
    let same_sheet = std::ptr::eq(grouped,shown);
    if !same_sheet {
        for (i,row) in grouped.rows.iter().enumerate() {
            for path in &row.paths { owners.insert(path,i); }
        }
    }
    let mut cells = Vec::with_capacity(shown.lines.len());
    for &(row, alt) in &shown.lines {
        let visible = &shown.rows[row];
        let owner = if same_sheet { Some(row) } else { visible.paths.first().and_then(|p| owners.get(p)).copied() };
        let order = owner.and_then(|i| {
            let line = &run.lines[i];
            if let Some(j) = alt {
                let a = &visible.alternates[j];
                let group = &grouped.rows[i];
                let candidate = if same_sheet { a } else { group.alternates.iter().find(|b| b.number == a.number && b.mpn == a.mpn &&
                    b.supplier == a.supplier && b.supplier_part == a.supplier_part && a.comps.iter().all(|c| b.comps.contains(c)))? };
                line.alternates.iter().find(|(k,n,_)| k == &candidate.key && *n == candidate.number).map(|(_,_,p)| p)
            } else { line.fitted.as_ref() }
        });
        let mut cell = order.map_or_else(|| PriceCell { label:"DNP · excluded from the order".into(), old:false }, |p| purchase(p,today));
        if let Some(i) = owner.filter(|&i| grouped.rows[i].key != visible.key) {
            cell.label = format!("{} line · {}",grouped.rows[i].cells[0],cell.label);
        }
        cells.push(cell);
    }
    (summary,details.join("\n"),cells)
}
