//! A picture made into a part: `fab.image_to_mesh` from the chat or an agent and the Parts
//! panel's Add > From image… (a picture dropped on the window prints in colour instead).
//!
//! Each request runs on its own worker thread: the picture is read and made ready
//! (`fab_ai::img2mesh::prepare_image`), then one hub job (`GenJob`, routed by the hub's
//! own fleet router to a node that has the image-to-3D model installed: a job never makes
//! a node download weights) is submitted and polled to its end; its GLB is welded, sized
//! and stood on the ground. The model worker then puts it on the plate
//! (`Session::import_mesh`, `Job::ImportMesh`). Cancel stops the job on its node (the
//! hub's backend checks its cancel token between stages, so the GPU work ends).
//!
//! What the person sees: the caption's job strip carries the stage, its share and the
//! node, and its × cancels (the node frees the model's GPU memory); the chat card carries
//! the same for a chat call.

use fab_ai::img2mesh::{self, Fitted, Provenance, Request};
use makepad_ai_hub::job::{process_origin, FleetRouter, GenJob, JobPoll, Want};
use makepad_ai_hub::protocol::GenerateRequestJson;
use makepad_ai_hub::registry::Domain;
use makepad_ai_hub::board::BoardGen;
use makepad_ai_services::progress_eta::plain_stage;
use makepad_ai_services::wire::TaskCard;
use makepad_widgets::jobs::{JobId, JobSpec};
use makepad_widgets::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long the fleet's beacons take to be heard once (they come every two seconds).
const HEAR_FLEET: Duration = Duration::from_millis(2500);

/// Where the picture comes from.
#[derive(Clone)]
pub enum Picture {
    File(PathBuf),
    /// A picture the person sent in the chat, as it was dropped (read on the worker).
    Sent(makepad_ai_services::engine::SentPicture),
}

impl Picture {
    fn label(&self) -> String {
        match self {
            Picture::File(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string()),
            Picture::Sent(picture) => picture.name.clone(),
        }
    }
}

/// The picture extensions a part is made from (and a dropped or opened file prints in colour):
/// what the app's one picture decoder reads (`fab_colour::decode_rgba`).
pub const PICTURE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

pub fn is_picture(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| PICTURE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A finished mesh, ready for the plate.
pub struct Made {
    pub name: String,
    pub fitted: Fitted,
    pub printable: Option<Result<fab_ai::printable::Report, String>>,
    pub from: Provenance,
    /// The node it was made on.
    pub node: String,
    /// Seconds per stage, in order: the picture, waiting for the node, the hub's own
    /// stages, the mesh.
    pub stages: Vec<(String, f64)>,
}

#[derive(Debug)]
pub enum Img2MeshEvent {
    Progress { job: u64, stage: String, permille: u16, node: Option<String> },
    /// The mesh (taken once: actions are delivered by reference).
    Made { job: u64, made: Mutex<Option<Made>> },
    Failed { job: u64, error: String, cancelled: bool },
}

impl std::fmt::Debug for Made {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Made").field("name", &self.name).field("node", &self.node).finish()
    }
}

/// One request in flight.
pub struct Running {
    pub job: u64,
    /// The chat's (or an agent's) call it answers; None: the person's own.
    pub call: Option<u64>,
    pub picture: String,
    /// The card's input picture: a chat picture's name or the picture file's path.
    pub chat_picture: Option<String>,
    pub stage: String,
    pub stages: Vec<String>,
    pub permille: u16,
    pub node: Option<String>,
    cancel: Arc<AtomicBool>,
    /// Its row on the caption's job strip.
    pub board: JobId,
}

impl Running {
    /// The task card while the picture is being made into a part.
    pub fn task(&self) -> TaskCard {
        TaskCard {
            title: "Making a part".into(),
            source: self.picture.clone(),
            picture: self.chat_picture.clone().unwrap_or_default(),
            stage: plain_stage(&self.stage),
            host: self.node.clone().unwrap_or_default(),
            stages: self.stages.clone(),
            ..Default::default()
        }
    }

    /// One line for the caption's job strip and the chat card.
    pub fn line(&self) -> String {
        let on = self.node.as_ref().map(|n| format!(" on {n}")).unwrap_or_default();
        format!("Making a part from {}: {} {}%{on}", self.picture, self.stage, self.permille / 10)
    }
}

