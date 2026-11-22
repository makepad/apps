//! FIPS 180-4 SHA-1, used only by the WebSocket handshake.

pub fn digest(input: &[u8]) -> [u8; 20] {
    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let mut blocks = input.chunks_exact(64);
    for block in &mut blocks { compress(&mut state, block); }
    let tail = blocks.remainder();
    let mut padding = [0u8; 128];
    padding[..tail.len()].copy_from_slice(tail);
    padding[tail.len()] = 0x80;
    let len = if tail.len() < 56 { 64 } else { 128 };
    padding[len - 8..len].copy_from_slice(&(input.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in padding[..len].chunks_exact(64) { compress(&mut state, block); }
    let mut out = [0; 20];
    for (dst, word) in out.chunks_exact_mut(4).zip(state) {
        dst.copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn compress(state: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for (dst, bytes) in w[..16].iter_mut().zip(block.chunks_exact(4)) {
        *dst = u32::from_be_bytes(bytes.try_into().unwrap());
    }
    for i in 16..80 { w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1); }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (i, word) in w.into_iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5a827999u32),
            20..=39 => (b ^ c ^ d, 0x6ed9eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
            _ => (b ^ c ^ d, 0xca62c1d6),
        };
        let next = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (dst, word) in state.iter_mut().zip([a, b, c, d, e]) { *dst = dst.wrapping_add(word); }
}
