//! The take's file: mono 16-bit PCM WAV at the take's own rate, and back.

/// Mono `pcm` (-1..1) as a 16-bit PCM WAV file.
pub fn encode_wav_mono16(pcm: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in pcm {
        out.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    out
}

/// A 16-bit PCM WAV file as mono samples (channels averaged) and its rate;
/// `None` for any other format.
pub fn decode_wav16(bytes: &[u8]) -> Option<(Vec<f32>, u32)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (mut at, mut format) = (12usize, None);
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32_at(at + 4)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            // (tag, channels, rate, bits)
            format = Some((u16_at(body)?, u16_at(body + 2)?, u32_at(body + 4)?, u16_at(body + 14)?));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = format?;
            if tag != 1 || bits != 16 || channels == 0 || rate == 0 {
                return None;
            }
            let data = bytes.get(body..body + len.min(bytes.len() - body))?;
            let frame = channels as usize * 2;
            let pcm = data.chunks_exact(frame).map(|frame| {
                let sum: f32 = frame.chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0).sum();
                sum / channels as f32
            }).collect();
            return Some((pcm, rate));
        }
        at = body + len + (len & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let pcm = [0.0, 0.5, -0.5, 1.0, -1.0];
        let wav = encode_wav_mono16(&pcm, 24_000);
        assert_eq!(wav.len(), 44 + 10);
        let (back, rate) = decode_wav16(&wav).unwrap();
        assert_eq!(rate, 24_000);
        assert!(back.iter().zip(pcm).all(|(a, b)| (a - b).abs() < 1.0 / 16_000.0), "{back:?}");
        assert!(decode_wav16(b"RIFF0000WAVE").is_none());
    }
}
