use super::super::touched_by;
use super::*;
use crate::model::{Action, ItemStyle, Model, Origin};
use makepad_widgets::cx_draw::CxDraw;

fn draw_pass(cx: &mut Cx, size: DVec2, pressure: f64) -> impl FnMut(&mut Cx, &mut BoardCanvas) {
    draw_pass_with(cx, size, pressure, |_, _| {})
}

fn draw_pass_with(cx: &mut Cx, size: DVec2, pressure: f64, mut after: impl FnMut(&mut Cx2d, &mut BoardCanvas))
    -> impl FnMut(&mut Cx, &mut BoardCanvas) {
    let pass = DrawPass::new(cx);
    let mut list = DrawList2d::new(cx);
    pass.set_size(cx, size);
    move |cx, canvas| {
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut cx = Cx2d::new(&mut draw);
        cx.begin_pass(&pass, None);
        list.begin_always(&mut cx);
        cx.begin_root_turtle(size, Layout::flow_down());
        // Use the scenario's frame pressure, independent of time spent compiling the last draw.
        canvas.pressure = pressure;
        let _ = canvas.draw_walk(&mut cx, &mut Scope::empty(), Walk::fixed(size.x, size.y));
        after(&mut cx, canvas);
        cx.end_pass_sized_turtle();
        list.end(&mut cx);
        cx.end_pass(&pass);
    }
}

#[test]
fn settled_canvas_culls_and_requests_only_one_frame_for_an_item_change() {
    let dir = std::env::temp_dir().join(format!("create-board-canvas-{}", std::process::id()));
    let (mut model, _) = Model::open(&dir).unwrap();
    let project = model.project().id;
    let board = model.act(Action::AddBoard { title: "Study".into(), project }, Origin::Tool).unwrap();
    let mut add = |x: i64, y: i64| {
        model
            .act(
                Action::AddItem {
                    board,
                    kind: Key::Number(kind::STICKY),
                    x: x * 64,
                    y: y * 64,
                    w: 36 * 64,
                    h: 32 * 64,
                    parent: None,
                    style: ItemStyle { text: Some("Ink".into()), ..Default::default() },
                },
                Origin::Tool,
            )
            .unwrap()
    };
    let a = add(0, 0);
    let b = add(41, 0);
    let distant = add(300, 8);
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.select_board(&mut cx, &model.state, Some(board), true);
    let size = dvec2(100., 80.);
    canvas.camera =
        Camera::new(Rect { pos: DVec2::default(), size }, CameraState { zoom: 1., pan_x: 0., pan_y: 0. });
    canvas.fit_pending = false;
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    let scene = canvas.scene.as_ref().unwrap();
    assert_eq!(scene.visible, [a, b], "rect query admits only the two on-screen cards");
    assert_eq!(scene.view.layers(&scene.visible), [0..2], "the bodies have a gap");
    assert_eq!(scene.layers, [0..2], "shadows lie under their layer's cards, so they never split it");
    assert!(scene.faces[&distant].layout.is_none(), "off-screen text was never laid out");
    let first = scene.faces[&a].layout.as_ref().unwrap().1.clone();
    let start = cx.frame_requests();
    canvas.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
    canvas.select_board(&mut cx, &model.state, Some(board), true);
    let unrelated =
        model.act(Action::AddTask { title: "Elsewhere".into(), text: String::new() }, Origin::Tool).unwrap();
    canvas.applied(&mut cx, &model.state, &touched_by(model.event(&unrelated).unwrap(), |id| model.event(id)), true);
    assert_eq!(cx.frame_requests(), start, "idle events and an unrelated edit request no frame");
    let change = model
        .act(
            Action::EditItem {
                item: b,
                style: ItemStyle { text: Some("Edit".into()), ..Default::default() },
                w: None,
                h: None,
                locked: None,
            },
            Origin::Tool,
        )
        .unwrap();
    canvas.applied(&mut cx, &model.state, &touched_by(model.event(&change).unwrap(), |id| model.event(id)), true);
    assert_eq!(cx.frame_requests(), start + 1, "a touched item requests one redraw");
    draw(&mut cx, &mut canvas);
    let after = cx.frame_requests();
    let scene = canvas.scene.as_ref().unwrap();
    assert_eq!(scene.faces[&b].text, "Edit");
    assert!(
        Rc::ptr_eq(&first, &scene.faces[&a].layout.as_ref().unwrap().1),
        "an unrelated layout survives the edit"
    );
    canvas.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
    assert_eq!(cx.frame_requests(), after, "redrawing did not arm an idle frame");
    canvas.zoom(&mut cx, dvec2(20., 20.), 2.);
    draw(&mut cx, &mut canvas);
    assert!(
        Rc::ptr_eq(&first, &canvas.scene.as_ref().unwrap().faces[&a].layout.as_ref().unwrap().1),
        "zoom preserves canvas-unit layout"
    );
    // Dispatch the same window-space pointer events as the native input path.
    let win = WindowId(1, 1);
    let start = canvas.camera.local_to_screen(canvas.camera.world_to_local(dvec2(12., 12.)));
    cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, win));
    canvas.input(&mut cx, &Event::MouseDown(MouseDownEvent {
        abs: start,
        button: MouseButton::PRIMARY,
        window_id: win,
        modifiers: Default::default(),
        handled: Default::default(),
        time: 1.,
    }));
    draw(&mut cx, &mut canvas);
    assert_eq!(canvas.scene.as_ref().unwrap().lifted, [a], "the pressed card moved into the overlay");
    let base = canvas.recording.as_ref().unwrap().id();
    let overlay = canvas.overlay.as_ref().unwrap().id();
    let base_gen = cx.draw_lists[base].recording_gen;
    for step in [6., 12.] {
        let gen = cx.draw_lists[overlay].recording_gen;
        canvas.input(&mut cx, &Event::MouseMove(MouseMoveEvent {
            abs: start + dvec2(step, 0.),
            lock_delta: Default::default(),
            window_id: win,
            modifiers: Default::default(),
            time: 1. + step * 0.01,
            handled: Default::default(),
        }));
        draw(&mut cx, &mut canvas);
        assert_eq!(cx.draw_lists[base].recording_gen, base_gen, "drag motion reused the board recording");
        assert_ne!(cx.draw_lists[overlay].recording_gen, gen, "only the overlay was recorded");
    }
    let pointer = start + dvec2(12., 10.);
    canvas.input(&mut cx, &Event::MouseMove(MouseMoveEvent {
        abs: pointer, lock_delta: Default::default(), window_id: win, modifiers: Default::default(),
        time: 1.2, handled: Default::default(),
    }));
    let top = |c: &BoardCanvas| c.gesture.moving().unwrap().rect(&a).unwrap().y0;
    assert_eq!(top(&canvas), 5., "the distant card cannot pull the top while outside the camera");
    let original = canvas.camera;
    canvas.pan(&mut cx, original.pan - dvec2(540., 0.));
    assert_eq!(top(&canvas), 8., "panning during the drag snaps to the newly visible neighbour without a mouse move");
    canvas.change_camera(&mut cx, original);
    assert_eq!(top(&canvas), 5., "panning back removes the former neighbour's pull");
    canvas.zoom(&mut cx, pointer, 0.2);
    assert_eq!(top(&canvas), 8., "zooming out admits the neighbour and preserves the world point held at the pointer");
    canvas.change_camera(&mut cx, original);
    cx.fingers.first_mouse_button = None;
    let actions = cx.capture_actions(|cx| {
        canvas.input(cx, &Event::MouseUp(MouseUpEvent {
            abs: start + dvec2(12., 0.),
            button: MouseButton::PRIMARY,
            window_id: win,
            modifiers: Default::default(),
            time: 1.3,
        }));
    });
    let acts: Vec<_> = actions.iter()
        .filter_map(|a| a.as_widget_action()?.action.downcast_ref::<BoardCanvasAction>())
        .collect();
    assert!(matches!(acts.as_slice(), [BoardCanvasAction::Act(Action::MoveItems { moves, .. })] if moves.len() == 1));
    let BoardCanvasAction::Act(action) = acts[0] else { unreachable!() };
    let moved = model.act(action.clone(), Origin::Ui).unwrap();
    let ids = touched_by(model.event(&moved).unwrap(), |id| model.event(id));
    canvas.applied(&mut cx, &model.state, &ids, true);
    draw(&mut cx, &mut canvas);
    assert!(canvas.scene.as_ref().unwrap().lifted.is_empty());
    let settled = cx.frame_requests();
    canvas.input(&mut cx, &Event::Signal);
    assert_eq!(cx.frame_requests(), settled, "the finished drag is idle");
    let view = &canvas.scene.as_ref().unwrap().view;
    let r = view.get(&a).unwrap().rect;
    let east = canvas.camera.local_to_screen(canvas.camera.world_to_local(dvec2(r.x1, (r.y0 + r.y1) * 0.5)));
    cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, win));
    canvas.input(&mut cx, &Event::MouseDown(MouseDownEvent {
        abs: east, button: MouseButton::PRIMARY, window_id: win, modifiers: Default::default(),
        handled: Default::default(), time: 2.,
    }));
    draw(&mut cx, &mut canvas);
    let base_gen = cx.draw_lists[base].recording_gen;
    let layout = canvas.scene.as_ref().unwrap().faces[&a].layout.as_ref().unwrap().1.clone();
    let neighbour = canvas.scene.as_ref().unwrap().faces[&b].layout.as_ref().unwrap().1.clone();
    let modifiers = KeyModifiers { alt: true, ..Default::default() };
    for step in [8., 16.] {
        let gen = cx.draw_lists[overlay].recording_gen;
        canvas.input(&mut cx, &Event::MouseMove(MouseMoveEvent {
            abs: east + dvec2(step, 0.), lock_delta: Default::default(), window_id: win, modifiers,
            time: 2. + step * 0.01, handled: Default::default(),
        }));
        draw(&mut cx, &mut canvas);
        assert_eq!(cx.draw_lists[base].recording_gen, base_gen, "resize motion reused the board recording");
        assert_ne!(cx.draw_lists[overlay].recording_gen, gen, "resize motion recorded the overlay");
    }
    assert!(!Rc::ptr_eq(&layout, &canvas.scene.as_ref().unwrap().faces[&a].layout.as_ref().unwrap().1),
        "the resized item's text rewrapped during the gesture");
    assert!(Rc::ptr_eq(&neighbour, &canvas.scene.as_ref().unwrap().faces[&b].layout.as_ref().unwrap().1),
        "a neighbouring item kept its layout throughout the resize");
    cx.fingers.first_mouse_button = None;
    let actions = cx.capture_actions(|cx| {
        canvas.input(cx, &Event::MouseUp(MouseUpEvent {
            abs: east + dvec2(16., 0.), button: MouseButton::PRIMARY, window_id: win, modifiers, time: 2.3,
        }));
    });
    let edits: Vec<_> = actions.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<BoardCanvasAction>()).collect();
    let [BoardCanvasAction::Act(action @ Action::SetFields { .. })] = edits.as_slice() else { panic!("one resize edit") };
    let count = |m: &Model| makepad_town_core::log::Log::open(&m.home.town_log_path(&m.open_id())).unwrap().0.events().len();
    let n = count(&model);
    model.act(action.clone(), Origin::Ui).unwrap();
    assert_eq!(count(&model), n + 1, "the resize writes exactly one event");
    drop(model);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_future_constructor_and_unfinished_design_have_readable_cards() {
    use crate::board::tests::History;
    use makepad_town_core::{entity::field, event::{Content, Value}};
    let mut h = History::new();
    let board = h.board();
    let mut scene = Scene::new(&h.state, board);
    let (id, touched) = h.push(Content::Entity { copied_from: vec![], kind: Key::Name("FutureCard".into()), derive: None,
        project: None, fields: vec![(field::PARENT, Value::Id(board))] });
    assert!(scene.apply(&h.state, &touched, &mut Gesture::default()));
    let card = scene.view.get(&id).unwrap();
    assert!(supported(&card.kind) && rect(card.rect).size.x > 0. && rect(card.rect).size.y > 0.);
    assert_eq!(scene.faces[&id].text, "unknown kind FutureCard");

    let (text, touched) = h.push(Content::Entity { copied_from: vec![], kind: Key::Number(kind::TEXT_BLOCK), derive: None, project: None,
        fields: vec![(field::PARENT, Value::Id(board)), (field::W, Value::Int(100 * 64)),
            (field::H, Value::Int(32 * 64)), (field::TEXT, Value::Text("Text".into()))] });
    assert!(scene.apply(&h.state, &touched, &mut Gesture::default()));
    let (sticky, touched) = h.push(Content::Entity { copied_from: vec![], kind: Key::Number(kind::STICKY), derive: None, project: None,
        fields: vec![(field::PARENT, Value::Id(board)), (field::W, Value::Int(96 * 64)),
            (field::H, Value::Int(40 * 64)), (field::TEXT, Value::Text("Plan the demo".into()))] });
    assert!(scene.apply(&h.state, &touched, &mut Gesture::default()));
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.scene = Some(scene);
    let size = dvec2(160., 64.);
    let mut draw = draw_pass(&mut cx, size, 100.);
    draw(&mut cx, &mut canvas);
    assert!(canvas.scene.as_ref().unwrap().faces[&id].layout.is_some(),
        "frame pressure cannot hide the only label identifying the card");
    assert!(canvas.scene.as_ref().unwrap().faces[&text].layout.is_some(),
        "a readable text block still shows its text under frame pressure");
    assert!(canvas.scene.as_ref().unwrap().faces[&sticky].detail.text,
        "a sticky keeps its saved text at reading size after the editor closes");

    let (design, touched) = h.push(Content::Entity { copied_from: vec![], kind: Key::Number(kind::DESIGN), derive: None, project: None,
        fields: vec![(field::PARENT, Value::Id(board)), (field::Z, Value::Key(vec![2])), (field::W, Value::Int(390 * 64)),
            (field::H, Value::Int(844 * 64)), (field::TITLE, Value::Text("Welcome".into())),
            (field::SRC, Value::Text("arbitrary unfinished Splash{".into()))] });
    canvas.applied(&mut cx, &h.state, &touched, true);
    draw(&mut cx, &mut canvas);
    canvas.sync_media(&mut cx, [1; 32], Path::new("."), |_| (false, false, None));
    draw(&mut cx, &mut canvas);
    advance_design(&mut cx, &mut canvas, &mut draw, |c| matches!(c.design_state(design), PictureState::Failed { .. }));
    let PictureState::Failed { message } = canvas.design_state(design) else { unreachable!() };
    assert!(!message.is_empty() && canvas.design_message(design) == Some(message.as_str()), "unfinished Splash shows its error");
    let scene = canvas.scene.as_ref().unwrap();
    let face = &scene.faces[&design];
    assert!(face.detail.body && face.layout.is_some(), "the design frame and its label draw");
    assert_eq!(face.text, "Welcome · phone");
    let p = dvec2(130., 50.);
    let hit = scene.pick(p);
    assert_eq!(hit, Some(design), "the frame interior can be picked");
    canvas.gesture.press(&scene.view, hit, p, p, false, None, false);
    assert!(canvas.gesture.selection.ids.contains(&design));
    let to = p + dvec2(14., 7.);
    canvas.gesture.motion(&scene.view, to, to, 1., true, false);
    assert!(matches!(canvas.gesture.release(&scene.view), Some(Action::MoveItems { moves, .. })
        if moves == [(design, 14 * 64, 7 * 64)]), "dragging a design emits an ordinary move");
}

