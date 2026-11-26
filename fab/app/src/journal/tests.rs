use super::*;
use std::thread;

// Hooks run only on the test's writer thread, at real filesystem operations.
type Probe = Box<dyn FnMut(&'static str) -> io::Result<()>>;
thread_local! { static PROBE: Cell<Option<Probe>> = const { Cell::new(None) }; }

pub(super) fn probe(point: &'static str) -> io::Result<()> {
    PROBE.with(|slot| {
        let Some(mut hook) = slot.take() else { return Ok(()) };
        let result = hook(point);
        slot.set(Some(hook));
        result
    })
}

fn with_probe<T>(hook: impl FnMut(&'static str) -> io::Result<()> + 'static, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { PROBE.with(|slot| slot.set(None)); } }
    let _reset = Reset;
    PROBE.with(|slot| assert!(slot.replace(Some(Box::new(hook))).is_none()));
    run()
}

pub(super) struct ReadingPrefix(pub File);
impl Read for ReadingPrefix {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        probe("reading rollover prefix")?;
        self.0.read(bytes)
    }
}

pub(crate) fn temp() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fab-journal-{}-{}-{}", std::process::id(), time_ms(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    path
}

fn call() -> Record {
    Record::Call { reference: "call-fixture".into(), actor: s("person"), action: "elec.check.list".into(), args: obj(vec![]) }
}

