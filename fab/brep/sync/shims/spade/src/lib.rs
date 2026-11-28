//! The constrained triangulation interface used by solidcore's planar caps.
use makepad_csg_math::orient2d;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

pub mod handles;
mod mesh;
use handles::{FixedFaceHandle, FixedVertexHandle, Handle, InnerTag};
use mesh::Mesh;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point2<S> {
    pub x: S,
    pub y: S,
}

impl<S> Point2<S> {
    pub fn new(x: S, y: S) -> Self {
        Self { x, y }
    }
}

// The coordinate range of spade's double-precision geometric predicates.
const MIN_COORD: f64 = f64::from_bits((1023 - 142) << 52);
const MAX_COORD: f64 = f64::from_bits((1023 + 201) << 52);

pub fn mitigate_underflow(p: Point2<f64>) -> Point2<f64> {
    let normalize = |v: f64| if v.abs() < MIN_COORD { 0.0 } else { v };
    Point2::new(normalize(p.x), normalize(p.y))
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct InsertionError;

pub struct ConstrainedDelaunayTriangulation<V> {
    points: Vec<V>,
    by_position: HashMap<[u64; 2], FixedVertexHandle>,
    // Preserve insertion order when enforcing constraints in the CDT.
    constraints: Vec<[usize; 2]>,
    constraint_set: HashSet<[usize; 2]>,
    mesh: OnceCell<Mesh>,
}

fn side(a: usize, b: usize) -> [usize; 2] {
    [a.min(b), a.max(b)]
}

fn turn(a: Point2<f64>, b: Point2<f64>, c: Point2<f64>) -> f64 {
    orient2d(a.x, a.y, b.x, b.y, c.x, c.y)
}

pub trait Triangulation: Sized {
    fn new() -> Self;
    fn insert(&mut self, point: Point2<f64>) -> Result<FixedVertexHandle, InsertionError>;
    fn num_all_faces(&self) -> usize;
    fn inner_faces(&self) -> impl Iterator<Item = Handle<'_, InnerTag>>;
    fn convex_hull(&self) -> impl Iterator<Item = handles::DirectedEdgeHandle<'_, Point2<f64>, (), (), ()>>;
    fn face(&self, id: FixedFaceHandle<InnerTag>) -> Handle<'_, InnerTag>;
}

impl Triangulation for ConstrainedDelaunayTriangulation<Point2<f64>> {
    fn new() -> Self {
        Self {
            points: Vec::new(),
            by_position: HashMap::new(),
            constraints: Vec::new(),
            constraint_set: HashSet::new(),
            mesh: OnceCell::new(),
        }
    }

    fn insert(&mut self, point: Point2<f64>) -> Result<FixedVertexHandle, InsertionError> {
        fn coordinate(v: f64) -> Result<f64, InsertionError> {
            if v == 0.0 {
                Ok(0.0)
            } else if (MIN_COORD..=MAX_COORD).contains(&v.abs()) {
                Ok(v)
            } else {
                Err(InsertionError)
            }
        }
        let point = Point2::new(coordinate(point.x)?, coordinate(point.y)?);
        let key = [point.x.to_bits(), point.y.to_bits()];
        if let Some(&handle) = self.by_position.get(&key) {
            return Ok(handle);
        }
        let handle = FixedVertexHandle(self.points.len());
        self.points.push(point);
        self.by_position.insert(key, handle);
        self.mesh.take();
        Ok(handle)
    }

    fn num_all_faces(&self) -> usize {
        self.mesh().inner_faces + 1
    }

    fn inner_faces(&self) -> impl Iterator<Item = Handle<'_, InnerTag>> {
        let mesh = self.mesh();
        (1..=mesh.inner_faces).map(move |index| Handle::new(mesh, index))
    }

    fn convex_hull(&self) -> impl Iterator<Item = handles::DirectedEdgeHandle<'_, Point2<f64>, (), (), ()>> {
        let mesh = self.mesh();
        (mesh.inner_faces * 3..mesh.edges.len()).map(move |index| Handle::new(mesh, index))
    }

    fn face(&self, id: FixedFaceHandle<InnerTag>) -> Handle<'_, InnerTag> {
        Handle::new(self.mesh(), id.index())
    }
}

impl ConstrainedDelaunayTriangulation<Point2<f64>> {
    fn mesh(&self) -> &Mesh {
        self.mesh.get_or_init(|| Mesh::build(&self.points, &self.constraints, &self.constraint_set))
    }

    pub fn exists_constraint(&self, a: FixedVertexHandle, b: FixedVertexHandle) -> bool {
        self.constraint_set.contains(&side(a.index(), b.index()))
    }

    pub fn can_add_constraint(&self, a: FixedVertexHandle, b: FixedVertexHandle) -> bool {
        if a == b {
            return false;
        }
        let (p, q) = (self.points[a.index()], self.points[b.index()]);
        for (i, &v) in self.points.iter().enumerate() {
            if i != a.index() && i != b.index()
                && (p.x.min(q.x)..=p.x.max(q.x)).contains(&v.x)
                && (p.y.min(q.y)..=p.y.max(q.y)).contains(&v.y)
                && turn(p, q, v) == 0.0
            {
                return false;
            }
        }
        let opposite = |x: f64, y: f64| (x < 0.0 && y > 0.0) || (x > 0.0 && y < 0.0);
        !self.constraints.iter().any(|&[i, j]| {
            let (u, v) = (self.points[i], self.points[j]);
            opposite(turn(p, q, u), turn(p, q, v)) && opposite(turn(u, v, p), turn(u, v, q))
        })
    }

    pub fn add_constraint(&mut self, a: FixedVertexHandle, b: FixedVertexHandle) -> bool {
        if a == b || !self.constraint_set.insert(side(a.index(), b.index())) {
            return false;
        }
        self.constraints.push([a.index(), b.index()]);
        self.mesh.take();
        true
    }
}