#[test]
fn canvas_draws_arriving_section_children_and_a_child_surviving_its_section() {
    use crate::board::tests::{change, item, set, History};
    use makepad_town_core::event::{Content, Value};

    let mut h = History::new();
    let source = h.board();
    let board = h.board();
    let section = h.push(item(kind::SECTION, source, [0, 0, 95, 75], Some(1))).0;
    let shape = h.push(item(kind::SHAPE, section, [5, 40, 24, 20], Some(2))).0;
    let mut note = item(kind::STICKY, section, [5, 5, 70, 32], Some(3));
    let Content::Entity { fields, .. } = &mut note else { unreachable!() };
    fields.extend([(field::TEXT, Value::Text("Peer note".into())), (field::BOARD, Value::Id(source))]);
    let child = h.branch(&h.peer, note);

    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    let size = dvec2(100., 80.);
    canvas.camera = Camera::new(Rect { pos: DVec2::default(), size }, CameraState { zoom: 1., pan_x: 0., pan_y: 0. });
    canvas.scene = Some(Scene::new(&h.state, board));
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);

    // The receiving fold sees the peer's note. The moving writer's tip stays on its own branch,
    // so neither its transfer nor its deletion names that note.
    h.receive(child.clone());
    assert!(!canvas.scene.as_mut().unwrap().apply(&h.state, &touched_by(&child, |_| None), &mut canvas.gesture));
    let mut draw_change = |canvas: &mut BoardCanvas, state: &State, touched: &[Id]| {
        assert!(canvas.scene.as_mut().unwrap().apply(state, touched, &mut canvas.gesture));
        canvas.request(&mut cx);
        draw(&mut cx, canvas);
    };
    let (_, touched) = h.push(set(vec![
        change(section, field::PARENT, Value::Id(board)),
        change(section, field::BOARD, Value::Id(board)),
        change(shape, field::BOARD, Value::Id(board)),
    ]));
    draw_change(&mut canvas, &h.state, &touched);
    let scene = canvas.scene.as_ref().unwrap();
    assert_eq!(scene.visible, [section, shape, child.id], "both children draw on the destination board");
    assert_eq!(scene.view.get(&child.id).unwrap().parent_section, Some(section));
    assert_eq!(scene.faces[&child.id].text, "Peer note");
    let layout = scene.faces[&child.id].layout.as_ref().unwrap().1.clone();

    let (_, touched) = h.push(set(vec![
        change(section, field::DELETED, Value::Bool(true)),
        change(shape, field::DELETED, Value::Bool(true)),
    ]));
    draw_change(&mut canvas, &h.state, &touched);
    let scene = canvas.scene.as_ref().unwrap();
    assert_eq!(scene.visible, [child.id], "only the peer's unseen child survives the delete");
    assert_eq!(scene.view.get(&child.id).unwrap().parent_section, None, "the child hangs from the board");
    assert_eq!(scene.faces.len(), 1, "the deleted faces leave the canvas");
    assert!(Rc::ptr_eq(&layout, &scene.faces[&child.id].layout.as_ref().unwrap().1));
}

#[test]
fn one_slow_draw_never_hides_detail_and_a_still_board_recovers_it() {
    // A first draw that compiles for two seconds at 60 Hz costs 120 frame budgets.
    let (after_spike, again) = pressure_after(1., 120., 0.);
    assert_eq!(after_spike, 1., "a single slow draw is a hiccup and hides no text");
    assert!(!again, "a board at full detail asks for no extra frame");
    let (heavy, again) = pressure_after(1., 16., 16.);
    assert_eq!(heavy, 4., "two slow draws in a row raise the pressure");
    assert!(!again, "a hot draw asks for nothing, so a heavy board doesn't keep redrawing");
    let (mut pressure, mut frames) = (heavy, 0);
    loop {
        let (next, again) = pressure_after(pressure, 0.5, 0.5);
        pressure = next;
        if !again {
            break;
        }
        frames += 1;
        assert!(frames < 100, "recovery ends");
    }
    assert_eq!(pressure, 1., "a board that stopped changing recovers full detail by itself");
}

#[test]
fn placement_tools_centre_clicks_size_drags_and_cancel_on_escape() {
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    let mut draw = draw_pass(&mut cx, dvec2(640., 480.), 1.);
    draw(&mut cx, &mut canvas);
    let win = WindowId(1, 1);
    let point = dvec2(240., 180.);
    for (tool, kind, size) in [
        (BoardTool::Sticky, kind::STICKY, (220, 160)), (BoardTool::Text, kind::TEXT_BLOCK, (180, 40)),
        (BoardTool::Shape, kind::SHAPE, (144, 96)), (BoardTool::Section, kind::SECTION, (480, 320)),
    ] {
        for (drag, cancel) in [(false, false), (true, false), (true, true)] {
            canvas.arm(&mut cx, tool);
            cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, win));
            let actions = cx.capture_actions(|cx| {
                canvas.input(cx, &Event::MouseDown(MouseDownEvent {
                    abs: point, button: MouseButton::PRIMARY, window_id: win, modifiers: Default::default(),
                    handled: Default::default(), time: 1.,
                }));
                assert!(canvas.ghost.is_none(), "nothing is drawn on press");
                let wobble = |canvas: &mut BoardCanvas, cx: &mut Cx, abs, time| canvas.input(cx, &Event::MouseMove(MouseMoveEvent {
                    abs, lock_delta: Default::default(), window_id: win, modifiers: Default::default(),
                    handled: Default::default(), time,
                }));
                wobble(&mut canvas, cx, point + dvec2(1., -1.), 1.01);
                assert!(canvas.ghost.is_none(), "a move inside the slop still draws nothing");
                let end = if drag { point - dvec2(40., 32.) } else { point };
                if drag {
                    let first = point - dvec2(4., 3.);
                    wobble(&mut canvas, cx, first, 1.05);
                    let ghost = canvas.ghost.as_ref().expect("the first move past the slop starts the drag");
                    let size = dvec2(ghost.rect.x1 - ghost.rect.x0, ghost.rect.y1 - ghost.rect.y0);
                    assert!(size.x <= 4. && size.y <= 3. && ghost.rect.x1 == point.x && ghost.rect.y1 == point.y,
                        "the first drag frame covers only what the pointer swept, from the press point: {size:?}");
                    canvas.input(cx, &Event::MouseMove(MouseMoveEvent {
                        abs: end, lock_delta: Default::default(), window_id: win, modifiers: Default::default(),
                        handled: Default::default(), time: 1.1,
                    }));
                    assert!(canvas.ghost.is_some(), "show the dragged rectangle before release");
                }
                if cancel { canvas.input(cx, &Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() })); }
                cx.fingers.first_mouse_button = None;
                canvas.input(cx, &Event::MouseUp(MouseUpEvent {
                    abs: end, button: MouseButton::PRIMARY, window_id: win, modifiers: Default::default(), time: 1.2,
                }));
            });
            let items: Vec<_> = actions.iter().filter_map(|a| match a.as_widget_action()?.action.downcast_ref()? {
                BoardCanvasAction::Place(item) => Some(item),
                _ => None,
            }).collect();
            assert_eq!(canvas.tool, BoardTool::Select);
            assert!(canvas.ghost.is_none());
            if cancel { assert!(items.is_empty()); continue }
            let [item] = items[..] else { panic!("placement emits exactly one item") };
            assert_eq!(item.kind, kind);
            let (x, y, w, h) = if drag { (200, 148, 40, 32) } else { (240 - size.0 / 2, 180 - size.1 / 2, size.0, size.1) };
            assert_eq!((item.x, item.y, item.w, item.h), (x * 64, y * 64, w * 64, h * 64));
        }
    }
}

#[test]
fn fit_centres_a_small_board_at_one_to_one_and_shrinks_a_large_board() {
    use crate::board::tests::{item, History};
    let mut h = History::new();
    let board = h.board();
    h.push(item(kind::STICKY, board, [300, -400, 220, 160], None));
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.select_board(&mut cx, &h.state, Some(board), true);
    let view = Rect { pos: dvec2(100., 50.), size: dvec2(600., 480.) };
    let fit = canvas.fit(view).unwrap();
    assert_eq!(fit.scale, 1.);
    let point = |p| fit.local_to_screen(fit.world_to_local(p));
    assert_eq!(point(dvec2(520., -240.)) - point(dvec2(300., -400.)), dvec2(220., 160.));
    let world = canvas.scene.as_ref().unwrap().bounds().unwrap();
    assert!((point(world.center()) - (view.center() + dvec2(0., 20.))).length() < 0.001);
    let small = canvas.fit(Rect { size: dvec2(160., 140.), ..view }).unwrap();
    assert!(small.scale < 1.);
    assert!(world.size.x * small.scale <= 128. && world.size.y * small.scale <= 68.);
}

