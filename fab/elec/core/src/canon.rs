//! The canonical binary form of the models, and the content hash over it.
//!
//! Same idea as `makepad_model`'s canonical source: little-endian fixed-width fields, counts as
//! u32, strings as length + UTF-8, every float with one zero (-0.0 is written as +0.0) and
//! never NaN. A model's canonical form depends on its content only: never on `Arc` sharing,
//! allocation, generation counters or caches. Two models with equal content hash the same;
//! caches (zone fills, connectivity, the resolved rule matrix) key on these hashes.
//!
//! Hashing streams the bytes into SHA-256 without building the whole form in memory.

use crate::geom::{Angle, BBox, Contour, OnTrack, Path, Seg, P};
use crate::layer::{LayerId, LayerSet};
use crate::poly::PolySet;
use crate::shape::Shape;
use makepad_core_util::sha256::Sha256;

/// A content hash: the first 16 bytes of SHA-256 over the canonical form.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 16]);

impl ContentHash {
    /// Lower-case hex, the form heads are written in (`generation:hash`).
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Where canonical bytes go: kept (tests, binary encodings) or hashed.
pub struct Writer {
    buf: Vec<u8>,
    sha: Option<Sha256>,
}

const FLUSH: usize = 1 << 16;

impl Writer {
    /// A writer that keeps the bytes (`into_bytes`).
    pub fn bytes() -> Writer {
        Writer { buf: Vec::new(), sha: None }
    }
    /// A writer that hashes as it goes (`finish`).
    pub fn hasher() -> Writer {
        Writer { buf: Vec::with_capacity(FLUSH), sha: Some(Sha256::new()) }
    }
    #[inline]
    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= FLUSH {
            if let Some(sha) = &mut self.sha {
                sha.update(&self.buf);
                self.buf.clear();
            }
        }
    }
    #[inline]
    pub fn u8(&mut self, v: u8) {
        self.raw(&[v]);
    }
    #[inline]
    pub fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
    #[inline]
    pub fn u16(&mut self, v: u16) {
        self.raw(&v.to_le_bytes());
    }
    #[inline]
    pub fn u32(&mut self, v: u32) {
        self.raw(&v.to_le_bytes());
    }
    #[inline]
    pub fn i32(&mut self, v: i32) {
        self.raw(&v.to_le_bytes());
    }
    #[inline]
    pub fn u64(&mut self, v: u64) {
        self.raw(&v.to_le_bytes());
    }
    #[inline]
    pub fn u128(&mut self, v: u128) {
        self.raw(&v.to_le_bytes());
    }
    /// A float with canonical zero. NaN is written as a fixed quiet NaN (models never hold one;
    /// this keeps the writer total).
    #[inline]
    pub fn f64(&mut self, v: f64) {
        let bits = if v == 0.0 { 0 } else if v.is_nan() { 0x7ff8_0000_0000_0000 } else { v.to_bits() };
        self.u64(bits);
    }
    #[inline]
    pub fn count(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("canonical count over u32"));
    }
    pub fn str(&mut self, s: &str) {
        self.count(s.len());
        self.raw(s.as_bytes());
    }
    /// A tag byte opening a variant or an optional value.
    #[inline]
    pub fn tag(&mut self, t: u8) {
        self.u8(t);
    }
    pub fn into_bytes(self) -> Vec<u8> {
        assert!(self.sha.is_none(), "a hashing writer keeps no bytes");
        self.buf
    }
    pub fn finish(mut self) -> ContentHash {
        let mut sha = self.sha.take().expect("finish on a hashing writer");
        sha.update(&self.buf);
        let full = sha.finish();
        let mut h = [0u8; 16];
        h.copy_from_slice(&full[..16]);
        ContentHash(h)
    }
}

/// A value with a canonical form.
pub trait Canon {
    fn canon(&self, w: &mut Writer);
    /// The content hash of the value.
    fn content_hash(&self) -> ContentHash {
        let mut w = Writer::hasher();
        self.canon(&mut w);
        w.finish()
    }
    /// The canonical bytes (tests, binary encodings).
    fn canon_bytes(&self) -> Vec<u8> {
        let mut w = Writer::bytes();
        self.canon(&mut w);
        w.into_bytes()
    }
}

