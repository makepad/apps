use super::*;
use makepad_ai_services::pictures::RgbaPicture;
use makepad_remote::{Channel, Client, Conn, Event};
use makepad_strict_json::Value;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::time::Duration;

const ACTIVITY: Duration = Duration::from_millis(200);
const WINDOWS: Duration = Duration::from_secs(1);
const SERVICE: Duration = Duration::from_millis(8);

struct Reply {
    body: String,
    value: Option<Value>,
    ok: bool,
}
struct Pipe<T> {
    ch: Channel,
    pending: Option<T>,
    bytes: Vec<u8>,
}
impl<T> Pipe<T> {
    fn new(conn: &Conn) -> Result<Self, String> {
        Ok(Self {
            ch: conn.open("cmd", "").map_err(|e| e.1)?,
            pending: None,
            bytes: Vec::new(),
        })
    }
    fn send(&mut self, kind: T, line: String) -> Result<(), String> {
        debug_assert!(self.pending.is_none());
        // Only one command is outstanding on this channel. Its end acknowledges consumption.
        self.ch.send(line.into_bytes()).map_err(|e| e.to_string())?;
        self.pending = Some(kind);
        Ok(())
    }
    fn reply(&mut self) -> Result<Option<(T, Reply)>, String> {
        if self.pending.is_none() {
            return Ok(None);
        }
        loop {
            match self.ch.try_recv() {
                Ok(Event::Data(part)) => {
                    self.bytes.extend(part);
                    // The service writes end as the last line and may split UTF-8 across messages.
                    if !self.bytes.ends_with(b"\n") {
                        continue;
                    }
                    let lines = self.bytes.strip_suffix(b"\n").unwrap();
                    let last = lines.rsplit(|b| *b == b'\n').next().unwrap_or_default();
                    if !last.starts_with(b"{\"end\":") {
                        continue;
                    }
                    let body = std::str::from_utf8(&lines[..lines.len() - last.len()])
                        .map_err(|_| "invalid command response")?
                        .trim_end()
                        .to_owned();
                    let value = makepad_strict_json::parse(body.as_bytes()).ok();
                    let ok = !body.lines().any(|l| {
                        makepad_strict_json::parse(l.as_bytes())
                            .is_ok_and(|v| v.get("err").is_some())
                    });
                    self.bytes.clear();
                    return Ok(Some((
                        self.pending.take().unwrap(),
                        Reply { body, value, ok },
                    )));
                }
                Ok(Event::Opened(_)) => {}
                Err(true) => return Ok(None),
                Ok(Event::Closed(_, why)) => return Err(why),
                Err(false) => return Err("command channel ended".into()),
            }
        }
    }
}
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}
fn windows(r: &Reply) -> Result<Vec<AppWindow>, String> {
    let rows = r
        .value
        .as_ref()
        .and_then(|v| v.get("w"))
        .and_then(Value::as_arr)
        .ok_or("invalid window status")?;
    rows.iter()
        .map(|v| {
            let read = || {
                let size = v.get("sz")?.as_arr()?;
                let (w, h) = (number(size.first()?)?, number(size.get(1)?)?);
                let dpi = number(v.get("dpi")?)?;
                if w < 0. || h < 0. || dpi <= 0. || !(w * dpi).is_finite() || !(h * dpi).is_finite()
                {
                    return None;
                }
                Some(AppWindow {
                    id: WindowId(v.get("i")?.as_u64()?.try_into().ok()?),
                    title: v.get("title").or_else(|| v.get("t")).and_then(Value::as_str).unwrap_or_default().to_owned(),
                    visible: w > 0. && h > 0. && v.get("visible").and_then(Value::as_bool) == Some(true),
                    size: (w, h),
                    dpi,
                })
            };
            read().ok_or_else(|| "invalid window geometry".into())
        })
        .collect()
}
enum Pending {
    Status,
    Design(Ticket),
    Input(Receipt),
    Release,
    Start(Ticket, PathBuf),
    Stop(Ticket),
}
// An input already handed to the app may answer after Take back. Its original
// Allow and activity counter travel with the reply, even across an immediate regrant.
struct Receipt {
    epoch: u64,
    seq: u64,
    window: WindowId,
    event: Option<AppInput>,
}

