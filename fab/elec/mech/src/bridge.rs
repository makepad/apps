//! From a committed board to Fab's CAD language (C8): what the electronics host calls per board
//! unit on each commit, and what it hands Fab's model.
//! Board names resolve in the edited design's boards first, then the stock boards.
//!
//! ```text
//! let changed = inputs.update(board_inputs(&unit.name, board, &unit.circuit, &drills));
//! if changed { fab_model.set_env(inputs.env()) }   // the one gated call
//! ```
//!
//! Setting the env is the slicing path: an input whose hash moved rebuilds the scripts that read
//! it, and their parts re-slice. Until the output cut-off and the preview/commit split are settled
//! (gate 3 verdict 1b, 1c), the host makes that one call for committed snapshots only; nothing else
//! here changes when they are.
//!
//! Cost: `board_inputs` is `board_mech_with` and two Splash writers, O(parts + drills + keepouts +
//! port vertices), plus one cache read per mesh file, on the pool per changed unit, never per
//! frame; `CaseInputs::update` compares two hashes.
//!
//! Before the electronics host runs, [`LazyBoards`] gives scripts the same two kinds from the
//! start: registering them names the kinds and builds nothing, and a script's first read of a
//! board builds that board's inputs on the thread evaluating it (the model's worker), once.

use crate::board_mech::{board_mech_with, dot, normalised, port_axes, BodyBox, BoardMech};
use crate::checks::{posed, CaseSolid};
use crate::csg::Csg;
use crate::generate::{ElecGenInput, ElecGenerator, PanelAt, ParamValue};
use fab_model::eval::Evaluated;
use fab_model::program::{GenPart, GenSolid, Generator, PanelBase, ScriptEnv};
use fab_elec_board::{Board, DrillHole, Item};
use fab_elec_circuit::Circuit;
use fab_elec_core::{to_mm, BoardId, Canon, CompRef, FeatureRef, Side, Writer};
use fab_elec_design::options::Field;
use fab_elec_design::Design;
use fab_elec_fixtures::stock;
use fab_elec_parts::BodySource;
use makepad_csg_graph::{Op, TriMesh};
use makepad_model_script::lang::{input_key, Input, Inputs, Lazy, Library, Provide, RecordValue, Refusal};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The input kind of a board's case-facing summary: `board("name")`.
pub const BOARD: &str = "board";
/// The input kind of a board's part bodies: `board_bodies("name")`.
pub const BOARD_BODIES: &str = "board_bodies";
/// The host generator for a mesh-sourced body in the case's frame.
pub const ELEC_BODY: &str = "elec_body";

/// The enclosure prelude's source.
pub const ENCLOSURE: &str = include_str!("enclosure.splash");
/// The heat-set insert prelude's source: `heat_insert`, `heat_seat`.
pub const HEAT_SET: &str = include_str!("heat_set.splash");

/// The libraries the host adds to the language: the heat-set inserts, then the enclosure prelude
/// (which seats them).
pub fn libraries() -> Vec<Library> {
    vec![Library { file: "heat_set.splash".into(), source: HEAT_SET.into() }, Library { file: "enclosure.splash".into(), source: ENCLOSURE.into() }]
}

/// A case environment with the body generator and the inputs it resolves against.
pub fn case_env(inputs: Inputs, bodies: Arc<dyn BodyLookup>) -> ScriptEnv {
    ScriptEnv::new(libraries(), inputs).with_generator(ELEC_BODY, body_generator(bodies))
}

/// One board's two inputs, named as scripts name it (its unit's name).
#[derive(Clone, Debug)]
pub struct BoardInputs {
    pub name: String,
    pub board: Input,
    pub bodies: Input,
    /// Mesh-sourced bodies, including those whose files could not be read, keyed by body path.
    pub mesh_bodies: Arc<BTreeMap<String, BodyBox>>,
}

/// A board unit's inputs from its committed snapshot: the board, its circuit (the parts' bodies and
/// pins) and the board's `drills()`.
pub fn board_inputs(name: &str, board: &Board, circuit: &Circuit, drills: &[DrillHole]) -> BoardInputs {
    let mech = board_mech_with(board, circuit, drills);
    let (bodies, mesh_keys) = mech.bodies_splash_with_mesh_keys();
    let mut hash = Writer::hasher();
    mech.bodies_hash.canon(&mut hash);
    mesh_keys.canon(&mut hash);
    BoardInputs {
        name: name.to_string(),
        board: Input { source: mech.splash().into(), hash: mech.hash.0 },
        bodies: Input { source: bodies.into(), hash: hash.finish().0 },
        mesh_bodies: Arc::new(mech.bodies.into_iter().filter(|body| matches!(&body.source, Some(BodySource::Mesh(_))))
            .map(|body| (body.path.clone(), body)).collect()),
    }
}

/// Mesh-sourced bodies from the same board snapshots that supply the script's inputs.
pub trait BodyLookup: Send + Sync {
    fn body(&self, board: &str, path: &str) -> Option<BodyBox>;
}

/// Flatten a record's leaves in key order, joining nested keys with dots (`usb.height`).
/// Numbers keep the script's units (lengths in metres); ids and flags become board fields.
/// Text and list leaves are refused at their dotted key; the root must be a record.
pub fn record_leaves(record: &RecordValue) -> Result<Vec<(String, Field)>, Refusal> {
    fn visit(key: String, value: &RecordValue, leaves: &mut Vec<(String, Field)>) -> Result<(), Refusal> {
        let field = match value {
            RecordValue::Rec(fields) => {
                for (name, value) in fields {
                    let key = if key.is_empty() { name.clone() } else { format!("{key}.{name}") };
                    visit(key, value, leaves)?;
                }
                return Ok(());
            }
            RecordValue::Num(value) => Field::Num(*value),
            RecordValue::Id(value) => Field::Id(value.clone()),
            RecordValue::Bool(value) => Field::Bool(*value),
            RecordValue::Text(_) | RecordValue::List(_) => {
                return Err(Refusal { key, message: "is not a number, id or flag".into() });
            }
        };
        leaves.push((key, field));
        Ok(())
    }

    if !matches!(record, RecordValue::Rec(_)) {
        return Err(Refusal { key: String::new(), message: "is not a record".into() });
    }
    let mut leaves = Vec::new();
    visit(String::new(), record, &mut leaves)?;
    Ok(leaves)
}

