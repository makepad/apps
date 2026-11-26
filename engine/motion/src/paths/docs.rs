//! Paths across documents: what an edit's panel binds. A path names the
//! document it is in first:
//! - `edit.<clip>.FIELD`: a field of a clip of the edit (`edit.p_open.span`,
//!   `edit.p_loss.span[1]`, `edit.p_song.gain`); `start` and `length` are
//!   the span's two numbers, and `cut` is the clip's start as an editor
//!   rolls it: the clip's start, its length the other way and the length of
//!   the clip ending there on its track, moved together (the frames after
//!   stay where they are); `end` is the roll of the cut after the clip;
//! - `<node>:PATH`: a path of the graphic a Graphic clip shows
//!   (`p_open:WORD_SPARKS.count`, `p_open:look.BONE`);
//! - `<module>.PATH`: a path of a module the graphics share, by the name
//!   they import it as (`look.POST.bloom`).
//!
//! A clip or node can be a set: `*` (every graphic, or every clip; for a
//! graphic path, every graphic where it resolves), `a + b` (named ones), or
//! the name of a set the edit writes (`let edit_sets = {lyric_plates:
//! "p_outro + p_loss"}`): `*:LYRIC_LOOK.font`, `lyric_plates:BONE`,
//! `edit.*.cut`. A set resolves to every member's site; reading it reads
//! the first.
//!
//! Nothing is evaluated: the texts are read, as [`super::resolve`] reads
//! one document.

use makepad_edit_values::ValueKind;
use super::{resolve_exact, File, Site};

/// A graphic an edit shows: its node, its document's file, the modules it
/// imports by the names it imports them as.
#[derive(Clone, Debug, Default)]
pub struct NodeDoc {
    pub node: String,
    pub file: usize,
    pub imports: Vec<(String, usize)>,
}

/// Which file is which document: indices into the files given.
#[derive(Clone, Debug, Default)]
pub struct DocTable {
    /// The edit itself.
    pub edit: Option<usize>,
    /// The edit's clips on their tracks: (clip, track, start, length), for
    /// `cut` and `end` (the clip ending where another starts). A track is
    /// one lane's: the host keeps sound apart from picture.
    pub clips: Vec<(String, u32, f64, f64)>,
    pub nodes: Vec<NodeDoc>,
    /// The modules the graphics share, by the name they import each as.
    pub modules: Vec<(String, usize)>,
    /// Named sets: name, selector (`"p_outro + p_loss"`, `"*"`).
    pub sets: Vec<(String, String)>,
    /// The song's beats in the edit's seconds (its timing module's, where
    /// the edit plays one): a clip's `cut`, `end` and `start` set from a
    /// panel land on the nearest ([`snap`]).
    pub beats: Vec<f64>,
    /// The edit's beat (seconds), when no song says where its beats are:
    /// times then move by whole beats (a cut on a beat stays on one).
    pub beat: Option<f64>,
}

/// Whether a value set at `path` lands on the edit's beats: a clip's
/// `cut`, `end` or `start`.
pub fn snaps(path: &str) -> bool {
    path.strip_prefix("edit.").and_then(|rest| rest.rsplit('.').next()).is_some_and(|f| f == "cut" || f == "end" || f == "start")
}

/// Where a value set at `path`, moved from `from` to `to`, is written: a
/// clip's time on the song's nearest beat (else by whole beats of the
/// edit's tempo), anything else as given.
pub fn snap(table: &DocTable, path: &str, from: f64, to: f64) -> f64 {
    if !snaps(path) {
        return to;
    }
    if !table.beats.is_empty() {
        let at = table.beats.partition_point(|b| *b < to);
        let near = [at.checked_sub(1), Some(at)].into_iter().flatten().filter_map(|k| table.beats.get(k)).min_by(|a, b| (*a - to).abs().total_cmp(&(*b - to).abs()));
        return near.copied().unwrap_or(to);
    }
    match table.beat.filter(|b| *b > 1e-3) {
        Some(beat) => from + ((to - from) / beat).round() * beat,
        None => to,
    }
}

/// One place a path reaches, and how a set moves it: a number set by `d`
/// from where the gesture began moves this one by `scale * d`.
#[derive(Clone, Debug, PartialEq)]
pub struct Bound {
    pub site: Site,
    pub scale: f64,
    /// The member it belongs to (a clip, a node, a module), for showing.
    pub member: String,
}

