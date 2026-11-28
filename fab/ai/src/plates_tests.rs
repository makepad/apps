//! The plate store: a switch keeps every edit, a crash mid-autosave keeps every committed one,
//! only Save writes a plate's own file, a demo's first edit makes a plate, the lists of earlier
//! versions fold into plates with nothing moved or deleted, and a working copy changed outside
//! Fab is taken as it reads, or kept aside whole, with nothing of it pruned.

use crate::plates::{Current, Origin, PlateStore};
use crate::{autosave, Session};
use fab_model::{project, Change, FabModel};
use fab_plate::machine::Machine;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn args(text: &str) -> makepad_model::json::Value {
    makepad_model::json::parse(text.as_bytes()).unwrap()
}

fn root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fab-plates-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn home(root: &Path) -> PathBuf {
    root.join("Documents").join("Fab")
}

fn store(root: &Path) -> PlateStore {
    PlateStore::under(&root.join("fab"), &home(root))
}

fn session(root: &Path) -> Session {
    let mut s = Session::new(Machine::dual_nozzle_350(), home(root));
    s.store = Some(store(root));
    s
}

fn block(name: &str) -> FabModel {
    let source = format!("{}let block = m.box({{size: [10mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY);
    FabModel::with_script(name, &format!("{name}.cad.splash"), &source)
}

/// An edit the project shows: the plate's infill, committed (not yet autosaved).
fn edit(s: &mut Session, infill: &str) {
    let r = s.commit("infill", vec![Change::PlateSettings { set: vec![("sparse_infill_density".into(), infill.into())], reset: Vec::new() }]);
    assert!(r.ok, "{}", r.text);
}

/// An edit and the autosave the model worker runs after every job.
fn edit_saved(s: &mut Session, infill: &str) {
    edit(s, infill);
    s.autosave().unwrap();
}

fn infill(s: &Session) -> Option<String> {
    s.model.project().settings.iter().find(|(k, _)| k == "sparse_infill_density").map(|(_, v)| v.clone())
}

/// The file plate `id`'s working copy keeps script `name`'s text in, as its map names it.
fn script_file(root: &Path, id: &str, name: &str) -> PathBuf {
    let scripts = store(root).dir(id).join("scripts");
    let map = std::fs::read_to_string(scripts.join("map")).unwrap();
    let hash = map.lines().find_map(|l| l.strip_suffix(&format!("\tscripts/{name}"))).expect("the map names the script");
    scripts.join(format!("{hash}.cad.splash"))
}

fn files_under(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, out);
        } else {
            out.insert(path.clone(), std::fs::read(&path).unwrap());
        }
    }
}

/// Edit A, switch to B, edit B, back to A: both edits are there, the one made just before a
/// switch too (the switch autosaves first). A crash in the middle of an autosave (a half-written
/// map, content file and record) brings back the plate on the bed as last committed, and the
/// other plate as it was.
#[test]
fn switching_keeps_every_edit_and_a_crash_mid_autosave_loses_none() {
    let root = root("switch");
    let mut s = session(&root);
    s.replace_model(block("a"), None).unwrap();
    let a = s.plate.clone().expect("a new plate is on the list");
    edit_saved(&mut s, "11%");
    s.replace_model(block("b"), None).unwrap();
    let b = s.plate.clone().unwrap();
    assert_ne!(a, b);
    edit_saved(&mut s, "22%");
    s.switch(&a).unwrap();
    assert_eq!(infill(&s).as_deref(), Some("11%"), "A as it was left");
    // an edit and straight away a switch: the switch keeps it
    edit(&mut s, "13%");
    s.switch(&b).unwrap();
    assert_eq!(infill(&s).as_deref(), Some("22%"), "B as it was left");
    s.switch(&a).unwrap();
    assert_eq!(infill(&s).as_deref(), Some("13%"), "the edit made just before the switch");
    // back on the bed, its undo history is as it was left: 13% undoes to 11%, and redoes
    assert!(s.undo_redo(fab_model::HistoryAction::Undo).ok);
    assert_eq!(infill(&s).as_deref(), Some("11%"), "undo goes on after a switch back");
    assert!(s.undo_redo(fab_model::HistoryAction::Redo).ok);
    assert_eq!(infill(&s).as_deref(), Some("13%"));
    let listed: BTreeSet<String> = s.plates().into_iter().map(|p| p.id).collect();
    assert_eq!(listed, BTreeSet::from([a.clone(), b.clone()]));
    assert!(s.plates().iter().all(|p| p.modified && p.file.is_none()), "never saved, each has a *");

    // A dies in the middle of its next autosave.
    edit(&mut s, "14%");
    let dir = store(&root).dir(&a);
    std::fs::write(dir.join("scripts").join("map.tmp"), b"half a map").unwrap();
    std::fs::write(dir.join("scripts").join("00000000deadbeef.fab.splash"), b"half a project").unwrap();
    std::fs::write(dir.join("plate.tmp"), b"half a record").unwrap();
    drop(s);
    let mut t = session(&root);
    t.recover().expect("the bed comes back").unwrap();
    assert_eq!(t.plate.as_deref(), Some(a.as_str()));
    assert_eq!(infill(&t).as_deref(), Some("13%"), "A as last committed");
    t.switch(&b).unwrap();
    assert_eq!(infill(&t).as_deref(), Some("22%"));
    let _ = std::fs::remove_dir_all(&root);
}

