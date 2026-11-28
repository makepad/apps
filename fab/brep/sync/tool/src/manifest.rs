use crate::config::{document, Row, Value};
use crate::table::{CrateSpec, DepTarget, Table, Tree};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

fn rows(tree: &Tree, path: &str) -> Result<Vec<Row>, String> {
    let data = tree.get(path).ok_or_else(|| format!("M1: missing {path}"))?;
    document(path, std::str::from_utf8(data).map_err(|e| format!("{path}: {e}"))?, true)
}

// Follow local feature aliases in the manifest M1 actually emits. A
// dependency's forwarded feature (dep/feature) is not a local alias.
pub(crate) fn default_features(tree: &Tree, spec: &CrateSpec, table: &Table) -> Result<BTreeSet<String>, String> {
    let manifest = generate(tree, spec, table, "")?;
    let mut enabled = BTreeSet::new();
    if let Some(features) = document("generated Cargo.toml", &manifest, true)?.into_iter().find(|r| r.name == "features") {
        let mut pending = vec!["default"];
        while let Some(name) = pending.pop() {
            let Some((_, Value::Strings(children))) = features.fields.get(name) else { continue; };
            if enabled.insert(name.to_owned()) { pending.extend(children.iter().map(String::as_str)); }
        }
    }
    Ok(enabled)
}


pub(crate) fn edition(tree: &Tree, krate: &str, workspace: &str) -> Result<u16, String> {
    let path = format!("{krate}/Cargo.toml");
    let mut package = rows(tree, &path)?.into_iter().find(|r| r.name == "package" && !r.array)
        .ok_or_else(|| format!("M1 {path}: missing [package]"))?;
    let text = if package.fields.contains_key("edition.workspace") {
        if package.fields.contains_key("edition") || !package.boolean("edition.workspace")? {
            return Err(format!("M1 {path}: invalid workspace edition"));
        }
        let mut ws = rows(tree, workspace)?.into_iter().find(|r| r.name == "workspace.package" && !r.array)
            .ok_or_else(|| format!("M1 {workspace}: missing [workspace.package]"))?;
        ws.string("edition")?
    } else { package.string("edition")? };
    match text.as_str() {
        "2015" | "2018" | "2021" | "2024" => Ok(text.parse().unwrap()),
        _ => Err(format!("M1 {path}: unknown Rust edition {text}")),
    }
}

fn relative(from: &str, to: &str) -> String {
    let from: Vec<_> = from.split('/').collect();
    let to: Vec<_> = to.split('/').collect();
    let same = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path = vec![".."; from.len() - same];
    path.extend_from_slice(&to[same..]);
    path.join("/")
}

fn fields(row: &Row, workspace: Option<&Row>) -> Result<BTreeMap<String, Value>, String> {
    let mut fields = BTreeMap::new();
    for (key, (at, value)) in &row.fields {
        let (name, value) = if let Some(name) = key.strip_suffix(".workspace") {
            if value != &Value::Bool(true) { return Err(format!("{at}: expected {key} = true")); }
            let inherited = workspace.and_then(|w| w.fields.get(name))
                .ok_or_else(|| format!("{at}: missing workspace {}.{name}", row.name))?;
            (name, &inherited.1)
        } else { (key.as_str(), value) };
        if fields.insert(name.into(), value.clone()).is_some() { return Err(format!("{at}: duplicate {name}")); }
    }
    Ok(fields)
}

fn dep_attributes(value: Value) -> Result<BTreeMap<String, Value>, String> {
    match value {
        Value::Text(v) => Ok(BTreeMap::from([("version".into(), Value::Text(v))])),
        Value::Inline(a) => Ok(a),
        _ => Err("expected a version string or dependency table".into()),
    }
}

