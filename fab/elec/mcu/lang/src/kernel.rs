//! Radio kernel front end. Only this API installs the privileged spec.
use std::collections::{HashMap, HashSet};
use makepad_script_compute::{parse::{self, AssignOp, Expr, ExprKind, FnDecl, Item, Span, Stmt, TypeAnn}, ShaderError};
use makepad_script_compute::ir::{PortSig, Ty};
use makepad_script_compute::lower::event::{lower_events, DeclForm, DeclHandle, EventSpec};
use crate::{encode, finish_compiled, load_error, rt, Compiled, Profile};

const PRELUDE: &str = include_str!("kernel_prelude.splash");

const fn types(n: usize) -> &'static [Ty] {
    match n { 0 => &[], 1 => &[Ty::I32], 2 => &[Ty::I32; 2], 4 => &[Ty::I32; 4], _ => panic!("kernel port arity") }
}

const fn port_table() -> [PortSig; rt::ports::KERNEL_BASE as usize + rt::ports::KERNEL_PORTS.len()] {
    let mut table = [PortSig { name: "", params: &[], rets: &[], bufs: &[] }; rt::ports::KERNEL_BASE as usize + rt::ports::KERNEL_PORTS.len()];
    let mut i = 0;
    while i < rt::ports::KERNEL_PORTS.len() {
        let p = rt::ports::KERNEL_PORTS[i];
        table[rt::ports::KERNEL_BASE as usize + i] = PortSig { name: p.name, params: types(p.params.len()), rets: types(p.rets.len()), bufs: &[] };
        i += 1;
    }
    table
}

const fn form_table() -> [DeclForm; rt::ports::FORM_EXPORT as usize + 1] {
    let mut table = [DeclForm { name: "", handle: DeclHandle::Ordinal, class: 0, buffer: None }; rt::ports::FORM_EXPORT as usize + 1];
    table[rt::ports::FORM_EXPORT as usize] = DeclForm { name: "export", handle: DeclHandle::Ordinal, class: 0, buffer: None };
    table
}

/// Full indexed tables: the uncallable empty names reserve indices 0..64.
pub const KERNEL_PORTS: &[PortSig] = &port_table();
pub const KERNEL_FORMS: &[DeclForm] = &form_table();

/// The shared lowerer evaluates an unknown call's arguments before naming the
/// unknown function. For foreign declaration forms, its lambda-value error
/// would hide the actual domain mistake. Only remap failed unresolved calls;
/// a user-defined ordinary helper with the same name retains its diagnostics.
pub(super) fn foreign_form_error(error: ShaderError, items: &[Item], forbidden: &[DeclForm]) -> ShaderError {
    for item in items {
        if let Item::Let { value: Expr { kind: ExprKind::Call(name, _), span }, .. } = item {
            if forbidden.iter().any(|f| f.name == name) && span.start <= error.start && error.end <= span.end
                && !items.iter().any(|i| matches!(i, Item::Fn(f) if f.name == *name)) {
                return ShaderError::new(span.start, span.end, format!("unknown function `{name}`"));
            }
        }
    }
    error
}

fn expr(kind: ExprKind, span: Span) -> Expr { Expr { kind, span } }
fn ident(name: String, span: Span) -> Expr { expr(ExprKind::Ident(name), span) }
fn zero(span: Span) -> Expr { expr(ExprKind::Num(0.0, true), span) }
fn assign(name: &str, value: Expr, span: Span) -> Stmt {
    Stmt::Assign { target: ident(name.into(), span), op: AssignOp::Set, value, span }
}