#[test]
fn toolbar_drop_uses_the_preview_and_cancel_discards_it() {
    use crate::board::tests::{item, History};
    use std::sync::Arc;
    let mut h = History::new();
    let board = h.board();
    let section = h.push(item(kind::SECTION, board, [10, 10, 400, 300], None)).0;
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.select_board(&mut cx, &h.state, Some(board), true);
    let mut draw = draw_pass(&mut cx, dvec2(640., 480.), 1.);
    draw(&mut cx, &mut canvas);
    let toolbar = WidgetRef::empty();
    let abs = canvas.camera.local_to_screen(canvas.camera.world_to_local(dvec2(122., 115.)));
    let items = Arc::new(vec![DragItem::String { value: "Sticky".into(), internal_id: Some(live_id!(board_creation_tool)) }]);
    for cancel in [true, false] {
        canvas.toolbar_drag = Some((BoardTool::Sticky, toolbar.clone()));
        canvas.input(&mut cx, &Event::Drag(DragEvent { window_id: WindowId(1, 1), abs, items: items.clone(),
            modifiers: Default::default(), handled: Default::default(), response: Arc::new(std::sync::Mutex::new(DragResponse::None)),
        }));
        let ghost = canvas.ghost.as_ref().expect("the tool has a preview before release");
        assert_eq!(ghost.item.parent, Some(section));
        assert_eq!(ghost.item.x, 10 * 64, "the ghost snaps to the containing section's left edge");
        let expected = (ghost.item.x, ghost.item.y, ghost.item.w, ghost.item.h);
        let actions = cx.capture_actions(|cx| {
            if cancel { canvas.input(cx, &Event::DragEnd); }
            canvas.input(cx, &Event::Drop(DropEvent { window_id: WindowId(1, 1), abs, items: items.clone(),
                modifiers: Default::default(), handled: Default::default(),
            }));
        });
        let placed: Vec<_> = actions.iter().filter_map(|a| match a.as_widget_action()?.action.downcast_ref()? {
            BoardCanvasAction::Place(item) => Some(item),
            _ => None,
        }).collect();
        if cancel { assert!(placed.is_empty()); } else {
            let [item] = placed[..] else { panic!("one item per drop") };
            assert_eq!((item.x, item.y, item.w, item.h), expected);
            assert_eq!(item.parent, Some(section));
        }
        assert!(canvas.ghost.is_none());
    }
}

#[test]
fn a_stickys_editor_owns_typing_and_its_one_undo_on_both_devices() {
    use crate::board::gesture::BoardHistory;
    use crate::model::tests::{exchange, shared_town};
    use makepad_platform::studio::StudioToApp;

    let (_homes, mut a, mut b) = shared_town("board-typing");
    let project = a.project().id;
    let mut history = BoardHistory::default();
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| {
        let template = script_eval!(vm, {mod.widgets.BoardCanvas{}});
        BoardCanvas::script_from_value(vm, template)
    });
    canvas.edit_context(&mut cx, true, true);
    let mut draw = draw_pass(&mut cx, dvec2(640., 480.), 1.);
    draw(&mut cx, &mut canvas);
    canvas.arm(&mut cx, BoardTool::Sticky);
    let win = WindowId(1, 1);
    let point = dvec2(240., 180.);
    cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, win));
    canvas.input(&mut cx, &Event::MouseDown(MouseDownEvent { abs: point, button: MouseButton::PRIMARY,
        window_id: win, modifiers: Default::default(), handled: Default::default(), time: 1. }));
    cx.fingers.first_mouse_button = None;
    let actions = cx.capture_actions(|cx| canvas.input(cx, &Event::MouseUp(MouseUpEvent {
        abs: point, button: MouseButton::PRIMARY, window_id: win, modifiers: Default::default(), time: 1.1,
    })));
    let placement = actions.iter().find_map(|a| match a.as_widget_action()?.action.downcast_ref()? {
        BoardCanvasAction::Place(item) => Some(item),
        _ => None,
    }).unwrap();
    let (board, step) = placement.place(project, None, |action| Some(a.act(action, Origin::Ui).unwrap())).unwrap();
    let id = *step.last().unwrap();
    history.record_step(step);
    canvas.select_board(&mut cx, &a.state, Some(board), true);
    canvas.placed(&mut cx, id);
    let key = |key_code| Event::KeyDown(KeyEvent { key_code, ..Default::default() });
    let text = |s: &str| Event::TextInput(TextInputEvent { input: s.into(), ..Default::default() });
    let settle_focus = |cx: &mut Cx| {
        cx.dispatch_studio_msg(StudioToApp::KeyUp(KeyEvent::default()), win, DVec2::default());
    };
    let send = |canvas: &mut BoardCanvas, cx: &mut Cx, events: Vec<Event>| {
        let actions = cx.capture_actions(|cx| {
            for event in events { canvas.handle_event(cx, &event, &mut Scope::empty()); }
        });
        actions.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<BoardCanvasAction>().cloned())
            .collect::<Vec<_>>()
    };

    settle_focus(&mut cx);
    assert_eq!(canvas.gesture.selection.ids, [id].into());
    assert!(cx.has_key_focus_from_user(canvas.editor.area()));
    assert!(send(&mut canvas, &mut cx, vec![text("Plan")]).is_empty(), "typing before the first frame stays in the editor");
    draw(&mut cx, &mut canvas);
    let caret = |canvas: &BoardCanvas, cx: &mut Cx| {
        let input = canvas.editor.borrow::<TextInput>().unwrap();
        let mut colour = [0f32; 4];
        input.draw_cursor.draw_vars.get_uniform(cx, live_id!(color), &mut colour);
        let ink = input.draw_text.color;
        (colour, [ink.x, ink.y, ink.z, ink.w])
    };
    let (colour, ink) = caret(&canvas, &mut cx);
    let on_sticky = look::contrast(vec4(colour[0], colour[1], colour[2], 1.), look::opaque(canvas.sticky_1));
    assert!(colour == ink && on_sticky >= 7., "the caret takes the default sticky's ink, at {on_sticky:.1}:1: {colour:?}");
    let events = vec![text(" the demo"), key(KeyCode::ReturnKey), key(KeyCode::Backspace), key(KeyCode::Escape)];
    let actions = send(&mut canvas, &mut cx, events);
    let [BoardCanvasAction::CommitText { board: owner, action }] = &actions[..] else { panic!("only a text edit was expected") };
    assert_eq!(*owner, board);
    assert_eq!(action, &Action::EditItem {
        item: id, style: ItemStyle { text: Some("Plan the demo".into()), ..Default::default() },
        w: None, h: None, locked: None,
    });
    assert!(!canvas.editor.visible());
    history.record(a.act(action.clone(), Origin::Ui).unwrap());
    canvas.applied(&mut cx, &a.state, &[id], true);
    exchange(&mut a, &mut b);
    assert_eq!(b.state.entities[&id].text(field::TEXT), "Plan the demo");

    // The editor takes the item's text alignment from a script eval; a script error there
    // only logs, and the alignment silently falls back.
    cx.with_vm(|vm| vm.capture_errors_nested());
    canvas.open_text(&mut cx, id, false);
    let errors = cx.with_vm(|vm| { let errors = vm.take_errors(); vm.capture_errors_restore(); errors });
    assert!(errors.is_empty(), "opening the text editor raises no script errors:\n{}", errors.join("\n"));
    settle_focus(&mut cx);
    assert!(send(&mut canvas, &mut cx, vec![key(KeyCode::Escape)]).is_empty(), "unchanged text writes nothing");
    history.step(false, |undo| Some(a.act(undo, Origin::Ui).unwrap()));
    exchange(&mut a, &mut b);
    for model in [&a, &b] {
        assert_eq!(model.state.entities[&id].text(field::TEXT), "");
        assert!(BoardView::build(&model.state, board).get(&id).is_some(), "undoing text keeps the sticky");
    }
    canvas.applied(&mut cx, &a.state, &[id], true);
    canvas.open_text(&mut cx, id, false);
    settle_focus(&mut cx);
    assert!(send(&mut canvas, &mut cx, vec![key(KeyCode::Backspace), key(KeyCode::Escape)]).is_empty(),
        "Backspace in an empty editor leaves the sticky alone");
    let dark = Action::EditItem { item: id, style: ItemStyle { colour: Some(0x1f2a33), ..Default::default() },
        w: None, h: None, locked: None };
    a.act(dark, Origin::Ui).unwrap();
    canvas.applied(&mut cx, &a.state, &[id], true);
    canvas.open_text(&mut cx, id, false);
    draw(&mut cx, &mut canvas);
    let (colour, ink) = caret(&canvas, &mut cx);
    assert!(colour == ink && colour[..3].iter().sum::<f32>() > 2.4, "a light caret in a dark sticky: {colour:?}");
    send(&mut canvas, &mut cx, vec![key(KeyCode::Escape)]);

    let block = NewItem { kind: kind::TEXT_BLOCK, ..placement.clone() };
    let (_, step) = block.place(project, Some(board), |action| Some(a.act(action, Origin::Ui).unwrap())).unwrap();
    let id = step[0];
    canvas.applied(&mut cx, &a.state, &[id], true);
    canvas.placed(&mut cx, id);
    settle_focus(&mut cx);
    assert_eq!(canvas.editor.text(), "Text");
    assert!(send(&mut canvas, &mut cx, vec![text("Heading")]).is_empty());
    assert_eq!(canvas.editor.text(), "Heading", "typing replaces the selected default text");
    canvas.edit_context(&mut cx, true, false);
    assert!(!canvas.editor.visible(), "losing write access closes the draft");
    assert_eq!(a.state.entities[&id].text(field::TEXT), "Text");
}

#[test]
fn board_canvas_draws_image_placeholder_and_emits_open_media_on_double_click() {
    use crate::board::tests::{item, History};
    use makepad_platform::studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp};
    use makepad_town_core::{blob, event::Content};
    let mut history = History::new();
    let board = history.board();
    let mut content = item(kind::IMAGE, board, [20, 40, 200, 100], None);
    let Content::Entity { fields, .. } = &mut content else { unreachable!() };
    let large = 1024 * 1024 + 1;
    fields.extend([(field::BLOB, Value::Blob { size: 50, root: [1; 32] }),
        (field::POSTER, Value::Blob { size: large, root: [2; 32] }), (field::MEDIA, Value::Text("video/mp4".into())),
        (field::DURATION, Value::Int(65432))]);
    let id = history.push(content).0;
    let mut surface = WidgetRef::empty();
    let target = surface.clone();
    let heard = Rc::new(std::cell::RefCell::new(Vec::new()));
    let actions = heard.clone();
    let mut cx = Cx::new(Box::new(move |cx, event| {
        if let Event::Actions(batch) = event {
            actions.borrow_mut().extend(batch.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<BoardCanvasAction>().cloned()));
        }
        target.handle_event(cx, event, &mut Scope::empty());
    }));
    cx.init_cx_os();
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
        let template = script_eval!(vm, {mod.widgets.BoardCanvas{}});
        surface.script_apply(vm, &Apply::New, &mut Scope::empty(), template);
    });
    let size = dvec2(420., 240.);
    let mut paint = draw_pass(&mut cx, size, 1.);
    let status = |canvas: &mut BoardCanvas, cx: &mut Cx, wanted| {
        canvas.sync_media(cx, [8; 32], Path::new("blobs"), |_| (false, wanted, None));
    };
    let get_point = {
        let mut canvas = surface.borrow_mut::<BoardCanvas>().unwrap();
        canvas.select_board(&mut cx, &history.state, Some(board), true);
        canvas.edit_context(&mut cx, true, true);
        canvas.camera = Camera::new(Rect { pos: DVec2::default(), size }, CameraState { zoom: 1., pan_x: 0., pan_y: 0. });
        canvas.fit_pending = false;
        status(&mut canvas, &mut cx, false);
        paint(&mut cx, &mut canvas);
        let face = canvas.scene.as_ref().unwrap().faces[&id].media.as_ref().unwrap();
        assert_eq!(face.placeholder.text, "Get", "a large absent poster offers a fetch");
        assert_eq!(face.duration.text, "1:05", "the clip length is drawn on the card");
        canvas.camera.local_to_screen(face.get_rect.unwrap().center())
    };
    let click = |cx: &mut Cx, point: DVec2, time| {
        let win = WindowId(1, 1);
        let button_raw_bits = MouseButton::PRIMARY.bits();
        cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown { x: point.x, y: point.y, time,
            button_raw_bits, modifiers: Default::default() }), win, DVec2::default());
        cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp { x: point.x, y: point.y, time: time + 0.02,
            button_raw_bits, modifiers: Default::default() }), win, DVec2::default());
    };
    click(&mut cx, get_point, 1.);
    assert!(heard.borrow().iter().any(|a| matches!(a, BoardCanvasAction::FetchBlob { blob: b } if *b == blob::id(large, &[2; 32]))));
    {
        let mut canvas = surface.borrow_mut::<BoardCanvas>().unwrap();
        status(&mut canvas, &mut cx, true);
        paint(&mut cx, &mut canvas);
        let face = canvas.scene.as_ref().unwrap().faces[&id].media.as_ref().unwrap();
        assert_eq!(face.placeholder.text, "Loading…");
        assert!(face.get_rect.is_none(), "a wanted poster cannot be requested again");
    }
    click(&mut cx, get_point, 2.);
    click(&mut cx, get_point, 2.1);
    assert!(heard.borrow().iter().any(|a| matches!(a, BoardCanvasAction::OpenMedia { item, blob: b, media }
        if *item == id && *b == blob::id(50, &[1; 32]) && media == "video/mp4")), "double-click opens the primary blob");
}

