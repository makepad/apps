//! RFC 1321, section 3: incremental MD5 for flash verification.
//! Ported from this workspace's `libs/audio_decode/src/flac/metadata.rs`
//! (`Md5::new`, `update`, `finish` and its compression function; no PCM adapter).

pub struct Md5 {
    state: [u32; 4],
    buffer: [u8; 64],
    filled: usize,
    bit_len: u64,
}

impl Default for Md5 {
    fn default() -> Self { Self::new() }
}

impl Md5 {
    pub fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476],
            buffer: [0; 64],
            filled: 0,
            bit_len: 0,
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut data = data;
        self.bit_len = self.bit_len.wrapping_add((data.len() as u64).saturating_mul(8));
        if self.filled > 0 {
            let need = 64 - self.filled;
            if data.len() < need {
                self.buffer[self.filled..self.filled + data.len()].copy_from_slice(data);
                self.filled += data.len();
                return;
            }
            self.buffer[self.filled..].copy_from_slice(&data[..need]);
            let block = self.buffer;
            md5_block(&mut self.state, &block);
            self.filled = 0;
            data = &data[need..];
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            md5_block(&mut self.state, &block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.filled = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bit_len = self.bit_len;
        self.update(&[0x80]);
        if self.filled > 56 {
            let zeros = 64 - self.filled;
            self.update(&[0u8; 64][..zeros]);
        }
        let zeros = 56 - self.filled;
        if zeros > 0 {
            self.update(&[0u8; 64][..zeros]);
        }
        self.update(&bit_len.to_le_bytes());
        let mut out = [0u8; 16];
        for (i, &w) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// MD5 of exactly these bytes, without flash packet or sector padding.
pub fn digest(data: &[u8]) -> [u8; 16] {
    let mut md5 = Md5::new();
    md5.update(data);
    md5.finish()
}

/// The usual 32-character lowercase MD5 representation.
pub fn hex(digest: &[u8; 16]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn md5_block(state: &mut [u32; 4], block: &[u8; 64]) {
    let mut w = [0u32; 16];
    for i in 0..16 {
        w[i] = u32::from_le_bytes([
            block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3],
        ]);
    }
    let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
    for i in 0..64 {
        let (f, g) = match i {
            0..=15 => ((b & c) | ((!b) & d), i),
            16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
            32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(w[g]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(f.rotate_left(S[i]));
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K: [u32; 64] = [
    0xd76a_a478, 0xe8c7_b756, 0x2420_70db, 0xc1bd_ceee, 0xf57c_0faf, 0x4787_c62a, 0xa830_4613,
    0xfd46_9501, 0x6980_98d8, 0x8b44_f7af, 0xffff_5bb1, 0x895c_d7be, 0x6b90_1122, 0xfd98_7193,
    0xa679_438e, 0x49b4_0821, 0xf61e_2562, 0xc040_b340, 0x265e_5a51, 0xe9b6_c7aa, 0xd62f_105d,
    0x0244_1453, 0xd8a1_e681, 0xe7d3_fbc8, 0x21e1_cde6, 0xc337_07d6, 0xf4d5_0d87, 0x455a_14ed,
    0xa9e3_e905, 0xfcef_a3f8, 0x676f_02d9, 0x8d2a_4c8a, 0xfffa_3942, 0x8771_f681, 0x6d9d_6122,
    0xfde5_380c, 0xa4be_ea44, 0x4bde_cfa9, 0xf6bb_4b60, 0xbebf_bc70, 0x289b_7ec6, 0xeaa1_27fa,
    0xd4ef_3085, 0x0488_1d05, 0xd9d4_d039, 0xe6db_99e5, 0x1fa2_7cf8, 0xc4ac_5665, 0xf429_2244,
    0x432a_ff97, 0xab94_23a7, 0xfc93_a039, 0x655b_59c3, 0x8f0c_cc92, 0xffef_f47d, 0x8584_5dd1,
    0x6fa8_7e4f, 0xfe2c_e6e0, 0xa301_4314, 0x4e08_11a1, 0xf753_7e82, 0xbd3a_f235, 0x2ad7_d2bb,
    0xeb86_d391,
];
