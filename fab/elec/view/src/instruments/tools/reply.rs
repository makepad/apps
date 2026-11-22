//! The only Splash reply writer, including byte-exact whole-item pagination.
//!
//! A budget shapes an answer and never refuses one: a list answers the whole items that fit and
//! says how many are left and how to ask for them, carrying at least one item while any remain
//! (one item larger than the budget comes whole, alone on its page), and a single object is
//! answered whole.

use fab_elec_circuit::{QtyUnit, Quantity};
use fab_elec_lang::units::literal_text;

/// A tool result, either one Splash object or a single-line refusal.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolReply {
    /// One complete Splash object literal.
    Ok(String),
    /// One line explaining the refusal.
    Failed(String),
}

/// A source statement written by a host bench edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    /// Project-relative document path.
    pub file: String,
    /// One-based line number.
    pub line: usize,
}

pub(super) fn quote(text: &str) -> String {
    makepad_widgets_core::makepad_script::syntax::write::quote(text)
}

pub(super) fn qty(value: f64, unit: QtyUnit) -> String {
    literal_text(&Quantity { si: value, unit })
}

pub(super) fn percent(value: f64) -> String {
    format!("{value}%")
}

pub(super) fn list(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(", "))
}

#[derive(Clone, Default)]
pub(super) struct Object(Vec<(&'static str, String)>);

impl Object {
    pub fn field(&mut self, name: &'static str, value: String) { self.0.push((name, value)); }
    pub fn text(&mut self, name: &'static str, value: &str) { self.field(name, quote(value)); }
    pub fn symbol(&mut self, name: &'static str, value: &str) { self.field(name, format!("@{value}")); }
    pub fn int(&mut self, name: &'static str, value: usize) { self.field(name, value.to_string()); }
    pub fn num(&mut self, name: &'static str, value: Option<f64>, unit: QtyUnit) {
        if let Some(value) = value.filter(|v| v.is_finite()) { self.field(name, qty(value, unit)); }
    }
    pub fn site(&mut self, name: &'static str, site: &Site) {
        let mut object = Self::default(); object.text("file", &site.file); object.int("line", site.line);
        self.field(name, object.render());
    }
    /// A page's continuation: the cursor, and plainly how many are left and how to ask.
    pub fn next(&mut self, next: Option<&Next>) {
        if let Some(next) = next {
            self.text("next", &next.cursor);
            self.text("more", &next.more());
        }
    }
    pub fn render(&self) -> String {
        format!("{{{}}}", self.0.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(" "))
    }
    /// One object is answered whole, whatever the budget: only lists page.
    pub fn reply(&self) -> ToolReply {
        ToolReply::Ok(self.render())
    }
}

/// Where a page stops short of its list's end.
pub(super) struct Next {
    /// The opaque cursor the next page is asked with (`after`).
    pub cursor: String,
    /// Items after this page.
    pub left: usize,
}

impl Next {
    fn more(&self) -> String { format!("{} more; ask again with after: next", self.left) }
    /// The bytes `Object::next` adds after an object's other fields.
    pub fn bytes(&self) -> usize {
        " next: ".len() + quote(&self.cursor).len() + " more: ".len() + quote(&self.more()).len()
    }
}

pub(super) fn failed(reason: impl AsRef<str>) -> ToolReply {
    ToolReply::Failed(reason.as_ref().split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Cursor offsets are tied to an immutable query, including its span and filter.
pub(super) fn offset(after: Option<&str>, key: &str, total: usize) -> Result<usize, String> {
    let Some(after) = after else { return Ok(0) };
    let (prefix, index) = after.rsplit_once('/').ok_or("after: invalid cursor")?;
    if prefix != key { return Err("after: cursor belongs to another query".into()); }
    index.parse::<usize>().ok().filter(|&n| n < total).ok_or_else(|| "after: cursor is outside this query".into())
}

/// Render only an answer-sized prefix. Exponential growth and binary search avoid formatting
/// every prefix of a large page; work is O(answer size * log answer size), not O(record length).
pub(super) fn page(
    budget: usize, start: usize, total: usize, key: &str,
    mut item: impl FnMut(usize) -> String,
    render: impl Fn(&[String], Option<&Next>) -> String,
) -> ToolReply {
    page_values(budget, start, total, usize::MAX, key, |i| Ok(item(i)), render)
        .unwrap_or_else(failed)
}

pub(super) fn page_values<T>(
    budget: usize, start: usize, total: usize, limit: usize, key: &str,
    mut item: impl FnMut(usize) -> Result<T, String>,
    render: impl Fn(&[T], Option<&Next>) -> String,
) -> Result<ToolReply, String> {
    let count = total.saturating_sub(start).min(limit);
    let output = |items: &[T]| {
        let end = start + items.len();
        let next = (end < total).then(|| Next { cursor: format!("{key}/{end}"), left: total - end });
        render(items, next.as_ref())
    };
    if count == 0 { return Ok(ToolReply::Ok(render(&[], None))); }
    let mut items = vec![item(start)?];
    let mut best = output(&items);
    if best.len() > budget {
        // Removing the last cursor can make the complete list smaller than its first page.
        // Inspect only enough whole items to rule that out, never the rest of the record;
        // otherwise the first item answers alone, over the budget, rather than nothing.
        let first = best.len();
        while items.len() < count && render(&items, None).len() < first {
            items.push(item(start + items.len())?);
        }
        if start + items.len() == total {
            let text = output(&items);
            if text.len() <= budget { return Ok(ToolReply::Ok(text)); }
        }
        return Ok(ToolReply::Ok(best));
    }
    let mut accepted = 1;
    while items.len() < count {
        let target = items.len().saturating_mul(2).min(count);
        while items.len() < target { items.push(item(start + items.len())?); }
        let text = output(&items);
        if text.len() <= budget { accepted = items.len(); best = text; continue; }
        let (mut lo, mut hi) = (accepted + 1, items.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let text = output(&items[..mid]);
            if text.len() <= budget { best = text; lo = mid + 1; }
            else { hi = mid; }
        }
        break;
    }
    Ok(ToolReply::Ok(best))
}
