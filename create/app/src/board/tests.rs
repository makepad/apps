use super::*;
use makepad_work_core::entity::{field, kind};
use makepad_work_core::event::{Actor, Change, Content, Draft, Event, Id, Key, Member, Value, Via};
use makepad_work_core::fold::State;
use makepad_work_core::key::DeviceKey;
use std::collections::HashMap;
use std::time::Instant;

pub(super) struct History {
    key: DeviceKey,
    pub(super) peer: DeviceKey,
    room: Id,
    latest: HashMap<makepad_work_core::key::PublicKey, Id>,
    events: HashMap<Id, Event>,
    tip: Option<Id>,
    pub(super) state: State,
}

impl History {
    pub(super) fn new() -> Self {
        let key = DeviceKey::generate().unwrap();
        let peer = DeviceKey::generate().unwrap();
        let (room, genesis) = Event::found(&key, 2, 0, 0, [0; 16]).unwrap();
        let mut h = Self { key, peer, room, latest: HashMap::new(), events: HashMap::new(), tip: None, state: State::default() };
        h.record(genesis);
        let entry = h.push(Content::Member(Member::Invite { subject: h.peer.public(), role: 0 })).0;
        let accept = h.branch(&h.peer, Content::Member(Member::Accept { entry }));
        h.record(accept);
        h
    }

    pub(super) fn branch(&self, key: &DeviceKey, content: Content) -> Event {
        let bytes = content.encode();
        let mut parents: Vec<_> = self.tip.into_iter().collect();
        if let Some(prev) = self.latest.get(&key.public()) {
            if !parents.contains(prev) { parents.push(*prev); }
        }
        let draft = Draft {
            kind: content.kind(), kind_version: content.version(), critical: false,
            actor: Actor::Human, via: Via::Ui, wall_ms: 0,
            parents, content: &bytes,
        };
        Event::create(key, self.room, draft, (self.events.len() as u128).to_le_bytes()).unwrap()
    }

    pub(super) fn push(&mut self, content: Content) -> (Id, Vec<Id>) {
        let event = self.branch(&self.key, content);
        let touched = touched_by(&event, |id| self.events.get(id));
        let id = event.id;
        self.record(event);
        (id, touched)
    }

    fn record(&mut self, event: Event) {
        self.tip = Some(event.id);
        self.receive(event);
    }

    // A peer event can arrive while the local writer still works from its own tip.
    pub(super) fn receive(&mut self, event: Event) {
        self.state.receive(&event);
        self.latest.insert(event.header.author, event.id);
        self.events.insert(event.id, event);
    }

    fn apply(&mut self, view: &mut BoardView, event: Content) -> Id {
        let (id, touched) = self.push(event);
        apply_delta(view, &self.state, &touched);
        id
    }

    pub(super) fn board(&mut self) -> Id {
        self.push(Content::Entity { kind: Key::Number(kind::BOARD), project: None, derive: None, fields: vec![] }).0
    }
}

pub(super) fn item(kind: u16, parent: Id, [x, y, w, h]: [i64; 4], z: Option<u8>) -> Content {
    let mut fields = vec![(field::PARENT, Value::Id(parent)), (field::X, Value::Int(x * 64)),
        (field::Y, Value::Int(y * 64)), (field::W, Value::Int(w * 64)), (field::H, Value::Int(h * 64))];
    if let Some(z) = z {
        // Rank one precedes [2] and needs a non-minimal final digit.
        let key = if z == 1 { vec![1, 2] } else { vec![z] };
        fields.push((field::Z, Value::Key(key)));
    }
    Content::Entity { kind: Key::Number(kind), project: None, derive: None, fields }
}

pub(super) fn change(entity: Id, field: Key, value: Value) -> Change {
    Change { entity, field, value }
}

pub(super) fn set(changes: Vec<Change>) -> Content {
    Content::Set { changes, label: None }
}

fn all() -> Rect {
    Rect::new(-f64::MAX, -f64::MAX, f64::MAX, f64::MAX)
}

fn placed(kind: u16, board: Id, parent: Id, x: i64) -> Content {
    let mut content = item(kind, parent, [x, 0, 5, 5], Some(x as u8));
    if let Content::Entity { fields, .. } = &mut content { fields.push((field::BOARD, Value::Id(board))); }
    content
}

// A consumer retaining membership and parentage must be able to follow only the reported ids.
fn apply_delta(view: &mut BoardView, state: &State, touched: &[Id]) {
    let parents = |view: &BoardView| -> HashMap<_, _> {
        view.visible(all()).into_iter().map(|id| (id, view.get(&id).unwrap().parent_section)).collect()
    };
    let mut retained = parents(view);
    for id in view.apply(state, touched) {
        if let Some(item) = view.get(&id) {
            retained.insert(id, item.parent_section);
        } else {
            assert!(retained.remove(&id).is_some(), "only board records can be reported as removed");
        }
    }
    assert_eq!(retained, parents(view), "the delta covers every membership and parent change");
}

fn both_orders(base: &State, board: Id, left: &Event, right: &Event, check: impl Fn(&BoardView)) {
    for events in [[left, right], [right, left]] {
        let mut state = base.clone();
        let mut view = BoardView::build(&state, board);
        for event in events {
            state.receive(event);
            apply_delta(&mut view, &state, &touched_by(event, |_| None));
        }
        check(&view);
        check(&BoardView::build(&state, board));
    }
}

fn check_hanging(view: &BoardView, id: Id, x: f64) {
    let item = view.get(&id).expect("the hanging item is on this board");
    assert_eq!(item.parent_section, None, "the hanging item is a direct child of the board");
    assert_eq!(item.rect, Rect::new(x, 0.0, x + 5.0, 5.0), "the stored geometry survives");
    assert_eq!(view.hit((x + 1.0, 1.0), 0.0), Some(id), "the spatial index follows the tree");
}

#[test]
fn concurrent_reparent_cycle_resolves_the_same() {
    let mut h = History::new();
    let board = h.board();
    let a = h.push(placed(kind::SECTION, board, board, 10)).0;
    let b = h.push(placed(kind::SECTION, board, board, 20)).0;
    let left = h.branch(&h.key, set(vec![change(a, field::PARENT, Value::Id(b))]));
    let right = h.branch(&h.peer, set(vec![change(b, field::PARENT, Value::Id(a))]));
    both_orders(&h.state, board, &left, &right, |view| {
        assert_eq!(view.visible(all()), [a, b], "both replicas keep both sections in painter order");
        check_hanging(view, a, 10.0);
        check_hanging(view, b, 20.0);
    });
}

