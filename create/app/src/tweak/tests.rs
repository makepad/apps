use super::*;
use super::wire::{Body, Packet};
use crate::app_link::PngStill;
use crate::model::{Action, Model, Origin};
use crate::tools::{app_tool, AppTools};
use makepad_strict_json as json;
use makepad_widgets::{test_cx, ActionsBuf, Cx, Event, Vec2d, WidgetRef};
use makepad_town_core::{hex, queue::Credential};
use std::{cell::RefCell, collections::{BTreeMap, VecDeque}, rc::Rc};

const SHARE: SessionId = [17; 16];
const ITEM: [u8; 32] = [24; 32];
const OWNER: Seat = [1; 65];
const ANA: Seat = [2; 65];
const ANA_AGENT: [u8; 32] = [42; 32];
const BEN: Seat = [3; 65];
const LINK: u64 = 42;
const TOWN: [u8; 32] = [12; 32];
const PATH: &str = "ui.panel";
const ROWS: &str = r#"{"props":[{"name":"padding","value":"8","set":true,"origin":"authored","level":0}],"cascade":[]}"#;
const TREE: &str = r#"{"window":0,"mode":"children","total_nodes":1,"total_matches":0,"truncated":false,"parents":[{"path":"/dock","offset":0,"total":1,"nodes":[{"path":"/dock/card","name":"card","ty":"View","children":0,"visible":true,"rect":[10,20,100,40]}]}]}"#;

const THEME: &str = r##"{"base":"dark","sheet":null,"has_mix":false,"has_pins":false,"edits":[],"sheets":[],"values":[{"name":"color_primary","value":"#e6a294ff","site":"dark.rs:12"},{"name":"color_bg_app","value":"#1e1e1eff","site":"dark.rs:14"},{"name":"space_2","value":"8","site":"dark.rs:9"},{"name":"space_factor","value":"10","site":"dark.rs:8"}]}"##;

fn install(label: &str) -> Request {
    Request::Theme(ThemeOp::Install(ThemeInstall { label: label.into(), base: "light".into(), sheet: None,
        globals: vec![("space_factor".into(), ThemeVal::Num(12.0))],
        overrides: vec![("color_primary".into(), ThemeVal::Color(0xff5c39ff))] }))
}

type Inbox = VecDeque<(Seat, u8, Vec<u8>)>;
#[derive(Default)]
struct Network { inbox: BTreeMap<Seat, Inbox> }

struct Endpoint { dropped: Vec<PeerUndelivered>, me: Seat, links: Vec<Seat>, net: Rc<RefCell<Network>> }
impl Relay for Endpoint {
    fn peers(&self) -> Vec<Seat> { self.links.clone() }
    fn send(&mut self, peer: Seat, topic: u8, body: Vec<u8>) -> Result<(), NotLinked> {
        if !self.links.contains(&peer) { return Err(NotLinked) }
        self.net.borrow_mut().inbox.entry(peer).or_default().push_back((self.me, topic, body));
        Ok(())
    }
    fn take_dropped(&mut self, topic: u8) -> Vec<PeerUndelivered> {
        let (ours, other) = self.dropped.drain(..).partition(|e| e.topic == topic);
        self.dropped = other;
        ours
    }
    fn take(&mut self, topic: u8) -> Vec<(Seat, Vec<u8>)> {
        let mut net = self.net.borrow_mut();
        let inbox = net.inbox.entry(self.me).or_default();
        let (ours, others): (Inbox, Inbox) = std::mem::take(inbox).into_iter().partition(|(_, t, _)| *t == topic);
        *inbox = others;
        ours.into_iter().map(|(peer, _, body)| (peer, body)).collect()
    }
}

#[derive(Default)]
struct FakeApp {
    stills: Vec<(u64, WindowId)>,
    sent: Vec<(u64, Option<WindowId>, DesignOp)>,
    events: Vec<(u64, u64, bool, String)>,
    padding: String,
    prop_values: BTreeMap<String, String>,
    theme: String,
    props: String,
    tree: String,
    trees: VecDeque<String>,
    /// Values the app answers with an error, as a real app refuses what is not a value of the type.
    refused_values: Vec<String>,
    /// Scripted pick answers, in order; the default pick follows once they run out.
    picks: VecDeque<Result<String, String>>,
}
impl AppLink<u64, u64> for FakeApp {
    fn png_still(&mut self, id: u64, window: WindowId) -> u64 {
        self.stills.push((id, window));
        self.stills.len() as u64
    }

    fn design(&mut self, id: u64, window: Option<WindowId>, op: DesignOp) -> u64 {
        let ticket = self.sent.len() as u64 + 1;
        let DesignOp::Design { verb, args } = &op else { panic!("unexpected G11 op") };
        let mut ok = true;
        let json = match verb {
            DesignVerb::Pick if !self.picks.is_empty() => self.picks.pop_front().unwrap().unwrap_or_else(|why| { ok = false; why }),
            DesignVerb::Pick => format!(r#"{{"window":{},"rect":[2,4,12,16],"path":"{PATH}","type":"View"}}"#, window.map_or(0, |w| w.0)),
            DesignVerb::Props if !self.prop_values.is_empty() => {
                let json::Value::Obj(mut fields) = parse_json(&self.props).expect("valid Props fixture") else { panic!("Props object") };
                for (key, array) in &mut fields {
                    if key != "props" { continue }
                    if let json::Value::Arr(rows) = array {
                        for row in rows {
                            let Some(value) = row.get("name").and_then(json::Value::as_str).and_then(|k| self.prop_values.get(k)) else { continue };
                            if let json::Value::Obj(fields) = row {
                                if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == "value") { *v = json::s(value); }
                            }
                        }
                    }
                }
                json::Value::Obj(fields).to_json()
            }
            DesignVerb::Props => self.props.clone(),
            DesignVerb::Tree => self.trees.pop_front().unwrap_or_else(|| self.tree.clone()),
            DesignVerb::Theme => self.theme_op(args).unwrap_or_else(|why| { ok = false; why }),
            DesignVerb::Typed => {
                let arg = |name| args.iter().find(|(k, _)| k == name).unwrap().1.as_str();
                let prop = arg("prop");
                let value = arg("value");
                let resolved = match value.trim().strip_prefix("theme.") {
                    Some(name) => self.token_value(name),
                    None => Some(value.to_owned()),
                };
                if let Some(new) = resolved.filter(|_| !self.refused_values.iter().any(|v| v == value)) {
                    let current = self.prop_values.get_mut(prop).unwrap_or(&mut self.padding);
                    let old = std::mem::replace(current, new.clone());
                    json::obj(vec![("old", json::obj(vec![(prop, json::s(old))])),
                        ("new", json::obj(vec![(prop, json::s(new))]))]).to_json()
                } else {
                    ok = false;
                    format!("not a data value: {value}")
                }
            }
            DesignVerb::Apply => panic!("Town never sends design apply"),
            _ => panic!("unexpected T1 verb"),
        };
        self.sent.push((id, window, op));
        self.events.push((id, ticket, ok, json));
        ticket
    }
}
impl FakeApp {
    fn token_value(&self, name: &str) -> Option<String> {
        parse_json(&self.theme).ok()?.get("values")?.as_arr()?.iter()
            .find(|t| t.get("name").and_then(json::Value::as_str) == Some(name))?.get("value")?.as_str().map(str::to_owned)
    }

    fn theme_op(&mut self, args: &[(String, String)]) -> Result<String, String> {
        let arg = |key| args.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        if arg("get") == Some("1") { return Ok(self.theme.clone()) }
        if arg("install") == Some("1") {
            let spec = parse_json(arg("spec").ok_or("missing install spec")?)?;
            let base = spec.get("base").and_then(json::Value::as_str).ok_or("missing base")?;
            if spec.get("sheet") != Some(&json::Value::Null) { return Err("unknown sheet".into()) }
            self.theme = THEME.replace(r#""base":"dark""#, &format!(r#""base":"{base}""#));
            return Ok(json::obj(vec![("ok", json::Value::Bool(true)), ("base", json::s(base)), ("sheet", json::Value::Null)]).to_json())
        }
        let name = arg("name").ok_or("missing token")?;
        let old = self.token_value(name).ok_or("unknown token")?;
        let value = arg("value").ok_or("missing value")?;
        let new = match value.strip_prefix("theme.") {
            Some(other) => self.token_value(other).ok_or("unknown alias")?,
            None if value.starts_with('#') || value.parse::<f64>().is_ok_and(f64::is_finite) => value.into(),
            _ => return Err("expected a literal".into()),
        };
        app::patch_cached_theme(&mut self.theme, &[(name.into(), new.clone())].into());
        Ok(json::obj(vec![("ok", json::Value::Bool(true)), ("name", json::s(name)),
            ("changed", json::Value::Bool(new != old)), ("global", json::Value::Bool(makepad_widgets::theme_tokens::is_global_key(name))),
            ("old", json::s(old)), ("new", json::s(new))]).to_json())
    }

    fn assert_typed_set(&self, since: usize, value: &str) {
        assert!(matches!(&self.sent[since..], [(LINK, None, DesignOp::Design { verb: DesignVerb::Typed, args })]
            if args == &[("path".into(), PATH.into()), ("prop".into(), "padding".into()), ("value".into(), value.into())]),
            "expected one typed Set with the original path, property and value");
    }

    fn answer(&mut self, s: &mut Session<u64, u64>) {
        for (id, ticket, ok, json) in self.events.drain(..) { s.design_reply(id, ticket, ok, &json); }
    }
}

struct TownApp { session: Session<u64, u64>, relay: Endpoint }
impl TownApp {
    fn poll(&mut self) {
        assert!(poll_relay(std::slice::from_mut(&mut self.session), &mut self.relay).is_empty(), "valid routed bodies decode");
    }
    fn send(&mut self, request: Request) -> RequestId { self.session.request(None, request).unwrap() }
}

struct Copies { host: TownApp, ana: TownApp, ben: TownApp, app: FakeApp, records: Vec<AppEdit> }
impl Copies {
    fn new(owner: Seat, ana: Seat, item: [u8; 32]) -> Self {
        Self::in_town(TOWN, owner, ana, item)
    }
    fn in_town(town: [u8; 32], owner: Seat, ana: Seat, item: [u8; 32]) -> Self {
        let net = Rc::<RefCell<Network>>::default();
        let town = |me, links: Vec<Seat>| TownApp {
            session: if me == owner { Session::owner(town, SHARE, item, me, LINK) } else { Session::peer(town, SHARE, item, me, owner) }.unwrap(),
            relay: Endpoint { me, links, dropped: Vec::new(), net: net.clone() },
        };
        let mut s = Self { host: town(owner, vec![ana, BEN]), ana: town(ana, vec![owner]), ben: town(BEN, vec![owner]),
            app: FakeApp { theme: THEME.into(), padding: "0".into(), props: ROWS.into(), tree: TREE.into(), ..Default::default() }, records: Vec::new() };
        s.host.session.set_members([owner, ana, BEN]);
        s.host.session.set_peer_level(ana, Some(THEME_LEVEL));
        s.host.session.set_peer_level(BEN, Some(THEME_LEVEL));
        s.ana.session.set_peer_level(owner, Some(THEME_LEVEL));
        s.ben.session.set_peer_level(owner, Some(THEME_LEVEL));
        s.exchange();
        s
    }
    fn exchange(&mut self) {
        self.ana.poll(); self.ben.poll(); self.host.poll(); self.ana.poll(); self.ben.poll();
    }
    fn allow(&mut self, seat: Seat, yes: bool) { self.host.session.consent(seat, yes).unwrap(); self.exchange(); }
    fn finish(&mut self) {
        self.app.answer(&mut self.host.session);
        while let Some(Action::RecordAppEdit { edit, .. }) = self.host.session.next_record() {
            self.records.push(edit);
            self.host.session.record_saved();
        }
        self.exchange();
    }
    fn one(&mut self) {
        self.exchange();
        self.host.session.drive_app(&mut self.app);
        self.finish();
    }
    /// The app lists the widget's properties, which a Set must name.
    fn list(&mut self) {
        self.host.send(Request::Props { path: PATH.into() });
        self.one();
    }
}

fn test_still() -> PngStill {
    use makepad_ai_services::pictures::{encode_png, RgbaPicture};
    let frame = RgbaPicture { width: 2, height: 1, rgba: vec![3, 5, 7, 255, 11, 13, 17, 255] };
    PngStill { png: encode_png(&frame).unwrap().into(), size: [2, 1] }
}

fn set(value: &str) -> Request { Request::Set { path: PATH.into(), prop: "padding".into(), value: value.into() } }
fn ana_agent_state(sharer: Seat) -> makepad_town_core::fold::State {
    use makepad_town_core::fold::{Agent, Profile, State};
    let mut state = State::default();
    for (seat, name) in [(sharer, "Walk A"), (ANA, "Ana")] {
        state.profiles.insert(seat, Profile { name: name.into(), colour: 0 });
    }
    state.agents.insert(ANA_AGENT, Agent { owner: ANA, name: "Stylist".into(), kind: String::new() });
    state
}
fn has(town: &TownApp, req: RequestId, test: impl Fn(&Notice) -> bool) -> bool {
    town.session.view.updates.iter().any(|(_, id, _, n)| *id == req && test(n))
}

#[test]
fn a_pick_on_an_unnamed_widget_lands_on_its_named_ancestor() {
    let pick = |path: &str, exact: Option<&str>, ty: &str| {
        let exact = exact.map_or(String::new(), |r| format!(r#","ref":"{r}""#));
        Ok(format!(r#"{{"window":0,"rect":[2,4,12,16],"path":"{path}"{exact},"type":"{ty}"}}"#))
    };
    let picked = |s: &Copies, req| s.ana.session.view.updates.iter().rev().find_map(|(_, id, _, n)| match n {
        Notice::Picked { path, ty, .. } if *id == req => Some((path.clone(), ty.clone())),
        _ => None,
    });
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    // Each case: the app's answers, then what ana's pick shows and what the owner asked the app.
    let cases = [
        ("an unnamed wrapper climbs to the named card, by its exact ref",
            vec![pick("-.w.body.card.-", Some("/card/View"), "View"), pick("-.w.body.card", Some("/card"), "RoundedView")],
            ("/card", "RoundedView"), Some("/card")),
        ("an app without refs climbs by the dotted path",
            vec![pick("-.w.body.card.-.-2", None, "View"), pick("-.w.body.card", None, "RoundedView")],
            ("-.w.body.card", "RoundedView"), Some("-.w.body.card")),
        ("a named ancestor that cannot be picked leaves the original pick",
            vec![pick("-.w.body.card.-", Some("/card/View"), "View"), Err("no widget at path".into())],
            ("/card/View", "View"), Some("/card")),
        ("nothing named above but the window: the pick stands",
            vec![pick("-.w.-", Some("/View"), "View")], ("/View", "View"), None),
        ("a named pick answers at once", vec![pick("-.w.body.card", Some("/card"), "RoundedView")], ("/card", "RoundedView"), None),
    ];
    for (why, answers, shown, climbed) in cases {
        s.app.picks = answers.into();
        let sent = s.app.sent.len();
        let req = s.ana.send(Request::Pick(At::Point { window: 0, x: 3., y: 5. }));
        s.one();
        s.one();
        assert_eq!(picked(&s, req), Some((shown.0.to_owned(), shown.1.to_owned())), "{why}");
        let paths: Vec<_> = s.app.sent[sent..].iter().filter_map(|(_, _, op)| match op {
            DesignOp::Design { args, .. } => args.iter().find(|(k, _)| k == "path").map(|(_, v)| v.as_str()),
            _ => None,
        }).collect();
        assert_eq!(paths, climbed.into_iter().collect::<Vec<_>>(), "{why}: the climb asks the app once, by that path");
        assert!(s.app.picks.is_empty(), "{why}: every scripted answer was asked for");
    }
}

#[test]
fn property_rows_leave_out_whole_object_dumps() {
    let rows = r#"{"props":[{"name":"draw_bg","value":"{rect_pos: vec2f(0 0) draw_clip: #00000000}","set":false,"origin":"runtime","level":null},
        {"name":"width","value":"Fill","set":true,"origin":"authored","level":0},
        {"name":"padding.left","value":"0","set":false,"origin":"runtime","level":null}],"cascade":[]}"#;
    let names: Vec<_> = props::Props::rebuild_rows(rows).unwrap().rows.into_iter().map(|p| p.prop).collect();
    assert_eq!(names, ["width", "padding.left"]);
}

#[test]
fn three_seats_share_order_presence_consent_and_retry() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.send(Request::AskEdit);
    s.ben.send(Request::AskEdit);
    let no_pick = s.ben.send(Request::Pick(At::Path(PATH.into())));
    let no_props = s.ben.send(Request::Props { path: PATH.into() });
    s.one();
    assert_eq!(s.host.session.view.asked, [ANA, BEN].into(), "the owner sees both people asking");
    assert!(s.app.sent.is_empty(), "reading the running app requires Allow");
    for req in [no_pick, no_props] {
        assert!(has(&s.ben, req, |n| matches!(n, Notice::Refused(v) if v.contains("Ask"))), "denied reads explain how to ask");
    }
    s.host.session.set_agents(&ana_agent_state(OWNER));
    let denied_ai = s.ana.session.request(Some(ANA_AGENT), Request::Props { path: PATH.into() }).unwrap();
    s.one();
    assert!(has(&s.ana, denied_ai, |n| matches!(n, Notice::Refused(why) if why.contains("allow"))),
        "a recognized AI needs its person's Allow to inspect the app");
    s.allow(ANA, true);
    for id in [[99; 32], ANA_AGENT] {
        let req = s.ben.session.request(Some(id), Request::Select(Some(PATH.into()))).unwrap();
        s.exchange();
        assert!(has(&s.ben, req, |n| matches!(n, Notice::Refused(why) if why.contains("one of your AIs"))),
            "even consent-free selection refuses an unknown AI or one owned by another seat");
        assert!(!s.host.session.view.selections.contains_key(&BEN), "a refused AI cannot change selection");
        assert!(!s.ben.session.view.agents.contains_key(&(BEN, id)), "a refused identity cannot introduce a chip");
    }
    s.ana.send(Request::Pick(At::Point { window: 2, x: 8., y: 12. }));
    s.one();
    s.list();
    s.ben.send(Request::Select(Some(PATH.into())));
    let edit = s.ana.session.request(Some(ANA_AGENT), set("8")).unwrap();
    let queued_ai = s.ana.session.request(Some(ANA_AGENT), set("9")).unwrap();
    let refused = s.ben.send(set("16"));
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    // Lose an in-flight request's answer from the peer's perspective and retransmit before completion.
    s.ana.relay.dropped.push(PeerUndelivered { peer: OWNER, topic: TWEAK, count: 1 });
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    assert_eq!(s.app.sent.len(), 3, "a duplicate in-flight request cannot reach the app again");
    s.allow(ANA, false);
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, queued_ai, |n| matches!(n, Notice::Refused(_))), "revocation answers the queued AI Set");
        assert_eq!(town.session.view.running.iter().collect::<Vec<_>>(), [(&(ANA, Some(ANA_AGENT)), &(edit, set("8")))],
            "only the dispatched AI Set is running, even after its queued sibling is refused");
    }
    s.finish();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(town.session.view.running.is_empty(), "Applied clears the matching AI Set on every seat");
        assert_eq!(town.session.view.selections[&ANA], town.session.view.selections[&BEN], "both seat outlines resolve to the same widget");
        assert!(has(town, edit, |n| matches!(n, Notice::Applied { seq: 1, new, .. } if new == "8")), "all three see applied sequence 1");
        assert!(has(town, refused, |n| matches!(n, Notice::Refused(_))), "the unallowed peer's set is refused on every seat");
    }
    assert_eq!(s.records[0].agent, Some(ANA_AGENT), "the allowed person's own AI can edit under that Allow");
    assert_eq!(s.app.sent[0].1, Some(WindowId(2)), "coordinate pick keeps the shared window");

    s.allow(ANA, true);
    let next = s.ana.send(set("10"));
    let cancelled = s.ana.send(set("12"));
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    s.allow(ANA, false);
    s.finish();
    let denied = s.ana.send(set("20"));
    s.one();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, next, |n| matches!(n, Notice::Applied { seq: 2, old, new, .. } if old == "8" && new == "10")),
            "the owner's local input preserves Allow; an already-sent operation completes in order");
        for req in [cancelled, denied] { assert!(has(town, req, |n| matches!(n, Notice::Refused(_))), "revocation refuses queued and later edits"); }
        assert!(!town.session.view.allowed.contains(&ANA), "revocation reaches each seat");
    }
    // Lose an observer's outbound notification as a stream drops. Only that stream replays;
    // the owner never applies the edit again, even if its author repeats the original request.
    let edit = s.host.send(set("14"));
    s.host.session.drive_app(&mut s.app);
    s.app.answer(&mut s.host.session);
    let Some(Action::RecordAppEdit { edit: record, .. }) = s.host.session.next_record() else { panic!("owner edit awaits storage") };
    s.records.push(record);
    s.host.session.record_saved();
    s.host.poll();
    let lost = s.ben.relay.take(TWEAK);
    assert!(!lost.is_empty(), "the fake loses an actual outbound notification");
    s.host.relay.dropped.push(PeerUndelivered { peer: BEN, topic: TWEAK, count: lost.len() });
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, edit, |n| matches!(n, Notice::Applied { seq: 3, .. })), "the observer catches up in owner order");
    }
    assert_eq!(s.records.len(), 3, "only successful edits are durable");
    assert_eq!(s.app.padding, "14", "the owner can still apply after revoking");
    s.allow(ANA, true);
    let last = s.ana.send(set("16"));
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    s.exchange();
    assert_eq!(s.ana.session.view.running.get(&(ANA, None)), Some(&(last, set("16"))), "the sent Set is visible before Leave");
    s.host.session.end("session ended");
    s.ana.session.end("session ended");
    assert!(s.host.session.view.running.is_empty() && s.ana.session.view.running.is_empty(),
        "ending the share clears activity even while a Set still awaits its app answer");
    assert!(s.host.session.has_unsettled_edits(), "Leave retains the app ticket for an accepted edit");
    s.finish();
    assert!(has(&s.ben, last, |n| matches!(n, Notice::Applied { seq: 4, .. })), "an edit applied just before Leave is recorded and observed");
    assert!(has(&s.ana, last, |n| matches!(n, Notice::Applied { seq: 4, .. })) &&
        !has(&s.ana, last, |n| matches!(n, Notice::Refused(_))), "local Leave waits for the authoritative result of the sent Set");
    s.host.session.link_ended(LINK, "app unlinked");
    s.exchange();
    assert!(s.host.session.view.allowed.is_empty() && s.ana.session.view.allowed.is_empty(), "ending the link clears consent");
}

#[test]
fn app_refused_sets_become_attributed_notes() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.list();
    let refused = ["fn() { return 4 }", "compute(4)", "Inset{left: calc(4)}", "4 + 8"];
    s.app.refused_values = refused.map(String::from).to_vec();
    for text in refused {
        let sent = s.app.sent.len();
        let req = s.ana.send(set(text));
        s.one();
        s.app.assert_typed_set(sent, text);
        let refusal = format!("The app did not apply this value: not a data value: {text}. Saved a note for the implementer.");
        assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(reason) if reason == &refusal)));
        let edit = s.records.last().unwrap();
        assert_eq!((&edit.path[..], &edit.prop[..], edit.seat, &edit.change), (PATH, "padding", ANA, &Change::Note(text.into())),
            "the implementer gets the original source and target");
    }
    let req = s.ana.send(set("Inset{left: 4 top: 8}"));
    s.one();
    assert!(has(&s.ana, req, |n| matches!(n, Notice::Applied { .. })), "a nested literal remains editable");

    // Exercise the count a second close logs, including a different share with a read in flight.
    let mut other = Session::owner(TOWN, [33; 16], [77; 32], OWNER, LINK).unwrap();
    other.request(None, Request::Props { path: PATH.into() }).unwrap();
    other.drive_app(&mut s.app);
    s.app.answer(&mut other);
    // A Set the app refused leaves a pending note, here while a read holds the app ticket.
    s.app.refused_values = vec!["fn() {}".into()];
    other.request(None, set("fn() {}")).unwrap();
    other.drive_app(&mut s.app);
    s.app.answer(&mut other);
    other.request(None, Request::Props { path: PATH.into() }).unwrap();
    other.drive_app(&mut s.app);
    s.host.send(set("12"));
    s.host.session.drive_app(&mut s.app);
    s.host.send(set("14"));
    let mut hub = Tweaks::default();
    hub.sessions.extend([s.host.session, other]);
    hub.quit();
    assert_eq!(hub.unrecorded_edits(), 2, "forced close counts the submitted Set and pending note across shares, excluding the read and revoked queued Set");
}

