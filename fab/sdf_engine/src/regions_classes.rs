//! The fill's classes on the GPU (S1+S2, GPU-EXEC §9): the class program ([`regions_program`]) run
//! as compose rounds over a batch, from the outline's and the fill's fields.
//!
//! - A level-0 term reads its source directly: an input's FeatureSet with its field's sides, or an
//!   earlier round's output with its composed sides.
//! - A term at another level reads its source's level set: every level a region is read at is cut
//!   by one extract (wall_pieces) of that region's field, and the term's sides come from the
//!   field's distances (d' <= level is inside). A round's region read at levels gets its field
//!   right after the round: chain, its FeatureSet (regions_set), feature_field, the extract.
//! - A Keep round composes its region with its queries (R ∧ S@dz per Meets, R at -r per Thin)
//!   as one round, chains it with each query's reference level R, decides per component
//!   (regions_keep), and makes the kept loops a FeatureSet and a field; an identity round over
//!   that set is then the region every later round reads.
//! - Flags read the earlier rounds' any-words.
//! - Every class comes out of a round: a class equal to an input runs one identity round.
//!
//! Fields and extracts size their buffers at a checkpoint, so the rounds go on from their `then`:
//! the steps are continuation-passing, and [`steps`] hands the classes to its own `then`. A batch
//! takes one checkpoint per field and one per extract. Every stage's counts words are claimed by
//! [`claim`] before the item starts.
//!
//! Levels are mm in the program and grid units (0.1 mm = 1) on the GPU.
//!
//! Capacities come from the leaves a round reaches: the inputs' segs and the extracts' level
//! pieces, whose counts are exact. A composition's boundary lies on its leaves' boundaries, so a
//! round's pieces are sub-pieces of its leaves' pieces cut where they cross: per output, the
//! leaves' pieces plus half again for the cuts, never the product of earlier rounds' capacities.
//!
//! [`regions_program`]: crate::regions_program
use crate::chain::{self, ChainGrid, ChainKernels, Pieces};
use crate::feature_field::{self, FieldJoin, FieldKernels, FieldOut, FieldParams};
use crate::gpu_jobs::{CountWord, Counts, JobError, Pool, Step};
use crate::plane_abi::{LEVEL_REGION, NONE};
use crate::regions_compose::{self as rc, ComposeKernels, RoundOut, RoundWords, Term, TermSegs, TermSides, STATES};
use crate::regions_keep::{self, Keep, KeepKernels, TestKind};
use crate::regions_program::{self as program, Kind, KeepTest, Plane, Program, Source};
use crate::regions_set::{self, RegionSet, SetFrom, SetKernels};
use crate::wall_pieces::{self, Levels, PieceKernels, PiecesOut};
use makepad_csg_sdf::plan::{Region, ShellSpec};
use makepad_csg_sdf::regions::{Reg, CLASSES};
use makepad_draw::Cx;

/// Grid units per mm.
const PER_MM: f32 = 10.0;
/// The pieces' level of a round that makes no class (a class's is LEVEL_REGION + its index in
/// CLASSES); a Keep's queries take the levels after it.
const LEVEL_INNER: u32 = LEVEL_REGION + CLASSES.len() as u32;
/// The extract's level id of level slot l (only the rounds read these pieces).
const LEVEL_SLOT: u32 = LEVEL_INNER + 1;
/// The planner's offset joins (fab slicer DEFAULT_MITER): the metric of every region field.
const MITER: f32 = 3.0;
/// A region field's depth past the deepest level read on it, grid units.
const DEPTH_MARGIN: f32 = 2.0;

/// The batch: consecutive layers of one part of `n` layers, on an nx × ny coarse grid.
#[derive(Clone, Debug)]
pub struct Batch {
    pub nx: u32,
    pub ny: u32,
    pub n: usize,
    pub spec: ShellSpec,
    pub planes: Vec<Plane>,
}

/// An input region (the outline or the fill): its field, over the batch's planes, and the coarse
/// cells holding its segs. Its depths cover every level the program reads it at.
#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub field: FieldOut,
    pub cells: u32,
}

impl Input {
    /// The input's FeatureSet as a level-0 term at plane offset `dz`.
    fn term(&self, dz: i32) -> Term {
        let (i, f) = (&self.field.input, &self.field);
        Term::set(i.set_slot, TermSegs { off: i.seg_off, seg: i.seg, held: i.segs.capacity() }, TermSides { band_slot: f.band_slot, cover: f.cover, node_side: f.node_side }, dz)
    }
}

