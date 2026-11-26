//! Board view bench: a generated 100 000 segment board, the LED controller fixture, or a KiCad file.
//!
//! Flags: `--segments N` (100000), `--layers N` (8; 2, 4, 6 or 8), `--seed N` (1),
//! `--fixture`, `--light`, `--autopan` (continuous pan/zoom), `--direct` (no layer
//! compositing), `--frames N` (quit after N drawn frames; use with `--autopan`), and `--edit
//! move|route|fill` (once the fills are in, every 2 s: move one placed part 0.5 mm and back, or commit
//! 300 track segments in 30 tracks and remove them again, or move a part and refill the zones it
//! reaches; each edit's board and fills are made on the pool, and the pane's edit-to-picture time is
//! printed as a `boardbench picture` line), and `--art` (the top surface's layer art, drawn on the GPU
//! from the shared drawing, in the bottom-right corner), and `--unrouted` (no tracks or pours:
//! every connection is a ratsnest line). The bench is the pane's host: a drag's previews apply on the
//! pool, a drop commits, and the ratsnest is the connectivity of the board on screen.
//! `--drc` checks each connected board on the pool and draws its findings over the board.
//! `--heat-test` generates a synthetic temperature field on the pool for the board on screen.
//! `--route` streams previews from the route worker for all open nets, then commits its last proposal;
//! `--organic` makes that job the `@organic` method (the grid route relaxed into curves).
//! `--fixture --unplace N` leaves the first N fixture parts in the tray for drag placement.
//! `--fixture --board NAME` shows another stock fixture's hand-placed board (`handheld`, `tonebox`).
//! `--fixture --seed` places its parts afresh with the constructive placer (`place::seed`) from the
//! circuit alone, and prints a `boardbench seed` line: its time, parts left without room and its
//! wirelength against the hand placement's.
//! `--kicad <path>` loads a board on the pool while an empty board is on screen, with import timings
//! and its report; this takes precedence over generation and fixture flags.
//! `--probe NAME` enables the probe chip and logs probes addressed to that bench.
//! Prints one `boardbench` timing line per second and shows it at the top left.
//! Board construction has its own timing line; a frame limit adds a final run summary.
//! `boardbench first_frame` uses the pane's installation-to-picture timing for each new picture.

pub use makepad_widgets;

mod art_preview;

use fab_elec_board::{
    connect, drc, resolve_rules, RuleSource, Anchor, Board, BoardDelta, BoardOp, ConnItem, Connectivity, DrcResult, OpError,
    Origin, RuleMatrix, Track, ZoneFill, ARC_ERR,
};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{mm, Angle, BoardId, LayerId, NetId, Path, Sel, Side, P};
use fab_elec_design::{BenchOp, Design, DesignOp, PartRef};
use fab_elec_kicad::{read_kicad_board, KicadSources, KicadRuleSource, Refusal};
use fab_elec_route::{
    route_channel, Base, Effort, Engine, JobId, Method, ProposalId, ProposalSource, RouteEvent, RouteHandle,
    RouteJob, RouteProposal, RouteRequest, RouteScope, RouteWorker, Spawn,
};
use fab_elec_sim::heat::HeatField;
use fab_elec_view::board::{
    gen::{generated_board, GenSpec},
    rail::{BoardLayerRail, RailAction},
    findings::BoardChecks,
    plan::placement_labels,
    tray::{placement_op, unplaced, BoardTray},
    BoardDraws, BoardPane, BoardStats,
};
use fab_elec_view::ViewAction;
use fab_elec_view::live::probe::probe_drag_item;
use makepad_widgets::*;
use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Instant};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.inner_size: vec2(1600, 1000) pass.clear_color: #x14161a
                body +: {
                    flow: Down
                    status := Label{
                        width: Fill height: Fit
                        margin: Inset{left: 8 top: 8}
                        draw_text +: {color: #x8c95aa text_style +: {font_size: 9}}
                        text: ""
                    }
                    content := View{
                        width: Fill height: Fill flow: Right
                        sidebar := ScrollYView{
                            width: 220 height: Fill flow: Down
                            probe_chip := RoundedView{
                                visible: false
                                width: Fill height: Fit flow: Down spacing: 2
                                margin: 6
                                padding: Inset{top: 6 bottom: 6 left: 10 right: 10}
                                cursor: MouseCursor.Hand
                                capture_overload: true
                                grab_key_focus: false
                                show_bg: true
                                draw_bg +: {color: #x1e2128 border_radius: 6 material: 0.0}
                                Label{
                                    width: Fill height: Fit padding: 0
                                    draw_text +: {color: #x8c95aa text_style +: {font_size: 10}}
                                    text: "Probe"
                                }
                                Label{
                                    width: Fill height: Fit padding: 0
                                    draw_text +: {color: #x697285 text_style +: {font_size: 8}}
                                    text: "drag onto copper, or press P over it"
                                }
                            }
                            rail := mod.widgets.BoardLayerRail{width: Fill height: Fit}
                            tray := mod.widgets.BoardTray{width: Fill height: Fit}
                            checks := mod.widgets.BoardChecks{width: Fill height: 320}
                        }
                        View{
                            width: Fill height: Fill flow: Overlay align: Align{x: 1.0 y: 1.0}
                            board := mod.widgets.BoardPane{width: Fill height: Fill}
                            art := mod.widgets.ArtPreview{}
                        }
                    }
                }
            }
        }
    }
}

fn number<T: std::str::FromStr>(args: &[String], flag: &str) -> Option<T> {
    let index = args.iter().position(|arg| arg == flag)?;
    match args.get(index + 1) {
        Some(value) => match value.parse() {
            Ok(value) => Some(value),
            Err(_) => {
                eprintln!("boardbench invalid {flag} value {value:?}; keeping default");
                None
            }
        },
        None => {
            eprintln!("boardbench missing {flag} value; keeping default");
            None
        }
    }
}

struct FilledBoard {
    /// The board and resolved rules used by this fill job, also the route base's rules.
    board: Arc<Board>,
    rules: Arc<RuleMatrix>,
    fills: Vec<ZoneFill>,
    ms: f64,
    polys: usize,
}

struct KicadBoard {
    rule_source: Option<Arc<KicadRuleSource>>,
    board: Arc<Board>,
    line: String,
    report: String,
}

fn load_kicad(path: &std::path::Path, splash: bool) -> Result<KicadBoard, Refusal> {
    let start = Instant::now();
    let bytes = std::fs::read(path).map_err(|e| Refusal::new((0, 0), e.to_string()))?;
    let companion = |extension: &str| -> Option<Vec<u8>> {
        match std::fs::read(path.with_extension(extension)) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => { eprintln!("boardbench cannot read companion {extension}: {e}"); None },
        }
    };
    let project = companion("kicad_pro");
    let rules = companion("kicad_dru");
    let read_ms = start.elapsed().as_secs_f64() * 1000.0;
    let name = path.file_stem().unwrap_or_default().to_string_lossy();
    let pcb = read_kicad_board(&KicadSources { board: &bytes, project: project.as_deref(), rules: rules.as_deref(), name: &name })?;
    let parse_ms = pcb.parse_time.as_secs_f64() * 1000.0;
    let build_ms = pcb.build_time.as_secs_f64() * 1000.0;
    let project = if pcb.report.gaps.contains_key("project.refused") { "refused" } else if project.is_some() { "yes" } else { "no" };
    let board = &pcb.board;
    let pads: usize = board.placed.iter().map(|placed| placed.footprint.pads.len()).sum();
    let segs: usize = board.tracks.iter().map(|track| track.path.segs.len()).sum();
    let mut line = format!(
        "boardbench kicad bytes={} project={project} read_ms={read_ms:.3} parse_ms={parse_ms:.3} build_ms={build_ms:.3} placed={} pads={pads} segs={segs} vias={} zones={} gaps={} unknown={}",
        bytes.len(), board.placed.len(), board.vias.len(), board.zones.len(),
        pcb.report.gaps.len(), pcb.report.unknown.len(),
    );
    let report = pcb.report.render();
    let board = if splash {
        let start = Instant::now();
        let project = fab_elec_kicad::splash::to_splash(&pcb, &name).map_err(|e| Refusal::new((0, 0), e))?;
        let convert_ms = start.elapsed().as_secs_f64() * 1000.0;
        let bytes: usize = project.docs.iter().map(|(_, text)| text.len()).sum();
        let mut sources = fab_elec_lang::Sources::new();
        for (name, text) in project.docs {
            sources.add(name, text).map_err(|d| Refusal::new((0, 0), d.message))?;
        }
        let libs = fab_elec_lang::std_libs();
        let diagnostic = |diags: &[fab_elec_core::Diag]| {
            let messages: Vec<_> = diags.iter().take(5).map(|diag| {
                let locations: Vec<_> = diag.sel.iter().filter_map(|s| match s {
                    fab_elec_core::Sel::Source { doc, start, end } => Some(sources.describe(&libs, &fab_elec_lang::SourceRange { doc: *doc, span: *start..*end })),
                    _ => None,
                }).collect();
                format!("{}: {}", if locations.is_empty() { "project".into() } else { locations.join(", ") }, diag.message)
            }).collect();
            Refusal::new((0, 0), messages.join("\n"))
        };
        let start = Instant::now();
        let evaluated = fab_elec_lang::evaluate(&sources, &libs).map_err(|d| diagnostic(&d))?;
        let eval_ms = start.elapsed().as_secs_f64() * 1000.0;
        if let Some(unit) = evaluated.units.first() {
            if !unit.state.diags().is_empty() { return Err(diagnostic(unit.state.diags())); }
        }
        let board = evaluated.design.units.first().and_then(|u| u.board.clone())
            .ok_or_else(|| Refusal::new((0, 0), "Splash evaluation produced no board"))?;
        line.push_str(&format!(" splash_convert_ms={convert_ms:.3} splash_eval_ms={eval_ms:.3} splash_bytes={bytes}"));
        board
    } else { Arc::new(pcb.board) };
    Ok(KicadBoard { board, line, report, rule_source: if splash { None } else { pcb.rules.map(Arc::new) } })
}

