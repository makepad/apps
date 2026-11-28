use makepad_call::driver::codec::StreamFormat;
use makepad_town_core::{key::PublicKey, share::hello::MediaCaps};
use makepad_video::{StreamVideoCodec, VideoStreamDecoder};
use makepad_widgets::{log, Cx, makepad_platform::thread::{Class, TaskHandle}};

/// The shared app uses the platform encoder on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EncoderFormats {
    pub hevc: bool,
    pub full_chroma: bool,
}

impl EncoderFormats {
    pub(super) fn local() -> Self {
        static FORMATS: std::sync::OnceLock<EncoderFormats> = std::sync::OnceLock::new();
        *FORMATS.get_or_init(|| Self {
            hevc: makepad_video::VideoStreamEncoder::caps().hevc,
            // Enable this when Town draws full-chroma pictures.
            full_chroma: false,
        })
    }

    pub(super) fn makes(self, f: StreamFormat) -> bool {
        (!f.hevc || self.hevc) && (!f.full_chroma || self.full_chroma)
    }

    pub(super) fn available(self, lost: &[StreamFormat]) -> impl Iterator<Item = StreamFormat> + '_ {
        StreamFormat::ORDER.into_iter().filter(move |&f| self.makes(f) && !lost.contains(&f))
    }
}

pub(super) fn decode_caps(h264: bool, hevc: bool) -> MediaCaps {
    let mut bits = 0;
    if h264 { bits |= MediaCaps::H264_DECODE; }
    if hevc { bits |= MediaCaps::HEVC_DECODE; }
    MediaCaps(bits)
}

#[derive(Default)]
pub(super) enum DecodeProbe {
    #[default]
    New,
    Running(TaskHandle<MediaCaps>),
    Done,
}

impl DecodeProbe {
    pub(super) fn start(&mut self, cx: &Cx) {
        if !matches!(self, Self::New) { return }
        *self = match cx.task_pool().submit_named(Class::Background, "Town share decoders", || {
            static CAPS: std::sync::OnceLock<MediaCaps> = std::sync::OnceLock::new();
            *CAPS.get_or_init(|| decode_caps(VideoStreamDecoder::decodes(StreamVideoCodec::H264),
                VideoStreamDecoder::decodes(StreamVideoCodec::Hevc)))
        }) {
            Ok(task) => Self::Running(task),
            Err(err) => {
                log!("Share: couldn't schedule the decoder probe: {err}");
                Self::Done
            }
        };
    }

    pub(super) fn try_take(&mut self) -> Option<MediaCaps> {
        let Self::Running(task) = self else { return None };
        let result = task.try_take()?;
        *self = Self::Done;
        match result {
            Ok(caps) => Some(caps),
            Err(err) => {
                log!("Share: decoder probe stopped: {err}");
                None
            }
        }
    }
}

pub(super) fn decodes(caps: MediaCaps, f: StreamFormat) -> bool {
    match f {
        StreamFormat::H264 => true, // A watcher from before View carried caps.
        StreamFormat::H264_FULL => caps.supports(MediaCaps::H264_444_DECODE),
        StreamFormat::HEVC => caps.supports(MediaCaps::HEVC_DECODE),
        StreamFormat::HEVC_FULL => caps.supports(MediaCaps::HEVC_444_DECODE),
    }
}

pub(super) fn failed_decode_bits(f: StreamFormat) -> u32 {
    match f {
        StreamFormat::H264 => MediaCaps::H264_DECODE | MediaCaps::H264_444_DECODE,
        StreamFormat::H264_FULL => MediaCaps::H264_444_DECODE,
        StreamFormat::HEVC => MediaCaps::HEVC_DECODE | MediaCaps::HEVC_444_DECODE,
        StreamFormat::HEVC_FULL => MediaCaps::HEVC_444_DECODE,
    }
}

pub(super) fn share_format(encoder: EncoderFormats, lost: &[StreamFormat],
    watchers: impl IntoIterator<Item = (PublicKey, MediaCaps)>) -> (StreamFormat, Vec<PublicKey>) {
    let mut common = MediaCaps(u32::MAX);
    let mut holders = Vec::new();
    let best = encoder.available(lost).next().unwrap_or(StreamFormat::H264);
    for (peer, caps) in watchers {
        common.0 &= caps.0;
        if !decodes(caps, best) { holders.push(peer); }
    }
    let format = encoder.available(lost).find(|&f| decodes(common, f)).unwrap_or(StreamFormat::H264);
    (format, holders)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_caps_advertise_each_available_decoder() {
        let h264 = MediaCaps::H264_DECODE;
        let hevc = MediaCaps::HEVC_DECODE;
        for (has_h264, has_hevc, bits) in [
            (true, true, h264 | hevc), (true, false, h264), (false, true, hevc), (false, false, 0),
        ] {
            let caps = decode_caps(has_h264, has_hevc);
            assert_eq!(caps, MediaCaps(bits), "the View reflects H.264={has_h264} and H.265={has_hevc}");
            assert!(!caps.supports(MediaCaps::H264_444_DECODE | MediaCaps::HEVC_444_DECODE),
                "neither decoder advertises 4:4:4 for H.264={has_h264} and H.265={has_hevc}");
        }
    }

    #[test]
    fn the_share_takes_the_best_format_every_watcher_decodes() {
        use StreamFormat as F;
        let (a, b) = ([1; 65], [2; 65]);
        let hevc = MediaCaps(MediaCaps::HEVC_DECODE);
        let h264_full = MediaCaps(MediaCaps::H264_444_DECODE);
        let all = MediaCaps(MediaCaps::HEVC_DECODE | MediaCaps::HEVC_444_DECODE |
            MediaCaps::H264_DECODE | MediaCaps::H264_444_DECODE | MediaCaps::GPU_DECODE);
        let full = EncoderFormats { hevc: true, full_chroma: true };
        let video = EncoderFormats { full_chroma: false, ..full };
        let base = EncoderFormats { hevc: false, ..video };
        for (why, encoder, lost, watchers, format, holders) in [
            ("an unwatched share chooses the encoder's best format", full, vec![], vec![], F::HEVC_FULL, vec![]),
            ("full-chroma watchers get the encoder's best format", full, vec![], vec![(a, all), (b, all)], F::HEVC_FULL, vec![]),
            ("two HEVC watchers get HEVC", video, vec![], vec![(a, hevc), (b, hevc)], F::HEVC, vec![]),
            ("both HEVC watchers rule out full chroma", full, vec![], vec![(a, hevc), (b, hevc)], F::HEVC, vec![a,b]),
            ("only the older watcher rules out HEVC", video, vec![], vec![(a, hevc), (b, MediaCaps::NONE)], F::H264, vec![b]),
            ("a watcher can decode full-chroma H.264 alone", full, vec![], vec![(a, h264_full)], F::H264_FULL, vec![a]),
            ("encoder limits name no watcher", base, vec![], vec![(a, hevc)], F::H264, vec![]),
            ("a format the app lost names no watcher", video, vec![F::HEVC], vec![(a, hevc)], F::H264, vec![]),
        ] {
            assert_eq!(share_format(encoder, &lost, watchers), (format, holders), "{why}");
        }
        assert_eq!(failed_decode_bits(F::HEVC), MediaCaps::HEVC_DECODE | MediaCaps::HEVC_444_DECODE,
            "losing HEVC clears both HEVC bits and preserves the other capabilities");
    }
}
