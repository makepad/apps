use crate::config;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub type Tree = BTreeMap<String, Vec<u8>>;

pub fn dest_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub struct CrateSpec {
    pub upstream: String,
    pub dest: String,
    pub package: String,
    pub report_sites: Vec<String>,
    pub rust_roots: Vec<String>,
    pub copy: Vec<String>,
    pub keep: Vec<String>,
    pub sections: Vec<String>,
    pub dev_dependencies: Vec<String>,
    pub dependencies: Vec<String>,
}

pub enum DepTarget {
    Shim { package: String, shim: String },
    Sibling(String),
    Drop,
}

pub struct DepMap {
    pub target: DepTarget,
    pub path: Option<String>,
    pub version: String,
    pub features: Vec<String>,
    pub default_features: bool,
}

pub struct NamedRemoval {
    pub upstream: String,
    pub item: String,
    pub uses: String,
    pub reason: String,
    pub expected: usize,
}

pub struct Orphan {
    pub upstream: String,
    pub item: String,
    pub name: Option<String>,
    pub production: bool,
}

pub struct DropModule {
    pub upstream: String,
    pub file: String,
    pub line: String,
    pub expected: usize,
}

pub struct Outcome {
    pub rule: &'static str,
    pub crate_: String,
    pub count: usize,
    pub files: Vec<String>,
}

pub struct Expected {
    pub a1_per_crate: usize,
    pub c1_each_path_min: usize,
    pub c2_denied: usize,
    pub s1_min: usize,
    pub s2_min: usize,
    pub s3_each_pattern_min: usize,
}

pub struct Deny {
    pub names: Vec<String>,
    pub directories: Vec<String>,
    pub extensions: Vec<String>,
}

impl Deny {
    pub fn matches(&self, path: &str) -> bool {
        path.split('/').any(|p| self.names.iter().any(|s| s == p))
            || Path::new(path).ancestors().any(|p| self.directories.iter().any(|d| p.ends_with(d)))
            || path.rsplit_once('.').is_some_and(|(_, ext)| {
                self.extensions.iter().any(|x| ext.eq_ignore_ascii_case(x))
            })
    }
}

pub struct Generated {
    pub inputs: String,
    pub hash: String,
    pub producer: String,
    pub consumer: String,
    pub output: String,
}

pub struct InventorySpec {
    pub licenses: Vec<String>,
    pub license_files: Vec<String>,
    pub serde_shim: Vec<String>,
    pub i1: Vec<String>,
    pub i2: Vec<String>,
}

pub struct Table {
    pub attribution_section: String,
    pub inventory: InventorySpec,
    pub workspace_manifest: String,
    pub root_files: Vec<String>,
    pub shim_root: String,
    pub crates: Vec<CrateSpec>,
    pub dependencies: BTreeMap<String, DepMap>,
    pub removals: Vec<NamedRemoval>,
    pub orphans: Vec<Orphan>,
    pub drops: Vec<DropModule>,
    pub deny: Deny,
    pub expect: Expected,
    pub build: Generated,
}

fn relative(path: &str, at: &str) -> Result<(), String> {
    if path.is_empty() || path.contains(['\\', ':']) || path.bytes().any(|b| b.is_ascii_control())
        || !Path::new(path).components().all(|p| matches!(p, Component::Normal(_)))
    {
        return Err(format!("{at}: expected a relative path: {path:?}"));
    }
    Ok(())
}