fn dep_lines(row: &Row, workspace: Option<&Row>, spec: &CrateSpec, table: &Table, dev: bool) -> Result<String, String> {
    let mut lines = String::new();
    let deps = fields(row, workspace)?;
    let expected = if dev { &spec.dev_dependencies } else { &spec.dependencies };
    if deps.keys().collect::<BTreeSet<_>>() != expected.iter().collect() {
        return Err(format!("M1 {} [{}]: expected {:?}, found {:?}", spec.upstream, row.name, expected, deps.keys()));
    }
    for (name, value) in deps {
        let map = &table.dependencies[&name];
        let mut attrs = dep_attributes(value)?;
        if let Some(inherit) = attrs.remove("workspace") {
            if inherit != Value::Bool(true) { return Err(format!("M1 {name}.workspace must be true")); }
            let mut base = dep_attributes(workspace.and_then(|w| w.fields.get(&name))
                .ok_or_else(|| format!("M1 missing workspace dependency {name}"))?.1.clone())?;
            if let Some(value) = attrs.remove("features") {
                let Value::Strings(mut extra) = value else { return Err(format!("M1 {name}: invalid inherited features")); };
                if let Some(value) = base.remove("features") {
                    let Value::Strings(orig) = value else { return Err(format!("M1 {name}: invalid workspace features")); };
                    extra.extend(orig);
                }
                base.insert("features".into(), Value::Strings(extra));
            }
            if !attrs.is_empty() { return Err(format!("M1 {name}: unsupported workspace override {:?}", attrs.keys())); }
            attrs = base;
        }
        let version = attrs.remove("version");
        let path = attrs.remove("path");
        let defaults = attrs.remove("default-features").unwrap_or(Value::Bool(true));
        let features = attrs.remove("features").unwrap_or_else(|| Value::Strings(Vec::new()));
        let Value::Strings(mut features) = features else { return Err(format!("M1 {name}: invalid features")); };
        let mut wanted = map.features.clone();
        wanted.sort();
        features.sort();
        let expected_version = (!map.version.is_empty()).then(|| Value::Text(map.version.clone()));
        if !attrs.is_empty() || version != expected_version || path != map.path.clone().map(Value::Text)
            || defaults != Value::Bool(map.default_features) || features != wanted {
            return Err(format!("M1 {name}: unexpected dependency specification: version {version:?}, path {path:?}, \
                default-features {defaults:?}, features {features:?}, extra keys {:?}", attrs.keys()));
        }
        if dev { continue; }
        let (package, dest) = match &map.target {
            DepTarget::Shim { package, shim } => (package, format!("{}/{shim}", table.shim_root)),
            DepTarget::Sibling(up) => {
                let sibling = table.crates.iter().find(|s| s.upstream == *up)
                    .ok_or_else(|| format!("M1 {name}: missing sibling {up}"))?;
                (&sibling.package, sibling.dest.clone())
            }
            DepTarget::Drop => continue,
        };
        writeln!(lines, "{name} = {{ package = {package:?}, path = {:?} }}", relative(&spec.dest, &dest)).unwrap();
    }
    Ok(lines)
}

