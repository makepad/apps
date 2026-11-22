//! Native page units shared by the MCU emulators.

use relax_backend::standalone::{self, Alloc, Passes};
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Instant;

/// Relax instructions, registers and symbols used to lower guest code.
pub use relax_backend::rir;
use rir::{Callee, Cls, Cond, Conv, Func, IOp, Inst, Reloc, Sym, Term, VReg};

/// Native entry taking the emulator context and a guest PC and returning an exit code.
pub type UnitEntry = relax_backend::host_fn_ptr!(fn(*mut u8, u32) -> u32);

/// Linked executable code for one guest page and the PCs it accepts.
pub struct Unit {
    code: standalone::Emitted,
    entries: Vec<u32>,
}

impl Unit {
    /// Returns the guest entry PCs in ascending order.
    pub fn entries(&self) -> &[u32] {
        &self.entries
    }

    /// Returns the number of emitted bytes, including the constant pool.
    pub fn code_len(&self) -> usize {
        self.code.code.len()
    }

    /// Enters the page at `pc` and returns its exit code.
    ///
    /// # Safety
    /// `pc` must be one of this unit's entries. The caller must ensure the emitted
    /// instructions and helper calls are valid for `ctx` for the duration of the call,
    /// including all memory accesses and each helper's address and calling convention.
    pub unsafe fn run(&self, ctx: *mut u8, pc: u32) -> u32 {
        unsafe {
            let entry: UnitEntry = std::mem::transmute(self.code.code.ptr());
            entry(ctx, pc)
        }
    }
}

/// Size and wall-clock timings of a completed page compilation.
pub struct UnitStats {
    /// Number of RIR blocks after building the dispatch and before optimisation.
    pub blocks: u32,
    /// RIR blocks after optimisation: what the register allocator sees.
    pub opt_blocks: u32,
    /// Guest instructions the unit holds, as its builder counted them (`count_guest`).
    pub guest: u32,
    /// Nanoseconds from builder creation through completion of the entry dispatch.
    pub build_ns: u64,
    /// Nanoseconds spent verifying and optimising the RIR.
    pub optimise_ns: u64,
    /// Nanoseconds spent emitting, linking and mapping the code.
    pub emit_ns: u64,
    /// Emitted bytes, including the constant pool.
    pub code_len: u32,
}

/// Builds one native page from guest entry blocks, helper calls and shared exits.
pub struct UnitBuilder {
    f: Func,
    current: u32,
    ctx: VReg,
    pc_raw: VReg,
    /// The guest PC the dispatch block routes, zero-extended.
    pc: VReg,
    /// The entry dispatch, filled by `finish`; `redispatch` enters it again.
    dispatch: u32,
    helpers: Vec<u64>,
    entries: BTreeMap<u32, u32>,
    exits: Vec<(u32, u32)>,
    guest: u32,
    started: Instant,
}

impl UnitBuilder {
    /// Creates a function with context and PC parameters. Block zero is reserved for dispatch.
    pub fn new(name: &str) -> Self {
        let started = Instant::now();
        let mut f = Func::new(name.to_owned(), 0);
        let current = f.block();
        let ctx = f.vreg(Cls::I);
        let pc_raw = f.vreg(Cls::I);
        let pc = f.vreg(Cls::I);
        let dispatch = f.block();
        f.params.extend([ctx, pc_raw]);
        f.rets.push(Cls::I);
        Self {
            f,
            current,
            ctx,
            pc_raw,
            pc,
            dispatch,
            helpers: Vec::new(),
            entries: BTreeMap::new(),
            exits: Vec::new(),
            guest: 0,
            started,
        }
    }

    /// Counts `n` guest instructions into the unit's statistics (`UnitStats::guest`).
    pub fn count_guest(&mut self, n: u32) {
        self.guest += n;
    }

    /// Returns the register holding the opaque emulator context.
    pub fn ctx(&self) -> VReg {
        self.ctx
    }

