use crate::SyncError;
use relax_lex::lexer::{lex_with, line_starts, LexOptions, Tok, T};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

const ROOT: usize = usize::MAX;
type Proof<T> = Result<T, &'static str>;

fn opens(k: T) -> bool {
    matches!(k, T::OpenBrace | T::OpenBracket | T::OpenParen)
}

fn closes(k: T) -> bool {
    matches!(k, T::CloseBrace | T::CloseBracket | T::CloseParen)
}

fn name(k: T) -> bool {
    matches!(k, T::Ident | T::RawIdent)
}

#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum Context {
    Items,
    Block,
    Fields,
    Literal,
    Variants,
    Arms,
    Parameters,
    TupleFields,
    Other,
}

// All indices refer to the original source, including during removal. Delimiter
// links let a rule cross a body or a literal's arguments without searching it.
pub(crate) struct Source<'a> {
    pub path: &'a str,
    pub src: &'a str,
    pub toks: Vec<Tok>,
    mate: Vec<usize>,
    parent: Vec<usize>,
    opaque: Vec<bool>,
    lines: OnceCell<Vec<u32>>,
}

pub(crate) struct Edits {
    pub ranges: Vec<Range<usize>>,
    pub count: usize,
    pub errors: Vec<String>,
    pub remaining: usize,
}

impl<'a> Source<'a> {
    pub fn parse(path: &'a str, src: &'a [u8], edition: u16) -> Result<Self, SyncError> {
        let src = std::str::from_utf8(src).map_err(|e| format!("{path}: invalid Rust source: {e}"))?;
        let mut toks = Vec::new();
        lex_with(src.as_bytes(), &LexOptions { edition, trivia: true }, &mut toks)
            .map_err(|e| diagnostic(path, src, &line_starts(src.as_bytes()), e.lo as usize, &e.msg))?;
        // A comment between tokens remains in the byte stream. Only doc
        // comments have an attachment to the following or enclosing item.
        toks.retain(|t| t.kind != T::Comment);
        let n = toks.len();
        let mut result = Self {
            path, src, toks, mate: vec![ROOT; n], parent: vec![ROOT; n], opaque: vec![false; n],
            lines: OnceCell::new(),
        };
        let mut top = ROOT;
        let mut opaque = false;
        for i in 0..n {
            let k = result.kind(i);
            result.parent[i] = top;
            result.opaque[i] = opaque;
            if opens(k) {
                let invocation = i >= 2 && result.kind(i - 1) == T::Not
                    && (name(result.kind(i - 2)) || result.text(i - 2) == "try");
                let definition = i >= 3 && name(result.kind(i - 1)) && result.kind(i - 2) == T::Not
                    && result.text(i - 3) == "macro_rules";
                opaque |= invocation || definition;
                top = i;
            } else if closes(k) {
                if top == ROOT || !matches!((result.kind(top), k),
                    (T::OpenBrace, T::CloseBrace) | (T::OpenParen, T::CloseParen) |
                    (T::OpenBracket, T::CloseBracket))
                {
                    return Err(result.error(i, "unmatched closing delimiter"));
                }
                result.mate[top] = i;
                result.mate[i] = top;
                opaque = result.opaque[top];
                top = result.parent[top];
            }
        }
        if top != ROOT {
            return Err(result.error(top, "unclosed delimiter"));
        }
        Ok(result)
    }

    pub fn kind(&self, i: usize) -> T {
        self.toks.get(i).map_or(T::Eof, |t| t.kind)
    }