#[test]
fn stopping_one_ai_preserves_personal_consent_and_reaches_late_joiners() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    let mut state = ana_agent_state(OWNER);
    s.host.session.set_agents(&state);
    let ai = |town: &mut TownApp, request| town.session.request(Some(ANA_AGENT), request).unwrap();
    let select = || Request::Select(Some(PATH.into()));
    let actor = (ANA, ANA_AGENT);
    let no_select = ai(&mut s.ana, select());
    let no_ask = ai(&mut s.ana, Request::AskEdit);
    s.exchange();
    for req in [no_select, no_ask] {
        assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(why) if why.contains("allow"))),
            "even AI presence and AskEdit require the person's initial Allow");
    }
    assert_eq!(s.host.session.view.agents.get(&actor), Some(&false), "a verified first request introduces an idle AI");
    assert!(s.host.session.view.asked.is_empty(), "a denied AI AskEdit creates no consent prompt");
    s.host.session.bind_window(WindowId(3));
    let personal_look = s.ana.send(Request::Look);
    let own_look = ai(&mut s.host, Request::Look);
    let look = ai(&mut s.ana, Request::Look);
    let duplicate = ai(&mut s.ana, Request::Look);
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, personal_look, |n| matches!(n, Notice::Refused(why) if why.contains("guest AI"))),
            "a person cannot look through the AI tool");
        assert!(has(town, own_look, |n| matches!(n, Notice::Refused(why) if why == OWN_LOOK)),
            "the sharer's own AI is directed to its local grab tool");
        assert_eq!(town.session.view.looking.get(&actor), Some(&look), "a guest AI looks without Allow on every seat");
        assert!(town.session.view.running.is_empty(), "a look stays outside the app FIFO");
        assert!(has(town, duplicate, |n| matches!(n, Notice::Refused(_))), "an overlapping look is refused separately");
    }
    s.host.session.drive_app(&mut s.app);
    assert!(s.app.sent.is_empty(), "a look sends no design command to the app");
    s.allow(ANA, true);
    s.list();
    let running = ai(&mut s.ana, set("4"));
    let queued = ai(&mut s.ana, set("5"));
    let personal = s.ana.send(set("6"));
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert_eq!(town.session.view.looking.get(&actor), Some(&look), "the same AI can Set without replacing its look");
        assert_eq!(town.session.view.running.get(&(ANA, Some(ANA_AGENT))), Some(&(running, set("4"))), "the Set has its own running entry");
    }
    for (sender, agent, req) in [(BEN, Some(ANA_AGENT), look), (ANA, None, look), (ANA, Some(ANA_AGENT), duplicate)] {
        let packet = Packet { share: SHARE, sender, agent, req: 1, body: Body::Request(Request::LookDone { look: req, ok: true }) };
        if sender == BEN { &mut s.ben } else { &mut s.ana }.relay.send(OWNER, TWEAK, packet.encode()).unwrap();
    }
    s.exchange();
    assert_eq!(s.host.session.view.looking.get(&actor), Some(&look), "a mismatched seat, AI or request cannot finish a look");
    s.host.session.still_reply(LINK, 1, &Ok(test_still()));
    s.exchange();
    ai(&mut s.ana, Request::LookDone { look, ok: true });
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(!town.session.view.looking.contains_key(&actor) && has(town, look, |n| *n == Notice::Looked),
            "the matching LookDone finishes the original request everywhere");
        assert_eq!(town.session.view.running.get(&(ANA, Some(ANA_AGENT))), Some(&(running, set("4"))), "Looked leaves the Set in flight");
    }
    let failed = ai(&mut s.ana, Request::Look);
    s.exchange();
    ai(&mut s.ana, Request::LookDone { look: failed, ok: false });
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, failed, |n| matches!(n, Notice::Refused(why) if why == "The look did not finish on the guest's computer.")),
            "a failed capture settles the original look with a refusal on every seat");
        assert!(town.session.view.looking.is_empty() && !town.session.view.finished.contains_key(&actor),
            "a failed LookDone clears the look without leaving a past-tense action");
    }
    let interrupted = ai(&mut s.ana, Request::Look);
    s.exchange();
    assert!(s.ana.session.stop_agent(ANA, ANA_AGENT, true).is_err(), "a viewer cannot stop even its own AI on the share");
    s.host.session.stop_agent(ANA, ANA_AGENT, true).unwrap();
    let refused = ai(&mut s.ana, select());
    let asked = ai(&mut s.ana, Request::AskEdit);
    let stopped_look = ai(&mut s.ana, Request::Look);
    let person_select = s.ana.send(select());
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert_eq!(town.session.view.agents.get(&actor), Some(&true), "all three seats see the AI stopped");
        assert_eq!(town.session.view.running.get(&(ANA, Some(ANA_AGENT))), Some(&(running, set("4"))),
            "refusals for this AI's other requests preserve its in-flight Set");
        assert!(town.session.view.looking.is_empty(), "Stop clears the look on every seat");
        for req in [queued, refused, asked, interrupted, stopped_look] {
            assert!(has(town, req, |n| matches!(n, Notice::Refused(why) if why == "Walk A stopped Ana's AI.")),
                "the queued Set and subsequent AI presence requests give the explicit Stop reason");
        }
        assert!(has(town, person_select, |n| matches!(n, Notice::Selected { .. })), "the person's selection still works");
    }
    // A new observer gets a snapshot while the original Set is still in flight.
    s.host.session.set_members([OWNER, ANA]);
    s.ben.session = Session::peer(TOWN, SHARE, ITEM, BEN, OWNER).unwrap();
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.exchange();
    assert_eq!(s.ben.session.view.agents.get(&actor), Some(&true), "the snapshot restores the stopped AI");
    assert_eq!(s.ben.session.view.running.get(&(ANA, Some(ANA_AGENT))), Some(&(running, set("4"))),
        "the snapshot carries the real request id so its outcome can clear the status");
    s.finish();
    s.one();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(town.session.view.running.is_empty(), "the original Set clears on its own answer, including for the late joiner");
        assert!(has(town, personal, |n| matches!(n, Notice::Applied { new, .. } if new == "6")), "the person's queued edit survives Stop");
    }
    assert_eq!(s.records.len(), 2, "only the dispatched AI Set and the person's queued Set were recorded");
    let sibling = [43; 32];
    state.agents.insert(sibling, makepad_town_core::fold::Agent { owner: ANA, name: "Painter".into(), kind: String::new() });
    s.host.session.set_agents(&state);
    let other = s.ana.session.request(Some(sibling), set("7")).unwrap();
    s.one();
    assert!(has(&s.ben, other, |n| matches!(n, Notice::Applied { .. })), "stopping one AI preserves its sibling's access");
    assert_eq!(s.ben.session.view.agents.get(&actor), Some(&true), "the sibling's request leaves the original Stop in place");
    s.host.session.stop_agent(ANA, ANA_AGENT, false).unwrap();
    let resumed = ai(&mut s.ana, set("8"));
    s.one();
    for town in [&s.host, &s.ana, &s.ben] {
        assert_eq!(town.session.view.agents.get(&actor), Some(&false), "Allow clears this AI's stop on every seat");
        assert!(has(town, resumed, |n| matches!(n, Notice::Applied { new, .. } if new == "8")), "the AI can edit again under the person's grant");
    }
    s.allow(ANA, false);
    let revoked = ai(&mut s.ana, select());
    let disconnected = ai(&mut s.ana, Request::Look);
    s.exchange();
    assert!(has(&s.ana, revoked, |n| matches!(n, Notice::Refused(why) if why.contains("allow"))),
        "allowing the AI cannot override a later personal revocation");
    s.host.session.peer_unlinked(ANA);
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(town.session.view.looking.is_empty() && has(town, disconnected, |n| matches!(n, Notice::Refused(_))),
            "unlink refuses the departing seat's look even without an edit grant");
    }
}

#[test]
fn malformed_set_delta_queues_the_original_note_until_recording_settles() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.list();
    s.host.session.set_agents(&ana_agent_state(OWNER));
    let text = "  Size.Fixed(200)  ";
    let req = s.ana.session.request(Some(ANA_AGENT), set(text)).unwrap();
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    let [(_, _, true, json)] = s.app.events.as_mut_slice() else { panic!("expected one successful app reply") };
    *json = r#"{"old":{"padding":"0"}}"#.into();
    s.app.answer(&mut s.host.session);
    s.exchange();
    let Some(Action::RecordAppEdit { edit, .. }) = s.host.session.next_record() else { panic!("expected a note") };
    assert_eq!(edit, AppEdit { seat: ANA, agent: Some(ANA_AGENT), path: PATH.into(), prop: "padding".into(), change: Change::Note(text.into()) },
        "the note retains the requesting AI and its seat");
    assert_eq!(s.host.session.unrecorded_edits(), 1);
    let notices: Vec<_> = s.ana.session.view.updates.iter().filter(|(_, id, _, _)| *id == req).collect();
    assert!(matches!(&notices[..], [(_, _, _, Notice::Running(r))] if *r == set(text)),
        "the peer sees only the start while the note awaits recording");
    s.host.session.record_failed("town closed");
    s.exchange();
    assert_eq!(s.host.session.unrecorded_edits(), 0);
    assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(reason) if reason == "town closed")));
}

#[test]
fn values_reach_the_app_only_as_typed_edits() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    let unlisted = s.ana.send(set("8"));
    s.one();
    assert!(has(&s.ana, unlisted, |n| matches!(n, Notice::Refused(why) if why.contains("lists"))),
        "nothing is set before the app lists the widget's properties");
    assert!(s.app.sent.is_empty());
    s.list();
    let listed = s.app.sent.len();
    // The app never lists a function-valued prop or a named child, so neither can be named.
    for prop in ["on_render", "card", "padding.left"] {
        let req = s.ana.send(Request::Set { path: PATH.into(), prop: prop.into(), value: "4".into() });
        s.one();
        assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(why) if why.contains("lists"))), "{prop} is not listed");
    }
    assert_eq!(s.app.sent.len(), listed, "no unlisted property reaches the app");

    // Town reads no value: it sends each as the peer wrote it, and the app is the only judge.
    let deep = format!("{}0{}", "[".repeat(100_000), "]".repeat(100_000));
    let hostile = ["loop{}", "[loop{}]", "Inset{left: loop{}}", "try{}", "return", "{loop: 1}", "http_resource", "Fill", deep.as_str()];
    s.app.refused_values = hostile.map(String::from).to_vec();
    for text in hostile {
        let sent = s.app.sent.len();
        let req = s.ana.send(set(text));
        s.one();
        s.app.assert_typed_set(sent, text);
        assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(_))), "{text} is refused");
        assert_eq!(s.records.last().unwrap().change, Change::Note(text.into()), "{text} becomes a note");
    }

    let req = s.ana.send(set("8"));
    s.one();
    assert!(has(&s.ana, req, |n| matches!(n, Notice::Applied { .. })), "a value applies after the refused ones");
    assert!(s.app.sent.iter().all(|(_, _, op)| !matches!(op, DesignOp::Design { verb: DesignVerb::Apply, .. })));
}

#[test]
fn typed_values_apply_as_the_app_answers() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.list();
    for text in ["Size.Fixed(200)", "Size.Fill{weight: 100 basis: FitBound.Abs(0) shrink: 0}",
        "12", "-4", "#fff", "Inset{left: 4 top: 8}", "theme.space_2", "\"50%\"", "true", "nil", "[1, 2]", "@fast"] {
        let old = s.app.padding.clone();
        let sent = s.app.sent.len();
        let req = s.ana.send(set(text));
        s.one();
        s.app.assert_typed_set(sent, text);
        assert!(has(&s.ana, req, |n| matches!(n, Notice::Applied { old: before, new, .. } if before == &old && new == text)),
            "the peer receives the app's delta for {text}");
    }
}

fn list_apps(model: &mut Model, tools: &mut AppTools, sessions: &mut [Session<u64, u64>], waits: &mut crate::tools::AgentWaits,
    binding: &str, after: Option<&str>) -> makepad_ai_services::wire::ToolResult {
    let mut args = vec![("binding", json::s(binding))];
    if let Some(after) = after { args.push(("after", json::s(after))); }
    let call = app_tool("app_list", &json::obj(args).to_json()).unwrap();
    tools.start("list", call, model, sessions, waits).0.expect("app discovery must answer immediately")
}

fn check_app_list_pages(model: &mut Model, tools: &mut AppTools, waits: &mut crate::tools::AgentWaits, binding: &str, line: [u8; 32]) {
    use crate::ai_trace::Ink;
    use makepad_ai_services::wire::{ToolOutcome, MAX_RESULT_BYTES};
    use makepad_town_core::{entity::field, event::{Change as FieldChange, Value}};

    model.set_person_name("Ana").unwrap();
    let town = model.open_id();
    let me = model.me();
    let mut sessions = Vec::new();
    let mut add = |tag, title: &str| {
        let share = [tag; 16];
        let item = model.act(Action::AddApp { town, board: None, geometry: [0, 0, 6400, 6400], session: share, name: title.into() }, Origin::Ui).unwrap();
        sessions.push(Session::owner(town, share, item, me, LINK).unwrap());
        item
    };
    let titles = ["a".repeat(MAX_RESULT_BYTES / 2), "b".repeat(MAX_RESULT_BYTES / 2), "🦀\"\n".repeat(MAX_RESULT_BYTES / 4)];
    let mut expected = BTreeMap::new();
    for (index, title) in titles.into_iter().enumerate() {
        expected.insert(hex(&add(index as u8, &title)), title);
    }
    let archived = add(3, "Archived");
    let deleted = add(4, "Deleted");
    add(5, "Ended");
    sessions.last_mut().unwrap().end("the share ended");
    model.act(Action::SetFields { changes: vec![
        FieldChange { entity: archived, field: field::ARCHIVED, value: Value::Bool(true) },
        FieldChange { entity: deleted, field: field::DELETED, value: Value::Bool(true) }], label: None }, Origin::Ui).unwrap();

    let mut after = None::<String>;
    let mut fragments = String::new();
    let mut first_fragment = None;
    let mut cursors = std::collections::BTreeSet::new();
    let mut found = BTreeMap::new();
    loop {
        let out = list_apps(model, tools, &mut sessions, waits, binding, after.as_deref());
        assert_eq!(out.outcome, ToolOutcome::Ok, "each continuation delivers a page of the list");
        assert!(out.text.len() <= MAX_RESULT_BYTES, "the escaped page fits the router without truncation");
        let page = parse_json(&out.text).unwrap();
        let mut rows = page.get("apps").unwrap().as_arr().unwrap().to_vec();
        if let Some(part) = page.get("fragment").and_then(json::Value::as_str) {
            assert!(rows.is_empty() && !part.is_empty(), "a fragment page advances one oversized app row");
            assert_eq!(page.get("offset").and_then(json::Value::as_u64), Some(fragments.len() as u64), "fragment offsets count original UTF-8 bytes");
            fragments.push_str(part);
            if let Ok(row) = parse_json(&fragments) {
                assert_eq!(row.get("item"), page.get("id"), "a reconstructed fragment identifies the signed App item");
                rows.push(row);
                fragments.clear();
            } else if first_fragment.is_none() {
                first_fragment = page.get("next").and_then(json::Value::as_str).map(str::to_owned);
            }
        }
        for row in rows {
            let item = row.get("item").unwrap().as_str().unwrap();
            let title = row.get("title").unwrap().as_str().unwrap();
            assert_eq!(row, json::obj(vec![("item", json::s(item)), ("title", json::s(title)), ("sharer", json::s("Ana")),
                ("mine", json::Value::Bool(true)), ("may_edit", json::Value::Bool(true))]), "a sharer sees only its own listed metadata and grant");
            assert!(found.insert(item.to_owned(), title.to_owned()).is_none(), "a continuation never repeats a complete row");
        }
        after = page.get("next").unwrap().as_str().map(str::to_owned);
        let Some(next) = &after else { break };
        assert!(cursors.insert(next.clone()), "each list continuation makes progress");
    }
    assert!(fragments.is_empty() && first_fragment.is_some(), "the oversized escaped title was fragmented and reassembled");
    assert_eq!(found, expected,
        "every visible live App retains its full title, while archived, deleted and ended shares are absent");
    sessions[2].window(WindowId(7), [800., 600.]);
    let since = waits.traces.rows(&line)[0].output.len();
    let stale = list_apps(model, tools, &mut sessions, waits, binding, first_fragment.as_deref());
    assert_eq!(stale.outcome, ToolOutcome::Refused, "a changed live row cannot be spliced into an earlier fragment");
    let output = &waits.traces.rows(&line)[0].output[since..];
    assert!(matches!(output, [warning] if warning.ink == Ink::Warning && warning.text.contains(&stale.text)),
        "a refused continuation records one warning with its actual reason");
}

fn assert_trace(waits: &crate::tools::AgentWaits, line: [u8; 32], since: usize, expected: &[(crate::ai_trace::Ink, &str)], why: &str) {
    let output: Vec<_> = waits.traces.rows(&line)[0].output[since..].iter().map(|o| (o.ink, o.text.as_str())).collect();
    assert_eq!(output, expected, "{why}");
}

fn check_guest_looks(model: &mut Model, tools: &mut AppTools, waits: &mut crate::tools::AgentWaits, s: &mut Copies,
    binding: &str, line: [u8; 32], agent: [u8; 32], board: [u8; 32]) {
    use crate::{ai_trace::Ink, tools::AgentWaits};
    use makepad_ai_services::wire::ToolOutcome;
    use makepad_ai_services::wire::ToolResult;

    fn replies(tools: &mut AppTools, waits: &mut AgentWaits, town: &mut TownApp) -> Vec<ToolResult> {
        let mut results = Vec::new();
        for (seat, req, actor, notice) in std::mem::take(&mut town.session.view.updates) {
            if let Notice::Picture { png, size } = notice {
                assert_eq!(seat, town.session.me, "a guest receives only pictures addressed to its own seat");
                tools.picture(&mut town.session, req, actor.unwrap(), &png, size, waits, &mut |out| results.push(out));
            } else if let Some((out, _)) = tools.answer(&town.session, seat, req, actor, &notice, waits) {
                results.push(out);
            }
        }
        results
    }

    let town = model.open_id();
    let ana = model.me();
    let item = hex(&s.ana.session.item);
    let token = Credential::parse(binding).unwrap();
    let look = |item: &str| app_tool("app_look", &format!(r#"{{"binding":"{binding}","item":"{item}"}}"#)).unwrap();
    let own_share = [37; 16];
    let own_item = model.act(Action::AddApp { town, board: Some(board), geometry: [0, 0, 6400, 6400],
        session: own_share, name: "Local".into() }, Origin::Ui).unwrap();
    let mut own = Session::<u64, u64>::owner(town, own_share, own_item, ana, LINK).unwrap();
    let out = tools.start("local", look(&hex(&own_item)), model, std::slice::from_mut(&mut own), waits).0.unwrap();
    assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, OWN_LOOK), "an own share names its local tool even without a picture");
    assert!(own.view.updates.is_empty(), "the own-share refusal precedes any request");
    s.host.session.bind_window(WindowId(3));
    let start = |call, tools: &mut AppTools, model: &mut Model, waits: &mut AgentWaits, s: &mut Copies| {
        let before = s.app.stills.len();
        let sent = tools.start(call, look(&item), model, std::slice::from_mut(&mut s.ana.session), waits);
        assert_eq!(sent, (None, Some(token)), "a guest with no share on screen holds its look for the sharer");
        s.exchange();
        let req = s.ana.session.view.looking[&(ana, agent)];
        s.host.session.drive_app(&mut s.app);
        assert_eq!(&s.app.stills[before..], &[(LINK, WindowId(3))], "the grant requests one full-scale still from the shared app window");
        (req, s.app.stills.len() as u64)
    };

    let still = test_still();
    let (stopped, old_ticket) = start("stop-during-capture", tools, model, waits, s);
    s.host.session.stop_agent(ana, agent, true).unwrap();
    s.exchange();
    assert!(matches!(replies(tools, waits, &mut s.ana).as_slice(), [out] if out.call_id == "stop-during-capture"
        && out.outcome == ToolOutcome::Refused && out.images.is_empty()), "Stop settles the held call exactly once without an image");
    s.host.session.stop_agent(ana, agent, false).unwrap();
    s.exchange();

    let (revoked, queued_ticket) = start("stop-before-send", tools, model, waits, s);
    s.host.session.still_reply(LINK, queued_ticket, &Ok(still.clone()));
    s.host.session.stop_agent(ana, agent, true).unwrap();
    s.exchange();
    assert!(!has(&s.ana, revoked, |n| matches!(n, Notice::Picture { .. })), "Stop withdraws a finished PNG still queued in the private mailbox");
    assert!(matches!(replies(tools, waits, &mut s.ana).as_slice(), [out] if out.call_id == "stop-before-send"
        && out.outcome == ToolOutcome::Refused), "a revoked delivery settles the held call");
    s.host.session.stop_agent(ana, agent, false).unwrap();
    s.exchange();

    let since = waits.traces.rows(&line)[0].output.len();
    let (success, ticket) = start("picture", tools, model, waits, s);
    s.host.session.still_reply(LINK, old_ticket, &Ok(still.clone()));
    s.host.session.still_reply(LINK + 1, ticket, &Ok(still.clone()));
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        for req in [stopped, success] {
            assert!(!has(town, req, |n| matches!(n, Notice::Picture { .. })), "a stopped capture or wrong app link cannot release any picture");
        }
    }
    s.host.session.still_reply(LINK, ticket, &Ok(still.clone()));
    s.exchange();
    for town in [&s.host, &s.ben] {
        assert!(!has(town, success, |n| matches!(n, Notice::Picture { .. })), "the sharer and the third seat receive no PNG");
    }
    let results = replies(tools, waits, &mut s.ana);
    let [out] = results.as_slice() else { panic!("only the held look receives the PNG") };
    assert_eq!((&*out.call_id, out.outcome, &*out.text, &*out.data, &*out.note),
        ("picture", ToolOutcome::Ok, "Scene's share at full size, 2×1. The picture comes with this result.", "", ""),
        "the result contains one picture description and no chat or timeline");
    let [encoded] = out.images.as_slice() else { panic!("the tool must return exactly one image") };
    let bytes = makepad_ai_hub::makepad_base64::base64_decode(encoded.as_bytes()).expect("the attached image uses standard base64");
    assert_eq!(bytes.as_slice(), &*still.png, "the guest gets the exact PNG supplied by the sharer");
    let image = makepad_ai_services::pictures::decode_png(&bytes).unwrap();
    assert_eq!([image.width, image.height], still.size, "the attached PNG decodes at the reported size");
    assert_trace(waits, line, since, &[(Ink::Action, "looking at Scene's share"), (Ink::Action, "looked at Scene's share")],
        "only the delivered image leaves a successful trace");
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, success, |n| n == &Notice::Looked), "the receipt completes the original look across the private delivery gap");
    }

    let (failed, ticket) = start("failed-still", tools, model, waits, s);
    s.host.session.still_reply(LINK, ticket, &Err("no drawable".into()));
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert!(has(town, failed, |n| matches!(n, Notice::Refused(why) if why == "no drawable")),
            "every seat receives the app's capture failure for the original look");
        assert!(!town.session.view.looking.contains_key(&(ana, agent)) && !town.session.view.finished.contains_key(&(ana, agent)),
            "a failed capture clears every active chip without a finished action");
    }
    assert!(matches!(replies(tools, waits, &mut s.ana).as_slice(), [out] if out.call_id == "failed-still"
        && out.outcome == ToolOutcome::Refused && out.images.is_empty()), "the worker failure refuses the guest's held call");

    let mut wrong_signature = still.png.to_vec();
    wrong_signature[0] = 0;
    for invalid in [PngStill { png: wrong_signature.into(), size: still.size }, PngStill { png: still.png.clone(), size: [1, 2] }] {
        let (req, ticket) = start("invalid-png", tools, model, waits, s);
        s.host.session.still_reply(LINK, ticket, &Ok(invalid));
        s.exchange();
        assert!(matches!(replies(tools, waits, &mut s.ana).as_slice(), [out] if out.call_id == "invalid-png"
            && out.outcome == ToolOutcome::Refused && out.images.is_empty()), "a bad signature or mismatched IHDR refuses the tool call");
        s.exchange();
        assert!(has(&s.host, req, |n| matches!(n, Notice::Refused(why) if why == "The look did not finish on the guest's computer.")),
            "an invalid picture sends a failed LookDone for that request");
    }
}

#[cfg(unix)]
#[test]
fn granted_look_fetches_the_bound_window_through_frames() {
    use crate::app_link::RecordingRig;
    let mut rig = RecordingRig::new();
    rig.app.scan();
    rig.wait(|r| !r.app.apps().is_empty());
    let key = rig.app.apps()[0].key.clone();
    let link = rig.app.link(&key);
    rig.live(link);

    let net = Rc::<RefCell<Network>>::default();
    let endpoint = |me, peer| Endpoint { me, links: vec![peer], dropped: vec![], net: net.clone() };
    let mut host_relay = endpoint(OWNER, ANA);
    let mut guest = TownApp { session: Session::peer(TOWN, SHARE, ITEM, ANA, OWNER).unwrap(), relay: endpoint(ANA, OWNER) };
    let mut host = Tweaks::default();
    let mut session = Session::owner(TOWN, SHARE, ITEM, OWNER, link).unwrap();
    session.set_members([ANA]);
    session.set_agents(&ana_agent_state(OWNER));
    session.bind_window(WindowId(4));
    host.sessions.push(session);

    let look = guest.session.request(Some(ANA_AGENT), Request::Look).unwrap();
    guest.poll();
    host.relay(&mut host_relay);
    host.link_events(&[], &mut rig.app);
    rig.wait(|r| r.events.iter().any(|e| matches!(e, AppLinkEvent::PngStill { .. })));
    host.link_events(&std::mem::take(&mut rig.events), &mut rig.app);
    host.relay(&mut host_relay);
    guest.poll();
    let picture = guest.session.view.updates.iter().find_map(|(_, req, _, notice)| match notice {
        Notice::Picture { png, size } if *req == look => Some((png, size)), _ => None,
    }).expect("the guest receives the app-link worker's still for the granted look");
    let image = makepad_ai_services::pictures::decode_png(picture.0).unwrap();
    assert_eq!((*picture.1, [image.width, image.height]), ([16, 24], [16, 24]),
        "the frames service returns the bound window at native scale even with no guest display");
    assert_eq!(&image.rgba[..4], &[255; 4], "the still contains the bound window's pixels");
}