/// Whether `path` is a set (it can reach more than one place).
pub fn is_set(table: &DocTable, path: &str) -> bool {
    let sel = match path.split_once(':') {
        Some((sel, _)) => sel,
        None => match path.strip_prefix("edit.") {
            Some(rest) => {
                if rest.split('.').nth(1).is_some_and(|f| f == "cut" || f == "end") {
                    return true;
                }
                rest.split('.').next().unwrap_or("")
            }
            None => return false,
        },
    };
    let sel = sel.trim();
    sel == "*" || sel.contains('+') || table.sets.iter().any(|(n, _)| n == sel)
}

/// The members a selector names (`*` is `all`), or why it names none.
fn members(table: &DocTable, sel: &str, all: &[String]) -> Result<Vec<String>, String> {
    let sel = sel.trim();
    if sel == "*" {
        return Ok(all.to_vec());
    }
    let sel = table.sets.iter().find(|(n, _)| n == sel).map_or(sel, |(_, s)| s.as_str());
    if sel.trim() == "*" {
        return Ok(all.to_vec());
    }
    let mut out = Vec::new();
    for name in sel.split('+').map(str::trim).filter(|n| !n.is_empty()) {
        if !all.iter().any(|a| a == name) {
            return Err(format!("no `{name}` here (there are: {})", all.iter().take(8).cloned().collect::<Vec<_>>().join(", ")));
        }
        if !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        return Err(format!("`{sel}` names nothing"));
    }
    Ok(out)
}

/// A graphic's files as its own paths read them: its document, then what
/// it imports by those names; with each file's index in `files`.
fn node_view<'a>(files: &[File<'a>], node: &'a NodeDoc) -> (Vec<File<'a>>, Vec<usize>) {
    let mut view = vec![File { name: "", text: files[node.file].text, code: files[node.file].code }];
    let mut index = vec![node.file];
    for (name, k) in &node.imports {
        view.push(File { name, text: files[*k].text, code: files[*k].code });
        index.push(*k);
    }
    (view, index)
}

