use super::*;
use makepad_call::driver::codec::{Packet, TestCodecs};
use makepad_remote::{
    audio::{encode_audio_record, AudioRecord},
    conn::{ChannelTx, CLOSE_REFUSED, CLOSE_UNSUPPORTED},
    Conn, Endpoint, Event,
};
use std::{
    net::Shutdown,
    os::unix::net::{UnixListener, UnixStream},
    sync::{Condvar, Mutex},
    thread,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Data {
    lines: Vec<String>,
    seq: u64,
    touch_on_activity: bool,
    conflict: Option<bool>,
    gated: bool,
    hold_input: bool,
    input_waiting: bool,
    frames: Vec<Feed>,
    audio_opens: usize,
    audio_channels: Vec<ChannelTx>,
    audio_refusal: Option<(u16, String)>,
    video_opens: Vec<(usize, f64, u32, u32)>,
    raw_opens: usize,
    controls: Vec<(usize, String)>,
    refuse_video: bool,
    omit_codecs: bool,
    seeds: Vec<usize>,
    clock: u64,
    path: Option<String>,
    captures: u32,
    wide_window: bool,
    refuse_still: bool,
    peers: Vec<Conn>,
}
struct Feed {
    window: usize,
    scale: f64,
    fps: u32,
    last: Option<u64>,
    channel: ChannelTx,
    video: bool,
    last_pts: Option<i64>,
}
#[derive(Default)]
struct Fake {
    data: Mutex<Data>,
    release: Condvar,
}
fn dimensions(w: usize) -> (u32, u32) {
    if w == 3 {
        (24, 16)
    } else {
        (16, 24)
    }
}
fn record(w: usize, scale: f64, time: u64) -> Vec<u8> {
    let (width, height) = dimensions(w);
    let (width, height) = (
        (f64::from(width) * scale).round() as u32,
        (f64::from(height) * scale).round() as u32,
    );
    let mut pixels = vec![if w == 3 { 0 } else { 255 }; (4 * width * height) as usize];
    // Each present changes a pixel, leaving the first pixel as the window identity.
    pixels[4] = (time / 1_000_000_000) as u8;
    pixels[5] = (time / 100) as u8;
    makepad_remote::frames::encode_record(
        &format!(
            "{{\"w\":{width},\"h\":{height},\"window\":{w},\"t_ns\":{time},\"codec\":\"raw\"}}"
        ),
        &pixels,
    )
}
// A detailed, opaque image stresses the automatic poster transfer budget.
fn noisy_window(w: usize, h: usize) -> Vec<u8> {
    let mut state = 1u32;
    let mut pixels = vec![0; w * h * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        pixel.copy_from_slice(&state.to_le_bytes());
        pixel[3] = 255;
    }
    makepad_remote::frames::encode_record(&format!(r#"{{"w":{w},"h":{h},"window":4,"t_ns":0,"codec":"raw"}}"#), &pixels)
}

// The call fixture can decode this test access unit. The relay must leave every byte intact.
fn unit_bytes(window: usize, w: u32, h: u32, key: bool, pts: i64) -> Vec<u8> {
    let mut data = vec![b'T', u8::from(key)];
    data.extend_from_slice(&(w as u16).to_be_bytes());
    data.extend_from_slice(&(h as u16).to_be_bytes());
    data.extend_from_slice(&(window as u32).to_be_bytes());
    data.resize(16, 0);
    data.extend_from_slice(&pts.to_be_bytes());
    data
}

impl Feed {
    fn send(&mut self, time: u64, key: bool, advertise: bool) -> bool {
        let msg = if self.video {
            let (w, h) = dimensions(self.window);
            let even = |n| ((f64::from(n) * self.scale).round() as u32).max(2) & !1;
            let (w, h) = (even(w), even(h));
            let pts = (time / 100) as i64;
            let pts = self.last_pts.map_or(pts, |last| pts.max(last + 1));
            self.last_pts = Some(pts);
            let caps = if advertise { r#","codecs":["h264"],"full_chroma":false"# } else { "" };
            let json = format!(r#"{{"codec":"h264","window":{},"w":{w},"h":{h},"pts":{pts},"key":{key}{caps}}}"#, self.window);
            makepad_remote::frames::encode_record(&json, &unit_bytes(self.window, w, h, key, pts))
        } else {
            record(self.window, self.scale, time)
        };
        match self.channel.try_send(msg) {
            Ok(()) => {
                self.last = Some(time);
                true
            }
            Err(makepad_remote::conn::SendError::Closed) => false,
            Err(makepad_remote::conn::SendError::Full(_)) => panic!("fake app frame channel full"),
        }
    }
}
impl Fake {
    fn present(&self, window: usize) {
        self.present_after(window, 1_000_000_000);
    }
    fn present_after(&self, window: usize, advance_ns: u64) -> usize {
        let mut d = self.data.lock().unwrap();
        d.clock += advance_ns;
        let now = d.clock;
        let mut sent = 0;
        let mut closed = false;
        d.frames.retain_mut(|feed| {
            if feed.window == window
                && feed
                    .last
                    .is_none_or(|last| now - last >= 1_000_000_000 / u64::from(feed.fps))
            {
                if !feed.send(now, false, false) {
                    closed = true;
                    return false;
                }
                sent += 1;
            }
            true
        });
        if closed { self.release.notify_all(); }
        sent
    }
    fn answer(&self, line: &str, answers: &mut Vec<String>) {
        let mut d = self.data.lock().unwrap();
        d.lines.push(line.into());
        self.release.notify_all();
        while d.gated {
            d = self.release.wait(d).unwrap();
        }
        let r = Req::parse(line);
        if matches!(r.cmd.as_str(), "mouse" | "key" | "text") && d.hold_input {
            d.input_waiting = true;
            self.release.notify_all();
            while d.hold_input { d = self.release.wait(d).unwrap(); }
            d.input_waiting = false;
        }
        let answer = match r.cmd.as_str() {
            "status" if d.wide_window =>
                r#"{"w":[{"i":3,"title":"Canvas","visible":true,"sz":[24,16],"dpi":1},{"i":4,"title":"Details","visible":true,"sz":[640,360],"dpi":2}]}"#.into(),
            "status" => {
                r#"{"w":[{"i":3,"title":"Canvas","visible":true,"sz":[24,16],"dpi":1},{"i":4,"title":"Details","visible":true,"sz":[16,24],"dpi":1}]}"#.into()
            }
            "activity" if std::mem::take(&mut d.touch_on_activity) => {
                d.seq += 1;
                format!("{{\"user_seq\":{},\"idle_ms\":0}}", d.seq)
            }
            "activity" => format!("{{\"user_seq\":{},\"idle_ms\":null}}", d.seq),
            "mouse" | "key" | "text" => {
                if let Some(applied) = d.conflict.take() {
                    d.seq += 1;
                    format!("{{\"err\":\"user_intervened\",\"applied\":{applied}}}")
                } else if r.num("if_user_seq", u64::MAX) != d.seq {
                    r#"{"err":"user_intervened","applied":false}"#.into()
                } else {
                    r#"{"ok":1}"#.into()
                }
            }
            "cap" if r.pos == ["start"] => {
                assert!(r.get("audio").is_none(), "capture is silent");
                d.path = Some(r.get("path").unwrap().into());
                r#"{"ok":1,"fps":30}"#.into()
            }
            "cap" if r.pos == ["stop"] => {
                let path = d.path.take().unwrap();
                // The finalized movie spans 75 slots at 30 fps, including gaps between its 45 frames.
                let mut movie = Vec::from(36u32.to_be_bytes());
                movie.extend(b"moov");
                movie.extend(28u32.to_be_bytes());
                movie.extend(b"mvhd");
                movie.extend([0; 12]);
                movie.extend(30u32.to_be_bytes());
                movie.extend(75u32.to_be_bytes());
                std::fs::write(&path, movie).unwrap();
                d.captures += 1;
                format!(
                    "{{\"ok\":1,\"path\":{},\"frames\":45,\"sz\":[24,16]}}",
                    makepad_remote::json_str(&path)
                )
            }
            "design" if r.pos == ["pose"] => r#"{"err":"unknown command operation"}"#.into(),
            "design" | "tweak" => r#"{"ok":1}"#.into(),
            _ => panic!("unexpected request {line}"),
        };
        answers.push(answer);
    }
    fn inputs(&self) -> Vec<String> {
        self.data
            .lock()
            .unwrap()
            .lines
            .iter()
            .filter(|s| matches!(Req::parse(s).cmd.as_str(), "mouse" | "key" | "text"))
            .cloned()
            .collect()
    }
}
pub(crate) struct Rig {
    pub(crate) app: AppLinks,
    fake: Arc<Fake>,
    pub(crate) registry: PathBuf,
    socket: PathBuf,
    caps: String,
    done: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
    wakes: Receiver<()>,
    panics: Arc<AtomicU64>,
    workers: Arc<AtomicU64>,
    pub(crate) events: Vec<AppLinkEvent>,
}
impl Rig {
    pub(crate) fn new() -> Self {
        let pid = std::process::id();
        let (stem, registry) = loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let stem = format!("al-{pid}-{n}");
            let registry = std::env::temp_dir().join(&stem);
            match std::fs::create_dir(&registry) {
                Ok(()) => break (stem, registry),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("{err}"),
            }
        };
        let socket = makepad_remote::carrier::unix::own_socket_dir()
            .unwrap()
            .join(stem);
        let listener = UnixListener::bind(&socket).unwrap();
        let ep = Endpoint::new("app");
        let fake = Arc::new(Fake::default());
        let f = fake.clone();
        ep.register(
            "cmd",
            1,
            makepad_remote::lines::service("cmd", Arc::new(move |line, out| f.answer(line, out))),
        );
        let f = fake.clone();
        ep.register(
            "frames",
            1,
            Arc::new(move |_, open| {
                let args = Req::parse(&format!("frames {}", open.args));
                let w = args
                    .get("w").or_else(|| args.get("window"))
                    .expect("explicit capture window")
                    .parse()
                    .unwrap();
                let scale = args.get("scale").unwrap().parse().unwrap();
                let mut d = f.data.lock().unwrap();
                if args.get("cap") == Some("1") {
                    let fps = args.get("fps").unwrap().parse().unwrap();
                    let video = args.get("codec") == Some("h264");
                    if args.get("codec") == Some("raw") { d.raw_opens += 1; }
                    let mut feed = Feed { window: w, scale, fps, last: None, channel: open.channel.tx(),
                        video, last_pts: None };
                    if video {
                        assert_eq!(args.get("maxqp"), Some("26"));
                        let kbps = args.get("kbps").unwrap().parse().unwrap();
                        d.video_opens.push((w, scale, fps, kbps));
                        if std::mem::take(&mut d.refuse_video) {
                            return Err((makepad_remote::conn::CLOSE_REFUSED, "unknown codec".into()));
                        }
                        if !feed.send(d.clock, true, !std::mem::take(&mut d.omit_codecs)) { return Ok(()); }
                    }
                    let id = open.channel.id();
                    d.frames.push(feed);
                    let fake = f.clone();
                    thread::spawn(move || {
                        while let Some(Event::Data(msg)) = open.channel.recv() {
                            let text = String::from_utf8(msg).unwrap();
                            let mut d = fake.data.lock().unwrap();
                            let now = d.clock;
                            let Some(feed) = d.frames.iter_mut().find(|f| f.channel.id() == id && f.window == w) else { break };
                            if text == "key" { if !feed.send(now, true, false) { break; } }
                            else if let Some(rate) = text.strip_prefix("fps=") { feed.fps = rate.parse().unwrap(); }
                            else { assert!(text.starts_with("kbps=")); }
                            d.controls.push((w, text));
                            fake.release.notify_all();
                        }
                        fake.data.lock().unwrap().frames.retain(|f| f.channel.id() != id || f.window != w);
                        fake.release.notify_all();
                    });
                } else {
                    assert_eq!(args.get("once"), Some("1"));
                    d.seeds.push(w);
                    if std::mem::take(&mut d.refuse_still) {
                        open.channel.close(makepad_remote::conn::CLOSE_NORMAL, "no drawable");
                        return Ok(());
                    }
                    let frame = if d.wide_window && w == 4 {
                        if scale == 1.0 { noisy_window(1280, 720) } else if scale == 240. / 1280. {
                            noisy_window(240, 135)
                        } else {
                            assert_eq!(scale, 0.5, "the poster scale uses the listed physical window size");
                            noisy_window(640, 360)
                        }
                    } else { record(w, scale, d.clock) };
                    open.channel.send(frame).unwrap();
                    open.channel.close(makepad_remote::conn::CLOSE_NORMAL, "");
                }
                Ok(())
            }),
        );
        let (tx, wakes) = mpsc::channel();
        let audio_wake = tx.clone();
        let f = fake.clone();
        ep.register("audio", 1, Arc::new(move |_, open| {
            assert!(open.args.is_empty(), "app-wide audio opens without arguments");
            let mut d = f.data.lock().unwrap();
            d.audio_opens += 1;
            let refusal = d.audio_refusal.take();
            if refusal.is_none() {
                d.audio_channels.push(open.channel.tx());
            }
            f.release.notify_all();
            let _ = audio_wake.send(());
            drop(d);
            if let Some(reason) = refusal { return Err(reason); }

            let fake = f.clone();
            let wake = audio_wake.clone();
            thread::spawn(move || {
                while let Some(event) = open.channel.recv() {
                    if matches!(event, Event::Closed(..)) { break; }
                }
                fake.data.lock().unwrap().audio_channels.retain(|tx| tx.id() != open.channel.id());
                fake.release.notify_all();
                let _ = wake.send(());
            });
            Ok(())
        }));
        // Advertise the other services of a standalone app and its HAL host.
        ep.register_watch();
        for name in ["observe", "prof", "app", "gpuprof"] {
            ep.register(
                name,
                1,
                Arc::new(|_, _| {
                    Err((
                        makepad_remote::conn::CLOSE_REFUSED,
                        "unused fixture service".into(),
                    ))
                }),
            );
        }
        let caps = ep
            .caps()
            .iter()
            .map(|c| makepad_remote::json_str(c))
            .collect::<Vec<_>>()
            .join(",");
        let done = Arc::new(AtomicBool::new(false));
        let ended = done.clone();
        let f = fake.clone();
        let server = thread::spawn(move || {
            for s in listener.incoming() {
                let s = s.unwrap();
                if ended.load(Ordering::Acquire) {
                    break;
                }
                let r = makepad_remote::frame::FrameReader::new(Box::new(s.try_clone().unwrap()));
                let writer = Box::new(s.try_clone().unwrap());
                if let Ok(conn) = ep.serve(
                    r,
                    writer,
                    Box::new(move || {
                        let _ = s.shutdown(Shutdown::Both);
                    }),
                ) {
                    f.data.lock().unwrap().peers.push(conn);
                }
            }
        });
        let signal: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.send(());
        });
        let workers = Arc::new(AtomicU64::new(0));
        let panics = Arc::new(AtomicU64::new(0));
        let (n, b, wake) = (workers.clone(), panics.clone(), signal.clone());
        let spawn: Spawn = Arc::new(move |_, job| {
            n.fetch_add(1, Ordering::Release);
            let (n, b, wake) = (n.clone(), b.clone(), wake.clone());
            thread::spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    b.fetch_add(1, Ordering::Release);
                }
                n.fetch_sub(1, Ordering::Release);
                wake();
            });
        });
        // The fixture shares our OS process. Give the coordinator a distinct logical identity.
        let mut app = AppLinks::default();
        app.own = 0;
        app.configure(AppLinkEnv {
            registry: Some(registry.clone()),
            spawn,
            signal,
            codecs: Arc::new(TestCodecs),
        });
        let rig = Self {
            app,
            fake,
            registry,
            socket,
            caps,
            done,
            server: Some(server),
            wakes,
            workers,
            panics,
            events: vec![],
        };
        rig.advertise(&jit_fields(false));
        rig
    }
    fn advertise(&self, extra: &str) {
        self.advertise_socket(self.socket.to_str().unwrap(), extra);
    }
    fn advertise_socket(&self, socket: &str, extra: &str) {
        self.advertise_run(std::process::id(), socket, 7, &self.caps, extra);
    }
    fn advertise_run(&self, pid: u32, socket: &str, start: u64, caps: &str, extra: &str) {
        let socket = makepad_remote::json_str(socket);
        let json = format!(
            r#"{{"pid":{pid},"app":"app","socket":{socket},"started_ms":{start},"caps":[{caps}],"title":"Canvas","windows":{{"3":"Canvas","4":"Details"}}{extra}}}"#
        );
        std::fs::write(
            self.registry.join(format!("makepad-remote-{pid}.json")),
            json,
        )
        .unwrap();
    }
    pub(crate) fn restart(&mut self, id: LinkId) -> (u32, UnixListener, PathBuf) {
        for c in &self.fake.data.lock().unwrap().peers {
            c.close("app quit");
        }
        self.ended(id);
        // Both simulated runs live inside this test. Borrow the harness parent's live PID for the
        // second registry record's liveness check. Only this fixture owns its socket and registry.
        unsafe extern "C" {
            fn getppid() -> i32;
        }
        let next_pid = unsafe { getppid() } as u32;
        let next_socket = self.socket.with_extension("next");
        let listener = UnixListener::bind(&next_socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let next_path = next_socket.to_str().unwrap();
        std::fs::remove_file(
            self.registry
                .join(format!("makepad-remote-{}.json", std::process::id())),
        )
        .unwrap();
        self.advertise_run(next_pid, next_path, 8, &self.caps, &jit_fields(false));
        self.rescan();
        (next_pid, listener, next_socket)
    }

    fn rescan(&mut self) {
        let mark = self.events.len();
        self.app.scan();
        self.wait(|r| {
            r.events[mark..]
                .iter()
                .any(|e| matches!(e, AppLinkEvent::Apps)) && !r.app.scanning()
        });
    }
    pub(crate) fn wait(&mut self, mut until: impl FnMut(&mut Self) -> bool) {
        loop {
            self.events.extend(self.app.poll());
            assert_eq!(self.panics.load(Ordering::Acquire), 0, "worker panicked");
            if until(self) {
                return;
            }
            self.wakes.recv().unwrap();
        }
    }
    pub(crate) fn wide_window(&self) { self.fake.data.lock().unwrap().wide_window = true; }
    pub(crate) fn refuse_next_still(&self) { self.fake.data.lock().unwrap().refuse_still = true; }
    pub(crate) fn hold_input(&self, hold: bool) {
        self.fake.data.lock().unwrap().hold_input = hold;
        self.fake.release.notify_all();
    }
    pub(crate) fn wait_for_held_input(&self) {
        let data = self.fake.data.lock().unwrap();
        drop(self.fake.release.wait_while(data, |d| !d.input_waiting).unwrap());
    }
    pub(crate) fn input_lines(&self) -> Vec<String> { self.fake.inputs() }
    pub(crate) fn send_sound(&self, first_frame: u64, frames: usize, t_ns: u64) {
        let pcm = (0..frames).flat_map(|f| {
            let left = ((first_frame + f as u64) % 30_000) as i16;
            [left, -left]
        }).collect();
        self.send_raw_sound(encode_audio_record(&AudioRecord { first_frame, rate: 48_000, gen: 0, t_ns, pcm }));
    }
    pub(crate) fn send_raw_sound(&self, bytes: Vec<u8>) {
        let channels = self.fake.data.lock().unwrap().audio_channels.clone();
        for channel in channels { channel.send(bytes.clone()).unwrap(); }
    }
    pub(crate) fn audio_open_count(&self) -> usize { self.fake.data.lock().unwrap().audio_opens }
    pub(crate) fn audio_channels_open(&self) -> usize { self.fake.data.lock().unwrap().audio_channels.len() }
    fn link_app(&mut self) -> LinkId {
        self.rescan();
        let link = self.app.link(&self.app.apps()[0].key.clone());
        self.live(link);
        link
    }
    pub(crate) fn local_input(&self) { self.fake.data.lock().unwrap().seq += 1; }
    pub(crate) fn live(&mut self, id: LinkId) {
        self.wait(|r| {
            let s = r.app.state(id).unwrap();
            assert!(
                !matches!(s.phase, LinkPhase::Ended(_)),
                "link failed: {:?}",
                s.phase
            );
            s.phase == LinkPhase::Live
        });
    }
    fn ended(&mut self, id: LinkId) {
        self.wait(|r| {
            r.events
                .iter()
                .any(|e| matches!(e,AppLinkEvent::Ended{id:i,..} if *i==id))
        });
    }
    fn design_result(&mut self, ticket: Ticket, expected: bool) {
        self.wait(|r| {
            r.events.iter().any(|e| match e {
                AppLinkEvent::Design {
                    ticket: t,
                    ok,
                    json,
                    ..
                } if *t == ticket => {
                    assert_eq!(*ok, expected, "design answer: {json}");
                    true
                }
                _ => false,
            })
        });
    }
    pub(crate) fn barrier(&mut self, id: LinkId) {
        let t = self.app.design(id, None, DesignOp::State);
        self.design_result(t, true);
    }
    fn packet(&mut self, p: &PictureStream) -> Packet {
        let mut packet = None;
        self.wait(|_| {
            packet = p.pop();
            assert!(packet.is_some() || !p.ended(), "picture stream ended");
            packet.is_some()
        });
        packet.unwrap()
    }
    fn refused_all(&mut self, id: LinkId) {
        let logged = self.fake.inputs();
        let count = self.app.inputs_refused(id);
        for i in inputs() {
            self.app.inject(id, WindowId(3), i);
        }
        self.barrier(id);
        assert_eq!(self.fake.inputs(), logged, "no input without current Allow");
        assert_eq!(self.app.inputs_refused(id), count + 7);
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.app.shutdown();
        let mut d = self.fake.data.lock().unwrap_or_else(|e| e.into_inner());
        d.gated = false;
        d.hold_input = false;
        self.fake.release.notify_all();
        for c in d.peers.drain(..) {
            c.close("app quit");
        }
        drop(d);
        self.done.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket);
        if let Some(t) = self.server.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(self.socket.with_extension("next"));
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.registry);
    }
}