#[test]
fn moved_sections_hang_from_their_new_board_after_a_concurrent_cycle() {
    let mut h = History::new();
    let one = h.board();
    let two = h.board();
    let a = h.push(placed(kind::SECTION, one, one, 10)).0;
    let b = h.push(placed(kind::SECTION, one, one, 20)).0;
    let seen = h.push(placed(kind::STICKY, one, a, 30)).0;
    let unseen = h.branch(&h.peer, placed(kind::STICKY, one, a, 40));
    let mut changes = vec![change(a, field::PARENT, Value::Id(two)), change(b, field::PARENT, Value::Id(two))];
    super::tree::add_home_changes(&h.state, &mut changes);
    let transfer = h.push(set(changes)).0;
    h.receive(unseen.clone());
    for id in [a, b, seen] {
        let home = &h.state.entities[&id].fields[&field::BOARD];
        assert_eq!((&home.value, home.writer), (&Value::Id(two), transfer), "one SET changes every observed home");
    }
    assert_eq!(h.state.entities[&unseen.id].value(field::BOARD), Some(&Value::Id(one)));
    assert_eq!(BoardView::build(&h.state, two).visible(all()), [a, b, seen, unseen.id], "intact paths win over home");

    let left = h.branch(&h.key, set(vec![change(a, field::PARENT, Value::Id(b))]));
    let right = h.branch(&h.peer, set(vec![change(b, field::PARENT, Value::Id(a))]));
    both_orders(&h.state, two, &left, &right, |view| {
        assert_eq!(view.visible(all()), [a, b, seen]);
        for (id, x) in [(a, 10.0), (b, 20.0), (seen, 30.0)] { check_hanging(view, id, x); }
    });
    both_orders(&h.state, one, &left, &right, |view| {
        assert_eq!(view.visible(all()), [unseen.id], "only the concurrently added child's home stayed on One");
        check_hanging(view, unseen.id, 40.0);
    });
}

#[test]
fn section_delete_spares_an_unseen_child() {
    let mut h = History::new();
    let one = h.board();
    let two = h.board();
    let section = h.push(placed(kind::SECTION, one, one, 10)).0;
    let child = h.branch(&h.peer, placed(kind::STICKY, one, section, 20));
    h.push(set(vec![change(section, field::PARENT, Value::Id(two)), change(section, field::BOARD, Value::Id(two))]));
    let delete = h.branch(&h.key, set(vec![change(section, field::DELETED, Value::Bool(true))]));
    both_orders(&h.state, two, &delete, &child, |view| {
        assert_eq!(view.visible(all()), [child.id], "the unseen child follows the deleted section through to Two");
        check_hanging(view, child.id, 20.0);
    });
    both_orders(&h.state, one, &delete, &child, |view| assert!(view.visible(all()).is_empty()));
}

#[test]
fn incremental_board_matches_rebuild_and_spatial_scan_after_each_event() {
    let mut history = History::new();
    let board = history.board();
    let other = history.board();
    let section = history.push(item(kind::SECTION, board, [-500, -500, 4000, 2000], None)).0;
    let mut view = BoardView::build(&history.state, board);
    let mut seed = 2026u32;
    let mut draw = |bound: u32| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((seed as u64 * bound as u64) >> 32) as i64
    };
    let mut items = Vec::new();
    let mut last = section;
    for step in 0..48 {
        let x = draw(1800) - 900;
        let y = draw(1400) - 700;
        let w = draw(600) + 1;
        let h = draw(480) + 1;
        let add = step % 8 < 3;
        let content = if add {
            let parent = if draw(2) == 0 { board } else { section };
            item(kind::STICKY, parent, [x, y, w, h], Some(draw(3) as u8 + 2))
        } else {
            let id = items[draw(items.len() as u32) as usize];
            match step % 8 {
                3 => set(vec![change(id, field::X, Value::Int(x * 64 - 1)), change(id, field::Y, Value::Int(y * 64))]),
                4 => set(vec![change(id, field::W, Value::Int(w * 64)), change(id, field::H, Value::Int(h * 64))]),
                5 => set(vec![change(id, field::DELETED, Value::Bool(true)), change(items[0], field::ARCHIVED, Value::Bool(true))]),
                6 => Content::Undo { of: last },
                _ => set(vec![change(section, field::PARENT, Value::Id(if step % 16 == 7 { other } else { board })),
                              change(id, field::Z, Value::Key(vec![2]))]),
            }
        };
        last = history.apply(&mut view, content);
        if add { items.push(last); }
        let rebuilt = BoardView::build(&history.state, board);
        let section_here = history.state.entities[&section].value(field::PARENT) == Some(&Value::Id(board));
        let mut expected: Vec<_> = history.state.entities.values().filter(|e| {
            !e.flag(field::DELETED) && !e.flag(field::ARCHIVED) &&
                (e.value(field::PARENT) == Some(&Value::Id(board)) ||
                    (section_here && e.value(field::PARENT) == Some(&Value::Id(section))))
        }).collect();
        expected.sort_unstable_by(|a, b| (a.z_key(), a.id).cmp(&(b.z_key(), b.id)));
        let ids: Vec<_> = expected.iter().map(|e| e.id).collect();
        assert_eq!(view.visible(all()), ids, "board membership and painter order at step {step}");
        for id in items.iter().chain([&section]) {
            assert_eq!(view.get(id), rebuilt.get(id), "incremental record at step {step}");
        }
        // Scan every record independently of either index, including near a cell edge and far away.
        for (qx, qy) in [(x as f64, y as f64), (-256.0, 256.0), (1.0e16, -1.0e16)] {
            let rect = Rect::new(qx, qy, qx + 400.0, qy + 220.0);
            let scan: Vec<_> = ids.iter().copied().filter(|id| rect.intersects(rebuilt.get(id).unwrap().rect)).collect();
            assert_eq!(view.visible(rect), scan, "rect versus scan at step {step}");
            assert_eq!(rebuilt.visible(rect), scan, "rebuilt rect versus scan at step {step}");
            let point = Rect::new(qx, qy, qx, qy);
            let at_point: Vec<_> = ids.iter().filter(|id| rebuilt.get(id).unwrap().rect.encloses(point)).copied().collect();
            assert_eq!(view.visible(point), at_point, "every point candidate versus scan at step {step}");
            assert_eq!(view.hit((qx, qy), 0.0), at_point.last().copied(), "top hit versus scan at step {step}");
        }
    }
}

#[test]
fn hit_uses_z_writer_then_id_with_locked_deleted_and_missing_keys() {
    let mut history = History::new();
    let board = history.board();
    let mut view = BoardView::build(&history.state, board);
    let geometry = [240, 0, 15, 20];
    let a = history.apply(&mut view, item(kind::STICKY, board, geometry, Some(2)));
    let b = history.apply(&mut view, item(kind::SHAPE, board, geometry, Some(2)));
    let c = history.apply(&mut view, item(kind::IMAGE, board, geometry, None));
    history.apply(&mut view, set(vec![change(a, field::COLOUR, Value::Int(0xff0011)), change(b, field::LOCKED, Value::Bool(true))]));
    assert_eq!(view.get(&a).unwrap().colour, Some(0xff0011));
    assert_eq!(view.get(&a).unwrap().text_handle(), a);
    assert!(view.get(&b).unwrap().locked && view.get(&b).unwrap().selectable);
    assert_eq!(view.visible(all()), [c, a, b], "a colour edit leaves equal Z keys in writer order");
    assert_eq!(view.hit((256.0, 5.0), 1.0), Some(b), "hit across a cell boundary includes the locked item");
    assert_eq!(view.hit((256.0, 5.0), 0.5), None);
    let deletion = history.apply(&mut view, set(vec![change(b, field::DELETED, Value::Bool(true))]));
    assert_eq!(view.hit((245.0, 5.0), 0.0), Some(a), "deleted items leave the index");
    let undo = history.apply(&mut view, Content::Undo { of: deletion });
    assert_eq!(view.hit((245.0, 5.0), 0.0), Some(b));
    history.apply(&mut view, Content::Undo { of: undo });
    assert!(view.get(&b).is_none(), "redo invalidates the same entity");
    let group = history.apply(&mut view, set(vec![change(b, field::DELETED, Value::Null),
        change(b, field::LOCKED, Value::Null), change(b, field::Z, Value::Key(vec![3])),
        change(a, field::Z, Value::Key(vec![3]))]));
    assert_eq!(view.visible(all()), [c, a.min(b), a.max(b)], "one Z writer ties by entity id");
    history.apply(&mut view, Content::Undo { of: group });
    assert_eq!(view.hit((245.0, 5.0), 0.0), Some(a));
    let (_, text_touches) = history.push(Content::Text { doc: c, ops: Vec::new() });
    assert!(text_touches.is_empty(), "text events have no board delta");
}