#[test]
fn ai_tools_use_the_authenticated_seat_and_have_no_allow() {
    use crate::{ai_trace::Ink, tools::AgentWaits};
    use makepad_ai_services::wire::ToolOutcome;
    use makepad_town_core::{entity::field, event::Value};

    let (_homes, mut owner, mut model) = crate::model::tests::shared_town("tweak-ai");
    let town = model.open_id();
    let private = model.towns().into_iter().find(|s| s.id != town).unwrap().id;
    owner.set_person_name("Walk A").unwrap();
    let ana = model.me();
    let code = Credential::parse(&model.agent_code(None).unwrap()).unwrap();
    let (binding, agent) = model.agent_join_with_id(code, true).unwrap();
    let project = owner.current_project().unwrap().id;
    let board = owner.act(Action::AddBoard { project, title: "Board".into() }, Origin::Ui).unwrap();
    let id = owner.act(Action::AddApp { town: owner.open_id(), board: Some(board), geometry: [0, 0, 6400, 6400], session: SHARE, name: "Scene".into() }, Origin::Ui).unwrap();
    crate::model::tests::exchange(&mut owner, &mut model);
    let mut s = Copies::in_town(town, owner.me(), ana, id);
    s.host.session.set_agents(&owner.state);
    let mut tools = AppTools::default();
    let mut waits = AgentWaits::default();
    let item = hex(&id);
    let manifest = crate::tools::manifest(true);
    assert!(manifest.tools.iter().any(|tool| tool.name == "app_list" && tool.risk == makepad_ai_services::wire::Risk::Read),
        "discovery is advertised as a read tool");
    assert!(manifest.tools.iter().any(|tool| tool.name == "app_look" && tool.risk == makepad_ai_services::wire::Risk::Read),
        "the share's picture is advertised as a read tool");
    for name in ["app_allow", "app_revoke", "app_stop", "app_stop_agent", "app_allow_agent"] {
        assert!(app_tool(name, "{}").is_none() && crate::tools::read(name, "{}").is_err(), "consent has no {name} tool");
        assert!(manifest.tools.iter().all(|tool| tool.name != name), "the manifest exposes no {name} consent tool");
    }
    let calls = [("app_list", ""), ("app_look", ""), ("app_pick", r#","path":"ui.panel""#), ("app_props", r#","path":"ui.panel""#),
        ("app_set", r#","path":"ui.panel","prop":"padding","value":"18""#), ("app_select", r#","path":null"#), ("app_edits", "")];
    let (_, unknown) = Credential::issue(model.open_id()).unwrap();
    for (name, fields) in calls {
        for (arg, why) in [(String::new(), "Invalid `binding`."), (format!(r#","binding":"{unknown}""#), "This binding has ended.")] {
            let parsed = if name == "app_list" { app_tool(name, &format!("{{{}}}", arg.trim_start_matches(','))) }
                else { app_tool(name, &format!(r#"{{"item":"{item}"{fields}{arg}}}"#)) }.unwrap();
            let out = tools.start(name, parsed, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits).0.unwrap();
            assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, why), "{name} must explain the missing or unknown binding");
        }
    }
    let token = Credential::parse(&binding).unwrap();
    let peer_line = owner.mention_agent(agent, "inspect the panel".into(), None).unwrap();
    crate::model::tests::exchange(&mut owner, &mut model);
    model.decide_agent_request(model.open_id(), agent, peer_line, true).unwrap();
    waits.offered(&model, peer_line);
    let cursor = |reply: &makepad_ai_services::wire::ToolResult| {
        let page = parse_json(&reply.text).unwrap();
        makepad_town_core::parse_id(page.get("last").unwrap().as_str().unwrap()).unwrap()
    };
    let mut after_peer = None;
    for picked_up in [false, true] {
        if picked_up {
            let reply = waits.wait(&mut model, "peer", token, None);
            after_peer = Some(cursor(&reply[0]));
        }
        assert_eq!(waits.traces.working(token), picked_up, "only picking up the approved peer line marks its trace working");
        for (name, fields) in calls {
            let parsed = if name == "app_list" { app_tool(name, &format!(r#"{{"binding":"{binding}"}}"#)) }
                else { app_tool(name, &format!(r#"{{"binding":"{binding}","item":"{item}"{fields}}}"#)) }.unwrap();
            let out = tools.start(name, parsed, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits).0.unwrap();
            assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, "Your person hasn't asked you anything."),
                "{name} requires an owner turn, including after an approved peer request is picked up");
        }
        assert!(waits.traces.rows(&peer_line)[0].output.iter().all(|o| o.ink != Ink::Action),
            "refused tools record no action on an idle or peer-authored trace");
    }
    let line = model.mention_agent(agent, "tweak the shared app".into(), None).unwrap();
    waits.offered(&model, line);
    let page = waits.wait(&mut model, "own", token, after_peer);
    assert_eq!(page[0].outcome, ToolOutcome::Ok, "the AI picked up its own person's request");
    let invalid = app_tool("app_set", &format!(r#"{{"binding":"{binding}","item":"{item}","path":"ui","prop":"p","value":"2","allow":true}}"#)).unwrap();
    let (out, changed) = tools.start("invalid", invalid, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits);
    assert_eq!((out.unwrap().outcome, changed), (ToolOutcome::Refused, Some(token)), "a tool cannot smuggle an allow flag");
    assert_trace(&waits, line, 0, &[(Ink::Warning, "app_set refused: unexpected argument `allow`")],
        "a pre-send refusal adds only its warning to the owner's line");

    check_guest_looks(&mut model, &mut tools, &mut waits, &mut s, &binding, line, agent, board);

    let set = || app_tool("app_set", &format!(r#"{{"binding":"{binding}","item":"{item}","path":"ui.panel","prop":"padding","value":"18"}}"#)).unwrap();
    let since = waits.traces.rows(&line)[0].output.len();
    let (out, changed) = tools.start("denied", set(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits);
    assert!(out.is_none() && changed == Some(token), "a sent Set awaits the sharer's decision");
    s.one();
    while let Some((seat, req, actor, notice)) = s.ana.session.view.updates.pop_front() {
        if let Some((result, changed)) = tools.answer(&s.ana.session, seat, req, actor, &notice, &mut waits) {
            assert_eq!((result.outcome, changed), (ToolOutcome::Refused, Some(token)), "the sharer's refusal refreshes the requesting AI's trace");
        }
    }
    assert_trace(&waits, line, since, &[(Ink::Action, "proposing padding 18 on panel"),
        (Ink::Warning, "app_set refused: Ask the owner to allow editing this share.")],
        "a sent but refused Set has one start and one warning");

    model.open_town(private).unwrap();
    let private_app = model.act(Action::AddApp { town: private, board: None, geometry: [0, 0, 6400, 6400],
        session: SHARE, name: "Private app".into() }, Origin::Ui).unwrap();
    let mut wrong_town = [Session::owner(private, SHARE, private_app, ana, LINK).unwrap(),
        Session::peer(private, SHARE, id, ana, owner.me()).unwrap()];
    let hidden = list_apps(&mut model, &mut tools, &mut wrong_town, &mut waits, &binding, None);
    assert_eq!(parse_json(&hidden.text), parse_json(r#"{"apps":[],"next":null}"#),
        "neither the open town's App nor a matching item in a different session town is discoverable");
    let props = app_tool("app_props", &format!(r#"{{"binding":"{binding}","item":"{item}","path":"ui.panel"}}"#)).unwrap();
    let wrong = tools.start("wrong", props, &mut model, &mut wrong_town, &mut waits).0.unwrap();
    assert_eq!(wrong.outcome, ToolOutcome::Refused, "a matching signed item cannot route a request into another town");
    let edits = app_tool("app_edits", &format!(r#"{{"binding":"{binding}","item":"{}"}}"#, hex(&private_app))).unwrap();
    let wrong = tools.start("wrong", edits, &mut model, &mut wrong_town, &mut waits).0.unwrap();
    assert_eq!(wrong.outcome, ToolOutcome::Refused, "the open town's private history is outside the binding");
    let listing = |model: &mut Model, tools: &mut AppTools, s: &mut Copies, waits: &mut AgentWaits, may_edit, window| {
        let since = waits.traces.rows(&line)[0].output.len();
        let out = list_apps(model, tools, std::slice::from_mut(&mut s.ana.session), waits, &binding, None);
        assert_eq!(out.outcome, ToolOutcome::Ok, "a working binding can discover the share in its own town");
        assert_trace(waits, line, since, &[(Ink::Action, "listed 1 shared app")], "discovery records exactly one completed read");
        let page = parse_json(&out.text).unwrap();
        let mut fields = vec![("item", json::s(&item)), ("title", json::s("Scene")), ("sharer", json::s("Walk A")),
            ("mine", json::Value::Bool(false)), ("may_edit", json::Value::Bool(may_edit))];
        if window {
            fields.push(("window", json::obj(vec![("id", json::Value::Int(3)), ("size", json::Value::Arr(vec![json::Value::F64(640.), json::Value::F64(480.)]))])));
        }
        assert_eq!(page, json::obj(vec![("apps", json::Value::Arr(vec![json::obj(fields)])), ("next", json::Value::Null)]),
            "discovery returns exactly the signed App metadata, known window and this AI's current grant");
        page.get("apps").unwrap().as_arr().unwrap()[0].get("item").unwrap().as_str().unwrap().to_owned()
    };
    listing(&mut model, &mut tools, &mut s, &mut waits, false, false);
    s.host.session.window(WindowId(3), [640., 480.]);
    s.allow(ana, true);
    listing(&mut model, &mut tools, &mut s, &mut waits, true, true);
    s.host.session.stop_agent(ana, agent, true).unwrap();
    s.exchange();
    listing(&mut model, &mut tools, &mut s, &mut waits, false, true);
    s.host.session.stop_agent(ana, agent, false).unwrap();
    s.allow(ana, false);
    listing(&mut model, &mut tools, &mut s, &mut waits, false, true);
    s.allow(ana, true);
    let discovered = listing(&mut model, &mut tools, &mut s, &mut waits, true, true);
    s.app.props = r#"{"props":[{"name":"padding","value":"0"},{"name":"visible","value":"true"}],"cascade":[]}"#.into();
    for (name, fields, start, outcome) in [
        ("app_props", r#""path":"ui.panel""#, "reading panel's properties in Scene", Some("read 2 properties of panel")),
        ("app_pick", r#""path":"ui.panel""#, "picking panel in Scene", Some("picked View panel")),
        ("app_pick", r#""window":3,"x":282,"y":130"#, "picking at 282, 130 in Scene", Some("picked View panel")),
        ("app_set", r#""path":"ui.panel","prop":"padding","value":"18""#, "proposing padding 18 on panel", Some("set padding 0 → 18 on panel")),
        ("app_select", r#""path":"/dock/View.2""#, "showing View.2", None),
        ("app_select", r#""path":null"#, "clearing its selection", None),
    ] {
        let since = waits.traces.rows(&line)[0].output.len();
        let tool = app_tool(name, &format!(r#"{{"binding":"{binding}","item":"{discovered}",{fields}}}"#)).unwrap();
        let (out, changed) = tools.start(name, tool, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits);
        assert!(out.is_none() && changed == Some(token), "AI calls wait for their own result and refresh the starting trace");
        assert_trace(&waits, line, since, &[(Ink::Action, start)], "a sent request records only its start before the notice");
        s.one();
        let mut answers = Vec::new();
        while let Some((seat, req, actor, notice)) = s.ana.session.view.updates.pop_front() {
            assert!(tools.answer(&s.ana.session, owner.me(), req, actor, &notice, &mut waits).is_none(),
                "another seat's notice cannot settle {name}");
            assert!(tools.answer(&s.ana.session, seat, req, Some([99; 32]), &notice, &mut waits).is_none(),
                "a different AI's notice cannot settle {name}");
            if let Some((out, changed)) = tools.answer(&s.ana.session, seat, req, actor, &notice, &mut waits) {
                assert_eq!(changed, outcome.map(|_| token), "only an outcome with a trace line triggers a repaint");
                assert_eq!((actor, seat), (Some(agent), ana), "AI replies keep the authenticated person's seat and AI label");
                answers.push(out);
            }
        }
        let expected: Vec<_> = outcome.into_iter().map(|text| (Ink::Action, text)).collect();
        assert_trace(&waits, line, since + 1, &expected, "only the matching notice adds the actual outcome");
        assert_eq!(answers.len(), 1, "one response resolves {name}");
        assert_eq!(answers[0].outcome, ToolOutcome::Ok, "{name} succeeds through the owner");
    }
    assert_eq!((s.records[0].agent, s.records[0].seat), (Some(agent), ana), "the durable edit credits the peer's AI");
    assert_eq!(s.app.padding, "18", "the tool used the same app path");

    let saved = owner.act(Action::RecordAppEdit { town: owner.open_id(), item: id, edit: s.records[0].clone() }, Origin::Ui).unwrap();
    crate::model::tests::exchange(&mut owner, &mut model);
    let edit = &model.tweak_board_state(town).unwrap().0.entities[&saved];
    assert_eq!(edit.value(field::EDIT_AGENT), Some(&Value::Id(agent)), "the signed edit replicates the bound AI's id");
    assert!(edit.value(field::EDIT_AI).is_none(), "a new signed AI edit omits the legacy field entirely");
    let history_tool = || app_tool("app_edits", &format!(r#"{{"binding":"{binding}","item":"{item}"}}"#)).unwrap();
    let since = waits.traces.rows(&line)[0].output.len();
    let out = tools.start("history", history_tool(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits).0.unwrap();
    assert_eq!(out.outcome, ToolOutcome::Ok, "a bound agent can read signed edit history");
    assert_trace(&waits, line, since, &[(Ink::Action, "read Scene's edit history")], "history records one immediate result");
    let history = parse_json(&out.text).unwrap();
    let [row] = history.get("edits").unwrap().as_arr().unwrap() else { panic!("expected one signed edit") };
    assert_eq!(row.get("agent"), Some(&json::s(hex(&agent))), "the history API reports the same agent in hex");
    assert_eq!(row.get("ai"), Some(&json::Value::Bool(true)), "the history API recognizes a new AI edit from its agent id");
    assert_eq!(model.open_id(), private, "discovery, the live calls and history all leave the displayed town alone");
    model.open_town(town).unwrap();
    let since = waits.traces.rows(&line)[0].output.len();
    let (out, _) = tools.start("late", set(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits);
    assert!(out.is_none(), "the Set is still held on the original owner's turn");
    assert_trace(&waits, line, since, &[(Ink::Action, "proposing padding 18 on panel")], "the original line owns the Set start");
    let next = model.mention_agent(agent, "a different request".into(), None).unwrap();
    waits.offered(&model, next);
    let reply = waits.wait(&mut model, "next", token, Some(cursor(&page[0])));
    assert_eq!(reply[0].outcome, ToolOutcome::Ok, "the binding picked up the next request before the Set answer");
    assert_eq!(model.owner_turn(token), Ok((agent, next)), "the next request is now the binding's authorized turn");
    s.one();
    let late = std::mem::take(&mut s.ana.session.view.updates);
    let replies: Vec<_> = late.into_iter().filter_map(|(seat, req, actor, n)|
        tools.answer(&s.ana.session, seat, req, actor, &n, &mut waits)).collect();
    let [(out, changed)] = replies.as_slice() else { panic!("the late notice must resolve its one held Set") };
    assert_eq!((out.call_id.as_str(), out.outcome, *changed), ("late", ToolOutcome::Ok, None), "a late answer resolves its call without repainting either turn");
    assert_trace(&waits, next, 0, &[], "the new request never inherits the last Set's outcome");
    assert_trace(&waits, line, since, &[(Ink::Action, "proposing padding 18 on panel")], "a finished trace drops late output");

    s.ana.session.end("the sharer closed the app");
    let (out, changed) = tools.start("closed", set(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits);
    assert_eq!((out.unwrap().outcome, changed), (ToolOutcome::Refused, Some(token)), "a closed share refuses a Set before submission");
    assert_trace(&waits, next, 0, &[(Ink::Warning, "app_set refused: share ended")], "a failed session request adds only its warning");
    let out = list_apps(&mut model, &mut tools, std::slice::from_mut(&mut s.ana.session), &mut waits, &binding, None);
    assert_eq!(parse_json(&out.text), parse_json(r#"{"apps":[],"next":null}"#), "an ended session disappears while its signed App still exists");
    assert_trace(&waits, next, 1, &[(Ink::Action, "listed 0 shared apps")], "an empty discovery page records zero apps");
    check_app_list_pages(&mut model, &mut tools, &mut waits, &binding, next);

    model.remove_agent(agent).unwrap();
    let out = tools.start("inactive", history_tool(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits).0.unwrap();
    assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, "This binding has ended."),
        "removing the agent immediately ends its binding's access to history");
}

#[test]
fn wire_round_trips_and_refuses_hostile_bodies() {
    let rect = Rect { window: 3, x: 2., y: 4., w: 6., h: 8. };
    let packet = |body| Packet { share: SHARE, sender: ANA, req: 7, agent: Some(ANA_AGENT), body };
    let mut bodies: Vec<_> = [Request::AskEdit, Request::Select(None), Request::Select(Some(PATH.into())),
        Request::Pick(At::Point { window: 4, x: -1., y: 4.25 }), Request::Pick(At::Path(PATH.into())),
        Request::Props { path: PATH.into() }, set("8"), Request::Look, Request::LookDone { look: u128::MAX, ok: true },
        Request::LookDone { look: 15, ok: false }].into_iter().map(Body::Request).collect();
    bodies.push(Body::Notice { seat: ANA, delivery: 4, notice: Notice::Picture { png: [1, 2, 3].into(), size: [97, 51] } });
    bodies.extend([Notice::Asked, Notice::Allowed(true), Notice::Running(set("8")), Notice::Stopped(true), Notice::Running(Request::Look), Notice::Looked,
        Notice::Picked { path: PATH.into(), ty: "View".into(), rect },
        Notice::Rows { path: PATH.into(), rows: ROWS.into() }, Notice::Refused("ask".into()),
        Notice::Window { window: 3, size: [800., 600.] }, Notice::Ended("ended".into()),
        Notice::Selected { path: Some(PATH.into()), rect: Some(rect) }, Notice::Applied { path: PATH.into(), prop: "padding".into(),
            old: "0".into(), new: "8".into(), seq: 5 }].into_iter().map(|notice| Body::Notice { seat: BEN, delivery: 4, notice }));
    bodies.extend([Body::Ack(4), Body::Ready]);
    for query in [
        TreeQuery::Children { path: Some("/dock/card".into()), depth: u8::MAX, offset: u32::MAX, limit: u16::MAX },
        TreeQuery::Reveal { path: "/café".into(), limit: wire::TREE_PAGE },
        TreeQuery::Search { query: "é".repeat(150), offset: 30, limit: wire::TREE_PAGE },
    ] {
        bodies.push(Body::Request(Request::Tree(query.clone())));
        bodies.push(Body::Notice { seat: ANA, delivery: 4, notice: Notice::Tree { query, tree: TREE.into() } });
    }
    bodies.extend([Request::Theme(ThemeOp::Get), Request::Theme(ThemeOp::Set {
        name: "color_primary".into(), value: "theme.color_bg_app".into() }), install("Warm")].map(Body::Request));
    bodies.push(Body::Notice { seat: ANA, delivery: 4, notice: Notice::Theme { theme: THEME.into() } });
    for body in bodies {
        let p = packet(body);
        let bytes = p.encode();
        assert_eq!(wire::decode(ANA, &bytes), Ok(Some(p)), "all fields survive their wire layout");
        assert!(wire::decode(BEN, &bytes).is_err(), "a body cannot impersonate another stream's seat");
        assert!(wire::decode(ANA, &bytes[..bytes.len()-1]).is_err(), "truncated body refused");
        let mut extra = bytes.clone(); extra.push(0);
        assert!(wire::decode(ANA, &extra).is_err(), "trailing data refused");
    }
    for request in [Request::Theme(ThemeOp::Get), Request::Select(None), Request::AskEdit, Request::LookDone { look: 1, ok: true }, Request::Props { path: String::new() },
        Request::Tree(TreeQuery::Reveal { path: PATH.into(), limit: wire::TREE_PAGE })] {
        let body = Body::Notice { seat: ANA, delivery: 1, notice: Notice::Running(request) };
        assert!(wire::decode(ANA, &packet(body).encode()).is_err(), "Running requires a valid app operation");
    }
    for query in [
        TreeQuery::Children { path: None, depth: 0, offset: 0, limit: 10 },
        TreeQuery::Reveal { path: PATH.into(), limit: 0 },
        TreeQuery::Reveal { path: "\n".into(), limit: 10 },
        TreeQuery::Search { query: "  ".into(), offset: 0, limit: 10 },
        TreeQuery::Search { query: "one\ntwo".into(), offset: 0, limit: 10 },
    ] {
        for body in [Body::Request(Request::Tree(query.clone())), Body::Notice { seat: ANA, delivery: 4,
            notice: Notice::Tree { query, tree: TREE.into() } }] {
            assert!(wire::decode(ANA, &packet(body).encode()).is_err(), "invalid query fields are refused in either direction");
        }
    }
    for (from, to) in [("children\"", "unknown\""), ("\"/dock/card\"", "\"\""),
        ("[10,20,100,40]", "[10,20,-1,40]"), ("\"visible\":true", "\"visible\":true,\"matched\":7")] {
        assert_eq!(wire::validate_tree(&TREE.replace(from, to)), Err("invalid widget tree"), "invalid tree field: {to}");
    }
    let picture = |size| packet(Body::Notice { seat: ANA, delivery: 1, notice: Notice::Picture { png: [1].into(), size } });
    let mut anonymous = picture([1, 1]);
    anonymous.agent = None;
    assert!(wire::decode(ANA, &anonymous.encode()).is_err(), "a picture delivery must identify its guest AI");
    for size in [[0, 1], [1, u32::MAX]] {
        assert!(wire::decode(ANA, &picture(size).encode()).is_err(), "picture dimensions must be valid PNG dimensions");
    }
    let mut oversized = picture([1, 1]).encode();
    let length = oversized.len() - 9;
    let over = (makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES / 4 * 3 + 1) as u64;
    oversized[length..length + 8].copy_from_slice(&over.to_le_bytes());
    assert_eq!(wire::decode(ANA, &oversized), Err("invalid picture byte count"), "reject a PNG outside the base64 budget before allocating");
    let mut stopped = packet(Body::Notice { seat: ANA, delivery: 1, notice: Notice::Stopped(false) });
    stopped.agent = None;
    assert!(wire::decode(ANA, &stopped.encode()).is_err(), "only an AI can be stopped");
    let mut person = packet(Body::Request(Request::AskEdit));
    person.agent = None;
    let mut header = person.encode();
    assert_eq!(header.last(), Some(&0), "a person's request ends its header with the clear flag");
    assert_eq!(wire::decode(ANA, &header), Ok(Some(person)), "a person's identity round-trips without an id");
    *header.last_mut().unwrap() = 2;
    assert_eq!(wire::decode(ANA, &header), Err("invalid flag"), "only the two identity flags are valid");
    *header.last_mut().unwrap() = 1;
    header.extend_from_slice(&ANA_AGENT[..31]);
    assert_eq!(wire::decode(ANA, &header), Err("truncated body"), "an AI flag requires all 32 id bytes");
    let mut town = Copies::new(OWNER, ANA, ITEM);
    let private = Notice::Picture { png: [1, 2, 3].into(), size: [2, 1] };
    town.host.session.emit_to(ANA, 7, Some(ANA_AGENT), private.clone());
    town.host.poll();
    assert!(town.ben.relay.take(TWEAK).is_empty(), "a private picture never enters the observer's transport");
    assert!(!has(&town.host, 7, |n| n == &private), "a private delivery bypasses the sharer's view");
    town.ana.poll();
    assert!(has(&town.ana, 7, |n| n == &private), "the asking guest receives the private delivery");
    let forged = Packet { share: SHARE, sender: ANA, req: 13, agent: None,
        body: Body::Notice { seat: BEN, delivery: 1, notice: Notice::Allowed(true) } };
    town.ana.relay.send(OWNER, TWEAK, forged.encode()).unwrap();
    let errs = poll_relay(std::slice::from_mut(&mut town.host.session), &mut town.host.relay);
    assert_eq!(errs, ["unexpected tweak message direction"], "an authenticated peer cannot announce consent");
    assert!(town.host.session.view.allowed.is_empty(), "forged consent cannot reach the owner gate");

    let mut bytes = packet(Body::Request(Request::Props { path: "x".into() })).encode();
    let count = bytes.len() - 9;
    bytes[count..count+8].fill(255);
    assert!(wire::decode(ANA, &bytes).is_err(), "declared counts must fit the remaining bytes");
    bytes[1] = 255;
    assert_eq!(wire::decode(ANA, &bytes), Ok(None), "unknown tag ignored");
    bytes[0] = 99;
    assert_eq!(wire::decode(ANA, &bytes), Err("unknown tweak version"), "unknown version refused");
}

#[test]
fn tweak_wire_ignores_unknown_tags_and_tlv_tails() {
    let packet = Packet { share: SHARE, sender: ANA, req: 7, agent: None, body: Body::Ready };
    let mut known = packet.encode();
    makepad_town_core::tlv::write_tlv(&mut known, 0x7f, b"ext");
    assert_eq!(wire::decode(ANA, &known), Ok(Some(packet.clone())));
    known.pop();
    assert_eq!(wire::decode(ANA, &known), Err("truncated tlv value"));

    let mut future = packet.encode();
    future[1] = 15;
    assert_eq!(wire::decode(ANA, &future), Ok(None));
    assert_eq!(wire::decode(BEN, &future), Err("sender differs from authenticated peer"));
    let mut bad_header = future.clone();
    *bad_header.last_mut().unwrap() = 2;
    assert_eq!(wire::decode(ANA, &bad_header), Err("invalid flag"));
    bad_header[0] = 2;
    assert_eq!(wire::decode(ANA, &bad_header), Err("unknown tweak version"));
    // A newer copy's tree query kind is skipped too, like an unknown tag.
    let tree = Packet { body: Body::Request(Request::Tree(TreeQuery::Children { path: None, depth: 1, offset: 0, limit: 1 })), ..packet.clone() };
    let mut newer_query = tree.encode();
    assert_eq!(wire::decode(ANA, &newer_query), Ok(Some(tree)));
    newer_query[packet.encode().len()] = 4;
    assert_eq!(wire::decode(ANA, &newer_query), Ok(None), "an unknown tree query kind is skipped");

    let get = Packet { body: Body::Request(Request::Theme(ThemeOp::Get)), ..packet.clone() };
    let snapshot = Packet { body: Body::Notice { seat: ANA, delivery: 1, notice: Notice::Theme { theme: THEME.into() } }, ..packet.clone() };
    for (p, tag) in [(get.clone(), 27), (snapshot, 28)] {
        let mut bytes = p.encode();
        assert_eq!(bytes[1], tag, "theme uses its allocated tag");
        assert_eq!(wire::min_level(&bytes), THEME_LEVEL, "theme frames require the theme Hello level");
        makepad_town_core::tlv::write_tlv(&mut bytes, 0x7f, b"ext");
        assert_eq!(wire::decode(ANA, &bytes), Ok(Some(p)), "theme packets skip unknown TLV fields");
    }
    let mut future_op = get.encode();
    *future_op.last_mut().unwrap() = 4;
    assert_eq!(wire::decode(ANA, &future_op), Ok(None), "a future theme operation is skipped");

    let Request::Theme(ThemeOp::Install(spec)) = install("Warm") else { unreachable!() };
    let mut hostile_count = Packet { body: Body::Request(Request::Theme(ThemeOp::Install(ThemeInstall {
        globals: vec![], overrides: vec![], ..spec.clone() }))), ..packet.clone() }.encode();
    let counts = hostile_count.len() - 16;
    hostile_count[counts..counts+8].fill(0xff);
    assert!(wire::decode(ANA, &hostile_count).is_err(), "a declared theme vector must fit the remaining body");
    for bad in [ThemeInstall { base: "surprise".into(), ..spec.clone() }, ThemeInstall { sheet: Some("\n".into()), ..spec.clone() },
        ThemeInstall { label: String::new(), ..spec.clone() }, ThemeInstall { overrides: vec![("bad.key".into(), ThemeVal::Color(0))], ..spec.clone() },
        ThemeInstall { globals: vec![("space_factor".into(), ThemeVal::Num(f64::INFINITY))], ..spec }] {
        let p = Packet { body: Body::Request(Request::Theme(ThemeOp::Install(bad))), ..packet.clone() };
        assert!(wire::decode(ANA, &p.encode()).is_err(), "invalid typed install fields are rejected on decode");
    }

    let running = |delivery| {
        let body = Body::Notice { seat: BEN, delivery, notice: Notice::Running(Request::Look) };
        let mut bytes = Packet { body, agent: Some(ANA_AGENT), ..packet.clone() }.encode();
        *bytes.last_mut().unwrap() = 0xff;
        bytes
    };
    let future_request = running(1);
    assert_eq!(wire::decode(ANA, &future_request), Ok(None));
    assert_eq!(wire::decode(ANA, &running(0)), Err("invalid delivery id"));

    let mut town = Copies::new(OWNER, ANA, ITEM);
    for bytes in [future, future_request] { town.ana.relay.send(OWNER, TWEAK, bytes).unwrap(); }
    let errors = poll_relay(std::slice::from_mut(&mut town.host.session), &mut town.host.relay);
    assert!(errors.is_empty(), "unknown packets must bypass session dispatch: {errors:?}");
}

fn relay_controller() -> crate::call::CallController {
    use crate::call::{CallController, CallEnv};
    use std::sync::Arc;
    let mut call = CallController::default();
    call.configure(CallEnv { files: Default::default(), devices_path: Default::default(), voice: makepad_call::driver::VoiceSource::Off,
        spawn: Arc::new(|_, _| panic!("link proof alone starts no call worker")),
        jobs: Arc::new(|_, _| panic!("link proof alone starts no audio job")), signal: Arc::new(|| {}) });
    call
}

#[test]
fn controller_relay_binds_messages_and_consent_to_the_link_epoch() {
    let mut call = relay_controller();
    let cx = &mut Cx::new(Box::new(|_, _| {}));
    let port = call.link_up(cx, OWNER, ANA, "Peer").expect("proved first epoch");
    let mut hub = Tweaks::default();
    hub.sessions.push(Session::<u64, u64>::owner(TOWN, SHARE, ITEM, OWNER, LINK).unwrap());
    hub.sessions[0].set_members([OWNER, ANA]);
    hub.poll_call(&mut call);
    let payload = vec![42; makepad_town_core::connect::MAX_CALL + 1];
    assert_eq!(call.send(BEN, TWEAK, vec![7]), Err(NotLinked), "an unlinked seat cannot receive");
    call.send(ANA, TWEAK, payload.clone()).unwrap();
    call.send(ANA, crate::call::topic::PROBE, vec![9]).unwrap();
    call.poll(cx);
    for frame in port.messages_out.try_iter() { port.messages_in.try_send(frame).unwrap(); }
    call.poll(cx);
    assert_eq!(call.take(TWEAK), [(ANA, payload)],
        "the relay returns one reassembled body attributed to the proved seat");
    assert_eq!(call.take_peer(crate::call::topic::PROBE), [vec![9]], "the tweak drain leaves other topics for their consumers");
    hub.sessions[0].consent(ANA, true).unwrap();
    let _second = call.link_up(cx, OWNER, ANA, "Peer").unwrap();
    assert!(call.take_peer_events().is_empty(), "a clean same-seat replacement produces no loss count");
    hub.poll_call(&mut call);
    assert!(hub.sessions[0].view.allowed.is_empty(), "an empty queue on a same-seat relink still ends consent");
    hub.sessions[0].consent(ANA, true).unwrap();
    call.link_down(cx, ANA);
    let dropped = call.take_dropped(TWEAK);
    let [PeerUndelivered { peer: ANA, topic: TWEAK, count }] = dropped.as_slice() else {
        panic!("the closed controller must report its outstanding tweak notices")
    };
    assert!(*count > 0, "the loss count covers queued notices");
    hub.poll_call(&mut call);
    assert!(hub.sessions[0].view.allowed.is_empty(), "closing the link ends the replacement epoch's grant");
}

#[test]
fn call_controller_relay_broadcasts_and_receives_across_three_peers() {
    use crate::{call::{CallController, PeerEvent, topic::INPUT}, link_worker::MediaPort};

    struct Guest {
        call: CallController,
        session: Session<u64, u64>,
        owner_port: MediaPort,
        guest_port: MediaPort,
    }

    fn exchange(cx: &mut Cx, owner: &mut CallController, hub: &mut Tweaks<u64, u64>, guests: &mut [Guest]) {
        loop {
            owner.poll(cx);
            hub.poll_call(owner);
            owner.poll(cx);
            let mut moved = false;
            for g in guests.iter_mut() {
                g.call.poll(cx);
                assert!(poll_relay(std::slice::from_mut(&mut g.session), &mut g.call).is_empty());
                g.call.poll(cx);
                for (from, to) in [(&g.owner_port, &g.guest_port), (&g.guest_port, &g.owner_port)] {
                    for frame in from.messages_out.try_iter() {
                        to.messages_in.try_send(frame).unwrap();
                        moved = true;
                    }
                }
            }
            if !moved { return }
        }
    }

    let cx = &mut Cx::new(Box::new(|_, _| {}));
    let mut owner = relay_controller();
    let mut hub = Tweaks::default();
    hub.sessions.push(Session::owner(TOWN, SHARE, ITEM, OWNER, LINK).unwrap());
    hub.sessions[0].set_members([OWNER, ANA, BEN]);
    let mut guests = [ANA, BEN].map(|peer| {
        let mut call = relay_controller();
        let owner_port = owner.link_up(cx, OWNER, peer, "Guest").unwrap();
        let guest_port = call.link_up(cx, peer, OWNER, "Owner").unwrap();
        let mut session = Session::peer(TOWN, SHARE, ITEM, peer, OWNER).unwrap();
        session.request(None, Request::AskEdit).unwrap();
        Guest { call, session, owner_port, guest_port }
    });
    assert_eq!(owner.peers(), [ANA, BEN]);
    exchange(cx, &mut owner, &mut hub, &mut guests);
    assert_eq!(hub.sessions[0].view.asked, [ANA, BEN].into(), "requests retain both authenticated senders");
    for peer in [ANA, BEN] { hub.sessions[0].consent(peer, true).unwrap(); }
    exchange(cx, &mut owner, &mut hub, &mut guests);
    for guest in &guests { assert_eq!(guest.session.view.allowed, [ANA, BEN].into(), "both grants are broadcast"); }

    let ben_epoch = owner.peer_epochs().find(|(peer, _)| *peer == BEN).unwrap();
    owner.send(ANA, TWEAK, vec![1]).unwrap();
    owner.send(ANA, INPUT, vec![2]).unwrap();
    assert!(owner.link_down(cx, ANA));
    hub.poll_call(&mut owner);
    assert_eq!(hub.sessions[0].view.allowed, [BEN].into(), "only the disconnected seat loses consent");
    assert_eq!(owner.peer_epoch(), Some(ben_epoch));
    assert_eq!(owner.take_peer_events(), [PeerEvent::PeerUndelivered { peer: ANA, topic: INPUT, count: 1 }],
        "Tweak consumed its own loss and left the input loss for its dispatcher");

    guests[1].session.request(None, Request::Select(Some(PATH.into()))).unwrap();
    exchange(cx, &mut owner, &mut hub, &mut guests[1..]);
    assert_eq!(hub.sessions[0].view.selections[&BEN].path, PATH, "the surviving guest can still send requests");
    assert_eq!(guests[1].session.view.selections[&BEN].path, PATH, "the surviving guest receives the resulting notice");
    assert_eq!(guests[1].session.view.allowed, [BEN].into());
}

#[test]
fn large_rows_keep_notice_order_and_replay_as_whole_bodies() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    let content = "é".repeat(makepad_town_core::connect::MAX_CALL / 2);
    let rows = format!(r#"{{ "props": [{{"name":"padding","value":"8","level":2,"cascade":[{{"source":{{"text":"{content}"}}}}]}},{{"name":"visible","value":"true"}}] }}"#);
    s.app.props = rows.clone();
    assert_eq!(props::Props::rebuild_rows(&rows).unwrap().rows.iter().map(|p| (p.prop.as_str(), p.value.as_str(), p.level)).collect::<Vec<_>>(), [("padding","8",Some(2)), ("visible","true",None)],
        "the panel reads both required fields and an optional level through deeper cascade metadata");

    let before = s.host.send(Request::Select(Some(PATH.into())));
    let props = s.ana.send(Request::Props { path: PATH.into() });
    s.ana.poll();
    s.host.poll();
    s.host.session.drive_app(&mut s.app);
    s.app.answer(&mut s.host.session);
    let after = s.host.send(Request::Select(None));
    s.host.poll();
    // The earlier notice has crossed, but C6 loses the remaining bodies before handing them off.
    let lost = s.ana.relay.take(TWEAK);
    let [(first_seat, first), (_, running), (_, large), _] = &lost[..] else { panic!("one logical body per notice") };
    assert!(matches!(wire::decode(OWNER, running).unwrap().expect("known Running notice").body, Body::Notice { notice: Notice::Running(Request::Props { .. }), .. }),
        "the Props start travels before its complete Rows body");
    assert!(matches!(wire::decode(OWNER, large).unwrap().expect("known Rows notice").body, Body::Notice { notice: Notice::Rows { .. }, .. }),
        "the relay gets the entire encoded Rows in one send");
    s.host.relay.net.borrow_mut().inbox.entry(ANA).or_default().push_back((*first_seat, TWEAK, first.clone()));
    s.ana.poll();
    s.ben.poll();
    assert!(has(&s.ana, before, |n| matches!(n, Notice::Selected { .. })), "the earlier notice is already visible");
    assert!(!has(&s.ana, props, |_| true) && !has(&s.ana, after, |_| true), "undelivered bodies do not advance the receiver");
    s.host.relay.dropped.push(PeerUndelivered { peer: ANA, topic: TWEAK, count: 3 });
    s.host.session.peer_unlinked(ANA);
    s.ana.session.peer_unlinked(OWNER);
    s.exchange();
    for town in [&s.ana, &s.ben] {
        let observed: Vec<_> = town.session.view.updates.iter().filter_map(|(_, id, _, notice)| {
            if let Notice::Running(request) = notice {
                assert_eq!((*id, request), (props, &Request::Props { path: PATH.into() }), "replay preserves the running Props");
                return Some((*id, "running"))
            }
            if *id == props {
                assert!(matches!(notice, Notice::Rows { path, rows: full } if path == PATH && full == &rows),
                    "every byte of the app reply survives, including whitespace and unknown nested fields");
            }
            [before, props, after].contains(id).then_some((*id, "answer"))
        }).collect();
        assert_eq!(observed, [(before, "answer"), (props, "running"), (props, "answer"), (after, "answer")],
            "each seat sees one start and one complete Rows between the smaller notices");
    }
    assert_eq!(s.app.sent.len(), 1, "relink replay of the pending props request never queries the app twice");

    s.allow(ANA, true);
    let value = format!(r#""{content}""#);
    let set_id = s.ana.send(set(&value));
    s.one();
    assert!(has(&s.ben, set_id, |n| matches!(n, Notice::Applied { seq: 1, new, .. } if new == &value)),
        "a large request travels back to the owner on the peer's stream and applies once");
}

#[test]
fn json_depth_refuses_one_delivery_and_the_peer_stream_continues() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    let rows = |depth: usize| format!(r#"{{"props":[{{"name":"padding","value":"8"}}],"metadata":{}0{}}}"#,
        "[".repeat(depth - 1), "]".repeat(depth - 1));
    s.app.props = rows(200);
    let props = || Request::Props { path: PATH.into() };
    let ok = s.ana.send(props());
    s.one();
    assert!(has(&s.ana, ok, |n| matches!(n, Notice::Rows { rows, .. } if props::Props::rebuild_rows(rows).is_some_and(|p| p.rows.len() == 1 && p.rows[0].prop == "padding" && p.rows[0].value == "8"))),
        "a 200-deep app reply reaches the peer and renders in the panel");

    s.app.props = rows(300);
    let local = s.ana.send(props());
    s.one();
    assert!(has(&s.ana, local, |n| matches!(n, Notice::Refused(reason) if reason == TOO_DEEP)),
        "the owner returns a refusal for an over-depth reply from its app");

    // A peer can put JSON on the wire that this owner implementation would already have refused.
    let bad = s.ana.send(props());
    let next = s.ana.send(Request::AskEdit);
    let frame = |req, delivery, notice| Packet { share: SHARE, sender: OWNER, req, agent: None,
        body: Body::Notice { seat: ANA, delivery, notice } }.encode();
    let deep = frame(bad, 10, Notice::Rows { path: PATH.into(), rows: rows(300) });
    let good = frame(next, 11, Notice::Asked);
    assert_eq!(wire::decode(OWNER, &deep), Err(TOO_DEEP), "the wire decoder enforces the shared stack guard");
    s.ana.session.view.updates.clear();
    for body in [deep.clone(), deep, good] { s.host.relay.send(ANA, TWEAK, body).unwrap(); }
    s.ana.poll();
    let notices: Vec<_> = s.ana.session.view.updates.into_iter().map(|(_, req, _, n)| (req, n)).collect();
    assert_eq!(notices, [(bad, Notice::Refused(TOO_DEEP.into())), (next, Notice::Asked)],
        "an over-depth peer delivery is refused once across replay and the next notice arrives on the same stream");
}

#[test]
fn tree_wire_unicasts_supersedes_populates_rects_and_gates_older_peers() {
    use makepad_town_core::{link::VERSION, share::{hello::Hello, Message}};
    use wire::TREE_PAGE;

    let current = Some(TREE_LEVEL);
    let older = Some(TREE_LEVEL - 1);
    let card = "/dock/card";
    let children = || Request::Tree(TreeQuery::Children { path: None, depth: 1, offset: 0, limit: TREE_PAGE });
    let search = |query: &str| Request::Tree(TreeQuery::Search { query: query.into(), offset: 0, limit: TREE_PAGE });
    let tree_reply = |n: &Notice| matches!(n, Notice::Tree { tree, .. } if tree == TREE);
    fn answer(s: &mut Copies) {
        s.ana.poll();
        s.host.poll();
        s.host.session.drive_app(&mut s.app);
        s.app.answer(&mut s.host.session);
        s.host.poll();
    }

    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.session.set_peer_level(OWNER, older);
    assert_eq!(s.ana.session.request(None, children()), Err(OLDER_SHARER_TREE.into()), "an older sharer is refused locally");
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).is_empty(), "zero wire bytes reach the older sharer");

    s.ana.session.set_peer_level(OWNER, current);
    s.host.session.set_peer_level(BEN, older);
    s.allow(ANA, true);
    let page = s.ana.send(children());
    answer(&mut s);
    assert!(s.ben.relay.take(TWEAK).is_empty(), "zero wire bytes reach the older guest for another seat's page");
    s.ana.poll();
    assert!(has(&s.ana, page, tree_reply), "the requesting current guest receives the page");
    assert!(!has(&s.host, page, |_| true), "a remote tree read produces no owner activity");

    let forged = Packet { share: SHARE, sender: BEN, agent: None, req: 37, body: Body::Request(children()) }.encode();
    assert_eq!(forged[1], 25, "Tree request uses its assigned wire tag");
    s.ben.relay.send(OWNER, TWEAK, forged).unwrap();
    let sent = s.app.sent.len();
    s.host.poll();
    s.host.session.drive_app(&mut s.app);
    let refused = s.ben.relay.take(TWEAK);
    assert!(matches!(&refused[..], [(_, bytes)] if bytes[1] == 12 && matches!(wire::decode(OWNER, bytes).unwrap().expect("a known tweak packet").body,
        Body::Notice { notice: Notice::Refused(why), .. } if why.contains("does not support"))),
        "forged Tree from an older guest receives only a v1 refusal");
    assert_eq!(s.app.sent.len(), sent, "an older guest never reaches the shared app");

    s.host.session.set_peer_level(BEN, current);
    let denied = s.ben.send(children());
    s.one();
    assert!(has(&s.ben, denied, |n| matches!(n, Notice::Refused(why) if why.contains("Ask the owner"))), "Tree requires Allow");
    assert!(!has(&s.ana, denied, |_| true) && !has(&s.host, denied, |_| true), "Allow refusal stays with its requester");
    assert_eq!(s.app.sent.len(), sent, "a denied seat cannot read the app");

    let allowed = s.ana.send(children());
    answer(&mut s);
    s.ana.poll();
    assert!(has(&s.ana, allowed, tree_reply) && !has(&s.host, allowed, |_| true) && s.ben.relay.take(TWEAK).is_empty(),
        "the current observer and owner see no start or result for another seat's read");

    let sent = s.app.sent.len();
    let select = s.ana.send(Request::Select(Some(card.into())));
    s.one();
    let ring = Rect { window: 0, x: 10., y: 20., w: 100., h: 40. };
    for seat in [&s.host, &s.ana, &s.ben] {
        assert!(has(seat, select, |n| matches!(n, Notice::Selected { path: Some(p), rect: Some(r) } if p == card && *r == ring)),
            "every seat sees the selection ring from the tree page");
    }
    assert_eq!(s.app.sent.len(), sent, "selecting a tree row needs no extra app call");

    s.host.session.bind_window(WindowId(0));
    let local = s.host.session.request(Some(ANA_AGENT), Request::Tree(TreeQuery::Children {
        path: Some(card.into()), depth: u8::MAX, offset: 17, limit: u16::MAX,
    })).unwrap();
    s.host.session.drive_app(&mut s.app);
    s.app.answer(&mut s.host.session);
    s.host.poll();
    assert!(has(&s.host, local, tree_reply), "the owner receives its own page locally");
    assert!(matches!(s.app.sent.last(), Some((LINK, Some(WindowId(0)), DesignOp::Design { verb: DesignVerb::Tree, args }))
        if args == &[("path".into(), card.into()), ("depth".into(), "4".into()), ("offset".into(), "17".into()), ("limit".into(), "500".into())]),
        "Tree uses the owner's bound window and clamps depth and page size before querying the app");
    assert!(s.ben.relay.take(TWEAK).is_empty() && s.ana.relay.take(TWEAK).is_empty(), "owner-local tree reads send zero bytes");

    s.ana.send(Request::Props { path: card.into() });
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    let before = s.app.sent.len();
    let first = s.ana.send(search("ca"));
    let reveal = s.ana.send(Request::Tree(TreeQuery::Reveal { path: card.into(), limit: TREE_PAGE }));
    let second = s.ana.send(search("card"));
    s.exchange();
    assert!(has(&s.ana, first, |n| matches!(n, Notice::Refused(why) if why == "Superseded by a newer tree query.")),
        "the queued stale search is settled");
    assert!(!has(&s.ben, first, |_| true) && !has(&s.host, first, |_| true), "superseding a search is private");
    s.finish();
    s.one();
    s.one();
    assert!(has(&s.ana, reveal, tree_reply) && has(&s.ana, second, tree_reply), "search and reveal keep independent queue slots");
    assert!(matches!(&s.app.sent[before..], [(_, _, DesignOp::Design { verb: DesignVerb::Tree, args: r }),
        (_, _, DesignOp::Design { verb: DesignVerb::Tree, args: q })]
        if r.contains(&("reveal".into(), card.into())) && q.contains(&("query".into(), "card".into()))),
        "only the reveal and latest search reached the app in FIFO order");

    // A fresh stream accepts two bad deliveries exactly once, then acknowledges the following notice.
    let mut corrupt = Copies::new(OWNER, ANA, ITEM);
    let mut expected = Vec::new();
    for (i, tree) in ["{".into(), format!("[{}0{}]", "[".repeat(256), "]".repeat(256))].into_iter().enumerate() {
        let req = corrupt.ana.send(children());
        let notice = Notice::Tree { query: TreeQuery::Reveal { path: card.into(), limit: TREE_PAGE }, tree };
        let bytes = Packet { share: SHARE, sender: OWNER, agent: None, req, body: Body::Notice { seat: ANA, delivery: 1 + i as u64, notice } }.encode();
        let why = wire::decode(OWNER, &bytes).expect_err("standalone decode rejects a bad page");
        if i != 0 { assert_eq!(why, TOO_DEEP, "Tree shares the JSON stack guard"); }
        expected.push((ANA, req, None, Notice::Refused(why.into())));
        for _ in 0..2 { corrupt.host.relay.send(ANA, TWEAK, bytes.clone()).unwrap(); }
    }
    let req = corrupt.ana.send(Request::AskEdit);
    corrupt.host.relay.send(ANA, TWEAK, Packet { share: SHARE, sender: OWNER, agent: None, req,
        body: Body::Notice { seat: ANA, delivery: 3, notice: Notice::Asked } }.encode()).unwrap();
    expected.push((ANA, req, None, Notice::Asked));
    corrupt.ana.poll();
    assert_eq!(corrupt.ana.session.view.updates, expected, "bad pages each settle once and the next notice arrives");
    assert!(matches!(&corrupt.host.relay.take(TWEAK)[..], [(_, bytes)] if wire::decode(ANA, bytes).unwrap().expect("a known tweak packet").body == Body::Ack(3)),
        "the two refused requests left the retry queue and the next delivery was acknowledged");

    // Hello is independent of board revision. Drive the model and coordinator as Town does.
    let (_homes, pair) = crate::model::tests::group_town("tree-hello", 2);
    let [mut viewer, host]: [Model; 2] = pair.try_into().ok().unwrap();
    let peer = host.me();
    let town = viewer.open_id();
    viewer.pin_linked(&peer, VERSION, "Host").unwrap();
    viewer.link_proved(&peer, VERSION, "Host", None).unwrap();
    let mut hub = Tweaks::<u64, u64>::default();
    hub.sessions.push(Session::peer(town, SHARE, ITEM, viewer.me(), peer).unwrap());
    hub.resolve(&viewer);
    let revision = viewer.tweak_board_state(town).unwrap().1;
    assert!(viewer.peer_level(&peer).is_none() && !hub.session(SHARE).unwrap().supports_tree(), "a proved carrier awaits Hello");
    viewer.receive_from(peer, &Message::Hello(Hello { level: TREE_LEVEL, ..Default::default() }).encode()).unwrap();
    hub.resolve(&viewer);
    assert_eq!(viewer.tweak_board_state(town).unwrap().1, revision, "Hello did not change the board revision");
    assert!(hub.session(SHARE).unwrap().supports_tree(), "resolve propagates a late Hello on an unchanged board");

    // A terminal owner can still have unsent mail after its board has been unloaded.
    let mut draining = Copies::in_town(TOWN, viewer.me(), peer, ITEM);
    draining.allow(peer, true);
    draining.ana.send(children());
    draining.ana.poll();
    draining.host.poll();
    draining.host.session.drive_app(&mut draining.app);
    draining.app.answer(&mut draining.host.session);
    draining.host.session.end("Share closed");
    let mut ended = Tweaks::default();
    ended.sessions.push(draining.host.session);
    viewer.receive_from(peer, &Message::Hello(Hello { level: TREE_LEVEL - 1, ..Default::default() }).encode()).unwrap();
    assert!(viewer.tweak_board_state(TOWN).is_none(), "the draining mailbox belongs to an unloaded board");
    ended.resolve(&viewer);
    ended.relay(&mut draining.host.relay);
    let mail = draining.ana.relay.take(TWEAK);
    assert!(mail.iter().all(|(_, b)| wire::min_level(b) != TREE_LEVEL) && mail.iter().any(|(_, b)| b[1] == 17),
        "the older guest receives Ended and no retained Tree page");

    // Guest replay. Before Hello only Ready and Ack may precede the held Tree.
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    let keep = s.ana.send(children());
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).iter().any(|(_, b)| wire::min_level(b) == TREE_LEVEL), "the original request entered the lost transport");
    s.ana.session.peer_unlinked(OWNER);
    s.ana.session.set_peer_level(OWNER, None);
    let after = s.ana.send(Request::Select(Some(card.into())));
    s.ana.poll();
    let held = s.host.relay.take(TWEAK);
    assert!(held.iter().all(|(_, b)| matches!(b[1], 14 | 18)) && !has(&s.ana, keep, |_| true),
        "unknown sharer holds Tree and later Select without refusing either");
    s.ana.session.set_peer_level(OWNER, current);
    s.one();
    assert!(has(&s.ana, keep, tree_reply) && has(&s.ana, after, |n| matches!(n, Notice::Selected { .. })),
        "a current Hello releases the held request and subsequent Select");

    let drop_req = s.ana.send(search("card"));
    let ask = s.ana.send(Request::AskEdit);
    s.ana.poll();
    s.host.relay.take(TWEAK);
    s.ana.session.peer_unlinked(OWNER);
    s.ana.session.set_peer_level(OWNER, None);
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).iter().all(|(_, b)| matches!(b[1], 14 | 18)) && !has(&s.ana, drop_req, |_| true),
        "a missing Hello preserves the pending search and Ask");
    let waiting = s.ana.session.request(Some(ANA_AGENT), children()).expect("a new query queues while Hello is unknown");
    s.ana.session.set_peer_level(OWNER, older);
    assert!(s.ana.session.view.updates.contains(&(ANA, waiting, Some(ANA_AGENT), Notice::Refused(OLDER_SHARER_TREE.into()))),
        "purging a held query retains its requesting agent");
    assert!(s.ana.session.view.updates.contains(&(ANA, drop_req, None, Notice::Refused(OLDER_SHARER_TREE.into()))),
        "an older Hello immediately refuses the original request locally");
    s.ana.poll();
    let frames = s.host.relay.take(TWEAK);
    assert!(matches!(&frames[..], [(_, b)] if b[1] == 6 && wire::decode(ANA, b).unwrap().expect("a known tweak packet").req == ask),
        "the older sharer receives just the following Ask");
    for (_, bytes) in frames { s.ana.relay.send(OWNER, TWEAK, bytes).unwrap(); }
    s.exchange();
    assert!(s.host.session.view.asked.contains(&ANA), "the older sharer processes Ask after dropping Tree");

    // Owner replay, with an unacked page followed by Select and another read still in the app.
    s.ana.session.set_peer_level(OWNER, current);
    s.allow(ANA, true);
    let keep = s.ana.send(children());
    answer(&mut s);
    let after = s.host.send(Request::Select(Some(card.into())));
    let inflight = s.ana.send(children());
    // Dispatch the second request without delivering the first page to Ana.
    let lost = s.ana.relay.take(TWEAK);
    assert!(matches!(&lost[..], [(_, b)] if b[1] == 26), "the first private page was sent on the lost transport");
    assert_eq!(lost[0].1.len(), TREE.len() + 190, "a root page carries the v1 header, delivery, query and one length-prefixed JSON body");
    s.ana.poll();
    s.host.poll();
    s.host.session.drive_app(&mut s.app);
    s.ana.relay.take(TWEAK);
    s.host.session.peer_unlinked(ANA);
    s.host.session.set_peer_level(ANA, None);
    s.app.answer(&mut s.host.session);
    s.host.poll();
    assert!(s.ana.relay.take(TWEAK).is_empty(), "unknown guest holds the head page and every later delivery");
    s.host.session.set_peer_level(ANA, current);
    s.exchange();
    let order: Vec<_> = s.ana.session.view.updates.iter().filter_map(|(_, req, _, n)|
        ([keep, after, inflight].contains(req) && (tree_reply(n) || matches!(n, Notice::Selected { .. }))).then_some(*req)).collect();
    assert_eq!(order, [keep, after, inflight], "the current guest gets both pages in order, including the one completed before Hello");

    // An older guest skips the unacked Tree delivery number, then acks later notices normally.
    s.allow(ANA, true);
    s.ana.send(children());
    answer(&mut s);
    let after = s.host.send(Request::Select(None));
    s.host.poll();
    s.ana.relay.take(TWEAK);
    s.host.session.peer_unlinked(ANA);
    s.host.session.set_peer_level(ANA, None);
    s.host.poll();
    assert!(s.ana.relay.take(TWEAK).is_empty(), "the owner holds replay while awaiting the guest's Hello");
    s.host.session.set_peer_level(ANA, older);
    s.host.poll();
    let frames = s.ana.relay.take(TWEAK);
    assert!(!frames.is_empty() && frames.iter().all(|(_, b)| !matches!(b[1], 25 | 26)), "older guest receives no Tree tags during replay");
    assert!(frames.iter().any(|(_, b)| matches!(wire::decode(OWNER, b).unwrap().expect("a known tweak packet").body, Body::Notice { notice: Notice::Allowed(false), .. })),
        "replay includes the disconnected seat's revoked grant");
    for (_, bytes) in frames { s.host.relay.send(ANA, TWEAK, bytes).unwrap(); }
    s.ana.poll();
    s.host.poll();
    assert!(has(&s.ana, after, |n| matches!(n, Notice::Selected { path: None, rect: None })) && !s.ana.session.view.allowed.contains(&ANA),
        "later v1 notices cross the purged delivery number");
    s.host.session.peer_unlinked(ANA);
    s.host.poll();
    assert!(s.ana.relay.take(TWEAK).is_empty(), "Ack drained the mailbox through its final delivery");

    s.host.session.set_peer_level(ANA, current);
    s.allow(ANA, true);
    let queued = s.ana.send(children());
    s.exchange();
    let before = s.app.sent.len();
    s.host.session.set_peer_level(ANA, older);
    s.one();
    assert_eq!(s.app.sent.len(), before, "a known downgrade evicts queued Tree work before app dispatch");
    assert!(has(&s.ana, queued, |n| matches!(n, Notice::Refused(_))) && !has(&s.host, queued, |_| true) && !has(&s.ben, queued, |_| true),
        "only the submitting seat receives the v1 refusal for evicted work");
}

#[test]
fn theme_wire_broadcasts_records_edits_and_gates_older_peers_across_replay() {
    let get = || Request::Theme(ThemeOp::Get);
    let set_token = |name: &str, value: &str| Request::Theme(ThemeOp::Set { name: name.into(), value: value.into() });
    let tree = || Request::Tree(TreeQuery::Reveal { path: PATH.into(), limit: wire::TREE_PAGE });
    let snapshot = |n: &Notice| matches!(n, Notice::Theme { .. });
    let current = Some(THEME_LEVEL);
    let older = Some(THEME_LEVEL - 1);
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.session.set_peer_level(OWNER, older);
    for op in [get(), set_token("color_primary", "#ccc"), install("Light")] {
        assert_eq!(s.ana.session.request(None, op), Err(OLDER_SHARER_THEME.into()), "an older sharer refuses each theme operation locally");
    }
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).is_empty(), "no theme bytes reach the older sharer");
    s.allow(ANA, true);
    let page = s.ana.send(tree());
    s.one();
    assert!(has(&s.ana, page, |n| matches!(n, Notice::Tree { .. })), "level two still serves the widget tree");
    s.ana.session.set_peer_level(OWNER, current);

    let sent = s.app.sent.len();
    for op in [get(), set_token("color_primary", "#ccc"), install("Light")] {
        let id = s.ben.send(op);
        s.one();
        assert!(has(&s.ben, id, |n| matches!(n, Notice::Refused(s) if s == "Ask the owner to allow editing this share.")),
            "each theme operation requires Allow");
        assert!(!has(&s.ana, id, |_| true) && !has(&s.host, id, |_| true), "a denial is private");
    }
    assert!(s.app.sent.len() == sent && s.records.is_empty(), "denials leave the app and signed history alone");
    let read = s.ana.send(get());
    s.one();
    assert!(has(&s.ana, read, snapshot) && !has(&s.ben, read, |_| true) && !has(&s.host, read, |_| true), "Get is unicast");
    let note = s.ana.send(set_token("color_primary", "mix(#f00, #00f, 0.5)"));
    s.one();
    assert_eq!(s.records[0].change, Change::Note("mix(#f00, #00f, 0.5)".into()), "a refused token expression is saved verbatim");
    assert!(has(&s.ana, note, |n| matches!(n, Notice::Refused(s) if s.contains("theme value"))) && !has(&s.ben, note, |_| true),
        "the note refusal settles only the actor");
    s.list();
    let missing = s.ana.send(set("theme.nonexistent"));
    s.one();
    assert!(has(&s.ana, missing, |n| matches!(n, Notice::Refused(_))) &&
        s.records.last().unwrap().change == Change::Note("theme.nonexistent".into()), "failed typed references remain notes");

    let alias = s.ana.send(set_token("color_bg_app", "theme.color_primary"));
    s.one();
    assert_eq!(s.records.last().unwrap().change, Change::Applied { old: "#1e1e1eff".into(), new: "#e6a294ff".into(), seq: 1 },
        "a token alias records the resolved scalar");
    assert!(has(&s.ben, alias, |n| matches!(n, Notice::Applied { path, prop, .. } if path == "theme" && prop == "color_bg_app")),
        "token edits reach observers");
    let after_alias = s.app.sent.len();
    s.one();
    assert_eq!(s.app.sent.len(), after_alias, "ordinary token edits need no snapshot read");
    s.ana.send(set_token("space_factor", "12"));
    s.one();
    s.one();
    assert!(has(&s.ben, 0, snapshot), "a changed global refreshes every capable seat");
    s.ana.send(set_token("space_factor", "12"));
    s.one();
    let unchanged = s.app.sent.len();
    s.one();
    assert_eq!(s.app.sent.len(), unchanged, "an unchanged global schedules no refresh");
    s.host.session.set_peer_level(BEN, older);
    let before = s.app.sent.len();
    let records = s.records.len();
    let first = s.ana.send(install("First"));
    let last = s.ana.send(install("Light"));
    s.exchange();
    assert!(has(&s.ana, first, |n| matches!(n, Notice::Refused(s) if s == "Superseded by a newer theme install.")),
        "a queued install is superseded for its actor");
    s.host.session.drive_app(&mut s.app);
    s.app.answer(&mut s.host.session);
    if let Some(Action::RecordAppEdit { edit, .. }) = s.host.session.next_record() {
        s.records.push(edit);
        s.host.session.record_saved();
    }
    s.host.session.consent(ANA, false).unwrap();
    s.host.session.drive_app(&mut s.app);
    s.app.answer(&mut s.host.session);
    s.host.poll();
    let old_mail = s.ben.relay.take(TWEAK);
    assert!(old_mail.iter().all(|(_, bytes)| wire::min_level(bytes) == 0), "the older guest receives only v1 notices");
    for (_, bytes) in old_mail { s.host.relay.send(BEN, TWEAK, bytes).unwrap(); }
    s.exchange();
    assert!(has(&s.ben, last, |n| matches!(n, Notice::Applied { prop, .. } if prop == "install")), "v1 observers receive install history");
    let edits = &s.records[records..];
    assert!(matches!(edits, [AppEdit { path, prop, change: Change::Applied { old, new, .. }, .. }] if
        path == "theme" && prop == "install" && old == "Dark" && new == "Light"), "only the winning install is recorded");
    let light = |n: &Notice| matches!(n, Notice::Theme { theme } if theme.contains(r#""base":"light""#));
    assert!(has(&s.host, 0, light) && has(&s.ana, 0, light) && !has(&s.ben, 0, light),
        "the internal post-install read survives revocation and reaches capable seats");
    assert!(matches!(&s.app.sent[before..], [(_, _, DesignOp::Design { verb: DesignVerb::Theme, args: a }),
        (_, _, DesignOp::Design { verb: DesignVerb::Theme, args: b })] if
        a[0] == ("install".into(), "1".into()) && parse_json(&a[1].1) == parse_json(
        r##"{"base":"light","sheet":null,"globals":[["space_factor",12.0]],"overrides":[["color_primary","#ff5c39ff"]]}"##) &&
        b == &[("get".into(), "1".into())]), "one typed install omits the local label and precedes one post-reload Get");
    s.host.session.peer_unlinked(BEN);
    s.host.poll();
    assert!(s.ben.relay.take(TWEAK).is_empty(), "the older guest acknowledged all its v1 mail");
    s.host.session.set_members([OWNER, ANA]);
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.host.session.set_peer_level(BEN, current);
    s.exchange();
    assert!(has(&s.ben, 0, light), "a late member receives the latest cached theme");

    // A cached snapshot includes ordinary edits made since the last Get.
    s.host.send(set_token("color_primary", "#010203ff"));
    s.one();
    s.host.session.set_members([OWNER, ANA]);
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.host.session.set_peer_level(BEN, current);
    s.exchange();
    assert!(has(&s.ben, 0, |n| matches!(n, Notice::Theme { theme } if theme.contains("#010203ff"))),
        "a late seat sees the most recent scalar without querying the app again");

    let mut mixed = Copies::new(OWNER, ANA, ITEM);
    mixed.ana.session.set_peer_level(OWNER, None);
    let t = mixed.ana.send(tree());
    let h = mixed.ana.session.request(Some(ANA_AGENT), get()).unwrap();
    mixed.ana.session.set_peer_level(OWNER, Some(TREE_LEVEL - 1));
    assert!(has(&mixed.ana, t, |n| n == &Notice::Refused(OLDER_SHARER_TREE.into())) &&
        mixed.ana.session.view.updates.contains(&(ANA, h, Some(ANA_AGENT), Notice::Refused(OLDER_SHARER_THEME.into()))),
        "a mixed downgrade settles each held request with its own reason and actor");

    // Replay both directions with the old carrier's unacknowledged frame lost.
    for level in [current, older] {
        let mut r = Copies::new(OWNER, ANA, ITEM);
        r.allow(ANA, true);
        let req = r.ana.send(get());
        r.ana.poll();
        assert!(r.host.relay.take(TWEAK).iter().any(|(_, b)| b[1] == 27), "original Get was in the lost transport");
        r.ana.session.peer_unlinked(OWNER);
        r.ana.session.set_peer_level(OWNER, None);
        let ask = r.ana.send(Request::AskEdit);
        r.ana.poll();
        assert!(r.host.relay.take(TWEAK).iter().all(|(_, b)| matches!(b[1], 14 | 18)), "awaiting Hello holds Get and later Ask in order");
        r.ana.session.set_peer_level(OWNER, level);
        r.one();
        assert!(has(&r.ana, ask, |n| *n == Notice::Asked), "later v1 mail proceeds after Hello");
        if level == current {
            assert!(has(&r.ana, req, snapshot), "a capable sharer receives the replayed Get");
        } else {
            assert!(has(&r.ana, req, |n| n == &Notice::Refused(OLDER_SHARER_THEME.into())) && r.app.sent.is_empty(),
                "downgrading settles Get locally before sending Ask");
        }

        let mut r = Copies::new(OWNER, ANA, ITEM);
        r.allow(ANA, true);
        let req = r.ana.send(get());
        r.exchange();
        r.host.session.drive_app(&mut r.app);
        r.app.answer(&mut r.host.session);
        r.host.poll();
        assert!(r.ana.relay.take(TWEAK).iter().any(|(_, b)| b[1] == 28), "original snapshot was in the lost transport");
        r.host.session.peer_unlinked(ANA);
        r.host.session.set_peer_level(ANA, None);
        r.host.poll();
        assert!(r.ana.relay.take(TWEAK).is_empty(), "the owner holds snapshot and subsequent revocation while awaiting Hello");
        r.host.session.set_peer_level(ANA, level);
        r.exchange();
        assert_eq!(has(&r.ana, req, snapshot), level == current, "only a capable guest receives a retained snapshot");
        assert!(!r.ana.session.view.allowed.contains(&ANA), "the later revocation reaches either level");
    }

    let mut r = Copies::new(OWNER, ANA, ITEM);
    let mut expected = VecDeque::new();
    for (i, theme) in ["{}".into(), format!("{}0{}", "[".repeat(257), "]".repeat(257))].into_iter().enumerate() {
        let req = r.ana.send(get());
        let bytes = Packet { share: SHARE, sender: OWNER, req, agent: None, body: Body::Notice {
            seat: ANA, delivery: i as u64 + 1, notice: Notice::Theme { theme } } }.encode();
        let why = wire::decode(OWNER, &bytes).expect_err("bad snapshots fail standalone validation");
        if i == 1 { assert_eq!(why, TOO_DEEP, "theme JSON shares the depth guard"); }
        expected.push_back((ANA, req, None, Notice::Refused(why.into())));
        for _ in 0..2 { r.host.relay.send(ANA, TWEAK, bytes.clone()).unwrap(); }
    }
    r.ana.poll();
    assert_eq!(r.ana.session.view.updates, expected, "each malformed delivery settles exactly once across replay");
    assert!(matches!(&r.host.relay.take(TWEAK)[..], [(_, b)] if wire::decode(ANA, b).unwrap().unwrap().body == Body::Ack(2)),
        "refused snapshots acknowledge the final delivery and remove both requests");
}

fn panel_test_cx() -> test_cx::PooledCx {
    test_cx::checkout_test_cx(|vm| {
        makepad_widgets::script_mod(vm);
        crate::theme::script_mod(vm);
        crate::lists::script_mod(vm);
        crate::ui::navigation::script_mod(vm);
        crate::ui::tasks::script_mod(vm);
        panel::script_mod(vm);
    })
}

fn draw_panel(cx: &mut Cx, root: WidgetRef) -> impl FnMut(&mut Cx) {
    use makepad_widgets::*;
    let size = dvec2(540., 880.);
    let pass = DrawPass::new(cx);
    pass.set_size(cx, size);
    let mut draw_list = DrawList2d::new(cx);
    let overlay = cx.with_vm(|vm| Overlay::script_new(vm));
    move |cx: &mut Cx| {
        let ev = DrawEvent::default();
        let mut draw = CxDraw::new(cx, &ev);
        let mut frame = Cx2d::new(&mut draw);
        frame.begin_pass(&pass, None);
        draw_list.begin_always(&mut frame);
        overlay.begin(&mut frame);
        frame.begin_root_turtle(size, Layout::flow_down());
        root.draw_all(&mut frame, &mut Scope::empty());
        frame.end_pass_sized_turtle();
        overlay.end(&mut frame);
        draw_list.end(&mut frame);
        frame.end_pass(&pass);
    }
}

fn panel_click_at(cx: &mut Cx, root: &WidgetRef, abs: Vec2d, time: f64) -> (Event, ActionsBuf, ActionsBuf) {
    use makepad_widgets::{MouseButton, MouseDownEvent, MouseUpEvent, Scope};
    let window_id = makepad_widgets::WindowId(1, 1);
    let button = MouseButton::PRIMARY;
    cx.fingers.first_mouse_button = Some((button, window_id));
    let press = cx.capture_actions(|cx| root.handle_event(cx, &Event::MouseDown(MouseDownEvent {
        abs, button, window_id, modifiers: Default::default(), handled: Default::default(), time,
    }), &mut Scope::empty()));
    // The platform delivers the focus taken on MouseDown before dispatching MouseUp.
    cx.action(());
    cx.handle_actions();
    cx.fingers.first_mouse_button = None;
    let up = Event::MouseUp(MouseUpEvent { abs, button, window_id, modifiers: Default::default(), time: time + 0.05 });
    let release = cx.capture_actions(|cx| root.handle_event(cx, &up, &mut Scope::empty()));
    // This harness dispatches directly to the widget; retire the synthetic
    // mouse capture the platform would release at the end of MouseUp.
    cx.fingers = Default::default();
    // The captured actions are handed to the panel below. Also deliver the pending focus change.
    cx.action(());
    cx.handle_actions();
    (up, press, release)
}

#[test]
fn color_row_shows_theme_chip_and_palette_and_binds_by_reference() {
    use makepad_widgets::*;
    const COLORS: &str = r##"{"props":[{"name":"draw_bg.color","value":"#e6a294ff"},
        {"name":"draw_bg.border_color","value":"#123456ff"}]}"##;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 310} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut draw = draw_panel(cx, root.clone());
    let mut panel = panel::Panel::default();
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.app.props = COLORS.into();
    s.app.prop_values = [("draw_bg.color".into(), "#e6a294ff".into()), ("draw_bg.border_color".into(), "#123456ff".into())].into();
    let mut state = makepad_town_core::fold::State::default();
    state.profiles.insert(ANA, makepad_town_core::fold::Profile { name: "Peer".into(), colour: 0 });
    let dispatch = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp| {
        while let Some((seat, req, agent, notice)) = town.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut town.session, seat, req, agent, &state, &notice);
        }
    };
    let list = root.portal_list(cx, ids!(tweak_properties_list));
    let row = |cx: &Cx, name: &str| (0..list.borrow().unwrap().range_end()).filter_map(|i| list.get_item(i))
        .find(|(t, r)| *t == id!(ColorRow) && r.label(cx, ids!(name)).text() == name).expect("mounted colour property").1;
    let history = root.portal_list(cx, ids!(tweak_history_list));
    let entry = |cx: &Cx, i| history.get_item(i).expect("mounted edit history row").1.label(cx, ids!(tweak_entry)).text();
    let click = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp, target: WidgetRef, time| {
        let (_, press, release) = panel_click_at(cx, &root, target.area().rect(cx).center(), time);
        panel.actions(cx, &root, &press, &mut town.session);
        panel.actions(cx, &root, &release, &mut town.session);
    };

    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    panel.restore_selection(cx, &root, &mut s.ana.session);
    s.allow(ANA, true);
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    assert!(matches!(&s.app.sent[..], [(_, _, DesignOp::Design { verb: DesignVerb::Theme, args })] if args == &[("get".into(), "1".into())]),
        "the grant fetches the theme exactly once");
    s.ana.send(Request::Select(Some(PATH.into())));
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    let color = row(cx, "draw_bg.color");
    let border = row(cx, "draw_bg.border_color");
    let chip = color.widget(cx, ids!(tname));
    assert!(color.widget(cx, ids!(tname_wrap)).visible() && chip.text() == "≈ theme.color_primary", "matching RGBA shows the named chip");
    assert!(!border.widget(cx, ids!(tname_wrap)).visible(), "an unmatched colour has no chip");
    let applied = |seq: u64, prop: &str, old: &str, new: &str| Notice::Applied {
        path: PATH.into(), prop: prop.into(), old: old.into(), new: new.into(), seq };
    let typed = |op: &DesignOp, prop: &str, value: &str| matches!(op, DesignOp::Design { verb: DesignVerb::Typed, args }
        if args == &[("path".into(), PATH.into()), ("prop".into(), prop.into()), ("value".into(), value.into())]);
    click(cx, &mut panel, &mut s.ana, chip, 1.);
    s.one();
    assert!(typed(&s.app.sent.last().unwrap().2, "draw_bg.color", "theme.color_primary"), "clicking the chip sends the reference via design typed");
    assert_eq!(s.app.prop_values["draw_bg.color"], "#e6a294ff", "the live app resolves the reference to its current scalar");
    let original = Change::Applied { old: "#e6a294ff".into(), new: "theme.color_primary".into(), seq: 1 };
    assert_eq!(s.records.as_slice(), &[AppEdit { seat: ANA, agent: None, path: PATH.into(), prop: "draw_bg.color".into(), change: original.clone() }],
        "the signed edit preserves the reference even when the reflected hex did not change");
    for seat in [&s.host, &s.ana] {
        assert!(seat.session.view.updates.iter().any(|(_, _, _, n)| n == &applied(1, "draw_bg.color", "#e6a294ff", "theme.color_primary")),
            "owner and guest see the same named edit");
    }
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    let first_line = "1  Peer  ui.panel.draw_bg.color  #e6a294ff → theme.color_primary";
    assert_eq!(entry(cx, 0), first_line, "history renders the chip's reference");

    let swatch = border.widget(cx, ids!(swatch));
    click(cx, &mut panel, &mut s.ana, swatch.clone(), 2.);
    draw(cx);
    {
        let pick = swatch.borrow::<fab_controls::FabColorPick>().unwrap();
        assert!(pick.is_open(), "the mounted swatch opens its popover");
        assert_eq!(pick.channel_row(id!(palette_name)).text(), "theme colours (2)", "opening supplies the two theme colour names");
        assert!(pick.channel_row(id!(palette)).borrow::<fab_controls::FabPaletteStrip>().unwrap().height_for(228.) > 0.,
            "the populated strip occupies space in the popover");
    }
    let choose = cx.capture_actions(|cx| cx.group_widget_actions(list.widget_uid(), border.widget_uid(),
        |cx| cx.widget_action(swatch.widget_uid(), fab_controls::FabColorPickAction::PalettePick("color_bg_app".into()))));
    panel.actions(cx, &root, &choose, &mut s.ana.session);
    s.one();
    assert!(typed(&s.app.sent.last().unwrap().2, "draw_bg.border_color", "theme.color_bg_app"), "the palette sends the chosen token");
    assert_eq!(s.app.prop_values["draw_bg.border_color"], "#1e1e1eff", "the app stores the resolved palette colour");
    assert_eq!(s.records.last(), Some(&AppEdit { seat: ANA, agent: None, path: PATH.into(), prop: "draw_bg.border_color".into(),
        change: Change::Applied { old: "#123456ff".into(), new: "theme.color_bg_app".into(), seq: 2 } }), "the second edit keeps its reference");
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(entry(cx, 1), "2  Peer  ui.panel.draw_bg.border_color  #123456ff → theme.color_bg_app", "palette history names its token");
    swatch.borrow_mut::<fab_controls::FabColorPick>().unwrap().close_popover(cx, false);
    s.host.send(Request::Theme(ThemeOp::Set { name: "color_primary".into(), value: "#00aa00ff".into() }));
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert!(!color.widget(cx, ids!(tname_wrap)).visible(), "the token changed while the live widget kept its scalar");
    assert!(s.records[0].change == original && entry(cx, 0) == first_line, "a later token edit preserves the earlier reference");

    for initial in [Some(THEME_LEVEL - 1), None] {
        let mut old = Copies::new(OWNER, ANA, ITEM);
        old.app.props = COLORS.into();
        old.allow(ANA, true);
        old.ana.session.view.updates.clear();
        old.ana.session.set_peer_level(OWNER, initial);
        let mut panel = panel::Panel::default();
        panel.refresh(cx, &root, &old.ana.session, &state, &Default::default());
        panel.restore_selection(cx, &root, &mut old.ana.session);
        old.ana.session.set_peer_level(OWNER, Some(THEME_LEVEL - 1));
        old.ana.send(Request::Select(Some(PATH.into())));
        old.one();
        dispatch(cx, &mut panel, &mut old.ana);
        old.one();
        dispatch(cx, &mut panel, &mut old.ana);
        draw(cx);
        assert!(!row(cx, "draw_bg.color").widget(cx, ids!(tname_wrap)).visible(), "the older share still shows usable Props without theme chips");
        assert_eq!(root.label(cx, ids!(tweak_result)).text(), "", "both synchronous and delayed compatibility refusals stay silent");
        assert!(matches!(&old.app.sent[..], [(_, _, DesignOp::Design { verb: DesignVerb::Props, .. })]), "only Props reaches the older app");
    }
}

#[test]
fn panel_pick_rows_enter_and_collaborator_history() {
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let ui = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* View{
            tweak_panel := AppTweakPanel{}
            View{path := View{} name := View{} list := View{} status := View{}}
        } });
        WidgetRef::script_from_value(vm, value)
    });
    cx.widget_tree().check_lookup_clashes(None);
    let root = ui.widget(cx, ids!(tweak_panel));
    use crate::model::{Action as ModelAction, Origin};
    let home = crate::model::tests::ShareHomes::new("tweak-panel");
    let mut model = Model::open(&home.0).unwrap().0;
    let owner = model.me();
    let board = model.act(ModelAction::AddBoard {
        project: model.current_project().unwrap().id, title: "Board".into(),
    }, Origin::Ui).unwrap();
    let item = model.act(ModelAction::AddApp {
        town: model.open_id(), board: Some(board), geometry: [0, 0, 6400, 6400], session: SHARE, name: "Shared UI".into(),
    }, Origin::Ui).unwrap();
    let mut s = Copies::new(owner, ANA, item);
    s.allow(ANA, true);
    s.ana.session.view.updates.clear();
    let mut panel = panel::Panel::default();
    panel.refresh(cx, &root, &s.ana.session, &Default::default(), &Default::default());
    s.app.props = r##"{"props":[
        {"name":"surfaceColor","value":"#445566"},
        {"name":"runtimeOverride","value":"15","origin":"runtime","level":0},
        {"name":"flow","value":"Down"},{"name":"distribute","value":"Distribute.SpaceBetween"},
        {"name":"align.x","value":"0"},{"name":"align.y","value":"0"},
        {"name":"spacing","value":"2","origin":"inherited","level":2},
        {"name":"padding","value":"8","origin":"authored","level":0},
        {"name":"height","value":"Fit","origin":"authored","level":0}]}"##.into();
    s.ana.send(Request::Pick(At::Point { window: 2, x: 4., y: 8. }));
    s.one();
    let mut state = makepad_town_core::fold::State::default();
    for (seat, name) in [(owner, "Owner"), (ANA, "Peer")] {
        state.profiles.insert(seat, makepad_town_core::fold::Profile { name: name.into(), colour: 0x446688 });
    }
    let dispatch = |panel: &mut panel::Panel, town: &mut TownApp, cx: &mut Cx| {
        while let Some((seat, req, agent, n)) = town.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut town.session, seat, req, agent, &state, &n);
        }
    };
    dispatch(&mut panel, &mut s.ana, cx);
    s.one();
    dispatch(&mut panel, &mut s.ana, cx);
    s.one();
    assert_eq!(s.app.sent.len(), 2, "one local click produces exactly one follow-up");
    assert!(matches!(&s.app.sent[1].2, DesignOp::Design { verb: DesignVerb::Props, args } if args == &[("path".into(), PATH.into())]),
        "the follow-up queries the picked widget");
    let mut draw = draw_panel(cx, root.clone());
    draw(cx);
    let props = root.portal_list(cx, ids!(tweak_properties_list));
    let item_of = |template| (0..props.borrow().unwrap().range_end()).filter_map(|i| props.get_item(i))
        .find(|(id, _)| *id == template).unwrap_or_else(|| panic!("missing mounted {template} template")).1;
    assert!(props.borrow().is_some(), "the Props portal template is installed");
    assert_eq!(item_of(id!(MeasuredRow)).label(cx, ids!(measured)).text(),
        "View · measured 12 × 16 px · padding 8", "the measurement combines the picked rect with the selected padding");
    let identity = item_of(id!(IdentityRow));
    assert_eq!(identity.text_input(cx, ids!(name_field)).text(), "panel", "identity shows the picked widget name");
    assert!(identity.text_input(cx, ids!(name_field)).is_read_only(), "identity is read-only");
    let row = item_of(id!(NumRow));
    assert_eq!(row.label(cx, ids!(name)).text(), "runtimeOverride", "the ordinary number follows the Layout composites");
    assert_eq!(row.label(cx, ids!(origin)).text(), "●", "the row shows its cascade level dot");
    let click_at = |cx: &mut Cx, abs, time| panel_click_at(cx, &root, abs, time);
    let click = |cx: &mut Cx, widget: WidgetRef| {
        let abs = widget.area().rect(cx).center();
        let (up, _, release) = click_at(cx, abs, 1.0);
        (up, release)
    };
    let header = || item_of(id!(SectionRow));
    let (_, fold) = click(cx, header().widget(cx, ids!(header)));
    panel.actions(cx, &root, &fold, &mut s.ana.session);
    draw(cx);
    assert_eq!(props.get_item(2).unwrap().1.label(cx, ids!(title)).text(), "Other", "folding Layout hides its fields");
    let (_, unfold) = click(cx, header().widget(cx, ids!(header)));
    panel.actions(cx, &root, &unfold, &mut s.ana.session);
    draw(cx);
    let row = item_of(id!(NumRow));
    let field = row.widget(cx, ids!(value));
    let number = || field.borrow::<fab_controls::FabValueInput>().expect("runtimeOverride uses the Fab number field");
    assert_eq!(number().value(), 15.0, "the runtimeOverride scrubber starts with the reflected value");
    let _ = click(cx, field.clone());
    assert!(number().is_editing(), "clicking the number opens its text editor");
    draw(cx);
    assert_ne!(cx.key_focus(), Area::Empty, "the runtimeOverride number field takes keyboard focus");
    root.handle_event(cx, &Event::TextInput(TextInputEvent { input: "20".into(), ..Default::default() }), &mut Scope::empty());
    let enter = cx.capture_actions(|cx| root.handle_event(cx,
        &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }), &mut Scope::empty()));
    panel.actions(cx, &root, &enter, &mut s.ana.session);
    s.one();
    assert_eq!(s.app.padding, "20", "Enter sends the chosen literal through the peer's consent path");
    s.host.send(Request::Set { path: PATH.into(), prop: "runtimeOverride".into(), value: "30".into() });
    s.one();
    // Snapshot reconciliation or delayed UI dispatch can present an older seq after a newer one.
    s.ana.session.view.updates.make_contiguous().reverse();
    dispatch(&mut panel, &mut s.ana, cx);
    draw(cx);
    let edits = root.portal_list(cx, ids!(tweak_history_list));
    let text = |cx: &Cx, i| edits.get_item(i).unwrap().1.label(cx, ids!(tweak_entry)).text();
    assert_eq!(text(cx, 0), "1  Peer  ui.panel.runtimeOverride  0 → 20", "peer edit retains its position and attribution");
    assert_eq!(text(cx, 1), "2  Owner  ui.panel.runtimeOverride  20 → 30", "owner edit follows the peer in app sequence order");
    let sent = s.app.sent.len();
    s.one();
    s.one();
    assert!(matches!(&s.app.sent[sent..], [(_, None, DesignOp::Design { verb: DesignVerb::Props, .. })]),
        "the local end refreshes once, including the owner edit received while it was outstanding");
    dispatch(&mut panel, &mut s.ana, cx);
    draw(cx);
    s.app.refused_values = vec!["120".into()];
    let text_row = item_of(id!(SizeRow));
    let value = text_row.text_input(cx, ids!(h_input));
    let _ = click(cx, text_row.widget(cx, ids!(h_input)));
    assert!(value.submit_on_enter(), "the literal editor still submits on Enter");
    assert_eq!(cx.key_focus(), value.area(), "the literal editor has keyboard focus before typing");
    value.set_text(cx, "120");
    let enter = cx.capture_actions(|cx| root.handle_event(cx,
        &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }), &mut Scope::empty()));
    panel.actions(cx, &root, &enter, &mut s.ana.session);
    s.one();
    dispatch(&mut panel, &mut s.ana, cx);
    draw(cx);
    assert!(text_row.label(cx, ids!(refusal)).text().contains("did not apply"), "the sharer's refusal appears on the submitted row");
    for edit in &s.records {
        let id = model.act(ModelAction::RecordAppEdit { town: model.open_id(), item, edit: edit.clone() }, Origin::Ui).unwrap();
        panel.sync_history(cx, &root, &s.ana.session, &model, Some(id));
    }
    panel.sync_history(cx, &root, &s.ana.session, &model, None);
    draw(cx);
    assert!(edits.get_item(2).unwrap().1.label(cx, ids!(tweak_entry)).text().starts_with("Note  ")
        && edits.get_item(2).unwrap().1.label(cx, ids!(tweak_entry)).text().ends_with("ui.panel.height  120"),
        "signed history reconciles the two live edits and retains the implementer's note");

    let (_, fold) = click(cx, header().widget(cx, ids!(header)));
    panel.actions(cx, &root, &fold, &mut s.ana.session);
    draw(cx);
    let color = item_of(id!(ColorRow));
    let hex_field = color.text_input(cx, ids!(value));
    let swatch = color.widget(cx, ids!(swatch));
    let pick = || swatch.borrow::<fab_controls::FabColorPick>().expect("surfaceColor has a Fab swatch");
    assert_eq!(hex_field.text(), "#445566", "surfaceColor binds its hex field");
    assert_eq!(fab_controls::format_hex(pick().rgba(), true), "#445566ff", "surfaceColor binds its swatch");
    assert!(!color.widget(cx, ids!(tname_wrap)).visible(), "theme matching is hidden for the shared colour");
    let _ = click(cx, color.widget(cx, ids!(value)));
    assert_eq!(cx.key_focus(), hex_field.area(), "the mounted hex field receives keyboard input");
    hex_field.set_text(cx, "invalid");
    let sent = s.app.sent.len();
    let enter = cx.capture_actions(|cx| root.handle_event(cx,
        &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }), &mut Scope::empty()));
    panel.actions(cx, &root, &enter, &mut s.ana.session);
    s.one();
    assert_eq!(hex_field.text(), "#445566", "invalid hex reverts to the shared colour");
    assert_eq!(s.app.sent.len(), sent, "invalid hex sends no Set");

    let records_before = s.records.len();
    for (prop, row, field, first, last, history_index) in [
        ("runtimeOverride", item_of(id!(NumRow)), id!(value), "40", "60", 2),
        ("surfaceColor", color, id!(swatch), "#ff0000ff", "#0000ff80", 3),
    ] {
        let uid = row.child(id!(editor)).child(field).widget_uid();
        let old = s.app.padding.clone();
        let sent = s.app.sent.len();
        if prop == "surfaceColor" {
            let (_, press, release) = click_at(cx, swatch.area().rect(cx).center(), 4.0);
            panel.actions(cx, &root, &press, &mut s.ana.session);
            panel.actions(cx, &root, &release, &mut s.ana.session);
            assert!(pick().is_open(), "pressing the colour swatch opens its picker");
            let eyedropper = pick().channel_row(id!(hex_row)).child(id!(pick));
            assert!(!eyedropper.is_empty() && !eyedropper.visible(), "the shared picker hides its eyedropper");
        }
        // Keep each widget action in its own dispatch, holding the first reply
        // at the guest until all four batches have gone through the panel.
        for n in 0..4 {
            let actions = cx.capture_actions(|cx| cx.group_widget_actions(props.widget_uid(), row.widget_uid(), |cx| {
                if prop == "surfaceColor" {
                    use fab_controls::FabColorPickAction::{Changed, Ended};
                    let v = [vec4(1., 0., 0., 1.), vec4(0., 1., 0., 1.), vec4(0., 0., 1., 0.5)][n.min(2)];
                    swatch.borrow_mut::<fab_controls::FabColorPick>().unwrap().set_rgba(cx, [v.x, v.y, v.z, v.w]);
                    cx.widget_action(uid, if n == 3 { Ended(v) } else { Changed(v) });
                } else {
                    use fab_controls::FabValueInputAction::{Changed, Ended};
                    cx.widget_action(uid, if n == 3 { Ended(60.) } else { Changed(40. + n as f64 * 10.) });
                }
            }));
            panel.actions(cx, &root, &actions, &mut s.ana.session);
            s.one();
        }
        let typed = |args: &Vec<(String, String)>, value: &str| args == &[
            ("path".into(), PATH.into()), ("prop".into(), prop.into()), ("value".into(), value.into())];
        assert!(matches!(&s.app.sent[sent..], [(_, _, DesignOp::Design { verb: DesignVerb::Typed, args })]
            if typed(args, first)), "{prop} sends just its first value while its reply is held");
        dispatch(&mut panel, &mut s.ana, cx);
        draw(cx);
        if prop == "surfaceColor" {
            assert_eq!(fab_controls::format_hex(pick().rgba(), true), last, "a redraw on the first reply preserves the open picker's value");
        }
        s.one();
        assert!(matches!(&s.app.sent[sent + 1..], [(_, _, DesignOp::Design { verb: DesignVerb::Typed, args })]
            if typed(args, last)), "{prop} sends its final value on reply without an intermediate Props read");
        dispatch(&mut panel, &mut s.ana, cx);
        s.one();
        dispatch(&mut panel, &mut s.ana, cx);
        s.one();
        assert!(matches!(&s.app.sent[sent + 2..], [(_, _, DesignOp::Design { verb: DesignVerb::Props, args })]
            if args == &[("path".into(), PATH.into())]), "{prop} refreshes the selected path exactly once at the end");
        draw(cx);
        assert!(text(cx, history_index).ends_with(&format!("{PATH}.{prop}  {old} → {last} (2 steps)")),
            "{prop} history retains its original value, final value and two applied steps");
    }
    // The synthetic channel actions above leave the picker open.
    swatch.borrow_mut::<fab_controls::FabColorPick>().unwrap().close_popover(cx, false);
    for edit in s.records[records_before..].iter().rev() {
        let id = model.act(ModelAction::RecordAppEdit { town: model.open_id(), item, edit: edit.clone() }, Origin::Ui).unwrap();
        panel.sync_history(cx, &root, &s.ana.session, &model, Some(id));
        draw(cx);
        assert_eq!(edits.borrow().unwrap().range_end(), 5, "a signed step inside either live drag keeps its history line");
    }
    panel.sync_history(cx, &root, &s.ana.session, &model, None);
    draw(cx);
    assert!(text(cx, 2).ends_with("runtimeOverride  30 → 60 (2 steps)") && text(cx, 3).ends_with("surfaceColor  60 → #0000ff80 (2 steps)"),
        "a full signed-history rebuild preserves both gesture endpoints and counts");

    let (_, unfold) = click(cx, header().widget(cx, ids!(header)));
    panel.actions(cx, &root, &unfold, &mut s.ana.session);
    draw(cx);
    let before = s.app.sent.len();
    s.ana.send(Request::Select(Some(PATH.into())));
    s.host.send(Request::Select(Some("other.widget".into())));
    s.one();
    dispatch(&mut panel, &mut s.ana, cx);
    s.one();
    assert!(matches!(&s.app.sent[before..], [(_, None, DesignOp::Design { verb: DesignVerb::Props, args })]
        if args == &[("path".into(), PATH.into())]), "each panel reads only its own seat's Selected path");

    // Open a saved history containing a signed edit from before agent ids were recorded.
    model.set_person_name("Ana").unwrap();
    let legacy_id = {
        use makepad_town_core::{entity::{field, kind}, event::{Actor, Content, Draft, Event, Key, Value, Via}, log::Log};
        let legacy = Content::Entity { kind: Key::Number(kind::APP_EDIT), project: model.state.entities[&item].project,
            derive: None, fields: vec![(field::PARENT, Value::Id(item)), (field::EDIT_SEAT, Value::Key(owner.to_vec())),
                (field::EDIT_AI, Value::Bool(true)), (field::LABEL, Value::Text("note".into())),
                (field::WIDGET_PATH, Value::Text(PATH.into())), (field::PROPERTY, Value::Text("padding".into())),
                (field::TEXT, Value::Text("Check spacing".into()))] };
        let key = model.device_key();
        let path = model.home.town_log_path(&model.open_id());
        drop(model);
        let mut log = Log::open(&path).unwrap().0;
        let draft = Draft { kind: legacy.kind(), kind_version: legacy.version(), actor: Actor::Human, via: Via::Ui,
            critical: false, wall_ms: 0, parents: log.parents_for(&owner), content: &legacy.encode() };
        let event = Event::create(&key, log.town_id(), draft, [0; 16]).unwrap();
        let id = event.id;
        log.append(event).unwrap();
        id
    };
    let model = Model::open(&home.0).unwrap().0;
    let history = parse_json(&model.app_edits_json(item, None).unwrap()).unwrap();
    let rows = history.get("edits").unwrap().as_arr().unwrap();
    let id = json::s(hex(&legacy_id));
    let legacy = rows.iter().find(|row| row.get("id") == Some(&id)).expect("history lists the saved legacy edit");
    assert_eq!(legacy.get("ai"), Some(&json::Value::Bool(true)), "EDIT_AI alone identifies a legacy AI edit in JSON");
    assert!(legacy.get("agent").is_none(), "history leaves the unknown legacy agent id absent");
    panel.sync_history(cx, &root, &s.ana.session, &model, None);
    draw(cx);
    assert_eq!(edits.get_item(5).unwrap().1.label(cx, ids!(tweak_entry)).text(),
        "Note  Ana's AI  ui.panel.padding  Check spacing", "the history panel labels an EDIT_AI-only edit with the person's AI");

    dispatch(&mut panel, &mut s.ana, cx);
    draw(cx);
    let rect = item_of(id!(NumRow)).widget(cx, ids!(editor)).area().rect(cx);
    let label = dvec2(rect.pos.x + 16., rect.center().y);
    let before = s.app.sent.len();
    for time in [10., 10.2] {
        let (_, press, _) = click_at(cx, label, time);
        panel.actions(cx, &root, &press, &mut s.ana.session);
    }
    s.one();
    assert!(matches!(&s.app.sent[before..], [(_, None, DesignOp::Design { verb: DesignVerb::Typed, args })]
        if args == &[("path".into(), PATH.into()), ("prop".into(), "runtimeOverride".into()), ("value".into(), "0".into())]),
        "double-clicking runtimeOverride's label sends one reset to its original value");

    let refresh = |panel: &mut panel::Panel, s: &mut Copies, cx: &mut Cx| {
        dispatch(panel, &mut s.ana, cx);
        s.one();
        dispatch(panel, &mut s.ana, cx);
    };
    refresh(&mut panel, &mut s, cx);
    draw(cx);
    let size_row = item_of(id!(SizeRow));
    let before = s.app.sent.len();
    let (_, press) = click(cx, size_row.widget(cx, ids!(h_fix)));
    panel.actions(cx, &root, &press, &mut s.ana.session);
    s.one();
    let height_set = |app: &FakeApp, since, value: &str| matches!(&app.sent[since..],
        [(_, None, DesignOp::Design { verb: DesignVerb::Typed, args })]
        if args == &[("path".into(), PATH.into()), ("prop".into(), "height".into()), ("value".into(), value.into())]);
    assert!(height_set(&s.app, before, "100"), "pressing the fixed H segment sends one height Set");
    refresh(&mut panel, &mut s, cx);
    draw(cx);
    let label = size_row.widget(cx, ids!(h_axis)).area().rect(cx).center();
    let before = s.app.sent.len();
    for time in [20., 20.2] {
        let (_, press, _) = click_at(cx, label, time);
        panel.actions(cx, &root, &press, &mut s.ana.session);
    }
    s.one();
    assert!(height_set(&s.app, before, "0"), "double-clicking H resets height to its own first old value");

    let before = s.records.len();
    let (_, press) = click(cx, item_of(id!(AlignRow)).widget(cx, ids!(j_mid)));
    panel.actions(cx, &root, &press, &mut s.ana.session);
    s.one();
    s.one();
    let applied: Vec<_> = s.records[before..].iter().map(|edit| match &edit.change {
        Change::Applied { new, .. } => (edit.path.as_str(), edit.prop.as_str(), new.as_str()),
        _ => panic!("expected the sharer to apply {}", edit.prop),
    }).collect();
    assert_eq!(applied, [(PATH, "distribute", "Distribute.Start"), (PATH, "align.y", "0.5")],
        "one Centre press reaches the sharer as distribute followed by align.y");

    s.ben.send(Request::AskEdit);
    s.exchange();
    let mut owner_panel = panel::Panel::default();
    owner_panel.refresh(cx, &root, &s.host.session, &Default::default(), &Default::default());
    draw(cx);
    let chips = root.portal_list(cx, ids!(tweak_seats_list));
    let allowed = chips.get_item(0).unwrap().1;
    let asked = chips.get_item(1).unwrap().1;
    assert_eq!(allowed.label(cx, ids!(tweak_status)).text(), "Allowed", "the first peer's chip shows its grant");
    assert_eq!(asked.label(cx, ids!(tweak_status)).text(), "Asked", "the other peer's pending request has a separate chip");
    let (up, clicks) = click(cx, asked.widget(cx, ids!(tweak_allow)));
    owner_panel.local_input(cx, &Event::Startup, &root, &clicks, &mut s.host.session,
        |_| panic!("a detached action must never ask for consent"));
    assert!(!s.host.session.view.allowed.contains(&BEN), "detached clicks have no authority");
    // The platform keeps its remote-input scope private. Supply the driver's provenance at the gate.
    let injected = |test_home| matches!(crate::consent::decide(Some("walk".into()), test_home),
        crate::consent::Consent::TestDriver(_));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| injected(false));
    assert!(s.host.session.view.asked.contains(&BEN) && !s.host.session.view.allowed.contains(&BEN),
        "an injected Allow in an ordinary home leaves the painted seat Asked");
    let (up, clicks) = click(cx, asked.widget(cx, ids!(tweak_allow)));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| injected(true));
    assert!(s.host.session.view.allowed.contains(&BEN), "a verified test home permits the driver to allow the painted seat");
    let (up, clicks) = click(cx, allowed.widget(cx, ids!(tweak_revoke)));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |cx| crate::consent::granted(cx, false));
    assert!(!s.host.session.view.allowed.contains(&ANA) && s.host.session.view.allowed.contains(&BEN),
        "revoke belongs to the pressed seat even across a roster update");

    s.allow(ANA, true);
    let mut roster = ana_agent_state(owner);
    roster.entities.insert(item, model.state.entities[&item].clone());
    s.host.session.set_agents(&roster);
    let chip = |i| chips.get_item(i).unwrap().1;
    for (request, expected) in [
        (Request::Pick(At::Point { window: 2, x: 8., y: 12. }), "Ana · picking in Shared UI"),
        (Request::Pick(At::Path("/card/t_move".into())), "Ana · picking t_move"),
        (Request::Props { path: PATH.into() }, "Ana · reading panel's properties"),
    ] {
        s.ana.send(request);
        s.exchange();
        s.host.session.drive_app(&mut s.app);
        s.exchange();
        owner_panel.refresh(cx, &root, &s.ben.session, &roster, &Default::default());
        draw(cx);
        assert_eq!(chip(0).label(cx, ids!(tweak_seat_name)).text(), expected, "the observer's person chip describes the actual request");
        s.finish();
    }
    s.ana.session.request(Some(ANA_AGENT), set("8")).unwrap();
    s.exchange();
    owner_panel.refresh(cx, &root, &s.host.session, &roster, &Default::default());
    draw(cx);
    assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), "Ana's AI", "a queued AI has a chip and no running text");
    s.host.session.drive_app(&mut s.app);
    s.exchange();
    for town in [&s.ana, &s.ben, &s.host] {
        owner_panel.refresh(cx, &root, &town.session, &roster, &Default::default());
        draw(cx);
        assert_eq!(chip(0).label(cx, ids!(tweak_seat_name)).text(), "Ana", "the idle person precedes their AI");
        assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), "Ana's AI · proposing padding 8", "all seats bind the same running AI chip");
        assert_eq!(chip(1).widget(cx, ids!(tweak_stop)).visible(), town.session.me == owner, "only the sharer sees Stop");
    }
    let (up, clicks) = click(cx, chip(1).widget(cx, ids!(tweak_stop)));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| false);
    assert_eq!(s.host.session.view.agents[&(ANA, ANA_AGENT)], false, "an unverified Stop press cannot change AI consent");
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| true);
    s.finish();
    owner_panel.refresh(cx, &root, &s.host.session, &roster, &Default::default());
    draw(cx);
    assert_eq!(chip(1).label(cx, ids!(tweak_status)).text(), "stopped", "the chip shows the sharer's Stop");
    let set_time = crate::view::LocalClock::default().hms(s.host.session.view.finished[&(ANA, ANA_AGENT)].1);
    assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), format!("Ana's AI · set padding 8 · {}", &set_time[..5]),
        "an already dispatched Set that succeeds after Stop records its actual outcome and local minute");
    let (up, clicks) = click(cx, chip(1).widget(cx, ids!(tweak_allow)));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| true);
    assert_eq!(s.host.session.view.agents[&(ANA, ANA_AGENT)], false, "the AI chip's Allow releases just its stop");
    assert!(s.host.session.view.allowed.contains(&ANA), "both AI controls preserve the person's grant");

    s.host.session.bind_window(super::WindowId(2));
    let look = s.ana.session.request(Some(ANA_AGENT), Request::Look).unwrap();
    s.exchange();
    let clock = crate::view::LocalClock::default();
    for town in [&s.ana, &s.ben, &s.host] {
        owner_panel.refresh(cx, &root, &town.session, &roster, &clock);
        draw(cx);
        assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), "Ana's AI · looking at Shared UI's share",
            "every observer sees the guest looking at the share while the grant is active");
    }
    s.ana.session.request(Some(ANA_AGENT), Request::Props { path: PATH.into() }).unwrap();
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    s.exchange();
    owner_panel.refresh(cx, &root, &s.ana.session, &roster, &clock);
    draw(cx);
    assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), "Ana's AI · reading panel's properties",
        "the AI's running app action takes priority over its concurrent look");
    s.finish();
    owner_panel.refresh(cx, &root, &s.ana.session, &roster, &clock);
    draw(cx);
    assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), "Ana's AI · looking at Shared UI's share",
        "the chip returns to the live look when the app action finishes");
    s.host.session.still_reply(LINK, 1, &Ok(test_still()));
    s.exchange();
    s.ana.session.request(Some(ANA_AGENT), Request::LookDone { look, ok: true }).unwrap();
    s.exchange();
    for town in [&s.ana, &s.ben, &s.host] {
        owner_panel.refresh(cx, &root, &town.session, &roster, &clock);
        draw(cx);
        let time = clock.hms(town.session.view.finished[&(ANA, ANA_AGENT)].1);
        assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), format!("Ana's AI · looked at Shared UI's share · {}", &time[..5]),
            "each seat replaces the earlier action with one truthful look and its local minute");
    }
    let failed = s.ana.session.request(Some(ANA_AGENT), Request::Look).unwrap();
    s.exchange();
    s.ana.session.request(Some(ANA_AGENT), Request::LookDone { look: failed, ok: false }).unwrap();
    s.exchange();
    owner_panel.refresh(cx, &root, &s.host.session, &roster, &clock);
    draw(cx);
    assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), "Ana's AI", "a failed look leaves the sharer's chip without past-tense text");

    let mut hub = Tweaks::<u64, u64>::default();
    let town = model.open_id();
    root.set_visible(cx, false); // The Town window starts with the panel hidden.
    let idle_update = |cx: &mut Cx, hub: &mut Tweaks<u64, u64>| {
        let before = cx.widget_tree().stats().lookups;
        hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
        assert_eq!(cx.widget_tree().stats().lookups, before, "an idle update must perform zero widget lookups");
    };
    idle_update(cx, &mut hub);
    assert!(!root.visible(), "an idle window reserves no space for the panel");
    let other = [33; 16];
    for id in [SHARE, other] {
        hub.sessions.push(Session::owner(town, id, item, owner, LINK).unwrap());
    }
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "a live share alone leaves the panel closed");
    assert!(hub.open_panel(other), "Tweak selects the share named by the page");
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(root.visible(), "Tweak opens the inspector before a pick");
    hub.close_panel();
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "closing the panel keeps the share running");
    hub.pick(other, At::Path("body".into())).unwrap();
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(root.visible(), "the person's pick in the share's picture opens the panel");
    hub.share_ended(other);
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(root.visible(), "ending the selected share leaves the panel open for the other live share");
    hub.share_ended(SHARE);
    let background = [44; 16];
    hub.sessions.push(Session::owner(TOWN, background, ITEM, owner, LINK).unwrap());
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "closing the last visible share hides the panel even while another town has a share");
    idle_update(cx, &mut hub);
    hub.share_ended(background);
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    idle_update(cx, &mut hub);
    assert!(hub.sessions.is_empty(), "the finished owner sessions have retired");

    let viewing = [55; 16];
    hub.sessions.push(Session::peer(town, viewing, item, owner, ANA).unwrap());
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "a guest's share alone leaves the panel closed");
    hub.pick(viewing, At::Path("body".into())).unwrap();
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(root.visible(), "the guest's own pick opens the panel");
    hub.share_ended(viewing);
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "the very next update hides the last panel while the viewer awaits the owner's final notice");
    idle_update(cx, &mut hub);
    let asking = [66; 16];
    hub.sessions.push(Session::owner(town, asking, item, owner, LINK).unwrap());
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "the sharer's own share alone leaves the panel closed");
    hub.sessions.iter_mut().find(|s| s.share == asking).unwrap().view.updates.push_back((ANA, 1, None, Notice::Asked));
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(root.visible(), "a guest asking to edit opens the panel so the sharer can answer");
    hub.share_ended(asking);
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    for id in [ids!(path), ids!(name), ids!(list), ids!(status)] {
        assert!(!ui.widget(cx, id).is_empty(), "the other workspace controls remain addressable from the root");
    }
    assert_eq!(cx.widget_tree().lookup_clashes(), Vec::<String>::new(), "the panel must leave Town's root lookups unambiguous");
}