fn write_board_png(dir: &Path, town: TownId, n: u8) -> (Id, Value, PathBuf) {
    use makepad_ai_services::pictures::{encode_png, RgbaPicture};
    use makepad_town_core::{blob, hex};
    let png = encode_png(&RgbaPicture { width: 4, height: 4, rgba: [n, 0, 0, 255].repeat(16) }).unwrap();
    let size = png.len() as u64;
    let root = [n; 32];
    let id = blob::id(size, &root);
    let town_dir = dir.join(hex(&town));
    std::fs::create_dir_all(&town_dir).unwrap();
    let path = blob::data_path(&town_dir, &id);
    std::fs::write(&path, png).unwrap();
    (id, Value::Blob { size, root }, path)
}

fn board_picture(history: &mut crate::board::tests::History, board: Id, rect: [i64; 4], blob: Value) -> Id {
    let mut picture = crate::board::tests::item(kind::IMAGE, board, rect, None);
    let makepad_town_core::event::Content::Entity { fields, .. } = &mut picture else { unreachable!() };
    fields.extend([(field::BLOB, blob), (field::MEDIA, Value::Text("image/png".into()))]);
    history.push(picture).0
}

fn media_canvas(cx: &mut Cx, state: &State, board: Id, size: DVec2, town: TownId, dir: &Path) -> BoardCanvas {
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.select_board(cx, state, Some(board), true);
    canvas.edit_context(cx, true, true);
    canvas.camera = Camera::new(Rect { pos: DVec2::default(), size }, CameraState { zoom: 1., pan_x: 0., pan_y: 0. });
    canvas.fit_pending = false;
    canvas.sync_media(cx, town, dir, |_| (true, false, None));
    canvas
}

fn wait_for_board_pngs(cx: &mut Cx, canvas: &mut BoardCanvas, town: TownId, blobs: &[Id]) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    let ready = MediaSlotState::Ready { w: 4, h: 4 };
    while !blobs.iter().all(|&id| canvas.media.state(town, id) == ready) {
        canvas.media.poll(cx);
        assert!(Instant::now() < deadline, "board picture workers did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn board_canvas_media_evict_and_clear_release_bound_textures() {
    let dir = std::env::temp_dir().join(format!("town-board-media-release-{}", std::process::id()));
    let town = [7; 32];
    let (blob_a, value_a, _) = write_board_png(&dir, town, 1);
    let (blob_b, value_b, _) = write_board_png(&dir, town, 2);
    let mut history = crate::board::tests::History::new();
    let board = history.board();
    let item_a = board_picture(&mut history, board, [20, 40, 120, 80], value_a);
    board_picture(&mut history, board, [200, 40, 120, 80], value_b);
    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let size = dvec2(420., 240.);
    let mut canvas = media_canvas(&mut cx, &history.state, board, size, town, &dir);
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    wait_for_board_pngs(&mut cx, &mut canvas, town, &[blob_a, blob_b]);
    canvas.request(&mut cx);
    draw(&mut cx, &mut canvas);

    let center = rect(canvas.scene.as_ref().unwrap().view.get(&item_a).unwrap().rect).center();
    let start = canvas.camera.local_to_screen(canvas.camera.world_to_local(center));
    let end = start + dvec2(20., 0.);
    let win = WindowId(1, 1);
    cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, win));
    canvas.input(&mut cx, &Event::MouseDown(MouseDownEvent { abs: start, button: MouseButton::PRIMARY,
        window_id: win, modifiers: Default::default(), handled: Default::default(), time: 1. }));
    canvas.input(&mut cx, &Event::MouseMove(MouseMoveEvent { abs: end, lock_delta: Default::default(),
        window_id: win, modifiers: Default::default(), handled: Default::default(), time: 1.1 }));
    draw(&mut cx, &mut canvas);
    cx.fingers.first_mouse_button = None;
    canvas.input(&mut cx, &Event::MouseUp(MouseUpEvent { abs: end, button: MouseButton::PRIMARY,
        window_id: win, modifiers: Default::default(), time: 1.2 }));
    draw(&mut cx, &mut canvas);

    let a = canvas.media.request(&mut cx, town, blob_a, None).unwrap();
    let (id_a, readers_a) = (a.texture_id(), a.readers());
    let b = canvas.media.request(&mut cx, town, blob_b, None).unwrap();
    let (id_b, readers_b) = (b.texture_id(), b.readers());
    assert!(readers_a >= 3, "both the board and the stale drag preview retain the first texture");
    assert!(readers_b >= 2, "the board retains the second texture");
    let live_before = cx.texture_pool_live_count();
    let cpu_before = cx.gpu_inventory().texture_cpu_bytes();
    let free_cpu_before = cx.gpu_inventory().free_slot_cpu_bytes();

    canvas.media.evict(&mut cx, &HashSet::from([(town, blob_b)]), 0);
    assert_eq!(canvas.media.state(town, blob_a), MediaSlotState::Missing);
    assert_eq!(canvas.media.state(town, blob_b), MediaSlotState::Ready { w: 4, h: 4 });
    assert_eq!(cx.draw_lists.release_bindings(&[id_a], &[]), 0, "eviction cleared both layers without a redraw");
    assert_eq!(cx.texture_pool_live_count(), live_before - 1);
    let cpu_mid = cx.gpu_inventory().texture_cpu_bytes();
    assert!(cpu_mid < cpu_before, "eviction freed the first picture's pixels");
    assert_eq!(cx.gpu_inventory().free_slot_cpu_bytes(), free_cpu_before);

    canvas.clear_media(&mut cx);
    assert_eq!(canvas.media.state(town, blob_b), MediaSlotState::Missing);
    assert_eq!(cx.draw_lists.release_bindings(&[id_b], &[]), 0, "clear released the board's last binding");
    assert_eq!(cx.texture_pool_live_count(), live_before - 2);
    assert!(cx.gpu_inventory().texture_cpu_bytes() < cpu_mid, "clear freed the remaining pixels");
    assert_eq!(cx.gpu_inventory().free_slot_cpu_bytes(), free_cpu_before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn file_drop_ghost_snaps_in_the_overlay_and_free_spots_clear_offscreen_neighbours() {
    use crate::board::tests::{item, History};
    let mut history = History::new();
    let board = history.board();
    assert_eq!(BoardView::build(&history.state, board).next_free_spot(None, (120., 80.)), (0, 0));
    for bounds in [[60, 40, 20, 20], [190, 100, 100, 60], [430, 100, 70, 50]] {
        history.push(item(kind::SHAPE, board, bounds, None));
    }
    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.select_board(&mut cx, &history.state, Some(board), true);
    let size = dvec2(420., 240.);
    canvas.camera = Camera::new(Rect { pos: DVec2::default(), size }, CameraState { zoom: 1., pan_x: 0., pan_y: 0. });
    canvas.fit_pending = false;
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);

    let view = &canvas.scene.as_ref().unwrap().view;
    assert_eq!(view.next_free_spot(None, (120., 80.)), (532 * 64, 100 * 64));
    assert_eq!(view.next_free_spot(Some(Bounds::new(0., 0., 40., 40.)), (12., 8.)), (20 * 64, 20 * 64));
    assert_eq!(canvas.next_free_spot(board, (120., 80.)), Some((532 * 64, 100 * 64)),
        "after the visible obstruction, skip the card just outside the viewport too");
    assert_eq!(canvas.next_free_spot([0; 32], (120., 80.)), None);

    let base = canvas.recording.as_ref().unwrap().id();
    let base_gen = cx.draw_lists[base].recording_gen;
    let point = canvas.camera.local_to_screen(canvas.camera.world_to_local(dvec2(62., 42.)));
    canvas.file_hover(&mut cx, point, false, 1, Some("layers.png"));
    draw(&mut cx, &mut canvas);
    let ghost = canvas.drop_ghost.as_ref().unwrap();
    assert_eq!(ghost.rect, Bounds::new(60., 40., 380., 280.));
    assert!(ghost.guides.iter().all(Option::is_some));
    assert_eq!(ghost.badge.text, "Drop on board: layers.png");
    let layout = ghost.badge.layout.as_ref().unwrap().1.clone();
    canvas.file_hover(&mut cx, point, true, 1, Some("layers.png"));
    draw(&mut cx, &mut canvas);
    let ghost = canvas.drop_ghost.as_ref().unwrap();
    assert_eq!(ghost.rect, Bounds::new(62., 42., 382., 282.));
    assert!(ghost.guides.iter().all(Option::is_none), "Alt frees the ghost from its guides");
    assert!(Rc::ptr_eq(&layout, &ghost.badge.layout.as_ref().unwrap().1), "moving reuses the laid-out label");
    canvas.file_hover(&mut cx, point, false, 6, None);
    draw(&mut cx, &mut canvas);
    let ghost = canvas.drop_ghost.as_ref().unwrap();
    assert_eq!(ghost.count, 6);
    assert_eq!(ghost.badge.text, "Drop 6 files on board");
    assert_eq!(canvas.take_file_drop(&mut cx), Some((60 * 64, 40 * 64)));
    draw(&mut cx, &mut canvas);
    assert!(canvas.drop_ghost.is_none());
    assert_eq!(cx.draw_lists[base].recording_gen, base_gen, "file hover and drop reuse the base recording");
    canvas.file_hover(&mut cx, point, true, 1, None);
    assert_eq!(canvas.drop_ghost.as_ref().unwrap().label, "Drop on board");
    canvas.select_board(&mut cx, &history.state, None, false);
    assert!(canvas.drop_ghost.is_none(), "leaving the board clears the hover");
    assert!(canvas.show_invitation(), "an empty board invites");
    canvas.file_hover(&mut cx, point, false, 1, Some("layers.png"));
    assert!(!canvas.show_invitation(), "the invitation steps aside while a file's ghost shows the drop");
    assert!(canvas.clear_file_hover(&mut cx));
    assert!(canvas.show_invitation(), "the invitation returns when the file leaves");
}