    pub fn text(&self, i: usize) -> &'a str {
        self.toks.get(i).map_or("", |t| t.text(self.src))
    }

    pub fn after(&self, i: usize) -> usize {
        if opens(self.kind(i)) { self.mate[i] + 1 } else { i + 1 }
    }

    fn before(&self, i: usize) -> usize {
        if i == 0 { return ROOT; }
        if closes(self.kind(i - 1)) { self.mate[i - 1] } else { i - 1 }
    }

    fn end(&self, i: usize) -> usize {
        self.toks[i - 1].hi as usize
    }

    fn limit(&self, i: usize) -> usize {
        let parent = self.parent[i];
        if parent == ROOT { self.toks.len() - 1 } else { self.mate[parent] }
    }

    pub fn lines(&self) -> &[u32] {
        self.lines.get_or_init(|| line_starts(self.src.as_bytes()))
    }

    pub fn error(&self, i: usize, message: &str) -> String {
        diagnostic(self.path, self.src, self.lines(), self.toks[i].lo as usize, message)
    }

    pub fn in_macro(&self, i: usize) -> bool { self.opaque[i] }

    pub fn at_root(&self, i: usize) -> bool { self.parent[i] == ROOT }

    pub fn in_attribute(&self, i: usize) -> bool {
        let mut p = self.parent[i];
        while p != ROOT {
            if self.kind(p) == T::OpenBracket && ((p > 0 && self.kind(p - 1) == T::Pound) ||
                (p >= 2 && self.kind(p - 1) == T::Not && self.kind(p - 2) == T::Pound)) { return true; }
            p = self.parent[p];
        }
        false
    }

    fn attr(&self, i: usize) -> Option<usize> {
        if self.kind(i) != T::Pound { return None; }
        let open = i + 1 + usize::from(self.kind(i + 1) == T::Not);
        (self.kind(open) == T::OpenBracket).then_some(open)
    }

    fn outer_doc(&self, i: usize) -> bool {
        self.kind(i) == T::DocComment && !self.text(i).starts_with("//!") && !self.text(i).starts_with("/*!")
    }

    pub fn attr_start(&self, mut i: usize) -> usize {
        while i > 0 {
            if self.outer_doc(i - 1) {
                i -= 1;
            } else if self.kind(i - 1) == T::CloseBracket {
                let open = self.mate[i - 1];
                if open == 0 || self.kind(open - 1) != T::Pound { break; }
                i = open - 1;
            } else {
                break;
            }
        }
        i
    }

    fn after_attrs(&self, mut i: usize) -> usize {
        loop {
            if self.outer_doc(i) {
                i += 1;
            } else if let Some(open) = self.attr(i) {
                i = self.after(open);
            } else {
                return i;
            }
        }
    }

    fn word(&self, i: usize) -> &'a str {
        let s = self.text(i);
        s.strip_prefix("r#").unwrap_or(s)
    }

    // Only inspect complete attribute token trees. A nested cfg introduced
    // by cfg_attr can depend on test even if its first predicate does not.
    fn test_cfg(&self, open: usize) -> Proof<bool> {
        let cfg = self.word(open + 1);
        if !matches!(cfg, "cfg" | "cfg_attr") { return Ok(false); }
        let end = self.mate[open];
        let paren = open + 2;
        let first = paren + 1;
        if !(first..end).any(|i| self.word(i) == "test") { return Ok(false); }
        if self.kind(paren) != T::OpenParen || self.after(paren) != end {
            return Err("unrecognised test cfg attribute");
        }
        let close = self.mate[paren];
        if cfg == "cfg" && self.word(first) == "test" && first + 1 == close {
            return Ok(true);
        }
        if cfg == "cfg" && self.word(first) == "all" && self.kind(first + 1) == T::OpenParen
            && self.after(first + 1) == close && self.word(first + 2) == "test"
            && matches!(self.kind(first + 3), T::Comma | T::CloseParen)
        {
            return Ok(true);
        }
        if self.word(first) == "not" && self.kind(first + 1) == T::OpenParen
            && self.word(first + 2) == "test" && self.mate[first + 1] == first + 3
        {
            let next = first + 4;
            if (cfg == "cfg" && next == close) || (cfg == "cfg_attr" && self.kind(next) == T::Comma
                && !(next + 1..close).any(|i| self.word(i) == "test"))
            {
                return Ok(false);
            }
        }
        Err("unsupported cfg or cfg_attr involving test")
    }

    fn visibility(&self, i: usize) -> usize {
        if self.kind(i) != T::KwPub { return i; }
        if self.kind(i + 1) == T::OpenParen { self.after(i + 1) } else { i + 1 }
    }

    fn path_end(&self, mut i: usize) -> Option<usize> {
        if self.kind(i) == T::PathSep { i += 1; }
        if !self.path_name(i) { return None; }
        i += 1;
        while self.kind(i) == T::PathSep && self.path_name(i + 1) { i += 2; }
        Some(i)
    }

    fn path_name(&self, i: usize) -> bool {
        name(self.kind(i)) || matches!(self.kind(i), T::KwCrate | T::KwSuper | T::KwSelfType | T::KwSelfValue)
    }

    // Rust angle brackets are punctuation, not token-tree delimiters. A
    // comma inside Map<A, B> or inside closure parameters does not start
    // a new field/parameter/arm. Index every proven start once per list.
    fn list_starts(&self, open: usize, context: Context, generic: bool) -> HashSet<usize> {
        let types = generic || matches!(context, Context::Fields | Context::Parameters | Context::TupleFields);
        let limit = if generic { self.limit(open) } else { self.mate[open] };
        let mut starts = HashSet::new();
        let mut i = open + 1;
        let mut depth = 0i32;
        let mut pattern = context == Context::Arms;
        starts.insert(i);
        while i < limit {
            match self.kind(i) {
                T::Lt if types || depth > 0 || self.kind(i - 1) == T::PathSep => depth += 1,
                T::Gt | T::Shr if types || depth > 0 => {
                    depth -= if self.kind(i) == T::Shr { 2 } else { 1 };
                    if depth < 0 { break; }
                }
                T::Comma if depth == 0 => { starts.insert(i + 1); pattern = context == Context::Arms; }
                T::KwIf | T::FatArrow if context == Context::Arms => pattern = false,
                T::OpenBrace if context == Context::Arms && self.kind(i - 1) == T::FatArrow => {
                    starts.insert(self.after(i));
                    pattern = true;
                }
                T::Lt if !types && depth == 0 => break,
                T::Or if !types && depth == 0 && !pattern => break,
                _ => {}
            }
            i = self.after(i);
        }
        starts
    }

    fn generic_starts(&self) -> HashSet<usize> {
        let mut starts = HashSet::new();
        for i in 1..self.toks.len() {
            if self.kind(i) == T::Lt && (self.kind(i - 1) == T::KwImpl || (i >= 2 &&
                name(self.kind(i - 1)) && matches!(self.kind(i - 2),
                    T::KwFn | T::KwStruct | T::KwEnum | T::KwTrait | T::KwType)))
            {
                starts.extend(self.list_starts(i, Context::Other, true));
            }
        }
        starts
    }

    fn before_angles(&self, mut i: usize, first: usize) -> Option<usize> {
        let mut depth = 0i32;
        while i > first {
            i = self.before(i);
            match self.kind(i) {
                T::Gt => depth += 1,
                T::Shr => depth += 2,
                T::Lt => depth -= 1,
                T::Semi | T::OpenBrace => return None,
                _ => {}
            }
            if depth == 0 { return Some(i); }
        }
        None
    }

    fn parameter_owner(&self, open: usize) -> T {
        let mut i = open;
        if i > 0 && matches!(self.kind(i - 1), T::Gt | T::Shr) {
            let Some(start) = self.before_angles(i, 0) else { return T::Eof; };
            i = start;
        }
        if i >= 2 && name(self.kind(i - 1)) { self.kind(i - 2) } else { T::Eof }
    }

    fn context(&self, open: usize) -> Context {
        if open == ROOT { return Context::Items; }
        if self.kind(open) == T::OpenParen {
            return match self.parameter_owner(open) {
                T::KwFn => Context::Parameters,
                T::KwStruct => Context::TupleFields,
                _ => Context::Other,
            };
        }
        if self.kind(open) != T::OpenBrace { return Context::Other; }
        if open > 0 && matches!(self.kind(open - 1), T::Or | T::OrOr) { return Context::Block; }
        // Inspect siblings of the opening brace only. Nested groups (notably
        // a function's parameter list) cannot introduce its container.
        let enclosing = self.parent[open];
        let first = if enclosing == ROOT { 0 } else { enclosing + 1 };
        let mut i = open;
        while i > first {
            if self.kind(i - 1) == T::CloseBrace { break; }
            if matches!(self.kind(i - 1), T::Gt | T::Shr) {
                if let Some(start) = self.before_angles(i, first) { i = start; continue; }
            }
            i = self.before(i);
            match self.kind(i) {
                T::OpenParen if self.parameter_owner(i) == T::KwFn => return Context::Block,
                T::KwFn | T::KwIf | T::KwElse | T::KwWhile | T::KwLoop | T::KwFor => return Context::Block,
                T::KwMod | T::KwImpl | T::KwTrait | T::KwExtern => return Context::Items,
                T::KwStruct => return Context::Fields,
                T::KwEnum => return Context::Variants,
                T::KwMatch => return Context::Arms,
                T::Eq | T::Semi | T::FatArrow | T::Comma | T::KwLet => break,
                _ => {}
            }
        }
        let mut start = open;
        while start > first && self.path_name(start - 1) {
            start -= 1;
            if start > first && self.kind(start - 1) == T::PathSep { start -= 1; } else { break; }
        }
        if start < open && self.expression_position(start) { return Context::Literal; }
        if start == open && self.expression_position(open) { return Context::Block; }
        Context::Other
    }

    // Anchor a literal to an expression slot. A path followed by braces
    // also occurs in patterns, including inside tuple-variant patterns.
    fn expression_position(&self, start: usize) -> bool {
        if start == 0 { return false; }
        let parent = self.parent[start];
        match self.kind(start - 1) {
            T::Eq | T::KwReturn | T::FatArrow => true,
            T::Colon => self.context(parent) == Context::Literal,
            T::OpenBrace | T::Semi | T::CloseBrace => self.context(parent) == Context::Block,
            T::OpenParen | T::Comma if parent != ROOT && self.kind(parent) == T::OpenParen => {
                let mut callee = parent;
                while callee > 0 && self.path_name(callee - 1) {
                    callee -= 1;
                    if callee > 0 && self.kind(callee - 1) == T::PathSep { callee -= 1; } else { break; }
                }
                (callee > 0 && self.kind(callee - 1) == T::Dot)
                    || self.expression_position(callee)
            }
            _ => false,
        }
    }

    fn item_boundary(&self, start: usize) -> bool {
        if start == 0 { return true; }
        match self.kind(start - 1) {
            T::OpenBrace | T::Semi | T::CloseBrace => true,
            T::DocComment => !self.outer_doc(start - 1),
            T::CloseBracket => {
                let open = self.mate[start - 1];
                open >= 2 && self.kind(open - 1) == T::Not && self.kind(open - 2) == T::Pound
            }
            _ => false,
        }
    }

    fn semi(&self, mut i: usize, end: usize) -> Proof<usize> {
        while i < end {
            if self.kind(i) == T::Semi { return Ok(i + 1); }
            i = self.after(i);
        }
        Err("cannot prove the terminating semicolon")
    }

    fn item(&self, i: usize, limit: usize) -> Proof<Option<usize>> {
        let mut head = self.visibility(i);
        while matches!(self.kind(head), T::KwUnsafe | T::KwAsync) { head += 1; }
        if self.kind(head) == T::KwConst && matches!(self.kind(head + 1), T::KwFn | T::KwUnsafe) {
            head += 1;
            if self.kind(head) == T::KwUnsafe { head += 1; }
        }
        if self.kind(head) == T::KwExtern {
            head += 1;
            if self.kind(head) == T::Str { head += 1; }
            if self.kind(head) == T::OpenBrace { return Ok(Some(self.after(head))); }
            if self.kind(head) == T::KwCrate { return self.semi(head, limit).map(Some); }
        }
        match self.kind(head) {
            T::KwConst if !name(self.kind(head + 1)) => Err("unrecognised const statement"),
            T::KwUse | T::KwStatic | T::KwConst | T::KwType => self.semi(head, limit).map(Some),
            T::KwFn | T::KwMod | T::KwImpl | T::KwEnum | T::KwStruct | T::KwTrait => {
                let mut depth = 0i32;
                let mut j = head + 1;
                while j < limit {
                    match self.kind(j) {
                        T::Lt => depth += 1,
                        T::Gt => depth -= 1,
                        T::Shr => depth -= 2,
                        T::Semi if depth == 0 => return Ok(Some(j + 1)),
                        T::OpenBrace if depth == 0 => return Ok(Some(self.after(j))),
                        _ => {}
                    }
                    if depth < 0 { break; }
                    j = self.after(j);
                }
                Err("cannot prove the end of the item header")
            }
            _ => {
                let Some(bang) = self.path_end(head) else { return Ok(None); };
                if self.kind(bang) != T::Not { return Ok(None); }
                let mut group = bang + 1;
                if self.word(head) == "macro_rules" && name(self.kind(group)) { group += 1; }
                if !opens(self.kind(group)) { return Err("expected a macro token tree"); }
                let end = self.after(group);
                if self.kind(end) == T::Semi { Ok(Some(end + 1)) }
                else if self.kind(group) == T::OpenBrace && self.after_block(end, limit) { Ok(Some(end)) }
                else { Ok(None) }
            }
        }
    }

    fn after_block(&self, end: usize, limit: usize) -> bool {
        end == limit || name(self.kind(end)) || matches!(self.kind(end),
            T::KwLet | T::KwReturn | T::KwIf | T::KwMatch | T::KwLoop | T::KwWhile | T::KwFor |
            T::KwBreak | T::KwContinue | T::KwPub | T::KwFn | T::KwUse | T::KwMod | T::KwImpl |
            T::KwStruct | T::KwEnum | T::KwConst | T::Pound | T::DocComment)
    }

    // A type list tracks generic nesting. In expressions, only an explicit
    // turbofish opens type arguments. Refuse an ambiguous comma (e.g. in
    // closure parameters or an unqualified cast type) instead of truncating.
    fn comma_end(&self, mut i: usize, limit: usize, types: bool, generic: bool) -> Proof<usize> {
        let mut depth = 0i32;
        while i < limit {
            match self.kind(i) {
                T::Lt if types || depth > 0 || i > 0 && self.kind(i - 1) == T::PathSep => depth += 1,
                T::Gt | T::Shr if types || depth > 0 => {
                    let width = if self.kind(i) == T::Shr { 2 } else { 1 };
                    if depth < width {
                        if generic {
                            return Ok(if depth == 0 { self.end(i) } else { self.toks[i].lo as usize + depth as usize });
                        }
                        return Err("unbalanced angle brackets in list");
                    }
                    depth -= width;
                }
                T::Comma if depth == 0 => return Ok(self.end(i + 1)),
                T::Lt | T::Or | T::FatArrow | T::Semi if depth == 0 => return Err("ambiguous list element boundary"),
                _ => {}
            }
            i = self.after(i);
        }
        if depth == 0 && !generic { Ok(self.end(limit)) } else { Err("unterminated list element") }
    }

    fn match_arm(&self, mut i: usize, limit: usize) -> Proof<usize> {
        while i < limit && self.kind(i) != T::FatArrow {
            if self.kind(i) == T::Comma { return Err("missing match arm arrow"); }
            i = self.after(i);
        }
        if i + 1 >= limit { return Err("missing match arm body"); }
        let body = i + 1;
        if self.kind(body) == T::OpenBrace {
            let end = self.after(body);
            if self.kind(end) == T::Comma { return Ok(self.end(end + 1)); }
            if end == limit || name(self.kind(end)) || self.kind(end) == T::Pound
                || self.outer_doc(end) || self.kind(end).is_literal()
            {
                return Ok(self.end(end));
            }
            return Err("ambiguous boundary after block-bodied match arm");
        }
        self.comma_end(body, limit, false, false)
    }

    fn statement(&self, i: usize, limit: usize) -> Proof<usize> {
        match self.kind(i) {
            T::KwLet | T::KwReturn => self.semi(i, limit),
            T::OpenBrace | T::KwIf => {
                let mut head = i;
                let mut end;
                loop {
                    if self.kind(head) == T::KwIf {
                        head += 1;
                        while head < limit && self.kind(head) != T::OpenBrace {
                            if matches!(self.kind(head), T::KwLet | T::Semi | T::Or | T::KwMatch | T::KwIf) {
                                return Err("ambiguous if condition");
                            }
                            head = self.after(head);
                        }
                    }
                    if self.kind(head) != T::OpenBrace { return Err("expected a statement block"); }
                    end = self.after(head);
                    if self.kind(end) != T::KwElse { break; }
                    head = end + 1;
                }
                if self.kind(end) == T::Semi { return Ok(end + 1); }
                if self.after_block(end, limit) {
                    Ok(end)
                } else {
                    Err("ambiguous continuation of block expression")
                }
            }
            // Expressions without a leading block require their own
            // semicolon in statement position. A brace-delimited macro
            // may omit it and has already been handled by item().
            _ if self.path_end(i).is_some_and(|end| self.kind(end) != T::Not) => self.semi(i, limit),
            k if k.is_literal() || matches!(k, T::OpenParen | T::OpenBracket | T::Not | T::Minus |
                T::Star | T::And | T::KwTrue | T::KwFalse | T::KwBreak | T::KwContinue) => self.semi(i, limit),
            _ => Err("unrecognised statement"),
        }
    }

    fn extent(&self, start: usize, i: usize, generics: &HashSet<usize>,
        lists: &mut HashMap<usize, HashSet<usize>>) -> Proof<usize> {
        let open = self.parent[start];
        let limit = self.limit(start);
        if generics.contains(&start) {
            return self.comma_end(i, limit, true, true);
        }
        let context = self.context(open);
        match context {
            Context::Items | Context::Block if self.item_boundary(start) => {
                if let Some(end) = self.item(i, limit)? { return Ok(self.end(end)); }
                if context == Context::Block { return self.statement(i, limit).map(|end| self.end(end)); }
                Err("unrecognised item")
            }
            Context::Arms | Context::Fields | Context::Literal | Context::Variants | Context::TupleFields | Context::Parameters =>
            {
                if !lists.entry(open).or_insert_with(|| self.list_starts(open, context, false)).contains(&start) {
                    return Err("attribute is not at a proven list element boundary");
                }
                if context == Context::Arms { return self.match_arm(i, limit); }
                let head = self.visibility(i);
                if matches!(context, Context::Fields | Context::Literal | Context::Variants) && !name(self.kind(head)) {
                    return Err("unrecognised field or variant");
                }
                if context == Context::Fields && self.kind(head + 1) != T::Colon {
                    return Err("expected a named field and type");
                }
                self.comma_end(i, limit, !matches!(context, Context::Literal | Context::Variants), false)
            }
            _ => Err("unrecognised attribute position in its enclosing delimiter"),
        }
    }

    pub fn edits(&self) -> Edits {
        let mut edits = Edits { ranges: Vec::new(), count: 0, errors: Vec::new(), remaining: 0 };
        let mut generics = None;
        let mut lists = HashMap::new();
        let mut previous = None;
        for i in 0..self.toks.len() {
            let Some(open) = self.attr(i) else { continue; };
            let result = self.test_cfg(open).and_then(|test| {
                if !test { return Ok(None); }
                if self.opaque[i] { return Err("test cfg inside a macro token tree"); }
                if open != i + 1 { return Err("test cfg in an inner attribute"); }
                let start = self.attr_start(i);
                if previous == Some(start) { return Ok(None); }
                previous = Some(start);
                let end = self.extent(start, self.after_attrs(start),
                    generics.get_or_insert_with(|| self.generic_starts()), &mut lists)?;
                Ok(Some(self.toks[start].lo as usize..end))
            });
            match result {
                Ok(Some(range)) => {
                    if edits.ranges.last().is_some_and(|outer| outer.end > range.start) {
                        if range.end > edits.ranges.last().unwrap().end {
                            edits.errors.push(self.error(i, "overlapping cfg removals"));
                        }
                    } else {
                        edits.ranges.push(range);
                        edits.count += 1;
                    }
                }
                Err(why) => edits.errors.push(self.error(i, why)),
                Ok(None) => {}
            }
        }
        // Check the surviving tokens, including cfg predicates written in an
        // unexpected context. Literals and comments are indivisible tokens.
        let mut ranges = edits.ranges.iter().peekable();
        for i in 0..self.toks.len() {
            let pos = self.toks[i].lo as usize;
            while ranges.peek().is_some_and(|range| range.end <= pos) { ranges.next(); }
            if ranges.peek().is_some_and(|range| range.start <= pos) { continue; }
            if self.word(i) == "cfg" && self.kind(i + 1) == T::OpenParen &&
                (self.word(i + 2) == "test" || self.word(i + 2) == "all" &&
                self.kind(i + 3) == T::OpenParen && self.word(i + 4) == "test")
            {
                edits.remaining += 1;
                if !(i >= 2 && self.attr(i - 2) == Some(i - 1) && !edits.errors.is_empty()) {
                    edits.errors.push(self.error(i, "test cfg tokens survive outside a recognised attribute"));
                }
            }
        }
        edits
    }
}