#[derive(Default)]
pub struct Img2Mesh {
    next: u64,
    pub running: Vec<Running>,
    /// What a failed request's Retry starts again, by its board job.
    retries: std::collections::HashMap<JobId, (Request, Picture)>,
}

impl Img2Mesh {
    /// Start one request; its events come back as `Img2MeshEvent`s for job `job`.
    pub fn start(&mut self, call: Option<u64>, request: Request, picture: Picture) -> u64 {
        self.next += 1;
        let job = self.next;
        let label = picture.label();
        // The card's input picture: the chat picture's name, or the file.
        let chat_picture = match &picture {
            Picture::Sent(picture) => Some(picture.name.clone()),
            Picture::File(path) => Some(path.display().to_string()),
        };
        // The job on the caption's strip: its cancel is this request's cancel (the worker sends the
        // freeing cancel to the node at its next poll).
        let board = JobSpec::new(format!("Generating mesh from {label}")).machine("AI hub").stage("starting").start();
        let cancel = board.cancel_flag();
        let held: Vec<JobId> = makepad_widgets::jobs::snapshot().iter().map(|j| j.id).collect();
        self.retries.retain(|id, _| held.contains(id));
        self.retries.insert(board.id(), (request.clone(), picture.clone()));
        self.running.push(Running { job, call, picture: label, chat_picture, stage: "starting".into(), stages: vec![plain_stage("starting")], permille: 0, node: None, cancel: cancel.clone(), board: board.id() });
        let spawned = std::thread::Builder::new().name("fab-img2mesh".into()).spawn(move || {
            let reporter = board.reporter();
            let mut tracked = BoardGen::with_handle(board);
            let event = match run(job, &request, picture, &cancel, &mut tracked) {
                Ok(made) => {
                    reporter.stage("putting it on the plate", None, Some(1.0));
                    tracked.done();
                    Img2MeshEvent::Made { job, made: Mutex::new(Some(made)) }
                }
                Err(error) => {
                    let cancelled = cancel.load(Ordering::Relaxed);
                    tracked.fail(error.clone());
                    Img2MeshEvent::Failed { job, cancelled, error }
                }
            };
            Cx::post_action(event);
        });
        if let Err(e) = spawned {
            Cx::post_action(Img2MeshEvent::Failed { job, error: format!("could not start the worker: {e}"), cancelled: false });
        }
        job
    }

    pub fn get_mut(&mut self, job: u64) -> Option<&mut Running> {
        self.running.iter_mut().find(|r| r.job == job)
    }

    /// The request is over (made, failed or cancelled).
    pub fn finish(&mut self, job: u64) -> Option<Running> {
        let i = self.running.iter().position(|r| r.job == job)?;
        Some(self.running.remove(i))
    }

    /// Cancel a request: through its board job, so the caption's job strip shows it too.
    pub fn cancel(&mut self, job: u64) {
        if let Some(r) = self.get_mut(job) {
            if !makepad_widgets::jobs::board().cancel(r.board) {
                r.cancel.store(true, Ordering::Relaxed);
            }
            r.stage = "cancelling".into();
        }
    }

    /// Retry on the caption's job strip of a failed request: start it again. False when the job is
    /// not one of these.
    pub fn retry(&mut self, board: JobId) -> bool {
        // A chat call was answered with the failure: the retry is the person's own.
        let Some((request, picture)) = self.retries.remove(&board) else { return false };
        self.start(None, request, picture);
        true
    }

    /// The request a call started.
    pub fn of_call(&self, call: u64) -> Option<&Running> {
        self.running.iter().find(|r| r.call == Some(call))
    }
}

