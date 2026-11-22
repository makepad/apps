//! The link against the fake printer on the loopback address: a stack goes over whole and prints
//! layer by layer; the printer refuses a stack it can't print and says why; a broken connection
//! is an error, never a wait. The linked printer is the person's file; a print on its thread
//! follows the printer and takes pause, resume and stop.

use super::fake::FakePrinter;
use super::printing::{Ask, End, Printing, Seen, Stage, WAIT};
use super::{ResinLink, State, Status};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use crate::export::tests::{block, small};
use crate::export::{write, MANIFEST};
use crate::image::LayerStack;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A 4 mm block's 8 layers exported for the small screen (500 x 700 images once turned), in a
/// folder of `name`'s.
fn exported(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fab-sla-link-{name}-{}", std::process::id())).join("block.layers");
    let p = small();
    write(&dir, &LayerStack::new(vec![block(4.0)], &p), &p).expect("the export");
    dir
}

fn gone(dir: &Path) {
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

fn status(link: &mut ResinLink) -> (State, u32, u32) {
    let Status { state, layer, layers } = link.status().expect("the status");
    (state, layer, layers)
}

/// The stack goes over byte for byte, its layers in order and the manifest last. The job prints
/// as the printer cures its layers, holds while paused, and is done at its last layer.
#[test]
fn a_stack_goes_to_the_printer_and_prints_layer_by_layer() {
    let dir = exported("prints");
    let printer = FakePrinter::start("Simulated resin", [500, 700]);
    let mut link = ResinLink::connect(printer.addr(), WAIT).expect("the link");
    assert_eq!(link.printer(), "Simulated resin");
    let mut heard = Vec::new();
    assert_eq!(link.send_stack(&dir, &mut |k, n| {
        heard.push((k, n));
        true
    }).expect("the stack goes"), 8);
    assert_eq!(heard, (1..=8).map(|k| (k, 8)).collect::<Vec<_>>());
    let files = printer.files();
    assert_eq!(files.len(), 9, "8 layers and the manifest: {:?}", files.keys().collect::<Vec<_>>());
    for (name, bytes) in &files {
        assert_eq!(bytes, &std::fs::read(dir.join(name)).unwrap(), "{name} arrived as it was written");
    }
    let sends: Vec<String> = printer.requests().into_iter().filter(|r| r.starts_with("SEND ")).map(|r| r.split(' ').nth(1).unwrap().to_string()).collect();
    assert_eq!(sends.last().map(String::as_str), Some(MANIFEST), "the manifest goes last: {sends:?}");

    assert_eq!(status(&mut link), (State::Idle, 0, 0));
    link.print().expect("the job starts");
    assert_eq!(status(&mut link), (State::Printing, 0, 8));
    printer.step(3);
    assert_eq!(status(&mut link), (State::Printing, 3, 8));
    link.pause().expect("the job holds");
    printer.step(2);
    assert_eq!(status(&mut link), (State::Paused, 3, 8), "a held job cures nothing");
    link.resume().expect("the job goes on");
    printer.step(5);
    assert_eq!(status(&mut link), (State::Done, 8, 8));
    let e = link.stop().expect_err("nothing prints to stop");
    assert!(e.contains("refused STOP") && e.contains("done"), "{e}");
    drop(link);
    gone(&dir);
}

/// A stack for another screen, a missing layer, a job over a job and an odd file name are
/// refused, each with its reason. A stopped job is stopped.
#[test]
fn the_printer_refuses_a_stack_it_cannot_print() {
    let dir = exported("refuses");
    // the screen mounted the other way: the images don't fit it
    let other = FakePrinter::start("Other screen", [700, 500]);
    let mut link = ResinLink::connect(other.addr(), WAIT).expect("the link");
    link.send_stack(&dir, &mut |_, _| true).expect("the stack goes");
    let e = link.print().expect_err("another screen's stack");
    assert!(e.contains("500 x 700") && e.contains("700 x 500"), "{e}");
    drop(link);

    let printer = FakePrinter::start("Simulated resin", [500, 700]);
    let mut link = ResinLink::connect(printer.addr(), WAIT).expect("the link");
    link.send_stack(&dir, &mut |_, _| true).expect("the stack goes");
    printer.forget("layer_00003.png");
    let e = link.print().expect_err("a layer short");
    assert!(e.contains("layer_00003.png is missing"), "{e}");
    link.send_stack(&dir, &mut |_, _| true).expect("the stack again");
    link.print().expect("the whole stack prints");
    let e = link.print().expect_err("a job over a job");
    assert!(e.contains("printing"), "{e}");
    let e = link.send_stack(&dir, &mut |_, _| true).expect_err("a stack over a printing job");
    assert!(e.contains("refused CLEAR"), "{e}");
    let e = link.send_file("../escape.png", b"x").expect_err("a path is no file name");
    assert!(e.contains("no file name"), "{e}");
    link.stop().expect("the job stops");
    assert_eq!(status(&mut link), (State::Stopped, 0, 8));
    drop(link);
    gone(&dir);
}

/// The connection breaks halfway through the stack: the send fails at once with why, and the
/// next connection finds the printer as it was.
#[test]
fn a_broken_connection_is_an_error_not_a_wait() {
    let dir = exported("breaks");
    let printer = FakePrinter::start("Simulated resin", [500, 700]);
    printer.drop_after(3);
    let started = std::time::Instant::now();
    let mut link = ResinLink::connect(printer.addr(), WAIT).expect("the link");
    let e = link.send_stack(&dir, &mut |_, _| true).expect_err("the connection breaks");
    assert!(e.contains("closed the connection") || e.contains("sending SEND"), "{e}");
    assert!(started.elapsed() < WAIT, "the break is heard at once, not after the wait");
    drop(link);
    let mut link = ResinLink::connect(printer.addr(), WAIT).expect("a new link");
    assert_eq!(status(&mut link), (State::Idle, 0, 0));
    assert_eq!(printer.files().len(), 3, "the files that came before the break");
    drop(link);
    gone(&dir);
}

/// The first `.resin` file by name is the linked printer; the filament printers' `.conf` files
/// aren't. A file that says too little or too much is refused, naming the file and never what
/// it holds; no folder or no file links none. A job's printer is its own file, by name.
#[test]
fn the_linked_printer_is_the_first_resin_file_by_name() {
    use super::linked::{linked_in, named_in, Linked};
    let dir = std::env::temp_dir().join(format!("fab-sla-linked-{}", std::process::id()));
    assert_eq!(linked_in(&dir), Ok(None), "no folder");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("filament.conf"), "host = 127.0.0.1\n").unwrap();
    assert_eq!(linked_in(&dir), Ok(None), "a filament printer's file isn't a resin printer");
    std::fs::write(dir.join("vat.resin"), "# the vat\nhost = 127.0.0.1\nport = \"5005\"\n").unwrap();
    let vat = linked_in(&dir).unwrap().expect("the vat");
    assert_eq!((vat.name.as_str(), format!("{vat:?}")), ("vat", "Linked(vat)".to_string()), "its name, never its address");
    assert_eq!(vat.addr().unwrap(), SocketAddr::from(([127, 0, 0, 1], 5005)));
    std::fs::write(dir.join("a.resin"), "host = 127.0.0.1\n").unwrap();
    assert_eq!(linked_in(&dir), Err("a.resin: no port, or not one".to_string()), "the first by name, as it is");
    assert_eq!(named_in(&dir, "vat").map(|v| v.name), Ok("vat".to_string()), "a job's printer by its file's name");
    assert!(named_in(&dir, "tank").is_err_and(|e| e.starts_with("tank.resin: ")), "a file that went");
    assert_eq!(named_in(&dir, "../vat").map(|v| v.name), Err("\"../vat\" is no printer's file".to_string()), "only a file in the folder");
    std::fs::remove_dir_all(&dir).unwrap();
    for (text, why) in [
        ("port = 5005", "v.resin: no host, or not one"),
        ("host = 127.0.0.1\nport = 0", "v.resin: no port, or not one"),
        ("host = 127.0.0.1\nport = 70000", "v.resin: no port, or not one"),
        ("host = 127.0.0.1\nport = 1\nport = 2", "v.resin: port is set twice"),
        ("host = 127.0.0.1\nport = 1\ncode = 1234", "v.resin: no setting \"code\" (host and port are)"),
        ("host = a b\nport = 1", "v.resin: no host, or not one"),
        ("host 127.0.0.1", "v.resin: a line that isn't key = value"),
    ] {
        assert_eq!(Linked::parse("v", text), Err(why.to_string()), "{text:?}");
    }
}

