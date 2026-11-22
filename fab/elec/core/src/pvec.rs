//! A persistent sparse map laid out as a 32-way trie over five-bit groups of `u32` keys.
//!
//! Inner nodes contain 32 optional shared children; leaves contain 32 optional values. Both
//! track occupied slots. The root uses the smallest depth covering the largest key, at most
//! seven levels. Empty nodes disappear and redundant leading zero levels collapse, so the
//! layout depends only on the key-value set, not its edit history.
//!
//! Snapshots share nodes through `Arc`. Edits use `Arc::make_mut` along their path, preserving
//! older snapshots and updating uniquely owned nodes in place. Getting a value is O(levels);
//! setting or removing one is O(levels * 32) with path copies. Iteration is O(n) for n entries
//! with the fixed seven-level bound; cloning is O(1), cloning only the root `Arc`.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// A sparse, persistent map from item indices to values.
#[derive(Clone)]
pub struct PVec<T> {
    root: Option<Arc<Node<T>>>,
    levels: u8,
    len: usize,
}

#[derive(Clone, PartialEq)]
enum Node<T> {
    Inner {
        children: [Option<Arc<Node<T>>>; 32],
        count: u8,
    },
    Leaf {
        values: [Option<T>; 32],
        count: u8,
    },
}

fn levels_for(key: u32) -> u8 {
    (32 - key.leading_zeros()).max(1).div_ceil(5) as u8
}

fn slot(key: u32, levels: u8) -> usize {
    ((key >> (5 * (levels - 1))) & 31) as usize
}

impl<T: Clone> Node<T> {
    fn empty(levels: u8) -> Self {
        if levels == 1 {
            Self::Leaf {
                values: std::array::from_fn(|_| None),
                count: 0,
            }
        } else {
            Self::Inner {
                children: std::array::from_fn(|_| None),
                count: 0,
            }
        }
    }

    fn count(&self) -> u8 {
        match self {
            Self::Inner { count, .. } | Self::Leaf { count, .. } => *count,
        }
    }

    fn set(node: &mut Arc<Self>, levels: u8, key: u32, value: T) -> Option<T> {
        match Arc::make_mut(node) {
            Self::Leaf { values, count } => {
                let previous = values[slot(key, levels)].replace(value);
                if previous.is_none() {
                    *count += 1;
                }
                previous
            }
            Self::Inner { children, count } => {
                let child = children[slot(key, levels)].get_or_insert_with(|| {
                    *count += 1;
                    Arc::new(Self::empty(levels - 1))
                });
                Self::set(child, levels - 1, key, value)
            }
        }
    }

    fn remove(node: &mut Arc<Self>, levels: u8, key: u32) -> Option<T> {
        match Arc::make_mut(node) {
            Self::Leaf { values, count } => {
                let previous = values[slot(key, levels)].take()?;
                *count -= 1;
                Some(previous)
            }
            Self::Inner { children, count } => {
                let index = slot(key, levels);
                let child = children[index].as_mut()?;
                let previous = Self::remove(child, levels - 1, key)?;
                if child.count() == 0 {
                    children[index] = None;
                    *count -= 1;
                }
                Some(previous)
            }
        }
    }
}