/// The class rounds' kernels, made once.
#[derive(Clone)]
pub struct ClassKernels {
    compose: ComposeKernels,
    pieces: PieceKernels,
    chain: ChainKernels,
    set: SetKernels,
    field: FieldKernels,
    keep: KeepKernels,
}
impl ClassKernels {
    pub fn new(cx: &mut Cx) -> Result<Self, String> {
        Ok(Self { compose: ComposeKernels::new(cx)?, pieces: PieceKernels::new(cx)?, chain: ChainKernels::new(cx)?, set: SetKernels::new(cx)?,
            field: FieldKernels::new(cx)?, keep: KeepKernels::new(cx)? })
    }
}

/// A region's pieces: output `output` of a round (its words, and its piece capacity).
#[derive(Clone, Copy, Debug)]
pub struct ClassRegion {
    pub out: RoundOut,
    pub output: u32,
    pub words: RoundWords,
    pub capacity: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ClassOut {
    /// Per class, in csg_sdf regions::CLASSES order: its pieces, or None when it is empty on
    /// every plane.
    pub classes: Vec<(Region, Option<ClassRegion>)>,
    /// Every program region a round made, in round order, for checks against a CPU run.
    pub regions: Vec<(Reg, ClassRegion)>,
    /// Per batch plane: every class there reads only planes inside the batch.
    pub valid: Vec<bool>,
    /// Pieces or loops past a capacity: all 0 after the run, or run again with a larger factor.
    pub overflow: Vec<CountWord>,
    /// The stages' own invariants, for [`ClassOut::verify`].
    pub extracts: Vec<PiecesOut>,
    pub fields: Vec<FieldOut>,
    /// Per chained round, its region (None for an inner one) and the chain's words.
    pub chains: Vec<(Option<Reg>, CountWord)>,
}

impl ClassOut {
    /// Every stage held its capacities and invariants on the final counts.
    pub fn verify(&self, counts: &Counts) -> Result<(), JobError> {
        let over: u32 = self.overflow.iter().map(|&w| counts.word(w)).sum();
        if over != 0 { return Err(JobError::Failed(format!("regions: {over} past capacity: run again with a larger factor"))); }
        let mut bad = Vec::new();
        for &(reg, c) in &self.chains {
            let words: Vec<String> = [(chain::WORD_OVERFLOW, "loops past capacity"), (chain::WORD_OPEN_TAILS, "open tails"),
                (chain::WORD_OPEN_HEADS, "open heads"), (chain::WORD_DUPLICATES, "duplicate keys"), (chain::WORD_DEFECTS, "containment defects")]
                .into_iter().map(|(w, what)| (counts.word(CountWord(c.0 + w)), what)).filter(|&(n, _)| n != 0).map(|(n, what)| format!("{n} {what}")).collect();
            if !words.is_empty() { bad.push(format!("chain of {reg:?} ({} loops): {}", counts.word(CountWord(c.0 + chain::WORD_LOOPS)), words.join(", "))); }
        }
        let failed = |what: &str, i: usize, r: Result<(), JobError>| r.err().map(|e| format!("{what} {i}: {e}"));
        bad.extend(self.extracts.iter().enumerate().filter_map(|(i, e)| failed("extract", i, e.verify(counts))));
        bad.extend(self.fields.iter().enumerate().filter_map(|(i, f)| failed("field", i, f.verify(counts))));
        if !bad.is_empty() { return Err(JobError::Failed(format!("regions {}", bad.join("; ")))); }
        Ok(())
    }
}

/// A leaf the rounds compose: an input's segs or a source's extract, with its piece and coarse
/// cell counts.
#[derive(Clone, Copy, Debug)]
struct Leaf {
    pieces: u32,
    cells: u32,
}

/// Leaf ids: the outline's and the fill's segs, then source a's extract at 2 + a.
const LEAF_EXTRACT: usize = 2;

/// A round already in the steps: its output, piece capacity and level.
#[derive(Clone, Copy)]
struct Made {
    out: RoundOut,
    words: RoundWords,
    outputs: u32,
    level: u32,
}

/// The levels one source is read at: per slot, the level on every field plane (grid units, NaN
/// where unused), and the source's extract once it ran.
#[derive(Default, Debug)]
struct Read {
    slots: Vec<Vec<f32>>,
    pieces: Option<PiecesOut>,
}

fn same(a: &[f32], b: &[f32]) -> bool { a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits()) }