/// The edited boards' values, then stock boards built on first read, kept across commits.
#[derive(Clone, Debug)]
pub struct CaseInputs {
    boards: BTreeMap<String, BoardInputs>,
    stock: Lazy,
    stock_boards: Arc<LazyBoards>,
}

impl Default for CaseInputs {
    fn default() -> Self {
        let stock_boards = Arc::new(LazyBoards::stock());
        Self { boards: BTreeMap::new(), stock: Lazy::new(stock_boards.clone()), stock_boards }
    }
}

impl CaseInputs {
    /// Freeze the edited boards and their body lookup for this evaluation environment.
    pub fn env(&self) -> ScriptEnv {
        case_env(self.inputs(), Arc::new(self.clone()))
    }

    /// Take a board's newest inputs. True when it is new or a hash moved: only then does the host
    /// set the env again.
    pub fn update(&mut self, inputs: BoardInputs) -> bool {
        let same = self.boards.get(&inputs.name).is_some_and(|had| had.board.hash == inputs.board.hash && had.bodies.hash == inputs.bodies.hash);
        if !same {
            self.boards.insert(inputs.name.clone(), inputs);
        }
        !same
    }

    /// Forget a board the design no longer has. True when there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        self.boards.remove(name).is_some()
    }

    /// The edited boards' `board(name)` and `board_bodies(name)` values, then stock boards
    /// built on first read.
    pub fn inputs(&self) -> Inputs {
        let mut values = BTreeMap::new();
        for (name, b) in &self.boards {
            values.insert((BOARD.to_string(), name.clone()), b.board.clone());
            values.insert((BOARD_BODIES.to_string(), name.clone()), b.bodies.clone());
        }
        Inputs { kinds: vec![BOARD.into(), BOARD_BODIES.into()], values, lazy: Some(self.stock.clone()) }
    }
}

impl BodyLookup for CaseInputs {
    fn body(&self, board: &str, path: &str) -> Option<BodyBox> {
        match self.boards.get(board) {
            Some(inputs) => inputs.mesh_bodies.get(path).cloned(),
            None => self.stock_boards.body(board, path),
        }
    }
}

/// The edited design's boards first, then [`fab_elec_fixtures::stock::boards()`], each built on
/// its first read ([`Inputs::lazy`]). The edited design itself is made at the first read too.
/// A failed design serves nothing, including stock boards.
/// The provider's values are fixed for its life.
///
/// Cost: nothing until a read; a board's first read is its `board_inputs` (both kinds at once);
/// every later read is a map lookup under a lock held for that lookup alone.
pub struct LazyBoards {
    design: Option<Box<dyn Fn() -> Result<Design, String> + Send + Sync>>,
    made: OnceLock<Result<Design, String>>,
    boards: Mutex<BTreeMap<String, BoardInputs>>,
}

impl std::fmt::Debug for LazyBoards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyBoards").field("built", &self.built()).finish_non_exhaustive()
    }
}

impl LazyBoards {
    pub fn new(design: impl Fn() -> Design + Send + Sync + 'static) -> LazyBoards {
        LazyBoards::fallible(move || Ok(design()))
    }

    /// A design that may not build (a plate's own documents): when it does not, no board is
    /// served, none of the stock catalogue's either, and every read is refused with `why`.
    pub fn fallible(design: impl Fn() -> Result<Design, String> + Send + Sync + 'static) -> LazyBoards {
        LazyBoards { design: Some(Box::new(design)), made: OnceLock::new(), boards: Mutex::default() }
    }

    /// The stock boards alone, built on first read.
    pub fn stock() -> LazyBoards {
        LazyBoards { design: None, made: OnceLock::new(), boards: Mutex::default() }
    }

    /// The language's inputs: `board(name)` and `board_bodies(name)`, served by `boards`.
    pub fn inputs(boards: Arc<LazyBoards>) -> Inputs {
        Inputs { kinds: vec![BOARD.into(), BOARD_BODIES.into()], values: BTreeMap::new(), lazy: Some(Lazy::new(boards)) }
    }

    /// Register both board inputs and their body generator without building a board.
    pub fn env(boards: Arc<LazyBoards>) -> ScriptEnv {
        case_env(Self::inputs(boards.clone()), boards)
    }

    /// The boards built so far.
    pub fn built(&self) -> usize {
        self.boards.lock().unwrap().len()
    }

    fn design(&self) -> Option<&Design> {
        self.design.as_ref().and_then(|design| self.made.get_or_init(|| design()).as_ref().ok())
    }

    fn failure(&self) -> Option<&str> {
        self.design.as_ref().and_then(|design| self.made.get_or_init(|| design()).as_ref().err().map(String::as_str))
    }

    fn board(&self, name: &str) -> Option<BoardInputs> {
        // A failed design serves nothing, the stock catalogue included.
        if self.failure().is_some() {
            return None;
        }
        if let Some(had) = self.boards.lock().unwrap().get(name) {
            return Some(had.clone());
        }
        // Built outside the lock: a second reader meanwhile builds the same values.
        let built = if let Some(unit) = self.design().and_then(|design| design.units.iter().find(|u| u.name == name && u.board.is_some())) {
            let board = unit.board.as_deref()?;
            board_inputs(&unit.name, board, &unit.circuit, &board.drills())
        } else {
            let entry = stock::board(name)?;
            let design = (entry.build)();
            let unit = design.units.iter().find(|u| u.name == name && u.board.is_some())?;
            let board = unit.board.as_deref()?;
            board_inputs(&unit.name, board, &unit.circuit, &board.drills())
        };
        Some(self.boards.lock().unwrap().entry(built.name.clone()).or_insert(built).clone())
    }
}

impl BodyLookup for LazyBoards {
    fn body(&self, board: &str, path: &str) -> Option<BodyBox> {
        self.board(board)?.mesh_bodies.get(path).cloned()
    }
}

/// A record's first read builds that variant once; later reads are a lookup.
impl Provide for LazyBoards {
    fn provide(&self, kind: &str, name: &str) -> Option<Input> {
        match kind {
            BOARD => Some(self.board(name)?.board),
            BOARD_BODIES => Some(self.board(name)?.bodies),
            _ => None,
        }
    }

