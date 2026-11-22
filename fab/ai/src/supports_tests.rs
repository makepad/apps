//! The support tools on plates made of shapes: an open arch, a cup with an overhang inside, a lattice.

use super::*;
use crate::supports::{ColoredMesh, LookRequest, SliceSource};
use makepad_fab_slice::live::{Frame, LiveEngine, LiveEvent, LiveInput, PartField, PlatePaths, Sink, Spawn, Urgency};
use makepad_script::syntax::parse_value;
use makepad_script::syntax::Node;
use std::hash::{Hash, Hasher};

/// The live slice without the app: an engine on threads of its own, given what the app gives it
/// (Fab's default process, the parts in their frames, a field part as its field). It keeps the
/// last report of the sliced checks: what the Print gate reads.
#[derive(Default)]
pub(crate) struct EngineSlices {
    report: std::sync::Mutex<Option<Arc<fab_printability::Report>>>,
    /// The last slice made, by its input's key (plate, settings).
    last: std::sync::Mutex<Option<((u64, u64), Arc<PlatePaths>)>>,
}

impl EngineSlices {
    /// A live engine on plain threads, and what it says.
    fn engine() -> (LiveEngine, std::sync::mpsc::Receiver<LiveEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let spawn: Spawn = Arc::new(|_, job| {
            std::thread::spawn(job);
        });
        let version = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let next: makepad_fab_slice::live::Next = Arc::new(move || {
            use std::sync::atomic::Ordering;
            let mine = version.fetch_add(1, Ordering::AcqRel) + 1;
            let version = version.clone();
            Arc::new(move || version.load(Ordering::Acquire) != mine)
        });
        let sink: Sink = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let engine = LiveEngine::new(spawn.clone(), sink, next);
        engine.set_resume(Arc::new(move |checkpoint| {
            let spawn = spawn.clone();
            Arc::new(move |urgency, job| {
                let checkpoint = checkpoint.clone();
                spawn(urgency, Box::new(move || if urgency == Urgency::Viewed || !checkpoint() { job() }));
            })
        }));
        (engine, rx)
    }

    /// The plate as Print writes it: its G-code and stats.
    pub(crate) fn settled(input: &LiveInput) -> Result<Arc<makepad_fab_slice::live::Settled>, String> {
        let (engine, rx) = EngineSlices::engine();
        engine.settle(1, input.clone());
        rx.iter().find_map(|e| match e {
            LiveEvent::Settled { result, .. } => Some(result),
            LiveEvent::Failed { error, .. } => Some(Err(error)),
            _ => None,
        }).ok_or("the engine stopped")?
    }
}

impl SliceSource for EngineSlices {
    fn input(&self, ev: &Evaluated) -> Result<(LiveInput, String), String> {
        let (setup, paint, name) = crate::weave::sliced_setup(ev, None)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}{:?}{:?}", setup.process.values, setup.filaments, ev.plate.settings).hash(&mut h);
        let settings_id = h.finish();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (ev.content, ev.plate.parts.len()).hash(&mut h);
        let mut input = LiveInput::ready(h.finish(), settings_id, setup, Arc::new(ev.plate.printable()), Arc::new(paint));
        input.machine = ev.machine.clone();
        input.frames = Some(Arc::new(ev.frames.iter().enumerate().map(|(i, f)| {
            let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
            Frame { local: f.local.clone(), offset: f.offset, field }
        }).collect()));
        Ok((input, name))
    }

    fn sliced(&self, input: &LiveInput, _: Option<&dyn Fn() -> bool>) -> Result<Arc<PlatePaths>, String> {
        let (engine, rx) = EngineSlices::engine();
        engine.set_input(input.clone());
        // What their supports tell follows the toolpaths, before the sliced checks end.
        let mut paths = None;
        for e in rx.iter() {
            match e {
                LiveEvent::Toolpaths { toolpaths, .. } => paths = Some(toolpaths),
                LiveEvent::Issues { report, done, total, .. } if done == total && total > 0 && paths.is_some() => {
                    *self.report.lock().unwrap() = Some(report);
                    break;
                }
                LiveEvent::Failed { error, .. } => return Err(error),
                _ => {}
            }
        }
        let paths = paths.ok_or("the engine stopped")?;
        *self.last.lock().unwrap() = Some(((input.plate_id, input.settings_id), paths.clone()));
        Ok(paths)
    }

    fn made(&self, ev: &Evaluated) -> Option<Arc<PlatePaths>> {
        let (input, _) = self.input(ev).ok()?;
        self.last.lock().unwrap().as_ref().filter(|(key, _)| *key == (input.plate_id, input.settings_id)).map(|(_, paths)| paths.clone())
    }
}

/// A session whose support tools plan as the app's do.
fn sliced_session() -> Session {
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    session.slices = Some(Arc::new(EngineSlices::default()));
    session
}

/// A 14 mm span under a 4 mm roof, 12 mm deep, on two 5 mm legs: its underside faces the bed from z = 10.
const ARCH: &str = "let m = mod.model\nlet block = m.box({size: [24mm 14mm 12mm] at: [0 7mm 0]})\nlet gap = m.box({size: [14mm 10mm 20mm] at: [0 5mm 0]})\nobject(m.subtract(block, gap))\n";
/// The same arch as a field.
const FIELD_ARCH: &str = "let m = mod.model\nobject(m.sdf({bounds: [[-12mm, 0, -6mm] [12mm, 14mm, 6mm]] cell: 0.5mm field: fn(p) { return sdf_difference(sdf_box(p - vec3(0, 7mm, 0), vec3(24mm, 14mm, 12mm)), sdf_box(p - vec3(0, 5mm, 0), vec3(14mm, 10mm, 20mm))) }}))\n";
/// The arch's underside: its area and height.
const UNDERSIDE: f64 = 168.0;
const ROOF: f64 = 10.0;
/// A cup 24 mm across and 24 mm tall, floor and walls 3 mm, with a ring standing in from its wall at
/// z 12: the ring's underside is over the cup's floor, 9 mm below it.
const CUP: &str = "let m = mod.model\nlet body = m.cylinder({radius: 12mm height: 24mm at: [0 12mm 0]})\nlet hollow = m.cylinder({radius: 9mm height: 24mm at: [0 15mm 0]})\nlet ring = m.subtract(m.cylinder({radius: 9.5mm height: 2mm at: [0 13mm 0]}), m.cylinder({radius: 5mm height: 4mm at: [0 13mm 0]}))\nobject(m.union(m.subtract(body, hollow), ring))\n";

