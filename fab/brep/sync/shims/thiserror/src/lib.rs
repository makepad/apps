//! Error formatting for the forms used in the synced kernel.
use makepad_micro_proc_macro::{
    error, error_span,
    proc_macro::{Delimiter, Ident, Span, TokenStream, TokenTree},
    TokenBuilder, TokenParser,
};

mod format;
use format::Format;

#[proc_macro_derive(Error, attributes(error, from, source, backtrace))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    expand(input).unwrap_or_else(|diagnostic| diagnostic)
}

struct Case {
    pattern: TokenStream,
    bindings: Vec<Ident>,
    format: Format,
}

fn expand(input: TokenStream) -> Result<TokenStream, TokenStream> {
    let mut p = TokenParser::new(input);
    let outer = attributes(&mut p)?;
    visibility(&mut p);
    let is_enum = p.eat_ident("enum");
    if !is_enum && !p.eat_ident("struct") {
        return Err(error("Error requires a struct or enum"));
    }
    let name = identifier(&mut p)?;
    if p.is_punct_any('<') || p.eat_ident("where") {
        return Err(error("generic errors and where clauses are unsupported"));
    }
    let mut cases = Vec::new();
    if is_enum {
        if outer.is_some() || !p.open_brace() {
            return Err(error("Error requires an #[error(...)] on each enum variant"));
        }
        while !p.eat_eot() {
            let format = attributes(&mut p)?;
            let variant = identifier(&mut p)?;
            cases.push(case(&mut p, Some(variant), format)?);
            if !p.is_eot() && !p.eat_punct_any(',') {
                return Err(error("unsupported error variant discriminant or suffix"));
            }
        }
    } else {
        cases.push(case(&mut p, None, outer)?);
        p.eat_punct_any(';');
    }
    if p.current.is_some() {
        return Err(error("unsupported error type suffix or where clause"));
    }

    let mut out = TokenBuilder::new();
    let formatter = Ident::new("__solidsync_formatter", Span::mixed_site());
    out.add("impl ::core::fmt::Display for").extend(name.clone().into())
        .add("{ #[allow(unused_variables)] fn fmt(&self,").extend(formatter.clone().into())
        .add(": &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result { match self {");
    for case in &cases {
        out.stream(Some(case.pattern.clone())).add("=>");
        match &case.format {
            Format::Transparent => {
                out.add("::core::fmt::Display::fmt(").extend(case.bindings[0].clone().into())
                    .add(",").extend(formatter.clone().into()).add(")");
            }
            Format::Message(literal, args) => {
                out.add("::core::write!(").extend(formatter.clone().into()).add(",")
                    .extend(literal.clone().into())
                    .stream(Some(format::arguments(args.clone(), &case.bindings)?)).add(")");
            }
        }
        out.add(",");
    }
    out.add("} } } impl ::std::error::Error for").extend(name.into()).add("{");
    if cases.iter().any(|case| matches!(case.format, Format::Transparent)) {
        out.add("fn source(&self) -> ::core::option::Option<&(dyn ::std::error::Error + 'static)> { match self {");
        for case in &cases {
            if matches!(case.format, Format::Transparent) {
                out.stream(Some(case.pattern.clone())).add("=> ::std::error::Error::source(")
                    .extend(case.bindings[0].clone().into()).add("),");
            }
        }
        if cases.iter().any(|case| matches!(case.format, Format::Message(..))) {
            out.add("_ => ::core::option::Option::None,");
        }
        out.add("} }");
    }
    out.add("}");
    Ok(out.end())
}

fn identifier(p: &mut TokenParser) -> Result<Ident, TokenStream> {
    match p.current.clone() {
        Some(TokenTree::Ident(id)) => {
            p.advance();
            Ok(id)
        }
        _ => Err(error("expected an error type, variant, or field name")),
    }
}

fn visibility(p: &mut TokenParser) {
    if p.eat_ident("pub") && p.is_paren() {
        p.advance();
    }
}

fn attributes(p: &mut TokenParser) -> Result<Option<Format>, TokenStream> {
    let mut format = None;
    while p.eat_punct_any('#') {
        let Some(TokenTree::Group(group)) = p.current.take() else {
            return Err(error("expected an attribute"));
        };
        p.advance();
        let mut attr = group.stream().into_iter();
        let Some(TokenTree::Ident(name)) = attr.next() else { continue };
        match name.to_string().as_str() {
            "error" => match (attr.next(), attr.next()) {
                (Some(TokenTree::Group(args)), None)
                    if args.delimiter() == Delimiter::Parenthesis && format.is_none() => {
                    format = Some(Format::parse(args)?);
                }
                _ => return Err(error_span("expected a single #[error(...)]", name.span())),
            },
            "from" | "source" | "backtrace" => {
                return Err(error_span(&format!("unsupported #[{name}] error attribute"), name.span()));
            }
            _ => {}
        }
    }
    Ok(format)
}

fn case(p: &mut TokenParser, variant: Option<Ident>, format: Option<Format>) -> Result<Case, TokenStream> {
    let format = format.ok_or_else(|| error("missing #[error(...)] on error type or variant"))?;
    let mut pattern = TokenBuilder::new();
    pattern.add("Self");
    if let Some(variant) = variant {
        pattern.add("::").extend(variant.into());
    }
    let mut bindings = Vec::new();
    if p.is_brace() || p.is_paren() {
        let delimiter = p.open_group().unwrap();
        pattern.push_group(delimiter);
        while !p.eat_eot() {
            if attributes(p)?.is_some() {
                return Err(error("unsupported #[error] on a field"));
            }
            visibility(p);
            let binding = if delimiter == Delimiter::Brace {
                let field = identifier(p)?;
                if matches!(field.to_string().as_str(), "source" | "r#source") {
                    return Err(error_span("unsupported error field named source", field.span()));
                }
                p.expect_punct_any(':')?;
                field
            } else {
                Ident::new(&format!("_{}", bindings.len()), Span::call_site())
            };
            pattern.extend(binding.clone().into()).add(",");
            bindings.push(binding);
            // Only field boundaries matter. A comma inside a type argument is not a boundary.
            let mut depth = 0usize;
            while !p.is_eot() && (depth != 0 || !p.is_punct_any(',')) {
                if p.is_punct_any('<') {
                    depth += 1;
                } else if p.is_punct_any('>') {
                    depth = depth.saturating_sub(1);
                } else if p.is_punct_joint('-') {
                    p.advance();
                    if !p.is_punct_any('>') { continue; }
                }
                p.advance();
            }
            p.eat_punct_any(',');
        }
        pattern.pop_group(delimiter);
    }
    if matches!(format, Format::Transparent) && bindings.len() != 1 {
        return Err(error("#[error(transparent)] requires exactly one field"));
    }
    Ok(Case { pattern: pattern.end(), bindings, format })
}
