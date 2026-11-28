use crate::SyncError;
use relax_lex::lexer::{lex_with, line_starts, LexOptions, Tok, T};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

pub(crate) mod orphans;

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

    // Only a positive predicate, or a conjunct of all(...), is required
    // on every build of the item. any/not/cfg_attr cannot prove a gate.
    fn requires_cfg(&self, start: usize, end: usize, feature: Option<&str>) -> bool {
        if self.word(start) == "all" && self.kind(start + 1) == T::OpenParen && self.after(start + 1) == end {
            let mut child = start + 2;
            while child < end - 1 {
                let mut next = child;
                while next < end - 1 && self.kind(next) != T::Comma { next = self.after(next); }
                if self.requires_cfg(child, next, feature) { return true; }
                child = next + 1;
            }
            return false;
        }
        match feature {
            None => start + 1 == end && self.word(start) == "test",
            Some(name) => start + 3 == end && self.word(start) == "feature" && self.kind(start + 1) == T::Eq
                && crate::modules::literal(self.text(start + 2)).ok() == Some(name),
        }
    }

    fn feature_cfg(&self, open: usize, feature: &str) -> bool {
        let paren = open + 2;
        self.word(open + 1) == "cfg" && self.kind(paren) == T::OpenParen && self.after(paren) == self.mate[open]
            && self.requires_cfg(paren + 1, self.mate[paren], Some(feature))
    }

    pub(crate) fn feature_modules(&self, feature: &str) -> Vec<String> {
        let mut modules = Vec::new();
        for i in 0..self.toks.len() {
            if self.kind(i) != T::Pound || self.kind(i + 1) != T::OpenBracket || !self.module_scope(i)
                || self.in_macro(i) || !self.feature_cfg(i + 1, feature) { continue; }
            let keyword = self.item_head(self.after_attrs(i));
            if self.kind(keyword) == T::KwMod { modules.push(self.item_path(keyword)); }
        }
        modules
    }

    pub(crate) fn item_requires_feature(&self, item: &Item, feature: &str) -> bool {
        let mut i = self.toks.partition_point(|t| (t.lo as usize) < item.span.start);
        while i < item.keyword {
            if self.outer_doc(i) { i += 1; continue; }
            let Some(open) = self.attr(i) else { break; };
            if self.feature_cfg(open, feature) { return true; }
            i = self.after(open);
        }
        false
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
        if cfg == "cfg" && self.requires_cfg(first, close, None) {
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

    pub(crate) fn visibility(&self, i: usize) -> usize {
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

    fn module_scope(&self, i: usize) -> bool {
        let p = self.parent[i];
        p == ROOT || p >= 2 && self.kind(p) == T::OpenBrace && self.kind(p - 2) == T::KwMod
    }

    fn semi(&self, mut i: usize, end: usize) -> Proof<usize> {
        while i < end {
            if self.kind(i) == T::Semi { return Ok(i + 1); }
            i = self.after(i);
        }
        Err("cannot prove the terminating semicolon")
    }

    fn item_head(&self, i: usize) -> usize {
        let mut head = self.visibility(i);
        while matches!(self.kind(head), T::KwUnsafe | T::KwAsync) { head += 1; }
        if self.kind(head) == T::KwConst && matches!(self.kind(head + 1), T::KwFn | T::KwUnsafe) {
            head += 1;
            if self.kind(head) == T::KwUnsafe { head += 1; }
        }
        if self.kind(head) == T::KwExtern {
            let next = head + 1 + usize::from(self.kind(head + 1) == T::Str);
            if self.kind(next) == T::KwFn { return next; }
        }
        head
    }

    fn item(&self, i: usize, limit: usize) -> Proof<Option<usize>> {
        let head = self.item_head(i);
        match self.kind(head) {
            T::KwExtern if self.kind(head + 1) == T::KwCrate => self.semi(head, limit).map(Some),
            T::KwConst if !name(self.kind(head + 1)) => Err("unrecognised const statement"),
            T::KwUse | T::KwStatic | T::KwConst | T::KwType => self.semi(head, limit).map(Some),
            k if matches!(k, T::KwFn | T::KwMod | T::KwImpl | T::KwEnum | T::KwStruct | T::KwTrait | T::KwExtern)
                || self.word(head) == "union" && name(self.kind(head + 1)) => {
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
                else if self.kind(group) == T::OpenBrace && (self.module_scope(i) || self.after_block(end, limit)) { Ok(Some(end)) }
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


    pub(crate) fn enclosure(&self, i: usize) -> Range<usize> {
        if self.parent[i] == ROOT { 0..self.toks.len() } else { self.parent[i]..self.limit(i) }
    }

    pub(crate) fn std_member(&self, i: usize) -> bool {
        if i >= 2 && self.kind(i - 1) == T::PathSep && self.word(i - 2) == "std" { return true; }
        let p = self.parent[i];
        p != ROOT && p >= 2 && self.kind(p) == T::OpenBrace && self.kind(p - 1) == T::PathSep && self.word(p - 2) == "std"
    }

    pub(crate) fn thread_import(&self, i: usize) -> bool {
        let p = self.parent[i];
        p != ROOT && p >= 2 && self.kind(p) == T::OpenBrace && self.kind(p - 1) == T::PathSep && self.word(p - 2) == "thread"
    }

    pub(crate) fn scope(&self, i: usize) -> String {
        let mut names = Vec::new();
        let mut parent = self.parent[i];
        while parent != ROOT {
            if self.kind(parent) == T::OpenBrace && parent >= 2 && self.kind(parent - 2) == T::KwMod {
                names.push(self.word(parent - 1));
            }
            parent = self.parent[parent];
        }
        names.reverse();
        names.join("::")
    }

    fn derive_import(&self, i: usize) -> bool {
        let valid_name = |j| matches!(self.word(j), "Serialize" | "Deserialize")
            && self.kind(j + 1) != T::PathSep;
        let mut j = i;
        let is_use = loop {
            if j == 0 { break false; }
            if self.kind(j - 1) == T::KwUse { break true; }
            if self.kind(j - 1) == T::PathSep {
                j -= 1;
                if j > 0 && self.path_name(j - 1) { j -= 1; }
                continue;
            }
            let group = self.parent[j];
            if group == ROOT || self.kind(group) != T::OpenBrace { break false; }
            // The prefix of a braced use belongs to its parent group.
            if group == 0 || !matches!(self.kind(group - 1), T::KwUse | T::PathSep | T::Comma | T::OpenBrace) { break false; }
            j = group;
        };
        if is_use {
            let next = i + 2;
            if valid_name(next) { return true; }
            if self.kind(next) != T::OpenBrace { return false; }
            let mut j = next + 1;
            while j < self.mate[next] {
                if !valid_name(j) { return false; }
                j += 1;
                if self.kind(j) == T::KwAs && name(self.kind(j + 1)) { j += 2; }
                if self.kind(j) == T::Comma { j += 1; }
                else if j != self.mate[next] { return false; }
            }
            return true;
        }
        let p = self.parent[i];
        p != ROOT && p > 0 && self.kind(p) == T::OpenParen && self.word(p - 1) == "derive"
            && self.in_attribute(i) && valid_name(i + 2)
    }

    fn denied(&self, i: usize) -> Option<&'static str> {
        match self.kind(i) {
            T::Str | T::RawStr if self.text(i).trim_start_matches('r').trim_matches('#') == "\"CARGO_MANIFEST_DIR\"" => Some("CARGO_MANIFEST_DIR"),
            T::Ident | T::RawIdent => match self.word(i) {
                "serde_json" => Some("serde_json"),
                "flate2" => Some("flate2"),
                "CARGO_MANIFEST_DIR" => Some("CARGO_MANIFEST_DIR"),
                "include_str" if self.kind(i + 1) == T::Not => Some("include_str!"),
                "include_bytes" if self.kind(i + 1) == T::Not => Some("include_bytes!"),
                "fs" if self.std_member(i) && !(i >= 2 && self.word(i - 2) == "std" && self.kind(i - 1) == T::PathSep) => Some("std::fs"),
                "std" if self.kind(i + 1) == T::PathSep && self.word(i + 2) == "fs" => Some("std::fs"),
                "fs" if self.kind(i + 1) == T::PathSep && self.word(i + 2).starts_with("read")
                    && !(i >= 2 && self.word(i - 2) == "std" && self.kind(i - 1) == T::PathSep) => Some("std::fs"),
                "serde" if self.kind(i + 1) == T::PathSep && !self.derive_import(i) => Some("serde::"),
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn scan(&self) -> Result<Scan, String> {
        let mut scan = Scan::default();
        let mut lists = HashMap::new();
        let mut generics = None;
        let mut visited = None;
        for i in 0..self.toks.len() {
            if let Some(kind) = self.denied(i) {
                scan.denied.push((self.toks[i].lo as usize, kind));
            }
            let Some(open) = self.attr(i) else { continue; };
            let attr = self.word(open + 1);
            let cfg = self.test_cfg(open).map_err(|e| self.error(i, e))?;
            if !cfg && !matches!(attr, "ignore" | "test") { continue; }
            if open != i + 1 { return Err(self.error(i, "inner test attribute")); }
            let start = self.attr_start(i);
            if visited == Some(start) { continue; }
            visited = Some(start);
            let head = self.after_attrs(start);
            let mut cfg = false;
            let mut test = false;
            let mut ignored = false;
            for at in start..head {
                let Some(a) = self.attr(at) else { continue; };
                cfg |= self.test_cfg(a).map_err(|e| self.error(at, e))?;
                test |= self.word(a + 1) == "test" && self.mate[a] == a + 2;
                ignored |= self.word(a + 1) == "ignore";
            }
            let keyword = self.item_head(head);
            let parent = self.parent[start];
            let is_item = self.module_scope(start) && self.item_boundary(start);
            let extent = if self.in_macro(i) {
                if !cfg { continue; }
                // A macro's grammar is opaque. Use a syntactic item if it has a
                // provable end; otherwise keep the containing argument group as evidence.
                let end = self.item(head, self.limit(i)).ok().flatten()
                    .map_or(self.limit(i), |end| end);
                self.end(end.max(head))
            } else {
                self.extent(start, head, generics.get_or_insert_with(|| self.generic_starts()), &mut lists)
                    .map_err(|e| self.error(i, e))?
            };
            let span = self.toks[start].lo as usize..extent;
            if self.in_macro(i) || !is_item {
                if ignored || test { return Err(self.error(i, "test/ignore outside a proven function item")); }
                let statement_level = !self.in_macro(i) && self.context(parent) != Context::Items;
                scan.contexts.push((span, i, statement_level));
                continue;
            }
            if (test || ignored) && (self.kind(keyword) != T::KwFn || !test) {
                return Err(self.error(i, "ignore/test requires a #[test] function"));
            }
            scan.items.push(Item { span, hits: 0..0, keyword, path: self.item_path(keyword), cfg, test, ignored,
                module: self.kind(keyword) == T::KwMod, line: self.line(i) });
        }
        for (range, i, _) in &scan.contexts {
            if let Some((_, kind)) = scan.hits(range).first() {
                return Err(self.error(*i, &format!("denied {kind} in a non-item test context")));
            }
        }
        Ok(scan)
    }

    pub(crate) fn line(&self, i: usize) -> usize {
        self.lines().partition_point(|&start| start <= self.toks[i].lo)
    }


    fn item_path(&self, keyword: usize) -> String {
        let mut local = keyword + 1;
        if self.kind(keyword) == T::KwUse {
            while self.kind(local + 1) == T::PathSep && self.path_name(local + 2) { local += 2; }
        } else if self.word(keyword) == "macro_rules" || self.kind(keyword) == T::KwStatic && self.kind(local) == T::KwMut {
            local += 1;
        } else if let Some(end) = self.path_end(keyword).filter(|&end| self.kind(end) == T::Not) {
            local = end - 1;
        }
        qualify(&self.scope(keyword), self.word(local))
    }

    // Walk just the direct items in each test region. Delimiter links skip
    // non-module bodies in one step. Inline child modules continue the same
    // walk, and the module graph supplies roots for out-of-line children.
    pub(crate) fn index_test_items(&self, scan: &mut Scan, test_file: bool) -> Result<(), String> {
        let mut regions = Vec::new();
        if test_file {
            regions.push(0..self.toks.len() - 1);
        } else {
            for item in scan.items.iter().filter(|item| item.cfg && item.module) {
                let body = item.keyword + 2;
                if self.kind(body) == T::OpenBrace && regions.last().is_none_or(|r| r.end < body) {
                    regions.push(body + 1..self.mate[body]);
                }
            }
        }
        let mut annotated = std::mem::take(&mut scan.items).into_iter().peekable();
        for region in regions {
            let mut i = region.start;
            while i < region.end {
                if matches!(self.kind(i), T::CloseBrace | T::Semi)
                    || self.kind(i) == T::DocComment && !self.outer_doc(i) {
                    i += 1;
                    continue;
                }
                if self.kind(i) == T::Pound && self.kind(i + 1) == T::Not {
                    i = self.after(i + 2);
                    continue;
                }
                let head = self.after_attrs(i);
                let keyword = self.item_head(head);
                let end = self.item(head, self.limit(i)).map_err(|e| self.error(i, e))?
                    .ok_or_else(|| self.error(i, "unrecognised direct item in a test module"))?;
                let module = self.kind(keyword) == T::KwMod;
                while annotated.peek().is_some_and(|a| a.keyword < keyword) {
                    scan.items.push(annotated.next().unwrap());
                }
                if annotated.peek().is_some_and(|a| a.keyword == keyword) {
                    scan.items.push(annotated.next().unwrap());
                } else {
                    scan.items.push(Item { span: self.toks[i].lo as usize..self.end(end), hits: 0..0,
                        keyword, path: self.item_path(keyword), cfg: false, test: false,
                        ignored: false, module, line: self.line(keyword) });
                }
                i = if module && self.kind(keyword + 2) == T::OpenBrace { keyword + 3 } else { end };
            }
        }
        scan.items.extend(annotated);
        // Both streams are in source order. A module delegates its body
        // to its own items, so a hit is never charged up a module chain.
        let mut first = 0;
        for item in scan.items.iter_mut().filter(|item| !item.module) {
            while scan.denied.get(first).is_some_and(|(p, _)| *p < item.span.start) { first += 1; }
            let mut end = first;
            while scan.denied.get(end).is_some_and(|(p, _)| *p < item.span.end) { end += 1; }
            item.hits = first..end;
        }
        Ok(())
    }

    pub(crate) fn helper_reason(&self, item: &Item, hits: &[(usize, &'static str)]) -> String {
        let head = item.keyword;
        let label = if self.kind(head) == T::KwUse {
            self.src[self.toks[head].lo as usize..item.span.end].trim_end_matches(';')
                .split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            let name = item.path.rsplit("::").next().unwrap();
            format!("{} {name}", self.word(head))
        };
        let mut sites = Vec::new();
        for &(pos, kind) in hits {
            let line = self.lines().partition_point(|&start| start as usize <= pos);
            let site = format!("{kind} at line {line}");
            if sites.last() != Some(&site) { sites.push(site); }
        }
        format!("{label} (line {}): {}", self.line(head), sites.join(", "))
    }

    pub(crate) fn reference(&self, i: usize, file_module: &str) -> Vec<String> {
        // Collect the path prefix, including prefixes outside braced use groups.
        let mut parts = vec![self.word(i)];
        let mut j = i;
        loop {
            if j >= 2 && self.kind(j - 1) == T::PathSep && self.path_name(j - 2) {
                j -= 2;
                parts.push(self.word(j));
            } else {
                let group = self.parent[j];
                if group == ROOT || group < 2 || self.kind(group) != T::OpenBrace ||
                    self.kind(group - 1) != T::PathSep || !self.path_name(group - 2) { break; }
                j = group - 2;
                parts.push(self.word(j));
            }
        }
        if j > 0 && matches!(self.kind(j - 1), T::KwMod | T::KwFn | T::Dot) { return Vec::new(); }
        parts.reverse();
        let scope = qualify(file_module, &self.scope(i));
        let mut resolved: Vec<_> = scope.split("::").filter(|p| !p.is_empty()).collect();
        let anchored = matches!(parts[0], "crate" | "self" | "super");
        for part in &parts {
            match *part {
                "crate" => resolved.clear(),
                "self" => {}
                "super" => { resolved.pop(); }
                word => resolved.push(word),
            }
        }
        let mut paths = vec![resolved.join("::")];
        if !anchored && parts.len() > 1 { paths.push(parts.join("::")); }
        paths
    }

    // A removed use item can bind several names. Record its local names
    // (including renames and grouped self), and the source of a glob so
    // that a kept glob can pass a removed name on to its own callers.
    pub(crate) fn use_names(&self, item: &Item, module: &str) -> (Vec<String>, Vec<String>) {
        let mut names = Vec::new();
        let mut globs = Vec::new();
        let mut i = item.keyword + 1;
        while self.kind(i) != T::Semi {
            if self.kind(i) == T::Star {
                for path in self.reference(i, module) {
                    globs.push(path.strip_suffix("::*").unwrap_or("").to_owned());
                }
            } else if self.path_name(i) && self.kind(i + 1) != T::PathSep {
                if self.kind(i + 1) == T::KwAs { i += 2; }
                if self.kind(i) == T::KwSelfValue {
                    if let Some(path) = self.reference(i, module).first() {
                        if let Some(name) = path.rsplit("::").next() { names.push(name.to_owned()); }
                    }
                } else if self.word(i) != "_" {
                    names.push(self.word(i).to_owned());
                }
            }
            i += 1;
        }
        (names, globs)
    }
}

pub(crate) fn qualify(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() { suffix.into() } else if suffix.is_empty() { prefix.into() } else { format!("{prefix}::{suffix}") }
}

pub(crate) struct Item {
    pub span: Range<usize>,
    pub hits: Range<usize>,
    pub keyword: usize,
    pub path: String,
    pub cfg: bool,
    pub test: bool,
    pub ignored: bool,
    pub module: bool,
    pub line: usize,
}

#[derive(Default)]
pub(crate) struct Scan {
    pub items: Vec<Item>,
    pub contexts: Vec<(Range<usize>, usize, bool)>,
    pub denied: Vec<(usize, &'static str)>,
}

impl Scan {
    pub fn hits(&self, span: &Range<usize>) -> &[(usize, &'static str)] {
        &self.denied[self.denied.partition_point(|&(p, _)| p < span.start)..self.denied.partition_point(|&(p, _)| p < span.end)]
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::modules::tests::sample_rules;
    use crate::rules::{parse_skips, prepare};
    use crate::table::{NamedRemoval, Table, Tree};

    fn sample(lib: &str) -> (Table, Tree) {
        let mut table = sample_rules();
        table.drops.clear();
        table.crates[0].keep.clear();
        let tree = Tree::from([
            ("sample/Cargo.toml".into(), b"[package]\nedition = \"2021\"".to_vec()),
            ("sample/src/lib.rs".into(), lib.as_bytes().to_vec()),
        ]);
        (table, tree)
    }

    #[test]
    fn strip_removes_the_innermost_test_item_that_holds_a_denied_token() {
        let body = r#"mod clean { use serde::{Serialize, Deserialize}; pub fn helper() {} }
    #[test] fn first() { clean::helper(); }
    /// Attached to the fixture test.
    #[test] fn fixture() { include_str!("data"); }
    #[test] fn last() { #[cfg(test)] return; }
    mod nested { #[test] fn keep() {} #[test] fn drop_me() { serde_json::Value; } }"#;
        let lib = format!("#[cfg(test)] mod inline {{ {body} }}\n#[cfg(test)] mod outlined;\n");
        let free = "#[cfg(test)] fn needs_file() { fs::read_to_string(\"x\"); }\n#[cfg(test)] use serde_json::Value;";
        let (table, mut tree) = sample(&(lib.clone() + free));
        tree.insert("sample/src/outlined.rs".into(), body.as_bytes().to_vec());
        tree.insert("sample/src/data".into(), b"fixture".to_vec());
        let mut changes = prepare(&tree, &table, &[]).unwrap();
        let summary = &changes.summaries["sample"];
        assert_eq!((summary.kept_tests, summary.removed_tests), (6, 4));
        assert_eq!(summary.kinds["include_str!"], 2);
        assert_eq!(summary.kinds["serde_json"], 2);
        let expected = body.replace("/// Attached to the fixture test.\n    #[test] fn fixture() { include_str!(\"data\"); }", "")
            .replace("#[test] fn drop_me() { serde_json::Value; }", "");
        changes.apply_to(&mut tree, &Default::default()).unwrap();
        assert_eq!(tree["sample/src/outlined.rs"], expected.as_bytes());
        assert_eq!(tree["sample/src/lib.rs"], (lib.replace(body, &expected) + "\n").as_bytes());
    }

    #[test]
    fn strip_removes_a_module_whole_only_with_its_skip_entry() {
        let (mut table, mut tree) = sample("#[cfg(test)] mod outer { mod imports {\nuse serde_json::Value;\n#[test] fn ok() {} } mod helpers; }");
        tree.insert("sample/src/outer/helpers.rs".into(),
            b"fn load() { fs::read_to_string(\"x\"); }\nmod nested { const X: bool = serde_json::FLAG; }
macro_rules! value { () => { serde_json::Value } }\nmake_table!(flate2::Read);".to_vec());
        let error = prepare(&tree, &table, &[]).err().unwrap();
        assert!(error.contains("sample outer::imports module use serde_json::Value (line 2): serde_json"), "{error}");
        assert!(error.contains("sample outer::helpers module fn load (line 1): std::fs"), "{error}");
        assert!(error.contains("macro_rules value (line 3): serde_json") && error.contains("flate2 at line 4"), "{error}");
        assert!(!error.contains("outer::helpers::nested module"));
        let skips = parse_skips("sample outer::imports module use Value at line 2\nsample outer::helpers module load at line 1\nsample outer::imports::ok slow time 3 s").unwrap();
        let mut result = prepare(&tree, &table, &skips).unwrap();
        assert_eq!(result.summaries["sample"].whole_modules.len(), 2);
        table.removals.push(NamedRemoval { item: "outer".into(), uses: "outer::imports".into(),
            reason: "imports".into(), upstream: "sample".into(), expected: 1 });
        let error = prepare(&tree, &table, &skips).err().unwrap();
        assert!(error.contains("whole modules require a matching module entry"), "{error}");
        table.removals.clear();
        tree.insert("sample/src/outer/helpers.rs".into(), b"fn load() {}".to_vec());
        let error = prepare(&tree, &table, &skips).err().unwrap();
        assert!(error.contains("stale module outer::helpers"), "{error}");
        result.apply_to(&mut tree, &Default::default()).unwrap();
        assert_eq!(tree["sample/src/lib.rs"], b"#[cfg(test)] mod outer {   }");
        assert!(!tree.contains_key("sample/src/outer/helpers.rs"));
    }

    #[test]
    fn orphans_go_only_when_unused_test_code() {
        use crate::table::Orphan;
        // The macro defines the variant in m while keeping its declaration token
        // outside m. The kept path is the only reference under test.
        let lib = r#"macro_rules! define_kind { () => { enum Kind { helper } }; }
#[cfg(test)]
mod m {
    define_kind!();
    /// Docs travel with the helper.
    fn helper() {}
    const C: bool = true;
    use a::{b, c};
    mod empty { use super::*; }
    mod child;
    #[test] fn gone() { serde_json::read(); helper(); }
    fn keep() { c(); let _ = Kind::helper; log!("helper C b"); }
}
mod sibling { fn helper() {} }
#[cfg(test)]
use x::Y;
const PRIVATE: usize = 0;
"#;
        let (mut table, mut tree) = sample(lib);
        tree.insert("sample/src/m/child.rs".into(), b"// helper".to_vec());
        let entry = |item: &str, name: Option<&str>, production| Orphan {
            upstream: "sample".into(), item: item.into(), name: name.map(String::from), production,
        };
        table.orphans = vec![entry("m::helper", None, false), entry("m::C", None, false),
            entry("m::a", Some("b"), false), entry("m::empty::super", Some("*"), false),
            entry("Y", Some("Y"), false), entry("PRIVATE", None, true)];
        let mut prepared = prepare(&tree, &table, &[]).unwrap();
        let mut kept = tree.clone();
        prepared.apply_to(&mut kept, &Default::default()).unwrap();
        let expected = lib.replace("/// Docs travel with the helper.\n    fn helper() {}", "")
            .replace("const C: bool = true;", "").replace("use a::{b, c};", "use a::{c};")
            .replace("use super::*;", "").replace("#[cfg(test)]\nuse x::Y;", "")
            .replace("const PRIVATE: usize = 0;", "")
            .replace("#[test] fn gone() { serde_json::read(); helper(); }", "");
        assert_eq!(std::str::from_utf8(&kept["sample/src/lib.rs"]).unwrap(), expected);
        let o1 = prepared.outcomes.iter().find(|o| o.rule == "O1").unwrap();
        assert_eq!(o1.count, 6);
        assert_eq!(o1.files, ["sample/src/lib.rs"]);
        for (from, to, item, why) in [
            ("c();", "helper();", "m::helper", "stale"),
            ("c();", "self::helper();", "m::helper", "stale"),
            ("c();", "crate::m::helper();", "m::helper", "stale"),
            ("c();", "child::helper();", "m::helper", "stale"),
            ("mod sibling {", "mod sibling { mod Kind {}", "m::helper", "stale"),
            ("mod child;", "mod child { fn f() { log!(helper); } }", "m::helper", "stale"),
            ("use super::*;", "use super::*; fn kept() {}", "m::empty::super", "stale"),
            ("#[cfg(test)]\nuse x", "use x", "Y", "production"),
            ("const PRIVATE", "pub(crate) const PRIVATE", "PRIVATE", "private"),
            ("const PRIVATE", "const OTHER", "PRIVATE", "found 0"),
            ("b, c", "b, b, c", "m::a", "found 2"),
        ] {
            tree.insert("sample/src/lib.rs".into(), lib.replace(from, to).into_bytes());
            let error = prepare(&tree, &table, &[]).err().unwrap();
            assert!(error.contains(item) && error.contains(why), "{error}");
        }
        tree.insert("sample/src/lib.rs".into(), lib.as_bytes().to_vec());
        tree.insert("sample/src/m/child.rs".into(), b"fn f() { super::helper(); }".to_vec());
        let error = prepare(&tree, &table, &[]).err().unwrap();
        assert!(error.contains("m::helper") && error.contains("stale") && error.contains("child.rs:1"), "{error}");
    }

    #[test]
    fn named_removals_fail_when_stale() {
        let (mut table, tree) = sample("#[cfg(test)] mod m { #[test] fn denied() { flate2::read(); }
            #[test] fn local() { denied(); } pub fn kept() {} }
            #[cfg(test)] fn remote() { crate::m::denied(); }");
        let error = prepare(&tree, &table, &[]).err().unwrap();
        assert!(error.contains("m::local -> m::denied") && error.contains("remote -> m::denied"), "{error}");
        for item in ["m::local", "remote"] {
            table.removals.push(NamedRemoval { item: item.into(), uses: "m::denied".into(),
                reason: "calls denied".into(), upstream: "sample".into(), expected: 1 });
        }
        let result = prepare(&tree, &table, &[]).unwrap();
        assert_eq!(result.summaries["sample"].named_removals.len(), 2);
        table.removals[0].uses = "m::kept".into();
        let error = prepare(&tree, &table, &[]).err().unwrap();
        assert!(error.contains("stale named removal m::local: uses m::kept"), "{error}");

        let (mut table, tree) = sample("#[cfg(test)] mod m { #[test] fn denied() { flate2::read(); } }
            #[cfg(test)] mod callers { use super::m::{denied as imported};
                mod nested { use super::*; #[test] fn from_glob() { imported(); } } }");
        let error = prepare(&tree, &table, &[]).err().unwrap();
        assert!(error.contains("callers::m -> m::denied"), "{error}");
        table.removals.push(NamedRemoval { item: "callers::m".into(), uses: "m::denied".into(),
            reason: "imports denied".into(), upstream: "sample".into(), expected: 1 });
        let error = prepare(&tree, &table, &[]).err().unwrap();
        assert!(error.contains("callers::nested::from_glob -> callers::m"), "{error}");
    }
    #[test]
    fn strip_refuses_a_denied_token_outside_an_item() {
        for (input, line) in [
            ("fn f(n: N) { match n {\n#[cfg(test)] N::A => fs::read(\"x\"), _ => todo!() } }", 2),
            ("m!(\n#[cfg(test)] fn f() { serde_json::Value; });", 2),
        ] {
            let s = Source::parse("test.rs", input.as_bytes(), 2021).unwrap();
            let e = s.scan().err().unwrap();
            assert!(e.contains(&format!("test.rs:{line}: denied")), "{e}");
        }
        let s = Source::parse("test.rs", b"m!(#[cfg(test)] thing);", 2021).unwrap();
        assert_eq!(s.scan().unwrap().contexts.len(), 1);
    }
}