#[test]
fn marquee_encloses_sections_and_reparenting_moves_their_contents() {
    let mut history = History::new();
    let board = history.board();
    let other = history.board();
    let section = history.push(item(kind::SECTION, board, [-100, -100, 200, 200], None)).0;
    let sticky = history.push(item(kind::STICKY, section, [0, 0, 20, 20], Some(2))).0;
    let mut view = BoardView::build(&history.state, board);
    assert_eq!(view.get(&sticky).unwrap().parent_section, Some(section));
    assert_eq!(view.marquee(Rect::new(-2.0, -2.0, 5.0, 5.0)), [sticky], "partial sticky inside a section");
    assert_eq!(view.marquee(Rect::new(100.0, 100.0, -100.0, -100.0)), [section, sticky], "exact enclosure");
    let moved = history.apply(&mut view, set(vec![change(section, field::PARENT, Value::Id(other))]));
    assert!(view.visible(all()).is_empty(), "the section carries its child off the board");
    history.apply(&mut view, Content::Undo { of: moved });
    assert_eq!(view.visible(all()), [section, sticky], "undo brings the child back at its old absolute position");
    let archive = history.apply(&mut view, set(vec![change(board, field::ARCHIVED, Value::Bool(true))]));
    assert!(view.visible(all()).is_empty(), "archiving the board hides its contents");
    history.apply(&mut view, Content::Undo { of: archive });
    assert_eq!(view.visible(all()), [section, sticky], "restoring the board restores its contents");
}

#[test]
fn layers_partition_visible_z_order_for_a_grid_and_a_stack() {
    let mut history = History::new();
    let board = history.board();
    let mut stack = Vec::new();
    for i in 0..12 {
        let id = history.push(item(kind::STICKY, board, [(i % 4) * 20, (i / 4) * 20, 20, 20], Some(i as u8 + 1))).0;
        stack.push(change(id, field::X, Value::Int(0)));
        stack.push(change(id, field::Y, Value::Int(0)));
    }
    let mut view = BoardView::build(&history.state, board);
    let visible = view.visible(all());
    assert_eq!(view.layers(&visible), vec![0..12], "edge-adjacent grid fits in one layer");
    history.apply(&mut view, set(stack));
    let ranges = view.layers(&visible);
    assert!(ranges.iter().all(|r| r.len() == 1), "each overlapping card needs its own layer");
    let flattened: Vec<_> = ranges.into_iter().flat_map(|range| visible[range].iter().copied()).collect();
    assert_eq!(flattened, view.visible(all()), "layers cover the visible items in painter order");
    assert!(view.layers(&[]).is_empty());
}

#[test]
fn rect_and_point_queries_match_every_handle_across_grid_and_large_list_updates() {
    let mut index = RectIndex::new();
    let local = Rect::new(-320.0, -32.0, 256.0, 256.0);
    let large = Rect::new(-5000.0, -5000.0, 5000.0, 5000.0);
    let far = Rect::new(1e100, -1e120, 1e100, -1e120);
    let mut records = HashMap::new();
    let mut hits = Vec::new();
    for (id, rect) in [(99, Some(local)), (42, Some(local)), (7, Some(large)), (3, Some(large)),
        (u32::MAX, Some(far)), (99, Some(large)), (7, Some(local)), (42, None),
        (7, Some(far)), (99, None), (3, Some(local)), (u32::MAX, None)] {
        if let Some(rect) = rect {
            records.insert(id, rect);
            index.update(id, rect);
        } else {
            records.remove(&id);
            index.remove(id);
        }
        for query in [local, large, far, all(), Rect::new(-256.0, 0.0, -256.0, 0.0)] {
            let mut expected: Vec<_> = records.iter().filter(|(_, rect)| rect.intersects(query)).map(|(&id, _)| id).collect();
            expected.sort_unstable();
            index.query_rect(query, &mut hits);
            assert_eq!(hits, expected, "rect including cell boundaries after updating {id}");
            let point = Rect::new(query.x0, query.y0, query.x0, query.y0);
            expected.retain(|id| records[id].encloses(point));
            index.query_point((point.x0, point.y0), &mut hits);
            assert_eq!(hits, expected, "point including large items after updating {id}");
        }
    }
}

#[test]
fn query_cost_on_five_thousand_items() {
    let mut index = RectIndex::new();
    for handle in 0..5000 {
        let x = f64::from(handle % 100) * 112.0;
        let y = f64::from(handle / 100) * 80.0;
        index.insert(handle, Rect::new(x, y, x + 96.0, y + 64.0));
    }
    let viewport = Rect::new(2200.0, 1400.0, 4120.0, 2480.0);
    let point = (2288.0, 1460.0);
    let mut hits = Vec::new();
    index.query_rect(viewport, &mut hits);
    let visible = hits.len();
    const SAMPLES: u32 = 128;
    let start = Instant::now();
    for _ in 0..SAMPLES {
        index.query_rect(std::hint::black_box(viewport), &mut hits);
        std::hint::black_box(&hits);
    }
    let rect = start.elapsed() / SAMPLES;
    let start = Instant::now();
    for _ in 0..SAMPLES {
        index.query_point(std::hint::black_box(point), &mut hits);
        std::hint::black_box(&hits);
    }
    let point = start.elapsed() / SAMPLES;
    eprintln!("5000 items, 1920x1080 at 1x ({visible} visible), mean of {SAMPLES}: query_rect {rect:?}, query_point {point:?}");
}

#[test]
fn values_read_by_splash_pass_the_log_literal_check() {
    for src in [r#""a \" ]""#, "@tag", "#abc", "#001122ff", "-12", ".5", "-1.25e+2", "1_000",
                "accent", "theme.accent", "true", "false", "nil", "{x: 1}", "[4 2]",
                "View { width: Fill height: 1 }", "f (1)", "f/*arg*/(1)", "return {title: \"a\"}",
                "struct // comment\n { x: 3 }", r#"Sticky{text: "a \" } b"}"#] {
        assert!(makepad_splash_syntax::read_value(src).is_ok(), "reader fixture: {src:?}");
        assert!(makepad_work_core::literal::is_literal(src), "a reader value must fold: {src:?}");
    }
}

