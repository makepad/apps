use super::*;
use makepad_remote::Req;

fn values(line: &str) -> (String, Vec<String>, Vec<(String, String)>) {
    let r = Req::parse(line);
    (r.cmd, r.pos, r.kv)
}
fn inputs() -> Vec<AppInput> {
    let mods = Mods {
        shift: true,
        control: true,
        alt: true,
        logo: true,
    };
    vec![
        AppInput::PointerMove { x: -4., y: 23. },
        AppInput::PointerDown {
            x: 1.,
            y: 2.,
            button: 2,
            mods,
        },
        AppInput::PointerUp {
            x: 2.,
            y: 3.,
            button: 2,
            mods,
        },
        AppInput::Scroll {
            x: 1.,
            y: 1.,
            dx: -2.,
            dy: 3.,
            mods,
        },
        AppInput::KeyDown {
            key: makepad_widgets::KeyCode::KeyA,
            mods,
        },
        AppInput::KeyUp {
            key: makepad_widgets::KeyCode::KeyA,
            mods,
        },
        AppInput::Text("value with \"quotes\"\nand a second line".into()),
    ]
}
#[test]
fn typed_commands_preserve_fields_windows_and_unicode() {
    let w = AppWindow {
        id: WindowId(7), title: "Window".into(), visible: true,
        size: (10., 20.),
        dpi: 1.,
    };
    let expected = [
        "mouse w=7 if_user_seq=42 x=-4 y=23 k=move",
        "mouse w=7 if_user_seq=42 x=1 y=2 b=2 k=down shift=1 ctrl=1 alt=1 logo=1",
        "mouse w=7 if_user_seq=42 x=2 y=3 b=2 k=up shift=1 ctrl=1 alt=1 logo=1",
        "mouse w=7 if_user_seq=42 x=1 y=1 dx=-2 dy=3 k=scroll shift=1 ctrl=1 alt=1 logo=1",
        "key w=7 if_user_seq=42 k=down c=KeyA shift=1 ctrl=1 alt=1 logo=1",
        "key w=7 if_user_seq=42 k=up c=KeyA shift=1 ctrl=1 alt=1 logo=1",
    ];
    for (input, expected) in inputs().iter().zip(expected) {
        let lines = input.lines(&w, 42).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(values(&lines[0]), values(expected));
    }
    let text = "→\"\n".repeat(1600);
    let pieces = AppInput::Text(text.clone()).lines(&w, 42).unwrap();
    assert!(pieces.len() > 1);
    assert_eq!(
        pieces
            .iter()
            .map(|s| Req::parse(s))
            .map(|r| {
                assert_eq!(
                    (r.cmd.as_str(), r.get("w"), r.get("if_user_seq")),
                    ("text", Some("7"), Some("42"))
                );
                r.get("t").unwrap().to_string()
            })
            .collect::<String>(),
        text,
        "split text reaches the app intact"
    );
    assert!(AppInput::PointerMove { x: f64::NAN, y: 0. }
        .lines(&w, 42)
        .is_err());
    assert!(AppInput::Scroll {
        x: 0.,
        y: 0.,
        dx: 0.,
        dy: f64::INFINITY,
        mods: Mods::default()
    }
    .lines(&w, 42)
    .is_err());
    assert!(AppInput::PointerDown {
        x: 0.,
        y: 0.,
        button: 3,
        mods: Mods::default()
    }
    .lines(&w, 42)
    .is_err());

    let mods = Mods::default();
    for input in [
        AppInput::PointerDown { x: -1., y: 1., button: 0, mods },
        AppInput::Scroll { x: 1., y: 21., dx: 1., dy: 0., mods },
        AppInput::PointerMove { x: -4., y: 91. },
        AppInput::PointerUp { x: -10.01, y: 0., button: 0, mods },
    ] {
        assert!(input.lines(&w, 42).is_err(), "refuse {input:?} without moving it to an edge");
    }

    for (op, expected) in design_cases() {
        let windowed = op.clone().line(Some(w.id));
        match expected {
            "design pick" | "design tree" | "design props" | "design apply" | "design typed" | "design locate" | "design theme" |
            "design states" | "design pose" | "design verify" | "design drop" => assert_eq!(values(&windowed.unwrap()), values(&format!("{expected} w=7"))),
            _ => {
                let command = expected.split_whitespace().take_while(|word| !word.contains('=')).collect::<Vec<_>>().join(" ");
                let reason = format!("{command} takes no window");
                assert_eq!(windowed.unwrap_err(), reason);
                let mut links = AppLinks::default();
                let ticket = links.design(LinkId(1), Some(w.id), op.clone());
                assert!(matches!(links.poll().as_slice(), [AppLinkEvent::Design { ticket: t, ok: false, json, .. }]
                    if *t == ticket && *json == reason), "the caller gets the local refusal");
            }
        }
        assert_eq!(
            values(&op.line(None).unwrap()),
            values(expected),
            "wire command {expected}"
        );
    }
    let quoted = DesignOp::ApplySplash {
        path: "a b".into(),
        splash: "{x: \"a\\b\"}\n".into(),
    }
    .line(None)
    .unwrap();
    assert_eq!(Req::parse(&quoted).get("splash"), Some("{x: \"a\\b\"}\n"));
    let pick = DesignOp::Design {
        verb: DesignVerb::Pick,
        args: vec![("at".into(), "4,5".into())],
    };
    assert_eq!(
        values(&pick.line(Some(w.id)).unwrap()),
        values("design pick at=4,5 w=7")
    );
    for k in ["cmd", "w", "k=x", "a b", "if_user_seq"] {
        assert!(DesignOp::Design {
            verb: DesignVerb::Pick,
            args: vec![(k.into(), "x".into())]
        }
        .line(None)
        .is_err());
    }
}
fn design_cases() -> Vec<(DesignOp, &'static str)> {
    use DesignVerb::*;
    let mut ops = vec![
        (DesignOp::Overlay(true), "tweak on=1"),
        (DesignOp::Annotate(false), "tweak annotate=0"),
        (DesignOp::State, "tweak state"),
        (DesignOp::Diff, "tweak diff"),
        (DesignOp::Clear, "tweak clear"),
        (DesignOp::Final, "tweak final"),
        (
            DesignOp::ApplyProp {
                path: "a".into(),
                prop: "size".into(),
                value: "2".into(),
            },
            "tweak apply path=a prop=size value=2",
        ),
        (
            DesignOp::ApplySplash {
                path: "a".into(),
                splash: "{}".into(),
            },
            "tweak apply path=a splash={}",
        ),
    ];
    for (verb, line) in [
        (Open, "design open"),
        (State, "design state"),
        (Palette, "design palette"),
        (Insert, "design insert"),
        (Move, "design move"),
        (Delete, "design delete"),
        (Duplicate, "design duplicate"),
        (Wrap, "design wrap"),
        (Up, "design up"),
        (Down, "design down"),
        (Out, "design out"),
        (Rename, "design rename"),
        (Set, "design set"),
        (Bake, "design bake"),
        (Undo, "design undo"),
        (Redo, "design redo"),
        (Reset, "design reset"),
        (Patch, "design patch"),
        (Commit, "design commit"),
        (Verify, "design verify"),
        (Close, "design close"),
        (Pick, "design pick"),
        (Tree, "design tree"),
        (Props, "design props"),
        (Apply, "design apply"),
        (Typed, "design typed"),
        (Locate, "design locate"),
        (Theme, "design theme"),
        (States, "design states"),
        (Pose, "design pose"),
        (Preview, "design preview"),
        (PreviewChunk, "design preview_chunk"),
        (Revert, "design revert"),
        (Drop, "design drop"),
    ] {
        ops.push((DesignOp::Design { verb, args: vec![] }, line));
    }
    ops
}
#[test]
fn windows_carrier_requires_numeric_local_address_and_registry_token() {
    let token = "b4".repeat(32);
    let entry = |socket: &str, token: &str| {
        makepad_remote::discovery::parse(&format!(
            "{{\"pid\":1,\"socket\":{},\"app\":\"app\",\"token\":{}}}",
            makepad_remote::json_str(socket),
            makepad_remote::json_str(token)
        ))
        .unwrap()
    };
    for address in ["tcp:127.0.0.1:1234", "tcp:[::1]:1234"] {
        assert!(endpoint::windows_endpoint(&entry(address, &token)).is_ok());
        for bad in ["", "abc"] {
            assert!(endpoint::windows_endpoint(&entry(address, bad)).is_err());
        }
    }
    for address in [
        "tcp:localhost:1234",
        "tcp:127.0.0.2:1234",
        "tcp:192.0.2.1:1234",
        "tcp:[::ffff:127.0.0.1]:1234",
        "tcp:127.0.0.1:0",
        "host:a",
        "relay:a",
        "hub:a",
    ] {
        assert!(
            endpoint::windows_endpoint(&entry(address, &token)).is_err(),
            "{address}"
        );
    }
}
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::Rig as RecordingRig;

