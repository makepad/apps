//! Immutable chunk trees shared by accepted event times and their trend columns.

use std::fmt::Debug;
use std::sync::Arc;

const CHUNK: usize = 256;
const LEVELS: usize = usize::BITS as usize;

pub(super) trait ChunkValue: Copy + Debug + Default {
    type Summary: Clone + Debug + Default;
    fn summarize(values: &[Self]) -> Self::Summary;
    fn shift(summary: Self::Summary, offset: usize) -> Self::Summary;
    fn merge(left: Self::Summary, right: Self::Summary) -> Self::Summary;
}

impl ChunkValue for f64 {
    type Summary = ();
    fn summarize(_: &[Self]) {}
    fn shift(_: (), _: usize) {}
    fn merge(_: (), _: ()) {}
}

#[derive(Debug)]
enum NodeKind<T: ChunkValue> { Chunk(Arc<[T]>), Branch(Arc<Node<T>>, Arc<Node<T>>) }

#[derive(Debug)]
struct Node<T: ChunkValue> { len: usize, last: T, summary: T::Summary, kind: NodeKind<T> }

#[derive(Clone, Debug)]
struct Directory<T: ChunkValue> { roots: [Option<Arc<Node<T>>>; LEVELS], len: usize }

impl<T: ChunkValue> Default for Directory<T> {
    fn default() -> Self { Self { roots: std::array::from_fn(|_| None), len: 0 } }
}

/// Full chunks joined by binary carries, followed by one copied open tail. The fixed-size
/// directory and immutable nodes are shared; a snapshot clone never walks the record.
#[derive(Clone, Debug, Default)]
pub(super) struct ChunkList<T: ChunkValue> { directory: Option<Arc<Directory<T>>>, tail: Option<Arc<[T]>> }

fn same_arc<T: ?Sized>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) { (Some(a), Some(b)) => Arc::ptr_eq(a, b), (None, None) => true, _ => false }
}

impl<T: ChunkValue> ChunkList<T> {
    fn sealed(&self) -> usize { self.directory.as_ref().map_or(0, |directory| directory.len) }
    pub fn len(&self) -> usize { self.sealed() + self.tail.as_ref().map_or(0, |tail| tail.len()) }
    pub fn same(&self, other: &Self) -> bool { same_arc(&self.directory, &other.directory) && same_arc(&self.tail, &other.tail) }

    pub fn get(&self, mut index: usize) -> Option<T> {
        if index >= self.sealed() { return self.tail.as_ref()?.get(index - self.sealed()).copied(); }
        let directory = self.directory.as_ref()?;
        for root in directory.roots.iter().rev().flatten() {
            if index >= root.len { index -= root.len; continue; }
            let mut node = root.as_ref();
            loop {
                match &node.kind {
                    NodeKind::Chunk(values) => return values.get(index).copied(),
                    NodeKind::Branch(left, right) => {
                        if index < left.len { node = left; }
                        else { index -= left.len; node = right; }
                    }
                }
            }
        }
        None
    }

    /// `before` must be true for a prefix of the list.
    pub fn partition_point(&self, mut before: impl FnMut(T) -> bool) -> usize {
        let tail = |before: &mut dyn FnMut(T) -> bool| self.tail.as_ref().map_or(0, |tail| tail.partition_point(|&t| before(t)));
        let Some(directory) = &self.directory else { return tail(&mut before) };
        let mut index = 0;
        for root in directory.roots.iter().rev().flatten() {
            if before(root.last) { index += root.len; continue; }
            let mut node = root.as_ref();
            loop {
                match &node.kind {
                    NodeKind::Chunk(values) => return index + values.partition_point(|&t| before(t)),
                    NodeKind::Branch(left, right) => {
                        if before(left.last) { index += left.len; node = right; }
                        else { node = left; }
                    }
                }
            }
        }
        index + tail(&mut before)
    }

