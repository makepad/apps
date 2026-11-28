use crate::config::{self, Value};
use crate::files::{self, Scratch};
use crate::install;
use crate::table::{Table, Tree};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub type Counts = BTreeMap<String, usize>;
pub struct Pin {
    pub commit: String,
    pub date: String,
    pub subject: String,
    pub tool: String,
    pub inventory: Counts,
}
impl Pin {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        let mut rows = config::parse("pin.toml", &format!("[pin]\n{text}"))?.into_iter();
        let mut meta = rows.next().ok_or("missing pin")?;
        let mut pin = Self { commit: meta.string("commit")?, date: meta.string("date")?,
            subject: meta.string("subject")?, tool: meta.string("tool")?, inventory: Counts::new() };
        meta.finish()?;
        if pin.commit.len() != 40 || !pin.commit.bytes().all(|b| b.is_ascii_hexdigit()) { return Err("invalid installed pin".into()); }
        let mut inv = rows.next().filter(|r| r.name == "inventory" && !r.array).ok_or("missing pin inventory")?;
        for key in inv.fields.keys().cloned().collect::<Vec<_>>() {
            let value = inv.count(&key)?;
            pin.inventory.insert(key, value);
        }
        if rows.next().is_some() { return Err("unexpected table in pin.toml".into()); }
        Ok(pin)
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut s = format!("commit = {:?}\ndate = {:?}\nsubject = {:?}\ntool = {:?}\n\n[inventory]\n",
            self.commit, self.date, self.subject, self.tool);
        for (k,v) in &self.inventory { writeln!(s, "{k:?} = {v}").unwrap(); }
        Self::read(s.as_bytes())?;
        Ok(s.into_bytes())
    }
}

pub struct Report {
    pub rules: Counts,
    pub inventory: Counts,
    pub applied: Vec<String>,
    pub refreshed: Vec<String>,
}

#[derive(Default)]
struct Stats { add: usize, remove: usize, change: usize, plus: usize, minus: usize }

fn line_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count() + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}