#[test]
fn scanning_stays_true_until_a_coalesced_rescan_finishes() {
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    struct Progress {
        entered: usize,
        released: usize,
    }

    let (wake_tx, wake_rx) = mpsc::channel();
    let signal: Arc<dyn Fn() + Send + Sync> = Arc::new(move || wake_tx.send(()).unwrap());
    let mut app = AppLinks::default();
    app.configure(AppLinkEnv {
        signal: signal.clone(),
        registry: None,
        codecs: Arc::new(makepad_call::driver::codec::TestCodecs),
        spawn: Arc::new(|_, _| panic!("the test already installed its scan worker")),
    });
    let (request, rx) = mpsc::sync_channel(1);
    let (tx, result) = mpsc::sync_channel(1);
    let requested = Arc::new(AtomicU64::new(0));
    app.scan = Some(Scan { request, requested: requested.clone(), result });
    let progress = Arc::new((Mutex::new(Progress::default()), Condvar::new()));
    let worker_progress = progress.clone();
    let worker = std::thread::spawn(move || run_scan_worker(rx, requested, tx, signal, || {
        let (lock, wake) = &*worker_progress;
        let mut p = lock.lock().unwrap();
        p.entered += 1;
        let invocation = p.entered;
        wake.notify_all();
        drop(wake.wait_while(p, |p| p.released < invocation).unwrap());
        let apps = if invocation == 1 {
            Vec::new()
        } else {
            let key = AppKey {
                pid: 7, started_ms: 1, socket: "app.sock".into(), run_dir: None, restart_parent: None,
            };
            vec![LocalApp {
                pid: key.pid, started_ms: key.started_ms, key, name: "Makepad".into(),
                title: None, visible_windows: Vec::new(),
            }]
        };
        (apps, Vec::new())
    }));

    let (lock, wake) = &*progress;
    app.scan();
    drop(wake.wait_while(lock.lock().unwrap(), |p| p.entered == 0).unwrap());
    app.scan();
    app.scan();
    assert_eq!(app.scan_requested, 3, "coalesced requests still advance the requested generation");

    lock.lock().unwrap().released = 1;
    wake.notify_all();
    wake_rx.recv().unwrap();
    assert!(app.poll().iter().any(|e| matches!(e, AppLinkEvent::Apps)), "the first discovery result is published");
    assert!(app.scanning() && app.apps().is_empty(), "the first reply leaves the newer scan pending");
    lock.lock().unwrap().released = 2;
    wake.notify_all();
    wake_rx.recv().unwrap();
    assert!(app.poll().iter().any(|e| matches!(e, AppLinkEvent::Apps)), "the coalesced discovery result is published");
    assert!(!app.scanning(), "the rescan satisfies both queued refresh requests");
    assert!(matches!(app.apps(), [found] if found.name == "Makepad"), "the picker receives the fresh registry contents");
    drop(app);
    worker.join().expect("closing discovery lets the scan worker exit");
}