    fn provide_with(&self, kind: &str, name: &str, record: &RecordValue) -> Result<Option<Input>, Refusal> {
        if let Some(why) = self.failure() {
            return Err(Refusal { key: String::new(), message: why.to_string() });
        }
        let pick = |b: &BoardInputs| match kind {
            BOARD => Some(b.board.clone()),
            BOARD_BODIES => Some(b.bodies.clone()),
            _ => None,
        };
        let key = input_key(name, Some(record));
        if let Some(had) = self.boards.lock().unwrap().get(&key) {
            return Ok(pick(had));
        }
        // Built outside the lock, as with a board's default measurements.
        if self.design().is_some_and(|design| design.units.iter().any(|u| u.name == name && u.board.is_some())) {
            return Err(Refusal { key: String::new(), message: format!("'{name}' is this design's own board and takes no record") });
        }
        let Some(entry) = stock::board(name) else { return Ok(None) };
        let Some(with) = entry.with else {
            return Err(Refusal { key: String::new(), message: format!("'{name}' takes no record") });
        };
        let leaves = record_leaves(record)?;
        let design = with(&leaves).map_err(|refused| Refusal { key: refused.key, message: refused.message })?;
        let Some(unit) = design.units.iter().find(|u| u.name == name && u.board.is_some()) else { return Ok(None) };
        let Some(board) = unit.board.as_deref() else { return Ok(None) };
        let built = board_inputs(&key, board, &unit.circuit, &board.drills());
        let input = pick(&built);
        self.boards.lock().unwrap().entry(key).or_insert(built);
        Ok(input)
    }

    fn refused(&self, _kind: &str, _name: &str) -> Option<Refusal> {
        self.failure().map(|why| Refusal { key: String::new(), message: why.to_string() })
    }

    fn names(&self, _kind: &str) -> Vec<String> {
        if self.failure().is_some() {
            return Vec::new();
        }
        let mut names = Vec::new();
        if let Some(design) = self.design() {
            names.extend(design.units.iter().filter(|u| u.board.is_some()).map(|u| u.name.clone()));
        }
        // A stock board the edited design shadows is named once. O(stock x names).
        for entry in stock::boards() {
            if !names.iter().any(|name| name == entry.name) {
                names.push(entry.name.to_string());
            }
        }
        names
    }

