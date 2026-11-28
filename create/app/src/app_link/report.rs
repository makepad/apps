//! Receives an app's Alt+F10 report on its local endpoint. Only checked, open files cross
//! from the service thread to the draft, so workers never reopen a reporter-supplied path.
use crate::model::issues::IssueProject;
use makepad_remote::program::ServiceSink;
use makepad_strict_json::Value;
use makepad_widgets::makepad_platform::{home, SignalToUI};
use std::{
    collections::BTreeMap, ffi::c_void, fs::{self, File}, path::Path,
    sync::{atomic::{AtomicUsize, Ordering}, mpsc::{self, Receiver, Sender}, Mutex},
};

pub struct Report {
    pub project: IssueProject,
    pub label: String,
    pub size: [u32; 2],
    pub still: File,
    pub done: Receiver<Option<Recording>>,
}

pub struct Recording {
    pub file: File,
    pub duration_ms: i64,
}

static DESTINATION: Mutex<Option<(IssueProject, Sender<Report>)>> = Mutex::new(None);
static CHANNELS: Mutex<BTreeMap<usize, Reporter>> = Mutex::new(BTreeMap::new());
static NEXT: AtomicUsize = AtomicUsize::new(1);

pub struct Reports {
    to: Option<Sender<Report>>,
    incoming: Receiver<Report>,
    drained: bool,
    project: Option<IssueProject>,
}

impl Reports {
    pub fn register(_cx: &makepad_widgets::Cx) -> Self {
        let (to, incoming) = mpsc::channel();
        #[cfg(not(target_arch = "wasm32"))]
        {
            use makepad_widgets::makepad_platform::{remote, thread::Class};
            // A running endpoint republishes its caps in the registry during registration.
            match _cx.task_pool().submit_named(Class::Background, "Town report service", || {
                remote::register_service("report", 1, open, close, Some(data));
            }) {
                Ok(job) => job.detach(),
                Err(e) => makepad_widgets::log!("town: registering the report service: {e}"),
            }
        }
        Self { to: Some(to), incoming, project: None, drained: false }
    }

    pub fn take(&mut self) -> Option<Report> {
        match self.incoming.try_recv() {
            Ok(report) => Some(report),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => { self.drained = true; None }
        }
    }

    /// After disabling new takers, drain every report whose service thread may already have
    /// answered taken. The last of those threads drops its sender and wakes the UI.
    pub fn stop(&mut self) {
        self.destination(None);
        self.to = None;
    }

    pub fn draining(&self) -> bool { self.to.is_none() && !self.drained }

    pub fn destination(&mut self, project: Option<IssueProject>) {
        if self.project != project {
            self.project = project;
            *DESTINATION.lock().unwrap_or_else(|e| e.into_inner()) = project.zip(self.to.clone());
        }
    }
}

fn checked_file(name: &str, extension: &str) -> Result<File, &'static str> {
    let path = Path::new(name);
    if !path.is_absolute() || !path.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.contains("issue"))
        || path.extension().and_then(|s| s.to_str()) != Some(extension) {
        return Err("invalid issue capture name");
    }
    let before = fs::symlink_metadata(path).map_err(|_| "missing issue capture")?;
    if !before.is_file() { return Err("not a regular issue capture"); }
    let canonical = path.canonicalize().map_err(|_| "missing issue capture")?;
    let folder = canonical.parent().ok_or("invalid issue capture folder")?;
    let checkout = folder.ends_with("local/screencap") && folder.parent().and_then(Path::parent)
        .is_some_and(|root| root.join("platform/Cargo.toml").is_file() && root.join("widgets/Cargo.toml").is_file());
    if !checkout && home::app_data_dir("screencap").and_then(|p| p.canonicalize().ok()).as_deref() != Some(folder) {
        return Err("outside an issue capture folder");
    }
    if canonical.file_name() != path.file_name() { return Err("issue capture changed"); }
    let mut options = File::options();
    options.read(true);
    // Do not follow a replacement link or block opening a replacement FIFO.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let nofollow = if cfg!(any(target_arch = "arm", target_arch = "aarch64")) { 0x8000 } else { 0x20000 };
        options.custom_flags(nofollow | 0x800); // O_NOFOLLOW | O_NONBLOCK
    }
    #[cfg(any(target_vendor = "apple", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
    { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(0x100 | 0x4); } // O_NOFOLLOW | O_NONBLOCK
    #[cfg(windows)]
    { use std::os::windows::fs::OpenOptionsExt; options.custom_flags(0x00200000); } // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options.open(&canonical).map_err(|_| "cannot open issue capture")?;
    let opened = file.metadata().map_err(|_| "cannot inspect issue capture")?;
    if !opened.is_file() || path.canonicalize().ok().as_ref() != Some(&canonical) { return Err("issue capture changed"); }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if opened.file_attributes() & 0x400 != 0 { return Err("not a regular issue capture"); }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = fs::symlink_metadata(path).map_err(|_| "issue capture changed")?;
        let identity = (opened.dev(), opened.ino());
        if identity != (before.dev(), before.ino()) || identity != (after.dev(), after.ino()) { return Err("issue capture changed"); }
    }
    Ok(file)
}

