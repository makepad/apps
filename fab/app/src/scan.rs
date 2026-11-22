//! The Scan workspace's scan. Start asks the linked headset for its cameras (the wearer allows
//! them there) and keeps what comes back in a folder of its own, open to this user alone; each
//! record goes to one worker that writes it, places its keyframe and fuses the headset's depth
//! into the surface the view shows, so the scan is seen from its first keyframe. A kept scan is
//! read back through the same worker, keyframe by keyframe. The pictures stay in the folder: the
//! log holds numbers.
//!
//! Each keyframe's two pictures are then matched on this machine's GPU by a second worker, and
//! the dense points they give stand in the view for the base under them: the detail follows the
//! base by a keyframe or two as the wearer walks.
//!
//! Who waits on what: the headset's stream thread hands each record to the worker's channel and
//! goes on; the worker sends what changed back, and each keyframe with the base as it then
//! stands on to the detail worker; the app takes what both send on its timer (ten a second while
//! a scan is under way) without waiting. The detail's GPU work goes through the app's own GPU
//! device, which runs it between the app's frames.

use fab::api::*;
use makepad_compute_program::gpu::GpuLink;
use makepad_compute_program_gpu::GpuDevice;
use makepad_desk_link::xr_capture::{capture_state, CaptureAsk, CaptureCamera, CaptureKeyframe, CaptureRecord};
use makepad_ai_geometry::views::GrayView;
use makepad_fab_scan::detail::{Local, LocalWhile};
use makepad_fab_scan::session::{camera_to_world, looked_at};
use makepad_fab_scan::live::{LiveKeyframe, LiveScan, Taken, VOXEL};
use makepad_fab_scan::refine::{refine_looked, rematch, Hurried, Kept, Refined, WINDOW};
use makepad_widgets::*;
use makepad_xr_stream::XrServer;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

/// A keyframe per this much turn or walk, without the preview stream.
const ASK: CaptureAsk = CaptureAsk { step_deg: 8.0, step_m: 0.04, preview_hz: 0 };
/// The most points the view is given: past it the surface's cells widen.
const SURFACE_POINTS: usize = 150_000;
/// The detail's points, metres apart: the pictures' own spacing (about 1.5 mm at 40 cm), drawn as
/// splats of that size.
const DETAIL_CELL: f32 = 0.0015;
/// A kept scan's keyframes come back this far apart, so its growth is seen.
const SHOWN_EVERY: Duration = Duration::from_millis(350);
/// A record at least this large is a keyframe (a kept scan's count, before it is read).
const KEYFRAME_BYTES: u64 = 64 << 10;

/// The Scan workspace's tabs: what the scan's state draws.
pub const TABS: &[&[LiveId]] = &[ids!(tab_scan), ids!(tab_scan_view)];

/// What starts a scan over carries the scan's number: what comes back of an earlier one is told
/// apart by it.
enum Ask {
    /// A capture begins: its records are kept in this folder.
    Begin(PathBuf, u64),
    Record(CaptureRecord),
    /// The capture ended.
    End,
    /// This kept scan, read back.
    Show(PathBuf, u64),
    Clear(u64),
    /// The scans kept in this folder, listed.
    List(PathBuf),
}

/// To the detail worker.
enum DetailAsk {
    /// A keyframe, its cameras, and the base surface as it stood with it.
    /// `record`: the file the keyframe's record is kept in, where it is kept.
    Keyframe { keyframe: Arc<CaptureKeyframe>, record: Option<PathBuf>, cameras: Arc<Vec<CaptureCamera>>, base: Arc<Vec<ScanPoint>>, scan: u64 },
    /// The scan started over: what was gathered goes.
    Clear,
}

enum Tell {
    /// A keyframe's detail was made (or could not be): all the detail so far, and the base's
    /// cells it stands in for.
    /// `fresh`: of a keyframe new to the detail (not one matched again); `refining`: the
    /// keyframes matched in a hurry that are still to be matched again at the full size.
    Detail { scan: u64, points: Arc<Vec<ScanPoint>>, covered: Arc<HashSet<[i32; 3]>>, keyframes: usize, ms: f64, fresh: bool, refining: usize },
    Status { state: u8, text: String },
    Keyframe { keyframe: LiveKeyframe, bytes: u64 },
    Surface(Arc<Vec<ScanPoint>>),
    /// Bytes of records that are not keyframes.
    Bytes(u64),
    /// A kept scan was read back to its end.
    Shown,
    Kept(Vec<(PathBuf, ScanKept)>),
    Trouble(String),
}

/// The wall clock a detail line is stamped with, UTC to the millisecond: devhub logs the box's other work on the same
/// clock, so a slow keyframe can be laid next to what else ran there.
fn utc_now() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis()) % 86_400_000;
    format!("{:02}:{:02}:{:02}.{:03} UTC", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Where the scans are kept.
pub fn scans_dir() -> PathBuf {
    makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("scans")
}

fn private_folder(folder: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A scan's records in their order.
fn records_of(folder: &Path) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("record-") && n.ends_with(".bin")))
        .collect();
    names.sort();
    names
}

