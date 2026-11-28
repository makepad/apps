//! Town's adapter from gated content to AI service replies.
use makepad_ai_services::pictures::{decode_picture, decode_png, fit_pictures, RgbaPicture};
use makepad_ai_services::port::{AiServicePort, PortEvent};
use makepad_ai_services::wire::{ServiceManifest, ToolOutcome, ToolResult, MAX_RESULT_IMAGE_BYTES};
use makepad_network::digest::base64_encode;
use makepad_town_core::trust::{AiOut, ForAi, Withheld};
use makepad_widgets::{Cx, Event};

/// Clean pixels encoded on a worker. Delivery still requires a current gate proof.
pub struct ReencodedPng {
    bytes: Vec<u8>,
    size: [u32; 2],
}

impl ReencodedPng {
    pub fn size(&self) -> [u32; 2] { self.size }
    #[cfg(test)]
    pub fn bytes(&self) -> &[u8] { &self.bytes }
}

fn fit_png(rgba: RgbaPicture, max_side: u32) -> Result<ReencodedPng, Withheld> {
    let max_side = max_side.min(rgba.width.max(rgba.height)).max(1);
    let mut fitted = fit_pictures(&[(rgba, [1, 1])], MAX_RESULT_IMAGE_BYTES, max_side, |_, _, _| {})
        .map_err(|_| Withheld::InvalidImage)?;
    let (bytes, size) = fitted.pop().ok_or(Withheld::InvalidImage)?;
    Ok(ReencodedPng { bytes, size })
}

pub fn reencode_ai_png(raw_png: &[u8], expected_size: [u32; 2]) -> Result<ReencodedPng, Withheld> {
    if expected_size.contains(&0) { return Err(Withheld::InvalidImage) }
    let rgba = decode_png(raw_png).map_err(|_| Withheld::InvalidImage)?;
    if [rgba.width, rgba.height] != expected_size { return Err(Withheld::InvalidImage) }
    fit_png(rgba, expected_size[0].max(expected_size[1]))
}

pub fn reencode_ai_picture(raw: &[u8], max_side: u32) -> Result<ReencodedPng, Withheld> {
    let rgba = decode_picture(raw).map_err(|_| Withheld::InvalidImage)?;
    fit_png(rgba, max_side)
}

pub struct EncodedAiImage {
    base64: String,
    size: [u32; 2],
}

impl EncodedAiImage {
    pub fn seal(png: ReencodedPng, _proof: ForAi<()>) -> Self {
        Self { base64: base64_encode(&png.bytes), size: png.size }
    }
    pub fn size(&self) -> [u32; 2] { self.size }
}

pub struct TownToolResult(ToolResult);

impl TownToolResult {
    pub fn text(call_id: impl Into<String>, out: AiOut, note: &'static str) -> Self {
        Self(ToolResult::ok(call_id, out.into_string(), note))
    }
    pub fn text_and_images(call_id: impl Into<String>, out: AiOut, images: Vec<EncodedAiImage>, note: &'static str) -> Self {
        let mut result = Self::text(call_id, out, note);
        result.0.images = images.into_iter().map(|image| image.base64).collect();
        result
    }
    pub fn refused(call_id: impl Into<String>, err: SafeToolError) -> Self { Self(ToolResult::refused(call_id, err.0)) }
    pub fn failed(call_id: impl Into<String>, err: SafeToolError) -> Self { Self(ToolResult::failed(call_id, err.0)) }
    pub fn unavailable(call_id: impl Into<String>, err: SafeToolError) -> Self { Self(ToolResult::unavailable(call_id, err.0)) }
    pub fn call_id(&self) -> &str { &self.0.call_id }
    pub fn outcome(&self) -> ToolOutcome { self.0.outcome }
    pub fn note(&self) -> &str { &self.0.note }
    pub fn error_text(&self) -> Option<&str> { (!self.0.outcome.is_ok()).then_some(self.0.text.as_str()) }
    #[cfg(test)]
    pub fn text_for_test(&self) -> &str { &self.0.text }
    #[cfg(test)]
    pub fn images_for_test(&self) -> &[String] { &self.0.images }
}

/// Refusals and failures carry static explanations or structural identifiers and counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeToolError(String);