/// A plate switched back to shows the build it was left with: nothing is built again until it is
/// edited, or until what builds it changed (here the field meshing). Only the most recent plates
/// keep their build; a demo looked at keeps its own the same way.
#[test]
fn a_plate_switched_back_to_is_not_built_again() {
    let root = root("rebuild");
    let mut s = session(&root);
    s.replace_model(block("a"), None).unwrap();
    let a = s.plate.clone().unwrap();
    s.replace_model(block("b"), None).unwrap();
    let b = s.plate.clone().unwrap();
    let built = s.counts().0;
    let shown = s.switch(&a).unwrap();
    assert_eq!(s.counts().0, built, "a back on the bed is not built again");
    assert!(std::sync::Arc::ptr_eq(&shown, s.last.as_ref().unwrap()), "it shows the build it was left with");
    edit(&mut s, "12%");
    let edited = s.counts().0;
    assert!(edited > built, "an edit builds it");
    s.switch(&b).unwrap();
    assert_eq!(s.counts().0, edited, "b back neither");
    s.switch(&a).unwrap();
    assert_eq!((s.counts().0, infill(&s).as_deref()), (edited, Some("12%")), "a back with its edit, built once for it");

    // the app maps the print colours of each plate on the bed: a switch back is not built again
    // for them, and shows them as they were last mapped for it, not as the plate before it had
    // them (the app sends this plate's mapping next, and only when it has moved)
    let mapped = |ink: [f32; 3]| std::sync::Arc::new(fab_model::eval::Mapped { inks: [("Red".to_string(), ink)].into_iter().collect(), tools: Default::default() });
    let a_colours = s.map_colours(mapped([0.1, 0.2, 0.3])).expect("a takes its mapping");
    assert_eq!((s.counts().0, &*a_colours.mapped), (edited, &*mapped([0.1, 0.2, 0.3])), "a mapping is not a build");
    s.switch(&b).unwrap();
    s.map_colours(mapped([0.4, 0.5, 0.6])).expect("b takes its mapping");
    let shown = s.switch(&a).unwrap();
    assert_eq!((s.counts().0, &*shown.mapped, &*s.eval.mapped), (edited, &*mapped([0.1, 0.2, 0.3]), &*mapped([0.1, 0.2, 0.3])), "a back is not built again, and shows its own colours");
    assert!(s.map_colours(mapped([0.1, 0.2, 0.3])).is_none(), "a's own mapping again is no work");
    let moved = s.map_colours(mapped([0.7, 0.8, 0.9])).expect("a mapping moved while a was away re-colours it");
    assert_eq!((s.counts().0, &*moved.mapped), (edited, &*mapped([0.7, 0.8, 0.9])), "and is not a build either");

    // the view's look builds nothing: a switch back is not built again for it
    assert!(s.set_mesh_fields(true).is_none(), "blocks show no differently in the Mesh look");
    s.switch(&b).unwrap();
    s.switch(&a).unwrap();
    assert_eq!(s.counts().0, edited, "b and a back are not built again for the Mesh look");

    // what builds the plates changed (the tolerance exact parts slice at): the one switched
    // back to is built again
    assert!(s.set_print_chord(0.02), "the tolerance changed");
    let changed = s.counts().0;
    s.switch(&b).unwrap();
    assert!(s.counts().0 > changed, "b is built again for the new tolerance");
    // a went aside built at the old tolerance (nothing built it again on the bed): back on the
    // bed it is built again too, not shown with print meshes of the old one
    let b_built = s.counts().0;
    s.switch(&a).unwrap();
    assert!(s.counts().0 > b_built, "a, built at the old tolerance, is built again for the new one");

    // two plates keep their build: the third most recent is built again
    s.replace_model(block("c"), None).unwrap();
    s.replace_model(block("d"), None).unwrap();
    s.replace_model(block("e"), None).unwrap();
    let before = s.counts().0;
    s.switch(&b).unwrap();
    assert!(s.counts().0 > before, "b, three plates back, is built again");

    // a demo looked at, switched away from and back
    s.demos = crate::Demos { list: vec![("cube_demo".into(), "Cube demo".into())], build: |key| (key == "cube_demo").then(|| block("cube_demo")) };
    s.open_demo(block("cube_demo"), "cube_demo").unwrap();
    s.switch(&a).unwrap();
    let before = s.counts().0;
    s.open_demo(block("cube_demo"), "cube_demo").unwrap();
    assert_eq!((s.counts().0, s.demo_key()), (before, Some("cube_demo")), "the demo back is not built again");
    let _ = std::fs::remove_dir_all(&root);
}