/// A test source for the pane's heat slot, not a thermal solve. Values run from min_y up.
fn test_heat(board: &Board) -> Option<(HeatField, f64)> {
    let start = Instant::now();
    let bbox = board.outline_bbox();
    if bbox.is_empty() {
        return None;
    }
    let bounds_mm = [
        bbox.min.x as f64 / 1e6, bbox.min.y as f64 / 1e6,
        bbox.max.x as f64 / 1e6, bbox.max.y as f64 / 1e6,
    ];
    let width = bounds_mm[2] - bounds_mm[0];
    let height = bounds_mm[3] - bounds_mm[1];
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let cell = (width.max(height) / 256.0).max(0.5);
    let w = (width / cell).ceil() as u32;
    let h = (height / cell).ceil() as u32;
    // Fit the cells to the exact bounds used by the pane to sample this grid.
    let dx = width / w as f64;
    let dy = height / h as f64;
    let mut values = vec![25.0_f32; w as usize * h as usize];
    let outline = board.outline_polyset(ARC_ERR);
    for j in 0..h as usize {
        let y = bounds_mm[1] + (j as f64 + 0.5) * dy;
        for i in 0..w as usize {
            let x = bounds_mm[0] + (i as f64 + 0.5) * dx;
            if !outline.contains(P::mm(x, y)) {
                values[j * w as usize + i] = f32::NAN;
            }
        }
    }
    for part in &board.placed {
        let a = 6.0 * (part.footprint.pads.len() as f64).sqrt();
        if a == 0.0 {
            continue;
        }
        let bbox = part.bbox();
        let x = (bbox.min.x as f64 + bbox.max.x as f64) / 2e6;
        let y = (bbox.min.y as f64 + bbox.max.y as f64) / 2e6;
        let half_width = (bbox.max.x as f64 - bbox.min.x as f64) / 2e6;
        let half_height = (bbox.max.y as f64 - bbox.min.y as f64) / 2e6;
        let s = 1.5 + 0.6 * half_width.max(half_height);
        let radius = 3.0 * s;
        let radius_sq = radius * radius;
        let denom = 2.0 * s * s;
        let i0 = ((x - radius - bounds_mm[0]) / dx).floor().clamp(0.0, w as f64) as usize;
        let i1 = ((x + radius - bounds_mm[0]) / dx).ceil().clamp(0.0, w as f64) as usize;
        let j0 = ((y - radius - bounds_mm[1]) / dy).floor().clamp(0.0, h as f64) as usize;
        let j1 = ((y + radius - bounds_mm[1]) / dy).ceil().clamp(0.0, h as f64) as usize;
        for j in j0..j1 {
            let ry = bounds_mm[1] + (j as f64 + 0.5) * dy - y;
            for i in i0..i1 {
                let rx = bounds_mm[0] + (i as f64 + 0.5) * dx - x;
                let d_sq = rx * rx + ry * ry;
                if d_sq <= radius_sq {
                    values[j * w as usize + i] += (a * (-d_sq / denom).exp()) as f32;
                }
            }
        }
    }
    let [_, max] = HeatField::span(&values)?;
    let field = HeatField {
        w, h, bounds_mm, t: 0.0, generation: 0, values,
        range: [25.0, ((max / 5.0).ceil() * 5.0).max(30.0)],
        label: "Test heat field (synthetic)".into(),
        unit: "°C".into(),
    };
    Some((field, start.elapsed().as_secs_f64() * 1000.0))
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    timer: Timer,
    #[rust]
    frame_limit: Option<u64>,
    #[rust]
    frame_base: u64,
    #[rust]
    first_frames_seen: u64,
    #[rust]
    pending_kicad: Option<PathBuf>,
    #[rust]
    kicad_splash: bool,
    #[rust]
    kicad_job: Option<TaskHandle<Result<KicadBoard, Refusal>>>,
    #[rust]
    kicad_first_frame: bool,
    #[rust]
    rule_source: Option<Arc<KicadRuleSource>>,
    #[rust]
    art: bool,
    #[rust]
    reported_frames: u64,
    #[rust]
    draw_us_total: f64,
    #[rust]
    draw_us_max: f64,
    #[rust]
    pending_fills: Option<Arc<Board>>,
    #[rust]
    fill_job: Option<TaskHandle<FilledBoard>>,
    #[rust]
    heat_test: bool,
    #[rust]
    pending_heat: Option<Arc<Board>>,
    #[rust]
    heat_job: Option<TaskHandle<Option<(HeatField, f64)>>>,
    /// Keep the pane's field alive until replacement so its final drop can run on the pool.
    #[rust]
    heat: Option<Arc<HeatField>>,
    /// The board outside a preview, and the fills shared by committed and preview boards.
    #[rust]
    committed: Option<(Arc<Board>, Arc<Vec<ZoneFill>>)>,
    /// The fixture's committed design, shared with the pane for tray placement ops.
    #[rust]
    design: Option<Arc<Design>>,
    #[rust]
    tray_parts: usize,
    #[rust]
    preview: Option<Arc<Board>>,
    #[rust]
    want: Option<Vec<BoardOp>>,
    #[rust]
    preview_job: Option<TaskHandle<PreviewDone>>,
    #[rust]
    commit_after: bool,
    /// Cancel leaves the job running, but its result must never be drawn.
    #[rust]
    discard_preview: bool,
    #[rust]
    conn: Option<Arc<Connectivity>>,
    /// The pours of the preview board on screen, filled on the pool like the committed board's
    /// (latest wins), so a preview over a pour shows the drop's clearances and true findings.
    #[rust]
    preview_fills: Option<(Arc<Board>, Arc<Vec<ZoneFill>>)>,
    /// The exact inputs of `conn`; initial connectivity without fills is not a route base.
    #[rust]
    conn_inputs: Option<(Arc<Board>, Arc<Vec<ZoneFill>>)>,
    #[rust]
    conn_want: Option<(Arc<Board>, Arc<Vec<ZoneFill>>)>,
    /// The newest fill result and the board it was made for, to identify checks over held pours.
    #[rust]
    filled: Option<(Arc<Board>, Arc<Vec<ZoneFill>>)>,
    #[rust]
    conn_job: Option<TaskHandle<ConnDone>>,
    #[rust]
    drc: bool,
    #[rust]
    probe_bench: Option<String>,
    #[rust]
    probe_press: Option<DVec2>,
    /// Rules resolved with the fills, retained with their board identity.
    #[rust]
    rules: Option<(Arc<Board>, Arc<RuleMatrix>)>,
    /// The single route worker and its one startup job, when `--route` is enabled.
    #[rust]
    route: Option<Routing>,
    /// Replaced boards, fills and results may own the last reference: free them on the pool.
    #[rust]
    discarded: Vec<Box<dyn Send>>,
    #[rust]
    edit: Option<EditKind>,
    #[rust]
    edit_timer: Timer,
    #[rust]
    edit_step: u64,
    #[rust]
    edit_job: Option<TaskHandle<EditDone>>,
    #[rust]
    pictures_seen: u64,
}

#[derive(Clone, Copy, Debug)]
enum EditKind {
    Move,
    Route,
    Fill,
}

/// An edit made on the pool: the board, and for `Fill` the refilled zones with their time.
struct EditDone {
    board: Board,
    ms: f64,
    fills: Option<(Vec<ZoneFill>, f64)>,
}

struct PreviewDone {
    board: Result<Arc<Board>, OpError>,
    ops: usize,
    ms: f64,
}