#[test]
fn page_print_is_equal_from_shuffled_arrival() {
    use makepad_work_core::hex;
    use makepad_widgets::makepad_script::tokenizer::{lex, ScriptToken};

    let mut h = History::new();
    let board = h.board();
    let section = h.push(placed(kind::SECTION, board, board, 10)).0;
    let nested = h.push(placed(kind::SECTION, board, section, 20)).0;
    let sticky = h.push(placed(kind::STICKY, board, nested, 30)).0;
    let deleted = h.push(placed(kind::SECTION, board, board, 40)).0;
    let orphan = h.push(placed(kind::SHAPE, board, deleted, 50)).0;
    let text = "a \"quote\" ]} \\\n\t\u{0008}雪";
    h.push(set(vec![change(deleted, field::DELETED, Value::Bool(true)),
        change(board, field::TITLE, Value::Text("A board".into())),
        change(sticky, field::X, Value::Int(i64::MIN + 1)), change(sticky, field::W, Value::Int(i64::MAX)),
        change(sticky, field::TEXT, Value::Text(text.into()))]));
    let mut events: Vec<_> = h.events.values().cloned().collect();
    events.push(h.branch(&h.peer, set(vec![change(orphan, field::SHAPE, Value::Text("ellipse".into())),
        change(orphan, field::COLOUR, Value::Int(0x11223380))])));
    events.push(h.branch(&h.key, set(vec![change(sticky, field::COLOUR, Value::Int(0x001122))])));
    events.sort_by_key(|e| e.id);
    let mut first = None;
    for turn in 0..3 {
        if turn == 1 { events.reverse(); }
        if turn == 2 { events.rotate_left(3); }
        let mut state = State::default();
        for event in &events { state.receive(event); }
        let source = page::print(&state, board).unwrap();
        let reply = page::read(&state, board, None, None).unwrap();
        let parsed = makepad_strict_json::parse(reply.as_bytes()).unwrap();
        assert_eq!(parsed.get("splash").unwrap().as_str(), Some(source.as_str()));
        if let Some(expected) = &first { assert_eq!(expected, &reply, "both Splash and revision converge"); }
        else { first = Some(reply); }
        assert!(source.starts_with(&format!("Board{{title: \"A board\" items: [Section{{id: @{} ", hex(&section))));
        assert!(source.contains(&format!("items: [Section{{id: @{} ", hex(&nested))));
        assert!(source.contains(&format!("items: [Sticky{{id: @{} x: -144115188075855871.984375 y: 0 w: 144115188075855871.984375", hex(&sticky))));
        assert!(source.contains(&format!("}}]}}]}} Shape{{id: @{} ", hex(&orphan))), "the orphan is a root sibling");
        assert!(!source.contains(&hex(&deleted)));
        assert!(source.contains("color: #001122") && source.contains("color: #11223380 shape: @ellipse"));
        let mut stack = Vec::new();
        let mut body = false;
        for t in lex(&source) {
            match t.token() {
                ScriptToken::OpenCurly => stack.push('}'), ScriptToken::OpenSquare => stack.push(']'),
                ScriptToken::CloseCurly => assert_eq!(stack.pop(), Some('}')),
                ScriptToken::CloseSquare => assert_eq!(stack.pop(), Some(']')),
                ScriptToken::StringUnfinished => panic!("unterminated Splash string"),
                ScriptToken::String(_) => body |= t.string() == Some(text),
                _ => {}
            }
        }
        assert!(stack.is_empty() && body, "Splash lexes balanced delimiters and the exact body string");
    }
}

#[test]
fn named_constructors_and_open_registers_print_losslessly_after_schema_fields() {
    let mut h = History::new();
    let board = h.board();
    let name = |s: &str| Key::Name(s.into());
    let source = |s: &str| Value::Literal(s.into());
    let schema = schema::Vocabulary::Board.schema();
    let (section, _) = h.push(Content::Entity { kind: schema.kind_for_write(name("Section")), derive: None, project: None,
        fields: vec![(field::PARENT, Value::Id(board)), (name("footer"), source("accent"))] });
    let (id, _) = h.push(Content::Entity { kind: schema.kind_for_write(name("FutureDiagram")), derive: None, project: None,
        fields: vec![(field::PARENT, Value::Id(section)), (name("spec"), source("Diagram{ data: [4  2] }")),
            (name("dash"), source("[4  2] // note")), (field::LABEL, Value::Text("v".into()))] });
    h.push(set(vec![change(board, name("theme"), source("dark"))]));
    let printed = page::print(&h.state, board).unwrap();
    assert!(printed.contains(&format!("FutureDiagram{{id: @{}", makepad_work_core::hex(&id))));
    assert!(printed.ends_with("label: \"v\" dash: [4  2] // note\n spec: Diagram{ data: [4  2] }}] footer: accent}] theme: dark}"));
    assert_eq!(board_page(&h.state, board, None).unwrap().get("splash").unwrap().as_str(), Some(printed.as_str()));
    let view = BoardView::build(&h.state, board);
    assert_eq!(view.get(&id).unwrap().parent_section, Some(section));
    assert_eq!(view.get(&id).unwrap().kind, name("FutureDiagram"));
}

fn board_page(state: &State, board: Id, cursor: Option<&str>) -> Result<makepad_strict_json::Value, String> {
    let after = cursor.map(|s| page::Cursor::parse(s).expect("issued cursor"));
    let reply = page::read(state, board, None, after)?;
    assert!(reply.len() <= 15_872, "the full JSON reply fits on the wire: {} bytes", reply.len());
    Ok(makepad_strict_json::parse(reply.as_bytes()).unwrap())
}

#[test]
fn oversized_node_and_title_fragments_follow_their_own_bytes() {
    use makepad_work_core::hex;

    let mut h = History::new();
    let board = h.board();
    let sticky = h.push(placed(kind::STICKY, board, board, 10)).0;
    let body = "雪🌱\"\\ab\n".repeat(3000);
    h.push(set(vec![change(sticky, field::TEXT, Value::Text(body.clone()))]));
    let single = page::print(&h.state, board).unwrap();
    let expected = single.strip_prefix("Board{title: \"\" items: [").unwrap().strip_suffix("]}").unwrap();
    let mut reply = board_page(&h.state, board, None).unwrap();
    let revision = reply.get("revision").unwrap().clone();
    let cursor = reply.get("next").unwrap().as_str().unwrap().to_owned();
    let total = reply.get("fragment").unwrap().get("total").unwrap().as_i64().unwrap();
    assert!(total > 1);
    // Change both the board revision and the number of matching items during the read.
    h.push(placed(kind::STICKY, board, board, 20));
    let mut joined = String::new();
    for index in 1..=total {
        let part = reply.get("fragment").unwrap();
        assert_eq!(part.get("index").unwrap().as_i64(), Some(index));
        assert_eq!(part.get("total").unwrap().as_i64(), Some(total));
        assert_eq!(part.get("id").unwrap().as_str(), Some(hex(&sticky).as_str()));
        joined.push_str(reply.get("splash").unwrap().as_str().unwrap());
        if index < total {
            reply = board_page(&h.state, board, reply.get("next").unwrap().as_str()).unwrap();
            assert_ne!(reply.get("revision").unwrap(), &revision);
        }
    }
    assert_eq!(joined, expected, "fragments reconstruct the exact single node print");
    assert_eq!(reply.get("next").unwrap().as_str(), Some(hex(&sticky).as_str()), "ordinary id cursor resumes");
    h.push(set(vec![change(sticky, field::TEXT, Value::Text("replacement".into()))]));
    assert!(board_page(&h.state, board, Some(&cursor)).unwrap_err().contains("read again"));

    let title = "x🌱".repeat(4500);
    h.push(set(vec![change(board, field::TITLE, Value::Text(title.clone()))]));
    let mut reply = board_page(&h.state, board, None).unwrap();
    let first = reply.get("next").unwrap().as_str().unwrap().to_owned();
    let mut joined = String::new();
    loop {
        let frag = reply.get("fragment").unwrap();
        assert_eq!(frag.get("kind").unwrap().as_str(), Some("title"));
        joined.push_str(reply.get("splash").unwrap().as_str().unwrap());
        let next = reply.get("next").unwrap().as_str().unwrap();
        if next == hex(&board) {
            let nodes = board_page(&h.state, board, Some(next)).unwrap();
            assert!(nodes.get("splash").unwrap().as_str().unwrap().starts_with("Board{items: [Sticky{"));
            break
        }
        reply = board_page(&h.state, board, Some(next)).unwrap();
    }
    assert_eq!(joined, format!("\"{title}\""));
    h.push(set(vec![change(board, field::TITLE, Value::Text("short".into()))]));
    assert!(board_page(&h.state, board, Some(&first)).unwrap_err().contains("read again"));
}

