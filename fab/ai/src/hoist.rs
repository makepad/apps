//! A copy's own names. A body that holds a part twice (`leg1` and its mirror written inline,
//! `m.brep.union(leg1, m.brep.mirror(leg1, ..))`) gives both copies' faces the part's names, so
//! an edit by `leg1.top` could land on either copy. Every face of a copy also holds the name of
//! the copy's own step. A copy named by a `let` writes its faces by it (`leg2.top`); an inline
//! copy is named first: its call is hoisted into a `let` of its own (`let leg1_mirror =
//! m.brep.mirror(leg1, ..)`, [`edit::Edit::Rename`] of an inline solid), and the tool writes by
//! `leg1_mirror.top`.
use crate::inspect;
use fab_model::eval::ExactBody;
use fab_model::params::Writes;
use makepad_csg_graph::edges::FaceName;
use makepad_model_script::lang::{edit, Program};
use std::sync::Arc;

/// One exact body's face names as a tool writes them, and the inline copies named on the way.
pub(crate) struct Own<'p> {
    program: &'p Program,
    /// The body, its parts' ids taking the names hoisted so far (a copy of the build's once one
    /// is).
    pub body: Arc<ExactBody>,
    /// Each inline solid hoisted and the name it takes, in the order they are written.
    pub hoists: Vec<(String, String)>,
}

impl<'p> Own<'p> {
    pub fn new(program: &'p Program, body: Arc<ExactBody>) -> Self {
        Self { program, body, hoists: Vec::new() }
    }

    /// The face picked by `name` and the name a tool writes it by ([`Own::name`]). Of several
    /// faces holding `name` (the copies'), the one whose middle is nearest `at`, where the pick
    /// pointed: copies of one face are congruent, so their middles tell them apart.
    pub fn face(&mut self, name: &str, at: Option<[f64; 3]>) -> Result<(u32, String), String> {
        let b = &self.body;
        let d = |f: &&makepad_csg_graph::BrepEntity| at.map_or(0.0, |p| (0..3).map(|k| (f.at[k] - p[k]).powi(2)).sum::<f64>());
        let face = b.faces.iter().filter(|f| inspect::face_holds(b, f.id, name)).min_by(|x, y| d(x).total_cmp(&d(y))).ok_or_else(|| format!("the part has no face `{name}` any more"))?.id;
        Ok((face, self.name(face, name)?))
    }

    /// The name a tool writes face `face` by, picked by `name`: `name` when it is the face's own;
    /// when the body's copies share it, the name of the face's copy (the outermost one that is
    /// no repeated step), hoisting the copy into a `let` when it is an inline call. Err: why an
    /// edit by `name` must not be made ([`inspect::shared_name`]), and why the copy takes no `let`.
    pub fn name(&mut self, face: u32, name: &str) -> Result<String, String> {
        let Some(why) = inspect::shared_name(&self.body, face, name) else { return Ok(name.to_string()) };
        let b = &self.body;
        let (Some(f), Some(picked)) = (b.faces.iter().find(|f| f.id == face), inspect::face_named(b, face, name)) else { return Err(why) };
        let copy = |n: &&FaceName| n.role == picked.role && !b.repeated.contains(&n.origin);
        let outermost = |n: &&FaceName| b.order.iter().position(|k| *k == n.origin);
        // a copy named by a `let` (or hoisted already)
        if let Some(n) = f.names.iter().filter(copy).filter(|n| inspect::writable(n, &b.parts)).min_by_key(outermost) {
            return Ok(inspect::written(n, &b.parts, &b.sides));
        }
        let inline = |n: &&FaceName| b.parts.get(&n.origin).and_then(|id| self.program.solid(id)).is_some_and(|s| !s.named && s.call.is_none() && s.span.is_some());
        let Some(n) = f.names.iter().filter(copy).filter(inline).min_by_key(outermost).cloned() else { return Err(why) };
        let id = b.parts[&n.origin].clone();
        let taken: Vec<&str> = self.hoists.iter().map(|(_, to)| to.as_str()).collect();
        let to = edit::hoist_name(self.program, &id, &taken).ok_or_else(|| why.clone())?;
        edit::apply(self.program, &edit::Edit::Rename { solid: id.clone(), to: to.clone() }).map_err(|e| format!("{why}. Its copy takes no `let` of its own here: {e}"))?;
        let body = Arc::make_mut(&mut self.body);
        body.parts.insert(n.origin, to.clone());
        body.sides.remove(&n.origin);
        self.hoists.push((id, to));
        Ok(inspect::written(&n, &self.body.parts, &self.body.sides))
    }

    /// The selector a tool writes the edge where faces named `a` and `c` meet by (of several
    /// pieces, the one nearest `at`, where the pick pointed): None when `a` and `c` are its faces'
    /// own names, else `m.edge(..)` by its faces' copies' own names ([`Own::name`]).
    pub fn edge(&mut self, [a, c]: &[String; 2], at: Option<[f64; 3]>, chain: bool) -> Result<Option<String>, String> {
        let b = &self.body;
        let d = |e: &&makepad_csg_graph::BrepEntity| at.map_or(0.0, |p| crate::picks::polyline_distance(&e.points, p));
        let e = inspect::edges_between(b, a, c).into_iter().min_by(|x, y| d(x).total_cmp(&d(y))).ok_or_else(|| format!("`{a}` and `{c}` no longer meet"))?.clone();
        let [x, y] = e.faces[..] else { return Ok(None) };
        let (fa, fc) = if inspect::face_holds(b, x, a) && inspect::face_holds(b, y, c) { (x, y) } else { (y, x) };
        let pair = [self.name(fa, a)?, self.name(fc, c)?];
        if pair[0] == *a && pair[1] == *c {
            return Ok(None);
        }
        let mut pair = pair;
        pair.sort();
        Ok(Some(inspect::edge_selector(&self.body, &pair, &e, chain)))
    }

    /// `writes` once the copies named on the way are hoisted.
    pub fn writes(&self, writes: Writes) -> Writes {
        if self.hoists.is_empty() {
            return writes;
        }
        Writes::Hoisted { hoists: self.hoists.clone(), then: Box::new(writes) }
    }
}