    fn hash(&self, kind: &str, name: &str) -> Option<[u8; 16]> {
        let boards = self.boards.lock().unwrap();
        let had = boards.get(name)?;
        match kind {
            BOARD => Some(had.board.hash),
            BOARD_BODIES => Some(had.bodies.hash),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_core::{mm, Seg};
    use fab_elec_fixtures::ledctl;

    #[test]
    fn a_commit_sets_the_env_only_when_a_board_input_moved() {
        let design = ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_deref().expect("the fixture has a board");
        let mut inputs = CaseInputs::default();
        assert!(inputs.update(board_inputs(&unit.name, board, &unit.circuit, &board.drills())), "a new board sets the env");
        let values = inputs.inputs();
        assert_eq!(values.kinds, vec![BOARD.to_string(), BOARD_BODIES.to_string()], "two kinds: the board and its bodies");
        let keys: Vec<(&str, &str)> = values.values.keys().map(|(k, n)| (k.as_str(), n.as_str())).collect();
        assert_eq!(keys, vec![(BOARD, "ledctl"), (BOARD_BODIES, "ledctl")], "each board under its unit's name, once per kind");
        assert!(values.values[&(BOARD.to_string(), "ledctl".to_string())].source.contains("envelope:"), "board() is the case-facing summary");

        let mut routed = board.clone();
        if let Some(Seg::Line(end) | Seg::Arc { end, .. }) = routed.tracks[0].path.segs.last_mut() {
            end.x += mm(1.0);
        }
        assert!(!inputs.update(board_inputs(&unit.name, &routed, &unit.circuit, &routed.drills())), "a routing commit leaves the env as it is");

        let comp = unit.circuit.comp("usb.j").expect("fixture has usb.j").id;
        let mut moved = board.clone();
        moved.placed.iter_mut().find(|p| p.comp == comp).expect("fixture places usb.j").at.y += mm(2.0);
        assert!(inputs.update(board_inputs(&unit.name, &moved, &unit.circuit, &moved.drills())), "a connector move sets the env");
        assert!(inputs.remove("ledctl") && inputs.inputs().values.is_empty(), "a removed board leaves no eager values");
        assert!(libraries().iter().any(|l| l.source.contains("fn enclosure(")) && libraries()[0].source.contains("fn heat_seat("), "the preludes are the language's libraries, the heat-set inserts the enclosure seats first");
    }

    /// Why: a plate's broken electronics documents must never read as the stock board of the same name (11-15).
    #[test]
    fn a_design_that_does_not_build_serves_no_board_and_says_why() {
        let why = "ledctl.pcb.splash:3:5: an outline needs three points";
        let boards = LazyBoards::fallible(move || Err(why.into()));
        assert!(boards.provide(BOARD, "ledctl").is_none(), "a failed design must not fall back to the stock ledctl board");
        let refusal = boards.refused(BOARD, "ledctl").expect("a failed design explains why the board read was refused");
        assert_eq!(refusal.message, why, "a plain board read preserves the design failure message");
        let record = RecordValue::Rec(BTreeMap::new());
        let refusal = boards.provide_with(BOARD, "ledctl", &record).expect_err("a failed design refuses a board read with options");
        assert_eq!(refusal.message, why, "a board read with options preserves the design failure message");
        assert!(boards.names(BOARD).is_empty(), "a failed design advertises no boards, including stock boards");
        assert!(LazyBoards::new(Design::default).refused(BOARD, "ledctl").is_none(), "a design that built refuses nothing");
    }

    #[test]
    fn a_board_name_resolves_in_the_edited_design_then_the_stock_boards() {
        let stock = LazyBoards::new(Design::default);
        let from_stock = stock.provide(BOARD, "ledctl").expect("a name the edited design lacks comes from the stock boards");
        assert!(stock.provide(BOARD, "no_such_board").is_none(), "an unknown name has no value (the language refuses it)");
        assert!(stock.names(BOARD).contains(&"ledctl".to_string()), "the refusal's list names the stock boards");

        let stock_bodies = stock.provide(BOARD_BODIES, "ledctl").expect("the stock board's bodies");
        assert_eq!(stock.hash(BOARD, "ledctl"), Some(from_stock.hash), "a built stock board is cached");
        let mut edited = ledctl();
        let unit = &mut edited.units[0];
        let comp = unit.circuit.comp("usb.j").expect("fixture has usb.j").id;
        let board = Arc::make_mut(unit.board.as_mut().expect("the fixture has a board"));
        board.placed.iter_mut().find(|p| p.comp == comp).expect("fixture places usb.j").at.y += mm(2.0);
        let edited = LazyBoards::new(move || edited.clone());
        let own = edited.provide(BOARD_BODIES, "ledctl").expect("the edited design has its board");
        assert_ne!(own.hash, stock_bodies.hash, "the edited design's board comes before the stock board of the same name");
        assert_eq!(edited.names(BOARD).iter().filter(|name| *name == "ledctl").count(), 1, "the refusal's list names a shadowed stock board once");

        let mut inputs = CaseInputs::default();
        let lazy = inputs.inputs().lazy.expect("the host's env serves the stock boards");
        assert!(lazy.provider.provide(BOARD, "ledctl").is_some(), "with no edited board, the host's env still reads the stock board");
        assert_eq!(inputs.clone().inputs().lazy.map(|l| l.id), Some(lazy.id), "one provider identity for the host's life");
        assert!(!inputs.remove("ledctl"), "the stock board is no edited board to remove");
    }
}

/// The case's shells in `e` for board `board`, mapped into that board's frame (mm), for the case
/// checks: the parts whose script built them with the enclosure, found by the board frame the
/// enclosure gives them (`frame: Frame{name: "case:<board>:<role>" ..}`), never by name; the loose
/// parts it frames too (the keys' pins `pin:<ref>`, the knobs `knob:<ref>`) are not shells. One
/// part per role: a script that emits the printed lid and the lid in place gives the lid once,
/// mapped back onto the base through its frame either way. Cost: O(objects + those parts' vertices), on
/// the model host's worker after a plate rebuild.
pub fn case_solids(e: &Evaluated, board: &str) -> Vec<CaseSolid> {
    let prefix = format!("case:{board}:");
    let framed: std::collections::HashMap<&str, (&str, &[[f64; 4]; 3])> = e
        .objects
        .iter()
        .filter_map(|o| {
            let (name, frame) = o.frame.as_ref()?;
            Some((o.name.as_str(), (name.strip_prefix(prefix.as_str())?, frame)))
        })
        .collect();
    let mut roles: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for i in 0..e.plate.parts.len() + e.plate.excluded.len() {
        let part = e.placed_part(i);
        let (Some(&(role, frame)), Some(to_plate)) = (framed.get(part.object.as_str()), e.placed_frame(i)) else { continue };
        if roles.contains(&role) || role.starts_with("pin:") || role.starts_with("knob:") {
            continue;
        }
        roles.push(role);
        // plate -> model (the placement's inverse), then model -> board (the frame's).
        let to_board = affine_mul(&affine_inverse(frame), &affine_inverse(to_plate));
        out.push(CaseSolid::mapped(&part.name, &part.mesh.positions, &part.mesh.triangles, &to_board));
    }
    out
}

/// The clear light pipes (`m.optics` parts) printed as bodies of `board`'s case parts, in the
/// board's frame (mm): [`CheckOpts::pipes`](crate::CheckOpts::pipes). A pipe shares its case
/// part's placed object (`body_of`), so the case part's frame places it.
pub fn case_pipes(e: &Evaluated, board: &str) -> Vec<CaseSolid> {
    let prefix = format!("case:{board}:");
    let framed: std::collections::HashMap<&str, &[[f64; 4]; 3]> = e.objects.iter()
        .filter_map(|o| {
            let (name, frame) = o.frame.as_ref()?;
            name.strip_prefix(prefix.as_str())?;
            Some((o.name.as_str(), frame))
        })
        .collect();
    let placed = e.plate.parts.len() + e.plate.excluded.len();
    let groups: std::collections::HashMap<&str, &[[f64; 4]; 3]> = (0..placed)
        .filter_map(|i| {
            let part = e.placed_part(i);
            Some((part.body_of.as_deref()?, *framed.get(part.object.as_str())?))
        })
        .collect();
    (0..placed).filter_map(|i| {
        let part = e.placed_part(i);
        part.optics.as_ref()?;
        let frame = groups.get(part.body_of.as_deref()?)?;
        let to_board = affine_mul(&affine_inverse(frame), &affine_inverse(e.placed_frame(i)?));
        Some(CaseSolid::mapped(&part.name, &part.mesh.positions, &part.mesh.triangles, &to_board))
    }).collect()
}

/// What a part of a board's case is in the closed case.
#[derive(Clone, Debug, PartialEq)]
pub enum CaseRole {
    /// A shell (`base`, `lid`) or another framed part, by its role.
    Shell(String),
    /// A key's printed pin (keycap), on the switch at this port feature's part.
    Key(String),
    /// A knob, on the shaft of this part.
    Knob(String),
    /// A body printed in a framed part (a light pipe in the lid): the host part's role, and the
    /// feature the body links (a pipe's LED port), if any.
    Body { host: String, link: Option<FeatureRef> },
}

/// One placed part of a board's case where it sits in the closed case.
#[derive(Clone, Debug, PartialEq)]
pub struct CasePose {
    /// Its placed index (`Evaluated::placed_part`).
    pub part: usize,
    pub role: CaseRole,
    /// Its plate coordinates (mm) to the board's frame (mm, z up from the board's bottom face).
    pub to_board: [[f64; 4]; 3],
}

/// What a part of a closed case does with its running board.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CaseBinding {
    /// Nothing: a shell, or a body that links no part.
    None,
    /// A keycap: the switch it presses, and how far (mm) the switch's stem travels.
    Key { comp: CompRef, travel: f64 },
    /// A knob: the part whose shaft it turns, and the shaft's axis on the board (mm), which it
    /// turns round.
    Knob { comp: CompRef, axis: [f64; 2] },
    /// A light pipe: the LED whose light it carries.
    Pipe { comp: CompRef },
}

/// What `pose`'s part does with the board `mech` is of: a keycap's switch by its press port, a
/// knob's part by its shaft, a pipe's LED by its link. O(ports).
pub fn case_binding(pose: &CasePose, mech: &BoardMech) -> CaseBinding {
    let port = |path: &str, kind: fab_elec_parts::BodyPortKind| mech.ports.iter().find(|p| p.path == path && p.kind == kind);
    let comp = |comp| CompRef { board: mech.unit, comp };
    match &pose.role {
        CaseRole::Key(path) => port(path, fab_elec_parts::BodyPortKind::Press)
            .map_or(CaseBinding::None, |p| CaseBinding::Key { comp: comp(p.comp), travel: to_mm(p.travel) }),
        CaseRole::Knob(path) => port(path, fab_elec_parts::BodyPortKind::Shaft)
            .map_or(CaseBinding::None, |p| CaseBinding::Knob { comp: comp(p.comp), axis: [to_mm(p.at[0]), to_mm(p.at[1])] }),
        CaseRole::Body { link: Some(FeatureRef::Port(c, _)), .. } if c.board == mech.unit => CaseBinding::Pipe { comp: *c },
        _ => CaseBinding::None,
    }
}

/// The case part a board is fixed to: its `base` shell, else its first shell. None without one.
pub fn board_base(poses: &[CasePose]) -> Option<&CasePose> {
    poses.iter().find(|p| matches!(&p.role, CaseRole::Shell(role) if role == "base"))
        .or_else(|| poses.iter().find(|p| matches!(p.role, CaseRole::Shell(_))))
}

/// Board `board` printed bare, with no case around it: the placed part that links
/// `board:<board>` and whose object carries that frame (the board as Electronics draws it), and
/// its plate coordinates to the board's frame, as a case part's ([`CasePose::to_board`]).
/// O(placed parts + objects).
pub fn bare_board(e: &Evaluated, board: &str) -> Option<(usize, [[f64; 4]; 3])> {
    let key = format!("board:{board}");
    (0..e.plate.parts.len() + e.plate.excluded.len()).find_map(|i| {
        let part = e.placed_part(i);
        if part.link.as_deref() != Some(key.as_str()) {
            return None;
        }
        let (name, frame) = e.objects.iter().find(|o| o.name == part.object)?.frame.as_ref()?;
        (*name == key).then_some(())?;
        Some((i, affine_mul(&affine_inverse(frame), &affine_inverse(e.placed_frame(i)?))))
    })
}

/// Every placed part of `board`'s case where it sits in the closed case: each part whose object
/// carries a `case:<board>:<role>` frame (the shells, the keys' pins `pin:<ref>`, the knobs
/// `knob:<ref>`, whatever pose they print in), and each body printed in one of them (a light pipe
/// with the LED it links). O(placed parts + objects).
pub fn case_poses(e: &Evaluated, board: &str) -> Vec<CasePose> {
    let prefix = format!("case:{board}:");
    let framed: std::collections::HashMap<&str, (&str, &[[f64; 4]; 3])> = e.objects.iter()
        .filter_map(|o| {
            let (name, frame) = o.frame.as_ref()?;
            Some((o.name.as_str(), (name.strip_prefix(prefix.as_str())?, frame)))
        })
        .collect();
    let placed = e.plate.parts.len() + e.plate.excluded.len();
    let hosts: std::collections::HashMap<&str, (&str, &[[f64; 4]; 3])> = (0..placed)
        .filter_map(|i| {
            let part = e.placed_part(i);
            Some((part.body_of.as_deref()?, *framed.get(part.object.as_str())?))
        })
        .collect();
    let mut seen: Vec<&str> = Vec::new();
    (0..placed).filter_map(|i| {
        let part = e.placed_part(i);
        let to_plate = e.placed_frame(i)?;
        // A framed object's first part is the object itself; its other parts (and parts of its
        // group under another object) are bodies printed in it.
        let own = framed.get(part.object.as_str()).filter(|_| !seen.contains(&part.object.as_str()) && part.optics.is_none());
        let (role, frame) = match own {
            Some(&(role, frame)) => {
                seen.push(part.object.as_str());
                let role = if let Some(r) = role.strip_prefix("pin:") { CaseRole::Key(r.to_string()) }
                    else if let Some(r) = role.strip_prefix("knob:") { CaseRole::Knob(r.to_string()) }
                    else { CaseRole::Shell(role.to_string()) };
                (role, frame)
            }
            None => {
                let &(host, frame) = part.body_of.as_deref().and_then(|g| hosts.get(g))
                    .or_else(|| framed.get(part.object.as_str()))?;
                (CaseRole::Body { host: host.to_string(), link: part.link.as_deref().and_then(crate::keys::feature_of_key) }, frame)
            }
        };
        Some(CasePose { part: i, role, to_board: affine_mul(&affine_inverse(frame), &affine_inverse(to_plate)) })
    }).collect()
}

fn affine_mul(a: &[[f64; 4]; 3], b: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum::<f64>() + if c == 3 { a[r][3] } else { 0.0 }))
}