fn plate(script: &str, settings: &str) -> Session {
    let mut session = sliced_session();
    let scripts = BTreeMap::from([("shape.cad.splash".to_string(), format!("use mod.model.*\nuse mod.fab.*\n{script}"))]);
    let project = format!("use mod.fab.*\nsettings({{layer_height: \"0.2\" {settings}}})\nscript(\"shape.cad.splash\")\nplace(\"shape\", at: [100mm 100mm])\n");
    session.model = FabModel::load("supports", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    session
}

const TREE: &str = "enable_support: \"1\" support_type: \"tree(auto)\" support_threshold_angle: \"45\"";
/// Normal support, which stands wherever it is held.
const NORMAL: &str = "enable_support: \"1\" support_type: \"normal(auto)\" support_threshold_angle: \"45\"";
/// Trees, and asked to stand inside the part's hollows: by default none does.
const TREE_INSIDE: &str = "enable_support: \"1\" support_type: \"tree(auto)\" support_threshold_angle: \"45\" fab_support_inside: \"1\"";
/// Normal support, asked to stand inside the part's hollows.
const NORMAL_INSIDE: &str = "enable_support: \"1\" support_type: \"normal(auto)\" support_threshold_angle: \"45\" fab_support_inside: \"1\"";

fn lattice() -> Session {
    lattice_of(12.0, 6.0, TREE)
}

/// The app's lattice cube at a size and period (mm).
fn lattice_of(size: f64, period: f64, settings: &str) -> Session {
    let mut session = sliced_session();
    let scripts = BTreeMap::from([("lattice_cube.cad.splash".to_string(), include_str!("../../app/resources/lattice_cube.cad.splash").to_string())]);
    let project = format!("use mod.fab.*\nsettings({{layer_height: \"0.2\" {settings}}})\nscript(\"lattice_cube.cad.splash\", params: {{lattice_size: {size}mm lattice_period: {period}mm}})\nplace(\"lattice_cube\", object: \"lattice\", at: [100mm 100mm])\n");
    session.model = FabModel::load("supports", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    session
}

fn head(s: &Session) -> String {
    s.model.head().text()
}

struct Answer(String, Node);

impl Answer {
    fn of(r: &Reply) -> Answer {
        assert!(r.ok, "{}", r.text);
        Answer::text(r.text.clone())
    }

    fn text(text: String) -> Answer {
        let node = parse_value(&text).unwrap_or_else(|e| panic!("{e:?}: {text}"));
        Answer(text, node)
    }

    fn at(&self, path: &[&str]) -> &Node {
        path.iter().fold(&self.1, |n, k| &n.field(k).unwrap_or_else(|| panic!("no {k} in {}", self.0)).value)
    }

    fn word(&self, n: &Node) -> f64 {
        let t = n.text(&self.0).trim_end_matches("mm");
        t.parse().unwrap_or_else(|_| panic!("{t} is no number in {}", self.0))
    }

    fn num(&self, path: &[&str]) -> f64 {
        self.word(self.at(path))
    }

    fn rows(&self, path: &[&str]) -> &[Node] {
        self.at(path).items().unwrap_or_else(|| panic!("{path:?} is no list in {}", self.0))
    }

    fn of_row(&self, row: &Node, k: &str) -> f64 {
        self.word(&row.field(k).unwrap_or_else(|| panic!("no {k} in {}", self.0)).value)
    }

    fn point(&self, n: &Node) -> [f64; 3] {
        let items = n.items().unwrap_or_else(|| panic!("no point in {}", self.0));
        std::array::from_fn(|k| self.word(&items[k]))
    }
}

fn tool(s: &mut Session, name: &str, body: &str) -> Reply {
    let text = body.replace('\'', "\"");
    s.call(name, &makepad_model::json::parse(text.as_bytes()).unwrap_or_else(|_| panic!("bad args {text}")), None)
}

fn near(a: f64, b: f64, by: f64, what: &str) {
    assert!((a - b).abs() <= by, "{what}: {a} is not within {by} of {b}");
}

fn checks_the_arch(session: &mut Session) {
    let a = Answer::of(&tool(session, "overhangs", "{}"));
    assert_eq!(a.num(&["limit_deg"]), 45.0, "{}", a.0);
    let rows = a.rows(&["rows"]);
    let top = &rows[0];
    assert_eq!(top.field("id").map(|f| f.value.text(&a.0)), Some("@p1"), "{}", a.0);
    near(a.of_row(top, "area_mm2"), UNDERSIDE, 10.0, "the underside's area");
    near(a.point(&top.field("low").unwrap().value)[2], ROOF, 0.3, "its lowest point");
    near(a.of_row(top, "clear"), ROOF, 0.3, "the way down to the bed");
    assert_eq!(top.field("over").unwrap().value.text(&a.0), "@bed", "{}", a.0);
    assert!(a.of_row(top, "covered") > 0.5, "planned supports reach most of a plain underside: {}", a.0);
}

#[test]
fn an_arch_lists_its_underside_with_what_is_below_it() {
    checks_the_arch(&mut plate(ARCH, TREE));
}

/// The hair on a head (the guide's example): one object of three bodies, a field head and two
/// exact locks sunk into it. fab.tree and fab.inspect list the object with each body's engine.
/// Its volume is its bodies' own summed until the plate is sliced, the overlap counted once per
/// body; then the slice's, which counts it once.
#[test]
fn an_object_lists_its_bodies_with_their_engines_and_its_volume() {
    let mut session = sliced_session();
    let hair = "let m = mod.model\n\
let head = m.sdf({field: fn(p) { return sdf_sphere(p - vec3(0, 12mm, 0), 12mm) } bounds: [[-12.5mm, -0.5mm, -12.5mm] [12.5mm, 24.5mm, 12.5mm]] cell: 0.5mm})\n\
let lock = m.brep.sweep({sketch: Circle{radius: 2.5mm} path: [[0 14mm 0] [7mm 15mm 0] [14mm 16mm 0] [22mm 13mm 0]]})\n\
let other = m.brep.sweep({sketch: Circle{radius: 2.5mm} path: [[0 14mm 0] [-7mm 15mm 0] [-14mm 16mm 0] [-22mm 13mm 0]]})\n\
object([head, lock, other], name: \"bust\")\n";
    let scripts = BTreeMap::from([("hair.cad.splash".to_string(), format!("use mod.model.*\nuse mod.fab.*\n{hair}"))]);
    let project = format!("use mod.fab.*\nsettings({{layer_height: \"0.2\" {TREE}}})\nscript(\"hair.cad.splash\")\nplace(\"hair\", object: \"bust\", at: [100mm 100mm])\n");
    session.model = FabModel::load("hair", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    // the head's volume is read after the build, as the host reads it once the build is in
    for f in session.last.as_ref().unwrap().fields.iter().flatten() {
        if let Some(v) = &f.volume {
            v.read();
        }
    }
    // the one object of an answer: its id, its bodies with their engines, its volume and where that comes from
    let object = |a: &Answer| -> (String, Vec<(String, String)>, f64, String) {
        let rows = a.rows(&["objects"]);
        assert_eq!(rows.len(), 1, "one object: {}", a.0);
        let o = &rows[0];
        let text = |n: &Node, k: &str| n.field(k).and_then(|f| f.value.string()).unwrap_or_else(|| panic!("no {k} in {}", a.0)).to_string();
        let engine = |n: &Node| n.field("engine").and_then(|f| f.value.name()).unwrap_or_else(|| panic!("no engine in {}", a.0)).to_string();
        let bodies = o.field("bodies").and_then(|f| f.value.items()).unwrap_or_else(|| panic!("no bodies in {}", a.0)).iter().map(|b| (text(b, "part"), engine(b))).collect();
        (text(o, "object"), bodies, a.of_row(o, "volume_mm3"), text(o, "volume_from"))
    };
    let three: Vec<(String, String)> = [("hair/head", "field"), ("hair/lock", "exact"), ("hair/other", "exact")].map(|(p, e)| (p.to_string(), e.to_string())).into();
    let tree = Answer::of(&tool(&mut session, "tree", "{}"));
    let (id, bodies, summed, from) = object(&tree);
    assert_eq!((id.as_str(), &bodies), ("hair/bust", &three), "{}", tree.0);
    assert!(from.starts_with("bodies summed"), "not sliced yet: {from}");
    // A support tool plans with the plate's live slice: the volume is then the slice's.
    Answer::of(&tool(&mut session, "overhangs", "{}"));
    let inspect = Answer::of(&tool(&mut session, "inspect", "{'program': false}"));
    let (id, bodies, sliced, from) = object(&inspect);
    assert_eq!((id.as_str(), &bodies), ("hair/bust", &three), "{}", inspect.0);
    assert!(from.starts_with("slice layers"), "sliced: {from}");
    println!("[objects] the bust's volume: its bodies summed {summed:.0} mm3, the slice's {sliced:.0} mm3");
    assert!(sliced < 0.98 * summed && sliced > 0.85 * summed, "the slice counts the locks' sunk ends once: {sliced:.0} against {summed:.0} mm3");
}

#[test]
fn a_field_arch_lists_the_same_underside_from_its_field() {
    let mut session = plate(FIELD_ARCH, TREE);
    let ev = session.last.clone().unwrap();
    assert!(ev.plate.parts[0].mesh.triangles.is_empty(), "a field part has no mesh of its own");
    checks_the_arch(&mut session);
}

/// The support types a plate can ask for: a try plans each as the live slice does. (Coral joins
/// this list when slicing takes it.)
const SUPPORT_TYPES: [&str; 2] = [TREE, NORMAL];

/// A try (a change not made yet) has no live slice: the same planner plans it from the same
/// inputs. A try that changes nothing answers what the live slice does, figures and verdicts.
#[test]
fn a_try_plans_what_the_live_slice_prints() {
    for settings in SUPPORT_TYPES {
        for shape in [ARCH, SPOOL] {
            let mut session = plate(shape, settings);
            let sliced = verdicts(&mut session, "{'limit':40}");
            let same = verdicts(&mut session, "{'limit':40,'place':'place(\\'shape\\', at: [100mm 100mm])'}");
            assert_eq!(same.at(&["tried"]).text(&same.0), "true", "{}", same.0);
            let figures = |a: &Answer| a.rows(&["parts"]).iter().map(|r| ["pieces", "branches", "volume_mm3", "extruding_s", "tallest", "left_mm2"].map(|k| a.of_row(r, k))).collect::<Vec<_>>();
            let rows = |a: &Answer| a.rows(&["verdicts", "rows"]).iter().map(|r| (a.word_of(r, "piece"), a.word_of(r, "verdict"), a.word_of(r, "sections_out"), a.word_of(r, "envelope_mm3"))).collect::<Vec<_>>();
            assert!(figures(&sliced)[0][0] > 0.0, "{settings}: the plate has supports: {}", sliced.0);
            assert_eq!(figures(&same), figures(&sliced), "{settings}: the try's plan is the live slice's: {}\n{}", same.0, sliced.0);
            assert_eq!(rows(&same), rows(&sliced), "{settings}: and so are its verdicts: {}\n{}", same.0, sliced.0);
        }
    }
}

#[test]
fn no_plan_covers_nothing_and_says_so() {
    let mut session = plate(ARCH, "enable_support: \"0\"");
    let a = Answer::of(&tool(&mut session, "overhangs", "{}"));
    assert!(a.0.contains("plans no supports"), "{}", a.0);
    assert_eq!(a.rows(&["rows"])[0].field("covered").unwrap().value.text(&a.0), "nil", "{}", a.0);
    near(a.num(&["area_mm2"]), UNDERSIDE, 10.0, "the area stands without a plan");
}

#[test]
fn a_try_answers_for_the_change_and_commits_nothing() {
    let mut session = plate(ARCH, TREE);
    let before = head(&session);
    let now = Answer::of(&tool(&mut session, "supports", "{}"));
    let parts = now.rows(&["parts"]);
    assert_eq!(parts[0].field("type").unwrap().value.text(&now.0), "@tree", "{}", now.0);
    let volume = now.of_row(&parts[0], "volume_mm3");
    assert!(volume > 0.0 && now.of_row(&parts[0], "extruding_s") > 0.0, "{}", now.0);
    assert_eq!(parts[0].field("material_g").unwrap().value.text(&now.0), "nil", "the default filament has no density: no mass is made up: {}", now.0);
    assert!(now.of_row(&parts[0], "tallest") > ROOF - 1.0, "the branches reach the underside: {}", now.0);
    assert!(now.of_row(&parts[0], "left_mm2") < UNDERSIDE / 2.0, "{}", now.0);

    let off = Answer::of(&tool(&mut session, "supports", "{'place':'settings({enable_support: \\'0\\'})'}"));
    assert_eq!(off.at(&["tried"]).text(&off.0), "true", "{}", off.0);
    let parts = off.rows(&["parts"]);
    assert_eq!(parts[0].field("type").unwrap().value.text(&off.0), "@none", "{}", off.0);
    assert_eq!(off.of_row(&parts[0], "volume_mm3"), 0.0, "{}", off.0);
    near(off.of_row(&parts[0], "left_mm2"), UNDERSIDE, 10.0, "without supports the whole underside is left");

    assert_eq!(head(&session), before, "a try commits nothing");
    let again = Answer::of(&tool(&mut session, "supports", "{}"));
    assert_eq!(again.of_row(&again.rows(&["parts"])[0], "volume_mm3"), volume, "and the plate is what it was");
    assert!(again.at(&["parts"]).text(&again.0).len() > 10 && again.1.field("tried").is_none(), "{}", again.0);
}

#[test]
fn a_try_of_a_script_edit_moves_the_overhang() {
    let mut session = plate(ARCH, TREE);
    let before = head(&session);
    let a = Answer::of(&tool(&mut session, "overhangs", "{'edits':'let wider = m.box({size: [20mm 2mm 20mm] at: [0 23mm 0]})\\nobject(wider)'}"));
    assert_eq!(a.at(&["tried"]).text(&a.0), "true", "{}", a.0);
    assert_eq!(head(&session), before);
    let bad = tool(&mut session, "overhangs", "{'edits':'object(nothing_called_this)'}");
    assert!(!bad.ok, "a try that doesn't build says so: {}", bad.text);
    assert!(Answer::of(&tool(&mut session, "overhangs", "{}")).rows(&["rows"]).len() == 1);
}

/// A support under the cup's ledge stands on the cup's floor, and from the bed only none gets to
/// it: the cup is open, through the ring and its top, so a support there comes out, and it stands
/// there by default as when asked to stand where it can't be removed.
#[test]
fn a_ledge_inside_a_cup_is_over_the_part_and_bed_only_leaves_it() {
    let mut session = plate(CUP, TREE_INSIDE);
    let a = Answer::of(&tool(&mut session, "overhangs", "{}"));
    let ledge = a.rows(&["rows"]).iter().find(|r| a.point(&r.field("low").unwrap().value)[2] > 8.0).unwrap_or_else(|| panic!("no ledge in {}", a.0));
    assert_eq!(ledge.field("over").unwrap().value.text(&a.0), "@part", "{}", a.0);
    near(a.of_row(ledge, "clear"), 9.0, 0.6, "from the ledge down to the cup's floor");
    let full = a.num(&["left_mm2"]);
    let bed = Answer::of(&tool(&mut session, "overhangs", "{'place':'settings({support_on_build_plate_only: \\'1\\'})'}"));
    assert!(bed.num(&["left_mm2"]) > full, "nothing from the bed reaches inside a cup: {} against {}", bed.0, a.0);
    let ledge_of = |a: &Answer| a.rows(&["rows"]).iter().find(|r| a.point(&r.field("low").unwrap().value)[2] > 8.0).cloned().unwrap_or_else(|| panic!("no ledge in {}", a.0));

    // The ledge is a ring: its name goes on it, not in its hole.
    let id = ledge_of(&a).field("id").unwrap().value.text(&a.0).trim_start_matches('@').to_string();
    let look = look_of(tool(&mut session, supports::VIEW_TOOL, &format!("{{'patch':'{id}'}}")));
    let (at, id) = &look.marks[0];
    let r = (at[0] - 100.0).hypot(at[1] - 100.0);
    assert!(id.starts_with('p') && r > 4.5 && r < 10.0, "{id} is written on the ring (5 to 9.5 mm from the axis), at {at:?}");

    // By default a support stands under the ledge too: nothing says it's left.
    let a = Answer::of(&tool(&mut plate(CUP, TREE), "overhangs", "{}"));
    let ledge = ledge_of(&a);
    assert!(ledge.field("locked").is_none() && ledge.field("given_up").is_none(), "the open cup locks nothing in: {}", a.0);
    // From the bed only the ledge says why it's left, with no enforce: the cup is in the way.
    let mut from_bed = plate(CUP, &format!("{TREE} support_on_build_plate_only: \"1\""));
    let a = Answer::of(&tool(&mut from_bed, "overhangs", "{}"));
    let ledge = ledge_of(&a);
    assert!(ledge.field("given_up").is_some() && ledge.field("enforce").is_none(), "no support gets to the ledge from the bed: {}", a.0);
    // The Slice panel's list says so too.
    let list = from_bed.overhang_list(12, None).unwrap_or_else(|e| panic!("{e}"));
    let lip = list.lines.iter().find(|l| l.min[2] > 8.0).unwrap_or_else(|| panic!("no ledge in {list:?}"));
    assert_eq!(lip.unheld, Some(fab_printability::Unheld::GivenUp), "{list:?}");
}

/// A squat belly 40 mm across with a flat foot: its underside overhangs from the bed up to 2.5 mm,
/// where a tree's pads stand on the bed with no branch.
const BELLY: &str = "let m = mod.model\nobject(m.carve(m.ellipsoid({radii: [20mm, 10mm, 20mm] at: [0, 10mm, 0]}), m.box({size: [50mm, 20mm, 50mm] at: [0, -7mm, 0]}), 0))\n";

#[test]
fn a_tree_of_pads_alone_is_a_tree() {
    let mut session = plate(BELLY, TREE);
    let a = Answer::of(&tool(&mut session, "supports", "{}"));
    let part = &a.rows(&["parts"])[0];
    assert!(a.of_row(part, "pieces") > 0.0 && a.of_row(part, "branches") == 0.0, "the belly is held by pads alone: {}", a.0);
    assert_eq!(part.field("type").unwrap().value.text(&a.0), "@tree", "{}", a.0);
}

#[test]
fn a_lattice_clusters_and_every_patch_is_in_some_row() {
    let mut session = lattice();
    let a = Answer::of(&tool(&mut session, "overhangs", "{'limit':6}"));
    let total = a.num(&["patches"]) as usize;
    let rows = a.rows(&["rows"]);
    assert!(total > rows.len(), "this test wants more patches than rows: {}", a.0);
    assert!(rows.len() <= 6, "{}", a.0);
    let counted: usize = rows.iter().map(|r| r.field("patches").map_or(1, |f| a.word(&f.value) as usize)).sum();
    assert_eq!(counted, total, "{}", a.0);
    let cluster = rows.iter().find(|r| r.field("patches").is_some()).unwrap_or_else(|| panic!("no cluster in {}", a.0));
    // The Slice panel lists the same rows.
    let list = session.overhang_list(6, None).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((list.patches, list.lines.len()), (total, rows.len()), "{list:?}\n{}", a.0);
    near(list.left_mm2.unwrap_or(-1.0), a.num(&["left_mm2"]), 0.01, "what the list leaves");
    for (line, row) in list.lines.iter().zip(rows) {
        assert_eq!(line.patches, row.field("patches").map_or(1, |f| a.word(&f.value) as usize), "{line:?}\n{}", a.0);
        near(line.area_mm2, a.of_row(row, "area_mm2"), 0.01, "a line's area");
        near(line.held().unwrap_or(-1.0), a.of_row(row, "covered"), 0.001, "a line's share held");
        // The panel steps through a cluster's patches: each is its own line, inside the cluster.
        if line.patches > 1 {
            assert_eq!(line.members.len(), line.patches, "{line:?}");
            near(line.members.iter().map(|m| m.area_mm2).sum(), line.area_mm2, 0.01, "the patches make up the cluster");
            for m in &line.members {
                assert!(m.id.is_some() && m.patches == 1 && m.members.is_empty(), "{m:?}");
                assert!((0..3).all(|k| m.min[k] >= line.min[k] && m.max[k] <= line.max[k]), "{m:?} inside {line:?}");
            }
            let left = |m: &fab_printability::OverhangLine| m.left_mm2.unwrap_or(m.area_mm2);
            assert!(line.members.windows(2).all(|w| left(&w[0]) >= left(&w[1])), "worst first: {line:?}");
        } else {
            assert!(line.members.is_empty(), "{line:?}");
        }
    }
    // From the bed only, nothing stands on the lattice: no enforce is offered over it.
    let bed = Answer::of(&tool(&mut session, "overhangs", "{'limit':40,'place':'settings({support_on_build_plate_only: \\'1\\'})'}"));
    let over = bed.rows(&["rows"]).iter().filter(|r| r.field("over").is_some_and(|o| o.value.text(&bed.0) == "@part")).count();
    assert!(over > 0 && !bed.rows(&["rows"]).iter().any(|r| r.field("over").is_some_and(|o| o.value.text(&bed.0) == "@part") && r.field("enforce").is_some()), "{}", bed.0);
    let corners = cluster.field("box").unwrap().value.items().unwrap();
    let (lo, hi) = (a.point(&corners[0]), a.point(&corners[1]));
    let drill = format!("{{'box':[[{},{},{}],[{},{},{}]],'limit':40}}", lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]);
    let inner = Answer::of(&tool(&mut session, "overhangs", &drill));
    let inside = inner.num(&["patches"]) as usize;
    assert!(inside < total && inside >= a.word(&cluster.field("patches").unwrap().value) as usize, "{} of {}: {}", inside, total, inner.0);
}

fn look_of(r: Reply) -> LookRequest {
    assert!(r.ok, "{}", r.text);
    match r.render {
        Some(RenderRequest::Look(look)) => (*look).clone(),
        other => panic!("no picture asked for: {other:?}"),
    }
}

fn has_color(mesh: &ColoredMesh, c: [f32; 3]) -> bool {
    mesh.colors.iter().any(|&k| k == c)
}

#[test]
fn a_view_is_framed_on_its_patch_with_the_overhang_and_the_branches_in_it() {
    let mut session = plate(ARCH, TREE);
    let look = look_of(tool(&mut session, supports::VIEW_TOOL, "{'patch':'p1'}"));
    assert_eq!(look.views, [supports::BELOW], "from below unless asked");
    let (lo, hi) = look.frame;
    assert!(lo[2] < ROOF && lo[2] > 0.0 && hi[0] - lo[0] > 14.0 && hi[0] - lo[0] < 30.0, "framed on the 14 x 12 underside, with room: {:?}", look.frame);
    let reds = (1..6).any(|d| {
        let o = fab_printability::OVERHANG_COLORS[d];
        has_color(&look.mesh, [o[0], o[1], o[2]])
    });
    assert!(reds, "the overhang is coloured by its degree");
    // Under a flat a trunk spreads into pads below the roof: the branches, or those, are drawn.
    let flare = look.mesh.indices.chunks_exact(3).any(|tri| look.mesh.colors[tri[0] as usize] == [0.2, 0.35, 0.95] && tri.iter().all(|&v| (look.mesh.positions[v as usize][2] as f64) < ROOF - 1.0));
    assert!(has_color(&look.mesh, [0.05, 0.75, 0.95]) || flare, "the branches are drawn");
    for tri in look.mesh.indices.chunks_exact(3) {
        let z = (0..3).map(|k| look.mesh.positions[tri[k] as usize][2] as f64).sum::<f64>() / 3.0;
        assert!(z >= ROOF - 0.1 || look.mesh.colors[tri[0] as usize] == [0.05, 0.75, 0.95] || look.mesh.colors[tri[0] as usize] == [0.2, 0.35, 0.95], "the part below the patch is cut away: {z}");
    }
    assert_eq!(look.marks[0].1, "p1");
    let answer = Answer::text(look.answer.clone());
    assert_eq!(answer.at(&["patch", "id"]).text(&answer.0), "@p1", "{}", answer.0);

    let bare = look_of(tool(&mut session, supports::VIEW_TOOL, "{'patch':'p1','supports':false}"));
    assert!(!has_color(&bare.mesh, [0.05, 0.75, 0.95]), "branches can be left out");
    let wrong = tool(&mut session, supports::VIEW_TOOL, "{'patch':'p9'}");
    assert!(!wrong.ok && wrong.text.contains("p1"), "{}", wrong.text);
    let many = tool(&mut session, supports::VIEW_TOOL, "{'patch':'p1','views':['front','back','left','right','top']}");
    assert!(!many.ok, "{}", many.text);
}

/// A render adds the plan when asked; a field part it draws from its own display mesh, as the
/// Mesh look shows it ([`Session::review_plate`]), nothing added.
#[test]
fn a_render_draws_the_plan_when_asked_and_a_field_part_from_its_field() {
    let mut session = plate(ARCH, TREE);
    let extra_of = |r: Reply| match r.render {
        Some(RenderRequest::Sheets { extra, .. }) => extra,
        other => panic!("no sheets: {other:?}"),
    };
    assert!(extra_of(tool(&mut session, "render", "{}")).is_none(), "a meshed part needs nothing added");
    let extra = extra_of(tool(&mut session, "render", "{'supports':true}")).expect("the plan");
    assert!(has_color(&extra, [0.05, 0.75, 0.95]));
    let mut field = plate(FIELD_ARCH, TREE);
    assert!(extra_of(tool(&mut field, "render", "{}")).is_none(), "a field part is drawn once, from its field");
    let drawn = field.review_plate().expect("the plate");
    assert!(drawn.render[0].indices.is_empty() && drawn.fields[0].is_some(), "the review traces it: no mesh is built, {} triangles", drawn.render[0].indices.len() / 3);
}

#[test]
fn a_section_cuts_the_plan_with_the_part() {
    let blue = |png: &[u8]| {
        let image = makepad_model::RgbaImage::from_png(png, &makepad_model::Limits::default()).unwrap();
        let count = |c: [u8; 3]| image.pixels.chunks_exact(4).filter(|p| p[..3] == c).count();
        (count([20, 110, 230]), count([60, 60, 60]))
    };
    let mut session = plate(ARCH, TREE);
    // Under the roof: the legs, and branches standing between them.
    let r = tool(&mut session, "section", "{'at':5,'supports':true}");
    assert!(r.ok, "{}", r.text);
    let a = Answer::of(&r);
    assert!(a.num(&["supports_crossing"]) > 0.0, "{}", a.0);
    let (supports, part) = blue(&r.images[0].1);
    assert!(supports > 0 && part > 0, "{supports} {part}");
    near(a.of_row(&a.rows(&["sections"])[0], "area_mm2"), 120.0, 6.0, "the legs");
    // The part's section alone is the plain tool, in its own frame.
    let plain = tool(&mut session, "section", "{'at':5,'axis':'y'}");
    assert!(plain.ok && blue(&plain.images[0].1).0 == 0, "{}", plain.text);
    // Without a plan the same plane has none.
    let mut off = plate(ARCH, "enable_support: \"0\"");
    let r = tool(&mut off, "section", "{'at':5,'supports':true}");
    assert_eq!(blue(&r.images[0].1).0, 0, "{}", r.text);
}

#[test]
fn verdicts_are_asked_for_off_the_worker_and_come_back_with_the_plan() {
    let mut session = plate(ARCH, TREE);
    let r = tool(&mut session, "supports", "{'verdicts':true}");
    let Some(RenderRequest::Verdicts(job)) = &r.render else { panic!("the search runs off the model worker: {}", r.text) };
    let a = Answer::text(crate::out::text(&job.run().unwrap()));
    assert!(a.num(&["verdicts", "comes_out"]) > 0.0, "supports under an open arch come out: {}", a.0);
    assert!(a.rows(&["parts"]).len() == 1 && !a.rows(&["verdicts", "rows"]).is_empty(), "{}", a.0);
}

#[test]
fn verdicts_read_worst_first_and_count_what_is_left_out() {
    use fab_loose::pieces::Piece;
    use fab_loose::verdict::{PieceVerdict, Verdict};
    let piece = |name: &str| Piece { name: name.into(), volume: 10.0, ..Default::default() };
    let pieces = [piece("a"), piece("b"), piece("c"), piece("d")];
    let of = |verdict| Some(PieceVerdict { verdict, sections_out: 1, sections: 2, turned: false });
    let results = [of(Verdict::Free), of(Verdict::TightReach), None, of(Verdict::Trapped)];
    let a = Answer::text(crate::out::text(&supports::verdicts_value(&pieces.iter().collect::<Vec<_>>(), &results, 3, 12.0, &|_| Vec::new())));
    let names: Vec<&str> = a.rows(&["rows"]).iter().map(|r| r.field("piece").unwrap().value.string().unwrap()).collect();
    assert_eq!(names, ["d", "b", "c"], "{}", a.0);
    assert_eq!([a.num(&["comes_out"]), a.num(&["with_work"]), a.num(&["no_way_out"]), a.num(&["more"])], [1.0, 1.0, 1.0, 1.0], "{}", a.0);
    assert_eq!(a.rows(&["rows"])[2].field("verdict").unwrap().value.string(), Some("not checked"));
}

#[test]
fn support_answers_never_speak_of_force() {
    use fab_loose::pieces::Piece;
    use fab_loose::verdict::{PieceVerdict, Verdict};
    let mut session = plate(ARCH, TREE);
    let pieces = [Piece { name: "shape support 1".into(), ..Default::default() }];
    let mut all = crate::out::text(&supports::verdicts_value(&pieces.iter().collect::<Vec<_>>(), &[Some(PieceVerdict { verdict: Verdict::NoReach, sections_out: 0, sections: 1, turned: true })], 4, 1.0, &|_| Vec::new()));
    all += &tool(&mut session, "supports", "{}").text;
    all += &tool(&mut session, "overhangs", "{}").text;
    all += &look_of(tool(&mut session, supports::VIEW_TOOL, "{'patch':'p1'}")).answer;
    let section = guide_section(Some("supports")).unwrap();
    for word in ["force", "strength", "newton", "contact_area", "contact area", " N "] {
        assert!(!all.to_lowercase().contains(word), "{word}: {all}");
        assert!(!section.to_lowercase().contains(&format!("{word}:")), "{word} in the guide");
    }
    for tool_name in ["fab.overhangs", "fab.overhang_view", "fab.supports", "m.support_block", "m.support_enforce", "fab.history"] {
        assert!(section.contains(tool_name), "the guide's loop names {tool_name}");
    }
    let m = manifest();
    for name in ["overhangs", supports::VIEW_TOOL, "supports"] {
        assert!(m.tools.iter().any(|t| t.name == name), "{name}");
    }
}

/// A box with a cave 20 mm square and 12 mm high, open to one side through a slot 4 mm wide: what
/// holds the cave's roof up, asked to stand inside, stands on its floor and can't leave through
/// the slot.
const POCKET: &str = "let m = mod.model\nlet block = m.box({size: [30mm 20mm 30mm] at: [0 10mm 0]})\nlet cave = m.box({size: [20mm 12mm 20mm] at: [0 10mm 0]})\nlet slot = m.box({size: [4mm 12mm 10mm] at: [0 10mm 12mm]})\nlet pocket = m.subtract(block, [cave slot])\nobject(pocket)\n";
/// The cave's roof: its underside and the slot's.
const CAVE_ROOF: f64 = 420.0;
/// A spool: two flanges 20 mm across on a stem 6 mm across. What holds the upper flange stands on the
/// lower one round the stem.
const SPOOL: &str = "let m = mod.model\nlet spool = m.union(m.union(m.cylinder({radius: 10mm height: 3mm at: [0 1.5mm 0]}), m.cylinder({radius: 3mm height: 12mm at: [0 9mm 0]})), m.cylinder({radius: 10mm height: 3mm at: [0 16.5mm 0]}))\nobject(spool)\n";

/// `fab.supports` with verdicts, the search run here.
fn verdicts(s: &mut Session, body: &str) -> Answer {
    let inner = body.trim().trim_start_matches('{').trim_end_matches('}');
    let r = tool(s, "supports", &format!("{{'verdicts':true{}{inner}}}", if inner.is_empty() { "" } else { "," }));
    let Some(RenderRequest::Verdicts(job)) = &r.render else { panic!("the search runs off the model worker: {}", r.text) };
    Answer::text(crate::out::text(&job.run().unwrap()))
}

/// A try of these script statements, as a tool's arguments.
fn try_of(statements: &str) -> String {
    format!("{{'edits':'{}'}}", statements.replace('"', "\\'"))
}

impl Answer {
    fn word_of(&self, row: &Node, k: &str) -> Option<String> {
        row.field(k).map(|f| f.value.string().map(str::to_string).unwrap_or_else(|| f.value.text(&self.0).to_string()))
    }
}

fn stuck_row(a: &Answer) -> &Node {
    a.rows(&["verdicts", "rows"]).iter().find(|r| a.word_of(r, "verdict").as_deref() == Some("no way out found")).unwrap_or_else(|| panic!("no piece without a way out in {}", a.0))
}

/// Asked to stand inside the part's hollows, as trees or normal support. (Coral joins this list when
/// slicing takes it.)
const INSIDE_TYPES: [&str; 2] = [TREE_INSIDE, NORMAL_INSIDE];

#[test]
fn a_piece_with_no_way_out_comes_with_a_block_and_a_try_shows_it_gone() {
    for settings in INSIDE_TYPES {
        let mut session = plate(POCKET, settings);
        let before = head(&session);
        let a = verdicts(&mut session, "{}");
        let stuck = a.num(&["verdicts", "no_way_out"]);
        assert!(stuck >= 1.0, "{settings}: {}", a.0);
        let row = stuck_row(&a);
        let piece = a.word_of(row, "piece").unwrap();
        let block = a.word_of(row, "block").unwrap_or_else(|| panic!("a stuck piece comes with a block: {}", a.0));
        assert!(block.starts_with("m.support_block(m.box({"), "{block}");
        assert_eq!(a.word_of(row, "script").as_deref(), Some("shape.cad.splash"), "{}", a.0);
        near(a.of_row(row, "gives_up_mm2"), CAVE_ROOF, 10.0, "the block gives up the cave's roof");

        let tried = verdicts(&mut session, &try_of(&block));
        assert_eq!(tried.at(&["tried"]).text(&tried.0), "true", "{}", tried.0);
        assert_eq!(tried.num(&["verdicts", "no_way_out"]), 0.0, "{settings}: the block takes the piece away: {}", tried.0);
        assert_eq!(tried.num(&["verdicts", "was", "no_way_out"]), stuck, "{}", tried.0);
        let gone = tried.rows(&["verdicts", "gone"]);
        assert!(gone.iter().any(|g| tried.word_of(g, "piece") == Some(piece.clone()) && tried.word_of(g, "verdict").as_deref() == Some("no way out found")), "the stuck piece is gone: {}", tried.0);
        let part = &tried.rows(&["parts"])[0];
        near(tried.of_row(part, "left_mm2"), CAVE_ROOF, 10.0, "the cave's roof is left to print unsupported");
        let pieces = a.of_row(&a.rows(&["parts"])[0], "pieces");
        assert_eq!(tried.of_row(part.field("was").map(|f| &f.value).unwrap_or_else(|| panic!("a try says what it was: {}", tried.0)), "pieces"), pieces);
        assert_eq!(head(&session), before, "a try commits nothing");
    }

    // An open arch's supports come out: nothing to take away.
    let a = verdicts(&mut plate(ARCH, TREE), "{}");
    assert!(a.num(&["verdicts", "comes_out"]) > 0.0 && !a.0.contains("block:"), "{}", a.0);
}

/// Printed support paths on the plate (plan frame, mm): (x, y) of every path point of part 0.
fn printed_points(session: &mut Session, args: &str) -> Vec<(f64, f64)> {
    let args = makepad_model::json::parse(args.replace('\'', "\"").as_bytes()).unwrap();
    let (ev, _) = session.support_plate(&args, None).unwrap();
    let (input, _) = EngineSlices::default().input(&ev).unwrap();
    let planned = makepad_fab_slice::live::plan_input_supports(makepad_fab_toolpath::par::Ctl::default(), &input).unwrap();
    let at = ev.frames[0].offset;
    planned[0].additions.iter().flat_map(|(_, _, _, layer)| layer.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).flat_map(|p| p.pts.iter().map(|q| (q.x_mm() + at[0], q.y_mm() + at[1]))).collect::<Vec<_>>()).collect()
}

/// What holds the spool's upper flange would ring the stem: normal columns print it in arcs, kept
/// apart by walls through the stem's axis both ways, every arc comes out, and the flange stays
/// held.
#[test]
fn a_ring_round_a_stem_prints_in_arcs_that_come_out_and_keep_the_flange_held() {
    let mut session = plate(SPOOL, NORMAL);
    let a = verdicts(&mut session, "{}");
    assert_eq!(a.num(&["verdicts", "no_way_out"]), 0.0, "{}", a.0);
    let part = &a.rows(&["parts"])[0];
    assert!(a.of_row(part, "pieces") >= 4.0, "the ring comes apart in arcs: {}", a.0);
    assert!(a.of_row(part, "left_mm2") < 40.0, "the flange stays held: {}", a.0);
    // What prints keeps off the stem's axis both ways, out to the flange's rim.
    let ev = session.last.clone().unwrap();
    let mesh = &ev.plate.parts[0].mesh;
    let middle = |k: usize| mesh.positions.iter().map(|p| p[k]).sum::<f64>() / mesh.positions.len() as f64;
    let printed = printed_points(&mut session, "{}");
    assert!(!printed.is_empty());
    let at = |p: &(f64, f64), k: usize| if k == 0 { p.0 } else { p.1 };
    for across in 0..2 {
        let along = 1 - across;
        let on: Vec<&(f64, f64)> = printed.iter().filter(|p| (at(p, across) - middle(across)).abs() < 0.5 && (at(p, along) - middle(along)).abs() < 10.0).collect();
        assert!(on.is_empty(), "{} printed points on the wall across axis {across}: {:?}", on.len(), &on[..on.len().min(5)]);
    }
}

/// The same ring held by trees prints whole, as the reference's trees print it: tips under the
/// flange and a roof joining them round the stem, kept on it by the lower flange. The upper flange
/// stays held, and the removal check tells the ring as having no way out. (Cutting it into arcs
/// that come out is a departure for after parity.)
#[test]
fn a_ring_of_trees_round_a_stem_keeps_the_flange_held_and_is_told_as_having_no_way_out() {
    let mut session = plate(SPOOL, TREE);
    let a = verdicts(&mut session, "{}");
    let part = &a.rows(&["parts"])[0];
    assert!(a.of_row(part, "left_mm2") < 40.0, "the flange stays held: {}", a.0);
    assert!(a.num(&["verdicts", "no_way_out"]) >= 1.0, "the ring does not come out, and is told so: {}", a.0);
}

/// A slab tilted 50 degrees, its lower edge cut off a millimetre up so it stands on a foot 2 mm
/// wide: its underside is steeper than the plan's 45 degree threshold, so the plan leaves it
/// alone. (On its bare edge it would start narrower than a line, a sharp tail the trees hold all
/// the way up, as the reference's do.)
const STEEP: &str = "let m = mod.model\nlet slab = m.subtract(m.box({size: [20mm 4mm 12mm] at: [0 12mm 0] rotate: [0 0 50deg]}), m.box({size: [40mm 8.1mm 20mm] at: [0 0 0]}))\nobject(slab)\n";

#[test]
fn a_face_the_threshold_leaves_comes_with_an_enforce_that_holds_it() {
    let mut session = plate(STEEP, TREE);
    // The underside, 20 x 12 mm and more with the box's rounding, as the bed sees it at 50 degrees
    // (over 150 mm²); the slab's lower end is a small overhang of its own (under 40).
    let underside = |a: &Answer| a.rows(&["rows"]).iter().find(|r| a.of_row(r, "area_mm2") > 100.0).cloned();
    let plain = Answer::of(&tool(&mut session, "overhangs", "{}"));
    assert!(underside(&plain).is_none(), "past the threshold it is no overhang: {}", plain.0);
    let a = Answer::of(&tool(&mut session, "overhangs", "{'angle':60}"));
    assert_eq!(a.num(&["limit_deg"]), 60.0, "{}", a.0);
    let row = &underside(&a).unwrap_or_else(|| panic!("flatter than 60 degrees it is: {}", a.0));
    assert!(a.of_row(row, "covered") < 0.5, "{}", a.0);
    let enforce = a.word_of(row, "enforce").unwrap_or_else(|| panic!("a face the plan leaves comes with an enforce: {}", a.0));
    assert!(enforce.starts_with("m.support_enforce(m.box({"), "{enforce}");
    assert_eq!(a.word_of(row, "script").as_deref(), Some("shape.cad.splash"));
    let tried = Answer::of(&tool(&mut session, "overhangs", &format!("{{'angle':60,{}", try_of(&enforce).trim_start_matches('{'))));
    let row = &underside(&tried).expect("the underside");
    assert!(tried.of_row(row, "covered") > 0.9 && row.field("enforce").is_none(), "enforced, the plan holds it: {}", tried.0);
}

/// Two slabs tilted 50 degrees, as [`STEEP`]: a 30 mm one standing on a foot, and a 20 mm one
/// 0.25 mm over its upper face, 5 mm up it. Past a 60 degree angle both undersides overhang, and
/// the plan's 45 degree threshold leaves both alone: the lower one's has room under its middle
/// for a support, the upper one's lies over the lower slab with none.
const LEAN_TO: &str = "let m = mod.model\nlet lower = m.subtract(m.box({size: [30mm 4mm 12mm] at: [0 15mm 0] rotate: [0 0 50deg]}), m.box({size: [60mm 6.448mm 20mm] at: [0 0 0]}))\nlet upper = m.box({size: [20mm 4mm 12mm] at: [-0.0415mm 21.562mm 0] rotate: [0 0 50deg]})\nobject(m.union(lower, upper))\n";

#[test]
fn a_face_with_no_room_under_its_middle_comes_with_no_enforce() {
    let mut session = plate(LEAN_TO, TREE);
    let a = Answer::of(&tool(&mut session, "overhangs", "{'angle':60}"));
    let underside = |over: &str| a.rows(&["rows"]).iter().find(|r| a.of_row(r, "area_mm2") > 100.0 && r.field("over").is_some_and(|o| o.value.text(&a.0) == over)).unwrap_or_else(|| panic!("no underside over the {over} in {}", a.0));
    let (bed, slab) = (underside("@bed"), underside("@part"));
    for (row, what) in [(bed, "the lower slab's"), (slab, "the upper slab's")] {
        assert!(a.of_row(row, "covered") < 0.5, "{what} is left: {}", a.0);
        assert!(row.field("given_up").is_none() && row.field("locked").is_none() && row.field("blocked").is_none(), "{what}: nothing says why it is left: {}", a.0);
    }
    assert!(bed.field("enforce").is_some(), "room under its middle: {}", a.0);
    assert!(slab.field("enforce").is_none(), "the gap under its middle holds no support: {}", a.0);
}

fn commit(s: &mut Session, tool_name: &str, fields: &[(&str, &str)]) {
    let mut all: Vec<(String, makepad_model::json::Value)> = vec![("request_id".into(), makepad_model::json::Value::Str(format!("r{}", head(s)))), ("expected".into(), makepad_model::json::Value::Str(head(s)))];
    all.extend(fields.iter().map(|(k, v)| (k.to_string(), makepad_model::json::Value::Str(v.to_string()))));
    let r = s.call(tool_name, &makepad_model::json::Value::Obj(all), None);
    assert!(r.ok, "{tool_name}: {}", r.text);
}

/// Every part on the plate: no piece left that won't come out, the cave's roof blocked on each
/// as much as where the block was made (`made`, set by the first call).
fn blocked_everywhere(s: &mut Session, parts: usize, what: &str, made: &mut Option<f64>) {
    let a = verdicts(s, "{}");
    let rows = a.rows(&["parts"]);
    assert_eq!(rows.len(), parts, "{what}: {}", a.0);
    assert_eq!(a.num(&["verdicts", "no_way_out"]), 0.0, "{what}: {}", a.0);
    for r in rows {
        assert_eq!(a.of_row(r, "pieces"), 0.0, "{what}: the block took every copy's piece: {}", a.0);
    }
    let o = Answer::of(&tool(s, "overhangs", "{}"));
    let roofs: Vec<&Node> = o.rows(&["rows"]).iter().filter(|r| r.field("blocked").is_some()).collect();
    assert_eq!(roofs.len(), parts, "{what}: each copy's roof is blocked: {}", o.0);
    for r in roofs {
        let share = o.of_row(r, "blocked");
        let made = *made.get_or_insert(share);
        assert!(made > 0.9 && (share - made).abs() < 0.05, "{what}: blocked {share}, where made {made}: {}", o.0);
    }
}

#[test]
fn an_override_goes_with_its_object_moved_turned_scaled_and_copied() {
    let mut session = plate(POCKET, TREE_INSIDE);
    // Turned and scaled before the block is made: it is written in the object's own frame.
    commit(&mut session, "place", &[("edits", "place(\"shape\", at: [120mm 110mm], rotate: 90°, scale: 1.25)")]);
    let a = verdicts(&mut session, "{}");
    let block = a.word_of(stuck_row(&a), "block").expect("a block");
    commit(&mut session, "apply", &[("script", "shape.cad.splash"), ("edits", &block)]);
    let h = &session.last.as_ref().unwrap().handles[0];
    assert!(h.rotate[2] == 90.0 && h.scale == [1.25; 3] && h.at == Some([120.0, 110.0]), "the placement stays: {:?}", (h.rotate, h.scale, h.at));
    let mut made = None;
    blocked_everywhere(&mut session, 1, "where it was made", &mut made);
    commit(&mut session, "place", &[("edits", "place(\"shape\", at: [60mm 200mm], rotate: 215°, scale: 0.8)")]);
    blocked_everywhere(&mut session, 1, "moved, turned and shrunk", &mut made);
    commit(&mut session, "duplicate", &[("object", "shape"), ("mode", "linked")]);
    blocked_everywhere(&mut session, 2, "a linked copy", &mut made);
    commit(&mut session, "duplicate", &[("object", "shape"), ("mode", "copy")]);
    blocked_everywhere(&mut session, 3, "a copy with its own script", &mut made);
}

/// The user's Lattice cube (27 mm at a 13.2 mm period, 1.6 times) on its trees from the bed only,
/// 2 mm off its walls at 46 deg, and only outside its footprint: sliced as the app slices it, with
/// the sliced checks' last report.
fn user_cube() -> (Session, Arc<EngineSlices>) {
    user_cube_with("1")
}

/// [`user_cube`] with `footprint` its only-outside-its-footprint setting: "1" on, "0" off.
fn user_cube_with(footprint: &str) -> (Session, Arc<EngineSlices>) {
    let slices = Arc::new(EngineSlices::default());
    let mut session = Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
    session.slices = Some(slices.clone());
    let scripts = BTreeMap::from([("lattice_cube.cad.splash".to_string(), include_str!("../../app/resources/lattice_cube.cad.splash").to_string())]);
    let project = "use mod.fab.*\nsettings({layer_height: \"0.2\" enable_support: \"1\" support_type: \"tree(auto)\" support_object_xy_distance: \"2\" support_threshold_angle: \"46\" fab_tree_outside_footprint: \"1\"})\nscript(\"lattice_cube.cad.splash\", params: {lattice_size: 27mm lattice_period: 13.2195mm})\nplace(\"lattice_cube\", object: \"lattice\", at: [80mm 165mm], scale: 1.6, name: \"Lattice cube\", settings: {sparse_infill_density: \"100%\" support_on_build_plate_only: \"1\"})\n".replace("fab_tree_outside_footprint: \"1\"", &format!("fab_tree_outside_footprint: \"{footprint}\""));
    session.model = FabModel::load("supports", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    session.rebuild().unwrap_or_else(|e| panic!("{e}"));
    (session, slices)
}

/// With its trees in under its footprint too, the cube prints: the sliced checks find no error.
/// Where the lattice starts from its low points on the cube's sides (z 38.8), each first layer is
/// a strip 0.19 mm wide with nothing under it, narrower than a line: the walls print it all the
/// same, so the trees hold it. Nor is any layer told as uncovered overhang: what reaches past the
/// threshold is held, and the slivers by which the lattice's walls lean out a layer, steeper than
/// it, need nothing under them.
#[test]
fn a_cube_with_trees_under_its_footprint_too_prints() {
    let (mut session, slices) = user_cube_with("0");
    session.overhang_list(8, None).unwrap_or_else(|e| panic!("{e}"));
    let report = slices.report.lock().unwrap().clone().expect("the sliced checks ran");
    let errors: Vec<String> = report.issues.iter().filter(|i| i.severity == fab_printability::Severity::Error).map(|i| format!("{} on layer {:?} at {:?}: {} ({:.2} mm2)", i.kind, i.layer, i.at_mm, i.message, i.area_mm2)).collect();
    assert!(errors.is_empty(), "Print is refused: {errors:?}");
    let uncovered: Vec<(Option<u32>, f64)> = report.issues.iter().filter(|i| i.kind == "uncovered_overhang").map(|i| (i.layer, i.area_mm2)).collect();
    assert!(uncovered.is_empty(), "told as uncovered (layer, mm2): {uncovered:?}");
}

/// With its trees only outside its footprint, what of the cube's overhang lies deeper in than a
/// branch reaches is the person's choice: the Slice panel lists it apart, as inside the footprint
/// and unsupported by the setting, the share held in its header is of what the trees are asked to
/// hold, and nothing of it refuses the print: the sliced checks find no error, and the plan tells
/// it as information, once, which the sliced checks do not tell again as uncovered.
#[test]
fn a_cube_with_trees_outside_its_footprint_holds_what_they_reach_and_prints() {
    let (mut session, slices) = user_cube();
    let list = session.overhang_list(8, None).unwrap_or_else(|e| panic!("{e}"));
    let kept: Vec<&fab_printability::OverhangLine> = list.lines.iter().filter(|l| l.chosen()).collect();
    assert!(kept.len() == 1 && kept[0].unheld == Some(fab_printability::Unheld::Footprint) && kept[0].part == "Lattice cube", "one line for the cube's footprint: {list:?}");
    let (asked, left, kept_out) = (list.area_mm2, list.left_mm2.expect("the plate plans supports"), kept[0].area_mm2);
    let held = 1.0 - left / asked;
    eprintln!("lattice cube, trees outside its footprint: {asked:.1} mm2 asked of the trees, {left:.1} left ({:.1}% held), {kept_out:.1} mm2 kept out", held * 100.0);
    assert!(kept_out > 500.0, "the overhang inside the cube is the setting's: {kept_out:.1} mm2");
    assert!(asked > 20.0 && held >= 0.5, "held of what the trees are asked to hold, the kept-out area left out: {asked:.1} mm2, {left:.1} left");
    assert!(list.lines.iter().take_while(|l| !l.chosen()).count() + kept.len() == list.lines.len(), "the kept-out line comes after the ranked ones: {list:?}");
    // What it ranks are outer faces, and the trees reach each: none shows as part held for its
    // points on the cube's side, the footprint's edge, which lie over what it keeps them out of.
    let ranked = list.lines.iter().filter(|l| !l.chosen()).flat_map(|l| std::iter::once(l).chain(&l.members));
    let short: Vec<(Option<usize>, f64)> = ranked.filter_map(|l| l.held().filter(|h| *h < 0.9).map(|h| (l.id, h))).collect();
    assert!(short.is_empty(), "faces the trees reach, shown as part held (patch, share): {short:?}");
    let report = slices.report.lock().unwrap().clone().expect("the sliced checks ran");
    let errors: Vec<String> = report.issues.iter().filter(|i| i.severity == fab_printability::Severity::Error).map(|i| format!("{} on layer {:?} at {:?}: {}", i.kind, i.layer, i.at_mm, i.message)).collect();
    assert!(errors.is_empty(), "Print is refused: {errors:?}");
    // Told once, as information, and not again layer by layer as uncovered: what amber is left
    // lies outside what the cube keeps its trees out of.
    let told: Vec<&fab_printability::Issue> = report.issues.iter().filter(|i| i.kind == "support_footprint").collect();
    assert!(told.len() == 1 && told[0].severity == fab_printability::Severity::Info && told[0].message.contains("inside the footprint, unsupported by your setting") && told[0].area_mm2 > 500.0, "{:?}", told.iter().map(|i| (i.severity, &i.message)).collect::<Vec<_>>());
    // One number: the list's line says what Printability says, the overhang layer by layer.
    assert!((kept_out - told[0].area_mm2).abs() < 0.05, "the list's {kept_out:.1} mm2 kept out, Printability's {:.1}", told[0].area_mm2);
    let uncovered: Vec<&fab_printability::Issue> = report.issues.iter().filter(|i| i.kind == "uncovered_overhang").collect();
    let amber: f64 = uncovered.iter().map(|i| i.area_mm2).sum();
    assert!(amber < 0.1 * told[0].area_mm2, "{amber:.0} mm2 told as uncovered, of {:.0} kept out: {:?}", told[0].area_mm2, uncovered.iter().map(|i| (i.layer, &i.message)).collect::<Vec<_>>());
}

/// A test cube on the arch's far top corner: what it cuts, on the plate, and its sides.
fn arch_with_cube(script: &str, side: f64) -> (Session, fab_plate::Bounds) {
    let mut session = plate(script, TREE);
    let ev = session.last.clone().unwrap();
    let (object, b) = (ev.plate.parts[0].object.clone(), ev.plate.parts[0].bounds());
    let at = [b.max[0] - side / 2.0 + 1.0, b.max[1] - side / 2.0 + 1.0, b.max[2] - side / 2.0 + 1.0];
    let add = format!("{{'request_id':'cube','expected':'{}','action':'add','boxes':[{{'object':'{object}','at':[{},{},{}],'size':{side},'name':'Neck'}}]}}", head(&session), at[0], at[1], at[2]);
    let r = tool(&mut session, "test_print", &add);
    assert!(r.ok, "{}", r.text);
    let cut = fab_plate::Bounds { min: std::array::from_fn(|k| (at[k] - side / 2.0).max(b.min[k])), max: std::array::from_fn(|k| (at[k] + side / 2.0).min(b.max[k])) };
    (session, cut)
}

fn pieces_of(r: Reply) -> Arc<Evaluated> {
    assert!(r.ok, "{}", r.text);
    match r.render {
        Some(RenderRequest::Pieces(job)) => job.ev.clone(),
        other => panic!("no pieces: {other:?}"),
    }
}

/// What the views and the AI's pictures show of a test print is the piece: cut where the cube cuts,
/// in place by name, on the bed where it prints while the Slicer slices it. A field part draws from
/// its field cut, so a point past the cube is empty and one inside is as it was.
#[test]
fn a_test_prints_piece_shows_cut_in_place_and_on_the_bed() {
    for (script, field) in [(ARCH, false), (FIELD_ARCH, true)] {
        let (mut session, cut) = arch_with_cube(script, 6.0);
        let whole = session.last.clone().unwrap();
        assert_eq!(whole.fields[0].is_some(), field);

        // By name, in place: what the cube leaves of the part, closed.
        let ev = pieces_of(tool(&mut session, "render", "{'test_print':'Neck'}"));
        let part = &ev.plate.parts[0];
        assert_eq!((ev.plate.parts.len(), part.name.as_str()), (1, "Neck"));
        assert!(part.object.ends_with("#0"), "{}", part.object);
        let b = part.bounds();
        for k in 0..3 {
            near(b.min[k], cut.min[k], 1e-6, "the piece's low side");
            near(b.max[k], cut.max[k], 1e-6, "the piece's high side");
        }
        if let Some(from) = &whole.fields[0] {
            let shown = ev.fields[0].as_ref().expect("a field part's piece is a field");
            assert!(ev.render[0].indices.is_empty(), "drawn from its field");
            assert_eq!(shown.to_printer, *from.distance().2, "it stands where it is");
            // Plate points as the two fields read them.
            let (to_shape, whole_shape) = (fab_model::weave::invert(from.distance().2), from.distance().0);
            let read = |root: &makepad_csg_sdf::SdfNode, q: [f64; 3]| {
                let p = fab_model::params::xf_point(&to_shape, q);
                makepad_csg_sdf::distance(root, makepad_csg_math::dvec3(p[0], p[1], p[2]))
            };
            // In the arch's roof: past the cube over its opening, and inside it over its leg.
            let past = [cut.min[0] - 1.0, cut.min[1] + 2.0, cut.min[2] + 2.0];
            let inside = [cut.min[0] + 1.0, cut.min[1] + 2.0, cut.min[2] + 2.0];
            assert!(read(&whole_shape.root, past) < 0.0, "the part goes on past the cube here");
            assert!(read(&shown.shape.root, past) > 0.0, "past the cube the piece is empty");
            assert!(read(&whole_shape.root, inside) < 0.0 && read(&shown.shape.root, inside) < 0.0, "inside it is as it was");
        } else {
            assert!(!ev.render[0].indices.is_empty() && ev.fields[0].is_none(), "drawn from its mesh");
        }

        // The whole plate with what it cuts tinted, nothing of it moved.
        let r = tool(&mut session, "render", "{'test_print':'Neck','with_plate':true}");
        match r.render {
            Some(RenderRequest::Sheets { extra: Some(extra), objects, .. }) => assert!(objects.is_empty() && !extra.indices.is_empty(), "the tinted surface"),
            other => panic!("no tinted plate: {other:?}"),
        }

        // A name that is none is said, with what there is.
        let r = tool(&mut session, "render", "{'test_print':'Knee'}");
        assert!(!r.ok && r.text.contains("Knee") && r.text.contains("Neck"), "{}", r.text);

        // On the bed: standing on it, in its own place, while the plate is the plate's.
        assert!(matches!(tool(&mut session, "render", "{}").render, Some(RenderRequest::Sheets { .. })), "Whole plate: the plate");
        session.test_mode = true;
        let bed = pieces_of(tool(&mut session, "render", "{}"));
        let b = bed.plate.parts[0].bounds();
        near(b.min[2], 0.0, 1e-6, "it stands on the bed");
        for k in 0..3 {
            near(b.max[k] - b.min[k], cut.max[k] - cut.min[k], 1e-6, "its size");
        }
        let to_input = session.slice_input(&**session.slices.as_ref().unwrap(), &bed).unwrap().0;
        let printed = to_input.pieces.as_ref().expect("the test plate's input");
        assert_eq!(printed.parts[0].part.bounds().min, b.min, "the view's piece stands where the slice's does");
        assert_eq!(printed.parts[0].part.bounds().max, b.max);
        assert_eq!(to_input.plate.parts[0].object, bed.plate.parts[0].object, "the slice's parts are the view's");
        assert!(Arc::ptr_eq(&bed, &pieces_of(tool(&mut session, "render", "{}"))), "nothing changed: the same pieces");

        // None ticked: said, and how to go on.
        let off = format!("{{'request_id':'off','expected':'{}','action':'set','name':'Neck','on':false}}", head(&session));
        let r = tool(&mut session, "test_print", &off);
        assert!(r.ok, "{}", r.text);
        let r = tool(&mut session, "render", "{}");
        assert!(!r.ok && r.text.contains("none is ticked"), "{}", r.text);
        assert!(!tool(&mut session, "check", "{}").ok);
    }
}

/// A posed piece shows where the slice has it: its pose and its fall, the same box either way.
#[test]
fn a_posed_pieces_view_stands_where_its_slice_does() {
    use makepad_fab_slice::piece;
    let flat = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];
    for script in [ARCH, FIELD_ARCH] {
        let session = plate(script, TREE);
        let ev = session.last.clone().unwrap();
        let b = ev.plate.parts[0].bounds();
        let center = std::array::from_fn(|k| (b.min[k] + b.max[k]) / 2.0);
        let tests = [piece::TestPrint { name: "Flat".into(), cube: piece::Cube { center, size: [8.0; 3], turn: [0.0; 3] }, pose: Some(flat), brim: false }];
        let plan = piece::test_plan(&ev.plate, &ev.machine, &tests).unwrap();
        let shown = crate::test_view::plate(&ev, &tests, &plan, true);
        let (whole, _) = session.slices.as_ref().unwrap().input(&ev).unwrap();
        let input = piece::planned_input(&whole, &tests, &plan);
        let (view, slice) = (shown.plate.parts[0].bounds(), input.pieces.as_ref().unwrap().parts[0].part.bounds());
        for k in 0..3 {
            near(view.min[k], slice.min[k], 1e-6, "the low side");
            near(view.max[k], slice.max[k], 1e-6, "the high side");
        }
    }
}

/// A test print's piece lays flat on the face a normal, a side of its box or a point on it names,
/// and a click on it, in place or where it prints, names the face it meets: that face goes down.
/// A side that cuts nothing and a point off the piece are refused in words; the bottom down is as
/// printed.
#[test]
fn a_test_prints_piece_lays_flat_on_a_normal_a_side_a_point_or_a_click() {
    let down = [0.0, 0.0, -1.0];
    for script in [ARCH, FIELD_ARCH] {
        // A 5 mm corner of a leg and the roof: its box cuts the left, front and bottom.
        let (mut session, cut) = arch_with_cube(script, 6.0);
        let mut n = 0;
        let mut lay = |session: &mut Session, how: &str| {
            n += 1;
            let body = format!("{{'request_id':'lay{n}','expected':'{}','action':'lay','name':'Neck',{how}}}", head(session));
            tool(session, "test_print", &body)
        };
        let laid = |session: &Session| {
            let ev = session.last.clone().unwrap();
            ev.test_print_lay(session.model.project().test_print("Neck").unwrap())
        };
        let turned = |r: [[f64; 3]; 3], v: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| (0..3).map(|k| r[i][k] * v[k]).sum()) };
        let goes_down = |r: Option<[[f64; 3]; 3]>, v: [f64; 3], what: &str| {
            let r = r.unwrap_or_else(|| panic!("{what}: the piece is as printed"));
            let d = turned(r, v);
            for k in 0..3 {
                near(d[k], down[k], 1e-3, what);
            }
        };

        let r = lay(&mut session, "'face':'front'");
        assert!(r.ok, "{}", r.text);
        goes_down(laid(&session), [0.0, -1.0, 0.0], "the front side down");
        let r = lay(&mut session, "'normal':[3,0,0]");
        assert!(r.ok, "{}", r.text);
        goes_down(laid(&session), [1.0, 0.0, 0.0], "the normal's face down");
        let top = [(cut.min[0] + cut.max[0]) / 2.0, (cut.min[1] + cut.max[1]) / 2.0, cut.max[2]];
        let r = lay(&mut session, &format!("'point':[{},{},{}]", top[0], top[1], top[2] - 0.2));
        assert!(r.ok, "{}", r.text);
        goes_down(laid(&session), [0.0, 0.0, 1.0], "the face at the point down");

        let r = lay(&mut session, "'face':'top'");
        assert!(!r.ok && r.text.contains("top side cuts nothing"), "a side past the part: {}", r.text);
        let r = lay(&mut session, &format!("'point':[{},{},{}]", top[0], top[1], top[2] + 3.0));
        assert!(!r.ok && r.text.contains("mm from its piece"), "a point off the piece: {}", r.text);
        let r = lay(&mut session, "'face':'bottom'");
        assert!(r.ok, "{}", r.text);
        assert_eq!(laid(&session), None, "the bottom down is as printed");

        // A click from above meets the piece's top in place.
        let edit = session.lay_by_ray(Some("Neck"), [top[0], top[1], top[2] + 50.0], down).unwrap();
        let fab_model::project::TestEdit::LayFlat { name, normal } = edit else { panic!("{edit:?}") };
        assert_eq!(name, "Neck");
        for k in 0..3 {
            near(normal[k], [0.0, 0.0, 1.0][k], 1e-3, "the face a click in place meets");
        }
        // Laid on its front, it prints with its back up: a click from above where it prints
        // meets that, as its part stands.
        assert!(lay(&mut session, "'face':'front'").ok);
        let ev = session.last.clone().unwrap();
        let t = session.model.project().test_print("Neck").unwrap();
        let tests = [makepad_fab_slice::piece::TestPrint { name: "Neck".into(), cube: { let b = ev.test_print_box(t).unwrap(); makepad_fab_slice::piece::Cube { center: b.center, size: b.size, turn: b.turn } }, pose: ev.test_print_lay(t), brim: false }];
        let plan = makepad_fab_slice::piece::test_plan(&ev.plate, &ev.machine, &tests).unwrap();
        let bed = crate::test_view::plate(&ev, &tests, &plan, true).plate.parts[0].bounds();
        let over = [(bed.min[0] + bed.max[0]) / 2.0, (bed.min[1] + bed.max[1]) / 2.0, bed.max[2] + 50.0];
        let edit = session.lay_by_ray(None, over, down).unwrap();
        let fab_model::project::TestEdit::LayFlat { normal, .. } = edit else { panic!("{edit:?}") };
        for k in 0..3 {
            near(normal[k], [0.0, 1.0, 0.0][k], 1e-3, "the face a click where it prints meets");
        }
    }
}