impl SafeToolError {
    pub fn static_msg(msg: &'static str) -> Self { Self(msg.to_owned()) }
    pub fn withheld(why: Withheld) -> Self {
        Self::static_msg(match why {
            Withheld::TownUnknownCritical => "This town contains a critical record this version cannot read.",
            Withheld::TownPolicyUnreadable => "This version cannot read the town's AI policy.",
            Withheld::Hidden => "The source is hidden.",
            Withheld::External => "The source is external to the town's active membership.",
            Withheld::RequiresClearance => "The source requires clearance.",
            Withheld::MissingSource => "The source is missing.",
            Withheld::ForeignTurnOpen => "A foreign turn is open.",
            Withheld::InvalidImage => "The picture could not be verified.",
        })
    }
    pub fn formatted_ids_only(msg: String) -> Self { Self(msg) }
    pub fn as_str(&self) -> &str { &self.0 }
}

pub struct TownAiPort(AiServicePort);

impl TownAiPort {
    pub fn open(cx: &mut Cx, manifest: ServiceManifest) -> Option<Self> {
        AiServicePort::open(cx, manifest).map(Self)
    }
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> Vec<PortEvent> {
        self.0.handle_event(cx, event)
    }
    pub fn reply(&self, result: TownToolResult) { self.0.reply(result.0); }
}

/// Delivers a closed result while the app migrates its remaining tools to TownAiPort.
pub fn reply_to_port(port: &Option<AiServicePort>, result: TownToolResult) {
    if let Some(port) = port { port.reply(result.0); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_ai_services::pictures::{base64_len, encode_png};

    #[test]
    fn reencode_ai_png_strips_ancillary_chunks_and_fits_budget() {
        let pixels = RgbaPicture { width: 2, height: 1, rgba: vec![5, 10, 15, 255, 40, 60, 80, 93] };
        let mut raw = encode_png(&pixels).unwrap();
        // A PNG tEXt chunk after IHDR, including its length and CRC32(type + data).
        raw.splice(33..33, b"\x00\x00\x00\x13tEXtComment\x00SECRET-META\x1d\x3b\x4f\x39".iter().copied());
        for png in [reencode_ai_png(&raw, [2, 1]).unwrap(), reencode_ai_picture(&raw, 2).unwrap()] {
            assert_eq!(png.size(), [2, 1], "the output dimensions describe the encoded pixels");
            assert_eq!(decode_png(png.bytes()).unwrap(), pixels, "small RGBA pictures preserve every channel");
            assert!(!png.bytes().windows(11).any(|s| s == b"SECRET-META"), "both picture paths strip ancillary metadata");
        }
        let mut corrupt = raw.clone();
        corrupt[32] ^= 1; // Corrupt the required IHDR CRC.
        assert!(matches!(reencode_ai_png(&corrupt, [2, 1]), Err(Withheld::InvalidImage)), "corrupt PNG bytes are withheld");
        assert!(matches!(reencode_ai_png(&raw, [0, 1]), Err(Withheld::InvalidImage)), "zero expected size is withheld");
        assert!(matches!(reencode_ai_png(&raw, [1, 2]), Err(Withheld::InvalidImage)), "mismatched dimensions are withheld");
        assert!(matches!(reencode_ai_picture(b"invalid", 2), Err(Withheld::InvalidImage)), "the general decoder withholds garbage");

        // Just over the wire budget, with independent noise in all four channels.
        let side = 420;
        let mut seed = 1u32;
        let rgba = (0..side * side).flat_map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed.to_le_bytes()
        }).collect();
        let noisy = encode_png(&RgbaPicture { width: side, height: side, rgba }).unwrap();
        assert!(base64_len(noisy.len()) > MAX_RESULT_IMAGE_BYTES, "the source actually exceeds the base64 wire budget");
        let fitted = reencode_ai_png(&noisy, [side, side]).unwrap();
        assert!(base64_len(fitted.bytes().len()) <= MAX_RESULT_IMAGE_BYTES, "an oversized image fits after scaling");
        let actual = decode_png(fitted.bytes()).unwrap();
        assert_eq!(fitted.size(), [actual.width, actual.height], "scaled dimensions match the emitted PNG");
        assert!(actual.width < side && actual.height < side, "the oversized image was downscaled");
    }
}