/// The inverse of an invertible affine map (row-major 3x4).
fn affine_inverse(m: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    let c = |r: usize, k: usize| m[(r + 1) % 3][(k + 1) % 3] * m[(r + 2) % 3][(k + 2) % 3] - m[(r + 1) % 3][(k + 2) % 3] * m[(r + 2) % 3][(k + 1) % 3];
    let det = (0..3).map(|k| m[0][k] * c(0, k)).sum::<f64>();
    let inv: [[f64; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|k| c(k, r) / det));
    std::array::from_fn(|r| {
        let t = -(0..3).map(|k| inv[r][k] * m[k][3]).sum::<f64>();
        [inv[r][0], inv[r][1], inv[r][2], t]
    })
}

enum Param {
    Num(f64),
    Bool(bool),
    Text(String),
    Colour(u32),
}

fn parse_params(text: &str) -> Result<Vec<(String, Param)>, String> {
    let mut scanner = ParamScanner { text, offset: 0 };
    scanner.expect('{')?;
    let mut params = Vec::new();
    let mut keys = std::collections::BTreeSet::new();
    if !scanner.take('}') {
        loop {
            scanner.space();
            let start = scanner.offset;
            let key = if scanner.peek() == Some('"') {
                scanner.string()?
            } else {
                scanner.ident()?.to_string()
            };
            if !keys.insert(key.clone()) {
                return Err(format!("at byte {start}: duplicate parameter '{key}'"));
            }
            scanner.expect(':')?;
            if let Some(value) = scanner.value(&key)? {
                params.push((key, value));
            }
            if scanner.take('}') {
                break;
            }
            scanner.expect(',')?;
        }
    }
    scanner.space();
    if scanner.peek().is_some() {
        return Err(scanner.error("expected the end of the parameters"));
    }
    Ok(params)
}