impl Read {
    fn add(&mut self, levels: Vec<f32>) {
        if !self.slots.iter().any(|s| same(s, &levels)) { self.slots.push(levels); }
    }
    fn slot(&self, levels: &[f32]) -> Option<u32> { self.slots.iter().position(|s| same(s, levels)).map(|s| s as u32) }
}

/// Where a source's reads and field are: the inputs, then every round's region.
fn at(s: Source) -> Option<usize> {
    match s {
        Source::Outline => Some(0),
        Source::Fill => Some(1),
        Source::Round(j) => Some(2 + j),
        Source::Empty => None,
    }
}

/// A level term's levels on its field's planes: field plane q holds the level of reading plane
/// q - dz (grid units), NaN off the batch.
fn field_levels(level: &[f32], dz: i8) -> Vec<f32> {
    let planes = level.len() as i64;
    (0..planes).map(|q| {
        let p = q - dz as i64;
        if (0..planes).contains(&p) { level[p as usize] * PER_MM } else { f32::NAN }
    }).collect()
}

/// A Thin test's level per reading plane (mm): -r, none (NaN) where the plane tests nothing.
fn thin_levels(r: &[f32]) -> Vec<f32> { r.iter().map(|&r| if r == 0.0 { f32::NAN } else { -r }).collect() }

type Then = Box<dyn FnOnce(&mut Pool, &ClassOut) -> Result<Vec<Step>, JobError>>;
type Next = fn(Run, &mut Pool) -> Result<Vec<Step>, JobError>;

/// What the rounds need, owned so the steps can carry it past each checkpoint.
struct Run {
    /// Per leaf id, once known.
    leaf: Vec<Option<Leaf>>,
    /// Per source: the leaves it reaches.
    reach: Vec<Vec<usize>>,
    k: ClassKernels,
    batch: Batch,
    prog: Program,
    factor: u32,
    inputs: [Input; 2],
    reads: Vec<Read>,
    /// The stages' counts words (see [`Claimed`]).
    input_words: [Option<CountWord>; 2],
    claims: Vec<RoundClaim>,
    class_words: Vec<Option<RoundWords>>,
    /// Per source: its field and the coarse cells holding its segs.
    fields: Vec<Option<(FieldOut, u32)>>,
    made: Vec<Option<Made>>,
    /// The next round.
    i: usize,
    out: ClassOut,
    then: Option<Then>,
}

/// One round's counts words: its own, and those of the stages after it (a chain, a set, a field
/// and an extract where it is read at levels; a Keep's also an identity round).
#[derive(Clone, Copy, Debug)]
struct RoundClaim {
    round: RoundWords,
    chain: Option<CountWord>,
    set: Option<CountWord>,
    field: Option<CountWord>,
    identity: Option<RoundWords>,
    extract: Option<CountWord>,
}

/// A batch's compiled class program with every stage's counts words, claimed before the item
/// starts; [`steps`] takes it.
#[derive(Debug)]
pub struct Claimed {
    prog: Program,
    reads: Vec<Read>,
    inputs: [Option<CountWord>; 2],
    rounds: Vec<RoundClaim>,
    /// Per class: an identity round's words where the class is an input as is.
    classes: Vec<Option<RoundWords>>,
}

fn round_words(pool: &mut Pool, planes: u32, outputs: u32) -> Result<RoundWords, JobError> { RoundWords::claim(pool, "regions class round", planes, outputs) }
fn extract_words(pool: &mut Pool, read: &Read) -> Result<Option<CountWord>, JobError> {
    if read.slots.is_empty() { Ok(None) } else { wall_pieces::claim(pool).map(Some) }
}