/// Every place `path` reaches (one, unless it is a set), or why none.
pub fn resolve<'a>(files: &[File<'a>], table: &'a DocTable, path: &str) -> Result<Vec<Bound>, String> {
    let path = path.trim();
    // A graphic's path.
    if let Some((sel, inner)) = path.split_once(':') {
        let all: Vec<String> = table.nodes.iter().map(|n| n.node.clone()).collect();
        let wide = sel.trim() == "*";
        let mut out: Vec<Bound> = Vec::new();
        let mut first_err = None;
        for member in members(table, sel, &all)? {
            let node = table.nodes.iter().find(|n| n.node == member).expect("a member is a node");
            let (view, index) = node_view(files, node);
            match resolve_exact(&view, inner) {
                Ok(mut site) => {
                    site.file = index[site.file];
                    // A shared module's place is one place, however many reach it.
                    if !out.iter().any(|b| b.site.file == site.file && b.site.range.start == site.range.start) {
                        out.push(Bound { site, scale: 1.0, member });
                    }
                }
                Err(why) if wide => {
                    first_err.get_or_insert(why);
                }
                Err(why) => return Err(format!("{member}: {why}")),
            }
        }
        if out.is_empty() {
            return Err(first_err.unwrap_or_else(|| format!("`{path}` reaches nothing")));
        }
        return Ok(out);
    }
    // The edit's clips.
    if let Some(rest) = path.strip_prefix("edit.") {
        let edit = table.edit.ok_or("there is no edit here")?;
        let (sel, field) = rest.split_once('.').ok_or_else(|| format!("`{path}`: a clip's field is `edit.<clip>.<field>`"))?;
        let all: Vec<String> = table.clips.iter().map(|c| c.0.clone()).collect();
        let view = [File { name: "", text: files[edit].text, code: files[edit].code }];
        let at = |clip: &str, field: &str| -> Result<Site, String> {
            let mut site = resolve_exact(&view, &format!("{clip}.{field}"))?;
            site.file = edit;
            Ok(site)
        };
        let mut out = Vec::new();
        for clip in members(table, sel, &all)? {
            match field {
                "start" => out.push(Bound { site: at(&clip, "span[0]")?, scale: 1.0, member: clip }),
                "length" => out.push(Bound { site: at(&clip, "span[1]")?, scale: 1.0, member: clip }),
                "cut" | "end" => {
                    // A roll: the clip after the cut starts later and ends
                    // where it did, the clip before it ends later with it.
                    // Every place of one cut is its member's (the first, the
                    // start: where the cut is).
                    let &(_, track, start, length) = table.clips.iter().find(|c| c.0 == clip).expect("a member is a clip");
                    let (after, before) = if field == "cut" {
                        (Some(clip.clone()), table.clips.iter().find(|c| c.0 != clip && c.1 == track && (c.2 + c.3 - start).abs() < 1e-3).map(|c| c.0.clone()))
                    } else {
                        (table.clips.iter().find(|c| c.0 != clip && c.1 == track && (c.2 - (start + length)).abs() < 1e-3).map(|c| c.0.clone()), Some(clip.clone()))
                    };
                    let (Some(after), Some(before)) = (after, before) else {
                        // No cut there (the film's start, its end, a gap):
                        // a set leaves it out.
                        if members(table, sel, &all)?.len() > 1 {
                            continue;
                        }
                        return Err(format!("`{path}`: no clip meets `{clip}` there on its track"));
                    };
                    out.push(Bound { site: at(&after, "span[0]")?, scale: 1.0, member: clip.clone() });
                    out.push(Bound { site: at(&after, "span[1]")?, scale: -1.0, member: clip.clone() });
                    out.push(Bound { site: at(&before, "span[1]")?, scale: 1.0, member: clip.clone() });
                }
                field => out.push(Bound { site: at(&clip, field)?, scale: 1.0, member: clip }),
            }
        }
        return Ok(out);
    }
    // A shared module's.
    let (head, inner) = path.split_once('.').ok_or_else(|| missing_doc(table, path))?;
    let Some((_, file)) = table.modules.iter().find(|(name, _)| name == head) else { return Err(missing_doc(table, path)) };
    let view: Vec<File> = std::iter::once(File { name: "", text: files[*file].text, code: files[*file].code })
        .chain(table.modules.iter().filter(|(_, k)| k != file).map(|(name, k)| File { name, text: files[*k].text, code: files[*k].code }))
        .collect();
    let index: Vec<usize> = std::iter::once(*file).chain(table.modules.iter().filter(|(_, k)| k != file).map(|(_, k)| *k)).collect();
    let mut site = resolve_exact(&view, inner)?;
    site.file = index[site.file];
    Ok(vec![Bound { site, scale: 1.0, member: head.to_string() }])
}

fn missing_doc(table: &DocTable, path: &str) -> String {
    let modules: Vec<&str> = table.modules.iter().map(|(n, _)| n.as_str()).collect();
    format!("`{path}`: a path names its document first: `edit.<clip>.<field>`, `<node>:PATH` or `<module>.PATH` (modules: {})", modules.join(", "))
}

/// A path of the edit level worth binding, with what it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct DocEntry {
    pub path: String,
    pub kind: ValueKind,
    pub value: String,
    pub label: Option<String>,
    /// How many places it reaches (a set's members).
    pub reach: usize,
}