pub fn generate(tree: &Tree, spec: &CrateSpec, table: &Table, pin: &str) -> Result<String, String> {
    let source = format!("{}/Cargo.toml", spec.upstream);
    let workspace = rows(tree, &table.workspace_manifest)?;
    let section = |name| workspace.iter().find(|r| r.name == name && !r.array);
    let mut package = BTreeMap::new();
    let mut deps = String::new();
    let mut features = String::new();
    let mut seen = BTreeSet::new();
    for mut row in rows(tree, &source)? {
        seen.insert(row.name.clone());
        match (row.name.as_str(), row.array) {
            ("package", false) => {
                for (key, value) in fields(&row, section("workspace.package"))? {
                    match (key.as_str(), value) {
                        ("name" | "description" | "edition" | "license" | "version" | "rust-version", Value::Text(value)) =>
                            { package.insert(key, value); }
                        ("repository" | "readme", Value::Text(_)) | ("keywords" | "categories", Value::Strings(_))
                            | ("autotests", Value::Bool(_)) => {}
                        _ => return Err(format!("M1 {}: unknown package key or value: {key}", row.location)),
                    }
                }
            }
            ("features", false) => {
                for (name, (at, values)) in row.fields {
                    let Value::Strings(values) = values else { return Err(format!("{at}: expected feature array")); };
                    for dep in values.iter().filter_map(|v| v.split_once('/').map(|(d, _)| d)) {
                        if !spec.dependencies.iter().any(|k| k == dep) || matches!(table.dependencies[dep].target, DepTarget::Drop) {
                            return Err(format!("{at}: unknown forwarded dependency {dep}"));
                        }
                    }
                    writeln!(features, "{name} = {values:?}").unwrap();
                }
            }
            ("dependencies" | "dev-dependencies", false) => {
                let dev = row.name == "dev-dependencies";
                let output = dep_lines(&row, section("workspace.dependencies"), spec, table, dev)?;
                if !dev { deps = output; }
            }
            ("lints", false) => {
                if !row.boolean("workspace")? { return Err(format!("{}: expected workspace lints", row.location)); }
                row.finish()?;
            }
            ("test", true) => { row.string("name")?; row.string("path")?; row.finish()?; }
            _ => return Err(format!("M1 {}: unknown section {}", row.location, row.name)),
        }
    }
    if seen != spec.sections.iter().cloned().collect() {
        return Err(format!("M1 {source}: expected sections {:?}, found {seen:?}", spec.sections));
    }
    for key in ["name", "edition", "version", "rust-version", "license"] {
        if !package.contains_key(key) { return Err(format!("M1 {source}: missing package.{key}")); }
    }
    if Path::new(&spec.upstream).file_name().and_then(|v| v.to_str()) != Some(&package["name"]) {
        return Err(format!("M1 {source}: unexpected package name {}", package["name"]));
    }
    if !table.inventory.licenses.contains(&package["license"]) {
        return Err(format!("C3 {source}: unexpected license {}", package["license"]));
    }
    for path in &table.inventory.license_files {
        if !tree.contains_key(path) { return Err(format!("C3: missing {path}")); }
    }
    let mut out = format!("# Generated by makepad-solidsync; do not edit.\n[package]\nname = {:?}\n", spec.package);
    for key in ["version", "edition", "rust-version", "license", "description"] {
        if let Some(value) = package.get(key) { writeln!(out, "{key} = {value:?}").unwrap(); }
    }
    out.push_str("publish = false\n\n[lib]\ntest = true\ndoctest = false\n");
    if seen.contains("dependencies") { write!(out, "\n[dependencies]\n{deps}").unwrap(); }
    if seen.contains("features") { write!(out, "\n[features]\n{features}").unwrap(); }
    writeln!(out, "\n[package.metadata.source]\npin = {pin:?}\npath = {:?}", spec.upstream).unwrap();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tests::sample_rules;
    use crate::table::DepMap;

    #[test]
    fn manifest_resolves_workspace_keys_and_refuses_unknown_ones() {
        let mut rules = sample_rules();
        let spec = &mut rules.crates[0];
        spec.dest = "upstream/kernel".into();
        spec.sections = ["package", "dependencies", "features"].map(String::from).into();
        spec.dependencies.push("dep".into());
        rules.dependencies.insert("dep".into(), DepMap {
            target: DepTarget::Shim { package: "shim-dep".into(), shim: "dep".into() }, path: None,
            version: "1".into(), default_features: false, features: vec!["a".into()],
        });
        let workspace = "[workspace.package]\nedition = \"2021\"\nlicense = \"Apache-2.0\"\nrust-version = \"1.85\"\n";
        let manifest = "[package]\nname = \"sample\"\nversion = \"1.0.1\"\nedition = \"2024\"\nlicense.workspace = true\n\
            rust-version.workspace = true\n[features]\np = [\"dep/p\"]\n[dependencies]\ndep = { version = \"1\", features = [\"a\"], default-features = false }";
        let mut tree = Tree::from([("Cargo.toml".into(), workspace.as_bytes().to_vec()),
            ("LICENSE".into(), Vec::new()), ("NOTICE".into(), Vec::new())]);
        for (a, b, expected) in [("edition = \"2024\"", "edition.workspace = true", "edition = \"2021\""),
            ("", "", "edition = \"2024\"")] {
            tree.insert("sample/Cargo.toml".into(), manifest.replace(a, b).into_bytes());
            let result = generate(&tree, &rules.crates[0], &rules, "pin").unwrap();
            assert!(result.contains(expected) && result.contains("rust-version = \"1.85\"") && result.contains("../../shims/dep"));
        }
        for (a, b) in [("version = \"1\",", "version = \"2\","), ("[\"a\"]", "[\"b\"]"),
            ("dep = {", "dep = { path = \"x\","), ("name = \"sample\"", "extra = true\nname = \"sample\"")] {
            tree.insert("sample/Cargo.toml".into(), manifest.replace(a, b).into_bytes());
            assert!(generate(&tree, &rules.crates[0], &rules, "pin").is_err(), "accepted {b}");
        }
    }
}