// Match the extra fields written by inspect::start, including the optional process parent.
fn jit_fields(test_run: bool) -> String {
    let extra = format!(
        r#","session_log":"run/session.log","run":"unknown","run_dir":"cache","agent":false,"test_run":{test_run},"live_log":"run/live.log""#
    );
    if cfg!(target_os = "macos") {
        format!("{extra},\"restart_parent\":3")
    } else {
        extra
    }
}

#[test]
fn picker_lists_visible_windows_and_explains_a_refused_or_vanished_choice() {
    use crate::record::capture::{Purpose, WindowPicker};
    use crate::call::{ShareChoice, ShareRow};

    let mut rig = Rig::new();
    let entry = rig.registry.join(format!("makepad-remote-{}.json", std::process::id()));
    let original = std::fs::read_to_string(&entry).unwrap();
    let publish = |windows: &str| {
        std::fs::write(&entry, original.replace(r#"{"3":"Canvas","4":"Details"}"#, windows)).unwrap();
    };
    let menu = |rig: &mut Rig, picker: &mut WindowPicker, rows: &mut Option<Arc<[ShareRow]>>| {
        picker.close();
        picker.toggle(Purpose::Share);
        // Opening the picker requests a scan. Apply its reply just as the UI does.
        let mark = rig.events.len();
        picker.poll(&mut rig.app, &[], rows);
        rig.wait(|r| r.events[mark..].iter().any(|e| matches!(e, AppLinkEvent::Apps)) && !r.app.scanning());
        picker.poll(&mut rig.app, &rig.events[mark..], rows);
    };
    let mut picker = WindowPicker::default();
    let mut rows = None;
    rig.wide_window();
    rig.rescan();
    picker.toggle(Purpose::Share);
    picker.poll_rows(&mut rig.app, &[], &mut rows, false, Some(&|_, _| None));
    let first_rows = rows.as_ref().unwrap().clone();
    assert_eq!(first_rows.len(), 2);
    assert!(first_rows.iter().all(|r| r.selectable && r.thumbnail.as_ref().unwrap().picture.get().is_none()),
        "every window is selectable before its first picture");
    rig.wait(|r| {
        picker.poll_rows(&mut r.app, &[], &mut rows, false, Some(&|_, _| None));
        first_rows.iter().all(|row| row.thumbnail.as_ref().unwrap().picture.get().is_some())
    });
    for (row, (w, h, pixel)) in first_rows.iter().zip([(24, 16, 0), (240, 135, 0xff212004)]) {
        let Some(Ok(crate::record::capture::ThumbnailPicture::Still(p))) = row.thumbnail.as_ref().unwrap().picture.get() else {
            panic!("missing window thumbnail for {}", row.label)
        };
        assert_eq!((p.width, p.height), (w, h), "scale the drawable by physical width, without enlarging small windows");
        assert_eq!(p.data[0], pixel, "the row shows its own drawable's pixel");
    }
    assert_eq!(rig.fake.data.lock().unwrap().seeds, [3, 4], "one grab per window on opening");
    assert!(rig.app.linked().next().is_none(), "previewing a registry window leaves selection to the person");
    publish("{}");
    menu(&mut rig, &mut picker, &mut rows);
    assert!(rows.as_ref().unwrap().iter().all(|r| !r.selectable), "a process without visible windows has no selectable row");

    publish(r#"{"17":"Notes","9":"Notes","5":"Notes"}"#);
    menu(&mut rig, &mut picker, &mut rows);
    let shown = rows.as_ref().unwrap();
    assert_eq!(shown.iter().map(|r| &*r.label).collect::<Vec<_>>(), ["App · Notes", "App · Notes · 2", "App · Notes · 3"]);
    let named_window = |w| format!("window_{}_{w}", std::process::id());
    let names = shown.iter().map(|r| r.widget_id.to_string()).collect::<Vec<_>>();
    assert_eq!(names, [named_window(5), named_window(9), named_window(17)], "widget paths identify windows even when all titles match");
    let ShareChoice::Row(key) = shown[1].choice else { panic!("expected a window row") };
    assert!(picker.choose(key));
    picker.poll(&mut rig.app, &[], &mut rows);
    rig.wait(|r| r.app.linked().any(|l| l.phase == LinkPhase::Live));
    let (_, result) = picker.poll(&mut rig.app, &rig.events, &mut rows);
    assert_eq!(result.unwrap().1.unwrap_err(), "The selected window is no longer visible.",
        "an exact window selection never falls through to another window");

    let id = rig.app.linked().next().unwrap().id;
    let live = Arc::new(makepad_call::driver::codec::Picture {
        width: 2, height: 2, nv12: vec![32, 64, 96, 128, 128, 128], pts_100ns: 0,
    });
    let shared = |link, window| ((link, window) == (id, WindowId(3))).then(|| Some(live.clone()));
    let before = rig.fake.data.lock().unwrap().seeds.len();
    rig.refuse_next_still();
    picker.toggle(Purpose::Share);
    picker.poll_rows(&mut rig.app, &[], &mut rows, false, Some(&shared));
    rig.wait(|r| {
        picker.poll_rows(&mut r.app, &[], &mut rows, false, Some(&shared));
        rows.as_ref().unwrap().iter().all(|row| row.thumbnail.as_ref().unwrap().picture.get().is_some())
    });
    let shown = rows.as_ref().unwrap();
    assert!(matches!(shown[0].thumbnail.as_ref().unwrap().picture.get(), Some(Ok(crate::record::capture::ThumbnailPicture::Shared(p))) if Arc::ptr_eq(p, &live)),
        "the shared window reuses its live preview");
    assert!(shown[1].selectable && shown[1].thumbnail.as_ref().unwrap().picture.get().unwrap().is_err(),
        "a failed preview leaves its window selectable");
    assert_eq!(rig.fake.data.lock().unwrap().seeds[before..], [4], "only the unshared window was grabbed");

    rig.app.unlink(id);
    rig.ended(id);
    publish(r#"{"3":"Makepad","4":"Details"}"#);
    menu(&mut rig, &mut picker, &mut rows);
    assert_eq!(rows.as_ref().unwrap().iter().map(|r| &*r.label).collect::<Vec<_>>(), ["App", "App · Details"]);
    assert!(picker.choose_first());
    // The same process is now ineligible. Resolve must refuse it when the row is activated.
    rig.advertise(&format!(r#"{},"relay":"elsewhere""#, jit_fields(false)));
    picker.poll(&mut rig.app, &[], &mut rows);
    rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::Ended { id: other, .. } if *other != id)));
    let why = rig.events.iter().find_map(|e| match e {
        AppLinkEvent::Ended { id: other, why } if *other != id => Some(why.clone()), _ => None,
    }).unwrap();
    let (_, result) = picker.poll(&mut rig.app, &rig.events, &mut rows);
    assert_eq!(result.unwrap().1.unwrap_err(), why, "the picker forwards the endpoint's refusal");
}

#[test]
fn quit_waits_for_the_button_release_reply_and_a_second_close_can_force_exit() {
    // Each case starts with a fresh coordinator, as a quit seals its input for life.
    for second_close in [false, true] {
        let mut rig = Rig::new();
        rig.rescan();
        let key = rig.app.apps()[0].key.clone();
        let id = rig.app.link(&key);
        rig.live(id);
        let window = rig.app.state(id).unwrap().windows[0].id;
        rig.app.allow_input(id);
        assert!(!rig.app.input_settling(), "Allow alone owes no release");
        rig.app.inject(id, window, AppInput::PointerDown { x: 2., y: 3., button: 0, mods: Mods::default() });
        rig.barrier(id);
        rig.hold_input(true);
        assert_eq!(rig.app.quit_input(false), InputQuit::Wait, "the first close must keep the event loop alive");
        rig.wait_for_held_input();
        let lines = rig.input_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(Req::parse(&lines[0]).get("k"), Some("down"));
        assert_eq!(Req::parse(&lines[1]).get("k"), Some("up"));
        assert!(rig.app.input_settling(), "writing Up to the socket is too early to quit");
        rig.app.allow_input(id);
        assert!(!rig.app.state(id).unwrap().input_allowed, "input cannot reopen during quit");

        if second_close {
            assert_eq!(rig.app.quit_input(false), InputQuit::Wait, "a second close preserves unfinished store work");
            assert_eq!(rig.app.quit_input(true), InputQuit::Force { links_owing_release: 1 },
                "a second close with only app links settling quits while the fake still holds the Up reply");
            for conn in &rig.fake.data.lock().unwrap().peers { conn.close("app quit"); }
            rig.ended(id);
            assert!(!rig.app.input_settling(), "a dead link has no outstanding acknowledgement");
        } else {
            rig.hold_input(false);
            rig.wait(|r| !r.app.input_settling());
            assert_eq!(rig.app.quit_input(true), InputQuit::Ready, "the Up acknowledgement permits ordinary quit");
            assert_eq!(rig.input_lines(), lines, "the worker released the button exactly once");
            assert_eq!(rig.app.state(id).unwrap().phase, LinkPhase::Live, "the recorder can still use the link");
        }
    }
}

#[test]
fn a_share_sound_opens_the_apps_audio_once_and_relays_its_records_in_order() {
    let mut rig = Rig::new();
    let link = rig.link_app();
    let first = rig.app.open_sound(link).unwrap();
    rig.wait(|r| r.audio_channels_open() == 1);
    assert_eq!(rig.audio_open_count(), 1, "the first holder opens one app-wide audio channel");
    for i in 0..3 {
        rig.send_sound(i * 480, 480, 1_000_000_000 + i * 10_000_000);
    }
    rig.wait(|_| first.stats().records == 3);
    for i in 0..3 {
        let record = first.pop().expect("all three records are queued");
        assert_eq!((record.first_frame, record.t_ns), (i * 480, 1_000_000_000 + i * 10_000_000),
            "the relay preserves record order and the app clock");
        assert_eq!((record.rate, record.gen), (48_000, 0), "the source rate and generation survive");
        let expected: Vec<_> = (i * 480..(i + 1) * 480).flat_map(|f| [f as i16, -(f as i16)]).collect();
        assert_eq!(record.pcm, expected, "every stereo sample survives the relay");
    }
    assert_eq!(first.stats().frames, 1_440, "delivery counts stereo frames");
    let second = rig.app.open_sound(link).unwrap();
    rig.barrier(link); // The second subscription has reached the worker before the next record.
    rig.send_sound(1_440, 480, 1_030_000_000);
    rig.wait(|_| first.stats().records == 4 && second.stats().records == 1);
    let next = first.pop().expect("the original holder received the next record");
    assert_eq!(next.first_frame, 1_440, "the original holder continues in order");
    assert_eq!(second.pop(), Some(next), "the second holder receives the same record");
    assert_eq!(rig.audio_open_count(), 1, "both holders share one channel");
    drop(first);
    drop(second);
    rig.wait(|r| r.audio_channels_open() == 0);
    let _third = rig.app.open_sound(link).unwrap();
    rig.wait(|r| r.audio_channels_open() == 1);
    assert_eq!(rig.audio_open_count(), 2, "a later holder reopens the channel");
}

#[test]
fn an_app_without_audio_ends_the_sound_with_the_reason_and_keeps_its_picture() {
    let mut rig = Rig::new();
    let link = rig.link_app();
    rig.fake.data.lock().unwrap().audio_refusal = Some((CLOSE_UNSUPPORTED, "unknown service".into()));
    let unsupported = rig.app.open_sound(link).unwrap();
    rig.wait(|_| unsupported.ended());
    assert!(unsupported.ended_why().unwrap().contains("has no audio service"), "an older app explains its silent share");
    let picture = rig.app.open_picture(link, PictureWant {
        window: WindowId(3), max_w: 24, max_h: 16, fps: 30, kbps: 100,
    }).unwrap();
    assert!(rig.packet(&picture).key, "the same link still delivers its picture");

    let busy = "the app's audio taps are all in use";
    rig.fake.data.lock().unwrap().audio_refusal = Some((CLOSE_REFUSED, busy.into()));
    let refused = rig.app.open_sound(link).unwrap();
    rig.wait(|_| refused.ended());
    assert_eq!(refused.ended_why().as_deref(), Some(busy), "temporary refusal preserves the app's reason");
    let retry = rig.app.open_sound(link).unwrap();
    rig.wait(|r| r.audio_channels_open() == 1);
    rig.send_sound(960, 480, 1_020_000_000);
    rig.wait(|_| retry.stats().records == 1);
    assert_eq!(retry.pop().unwrap().first_frame, 960, "a new holder receives sound after refusal");
}

#[test]
fn a_holder_that_falls_behind_hears_the_newest_second() {
    let mut rig = Rig::new();
    let link = rig.link_app();
    let sound = rig.app.open_sound(link).unwrap();
    rig.wait(|r| r.audio_channels_open() == 1);
    for i in 0..75 {
        rig.send_sound(i * 1_920, 1_920, i * 40_000_000);
    }
    rig.wait(|_| sound.stats().records == 75);
    let stats = sound.stats();
    assert_eq!(stats.frames, 75 * 1_920, "all delivered frames count, including later evictions");
    assert_eq!(stats.dropped_records, 50, "the oldest two seconds were evicted");
    let mut frames = 0;
    for i in 50..75 {
        let record = sound.pop().expect("the entire newest second remains queued");
        assert_eq!(record.first_frame, i * 1_920, "the retained records form an ordered suffix through the last send");
        frames += record.pcm.len() / 2;
    }
    assert_eq!(frames, 48_000, "the holder retained one second of stereo frames");
    assert!(sound.pop().is_none(), "no extra records exceed the frame budget");
}

#[test]
fn a_bad_audio_record_ends_the_sound_with_its_error() {
    let mut rig = Rig::new();
    let link = rig.link_app();
    let sound = rig.app.open_sound(link).unwrap();
    rig.wait(|r| r.audio_channels_open() == 1);
    rig.send_sound(0, 1, 42);
    let mono = r#"{"first":1,"rate":48000,"ch":1,"gen":0,"t_ns":43}"#;
    rig.send_raw_sound(makepad_remote::frames::encode_record(mono, &[0; 4]));
    rig.wait(|_| sound.ended_why().is_some());
    assert!(sound.ended_why().unwrap().contains("stereo"), "the holder sees the decoder's stereo error");
    assert!(!sound.ended(), "a valid record preceding the error can still be drained");
    assert_eq!(sound.pop().unwrap().t_ns, 42, "the valid record survives the error");
    assert!(sound.ended() && sound.pop().is_none(), "the malformed record ends the stream without being relayed");
}

#[test]
fn fake_present_retires_a_closed_feed_without_panicking() {
    let mut rig = Rig::new();
    rig.rescan();
    let link = rig.app.link(&rig.app.apps()[0].key.clone());
    rig.live(link);
    let want = PictureWant { window: WindowId(3), max_w: 24, max_h: 16, fps: 30, kbps: 100 };
    let first = rig.app.open_picture(link, want).unwrap();
    assert!(rig.packet(&first).key, "the first stream starts with a keyframe");
    let stale_tx = rig.fake.data.lock().unwrap().frames[0].channel.clone();
    drop(first);
    let data = rig.fake.data.lock().unwrap();
    drop(rig.fake.release.wait_while(data, |d| !d.frames.is_empty()).unwrap());

    let live = rig.app.open_picture(link, want).unwrap();
    assert!(rig.packet(&live).key, "the replacement stream starts with its own keyframe");
    rig.fake.data.lock().unwrap().frames.insert(0, Feed { window: 3, scale: 1., fps: 30,
        last: None, channel: stale_tx, video: true, last_pts: None });
    assert_eq!(rig.fake.present_after(3, 1_000_000_000), 1, "only the live feed counts as a send");
    assert_eq!(rig.fake.data.lock().unwrap().frames.len(), 1, "presentation retires the closed feed");
    let delta = rig.packet(&live);
    assert!(!delta.key && delta.data == unit_bytes(3, 24, 16, false, delta.pts_100ns),
        "the live feed delivers the window's delta frame");
}

#[test]
fn preview_and_share_open_one_video_channel_only() {
    fn next_preview(rig: &mut Rig, preview: &PicturePreview) -> i64 {
        let mut picture = None;
        rig.wait(|_| {
            assert_eq!(preview.probe().ended_why(), None, "preview ended before its picture");
            picture = preview.take();
            picture.is_some()
        });
        let pic = picture.unwrap();
        assert_eq!((pic.width, pic.height, pic.nv12.len()), (12, 8, 12 * 8 * 3 / 2));
        pic.pts_100ns
    }

    let mut rig = Rig::new();
    rig.rescan();
    let link = rig.app.link(&rig.app.apps()[0].key.clone());
    rig.live(link);
    let want = PictureWant { window: WindowId(3), max_w: 12, max_h: 8, fps: 15, kbps: 0 };
    let preview = rig.app.open_preview(link, want).unwrap();
    next_preview(&mut rig, &preview);
    {
        let d = rig.fake.data.lock().unwrap();
        assert_eq!(d.video_opens, [(3, 0.5, 15, 4_000)], "a lone preview sets the local bitrate");
        assert_eq!(d.raw_opens, 0);
        assert!(d.seeds.is_empty());
    }

    let share = rig.app.open_picture(link, PictureWant { fps: 30, kbps: 600, ..want }).unwrap();
    let joined = rig.packet(&share);
    assert!(joined.key);
    assert_eq!(next_preview(&mut rig, &preview), joined.pts_100ns);
    {
        let d = rig.fake.data.lock().unwrap();
        assert_eq!((d.video_opens.len(), d.frames.len()), (1, 1), "both use the existing subscription");
        assert!(d.controls.contains(&(3, "kbps=600".into())));
        assert!(d.controls.contains(&(3, "fps=30".into())));
    }

    share.want(24, 16, 30);
    let reopened = rig.packet(&share);
    assert!(reopened.key);
    assert_eq!(share.size(), (24, 16));
    assert_eq!(next_preview(&mut rig, &preview), reopened.pts_100ns, "preview survives the scale change");
    {
        let d = rig.fake.data.lock().unwrap();
        let d = rig.fake.release.wait_while(d, |d| d.frames.len() != 1).unwrap();
        assert_eq!(d.video_opens, [(3, 0.5, 15, 4_000), (3, 1., 30, 600)]);
        assert_eq!(d.frames[0].scale, 1.);
    }
    drop(preview);

    // Leave one delta in the share slot and one on the channel before the preview joins.
    let controls = rig.fake.data.lock().unwrap().controls.len();
    let received = share.stats().received;
    rig.fake.present(3);
    share.wait_received(received + 1);
    rig.fake.present(3);
    let late = rig.app.open_preview(link, want).unwrap();
    rig.barrier(link);
    for pts in [10_000_000, 20_000_000] {
        let delta = rig.packet(&share);
        assert_eq!((delta.key, delta.pts_100ns), (false, pts), "joining preserves queued share deltas");
        assert_eq!(delta.data, unit_bytes(3, 24, 16, false, pts));
    }
    let key = rig.packet(&share);
    assert!(key.key && key.pts_100ns > 20_000_000);
    assert_eq!(next_preview(&mut rig, &late), key.pts_100ns, "the late preview starts on the same key");
    {
        let d = rig.fake.data.lock().unwrap();
        assert!(d.controls[controls..].contains(&(3, "key".into())));
        assert_eq!((d.video_opens.len(), d.frames.len(), d.raw_opens, d.seeds.len()), (2, 1, 0, 0));
    }

    drop(late);
    drop(share);
    {
        let d = rig.fake.data.lock().unwrap();
        let mut d = rig.fake.release.wait_while(d, |d| !d.frames.is_empty()).unwrap();
        d.refuse_video = true;
    }
    let refused = rig.app.open_preview(link, want).unwrap();
    rig.wait(|_| refused.probe().ended_why().is_some());
    assert_eq!(refused.probe().ended_why().as_deref(), Some("app can't encode video; restart it with a current build"));
    let d = rig.fake.data.lock().unwrap();
    assert!(d.frames.is_empty() && d.raw_opens == 0 && d.seeds.is_empty(), "no raw fallback on refusal");
}

#[test]
fn local_app_link_shares_windows_and_revokes_takeover() {
    let mut rig = Rig::new();
    assert!(
        endpoint::discover_all(&rig.registry, std::process::id()).0.is_empty(),
        "own endpoint is excluded"
    );
    rig.app.scan();
    rig.wait(|r| !r.app.apps().is_empty());
    let key = rig.app.apps()[0].key.clone();
    let jit = jit_fields(false);
    for key_name in ["via", "host", "route", "relay", "hub"] {
        rig.advertise(&format!(r#"{jit},"{key_name}":"elsewhere""#));
        assert!(endpoint::discover_all(&rig.registry, 0).0.is_empty());
        let refused = rig.app.link(&key);
        rig.ended(refused);
        assert!(
            rig.fake.data.lock().unwrap().peers.is_empty(),
            "routed entry wasn't dialed"
        );
    }
    for service in ["route", "relay"] {
        rig.advertise_run(
            std::process::id(),
            rig.socket.to_str().unwrap(),
            7,
            &format!(r#"{},"{service}/1""#, rig.caps),
            &jit,
        );
        assert!(endpoint::discover_all(&rig.registry, 0).0.is_empty());
        let refused = rig.app.link(&key);
        rig.ended(refused);
    }
    rig.advertise(&jit_fields(true));
    assert!(
        endpoint::discover_all(&rig.registry, 0).0.is_empty(),
        "test runs stay hidden"
    );
    assert!(endpoint::discover_all(&rig.registry, 0).1.is_empty(), "a test run is not named as an app Town can't reach");
    let refused = rig.app.link(&key);
    rig.ended(refused);
    assert!(
        rig.fake.data.lock().unwrap().peers.is_empty(),
        "test runs aren't dialed"
    );
    for socket in ["tcp:127.0.0.1:1", "tcp:192.0.2.1:1", "hub:app", "relay:app"] {
        rig.advertise_socket(socket, "");
        assert!(endpoint::discover_all(&rig.registry, 0).0.is_empty());
        if socket == "tcp:192.0.2.1:1" {
            let (_, unreachable) = endpoint::discover_all(&rig.registry, 0);
            assert_eq!(unreachable.iter().map(|(_, why)| why.as_str()).collect::<Vec<_>>(), ["outside the user's sockets"],
                "the Share list names an app Town can't reach, with the reason");
        }
        let refused = rig.app.link(&key);
        rig.ended(refused);
        assert!(rig.fake.data.lock().unwrap().peers.is_empty());
    }
    rig.advertise(&jit);
    let id = rig.app.link(&key);
    rig.live(id);
    assert_eq!(rig.app.state(id).unwrap().windows.len(), 2);
    rig.refused_all(id);
    rig.app.allow_input(id);
    rig.wait(|r| {
        r.fake
            .data
            .lock()
            .unwrap()
            .lines
            .iter()
            .any(|s| s == "activity")
    });
    rig.fake.data.lock().unwrap().seq += 1;
    rig.wait(|r| !r.app.state(id).unwrap().input_allowed);
    assert_eq!(
        rig.events
            .iter()
            .filter(|e| matches!(e, AppLinkEvent::LocalInput(_)))
            .count(),
        1,
        "a touch before the first injection ends Allow"
    );
    rig.refused_all(id);
    rig.app.allow_input(id);
    for input in inputs() {
        rig.app.inject(id, WindowId(3), input);
    }
    rig.barrier(id);
    let lines = rig.fake.inputs();
    assert_eq!(lines.len(), 7);
    let window = &rig.app.state(id).unwrap().windows[0];
    for (actual, input) in lines.iter().zip(inputs()) {
        assert_eq!(
            actual,
            &input.lines(window, 1).unwrap()[0],
            "exact guarded wire request"
        );
    }
    let long = "→a ".repeat(2600);
    rig.app
        .inject(id, WindowId(3), AppInput::Text(long.clone()));
    rig.barrier(id);
    let wire = rig.fake.inputs();
    assert_eq!(
        wire[lines.len()..]
            .iter()
            .map(|s| Req::parse(s).get("t").unwrap().to_owned())
            .collect::<String>(),
        long
    );
    let delivered = rig.app.inputs_delivered(id);
    for n in 0..100 {
        rig.app
            .inject(id, WindowId(3), AppInput::Text(n.to_string()));
    }
    rig.barrier(id);
    assert!(rig.app.state(id).unwrap().input_allowed);
    assert_eq!(
        rig.app.inputs_delivered(id),
        delivered + 100,
        "ordinary applied input preserves Allow"
    );
    rig.app.revoke_input(id);
    rig.refused_all(id);
    // No baseline has returned yet. The app's idle time dates this touch after the Allow press.
    rig.fake.data.lock().unwrap().touch_on_activity = true;
    rig.app.allow_input(id);
    rig.wait(|r| !r.app.state(id).unwrap().input_allowed);
    assert_eq!(
        rig.events
            .iter()
            .filter(|e| matches!(e, AppLinkEvent::LocalInput(i) if *i == id))
            .count(),
        2
    );
    rig.refused_all(id);
    for applied in [true, false] {
        rig.app.allow_input(id);
        rig.fake.data.lock().unwrap().conflict = Some(applied);
        let before = rig.fake.inputs().len();
        let (ok, refused) = (rig.app.inputs_delivered(id), rig.app.inputs_refused(id));
        rig.app
            .inject(id, WindowId(3), AppInput::Text("once".into()));
        rig.wait(|r| !r.app.state(id).unwrap().input_allowed);
        rig.barrier(id);
        assert_eq!(rig.app.inputs_delivered(id), ok + u64::from(applied));
        assert_eq!(rig.app.inputs_refused(id), refused + u64::from(!applied));
        assert_eq!(
            rig.fake.inputs().len(),
            before + 1,
            "an interrupted input is never replayed"
        );
        rig.refused_all(id);
    }
    let want = PictureWant {
        window: WindowId(3),
        max_w: 24,
        max_h: 16,
        fps: 60,
        kbps: 100,
    };
    let a = rig.app.open_picture(id, want).unwrap();
    assert!(rig.packet(&a).key, "a still window starts with a key");
    let b = rig.app.open_picture(id, want).unwrap();
    let c = rig
        .app
        .open_picture(
            id,
            PictureWant {
                window: WindowId(4),
                max_w: 16,
                max_h: 24,
                ..want
            },
        )
        .unwrap();
    for (stream, w, size) in [(&a, 3, (24, 16)), (&b, 3, (24, 16)), (&c, 4, (16, 24))] {
        let k = rig.packet(stream);
        assert!(k.key, "an idle window and a late viewer both get a key");
        assert_eq!(k.data, unit_bytes(w, size.0, size.1, true, k.pts_100ns), "selected window's own bytes");
    }
    {
        let d = rig.fake.data.lock().unwrap();
        assert_eq!(d.video_opens, [(3, 1., 60, 100), (4, 1., 60, 100)], "one video subscription per window");
        assert!(d.seeds.is_empty(), "the app supplies the first key without a raw seed");
    }
    rig.barrier(id);
    assert!(a.pop().is_none() && b.pop().is_none() && c.pop().is_none(), "still windows produce no repeats");
    let control_start = rig.fake.data.lock().unwrap().controls.len();
    a.set_kbps(80);
    a.want(12, 8, 30);
    b.want(24, 16, 30);
    a.key_now();
    for stream in [&a, &b] { assert!(rig.packet(stream).key); }
    {
        let d = rig.fake.data.lock().unwrap();
        assert_eq!(d.video_opens.len(), 2, "the largest consumer still wants the original scale");
        for command in ["kbps=80", "fps=30", "key"] {
            assert!(d.controls[control_start..].contains(&(3, command.into())), "{command} reached the app");
        }
    }
    let sessions = a.stats().sessions;
    b.want(12, 8, 30);
    for stream in [&a, &b] {
        assert!(rig.packet(stream).key, "the reopened stream starts with a key");
        assert_eq!(stream.size(), (12, 8));
    }
    assert_eq!(rig.fake.data.lock().unwrap().video_opens.last(), Some(&(3, 0.5, 30, 80)));
    assert_eq!(a.stats().sessions, sessions + 1);
    rig.fake.present(3);
    rig.packet(&b); // Leave a unit queued in a's outgoing session.
    assert_eq!(rig.fake.present_after(3, 1), 0, "the capture rate limits presents");
    a.want(0, 0, 0);
    rig.fake.present(3);
    let held = rig.packet(&b);
    assert!(a.pop().is_none(), "a paused session retires its queued unit");
    assert_eq!(a.stats().sessions, sessions + 1, "zero want doesn't start an encoder");
    a.want(12, 8, 30);
    assert!(rig.packet(&a).key, "a resumed viewer can decode its first unit");
    assert!(rig.packet(&b).key);
    let received = b.stats().received;
    let mut expected = Vec::new();
    for _ in 0..5 {
        rig.fake.present(3);
        expected.push((rig.fake.data.lock().unwrap().clock / 100) as i64);
    }
    b.wait_received(received + 1);
    rig.barrier(id);
    assert_eq!(b.stats().received, received + 1, "a full slot leaves later records on the channel");
    for pts in expected {
        assert!(held.pts_100ns < pts);
        for stream in [&a, &b] {
            let packet = rig.packet(stream);
            assert_eq!((packet.key, packet.pts_100ns), (false, pts), "every predicted unit stays in order");
            assert_eq!(packet.data, unit_bytes(3, 12, 8, false, pts));
        }
    }
    assert_eq!(c.stats().received, 1, "another window's presents never reach the stream");
    // The app presents one final change before its next capture slot, then stays still.
    // A replacement key of the previously held pixels alone would freeze the viewer there.
    rig.fake.present(4);
    let previous = rig.packet(&c);
    assert_eq!(rig.fake.present_after(4, 1_000), 0, "the live channel missed the last present");
    let final_pts = (rig.fake.data.lock().unwrap().clock / 100) as i64;
    let received = c.stats().received;
    c.key_now();
    c.wait_received(received + 1);
    let mut packet = rig.packet(&c);
    if packet.pts_100ns != final_pts { packet = rig.packet(&c); }
    assert!(packet.pts_100ns > previous.pts_100ns);
    assert_eq!(packet.pts_100ns, final_pts, "the key request recovered the actual final present without another app update");
    drop(c);
    rig.barrier(id);
    for refused_open in [true, false] {
        {
            let mut d = rig.fake.data.lock().unwrap();
            d.refuse_video = refused_open;
            d.omit_codecs = !refused_open;
        }
        let refused = rig.app.open_picture(id, PictureWant { window: WindowId(4), ..want }).unwrap();
        rig.wait(|_| refused.ended());
        assert_eq!(refused.probe().ended_why().as_deref(), Some("app can't encode video; restart it with a current build"));
    }
    for (op, expected) in design_cases() {
        let t = rig.app.design(id, None, op);
        let ok = expected != "design pose";
        rig.design_result(t, ok);
        assert!(rig
            .fake
            .data
            .lock()
            .unwrap()
            .lines
            .iter()
            .any(|s| values(s) == values(expected)));
    }
    let pick = rig.app.design(
        id,
        Some(WindowId(4)),
        DesignOp::Design {
            verb: DesignVerb::Pick,
            args: vec![],
        },
    );
    rig.design_result(pick, true);
    assert!(rig
        .fake
        .data
        .lock()
        .unwrap()
        .lines
        .iter()
        .any(|s| values(s) == values("design pick w=4")));
    let start = rig.app.record_start(id, WindowId(3), &rig.registry);
    rig.wait(|r| r.app.state(id).unwrap().recording == Some(start));
    let stop = rig.app.record_stop(id);
    rig.wait(|r| {
        r.events.iter().any(|e| match e {
            AppLinkEvent::Recorded { ticket, result, .. } if *ticket == stop => {
                let clip = result.as_ref().expect("recording finished");
                assert_eq!((clip.frames, clip.secs, clip.w, clip.h), (45, 2.5, 24, 16));
                assert!(clip.path.is_file());
                true
            }
            _ => false,
        })
    });
    let start = rig.app.record_start(id, WindowId(3), &rig.registry);
    rig.wait(|r| r.app.state(id).unwrap().recording == Some(start));
    rig.app.allow_input(id);
    rig.app.unlink(id);
    rig.ended(id);
    assert_eq!(
        rig.fake.data.lock().unwrap().captures,
        2,
        "unlink finished the second clip"
    );
    rig.wait(|r| r.workers.load(Ordering::Acquire) == 1); // only the registry worker
    let id = rig.app.link(&key);
    rig.live(id);
    rig.refused_all(id);
    // All UI methods return even while the app withholds every reply. More commands than the
    // channel can hold exercise retention and ordered delivery after pressure.
    let count = {
        let mut d = rig.fake.data.lock().unwrap();
        d.gated = true;
        d.lines.len()
    };
    let waiting = rig.app.design(id, None, DesignOp::Diff);
    {
        let mut d = rig.fake.data.lock().unwrap();
        while !d.lines[count..]
            .iter()
            .any(|l| values(l) == values("tweak diff"))
        {
            d = rig.fake.release.wait(d).unwrap();
        }
    }
    let pending_link = rig.app.link(&key);
    rig.app.unlink(pending_link);
    let pending_picture = rig.app.open_picture(id, want).unwrap();
    drop(pending_picture);
    let _start = rig.app.record_start(id, WindowId(3), &rig.registry);
    let _stop = rig.app.record_stop(id);
    rig.rescan();
    rig.app.allow_input(id);
    rig.app
        .inject(id, WindowId(3), AppInput::Text("pending".into()));
    rig.app.revoke_input(id);
    let tickets = (0..40)
        .map(|_| rig.app.design(id, None, DesignOp::Diff))
        .collect::<Vec<_>>();
    rig.events.extend(rig.app.poll());
    rig.fake.data.lock().unwrap().gated = false;
    rig.fake.release.notify_all();
    rig.design_result(waiting, true);
    let last = tickets.last().unwrap();
    rig.wait(|r| {
        r.events
            .iter()
            .any(|e| matches!(e,AppLinkEvent::Design{ticket,..} if ticket==last))
    });
    let delivered: Vec<_> = rig
        .events
        .iter()
        .filter_map(|e| match e {
            AppLinkEvent::Design { ticket, .. } if tickets.contains(ticket) => Some(*ticket),
            _ => None,
        })
        .collect();
    assert_eq!(
        delivered, tickets,
        "a full queue retains every command in order"
    );
    rig.ended(pending_link);
    let (next_pid, listener, next_socket) = rig.restart(id);
    let restarted: Vec<_> = rig
        .events
        .iter()
        .filter_map(|e| match e {
            AppLinkEvent::Restarted { ended, app } => Some((*ended, &app.key)),
            _ => None,
        })
        .collect();
    assert_eq!(
        restarted,
        [(id, &rig.app.apps()[0].key)],
        "only the lost live link gets a restart notice"
    );
    assert_eq!(rig.app.apps()[0].pid, next_pid);
    assert_eq!(
        rig.app.state(id).unwrap().phase,
        LinkPhase::Ended("app restarted. Link it again.".into())
    );
    assert!(!rig.app.state(id).unwrap().input_allowed);
    rig.rescan();
    assert_eq!(
        rig.events
            .iter()
            .filter(|e| matches!(e, AppLinkEvent::Restarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        rig.app.linked().count(),
        0,
        "linking the replacement requires another press"
    );
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    drop(listener);
    std::fs::remove_file(next_socket).unwrap();
    rig.app.shutdown();
    rig.wait(|r| r.workers.load(Ordering::Acquire) == 0);
}
