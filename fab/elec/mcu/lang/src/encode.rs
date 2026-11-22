//! Encode flat event AIR into the shared, versioned module format.
use fab_elec_mcu_rt::{format::*, ports};
use makepad_script_compute::ir::{Bin, Cmp, Fma, Op, Program, Region, Stmt, Ty, Un, Val};
use makepad_script_compute::lower::event::{DeclArg, EventLowered};
use std::collections::HashMap;

fn word(n: usize, what: &str) -> Result<u32, String> {
    u32::try_from(n).map_err(|_| format!("{what} does not fit a u32"))
}

#[derive(Default)]
struct Strings {
    ids: HashMap<String, u32>,
    values: Vec<String>,
}

impl Strings {
    fn intern(&mut self, s: &str) -> Result<u32, String> {
        if let Some(id) = self.ids.get(s) {
            return Ok(*id);
        }
        let id = word(self.values.len(), "string count")?;
        self.ids.insert(s.to_owned(), id);
        self.values.push(s.to_owned());
        Ok(id)
    }
}

fn region(r: Region) -> Result<u32, String> {
    match r {
        Region::State => Ok(REGION_STATE),
        Region::Shared => Ok(REGION_SHARED),
        Region::Frame => Ok(REGION_FRAME),
        Region::Ctx => Err("Ctx access is not a module operation".into()),
        Region::Buf(k) => Ok((REGION_BUF_BASE + u32::from(k)).min(REGION_BUF_LONG)),
    }
}

fn buffer_operand(out: &mut Vec<u32>, r: Region) {
    if let Region::Buf(k) = r {
        if REGION_BUF_BASE + u32::from(k) >= REGION_BUF_LONG { out.push(u32::from(k)); }
    }
}

fn offset(off: Option<Val>) -> Result<u32, String> {
    match off {
        Some(Val(NONE)) => Err("offset value collides with NONE".into()),
        Some(v) => Ok(v.0),
        None => Ok(NONE),
    }
}

// The format's codes for AIR's enums, named one by one: a new AIR variant
// is a compile error here, never a silent shift of the encoding.
fn ty_code(t: Ty) -> u32 {
    (match t {
        Ty::F32 => TY_F32,
        Ty::I32 => TY_I32,
        Ty::Bool => TY_BOOL,
        Ty::F64 => TY_F64,
    }) as u32
}

fn un_code(u: Un) -> u32 {
    match u {
        Un::NegF => UN_NEG_F,
        Un::AbsF => UN_ABS_F,
        Un::SqrtF => UN_SQRT_F,
        Un::FloorF => UN_FLOOR_F,
        Un::CeilF => UN_CEIL_F,
        Un::TruncF => UN_TRUNC_F,
        Un::RoundF => UN_ROUND_F,
        Un::F2I => UN_F2I,
        Un::I2F => UN_I2F,
        Un::BitsFI => UN_BITS_FI,
        Un::BitsIF => UN_BITS_IF,
        Un::NegI => UN_NEG_I,
        Un::NotB => UN_NOT_B,
        Un::NegD => UN_NEG_D,
        Un::AbsD => UN_ABS_D,
        Un::SqrtD => UN_SQRT_D,
        Un::FloorD => UN_FLOOR_D,
        Un::CeilD => UN_CEIL_D,
        Un::TruncD => UN_TRUNC_D,
        Un::RoundD => UN_ROUND_D,
        Un::F2D => UN_F2D,
        Un::D2F => UN_D2F,
        Un::D2I => UN_D2I,
        Un::I2D => UN_I2D,
        Un::HiD => UN_HI_D,
        Un::LoD => UN_LO_D,
    }
}