/// The class program of `batch` and its stages' counts words, in `pool` before its item starts.
pub fn claim(pool: &mut Pool, batch: &Batch) -> Result<Claimed, JobError> {
    let prog = Program::compile(&batch.spec, batch.n, &batch.planes).map_err(JobError::Failed)?;
    let reads = reads_of(&prog)?;
    let planes = batch.planes.len() as u32;
    let inputs = [extract_words(pool, &reads[0])?, extract_words(pool, &reads[1])?];
    let mut rounds = Vec::new();
    for (i, r) in prog.rounds.iter().enumerate() {
        let read = !reads[2 + i].slots.is_empty();
        let c = match &r.kind {
            Kind::Compose { .. } => {
                let round = round_words(pool, planes, 1)?;
                if !read { RoundClaim { round, chain: None, set: None, field: None, identity: None, extract: None } } else {
                    RoundClaim { round, chain: Some(chain::claim(pool)?), set: Some(regions_set::claim(pool)?), field: Some(feature_field::claim(pool)?),
                        identity: None, extract: extract_words(pool, &reads[2 + i])? }
                }
            }
            Kind::Keep { tests, .. } => {
                // output 0 is the region, then one per Thin or Meets query
                let outputs = 1 + tests.iter().filter(|t| !matches!(t, KeepTest::AreaOver(_))).count() as u32;
                RoundClaim { round: round_words(pool, planes, outputs)?, chain: Some(chain::claim(pool)?), set: Some(regions_set::claim(pool)?),
                    field: Some(feature_field::claim(pool)?), identity: Some(round_words(pool, planes, 1)?), extract: extract_words(pool, &reads[2 + i])? }
            }
        };
        rounds.push(c);
    }
    let mut classes = Vec::new();
    for (_, reg) in CLASSES.iter() {
        let input = matches!(prog.regions.get(reg), Some(Source::Outline | Source::Fill));
        classes.push(if input { Some(round_words(pool, planes, 1)?) } else { None });
    }
    Ok(Claimed { prog, reads, inputs, rounds, classes })
}

/// Every level each source is read at, so each source runs one extract.
fn reads_of(prog: &Program) -> Result<Vec<Read>, JobError> {
    let fail = JobError::Failed;
    let mut reads: Vec<Read> = (0..2 + prog.rounds.len()).map(|_| Read::default()).collect();
    for (i, r) in prog.rounds.iter().enumerate() {
        let mut read = |s: Source, levels: Vec<f32>| -> Result<(), JobError> {
            let a = at(s).ok_or_else(|| fail(format!("round {i} reads nothing at a level")))?;
            reads[a].add(levels);
            Ok(())
        };
        match &r.kind {
            Kind::Compose { terms, .. } => {
                for t in terms.iter().filter(|t| t.level.iter().any(|&l| l != 0.0)) { read(t.src, field_levels(&t.level, t.dz))?; }
            }
            Kind::Keep { of, tests, .. } => {
                for t in tests {
                    if let KeepTest::Thin(r) = t { read(*of, field_levels(&thin_levels(r), 0))?; }
                }
            }
        }
    }
    Ok(reads)
}

/// The class rounds of `batch` (its program and words from [`claim`]), from `outline` and
/// `fill`; `then` gets the classes and its steps run after theirs. `factor` (1, then larger on
/// an overflow) multiplies every round's capacity from its leaves and the chains' re-run factor.
pub fn steps(k: &ClassKernels, pool: &mut Pool, batch: &Batch, claimed: Claimed, outline: Input, fill: Input, factor: u32,
    then: impl FnOnce(&mut Pool, &ClassOut) -> Result<Vec<Step>, JobError> + 'static) -> Result<Vec<Step>, JobError> {
    let fail = JobError::Failed;
    let Claimed { prog, reads, inputs: input_words, rounds: claims, classes: class_words } = claimed;
    let planes = batch.planes.len() as u32;
    let inputs = [outline, fill];
    for (i, inp) in inputs.iter().enumerate() {
        let p = &inp.field.params;
        if p.planes != planes || p.nx != batch.nx || p.ny != batch.ny {
            return Err(fail(format!("input {i}: field {}x{}x{} for a {}x{}x{planes} batch", p.nx, p.ny, p.planes, batch.nx, batch.ny)));
        }
    }
    for (w, inp) in inputs.iter().enumerate() {
        let p = &inp.field.params;
        if let Some(l) = reads[w].slots.iter().flatten().find(|l| **l < -p.depth_in || **l > p.depth_out) {
            return Err(fail(format!("input {w} is read at level {l} past its field's depths ({}, {})", p.depth_in, p.depth_out)));
        }
    }
    let mut fields = vec![None; 2 + prog.rounds.len()];
    fields[0] = Some((outline.field, outline.cells));
    fields[1] = Some((fill.field, fill.cells));
    let made = vec![None; prog.rounds.len()];
    let sources = 2 + prog.rounds.len();
    let mut leaf = vec![None; LEAF_EXTRACT + sources];
    for (w, inp) in inputs.iter().enumerate() { leaf[w] = Some(Leaf { pieces: inp.field.input.segs.capacity(), cells: inp.cells }); }
    let mut reach = vec![Vec::new(); sources];
    (reach[0], reach[1]) = (vec![0], vec![1]);
    let run = Run { leaf, reach, k: k.clone(), batch: batch.clone(), prog, factor, inputs, reads, input_words, claims, class_words, fields, made, i: 0,
        out: ClassOut::default(), then: Some(Box::new(then)) };
    start(run, pool)
}

