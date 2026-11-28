use crate::table::{dest_root, Table};
use std::fmt::Write;
use std::fs;

const BEGIN: &str = "<!-- solidsync doc: begin -->";
const END: &str = "<!-- solidsync doc: end -->";

fn rule_table(t: &Table) -> String {
    let mut out = String::from("| Rule | Operation | Expectation |\n|---|---|---|\n");
    let mut row = |id: &str, operation: &str, expected: String| {
        writeln!(out, "| {id} | {operation} | {} |", expected.replace('|', "\\|")).unwrap();
    };
    row("C1", "Archive the configured root files and crate copy sets. Reject symlinks and compare archive paths to the pinned tree.",
        format!("At least {} file per copy path across {} configured crate(s)", t.expect.c1_each_path_min, t.crates.len()));
    row("C2", "Reject the configured directory names, paths and media extensions.",
        format!("{} denied paths", t.expect.c2_denied));
    row("C3", "Resolve the crate license and require the legal files in the stage.", t.inventory.licenses.join(" or "));
    row("B1", "Check the build script, its inputs and its generated include.",
        format!("One consumer of `{}` and the reviewed script hash `{}`", t.build.output, t.build.hash));
    row("D1", "Remove the configured module declaration in a copied crate.", t.drops.iter()
        .map(|d| format!("{} × `{}` when `{}` is copied", d.expected, d.line, d.upstream)).collect::<Vec<_>>().join(", "));
    row("S1", "Strip test items that use excluded capabilities, ignored tests and named callers. Check the surviving references.",
        format!("At least {} cut(s) per crate, {} configured caller(s) each matched once, zero surviving denied tokens", t.expect.s1_min, t.removals.len()));
    row("O1", "Remove reviewed items and import names left unused by earlier removals.",
        format!("{} configured orphan(s), each matched once with zero surviving uses in scope", t.orphans.len()));
    row("S2", "Remove files whose module or include edges were all removed.",
        format!("At least {} deleted files per crate", t.expect.s2_min));
    row("S3", "Preserve files matching the explicit keep patterns.",
        format!("At least {} file per pattern", t.expect.s3_each_pattern_min));
    row("S4", "Remove tests named in the skip list for a reviewed runtime, platform, test failure or default-off feature.", "Each entry matches once".into());
    row("M1", "Generate the local manifest from the crate and workspace manifests. Check sections and dependency mappings.",
        format!("One manifest per crate, {} dependency mappings", t.dependencies.len()));
    row("F1", "Check the crate-level prohibition on unsafe code.", "Exactly one per library".into());
    row("I1", "Count serde helpers and thiserror forms across kept production and test code.",
        format!("Only the {} reviewed kinds", t.inventory.i1.len()));
    row("I2", "Count production capability sites and record their upstream locations.",
        format!("Only the {} reviewed kinds", t.inventory.i2.len()));
    row("A1", "Append the attribution section. Refuse a heading already present upstream.",
        format!("{} section per crate from `{}`", t.expect.a1_per_crate, t.attribution_section));
    row("N1", "Append a language-appropriate last-line notice naming the rules and patches that changed the bytes.",
        "One per changed, surviving file".into());
    out
}

fn render(source: &str, table: &Table) -> Result<String, String> {
    if source.lines().filter(|l| *l == BEGIN).count() != 1 || source.lines().filter(|l| *l == END).count() != 1 {
        return Err("SYNC.md needs one solidsync doc begin/end marker pair".into());
    }
    let (head, rest) = source.split_once(&format!("{BEGIN}\n")).ok_or("missing begin marker")?;
    let (_, tail) = rest.split_once(&format!("\n{END}")).ok_or("end marker must follow begin marker")?;
    Ok(format!("{head}{BEGIN}\n\n{}\n{END}{tail}", rule_table(table)))
}

pub fn write() -> Result<(), String> {
    let path = dest_root().join("SYNC.md");
    let source = fs::read_to_string(&path).map_err(|e| crate::files::error(&path, e))?;
    let new = render(&source, &Table::load()?)?;
    if new != source { fs::write(&path, new).map_err(|e| crate::files::error(&path, e))?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_md_rule_table_is_current() {
        let source = fs::read_to_string(dest_root().join("SYNC.md")).unwrap();
        assert_eq!(source, render(&source, &Table::load().unwrap()).unwrap(), "regenerate the rule table with solidsync doc");
    }
}
