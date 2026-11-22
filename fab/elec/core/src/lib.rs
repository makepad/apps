//! Fab electronics core: contract C1 (E1 1.2, 8).
//!
//! Every electronics crate uses these types; nobody carries a private point type.
//!
//! - `geom`: integer nm points, boxes, angles, paths and contours with true arcs. Board frame: X
//!   right, Y UP.
//! - `layer`: fixed layer identities (front/inner/back copper, technical, edge, user) and sets.
//! - `shape`: the filled shapes copper and graphics are made of, as core plus radius.
//! - `poly`: `PolySet`, normalised polygons with holes; booleans and offsets over i_overlay.
//! - `index`: `Index<T>`, the R-tree (bulk-loaded static part plus a small dynamic part).
//! - `dist`: exact edge-to-edge distances between shapes; `clearance` is the one definition of
//!   legal spacing that DRC, the router and the zone filler share.
//! - `ids`: plain index ids, scoped per unit; `sel`: `Sel` and `FeatureRef`, the ids that cross
//!   units; `diag`: `Severity`, `Diag`.
//! - `sch`: `SchPt`, the 1.27 mm connection grid of symbols and sheets.
//! - `xform`: `Xform3`, a unit's rigid pose in its parent frame.
//! - `canon`: the canonical binary form and the content hash.
//! - `drive`: `Drive` and `Pull`, a bench's drive of a stubbed pin (benches and design ops).
//!
//! Performance: nothing here runs per frame. Shape and distance functions are O(1) per pair
//! (polygon cores O(vertices)); `PolySet` ops are O(n log n) in the vertices involved; index
//! queries are O(log n + hits).

pub mod canon;
pub mod diag;
pub mod drive;
pub mod dist;
pub mod geom;
pub mod ids;
pub mod index;
pub mod layer;
pub mod poly;
mod poly_grid;
pub mod pquad;
pub mod pvec;
pub mod sch;
pub mod sel;
pub mod shape;
pub mod xform;

pub use canon::{Canon, ContentHash, Writer};
pub use diag::{Diag, Severity};
pub use dist::{clearance, collides, point_clearance};
pub use geom::{metres, mil, mm, to_mm, um, Angle, BBox, Contour, Nm, OnTrack, Path, Seg, P};
pub use ids::*;
pub use index::Index;
pub use layer::{LayerId, LayerKind, LayerSet, LayerSide, Side};
pub use poly::{PolySet, Polygon, RowContains};
pub use poly_grid::{Hit, PolyGrid};
pub use pquad::PQuad;
pub use pvec::PVec;
pub use sch::SchPt;
pub use sel::{FeatureRef, Sel};
pub use shape::Shape;
pub use xform::Xform3;

/// The letters of a reference made of letters followed by digits. O(reference length).
pub fn refdes_prefix(reference: &str) -> Option<&str> {
    let prefix = reference.trim_end_matches(|c: char| c.is_ascii_digit());
    (!prefix.is_empty() && prefix.len() < reference.len() && prefix.bytes().all(|b| b.is_ascii_alphabetic()))
        .then_some(prefix)
}

/// List distinct references by prefix and number, joining consecutive numbers with an en dash.
/// Sorting takes O(n log n) comparisons and O(n) space for n references.
pub fn refdes_ranges<'a>(refs: impl IntoIterator<Item = &'a str>) -> String {
    let mut refs: Vec<_> = refs.into_iter().map(|name| {
        let prefix = refdes_prefix(name).unwrap_or(name);
        let digits = name[prefix.len()..].trim_start_matches('0');
        (prefix, digits.len(), digits, name)
    }).collect();
    refs.sort_unstable();
    refs.dedup();

    let mut out = String::new();
    let mut refs = refs.into_iter().peekable();
    while let Some((prefix, _, mut digits, first)) = refs.next() {
        let mut last = first;
        while let Some(&(next_prefix, _, next_digits, next)) = refs.peek() {
            if prefix != next_prefix || first == prefix || !consecutive_digits(digits, next_digits) {
                break;
            }
            last = next;
            digits = next_digits;
            refs.next();
        }
        if !out.is_empty() { out.push_str(", "); }
        out.push_str(first);
        if first != last {
            out.push('–');
            out.push_str(last);
        }
    }
    out
}

// Add one from the right so even a suffix too large for a machine integer keeps its order.
fn consecutive_digits(before: &str, after: &str) -> bool {
    let mut before = before.bytes().rev();
    let mut carry = 1;
    for digit in after.bytes().rev() {
        let sum = before.next().map_or(0, |d| d - b'0') + carry;
        if digit != b'0' + sum % 10 { return false; }
        carry = sum / 10;
    }
    carry == 0 && before.next().is_none()
}
