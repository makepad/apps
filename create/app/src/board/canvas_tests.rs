use super::super::touched_by;
use super::*;
use crate::model::{Action, ItemStyle, Model, Origin};
use makepad_widgets::cx_draw::CxDraw;

fn draw_pass(cx: &mut Cx, size: DVec2, pressure: f64) -> impl FnMut(&mut Cx, &mut BoardCanvas) {
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
    let mut add = |x: i64| {
        model
            .act(
                Action::AddItem {
                    board,
                    kind: Key::Number(kind::STICKY),
                    x: x * 64,
                    y: 0,
                    w: 36 * 64,
                    h: 32 * 64,
                    parent: None,
                    style: ItemStyle { text: Some("Ink".into()), ..Default::default() },
                },
                Origin::Tool,
            )
            .unwrap()
    };
    let a = add(0);
    let b = add(41);
    let distant = add(300);
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
    assert_eq!(scene.layers, [0..1, 1..2], "the shadows cross that gap");
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
    let count = |m: &Model| makepad_work_core::log::Log::open(&m.home.room_log_path(&m.open_id())).unwrap().0.events().len();
    let n = count(&model);
    model.act(action.clone(), Origin::Ui).unwrap();
    assert_eq!(count(&model), n + 1, "the resize writes exactly one event");
    drop(model);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_future_constructor_has_a_readable_card_with_default_geometry() {
    use crate::board::tests::History;
    use makepad_work_core::{entity::field, event::{Content, Value}};
    let mut h = History::new();
    let board = h.board();
    let mut scene = Scene::new(&h.state, board);
    let (id, touched) = h.push(Content::Entity { kind: Key::Name("FutureCard".into()), derive: None,
        project: None, fields: vec![(field::PARENT, Value::Id(board))] });
    assert!(scene.apply(&h.state, &touched, &mut Gesture::default()));
    let card = scene.view.get(&id).unwrap();
    assert!(supported(&card.kind) && rect(card.rect).size.x > 0. && rect(card.rect).size.y > 0.);
    assert_eq!(scene.faces[&id].text, "unknown kind FutureCard");

    let (text, touched) = h.push(Content::Entity { kind: Key::Number(kind::TEXT_BLOCK), derive: None, project: None,
        fields: vec![(field::PARENT, Value::Id(board)), (field::W, Value::Int(100 * 64)),
            (field::H, Value::Int(32 * 64)), (field::TEXT, Value::Text("Text".into()))] });
    assert!(scene.apply(&h.state, &touched, &mut Gesture::default()));
    let (sticky, touched) = h.push(Content::Entity { kind: Key::Number(kind::STICKY), derive: None, project: None,
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
}

#[test]
fn canvas_draws_arriving_section_children_and_a_child_surviving_its_section() {
    use crate::board::tests::{change, item, set, History};
    use makepad_work_core::event::{Content, Value};

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
    use crate::model::tests::{exchange, shared_space};
    use makepad_platform::studio::StudioToApp;

    let (_homes, mut a, mut b) = shared_space("board-typing");
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
    assert!(colour == ink && colour[..3].iter().sum::<f32>() < 0.6, "a dark caret in the light default sticky: {colour:?}");
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

    canvas.open_text(&mut cx, id, false);
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
fn double_click_and_return_reopen_only_writable_unlocked_items() {
    use crate::board::tests::{change, item, set, History};
    use makepad_platform::studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp};
    use makepad_work_core::event::Value;

    let mut h = History::new();
    let board = h.board();
    // Neighbouring items share an edge, within double-click distance.
    let ids = [(kind::STICKY, 20), (kind::TEXT_BLOCK, 116), (kind::SHAPE, 212), (kind::SECTION, 308)].map(|(kind, x)| {
        h.push(item(kind, board, [x, 80, 96, 80], None)).0
    });

    let mut surface = WidgetRef::empty();
    let target = surface.clone();
    let mut cx = Cx::new(Box::new(move |cx, event| target.handle_event(cx, event, &mut Scope::empty())));
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
    let click = |cx: &mut Cx, x, time, shift| {
        let camera = surface.borrow::<BoardCanvas>().unwrap().camera;
        let p = camera.local_to_screen(camera.world_to_local(dvec2(x, 108.)));
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
    let key = |cx: &mut Cx, key_code| {
        let k = KeyEvent { key_code, ..Default::default() };
        for msg in [StudioToApp::KeyDown(k.clone()), StudioToApp::KeyUp(k)] {
            cx.dispatch_studio_msg(msg, WindowId(1, 1), DVec2::default());
        }
    };
    let editing = || surface.borrow::<BoardCanvas>().unwrap().editing.as_ref().map(|e| e.item);

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