/// `fab.overhangs` reads the test slice while the Slicer slices it: the piece's underside, on the
/// bed where the piece prints, and `supports` draws its plan.
#[test]
fn overhangs_and_supports_read_a_test_pieces_slice_while_the_slicer_slices_it() {
    let mut session = plate(ARCH, TREE);
    let whole = Answer::of(&tool(&mut session, "overhangs", "{}")).num(&["area_mm2"]);
    let ev = session.last.clone().unwrap();
    let (object, b) = (ev.plate.parts[0].object.clone(), ev.plate.parts[0].bounds());
    // A leg and the roof's start over the opening, all its depth and height: standing on the
    // leg, the roof's underside overhangs 4 mm of it.
    let at = [b.max[0] - 5.0, b.min[1] + 7.0, (b.min[2] + b.max[2]) / 2.0];
    let add = format!("{{'request_id':'cube','expected':'{}','action':'add','boxes':[{{'object':'{object}','at':[{},{},{}],'size':[8,16,16],'name':'Middle'}}]}}", head(&session), at[0], at[1], at[2]);
    let r = tool(&mut session, "test_print", &add);
    assert!(r.ok, "{}", r.text);
    session.test_mode = true;
    let piece = pieces_of(tool(&mut session, "render", "{}"));
    let a = Answer::of(&tool(&mut session, "overhangs", "{}"));
    let area = a.num(&["area_mm2"]);
    assert!(area > 0.0 && area < whole, "the piece's underside: {area} of the arch's {whole}: {}", a.0);
    // Where the piece stands, not where the arch does.
    let on = piece.plate.parts[0].bounds();
    let rows = a.rows(&["rows"]);
    assert!(!rows.is_empty(), "{}", a.0);
    let extra = match tool(&mut session, "render", "{'supports':true}").render {
        Some(RenderRequest::Pieces(job)) => job.extra.clone().expect("the piece's plan"),
        other => panic!("no pieces: {other:?}"),
    };
    // Plate mm. A tree may stand beside what it holds, never as far off as the arch on the plate.
    let span = |k: usize| extra.positions.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), p| (a.min(p[k]), b.max(p[k])));
    let arch = ev.plate.parts[0].bounds();
    assert!(on.min[0] - arch.max[0] > 20.0 || arch.min[0] - on.max[0] > 20.0 || on.min[1] - arch.max[1] > 20.0 || arch.min[1] - on.max[1] > 20.0, "the piece prints away from the arch: {on:?} {arch:?}");
    for k in 0..2 {
        let (lo, hi) = span(k);
        assert!(lo as f64 >= on.min[k] - 10.0 && hi as f64 <= on.max[k] + 10.0, "its supports stand by it, axis {k}: {lo}..{hi} of {on:?}");
    }
}