// Return ownership stops at lambdas. Local functions, like top-level helper
// items, keep their own returns. All other expression containers are visited
// because a block/if/match can occur inside another expression.
fn normalize_expr(e: &mut Expr, result: &str) {
    match &mut e.kind {
        ExprKind::If(arms, otherwise) => {
            for (c, body) in arms { normalize_expr(c, result); normalize(body, result); }
            if let Some(body) = otherwise { normalize(body, result); }
        }
        ExprKind::Match(subject, arms) => {
            normalize_expr(subject, result);
            for (patterns, body) in arms {
                if let Some(patterns) = patterns { for p in patterns { normalize_expr(p, result); } }
                normalize(body, result);
            }
        }
        ExprKind::Block(body) => normalize(body, result),
        ExprKind::Bin(_, a, b) | ExprKind::Index(a, b) | ExprKind::ArrayRepeat(a, b) => {
            normalize_expr(a, result); normalize_expr(b, result);
        }
        ExprKind::Neg(a) | ExprKind::Not(a) | ExprKind::Field(a, _) => normalize_expr(a, result),
        ExprKind::Call(_, args) | ExprKind::ArrayList(args) => { for a in args { normalize_expr(a, result); } }
        ExprKind::StructLit(_, fields) => { for (_, e) in fields { normalize_expr(e, result); } }
        ExprKind::Lambda(_) | ExprKind::Num(..) | ExprKind::Bool(_) | ExprKind::Ident(_) | ExprKind::Str(_) | ExprKind::Unreadable(_) => {}
    }
}

fn normalize(body: &mut Vec<Stmt>, result: &str) {
    let mut out = Vec::with_capacity(body.len());
    for mut statement in core::mem::take(body) {
        match &mut statement {
            Stmt::Return(Some(value), span) => {
                normalize_expr(value, result);
                // Assignment's span is the complete user return. held(i32)
                // therefore reports float/bool/aggregate refusals right here.
                out.push(assign(result, value.clone(), *span));
                out.push(Stmt::Return(None, *span));
                continue;
            }
            Stmt::Let { value, .. } | Stmt::Expr(value) => normalize_expr(value, result),
            Stmt::Assign { target, value, .. } => { normalize_expr(target, result); normalize_expr(value, result); }
            Stmt::For { from, to, body, .. } => { normalize_expr(from, result); normalize_expr(to, result); normalize(body, result); }
            Stmt::While { cond, body, .. } => { normalize_expr(cond, result); normalize(body, result); }
            Stmt::Loop { body, .. } => normalize(body, result),
            Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => {}
        }
        out.push(statement);
    }
    *body = out;
}

// Like normalize, inspect only this function's own returns. In particular,
// a local lambda returning a value does not make its enclosing helper valued.
fn has_value_return(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return(Some(_), _) => true,
        Stmt::Let { value, .. } | Stmt::Expr(value) => returns_value_in(value),
        Stmt::Assign { target, value, .. } => returns_value_in(target) || returns_value_in(value),
        Stmt::For { from, to, body, .. } => returns_value_in(from) || returns_value_in(to) || has_value_return(body),
        Stmt::While { cond, body, .. } => returns_value_in(cond) || has_value_return(body),
        Stmt::Loop { body, .. } => has_value_return(body),
        Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => false,
    })
}

fn returns_value_in(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::If(arms, otherwise) => arms.iter().any(|(c, b)| returns_value_in(c) || has_value_return(b))
            || otherwise.as_ref().is_some_and(|b| has_value_return(b)),
        ExprKind::Match(subject, arms) => returns_value_in(subject) || arms.iter().any(|(patterns, b)| {
            patterns.as_ref().is_some_and(|p| p.iter().any(returns_value_in)) || has_value_return(b)
        }),
        ExprKind::Block(b) => has_value_return(b),
        ExprKind::Bin(_, a, b) | ExprKind::Index(a, b) | ExprKind::ArrayRepeat(a, b) => returns_value_in(a) || returns_value_in(b),
        ExprKind::Neg(a) | ExprKind::Not(a) | ExprKind::Field(a, _) => returns_value_in(a),
        ExprKind::Call(_, args) | ExprKind::ArrayList(args) => args.iter().any(returns_value_in),
        ExprKind::StructLit(_, fields) => fields.iter().any(|(_, e)| returns_value_in(e)),
        ExprKind::Lambda(_) | ExprKind::Num(..) | ExprKind::Bool(_) | ExprKind::Ident(_) | ExprKind::Str(_) | ExprKind::Unreadable(_) => false,
    }
}

