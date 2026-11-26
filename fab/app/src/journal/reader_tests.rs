use super::*;
use crate::journal::{at as packed, test_dir, Binding, Call, Plate, Record, Writer};
use std::io::Write;
use std::thread;

fn start(root: &Path, folder: &str) -> Reader {
    let (mut reader, worker) = Reader::channel(root.to_owned());
    thread::spawn(move || worker.run());
    select(&mut reader, folder);
    reader
}

fn select(reader: &mut Reader, folder: &str) {
    let target = reader.select(folder).unwrap();
    let reply = receive(reader);
    assert!(reply.target == target && matches!(reply.answer, Answer::Changed), "the first scan belongs to the selected plate");
}

fn receive(reader: &mut Reader) -> Reply {
    reader.flush().unwrap();
    reader.replies.recv_timeout(Duration::from_secs(5)).expect("journal worker replies")
}

fn ask(reader: &mut Reader, request: Request) -> Answer {
    reader.request(request).unwrap();
    loop {
        let reply = receive(reader);
        assert_eq!(reply.target, reader.watch.as_ref().unwrap().target);
        if !matches!(reply.answer, Answer::Changed) { return reply.answer }
    }
}

fn page(reader: &mut Reader, filter: Filter, anchor: Option<Key>, size: usize) -> (Vec<Row>, Option<Key>) {
    let Answer::Page { rows, next } = ask(reader, Request::Page { filter, anchor, size }) else { panic!("expected a journal page") };
    (rows, next)
}

fn count(reader: &mut Reader, filter: Filter) -> usize {
    let Answer::Count(count) = ask(reader, Request::Count(filter)) else { panic!("expected a filtered count") };
    count
}

fn drain(writer: &Writer) {
    while let Ok(row) = writer.rx.try_recv() { writer.batch(row); }
}

fn open_and_record(dir: &Path, manifest: &str) {
    let plate = Plate::testing(dir, manifest, &Default::default());
    let writer = plate.take_writer();
    Call::begin(&plate, fab_elec_design::By::Person, "inspect", obj(vec![])).done("ok", "finished".into());
    drain(&writer);
    assert_eq!(writer.progress.pending(), 0);
}

fn assert_copy(dir: &Path, order: u64, dest: &Path, manifest: &str) {
    crate::journal::rebuild(dir, Some(order), dest).unwrap();
    assert_eq!(fs::read_dir(dest).unwrap().count(), 1);
    assert_eq!(fs::read(dest.join(manifest)).unwrap(), b"// fixture\nuse mod.fab.*\n");
}

fn assert_gap(dir: &Path, order: u64, dest: &Path, at: u64) {
    let mut steps = Vec::new();
    let report = crate::journal::replay::state(dir, Some(order), dest, &mut |step| steps.push(step.clone())).unwrap();
    assert!(matches!(steps.last(), Some(step) if matches!(step.outcome, crate::journal::replay::Outcome::Gap { .. })));
    assert_eq!(report.stopped.as_ref(), steps.last());
    let error = crate::journal::rebuild(dir, Some(order), dest).unwrap_err();
    assert_eq!(error.to_string(), format!("the journal has damage at byte {at} between the checkpoint and the requested record"));
    assert!(!dest.exists(), "a gap must be reported before creating the destination");
}

