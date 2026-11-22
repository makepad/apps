//! Firmware lane layout and lookup logic on a headless window; no pixel comparisons.
#![cfg(not(target_arch = "wasm32"))]

use fab_elec_design::{CursorSource, FwAt, TimeCursor};
use fab_elec_view::instruments::{
    fw_events::{fw_at, FwEvent, FwEvents, FwKind, FwSource, ScriptedFwEvents},
    fw_lane::{FwLane, TimeMap},
};
use makepad_widgets_core::*;
use std::{cell::Cell, rc::Rc, sync::Arc};

fn nodes(cx: &Cx, lane: &FwLane, terms: &str) -> Vec<a11y_tree::Node> {
    let query = a11y_tree::query::Query::parse(terms).expect("lookup grammar: the fixture query is valid");
    let mut out = Vec::new();
    lane.semantics_lookup(cx, &SemanticLookup::Terms(&query), &mut out);
    out.into_iter().map(|(_, _, node)| node).collect()
}

struct State {
    _window: WindowHandle,
    pass: DrawPass,
    list: DrawList2d,
    lane: FwLane,
}

struct Session(u32);
impl Drop for Session {
    fn drop(&mut self) { makepad_platform::makepad_hal::Hal::global().close(self.0); }
}

#[test]
fn firmware_columns_handlers_and_lookup() {
    use FwKind::*;
    use FwSource::*;
    let event = |seq, t, source, kind| FwEvent { seq, t, source, kind, site: None };
    let script = vec![
        event(1, 0.5, Image, Write { pin: 1, level: false }),
        event(2, 1.0, Trace, Enter { handler: 7 }),
        event(3, 2.25, Image, Write { pin: 1, level: true }),
        event(4, 2.28125, Image, Duty { pin: 2, level: 128 }),
        event(5, 2.25, Trace, Statement { statement: 17 }),
        event(6, 4.0, Trace, Statement { statement: 18 }),
        event(7, 5.0, Trace, Enter { handler: 9 }),
        event(8, 6.0, Trace, Exit { handler: 9 }),
        event(9, 7.0, Trace, Exit { handler: 7 }),
        event(10, 8.0, Image, Write { pin: 1, level: false }),
        event(11, 9.0, Trace, Statement { statement: 99 }),
    ];
    let mut store = ScriptedFwEvents::new(script.iter().rev().copied().collect());
    store.pin_names.insert(1, "LED1".into());
    store.pin_names.insert(2, "LED2".into());
    store.handler_names.insert(7, "timer".into());
    store.handler_names.insert(9, "receive".into());
    store.lost = 3;
    assert_eq!(store.len(), script.len(), "store retention: sorting keeps both sources");
    assert_eq!(store.first_at(2.25), 2, "partition: a tie starts at the first sequence");
    assert_eq!(store.get(2).map(|event| event.seq), Some(3), "sort order: equal times use sequence order");
    assert_eq!(store.get(3).map(|event| event.seq), Some(5), "sort order: sources share the combined timeline");
    assert_eq!(store.first_at(10.0), store.len(), "partition end: past the script returns len");
    assert_eq!(store.handler_at(0.5), None, "handler before Enter: no active span");
    assert_eq!(store.handler_at(1.0), Some((7, 1.0)), "handler at Enter: the span includes its start");
    assert_eq!(store.handler_at(3.0), Some((7, 1.0)), "handler inside span: statements do not change the active handler");
    assert_eq!(store.handler_at(5.0), Some((9, 5.0)), "nested handler: the innermost Enter wins");
    assert_eq!(store.handler_at(6.0), Some((7, 1.0)), "nested Exit: the outer Enter time is restored");
    assert_eq!(store.handler_at(7.0), None, "handler at Exit: the span excludes its end");
    assert_eq!(store.handler_at(7.5), None, "handler after Exit: no active span");
    assert_eq!(fw_at(&store, 2.25), FwAt { t: 2.25, event: Some(5), handler: Some(7) }, "cursor at exact time: the last tied sequence across both sources wins");
    assert_eq!(fw_at(&store, 2.28125), FwAt { t: 2.28125, event: Some(4), handler: Some(7) }, "cursor at image event: time order wins over sequence magnitude");
    assert_eq!(fw_at(&store, 3.0), FwAt { t: 3.0, event: Some(4), handler: Some(7) }, "cursor between events: the last recorded event and active handler persist");
    assert_eq!(fw_at(&store, 0.25), FwAt { t: 0.25, event: None, handler: None }, "cursor before first: neither an event nor a handler is invented");
    assert_eq!(fw_at(&store, 1.0), FwAt { t: 1.0, event: Some(2), handler: Some(7) }, "cursor at Enter: the handler is active immediately");
    assert_eq!(fw_at(&store, 5.5), FwAt { t: 5.5, event: Some(7), handler: Some(9) }, "cursor inside nested span: the inner handler is carried");
    assert_eq!(fw_at(&store, 6.0), FwAt { t: 6.0, event: Some(8), handler: Some(7) }, "cursor at nested Exit: the outer handler is restored");
    assert_eq!(fw_at(&store, 7.0), FwAt { t: 7.0, event: Some(9), handler: None }, "cursor at final Exit: the event remains and the handler ends");
    assert_eq!(fw_at(&store, 7.5), FwAt { t: 7.5, event: Some(9), handler: None }, "cursor outside handler: the last event needs no active handler");
    let cursor = TimeCursor { t: 3.0, playing: false, rate: 1.0, source: CursorSource::Sim, fw: None };
    assert_eq!(cursor.fw(), None, "cursor without firmware: absence is preserved");
    let at = fw_at(&store, cursor.t);
    let cursor = TimeCursor { fw: Some(at), ..cursor };
    assert_eq!(cursor.fw(), Some(at), "paused cursor: firmware belongs to its exact time");
    assert_eq!(TimeCursor { playing: true, ..cursor }.fw(), None, "playing cursor: stored firmware is not exposed");
    assert_eq!(TimeCursor { t: cursor.t.next_up(), ..cursor }.fw(), None, "moved cursor copy: even one bit of time change invalidates firmware");
    let zero = TimeCursor { t: 0.0, fw: Some(fw_at(&store, 0.0)), ..cursor };
    assert_eq!(TimeCursor { t: -0.0, ..zero }.fw(), None, "bit-equal cursor time: opposite signed zeros do not share firmware");
    let store = Arc::new(store);
    let map = TimeMap { x0: 64.25, width: 512.0, t0: 0.0, t1: 8.0 };
    let window_map = TimeMap { t0: 2.25, t1: 7.5, ..map };
    let window = Arc::new(ScriptedFwEvents::window(store.as_ref(), window_map.t0, window_map.t1));
    let mut state: Option<State> = None;
    let done = Rc::new(Cell::new(false));
    let finished = done.clone();
    let mut cx = Cx::new(Box::new(move |cx, event| {
        if finished.get() { return; }
        if matches!(event, Event::Startup) {
            let mut lane = cx.with_vm(|vm| {
                makepad_platform::script::script_mod(vm);
                makepad_draw::script_mod(vm);
                makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
                fab_elec_view::instruments::fw_lane::script_mod(vm);
                let value = vm.eval(script! {
                    use mod.prelude.widgets_internal.*
                    use mod.widgets.*
                    FwLane {}
                });
                FwLane::script_from_value(vm, value)
            });
            lane.set_events(cx, Some(store.clone()));
            let mut window = WindowHandle::new(cx);
            window.configure_window(cx, dvec2(640.0, 120.0), dvec2(0.0, 0.0), false, "Firmware layout".into());
            let pass = DrawPass::new(cx);
            window.set_pass(cx, &pass);
            pass.set_window_clear_color(cx, vec4(0.0, 0.0, 0.0, 1.0));
            state = Some(State { _window: window, pass, list: DrawList2d::new(cx), lane });
            cx.redraw_all();
        }
        if let Event::Draw(event) = event {
            let s = state.as_mut().expect("window lifecycle: startup precedes drawing");
            let mut draw = CxDraw::new(cx, event);
            let mut cx = Cx2d::new(&mut draw);
            cx.begin_pass(&s.pass, None);
            s.list.begin_always(&mut cx);
            cx.begin_root_turtle(dvec2(640.0, 120.0), Layout::flow_overlay());
            let view = Rect { pos: dvec2(0.0, 0.0), size: dvec2(640.0, 78.0) };
            s.lane.set_time_map(Some(map));
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            let marks = s.lane.drawn_marks();
            let inside: Vec<_> = script.iter().filter(|event| event.t >= map.t0 && event.t < map.t1).collect();
            assert_eq!(marks.len(), inside.len(), "resolved columns: every event in the window has a mark");
            for event in inside {
                let mark = marks.iter().find(|mark| mark.seq == event.seq).expect("resolved coverage: an in-window event is drawn");
                let x = map.x0 + (event.t - map.t0) / (map.t1 - map.t0) * map.width;
                assert!((mark.x - x).abs() <= 0.5, "time alignment: sequence {} is within half a point", event.seq);
                assert_eq!(mark.source, event.source, "source identity: each event stays on its own row");
                assert_eq!(mark.count, 1, "resolved count: a separate column contains one event");
            }
            for source in [Image, Trace] {
                let row: Vec<_> = marks.iter().filter(|mark| mark.source == source).collect();
                assert!(row.windows(2).all(|pair| (pair[0].t, pair[0].seq) < (pair[1].t, pair[1].seq)), "row order: {source:?} follows (t, seq)");
            }
            assert!(!marks.iter().any(|mark| mark.seq >= 10), "window end: events at and beyond t1 are absent");
            let group = nodes(&cx, &s.lane, "name=firmware role=group");
            assert_eq!(group.len(), 1, "card lookup: one firmware Group");
            assert_eq!(group[0].value, "3 lost", "lost before oldest: the card exposes the warning");
            assert!(!group[0].bounds.is_empty(), "card bounds: the drawn card has visible geometry");
            let image = nodes(&cx, &s.lane, "name~=image role=button");
            let trace = nodes(&cx, &s.lane, "name~=trace role=button");
            assert_eq!(image.len() + trace.len(), marks.len(), "mark lookup: exactly one Button per drawn mark");
            assert!(image.iter().any(|node| node.name == "image duty LED2 128 at 2281.25 ms"), "event labels: Duty names its kind, the pin, the level and its exact time");
            assert!(trace.iter().any(|node| node.name.starts_with("trace enter timer at ")), "event labels: Enter uses the handler name");
            assert!(image.iter().chain(&trace).all(|node| !node.states.contains(a11y_tree::States::SELECTED)), "no cursor: no firmware Button is selected");
            // A clip over the card while it draws: the lookup, taken during the draw, sees it.
            cx.push_clip_rect(Rect { size: dvec2(640.0, 48.0), ..view });
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            cx.pop_clip_rect();
            assert!(nodes(&cx, &s.lane, "name~=trace role=button").iter().all(|node| node.bounds.is_empty()), "clipping: the clipped trace row has no bounds");
            assert!(nodes(&cx, &s.lane, "name~=image role=button").iter().all(|node| !node.bounds.is_empty()), "clipping: the image row inside the clip keeps its bounds");

            for (t, source, label) in [(2.25, "trace", "stmt 17"), (3.0, "image", "duty LED2 128")] {
                s.lane.set_cursor(&mut cx, Some(fw_at(store.as_ref(), t)));
                s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
                let buttons = nodes(&cx, &s.lane, "name~=at role=button");
                let selected: Vec<_> = buttons.iter().filter(|node| node.states.contains(a11y_tree::States::SELECTED)).collect();
                assert_eq!(selected.len(), 1, "cursor selection at {t}: exactly one Button is selected");
                assert!(selected[0].name.starts_with(&format!("{source} {label} at ")), "cursor identity at {t}: the selected Button names the carried event");
                let value = format!("3 lost; at cursor: {label} in timer");
                assert_eq!(nodes(&cx, &s.lane, "name=firmware role=group")[0].value, value, "cursor summary at {t}: the Group names the event and handler alongside lost history");
                let mut group = a11y_tree::Node::default();
                s.lane.semantics(&cx, &mut group);
                assert_eq!(group.value, value, "cursor semantics at {t}: the widget and lookup Group have the same value");
            }

            s.lane.set_time_map(Some(TimeMap { width: 64.0, ..map }));
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            let marks = s.lane.drawn_marks();
            let pair = marks.iter().find(|mark| mark.seq == 3).expect("coalesced coverage: the first image event represents its column");
            assert_eq!(pair.count, 2, "pixel count: two subpixel image events share one mark");
            assert!(!marks.iter().any(|mark| mark.seq == 4), "pixel representative: the second image event has no separate mark");
            assert_eq!(marks.iter().find(|mark| mark.seq == 5).map(|mark| mark.count), Some(1), "mixed sources: the trace event in that column retains its own mark and count");
            let buttons = nodes(&cx, &s.lane, "name~=at role=button");
            let selected: Vec<_> = buttons.iter().filter(|node| node.states.contains(a11y_tree::States::SELECTED)).collect();
            assert_eq!(selected.len(), 1, "coalesced cursor: only the containing column is selected");
            assert!(selected[0].name.starts_with("image write LED1 1 at "), "coalesced cursor identity: the first mark represents the later selected event");
            assert_eq!(nodes(&cx, &s.lane, "name=firmware role=group")[0].value, "3 lost; at cursor: duty LED2 128 in timer", "coalesced cursor summary: the Group retains the actual event instead of its representative");

            s.lane.set_cursor(&mut cx, None);
            assert!(nodes(&cx, &s.lane, "name~=at role=button").iter().all(|node| !node.states.contains(a11y_tree::States::SELECTED)), "cleared cursor: no Button retains selection");
            assert_eq!(nodes(&cx, &s.lane, "name=firmware role=group")[0].value, "3 lost", "cleared cursor summary: the original Group value is restored");
            s.lane.set_cursor(&mut cx, Some(fw_at(store.as_ref(), 0.25)));
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            assert!(nodes(&cx, &s.lane, "name~=at role=button").iter().all(|node| !node.states.contains(a11y_tree::States::SELECTED)), "cursor before history: no Button is selected");
            assert_eq!(nodes(&cx, &s.lane, "name=firmware role=group")[0].value, "3 lost; at cursor: between events, no handler", "cursor before history summary: absent event and handler are explicit");
            s.lane.set_cursor(&mut cx, None);

            s.lane.set_time_map(Some(TimeMap { t0: 1.0, ..map }));
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            assert_eq!(nodes(&cx, &s.lane, "name=firmware role=group")[0].value, "", "lost after oldest: moving t0 past retained history removes the warning");
            assert!(!s.lane.drawn_marks().iter().any(|mark| mark.seq == 1), "window start: an event before t0 is absent");
            assert!(s.lane.drawn_marks().iter().any(|mark| mark.seq == 2), "window start: an event exactly at t0 is included");

            // Capture equals lane: use the same copied window on the same time map.
            // These event times occupy separate pixels within each source row.
            s.lane.set_events(&mut cx, Some(window.clone()));
            s.lane.set_time_map(Some(window_map));
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            for source in [Image, Trace] {
                let expected: Vec<_> = (0..window.len()).filter_map(|index| window.get(index))
                    .filter(|event| event.source == source).map(|event| (event.seq, event.t)).collect();
                let drawn: Vec<_> = s.lane.drawn_marks().iter().filter(|mark| mark.source == source)
                    .map(|mark| (mark.seq, mark.t)).collect();
                assert_eq!(drawn, expected, "capture equals lane: {source:?} draws the window in (t, seq) order");
            }

            s.lane.set_time_map(None);
            s.lane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::abs_rect(view));
            assert!(s.lane.drawn_marks().is_empty(), "no time map: the card draws without stale marks");
            assert_eq!(nodes(&cx, &s.lane, "name=firmware role=group")[0].value, "", "no time map: no stale lost warning");
            cx.end_pass_sized_turtle();
            s.list.end(&mut cx);
            cx.end_pass(&s.pass);
            finished.set(true);
        }
    }));
    cx.init_cx_os();
    use makepad_platform::makepad_hal::{backend_id, open_flags, Hal, OpenReq};
    let info = Hal::global().open(&OpenReq { backend: backend_id::HEADLESS, flags: open_flags::OFFSCREEN_WINDOWS, dpi_hint: 1.0, ..Default::default() });
    assert_ne!(info.session, 0, "headless session: the layout harness opens");
    let _session = Session(info.session);
    cx.hal_attach_session(info);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !done.get() {
        assert!(std::time::Instant::now() < deadline, "headless drawing: the single layout frame completes");
        assert!(cx.hal_session_turn(0, true), "headless lifecycle: the session stays open through the layout frame");
    }
    cx.hal_close_session();
}
