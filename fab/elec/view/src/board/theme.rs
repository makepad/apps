//! How the board looks in 2D: the layer stack order, each layer's ink and opacity, and the pane's
//! own colours, for the dark and the light canvas.
//!
//! Inks are straight RGBA, display referred. Layers are composited bottom to top in `stack`
//! order (the board seen from the front): the back's documentation layers, the back copper, the
//! inner copper from the back up, the front copper, then the front's documentation and the edge.
//! The active copper layer draws at full opacity, the other copper layers dimmer so the one being
//! worked on reads first, and the holes are cut through everything after the composite.

use fab_elec_core::LayerId;

/// Straight RGBA from 0xRRGGBB.
const fn hex(c: u32, a: f32) -> [f32; 4] {
    [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0, a]
}

/// The board's 2D palette.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardTheme {
    pub light: bool,
    /// The pane behind the board.
    pub canvas: [f32; 4],
    /// The board's body under every layer.
    pub substrate: [f32; 4],
    /// A drilled hole: what lies under the board.
    pub hole: [f32; 4],
    /// Ratsnest hairlines.
    pub rats: [f32; 4],
    /// Finding markers and labels: info, warning, error, and the plate under a label.
    pub marks: [[f32; 4]; 4],
    /// Neutral overlay text on a mark plate (the heat legend).
    pub label: [f32; 4],
    pub front_cu: [f32; 4],
    pub back_cu: [f32; 4],
    /// Inner copper layers, cycled from the first inner layer down.
    pub inner_cu: [[f32; 4]; 6],
    pub front_silk: [f32; 4],
    pub back_silk: [f32; 4],
    pub mask: [f32; 4],
    pub paste: [f32; 4],
    pub courtyard: [f32; 4],
    pub fab: [f32; 4],
    pub edge: [f32; 4],
    /// Selected items: cyan-white, straight RGBA; applied after violation tinting.
    pub sel_ink: [f32; 4],
    /// Informational findings: soft blue, straight RGBA.
    pub info_ink: [f32; 4],
    /// Warning findings: amber, straight RGBA.
    pub warn_ink: [f32; 4],
    /// Error findings: red, straight RGBA.
    pub err_ink: [f32; 4],
    /// Alpha multiplier for retained items ghosted while a preview moves them.
    pub ghost_alpha: f32,
    /// Alpha multiplier at full pane-filter dimming; partial dimming interpolates from 1.
    pub dim_alpha: f32,
    /// The opacity of the active copper layer and of the other copper layers.
    pub active_alpha: f32,
    pub other_alpha: f32,
    /// The floor a stroke thinner than a device pixel fades to.
    pub thin_alpha: f32,
}

impl BoardTheme {
    pub fn dark() -> BoardTheme {
        BoardTheme {
            light: false,
            canvas: hex(0x14161a, 1.0),
            substrate: hex(0x0f1a14, 1.0),
            hole: hex(0x0c0d10, 1.0),
            rats: hex(0xf2ecdc, 0.55),
            marks: [hex(0x91baff, 1.0), hex(0xffc454, 1.0), hex(0xff5264, 1.0), hex(0x14161a, 0.86)],
            label: hex(0xc9ced8, 1.0),
            front_cu: hex(0xe07a4f, 1.0),
            back_cu: hex(0x4f95e0, 1.0),
            inner_cu: [
                hex(0x6cc070, 1.0),
                hex(0xc48be3, 1.0),
                hex(0xe3c25a, 1.0),
                hex(0x57c6c6, 1.0),
                hex(0xe2708f, 1.0),
                hex(0x9aa6f2, 1.0),
            ],
            front_silk: hex(0xf2ecdc, 1.0),
            back_silk: hex(0xa7b5cc, 1.0),
            mask: hex(0x3fae7a, 0.5),
            paste: hex(0xb8bcc4, 0.8),
            courtyard: hex(0x8a919c, 0.6),
            fab: hex(0x9aa0a8, 0.6),
            edge: hex(0xeed672, 1.0),
            sel_ink: hex(0xb8f4ff, 1.0),
            info_ink: hex(0x91baff, 1.0),
            warn_ink: hex(0xffc454, 1.0),
            err_ink: hex(0xff5264, 1.0),
            ghost_alpha: 0.25,
            dim_alpha: 0.2,
            active_alpha: 0.92,
            other_alpha: 0.5,
            thin_alpha: 0.4,
        }
    }