fn diagnostic(path: &str, src: &str, lines: &[u32], pos: usize, message: &str) -> String {
    let line = lines.partition_point(|&start| start as usize <= pos);
    let mut out = format!("S1 {path}:{line}: {message}");
    let length = lines.len() - usize::from(src.ends_with('\n'));
    let first = line.saturating_sub(2).min(length.saturating_sub(3));
    for i in first..(first + 3).min(length) {
        let end = lines.get(i + 1).map_or(src.len(), |&n| n as usize);
        let text = src[lines[i] as usize..end].trim_end_matches(['\r', '\n']);
        out.push_str(&format!("\n  {:>5} | {text}", i + 1));
    }
    out
}

pub(crate) fn apply(src: &[u8], ranges: &[Range<usize>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() - ranges.iter().map(Range::len).sum::<usize>());
    let mut pos = 0;
    for range in ranges {
        out.extend_from_slice(&src[pos..range.start]);
        pos = range.end;
    }
    out.extend_from_slice(&src[pos..]);
    out
}

pub fn strip_cfg_test(path: &str, src: &[u8], edition: u16) -> Result<(Vec<u8>, usize), SyncError> {
    let source = Source::parse(path, src, edition)?;
    let edits = source.edits();
    if !edits.errors.is_empty() { return Err(edits.errors.join("\n")); }
    Ok((apply(src, &edits.ranges), edits.count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_removes_every_recognised_cfg_test_context() {
        // The markers specify the exact bytes each rule owns, including
        // commas, attached docs, and whitespace between stacked attributes.
        let marked = r#"//! Inner doc remains.
«/// Attached doc.
#[allow(dead_code)] /* between */ #[cfg(test)] mod x;»
«#[cfg(all(test, feature = "x"))] mod nested { fn x() {} }»
«#[cfg(test)] fn f() -> i32 { 1 }» «#[cfg(test)] use a::{b, c};»
«#[cfg(test)] impl X { fn f() {} }»
impl X { «#[cfg(test)] pub(crate) fn f() {}» fn kept() {} }
impl<A, B> Pair<A, B> { «#[cfg(test)] fn paired() {}» }
trait Y { «#[cfg(test)] fn f();» fn kept(); }
struct X { «#[cfg(test)] removed: Map<A, B>,» value: usize }
enum E { A, «#[cfg(test)] B(i32),» C }
struct Pair(«#[cfg(test)] pub Map<A, B>,» usize);
fn run<«#[cfg(test)] U: Trait<A, B>,» T>(«#[cfg(test)] x: (U, T),» y: T) {
    let _ = X { «#[cfg(test)] value: make::<A, B>(),» kept: 0 };
    «#[cfg(test)] let x = match y { _ => { y } };» use_it();
    «#[cfg(test)] return 42;» «#[cfg(test)] observe(y);»
    consume(|a, b| { «#[cfg(test)] let c = a;» use_it(b) });
    «#[cfg(test)] { use_it(); }» #[cfg(not(test))] { keep(); }
    match y {
        «#[cfg(test)] A(v) if v > 1 => make(v),»
        «#[cfg(test)] B => { x() },»
        «#[cfg(test)] C => { y() }» D => { keep() }
        «#[cfg(test)] E => tail()»
    }
}
#[cfg(not(test))] fn keep() {}
#[cfg_attr(not(test), allow(unused))] fn keep_too() {}
"#;
        let mut input = String::new();
        let mut expected = String::new();
        let mut pieces = marked.split('«');
        let prefix = pieces.next().unwrap();
        input.push_str(prefix);
        expected.push_str(prefix);
        let mut count = 0;
        for piece in pieces {
            let (removed, kept) = piece.split_once('»').unwrap();
            input.push_str(removed);
            input.push_str(kept);
            expected.push_str(kept);
            count += 1;
        }
        let (out, found) = strip_cfg_test("sample.rs", input.as_bytes(), 2021).unwrap();
        assert_eq!(found, count, "number of removed constructs");
        assert_eq!(std::str::from_utf8(&out).unwrap(), expected, "bytes outside the marked spans");
        let (again, remaining) = strip_cfg_test("sample.rs", &out, 2021).unwrap();
        assert_eq!(remaining, 0, "surviving test cfg tokens");
        assert_eq!(again, out, "second strip changed the file");
    }

    #[test]
    fn strip_refuses_what_it_cannot_prove() {
        for (prefix, target, suffix) in [
            ("macro_rules! a { () => {", "#[cfg(test)] fn f() {}", "} }"),
            ("fn f() { a!(nested![", "#[cfg(test)] token", "]); }"),
            ("#[cfg(test)] mod t { m!(", "#[cfg(test)] item", "); }"),
            ("", "#[cfg(any(test, unix))] fn f() {}", ""),
            ("", "#[cfg_attr(test, inline)] fn f() {}", ""),
            ("", "#[cfg_attr(unix, cfg(test))] fn f() {}", ""),
            ("", "#![cfg(test)]", "fn f() {}"),
            ("fn f() { let _ = [", "#[cfg(test)] 1", "]; }"),
            ("fn f() { let X {", "#[cfg(test)] x,", "y } = value; }"),
            ("fn f() { call(", "#[cfg(test)] value,", "other); }"),
            ("fn f(x: Map<A,", "#[cfg(test)] B,", "C>) {}"),
            ("fn f() { let _ = V { cb: |x,", "#[cfg(test)] y,", "z| x }; }"),
        ] {
            let src = format!("{prefix}\n{target}\n{suffix}");
            let e = strip_cfg_test("refuse.rs", src.as_bytes(), 2024).unwrap_err();
            assert!(e.contains("refuse.rs:2:") && e.contains(target), "{e}");
        }
    }

    #[test]
    fn strip_ignores_test_code_inside_literals() {
        let src = br##"const TEXT: &str = r#"
#[cfg(test)] mod fake { "{" }
#[cfg(any(test, unix))] fn also_fake() {}
"#; // #[cfg(test)] not an attribute
const BYTE: u8 = b'}';"##;
        let (out, count) = strip_cfg_test("literal.rs", src, 2021).unwrap();
        assert_eq!(count, 0);
        assert_eq!(out, src);
    }
}