// A value as a body's last statement reads as the export's result but would
// be discarded. Preserve the offending expression's span, including calls.
fn value_tail(body: &[Stmt], valued: &HashSet<String>, locals: &HashMap<String, bool>) -> Option<Span> {
    let Some(Stmt::Expr(e)) = body.last() else { return None; };
    let mut locals = locals.clone();
    for s in body {
        if let Stmt::Let { name, value, .. } = s {
            // Local functions capture the preceding bindings. A block/arm
            // gets its own copy, and a lambda's returns stay in that lambda.
            let gives_value = if let ExprKind::Lambda(f) = &value.kind {
                let mut captured = locals.clone();
                // Its own name denotes itself; recursion is left to the lowerer.
                captured.insert(name.clone(), false);
                for (param, _) in &f.params { captured.insert(param.clone(), valued.contains(param)); }
                has_value_return(&f.body) || value_tail(&f.body, valued, &captured).is_some()
            } else {
                // A scalar binding hides an outer closure. Call resolution
                // then falls back to the global helpers/ports, as in lower_events.
                valued.contains(name)
            };
            locals.insert(name.clone(), gives_value);
        }
    }
    match &e.kind {
        ExprKind::Num(..) | ExprKind::Bool(_) | ExprKind::Ident(_) | ExprKind::Bin(..) | ExprKind::Neg(_)
        | ExprKind::Not(_) | ExprKind::Field(..) | ExprKind::Index(..) => Some(e.span),
        ExprKind::Call(name, _) if locals.get(name).copied().unwrap_or_else(|| valued.contains(name)) => Some(e.span),
        ExprKind::If(arms, otherwise) => arms.iter().map(|(_, b)| b).chain(otherwise.iter()).find_map(|b| value_tail(b, valued, &locals)),
        ExprKind::Match(_, arms) => arms.iter().find_map(|(_, b)| value_tail(b, valued, &locals)),
        ExprKind::Block(b) => value_tail(b, valued, &locals),
        _ => None,
    }
}

fn valued_functions(items: &[Item]) -> HashSet<String> {
    let functions: Vec<_> = items.iter().filter_map(|i| if let Item::Fn(f) = i { Some(f) } else { None }).collect();
    let mut valued: HashSet<_> = rt::ports::KERNEL_PORTS.iter().filter(|p| !p.rets.is_empty()).map(|p| p.name.to_owned()).collect();
    // The lowerer resolves named helpers before ports. User helpers also
    // shadow prelude helpers (with_prelude has already renamed those).
    for f in &functions { valued.remove(&f.name); }
    loop {
        let mut changed = false;
        for f in &functions {
            if !valued.contains(&f.name) && (has_value_return(&f.body) || value_tail(&f.body, &valued, &HashMap::new()).is_some()) {
                valued.insert(f.name.clone()); changed = true;
            }
        }
        if !changed { return valued; }
    }
}

fn rewrite(items: Vec<Item>) -> Result<Vec<Item>, ShaderError> {
    let valued = valued_functions(&items);
    let mut exports = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if let Item::Fn(f) = &item {
            if f.name == rt::SETUP { return Err(ShaderError::new(f.span.start, f.span.end, "a kernel module cannot declare a setup fn; use export".into())); }
        }
        let Item::Let { name, ann, value: Expr { kind: ExprKind::Call(form, args), .. }, span } = &item else { out.push(item); continue; };
        if form != "export" { out.push(item); continue; }
        if !exports.insert(name.clone()) { return Err(ShaderError::new(span.start, span.end, format!("two exports cannot share the name `{name}`"))); }
        let [Expr { kind: ExprKind::Lambda(f), .. }] = args.as_slice() else {
            return Err(ShaderError::new(span.start, span.end, "use `let name = export(fn(args) { ... })`".into()));
        };
        let n = f.params.len();
        if n > 6 { return Err(ShaderError::new(span.start, span.end, "export takes at most 6 i32 parameters".into())); }
        if f.params.iter().any(|(_, ty)| ty.as_ref().is_some_and(|ty| *ty != TypeAnn::I32)) {
            return Err(ShaderError::new(span.start, span.end, "export parameters must all be i32".into()));
        }
        if let Some(at) = value_tail(&f.body, &valued, &HashMap::new()) {
            return Err(ShaderError::new(at.start, at.end, "an export's result is `return e`; this trailing value would be discarded".into()));
        }
        let result = format!("{name}#r");
        let params: Vec<_> = (0..n).map(|p| format!("{name}#p{p}")).collect();
        for slot in params.iter().chain(core::iter::once(&result)) {
            out.push(Item::Var { name: slot.clone(), ann: Some(TypeAnn::I32), value: zero(*span), span: *span });
        }
        let mut helper = (**f).clone();
        helper.name = format!("{name}#body");
        helper.span = *span;
        for (_, ty) in &mut helper.params { *ty = Some(TypeAnn::I32); }
        normalize(&mut helper.body, &result);
        // Falling off returns zero. Valued tails have already been refused;
        // keep the helper unit-valued alongside its early bare returns.
        helper.body.push(Stmt::Return(None, *span));
        let body = vec![assign(&result, zero(*span), *span), Stmt::Expr(expr(ExprKind::Call(helper.name.clone(), params.into_iter().map(|p| ident(p, *span)).collect()), *span))];
        out.push(Item::Fn(helper));
        let entry = FnDecl { name: name.clone(), params: vec![], body, span: *span };
        out.push(Item::Let { name: name.clone(), ann: ann.clone(), span: *span, value: expr(ExprKind::Call("export".into(), vec![
            expr(ExprKind::Lambda(Box::new(entry)), *span), expr(ExprKind::Num(n as f64, true), *span),
        ]), *span) });
    }
    Ok(out)
}