/// Waits for what the print sees to be `ok`.
fn wait_seen(p: &Printing, what: &str, ok: impl Fn(&Seen) -> bool) -> Seen {
    let started = std::time::Instant::now();
    loop {
        let seen = p.seen();
        if ok(&seen) {
            return seen;
        }
        assert!(started.elapsed() < WAIT, "{what}: the print sees {seen:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn at(seen: &Seen) -> Option<(State, u32, u32)> {
    seen.status.map(|s| (s.state, s.layer, s.layers))
}

/// A print on its thread: the stack goes over, the job starts, and how far it is follows the
/// printer; pause, resume and stop go to the printer, a refused ask says why, and a job ends
/// stopped, or done at its last layer. Every change wakes the owner.
#[test]
fn a_print_follows_the_printer_and_takes_its_asks() {
    let dir = exported("follows");
    let printer = FakePrinter::start("Simulated resin", [500, 700]);
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let p = Printing::start(printer.addr(), dir.clone(), Duration::from_millis(10), Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }))
    .expect("the print");
    let seen = wait_seen(&p, "following", |s| s.stage == Stage::Following && s.status.is_some());
    assert_eq!((seen.printer.as_str(), at(&seen)), ("Simulated resin", Some((State::Printing, 0, 8))));
    assert_eq!(printer.files().len(), 9, "8 layers and the manifest went over");
    printer.step(3);
    wait_seen(&p, "3 layers", |s| at(s) == Some((State::Printing, 3, 8)));
    p.ask(Ask::Pause);
    wait_seen(&p, "paused", |s| at(s) == Some((State::Paused, 3, 8)));
    p.ask(Ask::Pause);
    let refused = wait_seen(&p, "a second pause", |s| s.refused.is_some()).refused.unwrap();
    assert!(refused.contains("refused PAUSE") && refused.contains("paused"), "{refused}");
    printer.step(2);
    p.ask(Ask::Resume);
    let seen = wait_seen(&p, "resumed", |s| at(s) == Some((State::Printing, 3, 8)));
    assert_eq!(seen.refused, None, "the resume went");
    p.ask(Ask::Stop);
    let seen = wait_seen(&p, "stopped", |s| matches!(s.stage, Stage::Ended(_)));
    assert_eq!((seen.stage.clone(), at(&seen)), (Stage::Ended(End::Stopped), Some((State::Stopped, 3, 8))));
    assert!(wakes.load(Ordering::SeqCst) >= 8, "every change wakes the owner");

    // the next print runs to its last layer
    let p = Printing::start(printer.addr(), dir.clone(), Duration::from_millis(10), Arc::new(|| {})).expect("the print");
    wait_seen(&p, "following", |s| s.stage == Stage::Following);
    printer.step(8);
    let seen = wait_seen(&p, "done", |s| matches!(s.stage, Stage::Ended(_)));
    assert_eq!((seen.stage.clone(), at(&seen)), (Stage::Ended(End::Done), Some((State::Done, 8, 8))));
    drop(p);
    gone(&dir);
}

