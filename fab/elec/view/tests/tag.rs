//! Probe identity layout without a window.

use fab_elec_view::live::tag::{layout, LaneSide, ProbeTag, TagKind, TagMetrics, TagState, TagStyle};
use makepad_widgets_core::{dvec2, vec4};

#[test]
fn pin_yields_before_title() {
    let title = "Long channel name";
    let pin = "controller.output.pin";
    let metrics = TagMetrics { title: dvec2(119.0, 16.0), pin: dvec2(147.0, 14.0) };
    for kind in [TagKind::Volt, TagKind::Amp, TagKind::Power] {
        let full = layout(title, pin, metrics, f64::INFINITY, kind);
        let mut saw_short_pin = false;
        let mut saw_short_title = false;
        for width in (0..=full.size.x.ceil() as usize).rev() {
            let tag = layout(title, pin, metrics, width as f64, kind);
            assert!(tag.size.x <= width as f64, "the tag stays within the requested width");
            for rect in [tag.glyph, tag.title, tag.pin] {
                assert!(rect.pos.x >= 0.0 && rect.pos.y >= 0.0, "rects start inside the tag at width {width}");
                assert!(rect.size.x >= 0.0 && rect.size.y >= 0.0, "rects have nonnegative sizes");
                assert!(rect.pos.x + rect.size.x <= tag.size.x, "rects stay inside the right edge at width {width}");
                assert!(rect.pos.y + rect.size.y <= tag.size.y, "rects stay inside the bottom edge");
            }
            assert!(tag.glyph.pos.x + tag.glyph.size.x <= tag.title.pos.x, "glyph and title never overlap");
            assert!(tag.title.pos.x + tag.title.size.x <= tag.pin.pos.x, "title and pin never overlap");
            if tag.title.size.x < metrics.title.x {
                saw_short_title = true;
                assert_eq!(tag.pin.size.x, 0.0, "the pin is gone before the title truncates");
            } else if tag.pin.size.x < metrics.pin.x && tag.pin.size.x > 0.0 {
                saw_short_pin = true;
            }
            if (width as f64) < full.glyph.size.x {
                assert_eq!(tag.title.size.x, 0.0, "below glyph width the title is empty");
            }
        }
        assert!(saw_short_pin && saw_short_title, "decreasing widths exercise both truncation stages");
        assert_eq!(layout(title, pin, metrics, -1.0, kind).size.x, 0.0, "negative room collapses safely");
    }
}

#[test]
fn state_keeps_the_same_geometry() {
    let tag = ProbeTag::from_label("Channel\npart.pin", "fallback");
    let metrics = TagMetrics { title: dvec2(49.0, 16.0), pin: dvec2(56.0, 14.0) };
    for kind in [TagKind::Volt, TagKind::Amp, TagKind::Power] {
        for side in [LaneSide::Sim, LaneSide::Real, LaneSide::Replay] {
            let style = TagStyle { hue: vec4(0.2, 0.7, 0.9, 1.0), kind, side, state: TagState::Rest };
            for width in [f64::INFINITY, 100.0, 35.0, 0.0] {
                let rest = tag.layout(metrics, width, style);
                for state in [TagState::Rest, TagState::Hover, TagState::Carried, TagState::Ghost] {
                    assert_eq!(tag.layout(metrics, width, TagStyle { state, ..style }), rest, "state cannot move or resize any part of the tag");
                }
            }
        }
    }
}