/// Compile an in-repo kernel source with the caller's chip-neutral IRQ policy.
/// Callers loading the resulting words use the same policy with load_kernel.
/// App compilation never installs these names.
/// Layout constants are numeric prelude lets, never persistent state; source
/// items cannot redeclare them.
pub fn compile_kernel(source: &str, profile: &Profile, lines: &rt::KernelLines, consts: &[(&str, i32)]) -> Result<Compiled, Vec<ShaderError>> {
    let error = |e| vec![e];
    let tokens = parse::lex(source).map_err(error)?;
    let items = parse::Parser::new(&tokens).items().map_err(error)?;
    let mut prelude = makepad_script_compute::parse_prelude(PRELUDE, source.len());
    let own: HashSet<String> = items.iter().map(|i| i.name().to_owned()).collect();
    // Unlike ordinary prelude helpers, layout names cannot be shadowed by
    // source. Check before with_prelude renames shadowed prelude items.
    for &(name, _) in consts {
        if own.contains(name) {
            let item = items.iter().find(|i| i.name() == name).unwrap();
            let at = match item {
                Item::Let { span, .. } | Item::Var { span, .. } | Item::Struct { span, .. }
                | Item::Use { span, .. } | Item::Decl { span, .. } => *span,
                Item::Fn(f) => f.span,
            };
            return Err(vec![ShaderError::new(at.start, at.end,
                format!("kernel layout constant `{name}` cannot be redeclared"))]);
        }
    }
    let at = Span { start: source.len() + 1 + PRELUDE.len(), end: source.len() + 1 + PRELUDE.len() };
    prelude.extend(consts.iter().map(|&(name, value)| Item::Let {
        name: name.to_owned(), ann: None, value: expr(ExprKind::Num(value as f64, true), at), span: at,
    }));
    let all = rewrite(makepad_script_compute::with_prelude(&items, prelude, &own)).map_err(error)?;
    let spec = EventSpec { ports: KERNEL_PORTS, forms: KERNEL_FORMS, entry_fns: &[], floats: false, max_entry_cost: profile.max_entry_ops,
        cap_port: Some(rt::ports::KERNEL_LOOP_CAP), marks: true, call_ops: profile.call_ops };
    let lowered = lower_events(&all, source.len() + 1, &spec)
        .map_err(|e| vec![foreign_form_error(e, &all, crate::FORMS)])?;
    let (module, marks) = encode::encode_marked(&lowered, profile.id, &[])
        .map_err(|m| vec![ShaderError::new(0, 1, format!("internal compiler error: {m}"))])?;
    rt::module::load_kernel(&module, profile, lines)
        .map_err(|e| vec![load_error(&e, &lowered, profile, &[])])?;
    Ok(finish_compiled(source, module, vec![], lowered, marks, vec![], vec![], vec![]))
}