/// `Canon` for a struct, writing the listed fields in order: `canon_struct!(Net { id, name, class });`.
/// List every content field; leave out generation counters, caches and spans (and say so at the
/// type).
#[macro_export]
macro_rules! canon_struct {
    ($t:ty { $($f:tt),* $(,)? }) => {
        impl $crate::canon::Canon for $t {
            fn canon(&self, w: &mut $crate::canon::Writer) {
                $( $crate::canon::Canon::canon(&self.$f, w); )*
                let _ = w;
            }
        }
    };
}

/// `Canon` for an enum: a tag byte per variant, then its fields in order. Tags are written
/// explicitly so adding a variant never renumbers the others:
/// `canon_enum!(Area { 0 => Board, 1 => Poly(p) });`,
/// `canon_enum!(Anchor { 0 => Free, 1 => Pad { placed, pad }, 2 => Via(v) });`.
#[macro_export]
macro_rules! canon_enum {
    ($t:ident { $( $tag:literal => $v:ident $( ( $($a:ident),* ) )? $( { $($n:ident),* } )? ),* $(,)? }) => {
        impl $crate::canon::Canon for $t {
            fn canon(&self, w: &mut $crate::canon::Writer) {
                match self {
                    $( $t::$v $( ( $($a),* ) )? $( { $($n),* } )? => {
                        w.tag($tag);
                        $( $( $crate::canon::Canon::canon($a, w); )* )?
                        $( $( $crate::canon::Canon::canon($n, w); )* )?
                    } )*
                }
            }
        }
    };
}

/// `Canon` for a fieldless `Copy` enum, by its declared discriminant (append new variants at the
/// end): `canon_tags!(PinKind, PinDir);`.
#[macro_export]
macro_rules! canon_tags {
    ($($t:ty),* $(,)?) => {
        $( impl $crate::canon::Canon for $t {
            fn canon(&self, w: &mut $crate::canon::Writer) {
                w.tag(*self as u8)
            }
        } )*
    };
}

impl Canon for bool {
    fn canon(&self, w: &mut Writer) {
        w.bool(*self)
    }
}
impl Canon for u8 {
    fn canon(&self, w: &mut Writer) {
        w.u8(*self)
    }
}
impl Canon for u16 {
    fn canon(&self, w: &mut Writer) {
        w.u16(*self)
    }
}
impl Canon for u32 {
    fn canon(&self, w: &mut Writer) {
        w.u32(*self)
    }
}
impl Canon for i32 {
    fn canon(&self, w: &mut Writer) {
        w.i32(*self)
    }
}
impl Canon for u64 {
    fn canon(&self, w: &mut Writer) {
        w.u64(*self)
    }
}
impl Canon for i64 {
    fn canon(&self, w: &mut Writer) {
        w.u64(*self as u64)
    }
}
impl Canon for f64 {
    fn canon(&self, w: &mut Writer) {
        w.f64(*self)
    }
}
impl Canon for f32 {
    fn canon(&self, w: &mut Writer) {
        w.f64(*self as f64)
    }
}
impl Canon for String {
    fn canon(&self, w: &mut Writer) {
        w.str(self)
    }
}
impl Canon for str {
    fn canon(&self, w: &mut Writer) {
        w.str(self)
    }
}
impl<T: Canon> Canon for Option<T> {
    fn canon(&self, w: &mut Writer) {
        match self {
            None => w.tag(0),
            Some(v) => {
                w.tag(1);
                v.canon(w)
            }
        }
    }
}
impl<T: Canon> Canon for [T] {
    fn canon(&self, w: &mut Writer) {
        w.count(self.len());
        self.iter().for_each(|v| v.canon(w));
    }
}
impl<T: Canon> Canon for Vec<T> {
    fn canon(&self, w: &mut Writer) {
        self.as_slice().canon(w)
    }
}
impl<A: Canon, B: Canon> Canon for (A, B) {
    fn canon(&self, w: &mut Writer) {
        self.0.canon(w);
        self.1.canon(w);
    }
}
impl<A: Canon, B: Canon, C: Canon> Canon for (A, B, C) {
    fn canon(&self, w: &mut Writer) {
        self.0.canon(w);
        self.1.canon(w);
        self.2.canon(w);
    }
}
/// A fixed-size array writes its elements without a count.
impl<T: Canon, const N: usize> Canon for [T; N] {
    fn canon(&self, w: &mut Writer) {
        self.iter().for_each(|v| v.canon(w));
    }
}
impl<T: Canon + ?Sized> Canon for Box<T> {
    fn canon(&self, w: &mut Writer) {
        (**self).canon(w)
    }
}
impl<T: Canon> Canon for std::ops::Range<T> {
    fn canon(&self, w: &mut Writer) {
        self.start.canon(w);
        self.end.canon(w);
    }
}
impl<T: Canon + ?Sized> Canon for std::sync::Arc<T> {
    fn canon(&self, w: &mut Writer) {
        (**self).canon(w)
    }
}
impl Canon for ContentHash {
    fn canon(&self, w: &mut Writer) {
        w.raw(&self.0)
    }
}