fn bin_code(b: Bin) -> u32 {
    match b {
        Bin::AddF => BIN_ADD_F,
        Bin::SubF => BIN_SUB_F,
        Bin::MulF => BIN_MUL_F,
        Bin::DivF => BIN_DIV_F,
        Bin::MinF => BIN_MIN_F,
        Bin::MaxF => BIN_MAX_F,
        Bin::AddI => BIN_ADD_I,
        Bin::SubI => BIN_SUB_I,
        Bin::MulI => BIN_MUL_I,
        Bin::DivI => BIN_DIV_I,
        Bin::RemI => BIN_REM_I,
        Bin::AndI => BIN_AND_I,
        Bin::OrI => BIN_OR_I,
        Bin::XorI => BIN_XOR_I,
        Bin::ShlI => BIN_SHL_I,
        Bin::ShrI => BIN_SHR_I,
        Bin::ShrUI => BIN_SHR_UI,
        Bin::AndB => BIN_AND_B,
        Bin::OrB => BIN_OR_B,
        Bin::AddD => BIN_ADD_D,
        Bin::SubD => BIN_SUB_D,
        Bin::MulD => BIN_MUL_D,
        Bin::DivD => BIN_DIV_D,
        Bin::MinD => BIN_MIN_D,
        Bin::MaxD => BIN_MAX_D,
        Bin::MakeD => BIN_MAKE_D,
    }
}

fn cmp_code(c: Cmp) -> u32 {
    match c {
        Cmp::Lt => CMP_LT,
        Cmp::Le => CMP_LE,
        Cmp::Gt => CMP_GT,
        Cmp::Ge => CMP_GE,
        Cmp::Eq => CMP_EQ,
        Cmp::Ne => CMP_NE,
    }
}

fn types(out: &mut Vec<u32>, ts: &[Ty]) {
    for chunk in ts.chunks(4) {
        let mut w = 0;
        for (k, t) in chunk.iter().enumerate() {
            w |= ty_code(*t) << (k * 8);
        }
        out.push(w);
    }
}