#[test]
fn board_canvas_zero_size_ready_image_skips_draw_without_error_placeholder() {
    let dir = std::env::temp_dir().join(format!("town-board-zero-size-{}", std::process::id()));
    let town = [7; 32];
    let (blob, value, path) = write_board_png(&dir, town, 3);
    let mut history = crate::board::tests::History::new();
    let board = history.board();
    let flat = board_picture(&mut history, board, [20, 40, 200, 0], value.clone());
    let thin = board_picture(&mut history, board, [20, 80, 0, 100], value);
    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let size = dvec2(320., 240.);
    let mut canvas = media_canvas(&mut cx, &history.state, board, size, town, &dir);
    canvas.media.request(&mut cx, town, blob, Some(path));
    wait_for_board_pngs(&mut cx, &mut canvas, town, &[blob]);
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    assert_eq!(canvas.media.request(&mut cx, town, blob, None).unwrap().readers(), 1,
        "a zero-size card never binds the ready texture");
    for id in [flat, thin] {
        let face = canvas.scene.as_ref().unwrap().faces[&id].media.as_ref().unwrap();
        assert!(face.placeholder.text.is_empty(), "a ready image has no error placeholder");
        assert!(face.get_rect.is_none(), "a ready image has no fetch target");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn double_click_and_return_reopen_only_writable_unlocked_items() {
    use crate::board::tests::{change, item, set, History};
    use makepad_platform::studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp};
    use makepad_town_core::event::Value;

    let mut h = History::new();
    let board = h.board();
    // Neighbouring items share an edge, within double-click distance.
    let ids = [(kind::STICKY, 20), (kind::TEXT_BLOCK, 116), (kind::SHAPE, 212), (kind::SECTION, 308)].map(|(kind, x)| {
        h.push(item(kind, board, [x, 80, 96, 80], None)).0
    });
    let design = h.push(item(kind::DESIGN, board, [20, 170, 96, 60], None)).0;

    let mut surface = WidgetRef::empty();
    let target = surface.clone();
    let heard = Rc::new(std::cell::RefCell::new(Vec::new()));
    let actions = heard.clone();
    let mut cx = Cx::new(Box::new(move |cx, event| {
        if let Event::Actions(batch) = event {
            actions.borrow_mut().extend(batch.iter().filter_map(|a| match a.as_widget_action()?.action.downcast_ref::<BoardCanvasAction>()? {
                BoardCanvasAction::OpenDesign(id) => Some(*id), _ => None,
            }));
        }
        target.handle_event(cx, event, &mut Scope::empty())
    }));
    cx.init_cx_os();
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
        let template = script_eval!(vm, {mod.widgets.BoardCanvas{}});
        surface.script_apply(vm, &Apply::New, &mut Scope::empty(), template);
    });
    let size = dvec2(460., 240.);
    {
        let mut canvas = surface.borrow_mut::<BoardCanvas>().unwrap();
        canvas.select_board(&mut cx, &h.state, Some(board), true);
        canvas.edit_context(&mut cx, true, true);
        canvas.camera = Camera::new(Rect { pos: DVec2::default(), size }, CameraState { zoom: 1., pan_x: 0., pan_y: 0. });
        canvas.fit_pending = false;
        draw_pass(&mut cx, size, 1.)(&mut cx, &mut canvas);
    }
    let click_at = |cx: &mut Cx, x, y, time, shift| {
        let camera = surface.borrow::<BoardCanvas>().unwrap().camera;
        let p = camera.local_to_screen(camera.world_to_local(dvec2(x, y)));
        let win = WindowId(1, 1);
        let button_raw_bits = MouseButton::PRIMARY.bits();
        let modifiers = makepad_platform::studio::RemoteKeyModifiers { shift, ..Default::default() };
        cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown {
            x: p.x, y: p.y, time, button_raw_bits, modifiers,
        }), win, DVec2::default());
        cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp {
            x: p.x, y: p.y, time: time + 0.02, button_raw_bits, modifiers,
        }), win, DVec2::default());
    };
    let click = |cx: &mut Cx, x, time, shift| click_at(cx, x, 108., time, shift);
    let key = |cx: &mut Cx, key_code| {
        let k = KeyEvent { key_code, ..Default::default() };
        for msg in [StudioToApp::KeyDown(k.clone()), StudioToApp::KeyUp(k)] {
            cx.dispatch_studio_msg(msg, WindowId(1, 1), DVec2::default());
        }
    };
    let editing = || surface.borrow::<BoardCanvas>().unwrap().editing.as_ref().map(|e| e.item);
    let opens = || std::mem::take(&mut *heard.borrow_mut());

    // A design card: one click only selects it; a double-click or Return is the ask to edit it.
    click_at(&mut cx, 64., 200., 0.2, false);
    assert_eq!(surface.borrow::<BoardCanvas>().unwrap().selected_item(), Some(design), "a click selects the design card");
    assert_eq!(opens(), Vec::<Id>::new(), "selecting a design card does not open the designer");
    click_at(&mut cx, 64., 200., 0.3, false);
    assert_eq!((opens(), editing()), (vec![design], None), "a double-click asks to open the designer");
    key(&mut cx, KeyCode::ReturnKey);
    assert_eq!(opens(), vec![design], "Return on the selected design card asks to open the designer");

    click(&mut cx, 115., 1., false);
    click(&mut cx, 117., 1.1, false);
    assert_eq!(editing(), None, "two clicks on different items only select");
    for (i, id) in ids.iter().enumerate() {
        let x = 64. + 96. * i as f64;
        let t = 2. + i as f64;
        click(&mut cx, x, t, false);
        assert_eq!(editing(), None, "the first click selects");
        click(&mut cx, x, t + 0.1, false);
        assert_eq!(editing(), Some(*id), "the second click on the item opens it");
        key(&mut cx, KeyCode::Escape);
        key(&mut cx, KeyCode::ReturnKey);
        assert_eq!(editing(), Some(*id), "Return reopens the single selection");
        key(&mut cx, KeyCode::Escape);
    }

    click(&mut cx, 64., 6., true);
    key(&mut cx, KeyCode::ReturnKey);
    assert_eq!(editing(), None, "Return on a multiple selection keeps the editor closed");
    click(&mut cx, 440., 7., false);
    key(&mut cx, KeyCode::ReturnKey);
    assert_eq!(editing(), None, "Return on an empty selection keeps the editor closed");

    h.push(set(vec![change(ids[0], field::LOCKED, Value::Bool(true))]));
    surface.borrow_mut::<BoardCanvas>().unwrap().applied(&mut cx, &h.state, &ids[..1], true);
    for (x, t, writable) in [(64., 8., true), (160., 9., false)] {
        surface.borrow_mut::<BoardCanvas>().unwrap().edit_context(&mut cx, true, writable);
        click(&mut cx, x, t, false);
        click(&mut cx, x, t + 0.1, false);
        assert_eq!(editing(), None, "double-click respects locks and write access");
        key(&mut cx, KeyCode::ReturnKey);
        assert_eq!(editing(), None, "Return respects locks and write access");
    }
}

#[test]
fn present_frames_each_step_in_turn_stays_read_only_and_escape_returns_the_camera() {
    use crate::board::tests::{item, step, History};
    let mut h = History::new();
    let board = h.board();
    let a = h.push(item(kind::STICKY, board, [0, 0, 200, 100], None)).0;
    let b = h.push(item(kind::STICKY, board, [2000, 1000, 400, 200], None)).0;
    let mut cx = test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });
    let mut canvas = cx.with_vm(|vm| BoardCanvas::script_new_with_default(vm));
    canvas.select_board(&mut cx, &h.state, Some(board), true);
    let mut draw = draw_pass(&mut cx, dvec2(800., 600.), 1.);
    draw(&mut cx, &mut canvas);
    let before = canvas.camera;
    // Every glide has landed by then.
    let later = makepad_platform::monotonic_seconds() + 600.;
    let landed = |canvas: &BoardCanvas| canvas.present.as_ref().expect("presenting").camera(later);
    let centre = |camera: Camera, world: DVec2| camera.local_to_screen(camera.world_to_local(world));
    let near = |a: DVec2, b: DVec2| (a - b).length() < 0.01;
    let at = |canvas: &BoardCanvas| canvas.present.as_ref().and_then(|p| p.step());

    assert!(canvas.present(&mut cx), "an empty path presents the board");
    let shown = landed(&canvas);
    assert!(near(centre(shown, dvec2(1200., 600.)), shown.view.center()), "the whole board is framed");
    canvas.end_present(&mut cx);
    assert!(!canvas.presenting() && canvas.camera.pan == before.pan && canvas.camera.scale == before.scale,
        "ending puts the camera back");

    canvas.gesture.selection.ids.insert(a);
    assert!(canvas.present(&mut cx));
    let shown = landed(&canvas);
    assert!(near(centre(shown, dvec2(100., 50.)), shown.view.center()) && (shown.scale - 752. / 200.).abs() < 1e-9,
        "with no path the selection is framed, inside the margin: {}", shown.scale);
    canvas.end_present(&mut cx);
    canvas.gesture.selection.ids.clear();

    let add = |h: &mut History, canvas: &mut BoardCanvas, cx: &mut Cx, target: Id, order: u8| {
        let (id, touched) = h.push(step(board, target, vec![order], None));
        canvas.applied(cx, &h.state, &touched, true);
        id
    };
    let s1 = add(&mut h, &mut canvas, &mut cx, b, 2);
    add(&mut h, &mut canvas, &mut cx, [9; 32], 4);
    let s3 = add(&mut h, &mut canvas, &mut cx, a, 6);
    assert!(canvas.present(&mut cx));
    assert_eq!(at(&canvas), Some(s1), "presenting starts at the first step");
    assert!(near(centre(landed(&canvas), dvec2(2200., 1100.)), landed(&canvas).view.center()));

    // A click advances, past the step with nothing to show, and selects nothing.
    let win = WindowId(1, 1);
    cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, win));
    let click = centre(canvas.camera, dvec2(2200., 1100.));
    let actions = cx.capture_actions(|cx| {
        canvas.handle_event(cx, &Event::MouseDown(MouseDownEvent {
            abs: click, button: MouseButton::PRIMARY, window_id: win, modifiers: Default::default(),
            handled: Default::default(), time: 1.,
        }), &mut Scope::empty());
        cx.fingers.first_mouse_button = None;
        canvas.handle_event(cx, &Event::MouseUp(MouseUpEvent {
            abs: click, button: MouseButton::PRIMARY, window_id: win, modifiers: Default::default(), time: 1.05,
        }), &mut Scope::empty());
    });
    assert_eq!(at(&canvas), Some(s3), "the unknown target is skipped");
    assert!(actions.is_empty() && canvas.gesture.selection.ids.is_empty(), "presenting is read-only");
    canvas.present_move(&mut cx, true);
    assert_eq!(at(&canvas), Some(s3), "at the end it holds");

    // A step added meanwhile is reachable, and a resize keeps the step framed.
    let s4 = add(&mut h, &mut canvas, &mut cx, board, 8);
    canvas.present_move(&mut cx, true);
    assert_eq!(at(&canvas), Some(s4));
    canvas.present_move(&mut cx, false);
    let mut small = draw_pass(&mut cx, dvec2(600., 400.), 1.);
    small(&mut cx, &mut canvas);
    let shown = landed(&canvas);
    assert_eq!(shown.view.size, dvec2(600., 400.));
    assert!(near(centre(shown, dvec2(100., 50.)), shown.view.center()), "the resized window still frames the step");

    canvas.end_present(&mut cx);
    assert!(canvas.camera.pan == before.pan && canvas.camera.scale == before.scale && canvas.camera.view.size == dvec2(600., 400.),
        "Esc's camera is the one from before, in the window as it is now");
}

fn draw_page(cx: &mut Cx, size: DVec2) -> impl FnMut(&mut Cx, &WidgetRef) {
    let pass = DrawPass::new(cx);
    let mut list = DrawList2d::new(cx);
    pass.set_size(cx, size);
    move |cx, page| {
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut cx = Cx2d::new(&mut draw);
        cx.begin_pass(&pass, None);
        list.begin_always(&mut cx);
        cx.begin_root_turtle(size, Layout::flow_down());
        page.draw_all(&mut cx, &mut Scope::empty());
        cx.end_pass_sized_turtle();
        list.end(&mut cx);
        cx.end_pass(&pass);
    }
}