// Give git the two directory prefixes that its unified diff will print.
// Feed exactly those bytes to the repository's scanner, including metadata.
fn scan(dir: &Path, dest: &Path, before: &Tree, after: &Tree, table: &Table) -> Result<Vec<Stats>, String> {
    let script = install::scanner(dest)?;
    let tmp = Scratch::new(dir, "scan")?;
    let mut stats: Vec<_> = table.crates.iter().map(|_| Stats::default()).collect();
    files::directory(&tmp.0.join("a"))?;
    files::directory(&tmp.0.join("b"))?;
    for path in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        let (old, new) = (before.get(path), after.get(path));
        if old == new { continue; }
        // The scanner consumes textual hunks. Refuse a blob git would silently
        // summarize as binary, and paths it would C-quote in its diff header.
        if path.contains('"') || old.into_iter().chain(new).any(|v| v.contains(&0)) {
            return Err(format!("private scan: cannot represent {path} in a text diff"));
        }
        if let Some(bytes) = old { files::write(&tmp.0, &format!("a/fab/brep/{path}"), bytes)?; }
        if let Some(bytes) = new { files::write(&tmp.0, &format!("b/fab/brep/{path}"), bytes)?; }
        if let Some(i) = table.crates.iter().position(|c| path.starts_with(&format!("{}/", c.dest))) {
            let s = &mut stats[i];
            match (old, new) {
                (Some(_), Some(_)) => s.change += 1,
                (Some(_), _) => s.remove += 1,
                (_, Some(_)) => s.add += 1,
                _ => unreachable!(),
            }
        }
    }
    let diff = tmp.0.join("diff");
    let output = fs::File::create_new(&diff).map_err(|e| files::error(&diff, e))?;
    let git = Command::new("git").arg("-C").arg(tmp.0.canonicalize().map_err(|e| e.to_string())?)
        .env("GIT_CEILING_DIRECTORIES", tmp.0.parent().unwrap().canonicalize().map_err(|e| e.to_string())?)
        .env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "core.quotePath=false", "diff", "--no-index", "--no-prefix", "--no-color", "--no-ext-diff",
            "--no-textconv", "--no-renames", "--text", "--unified=0", "--inter-hunk-context=0", "--", "a", "b"])
        .stdout(Stdio::from(output)).stderr(Stdio::piped()).spawn().and_then(|p| p.wait_with_output()).map_err(|e| e.to_string())?;
    if !matches!(git.status.code(), Some(0 | 1)) || !git.stderr.is_empty() {
        return Err(format!("private scan diff: {}", String::from_utf8_lossy(&git.stderr)));
    }
    let bytes = fs::read(&diff).map_err(|e| files::error(&diff, e))?;
    private_scan(&script, &tmp.0, &bytes)?;
    // U0 hunk ranges are exact added/removed counts. Skip the whole body by
    // those counts, so source lines resembling diff headers remain data.
    let mut owner = None;
    let mut remaining = 0;
    let mut plus_total = 0;
    let mut minus_total = 0;
    for line in bytes.split(|&b| b == b'\n') {
        if line.starts_with(b"\\") { continue; }
        if remaining > 0 { remaining -= 1; continue; }
        if line.starts_with(b"diff --git ") { owner = None; }
        else if let Some(path) = line.strip_prefix(b"+++ b/fab/brep/").or_else(|| line.strip_prefix(b"--- a/fab/brep/")) {
            owner = table.crates.iter().position(|c| path.starts_with(format!("{}/", c.dest).as_bytes()));
        } else if line.starts_with(b"@@ ") {
            let mut words = std::str::from_utf8(line.splitn(2, |&b| b == b'\t').next().unwrap())
                .map_err(|e| e.to_string())?.split_whitespace().skip(1);
            let mut count = || -> Result<usize, String> {
                let range = words.next().ok_or("missing diff range")?;
                range.split_once(',').map_or(Ok(1), |(_, n)| n.parse::<usize>().map_err(|e| e.to_string()))
            };
            let minus = count()?;
            let plus = count()?;
            remaining = plus + minus;
            if let Some(i) = owner {
                stats[i].minus += minus;
                stats[i].plus += plus;
                plus_total += plus;
                minus_total += minus;
            }
        }
    }
    let lines = |tree: &Tree| tree.iter().filter(|(p, _)| table.crates.iter().any(|c| p.starts_with(&format!("{}/", c.dest))))
        .map(|(_, b)| line_count(b)).sum::<usize>();
    if remaining != 0 || lines(before) + plus_total != lines(after) + minus_total {
        return Err("summary: diff hunk counts disagree with crate line counts".into());
    }
    Ok(stats)
}

// The repository scanner is sequential. Each file's unified diff is
// self-contained, so independent invocations can share the first import.
// Split only between files and give every byte to exactly one scanner.
fn private_scan(script: &Path, dir: &Path, diff: &[u8]) -> Result<(), String> {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let budget = diff.len().div_ceil(cpus);
    let mut cuts = vec![0usize];
    let mut offset = 0;
    for line in diff.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"diff --git ") && offset > 0 && offset - cuts.last().unwrap() >= budget { cuts.push(offset); }
        offset += line.len();
    }
    cuts.push(diff.len());
    let mut jobs = Vec::new();
    let mut errors = Vec::new();
    for (i, range) in cuts.windows(2).enumerate() {
        let name = format!("scan-{i}");
        let start = || -> Result<_, String> {
            files::write(dir, &name, &diff[range[0]..range[1]])?;
            let input = fs::File::open(dir.join(&name)).map_err(|e| e.to_string())?;
            Command::new("sh").arg(script).arg("diff").stdin(Stdio::from(input))
                .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| files::error(script, e))
        };
        match start() {
            Ok(p) => jobs.push(p),
            Err(e) => { errors.push(e); break; }
        }
    }
    for job in jobs {
        match job.wait_with_output() {
            Ok(out) if out.status.success() && out.stderr.is_empty() => (),
            Ok(out) => errors.push(format!("{}: {}{}", out.status, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))),
            Err(e) => errors.push(e.to_string()),
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(format!("private scan failed:\n{}", errors.join("\n"))) }
}