#[test]
fn sections_page_as_whole_subtrees() {
    use makepad_work_core::hex;

    let mut h = History::new();
    let board = h.board();
    let first = h.push(placed(kind::STICKY, board, board, 10)).0;
    let section = h.push(placed(kind::SECTION, board, board, 20)).0;
    let a = h.push(placed(kind::STICKY, board, section, 30)).0;
    let b = h.push(placed(kind::STICKY, board, section, 40)).0;
    let sibling = h.push(placed(kind::SHAPE, board, board, 50)).0;
    let text = Value::Text("xy".repeat(4000));
    h.push(set(vec![change(first, field::TEXT, text.clone()), change(a, field::TEXT, text.clone())]));

    let one = board_page(&h.state, board, None).unwrap();
    assert_eq!(one.get("next").unwrap().as_str(), Some(hex(&first).as_str()));
    assert!(!one.get("splash").unwrap().as_str().unwrap().contains("Section{"));
    let two = board_page(&h.state, board, one.get("next").unwrap().as_str()).unwrap();
    let source = two.get("splash").unwrap().as_str().unwrap();
    for item in [section, a, b, sibling] { assert!(source.contains(&hex(&item))); }
    assert!(two.get("next").unwrap().is_null() && two.get("fragment").is_none());

    h.push(set(vec![change(b, field::TEXT, text)]));
    let whole = page::print(&h.state, board).unwrap();
    let expected = &whole[whole.find("Section{").unwrap()..whole.find(" Shape{").unwrap()];
    let mut joined = String::new();
    let mut next = hex(&first);
    loop {
        let reply = board_page(&h.state, board, Some(&next)).unwrap();
        assert_eq!(reply.get("fragment").unwrap().get("id").unwrap().as_str(), Some(hex(&section).as_str()));
        joined.push_str(reply.get("splash").unwrap().as_str().unwrap());
        next = reply.get("next").unwrap().as_str().unwrap().to_owned();
        if next == hex(&section) { break }
    }
    assert_eq!(joined, expected, "an oversized section fragments its exact nested print");
    let rest = board_page(&h.state, board, Some(&next)).unwrap();
    assert!(rest.get("splash").unwrap().as_str().unwrap().contains(&hex(&sibling)));
    assert!(rest.get("next").unwrap().is_null());
}

#[test]
fn window_section_capture_survives_peer_edits_and_undo() {
    use super::gesture::Gesture;
    use crate::model::{Action, ItemStyle, Model, Origin};
    use makepad_widgets::dvec2;

    // Each model signs its own branch. Deliver the real sync messages between gestures,
    // so my release names the received event in its past, just as on a linked device.
    use crate::model::tests::{exchange as deliver, shared_space};

    let (_homes, mut me, mut peer) = shared_space("board-move");
    let board = me.act(Action::AddBoard { project: me.project().id, title: "Plan".into() }, Origin::Ui).unwrap();
    let add = |m: &mut Model, kind, parent, x: i64| {
        m.act(Action::AddItem {
            board, kind: Key::Number(kind), parent, x: x * 64, y: 0, w: 24 * 64, h: 20 * 64,
            style: ItemStyle::default(),
        }, Origin::Ui).unwrap()
    };
    let section = add(&mut me, kind::SECTION, None, 0);
    let child = add(&mut me, kind::STICKY, Some(section), 10);
    let pinned = add(&mut me, kind::STICKY, Some(section), 20);
    let removed = add(&mut me, kind::STICKY, Some(section), 30);
    me.act(Action::SetFields {
        changes: vec![change(pinned, field::LOCKED, Value::Bool(true))], label: None,
    }, Origin::Ui).unwrap();
    deliver(&mut me, &mut peer);

    let mut view = BoardView::build(&me.state, board);
    let mut g = Gesture::default();
    g.selection.ids.extend([section, child]);
    g.press(&view, Some(section), dvec2(0., 0.), dvec2(0., 0.), false, None, false);
    g.motion(&view, dvec2(7., 3.), dvec2(7., 3.), 1., true, false);
    let arrival = add(&mut peer, kind::STICKY, Some(section), 40);
    let shift = peer.act(Action::MoveItems { moves: vec![(child, 90 * 64, 0)], members: None }, Origin::Ui).unwrap();
    let colour = peer.act(Action::SetFields {
        changes: vec![change(child, field::COLOUR, Value::Int(0x119933))], label: None,
    }, Origin::Ui).unwrap();
    let deleted = peer.act(Action::DeleteItems { items: vec![removed], members: None }, Origin::Ui).unwrap();
    deliver(&mut peer, &mut me);
    for id in [arrival, shift, colour, deleted] {
        let ids = touched_by(me.event(&id).unwrap(), |id| me.event(id));
        let changed = view.apply(&me.state, &ids);
        g.fold_changed(&view, &changed);
    }

    let p = g.preview.as_ref().unwrap();
    assert_eq!(view.get(&child).unwrap().colour, Some(0x119933), "the peer's colour shows through");
    assert_eq!(p.rect(&child).unwrap().x0, 17., "the peer's move leaves the preview under the pointer");
    let preview: Vec<_> = p.items.keys().map(|id| (*id, p.rect(id).unwrap())).collect();
    let action = g.release(&view).unwrap();
    let Action::MoveItems { moves, members: Some(captured) } = &action else {
        panic!("one absolute move on release");
    };
    assert_eq!(moves.len(), 2, "a doubly selected child is moved once");
    assert_eq!(*captured, [child].into(), "late, pinned and deleted children are excluded");
    let count = |m: &Model| makepad_work_core::log::Log::open(&m.home.room_log_path(&m.open_id())).unwrap().0.events().len();
    let before = count(&me);
    let moved = me.act(action, Origin::Ui).unwrap();
    assert_eq!(count(&me), before + 1);
    let touched = touched_by(me.event(&moved).unwrap(), |id| me.event(id));
    view.apply(&me.state, &touched);
    for (id, r) in preview {
        assert_eq!(view.get(&id).unwrap().rect, r, "drop matches the preview");
    }
    assert_eq!(view.get(&pinned).unwrap().rect.x0, 20.);
    assert_eq!(view.get(&pinned).unwrap().parent_section, Some(section));
    assert_eq!(view.get(&arrival).unwrap().rect.x0, 40.);
    assert!(view.get(&removed).is_none(), "my drop cannot resurrect the peer's deletion");

    me.act(Action::Undo { of: moved }, Origin::Ui).unwrap();
    view.apply(&me.state, &touched);
    assert_eq!(view.get(&child).unwrap().rect.x0, 90., "undo restores the peer's position");
    let del = g.delete(&view).unwrap();
    let deleted = me.act(del, Origin::Ui).unwrap();
    view.apply(&me.state, &touched_by(me.event(&deleted).unwrap(), |id| me.event(id)));
    assert_eq!(view.visible(all()), [pinned]);
    assert_eq!(view.get(&pinned).unwrap().parent_section, None, "the locked child hangs on the board");
    drop((peer, me));
}