/// A plate opened from disk is used in place: an edit goes to its working copy (a `*`), never
/// into its file; opened again it comes back as it was left; Save writes the file and the `*`
/// goes. A new plate is saved into a folder of its own in the plates home.
#[test]
fn a_plate_from_disk_is_edited_in_its_working_copy_and_only_save_writes_its_file() {
    let root = root("save");
    let file = root.join("elsewhere").join("part.fab.splash");
    project::save(&block("part"), &file).unwrap();
    let before = std::fs::read(&file).unwrap();
    let mut s = session(&root);
    s.open(&file).unwrap();
    let id = s.plate.clone().unwrap();
    assert!(!s.modified());
    edit_saved(&mut s, "30%");
    assert!(s.modified());
    let record = store(&root).read(&id).unwrap();
    assert!(record.modified && record.title().ends_with(" *"), "{record:?}");
    assert_eq!(std::fs::read(&file).unwrap(), before, "an edit never writes the plate's own file");
    s.new_plate("other").unwrap();
    s.open(&file).unwrap();
    assert_eq!(s.plate.as_deref(), Some(id.as_str()), "the file's plate, not a second one");
    assert_eq!(infill(&s).as_deref(), Some("30%"), "opened again, as it was left");
    assert_eq!(s.save(None).unwrap(), file);
    assert!(!s.modified() && !store(&root).read(&id).unwrap().modified);
    assert!(String::from_utf8(std::fs::read(&file).unwrap()).unwrap().contains("30%"));
    // an edit after the save is a * again; undone back to what was saved, the * goes
    edit_saved(&mut s, "31%");
    assert!(store(&root).read(&id).unwrap().modified);
    assert!(s.undo_redo(fab_model::HistoryAction::Undo).ok);
    s.autosave().unwrap();
    assert!(!s.modified() && !store(&root).read(&id).unwrap().modified, "back to the file as saved");
    // the history from before the switch away and back: undone past the save, a * again
    assert!(s.undo_redo(fab_model::HistoryAction::Undo).ok);
    assert_eq!(infill(&s), None);
    s.autosave().unwrap();
    assert!(s.modified() && store(&root).read(&id).unwrap().modified);

    s.replace_model(block("fresh"), None).unwrap();
    let saved = s.save(None).unwrap();
    assert_eq!(saved, home(&root).join("fresh").join("fresh.fab.splash"));
    assert!(saved.is_file() && saved.with_file_name("fresh.cad.splash").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

/// A demo is shown as it ships and is no plate; its first edit makes the person's own plate (a
/// copy of the demo), and the demo opened again is as it ships. The AI lists the demos and opens
/// one by key. A plate copies, renames and comes off the list with its files kept.
#[test]
fn a_demo_stays_as_it_ships_and_its_first_edit_makes_a_plate() {
    let root = root("demo");
    let st = store(&root);
    let mut s = session(&root);
    s.demos = crate::Demos { list: vec![("cube_demo".into(), "Cube demo".into())], build: |key| (key == "cube_demo").then(|| block("cube_demo")) };
    let list = s.call("plates", &args(r#"{"action":"list"}"#), None);
    assert!(list.ok && list.text.contains("key: \"cube_demo\"") && list.text.contains("Cube demo"), "{}", list.text);
    let opened = s.call("plates", &args(r#"{"action":"open","demo":"cube_demo"}"#), None);
    assert!(opened.ok, "{}", opened.text);
    assert_eq!(s.demo_key(), Some("cube_demo"));
    assert!(!s.call("plates", &args(r#"{"action":"open","demo":"no_such"}"#), None).ok);
    s.autosave().unwrap();
    assert!(s.plate.is_none() && s.plates().is_empty(), "a demo looked at is no plate");
    assert_eq!(st.current(), Some(Current::Demo("cube_demo".into())));
    edit_saved(&mut s, "40%");
    let id = s.plate.clone().expect("the first edit makes a plate");
    let record = st.read(&id).unwrap();
    assert_eq!(record.origin, Origin::Demo("cube_demo".into()));
    assert!(record.modified && record.file.is_none());
    assert_eq!(st.current(), Some(Current::Plate(id.clone())));
    s.open_demo(block("cube_demo"), "cube_demo").unwrap();
    assert_eq!(infill(&s), None, "the demo as it ships");
    s.switch(&id).unwrap();
    assert_eq!(infill(&s).as_deref(), Some("40%"));

    let copy = s.duplicate_plate(&id).unwrap();
    assert_eq!(copy.name, "cube_demo copy");
    let renamed = s.rename_plate(&copy.id, "mine").unwrap();
    assert_eq!(renamed.name, "mine");
    s.switch(&copy.id).unwrap();
    assert_eq!((infill(&s).as_deref(), s.model.name.as_str()), (Some("40%"), "mine"));
    s.remove_plate(&id).unwrap();
    assert!(s.plates().iter().all(|p| p.id != id), "off the list");
    assert!(st.dir(&id).join("scripts").join("map").is_file(), "its working copy stays");
    // the plate on the bed taken off: the next one on the list comes
    s.remove_plate(&copy.id).unwrap();
    assert!(s.plate.is_some() && s.plate.as_deref() != Some(copy.id.as_str()));
    let _ = std::fs::remove_dir_all(&root);
}

/// The first start folds what earlier versions kept into plates: the working document's crash
/// copy (on the bed, its edit kept, a `*` because its file lacks it), the recent list, the plates
/// kept when replaced, the image-to-mesh results and the plates in the plates home. Every file
/// stays where it was, byte for byte, and a second start lists each plate once.
#[test]
fn the_lists_and_crash_copy_of_earlier_versions_become_plates_with_nothing_lost() {
    let root = root("migrate");
    let fab = root.join("fab");
    let saved = home(&root).join("demo 2026-11-04").join("demo.fab.splash");
    project::save(&block("demo"), &saved).unwrap();
    // the old single crash copy, one edit further than its file
    let mut old = Session::new(Machine::dual_nozzle_350(), home(&root));
    old.open(&saved).unwrap();
    edit(&mut old, "50%");
    autosave::Snapshot::default().save(&fab.join("autosave"), &old.model, Some(&saved)).unwrap();
    let elsewhere = root.join("elsewhere").join("a.fab.splash");
    project::save(&block("a"), &elsewhere).unwrap();
    std::fs::write(fab.join("recent"), format!("{}\n{}\n", elsewhere.display(), saved.display())).unwrap();
    project::save(&block("old"), &fab.join("previous").join("1793591509-old").join("old.fab.splash")).unwrap();
    project::save(&block("lattice"), &fab.join("made").join("1793602544-lattice").join("lattice.fab.splash")).unwrap();
    project::save(&block("two-vases"), &fab.join("made").join("two-vases").join("two-vases.fab.splash")).unwrap();
    project::save(&block("stray"), &home(&root).join("stray").join("stray.fab.splash")).unwrap();
    let mut before = BTreeMap::new();
    files_under(&root, &mut before);

    let mut s = session(&root);
    s.recover().expect("the working document").unwrap();
    assert_eq!(infill(&s).as_deref(), Some("50%"), "the crash copy's edit");
    assert_eq!(s.path.as_ref(), Some(&saved));
    assert!(s.modified(), "its file lacks the edit");
    let plates = s.plates();
    let names: BTreeSet<&str> = plates.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["demo 2026-11-04", "a", "old", "lattice", "two-vases", "stray"]), "a plate in a folder of its own is named after it: {plates:?}");
    assert_eq!(plates.iter().filter(|p| p.origin == Origin::Made).count(), 2);
    assert!(plates.iter().filter(|p| p.name != "demo 2026-11-04").all(|p| !p.modified && p.file.as_ref().is_some_and(|f| f.is_file())));
    let mut after = BTreeMap::new();
    files_under(&root, &mut after);
    for (path, bytes) in &before {
        assert_eq!(after.get(path), Some(bytes), "{} stays as it was", path.display());
    }
    drop(s);
    let mut t = session(&root);
    t.recover().unwrap().unwrap();
    assert_eq!(t.plates().len(), 6, "a second start lists each plate once");
    let _ = std::fs::remove_dir_all(&root);
}

/// One file of a working copy edited in place while Fab was closed (so its name is no longer
/// the hash of what it holds) is the person's newest work: the plate comes back with that file
/// as it reads, its settings as they were left, and says so. Nothing of the working copy is
/// pruned or written over, then or after: the next autosave writes the file again under its own
/// hash and leaves the edited one as it is, and the run after reads it all back as Fab wrote it.
#[test]
fn a_file_changed_outside_fab_is_taken_as_it_reads_and_nothing_is_pruned() {
    let root = root("outside");
    let mut s = session(&root);
    s.replace_model(block("a"), None).unwrap();
    let a = s.plate.clone().unwrap();
    edit_saved(&mut s, "11%");
    drop(s);
    let file = script_file(&root, &a, "a.cad.splash");
    let text = std::fs::read_to_string(&file).unwrap().replace("size: [10mm 10mm 10mm]", "size: [12mm 10mm 10mm]");
    assert!(text.contains("12mm"));
    std::fs::write(&file, &text).unwrap();
    let scripts = file.parent().unwrap().to_path_buf();
    let mut before = BTreeMap::new();
    files_under(&scripts, &mut before);
    before.remove(&scripts.join("map"));

    let mut t = session(&root);
    t.recover().expect("the bed comes back").unwrap();
    assert_eq!(t.plate.as_deref(), Some(a.as_str()));
    assert_eq!(infill(&t).as_deref(), Some("11%"), "the plate's settings as they were left");
    assert_eq!(t.model.source("a.cad.splash"), Some(text.as_str()), "the script as it reads");
    let said = t.outside_edits.get(&a).cloned().unwrap_or_default();
    let named = file.file_name().unwrap().to_str().unwrap();
    assert!(said.len() == 1 && said[0].contains("scripts/a.cad.splash was changed outside Fab") && said[0].contains(named), "{said:?}");
    assert!(store(&root).kept_aside(&a).is_empty(), "it reads: nothing is kept aside");
    t.autosave().unwrap();
    let mut after = BTreeMap::new();
    files_under(&scripts, &mut after);
    for (path, bytes) in &before {
        assert_eq!(after.get(path), Some(bytes), "{} stays as it was", path.display());
    }
    edit_saved(&mut t, "12%");
    assert_eq!(std::fs::read_to_string(&file).ok(), Some(text.clone()), "the edited file stays as it is after the next edit too");
    drop(t);
    let mut u = session(&root);
    u.recover().expect("the bed comes back").unwrap();
    assert_eq!((infill(&u).as_deref(), u.model.source("a.cad.splash")), (Some("12%"), Some(text.as_str())));
    assert!(u.outside_edits.is_empty(), "read back as Fab wrote it: {:?}", u.outside_edits);
    let _ = std::fs::remove_dir_all(&root);
}

/// A working copy that does not read back (here its script, changed outside Fab, is no longer
/// text) is never pruned: it is kept aside whole, with why, before the plate opens again as it
/// would with none, and the autosaves and runs after leave it as it was.
#[test]
fn a_working_copy_that_does_not_read_back_is_kept_aside_whole() {
    let root = root("kept");
    let mut s = session(&root);
    s.replace_model(block("a"), None).unwrap();
    let a = s.plate.clone().unwrap();
    edit_saved(&mut s, "11%");
    drop(s);
    let dir = store(&root).dir(&a);
    std::fs::write(script_file(&root, &a, "a.cad.splash"), [0xff, 0xfe, 0x00]).unwrap();
    let mut before = BTreeMap::new();
    files_under(&dir.join("scripts"), &mut before);

    let mut t = session(&root);
    t.recover().expect("the bed comes back").unwrap();
    assert_eq!(t.plate.as_deref(), Some(a.as_str()), "the plate opens again, as it would with no working copy");
    let kept = store(&root).kept_aside(&a);
    assert_eq!(kept.len(), 1, "{kept:?}");
    let (folder, why) = &kept[0];
    assert!(why.contains("did not read back") && why.contains("scripts/a.cad.splash") && why.contains("changed outside Fab"), "{why}");
    let whole = |when: &str| {
        for (path, bytes) in &before {
            let at = folder.join(path.strip_prefix(&dir).unwrap());
            assert_eq!(std::fs::read(&at).ok().as_ref(), Some(bytes), "{when}: {} is kept as it was", at.display());
        }
    };
    whole("kept aside");
    t.autosave().unwrap();
    edit_saved(&mut t, "12%");
    whole("after the autosaves");
    drop(t);
    let mut u = session(&root);
    u.recover().expect("the bed comes back").unwrap();
    assert_eq!(store(&root).kept_aside(&a).len(), 1, "kept aside once");
    whole("the next run");
    let _ = std::fs::remove_dir_all(&root);
}

/// A working copy whose map is gone, its files still there, is not taken for none: the files are
/// kept aside, not pruned by the autosave after.
#[test]
fn files_with_no_map_are_kept_aside_not_pruned() {
    let root = root("nomap");
    let mut s = session(&root);
    s.replace_model(block("a"), None).unwrap();
    let a = s.plate.clone().unwrap();
    edit_saved(&mut s, "11%");
    drop(s);
    let dir = store(&root).dir(&a);
    std::fs::remove_file(dir.join("scripts/map")).unwrap();
    let mut before = BTreeMap::new();
    files_under(&dir.join("scripts"), &mut before);
    let mut t = session(&root);
    t.recover().expect("the bed comes back").unwrap();
    t.autosave().unwrap();
    let kept = store(&root).kept_aside(&a);
    assert!(kept.len() == 1 && kept[0].1.contains("scripts/map is not there, and 3 of its files are"), "{kept:?}");
    for (path, bytes) in &before {
        let at = kept[0].0.join(path.strip_prefix(&dir).unwrap());
        assert_eq!(std::fs::read(&at).ok().as_ref(), Some(bytes), "{} is kept as it was", at.display());
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn docs(pairs: &[(&str, &str)]) -> std::sync::Arc<fab_model::Docs> {
    std::sync::Arc::new(pairs.iter().map(|(file, text)| (file.to_string(), std::sync::Arc::<str>::from(*text))).collect())
}

/// The file plate `id`'s working copy keeps electronics `name` in, as its map names it.
fn elec_file(root: &Path, id: &str, name: &str) -> PathBuf {
    let scripts = store(root).dir(id).join("scripts");
    let map = std::fs::read_to_string(scripts.join("map")).unwrap();
    let hash = map.lines().find_map(|l| l.strip_suffix(&format!("\telec/{name}"))).expect("the map names the document");
    let extension = if name.ends_with(".splash") { "elec.splash" } else { "elec.bin" };
    scripts.join(format!("{hash}.{extension}"))
}

fn scripts_of(root: &Path, id: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    files_under(&store(root).dir(id).join("scripts"), &mut files);
    files
}

/// Why: a plate's electronics documents and assets are files of the plate, outside its object
/// history. They are saved, recovered and pruned like its scripts; an object edit, undo or redo
/// never moves them; a document edit alone is an autosave, a demo's fork and a `*`; and a plate
/// whose documents were never opened keeps them through object edits, autosave and a restart.
#[test]
fn a_plates_electronics_are_its_files_and_outside_its_object_history() {
    use std::sync::Arc;
    let root = root("docs");
    let (pcb, ckt) = ("ledctl.pcb.splash", "ledctl.ckt.splash");
    let (part, body) = ("parts/lcsc/C1.part.splash", "parts/lcsc/C1.glb");
    let assets = Arc::new(fab_model::ElecAssets::from([(body.into(), Arc::<[u8]>::from([0x00, 0xff, 0x80]))]));
    let mut s = session(&root);
    s.replace_model(block("a"), None).unwrap();
    let a = s.plate.clone().unwrap();
    edit_saved(&mut s, "11%");
    // (b) no electronics: the content is the object document's, and a second save writes nothing
    assert_eq!(s.model.content(), s.model.head().content, "(b) with no electronics the content is the head's");
    let before = scripts_of(&root, &a);
    s.autosave().unwrap();
    assert_eq!(scripts_of(&root, &a), before, "(b) a second save writes no file");

    // (f) setting electronics moves neither the head nor the undo list
    let (head, undo) = (s.model.head(), s.model.undo_labels().count());
    let d1 = docs(&[(pcb, "board one\n"), (ckt, "circuit one\n"), (part, "part one\n")]);
    s.model.set_docs(d1.clone());
    assert_eq!((s.model.head(), s.model.undo_labels().count()), (head, undo), "(f) set_docs moves neither head nor undo");
    assert_ne!(s.model.content(), head.content, "documents are part of the plate's content");
    // (c) a document edit alone is an autosave: one content file per changed document and the map
    assert!(s.needs_autosave(), "(c) set_docs alone needs an autosave");
    s.model.set_elec_assets(assets.clone());
    assert_eq!((s.model.head(), s.model.undo_labels().count()), (head, undo), "assets move neither head nor undo");
    s.autosave().unwrap();
    let pcb_one = elec_file(&root, &a, pcb);
    let ckt_one = elec_file(&root, &a, ckt);
    let body_one = elec_file(&root, &a, body);
    assert_eq!(std::fs::read(&body_one).expect("asset is saved in the working copy"), assets[body].as_ref());
    assert_eq!(std::fs::read_to_string(&pcb_one).unwrap(), "board one\n");
    let before = scripts_of(&root, &a);
    let mut d2 = (*d1).clone();
    d2.insert(pcb.into(), "board two\n".into());
    let d2 = Arc::new(d2);
    s.model.set_docs(d2.clone());
    assert!(s.needs_autosave(), "(c) a changed document needs an autosave");
    s.autosave().unwrap();
    let after = scripts_of(&root, &a);
    let pcb_two = elec_file(&root, &a, pcb);
    let added: Vec<_> = after.keys().filter(|p| !before.contains_key(*p)).collect();
    let removed: Vec<_> = before.keys().filter(|p| !after.contains_key(*p)).collect();
    let changed: Vec<_> = after.iter().filter(|(p, b)| before.get(*p).is_some_and(|old| old != *b)).map(|(p, _)| p.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!((added, removed, changed), (vec![&pcb_two], vec![&pcb_one], vec!["map"]), "(c) one content file and the map; prune removes only that document's old file");
    assert_eq!(elec_file(&root, &a, ckt), ckt_one, "(c) the unchanged document keeps its file");
    assert_eq!(elec_file(&root, &a, body), body_one, "the unchanged asset keeps its file through a document edit");

    // (f) an object commit, undo and redo keep the documents as they are
    edit(&mut s, "12%");
    assert!(Arc::ptr_eq(s.model.docs(), &d2), "(f) an object commit keeps the documents");
    assert!(Arc::ptr_eq(s.model.elec_assets(), &assets), "an object commit keeps the assets");
    assert!(s.undo_redo(fab_model::HistoryAction::Undo).ok);
    assert!(Arc::ptr_eq(s.model.docs(), &d2), "(f) an object undo keeps the documents");
    assert!(Arc::ptr_eq(s.model.elec_assets(), &assets), "an object undo keeps the assets");
    assert!(s.undo_redo(fab_model::HistoryAction::Redo).ok);
    assert!(Arc::ptr_eq(s.model.docs(), &d2), "(f) an object redo keeps the documents");
    assert!(Arc::ptr_eq(s.model.elec_assets(), &assets), "an object redo keeps the assets");
    // the permanent case: object scripts edited, the documents never touched, then a restart
    let source = s.model.source("a.cad.splash").unwrap().replace("10mm 10mm 10mm", "14mm 10mm 10mm");
    let (r, problem) = s.set_source("a.cad.splash", &source);
    assert!(r.ok && problem.is_none(), "{}", r.text);
    s.autosave().unwrap();
    drop(s);
    let mut t = session(&root);
    t.recover().expect("the bed comes back").unwrap();
    assert_eq!(t.model.source("a.cad.splash"), Some(source.as_str()));
    assert_eq!(**t.model.docs(), *d2, "a plate edited only in its object scripts keeps its documents after autosave and restart");
    assert_eq!(t.model.elec_assets(), &assets, "object edits, autosave and restart keep the assets");
    assert!(t.outside_edits.is_empty(), "{:?}", t.outside_edits);

    // (a) a plate with documents and no object script saves and recovers them byte for byte
    t.new_plate("e").unwrap();
    let e = t.plate.clone().unwrap();
    assert!(t.model.is_empty());
    t.model.set_elec_assets(assets.clone());
    assert!(!t.model.is_empty() && t.needs_autosave(), "assets alone form a plate and need autosave");
    t.autosave().expect("autosave a plate with only assets");
    let only_assets = store(&root).working_model(&e).expect("an asset-only working copy recovers");
    assert!(only_assets.docs().is_empty(), "asset-only recovery invents no documents");
    assert_eq!(only_assets.elec_assets(), &assets, "asset-only recovery keeps every byte");
    t.model.set_docs(d1.clone());
    assert!(!t.model.is_empty() && t.needs_autosave());
    t.autosave().unwrap();
    // (g) a demo with documents set is the person's plate
    t.demos = crate::Demos { list: vec![("cube_demo".into(), "Cube demo".into())], build: |key| (key == "cube_demo").then(|| block("cube_demo")) };
    t.open_demo(block("cube_demo"), "cube_demo").unwrap();
    assert!(!t.needs_autosave(), "a demo looked at is no plate");
    t.model.set_docs(d1.clone());
    assert!(t.needs_autosave(), "(g) documents set on a demo fork it");
    t.model.set_elec_assets(assets.clone());
    t.autosave().unwrap();
    let g = t.plate.clone().expect("(g) the fork is a plate");
    assert_eq!(store(&root).read(&g).unwrap().origin, Origin::Demo("cube_demo".into()));
    t.switch(&e).unwrap();
    assert_eq!(**t.model.docs(), *d1, "(a) recovered byte for byte");
    assert_eq!(t.model.elec_assets(), &assets, "switching plates recovers their assets");
    assert!(t.model.project().scripts.is_empty(), "(a) and still no object script");
    t.switch(&g).unwrap();
    assert_eq!(**t.model.docs(), *d1, "(g) the fork keeps its documents");
    assert_eq!(t.model.elec_assets(), &assets, "the demo fork keeps its assets");
    drop(t);

    // (d) a document changed outside Fab is taken as it reads, said, and never pruned
    let file = elec_file(&root, &a, pcb);
    std::fs::write(&file, "board by hand\n").unwrap();
    let mut u = session(&root);
    u.switch(&a).unwrap();
    assert_eq!(u.model.docs()[pcb].as_ref(), "board by hand\n", "(d) taken as it reads");
    let said = u.outside_edits.get(&a).cloned().unwrap_or_default();
    assert!(said.len() == 1 && said[0].contains(&format!("elec/{pcb} was changed outside Fab")), "(d) {said:?}");
    u.autosave().unwrap();
    edit_saved(&mut u, "13%");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "board by hand\n", "(d) never pruned or written over");

    // (h) a file save carries the documents, a load reads the same content; a document edit is a
    // `*` and Save clears it
    let saved = u.save(None).unwrap();
    assert!(!u.modified());
    assert_eq!(std::fs::read_to_string(project::elec_dir(&saved).join(pcb)).unwrap(), "board by hand\n");
    let loaded = project::load(&saved, u.model.env()).unwrap();
    assert_eq!(loaded.content(), u.model.content(), "(h) file save then load: equal content");
    assert_eq!(loaded.elec_assets(), &assets, "file save then load keeps assets");
    u.model.set_docs(d1.clone());
    assert!(u.modified(), "(h) a document change is a *");
    u.save(None).unwrap();
    assert!(!u.modified(), "(h) Save clears it");
    assert_eq!(std::fs::read_to_string(project::elec_dir(&saved).join(pcb)).unwrap(), "board one\n");
    let edited_assets = Arc::new(fab_model::ElecAssets::from([(body.into(), Arc::<[u8]>::from([0x00, 0xfe]))]));
    u.model.set_elec_assets(edited_assets.clone());
    assert!(u.modified() && u.needs_autosave(), "an asset edit alone marks the plate modified and needs autosave");
    u.save(None).expect("save the edited asset");
    assert!(!u.modified(), "saving assets clears the modified mark");
    assert_eq!(project::load(&saved, u.model.env()).expect("reload edited assets").elec_assets(), &edited_assets);
    drop(u);

    // (e) a document that is no longer text keeps the working copy aside whole
    std::fs::write(elec_file(&root, &e, ckt), [0xff, 0xfe, 0x00]).unwrap();
    let kept_body = elec_file(&root, &e, body);
    let mut v = session(&root);
    v.switch(&e).unwrap();
    let kept = store(&root).kept_aside(&e);
    assert!(kept.len() == 1 && kept[0].1.contains(&format!("elec/{ckt}")), "(e) {kept:?}");
    let plate_dir = store(&root).dir(&e);
    let relative = kept_body.strip_prefix(&plate_dir).expect("asset belongs to the plate's working copy");
    assert_eq!(std::fs::read(kept[0].0.join(relative)).expect("the whole working copy keeps its binary asset"),
        assets[body].as_ref());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn delete_moves_the_id_folder_first_and_a_refused_one_moves_nothing_else() {
    let root = root("delete-order");
    let store = store(&root);
    let outside = root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let make = |name: &str| {
        let file = outside.join(format!("{name}.fab.splash"));
        std::fs::write(&file, "// project").unwrap();
        let plate = store.create(name, Some(file.clone()), String::new(), Origin::Plate).unwrap();
        (plate.id, file)
    };
    // a trash that records its paths and moves them into a folder of its own
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let moves = |refuse: Option<PathBuf>, seen: &std::rc::Rc<std::cell::RefCell<Vec<PathBuf>>>| {
        let (bin, seen) = (bin.clone(), seen.clone());
        move |path: &Path| -> Result<(), String> {
            seen.borrow_mut().push(path.to_path_buf());
            if refuse.as_deref() == Some(path) { return Err(format!("{}: refused", path.display())) }
            let to = bin.join(format!("{}-{}", seen.borrow().len(), path.file_name().unwrap().to_string_lossy()));
            std::fs::rename(path, to).map_err(|e| e.to_string())
        }
    };
    let fresh = || std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    let (id, file) = make("first");
    let seen = fresh();
    store.delete(&id, &moves(None, &seen)).unwrap();
    assert_eq!(seen.borrow()[0], store.dir(&id), "the id folder moves first");
    assert_eq!(seen.borrow()[1], file, "then the project's file");
    assert!(!store.dir(&id).exists() && !file.exists(), "both are in the trash");

    let (id, file) = make("second");
    let seen = fresh();
    assert!(store.delete(&id, &moves(Some(store.dir(&id)), &seen)).is_err(), "a refused id folder is an error");
    assert_eq!(*seen.borrow(), vec![store.dir(&id)], "nothing else was tried");
    assert!(store.dir(&id).exists() && file.exists(), "every file is kept");

    let (id, file) = make("third");
    let seen = fresh();
    assert!(store.delete(&id, &moves(Some(file.clone()), &seen)).is_err(), "a refused project file is an error");
    assert!(!store.dir(&id).exists(), "the id folder had moved");
    assert!(file.exists(), "the project's file is still there to open");
    let _ = std::fs::remove_dir_all(root);
}