fn toml(value: Option<&Value>) -> String {
    match value {
        None => "(absent)".into(),
        Some(Value::Text(s)) => format!("{s:?}"),
        Some(Value::Strings(ss)) => format!("{ss:?}"),
        Some(Value::Integer(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Inline(kv)) => {
            let fields = kv.iter().map(|(k,v)| format!("{k} = {}", toml(Some(v)))).collect::<Vec<_>>().join(", ");
            format!("{{ {fields} }}")
        }
    }
}

fn manifest_fields(tree: &Tree, path: &str) -> Result<BTreeMap<String, Value>, String> {
    let mut fields = BTreeMap::new();
    let Some(data) = tree.get(path) else { return Ok(fields); };
    for row in config::document(path, std::str::from_utf8(data).map_err(|e| e.to_string())?, true)? {
        for (name, (_, val)) in row.fields {
            if row.name == "features" || row.name == "dependencies" || row.name == "package" && name == "edition" {
                fields.insert(format!("{}.{name}", row.name), val);
            }
        }
    }
    Ok(fields)
}

pub fn summarize(dir: &Path, dest: &Path, table: &Table, before: &Tree, after: &Tree,
    previous: Option<&Pin>, pin: &Pin, report: &Report) -> Result<String, String> {
    let stats = scan(dir, dest, before, after, table)?;
    let mut summary = format!("sync {} {} {}\n", pin.commit, pin.date, pin.subject);
    for (c, s) in table.crates.iter().zip(stats) {
        writeln!(summary, "{}: files +{} -{} changed {}, lines +{} -{}", c.dest, s.add, s.remove, s.change, s.plus, s.minus).unwrap();
        if table.build.producer.starts_with(&format!("{}/", c.upstream)) {
            let (dir, suffix) = table.build.inputs.rsplit_once("/*").ok_or("invalid B1 input pattern")?;
            let prefix = format!("{}/{dir}/", c.dest);
            let flags = |t: &Tree| t.keys().filter_map(|p| p.strip_prefix(&prefix)?.strip_suffix(suffix))
                .filter(|s| !s.contains('/')).map(str::to_owned).collect::<BTreeSet<_>>();
            let (a, b) = (flags(before), flags(after));
            writeln!(summary, "  UpstreamFixes new: {}", names_or_none(b.difference(&a))).unwrap();
            writeln!(summary, "  UpstreamFixes retired: {}", names_or_none(a.difference(&b))).unwrap();
        }
        let path = format!("{}/Cargo.toml", c.dest);
        let (a, b) = (manifest_fields(before, &path)?, manifest_fields(after, &path)?);
        let mut n = 0;
        for k in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
            if a.get(k) != b.get(k) {
                writeln!(summary, "  Cargo.toml {k}: {} -> {}", toml(a.get(k)), toml(b.get(k))).unwrap(); n += 1;
            }
        }
        if n == 0 { summary.push_str("  Cargo.toml dependencies, features and edition unchanged\n"); }
    }
    for (rule, count) in &report.rules { writeln!(summary, "{rule}: {count}").unwrap(); }
    writeln!(summary, "patches applied: {}\npatches refreshed: {}", names_or_none(report.applied.iter()), names_or_none(report.refreshed.iter())).unwrap();
    let empty = Counts::new();
    let old = previous.map_or(&empty, |p| &p.inventory);
    let mut n = 0;
    for k in old.keys().chain(report.inventory.keys()).collect::<BTreeSet<_>>() {
        let (a,b) = (old.get(k).copied().unwrap_or(0), report.inventory.get(k).copied().unwrap_or(0));
        if a != b { writeln!(summary, "{k}: {a} -> {b}").unwrap(); n += 1; }
    }
    if n == 0 { summary.push_str("I1 and I2 counts unchanged\n"); }
    summary.push_str("private scan: passed\n");
    Ok(summary)
}

fn names_or_none<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let list = names.cloned().collect::<Vec<_>>().join(", ");
    if list.is_empty() { "none".into() } else { list }
}