#[test]
fn restart_notice_requires_an_unambiguous_replacement() {
    let app = |pid: u32, name: &str, dir: Option<&str>, parent: Option<u32>| LocalApp {
        key: AppKey {
            pid,
            started_ms: u64::from(pid),
            socket: PathBuf::from(format!("app-{pid}.sock")),
            run_dir: dir.map(str::to_owned),
            restart_parent: parent,
        },
        pid,
        started_ms: u64::from(pid),
        name: name.into(), title: None, visible_windows: Vec::new(),
    };
    let old = app(4, "app", Some("cache"), Some(3));
    assert_eq!(
        old.key,
        app(4, "app", None, None).key,
        "run metadata does not change the selected endpoint"
    );
    let sibling = app(5, "app", Some("cache"), Some(2));
    let by_dir = app(6, "app", Some("cache"), None);
    let by_parent = app(7, "app", Some("new-cache"), Some(3));
    let mut watch = endpoint::RestartWatch::default();
    assert!(watch
        .replacement(&old, false, &[old.clone(), sibling.clone()])
        .is_none());
    assert!(
        watch.replacement(&old, true, &[sibling]).is_none(),
        "a known sibling cannot be the restart"
    );
    for other in [
        app(7, "other", Some("cache"), Some(3)),
        app(7, "app", Some("other"), Some(2)),
        app(7, "app", None, None),
        app(2, "app", Some("cache"), None),
    ] {
        assert!(watch.replacement(&old, true, &[other]).is_none());
    }
    assert!(watch
        .replacement(&old, true, &[by_dir.clone(), by_parent.clone()])
        .is_none());
    assert!(
        watch
            .replacement(&old, true, &[old.clone(), by_dir.clone()])
            .is_none(),
        "wait for the departing registry entry to clear"
    );
    for replacement in [by_dir, by_parent] {
        let one = std::slice::from_ref(&replacement);
        assert!(
            watch.replacement(&old, false, one).is_none(),
            "the old link is still live"
        );
        assert_eq!(
            watch.replacement(&old, true, one).unwrap().pid,
            replacement.pid
        );
    }
    let native = app(4, "app", None, None);
    assert!(watch
        .replacement(&native, true, &[app(6, "app", None, None)])
        .is_some());
}

