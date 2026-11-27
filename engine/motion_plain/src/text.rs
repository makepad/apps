use crate::{Prop, Stagger, TextBy, TextUnit, Transform};
use crate::sample::Sampler;

/// Byte ranges of the units `by` splits `text` into. Letters skip
/// whitespace (a space takes no stagger step); words and lines are the
/// runs between whitespace and newlines.
pub fn unit_ranges(text: &str, by: TextBy) -> Vec<std::ops::Range<usize>> {
    unit_range_iter(text, by).collect()
}

/// The same segmentation without an intermediate allocation, for frame
/// units and coarser range selections. Empty lines keep their old behaviour
/// (skipped); a whitespace-only line is still a line.
pub fn unit_range_iter(text: &str, by: TextBy) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut chars = text.char_indices();
    let mut line_start = 0;
    let mut whole = false;
    std::iter::from_fn(move || match by {
        TextBy::Whole => {
            if whole { return None; }
            whole = true;
            Some(0..text.len())
        }
        TextBy::Letter | TextBy::Syllable => chars.find(|(_, c)| !c.is_whitespace()).map(|(at, c)| at..at + c.len_utf8()),
        TextBy::Word => {
            // A script written without spaces (Chinese, Japanese) is a word
            // per character.
            let (start, first) = chars.find(|(_, c)| !c.is_whitespace())?;
            if is_cjk(first) {
                return Some(start..start + first.len_utf8());
            }
            let rest = &text[start..];
            let end = rest.char_indices().skip(1).find(|(_, c)| c.is_whitespace() || is_cjk(*c)).map_or(text.len(), |(at, _)| start + at);
            while chars.as_str().len() > text.len() - end {
                chars.next();
            }
            Some(start..end)
        }
        TextBy::Line => loop {
            let start = line_start;
            let end = match chars.find(|(_, c)| *c == '\n') {
                Some((at, _)) => { line_start = at + 1; at }
                None => { line_start = text.len(); text.len() }
            };
            if end > start { return Some(start..end); }
            if line_start == text.len() { return None; }
        },
    })
}

/// A character of a script written without spaces between words (Han,
/// kana, Hangul syllables and CJK punctuation): each is its own word.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

impl Sampler<'_> {
    pub fn text_units(&mut self, text: &str, by: TextBy, stagger: &Stagger,
                      transform: &Transform, opacity: &Prop<1>, blur: &Prop<1>, color: &Prop<4>, lt: f64) -> Vec<TextUnit> {
        let count = unit_range_iter(text, by).count();
        let mut units = Vec::with_capacity(count);
        units.extend(unit_range_iter(text, by).enumerate().map(|(i, range)| {
            if by == TextBy::Whole {
                // The item carries the text's own animation.
                TextUnit { range, offset: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0, opacity: 1.0, color: self.sample(color, lt, 0), blur: 0.0, axes: Vec::new() }
            } else {
                self.text_unit(transform, opacity, blur, range, lt - stagger.delay(i, count), i, color)
            }
        }));
        units
    }

    /// One unit's own animation at its clock `ut`.
    pub fn text_unit(&mut self, tr: &Transform, opacity: &Prop<1>, blur: &Prop<1>, range: std::ops::Range<usize>, ut: f64, index: usize, color: &Prop<4>) -> TextUnit {
        let x = self.sample(&tr.x, ut, index)[0];
        let y = self.sample(&tr.y, ut, index)[0];
        TextUnit {
            range,
            offset: [x, y],
            scale: self.sample(&tr.scale, ut, index),
            rotate: self.sample(&tr.rotate, ut, index)[0],
            opacity: self.sample(opacity, ut, index)[0].clamp(0.0, 1.0),
            color: self.sample(color, ut, index),
            blur: self.sample(blur, ut, index)[0].max(0.0),
            axes: Vec::new(),
        }
    }

    pub fn text_reveal(&mut self, text: &str, reveal: Option<&Prop<1>>, lt: f64) -> Option<usize> {
        reveal.map(|r| {
            let r = self.sample(r, lt, 0)[0].max(0.0);
            // A fraction of the characters, or (above 1) a count; spaces
            // count too, as they appear.
            let all = text.chars().count();
            let n = if r <= 1.0 { (r * all as f64).round() as usize } else { r.floor() as usize };
            n.min(all)
        })
    }
}
