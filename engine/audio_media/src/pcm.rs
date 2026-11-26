//! The one shape decoded audio takes on its way through the engine.

/// A fully decoded, immutable PCM clip (interleaved stereo i16).
pub struct TrackPcm {
    pub frames: Vec<[i16; 2]>,
    pub sample_rate: u32,
}

impl TrackPcm {
    pub fn seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frames.len() as f64 / self.sample_rate as f64
    }

    #[inline]
    pub fn frame_f32(&self, index: usize) -> [f32; 2] {
        match self.frames.get(index) {
            Some(frame) => [frame[0] as f32 / 32768.0, frame[1] as f32 / 32768.0],
            None => [0.0, 0.0],
        }
    }
}