/// The scans kept in `folder`, newest first.
fn kept_in(folder: &Path) -> Vec<(PathBuf, ScanKept)> {
    let mut kept: Vec<(std::time::SystemTime, PathBuf, ScanKept)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let sizes: Vec<u64> = records_of(&path).iter().filter_map(|r| std::fs::metadata(r).ok()).map(|m| m.len()).collect();
            let keyframes = sizes.iter().filter(|s| **s >= KEYFRAME_BYTES).count();
            if keyframes == 0 {
                return None;
            }
            let made = entry.metadata().and_then(|m| m.modified()).ok()?;
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some((made, path, ScanKept { name, keyframes, megabytes: sizes.iter().sum::<u64>() as f64 / 1e6 }))
        })
        .collect();
    kept.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    kept.into_iter().map(|(_, path, scan)| (path, scan)).collect()
}

/// The worker's scan: the surface so far and, in a capture, where its records go.
#[derive(Default)]
struct Work {
    scan: LiveScan,
    folder: Option<PathBuf>,
    written: usize,
    /// Keyframes went into the scan since its surface was last sent.
    stale: bool,
    /// The scan's number, its cameras, and the keyframes that wait for the next surface to go to
    /// the detail worker with it.
    number: u64,
    cameras: Arc<Vec<CaptureCamera>>,
    waiting: Vec<(Arc<CaptureKeyframe>, Option<PathBuf>)>,
    detail: Option<Sender<DetailAsk>>,
}

impl Work {
    /// A record into the scan; what it was goes to the app.
    fn take(&mut self, record: &CaptureRecord, bytes: u64, tell: &Sender<Tell>, kept_in: Option<PathBuf>) {
        let began = std::time::Instant::now();
        match record {
            CaptureRecord::Cameras(list) => self.cameras = Arc::new(list.clone()),
            // (the detail matches a keyframe's pictures against each other: a one-camera device's
            // keyframe has no pair, its depth alone makes the surface)
            CaptureRecord::Keyframe(keyframe) if self.detail.is_some() && keyframe.views.len() >= 2 => self.waiting.push((Arc::new(keyframe.clone()), kept_in)),
            _ => {}
        }
        match self.scan.take(record) {
            Ok(Taken::Status { state, text }) => drop(tell.send(Tell::Status { state, text })),
            Ok(Taken::Keyframe(keyframe)) => {
                log!(
                    "scan: keyframe {} ({:.1} MB, depth {}, agreement {:?} mm, head at {:.2} {:.2} {:.2}, {} cells, fused in {:.0} ms)",
                    keyframe.index, bytes as f64 / 1e6, keyframe.depth, keyframe.agreement_mm, keyframe.head[0], keyframe.head[1], keyframe.head[2], self.scan.cells(), began.elapsed().as_secs_f64() * 1000.0,
                );
                let _ = tell.send(Tell::Keyframe { keyframe, bytes });
                self.stale = true;
            }
            Ok(Taken::Cameras(_) | Taken::Preview) => drop(tell.send(Tell::Bytes(bytes))),
            Err(e) => drop(tell.send(Tell::Trouble(format!("A record could not be read: {e}")))),
        }
    }

    /// The surface as it stands, to the app.
    fn surface(&mut self, tell: &Sender<Tell>) {
        self.stale = false;
        let began = std::time::Instant::now();
        let surface = self.scan.surface(SURFACE_POINTS);
        let finest = surface.points.iter().filter(|s| s.size <= makepad_fab_scan::live::VOXEL).count();
        let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
        for s in &surface.points {
            (low, high) = (low.min(s.point[1]), high.max(s.point[1]));
        }
        log!("scan: surface of {} points ({finest} at 10 mm) from {low:.2} to {high:.2} m up, in {:.0} ms", surface.points.len(), began.elapsed().as_secs_f64() * 1000.0);
        let points: Arc<Vec<ScanPoint>> = Arc::new(surface.points.iter().map(|s| ScanPoint { point: s.point, normal: s.normal, first: s.first, size: s.size, colour: s.colour }).collect());
        if let Some(detail) = &self.detail {
            for (keyframe, record) in self.waiting.drain(..) {
                let _ = detail.send(DetailAsk::Keyframe { keyframe, record, cameras: self.cameras.clone(), base: points.clone(), scan: self.number });
            }
        }
        let _ = tell.send(Tell::Surface(points));
    }

    /// A scan started over: nothing of the last one, the detail worker told.
    fn fresh(&self, folder: Option<PathBuf>, number: u64) -> Work {
        if let Some(detail) = &self.detail {
            let _ = detail.send(DetailAsk::Clear);
        }
        Work { folder, number, detail: self.detail.clone(), ..Work::default() }
    }

    /// A capture's record: kept in its folder, then taken.
    fn keep(&mut self, record: &CaptureRecord, tell: &Sender<Tell>) {
        let bytes = record.encode();
        let mut kept_in = None;
        if let Some(folder) = &self.folder {
            let path = folder.join(format!("record-{:06}.bin", self.written));
            match std::fs::write(&path, &bytes) {
                Ok(()) => kept_in = Some(path),
                Err(e) => drop(tell.send(Tell::Trouble(format!("The scan is not being kept: {}", e.kind())))),
            }
            self.written += 1;
        }
        self.take(record, bytes.len() as u64, tell, kept_in);
    }
}