#[test]
fn two_producers_preserve_creation_order() {
    let root = temp();
    let dir = root.join("journal");
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 4, Arc::new(|_| {}));
    let worker = thread::spawn(move || writer.run());
    let threads: Vec<_> = (0..2).map(|_| {
        let journal = journal.clone();
        thread::spawn(move || (0..40).map(|_| journal.send(call())).collect::<Vec<_>>())
    }).collect();
    let mut sent: Vec<_> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
    drop(journal);
    worker.join().unwrap();
    sent.sort_unstable();
    let mut disk: Vec<_> = read_rows(&dir).unwrap().iter().map(|r| r.get("order").unwrap().as_u64().unwrap()).collect();
    disk.sort_unstable();
    assert_eq!(disk, sent.into_iter().map(|n| at(1, n)).collect::<Vec<_>>());
    assert_eq!(disk, (1..=80).map(|n| at(1, n)).collect::<Vec<_>>());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn two_writers_on_one_journal_keep_every_row_whole_and_uniquely_ordered() {
    let root = temp();
    let channel = |dir: &Path| Journal::channel(dir.to_owned(), Arc::default(), 4, Arc::new(|_| ()));
    let named_call = |reference: &str| Record::Call {
        reference: reference.into(), actor: s("person"), action: "elec.check.list".into(), args: obj(vec![]),
    };
    let source = b"// shared checkpoint\nuse mod.fab.*\n";
    let checkpoint = |name: &str| Record::Checkpoint {
        manifest: Some(name.into()), files: Arc::new([(name.into(), Arc::from(source.as_slice()))].into()),
    };
    let write_next = |writer: &Writer| writer.try_write(&writer.rx.try_recv().unwrap(), |_| ());
    let seed_note = |dir: &Path| {
        let (journal, writer) = channel(dir);
        journal.send(Record::Checkpoint { manifest: None, files: Arc::default() });
        journal.send(named_call("before-cut"));
        assert!(write_next(&writer) && write_next(&writer), "both rows before the cut must be durable");
        journal.send(named_call("outstanding"));
        let note = loss::Note::cut(dir, &mut journal.progress.orders.lock().unwrap()).unwrap();
        loss::Attempt::new(note, dir, None).write();
        let path = fs::read_dir(dir.parent().unwrap()).unwrap().map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|ext| ext == "json")).expect("the cut must leave a loss note");
        let bytes = fs::read(&path).unwrap();
        let note = reader::parse(&bytes).unwrap();
        (path, bytes, note.get("note").unwrap().as_str().unwrap().to_owned())
    };
    let markers = |rows: &[Value], note: &str| rows.iter().filter(|r|
        r.get("cause").and_then(Value::as_str) == Some("unwritten")
        && r.get("kind").and_then(Value::as_str) == Some("lost")
        && r.get("note").and_then(Value::as_str) == Some(note)).count();
    let unique = |rows: &[Value]| {
        let mut seen = BTreeSet::new();
        rows.iter().all(|r| r.get("order").and_then(Value::as_u64).is_some_and(|order| seen.insert(order)))
    };
    // The production scan runs headers through reader::Index and reports the
    // byte offset of each damaged, duplicate or disowned row.
    let diagnostics = |bytes: &[u8]| {
        let mut offsets = Vec::new();
        scan(bytes, reader::header, |at, row| if row.is_none() { offsets.push(at) }).unwrap();
        offsets
    };
    // Keep independent observations even when the first property fails on this writer.
    let mut failures = Vec::new();
    let mut check = |holds: bool, message: &str| if !holds { failures.push(message.to_owned()); };

    // A. Both startup scans can finish before either opening writes its checkpoint.
    let dir = root.join("interleaved/journal");
    let (path, note_bytes, note) = seed_note(&dir);
    let (a, writer_a) = channel(&dir);
    let (b, writer_b) = channel(&dir);
    for (journal, writer, name, refs) in [
        (&a, &writer_a, "a.fab.splash", ["a1", "a2"]),
        (&b, &writer_b, "b.fab.splash", ["b1", "b2"]),
    ] {
        let opening = journal.send(checkpoint(name));
        for reference in refs {
            journal.sender.push(Ordered {
                reservation: journal.reserve(), checkpoint: Some(opening), record: named_call(reference),
            });
        }
        writer.prepare(&mut |_| ()).expect("each writer must prepare its opening");
    }
    for writer in [&writer_a, &writer_b, &writer_a, &writer_a, &writer_b, &writer_b] {
        assert!(write_next(writer), "each interleaved append must succeed");
    }
    let bytes = fs::read(dir.join(LOG)).unwrap();
    let lines: Vec<_> = bytes.split_inclusive(|b| *b == b'\n').collect();
    let rows: Vec<_> = lines.iter().filter_map(|line| reader::parse(line).ok()).collect();
    check(rows.len() == lines.len() && bytes.ends_with(b"\n"), "A: every line on disk must be complete and parse");
    check(rows.len() == 9, "A: both earlier rows, the marker and all six interleaved rows must survive");
    check(unique(&rows), "A: every order on disk must be unique");
    check(diagnostics(&bytes).is_empty(), "A: the reader's Index must have no diagnostic");
    let order_of = |field, value| rows.iter().find(|r| r.get(field).and_then(Value::as_str) == Some(value))
        .and_then(|r| r.get("order")?.as_u64()).unwrap();
    let life_a = order_of("manifest", "a.fab.splash") >> LIFE_BITS;
    let life_b = order_of("manifest", "b.fab.splash") >> LIFE_BITS;
    check(life_a != life_b, "A: independent writers must claim distinct lives");
    for (life, refs) in [(life_a, ["a1", "a2"]), (life_b, ["b1", "b2"])] {
        check(refs.iter().all(|r| order_of("ref", r) >> LIFE_BITS == life), "A: each opening and its calls must share a life");
    }
    let mut lives = BTreeMap::<u64, BTreeSet<u64>>::new();
    for n in rows.iter().filter_map(|r| r.get("order")?.as_u64()) {
        lives.entry(n >> LIFE_BITS).or_default().insert(n & LIFE_MASK);
    }
    check(lives.values().all(|ns| ns.iter().copied().eq(1..=ns.len() as u64)), "A: every life must be dense from its first order");
    for name in ["a.fab.splash", "b.fab.splash"] {
        let order = rows.iter().find(|r| r.get("manifest").and_then(Value::as_str) == Some(name))
            .and_then(|r| r.get("order")?.as_u64());
        let copy = root.join(name);
        check(order.is_some_and(|at| rebuild(&dir, Some(at), &copy).is_ok()), &format!("A: rebuild must succeed at the {name} checkpoint"));
        let restored = fs::read(copy.join(name)).ok();
        check(restored.as_deref() == Some(source.as_slice()), &format!("A: rebuild must restore {name} at its checkpoint"));
    }

    // B. A consumer can die after syncing its marker and before unlinking the note.
    check(markers(&rows, &note) == 1, "B: the original note must have exactly one unwritten marker");
    check(!path.try_exists().unwrap(), "B: the consumed note file must be gone");
    fs::write(&path, note_bytes).unwrap();
    let (_, third) = channel(&dir);
    third.prepare(&mut |_| ()).expect("a third writer must prepare with the restored note");
    let rows = read_rows(&dir).unwrap();
    check(markers(&rows, &note) == 1, "B: restoring the note beside its marker must leave exactly one marker");
    check(!path.try_exists().unwrap(), "B: the restored note file must be gone");

    // C. First appends pass the barrier together, each with its own unprepared writer.
    let race = |first: Writer, second: Writer| {
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let start = |writer: Writer| {
            let barrier = barrier.clone();
            thread::spawn(move || {
                let row = writer.rx.try_recv().unwrap();
                barrier.wait();
                writer.try_write(&row, |_| ());
                writer.progress.health()
            })
        };
        let first = start(first);
        let second = start(second);
        [first.join().unwrap(), second.join().unwrap()]
    };
    for trial in 0..3 {
        let dir = root.join(format!("preparers-{trial}/journal"));
        let (_, _, note) = seed_note(&dir);
        let (a, first) = channel(&dir);
        let (b, second) = channel(&dir);
        a.send(named_call("first"));
        b.send(named_call("second"));
        let health = race(first, second);
        let rows = read_rows(&dir).unwrap();
        check(health == [Health::Ok; 2], "C: both preparers must append their row");
        check(markers(&rows, &note) == 1, "C: concurrent preparers must leave exactly one unwritten marker");
        check(unique(&rows), "C: concurrent preparers must leave every order unique");
    }

    // D. A is still prepared when the other process leaves an unterminated line.
    let at = fs::metadata(dir.join(LOG)).unwrap().len();
    let fragment = br#"{"kind":"call","ref":"dead"#;
    OpenOptions::new().append(true).open(dir.join(LOG)).unwrap().write_all(fragment).unwrap();
    a.send(named_call("after-fragment"));
    assert!(write_next(&writer_a), "D: the live writer must acknowledge its next row");
    let bytes = fs::read(dir.join(LOG)).unwrap();
    let suffix = &bytes[at as usize..];
    let rows: Vec<_> = suffix.split_inclusive(|b| *b == b'\n').filter(|line| line.ends_with(b"\n"))
        .filter_map(|line| reader::parse(line).ok()).collect();
    check(rows.iter().any(|r| r.get("ref").and_then(Value::as_str) == Some("after-fragment")),
        "D: the live writer's next row must parse on its own line");
    check(rows.iter().filter_map(torn_range).eq([at..at + fragment.len() as u64]),
        "D: one torn marker must identify the dead writer's fragment");
    check(suffix.strip_prefix(fragment).is_some_and(|rest| rest.starts_with(b"\n")),
        "D: the fragment must end at its own newline");
    check(diagnostics(&bytes).into_iter().filter(|offset| *offset >= at).eq([at]),
        "D: the reader must index exactly one diagnostic at the fragment");

    let live_order = rows.iter().find(|r| r.get("ref").and_then(Value::as_str) == Some("after-fragment"))
        .and_then(|r| r.get("order")?.as_u64());
    check(live_order.is_some_and(|n| rebuild(&dir, Some(n), &root.join("after-tear")).is_ok()),
        "D: replay through the live writer's next row must report no gap");
    check(rows.iter().any(|r| torn_range(r).is_some() && r.get("order").and_then(Value::as_u64).map(|n| n >> LIFE_BITS) == Some(life_a)),
        "D: the torn marker must belong to the live writer's life");

    // E. Model the interval between two writes of a single row on another handle.
    let flight = root.join("in-flight/journal");
    fs::create_dir_all(&flight).unwrap();
    let outer = File::create(flight.parent().unwrap().join(LIFETIME)).unwrap();
    let lock = File::create(flight.join(LOCK)).unwrap();
    lock.lock().unwrap();
    let mut log = File::create(flight.join(LOG)).unwrap();
    let half = br#"{"kind":"reload","#;
    log.write_all(half).unwrap();
    let (j, w) = channel(&flight);
    j.send(named_call("after-flight"));
    let worker = thread::spawn(move || w.try_write(&w.rx.recv().unwrap(), |_| ()));
    let start = std::time::Instant::now();
    while j.progress.lock_waits.load(Ordering::Acquire) == 0 && !worker.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(5), "E: the second writer must reach the append lock");
        thread::yield_now();
    }
    assert!(matches!(outer.try_lock(), Err(fs::TryLockError::WouldBlock)), "a writer waiting on a peer still holds shared");
    assert!(!flight.parent().unwrap().join(UNLOCKED).exists());
    drop(outer);
    check(j.progress.lock_waits.load(Ordering::Acquire) > 0, "E: the second writer must wait for the row in flight");
    check(fs::read(flight.join(LOG)).unwrap() == half, "E: the log must remain readable while the append lock is held");
    log.write_all(b"\"changes\":[],\"order\":1}\n").unwrap();
    log.sync_data().unwrap();
    drop((log, lock));
    check(worker.join().unwrap(), "E: the waiting writer must append after release");
    let rows = read_rows(&flight).unwrap();
    check(rows.len() == 2 && rows[0].get("order").and_then(Value::as_u64) == Some(1)
        && rows[1].get("ref").and_then(Value::as_str) == Some("after-flight"), "E: both complete rows must survive in file order");
    check(rows.iter().all(|r| torn_range(r).is_none()), "E: an in-flight row must never receive a torn marker");

    // Isolate blob publication from note recovery and startup. Both checkpoints need
    // the same previously absent hash. Exercise the real create/write/sync/rename.
    let blob_dir = root.join("shared-blob/journal");
    let (a, first) = channel(&blob_dir);
    let (b, second) = channel(&blob_dir);
    for (journal, writer) in [(&a, &first), (&b, &second)] {
        journal.send(checkpoint("shared.fab.splash"));
        writer.prepare(&mut |_| ()).expect("both blob writers must prepare");
    }
    let health = race(first, second);
    check(health == [Health::Ok; 2], &format!("Blob: both writers must append a checkpoint using the same new blob (writers: {health:?})"));
    let blob = fs::read(blob_dir.join("blobs").join(sha256_hex(source)));
    check(blob.as_deref().ok() == Some(source.as_slice()), "Blob: the published blob must contain the complete checkpoint bytes");

    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn rollover_rebuilds_only_its_writer_and_maps_outstanding_reservations() {
    let root = temp();
    let channel = |dir: &Path| {
        let (j, mut w) = Journal::channel(dir.to_owned(), Arc::default(), 8, Arc::new(|_| ()));
        w.block_bits = 2; // Three usable slots; rolled segments reserve the first for their base.
        (j, w)
    };
    let cp = |name: &str| Record::Checkpoint {
        manifest: Some(name.into()), files: Arc::new([(name.into(), Arc::from(b"private fixture bytes".as_slice()))].into()),
    };
    let write = |w: &Writer, messages: &mut Vec<String>| {
        assert!(w.try_write(&w.rx.try_recv().unwrap(), |s| messages.push(s)), "the queued row must sync");
    };
    let dir = root.join("complete/journal");
    let mut messages = Vec::new();
    let (a, first) = channel(&dir);
    let opening = a.send(cp("a.fab.splash"));
    let reload = |content: &[u8]| a.sender.push(Ordered { reservation: a.reserve(), checkpoint: Some(opening),
        record: Record::Reload(vec![Change::whole("part".into(), None, Some(Arc::from(content)))]) });
    a.send(Record::Reload(vec![]));
    a.send(Record::Reload(vec![]));
    for _ in 0..3 { write(&first, &mut messages); }
    let (b, second) = channel(&dir);
    b.send(cp("b.fab.splash"));
    b.send(Record::Reload(vec![]));
    for _ in 0..2 { write(&second, &mut messages); }
    reload(b"first change");
    write(&first, &mut messages);
    a.send(Record::Reload(vec![]));
    write(&first, &mut messages);
    a.send(Record::Reload(vec![]));
    write(&first, &mut messages);
    let rows = read_rows(&dir).unwrap();
    let ns: Vec<_> = rows.iter().map(|r| r.get("order").unwrap().as_u64().unwrap()).collect();
    assert_eq!(ns, [at(1,1),at(1,2),at(1,3),at(2,1),at(2,2),at(3,1),at(3,2),at(3,3),at(4,1),at(4,2)],
        "each life must have distinct, dense orders, starting with its own checkpoint");
    assert_eq!(rows[6].get("checkpoint").and_then(Value::as_u64), Some(at(1,1)), "the producer's opening keeps its absolute order");
    assert_eq!(rows[5].get("manifest"), rows[0].get("manifest"), "the rolled base must carry its opening's manifest");
    scan(File::open(dir.join(LOG)).unwrap(), reader::header, |_, r| assert!(r.is_some(), "the reader must accept every row")).unwrap();
    for (n, name, part) in [(at(3,1),"a",false),(at(3,2),"a",true),(at(3,3),"a",true),(at(4,2),"a",true),(at(2,2),"b",false)] {
        let copy = root.join(n.to_string());
        rebuild(&dir, Some(n), &copy).expect("each selected life must rebuild its own files");
        assert_eq!(fs::read(copy.join(format!("{name}.fab.splash"))).unwrap(), b"private fixture bytes");
        assert!(!copy.join(if name == "a" { "b.fab.splash" } else { "a.fab.splash" }).exists(), "a foreign opening must stay out of the copy");
        assert_eq!(fs::read(copy.join("part")).ok().as_deref(), part.then_some(b"first change".as_slice()), "later rows must apply to the rolled base");
    }
    assert_eq!(messages.len(), 4, "each rollover must log its start and finish once");
    for (pair, through) in messages.chunks_exact(2).zip([at(1,3), at(3,3)]) {
        assert_eq!(pair[0], format!("journal: {}: rollover replay started through order {through}", dir.display()));
        assert!(pair[1].contains("ended (3 rows,") && pair[1].contains(" ms, complete)"));
    }
    assert!(messages.iter().all(|m| !m.contains("private fixture bytes") && !m.contains("first change")), "replay logs must omit contents");

    let dir = root.join("incomplete/journal");
    let (a, first) = channel(&dir);
    a.send(cp("a.fab.splash"));
    write(&first, &mut messages);
    let missing = a.reserve(); // Local 2 is still held by a producer.
    a.send(Record::Reload(vec![])); // Local 3 fills life 1.
    write(&first, &mut messages);
    let (b, second) = channel(&dir);
    b.send(cp("b.fab.splash"));
    write(&second, &mut messages);
    a.reserve(); // Local 4 maps to (3,2); local 5 actually lands first.
    a.send(Record::Reload(vec![]));
    write(&first, &mut messages);
    let notice: SettleNotice = Arc::new(|_| ());
    assert_eq!(live::subscribe(&first.progress, at(2,1), &notice), 1, "a foreign life waits only for earlier local reservations");
    let copy = root.join("missing-base");
    let stop = replay::state(&dir, None, &copy, &mut |_| ()).unwrap().stopped.unwrap();
    assert_eq!(stop.order, at(3,3), "a gap names the first actual row in the life without a base");
    assert_eq!(stop.outcome, replay::Outcome::Gap { reason: "no checkpoint in the requested range".into() });
    scan(File::open(dir.join(LOG)).unwrap(), reader::header, |_, row| {
        assert!(row.is_some(), "the reader must accept a life with its rollover base absent");
    }).unwrap();
    assert!(!copy.exists());
    assert_eq!(messages.iter().filter(|s| s.contains("gap, no base")).count(), 1, "a failed rollover replay logs the gap once");
    a.reserve(); // Local 6 has no claim yet.
    let notes = loss::Note::cut(&dir, &mut a.progress.orders.lock().unwrap()).unwrap();
    loss::Attempt::new(notes, &dir, None).write();
    let mut ranges = BTreeSet::new();
    for file in fs::read_dir(dir.parent().unwrap()).unwrap().map(Result::unwrap) {
        if file.path().extension().is_some_and(|e| e == "json") {
            let note = reader::parse(&fs::read(file.path()).unwrap()).unwrap();
            let n = |k| note.get(k).unwrap().as_u64().unwrap();
            ranges.insert((note.get("prefix").is_some(), n("from"), n("to"), n("count")));
        }
    }
    assert_eq!(ranges, [(false,at(1,2),at(1,2),1),(false,at(3,2),at(3,2),1),(true,6,6,1)].into(),
        "the cut must preserve each claimed range and report the unclaimed suffix as local");
    assert!(first.try_write(&Ordered { reservation: missing, checkpoint: None, record: Record::Reload(vec![]) }, |_| ()),
        "an absolute reservation can still become durable after the cut");
    assert!(read_rows(&dir).unwrap().iter().all(|r| r.get("order").and_then(Value::as_u64) != Some(at(3,1))),
        "a previous gap must leave the new life without a base");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn damaged_life_claim_recovers_from_the_log_and_replaces_the_file() {
    let root = temp();
    for case in ["empty", "garbage", "invalid-utf8", "out-of-range", #[cfg(unix)] "link"] {
        let dir = root.join(case).join("journal");
        fs::create_dir_all(&dir).unwrap();
        let life = dir.join("life");
        File::create(dir.parent().unwrap().join(LIFETIME)).unwrap();
        fs::write(&life, "2").unwrap();
        let (seed, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
        seed.send(Record::Reload(vec![]));
        assert!(writer.try_write(&writer.rx.recv().unwrap(), |s| panic!("{s}")));

        match case {
            "empty" => fs::write(&life, []).unwrap(),
            "garbage" => fs::write(&life, "private damaged claim contents").unwrap(),
            "invalid-utf8" => fs::write(&life, [0xff]).unwrap(),
            "out-of-range" => fs::write(&life, (LAST_LIFE + 1).to_string()).unwrap(),
            #[cfg(unix)]
            "link" => {
                fs::remove_file(&life).unwrap();
                fs::write(dir.join("target"), LAST_LIFE.to_string()).unwrap();
                std::os::unix::fs::symlink(dir.join("target"), &life).unwrap();
            }
            _ => unreachable!(),
        }
        let (j, w) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
        j.send(call());
        let row = w.rx.recv().unwrap();
        let mut logs = Vec::new();
        // The failed publication leaves the damaged claim in place for a second read.
        fs::create_dir(dir.join("life.tmp")).unwrap();
        assert!(w.append(&row, &mut |s| logs.push(s)).is_err());
        fs::remove_dir(dir.join("life.tmp")).unwrap();
        assert!(w.try_write(&row, |s| logs.push(s)), "{case}: a damaged claim must let the writer record");
        assert_eq!(logs, [format!("journal: {}: InvalidData", life.display())], "{case}: log only the path and kind, once");
        let orders: Vec<_> = read_rows(&dir).unwrap().iter().map(|r| r.get("order").unwrap().as_u64().unwrap()).collect();
        assert_eq!(orders, [at(3,1), at(4,1)], "{case}: claim from the highest life in the log");
        assert!(fs::symlink_metadata(&life).unwrap().is_file());
        assert_eq!(fs::read_to_string(&life).unwrap(), "4");
        if case == "link" { assert_eq!(fs::read_to_string(dir.join("target")).unwrap(), LAST_LIFE.to_string()); }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_spent_torn_marker_local_keeps_its_life_dense_after_a_peer_repairs_the_fragment() {
    for prepared in [false, true] { spent_torn_marker_after_peer_repair(prepared); }
}

fn spent_torn_marker_after_peer_repair(prepared: bool) {
    let root = temp();
    let dir = root.join("plate/journal");
    let channel = || Journal::channel(dir.clone(), Arc::default(), 4, Arc::new(|_| ()));
    let write = |w: &Writer| assert!(w.try_write(&w.rx.try_recv().unwrap(), |_| ()), "the queued row must sync");
    let (a, mut first) = channel();
    first.block_bits = 3;
    a.send(Record::Checkpoint { manifest: None, files: Arc::default() });
    write(&first);
    let (b, second) = channel();
    if prepared { second.prepare(&mut |_| ()).unwrap(); }

    let at_byte = fs::metadata(dir.join(LOG)).unwrap().len();
    OpenOptions::new().append(true).open(dir.join(LOG)).unwrap().write_all(br#"{"kind":"call""#).unwrap();
    a.send(Record::Reload(vec![Change::whole("part".into(), None, Some(Arc::from(b"recovered file".as_slice())))]));
    let retry = first.rx.try_recv().unwrap();
    // Blocking torn.tmp fails staging after allocation. Blocking torn.pending
    // would fail the earlier tail check before the writer takes a local.
    let staging = dir.join("torn.tmp");
    fs::create_dir(&staging).unwrap();
    assert!(!first.try_write(&retry, |_| ()), "the blocked marker stage must fail");
    assert_eq!(first.torn_local.get(), Some(3));
    assert_eq!(a.send(Record::Reload(vec![])), 4, "a later reservation must pin the spent local");
    fs::remove_dir(&staging).unwrap();
    b.send(Record::Reload(vec![]));
    write(&second);
    let notes = loss::Note::cut(&dir, &mut a.progress.orders.lock().unwrap()).unwrap();
    loss::Attempt::new(notes, &dir, None).write();
    assert!(first.try_write(&retry, |_| ()), "the original producer row must survive retry");
    write(&first);
    for _ in 5..=8 { a.send(Record::Reload(vec![])); write(&first); }
    let (_, consumer) = channel();
    consumer.recover_notes(&mut |_| ()).unwrap();

    let mut rows = BTreeMap::new();
    let mut damage = Vec::new();
    scan(File::open(dir.join(LOG)).unwrap(), reader::parse, |offset, row| match row {
        Some(r) => { rows.insert(r.get("order").unwrap().as_u64().unwrap(), r); }
        None => damage.push(offset),
    }).unwrap();
    assert_eq!(damage, [at_byte], "only the original fragment may be a diagnostic");
    assert_eq!(rows.values().filter(|r| torn_range(r).is_some()).count(), 1, "the fragment gets one torn marker");
    assert!(rows.range(at(1,1)..=at(1,7)).map(|(&n, _)| n).eq(at(1,1)..=at(1,7)), "life 1 must be dense");
    let spent = &rows[&at(1,3)];
    assert_eq!(loss::file_range(spent), Some((at(1,3), at(1,3))));
    assert_eq!(spent.get("count").and_then(Value::as_u64), Some(0), "the spent local must report zero loss");
    let recovered = rows.values().find(|r| r.get("note").is_some()).expect("the cut must be acknowledged");
    assert_eq!(recovered.get("count").and_then(Value::as_u64), Some(0), "both producer retries in the cut landed");
    assert_eq!(rows[&at(3,1)].get("kind").and_then(Value::as_str), Some("checkpoint"), "rollover must publish the full base");
    for order in [at(1,3), at(3,1), at(3,2)] {
        let copy = root.join(order.to_string());
        rebuild(&dir, Some(order), &copy).expect("the spent local and rolled life must replay without a gap");
        assert_eq!(fs::read(copy.join("part")).unwrap(), b"recovered file");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exhausted_writer_lives_report_once_and_keep_the_row_pending() {
    let root = temp();
    let dir = root.join("journal");
    fs::create_dir(&dir).unwrap();
    File::create(root.join(LIFETIME)).unwrap();
    fs::write(dir.join("life"), LAST_LIFE.to_string()).unwrap();
    let (j, w) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    j.send(call());
    let row = w.rx.recv().unwrap();
    let mut logs = Vec::new();
    for _ in 0..2 {
        assert!(!w.try_write(&row, |s| logs.push(s)), "an exhausted journal must refuse the append");
        assert_eq!(w.progress.health(), Health::Failing(Failure::NoFreeLives), "the Journal tab must receive the distinct failure");
        assert_eq!(w.progress.pending(), 1, "the failed row must stay outstanding for retry");
    }
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains("this journal has no free writer lives"));
    assert_eq!(fs::metadata(dir.join(LOG)).unwrap().len(), 0, "exhaustion must not append a marker or row");
    assert_eq!(fs::read_to_string(dir.join("life")).unwrap(), LAST_LIFE.to_string());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_truncates_a_failed_append_and_the_next_row_parses() {
    let root = temp();
    let dir = root.join("journal");
    let path = dir.join(LOG);
    fs::create_dir(&dir).unwrap();
    File::create(root.join(LIFETIME)).unwrap();
    fs::write(&path, b"{\"kind\":\"reload\",\"changes\":[],\"order\":1}\n").unwrap();
    let at = fs::metadata(&path).unwrap().len();
    // The writer's failed append leaves part of a row through its append-only handle.
    let mut held = OpenOptions::new().append(true).read(true).open(&path).unwrap();
    held.write_all(b"{\"kind\":\"rel").unwrap();
    roll_back(&held, &path, at).expect("the rollback must shorten the log on every platform");
    assert_eq!(fs::metadata(&path).unwrap().len(), at, "the partial row must be gone");
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    journal.send(call());
    let mut lines = Vec::new();
    assert!(writer.try_write(&writer.rx.recv().unwrap(), |line| lines.push(line)), "the next append must succeed");
    assert!(lines.is_empty(), "a clean rollback leaves nothing to repair or report");
    let rows = read_rows(&dir).unwrap();
    assert_eq!(rows.len(), 2, "the old row and the new one, with no torn marker");
    assert_eq!(rows[1].get("kind").and_then(Value::as_str), Some("call"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn append_failure_reports_only_path_and_kind_then_retries_the_same_record() {
    let root = temp();
    let dir = root.join("plate/journal");
    let plate = Plate::testing(&dir, "board.fab.splash", &Default::default());
    let writer = plate.take_writer();
    let progress = writer_progress(&dir).unwrap();
    plate.checkpoint();
    writer.batch(writer.rx.recv().unwrap());
    let path = dir.join(LOG);
    let saved = dir.join("saved");
    fs::rename(&path, &saved).unwrap();
    fs::create_dir(&path).unwrap();
    let kind = io::ErrorKind::InvalidData;
    plate.send(call());
    let row = writer.rx.recv().unwrap();
    let mut lines = Vec::new();
    for _ in 0..2 {
        assert!(!writer.try_write(&row, |line| lines.push(line)));
        assert!(progress.write_error());
        assert_eq!(progress.health(), Health::Failing(kind.into()));
        assert_eq!(progress.pending(), 1);
    }
    assert_eq!(lines, [format!("journal: {}: {kind:?}", path.display())]);
    fs::remove_dir(&path).unwrap();
    fs::rename(saved, &path).unwrap();
    assert!(writer.try_write(&row, |_| panic!("unexpected write failure")));
    assert_eq!(progress.health(), Health::Ok);
    assert!(!progress.write_error());
    assert_eq!(progress.pending(), 0);
    assert_eq!(read_rows(&dir).unwrap().last().unwrap().get("action").and_then(Value::as_str), Some("elec.check.list"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn held_writer_retains_overflow_without_waiting() {
    let root = temp();
    let dir = root.join("journal");
    let notices = Arc::new(AtomicU64::default());
    let log = notices.clone();
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 1,
        Arc::new(move |n| { assert!(n > 0); log.fetch_add(1, Ordering::Relaxed); }));
    let (release, gate) = mpsc::channel();
    let worker = thread::spawn(move || { gate.recv().unwrap(); writer.run(); });
    for _ in 0..36 { journal.send(call()); }
    assert_eq!(notices.load(Ordering::Relaxed), 1);
    assert!(!dir.join(LOG).exists());
    drop(journal);
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(read_rows(&dir).unwrap().len(), 36);
    fs::remove_dir_all(root).unwrap();
}

fn write_checkpoint(dir: &Path, loaded: fab_model::LoadedProject) {
    let (journal, writer) = Journal::channel(dir.to_owned(), Arc::default(), 4, Arc::new(|_| {}));
    journal.send(Record::Checkpoint { manifest: Some(loaded.manifest), files: Arc::new(loaded.files) });
    drop(journal);
    writer.run();
}

#[test]
fn rebuild_keeps_the_manifest_that_was_opened() {
    let root = temp();
    let manifest = root.join("case.fab.splash");
    let original = b"// the author's opening comment\nuse mod.fab.*\n";
    fs::write(&manifest, original).unwrap();
    let mut model = fab_model::project::load(&manifest, &Default::default()).unwrap();
    fs::write(&manifest, b"// a later revision\n").unwrap();
    let journal = root.join("journal");
    write_checkpoint(&journal, model.opened_files.take().unwrap());
    let copy = root.join("copy");
    let mut steps = Vec::new();
    let report = replay::state(&journal, None, &copy, &mut |step| steps.push(step.clone())).unwrap();
    assert!(report.stopped.is_none());
    assert_eq!(report.manifest.as_deref(), Some("case.fab.splash"));
    assert_eq!(report.steps, steps.len());
    assert!(matches!(&steps[..], [step] if step.kind == replay::StepKind::Checkpoint && step.outcome == replay::Outcome::Applied));
    assert_eq!(read_rows(&journal).unwrap()[0].get("manifest").and_then(Value::as_str), Some("case.fab.splash"));
    assert_eq!(fs::read(copy.join("case.fab.splash")).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rebuild_applies_only_the_requested_prefix() {
    let root = temp();
    let dir = root.join("journal");
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 4, Arc::new(|_| {}));
    let name = "board.elec/main.pcb.splash";
    let change = |before: &[u8], after: &[u8]| Change::whole(name.into(), Some(before), Some(Arc::from(after)));
    journal.send(Record::Checkpoint { manifest: None, files: Arc::new([(name.into(), Arc::from(&b"one"[..]))].into()) });
    let call_id = journal.send(call());
    let effect = journal.send(Record::Effect { call: None, foreign: None, seq: 7, actor: s("person"), label: "type".into(), action: None, changes: vec![Change {
        name: name.into(), before: Some(sha256_hex(b"one")), after: Some(sha256_hex(b"once")),
        edits: vec![(2, 2, "c".into())], blob: None,
    }] });
    journal.send(Record::Done { call: call_id, outcome: "ok", reply: "ok".into(), seqs: vec![7], elsewhere: Default::default() });
    journal.send(Record::History { undo: true, before: Some(7), after: None, changes: vec![change(b"once", b"one")] });
    journal.send(Record::Reload(vec![change(b"one", b"outside")]));
    drop(journal);
    writer.run();
    for (bound, folder, expected) in [(Some(at(1, effect)), "point", "once"), (None, "final", "outside")] {
        let dest = root.join(folder);
        rebuild(&dir, bound, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join(name)).unwrap(), expected);
        assert!(rebuild(&dir, bound, &dest).is_err());
    }
    fs::remove_dir_all(root).unwrap();
}

// Recovery (fab-ai) captures the bytes; the app owns rebuild, so the regression runs here.
#[test]
fn rebuild_keeps_the_trailing_byte_of_a_recovered_mesh() {
    let root = temp();
    let store = fab_ai::plates::PlateStore::under(&root.join("settings"), &root.join("projects"));
    let plate = store.dir("fixture");
    fs::create_dir_all(plate.join("scripts")).unwrap();
    fs::create_dir_all(plate.join("current.meshes")).unwrap();
    // Recovery accepts readable files changed outside Fab under their previous digest.
    let previous = "0000000000000000";
    let source = b"// retained by recovery\nuse mod.fab.*\n";
    fs::write(plate.join(format!("scripts/{previous}.fab.splash")), source).unwrap();
    fs::write(plate.join(format!("scripts/{previous}.meta")), b"name\tcase\npath\tcase.fab.splash\n").unwrap();
    let mesh = b"FMSH\0\0\0\0\0\0\0\0\x7f";
    let mesh_path = plate.join(format!("current.meshes/{previous}.mesh"));
    fs::write(&mesh_path, mesh).unwrap();
    fs::write(plate.join("scripts/map"),
        format!("{previous}\tcurrent.fab.splash\n{previous}\tmeta\n{previous}\tcurrent.meshes/part.mesh\n")).unwrap();
    let mut model = store.working_model("fixture").unwrap();
    let decoded = &model.assets["part"].data;
    assert_eq!(fab_model::project::mesh_bytes(decoded), mesh[..mesh.len() - 1]);
    fs::remove_file(mesh_path).unwrap();
    let journal = plate.join("journal");
    write_checkpoint(&journal, model.opened_files.take().unwrap());
    let copy = root.join("copy");
    rebuild(&journal, None, &copy).unwrap();
    assert_eq!(fs::read(copy.join("case.meshes/part.mesh")).unwrap(), mesh);
    assert_eq!(fs::read(copy.join("case.fab.splash")).unwrap(), source);
    assert!(!copy.join("meta").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn quit_waits_for_a_refused_call_and_only_overrides_a_write_error() {
    use crate::elec_host::ElecHost;
    use fab_elec_design::By;
    use makepad_widgets::Cx;
    let _cx = crate::cx_shared();
    let mut cx = Cx::new(Box::new(|_, _| {}));
    for broken in [false, true] {
        let root = temp();
        let dir = root.join("journal");
        let plate = Plate::testing(&dir, "clean.fab.splash", &Default::default());
        let writer = plate.take_writer();
        let (open, gate) = mpsc::channel();
        let progress = writer.progress.clone();
        let worker = thread::spawn(move || {
            gate.recv().unwrap();
            let first = writer.rx.recv().unwrap();
            if broken { assert!(!writer.try_write(&first, |_| ())); }
            else { writer.batch(first); }
        });
        let mut host = ElecHost::open(&mut cx, Some(plate.clone()), 1, None, Arc::default(), Arc::default(), Arc::default(), Arc::default());
        host.run_action(&mut cx, "unregistered-private-action", vec![], By::Person, None);
        assert!(host.take_answers().is_empty());
        assert_eq!(plate.pending(), 3); // opening, refused call, done
        assert!(!dir.exists());
        let mut quit = Quit::default();
        if !broken { quit.answered(); } // exercise both orders of the save and journal acknowledgements
        let mut lines = Vec::new();
        let no_note = |_, _| panic!("no loss to record");
        assert!(!live::quit_for(&mut quit, &plate, no_note, |s| lines.push(s)));
        if broken {
            fs::write(&dir, b"occupied").unwrap();
            open.send(()).unwrap();
            while !progress.write_error.load(Ordering::Acquire) { thread::yield_now(); }
            assert!(!live::quit_for(&mut quit, &plate, no_note, |s| lines.push(s)));
            quit.repeat();
            assert!(!live::quit_for(&mut quit, &plate, |notes, done| {
                for note in notes.iter() { note.write(); }
                done.send(()).unwrap();
            }, |_| panic!("the worker has only been queued")));
            assert!(!live::quit_for(&mut quit, &plate, no_note, |_| panic!("the note finished before the model save")));
            quit.answered();
            assert!(live::quit_for(&mut quit, &plate, no_note, |s| lines.push(s)));
            assert_eq!(lines.len(), 1);
            // The summary escapes paths, which doubles a Windows separator.
            assert!(lines[0].contains("3 for ") && lines[0].contains(&root.display().to_string().escape_debug().to_string()));
            assert!(!live::quit_for(&mut quit, &plate, no_note, |_| panic!("duplicate exit")));
            fs::remove_file(&dir).unwrap();
        } else {
            quit.repeat();
            assert!(!live::quit_for(&mut quit, &plate, no_note, |s| lines.push(s)));
            open.send(()).unwrap();
        }
        worker.join().unwrap();
        if broken {
            assert_eq!(plate.pending(), 3);
        } else {
            assert_eq!(plate.pending(), 0);
            assert!(live::quit_for(&mut quit, &plate, no_note, |_| panic!("healthy writer")));
            let rows = read_rows(&dir).unwrap();
            assert!(rows.iter().any(|r| r.get("outcome").and_then(Value::as_str) == Some("refused")));
            assert_eq!(rows.len(), 3);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn demo_reserves_its_journal_privately_and_save_adopts_it() {
    use fab_ai::{plates::PlateStore, Session};
    use fab_elec_design::By;
    use fab_plate::machine::Machine;
    use makepad_widgets::Cx;
    let root = temp();
    let make = || Session::new(Machine::dual_nozzle_350(), root.join("Documents"));
    let store = PlateStore::under(&root.join("fab"), &root.join("Documents"));
    let mut remote = make();
    assert!(Plate::opened(&mut remote).is_none());

    let _cx = crate::cx_shared();
    let cx = Cx::new(Box::new(|_, _| {}));
    let demo = || {
        let mut s = make();
        s.store = Some(store.clone());
        s.open_demo(fab_model::FabModel::new("unused"), "unused").unwrap();
        s
    };
    {
        let mut s = demo();
        Plate::opened(&mut s).unwrap().attach(&cx.thread_spawner());
    }
    assert!(fs::read_dir(&store.root).unwrap().all(|f| !f.unwrap().file_type().unwrap().is_dir()), "an untouched demo has no private working directory");
    let mut session = demo();
    let plate = Plate::opened(&mut session).unwrap();
    plate.attach(&cx.thread_spawner());
    Call::begin(&plate, By::Person, "elec.check.list", arguments(&[])).done("ok", "{}".into());
    while plate.pending() != 0 { thread::yield_now(); }
    assert!(store.list().is_empty());
    let id = session.working_id.id().to_owned();
    assert_eq!(read_rows(&store.dir(&id).join("journal")).unwrap().len(), 3);
    session.save(Some(root.join("saved.fab.splash"))).unwrap();
    assert_eq!(session.plate.as_deref(), Some(id.as_str()));
    assert_eq!(store.list().len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn new_plate_import_links_the_origin_and_rebuilds_each_plate_independently() {
    use fab_elec_design::By;
    let root = temp();
    let store = fab_ai::plates::PlateStore::under(&root.join("settings"), &root.join("projects"));
    let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), root.join("documents"));
    session.store = Some(store.clone());
    let context = ModelJournal::default();
    context.install(&mut session, None);
    let manifest = root.join("first.fab.splash");
    let first = b"// as opened\nuse mod.fab.*\n";
    fs::write(&manifest, first).unwrap();
    session.open(&manifest).unwrap();
    let a = Plate::opened(&mut session).unwrap();
    let a_id = session.plate.clone().unwrap();
    let dir_a = store.dir(&a_id).join("journal");
    // A pending read gives the import the same local number as B's first read.
    let held = Call::begin(&a, By::Person, "inspect", obj(vec![]));
    let mesh = root.join("solid.obj");
    fs::write(&mesh, b"v 0 0 0\nv 5 0 0\nv 0 5 0\nv 0 0 5\nf 1 3 2\nf 1 2 4\nf 1 4 3\nf 2 3 4\n").unwrap();
    let by = By::Agent { call: 23 };
    let call = Call::begin(&a, by, "fab.import", obj(vec![("path", s(mesh.to_str().unwrap())), ("new_plate", Value::Bool(true))]));
    let reply = context.during(Some(Work::new(by, Some(&call))), || session.as_author(fab_ai::Author::Agent).import_file(&mesh, true));
    assert!(reply.ok, "new-plate import must commit");
    call.done("ok", reply.text);
    held.done("ok", "{}".into());
    let b = Plate::opened(&mut session).unwrap();
    let b_id = session.plate.clone().unwrap();
    assert_ne!(a_id, b_id);
    let dir_b = store.dir(&b_id).join("journal");
    Call::begin(&b, By::Person, "inspect", obj(vec![])).done("ok", "{}".into());
    a.measured_writer()();
    b.measured_writer()();
    let a_rows = read_rows(&dir_a).unwrap();
    let b_rows = read_rows(&dir_b).unwrap();
    let find = |rows: &[Value], key, val| rows.iter().find(|row| row.get(key).and_then(Value::as_str) == Some(val)).unwrap().clone();
    let import = find(&a_rows, "action", "fab.import");
    let local = find(&b_rows, "kind", "call");
    let effect = find(&b_rows, "kind", "effect");
    assert_eq!(import.get("id"), local.get("id"), "the two real calls must collide numerically");
    assert_ne!(import.get("ref"), local.get("ref"));
    assert_eq!(effect.get("call"), Some(&Value::Null));
    assert_eq!(effect.get("call_ref"), import.get("ref"));
    assert_eq!(effect.get("call_journal"), Some(&s(&a_id)));
    reader::tests::check_foreign(&store.root, &b_id, effect.get("order").unwrap().as_u64().unwrap(), &a_id, &import);
    let done = a_rows.iter().find(|r| r.get("outcome").is_some() && r.get("id") == import.get("id")).unwrap();
    assert_eq!(done.get("seqs").unwrap().as_arr().unwrap().len(), 0);
    let elsewhere = done.get("elsewhere").unwrap().as_arr().unwrap();
    assert_eq!(elsewhere, [obj(vec![("journal", s(&b_id)), ("seqs", Value::Arr(vec![effect.get("seq").unwrap().clone()]))])]);

    let copy_a = root.join("copy-a");
    rebuild(&dir_a, None, &copy_a).unwrap();
    assert_eq!(fs::read_dir(&copy_a).unwrap().count(), 1);
    assert_eq!(fs::read(copy_a.join("first.fab.splash")).unwrap(), first);
    let check_b = |name| {
        let copy = root.join(name);
        rebuild(&dir_b, None, &copy).unwrap();
        assert_eq!(fs::read_to_string(copy.join("model.fab.splash")).unwrap(), session.model.project_text());
        for (file, source) in &session.model.state().scripts {
            assert_eq!(fs::read_to_string(copy.join(file)).unwrap(), *source);
        }
    };
    check_b("copy-b");
    // A reader of the change stream is independent of the call-link schema.
    let mut unknown = effect;
    if let Value::Obj(fields) = &mut unknown {
        fields.retain(|(key, _)| !matches!(key.as_str(), "call" | "call_journal" | "order" | "changes"));
        let last = b_rows.iter().filter_map(|r| r.get("order").and_then(Value::as_u64)).max().unwrap();
        fields.extend([("call".into(), s("future link")), ("order".into(), number(last + 1)),
            ("changes".into(), Value::Arr(vec![]))]);
    }
    writeln!(OpenOptions::new().append(true).open(dir_b.join(LOG)).unwrap(), "{}", unknown.to_json()).unwrap();
    check_b("copy-b-future");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn undelivered_and_cancelled_panel_replies_finish_once() {
    let root = temp();
    let dir = root.join("journal");
    let plate = Plate::testing(&dir, "model.fab.splash", &Default::default());
    drop(PersonReply::begin(&plate, "abandoned", obj(vec![])));
    for (tool, ok, text) in [("finished", true, "{}"), ("failed", false, "{error: \"refused\"}"),
        ("stopped", false, "{error: \"cancelled\"}")] {
        let work = PersonReply::begin(&plate, tool, obj(vec![]));
        let reply = work.person.clone().unwrap();
        drop(work);
        reply.receive(ok, text);
    }
    plate.measured_writer()();
    let rows = read_rows(&dir).unwrap();
    let outcomes: Vec<_> = rows.iter().filter_map(|r| r.get("outcome")?.as_str()).collect();
    assert_eq!(outcomes, ["cancelled", "ok", "refused", "cancelled"]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn third_quit_leaves_without_waiting_for_the_loss_note_worker() {
    let root = temp();
    let dir = root.join("plate/journal");
    let plate = Plate::testing(&dir, "model.fab.splash", &Default::default());
    let writer = plate.take_writer();
    plate.checkpoint();
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir(dir.join(LOG)).unwrap();
    assert!(!writer.try_write(&writer.rx.recv().unwrap(), |_| ()));
    let mut quit = Quit::default();
    let mut held = None;
    quit.repeat();
    assert!(!live::quit_for(&mut quit, &plate, |notes, done| held = Some((notes, done)), |_| panic!("still writing")));
    assert!(held.is_some());
    let no_second_worker = |_, _| panic!("the note is already queued");
    assert!(!live::quit_for(&mut quit, &plate, no_second_worker, |_| panic!("still writing")));
    quit.repeat();
    assert!(!live::quit_for(&mut quit, &plate, no_second_worker, |_| panic!("the model save is still pending")));
    quit.answered();
    let mut lines = Vec::new();
    assert!(live::quit_for(&mut quit, &plate, no_second_worker, |msg| lines.push(msg)));
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("1 for plate") && lines[0].contains("paths tried: none"));
    assert!(!live::quit_for(&mut quit, &plate, no_second_worker, |_| panic!("already exited")));
    drop(held);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_foreign_loss_marker_does_not_interrupt_rebuild_of_local_rows() {
    let root = temp();
    let dir = root.join("journal");
    fs::create_dir(&dir).unwrap();
    let checkpoint = obj(vec![("kind", s("checkpoint")), ("order", number(at(1, 1))), ("files", Value::Arr(vec![]))]);
    let reload = |order| obj(vec![("kind", s("reload")), ("order", number(order)), ("changes", Value::Arr(vec![]))]);
    let mut results = Vec::new();
    let other = s("0123456789abcdef0123456789abcdef");
    for (marker_order, selected, origin) in [(at(2, 1), at(1, 2), other.clone()), (at(1, 2), at(1, 3), other), (at(1, 2), at(1, 3), Value::Null)] {
        let marker = obj(vec![("kind", s("lost")), ("cause", s("unwritten")), ("order", number(marker_order)),
            ("count", number(1)), ("from", number(at(1, 2))), ("to", number(at(1, 2))),
            ("journal", origin)]);
        assert_eq!(loss::file_range(&marker), None, "the original range belongs to another journal");
        let mut log = File::create(dir.join(LOG)).unwrap();
        let effect = reload(selected);
        let rows = if marker_order < selected { [&checkpoint, &marker, &effect] } else { [&checkpoint, &effect, &marker] };
        for row in rows { writeln!(log, "{}", row.to_json()).unwrap(); }
        drop(log);
        results.push(rebuild(&dir, Some(selected), &root.join(results.len().to_string())).map_err(|e| e.kind()));
    }
    fs::remove_dir_all(root).unwrap();
    assert_eq!(results[0], Ok(()), "a separate recovery life cannot interrupt the local selection");
    assert_eq!(results[1], Ok(()), "a foreign marker inside the selection is evidence about another journal");
    assert_eq!(results[2], Ok(()), "an unattributed older note cannot interrupt a selected revision either");
}

#[test]
fn a_replacement_journals_reused_orders_cannot_discount_a_delayed_note() {
    let root = temp();
    let dir = root.join("plate/journal");
    let channel = || Journal::channel(dir.clone(), Arc::default(), 4, Arc::new(|_| ()));
    let (a, old) = channel();
    a.send(call());
    assert!(old.try_write(&old.rx.recv().unwrap(), |_| ()));
    let original = fs::read_to_string(dir.join(INCARNATION)).unwrap();
    a.reserve();
    a.reserve();
    let cut = loss::Note::cut(&dir, &mut a.progress.orders.lock().unwrap()).unwrap();
    let delayed = loss::Attempt::new(cut, &dir, None);
    drop((old, a));

    let archive = root.join("archived");
    fs::rename(&dir, &archive).unwrap();
    let (b, new) = channel();
    let files = [("part.splash".to_owned(), Arc::from(b"replacement".as_slice()))].into();
    b.send(Record::Checkpoint { manifest: None, files: Arc::new(files) });
    b.send(Record::Reload(vec![]));
    for _ in 0..2 { assert!(new.try_write(&new.rx.recv().unwrap(), |_| ())); }
    drop((new, b));
    let current = fs::read_to_string(dir.join(INCARNATION)).unwrap();
    assert_ne!(original, current, "a fresh journal at the same path needs a fresh id");
    delayed.write();
    let plate = dir.parent().unwrap();
    let note = fs::read_dir(plate).unwrap().map(|e| e.unwrap().path())
        .find(|path| path.extension().is_some_and(|e| e == "json")).unwrap();
    assert_eq!(reader::parse(&fs::read(&note).unwrap()).unwrap().get("journal").and_then(Value::as_str), Some(original.as_str()));

    let (_, recovery) = channel();
    recovery.recover_notes(&mut |_| ()).unwrap();
    let rows = read_rows(&dir).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].get("order").and_then(Value::as_u64), Some(at(1, 2)), "the replacement really reused the first lost order");
    let marker = &rows[2];
    assert_eq!(marker.get("kind").and_then(Value::as_str), Some("lost"));
    assert_eq!(marker.get("journal").and_then(Value::as_str), Some(original.as_str()));
    assert_eq!(marker.get("count").and_then(Value::as_u64), Some(2), "neither old loss belongs to the replacement");
    assert_eq!(loss::file_range(marker), None);
    assert!(!note.exists());
    let copy = root.join("copy");
    rebuild(&dir, None, &copy).unwrap();
    assert_eq!(fs::read(copy.join("part.splash")).unwrap(), b"replacement");
    assert_eq!(read_rows(&archive).unwrap().len(), 1);
    drop(recovery);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn changed_incarnation_permanently_refuses_a_bound_writer() {
    let root = temp();
    let dir = root.join("journal");
    let (j, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    j.send(call());
    assert!(writer.try_write(&writer.rx.recv().unwrap(), |_| ()));
    let path = dir.join(INCARNATION);
    let original = fs::read(&path).unwrap();
    let log = fs::read(dir.join(LOG)).unwrap();
    let life = fs::read(dir.join("life")).unwrap();
    j.send(call());
    let pending = writer.rx.recv().unwrap();
    fs::write(&path, Incarnation::mint().to_string()).unwrap();
    let mut messages = Vec::new();
    assert!(writer.prepare(&mut |s| messages.push(s)).is_err(), "an existing segment must still check its journal");
    assert_eq!(j.progress.health(), Health::Failing(Failure::IncarnationChanged));
    fs::write(&path, original).unwrap();
    assert!(!writer.try_write(&pending, |s| messages.push(s)), "restoring the file cannot revive a displaced writer");
    assert_eq!(messages.len(), 1);
    assert_eq!(j.progress.pending(), 1);
    assert_eq!(fs::read(dir.join(LOG)).unwrap(), log);
    assert_eq!(fs::read(dir.join("life")).unwrap(), life);
    drop(writer);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollover_rechecks_the_incarnation_before_publishing_a_base() {
    let root = temp();
    let dir = root.join("journal");
    let (j, mut writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    writer.block_bits = 2;
    for row in [Record::Checkpoint { manifest: None, files: Arc::default() }, Record::Reload(vec![]), Record::Reload(vec![])] {
        j.send(row);
        assert!(writer.try_write(&writer.rx.recv().unwrap(), |_| ()));
    }
    let before = fs::read(dir.join(LOG)).unwrap();
    j.send(call());
    let pending = writer.rx.recv().unwrap();
    let incarnation = dir.join(INCARNATION);
    assert!(!with_probe(move |point| {
        if point == "reading rollover prefix" { fs::write(&incarnation, Incarnation::mint().to_string())?; }
        Ok(())
    }, || writer.try_write(&pending, |_| ())));
    assert_eq!(j.progress.health(), Health::Failing(Failure::IncarnationChanged));
    assert_eq!(j.progress.pending(), 1);
    assert!(writer.pending_bases.borrow()[0].snapshot.is_none());
    assert_eq!(fs::read(dir.join(LOG)).unwrap(), before, "no base or triggering row can be published after the change");
    drop(writer);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn old_journal_gets_one_durable_incarnation_and_keeps_its_checkpoint() {
    for corrupt in [false, true] {
        let root = temp();
        let dir = root.join("journal");
        fs::create_dir_all(dir.join("blobs")).unwrap();
        File::create(root.join(LIFETIME)).unwrap();
        let files = [("legacy.splash".to_owned(), Arc::from(b"original".as_slice()))].into();
        let opening = Ordered { reservation: Reservation { order: 1, time_ms: 0 }, checkpoint: None,
            record: Record::Checkpoint { manifest: None, files: Arc::new(files) } };
        let old = format!("{}\n", opening.json(&dir, 0).unwrap().to_json());
        fs::write(dir.join(LOG), &old).unwrap();
        let path = dir.join(INCARNATION);
        if corrupt { fs::write(&path, [0xff; 32]).unwrap(); }
        let channel = || Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
        let (j, first) = channel();
        j.send(call());
        let row = first.rx.recv().unwrap();
        let mut warnings = Vec::new();
        let observing = dir.clone();
        assert!(!with_probe(move |point| {
            if point == "incarnation directory sync" {
                let row_lock = File::open(observing.join(LOCK))?;
                assert!(matches!(row_lock.try_lock(), Err(fs::TryLockError::WouldBlock)), "the id must be published under the row lock");
                return Err(io::ErrorKind::StorageFull.into())
            }
            Ok(())
        }, || first.try_write(&row, |s| warnings.push(s))));
        let id = fs::read_to_string(&path).unwrap();
        assert!(Incarnation::parse(&id).is_some());
        assert!(!dir.join("life").exists(), "a life cannot be claimed until the id and its directory are durable");
        assert!(first.try_write(&row, |s| warnings.push(s)));
        let (_, second) = channel();
        second.prepare(&mut |s| warnings.push(s)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), id);
        assert_eq!(warnings.len(), 1 + usize::from(corrupt), "one injected I/O failure, plus one warning for a malformed id");
        if corrupt { assert!(warnings[0].contains(&format!("{path:?}"))); }
        assert!(fs::read(dir.join(LOG)).unwrap().starts_with(old.as_bytes()));
        let copy = root.join("copy");
        rebuild(&dir, Some(1), &copy).unwrap();
        assert_eq!(fs::read(copy.join("legacy.splash")).unwrap(), b"original");
        drop((first, second));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn loss_note_discounts_a_retry_that_became_durable_before_exit() {
    let root = temp();
    for all in [false, true] {
        let dir = root.join(if all { "plates/all/journal" } else { "plates/some/journal" });
        let channel = || {
            let (journal, mut writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
            writer.settings = Some(root.clone());
            (journal, writer)
        };
        let (j, w) = channel();
        assert_eq!(w.prepare(&mut |_| ()).unwrap(), at(1, 0));
        j.send(Record::Checkpoint { manifest: None, files: Arc::default() });
        let row = w.rx.recv().unwrap();
        let second = j.reserve();
        let note = loss::Note::cut(&dir, &mut j.progress.orders.lock().unwrap()).unwrap();
        loss::Attempt::new(note, &dir, Some(&root)).write();
        assert!(w.try_write(&row, |_| panic!("the retry can write")));
        if all {
            let row = Ordered { reservation: second, checkpoint: None, record: call() };
            assert!(w.try_write(&row, |_| panic!("the retry can write")));
        }
        drop((j, w));
        let (_, w) = channel();
        assert_eq!(w.prepare(&mut |_| ()).unwrap(), at(2, 1));
        let rows = read_rows(&dir).unwrap();
        let loss = rows.iter().find(|r| r.get("cause").and_then(Value::as_str) == Some("unwritten")).unwrap();
        let (count, from) = if all { (0, 1) } else { (1, 2) };
        for (key, n) in [("count", count), ("from", at(1, from)), ("to", at(1, 2))] {
            assert_eq!(loss.get(key).and_then(Value::as_u64), Some(n));
        }
        assert_eq!(rebuild(&dir, None, &root.join("copy")).is_ok(), all);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prefix_unknown_loss_note_refuses_any_journal_field() {
    let root = temp();
    let dir = root.join("journal");
    let channel = || Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    let (j, _) = channel();
    j.reserve();
    let notes = loss::Note::cut(&dir, &mut j.progress.orders.lock().unwrap()).unwrap();
    loss::Attempt::new(notes, &dir, None).write();
    let path = fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|s| s == "json")).unwrap();
    let original = fs::read(&path).unwrap();
    let note = reader::parse(&original).unwrap();
    assert!(note.get("journal").is_none() && note.get("prefix").and_then(Value::as_str) == Some("unknown"));
    for field in [s(Incarnation::mint().to_string()), Value::Null] {
        let Value::Obj(mut fields) = note.clone() else { panic!("expected the on-disk note") };
        fields.push(("journal".into(), field));
        let damaged = Value::Obj(fields).to_json();
        fs::write(&path, &damaged).unwrap();
        let (_, recovery) = channel();
        recovery.recover_notes(&mut |_| ()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), damaged.as_bytes(), "a rejected note must remain intact");
        assert_eq!(fs::metadata(dir.join(LOG)).unwrap().len(), 0);
    }
    fs::write(&path, original).unwrap();
    let (_, recovery) = channel();
    recovery.recover_notes(&mut |_| ()).unwrap();
    assert!(!path.exists(), "the original prefix-unknown note is still accepted");
    let rows = read_rows(&dir).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get("journal").is_none() && rows[0].get("prefix").and_then(Value::as_str) == Some("unknown"));
    drop(recovery);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn absolute_loss_notes_never_recreate_a_deleted_adopted_plate() {
    let root = temp();
    let store = fab_ai::plates::PlateStore::under(&root, &root.join("projects"));
    let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), store.home.clone());
    session.store = Some(store.clone());
    let id = session.working_id.id().to_owned();
    let project = session.save(None).unwrap();
    assert_eq!(session.plate.as_deref(), Some(id.as_str()), "Save must adopt the original working id");
    let plate = store.dir(&id);
    let dir = plate.join("journal");
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    journal.send(call());
    assert!(writer.try_write(&writer.rx.recv().unwrap(), |_| ()));

    journal.send(call());
    cut_note(&journal, &dir, &root).write();
    let fallback = root.join("journal-loss").join(&id);
    fs::create_dir_all(&fallback).unwrap();
    let primary = fs::read_dir(&plate).unwrap().map(Result::unwrap)
        .find(|e| e.file_name().to_string_lossy().starts_with("loss-")).unwrap();
    let early = fallback.join(primary.file_name());
    fs::rename(primary.path(), &early).unwrap();
    let bytes = fs::read(&early).unwrap();
    assert_eq!(reader::parse(&bytes).unwrap().get("from").and_then(Value::as_u64), Some(at(1, 2)));
    assert!(writer.try_write(&writer.rx.recv().unwrap(), |_| ()));
    let log = fs::read(dir.join(LOG)).unwrap();
    journal.reserve();
    let late = cut_note(&journal, &dir, &root);
    drop(writer); // Release the journal handles so Plate Delete can move it on Windows.
    // A recovery queued before Delete may reach its first filesystem operation after it.
    let mut queued = Vec::new();
    loss::discover_for_recovery(&store, &mut |_| panic!("the saved plate exists"), |dir, create| queued.push((dir, create)));
    assert_eq!(queued.iter().map(|(path, _)| path).collect::<Vec<_>>(), [&dir]);
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    store.delete(&id, &|path| fs::rename(path, bin.join(path.file_name().unwrap())).map_err(|e| e.to_string())).unwrap();
    assert!(!plate.exists() && !project.exists());
    let store_before = directory_bytes(&store.root); // Save also wrote the current-plate pointer.
    late.write();
    assert!(!plate.exists(), "writing the second note must leave the deleted plate absent");
    let before = directory_bytes(&fallback);
    assert_eq!(before.len(), 2, "the note cut from the saved writer must reach the fallback too");
    assert_eq!(before.get(&early), Some(&bytes));
    let late_bytes = before.iter().find(|(p, _)| **p != early).unwrap().1;
    let late_note = reader::parse(late_bytes).unwrap();
    assert_eq!(late_note.get("from").and_then(Value::as_u64), Some(at(1, 3)));
    assert_eq!(late_note.get("plate").and_then(Value::as_str), Some(id.as_str()));

    let mut lines = Vec::new();
    for (dir, create) in queued {
        recover_discovered_writer(&store, dir, create, &mut lines);
    }
    assert!(!plate.exists(), "a recovery queued before Delete must not recreate the adopted plate");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with(&format!("{plate:?}")), "the skip names the missing folder");
    loss::discover_for_recovery(&store, &mut |line| lines.push(line), |_, _| panic!("a missing saved plate must not wake a writer"));
    assert_eq!(lines.len(), 1, "the next discovery must not log the same path again");
    assert!(!plate.exists(), "discovery must leave the deleted plate absent");
    assert_eq!(directory_bytes(&fallback), before, "both fallback notes must survive recovery byte for byte");
    assert_eq!(directory_bytes(&store.root), store_before, "nothing else may be written to the store");
    assert_eq!(fs::read(bin.join(&id).join("journal").join(LOG)).unwrap(), log);
    fs::remove_dir_all(root).unwrap();
}

fn directory_bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fs::read_dir(dir).unwrap().map(|entry| {
        let path = entry.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        (path, bytes)
    }).collect()
}

fn cut_note(journal: &Journal, dir: &Path, settings: &Path) -> loss::Attempt {
    let notes = loss::Note::cut(dir, &mut journal.progress.orders.lock().unwrap()).unwrap();
    loss::Attempt::new(notes, dir, Some(settings))
}

fn recover_discovered_writer(store: &fab_ai::plates::PlateStore, dir: PathBuf, create_plate: bool, lines: &mut Vec<String>) {
    let (_, mut writer) = Journal::channel(dir, Arc::default(), 1, Arc::new(|_| ()));
    writer.settings = store.root.parent().map(Path::to_owned);
    writer.create_plate = create_plate;
    writer.recover_notes(&mut |line| lines.push(line)).unwrap();
}

#[test]
fn fallback_only_working_session_recovers_one_count_only_marker() {
    let root = temp();
    let store = fab_ai::plates::PlateStore::under(&root, &root.join("projects"));
    let working = fab_ai::WorkingId::default();
    let plate = store.dir(working.id());
    let dir = plate.join("journal");
    let (journal, _) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    journal.reserve();
    journal.reserve();
    let notes = loss::Note::cut(&dir, &mut journal.progress.orders.lock().unwrap()).unwrap();
    loss::Attempt::new(notes, &dir, Some(&root)).write();
    let fallback = root.join("journal-loss").join(working.id());
    assert!(!plate.exists(), "the note itself must create only the fallback");
    assert_eq!(fs::read_dir(&fallback).unwrap().count(), 1);
    let mut lines = Vec::new();
    loss::discover_for_recovery(&store, &mut |_| panic!("a working id can be recovered"), |dir, create| {
        recover_discovered_writer(&store, dir, create, &mut lines);
    });
    assert!(lines.is_empty());
    assert!(fs::read_dir(&fallback).unwrap().next().is_none(), "recovery must consume the fallback note");
    loss::discover_for_recovery(&store, &mut |_| panic!("no note left"), |_, _| panic!("the note was already consumed"));
    let rows = read_rows(&dir).unwrap();
    assert_eq!(rows.len(), 1);
    let marker = &rows[0];
    assert_eq!(marker.get("kind").and_then(Value::as_str), Some("lost"));
    assert_eq!(marker.get("cause").and_then(Value::as_str), Some("unwritten"));
    assert_eq!(marker.get("count").and_then(Value::as_u64), Some(2));
    assert_eq!(marker.get("prefix").and_then(Value::as_str), Some("unknown"));
    assert_eq!(loss::file_range(marker), None, "the loss has no absolute order range");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mixed_loss_notes_keep_a_missing_working_folder_absent() {
    let root = temp();
    let store = fab_ai::plates::PlateStore::under(&root, &root.join("projects"));
    let working = fab_ai::WorkingId::default();
    let plate = store.dir(working.id());
    let dir = plate.join("journal");
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    journal.send(call());
    assert!(writer.try_write(&writer.rx.recv().unwrap(), |_| ()));
    journal.reserve();
    let absolute = cut_note(&journal, &dir, &root);
    drop(writer);
    fs::rename(&plate, root.join("moved")).unwrap();

    let (unprepared, _) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    unprepared.reserve();
    cut_note(&unprepared, &dir, &root).write();
    let mut queued = Vec::new();
    loss::discover_for_recovery(&store, &mut |_| panic!("only a prefix-unknown note is present"), |dir, create| queued.push((dir, create)));
    assert_eq!(queued, [(dir.clone(), true)]);
    absolute.write(); // The earlier discovery's permission to create is now stale.
    let fallback = root.join("journal-loss").join(working.id());
    let before = directory_bytes(&fallback);
    assert_eq!(before.len(), 2);
    assert_eq!(before.values().filter(|bytes| reader::parse(bytes).unwrap().get("prefix").is_some()).count(), 1);

    let mut lines = Vec::new();
    loss::discover_for_recovery(&store, &mut |line| lines.push(line), |_, _| panic!("mixed notes must not wake a writer"));
    for (dir, create) in queued { recover_discovered_writer(&store, dir, create, &mut lines); }
    assert_eq!(lines.len(), 1, "discovery and the stale writer report the missing path only once");
    assert!(!plate.exists(), "an absolute note must also veto creation at the writer");
    assert_eq!(directory_bytes(&fallback), before, "both kinds of note must stay intact");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_loss_note_is_logged_once_and_the_plate_keeps_recording() {
    let root = temp();
    let plate = root.join("fixture");
    let dir = plate.join("journal");
    fs::create_dir(&plate).unwrap();
    let path = plate.join("loss-damaged.json");
    fs::write(&path, b"{unreadable").unwrap();

    let mut lines = Vec::new();
    // An idle discovery and separate producer openings can see the same note.
    let channel = || Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    let (_, idle) = channel();
    idle.recover_notes(&mut |line| lines.push(line)).unwrap();
    for _ in 0..2 {
        let (journal, writer) = channel();
        journal.send(call());
        assert!(writer.try_write(&writer.rx.recv().unwrap(), |line| lines.push(line)));
    }
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(&format!("{path:?}")), "the skipped note is named by its escaped path");
    assert!(!lines[0].contains("unreadable"));
    assert_eq!(fs::read(&path).unwrap(), b"{unreadable");
    let rows = read_rows(&dir).unwrap();
    assert_eq!(rows.len(), 2);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.get("kind").and_then(Value::as_str), Some("call"));
        assert_eq!(row.get("order").and_then(Value::as_u64), Some(at(i as u64 + 1, 1)));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unappliable_loss_note_leaves_room_for_valid_notes_and_calls() {
    let root = temp();
    let dir = root.join("journal");
    let plate = root.file_name().unwrap().to_str().unwrap();
    fs::create_dir(&dir).unwrap();
    File::create(root.join(LIFETIME)).unwrap();
    let current = Incarnation::mint().to_string();
    fs::write(dir.join(INCARNATION), &current).unwrap();
    // The first note parses but its marker would exceed the journal order type.
    let write_note = |id, order, origin: Option<&str>| {
        let path = root.join(format!("loss-{id}.json"));
        let mut fields = vec![("note", s(id)), ("plate", s(plate)), ("time_ms", number(0)), ("count", number(1)),
            ("from", number(order)), ("to", number(order))];
        if let Some(origin) = origin { fields.push(("journal", s(origin))); }
        fs::write(&path, obj(fields).to_json()).unwrap();
        path
    };
    let stuck = write_note("a", i64::MAX as u64, Some(&current));
    let good = write_note("b", 1, Some(&current));
    let older = write_note("c", i64::MAX as u64, None);
    let other = Incarnation::mint().to_string();
    let foreign = write_note("d", at(LAST_LIFE - 1, 2), Some(&other));
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    journal.send(call());
    let mut lines = Vec::new();
    assert!(writer.try_write(&writer.rx.recv().unwrap(), |line| lines.push(line)));
    assert!(stuck.exists() && !good.exists() && !older.exists() && !foreign.exists());
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(&format!("{stuck:?}")), "the skipped note is named by its escaped path");
    let rows = read_rows(&dir).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].get("note").and_then(Value::as_str), Some("b"));
    assert!(rows[0].get("journal").is_none(), "a same-journal marker retains its range");
    assert!(rows[1].get("journal").is_some_and(Value::is_null), "an older absolute note is explicitly unattributed");
    assert_eq!(rows[2].get("journal").and_then(Value::as_str), Some(other.as_str()));
    for marker in &rows[1..3] {
        assert_eq!(loss::file_range(marker), None);
        assert_eq!(marker.get("count").and_then(Value::as_u64), Some(1));
    }
    assert_eq!(rows[3].get("kind").and_then(Value::as_str), Some("call"));
    assert_eq!(rows[3].get("order").and_then(Value::as_u64), Some(at(1, 4)), "foreign ranges cannot exhaust or advance this journal's life counter");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn loss_without_a_plate_path_still_reports_the_count() {
    let dir = Path::new("");
    let (journal, _) = Journal::channel(dir.to_owned(), Arc::default(), 1, Arc::new(|_| ()));
    journal.reserve();
    let note = loss::Note::cut(dir, &mut journal.progress.orders.lock().unwrap()).unwrap();
    let attempt = loss::Attempt::new(note, dir, None);
    attempt.write();
    assert!(attempt.summary().contains("1 for unknown plate"));
    assert!(attempt.summary().contains("paths tried: none"));
}

fn model_save_at_quit(gone: bool) {
    use crate::model_host::{HostAction, ModelHost};
    use makepad_widgets::{Cx, Event};
    use std::time::Instant;

    let _owner = crate::cx_owned();
    let (send, answers) = mpsc::channel();
    let mut cx = Cx::new(Box::new(move |_, event| {
        if let Event::Actions(actions) = event {
            for action in actions {
                match action.downcast_ref::<HostAction>() {
                    Some(HostAction::Quit { id }) => send.send((*id, false)).unwrap(),
                    Some(HostAction::QuitHostGone { id }) => send.send((*id, true)).unwrap(),
                    _ => (),
                }
            }
        }
    }));
    if gone { cx.use_closed_task_pool(); }
    let mut host = ModelHost::start(&cx, false);
    let root = temp();
    let dir = root.join("plate/journal");
    let plate = Plate::testing(&dir, "model.fab.splash", &Default::default());
    let _writer = plate.take_writer();
    let mut quit = Quit::default();
    let asked = Instant::now();
    quit.begin(asked);
    host.hold_document_changes(true);
    let id = host.quit(None);
    assert!(host.document_waits());
    let no_note = |_, _| panic!("no loss note expected");
    let no_log = |_| panic!("no loss expected");
    let later = asked + Duration::from_secs(400);
    assert!(quit.saving_line(asked).unwrap().contains("Saving the plate"));
    assert!(quit.saving_line(asked + Duration::from_millis(800)).is_none());
    assert!(quit.saving_line(asked + Duration::from_secs(4)).unwrap().contains("4 s"));
    assert!(!live::quit_for_at(&mut quit, &plate, later, no_note, no_log));
    cx.handle_action_receiver();
    assert!(answers.try_recv().is_err());

    // On a closed pool the released job is dropped in Strand::submit. On
    // the live pool it runs the save. Both deliver exactly one tagged answer.
    if gone {
        quit.repeat();
        quit.repeat();
        assert!(!live::quit_for_at(&mut quit, &plate, later, no_note, no_log));
    }
    host.hold_document_changes(false);
    let watch = Instant::now();
    let outcome = loop {
        cx.handle_action_receiver();
        if let Ok(answer) = answers.try_recv() { break answer }
        assert!(watch.elapsed() < Duration::from_secs(60), "quit job did not answer");
        thread::yield_now();
    };
    assert_eq!(outcome, (id, gone));
    assert!(answers.try_recv().is_err());
    if gone {
        plate.checkpoint(); // healthy writer, one reserved record
        let line = quit.host_gone(later);
        assert!(line.contains("host stopped") && line.contains("unsaved") && line.contains("Journal entries kept"));
        let mut writing = None;
        let mut lines = Vec::new();
        assert!(!live::quit_for_at(&mut quit, &plate, later, |notes, done| writing = Some((notes, done)), |s| lines.push(s)));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("journals=1, outstanding=1") && !lines[0].contains(dir.to_str().unwrap()));
        let after_display = later + Duration::from_secs(60);
        assert!(!live::quit_for_at(&mut quit, &plate, after_display, no_note, no_log));
        let (notes, done) = writing.unwrap();
        for note in notes.iter() { note.write(); }
        done.send(()).unwrap();
        assert!(fs::read_dir(dir.parent().unwrap()).unwrap().flatten().any(|f| f.file_name().to_string_lossy().starts_with("loss-")));
        assert!(live::quit_for_at(&mut quit, &plate, after_display, no_note, no_log));
    } else {
        quit.answered();
        assert!(live::quit_for_at(&mut quit, &plate, later, no_note, no_log));
    }
    assert!(quit.saving_line(later).is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn slow_model_save_shows_elapsed_until_its_answer() { model_save_at_quit(false); }

#[test]
fn dropped_model_quit_reports_unsaved_edits_and_writes_the_loss_note() { model_save_at_quit(true); }

/// A real model tool supplies the source and the effect bytes for the journal. The
/// optional println is part of the replayed script edit, after the checkpoint.
pub(crate) fn calls_fixture(root: &Path, print: bool, corrupt: bool, reserved: bool) -> PathBuf {
    let manifest = "piece.fab.splash";
    let initial = "// source fixture\nuse mod.fab.*\n";
    let path = root.join(manifest);
    fs::create_dir_all(root).unwrap();
    fs::write(&path, initial).unwrap();
    let mut session = fab_ai::Session::new(crate::print::CheckSetup::fab_defaults().machine().unwrap(), root.to_owned());
    session.open(&path).unwrap();
    let mut files = Files::from([(manifest.to_string(), Arc::from(initial.as_bytes()))]);
    if reserved { files.insert(replay::REPORT_FILE.into(), Arc::from(&b"plate file"[..])); }
    let dir = root.join("journal");
    let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 32, Arc::new(|_| {}));
    journal.send(Record::Checkpoint { manifest: Some(manifest.into()), files: Arc::new(files.clone()) });
    let actor = obj(vec![("kind", s("person"))]);
    let call = |action: &str, args| journal.send(Record::Call { reference: action.into(), actor: actor.clone(), action: action.into(), args });
    let done = |id, seqs| journal.send(Record::Done { call: id, outcome: "ok", reply: "private-reply-marker".into(), seqs, elsewhere: Default::default() });
    done(call("fab.inspect", obj(vec![("marker", s("private-argument-marker"))])), vec![]);
    let layout = call("fab.workspace_edit", obj(vec![]));
    journal.send(Record::Effect { call: Some(layout), foreign: None, seq: 1, actor: actor.clone(), label: "layout".into(), action: None,
        changes: vec![Change::whole("kept.txt".into(), None, Some(Arc::from(&b"recorded effect"[..])))] });
    done(layout, vec![1]);
    done(call("fab.unknown_fixture_action", obj(vec![])), vec![]);
    journal.send(Record::Effect { call: None, foreign: None, seq: 2, actor: actor.clone(), label: "gesture".into(), action: None,
        changes: vec![Change::text("kept.txt".into(), "recorded effect", "after gesture")] });
    let print = if print { "mod.std.println(\"private-plate-output\")\n" } else { "" };
    let source = format!("use mod.fab.*\nlet m = mod.model\n{print}let block = m.box({{size: [8mm 4mm 4mm]}})\nobject(block)\n");
    let args = obj(vec![("script", s("block.cad.splash")), ("source", s(source)), ("request_id", s("private-argument-marker")),
        ("expected", s(session.model.head().text()))]);
    let id = call("fab.script", args.clone());
    let old = session.model.state().clone();
    let mut saved = mechanical::Saved::opened(manifest, &files, &old);
    let reply = session.as_author(fab_ai::Author::Person).call("script", &args, None);
    assert!(reply.ok, "fixture script must commit");
    let mut changes = saved.changes(&old, &session.model);
    assert!(changes.iter().any(|c| c.name == "block.cad.splash"));
    if corrupt { changes[0].after = Some(sha256_hex(b"a different effect")); }
    journal.send(Record::Effect { call: Some(id), foreign: None, seq: 3, actor: actor.clone(), label: "script".into(), action: None, changes });
    done(id, vec![3]);
    done(call("fab.params", obj(vec![])), vec![]);
    drop(journal);
    writer.run();
    dir
}

#[test]
fn moved_plate_writer_never_appends_to_the_replacement_journal() {
    use fab_ai::plates::{Origin, Plate as StoredPlate, PlateStore};

    let root = temp();
    let store = PlateStore::under(&root.join("settings"), &root.join("projects"));
    let working = fab_ai::WorkingId::default();
    let id = working.id();
    let plate = store.dir(id);
    let dir = plate.join("journal");
    fs::create_dir_all(&plate).unwrap();
    store.write(&StoredPlate { id: id.into(), name: "Fixture".into(), file: None,
        saved: String::new(), edited: 0, modified: false, origin: Origin::Plate,
        printer: String::new(), removed: false }).unwrap();

    let channel = || Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    let (jb, b) = channel();
    jb.send(call());
    assert!(b.try_write(&b.rx.recv().unwrap(), |_| ()));
    let original = fs::read(dir.join(LOG)).unwrap();
    let (ja, mut a) = channel();
    a.create_plate = false;
    let trash = root.join("trash");
    let moved = store.delete(id, &|path| fs::rename(path, &trash).map_err(|e| e.to_string()));
    if cfg!(windows) {
        assert!(moved.is_err(), "Windows must refuse the directory move with B's handles open");
        assert!(store.read(id).is_some() && !trash.exists());
        assert!(fs::read(dir.join(LOG)).unwrap() == original);
        let outer = File::open(plate.join(LIFETIME)).unwrap();
        assert!(matches!(outer.try_lock(), Err(fs::TryLockError::WouldBlock)));
        drop((a, b, outer));
        fs::rename(&plate, &trash).expect("the same directory can move after release");
        fs::remove_dir_all(root).unwrap();
        return
    }
    moved.unwrap();
    ja.send(Record::History { undo: true, before: None, after: None, changes: vec![] });
    let a1 = a.rx.recv().unwrap();
    let wrote_without_parent = a.try_write(&a1, |_| ());
    let paused_without_parent = matches!(ja.progress.health(), Health::Failing(_))
        && ja.progress.pending() == 1 && !plate.exists();
    if !wrote_without_parent {
        fs::create_dir(&plate).unwrap();
        assert!(a.try_write(&a1, |_| ()), "A must record once the registered parent returns");
    }
    drop(a);
    let replacement = fs::read(dir.join(LOG)).unwrap();
    let bytes: Arc<[u8]> = Arc::from(&b"fixture bytes"[..]);
    let hash = sha256_hex(&bytes);
    jb.send(Record::Reload(vec![Change::whole("part.splash".into(), None, Some(bytes))]));
    let r2 = b.rx.recv().unwrap();
    assert!(!b.try_write(&r2, |_| ()), "step 6: B wrote through the replacement path");
    assert!(paused_without_parent, "step 4: the registered writer must wait for its parent");
    assert!(matches!(jb.progress.health(), Health::Failing(_)), "B must report the move");
    assert_eq!(jb.progress.pending(), 1, "B must retain its unwritten record");
    assert!(fs::read(dir.join(LOG)).unwrap() == replacement, "the store log must keep just A's record");
    assert!(fs::read(trash.join("journal").join(LOG)).unwrap() == original, "the moved log must keep just B's first record");
    for journal in [&dir, &trash.join("journal")] {
        assert!(!journal.join("blobs").join(&hash).exists(), "B must detect the move before writing its blob");
    }
    drop(b);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollover_yields_the_row_lock_to_note_recovery_then_checks_the_new_tail() {
    let root = temp();
    let dir = root.join("plate/journal");
    let channel = || Journal::channel(dir.clone(), Arc::default(), 4, Arc::new(|_| ()));
    let (j, mut writer) = channel();
    writer.block_bits = 2;
    let manifest = "fixture.fab.splash";
    j.send(Record::Checkpoint {
        manifest: Some(manifest.into()),
        files: Arc::new([(manifest.into(), Arc::from(b"fixture".as_slice()))].into()),
    });
    j.send(Record::Reload(vec![]));
    j.send(Record::Reload(vec![]));
    for _ in 0..3 { assert!(writer.try_write(&writer.rx.try_recv().unwrap(), |_| ())); }

    let (cut, _) = channel();
    cut.reserve();
    let notes = loss::Note::cut(&dir, &mut cut.progress.orders.lock().unwrap()).unwrap();
    loss::Attempt::new(notes, &dir, None).write();
    let mut interrupted = false;
    j.send(call());
    let committed = writer.try_write(&writer.rx.try_recv().unwrap(), |line| {
        if line.contains("rollover replay started") {
            // The callback runs after dropping the row lock and before the archive read.
            let row_lock = OpenOptions::new().read(true).write(true).open(dir.join(LOCK)).unwrap();
            row_lock.try_lock().expect("rollover replay must leave the append lock free");
            row_lock.unlock().unwrap();
            let (_, recovering) = channel();
            recovering.recover_notes(&mut |_| ()).unwrap();
            row_lock.try_lock().expect("recovery must release the append lock");
            let mut file = OpenOptions::new().append(true).open(dir.join(LOG)).unwrap();
            file.write_all(b"{").unwrap();
            file.sync_data().unwrap();
            interrupted = true;
        }
    });
    assert!(interrupted && committed, "the original writer must resume after the recovery and partial peer row");
    let mut causes = BTreeMap::new();
    let mut orders = BTreeSet::new();
    scan(File::open(dir.join(LOG)).unwrap(), reader::header, |_, row| {
        if let Some(row) = row {
            assert!(orders.insert(row.get("order").unwrap().as_u64().unwrap()), "each complete row needs its own order");
            if let Some(cause) = row.get("cause").and_then(Value::as_str) { *causes.entry(cause.to_owned()).or_insert(0) += 1; }
        }
    }).unwrap();
    assert_eq!(causes.get("unwritten"), Some(&1), "one loss note must produce one marker");
    assert_eq!(causes.get("torn"), Some(&1), "the returning writer must repair the new tail");
    assert!(orders.contains(&at(2, 1)) && orders.contains(&at(2, 2)), "the rollover base and triggering call must both be durable");
    drop(writer);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_lock_error_marks_before_recording_and_retries_an_exclusive_holder() {
    for existing in [false, true] {
        for case in ["second succeeds", "second busy", "both fail", "marker fails"] {
            let root = temp();
            let dir = root.join("journal");
            let path = root.join(LIFETIME);
            let outer = File::create(&path).unwrap();
            if existing { fs::create_dir(&dir).unwrap(); }
            if case == "second busy" { outer.try_lock().unwrap(); }
            let (j, w) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
            j.send(call());
            let row = w.rx.recv().unwrap();
            let mut tries = 0;
            let marker = root.join(UNLOCKED);
            let observed = dir.clone();
            let success = with_probe(move |point| {
                if point == "shared try" {
                    tries += 1;
                    if tries == 2 {
                        assert!(marker.is_file(), "the marker must precede the second try");
                        assert_eq!(observed.exists(), existing, "prepare created the journal too early");
                    }
                    if tries == 1 || case == "both fail" { return Err(io::ErrorKind::Unsupported.into()) }
                }
                if point == "marker sync" && case == "marker fails" { return Err(io::ErrorKind::StorageFull.into()) }
                Ok(())
            }, || w.try_write(&row, |_| ()));
            assert_eq!(success, matches!(case, "second succeeds" | "both fail"));
            if !success {
                assert!(!dir.join(LOG).exists() && j.progress.pending() == 1);
                let expected = if case == "second busy" { io::ErrorKind::WouldBlock } else { io::ErrorKind::StorageFull };
                assert_eq!(j.progress.health(), Health::Failing(expected.into()));
                if case == "second busy" { outer.unlock().unwrap(); }
                assert!(w.try_write(&row, |_| ()), "the queued record must land on retry");
            }
            assert!(root.join(UNLOCKED).is_file(), "uncertainty stays recorded");
            assert_eq!(read_rows(&dir).unwrap().len(), 1);
            assert_eq!(j.progress.pending(), 0);
            if case == "both fail" { outer.try_lock().expect("this writer recorded unlocked"); }
            else { assert!(matches!(outer.try_lock(), Err(fs::TryLockError::WouldBlock))); }
            drop((w, outer));
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn row_lock_error_still_publishes_an_incarnation_and_records_the_first_call() {
    for corrupt in [false, true] {
        let root = temp();
        let dir = root.join("journal");
        let path = dir.join(INCARNATION);
        if corrupt {
            File::create(root.join(LIFETIME)).unwrap();
            fs::create_dir(&dir).unwrap();
            fs::write(&path, b"incomplete").unwrap();
        }
        let (journal, writer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
        journal.send(call());
        let marker = root.join(UNLOCKED);
        assert!(with_probe(move |point| {
            if point == "row try" { return Err(io::ErrorKind::Unsupported.into()) }
            if point == "incarnation directory sync" {
                assert!(marker.is_file(), "unlocked recording must be marked before the id is published");
            }
            Ok(())
        }, || writer.try_write(&writer.rx.recv().unwrap(), |_| ())), "the first call must be written without a row lock");
        assert!(Incarnation::parse(&fs::read_to_string(path).unwrap()).is_some());
        assert!(root.join(UNLOCKED).is_file());
        assert_eq!(journal.progress.pending(), 0);
        let rows = read_rows(&dir).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("ref").and_then(Value::as_str), Some("call-fixture"));
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn legacy_and_busy_unsaved_discovery_leave_the_loss_note_for_later() {
    for legacy in [false, true] {
        let root = temp();
        let plate = root.join("working-1-2-3");
        let dir = plate.join("journal");
        fs::create_dir_all(&dir).unwrap();
        let (j, recovering) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
        j.reserve();
        let notes = loss::Note::cut(&dir, &mut j.progress.orders.lock().unwrap()).unwrap();
        loss::Attempt::new(notes, &dir, None).write();
        let note = fs::read_dir(&plate).unwrap().map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|ext| ext == "json")).unwrap();
        let bytes = fs::read(&note).unwrap();
        let outer = if legacy { None } else {
            let lock = File::create(plate.join(LIFETIME)).unwrap();
            lock.try_lock().unwrap();
            Some(lock)
        };
        let result = recovering.recover_notes(&mut |_| ());
        if legacy { result.unwrap(); } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        }
        assert!(fs::read(note).unwrap() == bytes, "the original note must survive discovery");
        assert_eq!(plate.join(LIFETIME).exists(), !legacy, "discovery must not upgrade a legacy folder");
        assert!(!plate.join(UNLOCKED).exists() && !dir.join(LOG).exists());
        if legacy {
            let (j, producer) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
            j.send(Record::Checkpoint { manifest: None, files: Arc::default() });
            assert!(producer.try_write(&producer.rx.recv().unwrap(), |_| ()));
            drop(producer);
            assert!(plate.join(UNLOCKED).is_file() && !plate.join(LIFETIME).exists());
            let rows = read_rows(&dir).unwrap();
            assert_eq!(rows.len(), 2, "the marker-only session retains its loss and opening");
            rebuild(&dir, None, &root.join("copy")).expect("its opening is readable after the writer exits");
        } else {
            let (j, late) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
            j.send(call());
            let row = late.rx.recv().unwrap();
            assert!(!late.try_write(&row, |_| ()));
            assert!(!plate.join(UNLOCKED).exists() && !dir.join(LOCK).exists());
            drop(outer);
            assert!(late.try_write(&row, |_| ()));
            assert_eq!(j.progress.pending(), 0);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn moved_folder_at_prepare_blob_repair_or_rollover_never_publishes_a_replacement_row() {
    for point in ["acquired row lock", "validated log", "blob synced", "repair synced", "reading rollover prefix"] {
        let root = temp();
        let plate = root.join("plate");
        let dir = plate.join("journal");
        let moved = root.join("moved");
        let (j, mut w) = Journal::channel(dir.clone(), Arc::default(), 4, Arc::new(|_| ()));
        w.block_bits = 2;
        let opening = || Record::Checkpoint { manifest: None, files: Arc::default() };
        if matches!(point, "repair synced" | "reading rollover prefix") {
            j.send(opening());
            assert!(w.try_write(&w.rx.recv().unwrap(), |_| ()));
            if point == "repair synced" {
                OpenOptions::new().append(true).open(dir.join(LOG)).unwrap().write_all(b"{").unwrap();
            } else {
                for _ in 0..2 {
                    j.send(Record::Reload(vec![]));
                    assert!(w.try_write(&w.rx.recv().unwrap(), |_| ()));
                }
            }
        }
        let record = if point == "blob synced" {
            Record::Reload(vec![Change::whole("part".into(), None, Some(Arc::from(b"source".as_slice())))])
        } else { call() };
        j.send(record);
        let row = w.rx.recv().unwrap();
        let visits = Arc::new(AtomicU64::new(0));
        let visited = visits.clone();
        // B has its own legitimate log, lifetime and row lock, ready to take the old name.
        let fresh = root.join("replacement");
        let (peer, other) = Journal::channel(fresh.join("journal"), Arc::default(), 1, Arc::new(|_| ()));
        peer.send(opening());
        assert!(other.try_write(&other.rx.recv().unwrap(), |_| ()));
        let replacement = fs::read(fresh.join("journal").join(LOG)).unwrap();
        drop(other);
        let (src, dst) = (plate.clone(), moved.clone());
        let mut messages = Vec::new();
        let success = with_probe(move |at| {
            if at != point || visited.fetch_add(1, Ordering::Relaxed) != 0 { return Ok(()) }
            let outer = File::open(src.join(LIFETIME))?;
            assert!(matches!(outer.try_lock(), Err(fs::TryLockError::WouldBlock)), "{point}: the shared lifetime ended early");
            drop(outer);
            let rename = fs::rename(&src, &dst);
            if cfg!(windows) {
                assert_eq!(rename.unwrap_err().kind(), io::ErrorKind::PermissionDenied, "{point}: Windows must pin the folder");
            } else {
                rename?;
                fs::rename(&fresh, &src)?;
            }
            Ok(())
        }, || w.try_write(&row, |message| messages.push(message)));
        if point == "reading rollover prefix" {
            assert!(messages.iter().any(|m| m.contains("ended (3 rows,") && m.ends_with("complete)")),
                "replay must read the three-row prefix from the held file even when its path names B's one-row log");
        }
        assert!(visits.load(Ordering::Relaxed) > 0, "{point}: hook was not reached");
        assert_eq!(peer.progress.pending(), 0);
        if cfg!(windows) {
            assert!(success && !moved.exists(), "{point}: a refused move must leave the original writer healthy");
            assert_eq!(j.progress.pending(), 0);
        } else {
            assert!(!success && matches!(j.progress.health(), Health::Failing(_)), "{point}: a moved append must fail");
            assert_eq!(j.progress.pending(), 1);
            assert!(fs::read(dir.join(LOG)).unwrap() == replacement, "{point}: B's files must keep B's opening alone");
            let old = moved.join("journal");
            if point == "repair synced" {
                assert!(old.join(REPAIR).exists(), "the repair intent must be retained after a move");
            } else if point == "reading rollover prefix" {
                assert_eq!(read_rows(&old).unwrap().len(), 3, "no rolled base or triggering row in the old log");
                assert!(w.pending_bases.borrow()[0].snapshot.is_none(), "a moved replay must not be cached");
            } else if old.join(LOG).exists() {
                assert_eq!(fs::metadata(old.join(LOG)).unwrap().len(), 0, "{point}: no row in the moved file");
            }
        }
        drop(w);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn lifetime_precedes_the_row_lock_and_outlives_a_failed_prepare() {
    let root = temp();
    let dir = root.join("journal");
    let (j, w) = Journal::channel(dir.clone(), Arc::default(), 1, Arc::new(|_| ()));
    j.send(call());
    let row = w.rx.recv().unwrap();
    let inside = root.clone();
    let ok = with_probe(move |point| {
        if point == "before row lock" {
            assert!(!inside.join("journal").exists(), "the lifetime lock must be first");
            let peer = File::open(inside.join(LIFETIME))?;
            assert!(matches!(peer.try_lock(), Err(fs::TryLockError::WouldBlock)));
            return Err(io::ErrorKind::Interrupted.into())
        }
        Ok(())
    }, || w.try_write(&row, |_| ()));
    assert!(!ok && j.progress.pending() == 1);
    let peer = File::open(root.join(LIFETIME)).unwrap();
    assert!(matches!(peer.try_lock(), Err(fs::TryLockError::WouldBlock)), "the retry interval must remain protected");
    assert!(w.try_write(&row, |_| ()));
    assert!(matches!(peer.try_lock(), Err(fs::TryLockError::WouldBlock)));
    assert!(!root.join(UNLOCKED).exists());
    drop(w);
    peer.try_lock().expect("ending the writer life must release its lock");
    drop(peer);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rollback_cannot_truncate_a_different_log() {
    let root = temp();
    let original = root.join("original");
    let other = root.join("replacement");
    fs::write(&original, b"old partial").unwrap();
    fs::write(&other, b"peer row").unwrap();
    let held = OpenOptions::new().read(true).append(true).open(&original).unwrap();
    assert_eq!(same_file(&held, &original).unwrap(), Some(true));
    assert_eq!(same_file(&held, &other).unwrap(), Some(false));
    let result = roll_back(&held, &other, 3);
    if cfg!(windows) {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData, "the separate write handle must be checked");
        assert_eq!(fs::read(&original).unwrap(), b"old partial");
    } else {
        result.expect("Unix truncates through the held log");
        assert_eq!(fs::read(&original).unwrap(), b"old");
    }
    assert_eq!(fs::read(other).unwrap(), b"peer row", "a rollback must never truncate the other log");
    drop(held);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reader_reindexes_a_replacement_journal_even_when_length_and_order_repeat() {
    let _guard = crate::cx_shared();
    let cx = makepad_widgets::Cx::new(Box::new(|_, _| {}));
    let root = temp();
    let dir = root.join("plate/journal");
    let next = root.join("next");
    let archived = root.join("archived");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir(&next).unwrap();
    let row = |kind: &str| {
        let mut bytes = obj(vec![("kind", s(kind)), ("changes", Value::Arr(vec![])), ("order", number(at(1, 1)))]).to_json();
        if kind == "reload" { bytes.push(' '); } // Same length as "history", including the newline.
        bytes.push('\n');
        bytes
    };
    fs::write(dir.join(LOG), row("history")).unwrap();
    fs::write(next.join(LOG), row("reload")).unwrap();
    let mut reader = reader::Reader::start(&cx.thread_spawner(), root.clone()).unwrap();
    reader.select("plate").unwrap();
    let page = |reader: &mut reader::Reader, after: Box<dyn FnOnce() + Send>| {
        let (tx, rx) = mpsc::channel();
        reader.query(move |browse| {
            let (rows, _) = browse.unwrap().page(reader::Filter::default(), None, 1).unwrap();
            let kind = rows[0].kind;
            // Replace between two requests, so no poll can observe the intermediate empty path.
            after();
            tx.send(kind).unwrap();
        }).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            reader.try_recv().unwrap();
            if let Ok(kind) = rx.try_recv() { break kind }
            assert!(std::time::Instant::now() < deadline, "the journal reader must deliver the page");
            thread::yield_now();
        }
    };
    let before = page(&mut reader, Box::new(move || {
        fs::rename(&dir, archived).unwrap();
        fs::rename(next, dir).unwrap();
    }));
    let after = page(&mut reader, Box::new(|| {}));
    drop(reader);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(before, reader::Kind::History);
    assert_eq!(after, reader::Kind::Reload, "the new journal must never inherit the moved log's index");
}

mod removal {
    use super::*;
    use fab_ai::plates::{PlateStore, REMOVED_SESSIONS};
    use remove::{empty_removed, remove_session, Emptied, Refusal, Removed, EMPTY_LOCK};
    use reader::sessions::list_removed;

    const ID: &str = "working-1-2-3";

    fn store() -> (PathBuf, PlateStore) {
        let root = temp();
        let store = PlateStore::under(&root, &root.join("projects"));
        (root, store)
    }

    fn channel(dir: &Path) -> (Journal, Writer) {
        Journal::channel(dir.to_owned(), Arc::default(), 4, Arc::new(|_| ()))
    }

    fn write(j: &Journal, w: &Writer, record: Record) {
        j.send(record);
        assert!(w.try_write(&w.rx.try_recv().unwrap(), |_| ()));
    }

    fn seed(store: &PlateStore, id: &str) -> PathBuf {
        let dir = store.dir(id).join("journal");
        let (j, w) = channel(&dir);
        write(&j, &w, Record::Checkpoint {
            manifest: Some("demo.fab.splash".into()),
            files: Arc::new([("demo.fab.splash".into(), Arc::from(b"before".as_slice()))].into()),
        });
        dir
    }

    fn listed(store: &PlateStore, id: &str) -> u64 {
        reader::sessions::list(&store.root, "", &|| true).unwrap().into_iter()
            .find(|session| &*session.folder == id).unwrap().token.unwrap()
    }

    fn archive(store: &PlateStore, id: &str) -> PathBuf {
        seed(store, id);
        assert_eq!(remove_session(store, id, listed(store, id), &mut |_| ()), Ok(Removed::Moved));
        store.root.join(REMOVED_SESSIONS).join(id)
    }

    #[test]
    fn empty_keeps_unconfirmed_changed_and_busy_archives() {
        let (root, store) = store();
        let changed = archive(&store, ID);
        let busy = archive(&store, "working-1-2-4");
        let verified = archive(&store, "working-1-2-5");
        let no_lock = archive(&store, "working-1-2-6");
        // Earlier archives can have neither an incarnation nor a log.
        fs::remove_file(verified.join(INCARNATION)).unwrap();
        fs::remove_file(verified.join(LOG)).unwrap();
        let confirmed = list_removed(&store.root).unwrap();
        assert_eq!(confirmed.len(), 4, "listing must leave all four archives in place");
        let late = archive(&store, "working-1-2-7");
        let late_bytes = contents(&late);
        OpenOptions::new().append(true).open(changed.join(LOG)).unwrap().write_all(b"\n").unwrap();
        let changed_bytes = contents(&changed);
        let row = File::open(busy.join(LOCK)).unwrap();
        row.try_lock().unwrap();
        fs::remove_file(no_lock.join(LOCK)).unwrap();
        let result = empty_removed(&store, &confirmed, &mut |_| ());
        assert_eq!(result, Emptied { deleted: 1, kept: 3 });
        assert_eq!(result.message(), "3 removed sessions changed since you looked. They are kept.");
        assert!(!verified.exists(), "the explicit Empty deletes the verified archive");
        assert!(contents(&late) == late_bytes, "an archive added after confirmation must survive");
        assert!(contents(&changed) == changed_bytes, "a changed length must preserve the archive");
        assert!(busy.join(LOG).is_file() && no_lock.join(LOG).is_file());
        drop(row);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_confirmation_when_an_archive_name_is_reused() {
        let (root, store) = store();
        let path = archive(&store, ID);
        let first = list_removed(&store.root).unwrap();
        let second = list_removed(&store.root).unwrap();
        let replacement = seed(&store, ID);
        let expected = contents(&replacement);
        let incarnation = fs::read(replacement.join(INCARNATION)).unwrap();
        assert!(fs::read(path.join(INCARNATION)).unwrap() != incarnation);
        let peer = store.clone();
        let stale = second.clone();
        let contended = Arc::new(AtomicBool::new(false));
        let observed = contended.clone();
        with_probe(move |point| {
            if point == "empty verified" {
                let lock = File::open(peer.root.join(REMOVED_SESSIONS).join(EMPTY_LOCK))?;
                assert!(matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock)), "Empty must still own the archive-wide lock");
                assert_eq!(empty_removed(&peer, &stale, &mut |_| ()), Emptied { deleted: 0, kept: 1 },
                    "a second confirmed Empty must keep A at the first deleter's last check");
                observed.store(true, Ordering::Release);
            }
            Ok(())
        }, || assert_eq!(empty_removed(&store, &first, &mut |_| ()), Emptied { deleted: 1, kept: 0 }));
        assert!(contended.load(Ordering::Acquire));
        // No Empty can still have a verified delete pending when Remove reuses the name.
        assert_eq!(remove_session(&store, ID, listed(&store, ID), &mut |_| ()), Ok(Removed::Moved));
        assert_eq!(empty_removed(&store, &second, &mut |_| ()), Emptied { deleted: 0, kept: 1 });
        assert!(fs::read(path.join(INCARNATION)).unwrap() == incarnation && contents(&path) == expected,
            "retrying A's confirmation must preserve B and its incarnation");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_never_follows_archive_or_lock_links() {
        let (root, store) = store();
        let verified = archive(&store, ID);
        let replaced = archive(&store, "working-1-2-4");
        let archive_dir = store.root.join(REMOVED_SESSIONS);
        let confirmed = list_removed(&store.root).unwrap();
        let outside = root.join("outside-store");
        fs::rename(&replaced, &outside).unwrap();
        let expected = contents(&outside);
        let link = |path: &Path| {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&outside, path).unwrap();
            #[cfg(windows)]
            assert!(std::process::Command::new("cmd").args(["/C", "mklink", "/J"])
                .arg(path).arg(&outside).output().unwrap().status.success());
        };
        link(&replaced);
        link(&verified.join("linked-directory"));
        let empty_lock = archive_dir.join(EMPTY_LOCK);
        link(&empty_lock);
        assert_eq!(list_removed(&store.root).unwrap().len(), 1, "linked entries must be excluded");
        assert_eq!(empty_removed(&store, &confirmed, &mut |_| ()), Emptied { deleted: 0, kept: 2 },
            "a linked Empty lock must keep the entire confirmation");
        #[cfg(unix)]
        fs::remove_file(&empty_lock).unwrap();
        #[cfg(windows)]
        fs::remove_dir(&empty_lock).unwrap();
        assert_eq!(empty_removed(&store, &confirmed, &mut |_| ()), Emptied { deleted: 1, kept: 1 });
        assert!(!verified.exists());
        assert!(fs::symlink_metadata(&replaced).unwrap().file_type().is_symlink());
        assert!(contents(&outside) == expected, "removing a nested link must preserve its target byte for byte");
        assert!(archive_dir.is_dir() && empty_lock.is_file());
        assert!(list_removed(&store.root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    // Include empty directories, sidecars and blobs. Relative keys survive a rename.
    fn contents(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn visit(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for child in fs::read_dir(dir).unwrap() {
                let child = child.unwrap();
                let path = child.path();
                let name = path.strip_prefix(base).unwrap().to_owned();
                let kind = child.file_type().unwrap();
                if kind.is_dir() {
                    out.insert(name, None);
                    visit(&path, base, out);
                } else {
                    assert!(kind.is_file());
                    out.insert(name, Some(fs::read(&path).unwrap()));
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(dir, dir, &mut files);
        files
    }

    fn note_at(plate: &Path) -> PathBuf {
        fs::read_dir(plate).unwrap().map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|e| e == "json")).unwrap()
    }

    #[test]
    fn remove_legacy_session_keeps_recovery_on_changed_confirmation() {
        for with_note in [false, true] {
            let (root, store) = store();
            let dir = seed(&store, ID);
            fs::remove_file(dir.join(INCARNATION)).unwrap();
            let old = listed(&store, ID);
            if with_note {
                let (j, _) = channel(&dir);
                j.reserve();
                cut_note(&j, &dir, &root).write();
                let note = note_at(&store.dir(ID));
                let result = remove_session(&store, ID, old, &mut |_| ());
                assert_eq!(result.unwrap_err().to_string(), "This session changed. Look again.");
                assert!(!note.exists(), "the recovery preceding the refusal consumed the note");
                assert_eq!(read_rows(&dir).unwrap().iter().filter(|r| r.get("kind").and_then(Value::as_str) == Some("lost")).count(), 1);
                assert_ne!(listed(&store, ID), old);
            }
            let before = contents(&dir);
            assert_eq!(remove_session(&store, ID, listed(&store, ID), &mut |_| ()), Ok(Removed::Moved));
            let archive = store.root.join(REMOVED_SESSIONS).join(ID);
            assert!(contents(&archive) == before, "the move preserves the state after recovery");
            assert_eq!(archive.join(INCARNATION).exists(), with_note, "Remove itself must never assign an id");
            assert!(store.dir(ID).join(LIFETIME).is_file());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn remove_parent_sync_failure_reports_the_committed_move() {
        let (root, store) = store();
        let dir = seed(&store, ID);
        let before = contents(&dir);
        let syncs = Arc::new(AtomicU64::new(0));
        let observer = syncs.clone();
        let outcome = with_probe(move |point| {
            if point == "remove parent sync" && observer.fetch_add(1, Ordering::Relaxed) == 0 {
                return Err(io::ErrorKind::PermissionDenied.into())
            }
            Ok(())
        }, || remove_session(&store, ID, listed(&store, ID), &mut |_| ())).unwrap();
        assert_eq!(syncs.load(Ordering::Relaxed), 2, "both parents must be attempted");
        assert_eq!(outcome.warning(), Some("The session was moved, but Fab could not confirm the move was saved to disk."));
        assert!(!dir.exists());
        assert!(contents(&store.root.join(REMOVED_SESSIONS).join(ID)) == before, "the archive bytes changed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_freezes_lazy_rows_and_keeps_the_successors_lock_name() {
        let (root, store) = store();
        let dir = seed(&store, ID);
        let token = listed(&store, ID);
        let before = contents(&dir);
        let (j, w) = channel(&dir);
        j.send(call());
        j.send(Record::Reload(vec![]));
        let first = w.rx.recv().unwrap();
        let path = store.dir(ID).join(LIFETIME);
        let successor = File::open(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut work = Some((w, first));
        let path_in_hook = path.clone();
        let progress = j.progress.clone();
        with_probe(move |point| {
            if point == "remove checked" {
                let (w, first) = work.take().unwrap();
                assert!(!w.try_write(&first, |_| ()), "an unprepared producer cannot enter during Remove");
                assert_eq!(progress.pending(), 2);
                let opened = File::open(&path_in_hook).unwrap();
                assert!(matches!(opened.try_lock_shared(), Err(fs::TryLockError::WouldBlock)));
                tx.send((w, first, opened)).unwrap();
            }
            Ok(())
        }, || remove_session(&store, ID, token, &mut |_| ())).unwrap();
        let (w, first, opened) = rx.recv().unwrap();
        assert_eq!(same_file(&successor, &path).unwrap(), Some(true));
        opened.try_lock_shared().expect("a successor can acquire the very handle opened during Remove");
        assert_eq!(same_file(&opened, &path).unwrap(), Some(true));
        assert!(w.try_write(&first, |_| ()));
        assert!(w.try_write(&w.rx.try_recv().unwrap(), |_| ()));
        assert_eq!(j.progress.pending(), 0);
        assert_eq!(read_rows(&dir).unwrap().len(), 2, "both queued reservations were acknowledged once");
        assert!(!store.dir(ID).join(UNLOCKED).exists());
        let archive = store.root.join(REMOVED_SESSIONS);
        assert!(contents(&archive.join(ID)) == before, "retry cannot alter the archived blobs, log or life");
        let after = contents(&dir);
        drop((w, opened, successor));
        let next = listed(&store, ID);
        assert_ne!(next, token);
        assert_eq!(remove_session(&store, ID, token, &mut |_| ()), Err(Refusal::Changed));
        remove_session(&store, ID, next, &mut |_| ()).unwrap();
        assert!(contents(&archive.join(format!("{ID} 2"))) == after, "the suffix must name the second journal");
        assert!(contents(&archive.join(ID)) == before, "the first archive must keep its bytes");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unprepared_producer_can_publish_a_loss_note_while_remove_holds_the_lock() {
        let (root, store) = store();
        let dir = seed(&store, ID);
        let archive = store.root.join(REMOVED_SESSIONS).join(ID);
        let before = contents(&dir);
        let (journal, _writer) = channel(&dir);
        let settings = root.clone();
        let source = dir.clone();
        with_probe(move |point| {
            if point != "remove checked" { return Ok(()) }
            let dir = &source;
            let plate = dir.parent().unwrap();
            let root = &settings;
            journal.send(call());
            let notes = loss::Note::cut(dir, &mut journal.progress.orders.lock().unwrap()).unwrap();
            let attempt = loss::Attempt::new(notes, dir, Some(root));
            attempt.write();
            let note = fs::read_dir(plate).unwrap().map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|ext| ext == "json")).expect("the late cut must publish its note");
            let bytes = fs::read(&note).unwrap();
            let row = reader::parse(&bytes).unwrap();
            assert_eq!(row.get("prefix").and_then(Value::as_str), Some("unknown"));
            assert_eq!(row.get("count").and_then(Value::as_u64), Some(1));
            fs::rename(&note, note.with_extension("tmp")).unwrap();
            attempt.write();
            let fallback = root.join("journal-loss/working-1-2-3").join(note.file_name().unwrap());
            assert!(fs::read(fallback).unwrap() == bytes, "the fallback must work while the same lock is held");
            assert_eq!(journal.progress.pending(), 1, "the call still has no durable row");
            fs::rename(note.with_extension("tmp"), &note).unwrap();
            let discovering = PlateStore::under(root, &root.join("projects"));
            let mut woke = false;
            loss::discover_for_recovery(&discovering, &mut |_| (), |found, create| {
                let (_, mut recovery) = channel(&found);
                recovery.settings = Some(root.clone());
                recovery.create_plate = create;
                assert_eq!(recovery.recover_notes(&mut |_| ()).unwrap_err().kind(), io::ErrorKind::WouldBlock,
                    "discovery must defer while Remove holds the lifetime lock");
                woke = true;
            });
            assert!(woke && fs::read(note).unwrap() == bytes);
            Ok(())
        }, || remove_session(&store, ID, listed(&store, ID), &mut |_| ())).unwrap();
        let local = note_at(&store.dir(ID));
        let fallback = root.join("journal-loss").join(ID).join(local.file_name().unwrap());
        assert!(fs::read(&local).unwrap() == fs::read(&fallback).unwrap(), "Remove touches neither publication");
        loss::discover_for_recovery(&store, &mut |_| (), |dir, create| recover_discovered_writer(&store, dir, create, &mut Vec::new()));
        assert!(!local.exists() && !fallback.exists());
        let rows = read_rows(&dir).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("prefix").and_then(Value::as_str), Some("unknown"));
        assert_eq!(loss::file_range(&rows[0]), None);
        assert_ne!(fs::read(dir.join(INCARNATION)).unwrap(), fs::read(archive.join(INCARNATION)).unwrap());
        assert!(contents(&archive) == before, "the archived files changed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_refuses_during_shared_prepare_row_contention_rollover_and_adoption() {
        let (root, store) = store();
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), store.home.clone());
        session.store = Some(store.clone());
        let second = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), store.home.clone());
        let id = session.working_id.id().to_owned();
        assert_ne!(id, second.working_id.id(), "two sessions must mint different working ids");
        let dir = seed(&store, &id);
        let token = listed(&store, &id);
        let busy = {
            let store = store.clone();
            let folder = id.clone();
            move || assert_eq!(remove_session(&store, &folder, token, &mut |_| ()), Err(Refusal::Open))
        };
        let (a, mut first) = channel(&dir);
        first.block_bits = 2;
        let before_row = busy.clone();
        with_probe(move |point| {
            if point == "before row lock" { before_row(); }
            Ok(())
        }, || write(&a, &first, Record::Checkpoint { manifest: None, files: Arc::default() }));
        let (b, peer) = channel(&dir);
        // The peer is blocked in the real OS row lock, with its shared lifetime already held.
        let row = File::open(dir.join(LOCK)).unwrap();
        row.try_lock().unwrap();
        b.send(call());
        let waiting = peer.progress.clone();
        let peer = thread::spawn(move || {
            assert!(peer.try_write(&peer.rx.recv().unwrap(), |_| ()));
            peer
        });
        while waiting.lock_waits.load(Ordering::Acquire) == 0 { thread::yield_now(); }
        busy();
        drop(row);
        let peer = peer.join().unwrap();
        busy();
        write(&a, &first, Record::Reload(vec![]));
        write(&a, &first, Record::Reload(vec![]));
        let mut replayed = false;
        a.send(call());
        assert!(first.try_write(&first.rx.recv().unwrap(), |line| {
            if line.contains("rollover replay started") {
                let free = File::open(dir.join(LOCK)).unwrap();
                free.try_lock().expect("only the lifetime lock guards the rollover read");
                drop(free);
                busy();
                replayed = true;
            }
        }));
        assert!(replayed);
        drop(first);
        busy(); // the second shared holder alone still protects adoption
        assert!(!store.dir(&id).join("plate").exists());
        let adopting = id.clone();
        let overlap = with_probe(move |point| {
            if point == "remove recovered" {
                session.save(None).unwrap();
                assert_eq!(session.plate.as_deref(), Some(adopting.as_str()));
            }
            Ok(())
        }, || remove_session(&store, &id, token, &mut |_| ()));
        assert_eq!(overlap, Err(Refusal::Open), "Save publishing the record during Remove remains protected");
        assert_eq!(remove_session(&store, &id, token, &mut |_| ()), Err(Refusal::Ineligible));
        assert!(!store.dir(&id).join(UNLOCKED).exists(), "ordinary contention leaves no marker");
        drop(peer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_recovers_an_earlier_absolute_note_and_preserves_one_arriving_at_recheck() {
        let (root, store) = store();
        let dir = store.dir(ID).join("journal");
        let (j, w) = channel(&dir);
        write(&j, &w, call());
        j.reserve();
        let note = cut_note(&j, &dir, &root);
        drop(w);
        let token = listed(&store, ID);
        let plate = store.dir(ID);
        let settings = root.clone();
        let after_recovery = Arc::new(Mutex::new(None));
        let observed = after_recovery.clone();
        let refused = with_probe(move |point| {
            if point == "remove recovered" {
                note.write();
                let primary = note_at(&plate);
                let fallback = settings.join("journal-loss").join(ID);
                fs::create_dir_all(&fallback)?;
                fs::copy(&primary, fallback.join(primary.file_name().unwrap()))?;
                *observed.lock().unwrap() = Some(contents(&settings));
            }
            Ok(())
        }, || remove_session(&store, ID, token, &mut |_| ()));
        assert_eq!(refused, Err(Refusal::Arrived));
        assert!(contents(&root) == after_recovery.lock().unwrap().take().unwrap(),
            "refusal must preserve both locks, the log and both copies of the new note");

        // This time the absolute note precedes recovery. Its marker travels with the journal.
        let primary = note_at(&store.dir(ID));
        let fallback = root.join("journal-loss").join(ID).join(primary.file_name().unwrap());
        remove_session(&store, ID, token, &mut |_| ()).unwrap();
        assert!(!primary.exists() && !fallback.exists());
        let rows = read_rows(&store.root.join(REMOVED_SESSIONS).join(ID)).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(loss::file_range(&rows[1]), Some((at(1, 2), at(1, 2))));
        assert_eq!(rows[1].get("count").and_then(Value::as_u64), Some(1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_keeps_late_absolute_loss_when_replacement_reuses_the_order() {
        let (root, store) = store();
        let dir = store.dir(ID).join("journal");
        let (j, w) = channel(&dir);
        write(&j, &w, call());
        j.reserve();
        let note = cut_note(&j, &dir, &root);
        drop(w);
        let token = listed(&store, ID);
        let old = fs::read_to_string(dir.join(INCARNATION)).unwrap();
        remove_session(&store, ID, token, &mut |_| ()).unwrap();
        let archive = store.root.join(REMOVED_SESSIONS).join(ID);
        let before = contents(&archive);
        let (next, writer) = channel(&dir);
        write(&next, &writer, call());
        write(&next, &writer, call());
        drop(writer);
        note.write();
        let primary = note_at(&store.dir(ID));
        let bytes = fs::read(&primary).unwrap();
        let fallback_dir = root.join("journal-loss").join(ID);
        fs::create_dir_all(&fallback_dir).unwrap();
        let fallback = fallback_dir.join(primary.file_name().unwrap());
        fs::write(&fallback, &bytes).unwrap();
        let recover = || loss::discover_for_recovery(&store, &mut |_| (), |dir, create|
            recover_discovered_writer(&store, dir, create, &mut Vec::new()));
        thread::scope(|scope| {
            let gate = Arc::new(std::sync::Barrier::new(2));
            for _ in 0..2 {
                let gate = gate.clone();
                let store = &store;
                scope.spawn(move || loss::discover_for_recovery(store, &mut |_| (), |dir, create| {
                    gate.wait(); // both discoveries have found the two publications
                    recover_discovered_writer(store, dir, create, &mut Vec::new());
                }));
            }
        });
        // Reproduce the durable state of a crash after marker sync, before either unlink.
        fs::write(&primary, &bytes).unwrap();
        fs::write(&fallback, &bytes).unwrap();
        recover();
        let rows = read_rows(&dir).unwrap();
        assert_eq!(rows.len(), 3, "recovering the same note twice cannot duplicate its marker");
        assert_eq!(rows[1].get("order").and_then(Value::as_u64), Some(at(1, 2)));
        assert_eq!(rows[2].get("journal").and_then(Value::as_str), Some(old.as_str()));
        assert_eq!(rows[2].get("count").and_then(Value::as_u64), Some(1));
        assert_eq!(loss::file_range(&rows[2]), None);
        assert!(!primary.exists() && !fallback.exists());
        assert!(contents(&archive) == before, "the archived files changed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_a_readable_absolute_note_for_the_confirmed_journal_blocks_the_move() {
        let (root, store) = store();
        let dir = store.dir(ID).join("journal");
        let (j, w) = channel(&dir);
        write(&j, &w, call());
        j.reserve();
        let foreign = cut_note(&j, &dir, &root);
        drop(w);
        remove_session(&store, ID, listed(&store, ID), &mut |_| ()).unwrap();
        seed(&store, ID);
        let (j, _) = channel(&dir);
        j.reserve();
        let local = cut_note(&j, &dir, &root);
        let plate = store.dir(ID);
        let publications = Arc::new(Mutex::new(BTreeMap::new()));
        let captured = publications.clone();
        with_probe(move |point| {
            if point == "remove recovered" {
                foreign.write();
                local.write();
                fs::write(plate.join("loss-unreadable.json"), b"{")?;
                for e in fs::read_dir(&plate)? {
                    let path = e?.path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        captured.lock().unwrap().insert(path.clone(), fs::read(path)?);
                    }
                }
            }
            Ok(())
        }, || remove_session(&store, ID, listed(&store, ID), &mut |_| ())).unwrap();
        let published = publications.lock().unwrap();
        assert_eq!(published.len(), 3);
        for (path, bytes) in published.iter() {
            assert!(fs::read(path).unwrap() == *bytes, "the final check can inspect notes but cannot consume them");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_refusals_preserve_files_and_store_scans_ignore_the_archive() {
        let (root, store) = store();
        let dir = seed(&store, ID);
        let token = listed(&store, ID);
        let plate = store.dir(ID);
        let check = |folder, token, expected| {
            let before = contents(&root);
            assert_eq!(remove_session(&store, folder, token, &mut |_| ()), Err(expected));
            assert!(contents(&root) == before, "a refusal changed the fixture");
        };
        for folder in ["../working-1-2-3", "working-01-2-3", "removed-sessions", "working-1-2-3/journal"] {
            check(folder, token, Refusal::Ineligible);
        }
        check(ID, token ^ 1, Refusal::Changed);
        for file in [plate.join(LIFETIME), dir.join(LOCK)] {
            fs::remove_file(&file).unwrap();
            check(ID, token, Refusal::Legacy(plate.clone()));
            fs::create_dir(&file).unwrap();
            check(ID, token, Refusal::CannotLock);
            fs::remove_dir(&file).unwrap();
            File::create_new(&file).unwrap();
            let busy = File::open(&file).unwrap();
            busy.try_lock_shared().unwrap();
            check(ID, token, Refusal::Open);
        }
        fs::write(plate.join(UNLOCKED), []).unwrap();
        check(ID, token, Refusal::Unlocked);
        fs::remove_file(plate.join(UNLOCKED)).unwrap();
        let archive = store.root.join(REMOVED_SESSIONS);
        fs::write(&archive, b"keep").unwrap();
        check(ID, token, Refusal::CannotMove);
        fs::remove_file(&archive).unwrap();
        fs::write(plate.join("plate"), b"unreadable as a plate").unwrap();
        let (j, _) = channel(&dir);
        j.reserve();
        cut_note(&j, &dir, &root).write();
        check(ID, token, Refusal::Ineligible); // even readable notes are untouched at step zero
        fs::remove_file(plate.join("plate")).unwrap();
        remove_session(&store, ID, token, &mut |_| ()).unwrap();
        // A scan that enters the reserved folder would find both a plate record and a note.
        let saved = store.create("fixture", None, String::new(), fab_ai::plates::Origin::Plate).unwrap();
        fs::rename(store.dir(&saved.id).join("plate"), archive.join("plate")).unwrap();
        fs::write(archive.join("loss-reserved.json"), []).unwrap();
        assert!(store.all().is_empty());
        assert!(reader::sessions::list(&store.root, "", &|| true).unwrap().is_empty());
        loss::discover(&store, Some(&root), |_| panic!("discovery woke for the archive"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn remove_freeze_excludes_the_row_or_displaces_the_unlocked_waiter() {
        for writer_first in [true, false] {
            let (root, store) = store();
            let dir = seed(&store, ID);
            let before = contents(&dir);
            let token = listed(&store, ID);
            let (j, w) = channel(&dir);
            j.send(call());
            let item = w.rx.recv().unwrap();
            let failed_lock = || Err(io::Error::from(io::ErrorKind::Unsupported));
            if writer_first {
                let observer = store.clone();
                let tries = Arc::new(AtomicU64::new(0));
                let failures = tries.clone();
                with_probe(move |point| {
                    if point == "shared try" {
                        failures.fetch_add(1, Ordering::Relaxed);
                        return failed_lock()
                    }
                    if point == "acquired row lock" {
                        assert_eq!(remove_session(&observer, ID, token, &mut |_| ()), Err(Refusal::Open),
                            "the freeze must refuse even when the writer lacks a lifetime lock");
                    }
                    Ok(())
                }, || assert!(w.try_write(&item, |_| ())));
                assert_eq!(tries.load(Ordering::Relaxed), 2);
                assert_eq!(read_rows(&dir).unwrap().len(), 2);
                drop(w);
            } else {
                let (start, ready) = mpsc::channel();
                let waiting = j.progress.clone();
                let worker = thread::spawn(move || {
                    ready.recv().unwrap();
                    let mut tries = 0;
                    with_probe(move |point| {
                        if point == "shared try" { tries += 1; return failed_lock() }
                        if point == "row try" { assert_eq!(tries, 2); }
                        Ok(())
                    }, || w.try_write(&item, |_| ()))
                });
                with_probe(move |point| {
                    if point == "remove checked" {
                        start.send(()).unwrap();
                        while waiting.lock_waits.load(Ordering::Acquire) == 0 { thread::yield_now(); }
                    }
                    Ok(())
                }, || remove_session(&store, ID, token, &mut |_| ())).unwrap();
                assert!(!worker.join().unwrap(), "a waiter on the moved row lock must fail identity validation");
                assert_eq!(j.progress.pending(), 1);
                assert!(matches!(j.progress.health(), Health::Failing(_)));
                assert!(contents(&store.root.join(REMOVED_SESSIONS).join(ID)) == before, "the archive bytes changed");
            }
            assert!(store.dir(ID).join(UNLOCKED).is_file());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn remove_windows_open_child_refuses_rename_after_releasing_the_row_lock() {
        let (root, store) = store();
        let dir = seed(&store, ID);
        let before = contents(&dir);
        let token = listed(&store, ID);
        let open = File::open(dir.join(LOG)).unwrap();
        assert_eq!(remove_session(&store, ID, token, &mut |_| ()), Err(Refusal::Open));
        assert!(contents(&dir) == before, "the refused rename changed the journal");
        drop(open);
        assert_eq!(remove_session(&store, ID, token, &mut |_| ()), Ok(Removed::Moved));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_refuses_linked_source_and_archive_directories() {
        let (root, store) = store();
        let dir = seed(&store, ID);
        let token = listed(&store, ID);
        let outside = root.join("outside-store");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"fixture").unwrap();
        let before = contents(&outside);
        let link = |name: &Path| {
            #[cfg(unix)]
            std::os::unix::fs::symlink(&outside, name).unwrap();
            #[cfg(windows)]
            assert!(std::process::Command::new("cmd").args(["/C", "mklink", "/J"])
                .arg(name).arg(&outside).output().unwrap().status.success());
        };
        let unlink = |name: &Path| {
            #[cfg(unix)]
            fs::remove_file(name).unwrap();
            #[cfg(windows)]
            fs::remove_dir(name).unwrap();
        };
        let archive = store.root.join(REMOVED_SESSIONS);
        link(&archive);
        let original = contents(&dir);
        assert_eq!(remove_session(&store, ID, token, &mut |_| ()), Err(Refusal::CannotMove));
        assert!(contents(&dir) == original, "the linked archive changed the source");
        unlink(&archive);
        let saved = store.dir(ID).join("original");
        fs::rename(&dir, &saved).unwrap();
        link(&dir);
        assert_eq!(remove_session(&store, ID, token, &mut |_| ()), Err(Refusal::Ineligible));
        assert!(contents(&outside) == before, "the linked target changed");
        unlink(&dir);
        assert!(contents(&saved) == original, "the source bytes changed");
        fs::rename(&saved, &dir).unwrap();
        // Recovery opens the sidecars too. A readable note must not turn a link
        // into a write outside the store before the exclusive checks begin.
        let (j, _) = channel(&dir);
        j.reserve();
        cut_note(&j, &dir, &root).write();
        let note = note_at(&store.dir(ID));
        let bytes = fs::read(&note).unwrap();
        let row_lock = dir.join(LOCK);
        fs::remove_file(&row_lock).unwrap();
        link(&row_lock);
        assert_eq!(remove_session(&store, ID, token, &mut |_| ()), Err(Refusal::CannotLock));
        assert!(contents(&outside) == before, "the linked target changed");
        assert!(fs::read(&note).unwrap() == bytes, "refusing the linked sidecar must preserve the note");
        unlink(&row_lock);
        fs::remove_dir_all(root).unwrap();
    }
}