#[test]
fn arrange_keeps_locks_and_peer_order_across_equal_and_keyless_bounds() {
    use super::gesture::{AlignTo, Gesture};
    use crate::model::{Action, ItemStyle, Model, Origin, RestackTo};
    use crate::tools::{self, Request};

    use crate::model::tests::{exchange, shared_space};

    let (_homes, mut me, mut peer) = shared_space("arrange");
    let board = me.act(Action::AddBoard { project: me.project().id, title: "Arrange".into() }, Origin::Ui).unwrap();
    let add = |m: &mut Model, x| m.act(Action::AddItem {
        board, kind: Key::Number(kind::STICKY), parent: None, x, y: 0, w: 1024, h: 1024, style: ItemStyle::default(),
    }, Origin::Tool).unwrap();
    let floor = add(&mut me, 0);
    let mover = add(&mut me, 256);
    let a = add(&mut me, 512);
    let b = add(&mut me, 768);
    let set_fields = |m: &mut Model, changes| m.act(Action::SetFields { changes, label: None }, Origin::Tool).unwrap();
    set_fields(&mut me, vec![change(floor, field::Z, Value::Null), change(mover, field::Z, Value::Key(vec![64])),
        change(a, field::Z, Value::Key(vec![128])), change(b, field::Z, Value::Key(vec![128])),
        change(b, field::LOCKED, Value::Bool(true))]);
    let stack = |m: &Model| BoardView::build(&m.state, board).visible(all());
    let original = stack(&me);
    let count = |m: &Model| makepad_work_core::log::Log::open(&m.home.room_log_path(&m.open_id())).unwrap().0.events().len();
    let n = count(&me);
    let refused = me.act(Action::Restack { items: vec![mover], to: RestackTo::Backward }, Origin::Ui).unwrap_err();
    assert!(refused.contains("already as far back") && count(&me) == n, "no byte key goes below a keyless item");

    let args = format!(r#"{{"items":["{}","{}"],"to":"forward"}}"#,
        makepad_work_core::hex(&mover), makepad_work_core::hex(&b));
    let Request::Do(action) = tools::read("item_restack", &args).unwrap() else { panic!("expected restack") };
    let event = me.act(action, Origin::Tool).unwrap();
    let reply = tools::action_reply(&me, &event, "item_restack");
    assert!(reply.contains("locked item stayed") && reply.contains(&makepad_work_core::hex(&b)));
    assert_eq!(touched_by(me.event(&event).unwrap(), |id| me.event(id)), [mover]);
    assert_eq!(count(&me), n + 1);
    let mut above = original.clone();
    above.remove(1);
    above.push(mover);
    assert_eq!(stack(&me), above, "one step crosses the entire equal-key run");
    let refused = me.act(Action::Restack { items: vec![mover], to: RestackTo::Front }, Origin::Ui).unwrap_err();
    assert!(refused.contains("already as far forward"));
    me.act(Action::Restack { items: vec![mover], to: RestackTo::Backward }, Origin::Ui).unwrap();
    assert_eq!(stack(&me), original, "one backward step crosses both tied neighbours");
    me.act(Action::Restack { items: vec![mover], to: RestackTo::Front }, Origin::Ui).unwrap();
    me.act(Action::Restack { items: vec![mover], to: RestackTo::Back }, Origin::Ui).unwrap();
    assert_eq!(stack(&me), original, "back crosses the tied valid keys and stops just above the keyless item");

    let view = BoardView::build(&me.state, board);
    let mut g = Gesture::default();
    g.selection.ids = [mover, a, b].into();
    let n = count(&me);
    let event = me.act(g.align(&view, AlignTo::Right).unwrap(), Origin::Ui).unwrap();
    assert_eq!(count(&me), n + 1, "align signs a single event");
    let ids = touched_by(me.event(&event).unwrap(), |id| me.event(id));
    assert_eq!(ids.into_iter().collect::<std::collections::BTreeSet<_>>(), [mover, a].into());
    let view = BoardView::build(&me.state, board);
    for id in [mover, a, b] { assert_eq!(view.get(&id).unwrap().x, 768); }
    assert!(g.align(&view, AlignTo::Right).is_none(), "an already aligned selection writes nothing");

    exchange(&mut me, &mut peer);
    let first = me.act(Action::Restack { items: vec![mover, a], to: RestackTo::Front }, Origin::Ui).unwrap();
    peer.act(Action::Restack { items: vec![a], to: RestackTo::Back }, Origin::Tool).unwrap();
    let order = stack(&me);
    assert!(order.iter().position(|i| *i == mover) < order.iter().position(|i| *i == a));
    assert!(touched_by(me.event(&first).unwrap(), |i| me.event(i)).iter().all(|id| *id == mover || *id == a));
    exchange(&mut me, &mut peer);
    assert_eq!(stack(&me), stack(&peer), "independent restacks converge after real sync");
    assert_eq!(me.state.entities[&floor].z_key(), None);
    assert_eq!(me.state.entities[&b].z_key().unwrap().0, &[128]);
    drop((peer, me));
}

