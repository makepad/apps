/// A PNG of `w` x `h` pixels, rows from the top.
pub fn png(w: usize, h: usize, rgb: &[[u8; 3]]) -> Vec<u8> {
    use makepad_zune_png::{PngEncoder, makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions}};
    let bytes: Vec<u8> = rgb.iter().flatten().copied().collect();
    let mut encoded = Vec::new();
    PngEncoder::new(&bytes, EncoderOptions::new(w, h, ColorSpace::RGB, BitDepth::Eight)).encode(&mut encoded).expect("the PNG encodes");
    encoded
}