// Obtain opaque G11 identities from its public API. Only the isolated fixture registry is scanned.
// The unconfigured command link below never connects to an endpoint.
#[cfg(any(unix, windows))]
fn fixture_app(home: &std::path::Path) -> crate::app_link::LocalApp {
    use crate::app_link::{AppLinkEnv, AppLinks};
    use std::sync::{mpsc, Arc};
    let pid = std::process::id();
    #[cfg(unix)]
    let socket = makepad_remote::carrier::unix::own_socket_dir().unwrap().join(format!("tweak-test-{pid}"));
    #[cfg(unix)]
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    #[cfg(windows)]
    let socket = std::path::PathBuf::from("tcp:127.0.0.1:1");
    // The discovery file belongs to this test process; the synthetic entry describes a different app.
    let entry = format!(r#"{{"pid":{},"app":"Fixture","socket":{},"token":"{}","started_ms":1,"caps":["cmd","frames"]}}"#,
        u64::from(pid) + 1, makepad_remote::json_str(socket.to_str().unwrap()), "0".repeat(64));
    std::fs::write(home.join(format!("makepad-remote-{pid}.json")), entry).unwrap();
    let (tx, rx) = mpsc::channel();
    let (done, joined) = mpsc::channel();
    let mut scan = AppLinks::default();
    scan.configure(AppLinkEnv { registry: Some(home.into()), codecs: makepad_call::driver::codec::platform_codecs(),
        signal: Arc::new(move || tx.send(()).unwrap()), spawn: Arc::new(move |_, job| {
            let done = done.clone();
            std::thread::spawn(move || { job(); done.send(()).unwrap(); });
        }) });
    scan.scan();
    rx.recv().unwrap();
    scan.poll();
    #[cfg(unix)]
    { drop(listener); std::fs::remove_file(socket).unwrap(); }
    let app = scan.apps().first().expect("G11 discovered the synthetic app").clone();
    drop(scan);
    joined.recv().unwrap();
    app
}

