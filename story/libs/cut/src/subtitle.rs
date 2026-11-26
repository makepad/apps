//! Captions: timed words broken into short captions, the
//! word-by-word highlight at a time, and where a caption may sit inside a
//! platform's safe zone. Pure: the same captions are burned into frames and
//! written as SRT and WebVTT sidecars.

use crate::model::{SafeZone, SpokenLine, Word};

/// Seconds a caption stays after its last word, unless the next one
/// starts sooner.
pub const CAPTION_HOLD: f64 = 0.5;
/// A silence between two words at least this long starts a new caption.
pub const CAPTION_PAUSE: f64 = 0.7;
/// A caption stays on screen at least this long when the next one leaves
/// room: shorter ones flicker.
pub const MIN_CAPTION_SECONDS: f64 = 0.8;
/// Characters one caption line holds at most, landscape and vertical
/// outputs (a one-line caption must fit the frame).
pub const MAX_CAPTION_CHARS: usize = 42;
pub const MAX_CAPTION_CHARS_VERTICAL: usize = 32;
/// Speaking rate assumed for a line whose take is not measured yet, in
/// seconds per word.
pub const ESTIMATED_SECONDS_PER_WORD: f64 = 0.4;

/// One caption: at most `max_words` words shown together, on
/// `[start, end)` of the cut.
#[derive(Clone, Debug, PartialEq)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    pub words: Vec<Word>,
    /// The words are the current text but not (yet) what the take says
    /// (`PlacedLine::provisional`); set by `CutEval::captions`.
    pub provisional: bool,
}