impl<T: Clone> PVec<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, key: u32) -> Option<&T> {
        if levels_for(key) > self.levels {
            return None;
        }
        let mut node = self.root.as_deref()?;
        let mut levels = self.levels;
        loop {
            match node {
                Node::Leaf { values, .. } => return values[slot(key, levels)].as_ref(),
                Node::Inner { children, .. } => {
                    node = children[slot(key, levels)].as_deref()?;
                    levels -= 1;
                }
            }
        }
    }

    /// Insert or replace; returns the previous value.
    pub fn set(&mut self, key: u32, value: T) -> Option<T> {
        let needed = levels_for(key);
        if self.root.is_none() {
            self.levels = needed;
        }
        while self.levels < needed {
            let mut children = std::array::from_fn(|_| None);
            children[0] = self.root.take();
            self.root = Some(Arc::new(Node::Inner { children, count: 1 }));
            self.levels += 1;
        }
        let root = self.root.get_or_insert_with(|| Arc::new(Node::empty(needed)));
        let previous = Node::set(root, self.levels, key, value);
        if previous.is_none() {
            self.len += 1;
        }
        previous
    }

    /// Remove an entry; returns the previous value.
    pub fn remove(&mut self, key: u32) -> Option<T> {
        // Missing keys must not detach any shared nodes or alias a shorter trie.
        self.get(key)?;
        let previous = Node::remove(self.root.as_mut()?, self.levels, key)?;
        self.len -= 1;
        if self.len == 0 {
            self.root = None;
            self.levels = 0;
        } else {
            loop {
                let child = match self.root.as_deref() {
                    Some(Node::Inner { children, count: 1 }) => children[0].clone(),
                    _ => None,
                };
                let Some(child) = child else {
                    break;
                };
                self.root = Some(child);
                self.levels -= 1;
            }
        }
        Some(previous)
    }

    /// Present entries in ascending key order, including `u32::MAX` if present.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        Entries::new(self.root.as_deref(), self.levels, 0..(1u64 << 32))
    }

    /// Present entries in `range` (inclusive start, exclusive end), ascending.
    pub fn range(&self, range: Range<u32>) -> impl Iterator<Item = (u32, &T)> {
        Entries::new(self.root.as_deref(), self.levels, u64::from(range.start)..u64::from(range.end))
    }

    #[cfg(test)]
    fn levels(&self) -> u8 {
        self.levels
    }

    #[cfg(test)]
    fn shares_child(&self, other: &Self, index: usize) -> bool {
        match (self.root.as_deref(), other.root.as_deref()) {
            (Some(Node::Inner { children: left, .. }), Some(Node::Inner { children: right, .. }))
                if self.levels == other.levels =>
            {
                match (&left[index], &right[index]) {
                    (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

impl<T> Default for PVec<T> {
    fn default() -> Self {
        Self { root: None, levels: 0, len: 0 }
    }
}

impl<T: PartialEq> PartialEq for PVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.levels == other.levels && self.root == other.root
    }
}

impl<T: fmt::Debug> fmt::Debug for PVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PVec").field("len", &self.len).finish()
    }
}

struct Frame<'a, T> {
    node: &'a Node<T>,
    prefix: u64,
    levels: u8,
    next: usize,
}

struct Entries<'a, T> {
    stack: Vec<Frame<'a, T>>,
    range: Range<u64>,
}

impl<'a, T> Entries<'a, T> {
    fn new(root: Option<&'a Node<T>>, levels: u8, range: Range<u64>) -> Self {
        let mut stack = Vec::with_capacity(usize::from(levels));
        if range.start < range.end {
            if let Some(node) = root {
                stack.push(Frame { node, prefix: 0, levels, next: 0 });
            }
        }
        Self { stack, range }
    }
}