#[test]
#[cfg(any(unix, windows))]
fn share_lifecycle_pins_the_board_and_inflight_write_to_the_original_town() {
    use crate::app_link::AppLinks;
    use makepad_town_core::{entity::field, event::SessionMode};

    let (homes, mut viewer, mut model) = crate::model::tests::shared_town("tweak-source");
    let town = model.open_id();
    let me = model.me();
    let peer = viewer.me();
    let own = model.towns().into_iter().map(|town| town.id).find(|id| *id != town).unwrap();
    let board = model.act(Action::AddBoard { project: model.current_project().unwrap().id, title: "Editing".into() }, Origin::Ui).unwrap();
    let design = model.act(Action::AddDesign { board, x: 0, y: 0, preset: "phone".into(),
        title: "Layout".into(), src: "View{}".into() }, Origin::Ui).unwrap();
    let open = |m: &mut Model, share| m.act(Action::SessionOpen { town: None, session: share,
        mode: SessionMode::Share { title: "Fixture".into() }, to: vec![m.call_seat(&peer).unwrap()] }, Origin::Ui).unwrap();
    open(&mut model, SHARE);
    crate::model::tests::exchange(&mut model, &mut viewer);
    let app = fixture_app(&homes.0);
    let mut links = AppLinks::default();
    let link = links.link(&app.key);
    links.poll();
    let (mut host, mut watch) = (Tweaks::default(), Tweaks::default());
    watch.viewer_started(&viewer, SHARE, "Fixture".into()).unwrap();
    watch.resolve(&viewer);
    assert!(watch.session(SHARE).is_none(), "Watch waits for the synced App item");
    host.sharer_started(&model, SHARE, "Fixture".into(), link, WindowId(3), Some(board)).unwrap();
    model.open_town(own).unwrap();
    let [item] = host.record(&mut model)[..] else { panic!("the share creates one App item") };
    assert!(!model.state.entities.contains_key(&item), "AddApp leaves the displayed town alone");
    crate::model::tests::exchange(&mut model, &mut viewer);
    watch.resolve(&viewer);
    assert_eq!(watch.session(SHARE).unwrap().sharer, me, "viewer binds the signed item to the timeline opener");
    assert_eq!(viewer.state.entities[&item].value(field::BOARD), Some(&makepad_town_core::event::Value::Id(board)), "the captured board received the App");
    assert!(viewer.state.entities[&item].z_key().unwrap().0 > viewer.state.entities[&design].z_key().unwrap().0,
        "the App stacks above the existing design in the share's town after the owner switches towns");

    // A member can sign a field write directly, bypassing Town's local action checks.
    use makepad_town_core::event::{Actor, Change as FieldChange, Content, Draft, Event, Value, Via};
    let parents = makepad_town_core::log::Log::open(&viewer.home.town_log_path(&town)).unwrap().0.parents_for(&peer);
    for (field, bytes) in [(field::SESSION, vec![99; 16]), (field::SHARER, me.to_vec())] {
        let content = Content::Set { changes: vec![FieldChange { entity: item, field: field.clone(),
            value: Value::Key(bytes) }], label: None };
        let event = Event::create(&viewer.device_key(), town, Draft { kind: content.kind(), kind_version: content.version(),
            actor: Actor::Human, via: Via::Ui, critical: false, parents: parents.clone(), wall_ms: 0,
            content: &content.encode() }, [1; 16]).unwrap();
        let mut state = viewer.state.clone();
        assert_eq!(share_of(&state.entities[&item]), Some((SHARE, me)), "the original create binds both fields");
        state.receive(&event);
        let app = &state.entities[&item];
        assert_eq!(app.fields[&field].writer, event.id, "the member's signed write reached the register");
        assert_eq!(share_of(app), None, "a rewritten {field:?} cannot establish the share, even with the original value");
    }

    let mut net = Copies::new(me, peer, item);
    let exchange = |host: &mut Tweaks, watch: &mut Tweaks, net: &mut Copies| {
        watch.relay(&mut net.ana.relay);
        host.relay(&mut net.host.relay);
        watch.relay(&mut net.ana.relay);
    };
    let point = At::Point { window: 3, x: 9., y: 10. };
    let refused = watch.pick(SHARE, point.clone()).unwrap();
    watch.sessions[0].request(None, Request::AskEdit).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    assert!(watch.sessions[0].view.updates.iter().any(|(_, id, _, n)| *id == refused && matches!(n, Notice::Refused(_))),
        "the viewer's first pick requires Allow");
    assert!(host.sessions[0].view.asked.contains(&peer), "the owner's panel has a request for this seat");
    host.sessions[0].consent(peer, true).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    let wrong_window = watch.pick(SHARE, At::Point { window: 9, x: 0., y: 0. }).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    assert!(watch.sessions[0].view.updates.iter().any(|(_, id, _, n)| *id == wrong_window
        && matches!(n, Notice::Refused(why) if why.contains("window"))), "the captured window constrains picks before geometry arrives");
    let picture = Some((SHARE, [200., 100.]));
    host.sessions[0].window(WindowId(3), [200., 100.]);
    exchange(&mut host, &mut watch, &mut net);
    let mut overlay = None;
    assert!(watch.refresh_selections(&viewer, picture, &mut overlay), "the picture gets its initial overlay");
    assert!(!watch.refresh_selections(&viewer, picture, &mut overlay), "an idle event reuses the prepared rings");
    watch.pick(SHARE, point).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    // Commands through the real disconnected hub supply real G11 tickets. The fake app answers those tickets.
    let answer = |host: &mut Tweaks, links: &mut AppLinks, json: &str| {
        host.link_events(&[], links);
        let mut events = links.poll();
        let [AppLinkEvent::Design { ok, json: payload, .. }] = events.as_mut_slice() else { panic!("one G11 design op outstanding") };
        *ok = true;
        *payload = json.into();
        host.link_events(&events, links);
    };
    answer(&mut host, &mut links, r#"{"path":"ui.panel","type":"View","window":3,"rect":[1,2,3,4]}"#);
    exchange(&mut host, &mut watch, &mut net);
    assert_eq!(watch.sessions[0].view.selections[&peer].path, PATH, "the real Design answer reached the viewer");
    assert!(watch.refresh_selections(&viewer, picture, &mut overlay), "a pick rebuilds the displayed rings");
    assert!(watch.refresh_selections(&viewer, None, &mut overlay) && overlay.is_none(), "ending the picture clears its cache");
    watch.sessions[0].request(None, Request::Props { path: PATH.into() }).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    answer(&mut host, &mut links, ROWS);
    exchange(&mut host, &mut watch, &mut net);
    host.link_events(&[AppLinkEvent::LocalInput(link)], &mut links);
    model.open_town(town).unwrap();
    let set_req = watch.sessions[0].request(None, set("14")).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    host.link_events(&[], &mut links);
    assert_eq!(host.unrecorded_edits(), 1, "an accepted Set with an outstanding app ticket counts once");
    model.open_town(own).unwrap();
    let mut events = links.poll();
    let [AppLinkEvent::Design { ok, json, .. }] = events.as_mut_slice() else { panic!("LocalInput leaves the grant intact") };
    *ok = true;
    *json = r#"{"old":{"padding":"8"},"new":{"padding":"14"}}"#.into();
    host.link_events(&events, &mut links);
    exchange(&mut host, &mut watch, &mut net);
    let applied = |hub: &Tweaks| hub.session(SHARE).unwrap().view.updates.iter().any(|(seat, req, _, n)|
        *seat == peer && *req == set_req && matches!(n, Notice::Applied { old, new, seq: 1, .. } if old == "8" && new == "14"));
    assert!(!applied(&host) && !applied(&watch), "even a relay drain after G11's answer cannot publish an unsigned edit");
    assert!(host.settling() && host.unrecorded_edits() == 1, "the answered Set still needs its signed append");
    let [edit] = host.record(&mut model)[..] else { panic!("one signed AppEdit must settle the Set") };
    assert!(!host.settling() && host.unrecorded_edits() == 0, "a signed edit leaves the close count");
    let state = model.tweak_board_state(town).expect("the pinned town is loaded").0;
    assert_eq!(state.entities[&edit].value(field::PARENT), Some(&makepad_town_core::event::Value::Id(item)),
        "the edit is signed under the original App before the relay releases it to the viewer");
    assert!(applied(&host) && !applied(&watch), "the signed append releases the owner's Applied");
    exchange(&mut host, &mut watch, &mut net);
    assert!(applied(&host) && applied(&watch), "both seats see the same saved Applied");
    assert!(model.event(&edit).is_none(), "the write did not enter the displayed town");
    model.open_town(town).unwrap();
    assert!(model.event(&edit).is_some(), "the edit is durable in the original town");
    assert!(host.record(&mut model).is_empty(), "a settled edit is never appended twice");

    let other = [33; 16];
    let second = links.link(&app.key);
    links.poll();
    open(&mut model, other);
    host.sharer_started(&model, other, "Second".into(), second, WindowId(5), None).unwrap();
    assert_eq!(host.record(&mut model).len(), 1, "starting the share creates its own signed App item");
    host.sessions[1].consent(peer, true).unwrap();
    host.link_events(&[AppLinkEvent::Restarted { ended: second, app: app.clone() }], &mut links);
    assert!(host.sessions[1].is_ended() && host.sessions[1].view.allowed.is_empty(), "Restarted closes only the replaced link");
    assert!(host.sessions[0].view.allowed.contains(&peer), "the first share keeps its grant");
    host.peer_unlinked(peer);
    assert!(host.sessions[0].view.allowed.is_empty(), "unlink revokes a live grant");
    host.sessions[0].consent(peer, true).unwrap();
    host.share_ended(SHARE);
    assert!(host.sessions[0].view.allowed.is_empty(), "S2 Ended releases consent");

    let last = [44; 16];
    open(&mut model, last);
    host.sharer_started(&model, last, "Last".into(), link, WindowId(3), None).unwrap();
    assert_eq!(host.record(&mut model).len(), 1, "starting the share creates its own signed App item");
    host.sessions[2].consent(peer, true).unwrap();
    host.link_events(&[AppLinkEvent::Ended { id: link, why: "closed".into() }], &mut links);
    assert!(host.sessions[2].is_ended() && host.sessions[2].view.allowed.is_empty(), "G11 Ended releases consent");

    let departing = [55; 16];
    open(&mut model, departing);
    host.sharer_started(&model, departing, "Leaving".into(), second, WindowId(5), None).unwrap();
    assert_eq!(host.record(&mut model).len(), 1, "starting the share creates its own signed App item");
    crate::model::tests::exchange(&mut model, &mut viewer);
    watch.viewer_started(&viewer, departing, "Leaving".into()).unwrap();
    watch.resolve(&viewer);
    host.sessions[3].consent(peer, true).unwrap();
    watch.sessions.last_mut().unwrap().request(None, Request::Props { path: PATH.into() }).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    answer(&mut host, &mut links, ROWS);
    exchange(&mut host, &mut watch, &mut net);
    let final_set = watch.sessions.last_mut().unwrap().request(None, set("20")).unwrap();
    exchange(&mut host, &mut watch, &mut net);
    answer(&mut host, &mut links, r#"{"old":{"padding":"14"},"new":{"padding":"20"}}"#);
    model.leave_town().unwrap();
    let before = model.tweak_board_state(town).unwrap().1;
    assert!(host.record(&mut model).is_empty(), "the departed owner cannot append in that town");
    assert_eq!(model.tweak_board_state(town).unwrap().1, before, "a refused Set adds no signed record");
    exchange(&mut host, &mut watch, &mut net);
    for hub in [&host, &watch] {
        let session = hub.session(departing).unwrap();
        assert!(session.is_ended() && !session.has_unsettled_edits(), "the failed final append closes the share cleanly");
        let notices: Vec<_> = session.view.updates.iter().filter(|(_, req, _, _)| *req == final_set).collect();
        assert!(matches!(&notices[..], [(_, _, _, Notice::Running(r)), (_, _, _, Notice::Refused(why))]
            if *r == set("20") && why.contains("town")), "both seats see the Set start and only a refusal for its outcome");
    }
}

#[test]
fn props_sections_row_kinds_and_edit_marks() {
    use super::props::{AppliedEdit, Props, RowKind, SectionKind, StructKind, VisKind, SECTION_ORDER};
    use RowKind::{Bool, Color, Num, Text};
    let json = r##"{"props":[
        {"name":"padding.bottom","value":"4"},
        {"name":"draw_bg.rect_pos","value":"vec2f(0 0)"},
        {"name":"text","value":"\"Hello\"","set":true,"origin":"authored","level":0},
        {"name":"padding.right","value":"3"},
        {"name":"visible","value":"true"},
        {"name":"offset","value":"vec2f(1, 2)"},
        {"name":"draw_bg.color","value":"#c0ffee","set":false,"origin":"inherited","level":1},
        {"name":"padding.left","value":"1"},
        {"name":"padding.top","value":"2"}],
        "cascade":[{"loc":"panel.rs:7","file":"panel.rs","line":7},{"loc":"native","file":"","line":0}]}"##;
    let mut props = Props::rebuild_rows(json).expect("the shared Rows fixture decodes");
    let edit = |path: &str, prop: &str, old: &str, new: &str| AppliedEdit {
        path: path.into(), prop: prop.into(), old: old.into(), new: new.into(),
    };
    props.mark_changed(PATH, [
        edit(PATH, "padding.top", "0", "1"),
        edit(PATH, "padding.top", "1", "2"),
        edit(PATH, "visible", "true", "false"),
        edit(PATH, "visible", "false", "true"),
        edit("another", "draw_bg.color", "#fff", "#000"),
    ].iter());
    let visible = props.visible_rows(&[false; 6], &[false; 6], "View");
    let sections: Vec<_> = visible.iter().filter_map(|v| match v {
        VisKind::Section(section, _, _) => Some(*section), _ => None,
    }).collect();
    assert_eq!(props.cascade.iter().map(|l| (l.loc.as_str(), l.file.as_str())).collect::<Vec<_>>(),
        [("panel.rs:7", "panel.rs"), ("native", "")], "cascade levels retain instance-to-native order");
    assert_eq!(visible.last(), Some(&VisKind::CascadeLevel(1)), "the last cascade level uses its read-only row");
    assert_ne!(props.cascade[0].color, props.cascade[1].color, "cascade levels have distinct inks");
    assert_eq!(sections, SECTION_ORDER, "the grouped rows follow the sidebar's section order");
    assert_eq!(SECTION_ORDER.map(SectionKind::title), ["Layout", "Style", "Text", "Behavior", "Other", "Cascade"],
        "Cascade follows the five property sections");
    let expected = [("padding.left", Num), ("padding.top", Num), ("padding.right", Num), ("padding.bottom", Num),
        ("draw_bg.color", Color), ("text", Text), ("visible", Bool), ("offset", Text)];
    assert_eq!(props.rows.len(), expected.len(), "shader plumbing is omitted while design inputs survive");
    for (row, (name, kind)) in props.rows.iter().zip(expected) {
        assert_eq!(row.prop, name, "group and inset leg order at {name}");
        assert_eq!(row.kind, kind, "value-derived row kind for {name}");
        assert_eq!(row.set, name == "text", "instance set mark for {name}");
        assert_eq!(row.changed, name == "padding.top", "first old and last new on the selected path for {name}");
    }
    assert_eq!(props.rows.last().unwrap().struct_kind, StructKind::Vec2, "the offset value is a vec2");
}