#[derive(Default)]
struct HeldInputs {
    epoch: u64,
    seq: u64,
    releases: Vec<(WindowId, AppInput)>,
}

impl HeldInputs {
    fn applied(&mut self, receipt: Receipt) {
        let Some(event) = receipt.event else { return };
        debug_assert!(self.releases.is_empty() || self.epoch == receipt.epoch);
        self.epoch = receipt.epoch;
        self.seq = receipt.seq;
        let window = receipt.window;
        use AppInput::*;
        if let PointerDown { x, y, .. } | PointerUp { x, y, .. } | PointerMove { x, y }
            | Scroll { x, y, .. } = event
        {
            for (id, up) in &mut self.releases {
                if let PointerUp { x: last_x, y: last_y, .. } = up {
                    if *id == window {
                        (*last_x, *last_y) = (x, y);
                    }
                }
            }
        }
        match event {
            PointerDown { button, .. } | PointerUp { button, .. } => {
                self.releases.retain(|(id, up)| *id != window || !matches!(up, PointerUp { button: b, .. } if *b == button));
                if let PointerDown { x, y, mods, .. } = event {
                    self.releases.push((window, PointerUp { x, y, button, mods }));
                }
            }
            KeyDown { key, .. } | KeyUp { key, .. } => {
                // Keyboard focus and held keys belong to the app. Repeats owe a single Up.
                self.releases.retain(|(_, up)| !matches!(up, KeyUp { key: k, .. } if *k == key));
                if let KeyDown { mods, .. } = event {
                    self.releases.push((window, KeyUp { key, mods }));
                }
            }
            _ => {}
        }
    }
}

struct StillJob {
    window: WindowId,
    scale: f64,
    target: StillTarget,
}

// Both the OPEN handshake and a one-shot grab can wait for the app. This worker belongs
// to the link and reuses its verified connection; neither wait stalls cap stop or sharing.
fn still_queue(conn: Conn, env: &AppLinkEnv) -> Queue<StillJob> {
    let (tx, rx) = mpsc::sync_channel::<StillJob>(1);
    let wake = env.signal.clone();
    (env.spawn)("app-link-stills", Box::new(move || {
        while let Ok(job) = rx.recv() {
            match job.target {
                StillTarget::File { path, size, reply } => {
                    let result = grab_still(&conn, job.window, job.scale).and_then(|frame| write_still(&frame, &path, size)).map(|(w, h)| (path, w, h));
                    if let Err(mpsc::TrySendError::Disconnected(Ok((unclaimed, ..)))) = reply.try_send(result) {
                        if let Err(e) = std::fs::remove_file(unclaimed) { makepad_widgets::log!("Removing unused poster: {e}"); }
                    }
                }
                StillTarget::Thumbnail(reply) => {
                    let image = grab_still(&conn, job.window, job.scale).and_then(|p|
                        makepad_widgets::ImageBuffer::new(&p.rgba, p.width as usize, p.height as usize)
                            .map(Arc::new).map_err(|e| format!("Invalid thumbnail: {e:?}")));
                    let _ = reply.try_send(image);
                }
                StillTarget::Issue(reply) => {
                    let result = still_bytes(&conn, job.window, 1., "png").and_then(|(png, size)| IssueStill::from_png(png, size));
                    let _ = reply.try_send(result);
                }
                StillTarget::Png(reply) => { let _ = reply.try_send(grab_still(&conn, job.window, job.scale).and_then(fit_still)); }
            }
            wake();
        }
    }));
    Queue::new(tx)
}