struct ParamScanner<'a> {
    text: &'a str,
    offset: usize,
}

impl<'a> ParamScanner<'a> {
    fn peek(&self) -> Option<char> {
        self.text[self.offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        Some(c)
    }

    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            let _ = self.bump();
        }
    }

    fn take(&mut self, c: char) -> bool {
        self.space();
        if self.peek() != Some(c) {
            return false;
        }
        let _ = self.bump();
        true
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.take(c) {
            Ok(())
        } else {
            Err(self.error(&format!("expected '{c}'")))
        }
    }

    fn error(&self, message: &str) -> String {
        format!("at byte {}: {message}", self.offset)
    }

    fn ident(&mut self) -> Result<&'a str, String> {
        let start = self.offset;
        if !self.peek().is_some_and(|c| c == '_' || c.is_alphabetic()) {
            return Err(self.error("expected an identifier"));
        }
        let _ = self.bump();
        while self.peek().is_some_and(|c| c == '_' || c.is_alphanumeric()) {
            let _ = self.bump();
        }
        Ok(&self.text[start..self.offset])
    }

    fn hex(&mut self) -> Result<u32, String> {
        let digit = self.peek().and_then(|c| c.to_digit(16))
            .ok_or_else(|| self.error("expected a hexadecimal digit"))?;
        let _ = self.bump();
        Ok(digit)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            let start = self.offset;
            let c = self.bump().ok_or_else(|| self.error("unterminated string"))?;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let escaped = self.bump().ok_or_else(|| self.error("unterminated escape"))?;
                    out.push(match escaped {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '0' => '\0',
                        'x' => {
                            let n = self.hex()? * 16 + self.hex()?;
                            if n > 0x7f {
                                return Err(format!("at byte {start}: non-ASCII byte escape"));
                            }
                            char::from_u32(n).ok_or_else(|| self.error("invalid byte escape"))?
                        }
                        'u' => {
                            if self.bump() != Some('{') {
                                return Err(format!("at byte {start}: expected a Unicode escape"));
                            }
                            let mut n = 0;
                            let mut digits = 0;
                            while self.peek() != Some('}') {
                                if digits == 6 {
                                    return Err(self.error("Unicode escape has more than six digits"));
                                }
                                n = n * 16 + self.hex()?;
                                digits += 1;
                            }
                            let _ = self.bump();
                            if digits == 0 {
                                return Err(format!("at byte {start}: empty Unicode escape"));
                            }
                            char::from_u32(n).ok_or_else(|| format!("at byte {start}: invalid Unicode scalar"))?
                        }
                        _ => return Err(format!("at byte {start}: invalid string escape")),
                    });
                }
                c if c.is_control() => return Err(format!("at byte {start}: unescaped control character")),
                c => out.push(c),
            }
        }
    }

    fn digits(&mut self) -> Result<(), String> {
        let start = self.offset;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            let _ = self.bump();
        }
        if start == self.offset {
            Err(self.error("expected a digit"))
        } else {
            Ok(())
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.offset;
        if self.peek() == Some('-') {
            let _ = self.bump();
        }
        self.digits()?;
        if self.peek() == Some('.') {
            let _ = self.bump();
            self.digits()?;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let _ = self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                let _ = self.bump();
            }
            self.digits()?;
        }
        self.text[start..self.offset].parse::<f64>().ok().filter(|n| n.is_finite())
            .ok_or_else(|| format!("at byte {start}: expected a finite number"))
    }

    fn value(&mut self, key: &str) -> Result<Option<Param>, String> {
        self.space();
        let start = self.offset;
        let compound = || format!("'{key}': a list or an object is not a generator parameter");
        let value = match self.peek() {
            Some('[' | '{') => return Err(compound()),
            Some('"') => Some(Param::Text(self.string()?)),
            Some('@') => {
                let _ = self.bump();
                Some(Param::Text(self.ident()?.to_string()))
            }
            Some('#') => {
                let _ = self.bump();
                let mut colour = 0;
                for _ in 0..8 {
                    colour = colour * 16 + self.hex()?;
                }
                Some(Param::Colour(colour))
            }
            Some('-' | '0'..='9') => Some(Param::Num(self.number()?)),
            Some(c) if c == '_' || c.is_alphabetic() => {
                let name = self.ident()?;
                self.space();
                if self.peek() == Some('{') {
                    return Err(compound());
                }
                match name {
                    "nil" => None,
                    "true" => Some(Param::Bool(true)),
                    "false" => Some(Param::Bool(false)),
                    _ => return Err(format!("at byte {start}: expected a parameter value")),
                }
            }
            _ => return Err(self.error("expected a parameter value")),
        };
        self.space();
        if !matches!(self.peek(), Some(',' | '}')) {
            return Err(self.error("expected ',' or '}' after the parameter value"));
        }
        Ok(value)
    }
}

/// The board items each generated part last produced, already in its board's frame.
pub type BoardItemSink = Arc<Mutex<BTreeMap<String, (BoardId, Vec<Item>)>>>;