/// What an edit's panel can bind: the clips' times (and each clip's
/// `cut`), the shared modules' paths, and the paths most graphics have in
/// common as `*:PATH` (with how many have it). A graphic's own paths are
/// its own panel's; `scene` lists the names of each.
pub fn list(files: &[File], table: &DocTable) -> Vec<DocEntry> {
    let mut out = Vec::new();
    if table.edit.is_some() {
        for (clip, _, _, _) in &table.clips {
            for field in ["span", "cut", "in", "gain", "fit"] {
                if let Ok(b) = resolve(files, table, &format!("edit.{clip}.{field}")) {
                    out.push(DocEntry { path: format!("edit.{clip}.{field}"), kind: b[0].site.kind, value: b[0].site.text.clone(), label: None, reach: b.len() });
                }
            }
        }
    }
    for (name, file) in &table.modules {
        let view = [File { name: "", text: files[*file].text, code: files[*file].code }];
        for e in super::list(&view) {
            out.push(DocEntry { path: format!("{name}.{}", e.path), kind: e.kind, value: e.value, label: e.label, reach: 1 });
        }
    }
    // The graphics' own top-level names that several share.
    let mut common: Vec<(String, ValueKind, String, Option<String>, usize)> = Vec::new();
    for node in &table.nodes {
        let view = [File { name: "", text: files[node.file].text, code: files[node.file].code }];
        let mut seen = std::collections::HashSet::new();
        for e in super::list(&view) {
            if !seen.insert(e.path.clone()) {
                continue;
            }
            match common.iter_mut().find(|c| c.0 == e.path) {
                Some(c) => c.4 += 1,
                None => common.push((e.path, e.kind, e.value, e.label, 1)),
            }
        }
    }
    for (path, kind, value, label, n) in common {
        if n >= 2 {
            out.push(DocEntry { path: format!("*:{path}"), kind, value, label, reach: n });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::code::outline;

    #[test]
    fn edit_level_paths_reach_their_documents() {
        let edit = "let a = Graphic{span: [0.0, 2.0] track: 0}\nlet b = Graphic{span: [2.0, 3.0] track: 0}\nFlow{kind: @edit a, b}\n";
        let ga = "let look = import(asset(\"m\", \"1\"))\nlet SPARKS = {count: 12}\nMotion{}\n";
        let gb = "let look = import(asset(\"m\", \"1\"))\nlet SPARKS = {count: 30}\nMotion{}\n";
        let look = "let BONE = #eee9df\nlet POST = {bloom: 0.5}\n";
        let texts = [edit, ga, gb, look];
        let codes: Vec<_> = texts.iter().map(|t| outline(t)).collect();
        let files: Vec<File> = texts.iter().zip(&codes).map(|(t, c)| File { name: "", text: t, code: c }).collect();
        let table = DocTable {
            edit: Some(0),
            clips: vec![("a".into(), 0, 0.0, 2.0), ("b".into(), 0, 2.0, 3.0)],
            nodes: vec![NodeDoc { node: "a".into(), file: 1, imports: vec![("look".into(), 3)] }, NodeDoc { node: "b".into(), file: 2, imports: vec![("look".into(), 3)] }],
            modules: vec![("look".into(), 3)],
            sets: vec![("both".into(), "a + b".into())],
            beats: Vec::new(),
            beat: Some(0.5),
        };
        assert_eq!((snap(&table, "edit.b.cut", 2.0, 2.3), snap(&table, "a:SPARKS.count", 2.0, 2.3)), (2.5, 2.3));
        let on_song = DocTable { beats: vec![1.9, 2.4, 2.9], ..table.clone() };
        assert_eq!(snap(&on_song, "edit.b.cut", 2.0, 2.3), 2.4);
        let one = resolve(&files, &table, "a:SPARKS.count").unwrap();
        assert_eq!((one.len(), one[0].site.file, one[0].site.text.as_str()), (1, 1, "12"));
        let set = resolve(&files, &table, "both:SPARKS.count").unwrap();
        assert_eq!(set.iter().map(|b| b.site.text.as_str()).collect::<Vec<_>>(), ["12", "30"]);
        // A module reached through every graphic is one place.
        assert_eq!(resolve(&files, &table, "*:look.BONE").unwrap().len(), 1);
        assert_eq!(resolve(&files, &table, "look.POST.bloom").unwrap()[0].site.file, 3);
        let cut = resolve(&files, &table, "edit.b.cut").unwrap();
        assert_eq!(cut.iter().map(|b| (b.member.as_str(), b.site.text.as_str(), b.scale)).collect::<Vec<_>>(), [("b", "2.0", 1.0), ("b", "3.0", -1.0), ("b", "2.0", 1.0)]);
        assert_eq!(resolve(&files, &table, "edit.a.end").unwrap().len(), 3);
        // The film's first clip has no cut: a set leaves it out.
        assert_eq!(resolve(&files, &table, "edit.*.cut").unwrap().len(), 3);
        assert!(resolve(&files, &table, "edit.a.cut").is_err());
        assert!(is_set(&table, "edit.b.cut") && is_set(&table, "*:BONE") && !is_set(&table, "a:BONE"));
        assert!(resolve(&files, &table, "BONE").is_err());
        assert!(list(&files, &table).iter().any(|e| e.path == "*:SPARKS.count" && e.reach == 2));
    }
}