fn still_bytes(conn: &Conn, window: WindowId, scale: f64, codec: &str) -> Result<(Vec<u8>, [u32; 2]), String> {
    let args = format!("w={} codec={codec} scale={scale} once=1", window.0);
    let ch = conn.open("frames", &args).map_err(|e| e.1)?;
    let record = loop {
        match ch.recv() {
            Some(Event::Data(bytes)) => break makepad_remote::frames::decode_record(&bytes)?,
            Some(Event::Opened(_)) => continue,
            Some(Event::Closed(_, why)) if !why.is_empty() => return Err(why),
            _ => return Err("the app returned no picture".into()),
        }
    };
    let header = makepad_strict_json::parse(record.json.as_bytes()).map_err(|_| "invalid picture header")?;
    if header.get("window").and_then(Value::as_u64) != Some(window.0 as u64)
        || header.get("codec").and_then(Value::as_str) != Some(codec) {
        return Err("picture identifies the wrong window or format".into());
    }
    let dim = |key| header.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok())
        .filter(|&n| n > 0).ok_or("invalid picture dimensions");
    let (width, height) = (dim("w")?, dim("h")?);
    Ok((record.bytes, [width, height]))
}

fn grab_still(conn: &Conn, window: WindowId, scale: f64) -> Result<RgbaPicture, String> {
    let (rgba, [width, height]) = still_bytes(conn, window, scale, "raw")?;
    if (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4)) != Some(rgba.len()) {
        return Err("invalid picture pixels".into());
    }
    Ok(RgbaPicture { width, height, rgba })
}

impl IssueStill {
    pub(crate) fn from_report(mut file: std::fs::File, size: [u32; 2]) -> Result<Self, String> {
        use std::io::Read;
        let mut png = Vec::new();
        file.read_to_end(&mut png).map_err(|e| format!("Could not read the issue picture: {e}"))?;
        Self::from_png(png, size)
    }

    pub(crate) fn from_png(png: Vec<u8>, size: [u32; 2]) -> Result<Self, String> {
        let frame = makepad_ai_services::pictures::decode_png(&png)?;
        if size != [frame.width, frame.height] { return Err("The PNG dimensions differ from its frame.".into()); }
        Self::from_frame(frame, png)
    }

    pub(crate) fn from_picture(picture: &makepad_call::driver::codec::Picture) -> Result<Self, String> {
        let (width, height) = (picture.width, picture.height);
        let invalid = "Invalid shared picture.";
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 { return Err(invalid.into()); }
        let pixels = (width as usize).checked_mul(height as usize).ok_or(invalid)?;
        let len = pixels.checked_mul(4).ok_or(invalid)?;
        if pixels.checked_add(pixels / 2) != Some(picture.nv12.len()) { return Err(invalid.into()); }

        // DrawNv12 uses the default BT.709 limited-range matrix on these same tight planes.
        // Expand the converter's RGB triplets backwards in the reserved RGBA buffer.
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(len).map_err(|_| "Cannot allocate the issue picture.")?;
        makepad_video::nv12::nv12_to_rgb8(&picture.nv12, width, height, Default::default(), &mut rgba);
        rgba.resize(len, 0);
        for i in (0..pixels).rev() {
            rgba.copy_within(i * 3..i * 3 + 3, i * 4);
            rgba[i * 4 + 3] = 255;
        }
        let frame = RgbaPicture { width, height, rgba };
        let png = makepad_ai_services::pictures::encode_png(&frame)?;
        Self::from_frame(frame, png)
    }

    fn from_frame(frame: RgbaPicture, png: Vec<u8>) -> Result<Self, String> {
        use makepad_ai_services::pictures::{area_average, encode_png};
        let size = [frame.width, frame.height];
        let edge = frame.width.max(frame.height);
        let side = |n: u32| (f64::from(n) * POSTER_EDGE as f64 / f64::from(edge)).round().max(1.) as u32;
        let mut poster = if edge <= POSTER_EDGE as u32 { frame } else { area_average(&frame, side(frame.width), side(frame.height)) };
        let mut bytes = encode_png(&poster)?;
        // Noisy square windows can exceed the automatic-fetch threshold even at 640 px.
        // Reduce poster colour precision only as needed, preserving its size and opaque alpha.
        for bits in (1..8).rev() {
            if bytes.len() <= 1024 * 1024 { break }
            let mask = u8::MAX << (8 - bits);
            for v in &mut poster.rgba { if *v != 255 { *v &= mask; } }
            bytes = encode_png(&poster)?;
        }
        Ok(IssueStill { png: Arc::new(png), poster: Arc::new(bytes), size })
    }
}