/// A mesh body in the case frame: the convex hull of its posed mesh, or its plan box when the file
/// cannot be read. The hull, not the triangles: the union with the case's other solids costs about
/// the same as boxes, where supplier triangles make it several times slower, and it is the shape
/// the fit check reads. Cost: one cache stat per mesh request and one posed copy of the points,
/// linear in mesh size, on the evaluation worker. Unchanged requests do nothing: the evaluator
/// caches by generator, parameters (including the mesh key) and input stamps.
pub fn body_generator(bodies: Arc<dyn BodyLookup>) -> Generator {
    Arc::new(move |input| {
        let fail = |message| format!("'{}': {message}", input.part);
        if input.kind != ELEC_BODY {
            return Err(fail(format!("expected generator '{ELEC_BODY}', got '{}'", input.kind)));
        }
        let params: BTreeMap<_, _> = parse_params(input.params).map_err(&fail)?.into_iter().collect();
        let text = |key: &str| match params.get(key) {
            Some(Param::Text(value)) => Ok(value.as_str()),
            _ => Err(fail(format!("{key}: expected text"))),
        };
        let mm = |key: &str| match params.get(key) {
            Some(Param::Num(value)) if (value * 1000.0).is_finite() => Ok(value * 1000.0),
            _ => Err(fail(format!("{key}: expected a finite length"))),
        };
        let (board, path) = (text("board")?, text("ref")?);
        let (cx, cy, y0, thickness) = (mm("cx")?, mm("cy")?, mm("y0")?, mm("thickness")?);
        let body = bodies.body(board, path)
            .ok_or_else(|| fail(format!("body '{path}' was not found on board '{board}'")))?;
        let to_model = |[x, y, z]: [f64; 3]| [x - cx, y0 + z, cy - y];
        let cached = match params.get("mesh") {
            None => None,
            Some(Param::Text(_)) => match &body.source {
                Some(BodySource::Mesh(path)) => fab_elec_import::cache::MeshCache::shared().get(std::path::Path::new(path)).ok(),
                _ => None,
            },
            _ => return Err(fail("mesh: expected text or nil".into())),
        };
        let solid: GenSolid = if let Some(cached) = cached {
            let point = |p: [f32; 3]| to_model(posed(&body.pose, p.map(f64::from)));
            let points = Arc::new(cached.mesh.points().map(point).collect::<Vec<_>>());
            Arc::new(move |graph| graph.push(Op::Hull { points: (*points).clone() }))
        } else {
            // a model of no stated height whose file is not here stands as the scene's plain body
            let height = to_mm(if body.height > 0 { body.height } else { crate::scene::NO_BODY_HEIGHT });
            let lo = if body.side == Side::Top { thickness } else { -height };
            let at = to_model([(to_mm(body.min.x) + to_mm(body.max.x)) / 2.0,
                (to_mm(body.min.y) + to_mm(body.max.y)) / 2.0, lo + height / 2.0]);
            let size = [to_mm(body.max.x) - to_mm(body.min.x), height, to_mm(body.max.y) - to_mm(body.min.y)];
            Arc::new(move |graph| {
                let source = graph.push(Op::Box { size, radius: 0.0 })?;
                graph.push(Op::Transform { source, at, rotate: [0.0; 3], scale: [1.0; 3] })
            })
        };
        Ok(GenPart { solid, adds: vec![], cuts: vec![], claims: vec![] })
    })
}

/// The host generator for `kind`, built on `f`, resolving anchors in `design`. Register one per
/// kind in the ScriptEnv (ScriptEnv::with_generator) for each design generation: the closure holds
/// that generation's design, and the evaluator caches its results by request and generator.
/// Multi-body parts reach the model as their first body for now.
/// Claims cost one small mesh per panel cut per generator run (cached by the panel stage),
/// plus a few point tests per port of the anchored board.
pub fn elec_generator(kind: &'static str, f: ElecGenerator, design: Arc<Design>, sink: BoardItemSink) -> Generator {
    Arc::new(move |input| {
        let part = input.part;
        let fail = |message| format!("'{part}': {message}");
        if input.kind != kind {
            return Err(fail(format!("expected generator '{kind}', got '{}'", input.kind)));
        }
        let mut params = parse_params(input.params).map_err(&fail)?;
        let on = params.iter().position(|(key, _)| key == "on")
            .ok_or_else(|| fail("on: names the part it sits on (\"D1\" or \"board/D1\")".into()))?;
        let Param::Text(anchor) = params.remove(on).1 else {
            return Err(fail("on: expected a reference as text (\"D1\" or \"board/D1\")".into()));
        };
        params.retain(|(key, _)| key != "panel");
        let (board_name, refdes) = match anchor.split_once('/') {
            Some((board, refdes)) if !board.is_empty() && !refdes.is_empty() && !refdes.contains('/') => (Some(board), refdes),
            None if !anchor.is_empty() => (None, anchor.as_str()),
            _ => return Err(fail(format!("on: invalid anchor '{anchor}'; use refdes or board/refdes"))),
        };
        let unit = if let Some(name) = board_name {
            design.units.iter().find(|unit| unit.name == name)
                .ok_or_else(|| fail(format!("on: board '{name}' was not found")))?
        } else {
            let mut units = design.units.iter().filter(|unit| unit.board.is_some());
            let unit = units.next().ok_or_else(|| fail("on: no board was found".into()))?;
            if units.next().is_some() {
                return Err(fail("on: more than one board; use board/refdes".into()));
            }
            unit
        };
        let board = unit.board.as_deref().ok_or_else(|| fail(format!("on: unit '{}' has no board", unit.name)))?;
        let placed = board.placed.iter().find(|placed| placed.refdes == refdes)
            .ok_or_else(|| fail(format!("on: refdes '{refdes}' was not found on board '{}'", unit.name)))?;
        let pose = crate::scene::placement_pose(placed, board.stackup.thickness);
        let axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|axis| pose.rotate(axis));
        let part_to_board = std::array::from_fn(|r| [axes[0][r], axes[1][r], axes[2][r], to_mm(pose.t[r])]);
        let frame = input.panel.and_then(|panel| case_frame(&panel.pose));
        let part_to_model = frame.as_ref().map_or(part_to_board, |frame| affine_mul(frame, &part_to_board));
        let panel = input.panel.filter(|_| frame.is_some()).and_then(|panel| {
            let origin = part_to_model.map(|row| row[3]);
            let direction = part_to_model.map(|row| row[2]);
            let hit = panel.ray(origin, direction)?;
            Some(PanelAt {
                back: hit.back,
                thickness: hit.thickness,
                // A rigid rotation's inverse is its transpose.
                normal: std::array::from_fn(|r| (0..3).map(|k| part_to_model[k][r] * hit.normal[k]).sum()),
            })
        });
        let values: Vec<_> = params.iter().map(|(key, value)| {
            (key.as_str(), match value {
                Param::Num(n) => ParamValue::Num(*n),
                Param::Bool(b) => ParamValue::Bool(*b),
                Param::Text(s) => ParamValue::Text(s),
                Param::Colour(c) => ParamValue::Colour(*c),
            })
        }).collect();
        let output = f(&ElecGenInput { part, params: &values, panel }).map_err(&fail)?;
        let claims = match (input.panel, frame.as_ref()) {
            (Some(panel), Some(frame)) if !output.panel_cuts.is_empty() => {
                panel_claims(board, &unit.circuit, panel, frame, &part_to_model, &output.panel_cuts).map_err(&fail)?
            }
            _ => Vec::new(),
        };
        let body = output.bodies.into_iter().next().ok_or_else(|| fail(format!("generator '{kind}' returned no bodies")))?;
        let items = crate::derived::place(output.board_items, placed);
        let generated = GenPart {
            solid: generated_solid(body.csg, &part_to_model),
            adds: output.panel_adds.into_iter().map(|csg| generated_solid(csg, &part_to_model)).collect(),
            cuts: output.panel_cuts.into_iter().map(|csg| generated_solid(csg, &part_to_model)).collect(),
            claims,
        };
        let entry = (part.to_string(), (unit.id, items));
        let previous = {
            let mut items = sink.lock().map_err(|_| fail("board item sink is poisoned".into()))?;
            items.insert(entry.0, entry.1)
        };
        // Dispose of the replaced payload after releasing the publication lock.
        drop(previous);
        Ok(generated)
    })
}

