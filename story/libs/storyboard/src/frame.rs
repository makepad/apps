//! Frame layouts: where things sit inside the picture, so a board plans a
//! vertical video against the platform's UI. A board names one template
//! (`layout: @shorts`); its items name a band (`band: @headline`), and the
//! canvas draws the frame with its bands and the platform's safe zones.
//!
//! The Shorts framing is the "screen within a screen" of the reference
//! Short (REF-Ac7SuDYTQDc §3): a headline band, a 16:9 inset, a meta band
//! and a decorative floor, type always between 0.08 and 0.74 of the
//! height, only decoration in the UI bands.

/// One band of a frame layout, in fractions of the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub id: &'static str,
    pub label: &'static str,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    /// It may hold type (words people must read).
    pub text: bool,
    /// What goes there, for the AI and the inspector.
    pub what: &'static str,
}

/// The platform UI's covered margins, in fractions: top, bottom, left, right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Safe {
    pub top: f64,
    pub bottom: f64,
    pub left: f64,
    pub right: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Template {
    pub id: &'static str,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
    pub safe: Safe,
    /// What the covered margins hold on the platform.
    pub safe_what: &'static str,
    pub bands: &'static [Band],
    /// The rule the AI follows.
    pub rule: &'static str,
}

/// YouTube Shorts' UI margins (Stage's `SafeZone::Shorts`).
pub const SHORTS_SAFE: Safe = Safe { top: 0.08, bottom: 0.20, left: 0.05, right: 0.13 };

pub const SHORTS: Template = Template {
    id: "shorts",
    label: "Shorts framing",
    width: 1080,
    height: 1920,
    safe: SHORTS_SAFE,
    safe_what: "top: channel and search; bottom 20 %: title, channel and music; right 13 %: like, comment, share",
    bands: &[
        Band { id: "tag", label: "Series tag", x0: 0.05, y0: 0.015, x1: 0.87, y1: 0.035, text: false, what: "a tiny tracked mono series or brand tag; the platform's UI may cover it" },
        Band { id: "headline", label: "Headline", x0: 0.05, y0: 0.08, x1: 0.87, y1: 0.20, text: true, what: "the beat's headline: condensed heavy caps, the key word in the accent" },
        Band { id: "content", label: "16:9 inset", x0: 0.035, y0: 0.36, x1: 0.965, y1: 0.66, text: true, what: "the picture: a 16:9 inset (93 % of the width, never cropped), a terminal, cards" },
        Band { id: "meta", label: "Meta", x0: 0.05, y0: 0.67, x1: 0.87, y1: 0.74, text: true, what: "status line, progress bar and time counter in mono" },
        Band { id: "floor", label: "Floor", x0: 0.0, y0: 0.80, x1: 1.0, y1: 1.0, text: false, what: "decoration only (a grid floor, a progress bar): the platform's title covers it" },
    ],
    rule: "All type sits between 0.08 and 0.74 of the height, horizontally inside the safe margins; only decoration goes into the UI bands.",
};

pub const TEMPLATES: &[Template] = &[SHORTS];

pub fn template(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|template| template.id == id)
}

impl Template {
    pub fn band(&self, id: &str) -> Option<&'static Band> {
        self.bands.iter().find(|band| band.id == id)
    }

    pub fn band_ids(&self) -> Vec<&'static str> {
        self.bands.iter().map(|band| band.id).collect()
    }
}

impl Band {
    /// Inside the platform's safe area.
    pub fn safe_in(&self, safe: &Safe) -> bool {
        self.y0 >= safe.top - 1e-9 && self.y1 <= 1.0 - safe.bottom + 1e-9 && self.x0 >= safe.left - 1e-9 && self.x1 <= 1.0 - safe.right + 1e-9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorts_type_bands_are_in_the_safe_area_and_decoration_is_not_needed_there() {
        for band in SHORTS.bands {
            if band.text && band.id != "content" {
                assert!(band.safe_in(&SHORTS.safe), "{}", band.id);
            }
            assert!(band.y0 < band.y1 && band.x0 < band.x1, "{}", band.id);
        }
        // The inset is picture: it may reach under the right-hand column,
        // but not into the top or bottom UI bands.
        let content = SHORTS.band("content").unwrap();
        assert!(content.y0 >= SHORTS.safe.top && content.y1 <= 1.0 - SHORTS.safe.bottom);
        // Type sits between 0.08 and 0.74.
        assert!(SHORTS.bands.iter().filter(|band| band.text).all(|band| band.y0 >= 0.08 && band.y1 <= 0.74));
        // The floor is in the bottom UI band.
        assert!(!SHORTS.band("floor").unwrap().safe_in(&SHORTS.safe));
        assert_eq!(template("shorts"), Some(&SHORTS));
    }
}