#[test]
fn an_idle_writers_reader_sees_a_peer_and_never_lists_an_unfinished_repair() {
    use crate::journal::{number, Journal, Repair, LOCK};
    let root = test_dir();
    let dir = root.join("plate/journal");
    let a = Plate::testing(&dir, "a.fab.splash", &Default::default());
    let writer_a = a.take_writer();
    a.checkpoint();
    drain(&writer_a);
    let local_end = writer_a.progress.durable_end.load(Ordering::Acquire);
    assert!(live::writer_progress(&dir).is_some(), "A must have a registered live writer");
    let mut reader = start(&root, "plate");
    let (b, writer_b) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    b.send(Record::Checkpoint { manifest: Some("b.fab.splash".into()),
        files: Arc::new([("b.fab.splash".into(), Arc::from(b"// fixture\nuse mod.fab.*\n".as_slice()))].into()) });
    drain(&writer_b);
    let all = Filter::default();
    // A request runs the same refresh as an idle watch tick. It also guarantees
    // a reply if Changed regresses, without waiting for a timer to fire.
    reader.request(Request::Page { filter: all, anchor: None, size: 10 }).unwrap();
    assert!(matches!(reader.replies.recv().unwrap().answer, Answer::Changed), "B's append must notify A's tab");
    let Answer::Page { rows, .. } = reader.replies.recv().unwrap().answer else { panic!("expected A's updated page") };
    assert_eq!(rows.iter().map(|r| r.key.position).collect::<Vec<_>>(),
        [Position::Order(packed(2, 1)), Position::Order(packed(1, 1))], "the page must include both openings");
    assert_eq!(writer_a.progress.durable_end.load(Ordering::Acquire), local_end, "A must remain idle");
    assert_copy(&dir, packed(2, 1), &root.join("peer"), "b.fab.splash");

    let lock = fs::OpenOptions::new().read(true).write(true).open(dir.join(LOCK)).unwrap();
    lock.lock().unwrap();
    let mut log = fs::OpenOptions::new().append(true).open(dir.join(LOG)).unwrap();
    let at = log.metadata().unwrap().len();
    let fragment = format!(r#"{{"kind":"checkpoint","order":{},"files":[]}}"#, packed(2, 2));
    log.write_all(fragment.as_bytes()).unwrap();
    assert_eq!(count(&mut reader, all), 2, "an in-flight row needs its newline even when the JSON parses");
    File::unlock(&lock).unwrap();
    assert_eq!(count(&mut reader, all), 2, "a dead writer's fragment must stay off the page");
    // With no writer mid-row the fragment is a tear: the latest state is unknown until
    // a writer repairs it, while replay through the last complete row still works.
    let error = crate::journal::rebuild(&dir, None, &root.join("latest")).unwrap_err();
    assert_eq!(error.to_string(), format!("the journal has damage at byte {at} between the checkpoint and the requested record"),
        "a dead writer's unrepaired fragment must stop the latest replay");
    assert!(!root.join("latest").exists(), "a gap must be reported before creating the destination");
    assert_copy(&dir, packed(2, 1), &root.join("before-repair"), "b.fab.splash");

    lock.lock().unwrap();
    let end = at + fragment.len() as u64;
    let marker = obj(vec![("kind", s("lost")), ("cause", s("torn")), ("order", number(packed(2, 2))),
        ("at", number(at)), ("bytes", number(end - at)), ("after", number(packed(2, 1)))]);
    let repair = Repair { end, marker: marker.to_json().into_bytes() };
    repair.stage(&dir).unwrap();
    for piece in [b"\n".as_slice(), &repair.marker[..8], &repair.marker[8..], b"\n"] {
        log.write_all(piece).unwrap();
        let bound = live::read_boundary(&dir, &log, None).unwrap();
        assert!(bound.repairing && bound.end == end, "a pending repair caps every sample before the separator");
        assert_eq!(count(&mut reader, all), 2, "the fragment must never become a selectable checkpoint");
    }
    repair.finish(&writer_b).unwrap();
    drop((log, lock));
    let (rows, _) = page(&mut reader, all, None, 10);
    assert_eq!(rows.len(), 4, "the finished repair publishes its diagnostic and marker together");
    assert!(rows.iter().any(|r| r.key.position == Position::Diagnostic(at)));
    assert!(rows.iter().any(|r| r.key.position == Position::Order(packed(2, 2)) && r.kind == Kind::Lost));
    drop(reader);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn torn_tail_is_preserved_once_before_the_next_checkpoint_and_call() {
    for tail in [b"{\"broken\":\xff".as_slice(), format!(r#"{{"kind":"call","order":{0},"id":{0},"ref":"torn","actor":{{"kind":"ai"}}}}"#, packed(2, 1)).as_bytes()] {
        let root = test_dir();
        let dir = root.join("plate/journal");
        let path = dir.join(LOG);
        open_and_record(&dir, "old.fab.splash");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(format!("{{\"kind\":\"reload\",\"changes\":[],\"order\":{}}}\n{{\"kind\":\"reload\",\"changes\":[],\"order\":{}}}\n", packed(1, 5), packed(1, 4)).as_bytes()).unwrap();
        let at = file.metadata().unwrap().len();
        file.write_all(tail).unwrap();
        drop(file);
        let prefix = fs::read(&path).unwrap();
        let mut reader = start(&root, "plate");
        assert_eq!(count(&mut reader, Filter::default()), 5);
        open_and_record(&dir, "new.fab.splash");
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(&prefix));
        assert_eq!(bytes[prefix.len()], b'\n');
        let suffix: Vec<_> = bytes[prefix.len()+1..].split(|b| *b == b'\n').filter(|line| !line.is_empty())
            .map(|line| parse(line).unwrap()).collect();
        assert_eq!(suffix.iter().map(|r| r.get("kind").unwrap().as_str().unwrap()).collect::<Vec<_>>(), ["lost","checkpoint","call","done"]);
        assert_eq!(suffix.iter().map(|r| r.get("order").unwrap().as_u64().unwrap()).collect::<Vec<_>>(), [packed(2, 1),packed(2, 2),packed(2, 3),packed(2, 4)]);
        let lost = &suffix[0];
        assert_eq!(lost.get("cause").and_then(Value::as_str), Some("torn"));
        assert_eq!(lost.get("at").and_then(Value::as_u64), Some(at));
        assert_eq!(lost.get("bytes").and_then(Value::as_u64), Some(tail.len() as u64));
        assert_eq!(lost.get("after").and_then(Value::as_u64), Some(packed(1, 4)), "after is the last physical record; order uses the maximum");
        for row in &suffix[2..] {
            assert_eq!(row.get("id").and_then(Value::as_u64), Some(packed(2, 3)));
            assert_eq!(row.get("checkpoint").and_then(Value::as_u64), Some(packed(2, 2)));
        }
        let (rows, _) = page(&mut reader, Filter { kind: Some(Kind::Diagnostic), actor: None }, None, 2);
        assert!(matches!(rows.as_slice(), [row] if row.key.position == Position::Diagnostic(at)));
        let filter = Filter { kind: Some(Kind::Lost), actor: None };
        for actor in [Actor::Person, Actor::Ai, Actor::Agent, Actor::Disk] {
            assert_eq!(count(&mut reader, Filter { actor: Some(actor), ..filter }), 0);
        }
        assert_copy(&dir, packed(1, 5), &root.join("old"), "old.fab.splash");
        let dest = root.join("gap");
        let gap = crate::journal::replay::state(&dir, Some(packed(2, 1)), &dest, &mut |_| {}).unwrap().stopped.unwrap();
        assert_eq!(gap.order, packed(2, 1));
        assert_eq!(gap.outcome, crate::journal::replay::Outcome::Gap { reason: "no checkpoint in the requested range".into() });
        assert!(!dest.exists(), "a gap must be reported before creating the destination");
        assert_copy(&dir, packed(2, 3), &root.join("new"), "new.fab.splash");
        open_and_record(&dir, "third.fab.splash");
        assert_eq!(count(&mut reader, filter), 1, "the same torn bytes are reported only once");
        assert_copy(&dir, packed(3, 3), &root.join("third"), "third.fab.splash");
        // Order 13 is still outstanding. The old torn bytes provably belong to the previous writer.
        fs::OpenOptions::new().append(true).open(&path).unwrap()
            .write_all(format!("{{\"kind\":\"reload\",\"changes\":[],\"order\":{}}}\n", packed(3, 5)).as_bytes()).unwrap();
        assert_copy(&dir, packed(3, 5), &root.join("late"), "third.fab.splash");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn damage_in_the_middle_blocks_its_revision_and_spares_the_next_opening() {
    let root = test_dir();
    let dir = root.join("plate/journal");
    open_and_record(&dir, "old.fab.splash");
    let mut file = fs::OpenOptions::new().append(true).open(dir.join(LOG)).unwrap();
    file.write_all(format!("{{\"kind\":\"reload\",\"order\":{},\"changes\":[]}}\n", packed(1, 6)).as_bytes()).unwrap();
    let at = file.metadata().unwrap().len();
    file.write_all(&[b"\xff\n", format!("{{\"kind\":\"reload\",\"order\":{},\"changes\":[]}}\n", packed(1, 5)).as_bytes()].concat()).unwrap();
    drop(file);
    open_and_record(&dir, "new.fab.splash");
    let mut reader = start(&root, "plate");
    let (rows, _) = page(&mut reader, Filter::default(), None, 10);
    assert_eq!(rows[0].key.position, Position::Diagnostic(at));
    assert_eq!(rows[1].key.position, Position::Order(packed(2, 3)));
    assert_eq!(count(&mut reader, Filter { kind: Some(Kind::Lost), actor: None }), 0);
    assert_copy(&dir, packed(1, 3), &root.join("before"), "old.fab.splash");
    assert_gap(&dir, packed(1, 5), &root.join("crossing"), at);
    assert_gap(&dir, packed(1, 6), &root.join("late"), at);
    assert_copy(&dir, packed(2, 2), &root.join("after"), "new.fab.splash");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_interrupted_repair_finishes_the_same_marker() {
    use crate::journal::{number, Repair, REPAIR};
    for cut in [1, 8, usize::MAX] {
        let root = test_dir();
        let dir = root.join("plate/journal");
        open_and_record(&dir, "old.fab.splash");
        let path = dir.join(LOG);
        let at = fs::metadata(&path).unwrap().len();
        let tail = format!(r#"{{"kind":"checkpoint","order":{},"files":[]}}"#, packed(1, 4));
        let tail = tail.as_bytes();
        let marker = obj(vec![("kind", s("lost")), ("cause", s("torn")), ("order", number(packed(1, 4))),
            ("at", number(at)), ("bytes", number(tail.len() as u64)), ("after", number(packed(1, 3)))]);
        let suffix = format!("\n{}\n", marker.to_json());
        let end = at + tail.len() as u64;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(tail).unwrap();
        Repair { end, marker: marker.to_json().into_bytes() }.stage(&dir).unwrap();
        file.write_all(&suffix.as_bytes()[..cut.min(suffix.len())]).unwrap();
        drop(file);

        let mut reader = start(&root, "plate");
        assert_eq!(count(&mut reader, Filter::default()), 3, "a partial repair cannot publish a spurious checkpoint");
        assert_gap(&dir, packed(1, 4), &root.join("incomplete"), at);
        open_and_record(&dir, "new.fab.splash");
        let bytes = fs::read(path).unwrap();
        assert!(bytes[at as usize..].starts_with(tail));
        assert!(bytes[end as usize..].starts_with(suffix.as_bytes()));
        assert_eq!(count(&mut reader, Filter { kind: Some(Kind::Lost), actor: None }), 1);
        assert_copy(&dir, packed(2, 2), &root.join("new"), "new.fab.splash");
        assert!(!dir.join(REPAIR).exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn a_torn_first_record_has_no_preceding_order() {
    let root = test_dir();
    let dir = root.join("plate/journal");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(LOG), b"{").unwrap();
    open_and_record(&dir, "new.fab.splash");
    let bytes = fs::read(dir.join(LOG)).unwrap();
    let lost = parse(bytes.split(|b| *b == b'\n').nth(1).unwrap()).unwrap();
    assert_eq!(lost.get("after"), Some(&Value::Null));
    assert_eq!(lost.get("order").and_then(Value::as_u64), Some(packed(1, 1)));
    assert_copy(&dir, packed(1, 3), &root.join("after"), "new.fab.splash");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn late_done_keeps_the_anchor_and_joins_in_creation_order() {
    use fab_elec_design::By;
    let root = test_dir();
    let dir = root.join("plate/journal");
    let mut reader = start(&root, "plate");
    let all = Filter::default();
    assert_eq!(count(&mut reader, all), 0);
    assert!(!root.join("plate").exists(), "browsing an untouched plate creates nothing");

    let plate = Plate::testing(&dir, "example.fab.splash", &Default::default());
    let writer = plate.take_writer();
    let by = By::Agent { call: 9 };
    let call = Call::begin(&plate, by, "inspect", obj(vec![]));
    let binding = Binding::new(call.clone());
    call.done("cancelled", "cancelled".into()); // Reserve before the late commit.
    plate.effect(Some(&call), 7, by, "move", vec![]);
    drain(&writer);
    assert_eq!(writer.progress.durable_end.load(Ordering::Acquire), fs::metadata(dir.join(LOG)).unwrap().len());

    assert!(matches!(receive(&mut reader).answer, Answer::Changed),
        "the watch notices the first durable writes without a page request");
    let (before, next) = page(&mut reader, all, None, 1);
    assert_eq!(before[0].kind, Kind::Effect);
    let anchor = before[0].key.clone();
    let call_key = next.unwrap();
    drop(binding);
    plate.send(Record::History { undo: true, before: Some(7), after: None, changes: vec![] });
    plate.send(Record::Reload(vec![]));
    drain(&writer);
    let filter = Filter { actor: Some(Actor::Agent), kind: None };
    let (after, next) = page(&mut reader, filter, Some(anchor.clone()), 2);
    assert_eq!(after[0].key, anchor);
    assert_eq!(after[1].kind, Kind::Done);
    assert_eq!(after[1].actor, Some(Actor::Agent));
    assert!(after[1].call_time_ms.is_some());
    assert_eq!(next.as_ref(), Some(&call_key));
    assert_eq!(page(&mut reader, filter, next, 1).0[0].key, call_key);
    let done_key = after[1].key.clone();
    assert!(call_key.position < done_key.position && done_key.position < anchor.position);
    for (from, expected) in [(anchor, &call_key), (done_key.clone(), &call_key), (call_key.clone(), &done_key)] {
        let Answer::Detail { partner: Partner::Found { key, .. }, .. } = ask(&mut reader, Request::Detail(from))
            else { panic!("each local record opens its partner") };
        assert_eq!(&key, expected);
    }
    assert_eq!(count(&mut reader, filter), 3);
    assert_eq!(count(&mut reader, all), 6);
    for kind in [Kind::History, Kind::Reload] {
        assert_eq!(count(&mut reader, Filter { kind: Some(kind), actor: None }), 1);
        for actor in [Actor::Person, Actor::Ai, Actor::Agent, Actor::Disk] {
            assert_eq!(count(&mut reader, Filter { kind: Some(kind), actor: Some(actor) }), 0);
        }
    }
    drop(reader);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_partial_tail_waits_and_a_damaged_line_has_an_address() {
    use fab_elec_design::By;
    let root = test_dir();
    let dir = root.join("plate/journal");
    let plate = Plate::testing(&dir, "example.fab.splash", &Default::default());
    let writer = plate.take_writer();
    plate.effect(None, 1, By::Disk, "disk change", vec![]);
    drain(&writer);
    let mut reader = start(&root, "plate");
    let all = Filter::default();
    assert_eq!(count(&mut reader, Filter { actor: Some(Actor::Disk), kind: Some(Kind::Effect) }), 1);
    let offset = writer.progress.durable_end.load(Ordering::Acquire);
    let lock = fs::OpenOptions::new().read(true).write(true).open(dir.join(crate::journal::LOCK)).unwrap();
    lock.lock().unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(dir.join(LOG)).unwrap();
    file.write_all(b"{malformed}\n{\"kind\":\"history\",\"order\":3}\n{\"kind\":\"done\",\"order\":5,\"id\":4}").unwrap();
    assert_eq!(count(&mut reader, all), 4, "only newline-terminated rows are visible before sync");
    let (rows, _) = page(&mut reader, all, None, 10);
    let diagnostic = &rows.iter().find(|r| r.kind == Kind::Diagnostic).unwrap().key;
    assert_eq!(diagnostic.position, Position::Diagnostic(offset));
    let Answer::Detail { record, .. } = ask(&mut reader, Request::Detail(diagnostic.clone())) else { panic!("diagnostic detail") };
    assert_eq!(record.get("offset").and_then(Value::as_u64), Some(offset));
    assert!(rows.iter().any(|r| r.kind == Kind::History), "records after damage remain readable");
    file.write_all(b"\n").unwrap();
    file.sync_data().unwrap();
    let ai = Filter { actor: Some(Actor::Ai), kind: Some(Kind::Done) };
    assert_eq!(count(&mut reader, all), 5);
    assert_eq!(count(&mut reader, ai), 0);
    file.write_all(b"{\"kind\":\"call\",\"order\":4,\"id\":4,\"actor\":{\"kind\":\"ai\"}}\n").unwrap();
    file.sync_data().unwrap();
    assert_eq!(count(&mut reader, ai), 1, "a done resolves its actor even when its call arrives later");
    let rollback = file.metadata().unwrap().len();
    file.write_all(b"{\"kind\":\"reload\",\"order\":6}\n").unwrap();
    let reloads = Filter { kind: Some(Kind::Reload), actor: None };
    assert_eq!(page(&mut reader, reloads, None, 2).0[0].key.position, Position::Order(6));
    // Windows requires write-data access to truncate. Keep the append handle
    // for the replacement and the sidecar lock across both operations.
    fs::OpenOptions::new().write(true).open(dir.join(LOG)).unwrap().set_len(rollback).unwrap();
    file.write_all(b"{\"kind\":\"reload\",\"order\":7}\n").unwrap();
    file.sync_data().unwrap();
    drop((file, lock));
    let (rows, _) = page(&mut reader, reloads, None, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key.position, Position::Order(7), "a same-length replacement must retire the rolled-back row");
    drop(reader);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn switching_plate_during_scan_discards_the_old_generation() {
    let root = test_dir();
    let dir = root.join("first/journal");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(LOG), b"{\"order\":1,\"kind\":\"reload\"}\n{\"order\":2,\"kind\":\"history\"}\n").unwrap();
    let (mut reader, worker) = Reader::channel(root.clone());
    let (entered, scanning) = mpsc::channel();
    let (resume, gate) = mpsc::channel();
    let scan = thread::spawn(move || {
        let lines = std::cell::Cell::new(0);
        worker.serve(|generation| {
            lines.set(lines.get() + 1);
            if lines.get() == 2 { entered.send(()).unwrap(); gate.recv().unwrap(); }
            worker.generation.load(Ordering::Acquire) == generation
        })
    });
    let first = reader.select("first").unwrap();
    scanning.recv_timeout(Duration::from_secs(5)).unwrap(); // One line indexed; the scan is held before the next.
    reader.request(Request::Count(Filter::default())).unwrap();
    let second = reader.select("second").unwrap();
    assert_ne!(first.generation, second.generation);
    assert!(reader.try_recv().unwrap().is_none());
    resume.send(()).unwrap();
    // A queued old request may occupy the one-slot inbox. The UI services it only with try_recv.
    loop {
        if let Some(reply) = reader.try_recv().unwrap() {
            assert_eq!(reply.target, second, "no old page reaches the new plate");
            assert!(matches!(reply.answer, Answer::Changed));
            break
        }
        thread::yield_now();
    }
    assert!(page(&mut reader, Filter::default(), None, 2).0.is_empty());
    drop(reader);
    scan.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// Exercise the reader against the real new-plate import fixture and its colliding local id.
pub(in crate::journal) fn check_foreign(root: &Path, folder: &str, order: u64, origin: &str, expected: &Value) {
    let mut reader = start(root, folder);
    assert_eq!(count(&mut reader, Filter { actor: Some(Actor::Person), kind: Some(Kind::Call) }), 1);
    let key = Key { folder: folder.into(), journal: test_token(root, folder), position: Position::Order(order) };
    let Answer::Detail { partner: Partner::Found { key, record }, .. } = ask(&mut reader, Request::Detail(key))
        else { panic!("foreign effect must open the originating call") };
    assert_eq!(&*key.folder, origin);
    assert!(&record == expected, "the joined body is the originating call");
}

#[test]
fn foreign_links_only_open_literal_sibling_journals() {
    let root = test_dir();
    let local = root.join("store/b/journal");
    let outside = root.join("elsewhere/journal");
    fs::create_dir_all(&local).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join(LOG), b"{\"kind\":\"call\",\"id\":1,\"order\":1,\"ref\":\"portable\"}\n").unwrap();
    let mut file = File::create(local.join(LOG)).unwrap();
    let folders = ["gone", "../../elsewhere", "alias"];
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("store/alias")).unwrap();
    }
    for (i, folder) in folders.iter().enumerate() {
        writeln!(file, "{{\"kind\":\"effect\",\"order\":{},\"call\":null,\"call_ref\":\"portable\",\"call_journal\":\"{folder}\"}}", i + 1).unwrap();
    }
    let mut reader = start(&root.join("store"), "b");
    let (rows, _) = page(&mut reader, Filter::default(), None, folders.len());
    assert_eq!(rows.len(), folders.len());
    for row in rows {
        assert!(matches!(ask(&mut reader, Request::Detail(row.key)), Answer::Detail { partner: Partner::NotFound, .. }),
            "the local effect remains available when its call cannot be resolved safely");
    }
    drop(reader);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn forced_quit_recovers_outstanding_orders_once_and_marks_the_rebuild_gap() {
    use crate::journal::{loss, Journal};
    let root = test_dir();
    let store = fab_ai::plates::PlateStore::under(&root, &root);
    for local in [false, true] {
        let id = if local { "working-1-2-3" } else { "saved-plate" };
        let plate = store.dir(id);
        let dir = plate.join("journal");
        let channel = || {
            let (j, mut w) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
            w.settings = Some(root.clone());
            (j, w)
        };
        let checkpoint = || Record::Checkpoint { manifest: None, files: Arc::default() };
        let (j, w) = channel();
        j.send(checkpoint());
        drain(&w);
        drop((j, w));

        let saved = root.join("parked");
        let (j, w) = channel();
        if local {
            // A file in place of the plate deterministically defeats both the
            // prefix scan and the primary note, even for a privileged user.
            fs::rename(&plate, &saved).unwrap();
            fs::write(&plate, b"occupied").unwrap();
            j.send(Record::Reload(vec![]));
            assert!(!w.try_write(&w.rx.recv().unwrap(), |_| ()));
            j.reserve();
        } else {
            assert_eq!(w.prepare(&mut |_| ()).unwrap(), packed(2, 0));
            j.reserve(); // absolute order 2, still with its producer
            j.send(Record::Reload(vec![]));
            drain(&w);   // order 3 is durable
            j.reserve(); // order 4
            j.send(Record::Reload(vec![]));
            drain(&w);   // order 5 is durable
            j.reserve(); // order 6
        }
        let note = loss::Note::cut(&dir, &mut j.progress.orders.lock().unwrap()).unwrap();
        let attempt = loss::Attempt::new(note, &dir, Some(&root));
        attempt.write();
        let note_dir = if local { root.join("journal-loss").join(id) } else { plate.clone() };
        let path = fs::read_dir(&note_dir).unwrap().flatten().map(|f| f.path())
            .find(|p| p.extension().is_some_and(|e| e == "json")).unwrap();
        let bytes = fs::read(&path).unwrap();
        let note = parse(&bytes).unwrap();
        let (count, first, last) = if local { (2, 1, 2) } else { (3, packed(2, 1), packed(2, 5)) };
        let check_range = |row: &Value| {
            for (key, expected) in [("count", count), ("from", first), ("to", last)] {
                assert_eq!(row.get(key).and_then(Value::as_u64), Some(expected));
            }
            assert_eq!(row.get("prefix").and_then(Value::as_str), local.then_some("unknown"));
        };
        check_range(&note);
        assert_eq!(note.get("plate").and_then(Value::as_str), Some(id));
        assert!(note.get("time_ms").and_then(Value::as_u64).is_some());
        assert!(note.get("note").and_then(Value::as_str).is_some());
        assert!(attempt.summary().contains(&path.display().to_string().escape_debug().to_string()));
        let mut found = Vec::new();
        loss::discover(&store, Some(&root), |dir| found.push(dir));
        assert_eq!(found, [dir.clone()]);
        if local {
            fs::remove_file(&plate).unwrap();
            fs::rename(&saved, &plate).unwrap();
            // This is still the quitting writer. Its local orders cannot turn
            // into writes under a freshly discovered prefix after the cut.
            assert_eq!(w.prepare(&mut |_| ()).unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(path.exists());
        }
        drop((w, j));

        let marker = if local { packed(2, 1) } else { packed(3, 1) };
        let (_, w) = channel();
        assert_eq!(w.prepare(&mut |_| ()).unwrap(), marker);
        assert!(!path.exists());
        // Reproduce the on-disk state at death after sync but before unlink.
        fs::write(&path, &bytes).unwrap();
        let (j, w) = channel();
        assert_eq!(w.prepare(&mut |_| ()).unwrap(), if local { packed(3, 0) } else { packed(4, 0) });
        assert!(!path.exists());
        let mut reader = start(&store.root, id);
        let (rows, _) = page(&mut reader, Filter::default(), None, 10);
        assert_eq!(rows.iter().filter(|r| r.kind == Kind::Lost).count(), 1);
        let key = rows.iter().find(|r| r.kind == Kind::Lost).unwrap().key.clone();
        assert_eq!(key.position, Position::Order(marker));
        let Answer::Detail { record, .. } = ask(&mut reader, Request::Detail(key)) else { panic!("missing loss record") };
        check_range(&record);
        assert_eq!(record.get("cause").and_then(Value::as_str), Some("unwritten"));
        assert_eq!(record.get("note"), note.get("note"));

        let copy = root.join(id);
        for order in [if local { marker } else { packed(2, 2) }, marker] {
            assert_eq!(crate::journal::rebuild(&dir, Some(order), &copy).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert!(!copy.exists());
        }
        j.send(checkpoint());
        drain(&w);
        crate::journal::rebuild(&dir, Some(if local { packed(3, 1) } else { packed(4, 1) }), &copy).unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn diff_and_copy_ignore_a_zero_count_marker_and_reject_a_late_unwritten_range() {
    use crate::journal::{Change, Ordered, Reservation};
    use fab::ui::journal_panel::JournalDiffUpdate;
    let root = test_dir();
    let folder: Arc<str> = "plate".into();
    let dir = root.join(&*folder).join("journal");
    fs::create_dir_all(dir.join("blobs")).unwrap();
    let mut log = File::create(dir.join(LOG)).unwrap();
    let mut append = |row: Value| { writeln!(log, "{}", row.to_json()).unwrap(); log.sync_data().unwrap(); };
    let name = "part.splash";
    let checkpoint = Record::Checkpoint { manifest: None, files: Arc::new(BTreeMap::from([
        (name.to_owned(), Arc::from(&b"before\n"[..])),
    ])) };
    append(Ordered { reservation: Reservation { order: 1, time_ms: 0 }, checkpoint: None, record: checkpoint }.json(&dir, 0).unwrap());
    let marker = |order, from, count| obj(vec![("kind", s("lost")), ("cause", s("unwritten")),
        ("order", Value::Int(order)), ("from", Value::Int(from)), ("to", Value::Int(from)), ("count", Value::Int(count))]);
    append(marker(3, 1, 0)); // The retry for order 1 reached disk.
    let effect = Record::Effect { call: None, foreign: None, seq: 1, actor: obj(vec![("kind", s("person"))]),
        label: "edit".into(), action: None, changes: vec![Change::text(name.into(), "before\n", "after\n")] };
    append(Ordered { reservation: Reservation { order: 4, time_ms: 0 }, checkpoint: Some(1), record: effect }.json(&dir, 0).unwrap());

    let mut store = Store::default();
    let target = Target { folder: folder.clone(), generation: 1 };
    let key = Key { folder: folder.clone(), journal: test_token(&root, &folder), position: Position::Order(4) };
    store.folders.entry(folder.clone()).or_default().refresh(&root, &folder, &|| true).unwrap();
    let check = |store: &mut Store, shown: Option<&Arc<DiffVersion>>, rendered| {
        let b = Browse { root: &root, target: &target, store, current: &|| true };
        let (version, update) = b.diffs(key.position, shown).unwrap();
        let update = update.unwrap();
        let [JournalDiffUpdate::Prepared(diff)] = update.as_slice() else { panic!("expected one updated document") };
        assert_eq!(diff.text.is_some(), rendered);
        let end = b.complete_end();
        assert_eq!(b.script(&key, &key, Filter::default(), end).unwrap().contains("patch "), rendered);
        (version, end)
    };
    let (version, end) = check(&mut store, None, true);
    append(marker(5, 2, 1)); // The missing order precedes the selected effect.
    store.folders.get_mut(&folder).unwrap().refresh(&root, &folder, &|| true).unwrap();
    check(&mut store, Some(&version), false);
    let b = Browse { root: &root, target: &target, store: &mut store, current: &|| true };
    assert!(b.script(&key, &key, Filter::default(), end).unwrap().contains("patch "));
    drop(log);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsaved_session_inventory_excludes_saved_live_and_nonliteral_folders() {
    use fab_ai::plates::{Plate as StoredPlate, PlateStore, Origin};
    let root = test_dir();
    let store = PlateStore::under(&root, &root.join("copies"));
    let names = ["working-a-1-0", "working-b-1-0", "working-c-1-0", "working-d-1-0",
        "working-e-1-0", "working-f-1-0", "working-1-z-0", "working-1-2-3-extra"];
    for id in names { fs::create_dir_all(store.dir(id).join("journal")).unwrap(); }
    let dir = store.dir(names[0]).join("journal");
    fs::write(dir.join(LOG), b"{\"kind\":\"call\",\"order\":1,\"id\":1,\"time_ms\":60000}\n{\"kind\":\"lost\",\"order\":2}\n").unwrap();
    fs::write(store.dir(names[1]).join("plate"), b"malformed").unwrap();
    for (id, removed) in [(names[2], true), (names[3], false)] {
        store.write(&StoredPlate { id: id.into(), name:"Fixture".into(), file: None, saved: String::new(),
            edited: 0, modified: false, origin: Origin::Plate, printer: String::new(), removed }).unwrap();
    }
    let live = Plate::testing(&store.dir(names[4]).join("journal"), "fixture.fab.splash", &Default::default());
    let _writer = live.take_writer();
    #[cfg(unix)]
    {
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(elsewhere.join("journal")).unwrap();
        let linked = store.dir("working-11-1-0");
        fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(&elsewhere, linked.join("journal")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, store.dir("working-12-1-0")).unwrap();
    }
    let sessions = sessions::list(&store.root, names[5], &|| true).unwrap();
    assert_eq!(sessions.len(), 1, "only the unregistered, inactive session is eligible");
    assert_eq!(&*sessions[0].folder, names[0]);
    assert_eq!(sessions[0].start_ms, Some(60_000));
    assert_eq!(sessions[0].records, 2);
    assert!(sessions[0].lost, "a loss marker must be visible on the session row");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn journal_source_opens_refuse_linked_blobs_logs_and_repairs() {
    use crate::journal::replay::{replay, Mode, Request};
    let root = test_dir();
    let folder = "working-a-b-c";
    let source = root.join(folder);
    let dir = crate::journal::calls_fixture(&source, false, false, false);
    let rejects = |case: &str| {
        let copy = root.join(case);
        let result = replay(Request { journal: &dir, upto: Some(packed(1, 1)), into: &copy, mode: Mode::State }, &mut |_| {});
        assert!(result.is_err() || result.unwrap().stopped.is_some(), "{case}: State replay followed the link");
        assert!(!copy.join("piece.fab.splash").exists(), "{case}: replay produced a manifest from a link");
    };
    #[cfg(windows)]
    let junction_target = root.join("junction-target");
    #[cfg(windows)]
    fs::create_dir(&junction_target).unwrap();
    let with_link = |target: &Path, path: &Path, check: &dyn Fn()| {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, path).unwrap();
        #[cfg(windows)]
        {
            // File symlinks need administrator rights or developer mode, both unavailable
            // on the test box. A junction is also a reparse point reported as a link.
            let original = path.parent().unwrap().join(target);
            let target = if original.is_dir() { &original } else { &junction_target };
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"]).arg(path).arg(target).output().unwrap();
            assert!(output.status.success(), "mklink /J failed for {}", path.display());
            assert!(fs::symlink_metadata(path).unwrap().is_symlink(), "junction was not reported as a link at {}", path.display());
        }
        check();
        #[cfg(unix)]
        fs::remove_file(path).unwrap();
        #[cfg(windows)]
        fs::remove_dir(path).unwrap();
    };
    let link_in_place = |path: &Path, check: &dyn Fn()| {
        let saved = dir.join("fixture-target");
        fs::rename(path, &saved).unwrap();
        with_link(&saved, path, check);
        fs::rename(saved, path).unwrap();
    };
    let bytes = fs::read(source.join("piece.fab.splash")).unwrap();
    link_in_place(&dir.join("blobs").join(crate::journal::sha256_hex(&bytes)), &|| rejects("linked-blob"));
    link_in_place(&dir.join("blobs"), &|| rejects("linked-blobs-directory"));
    link_in_place(&dir.join(LOG), &|| {
        assert!(sessions::list(&root, "current", &|| true).unwrap().is_empty(), "linked log was listed");
        assert!(Index::default().refresh(&root, folder, &|| true).is_err(), "linked log was browsed");
        rejects("linked-log");
    });
    fs::write(dir.join("valid-repair"), br#"{"kind":"lost","cause":"torn","at":0,"bytes":0,"order":2}"#).unwrap();
    with_link(Path::new("valid-repair"), &dir.join("torn.pending"), &|| {
        assert!(crate::journal::Repair::pending(&dir).is_err(), "linked torn.pending supplied a repair");
        rejects("linked-repair");
    });
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn journal_unsaved_session_browse_copy_recover_and_tool_isolation() {
    use crate::chat::{journal::Browser, Calls};
    use fab::ui::journal_panel::{JournalAction as Act, JournalPage, RecordKey};
    use fab_ai::{plates::PlateStore, Session};
    use makepad_widgets::Cx;
    fn contents(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                for (p, bytes) in contents(&e.path()) { files.insert(PathBuf::from(e.file_name()).join(p), bytes); }
            } else { files.insert(PathBuf::from(e.file_name()), fs::read(e.path()).unwrap()); }
        }
        files
    }
    let _guard = crate::cx_shared();
    let root = test_dir();
    let store = PlateStore::under(&root, &root.join("copies"));
    let mut session = Session::new(crate::print::CheckSetup::fab_defaults().machine().unwrap(), store.home.clone());
    session.store = Some(store.clone());
    let plate = Plate::opened(&mut session).unwrap();
    let current = plate.reader_location().1;
    open_and_record(&store.dir(current).join("journal"), "current.fab.splash");
    let folder = "working-a-b-c";
    let source = store.dir(folder);
    let journal = crate::journal::calls_fixture(&source, false, false, false);
    let rows = crate::journal::read_rows(&journal).unwrap();
    let before = contents(&source);
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut browser = Browser::default();
    let mut page = JournalPage::default();
    let mut calls = Calls::default();
    browser.select(&mut cx, Some(&plate), "Current plate", &mut calls, &mut page);
    macro_rules! until {
        ($condition:expr) => {{
            let start = std::time::Instant::now();
            loop {
                page.drawn = true;
                browser.update(true, true, &mut page, &mut calls);
                if $condition { break }
                assert!(start.elapsed() < Duration::from_secs(5), "worker did not deliver {}", stringify!($condition));
                thread::yield_now();
            }
        }};
    }
    until!(page.rows.len() == 3);
    browser.action(&Act::Sessions, &mut page);
    until!(page.sessions.len() == 1);
    browser.action(&Act::BrowseSession(folder.into()), &mut page);
    until!(page.rows.len() == rows.len());
    let key = RecordKey { folder: folder.into(), journal: page.rows[0].key.journal, number: packed(1, 9), damaged: false };
    browser.action(&Act::Select(key), &mut page);
    until!(page.count == 1);
    assert!(page.diff_session(0).is_some(), "the session's text edit must open a diff");
    browser.action(&Act::Copy, &mut page);
    let mut clipboard;
    until!({ clipboard = browser.take_clipboard(); clipboard.is_some() });
    assert!(clipboard.unwrap().starts_with("patch "), "the session's direct edit copies as a patch");

    let rec = |f: &str, order| obj(vec![("folder", s(f)), ("journal", s(format!("{:016x}", test_token(&store.root, f)))), ("order", Value::Int(order))]);
    browser.tool(&mut calls, 7, "journal_list", &obj(vec![]));
    until!(calls.answers.contains_key(&7));
    let list = parse(calls.answers.remove(&7).unwrap().text.as_bytes()).unwrap();
    let listed = list.get("rows").unwrap().as_arr().unwrap();
    assert_eq!(listed.len(), 3, "list must stay on the current plate");
    assert!(listed.iter().all(|r| r.get("key").unwrap().get("folder").unwrap().as_str() == Some(current)));
    for (tool, args) in [
        ("journal_read", obj(vec![("record", rec(folder, packed(1, 2) as i64))])),
        ("journal_copy", obj(vec![("from", rec(folder, packed(1, 2) as i64)), ("to", rec(folder, packed(1, 2) as i64))])),
        ("journal_replay", obj(vec![("record", rec(folder, packed(1, 9) as i64))])),
    ] {
        browser.tool(&mut calls, 7, tool, &args);
        until!(calls.answers.contains_key(&7));
        assert!(!calls.answers.remove(&7).unwrap().ok, "{tool} accepted a browsed session's key");
    }
    browser.tool(&mut calls, 7, "journal_read", &obj(vec![("record", rec(current, packed(1, 2) as i64))]));
    until!(calls.answers.contains_key(&7));
    let answer = calls.answers.remove(&7).unwrap();
    assert!(answer.ok && answer.text.contains("inspect"), "read must use the current reader");
    browser.tool(&mut calls, 7, "journal_copy", &obj(vec![("from", rec(current, packed(1, 2) as i64)), ("to", rec(current, packed(1, 2) as i64))]));
    until!(calls.answers.contains_key(&7));
    let answer = calls.answers.remove(&7).unwrap();
    assert!(answer.ok && answer.text.contains("inspect"), "copy must use the current reader");
    browser.tool(&mut calls, 7, "journal_replay", &obj(vec![("record", rec(current, packed(1, 2) as i64))]));
    until!(browser.project_to_open().is_some());
    let (file, name) = browser.project_to_open().unwrap();
    assert!(file.ends_with("current.fab.splash") && name.starts_with("Current plate"), "tool replay must use the current checkpoint");
    browser.opening(Some(42), &mut page, &mut calls);
    browser.opened(42, true, &mut page, &mut calls);
    assert!(calls.answers.remove(&7).unwrap().ok);
    browser.action(&Act::Replay, &mut page);
    until!(browser.project_to_open().is_some());
    let (file, name) = browser.project_to_open().unwrap();
    let hm = |row: &Value| makepad_civil_time::local_or_utc((row.get("time_ms").unwrap().as_u64().unwrap() / 1000) as i64).hm();
    assert_eq!(name, format!("Unsaved session from {} (as of {})", hm(&rows[1]), hm(&rows[8])));
    assert!(fs::read(file.with_file_name("kept.txt")).unwrap() == b"after gesture", "recover the selected revision");
    assert!(!file.with_file_name("block.cad.splash").exists(), "a later effect must stay out of this copy");
    assert!(session.open(&file).is_ok(), "the rebuilt manifest must open as a plate");
    assert!(store.find_file(&file).is_some(), "the copy must have its own plate record");
    browser.opening(Some(43), &mut page, &mut calls);
    browser.opened(43, true, &mut page, &mut calls);
    assert!(contents(&source) == before, "reading and recovery must leave every source file unchanged");
    browser.action(&Act::ThisPlate, &mut page);
    until!(page.rows.len() == 3);
    assert!(page.rows.iter().all(|r| r.key.folder.as_ref() == current), "switching back restores the plate on the bed");
    drop(browser);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn journal_tab_removes_and_empties_only_confirmed_sessions_and_relists() {
    use crate::chat::{journal::Browser, Calls};
    use fab::ui::journal_panel::{JournalAction, JournalPage, JournalSource};
    use fab_ai::{plates::{PlateStore, REMOVED_SESSIONS}, Session};

    let _guard = crate::cx_shared();
    let root = test_dir();
    let store = PlateStore::under(&root, &root.join("copies"));
    let mut session = Session::new(crate::print::CheckSetup::fab_defaults().machine().unwrap(), store.home.clone());
    session.store = Some(store.clone());
    let plate = Plate::opened(&mut session).unwrap();
    let folder: Arc<str> = "working-a-b-c".into();
    let other = "working-a-b-d";
    let dir = store.dir(&folder).join("journal");
    open_and_record(&dir, "recover.fab.splash");
    open_and_record(&store.dir(other).join("journal"), "keep.fab.splash");

    let mut cx = makepad_widgets::Cx::new(Box::new(|_, _| {}));
    let mut browser = Browser::default();
    let mut page = JournalPage::default();
    let mut calls = Calls::default();
    browser.select(&mut cx, Some(&plate), "Current plate", &mut calls, &mut page);
    macro_rules! until {
        ($condition:expr) => {{
            let start = std::time::Instant::now();
            loop {
                page.drawn = true;
                browser.update(true, true, &mut page, &mut calls);
                if $condition { break }
                assert!(start.elapsed() < Duration::from_secs(5), "worker did not deliver {}", stringify!($condition));
                thread::yield_now();
            }
        }};
    }
    browser.action(&JournalAction::Sessions, &mut page);
    until!(page.sessions.len() == 2);
    let listed_token = |page: &JournalPage| page.sessions.iter().find(|s| s.folder == folder).unwrap().token.unwrap();
    let token = listed_token(&page);
    let press = |token| JournalAction::RemoveSession { folder: folder.clone(), token };
    browser.action(&JournalAction::BrowseSession(folder.clone()), &mut page);
    until!(!page.rows.is_empty());
    let lock = File::open(store.dir(&folder).join(crate::journal::LIFETIME)).unwrap();
    lock.try_lock_shared().unwrap();
    page.remove_confirm = Some(folder.clone());
    browser.action(&press(token), &mut page);
    until!(page.message == "This session is open in Fab right now.");
    assert!(matches!(&page.source, JournalSource::Session(s) if s.folder == folder), "a busy session stays open for browsing");
    assert!(page.remove_confirm.is_none(), "a refusal dismisses the in-place confirmation");
    drop(lock);

    // Replace just the incarnation after the displayed session was confirmed.
    let incarnation = dir.join(crate::journal::INCARNATION);
    let old = u128::from_str_radix(&fs::read_to_string(&incarnation).unwrap(), 16).unwrap();
    fs::write(incarnation, format!("{:032x}", old ^ 1)).unwrap();
    let replacement = test_token(&store.root, &folder);
    assert_ne!(replacement, token);
    browser.action(&press(token), &mut page);
    let submitted = page.revision;
    browser.action(&press(token), &mut page);
    assert_eq!(page.revision, submitted, "a repeated confirmation must not submit a second move");
    browser.action(&press(replacement), &mut page);
    assert_eq!(page.revision, submitted, "a second Remove must do nothing while the first awaits delivery");
    until!(matches!(page.source, JournalSource::Sessions) && page.sessions.len() == 2);
    assert_eq!(page.message, "This session changed. Look again.", "relisting must preserve the refusal");
    assert_eq!(listed_token(&page), replacement, "the list must carry the replacement's own token");
    assert!(!store.root.join(REMOVED_SESSIONS).exists(), "neither press may move an unconfirmed replacement");

    browser.action(&JournalAction::BrowseSession(folder.clone()), &mut page);
    until!(!page.rows.is_empty());
    browser.action(&press(replacement), &mut page);
    until!(matches!(page.source, JournalSource::Sessions) && page.sessions.len() == 1);
    assert_eq!(page.sessions[0].folder.as_ref(), other, "the fresh inventory omits just the confirmed session");
    assert!(page.message.is_empty(), "a committed move is silent");
    assert!(!dir.exists() && store.root.join(REMOVED_SESSIONS).join(&*folder).join(LOG).is_file());

    let last = page.sessions[0].clone();
    browser.action(&JournalAction::BrowseSession(last.folder.clone()), &mut page);
    until!(!page.rows.is_empty());
    browser.action(&JournalAction::RemoveSession { folder: last.folder, token: last.token.unwrap() }, &mut page);
    until!(matches!(page.source, JournalSource::Sessions));
    let moved = page.revision;
    until!(page.revision != moved); // the empty inventory has arrived
    assert!(page.sessions.is_empty() && page.message.is_empty(), "relisting after the last move is silent too");

    assert!(page.can_empty() && page.removed.len() == 2, "the list offers Empty for the two archived sessions");
    let archives = store.root.join(REMOVED_SESSIONS);
    page.empty_confirm = Some(page.removed.clone());
    fs::OpenOptions::new().append(true).open(archives.join(other).join(LOG)).unwrap().write_all(b"\n").unwrap();
    browser.action(&JournalAction::EmptyRemoved, &mut page);
    let submitted = page.revision;
    browser.action(&JournalAction::EmptyRemoved, &mut page);
    assert_eq!(page.revision, submitted, "a second Empty must not submit another worker");
    until!(page.removed.len() == 1);
    assert_eq!(page.message, "1 removed session changed since you looked. It is kept.");
    assert!(archives.join(other).is_dir() && !archives.join(&*folder).exists());

    page.empty_confirm = Some(page.removed.clone());
    let late = "working-a-b-e";
    open_and_record(&store.dir(late).join("journal"), "late.fab.splash");
    crate::journal::remove::remove_session(&store, late, test_token(&store.root, late), &mut |_| ()).unwrap();
    browser.action(&JournalAction::EmptyRemoved, &mut page);
    until!(page.removed.len() == 1 && page.removed[0].name == late);
    assert!(matches!(page.source, JournalSource::Sessions) && page.message.is_empty(),
        "deleting the confirmed list must relist silently, including the new arrival");
    assert!(!archives.join(other).exists() && archives.join(late).join(LOG).is_file());
    drop(browser);
    fs::remove_dir_all(root).unwrap();
}
