//! Derive-only serde vocabulary for the synced kernel's in-memory records.
use makepad_micro_proc_macro::{
    error, error_span,
    proc_macro::{Delimiter, Group, Ident, Spacing, TokenStream, TokenTree},
    TokenBuilder,
};

#[proc_macro_derive(Serialize, attributes(serde))]
pub fn serialize(input: TokenStream) -> TokenStream {
    expand(input).unwrap_or_else(|diagnostic| diagnostic)
}

#[proc_macro_derive(Deserialize, attributes(serde))]
pub fn deserialize(input: TokenStream) -> TokenStream {
    expand(input).unwrap_or_else(|diagnostic| diagnostic)
}

fn expand(input: TokenStream) -> Result<TokenStream, TokenStream> {
    // Anonymous consts are emitted beside the item, where its name stands for Self.
    let mut tokens = input.clone().into_iter();
    tokens.find(|t| matches!(t, TokenTree::Ident(id)
        if matches!(id.to_string().as_str(), "struct" | "enum" | "union")));
    let Some(TokenTree::Ident(item)) = tokens.next() else {
        return Err(error("expected a serde derive type name"));
    };
    let mut uses = TokenBuilder::new();
    check_attributes(input, Delimiter::None, &item, &mut uses)?;
    Ok(uses.end())
}

fn check_attributes(
    input: TokenStream,
    level: Delimiter,
    item: &Ident,
    uses: &mut TokenBuilder,
) -> Result<(), TokenStream> {
    let tokens: Vec<_> = input.into_iter().collect();
    let mut rest = tokens.as_slice();
    while let Some((token, tail)) = rest.split_first() {
        rest = tail;
        match token {
            TokenTree::Punct(p) if p.as_char() == '#' => {
                if let Some((TokenTree::Group(g), tail)) = rest.split_first() {
                    if g.delimiter() == Delimiter::Bracket {
                        rest = tail;
                        check_attribute(g, level, rest, item, uses)?;
                    }
                }
            }
            TokenTree::Group(g) => check_attributes(g.stream(), g.delimiter(), item, uses)?,
            _ => {}
        }
    }
    Ok(())
}

fn check_attribute(
    attribute: &Group,
    level: Delimiter,
    rest: &[TokenTree],
    item: &Ident,
    uses: &mut TokenBuilder,
) -> Result<(), TokenStream> {
    let mut tokens = attribute.stream().into_iter();
    if !matches!(tokens.next(), Some(TokenTree::Ident(name)) if name.to_string() == "serde") {
        return Ok(());
    }
    let args = match (tokens.next(), tokens.next()) {
        (Some(TokenTree::Group(g)), None) if g.delimiter() == Delimiter::Parenthesis => g,
        _ => return Err(error_span("expected #[serde(...)]", attribute.span())),
    };
    let mut args = args.stream().into_iter().peekable();
    loop {
        let name = match args.next() {
            Some(TokenTree::Ident(name)) => name,
            _ => return Err(error_span("expected a serde helper name", attribute.span())),
        };
        let key = name.to_string();
        let diagnostic = |message: &str| error_span(&format!("serde helper `{key}` {message}"), name.span());
        match key.as_str() {
            "deny_unknown_fields" | "default" => {}
            "rename_all" | "tag" | "content" | "skip_serializing_if" | "rename" | "from" | "into" => {
                if !matches!(args.next(), Some(TokenTree::Punct(p)) if p.as_char() == '=') {
                    return Err(diagnostic("requires a string literal value"));
                }
                let Some(TokenTree::Literal(literal)) = args.next() else {
                    return Err(diagnostic("requires a string literal value"));
                };
                let spelling = literal.to_string();
                let value = string_contents(&spelling)
                    .ok_or_else(|| diagnostic("requires a string literal value"))?;
                let rust_tokens = || value.parse::<TokenStream>()
                    .map_err(|_| diagnostic("requires Rust tokens in its value"));
                match key.as_str() {
                    "skip_serializing_if" => {
                        let ty = field_type(level, rest).ok_or_else(|| diagnostic("requires a field"))?;
                        uses.add("const _: fn(&").stream(Some(ty)).add(") -> bool =")
                            .stream(Some(rust_tokens()?)).add(";");
                    }
                    "from" => {
                        let ty = rust_tokens()?;
                        uses.add("const _: fn(").stream(Some(ty.clone())).add(") ->")
                            .extend(item.clone().into()).add("= <").extend(item.clone().into())
                            .add("as ::core::convert::From<").stream(Some(ty)).add(">>::from;");
                    }
                    "into" => {
                        let ty = rust_tokens()?;
                        uses.add("const _: fn(").extend(item.clone().into()).add(") ->")
                            .stream(Some(ty.clone())).add("= <").extend(item.clone().into())
                            .add("as ::core::convert::Into<").stream(Some(ty)).add(">>::into;");
                    }
                    _ => {}
                }
            }
            _ => return Err(diagnostic("is outside the solidsync inventory")),
        }
        match args.next() {
            None => return Ok(()),
            Some(TokenTree::Punct(p)) if p.as_char() == ',' => {
                if args.peek().is_none() {
                    return Ok(());
                }
            }
            _ => return Err(diagnostic("has an unsupported argument shape")),
        }
    }
}

fn string_contents(spelling: &str) -> Option<&str> {
    if let Some(quoted) = spelling.strip_prefix('"') {
        return quoted.strip_suffix('"');
    }
    let raw = spelling.strip_prefix('r')?;
    let hashes = raw.len() - raw.trim_start_matches('#').len();
    raw[hashes..].strip_prefix('"')?.strip_suffix(&raw[..hashes])?.strip_suffix('"')
}

fn field_type(level: Delimiter, mut rest: &[TokenTree]) -> Option<TokenStream> {
    while let [TokenTree::Punct(p), TokenTree::Group(g), tail @ ..] = rest {
        if p.as_char() != '#' || g.delimiter() != Delimiter::Bracket { break; }
        rest = tail;
    }
    if matches!(rest.first(), Some(TokenTree::Ident(id)) if id.to_string() == "pub") {
        rest = &rest[1..];
        if let Some((TokenTree::Group(g), tail)) = rest.split_first() {
            if g.delimiter() == Delimiter::Parenthesis
                && matches!(g.stream().into_iter().next(), Some(TokenTree::Ident(id))
                    if matches!(id.to_string().as_str(), "crate" | "in" | "self" | "super"))
            {
                rest = tail;
            }
        }
    }
    if level == Delimiter::Brace {
        let [TokenTree::Ident(_), TokenTree::Punct(colon), tail @ ..] = rest else { return None };
        if colon.as_char() != ':' { return None; }
        rest = tail;
    } else if level != Delimiter::Parenthesis {
        return None;
    }
    // Delimited groups are single tokens. Only angle brackets can hide a top-level
    // comma, and the > of a function type's -> does not close a type argument.
    let mut depth = 0usize;
    let mut arrow = false;
    let end = rest.iter().position(|t| {
        if let TokenTree::Punct(p) = t {
            match p.as_char() {
                ',' if depth == 0 => return true,
                '<' => depth += 1,
                '>' if !arrow => depth = depth.saturating_sub(1),
                _ => {}
            }
            arrow = p.as_char() == '-' && p.spacing() == Spacing::Joint;
        } else {
            arrow = false;
        }
        false
    }).unwrap_or(rest.len());
    (end != 0).then(|| rest[..end].iter().cloned().collect())
}
