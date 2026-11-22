//! The compiler's source information joined to an image's addresses.

use fab_elec_mcu_emu::{FwCond, FwDebug, FwFunc, FwHandler, FwScalar, FwStmt, FwTy, FwVar, Owner, StepMap};
use fab_elec_mcu_rt::ports::FORM_MIC;
use makepad_script_compute::lower::event::DeclArg;

use crate::Compiled;

/// A chip image's addresses for the firmware debug table.
pub struct ChipMap<'a> {
    pub image_id: u32,
    /// (Address, statement id), ascending by address, outermost first at one address.
    pub stmt_starts: &'a [(u32, u32)],
    /// (Address of a source condition's conditional branch, condition id, the branch is taken when
    /// the condition's `if` finds its value true), any order.
    pub conds: Vec<(u32, u32, bool)>,
    /// (Entry address, handler number), ascending by address.
    pub entries: Vec<(u32, u32)>,
    /// Handler return addresses, ascending and unique.
    pub returns: Vec<u32>,
    pub funcs: Vec<(u32, u32, u32)>,
    pub calls: Vec<(u32, u32)>,
    pub owners: Vec<Owner>,
    /// Data address of the state block.
    pub state: u32,
    /// Named runtime routines by start address, ascending.
    pub runtime: Vec<(u32, &'static str)>,
    pub runtime_end: u32,
}

/// A state variable's type from the compiler's signature (`i32`, `[bool;16]`): scalars, arrays of
/// scalars a word each, and every other layout as its words.
fn var_ty(sig: &str, words: u32) -> FwTy {
    let scalar = |sig: &str| match sig {
        "i32" => Some(FwScalar::I32),
        "f32" => Some(FwScalar::F32),
        "bool" => Some(FwScalar::Bool),
        _ => None,
    };
    let array = sig.strip_prefix('[').and_then(|s| s.strip_suffix(']')).and_then(|s| s.rsplit_once(';'));
    match (scalar(sig), array) {
        (Some(FwScalar::I32), _) => FwTy::I32,
        (Some(FwScalar::F32), _) => FwTy::F32,
        (Some(FwScalar::Bool), _) => FwTy::Bool,
        (None, Some((elem, len))) => match (scalar(elem), len.trim().parse::<u32>()) {
            (Some(elem), Ok(len)) if len == words => FwTy::Array(elem, len),
            _ => FwTy::Words(words),
        },
        (None, None) => FwTy::Words(words),
    }
}

/// The image's debug table and any note about an unusable step map.
pub fn debug_table(c: &Compiled, source_len: usize, chip: ChipMap<'_>) -> (FwDebug, Option<String>) {
    let span = |start: u32, end: u32| {
        if start <= end && end as usize <= source_len { (start, end) } else { (0, 0) }
    };
    let mut stmts: Vec<FwStmt> = c.stmts.iter().map(|st| FwStmt {
        addr: None, span: span(st.site.start, st.site.end), line: st.site.line, col: st.site.col, owner: st.owner,
    }).collect();
    let mut starts: Vec<(u32, Vec<u32>)> = Vec::new();
    for &(at, id) in chip.stmt_starts {
        stmts[id as usize].addr.get_or_insert(at);
        match starts.last_mut() {
            Some((last, ids)) if *last == at => ids.push(id),
            _ => starts.push((at, vec![id])),
        }
    }
    let handlers = c.lowered.entries.iter()
        .map(|e| FwHandler { name: e.name.clone(), span: span(e.span.start as u32, e.span.end as u32) }).collect();
    let funcs = c.lowered.funcs.iter()
        .map(|f| FwFunc { name: f.name.clone(), span: span(f.span.start as u32, f.span.end as u32) }).collect();
    let vars = c.lowered.state_vars.iter().map(|v| FwVar {
        name: v.name.clone(),
        span: span(v.span.start as u32, v.span.end as u32),
        ty: var_ty(&v.sig, v.words),
        addr: chip.state + v.offset * 4,
    }).collect();
    let map = StepMap {
        statements: starts.iter().map(|s| s.0).collect(),
        entries: chip.entries,
        returns: chip.returns,
        funcs: chip.funcs, calls: chip.calls, owners: chip.owners,
    };
    let (map, note) = match map.check() {
        Ok(()) => (Some(map), None),
        Err(why) => (None, Some(format!("stepping is unavailable: the image's step map {why}"))),
    };
    let sites = c.sites.iter().map(|site| span(site.start, site.end)).collect();
    let mut conds: Vec<FwCond> = chip.conds.iter().filter_map(|&(pc, id, taken_if_true)| {
        let cond = c.conds.get(id as usize)?;
        Some(FwCond {
            pc, span: span(cond.site.start, cond.site.end), line: cond.site.line, owner: cond.owner,
            // The `if` tests the condition, or its negation (a `while`'s exit).
            taken_true: taken_if_true == cond.tests,
        })
    }).collect();
    conds.sort_by_key(|cond| cond.pc);
    let mic_rate = c.lowered.decls.iter().find_map(|d| match (u32::from(d.form), d.args.first()) {
        (FORM_MIC, Some(DeclArg::Int(rate))) => u32::try_from(*rate).ok(),
        _ => None,
    });
    let debug = FwDebug {
        image_id: chip.image_id, source_rev: 0, mic_rate, map, stmts, starts, conds, handlers, funcs, sites, vars,
        fault_cells: Vec::new(), runtime: chip.runtime, runtime_end: chip.runtime_end,
    };
    (debug, note)
}