    /// Registers a helper address, reusing its symbol on subsequent calls.
    pub fn helper(&mut self, addr: u64) -> Sym {
        if let Some(index) = self.helpers.iter().position(|&a| a == addr) {
            return Sym::new(index as u32);
        }
        let symbol = Sym::new(self.helpers.len() as u32);
        self.helpers.push(addr);
        symbol
    }

    /// Allocates an empty block and returns its number. The current block stays selected.
    pub fn block(&mut self) -> u32 {
        self.f.block()
    }

    /// Selects the block that receives subsequent instructions and its terminator.
    pub fn switch_to(&mut self, b: u32) {
        self.current = b;
    }

    /// Returns the selected block number.
    pub fn current(&self) -> u32 {
        self.current
    }

    /// Appends an instruction with source position zero to the current block.
    pub fn emit(&mut self, i: Inst) {
        let block = &mut self.f.blocks[self.current as usize];
        block.insts.push(i);
        block.pos.push(0);
    }

    /// A new integer or float register, for values that several blocks write.
    pub fn vreg(&mut self, cls: Cls) -> VReg {
        self.f.vreg(cls)
    }

    /// Ends the current block by returning `value` (an integer register) as the exit code.
    pub fn ret(&mut self, value: VReg) {
        self.f.blocks[self.current as usize].term = Term::Ret(vec![value]);
    }

    /// Ends the current block by entering the dispatch again at guest PC `pc` (a 32-bit value
    /// held zero-extended): the entry's block, or the miss exit `finish` names.
    pub fn redispatch(&mut self, pc: VReg) {
        self.emit(Inst::Mov(self.pc, pc));
        self.jump(self.dispatch);
    }

    /// Emits an integer constant and returns its register.
    pub fn iconst(&mut self, v: i64) -> VReg {
        let result = self.f.vreg(Cls::I);
        self.emit(Inst::Iconst(result, v));
        result
    }

    /// Emits a host helper call and returns its single integer result register.
    pub fn call(&mut self, helper: Sym, args: &[VReg]) -> VReg {
        let result = self.f.vreg(Cls::I);
        self.emit(Inst::Call(Callee::Host(helper), args.to_vec(), vec![result]));
        result
    }

    /// Returns the shared block that returns `code`, preserving the current block.
    pub fn exit(&mut self, code: u32) -> u32 {
        if let Some(&(_, block)) = self.exits.iter().find(|&&(c, _)| c == code) {
            return block;
        }
        let previous = self.current;
        let block = self.block();
        self.switch_to(block);
        let result = self.iconst(i64::from(code));
        self.f.blocks[block as usize].term = Term::Ret(vec![result]);
        self.switch_to(previous);
        self.exits.push((code, block));
        block
    }

    /// Ends the current block with an unconditional jump.
    pub fn jump(&mut self, target: u32) {
        self.f.blocks[self.current as usize].term = Term::Jump(target);
    }

    /// Ends the current block with a branch to `then_b` for nonzero `cond`, else `else_b`.
    pub fn branch(&mut self, cond: VReg, then_b: u32, else_b: u32) {
        self.f.blocks[self.current as usize].term = Term::Branch(cond, then_b, else_b);
    }

    /// Routes guest `pc` to block `b`, replacing any previous registration for that PC.
    pub fn entry(&mut self, pc: u32, b: u32) {
        self.entries.insert(pc, b);
    }

    /// One compare-and-branch block per key, in a chain ending at `miss`: the switch pass turns
    /// a chain into a jump table, bit tests or a search. Returns the chain's head.
    fn chain(&mut self, value: VReg, keys: impl DoubleEndedIterator<Item = (u32, u32)>, miss: u32) -> u32 {
        let mut next = miss;
        for (key, target) in keys.rev() {
            let block = self.block();
            self.switch_to(block);
            let cond = self.f.vreg(Cls::I);
            self.emit(Inst::ICmpI(Cond::Eq, false, cond, value, i64::from(key)));
            self.branch(cond, target, next);
            next = block;
        }
        next
    }