impl Table {
    pub fn load() -> Result<Self, String> {
        let path = dest_root().join("sync/rules.toml");
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&path.display().to_string(), &source)
    }

    pub fn parse(path: &str, src: &str) -> Result<Self, String> {
        let (mut workspace, mut deny, mut expect, mut build) = (None, None, None, None);
        let mut inventory = None;
        let mut crates = Vec::new();
        let mut dependencies = BTreeMap::new();
        let mut removals = Vec::new();
        let mut orphans = Vec::new();
        let mut drops = Vec::new();
        let mut upstreams = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let mut packages = BTreeSet::new();
        for mut row in config::parse(path, src)? {
            let at = row.location.clone();
            match (row.name.as_str(), row.array) {
                ("workspace", false) => {
                    let manifest = row.string("manifest")?;
                    let root_files = row.strings("root_files")?;
                    let shim_root = row.string("shim_root")?;
                    let attribution = row.string("attribution_section")?;
                    relative(&attribution, &at)?;
                    relative(&shim_root, &at)?;
                    for file in &root_files { relative(file, &at)?; }
                    if !root_files.contains(&manifest) { return Err(format!("{at}: root_files must include {manifest}")); }
                    workspace = Some((manifest, root_files, shim_root, attribution));
                }
                ("deny", false) => deny = Some(Deny {
                    names: row.strings("names")?, directories: row.strings("directories")?,
                    extensions: row.strings("extensions")?,
                }),
                ("expect", false) => {
                    let counts = Expected {
                        a1_per_crate: row.count("a1_per_crate")?,
                        c1_each_path_min: row.count("c1_each_path_min")?, c2_denied: row.count("c2_denied")?,
                        s1_min: row.count("s1_min")?, s2_min: row.count("s2_min")?,
                        s3_each_pattern_min: row.count("s3_each_pattern_min")?,
                    };
                    if counts.a1_per_crate != 1 { return Err(format!("{at}: A1 requires exactly one section per crate")); }
                    if counts.c1_each_path_min == 0 || counts.c2_denied != 0 || counts.s3_each_pattern_min == 0 {
                        return Err(format!("{at}: C1/S3 require a match, C2 requires zero denied paths"));
                    }
                    expect = Some(counts);
                }
                ("inventory", false) => inventory = Some(InventorySpec {
                    licenses: row.strings("licenses")?, license_files: row.strings("license_files")?,
                    serde_shim: row.strings("serde_shim")?, i1: row.strings("i1")?, i2: row.strings("i2")?,
                }),
                ("build", false) => {
                    let record = Generated {
                        inputs: row.string("inputs")?, hash: row.string("sha256")?,
                        producer: row.string("producer")?, consumer: row.string("consumer")?, output: row.string("output")?,
                    };
                    for file in [&record.producer, &record.consumer, &record.output] { relative(file, &at)?; }
                    build = Some(record);
                }
                ("crate", true) => {
                    let spec = CrateSpec {
                        upstream: row.string("upstream")?, dest: row.string("dest")?, package: row.string("package")?,
                        sections: row.strings("sections")?, dev_dependencies: row.strings("dev_dependencies")?,
                        report_sites: row.strings("report_sites")?,
                        rust_roots: row.strings("rust_roots")?,
                        copy: row.strings("copy")?, keep: row.strings("keep")?, dependencies: row.strings("dependencies")?,
                    };
                    relative(&spec.upstream, &at)?;
                    relative(&spec.dest, &at)?;
                    if spec.rust_roots.is_empty() { return Err(format!("{at}: missing Rust source roots")); }
                    for root in &spec.rust_roots {
                        relative(root, &at)?;
                        if !root.ends_with('/') || !spec.copy.contains(root) {
                            return Err(format!("{at}: Rust root {root} must be a copied directory"));
                        }
                    }
                    for file in spec.copy.iter().chain(&spec.keep) { relative(file, &at)?; }
                    if !upstreams.insert(spec.upstream.clone()) || !destinations.insert(spec.dest.clone())
                        || !packages.insert(spec.package.clone())
                    { return Err(format!("{at}: duplicate crate path or package in {}", spec.upstream)); }
                    if spec.copy.is_empty() { return Err(format!("{at}: empty crate copy set")); }
                    crates.push(spec);
                }
                ("dependency", true) => {
                    let name = row.string("name")?;
                    let version = row.string("version")?;
                    let path = if row.fields.contains_key("path") { Some(row.string("path")?) } else { None };
                    let target = if row.fields.contains_key("shim") {
                        let shim = row.string("shim")?;
                        relative(&shim, &at)?;
                        DepTarget::Shim { package: row.string("package")?, shim }
                    } else if row.fields.contains_key("sibling") {
                        DepTarget::Sibling(row.string("sibling")?)
                    } else if row.boolean("drop")? {
                        DepTarget::Drop
                    } else { return Err(format!("{at}: missing dependency target")); };
                    let mapping = DepMap { target, version, path,
                        features: row.strings("features")?, default_features: row.boolean("default_features")? };
                    if dependencies.insert(name.clone(), mapping).is_some() { return Err(format!("{at}: duplicate dependency {name}")); }
                }
                ("remove", true) => {
                    let rule = NamedRemoval { upstream: row.string("upstream")?, item: row.string("item")?,
                        uses: row.string("uses")?, reason: row.string("reason")?, expected: row.count("expected")? };
                    if rule.expected != 1 || rule.reason.trim().is_empty() || rule.uses.trim().is_empty() {
                        return Err(format!("{at}: named removal requires uses, a reason and expected = 1"));
                    }
                    removals.push(rule);
                }
                ("orphan", true) => {
                    let upstream = row.string("upstream")?;
                    let item = row.string("item")?;
                    let name = if row.fields.contains_key("name") { Some(row.string("name")?) } else { None };
                    let production = row.fields.contains_key("scope");
                    if production && row.string("scope")? != "production" {
                        return Err(format!("{at}: O1 {item}: unknown scope"));
                    }
                    if row.count("expected")? != 1 || row.string("reason")?.trim().is_empty() {
                        return Err(format!("{at}: O1 {item}: expected = 1 and a reason required"));
                    }
                    orphans.push(Orphan { upstream, item, name, production });
                }
                ("drop_module", true) => drops.push(DropModule {
                    upstream: row.string("upstream")?, file: row.string("file")?,
                    line: row.string("line")?, expected: row.count("expected")?,
                }),
                _ => return Err(format!("{at}: unknown table or wrong table type: {}", row.name)),
            }
            row.finish()?;
        }
        let missing = |key: &str| format!("{path}:1: missing [{key}]");
        let (workspace_manifest, root_files, shim_root, attribution_section) = workspace.ok_or_else(|| missing("workspace"))?;
        if crates.is_empty() { return Err(missing("[crate]")); }
        for spec in &crates {
            for name in spec.dependencies.iter().chain(&spec.dev_dependencies) {
                if !dependencies.contains_key(name) { return Err(format!("{path}:1: {}: no dependency map for {name}", spec.upstream)); }
            }
        }
        for rule in &orphans {
            if !upstreams.contains(&rule.upstream) { return Err(format!("{path}: O1 {}: unknown crate {}", rule.item, rule.upstream)); }
        }
        Ok(Self { attribution_section, inventory: inventory.ok_or_else(|| missing("inventory"))?, workspace_manifest, root_files, shim_root, dependencies, crates, drops, removals, orphans,
            expect: expect.ok_or_else(|| missing("expect"))?, deny: deny.ok_or_else(|| missing("deny"))?,
            build: build.ok_or_else(|| missing("build"))? })
    }

    pub fn copy_set(&self) -> Vec<String> {
        let mut paths = self.root_files.clone();
        for spec in &self.crates {
            paths.extend(spec.copy.iter().map(|file| format!("{}/{file}", spec.upstream)));
        }
        paths
    }
}

pub(crate) fn at_least(rule: &str, minimum: usize, found: usize) -> Result<(), String> {
    if found < minimum { Err(format!("{rule}: expected at least {minimum}, found {found}")) } else { Ok(()) }
}