/// A stop while the stack goes over cancels the print: the rest of the stack stays, and the
/// printer is never asked to print. A printer that refuses the stack, or isn't there, fails the
/// print with why.
#[test]
fn a_print_stopped_before_it_starts_prints_nothing() {
    let dir = exported("cancels");
    let printer = FakePrinter::start("Simulated resin", [500, 700]);
    printer.hold(true);
    let p = Printing::start(printer.addr(), dir.clone(), Duration::from_millis(10), Arc::new(|| {})).expect("the print");
    let started = std::time::Instant::now();
    while !printer.requests().iter().any(|r| r.starts_with("SEND ")) {
        assert!(started.elapsed() < WAIT, "no layer went: {:?}", printer.requests());
        std::thread::sleep(Duration::from_millis(5));
    }
    p.ask(Ask::Stop);
    printer.hold(false);
    let seen = wait_seen(&p, "cancelled", |s| matches!(s.stage, Stage::Ended(_)));
    assert_eq!(seen.stage, Stage::Ended(End::Cancelled));
    let asked = printer.requests();
    assert_eq!(asked.iter().filter(|r| r.starts_with("SEND ")).count(), 1, "one layer went: {asked:?}");
    assert!(!asked.iter().any(|r| r == "PRINT"), "the printer was never asked to print: {asked:?}");

    // the stack for another screen
    let other = FakePrinter::start("Other screen", [700, 500]);
    let p = Printing::start(other.addr(), dir.clone(), Duration::from_millis(10), Arc::new(|| {})).expect("the print");
    let seen = wait_seen(&p, "refused", |s| matches!(s.stage, Stage::Ended(_)));
    assert!(matches!(&seen.stage, Stage::Ended(End::Failed(why)) if why.contains("refused PRINT") && why.contains("500 x 700")), "{seen:?}");
    // a printer that isn't there
    let addr = other.addr();
    drop(other);
    let p = Printing::start(addr, dir.clone(), Duration::from_millis(10), Arc::new(|| {})).expect("the print");
    let seen = wait_seen(&p, "no printer", |s| matches!(s.stage, Stage::Ended(_)));
    assert!(matches!(&seen.stage, Stage::Ended(End::Failed(why)) if why.starts_with("connecting to the printer")), "{seen:?}");
    gone(&dir);
}