/// The inputs' extracts, then the rounds.
fn start(run: Run, pool: &mut Pool) -> Result<Vec<Step>, JobError> {
    match (0..2).find(|&w| !run.reads[w].slots.is_empty() && run.reads[w].pieces.is_none()) {
        Some(w) => extract(run, pool, w, start),
        None => next(run, pool),
    }
}

impl Run {
    fn planes(&self) -> u32 { self.batch.planes.len() as u32 }

    /// Source `s` at plane offset `dz`, level 0: the term and the leaves it reaches.
    fn term0(&self, s: Source, dz: i8) -> Result<(Term, Vec<usize>), JobError> {
        match s {
            Source::Round(j) => {
                let m = self.made[j].ok_or_else(|| JobError::Failed(format!("round {j} read before it is made")))?;
                Ok((m.out.term(0, dz as i32), self.reach[2 + j].clone()))
            }
            Source::Outline | Source::Fill => {
                let w = at(s).unwrap();
                Ok((self.inputs[w].term(dz as i32), vec![w]))
            }
            Source::Empty => Err(JobError::Failed("a term reads nothing".into())),
        }
    }

    /// Source `s`'s level set at `level` mm per reading plane, at plane offset `dz`.
    fn term_level(&self, s: Source, dz: i8, level: &[f32]) -> Result<(Term, Vec<usize>), JobError> {
        let a = at(s).ok_or_else(|| JobError::Failed("a level of nothing".into()))?;
        let read = &self.reads[a];
        let slot = read.slot(&field_levels(level, dz)).ok_or_else(|| JobError::Failed(format!("{s:?} has no extract slot for {level:?}")))?;
        let ((f, _), p) = (self.fields[a].ok_or_else(|| JobError::Failed(format!("{s:?} has no field")))?, read.pieces.ok_or_else(|| JobError::Failed(format!("{s:?} has no extract")))?);
        let reading = level.iter().map(|l| l * PER_MM).collect();
        Ok((Term::level(TermSides { band_slot: f.band_slot, cover: f.cover, node_side: f.nf }, TermSegs { off: p.piece_off, seg: p.piece, held: p.capacity },
            read.slots.len() as u32, slot, dz as i32, reading), vec![LEAF_EXTRACT + a]))
    }

    fn term(&self, t: &program::Term) -> Result<(Term, Vec<usize>), JobError> {
        if t.level.iter().all(|&l| l == 0.0) { self.term0(t.src, t.dz) } else { self.term_level(t.src, t.dz, &t.level) }
    }

    fn flags(&self, flags: &[program::Flag]) -> Result<Vec<rc::Flag>, JobError> {
        flags.iter().map(|f| self.made[f.of].map(|m| m.out.flag(0, f.dz as i32)).ok_or_else(|| JobError::Failed(format!("flag of round {} before it is made", f.of)))).collect()
    }