/// Through the board page as the app builds it and the input path the platform delivers: a
/// wheel zooms toward the pointer, a trackpad pans and pinches, right-drag, Space-drag and the
/// Hand tool pan without touching the item under them, and the corner cluster and the keys
/// step and fit the zoom.
#[test]
fn the_board_page_navigates_by_wheel_trackpad_drags_hand_cluster_and_keys() {
    use crate::board::tests::{item, History};
    use makepad_platform::studio::{
        PinchPhase, RemoteKeyModifiers, RemoteMouseDown, RemoteMouseMove, RemoteMouseUp, RemotePinch, RemoteScroll,
        StudioToApp,
    };

    let mut h = History::new();
    let board = h.board();
    h.push(item(kind::STICKY, board, [200, 150, 120, 80], None));
    let mut page = WidgetRef::empty();
    let target = page.clone();
    // The dispatch hands the page's actions back to the app as `Event::Actions`: count the
    // board edits there, as the app hears them.
    let acts = std::rc::Rc::new(std::cell::Cell::new(0));
    let heard = acts.clone();
    let mut cx = Cx::new(Box::new(move |cx, event| {
        if let Event::Actions(actions) = event {
            heard.set(heard.get() + actions.iter().filter(|a| matches!(a.as_widget_action().and_then(|w| w.action.downcast_ref()),
                Some(BoardCanvasAction::Act(_)))).count());
        }
        target.handle_event(cx, event, &mut Scope::empty())
    }));
    cx.init_cx_os();
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
        let template = script_eval!(vm, {mod.widgets.WhiteboardPage{}});
        page.script_apply(vm, &Apply::New, &mut Scope::empty(), template);
    });
    let surface = page.widget(&cx, ids!(surface));
    {
        let mut canvas = surface.borrow_mut::<BoardCanvas>().unwrap();
        canvas.select_board(&mut cx, &h.state, Some(board), true);
        canvas.edit_context(&mut cx, true, true);
    }
    // The app shows the empty board's invitation only while the board is empty.
    page.widget(&cx, ids!(board_empty)).set_visible(&mut cx, false);
    let mut draw = draw_page(&mut cx, dvec2(640., 480.));
    draw(&mut cx, &page);
    let camera = || surface.borrow::<BoardCanvas>().unwrap().camera;
    let fitted = camera();
    assert!(fitted.scale > 0., "the first frame fits the board");
    let on_screen = |world: DVec2| { let c = camera(); c.local_to_screen(c.world_to_local(world)) };
    let world_at = |screen: DVec2| camera().world_at(screen);

    let win = WindowId(1, 1);
    let time = std::cell::Cell::new(10.);
    let now = || { time.set(time.get() + 1.); time.get() };
    // How many board edits the input asked for: a pan asks for none.
    let edits = |cx: &mut Cx, msgs: Vec<StudioToApp>| {
        let before = acts.get();
        for msg in msgs { cx.dispatch_studio_msg(msg, win, DVec2::default()); }
        acts.get() - before
    };
    let drag = |cx: &mut Cx, button: MouseButton, from: DVec2, to: DVec2| {
        let (button_raw_bits, t) = (button.bits(), now());
        let mid = (from + to) * 0.5;
        edits(cx, vec![
            StudioToApp::MouseDown(RemoteMouseDown { x: from.x, y: from.y, time: t, button_raw_bits, modifiers: Default::default() }),
            StudioToApp::MouseMove(RemoteMouseMove { x: mid.x, y: mid.y, time: t + 0.05, modifiers: Default::default() }),
            StudioToApp::MouseMove(RemoteMouseMove { x: to.x, y: to.y, time: t + 0.1, modifiers: Default::default() }),
            StudioToApp::MouseUp(RemoteMouseUp { x: to.x, y: to.y, time: t + 0.15, button_raw_bits, modifiers: Default::default() }),
        ])
    };
    let scroll = |cx: &mut Cx, at: DVec2, by: DVec2, is_mouse, phase, modifiers| {
        edits(cx, vec![StudioToApp::Scroll(RemoteScroll { time: now(), sx: by.x, sy: by.y, x: at.x, y: at.y, is_mouse, modifiers, phase })]);
    };
    let key = |cx: &mut Cx, key_code, modifiers: KeyModifiers| {
        let k = KeyEvent { key_code, modifiers, ..Default::default() };
        edits(cx, vec![StudioToApp::KeyDown(k.clone()), StudioToApp::KeyUp(k)]);
    };
    let near = |a: DVec2, b: DVec2| (a - b).length() < 1e-6;
    let empty = dvec2(560., 400.);

    // A wheel zooms about the pointer: the point under it stays put.
    let anchor = world_at(empty);
    scroll(&mut cx, empty, dvec2(0., 32.), true, ScrollPhase::None, RemoteKeyModifiers::default());
    assert!(camera().scale < fitted.scale, "a wheel turned toward the person zooms out");
    assert!(near(world_at(empty), anchor), "the wheel zooms toward the pointer");

    // A trackpad's two fingers pan, momentum included, and its pinch zooms.
    let before = camera();
    for (phase, by) in [(ScrollPhase::Began, dvec2(0., 0.)), (ScrollPhase::Changed, dvec2(10., -20.)),
        (ScrollPhase::Ended, dvec2(0., 0.)), (ScrollPhase::Momentum, dvec2(2., -4.)), (ScrollPhase::MomentumEnded, dvec2(0., 0.))] {
        scroll(&mut cx, empty, by, false, phase, RemoteKeyModifiers::default());
    }
    assert!(near(camera().pan, before.pan + dvec2(-12., 24.)) && camera().scale == before.scale,
        "two fingers pan with the fingers, momentum included, without zooming");
    let pinch = |scale, phase| StudioToApp::Pinch(RemotePinch { time: now(), x: empty.x, y: empty.y, scale, phase,
        modifiers: Default::default() });
    edits(&mut cx, vec![pinch(1., PinchPhase::Begin), pinch(1.5, PinchPhase::Update), pinch(1., PinchPhase::End)]);
    assert!((camera().scale / before.scale - 1.5).abs() < 1e-9, "a pinch zooms");

    // Right-drag pans everywhere, over an item too; so does Space-drag and the Hand tool. Each
    // press follows the frame the last camera change asked for, as in the app.
    draw(&mut cx, &page);
    let card = on_screen(dvec2(260., 190.));
    let before = camera();
    assert_eq!(drag(&mut cx, MouseButton::SECONDARY, card, card + dvec2(30., 20.)), 0, "right-drag over a card leaves it");
    assert!(near(camera().pan, before.pan + dvec2(30., 20.)), "right-drag pans with the pointer");
    draw(&mut cx, &page);
    let card = on_screen(dvec2(260., 190.));
    let space = KeyEvent { key_code: KeyCode::Space, ..Default::default() };
    edits(&mut cx, vec![StudioToApp::KeyDown(space.clone())]);
    let before = camera();
    assert_eq!(drag(&mut cx, MouseButton::PRIMARY, card, card - dvec2(25., 5.)), 0, "Space-drag over a card leaves it");
    assert!(near(camera().pan, before.pan - dvec2(25., 5.)), "Space-drag pans");
    edits(&mut cx, vec![StudioToApp::KeyUp(space)]);
    assert!(!surface.borrow::<BoardCanvas>().unwrap().gesture.space, "releasing Space lifts the hand");
    key(&mut cx, KeyCode::KeyH, KeyModifiers::default());
    assert_eq!(surface.borrow::<BoardCanvas>().unwrap().tool, BoardTool::Hand, "H arms the Hand tool");
    draw(&mut cx, &page);
    let card = on_screen(dvec2(260., 190.));
    let before = camera();
    assert_eq!(drag(&mut cx, MouseButton::PRIMARY, card, card + dvec2(-8., 16.)), 0, "the Hand tool leaves the card");
    assert!(near(camera().pan, before.pan + dvec2(-8., 16.)), "the Hand tool pans with a left drag");
    key(&mut cx, KeyCode::KeyH, KeyModifiers::default());
    assert_eq!(surface.borrow::<BoardCanvas>().unwrap().tool, BoardTool::Select, "H again returns to Select");

    // The cluster steps the zoom and its level fits the board; a press on it is not the board's.
    draw(&mut cx, &page);
    let press = |cx: &mut Cx, path: &[LiveId]| {
        let at = page.widget(cx, path).area().clipped_rect(cx).center();
        let (button_raw_bits, t) = (MouseButton::PRIMARY.bits(), now());
        edits(cx, vec![
            StudioToApp::MouseDown(RemoteMouseDown { x: at.x, y: at.y, time: t, button_raw_bits, modifiers: Default::default() }),
            StudioToApp::MouseUp(RemoteMouseUp { x: at.x, y: at.y, time: t + 0.05, button_raw_bits, modifiers: Default::default() }),
        ]);
    };
    let before = camera();
    press(&mut cx, ids!(surface.zoom_in));
    assert!((camera().scale / before.scale - ZOOM_STEP).abs() < 1e-9, "+ zooms in a step");
    draw(&mut cx, &page);
    press(&mut cx, ids!(surface.zoom_out));
    assert!((camera().scale / before.scale - 1.).abs() < 1e-9, "− zooms back out");
    draw(&mut cx, &page);
    press(&mut cx, ids!(surface.zoom_level));
    assert_eq!((camera().scale, camera().pan), (fitted.scale, fitted.pan), "the level fits the board");
    assert!(surface.borrow::<BoardCanvas>().unwrap().gesture.marquee().is_none(), "the cluster's press is not a marquee");
    draw(&mut cx, &page);
    assert_eq!(page.button(&cx, ids!(surface.zoom_level)).text(), format!("{:.0} %", fitted.scale * 100.),
        "the level shows the zoom");

    // The keys: Command with = and − steps, Command 0 is 100 %, Shift 1 and F fit.
    let command = KeyModifiers { logo: true, ..Default::default() };
    key(&mut cx, KeyCode::Key0, command);
    assert_eq!(camera().scale, 1., "Command 0 is 100 %");
    key(&mut cx, KeyCode::Equals, command);
    assert_eq!(camera().scale, ZOOM_STEP, "Command = zooms in");
    key(&mut cx, KeyCode::Minus, command);
    assert_eq!(camera().scale, 1., "Command − zooms out");
    key(&mut cx, KeyCode::Equals, command);
    key(&mut cx, KeyCode::Key1, KeyModifiers { shift: true, ..Default::default() });
    assert_eq!((camera().scale, camera().pan), (fitted.scale, fitted.pan), "Shift 1 fits");
    key(&mut cx, KeyCode::Equals, command);
    key(&mut cx, KeyCode::KeyF, KeyModifiers::default());
    assert_eq!((camera().scale, camera().pan), (fitted.scale, fitted.pan), "F fits");

    // The control: a plain left-drag over the card does move it.
    draw(&mut cx, &page);
    let card = on_screen(dvec2(260., 190.));
    assert_eq!(drag(&mut cx, MouseButton::PRIMARY, card, card + dvec2(30., 0.)), 1, "Select's drag moves the card");
}

const SLIDE: &str = r##"SolidView{width: Fill height: Fill draw_bg +: {color: #241f3a} flow: Down padding: 40 spacing: 16 H1{text: "Why Town is cool"} View{flow: Down spacing: 8 Label{text: "Peers, not servers"} Label{text: "Your AI at the table"}}}"##;

thread_local! {
    static WHOLE_DESIGN: std::cell::Cell<Option<TextureId>> = const { std::cell::Cell::new(None) };
    static LOADS_IN_FLIGHT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn design_fixture(sources: &[&str]) -> (test_cx::PooledCx, BoardCanvas, crate::board::tests::History, Vec<Id>, DVec2) {
    use crate::board::tests::{History, item};
    use makepad_town_core::event::Content;
    let mut h = History::new();
    let board = h.board();
    let mut ids = Vec::new();
    for (i, &src) in sources.iter().enumerate() {
        let mut design = item(kind::DESIGN, board, [400 * i as i64, 0, 390, 844], Some(i as u8 + 1));
        let Content::Entity { fields, .. } = &mut design else { unreachable!() };
        fields.push((field::SRC, Value::Text(src.to_owned())));
        ids.push(h.push(design).0);
    }
    let size = dvec2(400. * sources.len() as f64, 850.);
    let mut cx = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); crate::script_mod(vm); });
    let mut canvas = media_canvas(&mut cx, &h.state, board, size, [1; 32], Path::new("."));
    WHOLE_DESIGN.with(|whole| whole.set(None));
    canvas.designs.painted = Some(|_, id| WHOLE_DESIGN.with(|whole| whole.get() == Some(id)));
    (cx, canvas, h, ids, size)
}

#[test]
fn a_board_view_build_copies_no_design_source() {
    use crate::board::view::DESIGN_READS;
    use std::sync::Arc;
    let huge = format!("SolidView{{Label{{text: \"{}\"}}}}", "Town ".repeat(1 << 18));
    let (mut cx, mut canvas, mut h, ids, _) = design_fixture(&[&huge]);
    let id = ids[0];
    let src = |canvas: &BoardCanvas| canvas.scene.as_ref().unwrap().view.get(&id).unwrap().design.as_ref().unwrap().src.clone();
    let reads = || DESIGN_READS.with(|reads| reads.get());
    let first = src(&canvas);
    let before = reads();
    canvas.peer_snapshot(&mut cx, &h.state, true);
    assert!(Arc::ptr_eq(&first, &src(&canvas)) && reads() == before, "a rebuilt view shares the design's text and does not hash it again");
    use crate::board::tests::{change, set};
    let (_, touched) = h.push(set(vec![change(id, field::X, Value::Int(64 * 64))]));
    canvas.applied(&mut cx, &h.state, &touched, true);
    assert!(Arc::ptr_eq(&first, &src(&canvas)) && reads() == before, "moving a design does not copy its text");
    edit_design_source(&mut cx, &mut canvas, &mut h, id, "SolidView{}");
    assert_eq!((&*src(&canvas), reads()), ("SolidView{}", before + 1), "a new revision is read once");
    let second = src(&canvas);
    canvas.peer_snapshot(&mut cx, &h.state, true);
    assert!(Arc::ptr_eq(&second, &src(&canvas)) && reads() == before + 1, "and shared from then on");
}

fn design_wake(cx: &mut Cx, canvas: &mut BoardCanvas) {
    let frame = canvas.design_frame;
    assert_ne!(frame, NextFrame::default(), "design work must arm a frame after discovery ({})", canvas.designs.stage());
    let event = Event::NextFrame(NextFrameEvent { set: [frame].into_iter().collect(), ..Default::default() });
    canvas.handle_event(cx, &event, &mut Scope::empty());
}

