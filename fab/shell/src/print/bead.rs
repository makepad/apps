//! A preview bead's instance: [`PREVIEW_SLOTS`](super::PREVIEW_SLOTS) floats, three vec4s.
//!
//! - `seg_a`: the start (x, y, z, printer mm) and the start time (s).
//! - `seg_b`: the end (x, y, z) and two f16s: the bead's duration and the colour mode's value.
//! - `seg_k`: four packed lanes: the width and the height (two f16s, negative at a free start or
//!   end); the layer + 1 and the tag's low 16 bits (two u16s); the role with its stack's steps
//!   (24 bits) and the kind with its mode (8 bits); the filament, the start's joint, the end's
//!   joint and the tag's high byte (four bytes).
//!
//! Positions and start times are the f32s they are; the end time is the start plus the
//! duration, as the shader takes it. A packed lane holds its bits in an f32 slot (the shader's
//! `unpack2f16` and `unpack4u8` read them): nothing does arithmetic on one, [`Bead::of`] reads
//! them and [`Bead::pack`] writes them.

use makepad_math::{f16_bits_to_f32, f32_to_f16_bits};

/// Floats per preview instance.
pub const SLOTS: usize = 12;

/// The packed tag is the tag + this: an overlay's tags are small negatives
/// ([`super::trees::PICKED_TAG`], the issue frame's).
const TAG_BIAS: f32 = 16.0;

/// A preview bead, unpacked: each field as the number it stands for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bead {
    pub a: [f32; 3],
    pub b: [f32; 3],
    /// Its start (s from the print's start).
    pub t0: f32,
    /// How long it takes (s): it ends at [`Bead::t1`].
    pub dur: f32,
    /// Its width and height (mm): negative where its start (width) or end (height) is free.
    pub width: f32,
    pub height: f32,
    /// The colour mode's value: the commanded speed (mm/s).
    pub value: f32,
    /// Its layer, -1 for none.
    pub layer: f32,
    /// Its role, + [`super::stack::ROLE_STRIDE`] * its stack's steps (an overlay's severity).
    pub role: f32,
    /// nozzle + 2 * (1 + part) ([`super::bead_object`]), or an overlay's tag.
    pub tag: f32,
    /// Its filament + 256 * (the start's joint + 256 * the end's) ([`super::join_beads`]).
    pub filament: f32,
    /// Its kind + [`super::kind::MODE_STRIDE`] * its move's mode.
    pub kind: f32,
}

fn halves(lo: f32, hi: f32) -> f32 {
    f32::from_bits(f32_to_f16_bits(lo) as u32 | (f32_to_f16_bits(hi) as u32) << 16)
}

fn halves_of(lane: f32) -> (f32, f32) {
    let bits = lane.to_bits();
    (f16_bits_to_f32(bits as u16), f16_bits_to_f32((bits >> 16) as u16))
}

/// A whole number of `bits` bits, as a lane holds it.
fn whole(v: f32, bits: u32, what: &str) -> u32 {
    debug_assert!(v >= 0.0 && v.fract() == 0.0 && v < (1u64 << bits) as f32, "a bead's {what} out of its {bits} bits: {v}");
    v as u32
}

impl Bead {
    /// Its end time, as the shader takes it.
    pub fn t1(&self) -> f32 {
        self.t0 + self.dur
    }

    /// Its instance's floats.
    pub fn pack(&self) -> [f32; SLOTS] {
        let layer = whole(self.layer + 1.0, 16, "layer");
        let tag = whole(self.tag + TAG_BIAS, 24, "tag");
        let role = whole(self.role, 24, "role");
        let kind = whole(self.kind, 8, "kind");
        let filament = whole(self.filament, 24, "filament");
        let [a, b] = [self.a, self.b];
        [
            a[0], a[1], a[2], self.t0,
            b[0], b[1], b[2], halves(self.dur, self.value),
            halves(self.width, self.height),
            f32::from_bits(layer | (tag & 0xffff) << 16),
            f32::from_bits(role | kind << 24),
            f32::from_bits(filament | (tag >> 16) << 24),
        ]
    }

    /// Writes it over the instance `slots`.
    pub fn write(&self, slots: &mut [f32]) {
        slots[..SLOTS].copy_from_slice(&self.pack());
    }

    /// The bead an instance's floats hold.
    pub fn of(s: &[f32]) -> Bead {
        let (dur, value) = halves_of(s[7]);
        let (width, height) = halves_of(s[8]);
        let [lt, rk, fj] = [s[9].to_bits(), s[10].to_bits(), s[11].to_bits()];
        Bead {
            a: [s[0], s[1], s[2]],
            b: [s[4], s[5], s[6]],
            t0: s[3],
            dur,
            width,
            height,
            value,
            layer: (lt & 0xffff) as f32 - 1.0,
            role: (rk & 0xff_ffff) as f32,
            tag: ((lt >> 16) | (fj >> 24) << 16) as f32 - TAG_BIAS,
            filament: (fj & 0xff_ffff) as f32,
            kind: (rk >> 24) as f32,
        }
    }