    /// One compose round over `terms` (with the leaves each reaches), per output its tables;
    /// the round's leaves come back with it.
    fn round(&mut self, pool: &mut Pool, steps: &mut Vec<Step>, words: RoundWords, terms: Vec<(Term, Vec<usize>)>, tables: Vec<Vec<[[u32; 2]; STATES]>>, flags: Vec<rc::Flag>, level: u32) -> Result<(Made, Vec<usize>), JobError> {
        let planes = self.planes();
        // Each term reads its leaves at its own plane offset, so two terms of one leaf mark
        // and cut apart: the bounds add up per term.
        let (mut pieces, mut cells) = (0u64, 0u64);
        for t in &terms {
            let mut reach = t.1.clone();
            reach.sort_unstable();
            reach.dedup();
            for l in reach {
                let leaf = self.leaf[l].ok_or_else(|| JobError::Failed(format!("leaf {l} read before it is counted")))?;
                (pieces, cells) = (pieces + leaf.pieces as u64, cells + leaf.cells as u64);
            }
        }
        let mut leaves: Vec<usize> = terms.iter().flat_map(|t| t.1.iter().copied()).collect();
        leaves.sort_unstable();
        leaves.dedup();
        let cells = cells.min((planes * self.batch.nx * self.batch.ny) as u64) as u32;
        let outputs = tables.len() as u64;
        let capacity = outputs * (pieces + pieces / 2 + 64) * self.factor.max(1) as u64;
        let capacity = u32::try_from(capacity).map_err(|_| JobError::Failed(format!("a round of {capacity} pieces")))?;
        let r = rc::Round { nx: self.batch.nx, ny: self.batch.ny, planes, level, terms: terms.into_iter().map(|t| t.0).collect(), flags, tables,
            cells, segs: pieces.min(u32::MAX as u64) as u32, outputs: capacity };
        let (s, out) = rc::steps(&self.k.compose, pool, &r, words).map_err(JobError::Failed)?;
        steps.extend(s);
        self.out.overflow.push(words.overflow);
        Ok((Made { out, words, outputs: r.outputs, level }, leaves))
    }

    /// Round i made `m`: later rounds read it, and a program region lists it.
    fn made(&mut self, i: usize, reg: Option<Reg>, m: Made) {
        self.made[i] = Some(m);
        if let Some(reg) = reg { self.out.regions.push((reg, ClassRegion { out: m.out, output: 0, words: m.words, capacity: m.outputs })); }
    }

    /// The chain of `m`'s outputs, each output past the first resolved against output 0.
    fn chain(&mut self, pool: &mut Pool, steps: &mut Vec<Step>, words: CountWord, m: &Made, reg: Option<Reg>) -> Result<chain::Chain, JobError> {
        let o = m.out.outputs;
        let pieces = Pieces { piece: m.out.piece, piece_geo: m.out.geo, piece_key: m.out.key, piece_info: m.out.info, piece_off: m.out.ooff,
            band_slot: m.out.rslot, count: m.words.outputs, capacity: m.outputs, levels_per_cell: o };
        let refs: Vec<(u32, u32)> = (1..o).map(|q| (m.level + q, m.level)).collect();
        let grid = ChainGrid::new(self.batch.nx, self.batch.ny, self.planes(), NONE, &refs);
        let (c, s) = chain::steps(&self.k.chain, pool, words, &pieces, &grid, self.factor.clamp(1, chain::K_MAX))?;
        steps.extend(s);
        self.out.chains.push((reg, c.words));
        Ok(c)
    }

    /// The set of `m`'s output 0, of the loops `keep` keeps.
    fn set(&self, pool: &mut Pool, steps: &mut Vec<Step>, words: CountWord, m: &Made, c: &chain::Chain, keep: Option<crate::gpu_jobs::BufRef>) -> Result<RegionSet, JobError> {
        let from = SetFrom { out: m.out, words: m.words, output: 0, level: m.level, capacity: m.outputs, nx: self.batch.nx, ny: self.batch.ny, planes: self.planes() };
        let (set, s) = regions_set::steps(&self.k.set, pool, words, &from, c, keep)?;
        steps.extend(s);
        Ok(set)
    }

    /// Round i's claimed words.
    fn claim(&self, i: usize) -> RoundClaim { self.claims[i] }
    fn unclaimed(what: &str, i: usize) -> JobError { JobError::Failed(format!("round {i}: no {what} words claimed")) }

    /// A region field deep enough for every level source `a` is read at.
    fn field_params(&self, a: usize) -> FieldParams {
        let levels = self.reads[a].slots.iter().flatten().filter(|l| !l.is_nan());
        let (inside, outside) = levels.fold((0.0f32, 0.0f32), |(i, o), &l| (i.max(-l), o.max(l)));
        FieldParams { nx: self.batch.nx, ny: self.batch.ny, planes: self.planes(), depth_in: inside + DEPTH_MARGIN, depth_out: outside + DEPTH_MARGIN,
            join: FieldJoin::Miter(MITER), bias_max: 0.0 }
    }
}