struct Reporter {
    sink: ServiceSink,
    ctx: usize,
    finish: Option<Sender<Option<Recording>>>,
}

impl Reporter {
    fn reply(&self, kind: u32, msg: &[u8]) -> bool {
        (self.sink)(self.ctx as *mut c_void, kind, msg.as_ptr(), msg.len())
    }

    /// True ends the channel. A dropped finish sender publishes the still on disconnect.
    fn line(&mut self, bytes: &[u8]) -> Result<bool, &'static str> {
        let Value::Obj(fields) = makepad_strict_json::parse_depth(bytes, 3)? else { return Err("invalid report line") };
        let [(kind, fields)] = fields.as_slice() else { return Err("expected one report line") };
        let string = |k| fields.get(k).and_then(Value::as_str).ok_or("missing report text");
        let number = |k| match fields.get(k) {
            Some(Value::Int(i)) => Ok(*i as f64),
            Some(Value::F64(f)) if f.is_finite() => Ok(*f),
            _ => Err("invalid report number"),
        };
        match (kind.as_str(), self.finish.as_ref()) {
            ("report", None) => {
                let pixels = |k| fields.get(k).and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok())
                    .filter(|i| *i > 0).ok_or("invalid report dimensions");
                let size = [pixels("w")?, pixels("h")?];
                let dpi = number("dpi")?;
                if dpi <= 0.0 { return Err("invalid report dpi"); }
                let label = format!("{} · {} · {} × {} pt · {dpi} dpi", string("app")?, string("title")?,
                    size[0] as f64 / dpi, size[1] as f64 / dpi).replace(['\r','\n'], " ");
                let still = checked_file(string("still")?, "png")?;
                let (project, to) = DESTINATION.lock().unwrap_or_else(|e| e.into_inner()).clone()
                    .ok_or("no writable project for an issue")?;
                let (finish, done) = mpsc::channel();
                if !self.reply(0, b"{\"taken\":true}\n") { return Ok(true); }
                to.send(Report { project, label, size, still, done }).map_err(|_| "issue draft unavailable")?;
                drop(to);
                self.finish = Some(finish);
                SignalToUI::set_ui_signal();
                Ok(false)
            }
            ("report_done", Some(finish)) => {
                let seconds = number("seconds")?;
                if seconds < 0.0 || seconds * 1000.0 >= i64::MAX as f64 { return Err("invalid recording duration"); }
                let file = checked_file(string("video")?, "mp4")?;
                let recording = (seconds >= 1.0).then(|| Recording { file, duration_ms: (seconds * 1000.0).round() as i64 });
                let _ = finish.send(recording);
                Ok(true)
            }
            _ => Err("unexpected report line"),
        }
    }

    fn end(self, reason: Option<&str>) {
        if let Some(reason) = reason {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "town: report refused: {reason}");
            self.reply(2, reason.as_bytes());
        } else {
            self.reply(1, &[]);
        }
        drop(self);
        SignalToUI::set_ui_signal();
    }
}

extern "C" fn open(_: *const u8, len: usize, sink: ServiceSink, ctx: *mut c_void) -> usize {
    if len == 0 {
        // Zero is the ABI's refused handle.
        let id = NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)).ok();
        let Some(id) = id else {
            let why = b"report handles exhausted";
            sink(ctx, 2, why.as_ptr(), why.len());
            return 0;
        };
        CHANNELS.lock().unwrap_or_else(|e| e.into_inner()).insert(id, Reporter { sink, ctx: ctx as usize, finish: None });
        id
    } else {
        let why = b"report needs empty arguments";
        sink(ctx, 2, why.as_ptr(), why.len());
        0
    }
}

extern "C" fn data(id: usize, ptr: *const u8, len: usize) -> bool {
    // The endpoint serializes DATA and close for this handle. Other reporters can validate
    // their own paths while this one runs.
    let channel = CHANNELS.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if let Some(mut channel) = channel {
        let bytes = if ptr.is_null() || len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len) } };
        match channel.line(bytes) {
            Ok(false) => { CHANNELS.lock().unwrap_or_else(|e| e.into_inner()).insert(id, channel); }
            Ok(true) => channel.end(None),
            Err(reason) => channel.end(Some(reason)),
        }
    }
    true
}