#[test]
fn typed_prop_rows_commit_one_value_and_reset_to_history() {
    use super::props::{AppliedEdit, EditorEvent, Props};
    use makepad_widgets::{fab_controls::FabValueInputAction::{Changed, Ended, Reset}, live_id, LiveId};
    let mut props = Props::rebuild_rows(r#"{"props":[
        {"name":"spacing","value":"3"},
        {"name":"visible","value":"true"},
        {"name":"offset","value":"vec2f(1, -2)"},
        {"name":"margin","value":"{left: 1 top: 2 right: 3 bottom: 4}"},
        {"name":"metrics","value":"{descender: -1 line_gap: 2 line_scale: 1}"},
        {"name":"text_style.font_family","value":"{latin: {..}}"}
    ]}"#).expect("typed Rows fixture decodes");
    props.mark_changed(PATH, [&AppliedEdit { path: PATH.into(), prop: "spacing".into(),
        old: "1.75".into(), new: "3".into() }].into_iter());
    let row = |name| props.rows.iter().find(|r| r.prop == name).unwrap_or_else(|| panic!("missing fixture row {name}"));
    for (name, template) in [("spacing", live_id!(NumRow)), ("visible", live_id!(BoolRow)),
        ("offset", live_id!(VecRow)), ("margin", live_id!(InsetRow)), ("metrics", live_id!(MetricsRow)),
        ("text_style.font_family", live_id!(NoEditorRow))] {
        assert_eq!(row(name).template(), template, "editor template for {name}");
    }

    let number = |action| EditorEvent::Number { leg: None, action };
    let leg = |index, action| EditorEvent::Number { leg: Some(index), action };
    assert_eq!(row("spacing").set_value(number(Changed(4.123456))).as_deref(), Some("4.1235"), "a live spacing step uses the final value spelling");
    assert_eq!(row("spacing").set_value(number(Ended(4.123456))).as_deref(), Some("4.1235"),
        "ending spacing sends one Set in the tweaker's four-decimal spelling");
    assert_eq!(row("visible").set_value(EditorEvent::Toggle(false)).as_deref(), Some("false"),
        "one visible checkbox change sends its boolean");
    assert_eq!(row("offset").set_value(leg(0, Ended(3.5))).as_deref(), Some("vec2f(3.5, -2)"),
        "the offset Set keeps the other leg and separates negative components with a comma");
    assert_eq!(row("margin").set_value(leg(2, Changed(7.5))).as_deref(), Some("Inset{left: 1 top: 2 right: 7.5 bottom: 4}"),
        "dragging margin.right preserves the other three legs");
    assert_eq!(row("margin").set_value(leg(2, Ended(7.5))).as_deref(),
        Some("Inset{left: 1 top: 2 right: 7.5 bottom: 4}"), "ending margin.right sends one whole inset");
    assert_eq!(row("metrics").set_value(leg(1, Ended(0.5))).as_deref(),
        Some("Metrics{descender: -1 line_gap: 0.5 line_scale: 1}"), "the metrics Set preserves the two untouched values");
    assert_eq!(row("spacing").set_value(number(Reset)).as_deref(), Some("1.75"), "double-click resets spacing to the first old value");
    assert_eq!(row("spacing").set_value(EditorEvent::Reset).as_deref(), Some("1.75"), "the spacing label resets to the same baseline");
    assert_eq!(row("margin").set_value(leg(2, Reset)), None, "resetting untouched margin generates no Set");
    for old in ["null", "@-"] {
        props.mark_changed(PATH, [&AppliedEdit { path: PATH.into(), prop: "spacing".into(),
            old: old.into(), new: "3".into() }].into_iter());
        let spacing = props.rows.iter().find(|r| r.prop == "spacing").unwrap();
        assert_eq!(spacing.set_value(EditorEvent::Reset).as_deref(), Some("nil"), "resetting spacing spells {old} as nil");
    }
}