/// Source `a`'s extract of every level it is read at, then `cont`.
fn extract(mut run: Run, pool: &mut Pool, a: usize, cont: Next) -> Result<Vec<Step>, JobError> {
    if run.reads[a].slots.is_empty() { return cont(run, pool); }
    let read = &run.reads[a];
    let (planes, slots) = (run.planes() as usize, read.slots.len());
    let ids: Vec<u32> = (0..slots as u32).map(|l| LEVEL_SLOT + l).collect();
    let values: Vec<f32> = (0..planes).flat_map(|q| read.slots.iter().map(move |s| s[q])).collect();
    let words = if a < 2 { run.input_words[a] } else { run.claims[a - 2].extract };
    let words = words.ok_or_else(|| JobError::Failed(format!("source {a}: no extract words claimed")))?;
    let (k, field) = (run.k.pieces.clone(), run.fields[a].ok_or_else(|| JobError::Failed(format!("source {a}: no field")))?.0);
    wall_pieces::steps(&k, pool, &field, words, &Levels { ids: &ids, values: &values }, move |_: &Counts, pool: &mut Pool, out: &PiecesOut| {
        run.reads[a].pieces = Some(*out);
        run.out.extracts.push(*out);
        run.leaf[LEAF_EXTRACT + a] = Some(Leaf { pieces: out.capacity, cells: field.slots });
        cont(run, pool)
    })
}

/// Round i's region field from `set`, then its extract, then the next rounds. For a Keep, an
/// identity round over the kept set first makes round i's region (`keep`: its region and level).
fn field(mut run: Run, pool: &mut Pool, i: usize, set: RegionSet, keep: Option<(Option<Reg>, u32, Vec<usize>)>) -> Result<Vec<Step>, JobError> {
    let params = run.field_params(2 + i);
    let k = run.k.field.clone();
    let c = run.claim(i);
    let words = c.field.ok_or_else(|| Run::unclaimed("field", i))?;
    feature_field::steps(&k, pool, words, set.input(), params, move |_: &Counts, pool: &mut Pool, field: &FieldOut| {
        let mut steps = Vec::new();
        run.out.fields.push(*field);
        run.fields[2 + i] = Some((*field, set.cells));
        if let Some((reg, level, leaves)) = keep {
            let term = Term::set(set.set_slot, TermSegs { off: set.seg_off, seg: set.seg, held: set.capacity },
                TermSides { band_slot: field.band_slot, cover: field.cover, node_side: field.node_side }, 0);
            let identity = rc::table(1, |b| b == 1);
            let planes = run.planes() as usize;
            let words = c.identity.ok_or_else(|| Run::unclaimed("identity round", i))?;
            // the kept loops lie on the region's own leaves
            let (m, leaves) = run.round(pool, &mut steps, words, vec![(term, leaves)], vec![vec![[identity; STATES]; planes]], Vec::new(), level)?;
            run.made(i, reg, m);
            run.reach[2 + i] = leaves;
        }
        steps.extend(extract(run, pool, 2 + i, next)?);
        Ok(steps)
    })
}