extern "C" fn close(id: usize) {
    let channel = CHANNELS.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    if let Some(channel) = channel { channel.end(None); }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{app_link::AppLinks, lists::{Page, ViewData}, model::{tests::ShareHomes, Action, Model, Origin},
        view::{LocalClock, Workspace}};
    use makepad_remote::{carrier::Carrier, conn::{CLOSE_NORMAL, CLOSE_REFUSED}, frame::FrameReader, Endpoint, Event as RemoteEvent};
    use makepad_town_core::{blob, entity::field, event::Value as Field};
    use makepad_widgets::*;
    use std::os::unix::{fs::symlink, net::UnixStream};

    #[test]
    fn reported_issue_validates_paths_and_files_one_record() {
        fn carrier(socket: UnixStream) -> Carrier {
            Carrier { reader: FrameReader::new(Box::new(socket.try_clone().unwrap())), writer: Box::new(socket.try_clone().unwrap()),
                shutdown: Box::new(move || { let _ = socket.shutdown(std::net::Shutdown::Both); }) }
        }
        let homes = ShareHomes::new("issue-taker");
        let mut model = Model::open(&homes.0.join("Ana")).unwrap().0;
        let project = model.act(Action::AddProject { title: "Design".into() }, Origin::Ui).unwrap();
        let clock = LocalClock::default();
        let mut view = ViewData { workspace: Workspace::read(&model, false, &clock), ..Default::default() };
        view.tree.set(model.towns(), model.town_tree());
        let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
        let page = cx.with_vm(|vm| {
            let v = script_eval!(vm, { use mod.widgets.* IssuesPage{} });
            WidgetRef::script_from_value(vm, v)
        });
        let (signal, wake) = mpsc::channel();
        model.start_store(cx.task_pool(), move || { let _ = signal.send(()); }).unwrap();
        let (to, incoming) = mpsc::channel();
        let mut reports = Reports { to: Some(to), incoming, project: None, drained: false };
        reports.destination(view.issue_destination(&model));
        let endpoint = Endpoint::new("Town");
        endpoint.register_program("report", 1, open, close, Some(data));
        let (a, b) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let c = carrier(a);
            endpoint.serve(c.reader, c.writer, c.shutdown).unwrap()
        });
        let client = Endpoint::new("Ben").connect(carrier(b), "reporter").unwrap();
        let server = server.join().unwrap();

        let shots = homes.0.join("local/screencap");
        fs::create_dir_all(&shots).unwrap();
        for dir in ["platform", "widgets"] {
            fs::create_dir(homes.0.join(dir)).unwrap();
            fs::write(homes.0.join(dir).join("Cargo.toml"), []).unwrap();
        }
        let still = shots.join("Ben-issue.png");
        let movie = shots.join("Ben-issue.mp4");
        let rgba = makepad_ai_services::pictures::RgbaPicture { width: 4, height: 2, rgba: [8, 12, 16, 255].repeat(8) };
        let png = makepad_ai_services::pictures::encode_png(&rgba).unwrap();
        let mp4 = b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2";
        fs::write(&still, &png).unwrap();
        fs::write(&movie, mp4).unwrap();
        let start = |p: &Path| format!(r#"{{"report":{{"app":"Ben","title":"Ana\nCanvas","w":4,"h":2,"dpi":2,"still":{}}}}}"#,
            makepad_strict_json::s(p.to_string_lossy()).to_json()) + "\n";
        let send = |bytes: Vec<u8>| {
            let ch = client.open("report", "").unwrap();
            ch.send(bytes).unwrap();
            ch
        };
        let outside = homes.0.join("outside-issue.png");
        let link = shots.join("link-issue.png");
        let wrong = shots.join("Ben.png");
        fs::write(&outside, &png).unwrap();
        symlink(&still, &link).unwrap();
        fs::write(&wrong, &png).unwrap();
        let before = model.open_event_count();
        for (why, bytes) in [("outside", start(&outside).into_bytes()), ("symlink", start(&link).into_bytes()),
            ("wrong name", start(&wrong).into_bytes()), ("malformed JSON", vec![0xff])] {
            assert!(matches!(send(bytes).recv(), Some(RemoteEvent::Closed(CLOSE_REFUSED, _))), "{why} closes with a refusal before acknowledgement");
            assert!(matches!(reports.incoming.try_recv(), Err(mpsc::TryRecvError::Empty)), "{why} hands no file to the copy path");
        }
        assert_eq!(model.open_event_count(), before, "refusals publish nothing");

        let invalid_movie = homes.0.join("outside-issue.mp4");
        fs::write(&invalid_movie, b"private").unwrap();
        let mut links = AppLinks::default();
        for (i, (note, seconds)) in [("Brief", Some(0.99)), ("Recording", Some(1.25)), ("Disconnected", None), ("Invalid movie", Some(1.0))].into_iter().enumerate() {
            let ch = send(start(&still).into_bytes());
            assert!(matches!(ch.recv(), Some(RemoteEvent::Data(d)) if d == b"{\"taken\":true}\n"), "taken arrives while the UI and copier are idle");
            let report = reports.incoming.recv().unwrap();
            view.report_issue(&mut cx, &mut model, report);
            assert_eq!((view.page, view.workspace.current), (Page::Issues, Some(project)), "the draft opens in the current project");
            let input = page.text_input(&cx, ids!(issue_input));
            let actions = cx.capture_actions(|cx| cx.widget_action(input.widget_uid(), TextInputAction::Changed(note.into())));
            view.issues.actions(&mut cx, &page, &actions, &mut model);
            while !view.issues.link_events(&mut links, &[], true, &mut model) { std::thread::yield_now(); }
            let receive_media = |view: &mut ViewData, model: &mut Model| loop {
                let receipts = model.drain_store().issues;
                if !receipts.is_empty() {
                    for receipt in receipts {
                        assert!(receipt.1.is_ok(), "issue media reaches the store: {:?}", receipt.1);
                        view.issues.stored(model, receipt);
                    }
                    break
                }
                wake.recv().expect("store wakes after importing issue media");
            };
            receive_media(&mut view, &mut model);
            assert_eq!(model.project_issues(project).len(), i, "the stored picture waits for report_done or disconnect");
            let invalid = note == "Invalid movie";
            if let Some(seconds) = seconds {
                let video = if invalid { &invalid_movie } else { &movie };
                ch.send(format!("{{\"report_done\":{{\"seconds\":{seconds},\"video\":{}}}}}\n",
                    makepad_strict_json::s(video.to_string_lossy()).to_json()).into_bytes()).unwrap();
                let code = if invalid { CLOSE_REFUSED } else { CLOSE_NORMAL };
                assert!(matches!(ch.recv(), Some(RemoteEvent::Closed(c, _)) if c == code), "report_done closes with the validation result");
            } else {
                ch.close(CLOSE_NORMAL, "");
            }
            while !view.issues.link_events(&mut links, &[], true, &mut model) { std::thread::yield_now(); }
            if !invalid && seconds.is_some_and(|s| s >= 1.0) { receive_media(&mut view, &mut model); }
            let issues = model.project_issues(project);
            assert_eq!((issues.len(), model.open_event_count()), (i + 1, before + i + 1), "each report appends just one entity");
            let e = issues[0];
            assert_eq!((e.text(field::TEXT), e.text(field::LABEL)), (note, "Ben · Ana Canvas · 2 × 1 pt · 2 dpi"), "one record carries the typed note and window label");
            let bytes = |key| {
                let Field::Blob { size, root } = e.value(key).expect("signed media field") else { panic!("expected a blob reference") };
                let dir = model.home.blobs_dir().join(makepad_town_core::hex(&model.open_id()));
                fs::read(blob::data_path(&dir, &blob::id(*size, root))).expect("signed blob is present")
            };
            assert_eq!(bytes(field::BLOB), png, "the still in the store is the app's PNG");
            assert_eq!(makepad_ai_services::pictures::decode_png(&bytes(field::POSTER)).unwrap().rgba, rgba.rgba, "the poster comes from the still");
            if seconds == Some(1.25) {
                assert_eq!(bytes(field::VIDEO), mp4, "the long recording is copied verbatim");
                assert_eq!((e.text(field::MEDIA), e.value(field::DURATION)), ("video/mp4", Some(&Field::Int(1250))), "clip media type and millisecond duration");
            } else {
                assert!(e.value(field::VIDEO).is_none() && e.value(field::DURATION).is_none() && e.value(field::MEDIA).is_none(), "short or interrupted recordings keep only the picture");
            }
        }
        reports.destination(None);
        client.close("done");
        server.close("done");
        model.stop_store();
        while !model.store_stopped() {
            model.drain_store();
            if !model.store_stopped() { wake.recv().unwrap(); }
        }
    }
}
