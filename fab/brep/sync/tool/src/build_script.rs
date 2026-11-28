use crate::inventory::{capabilities, matches, Inventory};
use crate::strip::Source;
use crate::table::{Generated, Table, Tree};
use makepad_core_util::sha256::sha256_hex;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub struct BuildReport {
    pub hash: String,
    pub sites: BTreeMap<String, Vec<String>>,
    pub details: String,
}

fn diff(old: &str, new: &str) -> String {
    let old: Vec<_> = old.lines().collect();
    let new: Vec<_> = new.lines().collect();
    let first = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let same_tail = old[first..].iter().rev().zip(new[first..].iter().rev()).take_while(|(a, b)| a == b).count();
    let mut out = format!("@@ from line {} @@\n", first + 1);
    for line in &old[first..old.len() - same_tail] { out.push_str(&format!("-{line}\n")); }
    for line in &new[first..new.len() - same_tail] { out.push_str(&format!("+{line}\n")); }
    out
}

fn producer_inputs(s: &Source<'_>, record: &Generated) -> Result<(), String> {
    let (dir, suffix) = record.inputs.rsplit_once("/*").ok_or("B1: expected a directory/*.suffix input pattern")?;
    let mut joins = Vec::new();
    for part in dir.split('/') {
        joins.extend([".".into(), "join".into(), "(".into(), format!("{part:?}"), ")".into()]);
    }
    let joins: Vec<&str> = joins.iter().map(String::as_str).collect();
    let extension = format!("{suffix:?}");
    let output = format!("{:?}", record.output);
    for tokens in [&joins[..], &[".", "strip_suffix", "(", &extension, ")"],
        &[".", "join", "(", &output, ")"], &["\"OUT_DIR\""]] {
        if !(0..s.toks.len()).any(|i| matches(s, i, tokens)) {
            return Err(format!("B1 {}: producer no longer declares {}", s.path, tokens.join(" ")));
        }
    }
    Ok(())
}

pub(crate) fn inspect(source: &Source<'_>, record: &Generated, installed: Option<&str>, accepted: Option<&str>,
    sites: &[(String, bool)]) -> Result<BuildReport, String> {
    let hash = sha256_hex(source.src.as_bytes());
    let capability_sites = capabilities(source);
    let changed = record.hash != hash;
    let mut details = String::new();
    if installed.is_none() { details.push_str(&format!("first sync of {}: {hash}\n", record.producer)); }
    if changed {
        details.push_str(&format!("old SHA-256 {}\nnew SHA-256 {hash}\n", record.hash));
        if let Some(old) = installed { details.push_str(&diff(old, source.src)); }
    }
    let known = sites.iter().filter(|(_, k)| *k).count();
    let extra = sites.iter().any(|(_, k)| !*k);
    for (site, ok) in sites {
        details.push_str(&format!("{} {site}\n", if *ok { "consumer" } else { "additional generated-code site" }));
    }
    if extra && !changed {
        details.push_str(&format!("old SHA-256 {}\nnew SHA-256 {hash}\n", record.hash));
    }
    if (changed || extra) && accepted != Some(hash.as_str()) {
        return Err(format!("B1: {details}review required: --accept-build-rs {hash}"));
    }
    if accepted.is_some_and(|given| given != hash) { return Err(format!("B1: accepted hash does not match {hash}")); }
    if known != 1 { return Err(format!("B1: {details}expected exactly one declared consumer, found {known}")); }
    producer_inputs(source, record).map_err(|e| format!("{details}{e}"))?;
    Ok(BuildReport { hash, sites: capability_sites, details })
}

pub fn check<'a>(tree: &Tree, table: &Table, dest: &Path, accepted: Option<&str>, inventories: impl Iterator<Item = &'a Inventory>)
    -> Result<BuildReport, String> {
    let record = &table.build;
    let (spec, rel) = table.crates.iter().find_map(|spec| {
        record.producer.strip_prefix(&format!("{}/", spec.upstream)).map(|rel| (spec, rel))
    }).ok_or("B1: producer outside copied crates")?;
    if !spec.keep.contains(&record.inputs) { return Err(format!("B1: missing keep pattern {}", record.inputs)); }
    let bytes = tree.get(&record.producer).ok_or_else(|| format!("B1: missing {}", record.producer))?;
    let edition = crate::manifest::edition(tree, &spec.upstream, &table.workspace_manifest)?;
    let source = Source::parse(&record.producer, bytes, edition)?;
    let installed_path = dest.join(&spec.dest).join(rel);
    let old = match fs::read_to_string(&installed_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("B1 {}: {e}", installed_path.display())),
    };
    let sites: Vec<_> = inventories.flat_map(|i| i.generated.iter().cloned()).collect();
    inspect(&source, record, old.as_deref(), accepted, &sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_rs_needs_its_hash_accepted() {
        let text = "fn main() { p.join(\"src\").join(\"flags\"); f.strip_suffix(\".md\"); var(\"OUT_DIR\"); out.join(\"gen.rs\"); }";
        let old = Source::parse("sample/build.rs", text.as_bytes(), 2021).unwrap();
        let record = Generated { producer: old.path.into(), consumer: "sample/src/lib.rs".into(),
            inputs: "src/flags/*.md".into(), output: "gen.rs".into(), hash: sha256_hex(text.as_bytes()) };
        let consumer = vec![("sample/src/lib.rs:1".into(), true)];
        let before = inspect(&old, &record, None, None, &consumer).unwrap();
        let changed = format!("{text}\nfn connect() {{ std::net::TcpStream::connect(\"x\"); }}");
        let new = Source::parse("sample/build.rs", changed.as_bytes(), 2021).unwrap();
        let err = inspect(&new, &record, Some(text), None, &consumer).err().unwrap();
        assert!(err.contains(&before.hash) && err.contains("--accept-build-rs") && err.contains("+fn connect"), "{err}");
        let hash = sha256_hex(changed.as_bytes());
        let after = inspect(&new, &record, Some(text), Some(&hash), &consumer).unwrap();
        assert!(after.sites.contains_key("std::net") && !before.sites.contains_key("std::net"));
        let mut table = crate::modules::tests::sample_rules();
        table.drops.clear();
        table.expect.s1_min = 0;
        table.crates[0].keep.clear();
        table.crates[0].copy.push("build.rs".into());
        table.build = record;
        let tree = Tree::from([
            ("sample/Cargo.toml".into(), b"[package]\nedition = \"2021\"".to_vec()),
            ("sample/src/lib.rs".into(), b"#![forbid(unsafe_code)]\ninclude!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));".to_vec()),
            ("sample/build.rs".into(), changed.as_bytes().to_vec()),
        ]);
        let prepared = crate::rules::prepare(&tree, &table, &[]).unwrap();
        let inventory = &prepared.summaries["sample"].inventory;
        inventory.check(&table.inventory).expect("build-script network access must not enter I2");
        let reason = inspect(&new, &table.build, Some(text), None, &inventory.generated).err().unwrap();
        assert!(reason.contains("--accept-build-rs") && reason.contains(&hash));
        let extra = vec![("expected".into(), true), ("another.rs:2".into(), false)];
        assert!(inspect(&old, &table.build, None, None, &extra).err().unwrap().contains("another.rs:2"));
    }
}