#[cfg(unix)]
impl RecordingRig {
    pub(crate) fn set_preview_codecs(&mut self, codecs: Arc<dyn Codecs>) {
        self.app.env.as_mut().unwrap().codecs = codecs;
    }
}

#[test]
fn a_preview_delivers_a_picture_that_finishes_after_push_without_another_unit() {
    use makepad_call::{driver::codec::{Decoder, Encoder, Picture, StreamFormat, TestCodecs}, guard::Rung};
    use std::sync::{Condvar, Mutex};

    #[derive(Default)]
    struct Progress { pushes: usize, polls: usize, release: bool }
    struct DelayedCodecs(Arc<(Mutex<Progress>, Condvar)>);
    struct DelayedDecoder {
        progress: Arc<(Mutex<Progress>, Condvar)>,
        inner: Box<dyn Decoder>,
        staged: Vec<Picture>,
    }
    impl Codecs for DelayedCodecs {
        fn encoder(&self, rung: Rung, kbps: u32) -> Result<Box<dyn Encoder>, String> { TestCodecs.encoder(rung, kbps) }
        fn decoder(&self, key_au: &[u8]) -> Result<Box<dyn Decoder>, String> {
            Ok(Box::new(DelayedDecoder { progress: self.0.clone(), inner: TestCodecs.decoder(key_au)?, staged: Vec::new() }))
        }
    }
    impl Decoder for DelayedDecoder {
        fn format(&self) -> StreamFormat { self.inner.format() }
        fn push_into(&mut self, au: &[u8], pts: i64, _: &mut Vec<Picture>) -> Result<(), String> {
            self.inner.push_into(au, pts, &mut self.staged)?;
            self.progress.0.lock().unwrap().pushes += 1;
            self.progress.1.notify_one();
            Ok(())
        }
        fn ready_into(&mut self, out: &mut Vec<Picture>) -> Result<(), String> {
            let mut state = self.progress.0.lock().unwrap();
            state.polls += 1;
            if state.release { out.append(&mut self.staged); }
            self.progress.1.notify_one();
            Ok(())
        }
        fn recycle(&mut self, nv12: Vec<u8>) { self.inner.recycle(nv12); }
        fn allocated(&self) -> u64 { self.inner.allocated() }
        fn name(&self) -> String { self.inner.name() }
    }

    let progress = Arc::new((Mutex::new(Progress::default()), Condvar::new()));
    let workers = Arc::new((Mutex::new(0usize), Condvar::new()));
    let tracked = workers.clone();
    let (tx, rx) = mpsc::channel();
    let env = AppLinkEnv {
        spawn: Arc::new(move |_, work| {
            let tracked = tracked.clone();
            *tracked.0.lock().unwrap() += 1;
            std::thread::spawn(move || {
                work();
                *tracked.0.lock().unwrap() -= 1;
                tracked.1.notify_one();
            });
        }),
        signal: Arc::new(move || { let _ = tx.send(()); }),
        codecs: Arc::new(DelayedCodecs(progress.clone())),
        registry: None,
    };
    let want = PictureWant { window: WindowId(0), max_w: 100, max_h: 100, fps: 15, kbps: 0, format: None };
    let (preview, slot) = PicturePreview::new(&env, want);
    let rung = Rung { width: 320, height: 160, fps: 15, min_ask_bps: 0, full_bps: 1_000 };
    let mut encoder = env.codecs.encoder(rung, 1).unwrap();
    let pixels = vec![128; 320 * 160 * 3 / 2];
    let mut relay = |pts| {
        let packet = encoder.push(&pixels, pts).unwrap().pop().unwrap();
        let key = packet.key;
        slot.relay(Arc::new(packet), (320, 160), key, makepad_call::driver::codec::StreamFormat::H264);
    };
    relay(0);
    let mut state = progress.1.wait_while(progress.0.lock().unwrap(), |s| s.pushes != 1 || s.polls == 0).unwrap();
    assert!(preview.take().is_none(), "an empty poll cannot publish a preview");
    state.release = true;
    drop(state);
    rx.recv().expect("the first picture wakes the page without a second access unit");
    let picture = preview.take().expect("the late picture reaches the preview");
    assert_eq!((picture.pts_100ns, picture.width, picture.height, picture.nv12.len()), (0, 100, 50, 7500),
        "the preview fits the original picture to the page");

    progress.0.lock().unwrap().release = false;
    relay(666_667);
    drop(progress.1.wait_while(progress.0.lock().unwrap(), |s| s.pushes != 2).unwrap());
    assert_eq!(*workers.0.lock().unwrap(), 1, "the worker still owes the second picture");
    drop(preview);
    drop(workers.1.wait_while(workers.0.lock().unwrap(), |n| *n != 0).unwrap());
    assert!(slot.demand().is_none(), "closing the page ends the worker and its demand while a picture is pending");
}