fn advance_design(cx: &mut Cx, canvas: &mut BoardCanvas, draw: &mut impl FnMut(&mut Cx, &mut BoardCanvas),
    until: impl Fn(&BoardCanvas) -> bool) {
    for _ in 0..256 {
        if until(canvas) { return }
        design_wake(cx, canvas);
        draw(cx, canvas);
        assert!(canvas.designs.steps_this_frame <= 1, "only one design stage in a drawing");
    }
    panic!("design did not reach its expected state, last stage {}", canvas.designs.stage());
}

fn edit_design_source(cx: &mut Cx, canvas: &mut BoardCanvas, h: &mut crate::board::tests::History, id: Id, src: &str) {
    use crate::board::tests::{change, set};
    let (_, touched) = h.push(set(vec![change(id, field::SRC, Value::Text(src.to_owned()))]));
    canvas.applied(cx, &h.state, &touched, true);
}

#[test]
fn a_design_card_draws_its_splash_as_a_cached_picture() {
    let (mut cx, mut canvas, mut h, ids, size) = design_fixture(&[SLIDE]);
    let id = ids[0];
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    assert_eq!(canvas.design_state(id), PictureState::Rendering, "the first draw queued the phone slide");
    let before = cx.frame_requests();
    design_wake(&mut cx, &mut canvas);
    assert!(cx.frame_requests() > before, "the design's own next-frame event redraws a settled board");
    draw(&mut cx, &mut canvas);
    // A glyph the drawing asked for is still loading: its picture would keep a blank there.
    LOADS_IN_FLIGHT.with(|n| n.set(1));
    cx.register_async_load_probe("design test load", |_| LOADS_IN_FLIGHT.with(|n| n.get()));
    advance_design(&mut cx, &mut canvas, &mut draw, |c| c.designs.drawings() >= 2);
    assert!(canvas.designs.pending_paint().is_none() && canvas.designs.stage() == "Draw",
        "a drawing made while a load is in flight is drawn again, never painted");
    LOADS_IN_FLIGHT.with(|n| n.set(0));
    // The pooled context outlives this test: leave its probe counting nothing.
    cx.register_async_load_probe("design test load", |_| 0);
    advance_design(&mut cx, &mut canvas, &mut draw, |c| c.designs.pending_paint().is_some());
    let (first, items) = canvas.designs.pending_paint().unwrap();
    assert!(items > 0, "the slide recorded draw items into its own pass");
    design_wake(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    assert_eq!(canvas.design_picture(id), None, "a pending or partial first paint stays hidden");
    WHOLE_DESIGN.with(|w| w.set(Some(first)));
    advance_design(&mut cx, &mut canvas, &mut draw, |c| !c.designs.busy());
    assert_eq!(canvas.design_state(id), PictureState::Ready { w: 390, h: 844 });
    assert_eq!(canvas.design_picture(id), Some(first));
    assert_eq!(canvas.design_frame, NextFrame::default());
    let renders = canvas.designs.renders;
    for i in 1..=10 {
        canvas.pan(&mut cx, dvec2(i as f64, 0.));
        draw(&mut cx, &mut canvas);
        assert_eq!((canvas.designs.renders, canvas.designs.steps_this_frame, canvas.design_picture(id)), (renders, 0, Some(first)));
    }
    edit_design_source(&mut cx, &mut canvas, &mut h, id, "Label{text: \"second\"}");
    draw(&mut cx, &mut canvas);
    advance_design(&mut cx, &mut canvas, &mut draw, |c| {
        assert_eq!(c.design_picture(id), Some(first));
        c.designs.pending_paint().is_some()
    });
    let stale = canvas.designs.pending_paint().unwrap().0;
    edit_design_source(&mut cx, &mut canvas, &mut h, id, "Label{text: \"third\"}");
    WHOLE_DESIGN.with(|w| w.set(Some(stale)));
    draw(&mut cx, &mut canvas);
    advance_design(&mut cx, &mut canvas, &mut draw, |c| {
        assert_eq!(c.design_picture(id), Some(first), "late completion cannot replace the displayed slide");
        c.designs.pending_paint().is_some_and(|(tex, _)| tex != stale)
    });
    let third = canvas.designs.pending_paint().unwrap().0;
    WHOLE_DESIGN.with(|w| w.set(Some(third)));
    advance_design(&mut cx, &mut canvas, &mut draw, |c| !c.designs.busy());
    assert_ne!(third, first);
    assert_eq!(canvas.design_picture(id), Some(third));
    assert_eq!(canvas.designs.renders, renders + 1);
    assert_eq!(canvas.design_frame, NextFrame::default());

    edit_design_source(&mut cx, &mut canvas, &mut h, id, "");
    draw(&mut cx, &mut canvas);
    assert_eq!(canvas.design_state(id), PictureState::Ready { w: 0, h: 0 });
    assert_eq!(canvas.design_picture(id), None, "an empty design returns to its frame");
    assert_eq!(canvas.designs.renders, renders + 1, "clearing the source needs no renderer");
    assert!(!canvas.designs.busy());
    assert_eq!(canvas.design_frame, NextFrame::default());
    assert_eq!(cx.draw_lists.release_bindings(&[third], &[]), 0, "clearing released the old recording's binding");
}

#[test]
fn a_design_with_a_script_error_shows_it_on_the_card() {
    let (mut cx, mut canvas, mut h, ids, size) = design_fixture(&["SolidView{width: )}", "SolidView{}\nmissing_slide_function()"]);
    use crate::board::tests::{change, set};
    let (_, touched) = h.push(set(vec![change(ids[1], field::W, Value::Int(128 * 64)), change(ids[1], field::H, Value::Int(32 * 64))]));
    canvas.applied(&mut cx, &h.state, &touched, true);
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    advance_design(&mut cx, &mut canvas, &mut draw, |c| ids.iter().all(|&id| matches!(c.design_state(id), PictureState::Failed { .. })));
    for (&id, causes) in ids.iter().zip([&["parse", "unexpected", "expected", "unmatched"][..], &["missing_slide_function"][..]]) {
        let PictureState::Failed { message } = canvas.design_state(id) else { unreachable!() };
        assert!(causes.iter().any(|cause| message.to_lowercase().contains(cause)), "actionable diagnostic: {message}");
        assert_eq!(canvas.design_message(id), Some(message.as_str()));
        let ink = canvas.drawn_design(id).unwrap().error_area.unwrap().clipped_rect_union(&cx);
        let frame = local_rect(&canvas.camera, canvas.scene.as_ref().unwrap().view.get(&id).unwrap().rect);
        assert!(ink.size.x > 0. && ink.size.y > 0. && frame.contains(ink.pos) && frame.contains(ink.pos + ink.size),
            "visible error glyphs {ink:?} fit the card {frame:?}");
        assert_eq!(canvas.design_picture(id), None);
    }
    let messages: Vec<String> = ids.iter().map(|&id| canvas.design_message(id).unwrap().to_owned()).collect();
    assert!(messages[0].starts_with("line 1: ") && messages[1].starts_with("line 2: "), "each error names its line in the design: {messages:?}");
    assert!(!messages.iter().any(|message| ["(obj", "(fn", "EndStmt", "EmitOp", "CloseRound"].iter().any(|internal| message.contains(internal))),
        "the errors show the script's names, not the interpreter's: {messages:?}");
    // The message reads at one screen size at any zoom, still clipped to its card.
    let centre = canvas.camera.view.center();
    for zoom in [canvas.camera.scale, canvas.camera.scale / 8.] {
        canvas.zoom(&mut cx, centre, zoom);
        draw(&mut cx, &mut canvas);
        for &id in &ids {
            let face = canvas.drawn_design(id).expect("the error card draws");
            let pt = face.error_pt.expect("its message draws");
            assert!((pt - DESIGN_ERROR_PT).abs() < 0.01, "{pt} screen points at zoom {}", canvas.camera.scale);
            let ink = face.error_area.unwrap().clipped_rect_union(&cx);
            let frame = local_rect(&canvas.camera, canvas.scene.as_ref().unwrap().view.get(&id).unwrap().rect);
            // A card a few pixels tall at overview zoom has no room for a line of it.
            let seen = ink.size.x > 0. && ink.size.y > 0.;
            assert!(seen || id != ids[0], "the slide-sized card shows its error at zoom {}", canvas.camera.scale);
            assert!(!seen || frame.contains(ink.pos) && frame.contains(ink.pos + ink.size), "clipped to the card at zoom {}", canvas.camera.scale);
        }
    }
}

#[test]
fn a_design_cannot_reach_files_network_or_the_host() {
    use makepad_platform::script::HostGrants;
    // The task pump repeats whenever a widget call was dequeued, including a call for
    // a dead VM. A single round after retirement proves that its queue was already empty.
    thread_local! { static PUMP_ROUNDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    extern "C-unwind" fn count_round(_: *mut u8, _: &mut makepad_script::ScriptHostContext) -> bool {
        PUMP_ROUNDS.with(|n| n.set(n.get() + 1));
        false
    }
    let cases = [
        (r#"fs.read("/card")"#, "storage"),
        (r#"fs.write("/card", "x")"#, "storage"),
        ("mod.net.http_request()", "net"),
        ("Root{}", "Root"),
        ("Markdown{}", "Markdown"),
        (r#"mod.std.start_interval(60, || {})
            host.request("card.probe", {}, fn(r){})
            let pending = mod.std.task(|| {})
            loop {}"#, "line 4: script instruction limit exceeded"),
    ];
    let sources: Vec<_> = cases.iter().map(|(src, _)| *src).collect();
    let (mut cx, mut canvas, _, ids, size) = design_fixture(&sources);
    let root = cx.with_vm(|vm| {
        let value = script_eval!(vm, { use mod.widgets.* View{} });
        WidgetRef::script_from_value(vm, value)
    });
    makepad_widgets::widget_tree::set_ui_root(&mut cx, &root);
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    for (&id, (_, reason)) in ids.iter().zip(cases) {
        advance_design(&mut cx, &mut canvas, &mut draw, |c| c.designs.stage() == "Eval");
        let heap = canvas.designs.heap_before_eval(&mut cx).expect("registered design heap");
        let mut tasks = None;
        let mut callback = None;
        assert!(cx.with_vm_of_heap(heap, &mut |vm| {
            assert_eq!(vm.cx().script_std().grants(), HostGrants::NONE);
            tasks = Some(Rc::downgrade(&vm.cx().script_std().data.tasks.tasks));
            let source = vm.new_object();
            let f = vm.eval(makepad_script::script! { || {} }).as_object().unwrap();
            callback = Some((vm.new_object_ref(source), vm.heap_mut().new_fn_ref(f)));
        }));
        let (source, callback) = callback.unwrap();
        cx.widget_to_script_call(WidgetUid(0), NIL, source, callback, &[]);
        advance_design(&mut cx, &mut canvas, &mut draw, |c| matches!(c.design_state(id), PictureState::Failed { .. }));
        let message = canvas.design_message(id).expect("the refusal is drawn on the card");
        assert!(message.contains(reason), "wanted {reason:?} in {message:?}");
        assert_eq!(canvas.design_picture(id), None);
        assert_eq!(cx.widget_tree().root_uid(), root.widget_uid(), "the host still owns its root");
        assert!(!cx.script_data.timers.timers.iter().any(|t| t.callback.heap_key() == heap));
        assert!(!splash_host::take_splash_host_requests().iter().any(|r| r.heap_key == heap));
        assert!(tasks.unwrap().upgrade().is_none_or(|tasks| tasks.borrow().is_empty()), "guest tasks retired");
        assert!(!cx.with_vm_of_heap(heap, &mut |_| {}), "the heap was retired before deferred calls can be dispatched");
        // SAFETY: the callback is static and ignores its null context.
        let hook = unsafe { makepad_platform::script::std::CxScriptTaskPumpHook::new(std::ptr::null_mut(), count_round) };
        cx.add_script_task_pump_hook(hook);
        PUMP_ROUNDS.with(|n| n.set(0));
        cx.with_vm_and_async(|_| {});
        cx.script_std_mut().data.tasks.hooks.pump.pop();
        assert_eq!(PUMP_ROUNDS.with(|n| n.get()), 1, "no queued widget call survived the guest heap");
        assert_eq!(cx.widget_tree().root_uid(), root.widget_uid(), "the widget-call pump leaves the host tree intact");
    }
}

#[test]
fn design_renders_take_one_stage_per_frame_and_pan_costs_none() {
    let slides: Vec<_> = (0..6).map(|i| SLIDE.replace("Why Town is cool", &format!("Slide {i}"))).collect();
    let sources: Vec<_> = slides.iter().map(String::as_str).collect();
    let (mut cx, mut canvas, _, ids, size) = design_fixture(&sources);
    let mut draw = draw_pass_with(&mut cx, size, 1., |cx, canvas| {
        let progress = |c: &BoardCanvas| (c.designs.stage(), c.designs.registration_steps(), c.designs.renders);
        let before = progress(canvas);
        assert!(!canvas.designs.step(cx), "a second call in one draw generation cannot publish");
        assert_eq!(progress(canvas), before, "a second call in one draw generation cannot advance registration or a stage");
    });
    draw(&mut cx, &mut canvas);
    let mut renders = 0;
    for _ in 0..6 * 256 {
        if renders == 6 { break }
        if let Some((id, _)) = canvas.designs.pending_paint() { WHOLE_DESIGN.with(|whole| whole.set(Some(id))); }
        design_wake(&mut cx, &mut canvas);
        draw(&mut cx, &mut canvas);
        assert!(canvas.designs.steps_this_frame <= 1, "registration, eval and draw each get their own generation");
        if canvas.designs.renders != renders {
            let times = canvas.designs.last_times.expect("a finished render has timings");
            let prepare = if times.register_steps == 1 { "cold" } else { "warm" };
            eprintln!("slide {} {prepare} prepare={:.3} ms {times:?} fuel={}", renders + 1, times.prepare_ms, times.fuel);
            assert!(times.register_steps > 0 && times.fuel > 0, "staged registration and metered guest execution");
            renders = canvas.designs.renders;
        }
    }
    assert_eq!(renders, 6, "six finished slides, stage {}", canvas.designs.stage());
    let pictures: Vec<_> = ids.iter().map(|&id| {
        assert!(matches!(canvas.design_state(id), PictureState::Ready { .. }));
        canvas.design_picture(id).expect("ready slide is bound")
    }).collect();
    for i in 1..=10 {
        canvas.pan(&mut cx, dvec2(i as f64, 0.));
        draw(&mut cx, &mut canvas);
        assert_eq!(canvas.designs.steps_this_frame, 0);
        assert_eq!(canvas.designs.renders, 6);
        assert!(!canvas.designs.busy());
        assert_eq!(canvas.design_frame, NextFrame::default());
        for (&id, &tex) in ids.iter().zip(&pictures) { assert_eq!(canvas.design_picture(id), Some(tex)); }
    }
}

fn finish_design_renders(cx: &mut Cx, canvas: &mut BoardCanvas, draw: &mut impl FnMut(&mut Cx, &mut BoardCanvas)) {
    for _ in 0..256 {
        if !canvas.designs.busy() { return }
        if let Some((tex, _)) = canvas.designs.pending_paint() { WHOLE_DESIGN.with(|whole| whole.set(Some(tex))); }
        design_wake(cx, canvas);
        draw(cx, canvas);
    }
    panic!("design renders did not finish, stage {}", canvas.designs.stage());
}

/// Finish the queued renders; the backend reports that `reads_clock`'s paints read `draw_pass.time`.
fn finish_renders_reading_clock(cx: &mut Cx, canvas: &mut BoardCanvas, draw: &mut impl FnMut(&mut Cx, &mut BoardCanvas), reads_clock: Id) {
    for _ in 0..256 {
        if !canvas.designs.busy() { return }
        if let Some((tex, _)) = canvas.designs.pending_paint() {
            let (key, pass) = canvas.designs.pending_pass().unwrap();
            cx.passes[pass].reads_time = key.1 == reads_clock;
            WHOLE_DESIGN.with(|whole| whole.set(Some(tex)));
        }
        design_wake(cx, canvas);
        draw(cx, canvas);
    }
    panic!("design renders did not finish, stage {}", canvas.designs.stage());
}

#[test]
fn a_design_whose_paint_reads_the_clock_stays_live_while_it_shows() {
    let (mut cx, mut canvas, _, ids, size) = design_fixture(&[SLIDE, SLIDE]);
    let town = canvas.media_town.unwrap();
    let (live, still) = ((town, ids[0]), (town, ids[1]));
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    finish_renders_reading_clock(&mut cx, &mut canvas, &mut draw, ids[0]);
    assert_eq!(canvas.designs.live_time(&cx, live), Some(None), "a card whose paint read the clock stays live");
    assert_eq!(canvas.designs.live_time(&cx, still), None, "any other card is a cached still");
    let renders = canvas.designs.renders;
    let tick = |cx: &mut Cx, canvas: &mut BoardCanvas, time: f64| {
        let frame = canvas.card_frame;
        assert_ne!(frame, NextFrame::default(), "a live card on screen asks for the next frame");
        let event = Event::NextFrame(NextFrameEvent { set: [frame].into_iter().collect(), time, ..Default::default() });
        canvas.handle_event(cx, &event, &mut Scope::empty());
    };
    tick(&mut cx, &mut canvas, 10.);
    tick(&mut cx, &mut canvas, 10.5);
    assert_eq!(canvas.designs.live_time(&cx, live), Some(Some(0.5)), "the card's clock starts at its first frame");
    assert_eq!(canvas.designs.renders, renders, "advancing the clock renders nothing");

    let home = canvas.camera.pan;
    canvas.pan(&mut cx, home + dvec2(4000., 0.));
    draw(&mut cx, &mut canvas);
    assert!(!canvas.design_keep[0].contains(&live), "the card is off screen");
    assert_eq!(canvas.designs.live_time(&cx, live), None, "off screen it is a still, so it costs nothing");
    tick(&mut cx, &mut canvas, 11.);
    assert_eq!(canvas.card_frame, NextFrame::default(), "and asks for no frames");

    canvas.pan(&mut cx, home);
    draw(&mut cx, &mut canvas);
    for _ in 0..2 {
        design_settle(&mut cx, &mut canvas);
        draw(&mut cx, &mut canvas);
    }
    assert!(canvas.designs.busy(), "shown again once the camera settles, it renders again");
    finish_renders_reading_clock(&mut cx, &mut canvas, &mut draw, ids[0]);
    assert_eq!(canvas.designs.live_time(&cx, live), Some(None), "and is live again");
    canvas.designs.suspend(&mut cx);
    assert_eq!(canvas.designs.live_time(&cx, live), None, "a parked board's live cards become stills");
}

fn design_settle(cx: &mut Cx, canvas: &mut BoardCanvas) {
    let frame = canvas.design_settle;
    assert_ne!(frame, NextFrame::default(), "a zoom or a pan arms the settle check");
    let event = Event::NextFrame(NextFrameEvent { set: [frame].into_iter().collect(), ..Default::default() });
    canvas.handle_event(cx, &event, &mut Scope::empty());
}

#[test]
fn zooming_in_rerenders_a_design_once_at_the_settled_density() {
    let (mut cx, mut canvas, _, ids, size) = design_fixture(&[SLIDE]);
    let id = ids[0];
    let max_side = cx.gpu_info().max_texture_size.clamp(1, 4096) as f64;
    // The picture's width at a camera scale (dpi 1): the screen's density rounded up to a power
    // of two, capped by the GPU's texture side.
    let picture = |scale: f64| {
        let density = 2f64.powf(scale.log2().ceil().max(-3.)).min(max_side / 844.);
        let (w, h) = Cx::pass_pixel_size(dvec2(390., 844.), density);
        PictureState::Ready { w: w as u32, h: h as u32 }
    };
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    finish_design_renders(&mut cx, &mut canvas, &mut draw);
    let scale = canvas.camera.scale;
    let first = canvas.design_picture(id).expect("the first picture");
    assert_eq!(canvas.design_state(id), picture(scale), "the first picture matches the card's density on screen");
    assert_eq!(canvas.design_settle, NextFrame::default(), "opening the board arms no settle check");
    let renders = canvas.designs.renders;

    let centre = canvas.camera.view.center();
    canvas.zoom(&mut cx, centre, scale * 3.);
    let zoomed = canvas.camera.scale;
    assert_ne!(picture(zoomed), picture(scale), "the zoom needs a denser picture ({zoomed} from {scale})");
    for _ in 0..3 {
        draw(&mut cx, &mut canvas);
        assert_eq!((canvas.designs.renders, canvas.design_picture(id)), (renders, Some(first)), "zooming re-renders nothing");
        assert!(!canvas.designs.busy(), "no render starts while the camera moves");
    }
    design_settle(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    assert!(!canvas.designs.busy(), "the first settle check only sees the zoom's last frame");
    design_settle(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    assert!(canvas.designs.busy(), "a camera standing still for a frame upgrades the visible card");
    advance_design(&mut cx, &mut canvas, &mut draw, |c| {
        assert_eq!(c.design_picture(id), Some(first), "the old picture draws until the new one is whole");
        c.designs.pending_paint().is_some()
    });
    finish_design_renders(&mut cx, &mut canvas, &mut draw);
    let sharp = canvas.design_picture(id).unwrap();
    assert_ne!(sharp, first);
    assert_eq!(canvas.design_state(id), picture(zoomed), "the new picture matches the zoomed card");
    assert_eq!(canvas.designs.renders, renders + 1, "one re-render for the settled zoom");
    assert_eq!(canvas.design_settle, NextFrame::default());
    for _ in 0..3 { draw(&mut cx, &mut canvas); }
    assert_eq!((canvas.designs.renders, canvas.design_picture(id)), (renders + 1, Some(sharp)), "a settled board renders nothing more");

    canvas.zoom(&mut cx, centre, zoomed / 4.);
    draw(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    assert!(!canvas.designs.busy(), "zooming out re-renders nothing");
    assert_eq!((canvas.designs.renders, canvas.design_picture(id)), (renders + 1, Some(sharp)), "zooming out keeps the sharper picture");
}

#[test]
fn every_visible_design_ends_at_its_final_density() {
    let (mut cx, mut canvas, _, ids, size) = design_fixture(&[SLIDE, SLIDE]);
    let max_side = cx.gpu_info().max_texture_size.clamp(1, 4096) as f64;
    let picture = |scale: f64| {
        let density = 2f64.powf(scale.log2().ceil().max(-3.)).min(max_side / 844.);
        let (w, h) = Cx::pass_pixel_size(dvec2(390., 844.), density);
        PictureState::Ready { w: w as u32, h: h as u32 }
    };
    let mut draw = draw_pass(&mut cx, size, 1.);
    draw(&mut cx, &mut canvas);
    finish_design_renders(&mut cx, &mut canvas, &mut draw);
    let scale = canvas.camera.scale;
    let first = canvas.scene.as_ref().unwrap().view.get(&ids[0]).unwrap().rect;
    let on_screen = |camera: &Camera, x: f64, y: f64| camera.local_to_screen(camera.world_to_local(dvec2(x, y)));
    let anchor = on_screen(&canvas.camera, first.x0 + 20., first.y0 + 20.);
    // A slow pinch: it pauses for a frame, so the card upgrades early, then zooms on.
    canvas.zoom(&mut cx, anchor, scale * 2.);
    draw(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    assert!(canvas.designs.busy(), "the pause upgrades the card on screen");
    // The gesture zooms on while that render is under way, then stops.
    canvas.zoom(&mut cx, anchor, scale * 8.);
    let zoomed = canvas.camera.scale;
    draw(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    finish_design_renders(&mut cx, &mut canvas, &mut draw);
    assert_eq!(canvas.design_state(ids[0]), picture(zoomed), "the final stop leaves the card at the final density");
    assert!(canvas.drawn_design(ids[1]).is_none(), "the second card is off screen at this zoom");
    // A pan brings the second card in: it renders at the zoom it is now seen at.
    let second = canvas.scene.as_ref().unwrap().view.get(&ids[1]).unwrap().rect;
    let pan = canvas.camera.pan + anchor - on_screen(&canvas.camera, second.x0 + 20., second.y0 + 20.);
    canvas.pan(&mut cx, pan);
    draw(&mut cx, &mut canvas);
    assert!(canvas.drawn_design(ids[1]).is_some(), "the pan shows the second card");
    design_settle(&mut cx, &mut canvas);
    design_settle(&mut cx, &mut canvas);
    draw(&mut cx, &mut canvas);
    finish_design_renders(&mut cx, &mut canvas, &mut draw);
    assert_eq!(canvas.design_state(ids[1]), picture(zoomed), "the card a zoom left off screen ends at the final density too");
}