#[test]
fn restack_preserves_subsequences_and_crosses_only_reachable_runs() {
    use crate::model::{Action, ItemStyle, Model, Origin, RestackTo};
    use std::collections::BTreeSet;

    let dir = std::env::temp_dir().join(format!("restack-grid-{}", std::process::id()));
    let (mut m, _) = Model::open(&dir).unwrap();
    let board = m.act(Action::AddBoard { project: m.project().id, title: "Grid".into() }, Origin::Ui).unwrap();
    let ids: Vec<_> = (0..8).map(|_| m.act(Action::AddItem {
        board, kind: Key::Number(kind::STICKY), parent: None, x: 0, y: 0, w: 12 * 64, h: 12 * 64,
        style: ItemStyle::default(),
    }, Origin::Ui).unwrap()).collect();
    let log = m.home.room_log_path(&m.open_id());
    let count = || makepad_work_core::log::Log::open(&log).unwrap().0.events().len();
    let mut seed = 17u32;
    let mut random = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 16
    };
    let mut expected_events = count();
    for trial in 0..12 {
        let mut selected = BTreeSet::new();
        let mut setup = Vec::new();
        for &id in &ids {
            for axis in [field::X, field::Y] {
                setup.push(change(id, axis, Value::Int(i64::from(random() % 3) * 8 * 64)));
            }
            let key = match random() % 4 {
                0 => Value::Null,
                n => Value::Key(vec![n as u8 * 64]),
            };
            setup.push(change(id, field::Z, key));
            if trial == 0 || random() % 2 == 0 { selected.insert(id); }
        }
        m.act(Action::SetFields { changes: setup, label: None }, Origin::Tool).unwrap();
        expected_events += 1;
        for to in [RestackTo::Forward, RestackTo::Backward, RestackTo::Front, RestackTo::Back] {
            let forward = matches!(to, RestackTo::Forward | RestackTo::Front);
            let step = matches!(to, RestackTo::Forward | RestackTo::Backward);
            let view = BoardView::build(&m.state, board);
            let order = |v: &BoardView| {
                let mut ids = v.visible(all());
                if !forward { ids.reverse(); }
                ids
            };
            let before = order(&view);
            let bytes = std::fs::metadata(&log).unwrap().len();
            let result = m.act(Action::Restack { items: selected.iter().copied().collect(), to }, Origin::Ui);
            let after = order(&BoardView::build(&m.state, board));
            let subsequence = |ids: &[Id], moving| ids.iter().copied()
                .filter(|id| selected.contains(id) == moving).collect::<Vec<_>>();
            for moving in [true, false] {
                assert_eq!(subsequence(&before, moving), subsequence(&after, moving), "{to:?} preserves internal order");
            }
            let rank = |id: &Id| after.iter().position(|other| other == id).unwrap();
            let mut possible = false;
            let mut crossed = false;
            for (i, id) in before.iter().enumerate().filter(|(_, id)| selected.contains(*id)) {
                assert!(rank(id) >= i, "{to:?} never sends a selected item the other way");
                let item = view.get(id).unwrap();
                let mut first = None;
                for other in before[i + 1..].iter().filter(|id| !selected.contains(*id)) {
                    let anchor = view.get(other).unwrap();
                    let passed = rank(other) < rank(id);
                    if !forward && anchor.z_key.is_none() {
                        assert!(!passed, "a backward move stops at the keyless floor");
                        continue;
                    }
                    if !item.rect.overlaps(anchor.rect) { continue }
                    possible = true;
                    let first_key = first.get_or_insert(&anchor.z_key);
                    if passed {
                        crossed = true;
                        assert!(!step || *first_key == &anchor.z_key, "{to:?} crossed a second overlapping run");
                    } else {
                        assert!(step, "{to:?} left an overlapping item ahead");
                    }
                }
            }
            assert_eq!(crossed, possible, "{to:?} makes progress when there is an overlapping run");
            assert_eq!(before != after, possible, "{to:?} changes order exactly when a crossing is possible");
            if possible {
                let id = result.unwrap();
                let e = m.event(&id).unwrap();
                let Some(Content::Set { changes, .. }) = Content::decode(e.header.kind, e.header.kind_version, e.content().unwrap())
                    else { panic!("restack must sign one field edit") };
                assert!(!changes.is_empty());
                assert!(changes.iter().all(|c| selected.contains(&c.entity) && c.field == field::Z),
                    "only selected z keys are written");
                m.act(Action::Undo { of: id }, Origin::Ui).unwrap();
                expected_events += 2;
            } else {
                let direction = if forward { "forward" } else { "back" };
                assert_eq!(result.unwrap_err(), format!("the selection is already as far {direction} as it can go"));
                assert_eq!(std::fs::metadata(&log).unwrap().len(), bytes, "a refusal appends nothing");
            }
        }
    }
    assert_eq!(count(), expected_events, "one log event per successful command and per undo");
    drop(m);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn first_placement_undo_and_redo_cross_the_shared_space_as_one_step() {
    use super::gesture::{BoardHistory, NewItem};
    use crate::model::{tests::{exchange, shared_space}, Action, Model, Origin};
    use makepad_work_core::fold::project_entities;

    let (_homes, mut me, mut peer) = shared_space("first-board-placement");
    let project = me.project().id;
    let boards = |model: &Model| project_entities(&model.state, Some(project), Some(kind::BOARD))
        .into_iter().map(|e| e.id).collect::<Vec<_>>();
    assert!(boards(&me).is_empty() && boards(&peer).is_empty());
    let sticky = NewItem { kind: kind::STICKY, x: 64, y: 128, w: 220 * 64, h: 160 * 64, parent: None };
    let apply = |model: &mut Model, action| Some(model.act(action, Origin::Ui).unwrap());
    let (board, step) = sticky.place(project, None, |action| apply(&mut me, action)).unwrap();
    let [created, first] = step[..] else { panic!("first placement signs a board and its item") };
    assert_eq!(created, board);
    let mut history = BoardHistory::default();
    history.record_step(step);
    let check = |model: &Model, visible, items: &[Id]| {
        assert_eq!(boards(model), if visible { vec![board] } else { vec![] });
        let view = BoardView::build(&model.state, board);
        assert_eq!(view.visible(all()).len(), items.len());
        assert!(items.iter().all(|id| view.get(id).is_some()));
        assert_eq!(model.state.entities[&first].flag(field::DELETED), !visible);
    };
    exchange(&mut me, &mut peer);
    for model in [&me, &peer] { check(model, true, &[first]); }
    let mut undos = Vec::new();
    history.step(false, |action| {
        let of = [first, board][undos.len()];
        assert_eq!(action, Action::Undo { of }, "undo takes the item before its board");
        let id = apply(&mut me, action)?;
        undos.push(id);
        Some(id)
    });
    exchange(&mut me, &mut peer);
    for model in [&me, &peer] { check(model, false, &[]); }
    history.step(true, |action| {
        assert_eq!(action, Action::Undo { of: undos.pop().unwrap() }, "redo restores the board before the item");
        apply(&mut me, action)
    });
    assert!(undos.is_empty());
    exchange(&mut me, &mut peer);
    for model in [&me, &peer] { check(model, true, &[first]); }
    let (same, step) = sticky.place(project, Some(board), |action| apply(&mut me, action)).unwrap();
    let [second] = step[..] else { panic!("later placement signs just the item") };
    assert_eq!(same, board);
    history.record_step(step);
    exchange(&mut me, &mut peer);
    for model in [&me, &peer] { check(model, true, &[first, second]); }
    history.step(false, |action| {
        assert_eq!(action, Action::Undo { of: second });
        apply(&mut me, action)
    });
    exchange(&mut me, &mut peer);
    for model in [&me, &peer] { check(model, true, &[first]); }
}