    /// Changes the bead the instance `slots` hold.
    pub fn change(slots: &mut [f32], f: impl FnOnce(&mut Bead)) {
        let mut bead = Bead::of(slots);
        f(&mut bead);
        bead.write(slots);
    }
}

/// Sets one field of an instance, for loops over many: its duration ([`Bead::dur`]).
pub fn set_dur(s: &mut [f32], dur: f32) {
    s[7] = f32::from_bits(s[7].to_bits() & 0xffff_0000 | f32_to_f16_bits(dur) as u32);
}

/// Its layer ([`Bead::layer`]).
pub fn set_layer(s: &mut [f32], layer: f32) {
    s[9] = f32::from_bits(s[9].to_bits() & 0xffff_0000 | whole(layer + 1.0, 16, "layer"));
}

/// Sets its role with its stack's steps ([`Bead::role`]).
pub fn set_role(s: &mut [f32], role: f32) {
    s[10] = f32::from_bits(s[10].to_bits() & 0xff00_0000 | whole(role, 24, "role"));
}

/// Its filament with its joints ([`Bead::filament`]).
pub fn set_filament(s: &mut [f32], filament: f32) {
    s[11] = f32::from_bits(s[11].to_bits() & 0xff00_0000 | whole(filament, 24, "filament"));
}

/// The bead `look` packs ([`Bead::pack`]) from `a` at `t0` to `b` over `dur`, its start free (its
/// width negative) if `free_start`: one f16 to pack where [`Bead::pack`] has four, for the moves
/// that share a look.
pub fn placed(look: &[f32; SLOTS], a: [f32; 3], b: [f32; 3], t0: f32, dur: f32, free_start: bool) -> [f32; SLOTS] {
    let mut s = *look;
    s[..3].copy_from_slice(&a);
    s[3] = t0;
    s[4..7].copy_from_slice(&b);
    set_dur(&mut s, dur);
    if free_start {
        s[8] = f32::from_bits(s[8].to_bits() | 0x8000);
    }
    s
}

/// Its end free: its height negative ([`Bead::height`]).
pub fn free_end(s: &mut [f32]) {
    s[8] = f32::from_bits(s[8].to_bits() | 0x8000_0000);
}

/// One field of an instance, for loops over many: its start time.
pub fn t0(s: &[f32]) -> f32 {
    s[3]
}

/// Its end time ([`Bead::t1`]).
pub fn t1(s: &[f32]) -> f32 {
    s[3] + halves_of(s[7]).0
}

/// The colour mode's value: the commanded speed ([`Bead::value`]).
pub fn value(s: &[f32]) -> f32 {
    halves_of(s[7]).1
}

/// Its layer (-1: none).
pub fn layer(s: &[f32]) -> f32 {
    (s[9].to_bits() & 0xffff) as f32 - 1.0
}

/// Its tag ([`Bead::tag`]).
pub fn tag(s: &[f32]) -> f32 {
    ((s[9].to_bits() >> 16) | (s[11].to_bits() >> 24) << 16) as f32 - TAG_BIAS
}

/// Its role with its stack's steps.
pub fn role(s: &[f32]) -> f32 {
    (s[10].to_bits() & 0xff_ffff) as f32
}

/// Whether `next` would draw as the bead `s` holds but for its ends, its times and its free
/// ends (the signs of its width and height): the same section, speed, layer, role, tag,
/// filament and kind, lane by lane.
pub fn same_look(s: &[f32], next: &[f32; SLOTS]) -> bool {
    const SIGNS: u32 = 0x8000_8000;
    s[7].to_bits() >> 16 == next[7].to_bits() >> 16
        && s[8].to_bits() & !SIGNS == next[8].to_bits() & !SIGNS
        && (9..SLOTS).all(|k| s[k].to_bits() == next[k].to_bits())
}

/// Its filament with its joints ([`Bead::filament`]).
pub fn filament(s: &[f32]) -> f32 {
    (s[11].to_bits() & 0xff_ffff) as f32
}

/// Its kind with its mode ([`Bead::kind`]).
pub fn kind(s: &[f32]) -> f32 {
    (s[10].to_bits() >> 24) as f32
}

/// Whether its start joins the bead before: its width above zero ([`Bead::width`]), read off its bits.
pub fn start_joins(s: &[f32]) -> bool {
    above_zero(s[8].to_bits() as u16)
}

/// Whether its end joins the bead after: its height above zero ([`Bead::height`]).
pub fn end_joins(s: &[f32]) -> bool {
    above_zero((s[8].to_bits() >> 16) as u16)
}

/// An f16's bits above zero: no sign, not zero, not a NaN.
fn above_zero(h: u16) -> bool {
    h != 0 && h <= 0x7c00
}

/// Its width and height ([`Bead::width`]).
pub fn section(s: &[f32]) -> (f32, f32) {
    halves_of(s[8])
}

