use super::*;
use crate::journal::{self, Plate};
use makepad_ai_services::{engine::RegistryUp, ServiceRegistry, wire::{CallOrigin, ServiceCall, ServiceDown, ToolOutcome}};
use makepad_app_shell::chat::{ChatCaller, ChatHost};
use makepad_strict_json::{obj, s};
use std::sync::mpsc::channel;

fn on_worker<T: Send + 'static>(host: &ModelHost, run: impl FnOnce(&mut HostState) -> T + Send + 'static) -> T {
    let (send, recv) = channel();
    host.strand.submit(Class::Interactive, Ticket::none(), move |state| { send.send(run(state)).unwrap(); }).detach();
    recv.recv().unwrap()
}

#[test]
fn mechanical_calls_rebuild_the_saved_revision_even_after_cancellation() {
    let _owner = crate::cx_owned();
    let root = journal::test_dir();
    let opened = root.join("fixture.fab.splash");
    std::fs::write(&opened, "// the original manifest\nuse mod.fab.*\n").unwrap();
    let (send, answers) = channel();
    let mut cx = Cx::new(Box::new(move |_, event| {
        if let Event::Actions(actions) = event {
            for action in actions {
                if let Some(HostAction::Done(done)) = action.downcast_ref::<HostAction>() {
                    if let Some(person) = &done.person { person.receive(done.ok, &done.text); }
                    if done.id != 0 { send.send((done.id, done.ok, done.text.clone())).unwrap(); }
                }
            }
        }
    }));
    let mut host = ModelHost::start(&cx, false);
    let store = fab_ai::plates::PlateStore::under(&root.join("settings"), &root.join("documents"));
    let (plate, dir, mut head) = on_worker(&host, move |state| {
        // The fixture has no plate-browser thumbnails to build.
        state.paint_seen.extend(crate::demo_part::listed().iter().map(|demo| format!("demo:{}", demo.key)));
        let session = state.session.as_mut().unwrap();
        session.store = Some(store.clone());
        state.journal.install(session, None);
        session.open(&opened).unwrap();
        (Plate::opened(session).unwrap(), store.dir(session.plate.as_ref().unwrap()).join("journal"), session.model.head())
    });
    host.plate_journal = Some(plate.clone());
    let mut app = cx.with_vm(crate::App::script_new);
    app.host = Some(host);
    app.status.journal = Some(plate.clone());
    // An in-process service port exercises the shell parser, before Fab dispatch.
    app.chat.handle_event(&Event::Draw(Default::default()));
    app.chat.handle_event(&Event::Signal);
    makepad_app_shell::chat::pump(&mut app, &mut cx);
    let registry = ServiceRegistry::process();
    let endpoint = registry.pick_instance("fab", None).unwrap();
    let raw = "{unfinished";
    let why = format!("the arguments do not parse: {}", makepad_strict_json::parse(raw.as_bytes()).unwrap_err());
    let frame = ServiceDown::Call(ServiceCall { call_id: "wire".into(), tool: "apply".into(),
        args: raw.into(), caller: None, origin: CallOrigin::Remote });
    frame.validate().expect("the envelope is valid, only the arguments are malformed");
    assert!(registry.send(&endpoint, frame));
    makepad_app_shell::chat::pump(&mut app, &mut cx);
    assert!(registry.pump().iter().any(|up| matches!(up, RegistryUp::Result(_, reply)
        if reply.call_id == "wire" && reply.outcome == ToolOutcome::Refused && !reply.text.contains(raw))),
        "the shell must refuse the call without echoing the malformed arguments");
    registry.unregister(&endpoint);
    let mut saved = Vec::new();
    for n in 1..=4 {
        let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet block = m.box({{size: [{}mm 4mm 4mm]}})\nobject(block)\n", n + 8);
        let args = obj(vec![("request_id", s(format!("edit-{n}"))), ("expected", s(head.text())), ("script", s("block.cad.splash")), ("source", s(source))]);
        let id = if n == 3 {
            app.host.as_mut().unwrap().call_as_person("script", args)
        } else if n == 4 {
            app.host.as_mut().unwrap().history(true)
        } else {
            let caller = if n == 1 { ChatCaller::Turn(7) } else { ChatCaller::Outside };
            // Hold the worker until cancellation has actually occurred.
            let (release, gate) = channel();
            app.host.as_ref().unwrap().strand.submit(Class::Interactive, Ticket::none(), move |_| { gate.recv().unwrap(); }).detach();
            let id = app.chat_turn_call(&mut cx, "fab.script", args, caller);
            if n == 2 { app.chat_cancel(&mut cx, id); }
            release.send(()).unwrap();
            id
        };
        let dest = root.join(format!("saved-{n}"));
        saved.push(dest.clone());
        head = on_worker(app.host.as_ref().unwrap(), move |state| {
            let model = &state.session.as_ref().unwrap().model;
            std::fs::create_dir(&dest).unwrap();
            fab_model::project::save(model, &dest.join("fixture.fab.splash")).unwrap();
            model.head()
        });
        cx.handle_action_receiver();
        let (answered, ok, text) = answers.try_recv().expect("the worker returned its answer");
        assert_eq!(answered, id);
        assert!(ok, "the requested mechanical edit or history move must succeed");
        if n <= 2 {
            app.calls.finish(id, crate::chat::Answer { ok, text, images: vec![] });
            assert_eq!(app.chat_answer("wire", id).is_some(), n == 1, "a cancelled call has no chat reply");
        }
    }
    while plate.pending() != 0 { std::thread::yield_now(); }
    let mut rows = journal::read_rows(&dir).unwrap();
    rows.sort_by_key(|r| r.get("order").unwrap().as_u64());
    let field = |row: &Value, key| row.get(key).and_then(Value::as_u64);
    let kind = |r: &Value, k| r.get("kind").and_then(Value::as_str) == Some(k);
    let calls: Vec<_> = rows.iter().filter(|r| kind(r, "call")).collect();
    assert_eq!(calls.len(), 4, "one call per tool request, including the shell refusal");
    let references: std::collections::BTreeSet<_> = calls.iter().filter_map(|r| r.get("ref")?.as_str()).collect();
    assert_eq!(references.len(), calls.len(), "the shell refusal and each dispatched call get a distinct reference");
    assert_eq!(rows.iter().filter(|r| kind(r, "done")).count(), calls.len());
    for (call, (who, outcome)) in calls.iter().zip([("agent","refused"),("ai","ok"),("agent","cancelled"),("person","ok")]) {
        assert_eq!(call.get("actor").unwrap().get("kind").and_then(Value::as_str), Some(who));
        let id = field(call,"id").unwrap();
        let done: Vec<_> = rows.iter().filter(|r| kind(r,"done") && field(r,"id") == Some(id)).collect();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].get("outcome").and_then(Value::as_str), Some(outcome));
        let effects: Vec<_> = rows.iter().filter(|r| kind(r,"effect") && field(r,"call") == Some(id)).collect();
        assert_eq!(effects.len(), usize::from(outcome != "refused"), "a cancelled caller's committed job keeps its effect");
        let seqs: Vec<_> = effects.iter().map(|r| {
            assert_eq!(r.get("actor"), call.get("actor"));
            r.get("seq").unwrap().clone()
        }).collect();
        assert_eq!(done[0].get("seqs").unwrap().as_arr().unwrap(), seqs);
        if outcome == "refused" {
            assert_eq!(call.get("args").and_then(Value::as_str), Some(raw));
            assert_eq!(done[0].get("reply").and_then(Value::as_str), Some(why.as_str()));
        }
    }
    let revisions: Vec<_> = rows.iter().filter(|r| kind(r,"effect") || kind(r,"history")).collect();
    assert_eq!(revisions.len(), saved.len());
    for (n, (record, expected)) in revisions.iter().zip(saved).enumerate() {
        let copy = root.join(format!("copy-{n}"));
        journal::rebuild(&dir, field(record,"order"), &copy).unwrap();
        for name in ["fixture.fab.splash", "block.cad.splash"] {
            assert_eq!(std::fs::read(copy.join(name)).unwrap(), std::fs::read(expected.join(name)).unwrap(),
                "rebuild differs from the project writer at revision {n} for {name}");
        }
    }
    assert_eq!(std::fs::read(root.join("saved-4/block.cad.splash")).unwrap(), std::fs::read(root.join("saved-2/block.cad.splash")).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