    pub fn light() -> BoardTheme {
        BoardTheme {
            light: true,
            canvas: hex(0xf3f2ee, 1.0),
            substrate: hex(0xdfe3d6, 1.0),
            hole: hex(0xffffff, 1.0),
            rats: hex(0x394352, 0.6),
            marks: [hex(0x527fba, 1.0), hex(0xb87508, 1.0), hex(0xc8283c, 1.0), hex(0xf3f2ee, 0.9)],
            label: hex(0x2b2f36, 1.0),
            front_cu: hex(0xc8582c, 1.0),
            back_cu: hex(0x2f6fbe, 1.0),
            inner_cu: [
                hex(0x3f9a46, 1.0),
                hex(0x9050b8, 1.0),
                hex(0xb08a1a, 1.0),
                hex(0x1f9a9a, 1.0),
                hex(0xc0405f, 1.0),
                hex(0x5664c8, 1.0),
            ],
            front_silk: hex(0x2c3038, 1.0),
            back_silk: hex(0x5d6b84, 1.0),
            mask: hex(0x2f8f5f, 0.4),
            paste: hex(0x80868f, 0.8),
            courtyard: hex(0x7a808a, 0.7),
            fab: hex(0x8a9098, 0.7),
            edge: hex(0xa68a12, 1.0),
            sel_ink: hex(0x8ae5f5, 1.0),
            info_ink: hex(0x527fba, 1.0),
            warn_ink: hex(0xb87508, 1.0),
            err_ink: hex(0xc8283c, 1.0),
            ghost_alpha: 0.25,
            dim_alpha: 0.2,
            active_alpha: 0.95,
            other_alpha: 0.45,
            thin_alpha: 0.45,
        }
    }

    pub fn for_light(light: bool) -> BoardTheme {
        if light {
            BoardTheme::light()
        } else {
            BoardTheme::dark()
        }
    }

    /// A layer's ink.
    pub fn ink(&self, l: LayerId) -> [f32; 4] {
        match l {
            LayerId::F_CU => self.front_cu,
            LayerId::B_CU => self.back_cu,
            LayerId::F_SILK => self.front_silk,
            LayerId::B_SILK => self.back_silk,
            LayerId::F_MASK | LayerId::B_MASK => self.mask,
            LayerId::F_PASTE | LayerId::B_PASTE => self.paste,
            LayerId::F_CRTYD | LayerId::B_CRTYD => self.courtyard,
            LayerId::F_FAB | LayerId::B_FAB => self.fab,
            LayerId::EDGE => self.edge,
            l => match l.inner_index() {
                Some(i) => self.inner_cu[(i.max(1) as usize - 1) % self.inner_cu.len()],
                None => self.fab,
            },
        }
    }

    /// A layer's opacity with `active` the layer being worked on.
    pub fn opacity(&self, l: LayerId, active: LayerId) -> f32 {
        if l.is_copper() {
            return if l == active { self.active_alpha } else { self.other_alpha };
        }
        match l {
            LayerId::F_SILK | LayerId::EDGE => 1.0,
            LayerId::B_SILK => 0.55,
            _ => 1.0,
        }
    }

    /// Whether a layer shows when nobody chose: copper, silk, courtyards and the edge; masks,
    /// paste and fabrication layers are hidden until asked for.
    pub fn shown_by_default(l: LayerId) -> bool {
        l.is_copper() || matches!(l, LayerId::F_SILK | LayerId::B_SILK | LayerId::F_CRTYD | LayerId::EDGE)
    }

    /// The draw order bottom to top (the board seen from the front) of the given layers.
    pub fn stack(layers: impl IntoIterator<Item = LayerId>) -> Vec<LayerId> {
        let mut out: Vec<LayerId> = layers.into_iter().collect();
        out.sort_by_key(|&l| Self::rank(l));
        out.dedup();
        out
    }

    /// Bottom to top: back documentation, back copper, inner copper from the back up, front
    /// copper, front documentation, edge.
    fn rank(l: LayerId) -> (u8, i32) {
        match l {
            LayerId::B_FAB => (0, 0),
            LayerId::B_CRTYD => (1, 0),
            LayerId::B_SILK => (2, 0),
            LayerId::B_PASTE => (3, 0),
            LayerId::B_MASK => (4, 0),
            LayerId::B_CU => (5, 0),
            LayerId::F_CU => (7, 0),
            LayerId::F_MASK => (8, 0),
            LayerId::F_PASTE => (9, 0),
            LayerId::F_SILK => (10, 0),
            LayerId::F_CRTYD => (11, 0),
            LayerId::F_FAB => (12, 0),
            LayerId::EDGE => (14, 0),
            l => match l.inner_index() {
                // deeper inner layers sit lower
                Some(i) => (6, -(i as i32)),
                None => (13, l.0 as i32),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_order_and_inks() {
        let s = BoardTheme::stack([LayerId::EDGE, LayerId::F_CU, LayerId::inner(2), LayerId::B_CU, LayerId::inner(1), LayerId::F_SILK, LayerId::B_SILK]);
        assert_eq!(
            s,
            vec![LayerId::B_SILK, LayerId::B_CU, LayerId::inner(2), LayerId::inner(1), LayerId::F_CU, LayerId::F_SILK, LayerId::EDGE],
            "bottom to top as seen from the front"
        );
        let t = BoardTheme::dark();
        assert_ne!(t.ink(LayerId::inner(1)), t.ink(LayerId::inner(2)), "inner layers differ");
        assert!(t.opacity(LayerId::F_CU, LayerId::F_CU) > t.opacity(LayerId::B_CU, LayerId::F_CU), "the active layer reads first");
        assert!(!BoardTheme::shown_by_default(LayerId::F_MASK) && BoardTheme::shown_by_default(LayerId::inner(3)), "default visibility");
    }
}