    /// Builds the entry dispatch and compiles the page. A dispatch miss returns `miss`.
    /// Returns `None` when verification, emission, linking or executable mapping fails.
    pub fn finish(self, miss: u32) -> Option<(Unit, UnitStats)> {
        let entries = self.entries.len() as u64;
        let done = self.compile(miss);
        let mut totals = TOTALS.lock().unwrap_or_else(|e| e.into_inner());
        match &done {
            Some((_, s)) => {
                totals.units += 1;
                totals.entries += entries;
                totals.blocks += u64::from(s.blocks);
                totals.opt_blocks += u64::from(s.opt_blocks);
                totals.guest += u64::from(s.guest);
                totals.build_ns += s.build_ns;
                totals.optimise_ns += s.optimise_ns;
                totals.emit_ns += s.emit_ns;
                totals.code_len += u64::from(s.code_len);
                totals.max_ns = totals.max_ns.max(s.build_ns + s.optimise_ns + s.emit_ns);
            }
            None => totals.failed += 1,
        }
        done
    }

    fn compile(mut self, miss: u32) -> Option<(Unit, UnitStats)> {
        let entries = std::mem::take(&mut self.entries);
        let miss_block = self.exit(miss);
        self.switch_to(0);
        self.emit(Inst::Conv(Conv::IntToInt(rir::U32), self.pc, self.pc_raw));
        self.jump(self.dispatch);
        // The switch pass follows a chain for a bounded number of blocks (switch.rs), so a
        // long one is split: a chain over PC / GROUP to one chain per group of PCs.
        const GROUP: u32 = 64;
        let head = if entries.len() <= GROUP as usize {
            self.chain(self.pc, entries.iter().map(|(&k, &t)| (k, t)), miss_block)
        } else {
            let mut groups: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
            for (&key, &target) in &entries { groups.entry(key / GROUP).or_default().push((key, target)); }
            let heads: Vec<(u32, u32)> = groups.into_iter()
                .map(|(group, keys)| (group, self.chain(self.pc, keys.into_iter(), miss_block)))
                .collect();
            let block = self.block();
            self.switch_to(block);
            let group = self.f.vreg(Cls::I);
            self.emit(Inst::IBinI(IOp::Shr, rir::U64, group, self.pc, i64::from(GROUP.trailing_zeros())));
            let top = self.chain(group, heads.into_iter(), miss_block);
            self.switch_to(block);
            self.jump(top);
            block
        };
        self.switch_to(self.dispatch);
        self.jump(head);
        let build_ns = self.started.elapsed().as_nanos() as u64;
        let blocks = self.f.blocks.len() as u32;

        let options = options();
        let started = Instant::now();
        self.f.verify().ok()?;
        standalone::optimise(options, &mut self.f, Passes::Blocks);
        self.f.verify().ok()?;
        let optimise_ns = started.elapsed().as_nanos() as u64;
        let opt_blocks = self.f.blocks.len() as u32;

        let resolve = |reloc| match reloc {
            Reloc::Sym(s) => self.helpers.get(s.id as usize).map(|addr| addr.wrapping_add_signed(s.off)),
            Reloc::Value(v) => Some(v),
            _ => None,
        };
        let started = Instant::now();
        let code = standalone::emit(options, &self.f, Alloc::Quick, &resolve)?;
        let emit_ns = started.elapsed().as_nanos() as u64;
        let unit = Unit { code, entries: entries.into_keys().collect() };
        let code_len = u32::try_from(unit.code_len()).ok()?;
        Some((unit, UnitStats { blocks, opt_blocks, guest: self.guest, build_ns, optimise_ns, emit_ns, code_len }))
    }
}