impl<'a, T> Iterator for Entries<'a, T> {
    type Item = (u32, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let frame = self.stack.last_mut()?;
            if frame.next == 32 {
                self.stack.pop();
                continue;
            }
            let index = frame.next;
            frame.next += 1;
            let shift = 5 * (frame.levels - 1);
            let key = frame.prefix | ((index as u64) << shift);
            // Widened bounds represent the end after u32::MAX without wrapping.
            if key >= self.range.end {
                self.stack.clear();
                return None;
            }
            if key + (1u64 << shift) <= self.range.start {
                continue;
            }
            match frame.node {
                Node::Leaf { values, .. } => {
                    if let Some(value) = &values[index] {
                        return Some((key as u32, value));
                    }
                }
                Node::Inner { children, .. } => {
                    if let Some(child) = &children[index] {
                        let next = Frame {
                            node: child.as_ref(),
                            prefix: key,
                            levels: frame.levels - 1,
                            next: 0,
                        };
                        self.stack.push(next);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PVec;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    struct Xorshift64(u64);

    impl Xorshift64 {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn key(&mut self) -> u32 {
            match self.next() % 3 {
                0 => (self.next() % 2000) as u32,
                1 => self.next() as u32,
                _ => {
                    if self.next() & 1 == 0 {
                        0
                    } else {
                        u32::MAX
                    }
                }
            }
        }
    }

    #[test]
    fn model_agrees() {
        let mut random = Xorshift64(0x4d59_5df4_d0f3_3173);
        let mut map = PVec::new();
        let mut model = BTreeMap::<u32, u64>::new();
        for step in 0..5000 {
            let key = random.key();
            match random.next() % 3 {
                0 => {
                    let value = random.next();
                    assert_eq!(
                        map.set(key, value),
                        model.insert(key, value),
                        "map equals the model: set result at operation {step}"
                    );
                }
                1 => {
                    assert_eq!(
                        map.remove(key),
                        model.remove(&key),
                        "map equals the model: remove result at operation {step}"
                    );
                }
                _ => {
                    assert_eq!(
                        map.get(key),
                        model.get(&key),
                        "map equals the model: get result at operation {step}"
                    );
                }
            }
            assert_eq!(map.len(), model.len(), "map equals the model: length at operation {step}");
            assert_eq!(map.is_empty(), model.is_empty(), "map equals the model: emptiness at operation {step}");
            if (step + 1) % 250 == 0 {
                assert!(
                    map.iter().eq(model.iter().map(|(&key, value)| (key, value))),
                    "map equals the model: ordered iteration at operation {step}"
                );
                for _ in 0..3 {
                    let a = random.key();
                    let b = random.key();
                    let range = a.min(b)..a.max(b);
                    assert!(
                        map.range(range.clone()).eq(model.range(range).map(|(&key, value)| (key, value))),
                        "map equals the model: ordered range at operation {step}"
                    );
                }
            }
        }
    }

    #[test]
    fn history_independent() {
        let entries: Vec<_> = (0..2000u32).map(|key| (key, u64::from(key) * 17 + 9)).collect();
        let extras: Vec<_> = (0..500u32)
            .map(|offset| match offset % 4 {
                0 => 2000 + offset,
                1 => (1 << 15) + offset,
                2 => (1 << 25) + offset,
                _ => u32::MAX - offset / 4,
            })
            .collect();
        let mut forward = PVec::new();
        let mut backward = PVec::default();
        for &(key, value) in &entries {
            forward.set(key, value);
        }
        for &key in &extras {
            backward.set(key, u64::from(key));
        }
        for &(key, value) in entries.iter().rev() {
            backward.set(key, value);
        }
        assert_eq!(backward.levels(), 7, "shape depends on the set only: large keys require seven levels");
        for &key in extras.iter().rev() {
            assert_eq!(
                backward.remove(key),
                Some(u64::from(key)),
                "shape depends on the set only: extra entry is removed"
            );
        }
        assert_eq!(forward, backward, "shape depends on the set only: equal entries give equal trees");
        assert_eq!(forward.levels(), backward.levels(), "shape depends on the set only: depths agree");
        assert_eq!(forward.levels(), 3, "shape depends on the set only: depth covers the largest key");

        for key in (0..2000u32).rev() {
            assert_eq!(
                forward.remove(key),
                Some(u64::from(key) * 17 + 9),
                "shape depends on the set only: removing the largest entry"
            );
            let expected = match key {
                0 => 0,
                1..=32 => 1,
                33..=1024 => 2,
                _ => 3,
            };
            assert_eq!(forward.levels(), expected, "shape depends on the set only: depth shrinks at boundaries");
        }
        assert_eq!(forward, PVec::new(), "shape depends on the set only: removing all entries restores empty");
    }

    #[test]
    fn persistent() {
        let before: Vec<_> = (0..2048u32).map(|key| u64::from(key) * 17 + 9).collect();
        let mut map = PVec::new();
        for (key, &value) in before.iter().enumerate() {
            map.set(key as u32, value);
        }
        let snapshot = map.clone();
        assert!(
            matches!((&map.root, &snapshot.root), (Some(left), Some(right)) if Arc::ptr_eq(left, right)),
            "edits copy only their paths: a clone initially shares the root"
        );
        for key in 0..300u32 {
            match key % 3 {
                0 => {
                    assert_eq!(
                        map.set(key, u64::MAX - u64::from(key)),
                        Some(before[key as usize]),
                        "a snapshot never changes: replacement returns the original value"
                    );
                }
                1 => {
                    assert_eq!(
                        map.remove(key),
                        Some(before[key as usize]),
                        "a snapshot never changes: removal returns the original value"
                    );
                }
                _ => {
                    assert_eq!(
                        map.set(4096 + key, u64::from(key)),
                        None,
                        "a snapshot never changes: insertion adds a new key"
                    );
                }
            }
        }
        for key in 0..4400u32 {
            assert_eq!(
                snapshot.get(key),
                before.get(key as usize),
                "a snapshot never changes: original and edited key {key}"
            );
        }
        assert_eq!(snapshot.get(u32::MAX), None, "a snapshot never changes: absent maximum key stays absent");
        assert_eq!(snapshot.len(), before.len(), "a snapshot never changes: length stays fixed");
        assert!(map.shares_child(&snapshot, 1), "edits copy only their paths: untouched subtree stays shared");
        assert!(!map.shares_child(&snapshot, 0), "edits copy only their paths: edited subtree is detached");
    }
}
