use std::collections::BTreeMap;

pub type Tree = BTreeMap<String, Vec<u8>>;

pub enum Expect {
    Exactly(usize),
    AtLeast(usize),
}

impl Expect {
    pub fn check(&self, id: &str, count: usize) -> Result<(), String> {
        match *self {
            Self::Exactly(n) if n != count => Err(format!("{id}: expected exactly {n}, found {count}")),
            Self::AtLeast(n) if n > count => Err(format!("{id}: expected at least {n}, found {count}")),
            _ => Ok(()),
        }
    }
}

pub struct CrateSpec {
    pub upstream: &'static str,
    pub dest: &'static str,
    pub package: &'static str,
    pub keep: &'static [&'static str],
    pub slice: u8,
}

pub enum RuleKind {
    StripCfgTest,
    ModuleGraph,
    DropModule { file: &'static str, line: &'static str },
    RemoveDep { name: &'static str },
    Literal { file: &'static str, find: &'static str, replace: &'static str },
}

pub struct Rule {
    pub id: &'static str,
    pub crate_: &'static str,
    pub kind: RuleKind,
    pub expect: Expect,
}

pub struct DepMap {
    pub upstream: &'static str,
    pub version: &'static str,
    pub features: &'static [&'static str],
    pub default_features: bool,
    pub line: &'static str,
}

pub struct Outcome {
    pub rule: &'static str,
    pub crate_: &'static str,
    pub count: usize,
    pub files: Vec<String>,
}

pub const KERNEL: &str = "solidcore/crates/solidcore";
pub(crate) const GENERATED_CONSUMER: &str = "solidcore/crates/solidcore/src/kernel.rs";
pub(crate) const GENERATED_INCLUDE: &[&str] = &[
    "include", "!", "(", "concat", "!", "(", "env", "!", "(", "\"OUT_DIR\"", ")",
    ",", "\"/upstream_fixes.rs\"", ")", ")",
];
pub const CRATES: &[CrateSpec] = &[CrateSpec {
    upstream: KERNEL,
    dest: "libs/csg/csg_brep",
    package: "makepad-csg-brep",
    keep: &["src/kernel/fixes/*.md"],
    slice: 1,
}];

pub const RULES: &[Rule] = &[
    Rule { id: "S1", crate_: KERNEL, kind: RuleKind::StripCfgTest, expect: Expect::AtLeast(1) },
    Rule { id: "S2", crate_: KERNEL, kind: RuleKind::ModuleGraph, expect: Expect::AtLeast(0) },
];

pub const DROP_NATIVE: Rule = Rule {
    id: "D1", crate_: "solidcore/crates/solidcore-record",
    kind: RuleKind::DropModule { file: "src/lib.rs", line: "pub mod native;" },
    expect: Expect::Exactly(1),
};

pub const ROOT_FILES: &[&str] = &[
    "solidcore/Cargo.toml", "solidcore/LICENSE", "solidcore/NOTICE", "solidcore/ATTRIBUTION.md",
];
const DENIED_NAMES: &[&str] = &["assets", "LICENSE-BRAND"];
const DENIED_DIRS: &[&str] = &["crates/look", "assets/branding", "assets/icons/app"];
const DENIED_EXTENSIONS: &[&str] = &["svg", "png", "ico", "icns", "ttf", "otf"];

pub(crate) fn denied(path: &str) -> bool {
    path.split('/').any(|p| DENIED_NAMES.contains(&p))
        || std::path::Path::new(path).ancestors().any(|p| DENIED_DIRS.iter().any(|d| p.ends_with(d)))
        || path.rsplit_once('.').is_some_and(|(_, ext)| {
            DENIED_EXTENSIONS.iter().any(|x| ext.eq_ignore_ascii_case(x))
        })
}

pub(crate) fn copy_set() -> Vec<String> {
    let mut paths: Vec<_> = ROOT_FILES.iter().map(|p| p.to_string()).collect();
    for spec in CRATES {
        paths.push(format!("{}/src/", spec.upstream));
        paths.push(format!("{}/Cargo.toml", spec.upstream));
        if spec.upstream == KERNEL {
            paths.push(format!("{}/build.rs", spec.upstream));
        }
    }
    paths
}