/// The rounds from `run.i` up to the next checkpoint, or to the end.
fn next(mut run: Run, pool: &mut Pool) -> Result<Vec<Step>, JobError> {
    let mut steps = Vec::new();
    while run.i < run.prog.rounds.len() {
        let i = run.i;
        run.i += 1;
        let r = run.prog.rounds[i].clone();
        let level = level_of(r.out);
        match &r.kind {
            Kind::Compose { terms, tables } => {
                let terms = terms.iter().map(|t| run.term(t)).collect::<Result<Vec<_>, _>>()?;
                let flags = run.flags(&r.flags)?;
                let w = run.claim(i);
                let (m, leaves) = run.round(pool, &mut steps, w.round, terms, vec![tables.clone()], flags, level)?;
                run.made(i, r.out, m);
                run.reach[2 + i] = leaves;
                if !run.reads[2 + i].slots.is_empty() {
                    let c = run.chain(pool, &mut steps, w.chain.ok_or_else(|| Run::unclaimed("chain", i))?, &m, r.out)?;
                    let set = run.set(pool, &mut steps, w.set.ok_or_else(|| Run::unclaimed("set", i))?, &m, &c, None)?;
                    steps.extend(field(run, pool, i, set, None)?);
                    return Ok(steps);
                }
            }
            Kind::Keep { of, tests, modes } => {
                // output 0 is the region; each query its own output
                let mut terms = vec![run.term0(*of, 0)?];
                let mut outs: Vec<(usize, Option<usize>)> = vec![(0, None)];
                let mut gpu = Vec::new();
                for t in tests {
                    let (kind, value) = match t {
                        KeepTest::AreaOver(v) => (TestKind::AreaOver, v.clone()),
                        KeepTest::Thin(r) => {
                            terms.push(run.term_level(*of, 0, &thin_levels(r))?);
                            outs.push((terms.len() - 1, None));
                            (TestKind::Thin, Vec::new())
                        }
                        KeepTest::Meets(s, dz) => {
                            terms.push(run.term0(*s, *dz)?);
                            outs.push((0, Some(terms.len() - 1)));
                            (TestKind::Meets, Vec::new())
                        }
                    };
                    let output = if kind == TestKind::AreaOver { 0 } else { outs.len() as u32 - 1 };
                    gpu.push(regions_keep::KeepTest { kind, output, value });
                }
                let n = terms.len() as u32;
                let planes = run.planes() as usize;
                let tables = outs.iter().map(|&(a, b)| {
                    let t = rc::table(n, |bits| bits >> a & 1 != 0 && b.is_none_or(|b| bits >> b & 1 != 0));
                    vec![[t; STATES]; planes]
                }).collect();
                let w = run.claim(i);
                let region = terms[0].1.clone();
                let (m, _) = run.round(pool, &mut steps, w.round, terms, tables, Vec::new(), LEVEL_INNER)?;
                let c = run.chain(pool, &mut steps, w.chain.ok_or_else(|| Run::unclaimed("chain", i))?, &m, r.out)?;
                let keep = Keep { planes: planes as u32, level: LEVEL_INNER, tests: gpu, modes: modes.clone(), flags: run.flags(&r.flags)? };
                let (kept, s) = regions_keep::steps(&run.k.keep, pool, &c, &keep)?;
                steps.extend(s);
                let set = run.set(pool, &mut steps, w.set.ok_or_else(|| Run::unclaimed("set", i))?, &m, &c, Some(kept))?;
                steps.extend(field(run, pool, i, set, Some((r.out, level, region)))?);
                return Ok(steps);
            }
        }
    }
    finish(run, pool, steps)
}

fn level_of(out: Option<Reg>) -> u32 {
    out.and_then(|r| CLASSES.iter().position(|c| c.1 == r)).map_or(LEVEL_INNER, |c| LEVEL_REGION + c as u32)
}

/// The classes, then the caller's steps.
fn finish(mut run: Run, pool: &mut Pool, mut steps: Vec<Step>) -> Result<Vec<Step>, JobError> {
    let fail = JobError::Failed;
    let planes = run.planes() as usize;
    let mut valid = vec![true; planes];
    for (c, (region, reg)) in CLASSES.iter().enumerate() {
        let src = run.prog.regions.get(reg).copied().ok_or_else(|| fail(format!("{reg:?} is not made")))?;
        let class = match src {
            Source::Empty => None,
            Source::Round(j) => {
                for (p, v) in valid.iter_mut().enumerate() { *v &= run.prog.valid[j][p]; }
                let m = run.made[j].ok_or_else(|| fail(format!("{reg:?}: round {j} is not made")))?;
                Some(ClassRegion { out: m.out, output: 0, words: m.words, capacity: m.outputs })
            }
            s => {
                // an input as is: one identity round brings it into the class pieces' layout
                let term = run.term0(s, 0)?;
                let identity = rc::table(1, |b| b == 1);
                let words = run.class_words[c].ok_or_else(|| fail(format!("{reg:?}: no identity round words claimed")))?;
                let (m, _) = run.round(pool, &mut steps, words, vec![term], vec![vec![[identity; STATES]; planes]], Vec::new(), LEVEL_REGION + c as u32)?;
                Some(ClassRegion { out: m.out, output: 0, words: m.words, capacity: m.outputs })
            }
        };
        run.out.classes.push((*region, class));
    }
    run.out.valid = valid;
    let then = run.then.take().expect("the classes finish once");
    steps.extend(then(pool, &run.out)?);
    Ok(steps)
}