fn program(out: &mut Vec<u32>, marks: &mut Vec<(u32, u32)>, p: &Program, frame: u32, function: bool) -> Result<(), String> {
    if !function && (!p.params.is_empty() || !p.results.is_empty()) {
        return Err("entry parameters and results are not module operations".into());
    }
    out.extend([word(p.vals.len(), "value count")?, word(p.vars.len(), "variable count")?, frame, p.uses_f64() as u32]);
    types(out, &p.vals);
    types(out, &p.vars);
    out.extend(p.params.iter().chain(&p.results).map(|v| v.0));
    enum Task<'a> {
        Block(&'a [Stmt]),
        Statements(&'a [Stmt]),
        End(usize),
    }
    let mut stack = vec![Task::Block(&p.body)];
    while let Some(task) = stack.pop() {
        match task {
            Task::Block(b) => {
                let at = out.len();
                out.push(0);
                stack.push(Task::End(at));
                stack.push(Task::Statements(b));
            }
            Task::End(at) => out[at] = word(out.len() - at - 1, "block word length")?,
            Task::Statements(b) => {
                let Some((s, rest)) = b.split_first() else { continue };
                stack.push(Task::Statements(rest));
                match s {
                    // Coincident marks all stay: a caller's statement and the first statement of
                    // a helper inlined into it begin at the same code index, and both begin there.
                    Stmt::Mark(id) => marks.push((word(out.len(), "mark code index")?, *id)),
                    Stmt::Def(dst, op) => {
                        let d = dst.0;
                        match *op {
                            Op::ConstF(x) => out.extend([OP_CONST, d, x.to_bits()]),
                            Op::ConstI(x) => out.extend([OP_CONST, d, x as u32]),
                            Op::ConstB(x) => out.extend([OP_CONST, d, x as u32]),
                            Op::ConstD(x) => out.extend([OP_CONST_D, d, x.to_bits() as u32, (x.to_bits() >> 32) as u32]),
                            Op::Get(v) => out.extend([OP_GET, d, v.0]),
                            Op::Un(u, a) => out.extend([OP_UN | un_code(u) << SUB_SHIFT, d, a.0]),
                            Op::Bin(b, a, c) => out.extend([OP_BIN | bin_code(b) << SUB_SHIFT, d, a.0, c.0]),
                            Op::CmpF(c, a, b) => out.extend([OP_CMP_F | cmp_code(c) << SUB_SHIFT, d, a.0, b.0]),
                            Op::CmpI(c, a, b) => out.extend([OP_CMP_I | cmp_code(c) << SUB_SHIFT, d, a.0, b.0]),
                            Op::CmpD(c, a, b) => out.extend([OP_CMP_D | cmp_code(c) << SUB_SHIFT, d, a.0, b.0]),
                            Op::Sel(c, a, b) => out.extend([OP_SEL, d, c.0, a.0, b.0]),
                            Op::Wrap(x, len) => out.extend([OP_WRAP, d, x.0, len]),
                            Op::Load { region: r, base, extent, off } => {
                                out.extend([OP_LOAD | region(r)? << SUB_SHIFT, d, base, extent, offset(off)?]);
                                buffer_operand(out, r);
                            }
                            Op::Fma(Fma::MulAddI, a, b, c) => out.extend([OP_FMA | FMA_MUL_ADD_I << SUB_SHIFT, d, a.0, b.0, c.0]),
                            Op::Fma(..) => return Err("float Fma is not a module operation".into()),
                            Op::In { .. } => return Err("In is not a module operation".into()),
                            Op::FrameCount => return Err("FrameCount is not a module operation".into()),
                            Op::BufLen(_) => return Err("BufLen is not a module operation".into()),
                        }
                    }
                    Stmt::Set(v, x) => out.extend([OP_SET, v.0, x.0]),
                    Stmt::Store { region: r, base, extent, off, val } => {
                        out.extend([OP_STORE | region(*r)? << SUB_SHIFT, *base, *extent, offset(*off)?, val.0]);
                        buffer_operand(out, *r);
                    }
                    Stmt::If(c, t, e) => {
                        out.extend([OP_IF, c.0]);
                        stack.push(Task::Block(e));
                        stack.push(Task::Block(t));
                    }
                    Stmt::Loop { cap, body } => {
                        out.extend([OP_LOOP, *cap]);
                        stack.push(Task::Block(body));
                    }
                    Stmt::Break(d) => out.extend([OP_BREAK, *d]),
                    Stmt::Continue(d) => out.extend([OP_CONTINUE, *d]),
                    Stmt::Port { port, site, args, rets } => {
                        out.extend([OP_PORT, *port as u32, *site, word(args.len(), "port argument count")?, word(rets.len(), "port result count")?]);
                        out.extend(args.iter().map(|v| v.0));
                        out.extend(rets.iter().map(|v| v.0));
                    }
                    Stmt::Out { .. } => return Err("Out is not a module operation".into()),
                    Stmt::CallHost { .. } => return Err("CallHost is not a module operation".into()),
                    Stmt::Call { f, args, rets } => {
                        out.extend([OP_CALL, u32::from(*f), word(args.len(), "call argument count")?, word(rets.len(), "call result count")?]);
                        out.extend(args.iter().chain(rets).map(|v| v.0));
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn encode(l: &EventLowered, profile_id: u32) -> Result<Vec<u32>, String> {
    encode_marked(l, profile_id, &[]).map(|(module, _)| module)
}

pub(crate) fn encode_marked(l: &EventLowered, profile_id: u32, images: &[crate::assets::Image]) -> Result<(Vec<u32>, Vec<(u32, u32)>), String> {
    if l.entries.iter().any(|e| e.program.funcs.len() != l.funcs.len()) {
        return Err("function metadata count differs from programs".into());
    }
    let mut marks = Vec::new();
    let mut strings = Strings::default();
    for e in &l.entries { strings.intern(&e.name)?; }
    for d in &l.decls { strings.intern(&d.name)?; }
    for v in &l.state_vars {
        strings.intern(&v.name)?;
        strings.intern(&v.sig)?;
    }
    for f in &l.funcs { strings.intern(&f.name)?; }
    let mut sections: [Vec<u32>; SEC_BUFS as usize] = std::array::from_fn(|_| Vec::new());
    sections[0].extend([
        word(l.state_init.len(), "state words")?, word(l.shared_init.len(), "shared words")?,
        word(l.sites.len(), "site count")?, word(strings.values.len(), "string count")?,
        word(l.entries.len(), "entry count")?, word(l.decls.len(), "declaration count")?,
        word(l.state_vars.len(), "state variable count")?, word(l.funcs.len(), "function count")?,
    ]);
    sections[1].extend_from_slice(&l.state_init);
    sections[2].extend_from_slice(&l.shared_init);
    for s in &strings.values {
        sections[3].push(word(s.len(), "string byte length")?);
        for chunk in s.as_bytes().chunks(4) {
            let mut bytes = [0; 4];
            bytes[..chunk.len()].copy_from_slice(chunk);
            sections[3].push(u32::from_le_bytes(bytes));
        }
    }
    for v in &l.state_vars {
        sections[4].extend([strings.intern(&v.name)?, strings.intern(&v.sig)?, v.offset, v.words]);
    }
    for d in &l.decls {
        sections[5].extend([d.form as u32, d.handle as u32, strings.intern(&d.name)?, word(d.args.len(), "declaration argument count")?]);
        for a in &d.args {
            sections[5].extend(match *a {
                DeclArg::Int(x) => [ARG_INT, x as u32],
                DeclArg::Float(x) => [ARG_FLOAT, x.to_bits()],
                DeclArg::Entry(x) => [ARG_ENTRY, x as u32],
            });
        }
    }
    for e in &l.entries {
        let start = sections[7].len();
        program(&mut sections[7], &mut marks, &e.program, e.program.frame_words, false).map_err(|why| format!("entry `{}`: {why}", e.name))?;
        let len = word(sections[7].len() - start, "entry code length").map_err(|why| format!("entry `{}`: {why}", e.name))?;
        sections[6].extend([strings.intern(&e.name)?, word(start, "code offset")?, len, e.cost as u32, (e.cost >> 32) as u32]);
    }
    let mut ids: Vec<u32> = (SEC_LAYOUT..=SEC_CODE).collect();
    if !images.is_empty() {
        ids.push(SEC_IMAGES);
        let section = &mut sections[(SEC_IMAGES - 1) as usize];
        section.push(word(images.len(), "image count")?);
        for image in images {
            section.extend([image.width, image.height]);
            for pair in image.pixels.chunks(2) {
                section.push((pair[0] as u32) << 16 | pair.get(1).copied().unwrap_or(0) as u32);
            }
        }
    }
    if !l.funcs.is_empty() {
        ids.push(SEC_FUNCS);
        let entry = l.entries.first().ok_or("functions need an entry")?;
        for (f, meta) in entry.program.funcs.iter().zip(&l.funcs) {
            let start = sections[7].len();
            program(&mut sections[7], &mut marks, f, entry.program.frame_words, true)?;
            let cost = Program { funcs: entry.program.funcs.clone(), ..f.clone() }.total_cost();
            let cost = u32::try_from(cost).map_err(|_| format!("function `{}` cost high word is nonzero", meta.name))?;
            let len = word(sections[7].len() - start, "function length")?;
            sections[(SEC_FUNCS - 1) as usize].extend([strings.intern(&meta.name)?, word(start, "function offset")?,
                len, word(f.params.len(), "parameters")?, word(f.results.len(), "results")?, cost]);
        }
    }
    if !l.buffers.is_empty() {
        ids.push(SEC_BUFS);
        for b in &l.buffers {
            sections[(SEC_BUFS - 1) as usize].extend([u32::from(b.decl), b.elem as u32, b.len, u32::from(b.writable)]);
        }
    }
    let count = ids.len();
    let mut out = vec![0; HEADER_WORDS + count * DIRECTORY_WORDS];
    out[H_MAGIC] = MAGIC;
    out[H_VERSION] = if l.funcs.is_empty() { FORMAT_VERSION } else { FUNCTION_VERSION };
    out[H_PORT_VERSION] = ports::PORT_TABLE_VERSION;
    out[H_PROFILE] = profile_id;
    out[H_SECTIONS] = count as u32;
    for (k, id) in ids.into_iter().enumerate() {
        let section = &sections[(id - 1) as usize];
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        out[at] = id;
        out[at + 1] = word(out.len(), "section offset")?;
        out[at + 2] = word(section.len(), "section length")?;
        out.extend_from_slice(section);
    }
    out[H_WORDS] = word(out.len(), "module length")?;
    Ok((out, marks))
}
