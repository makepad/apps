//! Host-only image conversion and the constants supplied to firmware.
use makepad_script_compute::parse::{Expr, ExprKind, Item, Span};
use makepad_script_compute::ShaderError;
use std::collections::HashMap;
use std::path::Path;

pub(crate) struct Image {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u16>,
}

/// The pictures among `assets`, each by its normalized stem; the other files' names go to
/// `others` (pictures are the only kind the module carries so far).
pub(crate) fn decode(assets: &[(&str, &[u8])], others: &mut Vec<String>) -> Result<Vec<Image>, ShaderError> {
    let mut names = HashMap::new();
    let mut files = Vec::new();
    for &(file, bytes) in assets {
        if makepad_image_decode::detect_image_format_from_path_and_data(Path::new(file), bytes).is_none() {
            others.push(file.to_string());
            continue;
        }
        let error = |message| ShaderError::new(0, 1, format!("image `{file}`: {message}"));
        let stem = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let name: String = stem.to_lowercase().chars().map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '_' }
        }).collect();
        if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
            return Err(error("the normalized file stem must start with a letter"));
        }
        for generated in [name.clone(), format!("{name}_w"), format!("{name}_h")] {
            if let Some(other) = names.insert(generated.clone(), file) {
                return Err(ShaderError::new(0, 1, format!("images `{other}` and `{file}` both define `{generated}`")));
            }
        }
        files.push((name, file, bytes));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut images = Vec::new();
    for (name, file, bytes) in files {
        let error = |message| ShaderError::new(0, 1, format!("image `{file}`: {message}"));
        let (width, height, rgba) = makepad_image_decode::decode_rgba(bytes)
            .map_err(|e| error(format!("cannot decode: {e:?}")))?;
        let dimensions = || error("dimensions must be non-zero and their even padding must fit i32".to_string());
        if width == 0 || height == 0 { return Err(dimensions()); }
        // CO5300 P15/P16 require even window sizes. Repeat the last column
        // and row so a whole image at an even position always has an even window.
        let w = width.checked_add(width % 2).filter(|n| *n <= i32::MAX as usize).ok_or_else(dimensions)?;
        let h = height.checked_add(height % 2).filter(|n| *n <= i32::MAX as usize).ok_or_else(dimensions)?;
        let count = w.checked_mul(h).ok_or_else(dimensions)?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(count).map_err(|_| error(format!("cannot allocate {count} pixels")))?;
        for y in 0..h {
            for x in 0..w {
                let at = (y.min(height - 1) * width + x.min(width - 1)) * 4;
                // Alpha is ignored: no blending in v1.
                pixels.push(((rgba[at] as u16 >> 3) << 11) | ((rgba[at + 1] as u16 >> 2) << 5) | (rgba[at + 2] as u16 >> 3));
            }
        }
        images.push(Image { name, width: w as u32, height: h as u32, pixels });
    }
    Ok(images)
}

pub(crate) fn declarations(images: &[Image], at: Span) -> Vec<Item> {
    let number = |n: u32| Expr { kind: ExprKind::Num(f64::from(n), true), span: at };
    let mut items = Vec::new();
    for (index, image) in images.iter().enumerate() {
        items.push(Item::Let {
            name: image.name.clone(), ann: None,
            value: Expr { kind: ExprKind::Call("image".to_string(), vec![number(index as u32), number(image.width), number(image.height)]), span: at }, span: at,
        });
        for (suffix, size) in [("w", image.width), ("h", image.height)] {
            items.push(Item::Let { name: format!("{}_{suffix}", image.name), ann: None, value: number(size), span: at });
        }
    }
    items
}
