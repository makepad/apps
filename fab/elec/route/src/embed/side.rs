//! `keep` is the side of the reference c lies on, modulo 2.
//!
//! A self-crossing trail is taken by its winding modulo 2: the embedder keeps sides, not
//! turns, so a full loop drawn round a part pulls tight to a side pass. The cost is O(p)
//! for p reference points; callers compute it once per obstacle part and sample.

use fab_elec_core::geom::orient;
use fab_elec_core::P;

/// Which side of the path, looking along it from start to end, an obstacle stays on.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Keep {
    Left,
    Right,
}

impl Keep {
    pub fn flip(self) -> Keep {
        match self {
            Keep::Left => Keep::Right,
            Keep::Right => Keep::Left,
        }
    }
}

/// The side the reference passes `c` on: `None` when the head is empty (s == e).
pub fn keep(reference: &[P], c: P) -> Option<Keep> {
    if reference.len() < 2 {
        return None;
    }
    let s = reference[0];
    let e = reference[reference.len() - 1];
    if s == e {
        return None;
    }
    let chord = orient(s, e, c);
    if chord == 0 {
        let mut nearest = distance2(s, reference[1], c);
        let mut side = orient(s, reference[1], c);
        for pair in reference.windows(2).skip(1) {
            let distance = distance2(pair[0], pair[1], c);
            if distance.less_than(&nearest) {
                nearest = distance;
                side = orient(pair[0], pair[1], c);
            }
        }
        return Some(if side < 0 { Keep::Right } else { Keep::Left });
    }

    let mut inside = false;
    for (a, b) in reference.windows(2).map(|pair| (pair[0], pair[1])).chain(std::iter::once((e, s))) {
        // Half-open y interval, and strictly right of c: an orientation of zero never counts.
        if a.y.min(b.y) <= c.y && c.y < a.y.max(b.y) {
            let o = orient(a, b, c);
            if (b.y > a.y && o > 0) || (b.y < a.y && o < 0) {
                inside = !inside;
            }
        }
    }
    let side = if chord > 0 { Keep::Left } else { Keep::Right };
    Some(if inside { side.flip() } else { side })
}

/// Squared distance as num[0] * num[1] / den; keeping the factors avoids squaring overflow.
struct Distance {
    num: [i128; 2],
    den: i128,
}

impl Distance {
    fn less_than(&self, other: &Distance) -> bool {
        product([self.num[0], self.num[1], other.den]) < product([other.num[0], other.num[1], self.den])
    }
}

fn distance2(a: P, b: P, c: P) -> Distance {
    let dx = b.x as i128 - a.x as i128;
    let dy = b.y as i128 - a.y as i128;
    let px = c.x as i128 - a.x as i128;
    let py = c.y as i128 - a.y as i128;
    let len2 = dx * dx + dy * dy;
    let dot = px * dx + py * dy;
    if dot <= 0 {
        return Distance { num: [c.dist2(a), 1], den: 1 };
    }
    if dot >= len2 {
        return Distance { num: [c.dist2(b), 1], den: 1 };
    }
    let cross = orient(a, b, c).abs();
    Distance { num: [cross, cross], den: len2 }
}

/// Exact cross-multiplication, most significant limb first for lexicographic comparison.
/// Each factor fits 65 bits, but their product can exceed i128. Seven base-2^32 limbs hold
/// all 195 bits; every multiply and carry is in i128 (at most three 32-by-32-bit products).
fn product(factors: [i128; 3]) -> [i128; 7] {
    const BASE: i128 = 1 << 32;
    let mut out = [0; 7];
    out[0] = 1;
    for factor in factors {
        let digits = [factor % BASE, (factor / BASE) % BASE, factor / (BASE * BASE)];
        let mut next = [0; 7];
        for i in 0..7 {
            for j in 0..3.min(7 - i) {
                next[i + j] += out[i] * digits[j];
            }
        }
        for i in 0..6 {
            next[i + 1] += next[i] / BASE;
            next[i] %= BASE;
        }
        out = next;
    }
    out.reverse();
    out
}