/// The worker: one ask after another. The surface is made again once no ask waits, so keyframes
/// that come faster than it is made are all in the next one, and none waits behind it. Reading a
/// kept scan back it looks for the next ask between keyframes, and leaves the scan for it.
fn work(asks: Receiver<Ask>, tell: Sender<Tell>, detail: Option<Sender<DetailAsk>>) {
    let mut w = Work { detail, ..Work::default() };
    let mut next = None;
    loop {
        if next.is_none() && w.stale {
            match asks.try_recv() {
                Ok(ask) => next = Some(ask),
                Err(TryRecvError::Empty) => w.surface(&tell),
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let Some(ask) = next.take().or_else(|| asks.recv().ok()) else { return };
        match ask {
            Ask::Begin(folder, number) => w = w.fresh(Some(folder), number),
            Ask::Record(record) => w.keep(&record, &tell),
            Ask::End => {
                if let Some(folder) = w.folder.take() {
                    let _ = tell.send(Tell::Kept(kept_in(folder.parent().unwrap_or(&folder))));
                }
            }
            Ask::Clear(number) => w = w.fresh(None, number),
            Ask::List(folder) => drop(tell.send(Tell::Kept(kept_in(&folder)))),
            Ask::Show(folder, number) => {
                w = w.fresh(None, number);
                for path in records_of(&folder) {
                    let read = std::fs::read(&path).ok().and_then(|bytes| Some((CaptureRecord::decode(&bytes).ok()?, bytes.len() as u64)));
                    // (a record cut short by the scan's end: the rest still shows)
                    let Some((record, bytes)) = read else { continue };
                    let keyframe = matches!(record, CaptureRecord::Keyframe(_));
                    w.take(&record, bytes, &tell, Some(path.clone()));
                    if keyframe {
                        w.surface(&tell);
                        match asks.recv_timeout(SHOWN_EVERY) {
                            Ok(ask) => {
                                next = Some(ask);
                                break;
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
                if next.is_none() {
                    let _ = tell.send(Tell::Shown);
                }
            }
        }
    }
}

/// A hurried keyframe waiting for its second match.
enum Waits {
    /// Its record in the scan's folder and what it added in a file of the detail worker's.
    OnDisk { record: PathBuf, kept: PathBuf, cameras: Arc<Vec<CaptureCamera>>, place: Option<[f64; 3]> },
    Held(Hurried),
}

impl Waits {
    fn read(&self) -> std::io::Result<Hurried> {
        match self {
            Waits::OnDisk { record, kept, cameras, place } => match CaptureRecord::decode(&std::fs::read(record)?)? {
                CaptureRecord::Keyframe(keyframe) => Ok(Hurried { keyframe: Arc::new(keyframe), cameras: cameras.clone(), place: *place, kept: Kept::read(kept)? }),
                _ => Err(std::io::Error::other("not a keyframe's record")),
            },
            Waits::Held(hurried) => Ok(Hurried { keyframe: hurried.keyframe.clone(), cameras: hurried.cameras.clone(), place: hurried.place, kept: hurried.kept.clone() }),
        }
    }

    /// Done with: its file goes.
    fn forget(&self) {
        if let Waits::OnDisk { kept, .. } = self {
            let _ = std::fs::remove_file(kept);
        }
    }
}

/// The detail worker: each keyframe's pictures matched on the GPU from the base as it stood, the
/// points gathered and sent. A keyframe is matched in a square of its pictures about what the
/// cameras look at ([`refine_looked`]); keyframes that come faster than they are matched are
/// matched at half the size until it has caught up: none is dropped. Those are kept, and once
/// no keyframe waits (the person slowed down, or stopped the scan) each is matched again at the
/// full size and put in the place of what it gave before. A keyframe that comes while one is
/// matched again ends that match at its next iteration: a new keyframe never waits for an old one.
fn detail_work(asks: Receiver<DetailAsk>, tell: Sender<Tell>, gpu: GpuLink) {
    let matcher = Local { gpu: Some(gpu.clone()) };
    // The matching's kernels are made for a picture size the first time one is matched at it
    // (about 0.4 s, measured): a made-up pair at each size a scan matches at, with and without a
    // start, before any keyframe comes, so the first keyframe is not the one that waits for it.
    let warmed = std::time::Instant::now();
    for side in [WINDOW, WINDOW / 2] {
        let (views, truth) = makepad_ai_geometry::synthetic::textured_plane(side, 0.68 * side as f64, 0.0, &[0.064]);
        for start in [Some(&truth), None] {
            let params = makepad_ai_geometry::patchmatch::PatchMatchParams { iterations: 1, ..makepad_fab_scan::detail::params_of(start) };
            if let Err(e) = makepad_fab_scan::detail::Matcher::run(&matcher, &views, start, &params) {
                log!("scan: the detail's matching did not warm at {side} px: {e}");
            }
        }
    }
    log!("scan: the detail's matching warmed in {:.0} ms, at {}", warmed.elapsed().as_secs_f64() * 1000.0, utc_now());
    let mut refined = Refined::new(DETAIL_CELL);
    let mut done = 0;
    // the keyframes' cameras so far: where they looked is where the detail is matched
    let mut looks: Vec<GrayView> = Vec::new();
    let mut waiting: VecDeque<DetailAsk> = VecDeque::new();
    // the keyframes matched in a hurry, the scan each is of, oldest first. A fast walk gives
    // hundreds, megabytes each: one whose record is kept in the scan's folder waits there, and
    // what it added waits in a file of this worker's own; only one that is not kept waits in memory.
    let mut hurried_ones: VecDeque<(u64, Waits)> = VecDeque::new();
    let spill = std::env::temp_dir().join(format!("fab-scan-refine-{}", std::process::id()));
    let mut latest: Arc<Vec<([f32; 3], f32)>> = Arc::new(Vec::new());
    let send = |refined: &Refined, scan: u64, done: usize, ms: f64, fresh: bool, refining: usize| {
        let points = refined.points().into_iter().map(|p| ScanPoint { point: p.point, normal: p.normal, first: 0, size: p.size, colour: p.colour }).collect();
        let _ = tell.send(Tell::Detail { scan, points: Arc::new(points), covered: Arc::new(refined.covered(VOXEL)), keyframes: done, ms, fresh, refining });
    };
    loop {
        if waiting.is_empty() && hurried_ones.is_empty() {
            match asks.recv() {
                Ok(ask) => waiting.push_back(ask),
                Err(_) => return,
            }
        }
        waiting.extend(asks.try_iter());
        if let Some(at) = waiting.iter().rposition(|ask| matches!(ask, DetailAsk::Clear)) {
            waiting.drain(..=at);
            (refined, done) = (Refined::new(DETAIL_CELL), 0);
            looks.clear();
            for (_, waits) in hurried_ones.drain(..) {
                waits.forget();
            }
            continue;
        }
        if waiting.is_empty() {
            // nothing waits: a hurried keyframe again, for as long as nothing comes
            let Some((scan, waits)) = hurried_ones.pop_front() else { continue };
            let hurried = match waits.read() {
                Ok(hurried) => hurried,
                Err(e) => {
                    // (the scan's folder went meanwhile: the keyframe has its hurried points still)
                    log!("scan: a hurried keyframe could not be read back for its second match: {}", e.kind());
                    waits.forget();
                    send(&refined, scan, done, 0.0, false, hurried_ones.len());
                    continue;
                }
            };
            let came = std::cell::RefCell::new(Vec::new());
            let gone = std::cell::Cell::new(false);
            let keep = || {
                loop {
                    match asks.try_recv() {
                        Ok(ask) => came.borrow_mut().push(ask),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            gone.set(true);
                            break;
                        }
                    }
                }
                came.borrow().is_empty() && !gone.get()
            };
            let began = std::time::Instant::now();
            let again = rematch(&LocalWhile { gpu: gpu.clone(), keep: &keep }, &mut refined, &hurried, &latest, DETAIL_CELL);
            let gave_way = !came.borrow().is_empty();
            waiting.extend(came.into_inner());
            match again {
                Ok(settled) => {
                    log!(
                        "scan: detail of keyframe {} again at the full size: settled by {:.2} mm, {} in all, in {:.0} ms, {} to go, at {}",
                        hurried.keyframe.index, settled.map_or(0.0, |s| s.by) * 1000.0, refined.len(), began.elapsed().as_secs_f64() * 1000.0, hurried_ones.len(), utc_now(),
                    );
                    waits.forget();
                    send(&refined, scan, done, began.elapsed().as_secs_f64() * 1000.0, false, hurried_ones.len());
                }
                // (a keyframe came: this one again later)
                Err(e) if e == "cancelled" && gave_way => {
                    log!("scan: detail of keyframe {} again gave way to a new keyframe after {:.0} ms, at {}", hurried.keyframe.index, began.elapsed().as_secs_f64() * 1000.0, utc_now());
                    hurried_ones.push_front((scan, waits));
                }
                // (the app's GPU device ended under the match, or the app let go of the worker: it is quitting)
                Err(e) if e == "cancelled" => {
                    waits.forget();
                    for (_, waits) in hurried_ones.drain(..) {
                        waits.forget();
                    }
                    let _ = std::fs::remove_dir(&spill);
                    return;
                }
                // (it has its hurried points still)
                Err(e) => {
                    log!("scan: no detail of keyframe {} again: {e}", hurried.keyframe.index);
                    waits.forget();
                }
            }
            continue;
        }
        let Some(DetailAsk::Keyframe { keyframe, record, cameras, base, scan }) = waiting.pop_front() else { continue };
        // where the keyframe's first camera stood, as the tracker has it
        let mut eye: Option<[f64; 3]> = None;
        for view in &keyframe.views {
            if let Some(lens) = cameras.get(view.camera as usize).and_then(|c| c.lens_pose.as_ref()) {
                let cam_to_world = camera_to_world(&view.head, lens);
                eye.get_or_insert([cam_to_world[0][3], cam_to_world[1][3], cam_to_world[2][3]]);
                looks.push(GrayView { width: 0, height: 0, pixels: Vec::new(), k: [1.0, 1.0, 0.0, 0.0], cam_to_world });
            }
        }
        // (where the cameras' axes meet, once three keyframes say; a head turning where it stands
        // gives its own place, which shows in no picture: the pictures' middles then)
        let place = (looks.len() >= 6).then(|| looked_at(&looks).ok()).flatten();
        let hurried = waiting.len() >= 2;
        let began = std::time::Instant::now();
        let seen: Arc<Vec<([f32; 3], f32)>> = Arc::new(base.iter().map(|p| (p.point, p.size)).collect());
        latest = seen.clone();
        let mut on_disk = "";
        match refine_looked(&matcher, &keyframe, &cameras, &seen, DETAIL_CELL, place, hurried) {
            Ok((mut points, shrink)) => {
                let settled = refined.settle(&mut points);
                if hurried {
                    let kept = refined.add_kept(&points, settled);
                    let file = spill.join(format!("kept-{scan}-{}.bin", keyframe.index));
                    let written = match record {
                        Some(record) => private_folder(&spill).and_then(|()| kept.write(&file)).map(|()| record),
                        None => Err(std::io::Error::other("the scan is not kept")),
                    };
                    hurried_ones.push_back((scan, match written {
                        Ok(record) => {
                            on_disk = ", waits on disk for its second match";
                            Waits::OnDisk { record, kept: file, cameras: cameras.clone(), place }
                        }
                        Err(e) => {
                            log!("scan: hurried keyframe {} waits in memory for its second match ({:.1} MB): {}", keyframe.index, kept.bytes() as f64 / 1e6, e.kind());
                            Waits::Held(Hurried { keyframe: keyframe.clone(), cameras: cameras.clone(), place, kept })
                        }
                    }));
                } else {
                    refined.add_settled(&points, settled.as_ref());
                }
                // (what the keyframe sees through, from where its camera stood as it was settled:
                // what other keyframes' matching bled past an edge there goes)
                if let Some(eye) = eye {
                    refined.carve(settled.map_or(eye, |s| s.moved(eye)), &points);
                }
                done += 1;
                log!(
                    "scan: detail of keyframe {}: {} points at 1/{shrink} size, settled by {:.2} mm, {} in all, in {:.0} ms, {} waiting{on_disk}, at {}",
                    keyframe.index, points.len(), settled.map_or(0.0, |s| s.by) * 1000.0, refined.len(), began.elapsed().as_secs_f64() * 1000.0, waiting.len(), utc_now(),
                );
            }
            // (the app's GPU device ended under the match: the app is quitting, nothing is wrong)
            Err(e) if e == "cancelled" => {
                for (_, waits) in hurried_ones.drain(..) {
                    waits.forget();
                }
                let _ = std::fs::remove_dir(&spill);
                return;
            }
            Err(e) => {
                log!("scan: no detail of keyframe {}: {e}", keyframe.index);
                let _ = tell.send(Tell::Trouble(format!("No detail from a keyframe's pictures: {e}")));
            }
        }
        send(&refined, scan, done, began.elapsed().as_secs_f64() * 1000.0, true, hurried_ones.len());
    }
}

/// A scan's name from when it began (seconds since 1970, UTC): `scan-2026-11-11-132005`.
fn name_at(seconds: u64) -> String {
    // days to a date of the civil calendar (eras of 400 years from 0000-03-01)
    let days = (seconds / 86_400) as i64 + 719_468;
    let (era, day_of_era) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 { month_from_march + 3 } else { month_from_march - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let of_day = seconds % 86_400;
    format!("scan-{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}", of_day / 3600, of_day / 60 % 60, of_day % 60)
}

#[derive(Default)]
pub struct ScanLab {
    asks: Option<Sender<Ask>>,
    told: Option<Receiver<Tell>>,
    timer: Timer,
    /// A capture is asked for; the headset session that has the ask.
    live: bool,
    asked: Option<u64>,
    /// The kept scans' folders, as the panel's rows.
    kept: Vec<PathBuf>,
    listed: bool,
    /// The worker was asked for the kept scans and has not answered yet.
    listing: bool,
    /// Of the scan in the view: when it began, its bytes, the last keyframes' agreement, the
    /// name it is kept or was kept under, the keyframes a kept one has.
    began: f64,
    bytes: u64,
    /// The scan's number (what comes back of an earlier one is dropped), the keyframes whose
    /// detail is still to come, and of the detail: its keyframes and the last one's time.
    number: u64,
    owed: usize,
    /// The keyframes matched in a hurry that are still to be matched again at the full size.
    refining: usize,
    detailed: usize,
    detail_ms: f64,
    /// This app's GPU as compute programs use it; None where it has no compute.
    device: Option<GpuDevice>,
    detailing: bool,
    agreement: Option<f32>,
    name: String,
    of: usize,
    headset: String,
}

impl ScanLab {
    /// The workers, started at the first ask: the scan's, and the detail's on this app's GPU.
    fn ask(&mut self, cx: &mut Cx, ask: Ask) {
        if self.asks.is_none() {
            let (asks, asked) = channel();
            let (tell, told) = channel();
            let detail = match GpuDevice::new(cx) {
                Ok((device, link)) => {
                    self.device = Some(device);
                    let (detail, detail_asked) = channel();
                    let tell = tell.clone();
                    match std::thread::Builder::new().name("fab-scan-detail".into()).spawn(move || detail_work(detail_asked, tell, link)) {
                        Ok(_) => Some(detail),
                        Err(e) => {
                            log!("scan: no detail worker: {e}");
                            None
                        }
                    }
                }
                Err(e) => {
                    log!("scan: no detail: this GPU runs no compute programs ({e})");
                    None
                }
            };
            self.detailing = detail.is_some();
            match std::thread::Builder::new().name("fab-scan".into()).spawn(move || work(asked, tell, detail)) {
                Ok(_) => (self.asks, self.told) = (Some(asks), Some(told)),
                Err(e) => log!("scan: no worker: {e}"),
            }
        }
        self.listing |= matches!(ask, Ask::End | Ask::List(_));
        if let Some(asks) = &self.asks {
            let _ = asks.send(ask);
        }
    }

    fn fresh(&mut self, state: &mut AppState) {
        let kept = std::mem::take(&mut state.scan.kept);
        state.scan = ScanView { kept, ..ScanView::default() };
        (self.began, self.bytes, self.agreement, self.of) = (Cx::time_now(), 0, None, 0);
        self.number += 1;
        (self.owed, self.refining, self.detailed, self.detail_ms) = (0, 0, 0, 0.0);
        self.name.clear();
    }

    /// The headset's ask and the stream's sink let go.
    fn release(&mut self, cx: &mut Cx) {
        if let Some(server) = XrServer::get(cx) {
            server.set_capture(None);
            server.set_capture_sink(None);
        }
        (self.live, self.asked) = (false, None);
    }

    /// True when what the tabs show changed.
    pub fn action(&mut self, cx: &mut Cx, action: &ScanAction, state: &mut AppState) -> bool {
        match action {
            ScanAction::Start => {
                if state.scan.phase.busy() {
                    return false;
                }
                self.fresh(state);
                let Some(server) = XrServer::get(cx) else {
                    state.scan.phase = ScanPhase::Failed;
                    state.scan.status = "Fab's headset link is off: turn it on, then start the scan".into();
                    return true;
                };
                self.name = name_at(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()));
                let folder = scans_dir().join(&self.name);
                if let Err(e) = private_folder(&folder) {
                    state.scan.phase = ScanPhase::Failed;
                    state.scan.status = format!("The scan's folder could not be made: {}", e.kind());
                    return true;
                }
                self.ask(cx, Ask::Begin(folder, self.number));
                if let Some(asks) = self.asks.clone() {
                    server.set_capture_sink(Some(Arc::new(move |_, record| {
                        let _ = asks.send(Ask::Record(record));
                    })));
                }
                self.live = true;
                state.scan.phase = ScanPhase::Waiting;
                log!("scan: started as {}", self.name);
            }
            ScanAction::Stop => {
                if !state.scan.phase.busy() {
                    return false;
                }
                let live = self.live;
                self.release(cx);
                state.scan.phase = ScanPhase::Idle;
                if live {
                    self.ask(cx, Ask::End);
                    log!("scan: stopped, {} keyframes", state.scan.keyframes.len());
                } else {
                    // (a kept scan being shown: the worker leaves it where it is)
                    self.ask(cx, Ask::List(scans_dir()));
                }
            }
            ScanAction::Clear => {
                if state.scan.phase.busy() {
                    return false;
                }
                self.fresh(state);
                self.ask(cx, Ask::Clear(self.number));
            }
            ScanAction::Colour(on) => {
                state.ui.scan_colour = *on;
                fab::view_prefs::save_scan_colour(*on);
                return true;
            }
            ScanAction::Cameras(on) => {
                state.ui.scan_cameras = *on;
                fab::view_prefs::save_scan_cameras(*on);
                return true;
            }
            ScanAction::Show(row) => {
                let Some(folder) = self.kept.get(*row).cloned().filter(|_| !state.scan.phase.busy()) else { return false };
                self.fresh(state);
                self.name = state.scan.kept.get(*row).map(|k| k.name.clone()).unwrap_or_default();
                self.of = state.scan.kept.get(*row).map_or(0, |k| k.keyframes);
                state.scan.phase = ScanPhase::Showing;
                self.ask(cx, Ask::Show(folder, self.number));
                log!("scan: showing {}", self.name);
            }
        }
        self.timer = cx.start_interval(0.1);
        self.words(state);
        true
    }

    /// The scan's line and the panel's figures, from what is known now.
    fn words(&self, state: &mut AppState) {
        let a = &mut state.scan;
        let count = a.keyframes.len();
        a.status = match a.phase {
            ScanPhase::Idle if count > 0 && !self.name.is_empty() && self.refining > 0 => format!("{}, kept as {}; refining the detail", keyframes_in_words(count), self.name),
            ScanPhase::Idle if count > 0 && !self.name.is_empty() => format!("{}, kept as {}", keyframes_in_words(count), self.name),
            ScanPhase::Idle => "No scan running".into(),
            ScanPhase::Waiting if self.headset.is_empty() => "Waiting for a headset: open XR Remote on it and link it to Fab".into(),
            ScanPhase::Waiting => format!("Waiting for {}'s cameras", self.headset),
            ScanPhase::Capturing => format!("Scanning: {}", keyframes_in_words(count)),
            ScanPhase::Showing => format!("Showing {}: {count} of {}", self.name, keyframes_in_words(self.of)),
            // (the reason stands as it was set)
            ScanPhase::Failed => std::mem::take(&mut a.status),
        };
        let mut stats = vec![("Headset".to_string(), if self.headset.is_empty() { "not linked".to_string() } else { format!("{} linked", self.headset) })];
        if count > 0 || a.phase.busy() {
            let with_depth = a.keyframes.iter().filter(|k| k.depth).count();
            stats.push(("Keyframes".into(), if with_depth == count { format!("{count}") } else { format!("{count} ({with_depth} with the headset's depth)") }));
            let finest = a.surface.iter().filter(|p| p.size <= 0.0101).count();
            stats.push(("Surface".into(), format!("{} points, {finest} of them at 10 mm", a.surface.len())));
            if let Some(mm) = self.agreement {
                stats.push(("Depth agreement".into(), format!("{mm:.1} mm between keyframes")));
            }
            if self.detailing {
                let detail = match (self.detailed, self.owed) {
                    (0, 0) => "none yet".to_string(),
                    (0, owed) => format!("matching {}", keyframes_in_words(owed)),
                    (done, owed) => format!("{} points at {:.1} mm from {}, {:.0} ms the last{}", a.detail.len(), DETAIL_CELL * 1000.0, keyframes_in_words(done), self.detail_ms, if owed > 0 { format!(", {owed} to come") } else if self.refining > 0 { format!("; refining {}", keyframes_in_words(self.refining)) } else { String::new() }),
                };
                stats.push(("Detail".into(), detail));
            } else {
                stats.push(("Detail".into(), "none: this GPU runs no compute programs".into()));
            }
            stats.push(("Received".into(), format!("{:.0} MB", self.bytes as f64 / 1e6)));
            stats.push(("Time".into(), format!("{:.0} s", a.seconds)));
        }
        a.stats = stats;
    }

    /// What the worker sent, into the state. True when anything came.
    fn told(&mut self, cx: &mut Cx, state: &mut AppState) -> bool {
        let mut changed = false;
        let told: Vec<Tell> = self.told.as_ref().map(|t| t.try_iter().collect()).unwrap_or_default();
        for tell in told {
            changed = true;
            let a = &mut state.scan;
            match tell {
                Tell::Status { state: s, text } => {
                    log!("scan: the headset says ({s}) {text}");
                    if self.live {
                        match s {
                            capture_state::CAPTURING => a.phase = ScanPhase::Capturing,
                            capture_state::FAILED => {
                                a.phase = ScanPhase::Failed;
                                a.status = "The headset cannot scan".into();
                                self.release(cx);
                                self.ask(cx, Ask::End);
                            }
                            capture_state::STARTING if a.phase != ScanPhase::Capturing => a.phase = ScanPhase::Waiting,
                            _ => {}
                        }
                    }
                    if self.live {
                        a.headset_says = text;
                    }
                }
                Tell::Detail { scan, points, covered, keyframes, ms, fresh, refining } => {
                    if scan == self.number {
                        self.owed = self.owed.saturating_sub(usize::from(fresh));
                        self.refining = refining;
                        (a.detail, a.covered, self.detailed, self.detail_ms) = (points, covered, keyframes, ms);
                    }
                }
                Tell::Keyframe { keyframe, bytes } => {
                    self.bytes += bytes;
                    self.owed += usize::from(self.detailing && !keyframe.cameras.is_empty());
                    self.agreement = keyframe.agreement_mm.or(self.agreement);
                    if self.live {
                        a.phase = ScanPhase::Capturing;
                    }
                    a.seconds = Cx::time_now() - self.began;
                    a.keyframes.push(ScanKeyframe { head: keyframe.head, cameras: keyframe.cameras, depth: keyframe.depth, at: a.seconds });
                }
                Tell::Surface(points) => a.surface = points,
                Tell::Bytes(bytes) => self.bytes += bytes,
                Tell::Shown => {
                    if a.phase == ScanPhase::Showing {
                        a.phase = ScanPhase::Idle;
                    }
                }
                Tell::Kept(kept) => {
                    self.listing = false;
                    self.kept = kept.iter().map(|(path, _)| path.clone()).collect();
                    a.kept = kept.into_iter().map(|(_, scan)| scan).collect();
                }
                Tell::Trouble(why) => {
                    log!("scan: {why}");
                    a.headset_says = why;
                }
            }
        }
        changed
    }

    /// Keeps the scan going; call on every event. True when what the tabs show changed (the
    /// caller redraws them at once).
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState) -> bool {
        // the detail's GPU work: what its sessions sent, run between this app's frames
        if let (Event::Signal, Some(device)) = (event, self.device.as_mut()) {
            device.handle(cx);
        }
        if state.ui.shows(Family::Scan) && !self.listed {
            self.listed = true;
            self.ask(cx, Ask::List(scans_dir()));
            self.timer = cx.start_interval(0.1);
            self.words(state);
            return true;
        }
        if self.timer.is_event(event).is_none() {
            return false;
        }
        let mut changed = self.told(cx, state);
        if self.live {
            // The headset may link late or link again, under a new session: each gets the ask.
            let headset = XrServer::get(cx).and_then(|s| s.live_headset());
            let (session, name) = (headset.as_ref().map(|h| h.session), headset.map(|h| h.name).unwrap_or_default());
            if session != self.asked {
                self.asked = session;
                if let Some(server) = XrServer::get(cx) {
                    server.set_capture(session.map(|s| (s, ASK)));
                }
                if session.is_none() {
                    state.scan.phase = ScanPhase::Waiting;
                    state.scan.headset_says.clear();
                }
                changed = true;
            }
            changed |= std::mem::replace(&mut self.headset, name.clone()) != name;
        }
        if state.scan.phase.busy() {
            let seconds = Cx::time_now() - self.began;
            changed |= seconds.floor() != state.scan.seconds.floor();
            state.scan.seconds = seconds;
        } else if !changed && !self.listing && self.owed == 0 && self.refining == 0 {
            // (nothing under way, nothing came, nothing is owed or being refined: the next ask starts the timer again)
            cx.stop_timer(self.timer);
        }
        if changed {
            self.words(state);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_desk_link::xr_capture::{CaptureKeyframe, CapturePose, CaptureView};

    fn keyframe(index: u32) -> CaptureRecord {
        let head = CapturePose { orientation: [0.0, 0.0, 0.0, 1.0], position: [index as f32, 1.5, 0.0] };
        CaptureRecord::Keyframe(CaptureKeyframe { index, views: vec![CaptureView { camera: 0, head, ..Default::default() }], depth: None })
    }

    /// A capture's records are kept in its own folder, readable only by this user, and each is
    /// told as it comes; the kept scan is listed and reads back the same, and an ask while it
    /// reads back takes over.
    #[test]
    fn a_capture_is_kept_told_listed_and_read_back() {
        assert_eq!(name_at(1_794_403_205), "scan-2026-11-11-132005", "a scan is named by its date");
        assert_eq!(name_at(951_782_400), "scan-2000-02-29-000000");
        let scans = std::env::temp_dir().join(format!("fab-scans-{}", std::process::id()));
        let folder = scans.join("one");
        private_folder(&folder).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700, "the scan's folder is this user's alone");
        }
        let (asks, asked) = channel();
        let (tell, told) = channel();
        let worker = std::thread::spawn(move || work(asked, tell, None));
        let records = [CaptureRecord::Status { state: capture_state::CAPTURING, text: "on".into() }, keyframe(0), keyframe(1)];
        asks.send(Ask::Begin(folder.clone(), 1)).unwrap();
        for record in &records {
            asks.send(Ask::Record(record.clone())).unwrap();
        }
        asks.send(Ask::End).unwrap();
        let mut heads = Vec::new();
        let kept = loop {
            match told.recv().unwrap() {
                Tell::Keyframe { keyframe, .. } => heads.push(keyframe.head[0]),
                Tell::Kept(kept) => break kept,
                Tell::Status { state, .. } => assert_eq!(state, capture_state::CAPTURING),
                Tell::Surface(points) => assert!(points.is_empty(), "no depth, no surface"),
                _ => panic!("a tell that no record asked for"),
            }
        };
        assert_eq!(heads, [0.0, 1.0], "each keyframe is told in its order");
        for (n, record) in records.iter().enumerate() {
            let bytes = std::fs::read(folder.join(format!("record-{n:06}.bin"))).unwrap();
            assert_eq!(&CaptureRecord::decode(&bytes).unwrap(), record, "record {n} is kept as it came");
        }
        // (these keyframes carry no pictures: too small to count as kept keyframes)
        assert!(kept.is_empty(), "a folder with no keyframe pictures is not a kept scan");
        std::fs::write(folder.join("record-000009.bin"), vec![0u8; KEYFRAME_BYTES as usize]).unwrap();
        let listed = kept_in(&scans);
        assert_eq!((listed.len(), listed[0].1.keyframes, listed[0].1.name.as_str()), (1, 1, "one"), "a scan with a keyframe's pictures is listed");
        // read back: the first keyframe again, then the wait between keyframes ends at the next ask
        asks.send(Ask::Show(folder.clone(), 2)).unwrap();
        let first = loop {
            if let Tell::Keyframe { keyframe, .. } = told.recv().unwrap() {
                break keyframe;
            }
        };
        assert_eq!(first.head[0], 0.0);
        asks.send(Ask::List(scans.clone())).unwrap();
        let after: Vec<Tell> = std::iter::from_fn(|| told.recv().ok()).take_while(|t| !matches!(t, Tell::Kept(_))).collect();
        assert!(!after.iter().any(|t| matches!(t, Tell::Keyframe { .. } | Tell::Shown)), "the ask took over before the second keyframe");
        drop(asks);
        worker.join().unwrap();
        std::fs::remove_dir_all(&scans).unwrap();
    }
}