#[test]
fn a_preview_shows_each_new_app_picture_at_the_page_size_until_dropped() {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let env = AppLinkEnv {
        spawn: Arc::new(|_, job| { std::thread::spawn(job); }),
        signal: Arc::new(move || { let _ = tx.lock().unwrap().send(()); }),
        codecs: makepad_call::driver::codec::platform_codecs(),
        registry: None,
    };
    let rung = makepad_call::guard::Rung {
        width: 320, height: 160, fps: 15, min_ask_bps: 0, full_bps: 4_000_000,
    };
    let mut encoder = match env.codecs.encoder(rung, 4_000) {
        Ok(encoder) => encoder,
        Err(why) => {
            eprintln!("preview test skipped: platform encoder unavailable: {why}");
            return;
        }
    };
    let want = PictureWant { window: WindowId(0), max_w: 100, max_h: 100, fps: 15, kbps: 0, format: None };
    let (preview, slot) = PicturePreview::new(&env, want);
    let mut nv12 = vec![96u8; 320 * 160 * 3 / 2];
    for uv in nv12[320 * 160..].chunks_exact_mut(2) {
        uv.copy_from_slice(&[80, 176]);
    }
    for (key, pts, luma) in [(true, 0, 96), (false, 666_667, 104)] {
        nv12[..320 * 160].fill(luma);
        let units = encoder.push(&nv12, pts).expect("encode preview fixture");
        assert_eq!(units.len(), 1, "the low-latency encoder returns the frame");
        let packet = units.into_iter().next().unwrap();
        assert_eq!(packet.key, key, "the preview decodes a key followed by its delta");
        if key {
            if let Err(why) = env.codecs.decoder(&packet.data) {
                eprintln!("preview test skipped: platform decoder unavailable: {why}");
                return;
            }
        }
        slot.relay(Arc::new(packet), (320, 160), key, makepad_call::driver::codec::StreamFormat::H264);
        rx.recv().unwrap();
        let picture = preview.take().expect("the decoded picture reaches the page");
        assert_eq!((picture.width, picture.height, picture.nv12.len()), (100, 50, 100 * 50 * 3 / 2),
            "the picture fits the page and keeps the window's shape");
        assert!(picture.nv12[25 * 100 + 50].abs_diff(luma) <= 6, "the decoded frame's luma");
        let uv = &picture.nv12[100 * 50..];
        assert!(uv[0].abs_diff(80) <= 6 && uv[1].abs_diff(176) <= 6, "UV pairs retain their order");
        assert!(preview.take().is_none(), "each picture reaches the page once");
    }
    preview.want(40, 40, 15);
    rx.recv().unwrap();
    let smaller = preview.take().expect("a new page size redraws the held picture");
    assert_eq!((smaller.width, smaller.height), (40, 20), "a smaller page gets a smaller picture");
    assert!(smaller.nv12[10 * 40 + 20].abs_diff(104) <= 6, "the resized picture retains the delta");
    assert!(slot.demand().is_some(), "the app keeps sending while the page shows the preview");
    drop(preview);
    assert!(slot.demand().is_none(), "closing the page ends the preview's demand on the app");
}