/// A job found again after Fab restarted is only asked how far it is until an ask comes: it is
/// followed while the printer prints or holds a job of its layers, and says how it ended when
/// the printer ended it. A printer that is idle, or whose job has other layers, has no such job:
/// it ended unseen, and what was seen of it last stays.
#[test]
fn a_job_found_again_is_only_asked_how_far_it_is() {
    let printer = FakePrinter::start("Simulated resin", [500, 700]);
    printer.set_job(State::Printing, 3, 8);
    let only_status = |from: usize| {
        let asked = printer.requests().split_off(from);
        assert!(asked.iter().all(|r| r == "HELLO 1" || r == "STATUS" || r == "BYE"), "nothing but how far it is: {asked:?}");
        asked.iter().filter(|r| *r == "STATUS").count()
    };
    let p = Printing::follow(printer.addr(), 8, Duration::from_millis(10), Arc::new(|| {})).expect("the follow");
    let seen = wait_seen(&p, "following", |s| s.stage == Stage::Following && at(s) == Some((State::Printing, 3, 8)));
    assert_eq!(seen.printer, "Simulated resin");
    printer.step(2);
    wait_seen(&p, "2 more layers", |s| at(s) == Some((State::Printing, 5, 8)));
    let started = std::time::Instant::now();
    while only_status(0) < 5 {
        assert!(started.elapsed() < WAIT, "asked how far it is again and again: {:?}", printer.requests());
        std::thread::sleep(Duration::from_millis(5));
    }
    p.ask(Ask::Pause);
    wait_seen(&p, "paused", |s| at(s) == Some((State::Paused, 5, 8)));
    assert_eq!(printer.requests().iter().filter(|r| !matches!(r.as_str(), "HELLO 1" | "STATUS")).collect::<Vec<_>>(), ["PAUSE"], "the ask, once asked");
    p.ask(Ask::Stop);
    let seen = wait_seen(&p, "stopped", |s| matches!(s.stage, Stage::Ended(_)));
    assert_eq!((seen.stage.clone(), at(&seen)), (Stage::Ended(End::Stopped), Some((State::Stopped, 5, 8))));
    drop(p);

    // it ended while Fab was closed: done or stopped, as the printer says
    for (state, end) in [(State::Done, End::Done), (State::Stopped, End::Stopped)] {
        let from = printer.requests().len();
        printer.set_job(state, 8, 8);
        let p = Printing::follow(printer.addr(), 8, Duration::from_millis(10), Arc::new(|| {})).expect("the follow");
        let seen = wait_seen(&p, "ended", |s| matches!(s.stage, Stage::Ended(_)));
        assert_eq!((seen.stage.clone(), at(&seen)), (Stage::Ended(end), Some((state, 8, 8))));
        drop(p);
        only_status(from);
    }
    // no such job: what was seen of it stays as it was
    for (state, layer, layers) in [(State::Idle, 0, 0), (State::Printing, 2, 12), (State::Done, 12, 12)] {
        let from = printer.requests().len();
        printer.set_job(state, layer, layers);
        let p = Printing::follow(printer.addr(), 8, Duration::from_millis(10), Arc::new(|| {})).expect("the follow");
        let seen = wait_seen(&p, "unseen", |s| matches!(s.stage, Stage::Ended(_)));
        assert_eq!((seen.stage.clone(), seen.status, seen.printer.as_str()), (Stage::Ended(End::Unseen), None, "Simulated resin"), "{state:?} {layers}");
        drop(p);
        only_status(from);
    }
}
