use makepad_micro_proc_macro::{
    error_span,
    proc_macro::{Group, Ident, Literal, Spacing, TokenStream, TokenTree},
};

pub(super) enum Format {
    Transparent,
    Message(Literal, TokenStream),
}

impl Format {
    pub(super) fn parse(group: Group) -> Result<Self, TokenStream> {
        let mut tokens = group.stream().into_iter();
        match tokens.next() {
            Some(TokenTree::Ident(id)) if id.to_string() == "transparent" && tokens.next().is_none() => {
                Ok(Self::Transparent)
            }
            Some(TokenTree::Literal(literal)) => {
                let args: TokenStream = tokens.collect();
                if !args.is_empty()
                    && !matches!(args.clone().into_iter().next(), Some(TokenTree::Punct(p)) if p.as_char() == ',')
                {
                    return Err(error_span("expected a comma after the error format", literal.span()));
                }
                Ok(Self::Message(tuple_captures(literal)?, args))
            }
            _ => Err(error_span("expected an error format literal or transparent", group.span())),
        }
    }
}

// Keep the literal's spelling and span. Only insert underscores in numeric format slots.
fn tuple_captures(literal: Literal) -> Result<Literal, TokenStream> {
    let spelling = literal.to_string();
    let cooked = spelling.starts_with('"');
    if !cooked && !spelling.trim_start_matches('r').trim_start_matches('#').starts_with('"') {
        return Err(error_span("expected a string literal in #[error]", literal.span()));
    }
    let bytes = spelling.as_bytes();
    let mut output = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if cooked && bytes[i] == b'\\' {
            if bytes[i..].starts_with(b"\\u{") {
                i += bytes[i..].iter().position(|&b| b == b'}').unwrap_or(0) + 1;
            } else {
                i += 2;
            }
        } else if bytes[i..].starts_with(b"{{") {
            i += 2;
        } else if bytes[i] == b'{' {
            i += 1;
            let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && matches!(bytes.get(i + digits), Some(b'}' | b':')) {
                output.push_str(&spelling[copied..i]);
                output.push('_');
                copied = i;
            }
        } else {
            i += 1;
        }
    }
    if copied == 0 {
        return Ok(literal);
    }
    output.push_str(&spelling[copied..]);
    let mut result: Literal = output.parse()
        .map_err(|_| error_span("invalid tuple capture in error format", literal.span()))?;
    result.set_span(literal.span());
    Ok(result)
}

// Rewrite a leading field shorthand at operand boundaries, including inside call arguments.
// Postfix field/method access and the dots of range operators keep their original tokens.
pub(super) fn arguments(input: TokenStream, bindings: &[Ident]) -> Result<TokenStream, TokenStream> {
    let mut output = Vec::new();
    let mut tokens = input.into_iter().peekable();
    let mut operand = true;
    while let Some(token) = tokens.next() {
        match token {
            TokenTree::Group(g) => {
                let mut group = Group::new(g.delimiter(), arguments(g.stream(), bindings)?);
                group.set_span(g.span());
                output.push(group.into());
                operand = false;
            }
            TokenTree::Punct(p) if p.as_char() == '.' => {
                if p.spacing() == Spacing::Joint
                    && matches!(tokens.peek(), Some(TokenTree::Punct(next)) if next.as_char() == '.')
                {
                    output.push(p.into());
                    output.push(tokens.next().unwrap());
                    operand = true;
                } else if operand {
                    let name = match tokens.next() {
                        Some(TokenTree::Ident(id)) => id.to_string(),
                        Some(TokenTree::Literal(index)) => {
                            // Rust tokenizes .0.1 as a dot followed by one float literal.
                            let spelling = index.to_string();
                            if !spelling.bytes().all(|b| b.is_ascii_digit()) {
                                return Err(error_span(
                                    &format!("unsupported error shorthand `.{spelling}`: expected a single integer tuple index"),
                                    index.span(),
                                ));
                            }
                            format!("_{spelling}")
                        }
                        _ => return Err(error_span("expected .field or .index in error argument", p.span())),
                    };
                    let field = bindings.iter().find(|id| id.to_string() == name)
                        .ok_or_else(|| error_span(&format!("unknown shorthand field {name}"), p.span()))?;
                    output.push(field.clone().into());
                    operand = false;
                } else {
                    output.push(p.into());
                }
            }
            token => {
                operand = match &token {
                    TokenTree::Punct(p) => !matches!(p.as_char(), '?' | '\''),
                    TokenTree::Ident(id) => matches!(id.to_string().as_str(), "if" | "match" | "return"),
                    _ => false,
                };
                output.push(token);
            }
        }
    }
    Ok(output.into_iter().collect())
}