fn secs(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

fn make_printable(fitted: Fitted, request: &Request, stages: &mut Vec<(String, f64)>, progress: impl FnOnce(&str)) -> (Fitted, Option<Result<fab_ai::printable::Report, String>>) {
    if !request.printable {
        return (fitted, None);
    }
    progress("making it printable");
    let started = Instant::now();
    let result = fab_ai::printable::make_printable(&fitted.mesh, &fab_ai::printable::Options { height_mm: None, ..Default::default() })
        .and_then(|(mesh, mut report)| {
            let extent = report.after.size.iter().copied().fold(0.0, f64::max);
            let mut repaired = img2mesh::restand(mesh, request.size_mm)?;
            repaired.dropped_sheets = fitted.dropped_sheets;
            let scale = request.size_mm / extent;
            report.scale *= scale;
            report.cell_mm *= scale;
            report.closing_radius_mm *= scale;
            report.base_cut_mm *= scale;
            report.base_area_mm2 *= scale.powi(2);
            report.thickened_mm3 *= scale.powi(3);
            report.overhang_before_mm2 *= scale.powi(2);
            report.overhang_after_mm2 *= scale.powi(2);
            report.notes.push(format!("stood on Y=0 and resized to {:.1} mm on its largest dimension", request.size_mm));
            // `restand` already scanned topology; scaling volume needs no second scan.
            report.after.size = repaired.size;
            report.after.open_edges = repaired.open_edges;
            report.after.nonmanifold_edges = repaired.nonmanifold_edges;
            report.after.shells = repaired.shells;
            report.after.volume_mm3 *= scale.powi(3);
            Ok((repaired, report))
        });
    stages.push(("making it printable".into(), secs(started)));
    match result {
        Ok((mesh, report)) => (mesh, Some(Ok(report))),
        Err(error) => (fitted, Some(Err(error))),
    }
}

fn read_picture(picture: Picture) -> Result<(String, Vec<u8>), String> {
    match picture {
        Picture::File(path) => {
            let bytes = std::fs::read(&path).map_err(|e| format!("cannot read the picture {}: {e}", path.display()))?;
            Ok((Picture::File(path).label(), bytes))
        }
        Picture::Sent(picture) => Ok((picture.name.clone(), picture.data()?.to_vec())),
    }
}

/// Preserve the router's complete fleet diagnosis, including nodes whose
/// beacons were never heard. Discovery failures use the same shared report.
fn unavailable(error: &str, discovery: &makepad_ai_hub::discovery::Discovery) -> String {
    use makepad_ai_hub::fleet_report::{diagnose, NETWORKING_OFF};
    let report = if error.contains("\nWhat to do:\n") {
        error.to_string()
    } else {
        let snapshots = makepad_ai_hub::job::fleet_snapshots(&[]);
        format!("{}\nRouting reason: {error}", diagnose(img2mesh::MODEL, "mesh", &snapshots, discovery).render(img2mesh::MODEL))
    };
    let lead = format!("No AI Hub node on the network has the image-to-3D model ({}) ready.", img2mesh::MODEL);
    if let Some(rest) = report.strip_prefix(NETWORKING_OFF) {
        format!("{NETWORKING_OFF}\n{lead}{rest}")
    } else {
        format!("{lead}\n{report}")
    }
}

fn run(job: u64, request: &Request, picture: Picture, cancel: &AtomicBool, tracked: &mut BoardGen) -> Result<Made, String> {
    let started = Instant::now();
    let mut stages: Vec<(String, f64)> = Vec::new();
    let reporter = tracked.handle().map(|h| h.reporter());
    let post = |stage: &str, permille: u16, node: Option<String>| {
        if let Some(r) = &reporter {
            if let Some(n) = &node {
                r.set_machine(n.clone());
            }
            r.stage(stage.to_string(), None, (permille > 0).then(|| permille as f32 / 1000.0));
        }
        Cx::post_action(Img2MeshEvent::Progress { job, stage: stage.to_string(), permille, node })
    };
    post("reading the picture", 0, None);
    let (label, bytes) = read_picture(picture)?;
    let (png, size) = img2mesh::prepare_image(&bytes)?;
    stages.push((format!("picture {}x{}", size[0], size[1]), secs(started)));
    let b64 = String::from_utf8(makepad_base64::base64_encode(&png, &makepad_base64::BASE64_STANDARD)).map_err(|e| e.to_string())?;
    let wire = GenerateRequestJson {
        model: img2mesh::MODEL.to_string(),
        input_b64: Some(b64),
        input_content_type: Some("image/png".into()),
        seed: request.seed,
        remesh_resolution: Some(request.detail),
        texture: Some(false),
        decimation_target: Some(request.face_target()),
        ..Default::default()
    };
    // The fleet's nodes are heard by their beacons (one every couple of seconds): the first
    // request of a run listens for them before the router picks one.
    post("looking for AI Hub nodes", 0, None);
    let discovery = makepad_ai_hub::discovery::start_listener();
    if let makepad_ai_hub::discovery::Discovery::Unavailable { reason } = &discovery {
        return Err(unavailable(reason, &discovery));
    }
    let listening = Instant::now();
    let first_listen = discovery.nodes().is_empty();
    while discovery.nodes().is_empty() || (first_listen && listening.elapsed() < HEAR_FLEET) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if listening.elapsed() > HEAR_FLEET * 3 {
            return Err(unavailable("No AI Hub beacon was heard on the local network", &discovery));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    stages.push(("looking for AI Hub nodes".into(), secs(listening)));
    post("finding a node with the model", 0, None);
    let submitted = Instant::now();
    let mut gen = GenJob::new(Arc::new(FleetRouter { installed_only: true }), process_origin("fab"));
    gen.start(Domain::Mesh, wire, Want::Outputs { media: 1, text: 0 }).map_err(|e| unavailable(&e, &discovery))?;
    let mut stage = ("waiting for the node".to_string(), Instant::now());
    let output = loop {
        // A cancel goes to the node as the freeing cancel (the model leaves its GPU), through
        // the board's tracker, which also puts the node and the hub's stages on the strip.
        if cancel.load(Ordering::Relaxed) && !tracked.is_cancelled() {
            gen.cancel_freeing();
            return Err("cancelled".into());
        }
        match tracked.poll(&mut gen) {
            JobPoll::Pending => std::thread::sleep(Duration::from_millis(400)),
            JobPoll::Progress { permille, stage: now } => {
                // the hub's stages carry their own counters ("load dino block 3/24"): time the stage
                let key = now.split(|c: char| c.is_ascii_digit()).next().unwrap_or(&now).trim().to_string();
                if key != stage.0 {
                    let took = secs(stage.1);
                    stages.push((std::mem::replace(&mut stage, (key, Instant::now())).0, took));
                }
                post(&now, permille, gen.node());
                std::thread::sleep(Duration::from_millis(400));
            }
            JobPoll::Delta(_) | JobPoll::Chunk(_) => {}
            JobPoll::Done(_) if cancel.load(Ordering::Relaxed) => {
                gen.cancel_freeing();
                return Err("cancelled".into());
            }
            JobPoll::Done(output) => break output,
            JobPoll::Failed(_) if cancel.load(Ordering::Relaxed) => return Err("cancelled".into()),
            JobPoll::Failed(error) => {
                gen.cancel();
                let node = gen.node().map(|n| format!(" on {n}")).unwrap_or_default();
                return Err(format!("The AI Hub's image-to-3D job failed{node}: {error}"));
            }
        }
    };
    stages.push((stage.0, secs(stage.1)));
    let hub_s = secs(submitted);
    let node = makepad_ai_hub::fleet::node_label(&output.node);
    post("reading the mesh", 1000, Some(node.clone()));
    let meshed = Instant::now();
    let artifact = output.artifacts.into_iter().next().ok_or("the hub answered without a mesh")?;
    if !artifact.content_type.starts_with("model/gltf-binary") {
        return Err(format!("the hub answered with {}, not a GLB mesh", artifact.content_type));
    }
    let mesh = img2mesh::mesh_from_glb(&artifact.bytes)?;
    let fitted = img2mesh::fit(&mesh, request.size_mm)?;
    if let Some(why) = img2mesh::flat_sheet(&fitted) {
        return Err(format!("{why} (made on {node}, model {})", output.model));
    }
    stages.push(("mesh: weld, size, stand".into(), secs(meshed)));
    let (fitted, printable) = make_printable(fitted, request, &mut stages, |stage| post(stage, 0, Some(node.clone())));
    stages.push((format!("hub total ({} KB GLB)", artifact.bytes.len() / 1024), hub_s));
    log!("fab image to mesh: {label} on {node}, {} triangles: {}", fitted.mesh.triangles.len(), stages.iter().map(|(s, t)| format!("{s} {t:.1}s")).collect::<Vec<_>>().join(", "));
    let name = request.name.clone().unwrap_or_else(|| img2mesh::ident(label.rsplit_once('.').map_or(label.as_str(), |(stem, _)| stem)));
    Ok(Made { name, fitted, printable, from: Provenance { image: label, model: output.model }, node, stages })
}