fn fit_still(frame: RgbaPicture) -> PngReply {
    let longest = frame.width.max(frame.height);
    let mut images = makepad_ai_services::pictures::fit_pictures(&[(frame, [1, 1])],
        makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES, longest, |_, _, _| {})?;
    let (png, size) = images.pop().ok_or("The shared window returned no picture.")?;
    Ok(PngStill { png: png.into(), size })
}

#[test]
fn full_scale_still_fits_the_rgba_frame_as_a_png() {
    let frame = RgbaPicture { width: 2, height: 1, rgba: vec![7, 11, 13, 255, 17, 19, 23, 128] };
    let still = fit_still(frame.clone()).expect("the small frame fits the result budget");
    assert_eq!(still.size, [2, 1], "a frame that fits keeps its native dimensions");
    assert_eq!(makepad_ai_services::pictures::decode_png(&still.png).unwrap(), frame,
        "the in-memory PNG preserves both pixels, including alpha");
}

fn write_still(frame: &RgbaPicture, path: &Path, size: StillSize) -> Result<(u32, u32), String> {
    use std::io::Write;
    let (bytes, width, height) = still_jpeg(frame.width as usize, frame.height as usize, &frame.rgba, size)?;
    let mut file = std::fs::File::create_new(path).map_err(|e| format!("Creating poster: {e}"))?;
    if let Err(e) = file.write_all(&bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("Writing poster: {e}"));
    }
    Ok((width, height))
}

const POSTER_EDGE: usize = 640;
fn still_jpeg(w: usize, h: usize, rgba: &[u8], size: StillSize) -> Result<(Vec<u8>, u32, u32), String> {
    // Fit portrait windows as well as landscape. Recheck the actual frame in case the
    // window was resized between status and capture. Leave room below automatic fetch.
    let mut edge = if size == StillSize::Full { w.max(h) } else { w.max(h).min(POSTER_EDGE) };
    loop {
        let dimension = |n| ((n as f64 * edge as f64 / w.max(h) as f64).round() as usize).max(1);
        let (width, height) = (dimension(w), dimension(h));
        let mut scaled = Vec::new();
        let pixels = if (width, height) == (w, h) { rgba } else {
            scaled.reserve_exact(width * height * 4);
            for y in 0..height {
                for x in 0..width {
                    let i = (y * h / height * w + x * w / width) * 4;
                    scaled.extend_from_slice(&rgba[i..i + 4]);
                }
            }
            &scaled
        };
        let mut bytes = Vec::new();
        jpeg_encoder::Encoder::new(&mut bytes, 80)
            .encode(pixels, width.try_into().map_err(|_| "Window too wide for JPEG")?,
                height.try_into().map_err(|_| "Window too tall for JPEG")?, jpeg_encoder::ColorType::Rgba)
            .map_err(|e| format!("Encoding poster: {e}"))?;
        if size == StillSize::Full || bytes.len() <= 512 * 1024 { return Ok((bytes, width as u32, height as u32)); }
        edge /= 2;
    }
}

type Args = (AppLinkEnv, u32, LinkId, AppKey, Arc<Gate>);