struct ConnDone {
    /// Inputs retained so stale connectivity can be retired without replacing the preview's.
    board: Arc<Board>,
    fills: Arc<Vec<ZoneFill>>,
    conn: Connectivity,
    ms: f64,
    drc: Option<(DrcResult, f64)>,
    held: bool,
}

/// With held pours every zone of `board` is pending: the sorted ids of the findings that name a
/// zone, and of the shorts whose cluster runs through a zone's fill (the held fill may be what joins
/// their nets); otherwise empty. O(findings x their items x log zones + shorts x their clusters).
fn awaiting_refill(board: &Board, conn: &Connectivity, violations: &[Violation], held: bool) -> Vec<u64> {
    if !held || board.zones.is_empty() {
        return Vec::new();
    }
    let mut zones: Vec<_> = board.zones.iter().map(|zone| zone.id).collect();
    zones.sort_unstable();
    // A short's finding carries its touching pair, not its cluster: match it by where it touches.
    let poured: Vec<P> = conn.shorts.iter()
        .filter(|short| conn.clusters.get(short.cluster as usize)
            .is_some_and(|cluster| cluster.items.iter().any(|item| matches!(item, ConnItem::Zone { .. }))))
        .filter_map(|short| Some(short.witness?.at))
        .collect();
    let mut awaiting: Vec<_> = violations.iter().filter(|violation| {
        violation.items.iter().any(|sel| {
            matches!(sel, Sel::Item(unit, item) if *unit == board.unit && zones.binary_search(item).is_ok())
        }) || violation.kind == CheckKind::Short && violation.at.is_some_and(|at| poured.contains(&at))
    }).map(|violation| violation.id).collect();
    awaiting.sort_unstable();
    awaiting
}

/// Dispatch route jobs on the application's existing pool, without a thread per job.
struct RouteSpawn(TaskPool);

impl Spawn for RouteSpawn {
    /// Detached handles leave jobs running; a closed pool cannot accept further work.
    fn spawn(&self, task: Box<dyn FnOnce() + Send>) {
        match self.0.submit(Class::Background, task) {
            Ok(handle) => handle.detach(),
            Err(e) => eprintln!("boardbench route job was not queued ({e:?})"),
        }
    }
}

/// One route run, retaining rejected requests and only the newest complete proposal.
struct Routing {
    handle: RouteHandle,
    method: Method,
    requests: VecDeque<RouteRequest>,
    started: Option<Instant>,
    done: bool,
    newest: Option<Arc<RouteProposal>>,
    shown: Option<ProposalId>,
    applying: Option<TaskHandle<RouteApplied>>,
    /// Unrouted nets when the job started, and now.
    nets: usize,
    open: usize,
    findings: usize,
}

/// An unchecked proposal applied to the committed base on the pool.
struct RouteApplied {
    proposal: Arc<RouteProposal>,
    board: Result<Arc<Board>, OpError>,
}

/// Edit `b` in place: step `n` of the move or route loop (even steps change, odd steps undo).
fn apply_edit(b: &mut Board, kind: EditKind, n: u64) {
    b.generation += 1;
    match kind {
        EditKind::Move | EditKind::Fill => {
            if b.placed.is_empty() {
                return;
            }
            let i = (n / 2) as usize % b.placed.len();
            let d = if n % 2 == 0 { mm(0.5) } else { -mm(0.5) };
            let p = &mut b.placed[i];
            p.at = P::new(p.at.x + d, p.at.y);
        }
        EditKind::Route if n % 2 == 0 => {
            let c = b.outline_bbox().center();
            for i in 0..30 {
                let y = c.y + mm(0.3) * i;
                let mut path = Path::new(P::new(c.x, y));
                for j in 1..=10 {
                    path = path.line(P::new(c.x + mm(1.0) * j, y + if j % 2 == 0 { 0 } else { mm(0.1) }));
                }
                let id = b.alloc_id();
                b.tracks.push(Track {
                    id, net: NetId(0), layer: LayerId::F_CU, w: mm(0.1), widths: None, path,
                    ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
                });
            }
        }
        EditKind::Route => {
            let keep = b.tracks.len().saturating_sub(30);
            b.tracks.truncate(keep);
        }
    }
}

impl App {
    fn start_kicad(&mut self, cx: &mut Cx) {
        // Let the placeholder draw before a fast import can replace it.
        if self.kicad_job.is_some() || self.first_frames_seen == 0 {
            return;
        }
        let Some(path) = self.pending_kicad.clone() else { return };
        let splash = self.kicad_splash;
        match cx.task_pool().submit(Class::Visible, move || load_kicad(&path, splash)) {
            Ok(handle) => {
                self.kicad_job = Some(handle);
                self.pending_kicad = None;
            }
            Err(e) => {
                eprintln!("boardbench kicad was not queued ({e:?}); retrying");
                self.ui.redraw(cx);
            }
        }
    }

    fn take_kicad(&mut self, cx: &mut Cx) {
        let Some(job) = self.kicad_job.as_mut() else { return };
        let Some(result) = job.try_take() else { return };
        self.kicad_job = None;
        match result {
            Ok(Ok(done)) => {
                println!("{}", done.line);
                for line in done.report.lines() {
                    println!("  {line}");
                }
                self.kicad_first_frame = true;
                self.rule_source = done.rule_source;
                let placeholder = self.committed.as_ref().map(|(board, _)| board.unit);
                self.install_board(cx, done.board);
                if let Some(unit) = placeholder {
                    BoardDraws::remove(cx, unit);
                }
                self.discarded.push(Box::new((done.line, done.report)));
            }
            Ok(Err(e)) => println!("boardbench kicad refused at {}:{}: {}", e.line, e.column, e.message),
            Err(e) => eprintln!("boardbench kicad job failed: {e:?}"),
        }
    }

    /// Generated and imported boards enter the same drawing and engine queues.
    fn install_board(&mut self, cx: &mut Cx, board: Arc<Board>) {
        let unit = board.unit;
        self.pending_fills = Some(board.clone());
        self.set_board(cx, board, Arc::new(Vec::new()));
        if self.art {
            if let Some(mut art) = self.ui.widget(cx, ids!(art)).borrow_mut::<art_preview::ArtPreview>() {
                art.set_unit(cx, Some(unit));
            }
        }
        self.refresh_rail(cx);
        self.start_fills(cx);
    }

    fn refresh_rail(&self, cx: &mut Cx) {
        let layers = self.ui.widget(cx, ids!(board)).borrow::<BoardPane>().map(|pane| pane.layers());
        if let Some(layers) = layers {
            if let Some(mut rail) = self.ui.widget(cx, ids!(rail)).borrow_mut::<BoardLayerRail>() {
                rail.set_layers(cx, &layers);
            }
        }
    }

    fn start_fills(&mut self, cx: &mut Cx) {
        if self.fill_job.is_some() {
            return;
        }
        let Some(board) = self.pending_fills.clone() else { return };
        let rule_source = self.rule_source.clone();
        match cx.task_pool().submit(Class::Visible, move || {
            let start = Instant::now();
            let rules = Arc::new(resolve_rules(&board, rule_source.as_deref().map(|s| s as &dyn RuleSource)));
            let fills = fab_elec_board::zone::fill_all(&board, &rules);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let polys = fills.iter().flat_map(|fill| &fill.layers)
                .flat_map(|layer| layer.kept()).map(|(_, poly)| poly.rings().count()).sum();
            // The pool signals the UI once the result is stored (`Event::Signal` takes it).
            FilledBoard { board, rules, fills, ms, polys }
        }) {
            Ok(handle) => {
                self.fill_job = Some(handle);
                if let Some(board) = self.pending_fills.take() {
                    self.discarded.push(Box::new(board));
                }
            }
            Err(e) => {
                eprintln!("boardbench fills were not queued ({e:?}); retrying");
                self.ui.redraw(cx);
            }
        }
    }

    fn start_heat(&mut self, cx: &mut Cx) {
        if self.heat_job.is_some() {
            return;
        }
        let Some(board) = self.pending_heat.clone() else { return };
        match cx.task_pool().submit(Class::Visible, move || test_heat(&board)) {
            Ok(handle) => {
                self.heat_job = Some(handle);
                if let Some(board) = self.pending_heat.take() {
                    self.discarded.push(Box::new(board));
                }
            }
            Err(e) => {
                eprintln!("boardbench heat-test was not queued ({e:?}); retrying");
                self.ui.redraw(cx);
            }
        }
    }