#[test]
fn picker_tracks_live_boards_and_keeps_the_choice_per_project() {
    use super::gesture::{BoardHistory, NewItem};
    use crate::model::{tests::{exchange, shared_space}, Action, Model, Origin};

    let (_homes, mut m, mut peer) = shared_space("board-picker");
    let project = m.project().id;
    let mut picker = BoardPicker::default();
    assert_eq!(picker.refresh(&m.state, Some(project)), None);
    let sticky = NewItem { kind: kind::STICKY, x: 0, y: 0, w: 640, h: 640, parent: None };
    let (first, step) = sticky.place(project, None, |action| m.act(action, Origin::Ui).ok()).unwrap();
    let first_item = *step.last().unwrap();
    assert_eq!(picker.refresh(&m.state, Some(project)), Some(0));
    assert_eq!(picker.labels, ["Board"], "the first item's implicit board is a named tab");
    let second = m.act(Action::AddBoard { project, title: picker.next_title() }, Origin::Ui).unwrap();
    picker.choose(project, second);
    let (_, step) = sticky.place(project, Some(second), |action| m.act(action, Origin::Ui).ok()).unwrap();
    let [second_item] = step[..] else { panic!("placement on an existing board is one event") };
    let visible = |picker: &mut BoardPicker, model: &Model| {
        let index = picker.refresh(&model.state, Some(project)).unwrap();
        BoardView::build(&model.state, picker.boards[index]).visible(all())
    };
    assert_eq!(visible(&mut picker, &m), [second_item]);
    assert_eq!(picker.labels, ["Board", "Board 2"]);
    check_tab_clicks(&picker);
    picker.choose(project, first);
    assert_eq!(visible(&mut picker, &m), [first_item], "switching restores just that board's items");

    let mut history = BoardHistory::default();
    history.record(m.act(picker.rename(first, "Plans").unwrap(), Origin::Ui).unwrap());
    exchange(&mut m, &mut peer);
    for home in [&m, &peer] {
        assert_eq!(visible(&mut picker, home), [first_item]);
        assert_eq!(picker.boards, [first, second], "a rename keeps both tabs in place");
        assert_eq!(picker.labels, ["Plans", "Board 2"]);
        assert!(picker.rename(first, " Plans ").is_none() && picker.rename(first, " ").is_none());
    }
    history.step(false, |action| m.act(action, Origin::Ui).ok());
    exchange(&mut m, &mut peer);
    for home in [&m, &peer] {
        assert_eq!(visible(&mut picker, home), [first_item]);
        assert_eq!(picker.labels, ["Board", "Board 2"], "one undo restores the name on both homes");
    }
    let other = m.act(Action::AddProject { title: "Other project".into() }, Origin::Ui).unwrap();
    assert_eq!(picker.refresh(&m.state, Some(other)), None);
    let other_board = m.act(Action::AddBoard { project: other, title: picker.next_title() }, Origin::Ui).unwrap();
    assert_eq!(picker.refresh(&m.state, Some(other)), Some(0));
    assert_eq!(picker.boards, [other_board]);
    assert_eq!(picker.refresh(&m.state, Some(project)), Some(0));
    let undo = m.act(Action::Undo { of: first }, Origin::Ui).unwrap();
    assert_eq!(picker.refresh(&m.state, Some(project)), Some(0));
    assert_eq!(picker.boards, [second]);
    picker.after_undo(project, first);
    assert_eq!(picker.history_board(project, Some(second), true), Some(first), "redo restores the board that undo hid");
    m.act(Action::Undo { of: undo }, Origin::Ui).unwrap();
    picker.choose(project, first);
    let index = picker.refresh(&m.state, Some(project)).unwrap();
    assert_eq!(picker.boards[index], first);
}

fn check_tab_clicks(picker: &BoardPicker) {
    use super::tabs::{BoardTabs, TabAction};
    use makepad_widgets::{cx_draw::CxDraw, makepad_platform::studio::{RemoteMouseDown, RemoteMouseUp, StudioToApp}, *};
    use std::{cell::Cell, rc::Rc};

    let chosen = Rc::new(Cell::new(None));
    let selected = chosen.clone();
    let mut page = WidgetRef::empty();
    let root = page.clone();
    let mut cx = Cx::new(Box::new(move |cx, event| {
        let tabs = root.widget(cx, ids!(board_tabs));
        let actions = cx.capture_actions(|cx| tabs.handle_event(cx, event, &mut Scope::empty()));
        if let Some(mut strip) = tabs.borrow_mut::<BoardTabs>() {
            for command in strip.actions(cx, &actions) {
                if let TabAction::Choose(board) = command { selected.set(Some(board)); }
            }
        };
    }));
    cx.init_cx_os();
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
        let template = script_eval!(vm, {mod.widgets.WhiteboardPage{}});
        page.script_apply(vm, &Apply::New, &mut Scope::empty(), template);
    });
    cx.widget_tree().set_root_widget(page.clone());
    let tabs = page.widget(&cx, ids!(board_tabs));
    let list = tabs.portal_list(&cx, ids!(list));
    let pass = DrawPass::new(&mut cx);
    let size = dvec2(500., 40.);
    pass.set_size(&mut cx, size);
    let mut draw_list = DrawList2d::new(&mut cx);
    let mut draw = |cx: &mut Cx, current, writable| {
        let mut strip = tabs.borrow_mut::<BoardTabs>().unwrap();
        strip.show(cx, picker, Some(current));
        strip.edit_context(cx, writable);
        let event = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &event);
        let mut cx = Cx2d::new(&mut draw);
        cx.begin_pass(&pass, None);
        draw_list.begin_always(&mut cx);
        cx.begin_root_turtle(size, Layout::flow_down());
        let _ = strip.draw_walk(&mut cx, &mut Scope::empty(), Walk::fixed(size.x, size.y));
        cx.end_pass_sized_turtle();
        draw_list.end(&mut cx);
        cx.end_pass(&pass);
    };
    let face = |cx: &Cx, i| list.get_item(i).unwrap().1.button(cx, ids!(choose)).area().rect(cx);
    let click = |cx: &mut Cx, x, time| {
        let win = WindowId(1, 1);
        let button_raw_bits = MouseButton::PRIMARY.bits();
        cx.dispatch_studio_msg(StudioToApp::MouseDown(RemoteMouseDown {
            x, y: 20., time, button_raw_bits, modifiers: Default::default(),
        }), win, DVec2::default());
        cx.dispatch_studio_msg(StudioToApp::MouseUp(RemoteMouseUp {
            x, y: 20., time: time + 0.02, button_raw_bits, modifiers: Default::default(),
        }), win, DVec2::default());
    };

    draw(&mut cx, 1, true);
    let first = face(&cx, 0);
    let second = face(&cx, 1);
    let x = second.pos.x + 0.2;
    click(&mut cx, first.pos.x + first.size.x - 0.2, 1.);
    assert_eq!(chosen.take(), Some(picker.boards[0]), "one click chooses the inactive board");
    draw(&mut cx, 0, true);
    assert_eq!(face(&cx, 1), second, "switching keeps each tab under the pointer");
    click(&mut cx, x, 1.1);
    assert_eq!(chosen.take(), Some(picker.boards[1]), "nearby clicks on different tabs only switch");
    draw(&mut cx, 1, true);
    click(&mut cx, x, 1.2);
    assert_eq!(list.get_item(1).unwrap().0, id!(Editing), "the second click opens the same board's field");

    draw(&mut cx, 1, false);
    click(&mut cx, x, 2.);
    click(&mut cx, x, 2.1);
    assert_eq!(list.get_item(1).unwrap().0, id!(Current), "a read-only board can only be selected");
}
