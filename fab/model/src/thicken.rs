//! Thicken: a part made thicker at one place, as a program edit. Each spot is one more link of a
//! chain of `m.blend`, a capsule round the place blended into the part's body, written as `let`s just
//! after the body's own statement, so several spots read as one chain and the name the rest of the
//! script uses for the body now means the thickened one:
//!
//! ```text
//! let figure_base = m.blend([..], 1mm)
//! let figure_thick1 = m.blend([figure_base m.capsule({a: [..] b: [..] radius: 4mm})], 1.2mm)
//! let figure = m.blend([figure_thick1 m.sphere({radius: 3.5mm at: [..]})], 1.05mm)
//! ```
//!
//! It's geometry like the rest of the body, so its bounds stay tight under any turn of the part, and
//! nothing past the capsule's blend moves. A part that can't take it (a body with no distance field)
//! says so when the edit is built.

use crate::program::Instance;
use crate::volumes::taken;
use crate::{Change, Edit};

/// One place to thicken, in the model frame (mm): a capsule from `a` to `b`, a ball where they're
/// the same.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thicken {
    pub a: [f64; 3],
    pub b: [f64; 3],
    /// Half the thickness it's taken to.
    pub radius: f64,
}

/// How far a thickening's shoulder runs, per mm of its radius.
const SHOULDER: f64 = 0.3;

/// A length in the program: thin places are fine work, to a hundredth of a mm.
fn fine(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{}mm", if r == 0.0 { 0.0 } else { r })
}

fn point(p: [f64; 3]) -> String {
    format!("[{}, {}, {}]", fine(p[0]), fine(p[1]), fine(p[2]))
}

/// A link of the chain on `source`.
fn link(source: &str, spot: &Thicken) -> String {
    let radius = fine(spot.radius);
    let shape = if spot.a == spot.b {
        format!("m.sphere({{radius: {radius} at: {}}})", point(spot.a))
    } else {
        format!("m.capsule({{a: {} b: {} radius: {radius}}})", point(spot.a), point(spot.b))
    };
    format!("m.blend([{source} {shape}], {})", fine(SHOULDER * spot.radius))
}

/// The script's edit that thickens solid `body` of `inst` at `spots`: one splice, so one change of a
/// transaction however many spots. A body of its own `let` keeps its name for the chain's last link
/// and every use of it goes on meaning the body; an inline one (`object(m.blend(..))`) takes the
/// chain's last name where it was written.
pub fn thicken_change(inst: &Instance, body: usize, stem: &str, spots: &[Thicken]) -> Result<Change, String> {
    let p = &inst.script.program;
    let solid = p.solids.get(body).ok_or("that part has no body to thicken")?;
    if spots.is_empty() {
        return Err("no places to thicken".into());
    }
    for spot in spots {
        if !(spot.radius > 0.0) {
            return Err(format!("a place to thicken takes a radius more than 0, not {}", spot.radius));
        }
    }
    let span = solid.call.as_ref().map_or_else(|| solid.span.clone(), |c| Some(c.span.clone())).ok_or_else(|| format!("'{}' isn't written where a chain could follow it", solid.id))?;
    let src = inst.script.source();
    let written = src.get(span.clone()).ok_or("the script changed under the edit")?;
    let name = if solid.named { solid.id.clone() } else { stem.to_string() };
    // the links' names, the last of them the body's: a first chain starts from `_base`, a longer
    // one goes on numbering
    let mut number = 0;
    let mut next = || loop {
        number += 1;
        let n = format!("{name}_thick{number}");
        if !taken(p, &n) {
            break n;
        }
    };
    let base = if [format!("[{name}_base"), format!("[{name}_thick")].iter().any(|link| written.starts_with("m.blend(") && written.contains(link.as_str())) {
        next()
    } else {
        (1..).map(|i| if i == 1 { format!("{name}_base") } else { format!("{name}_base{i}") }).find(|n| !taken(p, n)).unwrap()
    };
    let mut lines = String::new();
    let mut last = base.clone();
    for (i, spot) in spots.iter().enumerate() {
        let this = if i + 1 == spots.len() && solid.named { name.clone() } else { next() };
        lines += &format!("let {this} = {}\n", link(&last, spot));
        last = this;
    }
    if solid.named {
        let head = format!("let {name}");
        let from = src[..span.start].rfind(&head).filter(|at| src[at + head.len()..span.start].trim_start().starts_with('=')).ok_or_else(|| format!("'{name}' isn't written as a let a chain could follow"))?;
        // past a comment that ends the statement's line
        let rest = &src[span.end..];
        let line = rest.find('\n').unwrap_or(rest.len());
        let after = rest[..line].trim_start();
        let end = if after.is_empty() || after.starts_with("//") { span.end + line } else { span.end };
        let mut text = format!("let {base}{}", &src[from + head.len()..end]);
        text.push('\n');
        text.push_str(lines.trim_end_matches('\n'));
        return Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: from..end, text } });
    }
    let from = src[..span.start].rfind('\n').map_or(0, |at| at + 1);
    let text = format!("let {base} = {written}\n{lines}{}{last}", &src[from..span.start]);
    Ok(Change::Script { file: inst.script.file.clone(), edit: Edit::Splice { range: from..span.end, text } })
}