pub(super) fn run((env, own, id, key, gate): Args, rx: Receiver<Command>, tx: SyncSender<Update>) {
    let registry = env
        .registry
        .clone()
        .unwrap_or_else(makepad_remote::discovery::registry_dir);
    let mut updates = Queue::new(tx);
    let mut app_gone = false;
    let result = (|| {
        let target = endpoint::resolve(&registry, &key, own)?;
        let client = Client::new(target, "app link");
        let conn = client.conn().map_err(|e| e.to_string())?;
        if conn.peer().serving_pid != key.pid {
            return Err("selected app process changed".into());
        }
        let mut work = Worker {
            id,
            env: &env,
            stills: None,
            gate: gate.clone(),
            conn: conn.clone(),
            cmd: Pipe::new(&conn)?,
            activity: Pipe::new(&conn)?,
            windows: Vec::new(),
            live: false,
            commands: VecDeque::new(),
            captures: HashMap::new(),
            input: VecDeque::new(),
            held: HeldInputs::default(),
            allow: None,
            next_activity: Instant::now(),
            next_status: Instant::now(),
            recording: None,
            closing: false,
        };
        let result = work.serve(&rx, &mut updates, &env.signal);
        app_gone = work.live && !conn.is_usable();
        if let Err(why) = &result {
            let ticket = match &work.cmd.pending {
                Some(Pending::Stop(t) | Pending::Start(t, _)) => Some(*t),
                _ => work.recording.as_ref().map(|r| r.0),
            };
            if let Some(t) = ticket {
                work.recorded(&mut updates, t, Err(why.clone()));
            }
        }
        result
    })();
    drop(rx); // Disconnect still requests before signalling a failed connection.
    gate.close();
    gate.releases_finished();
    let why = result.err().unwrap_or_else(|| "unlinked".into());
    updates.push(Update::Ended { why, app_gone });
    (env.signal)();
    // Preserve the last results through shutdown even when the UI hasn't polled yet.
    for event in updates.pending {
        if updates.tx.send(event).is_err() {
            break;
        }
        (env.signal)();
    }
}