    /// Half-open index range. Only its two boundary paths descend; full subtrees use their
    /// summaries. At most two partial chunks and the tail are scanned, each under CHUNK values.
    pub fn summary(&self, i0: usize, i1: usize) -> T::Summary {
        fn visit<T: ChunkValue>(node: &Node<T>, offset: usize, i0: usize, i1: usize) -> T::Summary {
            if i1 <= offset || i0 >= offset + node.len { return T::Summary::default(); }
            if i0 <= offset && i1 >= offset + node.len { return T::shift(node.summary.clone(), offset); }
            match &node.kind {
                NodeKind::Chunk(values) => {
                    let start = i0.saturating_sub(offset);
                    let end = (i1 - offset).min(values.len());
                    T::shift(T::summarize(&values[start..end]), offset + start)
                }
                NodeKind::Branch(left, right) => T::merge(visit(left, offset, i0, i1), visit(right, offset + left.len, i0, i1)),
            }
        }
        if i0 >= i1 { return T::Summary::default(); }
        let mut result = T::Summary::default();
        let mut offset = 0;
        if let Some(directory) = &self.directory {
            for root in directory.roots.iter().rev().flatten() {
                result = T::merge(result, visit(root, offset, i0, i1));
                offset += root.len;
            }
        }
        if let Some(tail) = &self.tail {
            let start = i0.saturating_sub(offset).min(tail.len());
            let end = i1.saturating_sub(offset).min(tail.len());
            if start < end { result = T::merge(result, T::shift(T::summarize(&tail[start..end]), offset + start)); }
        }
        result
    }

    fn append(&mut self, values: Vec<T>) {
        let Some(&last) = values.last() else { return };
        let mut node = Arc::new(Node { len: values.len(), last, summary: T::summarize(&values), kind: NodeKind::Chunk(values.into()) });
        let directory = Arc::make_mut(self.directory.get_or_insert_with(|| Arc::new(Directory::default())));
        directory.len += node.len;
        for slot in &mut directory.roots {
            match slot.take() {
                None => { *slot = Some(node); return; }
                Some(left) => {
                    let summary = T::merge(left.summary.clone(), T::shift(node.summary.clone(), left.len));
                    node = Arc::new(Node { len: left.len + node.len, last, summary, kind: NodeKind::Branch(left, node) });
                }
            }
        }
        unreachable!("a chunk count cannot exceed usize::MAX");
    }
}

/// One writer. Only a changed open tail is copied at publication; sealed chunks never reopen.
#[derive(Default)]
pub(super) struct ChunkLog<T: ChunkValue> { list: ChunkList<T>, tail: Vec<T> }

impl<T: ChunkValue> ChunkLog<T> {
    pub fn restart(&mut self) { self.list = ChunkList::default(); self.tail.clear(); }
    pub fn len(&self) -> usize { self.list.sealed() + self.tail.len() }
    pub fn push(&mut self, value: T) {
        self.tail.push(value);
        if self.tail.len() == CHUNK { self.list.append(std::mem::take(&mut self.tail)); self.list.tail = None; }
    }
    pub fn snapshot(&mut self) -> ChunkList<T> {
        if self.list.tail.as_ref().map_or(0, |tail| tail.len()) != self.tail.len() {
            self.list.tail = (!self.tail.is_empty()).then(|| Arc::from(&self.tail[..]));
        }
        self.list.clone()
    }
}

/// Time-ordered accepted triggers, with O(1) snapshot clones and O(log N) index searches.
#[derive(Clone, Debug, Default)]
pub struct EventList(ChunkList<f64>);

impl EventList {
    pub fn len(&self) -> usize { self.0.len() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    pub fn same(&self, other: &Self) -> bool { self.0.same(&other.0) }
    pub fn get(&self, index: usize) -> Option<f64> { self.0.get(index) }
    /// As for a sorted slice: `before` must be true for a prefix of the list.
    pub fn partition_point(&self, before: impl FnMut(f64) -> bool) -> usize { self.0.partition_point(before) }
}

#[derive(Default)]
pub(super) struct EventLog(ChunkLog<f64>);

impl EventLog {
    pub fn restart(&mut self) { self.0.restart(); }
    pub fn len(&self) -> usize { self.0.len() }
    pub fn push(&mut self, t: f64) { self.0.push(t); }
    pub fn snapshot(&mut self) -> EventList { EventList(self.0.snapshot()) }
}
