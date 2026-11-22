//! Tool answers as Splash values: written by the Splash writer (the one way
//! Splash text is made), lengths with their unit (`12.5mm`), numbers rounded
//! to what is worth reading.

pub use makepad_script::syntax::write::{quote, Value as S};

pub fn obj(fields: Vec<(&str, S)>) -> S {
    S::object(fields)
}
pub fn s(text: impl Into<String>) -> S {
    S::Str(text.into())
}
pub fn name(n: &str) -> S {
    S::Name(n.to_string())
}
pub fn int(v: impl TryInto<i64>) -> S {
    S::Int(v.try_into().unwrap_or(i64::MAX))
}
pub fn flag(b: bool) -> S {
    S::Bool(b)
}
pub fn arr(items: impl IntoIterator<Item = S>) -> S {
    S::Array(items.into_iter().collect())
}
/// A row on one line however long it is: a reply is bounded, and a list broken into a line per field
/// spends half of that on indentation.
pub fn line(row: S) -> S {
    match makepad_script::syntax::write::write(&row) {
        Ok(text) => S::Verbatim(text),
        Err(_) => row,
    }
}
pub fn opt(v: Option<S>) -> S {
    v.unwrap_or(S::Nil)
}

fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    let r = (x * k).round() / k;
    if r == 0.0 { 0.0 } else { r }
}

/// A plain number, three decimals.
pub fn n(x: f64) -> S {
    if x.is_finite() { S::Num(round(x, 3)) } else { S::Nil }
}

/// A length in millimetres, written `12.35mm`.
pub fn mm(x: f64) -> S {
    if !x.is_finite() {
        return S::Nil;
    }
    S::Verbatim(format!("{}mm", round(x, 2)))
}

/// A point of millimetres.
pub fn mm3(p: [f64; 3]) -> S {
    arr(p.iter().map(|x| mm(*x)))
}

pub fn mm2(p: [f64; 2]) -> S {
    arr(p.iter().map(|x| mm(*x)))
}

/// An angle in degrees, written `12.5°`.
pub fn deg(x: f64) -> S {
    if !x.is_finite() {
        return S::Nil;
    }
    S::Verbatim(format!("{}°", round(x, 2)))
}

/// The answer's text.
pub fn text(v: &S) -> String {
    makepad_script::syntax::write::write_indented(v, "").unwrap_or_else(|e| format!("{{error: {}}}", makepad_script::syntax::write::quote(&e)))
}