impl Canon for P {
    fn canon(&self, w: &mut Writer) {
        w.i32(self.x);
        w.i32(self.y);
    }
}
impl Canon for Angle {
    fn canon(&self, w: &mut Writer) {
        w.i32(self.udeg())
    }
}
impl Canon for BBox {
    fn canon(&self, w: &mut Writer) {
        self.min.canon(w);
        self.max.canon(w);
    }
}
impl Canon for LayerId {
    fn canon(&self, w: &mut Writer) {
        w.u8(self.0)
    }
}
impl Canon for LayerSet {
    fn canon(&self, w: &mut Writer) {
        self.0.iter().for_each(|v| w.u64(*v))
    }
}
impl Canon for Seg {
    fn canon(&self, w: &mut Writer) {
        match self {
            Seg::Line(p) => {
                w.tag(0);
                p.canon(w)
            }
            Seg::Arc { mid, end } => {
                w.tag(1);
                mid.canon(w);
                end.canon(w)
            }
        }
    }
}
canon_struct!(OnTrack { seg, frac });
impl Canon for Path {
    fn canon(&self, w: &mut Writer) {
        self.start.canon(w);
        self.segs.canon(w);
    }
}
impl Canon for Contour {
    fn canon(&self, w: &mut Writer) {
        self.path.canon(w)
    }
}
impl Canon for PolySet {
    /// The normal form makes this canonical: equal regions built alike write equal bytes.
    fn canon(&self, w: &mut Writer) {
        w.count(self.polygons().len());
        for p in self.polygons() {
            p.outer.canon(w);
            p.holes.canon(w);
        }
    }
}
impl Canon for Shape {
    fn canon(&self, w: &mut Writer) {
        match self {
            Shape::Circle { c, r } => {
                w.tag(0);
                c.canon(w);
                w.i32(*r)
            }
            Shape::Segment { a, b, w: wd } => {
                w.tag(1);
                a.canon(w);
                b.canon(w);
                w.i32(*wd)
            }
            Shape::Arc { a, mid, b, w: wd } => {
                w.tag(2);
                a.canon(w);
                mid.canon(w);
                b.canon(w);
                w.i32(*wd)
            }
            Shape::Rect { c, half, rot, corner } => {
                w.tag(3);
                c.canon(w);
                half.canon(w);
                rot.canon(w);
                w.i32(*corner)
            }
            Shape::Oval { c, half, rot } => {
                w.tag(4);
                c.canon(w);
                half.canon(w);
                rot.canon(w)
            }
            Shape::Poly(s) => {
                w.tag(5);
                s.canon(w)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::mm;

    /// Canonical bytes and hashes: zeros, sharing, order sensitivity, streamed equals buffered.
    #[test]
    fn canonical_form_and_hash() {
        assert_eq!(0.0f64.canon_bytes(), (-0.0f64).canon_bytes(), "one zero");
        let s = Shape::Segment { a: P::mm(0.0, 0.0), b: P::mm(1.0, 0.0), w: mm(0.2) };
        let shared = std::sync::Arc::new(s.clone());
        assert_eq!(shared.content_hash(), s.content_hash(), "Arc sharing does not change the hash");
        let v1 = vec![s.clone(), Shape::Circle { c: P::ZERO, r: 5 }];
        let v2 = vec![Shape::Circle { c: P::ZERO, r: 5 }, s.clone()];
        assert_ne!(v1.content_hash(), v2.content_hash(), "order is content");
        // a long value crosses the flush size: streamed hash equals the hash of the kept bytes
        let long: Vec<P> = (0..50_000).map(|i| P::new(i, -i)).collect();
        let bytes = long.canon_bytes();
        assert!(bytes.len() > FLUSH, "test value crosses a flush");
        let mut sha = Sha256::new();
        sha.update(&bytes);
        assert_eq!(long.content_hash().0[..], sha.finish()[..16], "streamed hash equals buffered hash");
        assert_eq!(long.content_hash().hex().len(), 32, "hex form");
    }
}