/// Sample the same clearance-expanded opening that board_inputs gives the enclosure.
fn panel_claims(
    board: &Board,
    circuit: &Circuit,
    panel: &PanelBase,
    board_to_model: &[[f64; 4]; 3],
    part_to_model: &[[f64; 4]; 3],
    cuts: &[Csg],
) -> Result<Vec<String>, String> {
    let meshes = cuts.iter().map(crate::lower::mesh).collect::<Result<Vec<_>, _>>()?;
    let model_to_part = affine_inverse(part_to_model);
    let point = |m: &[[f64; 4]; 3], p: [f64; 3]| m.map(|row| dot([row[0], row[1], row[2]], p) + row[3]);
    let mech = board_mech_with(board, circuit, &board.drills());
    let mut claims = Vec::new();
    for port in &mech.ports {
        let Some(polygon) = port.opening.polygons().first() else { continue };
        let vertices: Vec<_> = polygon.outer.iter().map(|p| [to_mm(p.x), to_mm(p.y)]).collect();
        if vertices.len() < 3 { continue; }
        let mut area = 0.0;
        let mut centre = [0.0; 2];
        for (a, b) in vertices.iter().zip(vertices.iter().cycle().skip(1)) {
            let cross = a[0] * b[1] - b[0] * a[1];
            area += cross;
            for i in 0..2 { centre[i] += (a[i] + b[i]) * cross; }
        }
        if area.abs() < 1e-12 { continue; }
        let centre = centre.map(|c| c / (3.0 * area));
        let (u, v) = port_axes(port.dir);
        let dir = normalised(board_to_model.map(|row| dot([row[0], row[1], row[2]], port.dir)));
        let samples = vertices.iter().map(|p| {
            let delta = [centre[0] - p[0], centre[1] - p[1]];
            let distance = delta[0].hypot(delta[1]);
            let pull = if distance > 0.05 { 0.05 / distance } else { 1.0 };
            [p[0] + delta[0] * pull, p[1] + delta[1] * pull]
        }).chain(std::iter::once(centre));
        let covered = samples.into_iter().all(|p| {
            let mouth = point(board_to_model, std::array::from_fn(|i| to_mm(port.at[i]) + u[i] * p[0] + v[i] * p[1]));
            let Some(hit) = panel.ray(mouth, dir).filter(|hit| hit.thickness > 0.0) else { return false };
            let sample = point(&model_to_part, std::array::from_fn(|i| mouth[i] + dir[i] * (hit.back + hit.thickness / 2.0)));
            meshes.iter().any(|mesh| cut_contains(mesh, sample))
        });
        if covered {
            if let Some(key) = crate::keys::feature_key(&FeatureRef::Port(CompRef { board: mech.unit, comp: port.comp }, port.index)) {
                claims.push(key);
            }
        }
    }
    Ok(claims)
}

/// Parity along a tilted ray; coincident hits on a shared triangle edge count once.
fn cut_contains(mesh: &TriMesh, point: [f64; 3]) -> bool {
    let sub = |a: [f64; 3], b: [f64; 3]| std::array::from_fn(|i| a[i] - b[i]);
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let dir = [0.5773, 0.5774, 0.5775];
    let mut hits = Vec::new();
    for triangle in &mesh.triangles {
        let [a, b, c] = triangle.map(|i| { let v = mesh.vertices[i as usize]; [v.x, v.y, v.z] });
        let (ab, ac) = (sub(b, a), sub(c, a));
        let h = cross(dir, ac);
        let det = dot(ab, h);
        if det.abs() <= 1e-12 * (dot(ab, ab) * dot(h, h)).sqrt() { continue; }
        let s = sub(point, a);
        let u = dot(s, h) / det;
        let q = cross(s, ab);
        let v = dot(dir, q) / det;
        let t = dot(ac, q) / det;
        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 1e-9 { hits.push(t); }
    }
    hits.sort_unstable_by(f64::total_cmp);
    hits.dedup_by(|a, b| (*a - *b).abs() <= 1e-8);
    hits.len() % 2 == 1
}

fn case_frame(text: &str) -> Option<[[f64; 4]; 3]> {
    let params = parse_params(text).ok()?;
    let mm = |key: &str| {
        params.iter().find_map(|(name, value)| match value {
            Param::Num(n) if name == key => Some(*n * 1000.0).filter(|n| n.is_finite()),
            _ => None,
        })
    };
    Some([
        [1.0, 0.0, 0.0, -mm("cx")?],
        [0.0, 0.0, 1.0, mm("y0")?],
        [0.0, -1.0, 0.0, mm("cy")?],
    ])
}

fn generated_solid(csg: Csg, frame: &[[f64; 4]; 3]) -> GenSolid {
    let csg = Arc::new(csg);
    let at = frame.map(|row| row[3]);
    // Rz * Ry * Rx, as used by the graph's transform_matrix. At either gimbal
    // pole, set Z to zero and absorb the coupled X/Z turn into X.
    let cy = frame[0][0].hypot(frame[1][0]);
    let y = (-frame[2][0]).atan2(cy);
    let (x, z) = if cy > 1e-12 {
        (frame[2][1].atan2(frame[2][2]), frame[1][0].atan2(frame[0][0]))
    } else {
        ((-frame[1][2]).atan2(frame[1][1]), 0.0)
    };
    let rotate = [x, y, z].map(f64::to_degrees);
    Arc::new(move |graph| {
        let source = crate::lower::lower_into(&csg, graph)?;
        graph.push(Op::Transform { source, at, rotate, scale: [1.0; 3] })
    })
}
