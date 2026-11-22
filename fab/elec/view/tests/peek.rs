//! Held previews and placement without a window or simulation worker.

use fab_elec_view::live::{
    peek::{carries_peek, peek_drag_item, peek_rect, PeekState, PeekUpdate},
    probe::probe_drag_item,
};
use makepad_widgets_core::{dvec2, Rect, WidgetUid};

fn net() -> Option<(String, String)> { Some(("pa0".into(), "PA0".into())) }

#[test]
fn held_p_opens_a_net_on_press() {
    let mut state = PeekState::default();
    state.p_held = true;
    assert_eq!(state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), false), PeekUpdate::Opened);
    let open = state.open.as_ref().unwrap();
    assert_eq!((&*open.net, &*open.label), ("pa0", "PA0"));
    assert_eq!(open.pane, WidgetUid(1));
    assert_eq!(open.at, dvec2(30.0, 40.0));
    assert_eq!(open.opened, 1.0);
    assert!(state.p_used, "a P that opened a peek does not probe on its release");
    state.p_held = false;
    assert!(state.open.is_some(), "letting go of P does not release the pointer");
}

#[test]
fn a_tag_waits_350_ms_with_less_than_four_pixels_of_motion() {
    let mut state = PeekState::default();
    assert_eq!(state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), true), PeekUpdate::None);
    assert!(state.owns(WidgetUid(1)), "the hold owns its press before opening");
    state.move_to(dvec2(33.0, 42.0));
    assert_eq!(state.timeout(1.349), PeekUpdate::None);
    assert!(state.open.is_none());
    assert_eq!(state.timeout(1.35), PeekUpdate::Opened);
    assert_eq!(state.open.as_ref().unwrap().opened, 1.35);
    assert_eq!(state.timeout(2.0), PeekUpdate::None, "the timer opens only once");
}

#[test]
fn moving_four_pixels_cancels_the_hold_even_if_the_pointer_returns() {
    for distance in [4.0, 4.01, 10.0] {
        let mut state = PeekState::default();
        state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), true);
        state.move_to(dvec2(30.0 + distance, 40.0));
        state.move_to(dvec2(30.0, 40.0));
        assert_eq!(state.timeout(2.0), PeekUpdate::None);
        assert!(state.open.is_none());
        assert!(state.owns(WidgetUid(1)), "a cancelled hold cannot become the pane's click");
        state.release();
        assert!(!state.owns(WidgetUid(1)));
    }
}

#[test]
fn a_plain_net_press_and_an_empty_target_never_open() {
    let mut state = PeekState::default();
    state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), false);
    state.timeout(2.0);
    assert!(state.open.is_none());
    assert!(!state.owns(WidgetUid(1)));
    state.p_held = true;
    state.press(3.0, dvec2(30.0, 40.0), WidgetUid(1), None, true);
    state.timeout(4.0);
    assert!(state.open.is_none());
    assert!(!state.owns(WidgetUid(1)));
}

#[test]
fn motion_over_eight_pixels_carries_the_net_once() {
    let mut state = PeekState::default();
    state.p_held = true;
    state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), false);
    assert_eq!(state.move_to(dvec2(38.0, 40.0)), PeekUpdate::None);
    assert_eq!(state.move_to(dvec2(38.01, 40.0)), PeekUpdate::Drag("pa0".into()));
    assert!(state.dragging);
    assert_eq!(state.move_to(dvec2(200.0, 300.0)), PeekUpdate::None);
    assert_eq!(state.pointer, Some(dvec2(200.0, 300.0)));
    assert_eq!(state.leave(), PeekUpdate::None, "a drag can travel out of its source pane");
    assert!(state.open.is_some());
    assert_eq!(state.release(), PeekUpdate::Closed);
    assert!(!state.dragging);
}

#[test]
fn release_escape_and_pointer_loss_close_the_preview() {
    for end in [PeekState::release, PeekState::escape, PeekState::leave] {
        let mut state = PeekState::default();
        state.p_held = true;
        state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), false);
        assert_eq!(end(&mut state), PeekUpdate::Closed);
        assert!(state.open.is_none());
        assert_eq!(state.timeout(2.0), PeekUpdate::None);
    }
}

#[test]
fn released_or_cancelled_holds_cannot_open_from_a_late_timer() {
    for end in [PeekState::release, PeekState::escape, PeekState::leave] {
        let mut state = PeekState::default();
        state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), true);
        end(&mut state);
        assert_eq!(state.timeout(2.0), PeekUpdate::None);
        assert!(state.open.is_none());
    }
}

#[test]
fn another_pane_cannot_replace_the_held_net() {
    let mut state = PeekState::default();
    state.p_held = true;
    state.press(1.0, dvec2(30.0, 40.0), WidgetUid(1), net(), false);
    assert_eq!(state.press(2.0, dvec2(80.0, 90.0), WidgetUid(2), Some(("pb0".into(), "PB0".into())), false), PeekUpdate::None);
    assert_eq!(state.open.as_ref().unwrap().net, "pa0");
}

#[test]
fn placement_fits_every_edge_and_corner_without_covering_the_press() {
    for size in [dvec2(1024.0, 768.0), dvec2(320.0, 160.0), dvec2(120.0, 80.0)] {
        let window = Rect { pos: dvec2(17.0, 23.0), size };
        for x in [0.0, 1.0, size.x * 0.5, size.x - 1.0, size.x] {
            for y in [0.0, 1.0, size.y * 0.5, size.y - 1.0, size.y] {
                let at = window.pos + dvec2(x, y);
                let rect = peek_rect(window, at);
                assert!(rect.size.x > 0.0 && rect.size.y > 0.0);
                assert!(rect.pos.x >= window.pos.x && rect.pos.y >= window.pos.y);
                assert!(rect.pos.x + rect.size.x <= window.pos.x + size.x);
                assert!(rect.pos.y + rect.size.y <= window.pos.y + size.y);
                assert!(!rect.contains(at), "{rect:?} covers {at:?}");
            }
        }
    }
    let rect = peek_rect(Rect { pos: dvec2(0.0, 0.0), size: dvec2(1024.0, 768.0) }, dvec2(100.0, 100.0));
    assert_eq!(rect.pos, dvec2(112.0, 112.0));
    assert_eq!(rect.size, dvec2(320.0, 160.0));
}

#[test]
fn peek_drag_identity_is_distinct_from_a_probe() {
    assert_eq!(carries_peek(&[peek_drag_item("pa0")]), Some("pa0"));
    assert_eq!(carries_peek(&[probe_drag_item()]), None);
    assert_eq!(carries_peek(&[probe_drag_item(), peek_drag_item("pa0")]), Some("pa0"));
    assert_eq!(carries_peek(&[]), None);
}