#[test]
fn layout_composites_choose_ordered_sets() {
    use super::props::{layout::{BoxKind, LayoutClick, LayoutEvent, LayoutKind::*}, Props, VisKind};
    let view = Props::rebuild_rows(r#"{"props":[
        {"name":"width","value":"Size.Fill{weight: 2 basis: 0 shrink: 1}"},
        {"name":"height","value":"Fit"},{"name":"flow","value":"Down"},{"name":"spacing","value":"6"},
        {"name":"align.x","value":"0"},{"name":"align.y","value":"0.5"},
        {"name":"distribute","value":"Distribute.SpaceBetween"},
        {"name":"margin.left","value":"3"},{"name":"margin.top","value":"4"},
        {"name":"margin.right","value":"5"},{"name":"margin.bottom","value":"6"},{"name":"padding.left","value":"0"}]}"#).unwrap();
    let grid = Props::rebuild_rows(r#"{"props":[{"name":"columns","value":"[\"1fr\"]"},
        {"name":"rows","value":"[\"20px\"]"},{"name":"areas","value":"[\"head\"]"},
        {"name":"align_items","value":"CellAlign.Stretch"}]}"#).unwrap();
    for (ty, props, expected) in [
        ("View", &view, vec![Size, BoxInset(BoxKind::Margin), FlowSpacing, AlignGrid, BoxInset(BoxKind::Padding)]),
        ("Grid", &grid, vec![Size, GridTracks, AlignGrid]),
    ] {
        for expanded in [false, true] {
            let mut visible = Vec::new();
            for entry in props.visible_rows(&[false; 6], &[expanded; 6], ty) {
                match entry {
                    VisKind::Layout(Identity | Group(_)) => {}
                    VisKind::Layout(kind) => visible.push(kind),
                    VisKind::Prop(i) => panic!("{ty}.{} also appeared as a plain row", props.rows[i].prop),
                    _ => {}
                }
            }
            assert_eq!(visible, expected, "{ty} size, box, flow, alignment and tracks order (expanded={expanded})");
        }
    }
    let v = view.layout_values("View");
    let g = grid.layout_values("Grid");
    let set = |prop: &str, value: &str| (prop.to_owned(), value.to_owned());
    let sets = |layout: &super::props::layout::LayoutValues, event, pending| {
        layout.decision(event, pending).into_iter().flat_map(|sets| sets.iter())
            .map(|(prop, value)| (prop.into_owned(), value)).collect::<Vec<_>>()
    };
    assert_eq!(sets(&v, LayoutEvent::Click(LayoutClick::Justify(1.0)), None),
        [set("distribute", "Distribute.Start"), set("align.y", "1")], "justify End clears SpaceBetween and sets align.y in order");
    assert_eq!(sets(&v, LayoutEvent::Click(LayoutClick::Cross(0.5)), None), [set("align.x", "0.5")],
        "cross-axis alignment under SpaceBetween sends only align.x");
    let right = Props::rebuild_rows(r#"{"props":[{"name":"flow","value":"Right"},
        {"name":"distribute","value":"Distribute.Start"}]}"#).unwrap().layout_values("View");
    assert_eq!(sets(&right, LayoutEvent::Click(LayoutClick::Justify(0.0)), None), [set("align.x", "0")],
        "justify under Start in a Right flow sends only align.x");
    assert_eq!(sets(&v, LayoutEvent::Returned("width", "120"), None), [set("width", "120")], "width commits size_chunk on Return");
    assert_eq!(sets(&v, LayoutEvent::Click(LayoutClick::Size("height", 1)), None), [set("height", "Fit")], "height Fit sends one Set");
    use makepad_widgets::fab_controls::FabValueInputAction::{Ended, Changed};
    assert_eq!(sets(&v, LayoutEvent::Number("margin.left", &Changed(7.0)), None), [set("margin.left", "7")], "margin.left steps send the dotted leg");
    assert_eq!(sets(&v, LayoutEvent::Number("margin.left", &Ended(7.0)), None), [set("margin.left", "7")], "margin.left commits only its dotted leg");
    assert_eq!(sets(&v, LayoutEvent::Number("width#shrink", &Ended(0.5)), None),
        [set("width", "Size.Fill{weight: 2 basis: 0 shrink: 0.5}")], "width keeps its other Fill fields");
    assert_eq!(sets(&v, LayoutEvent::Number("width#shrink", &Changed(0.5)), Some("Size.Fill{weight: 3 basis: 0 shrink: 1}")),
        [set("width", "Size.Fill{weight: 3 basis: 0 shrink: 0.5}")], "a new Fill leg preserves the last queued weight");
    assert_eq!(sets(&g, LayoutEvent::Returned("columns", "minmax(20px, 1fr) 2fr"), None),
        [set("columns", r#"["minmax(20px, 1fr)" "2fr"]"#)], "columns commits tracks_chunk's balanced list");
    assert_eq!(sets(&g, LayoutEvent::Returned("areas", "head head / nav main"), None),
        [set("areas", r#"["head head" "nav main"]"#)], "areas commits areas_chunk's rows");
}

#[test]
fn live_sets_keep_the_last_value_until_reply() {
    use super::props::live::{LiveSets, Phase::*};
    let mut live = LiveSets::default();
    let path = Rc::<str>::from(PATH);
    let sent = RefCell::new(Vec::new());
    let send = |_: &Rc<str>, prop: &str, value: &str| {
        let mut sent = sent.borrow_mut();
        sent.push((prop.to_owned(), value.to_owned()));
        Some(sent.len() as RequestId)
    };
    let unexpected = |_: &Rc<str>, _: &str, _: &str| panic!("an unanswered Set must hold later values");
    live.offer(&path, "width", "1".into(), Step, send);
    live.offer(&path, "width", "2".into(), Step, unexpected);
    live.offer(&path, "width", "3".into(), End, unexpected);
    assert!(live.open_on(PATH), "the final width is still waiting for the first reply");
    assert!(live.reply(1, send).is_none(), "a refusal also releases the latest waiting width");
    assert_eq!(sent.borrow().as_slice(), [("width".into(), "1".into()), ("width".into(), "3".into())],
        "only the first and final width reach the sender");
    assert_eq!(live.reply(2, unexpected).as_deref(), Some(PATH), "the final width reply finishes its gesture");
    assert!(live.reply(2, unexpected).is_none() && !live.open_on(PATH), "the width gesture finishes exactly once");

    live.offer(&path, "x", "4".into(), Step, send);
    live.offer(&path, "x", "5".into(), Step, unexpected);
    live.offer(&path, "x", "4".into(), End, unexpected);
    assert_eq!(live.reply(3, unexpected).as_deref(), Some(PATH), "returning to the in-flight x cancels its waiting value");
    live.offer(&path, "y", "6".into(), End, send);
    assert_eq!(live.reply(4, unexpected).as_deref(), Some(PATH), "an idle end sends and finishes on reply");
    live.offer(&path, "y", "7".into(), Step, send);
    assert!(live.reply(5, unexpected).is_none() && live.open_on(PATH), "a caught-up drag stays open");
    assert_eq!(live.offer(&path, "y", "7".into(), End, unexpected).as_deref(), Some(PATH),
        "ending at the acknowledged value finishes without a duplicate Set");
}

#[test]
fn shader_metadata_fallback_and_function_edits() {
    use super::{props::Props, shader::*};
    let rows = r##"{"props":[{"name":"draw_bg.color","value":"#abc"},{"name":"draw_bg.radius","value":"3"},{"name":"width","value":"Fill"},{"name":"draw_text.color","value":"#def"}]}"##;
    let props = Props::rebuild_rows(rows).unwrap();
    let layers = props.shaders;
    assert_eq!(layers.iter().map(|l| l.layer.as_str()).collect::<Vec<_>>(), ["draw_bg", "draw_text"], "older sharers expose each draw layer once");
    assert!(layers.iter().all(|l| l.drawn && !l.has_metadata && l.fns.is_empty()), "fallback layers carry no invented metadata");
    assert!(format_fn_sources(&layers[0]).contains("unavailable from this app version"), "older sharers explain why source is unavailable");
    let meta = parse_json(r#"{"shaders":[{"layer":"draw_bg","drawn":true,"inputs":[{"name":"color","io":"instance"}],"fns":[{"name":"pixel","loc":"a:1","src":"pixel: fn() {\n  // old\n}"},{"name":"vertex","loc":"a:5","src":"vertex: fn() {}"}]}]}"#).unwrap();
    let invalid = parse_json(r#"{"shaders":[{"layer":"draw_bg","fns":[{"name":"pixel"},{"name":"vertex"},{"name":"shade"}]},{"layer":"a.b","fns":[{"name":"pixel"}]}]}"#).unwrap();
    assert_eq!(app::shader_fn_names(&invalid), ["draw_bg.pixel".into(), "draw_bg.vertex".into()].into(),
        "a note can address only a listed pixel or vertex on a single valid layer identifier");
    let layers = parse_shaders(&meta, &[]);
    let layer = &layers[0];
    assert_eq!(layer.inputs[0].doc, "", "input docs are optional");
    let text = format_fn_sources(layer);
    assert!(diff_fn_edits(layer, &text).is_empty(), "formatted functions round-trip without edits");
    assert_eq!(diff_fn_edits(layer, &text.replace("// old", "// revised")), [("draw_bg.pixel".into(), "pixel: fn() {\n  // revised\n}".into())], "submit only the changed function, preserving body comments and indentation");
}

#[test]
fn shader_tab_edits_literal_inputs_live_and_saves_fn_changes_as_notes() {
    use makepad_widgets::{fab_controls::FabColorPickAction, *};
    use crate::model::Action as ModelAction;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 285} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut draw = draw_panel(cx, root.clone());
    let home = crate::model::tests::ShareHomes::new("shader-tab");
    let mut model = Model::open(&home.0).unwrap().0;
    let item = model.act(ModelAction::AddApp { town: model.open_id(), board: None,
        geometry: [0, 0, 6400, 6400], session: SHARE, name: "Shared UI".into() }, Origin::Ui).unwrap();
    let mut s = Copies::new(model.me(), ANA, item);
    s.allow(ANA, true);
    s.ana.session.view.updates.clear();
    s.app.props = r##"{"props":[
        {"name":"draw_bg.color","value":"#445566"},{"name":"draw_bg.border_radius","value":"4"},
        {"name":"draw_bg.custom","value":"theme.accent"},{"name":"draw_text.color","value":"#ffffff"}],
        "shaders":[{"layer":"draw_bg","drawn":true,"doc":"Button surface","inputs":[
            {"name":"color","io":"instance"},{"name":"border_radius","io":"uniform","doc":"Corner radius"},{"name":"custom","io":"field"}],
            "consts":[{"name":"aa_soft","initial":"1.5","value":"1.5","loc":"button.rs:42","doc":"Edge softness"}],
            "fns":[{"name":"pixel","loc":"button.rs:12","src":"pixel: fn() {\n  return self.color\n}"}]},
        {"layer":"draw_text","drawn":false,"inputs":[{"name":"color","io":"instance"}],
            "fns":[{"name":"pixel","loc":"text.rs:9","src":"pixel: fn() {}"}]}]}"##.into();
    let state = Default::default();
    let mut panel = panel::Panel::default();
    let dispatch = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp| {
        while let Some((seat, req, agent, notice)) = town.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut town.session, seat, req, agent, &state, &notice);
        }
    };
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    draw(cx);
    s.ana.send(Request::Pick(At::Point { window: 0, x: 4., y: 8. }));
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    let click = |cx: &mut Cx, widget: WidgetRef, panel: &mut panel::Panel, town: &mut TownApp| {
        let (_, press, up) = panel_click_at(cx, &root, widget.area().rect(cx).center(), 1.0);
        panel.actions(cx, &root, &press, &mut town.session);
        panel.actions(cx, &root, &up, &mut town.session);
    };
    let shader_wrap = root.widget(cx, ids!(tweak_shader_wrap));
    assert!(root.widget(cx, ids!(tweak_props_wrap)).visible() && !shader_wrap.visible(), "Props opens first");
    click(cx, root.widget(cx, ids!(tab_shader)), &mut panel, &mut s.ana);
    draw(cx);
    let header_y = root.widget(cx, ids!(tab_shader)).area().rect(cx).center().y;
    let close = root.widget(cx, ids!(tweak_close)).area().rect(cx);
    assert!((close.pos.y..=close.pos.y + close.size.y).contains(&header_y), "the tabs sit beside Close at Town's panel width");
    let list = root.portal_list(cx, ids!(tweak_shader_list));
    let item_of = |template| (0..list.borrow().unwrap().range_end()).filter_map(|i| list.get_item(i))
        .find(|(id, _)| *id == template).unwrap_or_else(|| panic!("missing mounted {template} on the Shader tab")).1;
    let layers = root.flat_list(cx, ids!(shader_layers));
    let pill = |name| layers.borrow().unwrap().items.get(&LiveId::from_str(name)).expect("mounted layer pill").widget.clone();
    let src = root.text_input(cx, ids!(shader_src));
    assert!(shader_wrap.visible() && !root.widget(cx, ids!(tweak_props_wrap)).visible(), "Shader replaces Props");
    assert_eq!(root.label(cx, ids!(shader_title)).text(), "draw_bg", "first reflected layer is active");
    assert_eq!(root.label(cx, ids!(shader_doc)).text(), "Button surface", "the drawn layer shows its documentation");
    let constant = item_of(id!(InfoRow));
    assert_eq!([ids!(name), ids!(value), ids!(loc)].map(|id| constant.label(cx, id).text()),
        ["aa_soft", "1.5", "button.rs:42"], "the read-only constant has its name, value and location");
    assert_eq!(list.get_item(3).unwrap().1.label(cx, ids!(name)).text(), "draw_bg.border_radius · unif", "documented inputs lead");
    let color = item_of(id!(ColorRow));
    assert_eq!(color.text_input(cx, ids!(value)).text(), "#445566", "the live color editor binds its hex field");
    assert_eq!(item_of(id!(NumRow)).widget(cx, ids!(value)).borrow::<fab_controls::FabValueInput>().unwrap().value(),
        4., "the radius binds a number scrubber");
    let original_src = src.text();
    assert!(original_src.starts_with("// pixel — button.rs:12\npixel: fn()"), "source has its function heading and body");

    let constants = list.get_item(0).unwrap().1.widget(cx, ids!(header));
    click(cx, constants.clone(), &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(list.get_item(1).unwrap().1.label(cx, ids!(title)).text(), "Inputs", "folding constants preserves the Inputs section");
    click(cx, constants, &mut panel, &mut s.ana);
    draw(cx);
    let color = item_of(id!(ColorRow));
    let typed = |args: &[(String, String)], prop: &str, value: &str| args == [
        ("path".into(), PATH.into()), ("prop".into(), prop.into()), ("value".into(), value.into())];
    let sent = s.app.sent.len();
    let swatch = color.widget(cx, ids!(swatch)).widget_uid();
    let red = vec4(1., 0., 0., 1.);
    let blue = vec4(0., 0., 1., 0.5);
    for step in [FabColorPickAction::Changed(red), FabColorPickAction::Changed(vec4(0., 1., 0., 1.)),
        FabColorPickAction::Changed(blue), FabColorPickAction::Ended(blue)] {
        let actions = cx.capture_actions(|cx| cx.group_widget_actions(list.widget_uid(), color.widget_uid(),
            |cx| cx.widget_action(swatch, step)));
        panel.actions(cx, &root, &actions, &mut s.ana.session);
        s.one();
    }
    assert!(matches!(&s.app.sent[sent..], [(_, _, DesignOp::Design { verb: DesignVerb::Typed, args })]
        if typed(args, "draw_bg.color", "#ff0000ff")), "a drag holds intermediate colors behind one in-flight Set");
    let draft = original_src.replace("self.color", "self.custom");
    src.set_text(cx, &draft);
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    draw(cx);
    assert!(matches!(&s.app.sent[sent+1..], [(_, _, DesignOp::Design { verb: DesignVerb::Typed, args }),
        (_, _, DesignOp::Design { verb: DesignVerb::Props, .. })] if typed(args, "draw_bg.color", "#0000ff80")),
        "the final color sends once and the drag ends with exactly one Props read");
    let history = root.portal_list(cx, ids!(tweak_history_list));
    let entry = |cx: &Cx, i| history.get_item(i).unwrap().1.label(cx, ids!(tweak_entry)).text();
    assert!(history.borrow().unwrap().range_end() == 1 && entry(cx, 0).ends_with("draw_bg.color  0 → #0000ff80 (2 steps)"),
        "the whole color drag is one history run");
    assert_eq!(src.text(), draft, "Rows preserves an unfocused, unsubmitted shader draft");
    click(cx, pill("draw_text"), &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(root.label(cx, ids!(shader_doc)).text(), "draw_text is not drawn: no live draw call (show_bg off?)", "undrawn layer explains its status");
    assert_eq!(item_of(id!(ColorRow)).label(cx, ids!(name)).text(), "draw_text.color · inst", "layer switch rebinds the editable color");
    click(cx, pill("draw_bg"), &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(src.text(), original_src, "changing layers loads the selected function");
    src.set_text(cx, &draft);
    let (sent, recorded) = (s.app.sent.len(), s.records.len());
    click(cx, root.widget(cx, ids!(shader_propose)), &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    assert!(s.app.sent[sent..].is_empty(), "proposing shader source issues zero app commands");
    assert!(matches!(&s.records[recorded..], [AppEdit { seat: ANA, prop, path, change: Change::Note(body), .. }]
        if path == PATH && prop == "draw_bg.pixel" && body == "pixel: fn() {\n  return self.custom\n}"), "the owner saves exactly the changed function body as a note");
    assert!(root.label(cx, ids!(shader_src_status)).text().contains("Saved a note for the implementer"), "source proposals receive their own status");
    for edit in &s.records[recorded..] {
        let id = model.act(ModelAction::RecordAppEdit { town: model.open_id(), item, edit: edit.clone() }, Origin::Ui).unwrap();
        panel.sync_history(cx, &root, &s.ana.session, &model, Some(id));
    }
    draw(cx);
    assert!(entry(cx, 1).starts_with("Note  ") && entry(cx, 1).contains("ui.panel.draw_bg.pixel"), "signed history shows the shader note");
    let custom = item_of(id!(TextRow));
    let field = custom.widget(cx, ids!(value));
    click(cx, field.clone(), &mut panel, &mut s.ana);
    let expr = "sin(self.pos.x)";
    field.set_text(cx, expr);
    s.app.refused_values.push(expr.into());
    let (sent, recorded) = (s.app.sent.len(), s.records.len());
    let enter = cx.capture_actions(|cx| root.handle_event(cx, &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey,
        ..Default::default() }), &mut Scope::empty()));
    panel.actions(cx, &root, &enter, &mut s.ana.session);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert!(matches!(&s.app.sent[sent..], [(_, _, DesignOp::Design { verb: DesignVerb::Typed, args })]
        if typed(args, "draw_bg.custom", expr)), "a shader text input submits its expression to design typed");
    assert!(matches!(&s.records[recorded..], [AppEdit { prop, change: Change::Note(value), .. }]
        if prop == "draw_bg.custom" && value == expr), "a refused expression becomes one note on its input");
    assert!(custom.label(cx, ids!(refusal)).text().contains("not a data value"), "the input displays its own refusal");
    click(cx, root.widget(cx, ids!(shader_src)), &mut panel, &mut s.ana);
    assert_eq!(cx.key_focus(), src.area(), "the source field receives keyboard focus");
    src.set_text(cx, &original_src.replace("self.color", "self.border_radius"));
    let (sent, recorded) = (s.app.sent.len(), s.records.len());
    let submit = cx.capture_actions(|cx| root.handle_event(cx, &Event::KeyDown(KeyEvent {
        key_code: KeyCode::ReturnKey, modifiers: KeyModifiers {
            control: !cfg!(target_vendor = "apple"), logo: cfg!(target_vendor = "apple"), ..Default::default()
        }, ..Default::default()
    }), &mut Scope::empty()));
    panel.actions(cx, &root, &submit, &mut s.ana.session);
    s.one();
    assert!(s.app.sent[sent..].is_empty() && matches!(&s.records[recorded..], [AppEdit { prop, change: Change::Note(body), .. }]
        if prop == "draw_bg.pixel" && body.contains("self.border_radius")), "Ctrl/Cmd+Enter proposes the edited function without an app command");
    let (sent, recorded) = (s.app.sent.len(), s.records.len());
    let denied = s.ben.send(Request::Set { path: PATH.into(), prop: "draw_bg.pixel".into(), value: draft });
    s.one();
    assert!(has(&s.ben, denied, |n| matches!(n, Notice::Refused(why) if why.contains("Ask the owner"))), "a peer needs Allow to propose shader changes");
    assert_eq!((s.app.sent.len(), s.records.len()), (sent, recorded), "a denied proposal neither touches the app nor records a note");
    s.app.props = r##"{"props":[{"name":"draw_icon.color","value":"#abcdef"}]}"##.into();
    s.ana.send(Request::Select(Some("older.widget".into())));
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert!(src.is_read_only() && src.text().contains("shader source unavailable from this app version")
        && !root.widget(cx, ids!(shader_propose)).visible(), "older apps expose a read-only source explanation");
    assert_eq!(item_of(id!(ColorRow)).text_input(cx, ids!(value)).text(), "#abcdef", "older apps retain literal input editors");
    click(cx, root.widget(cx, ids!(src_fold)), &mut panel, &mut s.ana);
    assert!(!root.widget(cx, ids!(shader_src)).visible() && root.button(cx, ids!(src_fold)).text() == "+ source",
        "the source well folds independently of the inputs");
    root.borrow_mut::<View>().unwrap().walk.width = Size::Fixed(105.);
    draw(cx);
    assert!(!root.widget(cx, ids!(tab_props)).visible() && root.widget(cx, ids!(tab_props_i)).visible(),
        "the tab strip switches to icons when its words no longer fit");
    click(cx, root.widget(cx, ids!(tab_props_i)), &mut panel, &mut s.ana);
    assert!(root.widget(cx, ids!(tweak_props_wrap)).visible(), "the compact Props tab still navigates");
}

#[test]
fn tree_tab_expands_lazily_selects_with_seat_colours_follows_pick_and_filters() {
    use makepad_town_core::fold::{Profile, State};
    use makepad_widgets::*;

    const CARD: &str = "/dock/card";
    const HIDDEN: &str = "/dock/hidden_modal";
    const MOVE: &str = "/dock/card/t_move";
    const DEEP: &str = "/dock/deep";
    const LEAF: &str = "/dock/deep/leaf";

    fn node(path: &str, ty: &str, children: u32, matched: bool) -> String {
        let name = path.rsplit('/').next().unwrap();
        let rect = if path == MOVE { r#","rect":[10,20,80,22]"# } else { "" };
        let visible = path != HIDDEN;
        let hit = if matched { r#","matched":true"# } else { "" };
        format!(r#"{{"path":"{path}","name":"{name}","ty":"{ty}","children":{children},"visible":{visible}{hit}{rect}}}"#)
    }
    fn block(parent: &str, offset: u32, total: u32, nodes: &[String]) -> String {
        format!(r#"{{"path":"{parent}","offset":{offset},"total":{total},"nodes":[{}]}}"#, nodes.join(","))
    }
    fn reply(mode: &str, matches: u32, truncated: bool, parents: &[String]) -> String {
        format!(r#"{{"window":0,"mode":"{mode}","total_nodes":255,"total_matches":{matches},"truncated":{truncated},"parents":[{}]}}"#,
            parents.join(","))
    }
    fn assert_op(op: &DesignOp, verb: DesignVerb, expected: &[(&str, &str)], why: &str) {
        let DesignOp::Design { verb: actual, args } = op else { panic!("{why}: expected design op") };
        assert_eq!(std::mem::discriminant(actual), std::mem::discriminant(&verb), "{why}: app verb");
        assert_eq!(args.iter().map(|(k,v)| (k.as_str(), v.as_str())).collect::<Vec<_>>(), expected, "{why}: app arguments");
    }

    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 285} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut draw = draw_panel(cx, root.clone());
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.ana.session.view.updates.clear();
    s.allow(BEN, true);
    let mut panel = panel::Panel::default();
    let mut state = State::default();
    for (seat, name, colour) in [(ANA, "Ana", 0xace021), (BEN, "Ben", 0x1865bd)] {
        state.profiles.insert(seat, Profile { name: name.into(), colour });
    }
    let dispatch = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp| {
        while let Some((seat, req, agent, notice)) = town.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut town.session, seat, req, agent, &state, &notice);
        }
    };
    let click = |cx: &mut Cx, widget: WidgetRef, panel: &mut panel::Panel, town: &mut TownApp| {
        let (_, press, release) = panel_click_at(cx, &root, widget.area().rect(cx).center(), 1.);
        panel.actions(cx, &root, &press, &mut town.session);
        panel.actions(cx, &root, &release, &mut town.session);
    };
    let tab_tree = root.widget(cx, ids!(tab_tree));
    let tree = root.portal_list(cx, ids!(tweak_tree_list));
    let row = |i| tree.get_item(i).unwrap_or_else(|| panic!("tree row {i} should be mounted")).1;
    let label = |cx: &Cx, i| row(i).button(cx, ids!(select_btn)).text();
    let roots = block("", 0, 1, &[node("/window", "Window", 1, false)]);
    let window = block("/window", 0, 1, &[node("/dock", "Dock", 250, false)]);
    let children: Vec<_> = (0..250).map(|i| match i {
        0 => node(CARD, "View", 1, false),
        1 => node(HIDDEN, "Modal", 0, false),
        60 => node(DEEP, "View", 1, false),
        _ => node(&format!("/dock/item_{i}"), "Button", 0, false),
    }).collect();
    s.app.trees.extend([
        reply("children", 0, false, &[roots.clone(), window.clone(), block("/dock", 0, 250, &children[..200])]),
        reply("children", 0, false, &[block("/dock", 200, 250, &children[200..])]),
        reply("children", 0, false, &[block(CARD, 0, 1, &[node(MOVE, "Button", 0, false)])]),
    ]);
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    draw(cx);
    click(cx, tab_tree.clone(), &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(s.app.sent.len(), 1, "opening Tree requests only the first root page");
    assert_op(&s.app.sent[0].2, DesignVerb::Tree, &[("depth","3"), ("offset","0"), ("limit","200")], "initial root");
    assert_eq!(tree.borrow().unwrap().range_end(), 203, "two open levels show the 200 children and one pager");
    assert_eq!([label(cx, 0), label(cx, 1), label(cx, 2)], ["window · Window", "dock · Dock", "card · View"],
        "prefetched parents open in depth-first order");
    assert!(tree.get_item(202).is_none(), "the initial viewport does not mount 200 off-screen widgets");

    tree.set_first_id_and_scroll(195, 0.);
    draw(cx);
    let pager = row(202).widget(cx, ids!(tree_page));
    assert_eq!(pager.text(), "+ 50 more", "the dock pager reports the remaining children");
    click(cx, pager, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert_op(&s.app.sent[1].2, DesignVerb::Tree, &[("path","/dock"), ("depth","1"), ("offset","200"), ("limit","200")], "next dock page");
    assert_eq!(tree.borrow().unwrap().range_end(), 252, "the appended page replaces the pager");
    assert_eq!(label(cx, 202), "item_200 · Button", "pagination joins the next page at the requested offset");

    tree.set_first_id_and_scroll(0, 0.);
    draw(cx);
    click(cx, row(2).widget(cx, ids!(twisty)), &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(row(3).label(cx, ids!(tree_info)).text(), "loading…", "a fresh child page has a loading row");
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert_op(&s.app.sent[2].2, DesignVerb::Tree, &[("path",CARD), ("depth","1"), ("offset","0"), ("limit","200")], "lazy card children");
    assert_eq!(label(cx, 3), "t_move · Button", "expanding card mounts its child");
    click(cx, row(2).widget(cx, ids!(twisty)), &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(label(cx, 3), "hidden_modal · Modal", "collapse removes the subtree immediately");
    click(cx, row(2).widget(cx, ids!(twisty)), &mut panel, &mut s.ana);
    draw(cx);
    s.one();
    assert_eq!(s.app.sent.len(), 3, "collapse and cached re-expansion issue zero app requests");
    assert!(s.ana.session.view.selections.is_empty(), "expander clicks never select a widget");

    s.ben.send(Request::Select(Some(CARD.into())));
    click(cx, row(3).widget(cx, ids!(select_btn)), &mut panel, &mut s.ana);
    s.ana.poll();
    let outgoing = s.host.relay.net.borrow().inbox[&OWNER].clone();
    assert!(outgoing.iter().any(|(peer, _, body)| *peer == ANA && matches!(
        wire::decode(ANA, body).unwrap().unwrap().body, Body::Request(Request::Select(Some(path))) if path == MOVE)),
        "a click submits Select for the exact painted path");
    s.exchange();
    let rect = super::Rect {window: 0, x: 10., y: 20., w: 80., h: 22.};
    for seat in [&s.ana, &s.ben] {
        assert!(seat.session.view.updates.iter().any(|(actor, _, _, n)| *actor == ANA && matches!(n,
            Notice::Selected {path: Some(p), rect: Some(r)} if p == MOVE && *r == rect)), "both seats receive the selected widget's rect");
    }
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert_op(&s.app.sent[3].2, DesignVerb::Props, &[("path",MOVE)], "drawn selection loads Props");
    for (i, seat) in [(2, BEN), (3, ANA)] {
        let dot = row(i).view(cx, ids!(dot_a));
        assert!(dot.visible() && !row(i).widget(cx, ids!(dot_b)).visible(), "each collaborator has a dot on their own row");
        let mut rgba = [0.; 4];
        dot.borrow().unwrap().draw_bg.get_instance(cx, id!(color), &mut rgba);
        let c = super::overlay::seat_colour(&state, &seat);
        assert_eq!(rgba, [c.x,c.y,c.z,c.w], "the dot uses the seat's picture-ring colour");
    }
    click(cx, root.widget(cx, ids!(tab_props)), &mut panel, &mut s.ana);
    draw(cx);
    let props = root.portal_list(cx, ids!(tweak_properties_list));
    let identity = (0..props.borrow().unwrap().range_end()).filter_map(|i| props.get_item(i))
        .find(|(id, _)| *id == id!(IdentityRow)).expect("Props mounts an identity row").1;
    assert_eq!(identity.text_input(cx, ids!(name_field)).text(), "t_move", "tree selection supplies the exact widget name");
    assert_eq!(identity.label(cx, ids!(type_label)).text(), "Button", "tree selection supplies the type before a picture pick");
    click(cx, tab_tree.clone(), &mut panel, &mut s.ana);
    draw(cx);
    click(cx, row(4).widget(cx, ids!(select_btn)), &mut panel, &mut s.ana);
    s.exchange();
    assert!(s.ana.session.view.updates.iter().any(|(_, _, _, n)| matches!(n,
        Notice::Selected {path: Some(p), rect: None} if p == HIDDEN)), "undrawn widgets select without a picture rect");
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    assert_op(&s.app.sent[4].2, DesignVerb::Props, &[("path",HIDDEN)], "undrawn selection loads Props");

    s.app.picks.push_back(Ok(format!(r#"{{"window":0,"rect":[2,4,12,16],"path":"dock.deep.leaf","ref":"{LEAF}","type":"Label"}}"#)));
    s.app.trees.push_back(reply("reveal", 0, false, &[roots.clone(), window.clone(), block("/dock", 0, 250, &children[..200]),
        block(DEEP, 0, 1, &[node(LEAF, "Label", 0, false)])]));
    s.ana.send(Request::Pick(At::Point {window: 0, x: 4., y: 8.}));
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    let after_pick = &s.app.sent[6..];
    assert_eq!(after_pick.len(), 2, "one picture pick issues one Reveal and one Props");
    let reveal = after_pick.iter().find(|(_, _, op)| matches!(op, DesignOp::Design {verb: DesignVerb::Tree, ..})).unwrap();
    assert_op(&reveal.2, DesignVerb::Tree, &[("reveal",LEAF), ("limit","200")], "follow uncached pick");
    assert!(tree.first_id() > 0 && label(cx, 64) == "leaf · Label", "Reveal opens the deep branch and scrolls its leaf into view");

    let query = |hits: &[String], truncated| reply("query", 2, truncated, &[roots.clone(), window.clone(),
        block("/dock", 0, 1, &[node(CARD, "View", 1, false)]), block(CARD, 0, hits.len() as u32, hits)]);
    s.app.trees.extend([reply("query", 0, false, &[]), query(&[node(MOVE, "Button", 0, true)], true),
        query(&[node("/dock/card/move_again", "Button", 0, true)], false)]);
    let input = root.widget(cx, ids!(tree_filter.input));
    click(cx, input.clone(), &mut panel, &mut s.ana);
    let input_event = |cx: &mut Cx, event, panel: &mut panel::Panel, town: &mut TownApp| {
        let actions = cx.capture_actions(|cx| root.handle_event(cx, &event, &mut Scope::empty()));
        panel.actions(cx, &root, &actions, &mut town.session);
    };
    for letter in ["m", "o", "v"] {
        input_event(cx, Event::TextInput(TextInputEvent {input: letter.into(), ..Default::default()}), &mut panel, &mut s.ana);
    }
    let before = s.app.sent.len();
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    s.one();
    draw(cx);
    assert_eq!(s.app.sent.len(), before + 2, "three keystrokes issue only the first and last queries");
    for (i, q) in ["m", "mov"].into_iter().enumerate() {
        assert_op(&s.app.sent[before+i].2, DesignVerb::Tree, &[("query",q), ("offset","0"), ("limit","200")], "latest-only search");
    }
    assert_eq!([label(cx, 0), label(cx, 1), label(cx, 2), label(cx, 3)],
        ["window · Window", "dock · Dock", "card · View", "t_move · Button"], "search keeps the ancestor chain open");
    let more = row(4).widget(cx, ids!(tree_page));
    assert_eq!(more.text(), "+ load more matches (1 of 2)", "match pagination counts hits excluding ancestors");
    click(cx, more, &mut panel, &mut s.ana);
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert_op(&s.app.sent[before+2].2, DesignVerb::Tree, &[("query","mov"), ("offset","1"), ("limit","200")], "search pager");
    assert_eq!(label(cx, 4), "move_again · Button", "more matches merge under their original ancestor once");
    click(cx, input.clone(), &mut panel, &mut s.ana);
    s.exchange();
    input_event(cx, Event::KeyDown(KeyEvent {key_code: KeyCode::Escape, ..Default::default()}), &mut panel, &mut s.ana);
    draw(cx);
    assert_eq!(input.text(), "", "Escape clears the field");
    assert_eq!(tree.borrow().unwrap().range_end(), 254, "Escape restores both dock pages and expanded branches immediately");
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).is_empty(), "clearing the filter sends zero packets");

    for level in [Some(TREE_LEVEL - 1), None] {
        let mut older = Copies::new(OWNER, ANA, ITEM);
        older.exchange();
        older.ana.session.set_peer_level(OWNER, level);
        let mut panel = panel::Panel::default();
        panel.refresh(cx, &root, &older.ana.session, &state, &Default::default());
        draw(cx);
        click(cx, tab_tree.clone(), &mut panel, &mut older.ana);
        older.ana.poll();
        assert!(older.host.relay.take(TWEAK).is_empty(), "known old or awaiting Hello sends zero packets");
        if level.is_none() {
            draw(cx);
            assert_eq!(row(0).label(cx, ids!(tree_info)).text(), "loading…", "unknown Hello keeps the root query pending");
            older.ana.session.set_peer_level(OWNER, Some(TREE_LEVEL - 1));
            dispatch(cx, &mut panel, &mut older.ana);
        }
        draw(cx);
        assert_eq!(row(0).label(cx, ids!(tree_info)).text(), OLDER_SHARER_TREE, "both refusal paths display the session's upgrade message");
        older.ana.poll();
        assert!(older.host.relay.take(TWEAK).is_empty(), "the refused root never reaches the old sharer");
    }
}