/// Totals over every unit this process compiled.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub units: u64,
    /// Compiles that gave no code (the machine stays on its interpreter tier there).
    pub failed: u64,
    /// Entry PCs.
    pub entries: u64,
    /// Guest instructions, as the builders counted them.
    pub guest: u64,
    /// RIR blocks before optimisation.
    pub blocks: u64,
    /// RIR blocks after optimisation.
    pub opt_blocks: u64,
    pub build_ns: u64,
    pub optimise_ns: u64,
    pub emit_ns: u64,
    pub code_len: u64,
    /// The longest single compile.
    pub max_ns: u64,
}

static TOTALS: Mutex<Totals> = Mutex::new(Totals {
    units: 0, failed: 0, entries: 0, guest: 0, blocks: 0, opt_blocks: 0, build_ns: 0, optimise_ns: 0, emit_ns: 0, code_len: 0, max_ns: 0,
});

/// The compile totals so far.
pub fn totals() -> Totals {
    *TOTALS.lock().unwrap_or_else(|e| e.into_inner())
}

static INLINE: AtomicBool = AtomicBool::new(false);

/// Whether machines compile their units on the calling thread, at once, instead of on the
/// shared worker: for tests that must run native code from the first hot page on.
pub fn inline_compile() -> bool {
    INLINE.load(Ordering::Relaxed)
}

/// Sets `inline_compile` for the process.
pub fn set_inline_compile(on: bool) {
    INLINE.store(on, Ordering::Relaxed);
}

fn options() -> &'static relax_session::Options {
    static OPTIONS: OnceLock<relax_session::Options> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut o = relax_session::Options::from_env();
        o.nan_exact = true;
        #[cfg(target_arch = "x86_64")]
        {
            o.cpu = relax_backend::x64::detect_cpu(&o);
        }
        o
    })
}

/// A unit compile, as the compiler hands it to where it runs.
pub type Job = Box<dyn FnOnce() + Send>;

/// Runs compile jobs away from the thread that asks for them: an app's workpool (`compile_on`).
pub type Spawn = Arc<dyn Fn(Job) + Send + Sync>;

/// Queues page compilation jobs where the process compiles them: on the spawn an app installed
/// (`compile_on`), else on one long-lived worker of the compiler's own.
pub struct Compiler {
    jobs: OnceLock<mpsc::Sender<Job>>,
}

static SPAWN: OnceLock<Spawn> = OnceLock::new();

/// Has every later unit compile through `spawn` (an app's workpool, as background work), for the
/// rest of the process; the first install holds. A job `spawn` drops leaves its page on the
/// interpreter tiers.
pub fn compile_on(spawn: Spawn) {
    let _ = SPAWN.set(spawn);
}

/// Whether an app has installed where units compile (`compile_on`).
pub fn compiles_on_spawn() -> bool {
    SPAWN.get().is_some()
}

impl Compiler {
    /// Queues `job` and returns a cell to poll with `get()`. A panicking job leaves it empty.
    pub fn submit<T: Send + Sync + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> Arc<OnceLock<T>> {
        let cell = Arc::new(OnceLock::new());
        let result = Arc::clone(&cell);
        let job: Job = Box::new(move || {
            let _ = result.set(job());
        });
        match SPAWN.get() {
            Some(spawn) => spawn(job),
            None => { let _ = self.worker().send(job); }
        }
        cell
    }

    /// The compiler's own worker, started on first use. If the host cannot start it, submitted
    /// cells stay empty.
    fn worker(&self) -> &mpsc::Sender<Job> {
        self.jobs.get_or_init(|| {
            let (jobs, receiver) = mpsc::channel::<Job>();
            let worker = std::thread::Builder::new().name("mcu-jit".to_owned()).spawn(move || {
                for job in receiver {
                    let _ = catch_unwind(AssertUnwindSafe(job));
                }
            });
            if let Err(error) = worker {
                eprintln!("Could not start MCU JIT worker: {error}");
            }
            jobs
        })
    }
}

/// Returns the process-wide compiler.
pub fn compiler() -> &'static Compiler {
    static COMPILER: Compiler = Compiler { jobs: OnceLock::new() };
    &COMPILER
}