impl Caption {
    pub fn text(&self) -> String {
        self.words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    pub fn visible(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    /// The word being spoken at `t` (the last one started, through the
    /// pauses between words) and how many words are spoken: whole words
    /// count 1, the one being spoken its elapsed fraction. A karaoke
    /// renderer fills that many words.
    pub fn highlight(&self, t: f64) -> (Option<usize>, f64) {
        let mut active = None;
        let mut spoken = 0.0;
        for (index, word) in self.words.iter().enumerate() {
            if t < word.start {
                break;
            }
            active = Some(index);
            spoken += if t >= word.end || word.end <= word.start { 1.0 } else { (t - word.start) / (word.end - word.start) };
        }
        (active, spoken)
    }
}

/// Split `n` words into the fewest groups of at most `max` words, as even
/// as possible, longer groups first: 7 by 6 is 4 + 3, not 6 + 1.
pub fn balanced_groups(n: usize, max: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let max = max.max(1);
    even_groups(n, n.div_ceil(max))
}

/// `n` words in exactly `groups` groups (1..=n), as even as possible,
/// longer groups first.
fn even_groups(n: usize, groups: usize) -> Vec<usize> {
    let (base, extra) = (n / groups, n % groups);
    (0..groups).map(|index| base + (index < extra) as usize).collect()
}

/// Break timed lines into captions of at most `max_words` words. A line
/// never shares a caption with another, a pause of [`CAPTION_PAUSE`] or
/// more starts a new one, and each run between breaks is split evenly
/// ([`balanced_groups`]). A caption shows from its first word's start
/// until [`CAPTION_HOLD`] after its last word's end (at least
/// [`MIN_CAPTION_SECONDS`]), or until the next caption starts. Lines are
/// taken in time order.
pub fn captions(lines: &[SpokenLine], max_words: usize) -> Vec<Caption> {
    captions_with(lines, max_words, usize::MAX)
}

/// The fewest balanced groups of at most `max_words` words whose text
/// (words joined by spaces) fits `max_chars`; a single word longer than
/// that stays alone.
fn fitting_groups(words: &[Word], max_words: usize, max_chars: usize) -> Vec<usize> {
    let n = words.len();
    let mut groups = balanced_groups(n, max_words);
    while groups.len() < n {
        let mut at = 0;
        let fits = groups.iter().all(|&size| {
            let chars = words[at..at + size].iter().map(|word| word.text.chars().count()).sum::<usize>() + size - 1;
            at += size;
            size == 1 || chars <= max_chars
        });
        if fits {
            break;
        }
        groups = even_groups(n, groups.len() + 1);
    }
    groups
}

/// [`captions`] whose captions also fit `max_chars` characters.
pub fn captions_with(lines: &[SpokenLine], max_words: usize, max_chars: usize) -> Vec<Caption> {
    let mut lines = lines.iter().filter(|line| !line.words.is_empty()).collect::<Vec<_>>();
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Caption> = Vec::new();
    for line in lines {
        let mut run_start = 0;
        let words = &line.words;
        for index in 1..=words.len() {
            let pause = index < words.len() && words[index].start - words[index - 1].end >= CAPTION_PAUSE;
            if index < words.len() && !pause {
                continue;
            }
            let mut at = run_start;
            for size in fitting_groups(&words[run_start..index], max_words, max_chars) {
                let group = words[at..at + size].to_vec();
                let start = group[0].start;
                let end = (group[group.len() - 1].end.max(start) + CAPTION_HOLD).max(start + MIN_CAPTION_SECONDS);
                out.push(Caption { start, end, words: group, provisional: false });
                at += size;
            }
            run_start = index;
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    for index in 1..out.len() {
        let next = out[index].start;
        let previous = &mut out[index - 1];
        previous.end = previous.end.min(next).max(previous.start);
    }
    out.retain(|caption| caption.end > caption.start);
    out
}

/// Word times for `text` spoken on `[start, end)` when nothing measured
/// them: each word gets a share of the time proportional to its length
/// (plus one for the space after it).
pub fn estimate_words(text: &str, start: f64, end: f64) -> Vec<Word> {
    let words = text.split_whitespace().collect::<Vec<_>>();
    let weights = words.iter().map(|word| word.chars().count() as f64 + 1.0).collect::<Vec<_>>();
    let total: f64 = weights.iter().sum();
    let length = (end - start).max(0.0);
    let mut at = start;
    words.iter().zip(&weights).map(|(word, weight)| {
        let span = length * weight / total;
        let word = Word { text: word.to_string(), start: at, end: at + span };
        at += span;
        word
    }).collect()
}

/// A caption's line height, in ems (the renderer's 1.2 plus its shadow).
pub const CAPTION_LINE_EM: f64 = 1.3;
/// A caption character's width, in ems: a generous average, so the
/// estimate errs towards a wider box.
pub const CAPTION_CHAR_EM: f64 = 0.6;
/// Lines one caption uses before its font shrinks (`@movie`: one or two).
pub const CAPTION_MAX_LINES: usize = 2;
/// The smallest fraction of its authored size a caption shrinks to.
pub const CAPTION_MIN_SCALE: f64 = 0.6;

/// Where a caption goes (`@movie`): strictly inside the safe zone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptionPlacement {
    /// The caption block's centre, in output fractions.
    pub anchor: [f64; 2],
    /// The font size to draw at (the authored size, or smaller to fit).
    pub font_size: f64,
    /// Lines the caption wraps into at that size (estimated).
    pub lines: usize,
    /// The safe region `[x, y, w, h]` in output fractions: the renderer
    /// wraps its lines to its width and keeps the whole block inside it.
    pub safe: [f64; 4],
}

/// Place a caption of `chars` characters at `font_size` pixels, asked for
/// at `anchor`, inside `safe` in a `size` output. It wraps to the safe
/// width into at most [`CAPTION_MAX_LINES`] lines, shrinking the font
/// (down to [`CAPTION_MIN_SCALE`] of its size) when it would need more,
/// and shrinking further only if even then the block is taller than the
/// safe region. The block then moves (up, down, sideways) as little as
/// needed to lie wholly inside the safe region: an anchor inside a band is
/// pulled out of it.
pub fn place_caption(anchor: [f64; 2], safe: SafeZone, font_size: f64, chars: usize, size: (u32, u32)) -> CaptionPlacement {
    let (w, h) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let bands = safe.bands();
    let (sx, sy, sw, sh) = (bands.left * w, bands.top * h, (1.0 - bands.left - bands.right) * w, (1.0 - bands.top - bands.bottom) * h);
    let chars = chars.max(1) as f64;
    let mut font = font_size.max(1.0);
    let width = |font: f64| chars * font * CAPTION_CHAR_EM;
    let lines = |font: f64| (width(font) / sw).ceil().max(1.0);
    if lines(font) > CAPTION_MAX_LINES as f64 {
        font = (font * CAPTION_MAX_LINES as f64 * sw / width(font)).max(font_size * CAPTION_MIN_SCALE);
    }
    if lines(font) * font * CAPTION_LINE_EM > sh {
        font = sh / (lines(font) * CAPTION_LINE_EM);
    }
    let count = lines(font);
    let (box_w, box_h) = (width(font).min(sw), count * font * CAPTION_LINE_EM);
    let place = |value: f64, low: f64, span: f64, half: f64| {
        if span <= 2.0 * half { low + span * 0.5 } else { value.clamp(low + half, low + span - half) }
    };
    let x = place(anchor[0] * w, sx, sw, box_w * 0.5);
    let y = place(anchor[1] * h, sy, sh, box_h * 0.5);
    CaptionPlacement { anchor: [x / w, y / h], font_size: font, lines: count as usize, safe: [sx / w, sy / h, sw / w, sh / h] }
}

/// The anchor (output fractions) a caption of `chars` characters at
/// `font_size` pixels may have in a `size` output so that its box stays
/// inside `safe`: the requested anchor, moved in as little as needed. The
/// box is estimated at 0.55 em per character and 1.3 em tall; a box wider
/// than the safe region is centred on it.
pub fn safe_anchor(anchor: [f64; 2], safe: SafeZone, font_size: f64, chars: usize, size: (u32, u32)) -> [f64; 2] {
    let (w, h) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let bands = safe.bands();
    let half_w = chars as f64 * font_size * 0.55 * 0.5 / w;
    let half_h = font_size * 1.3 * 0.5 / h;
    let place = |value: f64, low: f64, high: f64, half: f64| {
        if high - low <= 2.0 * half { (low + high) * 0.5 } else { value.clamp(low + half, high - half) }
    };
    [
        place(anchor[0], bands.left, 1.0 - bands.right, half_w),
        place(anchor[1], bands.top, 1.0 - bands.bottom, half_h),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end }
    }

    fn line(words: &[(&str, f64, f64)]) -> SpokenLine {
        let words = words.iter().map(|(text, start, end)| word(text, *start, *end)).collect::<Vec<_>>();
        SpokenLine { start: words[0].start, end: words[words.len() - 1].end, words }
    }

    #[test]
    fn groups_are_balanced() {
        assert_eq!(balanced_groups(7, 6), [4, 3]);
        assert_eq!(balanced_groups(6, 6), [6]);
        assert_eq!(balanced_groups(13, 6), [5, 4, 4]);
        assert_eq!(balanced_groups(3, 1), [1, 1, 1]);
        assert!(balanced_groups(0, 6).is_empty());
    }

    #[test]
    fn captions_break_by_max_words_pauses_and_lines() {
        let one = line(&[("Type,", 1.0, 1.3), ("and", 1.3, 1.5), ("it", 1.5, 1.6), ("saves", 1.6, 2.0), ("as", 2.0, 2.1), ("you", 2.1, 2.3), ("go.", 2.3, 2.7)]);
        let captions = captions(&[one.clone()], 6);
        assert_eq!(captions.iter().map(Caption::text).collect::<Vec<_>>(), ["Type, and it saves", "as you go."]);
        assert_eq!((captions[0].start, captions[0].end), (1.0, 2.0), "held until the next caption starts");
        assert_eq!((captions[1].start, captions[1].end), (2.0, 3.2), "held half a second after the last word");

        // A pause splits first; lines never share a caption.
        let paused = line(&[("Notes", 0.0, 0.3), ("starts.", 0.3, 0.8), ("Then", 2.0, 2.2), ("more.", 2.2, 2.5)]);
        let next = line(&[("That's", 2.6, 2.8), ("it.", 2.8, 3.0)]);
        let captions = super::captions(&[next, paused], 6);
        assert_eq!(captions.iter().map(Caption::text).collect::<Vec<_>>(), ["Notes starts.", "Then more.", "That's it."]);
        assert_eq!(captions[0].end, 1.3);
        assert_eq!(captions[1].end, 2.6, "cut where the next line starts");
    }

    #[test]
    fn captions_fit_their_characters_and_stay_long_enough() {
        let long = line(&[("Extraordinarily", 0.0, 0.5), ("comprehensive", 0.5, 1.0), ("documentation", 1.0, 1.5), ("everywhere", 1.5, 2.0)]);
        let captions = captions_with(&[long.clone()], 6, 32);
        assert_eq!(captions.iter().map(Caption::text).collect::<Vec<_>>(), ["Extraordinarily comprehensive", "documentation everywhere"]);
        assert_eq!(super::captions(&[long], 6).len(), 1, "no character limit");
        // One fast word shows for the minimum, unless the next caption comes.
        let fast = line(&[("Go!", 1.0, 1.1)]);
        assert_eq!(super::captions(&[fast.clone()], 6)[0].end, 1.8);
        let after = line(&[("Now.", 1.3, 1.5)]);
        assert_eq!(super::captions(&[fast, after], 6)[0].end, 1.3);
    }

    #[test]
    fn the_highlight_counts_spoken_words() {
        let caption = &captions(&[line(&[("one", 1.0, 1.5), ("two", 2.0, 3.0)])], 6)[0];
        assert_eq!(caption.highlight(0.5), (None, 0.0));
        assert_eq!(caption.highlight(1.25), (Some(0), 0.5));
        assert_eq!(caption.highlight(1.75), (Some(0), 1.0), "a pause keeps the last word active");
        assert_eq!(caption.highlight(2.5), (Some(1), 1.5));
        assert_eq!(caption.highlight(3.2), (Some(1), 2.0));
        assert!(caption.visible(3.2) && !caption.visible(3.5));
    }

    #[test]
    fn estimated_words_share_time_by_length() {
        // "a" weighs 2 and "bbb" 4 (a character each, plus the space).
        let words = estimate_words("a bbb", 1.0, 4.0);
        assert_eq!(words, [word("a", 1.0, 2.0), word("bbb", 2.0, 4.0)]);
        assert!(estimate_words("  ", 0.0, 1.0).is_empty());
    }

    /// The estimated block of a placement, in output fractions.
    fn block(placement: &CaptionPlacement, chars: usize, size: (u32, u32)) -> (f64, f64, f64, f64) {
        let (w, h) = (size.0 as f64, size.1 as f64);
        let bw = (chars as f64 * placement.font_size * CAPTION_CHAR_EM).min(placement.safe[2] * w) / w;
        let bh = placement.lines as f64 * placement.font_size * CAPTION_LINE_EM / h;
        (placement.anchor[0] - bw / 2.0, placement.anchor[1] - bh / 2.0, placement.anchor[0] + bw / 2.0, placement.anchor[1] + bh / 2.0)
    }

    #[test]
    fn captions_lie_strictly_inside_every_presets_safe_zone() {
        let size = (1080, 1920);
        for safe in [SafeZone::Reels, SafeZone::TikTok, SafeZone::Shorts] {
            let bands = safe.bands();
            // The spec's anchor (0.86) lies in every preset's bottom band;
            // also one in the top band and one off the side.
            for (anchor, chars) in [([0.5, 0.86], 20), ([0.5, 0.86], 60), ([0.5, 0.02], 30), ([0.98, 0.5], 12), ([0.5, 0.99], 200)] {
                let placement = place_caption(anchor, safe, 64.0, chars, size);
                let (left, top, right, bottom) = block(&placement, chars, size);
                let eps = 1e-9;
                assert!(top >= bands.top - eps && bottom <= 1.0 - bands.bottom + eps, "{safe:?} {anchor:?} {chars}: {top}..{bottom} {placement:?}");
                assert!(left >= bands.left - eps && right <= 1.0 - bands.right + eps, "{safe:?} {anchor:?} {chars}: {left}..{right}");
                assert!(placement.lines <= CAPTION_MAX_LINES || placement.font_size <= 64.0 * CAPTION_MIN_SCALE + eps);
                assert_eq!(placement.safe, [bands.left, bands.top, 1.0 - bands.left - bands.right, 1.0 - bands.top - bands.bottom]);
            }
            // Moved up just enough: the block's bottom sits on the band.
            let placement = place_caption([0.5, 0.86], safe, 64.0, 20, size);
            assert!((block(&placement, 20, size).3 - (1.0 - bands.bottom)).abs() < 1e-9, "{safe:?}");
        }
        // A long caption wraps to two lines at its size, then shrinks.
        let two = place_caption([0.5, 0.5], SafeZone::Reels, 64.0, 30, size);
        assert_eq!((two.lines, two.font_size), (2, 64.0));
        let shrunk = place_caption([0.5, 0.5], SafeZone::Reels, 64.0, 60, size);
        assert!(shrunk.lines == 2 && shrunk.font_size < 64.0 && shrunk.font_size >= 64.0 * CAPTION_MIN_SCALE);
        // Inside the safe zone already: left where it was.
        assert_eq!(place_caption([0.5, 0.5], SafeZone::Reels, 64.0, 10, size).anchor, [0.5, 0.5]);
    }

    #[test]
    fn anchors_stay_inside_the_safe_zone() {
        // Reels hides the bottom 35%: a caption asked for at 0.72 moves up
        // until its box clears the band.
        let [x, y] = safe_anchor([0.5, 0.72], SafeZone::Reels, 64.0, 20, (1080, 1920));
        assert_eq!(x, 0.5);
        assert!((y - (0.65 - 64.0 * 1.3 * 0.5 / 1920.0)).abs() < 1e-12, "{y}");
        assert_eq!(safe_anchor([0.5, 0.72], SafeZone::None, 64.0, 20, (1080, 1920)), [0.5, 0.72]);
        // Pushed in from the side, and centred when wider than the region.
        let [x, _] = safe_anchor([0.02, 0.5], SafeZone::Reels, 64.0, 10, (1080, 1920));
        assert!((x - (0.06 + 10.0 * 64.0 * 0.55 * 0.5 / 1080.0)).abs() < 1e-12);
        let [x, _] = safe_anchor([0.1, 0.5], SafeZone::TikTok, 64.0, 200, (1080, 1920));
        assert!((x - (0.055 + 1.0 - 0.11) * 0.5).abs() < 1e-12);
    }
}
