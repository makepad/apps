use super::{mesh::Mesh, Point2};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FixedVertexHandle(pub(crate) usize);

impl FixedVertexHandle {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InnerTag;
pub struct VertexTag;
pub struct PossiblyOuterTag;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FixedFaceHandle<Tag>(usize, PhantomData<Tag>);

impl<Tag> FixedFaceHandle<Tag> {
    pub fn index(&self) -> usize {
        self.0
    }
}

/// A borrowing handle identifies an element by its mesh and table index.
pub struct Handle<'a, Tag> {
    mesh: &'a Mesh,
    index: usize,
    tag: PhantomData<fn() -> Tag>,
}

impl<Tag> Copy for Handle<'_, Tag> {}
impl<Tag> Clone for Handle<'_, Tag> {
    fn clone(&self) -> Self { *self }
}
impl<Tag> PartialEq for Handle<'_, Tag> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.mesh, other.mesh) && self.index == other.index
    }
}
impl<Tag> Eq for Handle<'_, Tag> {}
impl<Tag> Hash for Handle<'_, Tag> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.mesh, state);
        self.index.hash(state);
    }
}

impl<'a, Tag> Handle<'a, Tag> {
    pub(crate) fn new(mesh: &'a Mesh, index: usize) -> Self {
        Self { mesh, index, tag: PhantomData }
    }

    pub fn index(self) -> usize {
        self.index
    }
}

pub type DirectedEdgeHandle<'a, V, DE, UE, F> = Handle<'a, (V, DE, UE, F)>;
type CapEdge<'a> = DirectedEdgeHandle<'a, Point2<f64>, (), (), ()>;

impl Handle<'_, VertexTag> {
    pub fn fix(self) -> FixedVertexHandle {
        FixedVertexHandle(self.index)
    }
}

impl<'a, V, DE, UE, F> DirectedEdgeHandle<'a, V, DE, UE, F> {
    pub fn vertices(self) -> [Handle<'a, VertexTag>; 2] {
        self.mesh.edges[self.index].vertices.map(|v| Handle::new(self.mesh, v))
    }

    pub fn rev(self) -> Self {
        Self::new(self.mesh, self.mesh.edges[self.index].twin)
    }

    pub fn face(self) -> Handle<'a, PossiblyOuterTag> {
        Handle::new(self.mesh, self.mesh.edges[self.index].face)
    }

    pub fn is_constraint_edge(self) -> bool {
        self.mesh.edges[self.index].constraint
    }
}

impl<'a> Handle<'a, PossiblyOuterTag> {
    pub fn as_inner(self) -> Option<Handle<'a, InnerTag>> {
        (self.index != 0).then(|| Handle::new(self.mesh, self.index))
    }
}

impl<'a> Handle<'a, InnerTag> {
    pub fn fix(self) -> FixedFaceHandle<InnerTag> {
        FixedFaceHandle(self.index, PhantomData)
    }

    pub fn adjacent_edges(self) -> [CapEdge<'a>; 3] {
        let start = (self.index - 1) * 3;
        [start, start + 1, start + 2].map(|e| Handle::new(self.mesh, e))
    }

    pub fn vertices(self) -> [Handle<'a, VertexTag>; 3] {
        self.adjacent_edges().map(|e| e.vertices()[0])
    }
}