    fn take_heat(&mut self, cx: &mut Cx) {
        let Some(job) = self.heat_job.as_mut() else { return };
        let Some(result) = job.try_take() else { return };
        self.heat_job = None;
        match result {
            Ok(Some((field, ms))) => {
                if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
                    let (w, h) = (field.w, field.h);
                    let max = HeatField::span(&field.values).map_or(25.0, |[_, max]| max);
                    let field = Arc::new(field);
                    pane.set_heat(cx, Some(field.clone()));
                    if let Some(old) = self.heat.replace(field) {
                        self.discarded.push(Box::new(old));
                    }
                    println!("boardbench heat-test w={w} h={h} max={max:.1} ms={ms:.3}");
                    log!("boardbench heat-test w={w} h={h} max={max:.1} ms={ms:.3}");
                } else {
                    self.discarded.push(Box::new(field));
                }
            }
            Ok(None) => {
                if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
                    pane.set_heat(cx, None);
                }
                if let Some(old) = self.heat.take() {
                    self.discarded.push(Box::new(old));
                }
            }
            Err(e) => eprintln!("boardbench heat-test failed: {e:?}"),
        }
        // Even an older result is useful; compute the newest pending board next.
        self.start_heat(cx);
    }

    /// The board on screen and its pours: the preview with its own fills once they are in, else
    /// with the committed fills; without a preview the committed board.
    fn shown(&self) -> Option<(Arc<Board>, Arc<Vec<ZoneFill>>)> {
        let (board, fills) = self.committed.clone()?;
        let Some(preview) = self.preview.clone() else { return Some((board, fills)) };
        match &self.preview_fills {
            Some((filled, fills)) if Arc::ptr_eq(filled, &preview) => Some((preview, fills.clone())),
            _ => Some((preview, fills)),
        }
    }

    /// Show a new preview board: its pours are refilled on the pool (the newest request wins).
    fn show_preview(&mut self, cx: &mut Cx, board: Arc<Board>) {
        if let Some(old) = self.preview.replace(board.clone()) {
            self.discarded.push(Box::new(old));
        }
        if let Some(old) = self.preview_fills.take() {
            self.discarded.push(Box::new(old));
        }
        let fills = self.committed.as_ref().unwrap().1.clone();
        self.draw_board(cx, board.clone(), fills);
        if let Some(old) = self.pending_fills.replace(board) {
            self.discarded.push(Box::new(old));
        }
    }

    /// The bench is the board's host: it hands the board to the shared drawing and points the pane
    /// at its unit (views only read `BoardDraws::current`).
    fn set_board(&mut self, cx: &mut Cx, board: Arc<Board>, fills: Arc<Vec<ZoneFill>>) {
        let unit = board.unit;
        if let Some(old) = self.committed.replace((board.clone(), fills.clone())) {
            self.discarded.push(Box::new(old));
        }
        self.sync_design(cx, &board);
        if let Some((board, fills)) = self.shown() {
            self.draw_board(cx, board, fills);
        }
        if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
            pane.set_unit(cx, Some(unit));
        } else {
            eprintln!("boardbench BoardPane is unavailable");
        }
        if let Some(mut checks) = self.ui.widget(cx, ids!(checks)).borrow_mut::<BoardChecks>() {
            checks.set_unit(cx, Some(unit));
        }
    }

    /// Keep placement ops and the tray on the committed board, including automatic edits.
    fn sync_design(&mut self, cx: &mut Cx, board: &Arc<Board>) {
        let Some(design) = &self.design else { return };
        let Some(unit) = design.unit(board.unit) else { return };
        if unit.board.as_ref().is_some_and(|old| Arc::ptr_eq(old, board)) {
            return;
        }
        let mut design = (**design).clone();
        let unit = design.units.iter_mut().find(|unit| unit.id == board.unit).unwrap();
        unit.board = Some(board.clone());
        let design = Arc::new(design);
        let parts = unplaced(&design, board.unit);
        let n = parts.len();
        if let Some(old) = self.design.replace(design.clone()) {
            self.discarded.push(Box::new(old));
        }
        if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
            pane.set_design(cx, Some(design));
        }
        if let Some(mut tray) = self.ui.widget(cx, ids!(tray)).borrow_mut::<BoardTray>() {
            tray.set_parts(cx, parts);
        } else {
            self.discarded.push(Box::new(parts));
        }
        if n < self.tray_parts {
            println!("boardbench placed tray={n}");
        }
        self.tray_parts = n;
    }

    fn draw_board(&mut self, cx: &mut Cx, board: Arc<Board>, fills: Arc<Vec<ZoneFill>>) {
        BoardDraws::set_board(cx, board.clone(), fills.clone());
        if self.heat_test {
            if let Some(old) = self.pending_heat.replace(board.clone()) {
                self.discarded.push(Box::new(old));
            }
            self.start_heat(cx);
        }
        // A checked route preview already has connectivity and findings from the worker.
        let checked = self.route.as_ref().filter(|route| !route.done)
            .and_then(|route| route.newest.as_ref()).and_then(|p| p.checked.as_ref())
            .is_some_and(|checked| Arc::ptr_eq(&checked.board, &board));
        if checked {
            if let Some(old) = self.conn_want.take() {
                self.discarded.push(Box::new(old));
            }
            return;
        }
        if let Some(old) = self.conn_want.replace((board, fills)) {
            self.discarded.push(Box::new(old));
        }
        self.start_conn(cx);
    }

    fn start_preview(&mut self, cx: &mut Cx) {
        // An automatic edit already in flight finishes before this drag takes its base.
        if self.preview_job.is_some() || self.edit_job.is_some() {
            return;
        }
        let Some((board, _)) = self.committed.as_ref() else { return };
        let Some(ops) = self.want.clone() else { return };
        let board = board.clone();
        match cx.task_pool().submit(Class::Visible, move || {
            let start = Instant::now();
            let mut b = Board::clone(&board);
            let result = b.apply_all(&ops).map(|_| Arc::new(b));
            PreviewDone { board: result, ops: ops.len(), ms: start.elapsed().as_secs_f64() * 1000.0 }
        }) {
            Ok(handle) => {
                self.preview_job = Some(handle);
                self.discard_preview = false;
                if let Some(ops) = self.want.take() {
                    self.discarded.push(Box::new(ops));
                }
            }
            Err(e) => {
                eprintln!("boardbench preview was not queued ({e:?}); retrying");
                self.ui.redraw(cx);
            }
        }
    }

    fn take_preview(&mut self, cx: &mut Cx) {
        let Some(job) = self.preview_job.as_mut() else { return };
        let Some(result) = job.try_take() else { return };
        self.preview_job = None;
        let discard = std::mem::take(&mut self.discard_preview);
        match result {
            Ok(done) => {
                println!("boardbench preview ops={} apply_ms={:.3}", done.ops, done.ms);
                match done.board {
                    Ok(board) if !discard => self.show_preview(cx, board),
                    Ok(board) => self.discarded.push(Box::new(board)),
                    Err(e) => eprintln!("boardbench preview failed: {e:?}"),
                }
            }
            Err(e) => eprintln!("boardbench preview job failed: {e:?}"),
        }
        self.start_preview(cx);
        // A drop includes the newest queued drag sample, even if an older one was running.
        if self.commit_after && self.preview_job.is_none() && self.want.is_none() {
            self.commit_preview(cx);
        }
    }

    fn commit_preview(&mut self, cx: &mut Cx) {
        if self.preview_job.is_some() || self.want.is_some() {
            self.commit_after = true;
            self.start_preview(cx);
            return;
        }
        self.commit_after = false;
        if let Some(board) = self.preview.take() {
            match self.preview_fills.take() {
                // The preview's pours are already in: the drop shows them at once.
                Some((filled, fills)) if Arc::ptr_eq(&filled, &board) => self.set_board(cx, board, fills),
                stale => {
                    if let Some(old) = stale {
                        self.discarded.push(Box::new(old));
                    }
                    let fills = self.committed.as_ref().unwrap().1.clone();
                    self.set_board(cx, board.clone(), fills);
                    // The pours follow the committed board (on the pool), as a host refills after an edit.
                    if let Some(old) = self.pending_fills.replace(board) {
                        self.discarded.push(Box::new(old));
                    }
                }
            }
        }
        println!("boardbench commit");
    }

    fn cancel_preview(&mut self, cx: &mut Cx) {
        if let Some(ops) = self.want.take() {
            self.discarded.push(Box::new(ops));
        }
        self.commit_after = false;
        self.discard_preview = self.preview_job.is_some();
        if let Some(board) = self.preview.take() {
            self.discarded.push(Box::new(board));
        }
        if let Some(old) = self.preview_fills.take() {
            self.discarded.push(Box::new(old));
        }
        if let Some((board, fills)) = self.committed.clone() {
            // Keep the original Arcs so BoardDraws can recognise an unchanged drawing.
            self.draw_board(cx, board, fills);
        }
        println!("boardbench cancel");
    }

    fn start_conn(&mut self, cx: &mut Cx) {
        if self.conn_job.is_some() {
            return;
        }
        let Some((board, fills)) = self.conn_want.clone() else { return };
        let check = self.drc;
        let rule_source = self.rule_source.clone();
        let held = !self.filled.as_ref().is_some_and(|(filled, made)| {
            Arc::ptr_eq(filled, &board) && Arc::ptr_eq(made, &fills)
        });
        match cx.task_pool().submit(Class::Visible, move || {
            let start = Instant::now();
            let conn = connect(&board, &fills);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let drc = check.then(|| {
                let start = Instant::now();
                let rules = resolve_rules(&board, rule_source.as_deref().map(|s| s as &dyn RuleSource));
                let result = drc(&board, &conn, &rules, &fills, None);
                (result, start.elapsed().as_secs_f64() * 1000.0)
            });
            ConnDone { board, fills, conn, ms, drc, held }
        }) {
            Ok(handle) => {
                self.conn_job = Some(handle);
                if let Some(old) = self.conn_want.take() {
                    self.discarded.push(Box::new(old));
                }
            }
            Err(e) => {
                eprintln!("boardbench conn was not queued ({e:?}); retrying");
                self.ui.redraw(cx);
            }
        }
    }

    fn take_conn(&mut self, cx: &mut Cx) {
        let Some(job) = self.conn_job.as_mut() else { return };
        let Some(result) = job.try_take() else { return };
        self.conn_job = None;
        match result {
            Ok(done) if !self.shown().is_some_and(|(board, fills)| {
                Arc::ptr_eq(&board, &done.board) && Arc::ptr_eq(&fills, &done.fills)
            }) => self.discarded.push(Box::new(done)),
            Ok(done) => {
                println!("boardbench conn ms={:.3} rats={}", done.ms, done.conn.ratsnest.len());
                if let Some(old) = self.conn_inputs.replace((done.board.clone(), done.fills)) {
                    self.discarded.push(Box::new(old));
                }
                let conn = Arc::new(done.conn);
                let awaiting = done.drc.as_ref()
                    .map(|(drc, _)| awaiting_refill(&done.board, &conn, &drc.violations, done.held));
                if let Some(old) = self.conn.replace(conn.clone()) {
                    self.discarded.push(Box::new(old));
                }
                if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
                    pane.set_conn(cx, Some(conn));
                }
                if let Some((drc, ms)) = done.drc {
                    let awaiting = awaiting.unwrap_or_default();
                    println!("boardbench drc ms={ms:.3} findings={} awaiting={}", drc.violations.len(), awaiting.len());
                    if let Some(route) = self.route.as_mut().filter(|route| route.newest.is_none()) {
                        route.findings = drc.violations.len();
                    }
                    self.show_findings(cx, Arc::new(drc.violations), awaiting);
                }
            }
            Err(e) => eprintln!("boardbench conn failed: {e:?}"),
        }
        self.start_conn(cx);
    }

    /// Queue the one all-net job only when fills, rules and connectivity share its base.
    fn start_route(&mut self, cx: &mut Cx) {
        let Some(route) = self.route.as_mut() else { return };
        if route.started.is_some() || self.preview.is_some() || self.fill_job.is_some()
            || self.pending_fills.is_some()
        {
            return;
        }
        let (Some((board, fills)), Some((ruled, rules)), Some((connected, conn_fills)), Some(conn)) =
            (&self.committed, &self.rules, &self.conn_inputs, &self.conn)
        else { return };
        if !Arc::ptr_eq(board, ruled) || !Arc::ptr_eq(board, connected) || !Arc::ptr_eq(fills, conn_fills) {
            return;
        }
        let unit = board.unit;
        route.requests.push_back(RouteRequest::Base(Base::new(board.clone(), rules.clone(), conn.clone(), fills.clone())));
        route.requests.push_back(RouteRequest::Route(RouteJob {
            job: JobId(1), unit, scope: RouteScope::All, method: route.method, engine: Engine::Grid,
            style: None, effort: Effort::default(), layers: None, reroute: false,
        }));
        route.started = Some(Instant::now());
        route.open = conn.unrouted.len();
        route.nets = route.open;
        println!("boardbench route start nets={}", route.open);
        self.status(cx);
    }

    /// Preserve request order and retry a full command queue on the next signal or timer.
    fn retry_route(&mut self) {
        let Some(route) = self.route.as_mut().filter(|route| !route.done) else { return };
        while let Some(request) = route.requests.pop_front() {
            if let Err(request) = route.handle.request(request) {
                route.requests.push_front(request);
                eprintln!("boardbench route queue full; retrying");
                break;
            }
        }
    }

    /// Drain every event, retire superseded payloads on the pool, and show the newest answer.
    fn take_route(&mut self, cx: &mut Cx) {
        let Some(route) = self.route.as_mut() else { return };
        let mut received = false;
        while let Some(event) = route.handle.try_recv() {
            match event {
                RouteEvent::Proposal(proposal) if !route.done
                    && matches!(proposal.source, ProposalSource::Job { job: JobId(1), .. })
                    && self.committed.as_ref().is_some_and(|(board, _)| {
                        proposal.unit == board.unit && proposal.base_generation == board.generation
                    }) =>
                {
                    let ms = route.started.map_or(0.0, |start| start.elapsed().as_secs_f64() * 1000.0);
                    println!(
                        "boardbench route proposal ops={} routed={} failed={} verified={} last={} ms={ms:.3}",
                        proposal.ops.len(), proposal.metrics.nets_routed, proposal.open.len(),
                        proposal.verified, proposal.last,
                    );
                    // Open nets: the checked answer's own connectivity, else what has not routed yet.
                    route.open = match &proposal.checked {
                        Some(checked) => {
                            route.findings = checked.violations.len();
                            checked.conn.unrouted.len()
                        }
                        None if proposal.last => proposal.open.len(),
                        None => route.nets.saturating_sub(proposal.metrics.nets_routed as usize),
                    };
                    if let Some(old) = route.newest.replace(Arc::new(proposal)) {
                        self.discarded.push(Box::new(old));
                    }
                    received = true;
                }
                event => self.discarded.push(Box::new(event)),
            }
        }
        let applied = route.applying.as_mut().and_then(|job| job.try_take());
        if applied.is_some() {
            route.applying = None;
        }
        let newest = route.newest.clone();
        let active = !route.done;
        if received {
            self.status(cx);
        }
        match applied {
            Some(Ok(applied)) if active
                && newest.as_ref().is_some_and(|p| Arc::ptr_eq(p, &applied.proposal)) =>
            {
                match applied.board {
                    Ok(board) => self.show_route(cx, board, applied.proposal),
                    Err(e) => {
                        eprintln!("boardbench route application failed: {e:?}");
                        let route = self.route.as_mut().unwrap();
                        route.shown = Some(applied.proposal.id);
                        route.done = applied.proposal.last;
                        let last = applied.proposal.last;
                        self.discarded.push(Box::new(applied.proposal));
                        if last {
                            self.cancel_preview(cx);
                            self.status(cx);
                        }
                    }
                }
            }
            Some(Ok(applied)) => self.discarded.push(Box::new(applied)),
            Some(Err(e)) => eprintln!("boardbench route application job failed: {e:?}"),
            None => {}
        }
        let Some(route) = self.route.as_ref().filter(|route| !route.done) else { return };
        let Some(proposal) = route.newest.clone() else { return };
        if route.shown == Some(proposal.id) {
            return;
        }
        if let Some(checked) = &proposal.checked {
            self.show_route(cx, checked.board.clone(), proposal);
        } else {
            self.start_route_preview(cx);
        }
    }

    /// Apply unchecked ops on the pool against the committed board, never the last preview.
    fn start_route_preview(&mut self, cx: &mut Cx) {
        let Some(route) = self.route.as_mut().filter(|route| !route.done) else { return };
        if route.applying.is_some() {
            return;
        }
        let Some(proposal) = route.newest.clone() else { return };
        let Some((board, _)) = &self.committed else { return };
        let board = board.clone();
        match cx.task_pool().submit(Class::Visible, move || {
            let mut b = Board::clone(&board);
            let board = b.apply_all(&proposal.ops).map(|_| Arc::new(b));
            RouteApplied { proposal, board }
        }) {
            Ok(handle) => route.applying = Some(handle),
            Err(e) => eprintln!("boardbench route application was not queued ({e:?}); retrying"),
        }
    }

    /// Swap a route answer into the normal preview path; only the last answer commits.
    fn show_route(&mut self, cx: &mut Cx, board: Arc<Board>, proposal: Arc<RouteProposal>) {
        self.show_preview(cx, board.clone());
        let fills = self.committed.as_ref().unwrap().1.clone();
        if let Some(checked) = &proposal.checked {
            if let Some(old) = self.conn.replace(checked.conn.clone()) {
                self.discarded.push(Box::new(old));
            }
            if let Some(old) = self.conn_inputs.replace((board, fills)) {
                self.discarded.push(Box::new(old));
            }
            if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
                pane.set_conn(cx, Some(checked.conn.clone()));
            }
            if self.drc {
                self.show_findings(cx, checked.violations.clone(), Vec::new());
            }
        }
        let route = self.route.as_mut().unwrap();
        route.shown = Some(proposal.id);
        if proposal.last {
            route.done = true;
            let ms = route.started.unwrap().elapsed().as_secs_f64() * 1000.0;
            self.commit_preview(cx);
            println!("boardbench route done ms={ms:.3} open={}", proposal.open.len());
        }
        self.status(cx);
    }

    /// Hand one finding list to the Checks list and the canvas markers.
    fn show_findings(&mut self, cx: &mut Cx, violations: Arc<Vec<Violation>>, awaiting: Vec<u64>) {
        let awaiting = Arc::new(awaiting);
        if let Some(mut checks) = self.ui.widget(cx, ids!(checks)).borrow_mut::<BoardChecks>() {
            checks.set_violations(cx, Some(violations.clone()), awaiting.clone());
        }
        if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
            pane.set_violations(cx, Some(violations), awaiting);
        } else {
            self.discarded.push(Box::new(violations));
        }
    }

    /// Transfer large retired values to the pool to be dropped there.
    fn sweep(&mut self, cx: &mut Cx) {
        if self.discarded.is_empty() {
            return;
        }
        let discarded = std::mem::take(&mut self.discarded);
        match cx.task_pool().submit(Class::Background, move || drop(discarded)) {
            Ok(handle) => handle.detach(),
            // Only a closed pool refuses work: the app is shutting down.
            Err(e) => eprintln!("boardbench retired results were not queued for freeing ({e:?})"),
        }
    }

    /// Starts the next edit on the pool once the fills are in and the previous edit is drawn.
    fn start_edit(&mut self, cx: &mut Cx) {
        if self.route.as_ref().is_some_and(|route| !route.done) {
            return;
        }
        if self.preview.is_some() || self.want.is_some() || self.preview_job.is_some() || self.commit_after {
            return;
        }
        let (Some(kind), Some((board, fills))) = (self.edit, self.committed.clone()) else { return };
        if self.edit_job.is_some() || self.fill_job.is_some() || self.pending_fills.is_some() || fills.is_empty() {
            return;
        }
        let n = self.edit_step;
        let rule_source = self.rule_source.clone();
        match cx.task_pool().submit(Class::Visible, move || {
            let start = Instant::now();
            let mut b = Board::clone(&board);
            apply_edit(&mut b, kind, n);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let fills = matches!(kind, EditKind::Fill).then(|| {
                let start = Instant::now();
                let rules = resolve_rules(&b, rule_source.as_deref().map(|s| s as &dyn RuleSource));
                let delta = BoardDelta::between(&board, &b);
                let next = fills.iter().map(|f| fab_elec_board::zone::fill_update(&b, &rules, f, &delta)).collect();
                (next, start.elapsed().as_secs_f64() * 1000.0)
            });
            EditDone { board: b, ms, fills }
        }) {
            Ok(handle) => {
                self.edit_job = Some(handle);
                self.edit_step += 1;
            }
            Err(e) => eprintln!("boardbench edit was not queued ({e:?}); the next tick retries"),
        }
    }

    fn take_edit(&mut self, cx: &mut Cx) {
        let Some(job) = self.edit_job.as_mut() else { return };
        let Some(result) = job.try_take() else { return };
        self.edit_job = None;
        match result {
            Ok(done) => {
                let (fills, fill_ms) = match done.fills {
                    Some((fills, ms)) => (Arc::new(fills), ms),
                    None => (self.committed.as_ref().map(|c| c.1.clone()).unwrap_or_default(), 0.0),
                };
                println!("boardbench edit kind={:?} step={} board_ms={:.3} fill_ms={fill_ms:.3}",
                    self.edit.unwrap(), self.edit_step - 1, done.ms);
                self.set_board(cx, Arc::new(done.board), fills);
            }
            Err(e) => eprintln!("boardbench edit failed: {e:?}"),
        }
    }

    fn take_fills(&mut self, cx: &mut Cx) {
        let Some(job) = self.fill_job.as_mut() else { return };
        let Some(result) = job.try_take() else { return };
        self.fill_job = None;
        match result {
            Ok(filled) if self.preview.as_ref().is_some_and(|preview| Arc::ptr_eq(preview, &filled.board)) => {
                println!("boardbench preview fills ms={:.3} zones={}", filled.ms, filled.fills.len());
                let fills = Arc::new(filled.fills);
                if let Some(old) = self.preview_fills.replace((filled.board.clone(), fills.clone())) {
                    self.discarded.push(Box::new(old));
                }
                if let Some(old) = self.filled.replace((filled.board.clone(), fills.clone())) {
                    self.discarded.push(Box::new(old));
                }
                self.draw_board(cx, filled.board, fills);
                self.discarded.push(Box::new(filled.rules));
            }
            Ok(filled) if !self.committed.as_ref().is_some_and(|(board, _)| {
                Arc::ptr_eq(board, &filled.board)
            }) => {
                self.discarded.push(Box::new(filled));
                if self.pending_fills.is_none() {
                    self.pending_fills = self.preview.clone()
                        .or_else(|| self.committed.as_ref().map(|(board, _)| board.clone()));
                }
            }
            Ok(filled) => {
                println!("boardbench fills ms={:.3} zones={} polys={}",
                    filled.ms, filled.fills.len(), filled.polys);
                // Fills can arrive after a drag: retain its committed/preview board identity.
                let fills = Arc::new(filled.fills);
                if let Some(old) = self.filled.replace((filled.board.clone(), fills.clone())) {
                    self.discarded.push(Box::new(old));
                }
                self.set_board(cx, filled.board.clone(), fills);
                if let Some(old) = self.rules.replace((filled.board, filled.rules)) {
                    self.discarded.push(Box::new(old));
                }
                self.refresh_rail(cx);
            }
            Err(e) => eprintln!("boardbench fills failed: {e:?}"),
        }
    }

    /// Weight each timing window by its frame count for the whole-run summary.
    fn record_stats(&mut self, stats: &BoardStats) {
        let frames = stats.frames.saturating_sub(self.reported_frames);
        self.draw_us_total += stats.draw_us_mean * frames as f64;
        self.draw_us_max = self.draw_us_max.max(stats.draw_us_max);
        self.reported_frames = stats.frames;
    }

    fn status(&mut self, cx: &mut Cx) {
        let stats = self
            .ui
            .widget(cx, ids!(board))
            .borrow_mut::<BoardPane>()
            .map(|mut board| board.take_stats());
        let Some(stats) = stats else { return };
        self.record_stats(&stats);
        let gpu: String = stats
            .gpu_ms
            .iter()
            .map(|(name, ms)| format!(" {name}={ms:.3}"))
            .collect();
        let mut line = format!(
            "boardbench frames={} draw_us mean={:.2} max={:.2} gpu_ms{} instances={} lists={} layers={} scene_ms={:.3} batch_ms={:.3}",
            stats.frames, stats.draw_us_mean, stats.draw_us_max, gpu,
            stats.instances, stats.lists, stats.layers, stats.scene_ms, stats.batch_ms,
        );
        // Which consumers kept the semantic builder on (OS 1, REMOTE 2, TEST 4, AUTOMATION 8, HOST 16).
        line.push_str(&format!(" a11y={:#x}", cx.a11y.consumers()));
        if let Some(route) = self.route.as_ref().filter(|route| route.started.is_some() && !route.done) {
            line.push_str(&format!(" route open={}/{} findings={}", route.open, route.nets, route.findings));
        }
        if self.heat_test {
            line.push_str(" heat=test");
        }
        println!("{line}");
        self.ui.label(cx, ids!(status)).set_text(cx, &line);
        if stats.pictures > self.pictures_seen {
            self.pictures_seen = stats.pictures;
            println!(
                "boardbench picture n={} incremental={} edited={} worker_ms={:.3} edit_ms={:.3}",
                stats.pictures, stats.incremental, stats.edited, stats.batch_ms, stats.edit_ms
            );
        }
    }

    fn finish_if_ready(&mut self, cx: &mut Cx) {
        if self.pending_kicad.is_some() || self.kicad_job.is_some() || self.kicad_first_frame {
            return;
        }
        let Some(limit) = self.frame_limit else { return };
        let stats = self
            .ui
            .widget(cx, ids!(board))
            .borrow_mut::<BoardPane>()
            .and_then(|mut board| {
                if board.stats().frames.saturating_sub(self.frame_base) >= limit {
                    Some(board.take_stats())
                } else {
                    None
                }
            });
        let Some(stats) = stats else { return };
        self.record_stats(&stats);
        println!(
            "boardbench done frames={} draw_us_mean={:.2} draw_us_max={:.2}",
            stats.frames,
            self.draw_us_total / stats.frames.max(1) as f64,
            self.draw_us_max,
        );
        self.frame_limit = None;
        cx.quit();
    }

    fn report_first_frame(&mut self, cx: &mut Cx) {
        let widget = self.ui.widget(cx, ids!(board));
        let Some(pane) = widget.borrow::<BoardPane>() else { return };
        let stats = pane.stats();
        if stats.pictures > self.first_frames_seen {
            self.first_frames_seen = stats.pictures;
            println!("boardbench first_frame ms={:.3}", stats.edit_ms);
            if self.kicad_first_frame && pane.drawing().is_some_and(|draw| {
                self.committed.as_ref().is_some_and(|(board, _)| Arc::ptr_eq(board, &draw.board))
            }) {
                // The frame limit for an import counts its frames, excluding the loading screen.
                self.frame_base = stats.frames.saturating_sub(1);
                self.kicad_first_frame = false;
            }
        }
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let raw_args: Vec<_> = std::env::args_os().collect();
        let args: Vec<String> = raw_args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let flag = |name: &str| args.iter().any(|arg| arg == name);
        let kicad = flag("--kicad");
        self.kicad_splash = flag("--splash");
        if self.kicad_splash && !kicad {
            eprintln!("boardbench --splash needs --kicad <path>");
        }
        self.pending_kicad = raw_args.iter().position(|arg| arg == "--kicad")
            .and_then(|i| raw_args.get(i + 1))
            .filter(|path| !path.to_string_lossy().starts_with("--"))
            .map(PathBuf::from);
        if kicad && self.pending_kicad.is_none() {
            eprintln!("boardbench kicad refused at 0:0: missing --kicad path");
        }
        let segments = number(&args, "--segments").map_or(100_000, |value| value);
        let mut layers = number(&args, "--layers").map_or(8, |value| value);
        let seed = number(&args, "--seed").map_or(1, |value| value);
        if !matches!(layers, 2 | 4 | 6 | 8) {
            eprintln!("boardbench invalid --layers value {layers}; expected 2, 4, 6 or 8; keeping default 8");
            layers = 8;
        }
        self.frame_limit = number(&args, "--frames");
        self.drc = flag("--drc");
        self.heat_test = flag("--heat-test");
        self.art = flag("--art");
        self.probe_bench = number(&args, "--probe");
        self.ui.view(cx, ids!(probe_chip)).set_visible(cx, self.probe_bench.is_some());
        if flag("--route") {
            let (handle, inbox) = route_channel(SignalToUI::set_ui_signal);
            let spawn = Arc::new(RouteSpawn(cx.task_pool()));
            let worker = RouteWorker::new(inbox, spawn);
            let options = ThreadOptions { name: Some("boardbench-route".into()), ..Default::default() };
            match cx.thread_spawner().spawn_worker(options, move || worker.run()) {
                Ok(worker) => worker.detach(),
                Err(e) => {
                    eprintln!("boardbench route worker failed to start: {e:?}");
                    cx.quit();
                    return;
                }
            }
            let method = if flag("--organic") { Method::Organic } else { Method::Interactive };
            self.route = Some(Routing {
                handle, method, requests: VecDeque::new(), started: None, done: false,
                newest: None, shown: None, applying: None, nets: 0, open: 0, findings: 0,
            });
        }
        self.edit = match args.iter().position(|a| a == "--edit").and_then(|i| args.get(i + 1)).map(String::as_str) {
            Some("move") => Some(EditKind::Move),
            Some("route") => Some(EditKind::Route),
            Some("fill") => Some(EditKind::Fill),
            Some(other) => {
                eprintln!("boardbench invalid --edit value {other:?}; expected move, route or fill; no edits");
                None
            }
            None => None,
        };

        let start = Instant::now();
        let (mut board, mut design) = if kicad {
            // A separate unit makes the pane fit the imported board when it arrives.
            (Board::new(BoardId(1), "Loading KiCad board"), None)
        } else if flag("--fixture") {
            let name = args.iter().position(|arg| arg == "--board").and_then(|i| args.get(i + 1)).map_or("ledctl", String::as_str);
            let mut design = if name == "ledctl" {
                fab_elec_fixtures::ledctl()
            } else {
                match fab_elec_fixtures::docs(name).map(|sources| fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())) {
                    Some(Ok(evaluated)) => evaluated.design,
                    Some(Err(diags)) => {
                        eprintln!("boardbench fixture {name} does not evaluate: {diags:?}");
                        cx.quit();
                        return;
                    }
                    None => {
                        eprintln!("boardbench has no stock fixture {name}");
                        cx.quit();
                        return;
                    }
                }
            };
            let Some(unit) = design.units.iter_mut().find(|unit| unit.board.as_ref().is_some_and(|b| !b.placed.is_empty())) else {
                eprintln!("boardbench fixture has no board");
                cx.quit();
                return;
            };
            if flag("--seed") {
                use fab_elec_route::place::{seed, Cost, PlaceProblem, SeedOpts};
                let board = unit.board.as_mut().expect("the unit has a board");
                let started = Instant::now();
                let problem = PlaceProblem::new(board, &[]);
                let hand: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
                let mut poses = hand.clone();
                let report = seed(board, &unit.circuit, &problem, &mut poses, &SeedOpts::default());
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                let (ours, theirs) = (Cost::new(&problem, &poses).terms().wirelength, Cost::new(&problem, &hand).terms().wirelength);
                println!("boardbench seed ms={ms:.3} parts={} unplaced={} wirelength={:.2}x", poses.len(), report.unplaced.len(), ours as f64 / theirs.max(1) as f64);
                let board = Arc::make_mut(board);
                for (m, pose) in problem.movables.iter().zip(&poses) {
                    if let Some(p) = board.placed.iter_mut().find(|p| p.id == m.item) {
                        (p.at, p.rot, p.side) = (pose.at, pose.rot, pose.side);
                    }
                }
                // The hand placement's copper and pours belong to its poses.
                board.tracks.clear();
                board.vias.clear();
                board.zones.clear();
                board.reindex();
            }
            let board = Arc::make_mut(unit.board.as_mut().expect("the unit has a board"));
            let n = number::<usize>(&args, "--unplace").unwrap_or(0).min(board.placed.len());
            let removed: Vec<_> = board.placed.drain(..n).collect();
            self.discarded.push(Box::new(removed));
            (board.clone(), Some(design))
        } else {
            (generated_board(&GenSpec {
                segments,
                copper_layers: layers,
                seed,
                ..GenSpec::bar()
            }), None)
        };
        if flag("--unrouted") {
            // No tracks or pours: every connection is a ratsnest line (drag proofs).
            board.tracks.clear();
            board.zones.clear();
        }
        let board_ms = start.elapsed().as_secs_f64() * 1000.0;
        let segments: usize = board.tracks.iter().map(|track| track.path.segs.len()).sum();
        let pads: usize = board.placed.iter().map(|placed| placed.footprint.pads.len()).sum();
        if !kicad {
            println!("boardbench board ms={board_ms:.3} segments={segments} pads={pads}");
        }

        let board = Arc::new(board);
        if let Some(design) = design.as_mut() {
            let unit = design.units.iter_mut().find(|unit| unit.id == board.unit).unwrap();
            if let Some(old) = unit.board.replace(board.clone()) {
                self.discarded.push(Box::new(old));
            }
        }
        if let Some(old) = std::mem::replace(&mut self.design, design.map(Arc::new)) {
            self.discarded.push(Box::new(old));
        }
        let parts = self.design.as_ref().map(|design| unplaced(design, board.unit)).unwrap_or_default();
        self.tray_parts = parts.len();
        if kicad {
            self.set_board(cx, board, Arc::new(Vec::new()));
        } else {
            self.install_board(cx, board);
        }
        if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
            pane.set_design(cx, self.design.clone());
            if let Some(name) = &self.probe_bench {
                pane.set_bench(Some(name.clone()));
            }
            pane.set_light(cx, flag("--light"));
            pane.set_autopan(cx, flag("--autopan"));
            pane.set_composite(cx, !flag("--direct"));
        } else {
            eprintln!("boardbench BoardPane is unavailable");
            cx.quit();
            return;
        }
        self.refresh_rail(cx);
        if let Some(mut tray) = self.ui.widget(cx, ids!(tray)).borrow_mut::<BoardTray>() {
            tray.set_parts(cx, parts);
        } else {
            self.discarded.push(Box::new(parts));
        }
        self.timer = cx.start_interval(1.0);
        if self.edit.is_some() {
            self.edit_timer = cx.start_interval(2.0);
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.probe_bench.is_some() {
            let chip = self.ui.view(cx, ids!(probe_chip));
            if let Some(down) = chip.finger_down(actions) {
                self.probe_press = Some(down.abs);
            }
            if let Some(moved) = chip.finger_move(actions) {
                if self.probe_press.is_some_and(|abs| (moved.abs - abs).length() > 4.0) {
                    self.probe_press = None;
                    cx.start_dragging(vec![probe_drag_item()]);
                }
            }
            if chip.finger_up(actions).is_some() {
                self.probe_press = None;
            }
        }
        for action in actions {
            // The pane emits its actions as widget actions.
            if let Some(action) = action.as_widget_action().and_then(|w| w.action.downcast_ref::<ViewAction>()) {
                if let ViewAction::Ops { ops, label, .. } = action {
                    for op in ops {
                        if let DesignOp::Bench { bench, op: BenchOp::AddProbe { of, label: channel } } = op {
                            let net = &of.target;
                            println!("boardbench probe bench={bench} net={net} label={channel} action={label}");
                            log!("boardbench probe bench={bench} net={net} label={channel} action={label}");
                        }
                    }
                }
                // A Checks row (or any other view) frames its finding on the board.
                if let ViewAction::Focus(focus) = action {
                    if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
                        pane.answer_focus(cx, focus);
                    }
                }
                // The route owns the preview until its final proposal commits; layer controls still work.
                if self.route.as_ref().is_some_and(|route| !route.done) {
                    continue;
                }
                match action {
                    ViewAction::Preview { ops, place, .. } => {
                        let Some((board, _)) = self.committed.as_ref() else { continue };
                        // A tray drop's `place` (one part per drop), resolved as the host resolves it:
                        // the part, then the labels it disturbs, in the same preview.
                        let mut ops = ops.clone();
                        if let Some(design) = &self.design {
                            let placed: Vec<DesignOp> = place.iter().filter_map(|place| {
                                let PartRef::Path(path) = &place.part else { return None };
                                let comp = design.unit(place.unit)?.circuit.comp(path)?.id;
                                placement_op(design, place.unit, comp, place.at, place.rot.unwrap_or(Angle::ZERO), place.side.unwrap_or(Side::Top))
                            }).collect();
                            let labels = placement_labels(design, &placed);
                            ops.extend(placed);
                            ops.extend(labels);
                        }
                        let ops = ops.iter().filter_map(|op| match op {
                            DesignOp::Board { unit, op } if *unit == board.unit => Some(op.clone()),
                            _ => None,
                        }).collect();
                        if let Some(old) = self.want.replace(ops) {
                            self.discarded.push(Box::new(old));
                        }
                        self.start_preview(cx);
                    }
                    ViewAction::CommitPreview => self.commit_preview(cx),
                    ViewAction::CancelPreview => self.cancel_preview(cx),
                    _ => {}
                }
            }
        }
        let actions = self.ui.widget(cx, ids!(rail)).borrow::<BoardLayerRail>()
            .map(|rail| rail.rail_actions(actions)).unwrap_or_default();
        if actions.is_empty() {
            return;
        }
        if let Some(mut pane) = self.ui.widget(cx, ids!(board)).borrow_mut::<BoardPane>() {
            for action in actions {
                match action {
                    RailAction::Activate(layer) => pane.set_active_layer(cx, layer),
                    RailAction::Toggle(layer) => {
                        if let Some(row) = pane.layers().into_iter().find(|row| row.layer == layer) {
                            pane.set_layer_visible(cx, layer, !row.visible);
                        }
                    }
                    RailAction::Solo(layer) => pane.solo_layer(cx, layer),
                    RailAction::None => {}
                }
            }
        }
        self.refresh_rail(cx);
    }

    fn handle_timer(&mut self, cx: &mut Cx, event: &TimerEvent) {
        if self.timer.is_timer(event).is_some() {
            self.status(cx);
        }
        if self.edit_timer.is_timer(event).is_some() {
            self.start_edit(cx);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::script_mod(vm);
        // the board shaders use the live-row functions (`mod.elec_live`)
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::board::script_mod(vm);
        art_preview::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        if matches!(event, Event::Signal | Event::Timer(_) | Event::Draw(_)) {
            self.take_kicad(cx);
            self.start_kicad(cx);
            self.take_fills(cx);
            self.take_heat(cx);
            self.take_edit(cx);
            self.take_preview(cx);
            self.take_conn(cx);
            self.start_preview(cx);
            self.start_conn(cx);
        }
        if matches!(event, Event::Signal | Event::Timer(_)) {
            self.start_route(cx);
            self.retry_route();
            self.take_route(cx);
        }
        if matches!(event, Event::Draw(_)) {
            self.start_fills(cx);
            self.start_heat(cx);
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
        // set_board queues a worker: refresh again after the pane receives the built scene.
        if matches!(event, Event::Signal | Event::Timer(_)) {
            self.refresh_rail(cx);
        }
        if matches!(event, Event::Draw(_)) {
            self.report_first_frame(cx);
            self.start_kicad(cx);
            self.finish_if_ready(cx);
        }
        self.sweep(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::zone::fill_all;

    #[test]
    fn drag_over_held_pour_findings_are_pending_until_refill() {
        let design = fab_elec_fixtures::ledctl();
        let mut board = design.units.iter().find_map(|unit| unit.board.as_deref())
            .expect("the bench fixture has a board").clone();
        let unit = board.unit;
        let zones: Vec<_> = board.zones.iter().map(|zone| zone.id).collect();
        let names_zone = |violation: &Violation| violation.items.iter().any(|sel| {
            matches!(sel, Sel::Item(board_unit, item) if *board_unit == unit && zones.contains(item))
        });
        let rules = resolve_rules(&board, None);
        let fills = fill_all(&board, &rules);
        let conn = connect(&board, &fills);
        let initial = drc(&board, &conn, &rules, &fills, None);
        assert!(initial.violations.iter().all(|v| v.kind != CheckKind::Short || !names_zone(v)),
            "the fixture's exact pours must start without zone shorts");

        let ground = board.net("GND").expect("the fixture has a ground net");
        let pour = board.zones.iter().find(|zone| zone.net == Some(ground))
            .expect("the fixture has a ground pour");
        // The fixture's first two-pad part with neither pad on GND is lamps[0].d (D1).
        let item = board.placed.iter().find(|part| {
            part.footprint.pads.len() == 2 && part.pad_nets.len() == 2
                && part.pad_nets.iter().all(|net| net.is_some() && *net != pour.net)
        }).expect("the fixture has a two-pad part with both pads on nets other than the pour").id;
        board.apply_all(&[BoardOp::Move { item, by: P::mm(3.0, 0.0) }])
            .expect("the drag can move the selected part 3 mm in x");
        let moved = board.placed_item(item).expect("the moved part remains on the board");
        assert!(board.outline_bbox().contains_box(&moved.bbox()),
            "the moved part must stay inside the fixture's rectangular outline");

        let held_conn = connect(&board, &fills);
        let held = drc(&board, &held_conn, &rules, &fills, None);
        let awaiting = awaiting_refill(&board, &held_conn, &held.violations, true);
        assert!(held.violations.iter().any(names_zone),
            "the drag must put copper of another net into the held pour");
        assert!(awaiting.windows(2).all(|pair| pair[0] < pair[1]),
            "pending finding ids must be sorted and unique");
        assert!(held.violations.iter().filter(|v| names_zone(v)).all(|v| awaiting.binary_search(&v.id).is_ok()),
            "findings over held fills must be pending");
        let through_pour: Vec<_> = held_conn.shorts.iter().filter(|short| {
            held_conn.clusters[short.cluster as usize].items.iter().any(|item| matches!(item, ConnItem::Zone { .. }))
        }).filter_map(|short| Some(short.witness?.at)).collect();
        assert!(!through_pour.is_empty(), "the moved pads must join the held pour's cluster: a short through the pour");
        assert!(held.violations.iter().filter(|v| v.kind == CheckKind::Short && v.at.is_some_and(|at| through_pour.contains(&at)))
            .all(|v| awaiting.binary_search(&v.id).is_ok()),
            "a short through the held pour must be pending, not a definite short");

        let refilled = fill_all(&board, &rules);
        let refilled_conn = connect(&board, &refilled);
        let checked = drc(&board, &refilled_conn, &rules, &refilled, None);
        assert!(checked.violations.iter().all(|v| v.kind != CheckKind::Short || !names_zone(v)),
            "the refill must clear the pour around the moved pads");
        assert!(awaiting_refill(&board, &refilled_conn, &checked.violations, false).is_empty(),
            "findings over exact fills must not await a refill");
    }
}