struct Worker<'a> {
    env: &'a AppLinkEnv,
    stills: Option<Queue<StillJob>>,
    id: LinkId,
    gate: Arc<Gate>,
    conn: Conn,
    cmd: Pipe<Pending>,
    activity: Pipe<(u64, Instant)>,
    windows: Vec<AppWindow>,
    live: bool,
    commands: VecDeque<Command>,
    captures: HashMap<WindowId, capture::Capture>,
    input: VecDeque<(Receipt, String)>,
    held: HeldInputs,
    allow: Option<(u64, Instant, Option<u64>)>,
    next_activity: Instant,
    next_status: Instant,
    recording: Option<(Ticket, PathBuf)>,
    closing: bool,
}
impl Worker<'_> {
    fn serve(
        &mut self,
        rx: &Receiver<Command>,
        out: &mut Queue<Update>,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        loop {
            if !self.conn.is_usable() {
                return Err(self.conn.close_reason());
            }
            let mut dirty = false;
            loop {
                match rx.try_recv() {
                    Ok(c) => {
                        self.accept(c);
                        dirty = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.closing = true;
                        break;
                    }
                }
            }
            self.closing |= self.gate.closing.load(Ordering::Acquire);
            if self.closing {
                self.captures.clear();
                self.commands.clear();
                self.input.clear();
                // Finish any in-flight input on its original pipe: its reply determines
                // whether we owe the app a release. An unrelated command need not delay it.
                if !matches!(self.cmd.pending, Some(Pending::Input(_) | Pending::Release | Pending::Start(..) | Pending::Stop(_))) {
                    if self.held.releases.is_empty() && self.recording.is_none() {
                        return Ok(());
                    }
                    if self.cmd.pending.is_some() {
                        self.cmd = Pipe::new(&self.conn)?;
                    }
                    if !self.release_next()? {
                        let Some((ticket, _)) = self.recording else { return Ok(()) };
                        self.cmd.send(Pending::Stop(ticket), "cap stop".into())?;
                    }
                }
            }
            if let Some((op, r)) = self.cmd.reply()? {
                self.answer(op, r, out)?;
                dirty = true;
            }
            if !self.closing {
                dirty |= self.watch(out)?;
                self.dispatch(out)?;
                if let Some(stills) = &mut self.stills { stills.flush(); }
                self.captures.retain(|id, cap| {
                    let Some(window) = self.windows.iter().find(|w| w.id == *id) else {
                        return false;
                    };
                    if let Err(why) = cap.step(window) {
                        makepad_widgets::log!("App picture: window {}: {why}", id.0);
                        for slot in &cap.slots { slot.close_with(&why); }
                        return false;
                    }
                    !cap.slots.is_empty()
                });
            }
            out.flush();
            if std::mem::take(&mut out.published) || dirty {
                wake();
            }
            let wait = if self.cmd.pending.is_some()
                || self.activity.pending.is_some()
                || self.stills.as_ref().is_some_and(|q| !q.pending.is_empty())
                || !self.captures.is_empty()
                || !out.pending.is_empty()
                || !self.commands.is_empty()
                || !self.input.is_empty()
            {
                SERVICE
            } else if self.allow.is_some() {
                ACTIVITY.min(self.next_activity.saturating_duration_since(Instant::now()))
            } else {
                WINDOWS.min(self.next_status.saturating_duration_since(Instant::now()))
            };
            match rx.recv_timeout(wait) {
                Ok(c) => {
                    self.accept(c);
                    wake();
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.closing = true;
                    if self.cmd.pending.is_some() {
                        std::thread::sleep(SERVICE);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
    fn accept(&mut self, c: Command) {
        match c {
            Command::Allow(epoch, at) => {
                if epoch == self.gate.epoch() {
                    self.allow = Some((epoch, at, None));
                    self.next_activity = Instant::now();
                }
            }
            Command::Wake => {}
            other => self.commands.push_back(other),
        }
    }
    fn local(&mut self, epoch: u64, out: &mut Queue<Update>) {
        if self
            .gate
            .epoch
            .compare_exchange(epoch, epoch + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.allow = None;
            out.push(Update::LocalInput(epoch + 1));
        }
    }
    fn watch(&mut self, out: &mut Queue<Update>) -> Result<bool, String> {
        let mut changed = false;
        if let Some(((epoch, at), r)) = self.activity.reply()? {
            if self.gate.epoch() == epoch {
                let value = r.value.as_ref().ok_or("invalid activity response")?;
                let seq = value
                    .get("user_seq")
                    .and_then(Value::as_u64)
                    .ok_or("missing activity counter")?;
                let old = self.allow.and_then(|(_, _, seq)| seq);
                // Cover the interval between the local press and the first asynchronous snapshot.
                // idle_ms rounds down, so a boundary ambiguity yields to the person.
                let since_press = value
                    .get("idle_ms")
                    .and_then(Value::as_u64)
                    .is_some_and(|ms| u128::from(ms) <= at.elapsed().as_millis());
                if old.map_or(since_press, |previous| previous != seq) {
                    self.local(epoch, out);
                } else {
                    self.allow = Some((epoch, at, Some(seq)));
                }
                changed = true;
            }
        }
        if let Some((epoch, at, _)) = self.allow {
            if self.gate.epoch() != epoch {
                self.allow = None;
            } else if self.activity.pending.is_none() && Instant::now() >= self.next_activity {
                self.activity.send((epoch, at), "activity".into())?;
                self.next_activity = Instant::now() + ACTIVITY;
            }
        }
        Ok(changed)
    }
    // Serialized with viewer input on the same command pipe. The old user_seq
    // prevents releasing a person's own press if native input already cancelled ours.
    fn release_next(&mut self) -> Result<bool, String> {
        if !self.closing && self.held.epoch == self.gate.epoch() {
            return Ok(false);
        }
        while let Some((id, up)) = self.held.releases.pop() {
            if self.windows.iter().any(|w| w.id == id) {
                let line = up.command_lines(id, self.held.seq)?.pop().ok_or("missing release command")?;
                self.cmd.send(Pending::Release, line)?;
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn dispatch(&mut self, out: &mut Queue<Update>) -> Result<(), String> {
        if self.cmd.pending.is_some() || self.release_next()? {
            return Ok(());
        }
        if !self.live || Instant::now() >= self.next_status {
            self.cmd.send(Pending::Status, "status".into())?;
            self.next_status = Instant::now() + WINDOWS;
            return Ok(());
        }
        loop {
            if let Some((receipt, line)) = self.input.pop_front() {
                if self.gate.epoch() != receipt.epoch {
                    self.gate.refuse("Allow ended");
                    continue;
                }
                if matches!(receipt.event, Some(AppInput::PointerDown { .. } | AppInput::KeyDown { .. }))
                    && !self.gate.hand_over_press()
                {
                    self.gate.refuse("input closed");
                    continue;
                }
                return self.cmd.send(Pending::Input(receipt), line);
            }
            let Some(c) = self.commands.pop_front() else {
                return Ok(());
            };
            match c {
                Command::Allow(..) | Command::Wake => unreachable!(),
                Command::Input(epoch, w, input) => {
                    if self.gate.epoch() != epoch {
                        self.gate.refuse("Allow ended");
                        continue;
                    }
                    let Some((_, _, Some(seq))) = self.allow.filter(|(e, _, _)| *e == epoch) else {
                        self.commands.push_front(Command::Input(epoch, w, input));
                        return Ok(());
                    };
                    let Some(window) = self.windows.iter().find(|v| v.id == w) else {
                        self.gate.refuse("window closed");
                        continue;
                    };
                    match input.lines(window, seq) {
                        Ok(lines) => {
                            let mut event = (!matches!(input, AppInput::Text(_))).then_some(input);
                            self.input.extend(lines.into_iter().map(|line| {
                                (Receipt { epoch, seq, window: w, event: event.take() }, line)
                            }));
                        }
                        Err(why) => self.gate.refuse(why),
                    }
                }
                Command::Picture(slot) => {
                    let Some(want) = slot.demand() else { continue };
                    let Some(w) = self.windows.iter().find(|w| w.id == want.window) else {
                        slot.close();
                        continue;
                    };
                    if let std::collections::hash_map::Entry::Vacant(e) = self.captures.entry(w.id)
                    {
                        e.insert(capture::Capture::new(self.conn.clone()));
                    }
                    self.captures.get_mut(&w.id).unwrap().add(slot);
                }
                Command::Design(ticket, window, op) => match op.line(window) {
                    Ok(line) => return self.cmd.send(Pending::Design(ticket), line),
                    Err(why) => out.push(Update::Event(AppLinkEvent::Design {
                        id: self.id,
                        ticket,
                        ok: false,
                        json: error_json(&why),
                    })),
                },
                Command::Still(id, target) => {
                    if let Some(w) = self.windows.iter().find(|w| w.id == id) {
                        let scale = match target {
                            StillTarget::File { size: StillSize::Poster, .. } => (POSTER_EDGE as f64 / (w.size.0.max(w.size.1) * w.dpi)).min(1.),
                            StillTarget::Thumbnail(_) => (240. / (w.size.0 * w.dpi)).min(1.),
                            _ => 1.,
                        };
                        let queue = self.stills.get_or_insert_with(|| still_queue(self.conn.clone(), self.env));
                        queue.push(StillJob { window: id, scale, target });
                    } else {
                        match target {
                            StillTarget::File { reply, .. } => { let _ = reply.try_send(Err("window closed".into())); }
                            StillTarget::Png(reply) => { let _ = reply.try_send(Err("window closed".into())); }
                            StillTarget::Issue(reply) => { let _ = reply.try_send(Err("window closed".into())); }
                            StillTarget::Thumbnail(reply) => { let _ = reply.try_send(Err("window closed".into())); }
                        }
                        (self.env.signal)();
                    }
                }
                Command::Start(t, w, dir) => {
                    let result = (|| {
                        if self.recording.is_some() {
                            return Err("already recording".into());
                        }
                        if !self.windows.iter().any(|win| win.id == w) {
                            return Err("window closed".into());
                        }
                        let path =
                            std::fs::canonicalize(dir)
                                .map_err(|e| e.to_string())?
                                .join(format!(
                                    "clip-{}-{}-{}.mp4",
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_micros(),
                                    std::process::id(),
                                    t.0
                                ));
                        let text = path.to_str().ok_or("clip path is not UTF-8")?;
                        let line = commands::line(
                            "cap start",
                            &[("path".into(), text.into()), ("w".into(), w.0.to_string())],
                        );
                        Ok((path, line))
                    })();
                    match result {
                        Ok((path, line)) => return self.cmd.send(Pending::Start(t, path), line),
                        Err(e) => self.recorded(out, t, Err(e)),
                    }
                }
                Command::Stop(t) => {
                    if self.recording.is_none() {
                        self.recorded(out, t, Err("no recording".into()));
                        continue;
                    }
                    return self.cmd.send(Pending::Stop(t), "cap stop".into());
                }
            }
        }
    }
    fn recorded(&self, out: &mut Queue<Update>, ticket: Ticket, result: Result<Clip, String>) {
        out.push(Update::Event(AppLinkEvent::Recorded {
            id: self.id,
            ticket,
            result,
        }));
    }
    fn answer(&mut self, op: Pending, r: Reply, out: &mut Queue<Update>) -> Result<(), String> {
        match op {
            Pending::Status => {
                let new = windows(&r)?;
                if !self.live || self.windows != new {
                    self.held.releases.retain(|(id, _)| new.iter().any(|w| w.id == *id));
                    self.windows = new;
                    self.live = true;
                    out.push(Update::Windows(self.windows.clone()));
                }
            }
            Pending::Input(receipt) => {
                let epoch = receipt.epoch;
                let err = r
                    .value
                    .as_ref()
                    .and_then(|v| v.get("err"))
                    .and_then(Value::as_str);
                let applied = r
                    .value
                    .as_ref()
                    .and_then(|v| v.get("applied"))
                    .and_then(Value::as_bool);
                // A press is a person's deliberate act, a few a second at most: each one's answer is
                // logged, so a press that changed nothing can be told from one the app never took.
                let press = match receipt.event {
                    Some(AppInput::PointerDown { x, y, .. }) => Some(("press", x, y)),
                    Some(AppInput::PointerUp { x, y, .. }) => Some(("release", x, y)),
                    _ => None,
                };
                if r.ok || applied == Some(true) {
                    if let Some((what, x, y)) = press { makepad_widgets::log!("App link: {what} at {x:.0},{y:.0} applied"); }
                    self.held.applied(receipt);
                    self.gate.delivered.fetch_add(1, Ordering::Relaxed);
                    self.gate.refusal_run.store(false, Ordering::Relaxed);
                } else {
                    if let Some((what, x, y)) = press { makepad_widgets::log!("App link: {what} at {x:.0},{y:.0} refused: {}", r.body); }
                    self.gate.refuse(err.unwrap_or("input refused by app"));
                }
                if matches!(err, Some("user_intervened" | "user_interacting")) {
                    self.local(epoch, out);
                }
            }
            Pending::Release => {
                if !r.ok {
                    // Local input cancels injected gestures in the app before it reaches widgets.
                    // A closed window likewise has no held input left to release.
                    makepad_widgets::log!("App link: release answered {}", r.body);
                }
            }
            Pending::Design(ticket) => out.push(Update::Event(AppLinkEvent::Design {
                id: self.id,
                ticket,
                ok: r.ok,
                json: r.body,
            })),
            Pending::Start(t, path) => {
                if r.ok {
                    self.recording = Some((t, path));
                    out.push(Update::Recording(Some(t)));
                } else {
                    self.recorded(out, t, Err(r.body));
                }
            }
            Pending::Stop(t) => {
                let (_, path) = self.recording.take().ok_or("no recording")?;
                let clip = (|| {
                    let v = r.value.as_ref()?;
                    let frames = v.get("frames")?.as_u64()?;
                    let sz = v.get("sz")?.as_arr()?;
                    Some((
                        frames,
                        sz.first()?.as_u64()?.try_into().ok()?,
                        sz.get(1)?.as_u64()?.try_into().ok()?,
                    ))
                })()
                .ok_or(r.body)
                .and_then(|(frames, w, h)| {
                    Ok(Clip {
                        secs: recording::duration(&path)?,
                        path,
                        frames,
                        w,
                        h,
                    })
                });
                self.recorded(out, t, clip);
                out.push(Update::Recording(None));
            }
        }
        // Only one command is in flight. Its reply accounts for an applied Down,
        // a refused Down, an ordinary Up, or the last release owed by a revoked grant.
        if self.held.releases.is_empty() && self.gate.owes_release() {
            self.gate.releases_finished();
        }
        Ok(())
    }
}
