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
const fn seat(tag: u8) -> Seat {
    let mut key = [tag; 65];
    key[0] = 4;
    key
}
const OWNER: Seat = seat(1);
const ANA: Seat = seat(2);
const ANA_AGENT: [u8; 32] = [42; 32];
const BEN: Seat = seat(3);
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
    theme_base: Option<json::Value>,
    props: String,
    tree: String,
    trees: VecDeque<String>,
    /// Values the app answers with an error, as a real app refuses what is not a value of the type.
    refused_values: Vec<String>,
    design_source: Option<FakeDesignSource>,
    other_source: Option<FakeDesignSource>,
    /// Scripted pick answers, in order; the default pick follows once they run out.
    picks: VecDeque<Result<String, String>>,
    typed_rects: VecDeque<Rect>,
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
                    let mut fields = vec![("old", json::obj(vec![(prop, json::s(old))])),
                        ("new", json::obj(vec![(prop, json::s(new))]))];
                    if let Some(rect) = self.typed_rects.pop_front() {
                        fields.push(("window", json::Value::Int(rect.window as i64)));
                        fields.push(("rect", json::Value::Arr([rect.x, rect.y, rect.w, rect.h].map(json::Value::F64).to_vec())));
                    }
                    json::obj(fields).to_json()
                } else {
                    ok = false;
                    format!("not a data value: {value}")
                }
            }
            DesignVerb::Palette | DesignVerb::Drop | DesignVerb::Locate | DesignVerb::Preview | DesignVerb::Verify | DesignVerb::Revert | DesignVerb::PreviewChunk => {
                let source = self.other_source.as_mut().filter(|other| args.iter().any(|(k,v)|
                    k == "file" && *v == other.file || k == "path" && v.starts_with("/other/")));
                source.or(self.design_source.as_mut()).expect("structural test source").answer(*verb, args)
                    .unwrap_or_else(|why| { ok = false; why })
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
            let snapshot = parse_json(&self.theme)?;
            let original = self.theme_base.get_or_insert_with(|| snapshot.clone());
            let sheet = spec.get("sheet").ok_or("missing sheet")?;
            let sheets = snapshot.get("sheets").unwrap();
            if sheet != &json::Value::Null && !sheets.as_arr().unwrap().iter().any(|s| s.get("id") == Some(sheet)) {
                return Err("unknown sheet".into())
            }
            let globals = spec.get("globals").unwrap().as_arr().unwrap();
            let overrides = spec.get("overrides").unwrap().as_arr().unwrap();
            let mut values = original.get("values").unwrap().clone();
            if let json::Value::Arr(tokens) = &mut values {
                for pair in globals.iter().chain(overrides) {
                    let pair = pair.as_arr().unwrap();
                    if let Some(json::Value::Obj(fields)) = tokens.iter_mut().find(|t| t.get("name") == pair.first()) {
                        fields.iter_mut().find(|(k, _)| k == "value").unwrap().1 =
                            json::s(pair[1].as_str().map(str::to_owned).unwrap_or_else(|| pair[1].to_json()));
                    }
                }
            }
            self.theme = json::obj(vec![("base", json::s(base)), ("sheet", sheet.clone()), ("sheets", sheets.clone()),
                ("has_mix", json::Value::Bool(sheet == &json::Value::Null && !(globals.is_empty() && overrides.is_empty()))),
                ("has_pins", json::Value::Bool(sheet != &json::Value::Null && !overrides.is_empty())),
                ("edits", json::Value::Arr(vec![])), ("values", values)]).to_json();
            return Ok(json::obj(vec![("ok", json::Value::Bool(true)), ("base", json::s(base)), ("sheet", sheet.clone())]).to_json())
        }
        let name = arg("name").ok_or("missing token")?;
        if self.theme_base.is_none() { self.theme_base = Some(parse_json(&self.theme)?); }
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

struct Copies { host: TownApp, ana: TownApp, ben: TownApp, rights: ControlRights, app: FakeApp, records: Vec<AppEdit> }
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
            rights: ControlRights::default(),
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
    fn allow(&mut self, seat: Seat, yes: bool) {
        if yes { self.rights.seats.insert(seat); } else { self.rights.seats.remove(&seat); }
        self.host.session.set_control_rights(self.rights.clone());
        self.exchange();
    }
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
        state.profiles.insert(seat, Profile { writer: [0; 32], name: name.into(), colour: 0 });
    }
    state.agents.insert(ANA_AGENT, Agent { owner: ANA, name: "Stylist".into(), kind: String::new(), custom_name: None });
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
fn control_right_is_the_tweak_right() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_agents(&ana_agent_state(OWNER));
    s.list();
    s.allow(ANA, true);
    for guest in [&s.host, &s.ana, &s.ben] {
        assert_eq!(guest.session.view.allowed, [ANA].into(), "the owner and guests see Ana's control right");
    }
    let applied = |town: &TownApp, req, value: &str| has(town, req, |n| matches!(n, Notice::Applied { new, .. } if new == value));
    let refused = |town: &TownApp, req| has(town, req, |n| matches!(n, Notice::Refused(why) if why == "Ask for control to edit this share."));
    let ana = s.ana.send(set("1"));
    let ben = s.ben.send(set("2"));
    s.one();
    assert!(applied(&s.ana, ana, "1") && refused(&s.ben, ben), "only Ana can edit with individual control");
    let ai = s.ana.session.request(Some(ANA_AGENT), set("3")).unwrap();
    s.one();
    assert!(applied(&s.ana, ai, "3"), "Ana's AI inherits her individual control");

    s.rights = ControlRights { all: true, seats: Default::default() };
    s.host.session.set_control_rights(s.rights.clone());
    s.exchange();
    for guest in [&s.host, &s.ana, &s.ben] {
        assert_eq!(guest.session.view.allowed, [ANA, BEN].into(), "the owner and guests see Allow all");
    }
    let ben = s.ben.send(set("4"));
    s.one();
    assert!(applied(&s.ben, ben, "4"), "Allow all gives Ben control");
    let ai = s.ana.session.request(Some(ANA_AGENT), set("5")).unwrap();
    s.one();
    assert!(applied(&s.ana, ai, "5"), "Allow all covers Ana's AI");

    let queued = s.ana.send(set("6"));
    s.exchange();
    s.rights = ControlRights::default();
    s.host.session.set_control_rights(s.rights.clone());
    s.exchange();
    for guest in [&s.host, &s.ana, &s.ben] {
        assert!(guest.session.view.allowed.is_empty(), "the owner and guests see control end");
        for seat in [ANA, BEN] {
            assert!(guest.session.view.updates.iter().any(|(who, _, _, n)| *who == seat && *n == Notice::Allowed(false)),
                "each seat's loss of control reaches both guests");
        }
    }
    assert!(has(&s.ana, queued, |n| matches!(n, Notice::Refused(why) if why == "Control ended. Ask for control to edit this share.")),
        "ending control refuses Ana's queued edit");
    let ai = s.ana.session.request(Some(ANA_AGENT), set("7")).unwrap();
    s.one();
    assert!(refused(&s.ana, ai), "Ana's AI loses edit access when control ends");
    assert_eq!(s.app.padding, "5", "the queued and refused edits never reached the app");
}

#[test]
fn revoking_control_refuses_queued_edits_and_lets_the_running_one_finish() {
    let fixture = DesignTestDir::new("control-revoke");
    let disk = "script_mod! { mod.UI = View{ root := View{} } }";
    let (mut s, _cx) = structural_share(&fixture.0, disk);
    s.allow(ANA, true);
    guest_design(&mut s, drag_into("/root"));
    assert!(s.host.session.view.design.ghost.is_some());
    s.list();
    let running = s.ana.send(set("1"));
    let queued = s.ana.send(set("2"));
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    s.allow(ANA, false);
    let later = s.ana.send(set("3"));
    s.finish();
    settle_design(&mut s);
    for town in [&s.host, &s.ana, &s.ben] {
        let notices = &town.session.view.updates;
        let refusal = notices.iter().position(|(seat, req, _, n)| *seat == ANA && *req == queued
            && matches!(n, Notice::Refused(why) if why == "Control ended. Ask for control to edit this share.")).unwrap();
        let revoked = notices.iter().position(|(seat, _, _, n)| *seat == ANA && *n == Notice::Allowed(false)).unwrap();
        assert!(refusal < revoked, "queued edits are refused before loss of control is announced");
        assert!(has(town, running, |n| matches!(n, Notice::Applied { new, .. } if new == "1")));
        assert!(has(town, later, |n| matches!(n, Notice::Refused(why) if why == "Ask for control to edit this share.")));
        assert!(town.session.view.design.ghost.is_none(), "revocation retracts the held ghost");
    }
    assert!(matches!(&s.records[..], [AppEdit { seat: ANA, change: Change::Applied { new, .. }, .. }] if new == "1"));
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk);
}

#[test]
fn allow_all_covers_a_member_who_joins_later() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_members([OWNER, ANA]);
    s.rights.all = true;
    s.host.session.set_control_rights(s.rights.clone());
    s.exchange();
    assert_eq!(s.host.session.view.allowed, [ANA].into());
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.exchange();
    assert_eq!(s.ben.session.view.allowed, [ANA, BEN].into(), "the new member receives the snapshot and their own right");
    s.list();
    let edit = s.ben.send(set("9"));
    s.one();
    assert!(has(&s.ben, edit, |n| matches!(n, Notice::Applied { new, .. } if new == "9")));
    s.rights.all = false;
    s.allow(ANA, true);
    assert_eq!(s.ben.session.view.allowed, [ANA].into());
    assert!(s.ben.session.view.updates.iter().any(|(seat, _, _, n)| *seat == BEN && *n == Notice::Allowed(false)));
}

#[test]
fn a_member_who_leaves_and_returns_follows_the_given_right() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.host.session.set_members([OWNER, BEN]);
    s.exchange();
    assert!(s.ben.session.view.allowed.is_empty());
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.exchange();
    let transitions: Vec<_> = s.ben.session.view.updates.iter().filter_map(|(seat, _, _, n)| match n {
        Notice::Allowed(yes) if *seat == ANA => Some(*yes),
        _ => None,
    }).collect();
    assert_eq!(transitions, [true, false, true], "one named right is reconciled against each roster");
    assert_eq!(s.ana.session.view.allowed, [ANA].into());
}

#[test]
fn three_seats_share_order_presence_consent_and_retry() {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    let no_pick = s.ben.send(Request::Pick(At::Path(PATH.into())));
    let no_props = s.ben.send(Request::Props { path: PATH.into() });
    s.one();
    assert!(s.app.sent.is_empty(), "reading the running app requires Allow");
    for req in [no_pick, no_props] {
        assert!(has(&s.ben, req, |n| matches!(n, Notice::Refused(v) if v.contains("Ask"))), "denied reads explain how to ask");
    }
    s.host.session.set_agents(&ana_agent_state(OWNER));
    let denied_ai = s.ana.session.request(Some(ANA_AGENT), Request::Props { path: PATH.into() }).unwrap();
    s.one();
    assert!(has(&s.ana, denied_ai, |n| matches!(n, Notice::Refused(why) if why == "Ask for control to edit this share.")),
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
    s.exchange();
    assert!(has(&s.ana, no_select, |n| matches!(n, Notice::Refused(why) if why == "Ask for control to edit this share.")),
        "AI presence requires its person's control right");
    assert_eq!(s.host.session.view.agents.get(&actor), Some(&false), "a verified first request introduces an idle AI");
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
    let stopped_look = ai(&mut s.ana, Request::Look);
    let person_select = s.ana.send(select());
    s.exchange();
    for town in [&s.host, &s.ana, &s.ben] {
        assert_eq!(town.session.view.agents.get(&actor), Some(&true), "all three seats see the AI stopped");
        assert_eq!(town.session.view.running.get(&(ANA, Some(ANA_AGENT))), Some(&(running, set("4"))),
            "refusals for this AI's other requests preserve its in-flight Set");
        assert!(town.session.view.looking.is_empty(), "Stop clears the look on every seat");
        for req in [queued, refused, interrupted, stopped_look] {
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
    state.agents.insert(sibling, makepad_town_core::fold::Agent { owner: ANA, name: "Painter".into(), kind: String::new(), custom_name: None });
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
    assert!(has(&s.ana, revoked, |n| matches!(n, Notice::Refused(why) if why == "Ask for control to edit this share.")),
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

// These existing tool calls exercise no theme I/O. Theme scenarios below pass their member's scratch directory and persistent cache.
macro_rules! unused_theme_context {
    () => { crate::tools::ThemeToolContext { themes_dir: &std::env::temp_dir().join("town-unused-theme-store"),
        mixer_cache: &mut Default::default(), active_worn: None } };
}

fn list_apps(model: &mut Model, tools: &mut AppTools, sessions: &mut [Session<u64, u64>], waits: &mut crate::tools::AgentWaits,
    binding: &str, after: Option<&str>) -> makepad_ai_services::wire::ToolResult {
    let mut args = vec![("binding", json::s(binding))];
    if let Some(after) = after { args.push(("after", json::s(after))); }
    let call = app_tool("app_list", &json::obj(args).to_json()).unwrap();
    tools.start("list", call, model, sessions, waits, unused_theme_context!()).0.expect("app discovery must answer immediately")
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
            } else if let Some((out, _, _)) = tools.answer(&mut town.session, seat, req, actor, &notice, waits, unused_theme_context!()) {
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
    let out = tools.start("local", look(&hex(&own_item)), model, std::slice::from_mut(&mut own), waits, unused_theme_context!()).0.unwrap();
    assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, OWN_LOOK), "an own share names its local tool even without a picture");
    assert!(own.view.updates.is_empty(), "the own-share refusal precedes any request");
    s.host.session.bind_window(WindowId(3));
    let start = |call, tools: &mut AppTools, model: &mut Model, waits: &mut AgentWaits, s: &mut Copies| {
        let before = s.app.stills.len();
        let sent = tools.start(call, look(&item), model, std::slice::from_mut(&mut s.ana.session), waits, unused_theme_context!());
        assert_eq!(sent, (None, Some(token), None), "a guest with no share on screen holds its look for the sharer");
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
    for name in ["app_allow", "app_revoke", "app_stop", "app_stop_agent", "app_allow_agent", "app_theme_allow"] {
        assert!(app_tool(name, "{}").is_none() && crate::tools::read(name, "{}").is_err(), "consent has no {name} tool");
        assert!(manifest.tools.iter().all(|tool| tool.name != name), "the manifest exposes no {name} consent tool");
    }
    let calls = [("app_list", ""), ("app_look", ""), ("app_pick", r#","path":"ui.panel""#), ("app_props", r#","path":"ui.panel""#),
        ("app_set", r#","path":"ui.panel","prop":"padding","value":"18""#), ("app_select", r#","path":null"#), ("app_edits", ""),
        ("app_theme_get", ""), ("app_theme_set", r##","name":"color_bg_app","value":"#abc""##),
        ("app_theme_install", r#","preset":"dark""#), ("app_theme_save", r#","name":"my_sunset""#),
        ("app_theme_mix", r#","surprise":true"#), ("app_theme_build", ""), ("app_theme_delete", r#","name":"my_sunset""#),
        ("app_theme_palettes", r##","color":"#ff5c39""##)];
    let (_, unknown) = Credential::issue(model.open_id()).unwrap();
    for (name, fields) in calls {
        for (arg, why) in [(String::new(), "Invalid `binding`."), (format!(r#","binding":"{unknown}""#), "This binding has ended.")] {
            let parsed = if matches!(name, "app_list" | "app_theme_delete" | "app_theme_palettes") {
                app_tool(name, &format!("{{{}}}", format!("{fields}{arg}").trim_start_matches(','))) }
                else { app_tool(name, &format!(r#"{{"item":"{item}"{fields}{arg}}}"#)) }.unwrap();
            let out = tools.start(name, parsed, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!()).0.unwrap();
            assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, why), "{name} must explain the missing or unknown binding");
        }
    }
    let token = Credential::parse(&binding).unwrap();
    let peer_line = owner.mention_agent(agent, "inspect the panel".into(), None).unwrap();
    crate::model::tests::exchange(&mut owner, &mut model);
    model.decide_agent_request(model.open_id(), agent, peer_line, true).unwrap();
    waits.offered(&model, peer_line);
    let cursor = |reply: &crate::ai_out::TownToolResult| {
        let page = parse_json(reply.text_for_test()).unwrap();
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
            let parsed = if matches!(name, "app_list" | "app_theme_delete" | "app_theme_palettes") { app_tool(name, &format!(r#"{{"binding":"{binding}"{fields}}}"#)) }
                else { app_tool(name, &format!(r#"{{"binding":"{binding}","item":"{item}"{fields}}}"#)) }.unwrap();
            let out = tools.start(name, parsed, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!()).0.unwrap();
            assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, "Your person hasn't asked you anything."),
                "{name} requires an owner turn, including after an approved peer request is picked up");
        }
        assert!(waits.traces.rows(&peer_line)[0].output.iter().all(|o| o.ink != Ink::Action),
            "refused tools record no action on an idle or peer-authored trace");
    }
    let line = model.mention_agent(agent, "tweak the shared app".into(), None).unwrap();
    waits.offered(&model, line);
    let page = waits.wait(&mut model, "own", token, after_peer);
    assert_eq!(page[0].outcome(), ToolOutcome::Ok, "the AI picked up its own person's request");
    let invalid = app_tool("app_set", &format!(r#"{{"binding":"{binding}","item":"{item}","path":"ui","prop":"p","value":"2","allow":true}}"#)).unwrap();
    let (out, changed, _) = tools.start("invalid", invalid, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!());
    assert_eq!((out.unwrap().outcome, changed), (ToolOutcome::Refused, Some(token)), "a tool cannot smuggle an allow flag");
    assert_trace(&waits, line, 0, &[(Ink::Warning, "app_set refused: unexpected argument `allow`")],
        "a pre-send refusal adds only its warning to the owner's line");

    check_guest_looks(&mut model, &mut tools, &mut waits, &mut s, &binding, line, agent, board);

    let set = || app_tool("app_set", &format!(r#"{{"binding":"{binding}","item":"{item}","path":"ui.panel","prop":"padding","value":"18"}}"#)).unwrap();
    let since = waits.traces.rows(&line)[0].output.len();
    let (out, changed, _) = tools.start("denied", set(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!());
    assert!(out.is_none() && changed == Some(token), "a sent Set awaits the sharer's decision");
    s.one();
    while let Some((seat, req, actor, notice)) = s.ana.session.view.updates.pop_front() {
        if let Some((result, changed, _)) = tools.answer(&mut s.ana.session, seat, req, actor, &notice, &mut waits, unused_theme_context!()) {
            assert_eq!((result.outcome, changed), (ToolOutcome::Refused, Some(token)), "the sharer's refusal refreshes the requesting AI's trace");
        }
    }
    assert_trace(&waits, line, since, &[(Ink::Action, "proposing padding 18 on panel"),
        (Ink::Warning, "app_set refused: Ask for control to edit this share.")],
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
    let wrong = tools.start("wrong", props, &mut model, &mut wrong_town, &mut waits, unused_theme_context!()).0.unwrap();
    assert_eq!(wrong.outcome, ToolOutcome::Refused, "a matching signed item cannot route a request into another town");
    let edits = app_tool("app_edits", &format!(r#"{{"binding":"{binding}","item":"{}"}}"#, hex(&private_app))).unwrap();
    let wrong = tools.start("wrong", edits, &mut model, &mut wrong_town, &mut waits, unused_theme_context!()).0.unwrap();
    assert_eq!(wrong.outcome, ToolOutcome::Refused, "the open town's private history is outside the binding");
    for (name, fields) in calls.into_iter().filter(|(n, _)| n.starts_with("app_theme_")) {
        let args = format!(r#"{{"binding":"{binding}","item":"{item}"{fields}}}"#);
        let reply = tools.start(name, app_tool(name, &args).unwrap(), &mut model, &mut wrong_town, &mut waits, unused_theme_context!()).0.unwrap();
        let expected = if matches!(name, "app_theme_delete" | "app_theme_palettes") { "unexpected argument `item`" } else { "no active share" };
        assert!(reply.outcome == ToolOutcome::Refused && reply.text.contains(expected), "{name} respects its town and item scope: {}", reply.text);
    }
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
        let (out, changed, _) = tools.start(name, tool, &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!());
        assert!(out.is_none() && changed == Some(token), "AI calls wait for their own result and refresh the starting trace");
        assert_trace(&waits, line, since, &[(Ink::Action, start)], "a sent request records only its start before the notice");
        s.one();
        let mut answers = Vec::new();
        while let Some((seat, req, actor, notice)) = s.ana.session.view.updates.pop_front() {
            assert!(tools.answer(&mut s.ana.session, owner.me(), req, actor, &notice, &mut waits, unused_theme_context!()).is_none(),
                "another seat's notice cannot settle {name}");
            assert!(tools.answer(&mut s.ana.session, seat, req, Some([99; 32]), &notice, &mut waits, unused_theme_context!()).is_none(),
                "a different AI's notice cannot settle {name}");
            if let Some((out, changed, _)) = tools.answer(&mut s.ana.session, seat, req, actor, &notice, &mut waits, unused_theme_context!()) {
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
    let out = tools.start("history", history_tool(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!()).0.unwrap();
    assert_eq!(out.outcome, ToolOutcome::Ok, "a bound agent can read signed edit history");
    assert_trace(&waits, line, since, &[(Ink::Action, "read Scene's edit history")], "history records one immediate result");
    let history = parse_json(&out.text).unwrap();
    let [row] = history.get("edits").unwrap().as_arr().unwrap() else { panic!("expected one signed edit") };
    assert_eq!(row.get("agent"), Some(&json::s(hex(&agent))), "the history API reports the same agent in hex");
    assert_eq!(row.get("ai"), Some(&json::Value::Bool(true)), "the history API recognizes a new AI edit from its agent id");
    assert_eq!(model.open_id(), private, "discovery, the live calls and history all leave the displayed town alone");
    model.open_town(town).unwrap();
    let since = waits.traces.rows(&line)[0].output.len();
    let (out, _, _) = tools.start("late", set(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!());
    assert!(out.is_none(), "the Set is still held on the original owner's turn");
    assert_trace(&waits, line, since, &[(Ink::Action, "proposing padding 18 on panel")], "the original line owns the Set start");
    let next = model.mention_agent(agent, "a different request".into(), None).unwrap();
    waits.offered(&model, next);
    let reply = waits.wait(&mut model, "next", token, Some(cursor(&page[0])));
    assert_eq!(reply[0].outcome(), ToolOutcome::Ok, "the binding picked up the next request before the Set answer");
    assert_eq!(model.owner_turn(token), Ok((agent, next)), "the next request is now the binding's authorized turn");
    s.one();
    let late = std::mem::take(&mut s.ana.session.view.updates);
    let replies: Vec<_> = late.into_iter().filter_map(|(seat, req, actor, n)|
        tools.answer(&mut s.ana.session, seat, req, actor, &n, &mut waits, unused_theme_context!())).collect();
    let [(out, changed, _)] = replies.as_slice() else { panic!("the late notice must resolve its one held Set") };
    assert_eq!((out.call_id.as_str(), out.outcome, *changed), ("late", ToolOutcome::Ok, None), "a late answer resolves its call without repainting either turn");
    assert_trace(&waits, next, 0, &[], "the new request never inherits the last Set's outcome");
    assert_trace(&waits, line, since, &[(Ink::Action, "proposing padding 18 on panel")], "a finished trace drops late output");

    s.ana.session.end("the sharer closed the app");
    let (out, changed, _) = tools.start("closed", set(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!());
    assert_eq!((out.unwrap().outcome, changed), (ToolOutcome::Refused, Some(token)), "a closed share refuses a Set before submission");
    assert_trace(&waits, next, 0, &[(Ink::Warning, "app_set refused: share ended")], "a failed session request adds only its warning");
    let out = list_apps(&mut model, &mut tools, std::slice::from_mut(&mut s.ana.session), &mut waits, &binding, None);
    assert_eq!(parse_json(&out.text), parse_json(r#"{"apps":[],"next":null}"#), "an ended session disappears while its signed App still exists");
    assert_trace(&waits, next, 1, &[(Ink::Action, "listed 0 shared apps")], "an empty discovery page records zero apps");
    check_app_list_pages(&mut model, &mut tools, &mut waits, &binding, next);

    model.remove_agent(agent).unwrap();
    let out = tools.start("inactive", history_tool(), &mut model, std::slice::from_mut(&mut s.ana.session), &mut waits, unused_theme_context!()).0.unwrap();
    assert_eq!((out.outcome, out.text.as_str()), (ToolOutcome::Refused, "This binding has ended."),
        "removing the agent immediately ends its binding's access to history");
}

#[test]
fn wire_design_roundtrip_and_unknown_subtag_skip() {
    use DesignRequest::*;
    let packet = |body| Packet { share: SHARE, sender: OWNER, req: 73, agent: Some(ANA_AGENT), body };
    let request = |r| packet(Body::Request(Request::Design(r)));
    let notice = |n| packet(Body::Notice { seat: ANA, delivery: 19, notice: n });
    let req_at = packet(Body::Ready).encode().len();
    let notice_at = notice(Notice::Looked).encode().len();
    let roundtrip = |p: Packet, tag, at: usize, subtag| {
        let mut bytes = p.encode();
        assert_eq!((bytes[1], bytes[at], wire::min_level(&bytes)), (tag, subtag, DESIGN_LEVEL));
        makepad_town_core::tlv::write_tlv(&mut bytes, 0x7f, b"future");
        assert_eq!(wire::decode(OWNER, &bytes), Ok(Some(p)), "round trip for {tag}/{subtag}");
    };

    let path = "/dock/card";
    let target = "/dock/other";
    let file = "src/main.splash";
    let props = vec![("draw_bg.color".into(), "#abc".into()), ("text".into(), "Hello, 世界".into())];
    for (subtag, r) in [
        (1, State { path: None }),
        (2, Palette { query: Some("view".into()), offset: 70_000, limit: wire::MAX_PALETTE_LIMIT }),
        (3, DropTarget { window: 4, x: -0.25, y: 128.5, exclude: Some(path.into()) }),
        (4, Diff { file: Some(file.into()), offset: u32::MAX, limit: wire::MAX_DIFF_LIMIT }),
        (5, Verify { path: Some(path.into()) }),
        (6, Insert { path: path.into(), place: DesignPlace::Inside, ty: "mod.View".into(), name: Some("_new".into()), props }),
        (7, Move { path: path.into(), target: target.into(), place: DesignPlace::Before }),
        (8, Step { path: path.into(), dir: StepDir::Up }),
        (8, Step { path: path.into(), dir: StepDir::Down }),
        (8, Step { path: path.into(), dir: StepDir::Out }),
        (9, Delete { path: path.into(), force_rust_refs: true }),
        (10, Duplicate { path: path.into(), name: None }),
        (11, Wrap { path: path.into(), ty: "View".into(), name: Some("wrapper".into()) }),
        (12, Rename { path: path.into(), name: "content".into(), force_rust_refs: false }),
        (13, Bake { scope: BakeScope::Props, path: Some(path.into()), prop: Some("draw_bg.color".into()) }),
        (13, Bake { scope: BakeScope::Theme, path: None, prop: None }),
        (13, Bake { scope: BakeScope::All, path: None, prop: None }),
        (14, Undo), (15, Redo), (16, Reset), (17, WritePatch { file: Some(file.into()) }),
        (18, GhostInsert { target: target.into(), place: DesignPlace::After, ty: "Label".into() }),
        (19, GhostMove { source: path.into(), target: target.into(), place: DesignPlace::Inside }),
        (20, GhostCommit), (21, GhostCancel),
    ] {
        roundtrip(request(r), 29, req_at, subtag);
    }
    let reply = |kind| notice(Notice::Design(DesignNotice::Reply { kind, json: r#"{"rows":[]}"#.into() }));
    for (i, kind) in [DesignReplyKind::State, DesignReplyKind::Palette, DesignReplyKind::DropTarget,
        DesignReplyKind::Diff, DesignReplyKind::Verify, DesignReplyKind::WritePatch].into_iter().enumerate() {
        let p = reply(kind);
        assert_eq!(p.encode()[notice_at + 1], i as u8 + 1, "published reply kind");
        roundtrip(p, 30, notice_at, 2);
    }
    for (subtag, n) in [
        (1, DesignNotice::Status(r#"{"active":true}"#.into())),
        (3, DesignNotice::Mutated { op: "rename".into(), path: path.into(), focus_path: Some(target.into()),
            seq: u64::MAX, summary: "Renamed card".into() }),
        (4, DesignNotice::Running { summary: "Moving card".into() }),
    ] { roundtrip(notice(Notice::Design(n)), 30, notice_at, subtag); }

    for (p, at) in [(request(Undo), req_at), (reply(DesignReplyKind::State), notice_at),
        (reply(DesignReplyKind::State), notice_at + 1)] {
        let mut bytes = p.encode();
        bytes[at] = 0xff;
        assert_eq!(wire::decode(OWNER, &bytes), Ok(None), "skip the whole frame for an unknown kind");
    }
    let mut nested = notice(Notice::Running(Request::Look)).encode();
    nested.truncate(notice_at);
    nested.extend([29, 14]);
    assert_eq!(wire::decode(OWNER, &nested), Err("request cannot run in the app"));

    for (r, after_tag, why) in [
        (Move { path: path.into(), target: target.into(), place: DesignPlace::Before }, false, "invalid design place"),
        (Step { path: path.into(), dir: StepDir::Up }, false, "invalid step direction"),
        (Bake { scope: BakeScope::Theme, path: None, prop: None }, true, "invalid bake scope"),
    ] {
        let mut bytes = request(r).encode();
        let at = if after_tag { req_at + 1 } else { bytes.len() - 1 };
        bytes[at] = 0xff;
        assert_eq!(wire::decode(OWNER, &bytes), Err(why));
    }
    let insert = |props| Insert { path: path.into(), place: DesignPlace::Inside, ty: "View".into(), name: None, props };
    let mut truncated = request(insert(vec![])).encode();
    let count_at = truncated.len() - 8;
    truncated[count_at..].copy_from_slice(&1u64.to_le_bytes());
    assert_eq!(wire::decode(OWNER, &truncated), Err("overlong design props count"));
    for r in [
        State { path: Some("p".repeat(wire::MAX_DESIGN_PATH_LEN + 1)) },
        Palette { query: Some("a\nb".into()), offset: 0, limit: 1 },
        Palette { query: None, offset: 0, limit: 0 },
        DropTarget { window: 0, x: f64::INFINITY, y: 1., exclude: None },
        Diff { file: None, offset: 0, limit: wire::MAX_DIFF_LIMIT + 1 },
        WritePatch { file: Some(String::new()) },
        Duplicate { path: path.into(), name: Some("a".repeat(wire::MAX_DESIGN_NAME_LEN + 1)) },
        Wrap { path: path.into(), ty: "mod..View".into(), name: None },
        Bake { scope: BakeScope::Props, path: None, prop: Some("draw.".into()) },
        insert(vec![("text".into(), " ".into())]),
        insert(vec![("text".into(), "x".repeat(wire::MAX_DESIGN_VALUE_LEN + 1))]),
        insert(vec![("text".into(), "x".into()); wire::MAX_DESIGN_PROPS + 1]),
    ] {
        assert!(Request::Design(r.clone()).validate().is_err(), "reject invalid local fields");
        assert!(wire::decode(OWNER, &request(r).encode()).is_err(), "reject invalid wire fields");
    }
    for n in [
        DesignNotice::Running { summary: "".into() },
        DesignNotice::Mutated { op: "move".into(), path: path.into(), focus_path: None, seq: 0, summary: "".into() },
        DesignNotice::Status("{".into()),
        DesignNotice::Reply { kind: DesignReplyKind::State, json: format!("{}0{}", "[".repeat(257), "]".repeat(257)) },
    ] { assert!(wire::decode(OWNER, &notice(Notice::Design(n)).encode()).is_err()); }
}

#[test]
fn wire_round_trips_and_refuses_hostile_bodies() {
    let rect = Rect { window: 3, x: 2., y: 4., w: 6., h: 8. };
    let packet = |body| Packet { share: SHARE, sender: ANA, req: 7, agent: Some(ANA_AGENT), body };
    let mut bodies: Vec<_> = [Request::Select(None), Request::Select(Some(PATH.into())),
        Request::Pick(At::Point { window: 4, x: -1., y: 4.25 }), Request::Pick(At::Path(PATH.into())),
        Request::Props { path: PATH.into() }, set("8"), Request::Look, Request::LookDone { look: u128::MAX, ok: true },
        Request::LookDone { look: 15, ok: false }].into_iter().map(Body::Request).collect();
    bodies.push(Body::Notice { seat: ANA, delivery: 4, notice: Notice::Picture { png: [1, 2, 3].into(), size: [97, 51] } });
    bodies.extend([Notice::Allowed(true), Notice::Running(set("8")), Notice::Stopped(true), Notice::Running(Request::Look), Notice::Looked,
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
    for request in [Request::Theme(ThemeOp::Get), Request::Select(None), Request::LookDone { look: 1, ok: true }, Request::Props { path: String::new() },
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
    let mut person = packet(Body::Ready);
    person.agent = None;
    let mut header = person.encode();
    assert_eq!(header.last(), Some(&0), "a person's packet ends its header with the clear flag");
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
    for tag in [6, 7, 15] {
        future[1] = tag;
        assert_eq!(wire::decode(ANA, &future), Ok(None), "skip unassigned tweak tag {tag}");
    }
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
fn controller_relay_binds_messages_to_the_link_epoch_and_the_right_survives_a_relink() {
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
    assert_eq!(call.take_peer_with_sender(crate::call::topic::PROBE), [(ANA, vec![9])], "the tweak drain leaves other topics for their consumers");
    hub.sessions[0].set_control_rights(ControlRights { all: false, seats: [ANA].into() });
    let _second = call.link_up(cx, OWNER, ANA, "Peer").unwrap();
    assert!(call.take_peer_events().is_empty(), "a clean same-seat replacement produces no loss count");
    hub.poll_call(&mut call);
    assert_eq!(hub.sessions[0].view.allowed, [ANA].into(), "control survives a same-seat relink");
    call.link_down(cx, ANA);
    let dropped = call.take_dropped(TWEAK);
    let [PeerUndelivered { peer: ANA, topic: TWEAK, count }] = dropped.as_slice() else {
        panic!("the closed controller must report its outstanding tweak notices")
    };
    assert!(*count > 0, "the loss count covers queued notices");
    hub.poll_call(&mut call);
    assert_eq!(hub.sessions[0].view.allowed, [ANA].into(), "closing the replacement link preserves control");
    let _third = call.link_up(cx, OWNER, ANA, "Peer").unwrap();
    hub.poll_call(&mut call);
    assert_eq!(hub.sessions[0].view.allowed, [ANA].into(), "the same right survives a fresh link");
    hub.sessions[0].set_control_rights(ControlRights::default());
    assert!(hub.sessions[0].view.allowed.is_empty(), "only the call withdraws control");
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
        session.request(None, Request::Select(Some(PATH.into()))).unwrap();
        Guest { call, session, owner_port, guest_port }
    });
    assert_eq!(owner.peers(), [ANA, BEN]);
    exchange(cx, &mut owner, &mut hub, &mut guests);
    for peer in [ANA, BEN] {
        assert_eq!(hub.sessions[0].view.selections[&peer].path, PATH, "selection retains its authenticated sender");
    }
    hub.sessions[0].set_control_rights(ControlRights { all: false, seats: [ANA, BEN].into() });
    exchange(cx, &mut owner, &mut hub, &mut guests);
    for guest in &guests { assert_eq!(guest.session.view.allowed, [ANA, BEN].into(), "both grants are broadcast"); }

    let ben_epoch = owner.peer_epochs().find(|(peer, _)| *peer == BEN).unwrap();
    owner.send(ANA, TWEAK, vec![1]).unwrap();
    owner.send(ANA, INPUT, vec![2]).unwrap();
    assert!(owner.link_down(cx, ANA));
    hub.poll_call(&mut owner);
    assert_eq!(hub.sessions[0].view.allowed, [ANA, BEN].into(), "both seats keep control across the disconnect");
    assert_eq!(owner.peer_epochs().next(), Some(ben_epoch));
    assert_eq!(owner.take_peer_events(), [PeerEvent::PeerUndelivered { peer: ANA, topic: INPUT, count: 1 }],
        "Tweak consumed its own loss and left the input loss for its dispatcher");

    let after = "/after-relink";
    guests[1].session.request(None, Request::Select(Some(after.into()))).unwrap();
    exchange(cx, &mut owner, &mut hub, &mut guests[1..]);
    assert_eq!(hub.sessions[0].view.selections[&BEN].path, after, "the surviving guest can still send requests");
    assert_eq!(guests[1].session.view.selections[&BEN].path, after, "the surviving guest receives the resulting notice");
    assert_eq!(guests[1].session.view.allowed, [ANA, BEN].into());
}

#[cfg(unix)]
#[test]
fn the_call_allow_is_the_only_way_to_the_tweak_right() {
    use crate::call::{CallAction, CallController, Press};
    use makepad_widgets::*;

    let mut mesh = CallController::linked_tweak_test_mesh();
    let (share, _) = mesh.app_window();
    let [owner, ana, ben] = [0, 1, 2].map(|i| mesh.key(i));
    mesh.watch(1, share);
    assert!(mesh.ctl[1].ask_control(share, true));
    mesh.barrier(1, 0);
    let mut hub = Tweaks::default();
    let mut host = Session::owner(TOWN, share, ITEM, owner, LINK).unwrap();
    host.set_members([owner, ana, ben]);
    let mut app = FakeApp { props: ROWS.into(), ..Default::default() };
    host.request(None, Request::Props { path: PATH.into() }).unwrap();
    host.drive_app(&mut app);
    app.answer(&mut host);
    hub.sessions.push(host);
    let mut guests = [ana, ben].map(|peer| Session::<u64,u64>::peer(TOWN, share, ITEM, peer, owner).unwrap());
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 460} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut panel = test_panel();
    let mut draw = draw_panel(cx, root.clone());

    // The reliable marker places each Set in the owner's relay before the right changes.
    for (step, peer, applies) in [
        ("initial", 1, false), ("named", 1, true), ("other seat", 2, false),
        ("take back", 1, true), ("chip revoke", 1, false), ("allow all", 2, true), ("all off", 2, false),
    ] {
        let guest = &mut guests[peer-1];
        let value = (guest.view.updates.len() + 1).to_string();
        let req = guest.request(None, set(&value)).unwrap();
        assert!(poll_relay(std::slice::from_mut(guest), &mut mesh.ctl[peer]).is_empty());
        mesh.barrier(peer, 0);
        match step {
            "named" => { mesh.grant_to(share, 1); }
            "take back" => assert!(mesh.ctl[0].call_actions(cx, CallAction::OnShare(share, Press::TakeBack))),
            "chip revoke" => {
                let session = &hub.sessions[0];
                panel.refresh(cx, &root, session, &Default::default(), &Default::default());
                draw(cx);
                let row = root.portal_list(cx, ids!(tweak_seats_list)).get_item(0).unwrap().1;
                let button = row.widget(cx, ids!(tweak_revoke));
                assert!(!button.is_empty() && button.visible(), "a named right has a Revoke button");
                let (_, _, actions) = panel_click_at(cx, &root, button.area().rect(cx).center(), 1.);
                let action = actions.iter().filter_map(|a| a.as_widget_action())
                    .find_map(|a| a.action.downcast_ref::<CallAction>()).copied();
                assert_eq!(action, Some(CallAction::OnShare(share, Press::RevokeControl(ana))));
                assert!(mesh.ctl[0].call_actions(cx, action.unwrap()));
            }
            "allow all" => assert!(mesh.ctl[0].allow_all_control(&mut mesh.rig.as_mut().unwrap().app, share)),
            "all off" => assert!(mesh.ctl[0].call_actions(cx, CallAction::OnShare(share, Press::AllowAll(false)))),
            _ => {}
        }
        hub.poll_call(&mut mesh.ctl[0]);
        let session = &mut hub.sessions[0];
        session.drive_app(&mut app);
        app.answer(session);
        if applies {
            assert!(matches!(session.next_record(), Some(crate::model::Action::RecordAppEdit { edit, .. }) if edit.seat == guest.me), "{step}");
            session.record_saved();
        } else {
            assert!(session.next_record().is_none(), "{step} must not create a record");
        }
        if step == "allow all" {
            panel.refresh(cx, &root, session, &Default::default(), &Default::default());
            draw(cx);
            for index in 0..2 {
                let row = root.portal_list(cx, ids!(tweak_seats_list)).get_item(index).unwrap().1;
                assert!(!row.widget(cx, ids!(tweak_revoke)).visible(), "Allow all alone has no per-seat revoke");
            }
        }
        hub.poll_call(&mut mesh.ctl[0]);
        mesh.barrier(0, peer);
        assert!(poll_relay(std::slice::from_mut(guest), &mut mesh.ctl[peer]).is_empty());
        assert!(guest.view.updates.iter().any(|(who, id, _, n)| *who == guest.me && *id == req && match n {
            Notice::Applied { new, .. } => applies && new == &value,
            Notice::Refused(why) => !applies && why == "Ask for control to edit this share.",
            _ => false,
        }), "{step}: the guest receives the outcome under the current call right");
    }
}

#[cfg(unix)]
#[test]
fn a_member_removed_from_the_town_loses_the_tweak_right_at_once() {
    fn first_refusal(guest: &Session<u64, u64>, req: RequestId) -> Option<&str> {
        guest.view.updates.iter().find_map(|(seat, id, _, notice)| match notice {
            Notice::Refused(why) if *seat == guest.me && *id == req => Some(why.as_str()),
            _ => None,
        })
    }

    let mut mesh = crate::call::CallController::linked_tweak_test_mesh();
    let (share, _) = mesh.app_window();
    let town = mesh.ctl[0].open_town().unwrap().0;
    let [owner, ana, ben] = [0, 1, 2].map(|i| mesh.key(i));
    mesh.ctl[0].set_town_members([(town, [ana, ben].into())].into());
    mesh.request_named_control(1, share);
    mesh.grant_to(share, 1);

    let mut hub = Tweaks::default();
    let mut host = Session::owner(town, share, ITEM, owner, LINK).unwrap();
    host.set_members([owner, ana, ben]);
    let mut app = FakeApp { props: ROWS.into(), ..Default::default() };
    host.request(None, Request::Props { path: PATH.into() }).unwrap();
    host.drive_app(&mut app);
    app.answer(&mut host);
    hub.sessions.push(host);
    let mut guest = Session::<u64, u64>::peer(town, share, ITEM, ana, owner).unwrap();
    let queued = guest.request(None, set("12")).unwrap();
    assert!(poll_relay(std::slice::from_mut(&mut guest), &mut mesh.ctl[1]).is_empty());
    mesh.barrier(1, 0);
    hub.poll_call(&mut mesh.ctl[0]);
    assert!(hub.sessions[0].view.allowed.contains(&ana));

    // The sharer's session has accepted the Set but has not dispatched it to the app.
    mesh.ctl[0].set_town_members([(town, [ben].into())].into());
    hub.poll_call(&mut mesh.ctl[0]);
    assert!(!hub.sessions[0].view.allowed.contains(&ana), "one poll withdraws Ana's tweak right");
    mesh.barrier(0, 1);
    assert!(poll_relay(std::slice::from_mut(&mut guest), &mut mesh.ctl[1]).is_empty());
    assert_eq!(first_refusal(&guest, queued), Some(CONTROL_ENDED), "one poll refuses the queued Set");
    assert!(!guest.view.allowed.contains(&ana), "Ana sees that her Allow ended");
    let fresh = guest.request(None, set("14")).unwrap();
    assert!(poll_relay(std::slice::from_mut(&mut guest), &mut mesh.ctl[1]).is_empty());
    mesh.barrier(1, 0);
    hub.poll_call(&mut mesh.ctl[0]);
    mesh.barrier(0, 1);
    assert!(poll_relay(std::slice::from_mut(&mut guest), &mut mesh.ctl[1]).is_empty());
    assert_eq!(first_refusal(&guest, fresh), Some(NEEDS_CONTROL), "a new Set requires a fresh Allow");
    let sent = app.sent.len();
    hub.sessions[0].drive_app(&mut app);
    assert_eq!(app.sent.len(), sent, "neither Set reached the app");
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
    let next = s.ana.send(Request::Select(None));
    let frame = |req, delivery, notice| Packet { share: SHARE, sender: OWNER, req, agent: None,
        body: Body::Notice { seat: ANA, delivery, notice } }.encode();
    let deep = frame(bad, 10, Notice::Rows { path: PATH.into(), rows: rows(300) });
    let good = frame(next, 11, Notice::Selected { path: None, rect: None });
    assert_eq!(wire::decode(OWNER, &deep), Err(TOO_DEEP), "the wire decoder enforces the shared stack guard");
    s.ana.session.view.updates.clear();
    for body in [deep.clone(), deep, good] { s.host.relay.send(ANA, TWEAK, body).unwrap(); }
    s.ana.poll();
    let notices: Vec<_> = s.ana.session.view.updates.into_iter().map(|(_, req, _, n)| (req, n)).collect();
    assert_eq!(notices, [(bad, Notice::Refused(TOO_DEEP.into())), (next, Notice::Selected { path: None, rect: None })],
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
    assert!(has(&s.ben, denied, |n| matches!(n, Notice::Refused(why) if why.contains("Ask for control"))), "Tree requires Allow");
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
    let req = corrupt.ana.send(Request::Select(None));
    corrupt.host.relay.send(ANA, TWEAK, Packet { share: SHARE, sender: OWNER, agent: None, req,
        body: Body::Notice { seat: ANA, delivery: 3, notice: Notice::Selected { path: None, rect: None } } }.encode()).unwrap();
    expected.push((ANA, req, None, Notice::Selected { path: None, rect: None }));
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
    let select = s.ana.send(Request::Select(Some(card.into())));
    s.ana.poll();
    s.host.relay.take(TWEAK);
    s.ana.session.peer_unlinked(OWNER);
    s.ana.session.set_peer_level(OWNER, None);
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).iter().all(|(_, b)| matches!(b[1], 14 | 18)) && !has(&s.ana, drop_req, |_| true),
        "a missing Hello preserves the pending search and Select");
    let waiting = s.ana.session.request(Some(ANA_AGENT), children()).expect("a new query queues while Hello is unknown");
    s.ana.session.set_peer_level(OWNER, older);
    assert!(s.ana.session.view.updates.contains(&(ANA, waiting, Some(ANA_AGENT), Notice::Refused(OLDER_SHARER_TREE.into()))),
        "purging a held query retains its requesting agent");
    assert!(s.ana.session.view.updates.contains(&(ANA, drop_req, None, Notice::Refused(OLDER_SHARER_TREE.into()))),
        "an older Hello immediately refuses the original request locally");
    s.ana.poll();
    let frames = s.host.relay.take(TWEAK);
    assert!(matches!(&frames[..], [(_, b)] if b[1] == 5 && wire::decode(ANA, b).unwrap().expect("a known tweak packet").req == select),
        "the older sharer receives just the following Select");
    for (_, bytes) in frames { s.ana.relay.send(OWNER, TWEAK, bytes).unwrap(); }
    s.exchange();
    assert!(has(&s.host, select, |n| matches!(n, Notice::Selected { path: Some(p), .. } if p == card)), "the older sharer processes Select after dropping Tree");

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
    s.allow(ANA, false);
    s.host.poll();
    assert!(s.ana.relay.take(TWEAK).is_empty(), "the owner holds replay while awaiting the guest's Hello");
    s.host.session.set_peer_level(ANA, older);
    s.host.poll();
    let frames = s.ana.relay.take(TWEAK);
    assert!(!frames.is_empty() && frames.iter().all(|(_, b)| !matches!(b[1], 25 | 26)), "older guest receives no Tree tags during replay");
    assert!(frames.iter().any(|(_, b)| matches!(wire::decode(OWNER, b).unwrap().expect("a known tweak packet").body, Body::Notice { notice: Notice::Allowed(false), .. })),
        "replay includes the call's control revocation");
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
fn mixed_level_peer_replay_gate_blocks_level5_without_hurting_level3() {
    let edit = || Request::Design(DesignRequest::Rename { path: PATH.into(), name: "card".into(), force_rust_refs: false });
    let query = || TreeQuery::Reveal { path: PATH.into(), limit: 1 };
    let status = || Notice::Design(DesignNotice::Status("{}".into()));
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_peer_level(ANA, Some(DESIGN_LEVEL));
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    s.exchange(); // acknowledge the initial design status
    assert!(s.host.session.supports_design() && s.ana.session.supports_design());

    // Lose three unacknowledged deliveries, with design at the head of the mailbox.
    for notice in [status(), Notice::Tree { query: query(), tree: TREE.into() }, Notice::Theme { theme: THEME.into() }] {
        assert!(s.host.session.emit_to(ANA, 0, None, notice).is_some());
    }
    s.host.poll();
    let tags = |frames: &[(Seat, Vec<u8>)]| frames.iter().map(|(_, b)| b[1]).collect::<Vec<_>>();
    assert_eq!(tags(&s.ana.relay.take(TWEAK)), [30, 26, 28]);
    s.host.session.peer_unlinked(ANA);
    s.host.session.set_peer_level(ANA, None);
    s.host.poll();
    assert!(s.ana.relay.take(TWEAK).is_empty(), "mail stays ordered until the guest sends Hello");
    s.host.session.set_peer_level(ANA, Some(THEME_LEVEL));
    assert!(s.host.session.emit_to(ANA, 0, None, status()).is_none(), "new design mail is gated too");
    s.host.poll();
    let frames = s.ana.relay.take(TWEAK);
    assert_eq!(tags(&frames), [26, 28], "the theme-level guest skips design and replays both compatible notices");
    for (_, bytes) in frames { s.host.relay.send(ANA, TWEAK, bytes).unwrap(); }
    s.ana.poll();
    assert!(has(&s.ana, 0, |n| matches!(n, Notice::Tree { .. })));
    assert!(has(&s.ana, 0, |n| matches!(n, Notice::Theme { .. })));
    s.host.poll();
    s.host.session.peer_unlinked(ANA);
    s.host.poll();
    assert!(s.ana.relay.take(TWEAK).is_empty(), "the guest acknowledged through the skipped design delivery");

    // Replay in the other direction. Track the actor of the held structural edit.
    let req = s.ana.session.request(Some(ANA_AGENT), edit()).unwrap();
    s.ana.poll();
    assert_eq!(tags(&s.host.relay.take(TWEAK)), [29]);
    s.ana.session.peer_unlinked(OWNER);
    s.ana.session.set_peer_level(OWNER, None);
    s.ana.send(Request::Tree(query()));
    s.ana.send(Request::Theme(ThemeOp::Get));
    s.ana.poll();
    assert!(tags(&s.host.relay.take(TWEAK)).iter().all(|tag| matches!(tag, 14 | 18)));
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL - 1));
    assert!(!s.ana.session.supports_design() && s.ana.session.supports_theme() && s.ana.session.supports_tree());
    assert!(s.ana.session.view.updates.contains(&(ANA, req, Some(ANA_AGENT), Notice::Refused(OLDER_SHARER_DESIGN.into()))));
    assert_eq!(s.ana.session.request(None, edit()), Err(OLDER_SHARER_DESIGN.into()));
    s.ana.poll();
    assert_eq!(tags(&s.host.relay.take(TWEAK)), [25, 27], "the older sharer gets both compatible requests in order");

    // Capability and workspace refusals both go only to the requesting seat.
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    s.allow(ANA, true);
    let req = s.ana.send(edit());
    s.one();
    assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(why) if why == "This seat's copy of Makepad Town does not support structural design.")));
    s.host.session.set_peer_level(ANA, Some(DESIGN_LEVEL));
    let req = s.ana.send(edit());
    s.one();
    assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(why) if why == "no workspace root for this shared app")));
    assert!(!has(&s.ben, req, |_| true) && s.app.sent.is_empty() && s.records.is_empty());

    // Only terminal notices retire a request. Invalid JSON settles as one acknowledged refusal.
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    let reqs = [s.ana.send(edit()), s.ana.send(edit()), s.ana.send(edit()), s.ana.send(edit())];
    let send = |s: &mut Copies, req, delivery, notice| {
        let p = Packet { share: SHARE, sender: OWNER, req, agent: None, body: Body::Notice { seat: ANA, delivery, notice } };
        s.host.relay.send(ANA, TWEAK, p.encode()).unwrap();
    };
    send(&mut s, reqs[0], 1, status());
    send(&mut s, reqs[1], 2, Notice::Design(DesignNotice::Running { summary: "Renaming".into() }));
    send(&mut s, reqs[2], 3, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, json: "{}".into() }));
    send(&mut s, reqs[3], 4, Notice::Design(DesignNotice::Mutated { op: "rename".into(), path: PATH.into(),
        focus_path: None, seq: 1, summary: "Renamed".into() }));
    s.ana.poll();
    let sent: Vec<_> = s.host.relay.take(TWEAK).into_iter().filter(|(_, b)| b[1] == 29)
        .map(|(_, bytes)| wire::decode(ANA, &bytes).unwrap().unwrap().req).collect();
    assert_eq!(sent, reqs[..2], "status and running keep their requests, reply and mutated retire theirs");
    for (i, bad) in [DesignNotice::Status("{".into()), DesignNotice::Reply { kind: DesignReplyKind::Verify,
        json: format!("{}0{}", "[".repeat(257), "]".repeat(257)) }].into_iter().enumerate() {
        for _ in 0..2 { send(&mut s, reqs[i], i as u64 + 5, Notice::Design(bad.clone())); }
    }
    s.ana.poll();
    let refusals: Vec<_> = s.ana.session.view.updates.iter().filter_map(|(_, id, _, n)| match n {
        Notice::Refused(why) => Some((*id, why.as_str())), _ => None,
    }).collect();
    assert_eq!(refusals, [(reqs[0], parse_json("{").unwrap_err()), (reqs[1], TOO_DEEP)]);
    assert!(matches!(&s.host.relay.take(TWEAK)[..], [(_, b)] if wire::decode(ANA, b).unwrap().unwrap().body == Body::Ack(6)));
    s.ana.session.peer_unlinked(OWNER);
    s.ana.poll();
    assert!(tags(&s.host.relay.take(TWEAK)).iter().all(|tag| matches!(tag, 14 | 18)), "the malformed replies retired both requests");
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
        assert!(has(&s.ben, id, |n| matches!(n, Notice::Refused(s) if s == "Ask for control to edit this share.")),
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
    s.host.session.set_members([OWNER, ANA]);
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.host.session.set_peer_level(BEN, current);
    s.exchange();
    assert!(!has(&s.ben, 0, snapshot), "a join before the global refresh never sees stale derived values");
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
    s.allow(ANA, false);
    if let Some(Action::RecordAppEdit { edit, .. }) = s.host.session.next_record() {
        s.records.push(edit);
        s.host.session.record_saved();
    }
    s.host.session.drive_app(&mut s.app);
    s.app.answer(&mut s.host.session);
    s.host.poll();
    let old_mail = s.ben.relay.take(TWEAK);
    assert!(!old_mail.is_empty() && old_mail.iter().all(|(_, bytes)| wire::min_level(bytes) == 0),
        "the older guest receives only v1 notices");
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
    s.host.send(set_token("space_2", "11"));
    s.one();
    s.host.send(set_token("color_primary", "#abc"));
    s.one();
    s.host.session.set_members([OWNER, ANA]);
    s.host.session.set_members([OWNER, ANA, BEN]);
    s.host.session.set_peer_level(BEN, current);
    s.exchange();
    assert!(has(&s.ben, 0, |n| matches!(n, Notice::Theme { theme } if theme.contains(r##"["color_primary","#xAABBCCFF"],["space_2","11.0"]"##))),
        "a late seat sees the most recent scalar without querying the app again");

    s.host.send(install("my_sunset"));
    s.one();
    s.one();
    s.host.send(install("next"));
    s.one();
    assert!(matches!(&s.records.last().unwrap().change, Change::Applied { old, .. } if old == "my_sunset"),
        "a custom install label survives the refresh for the next history entry");

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
        let select = r.ana.send(Request::Select(None));
        r.ana.poll();
        assert!(r.host.relay.take(TWEAK).iter().all(|(_, b)| matches!(b[1], 14 | 18)), "awaiting Hello holds Get and later Select in order");
        r.ana.session.set_peer_level(OWNER, level);
        r.one();
        assert!(has(&r.ana, select, |n| matches!(n, Notice::Selected { path: None, rect: None })), "later v1 mail proceeds after Hello");
        if level == current {
            assert!(has(&r.ana, req, snapshot), "a capable sharer receives the replayed Get");
        } else {
            assert!(has(&r.ana, req, |n| n == &Notice::Refused(OLDER_SHARER_THEME.into())) && r.app.sent.is_empty(),
                "downgrading settles Get locally before sending Select");
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
        r.allow(ANA, false);
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

fn test_panel() -> panel::Panel {
    let mut panel = panel::Panel::default();
    panel.set_themes_dir(theme::store::tests::scratch("panel"));
    panel
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
fn guest_of_an_older_sharer_is_told_instead_of_offered_a_dead_ask() {
    use makepad_town_core::share::hello::{CONTROL_TWEAK_LEVEL, MULTIPARTY_SHARE_LEVEL};
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut session = Session::<u64,u64>::peer(TOWN, SHARE, ITEM, ANA, OWNER).unwrap();
    let mut panel = test_panel();
    let update = "The sharer's copy of Makepad Town is older and still asks for a separate edit Allow. Tweaking starts when it updates.";
    for (level, supported) in [(None, true), (Some(MULTIPARTY_SHARE_LEVEL), false), (Some(CONTROL_TWEAK_LEVEL), true)] {
        session.set_peer_level(OWNER, level);
        assert_eq!(session.supports_control_tweak(), supported);
        panel.refresh(cx, &root, &session, &Default::default(), &Default::default());
        assert_eq!(root.widget(cx, ids!(tweak_ask)).visible(), supported, "ask availability at {level:?}");
        assert_eq!(root.label(cx, ids!(tweak_result)).text() == update, !supported, "explanation at {level:?}");
    }
    for (asking, label) in [(Some(SHARE), "Asking for control…"), (None, "Ask for control")] {
        panel.asking = asking;
        panel.refresh(cx, &root, &session, &Default::default(), &Default::default());
        assert_eq!(root.button(cx, ids!(tweak_ask)).text(), label);
    }
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
    let mut panel = test_panel();
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.app.props = COLORS.into();
    s.app.prop_values = [("draw_bg.color".into(), "#e6a294ff".into()), ("draw_bg.border_color".into(), "#123456ff".into())].into();
    let mut state = makepad_town_core::fold::State::default();
    state.profiles.insert(ANA, makepad_town_core::fold::Profile { writer: [0; 32], name: "Peer".into(), colour: 0 });
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
    let editor_rect = color.widget(cx, ids!(editor)).area().rect(cx);
    let chip_rect = chip.area().rect(cx);
    assert!((color.widget(cx, ids!(value)).area().rect(cx).pos.x - border.widget(cx, ids!(value)).area().rect(cx).pos.x).abs() < 0.5,
        "the theme chip keeps the value column aligned");
    assert!(chip_rect.pos.y >= editor_rect.pos.y + editor_rect.size.y - 0.5 &&
        chip_rect.pos.x + chip_rect.size.x <= editor_rect.pos.x + editor_rect.size.x + 0.5, "the chip fits below the editor");
    s.ana.session.set_peer_level(OWNER, None);
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    draw(cx);
    assert!(color.widget(cx, ids!(tname_wrap)).visible(), "the cached palette survives a reconnect before Hello");
    s.ana.session.set_peer_level(OWNER, Some(THEME_LEVEL));
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
        let mut panel = test_panel();
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
    let mut panel = test_panel();
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
        state.profiles.insert(seat, makepad_town_core::fold::Profile { writer: [0; 32], name: name.into(), colour: 0x446688 });
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
        let legacy = Content::Entity { copied_from: vec![], kind: Key::Number(kind::APP_EDIT), project: model.state.entities[&item].project,
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

    let mut owner_panel = test_panel();
    owner_panel.refresh(cx, &root, &s.host.session, &Default::default(), &Default::default());
    draw(cx);
    let chips = root.portal_list(cx, ids!(tweak_seats_list));
    let allowed = chips.get_item(0).unwrap().1;
    assert_eq!(allowed.label(cx, ids!(tweak_status)).text(), "Allowed", "the first peer's chip shows its grant");
    assert!(!allowed.widget(cx, ids!(tweak_allow)).visible() && !allowed.widget(cx, ids!(tweak_stop)).visible(),
        "the person's control chip is informational");

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
    owner_panel.local_input(cx, &Event::Startup, &root, &clicks, &mut s.host.session,
        |_| panic!("a detached Stop must never ask for consent"));
    // Supply the remote driver's provenance at the same gate as the platform event.
    let injected = |test_home| matches!(crate::consent::decide(Some("walk".into()), test_home),
        crate::consent::Consent::TestDriver(_));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| injected(false));
    assert_eq!(s.host.session.view.agents[&(ANA, ANA_AGENT)], false, "an unverified Stop press cannot change AI consent");
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| injected(true));
    s.finish();
    owner_panel.refresh(cx, &root, &s.host.session, &roster, &Default::default());
    draw(cx);
    assert_eq!(chip(1).label(cx, ids!(tweak_status)).text(), "stopped", "the chip shows the sharer's Stop");
    let set_time = crate::view::LocalClock::default().hms(s.host.session.view.finished[&(ANA, ANA_AGENT)].1);
    assert_eq!(chip(1).label(cx, ids!(tweak_seat_name)).text(), format!("Ana's AI · set padding 8 · {}", &set_time[..5]),
        "an already dispatched Set that succeeds after Stop records its actual outcome and local minute");
    let (up, clicks) = click(cx, chip(1).widget(cx, ids!(tweak_allow)));
    owner_panel.local_input(cx, &up, &root, &clicks, &mut s.host.session, |_| true);
    assert_eq!(s.host.session.view.agents[&(ANA, ANA_AGENT)], false, "the AI chip's Resume releases just its stop");
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
    let shared = hub.sessions.iter_mut().find(|s| s.share == asking).unwrap();
    shared.set_members([owner, ANA]);
    shared.set_control_rights(ControlRights { all: false, seats: [ANA].into() });
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "granting control does not open the tweak panel");
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
    let open = |m: &mut Model, share| m.act(Action::SessionOpen { town_wide: false, town: None, session: share,
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
        let content = Content::Set { copied_from: vec![], changes: vec![FieldChange { entity: item, field: field.clone(),
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
    exchange(&mut host, &mut watch, &mut net);
    assert!(watch.sessions[0].view.updates.iter().any(|(_, id, _, n)| *id == refused && matches!(n, Notice::Refused(_))),
        "the viewer's first pick requires Allow");
    host.sessions[0].set_control_rights(ControlRights { all: false, seats: [peer].into() });
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
    host.sessions[1].set_control_rights(ControlRights { all: false, seats: [peer].into() });
    host.link_events(&[AppLinkEvent::Restarted { ended: second, app: app.clone() }], &mut links);
    assert!(host.sessions[1].is_ended() && host.sessions[1].view.allowed.is_empty(), "Restarted closes only the replaced link");
    assert!(host.sessions[0].view.allowed.contains(&peer), "the first share keeps its grant");
    host.peer_unlinked(peer);
    assert!(host.sessions[0].view.allowed.contains(&peer), "unlink preserves the call control right");
    host.share_ended(SHARE);
    assert!(host.sessions[0].view.allowed.is_empty(), "S2 Ended releases consent");

    let last = [44; 16];
    open(&mut model, last);
    host.sharer_started(&model, last, "Last".into(), link, WindowId(3), None).unwrap();
    assert_eq!(host.record(&mut model).len(), 1, "starting the share creates its own signed App item");
    host.sessions[2].set_control_rights(ControlRights { all: false, seats: [peer].into() });
    host.link_events(&[AppLinkEvent::Ended { id: link, why: "closed".into() }], &mut links);
    assert!(host.sessions[2].is_ended() && host.sessions[2].view.allowed.is_empty(), "G11 Ended releases consent");

    let departing = [55; 16];
    open(&mut model, departing);
    host.sharer_started(&model, departing, "Leaving".into(), second, WindowId(5), None).unwrap();
    assert_eq!(host.record(&mut model).len(), 1, "starting the share creates its own signed App item");
    crate::model::tests::exchange(&mut model, &mut viewer);
    watch.viewer_started(&viewer, departing, "Leaving".into()).unwrap();
    watch.resolve(&viewer);
    host.sessions[3].set_control_rights(ControlRights { all: false, seats: [peer].into() });
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
    let mut panel = test_panel();
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
    assert!(has(&s.ben, denied, |n| matches!(n, Notice::Refused(why) if why.contains("Ask for control"))), "a peer needs Allow to propose shader changes");
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
    let mut panel = test_panel();
    let mut state = State::default();
    for (seat, name, colour) in [(ANA, "Ana", 0xace021), (BEN, "Ben", 0x1865bd)] {
        state.profiles.insert(seat, Profile { writer: [0; 32], name: name.into(), colour });
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
        let mut panel = test_panel();
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

#[test]
fn theme_tab_edits_tokens_switches_presets_and_saves_replaces_deletes_in_store() {
    use makepad_widgets::{fab_controls::FabColorPickAction, *};

    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 420} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut draw = draw_panel(cx, root.clone());
    let scratch = crate::model::tests::ShareHomes::new("theme-tab");
    std::fs::create_dir_all(&scratch.0).unwrap();
    let dir = scratch.0.join("themes");
    let state = Default::default();
    let mut panel = panel::Panel::default();
    panel.set_themes_dir(dir.clone());
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.app.theme = THEME.replace(r#""sheets":[]"#,
        r#""sheets":[{"id":"macos-dark","label":"macOS dark","base":"dark","appearance":"dark"}]"#)
        .replace(r#""values":["#, r#""values":[{"name":"font_size_p","value":"9","site":"body:1"},"#);
    s.app.theme_base = Some(parse_json(&s.app.theme).unwrap());
    s.allow(ANA, true);
    s.ana.session.view.updates.clear();
    let dispatch = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp| {
        while let Some((seat, req, agent, notice)) = town.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut town.session, seat, req, agent, &state, &notice);
        }
    };
    let click = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp, widget: WidgetRef, time| {
        let (_, down, up) = panel_click_at(cx, &root, widget.area().rect(cx).center(), time);
        panel.actions(cx, &root, &down, &mut town.session);
        panel.actions(cx, &root, &up, &mut town.session);
    };
    let key = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp, key_code| {
        let actions = cx.capture_actions(|cx| root.handle_event(cx,
            &Event::KeyDown(KeyEvent { key_code, ..Default::default() }), &mut Scope::empty()));
        panel.actions(cx, &root, &actions, &mut town.session);
    };
    let settle = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies| {
        s.one();
        dispatch(cx, panel, &mut s.ana);
    };
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    draw(cx);
    for tab in [ids!(tab_props), ids!(tab_shader), ids!(tab_tree), ids!(tab_theme), ids!(tweak_close)] {
        assert!(root.widget(cx, tab).visible(), "a wide panel shows the full tab labels and Close");
    }
    root.borrow_mut::<View>().unwrap().walk.width = Size::Fixed(155.);
    draw(cx);
    for (tab, icon) in [(ids!(tab_props), ids!(tab_props_i)), (ids!(tab_shader), ids!(tab_shader_i)),
        (ids!(tab_tree), ids!(tab_tree_i)), (ids!(tab_theme), ids!(tab_theme_i))] {
        assert!(!root.widget(cx, tab).visible() && root.widget(cx, icon).visible(), "each narrow tab switches to its icon");
    }
    click(cx, &mut panel, &mut s.ana, root.widget(cx, ids!(tab_theme_i)), 1.);
    settle(cx, &mut panel, &mut s);
    root.borrow_mut::<View>().unwrap().walk.width = Size::Fixed(340.);
    draw(cx);
    let list = root.portal_list(cx, ids!(tweak_theme_list));
    let rows = || (0..list.borrow().unwrap().range_end()).filter_map(|i| list.get_item(i));
    let titles = || rows().filter(|(id, _)| *id == id!(SectionRow)).map(|(_, r)| r.child(id!(header)).child(id!(title)).text()).collect::<Vec<_>>();
    assert_eq!(titles(), ["Colours", "Spacing & sizes", "Font sizes"], "the three typed sections are mounted");
    let colours = rows().next().unwrap().1;
    click(cx, &mut panel, &mut s.ana, colours.widget(cx, ids!(header)), 1.3);
    draw(cx);
    assert!(!rows().any(|(id, _)| id == id!(ColorRow)) && rows().any(|(id, _)| id == id!(NumRow)),
        "folding Colours leaves the numeric sections visible");
    click(cx, &mut panel, &mut s.ana, colours.widget(cx, ids!(header)), 1.6);
    let filter = root.text_input(cx, ids!(theme_filter.input));
    let change = cx.capture_actions(|cx| cx.widget_action(filter.widget_uid(), TextInputAction::Changed("FoNt".into())));
    panel.actions(cx, &root, &change, &mut s.ana.session);
    draw(cx);
    assert_eq!(titles(), ["Font sizes"], "filtering matches token names case-insensitively");
    let escape = cx.capture_actions(|cx| cx.widget_action(filter.widget_uid(), TextInputAction::Escaped));
    panel.actions(cx, &root, &escape, &mut s.ana.session);
    draw(cx);
    let color = rows().find(|(id, r)| *id == id!(ColorRow) && r.label(cx, ids!(name)).text() == "color_bg_app").unwrap().1;
    assert!(!color.widget(cx, ids!(tname_wrap)).visible(), "Theme token editors have no match chip");
    assert!(rows().any(|(id, r)| id == id!(NumRow) && r.label(cx, ids!(name)).text() == "font_size_p"), "font tokens use the numeric editor");
    let pick = root.drop_down(cx, ids!(theme_pick));
    assert!(pick.has_labels(&["Dark","Light","Skeleton","macOS dark"].map(String::from)), "the dropdown uses the shared app's catalogue");

    let swatch = color.widget(cx, ids!(swatch));
    let send_colour = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp, event: FabColorPickAction| {
        let actions = cx.capture_actions(|cx| cx.group_widget_actions(list.widget_uid(), color.widget_uid(),
            |cx| cx.widget_action(swatch.widget_uid(), event)));
        panel.actions(cx, &root, &actions, &mut town.session);
    };
    let before = s.app.sent.len();
    for (i, v) in [vec4(1.,0.,0.,1.), vec4(0.,1.,0.,1.), vec4(0.,0.,1.,1.), vec4(1.,1.,1.,1.)].into_iter().enumerate() {
        send_colour(cx, &mut panel, &mut s.ana, if i == 3 { FabColorPickAction::Ended(v) } else { FabColorPickAction::Changed(v) });
        s.one();
    }
    let token_set = |op: &DesignOp, value: &str| matches!(op, DesignOp::Design { verb: DesignVerb::Theme, args } if
        args == &[("set".into(), "1".into()), ("name".into(), "color_bg_app".into()), ("value".into(), value.into())]);
    assert!(matches!(&s.app.sent[before..], [(_,_,op)] if token_set(op, "#ff0000ff")), "holding the reply sends only the first drag value");
    dispatch(cx, &mut panel, &mut s.ana);
    settle(cx, &mut panel, &mut s);
    assert!(matches!(&s.app.sent[before+1..], [(_,_,op)] if token_set(op, "#ffffffff")), "settling sends the final value with no Props query");
    draw(cx);
    let history = root.portal_list(cx, ids!(tweak_history_list));
    let entry = |cx: &Cx, i| history.get_item(i).unwrap().1.label(cx, ids!(tweak_entry)).text();
    assert!(history.borrow().unwrap().range_end() == 1 && entry(cx, 0).ends_with("theme.color_bg_app  #1e1e1eff → #ffffffff (2 steps)"),
        "the coalesced drag has one history run");
    let editor = color.widget(cx, ids!(editor)).area().rect(cx);
    for time in [2., 2.2] {
        let (_, down, _) = panel_click_at(cx, &root, editor.pos + dvec2(12., editor.size.y * 0.5), time);
        panel.actions(cx, &root, &down, &mut s.ana.session);
    }
    settle(cx, &mut panel, &mut s);
    assert!(token_set(&s.app.sent.last().unwrap().2, "#1e1e1eff"), "double-click restores the first recorded value");
    draw(cx);
    let value = color.text_input(cx, ids!(value));
    click(cx, &mut panel, &mut s.ana, color.widget(cx, ids!(value)), 3.);
    let expr = "mix(#f00, #00f, 0.5)";
    value.set_text(cx, expr);
    key(cx, &mut panel, &mut s.ana, KeyCode::ReturnKey);
    settle(cx, &mut panel, &mut s);
    draw(cx);
    assert!(s.records.last().unwrap().change == Change::Note(expr.into()) && color.label(cx, ids!(refusal)).text().contains("Saved a note"),
        "a nonliteral reaches the owner as a note and its refusal is shown on the token");

    click(cx, &mut panel, &mut s.ana, color.widget(cx, ids!(value)), 3.3);
    key(cx, &mut panel, &mut s.ana, KeyCode::Escape);
    draw(cx);
    assert!(!color.widget(cx, ids!(refusal)).visible(), "Escape clears the Theme row refusal");
    click(cx, &mut panel, &mut s.ana, color.widget(cx, ids!(value)), 3.6);
    value.set_text(cx, "theme.color_primary");
    key(cx, &mut panel, &mut s.ana, KeyCode::ReturnKey);
    settle(cx, &mut panel, &mut s);
    assert!(token_set(&s.app.sent.last().unwrap().2, "theme.color_primary") &&
        s.app.token_value("color_bg_app") == s.app.token_value("color_primary"), "the colour field accepts a typed token alias");
    let preset = root.widget(cx, ids!(theme_pick));
    click(cx, &mut panel, &mut s.ana, preset.clone(), 4.);
    draw(cx);
    key(cx, &mut panel, &mut s.ana, KeyCode::End);
    key(cx, &mut panel, &mut s.ana, KeyCode::ReturnKey);
    settle(cx, &mut panel, &mut s);
    assert!(matches!(&s.app.sent.last().unwrap().2, DesignOp::Design { verb: DesignVerb::Theme, args } if
        args[0].0 == "install" && parse_json(&args[1].1) == parse_json(r#"{"base":"dark","sheet":"macos-dark","globals":[],"overrides":[]}"#)),
        "the mounted picker installs the sheet on its registered base");
    settle(cx, &mut panel, &mut s);
    draw(cx);
    assert!(has(&s.ben, 0, |n| matches!(n, Notice::Theme { theme } if theme.contains(r#""sheet":"macos-dark""#))),
        "the new snapshot reaches the observer");
    assert!(entry(cx, 1).contains("theme.install") && pick.selected_label() == "macOS dark", "the preset switch has one install history entry");
    let choose = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp, index| {
        let action = cx.capture_actions(|cx| cx.widget_action(preset.widget_uid(), DropDownAction::Select(index)));
        panel.actions(cx, &root, &action, &mut town.session);
    };
    let name = root.text_input(cx, ids!(theme_name));
    let save = root.widget(cx, ids!(theme_save));
    let delete = root.widget(cx, ids!(theme_delete));
    let msg = root.label(cx, ids!(theme_msg));
    let file = dir.join("my_sunset.theme");
    name.set_text(cx, "My Sunset ");
    click(cx, &mut panel, &mut s.ana, save.clone(), 5.);
    draw(cx);
    assert!(file.is_file() && name.text() == "my_sunset" && pick.selected_label() == "my_sunset" && delete.visible(),
        "save normalizes the name, writes under the injected home and selects the deletable row");
    let original = std::fs::read_to_string(&file).unwrap();
    s.host.send(Request::Theme(ThemeOp::Set { name: "space_2".into(), value: "17".into() }));
    settle(cx, &mut panel, &mut s);
    click(cx, &mut panel, &mut s.ana, save.clone(), 6.);
    assert!(msg.text().contains("save again to replace") && std::fs::read_to_string(&file).unwrap() == original,
        "the first replace leaves the file intact");
    click(cx, &mut panel, &mut s.ana, save.clone(), 7.);
    assert!(std::fs::read_to_string(&file).unwrap().contains("token\tspace_2\t17.0"), "the confirmed replacement includes the observer's live edit");
    for builtin in ["dark", "windows_2000"] {
        name.set_text(cx, builtin);
        click(cx, &mut panel, &mut s.ana, save.clone(), 8.);
        assert!(msg.text().contains("cannot be saved over") && !dir.join(format!("{builtin}.theme")).exists(),
            "the built-in refusal protects {builtin}");
    }
    // A different actor's install clears the member-local file selection.
    s.host.send(Request::Theme(ThemeOp::Install(ThemeInstall { label: "Light".into(), base: "light".into(),
        sheet: None, globals: vec![], overrides: vec![] })));
    settle(cx, &mut panel, &mut s);
    settle(cx, &mut panel, &mut s);
    draw(cx);
    assert!(pick.selected_label() == "Light" && !delete.visible(), "another seat's install moves the picker to its new preset");
    choose(cx, &mut panel, &mut s.ana, 4);
    settle(cx, &mut panel, &mut s);
    settle(cx, &mut panel, &mut s);
    draw(cx);
    assert_eq!(pick.selected_label(), "my_sunset", "the picker follows the acknowledged saved install");
    assert!(delete.visible(), "the worn member-local file can be deleted");
    assert_eq!(s.app.token_value("space_2").and_then(|v| v.parse::<f64>().ok()), Some(17.0),
        "choosing a saved file installs its typed number");
    s.allow(ANA, false);
    dispatch(cx, &mut panel, &mut s.ana);
    choose(cx, &mut panel, &mut s.ana, 0);
    settle(cx, &mut panel, &mut s);
    assert!(msg.text().contains("Ask for control") && pick.selected_label() == "my_sunset" && delete.visible(),
        "a refused install preserves the already worn saved theme and its delete button");
    s.allow(ANA, true);
    dispatch(cx, &mut panel, &mut s.ana);
    click(cx, &mut panel, &mut s.ana, delete.clone(), 9.);
    assert!(file.exists() && msg.text().contains("press delete again"), "the first delete leaves the saved file in place");
    click(cx, &mut panel, &mut s.ana, delete.clone(), 10.);
    settle(cx, &mut panel, &mut s);
    settle(cx, &mut panel, &mut s);
    draw(cx);
    assert!(!file.exists() && !delete.visible() && pick.selected_label() == "Light" && msg.text() == "deleted my_sunset",
        "confirmed delete restores the last built-in and keeps its result visible");

    let before = s.records.len();
    choose(cx, &mut panel, &mut s.ana, 0);
    s.one();
    s.exchange();
    choose(cx, &mut panel, &mut s.ana, 1);
    choose(cx, &mut panel, &mut s.ana, 2);
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).is_empty(), "later choices send no install while the first reply is held");
    dispatch(cx, &mut panel, &mut s.ana);
    for _ in 0..3 { settle(cx, &mut panel, &mut s); }
    assert!(matches!(&s.records[before..], [AppEdit { change: Change::Applied { new: first, .. }, .. },
        AppEdit { change: Change::Applied { new: last, .. }, .. }] if first == "Dark" && last == "Skeleton"),
        "the first install and the latest waiting install are the only two applied");
    draw(cx);

    s.allow(ANA, false);
    dispatch(cx, &mut panel, &mut s.ana);
    let before = (s.app.sent.len(), s.records.len());
    send_colour(cx, &mut panel, &mut s.ana, FabColorPickAction::Ended(vec4(0.,1.,0.,1.)));
    choose(cx, &mut panel, &mut s.ana, 0);
    settle(cx, &mut panel, &mut s);
    draw(cx);
    assert!(color.label(cx, ids!(refusal)).text().contains("Ask for control") && msg.text().contains("Ask for control") &&
        before == (s.app.sent.len(), s.records.len()), "a denied guest cannot edit a token or install a preset");

    let mut older = Copies::new(OWNER, ANA, ITEM);
    older.allow(ANA, true);
    older.exchange(); // Deliver the grant acknowledgments before counting the tab’s traffic.
    older.ana.session.set_peer_level(OWNER, Some(THEME_LEVEL - 1));
    let mut panel = panel::Panel::default();
    panel.set_themes_dir(dir);
    panel.refresh(cx, &root, &older.ana.session, &state, &Default::default());
    panel.restore_selection(cx, &root, &mut older.ana.session);
    draw(cx);
    click(cx, &mut panel, &mut older.ana, root.widget(cx, ids!(tab_theme)), 11.);
    older.ana.poll();
    assert_eq!(root.label(cx, ids!(theme_status)).text(), OLDER_SHARER_THEME, "Theme explains the silent background compatibility refusal");
    assert!(older.host.relay.take(TWEAK).is_empty(), "the older sharer receives zero packets from Theme");
}

#[test]
fn theme_mix_throttles_drags_restores_on_leave_and_stands_down_for_peers() {
    use makepad_widgets::{fab_controls::{FabSliderAction, FabSliderWidgetRefExt}, theme_tokens::{Appearance, BlendTheme}, *};
    makepad_widgets_sheets::register();
    let cache = std::sync::Arc::new(super::theme::lab::tests::bench());
    let catalogue = cache.themes().filter_map(|t| match t {
        BlendTheme::Sheet(id) => Some(json::obj(vec![("id", json::s(id)), ("label", json::s(t.label())),
            ("base", json::s(t.base().theme_name())), ("appearance", json::s(if t.appearance() == Appearance::Dark { "dark" } else { "light" }))])),
        _ => None,
    }).collect();
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let v = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 420} });
        WidgetRef::script_from_value(vm, v)
    });
    let mut draw = draw_panel(cx, root.clone());
    let dir = super::theme::store::tests::scratch("mounted-mix");
    let mut panel = panel::Panel::default();
    panel.set_themes_dir(dir.clone());
    panel.mixer_cache.ready = Some(cache);
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.app.theme = THEME.replace(r#""sheets":[]"#, &format!(r#""sheets":{}"#, json::Value::Arr(catalogue).to_json()));
    s.allow(ANA, true);
    s.allow(BEN, true);
    s.ana.session.view.updates.clear();
    let state = Default::default();
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    let pump = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies| {
        s.one();
        while let Some((seat, req, agent, notice)) = s.ana.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut s.ana.session, seat, req, agent, &state, &notice);
        }
    };
    let button = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, path: &[LiveId]| {
        let uid = root.widget(cx, path).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(Default::default())));
        panel.actions(cx, &root, &actions, &mut s.ana.session);
    };
    let pick = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, idx| {
        let uid = root.widget(cx, ids!(theme_pick)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, DropDownAction::Select(idx)));
        panel.actions(cx, &root, &actions, &mut s.ana.session);
    };
    let labels = |s: &Copies| s.records.iter().filter(|e| e.path == "theme" && e.prop == "install")
        .filter_map(|e| match &e.change { Change::Applied { new, .. } => Some(new.clone()), _ => None }).collect::<Vec<_>>();
    button(cx, &mut panel, &mut s, ids!(tab_theme));
    pump(cx, &mut panel, &mut s);
    draw(cx);
    let fold = root.widget(cx, ids!(eq_fold));
    let (_, press, release) = panel_click_at(cx, &root, fold.area().rect(cx).center(), 1.);
    panel.actions(cx, &root, &press, &mut s.ana.session);
    panel.actions(cx, &root, &release, &mut s.ana.session);
    draw(cx);
    let rows = root.flat_list(cx, ids!(eq_rows));
    let count = rows.borrow().unwrap().items.len();
    assert!(count > 22 && count == BlendTheme::group(Appearance::Dark).len(),
        "the mixer mounts every shared dark slider including rows beyond the old 22 slots, got {count}");
    let row = |name: &str| rows.borrow().unwrap().items.get(&LiveId::from_str(name)).unwrap().widget.clone();
    let read = root.label(cx, ids!(eq_read));
    let msg = root.label(cx, ids!(theme_msg));
    assert!(fold.text() == "- mix" && row("dark").as_fab_slider().value() == 100. && read.text().contains("pairs read"),
        "opening anchors the weights and measures readability immediately");
    pump(cx, &mut panel, &mut s);
    assert!(labels(&s).is_empty(), "opening and measuring sends zero installs");

    let slide = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, name, weight, end| {
        let uid = row(name).widget_uid();
        let actions = cx.capture_actions(|cx| {
            cx.widget_action(uid, FabSliderAction::Changed(weight));
            if end { cx.widget_action(uid, FabSliderAction::Ended(weight)); }
        });
        panel.actions(cx, &root, &actions, &mut s.ana.session);
    };
    for i in 1..=5 {
        slide(cx, &mut panel, &mut s, "macos-dark", f64::from(i * 10), false);
        // Move events alone never install. One eligible frame may use the first value.
        if i == 1 { pump(cx, &mut panel, &mut s); assert!(labels(&s).is_empty(), "a move waits for its frame"); }
        panel.tick_theme_at(cx, &root, &mut s.ana.session, f64::from(i - 1) * 0.025);
        pump(cx, &mut panel, &mut s);
    }
    assert_eq!(labels(&s), ["mix"], "five moves in 100 ms send only the first eligible install");
    let first = s.app.token_value("color_bg_app");
    slide(cx, &mut panel, &mut s, "macos-dark", 90., true);
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 0.101);
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    assert!(labels(&s) == ["mix", "mix"] && first != s.app.token_value("color_bg_app") && read.text().contains("pairs read · closest"),
        "Ended in the same batch as Changed immediately installs the final blend with its contrast readout");
    button(cx, &mut panel, &mut s, ids!(eq_relative));
    button(cx, &mut panel, &mut s, ids!(eq_random));
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 0.102);
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    let shown: f64 = rows.borrow().unwrap().items.values().map(|r| r.widget.as_fab_slider().readout()).sum();
    assert_eq!(shown, 100., "surprise in relative mode distributes 100 displayed parts");
    pick(cx, &mut panel, &mut s, 1);
    pump(cx, &mut panel, &mut s);
    assert!(msg.text().contains("drops the mix") && labels(&s).last().unwrap() == "mix", "the first picker press keeps the mix");
    pick(cx, &mut panel, &mut s, 1);
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    assert!(labels(&s).last().unwrap() == "Light" && row("light").as_fab_slider().value() == 100.,
        "the second press installs the preset and the open mixer re-anchors on its refresh");

    slide(cx, &mut panel, &mut s, "macos", 55., true);
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 0.103);
    button(cx, &mut panel, &mut s, ids!(tab_props));
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    button(cx, &mut panel, &mut s, ids!(tab_theme));
    assert_eq!(fold.text(), "- mix", "inspecting Properties leaves an open mixer alive");
    let before = labels(&s).len();
    button(cx, &mut panel, &mut s, ids!(eq_fold));
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    assert_eq!(&labels(&s)[before..], ["Light"], "folding a standing mix restores the captured preset exactly once");

    button(cx, &mut panel, &mut s, ids!(eq_fold));
    slide(cx, &mut panel, &mut s, "macos", 70., true);
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 1.);
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    slide(cx, &mut panel, &mut s, "macos", 80., false);
    s.ben.send(Request::Theme(ThemeOp::Install(ThemeInstall { label: "Light".into(), base: "light".into(),
        sheet: None, globals: vec![], overrides: vec![] })));
    pump(cx, &mut panel, &mut s); // Applied, before the post-install snapshot.
    let before = labels(&s).len();
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 2.);
    button(cx, &mut panel, &mut s, ids!(eq_fold));
    pump(cx, &mut panel, &mut s);
    pump(cx, &mut panel, &mut s);
    assert!(labels(&s).len() == before && labels(&s).last().unwrap() == "Light" && fold.text() == "+ mix",
        "a peer's Applied cancels the pending settle and prevents even a pre-refresh close from restoring");

    button(cx, &mut panel, &mut s, ids!(eq_fold));
    slide(cx, &mut panel, &mut s, "macos", 60., true);
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 3.);
    pump(cx, &mut panel, &mut s);
    root.text_input(cx, ids!(theme_name)).set_text(cx, "saved mix");
    button(cx, &mut panel, &mut s, ids!(theme_save));
    pump(cx, &mut panel, &mut s);
    let saved = super::theme::store::load_in(&dir, "saved_mix").unwrap();
    let value = s.app.token_value("color_bg_app").unwrap();
    assert!(saved.to_install().overrides.contains(&("color_bg_app".into(), super::theme::scalar(&value).unwrap())),
        "save between Applied and Theme promotes the acknowledged live tokens");
    let before = labels(&s).len();
    button(cx, &mut panel, &mut s, ids!(eq_fold));
    pump(cx, &mut panel, &mut s);
    assert!(labels(&s).len() == before && root.drop_down(cx, ids!(theme_pick)).selected_label() == "saved_mix",
        "closing after promotion preserves the worn saved mix without a restore");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn theme_builder_defers_drags_honors_locks_carousel_and_multi_seat_standdown() {
    use makepad_widgets::{fab_controls::{FabColorPick, FabPaletteCarousel, FabPaletteCarouselAction,
        FabSliderAction, FabSliderWidgetRefExt}, *};
    use super::theme::{builder::{BuildLock, BuildRow, TB_COLOR_IDS, TB_LOCK_FACES}, store};

    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let v = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 420} });
        WidgetRef::script_from_value(vm, v)
    });
    let mut draw = draw_panel(cx, root.clone());
    let dir = store::tests::scratch("mounted-builder");
    std::fs::create_dir_all(&dir).unwrap();
    let own = [0xff5c39ff, 0xcc9933ff, 0x387fd2ff, 0x282c30ff];
    std::fs::write(dir.join("palettes.txt"), "#ff5c39 #cc9933 #387fd2 #282c30\n").unwrap();
    let mut panel = panel::Panel::default();
    panel.set_themes_dir(dir.clone());
    panel.mixer_cache.ready = Some(std::sync::Arc::new(super::theme::lab::tests::bench()));
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.allow(BEN, true);
    s.ana.session.view.updates.clear();
    let state = Default::default();
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    let pump = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies| {
        s.one();
        while let Some((seat, req, agent, notice)) = s.ana.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut s.ana.session, seat, req, agent, &state, &notice);
        }
    };
    let button = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, widget: WidgetRef| {
        let a = cx.capture_actions(|cx| cx.widget_action(widget.widget_uid(), ButtonAction::Clicked(Default::default())));
        panel.actions(cx, &root, &a, &mut s.ana.session);
    };
    let tick = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, now| {
        panel.tick_theme_at(cx, &root, &mut s.ana.session, now);
        pump(cx, panel, s);
        pump(cx, panel, s);
    };
    let installs = |s: &Copies| s.records.iter().filter(|e| e.path == "theme" && e.prop == "install").count();
    let tab = root.widget(cx, ids!(tab_theme));
    button(cx, &mut panel, &mut s, tab);
    pump(cx, &mut panel, &mut s);
    draw(cx);
    let fold = root.widget(cx, ids!(tb_fold));
    let mix = root.widget(cx, ids!(eq_fold));
    let at = fold.area().rect(cx).center();
    let (_, press, release) = panel_click_at(cx, &root, at, 1.);
    panel.actions(cx, &root, &press, &mut s.ana.session);
    panel.actions(cx, &root, &release, &mut s.ana.session);
    draw(cx);
    let body = root.widget(cx, ids!(tb_body));
    let seed_row = body.child(live_id!(tb_seed_row));
    let cols = TB_COLOR_IDS.map(|id| seed_row.child(id));
    let colors = || cols.each_ref().map(|col| u32::from_be_bytes(col.child(live_id!(tb_color)).borrow::<FabColorPick>().unwrap().rgba().map(|c| (c * 255.).round() as u8)));
    let original = colors();
    let carousel = root.widget(cx, ids!(tb_carousel));
    let chips = || carousel.borrow::<FabPaletteCarousel>().unwrap().chip_colors(0).unwrap().map(|v| v.to_u32());
    assert!(fold.text() == "- build" && cols.iter().all(|w| w.visible()) &&
        cols.each_ref().map(|c| c.child(live_id!(tb_color_name)).text()) == ["Prim", "Sec", "Tert", "Surf"],
        "the mounted fold opens all four labelled colour pickers");
    assert_eq!(chips(), own, "the member's palette is first, ahead of the book and harmony chips");
    tick(cx, &mut panel, &mut s, 0.);
    assert_eq!(installs(&s), 0, "opening reads the member's palette without installing");

    let count = root.widget(cx, ids!(tb_count_2));
    button(cx, &mut panel, &mut s, count);
    tick(cx, &mut panel, &mut s, 0.01);
    assert!(cols[0].visible() && !cols[1].visible() && !cols[2].visible() && cols[3].visible()
        && root.drop_down(cx, ids!(tb_text_color)).has_labels(&["None".into(), "Prim".into(), "Surf".into()])
        && carousel.borrow::<FabPaletteCarousel>().unwrap().bands() == 2 && installs(&s) == 1,
        "two colours trim the squares, text menu and carousel together and install once");
    let count = root.widget(cx, ids!(tb_count_4));
    button(cx, &mut panel, &mut s, count);
    tick(cx, &mut panel, &mut s, 0.02);
    assert_eq!(colors(), original, "returning to four restores the previously hidden chosen colours");

    let pick = cx.capture_actions(|cx| cx.widget_action(carousel.widget_uid(), FabPaletteCarouselAction::Pick(0)));
    let before = installs(&s);
    panel.actions(cx, &root, &pick, &mut s.ana.session);
    tick(cx, &mut panel, &mut s, 0.03);
    let value = |cx: &Cx, path| root.widget(cx, path).as_fab_slider().value();
    let (sat, light) = super::theme::builder::surface_sliders(own[3], true);
    assert_eq!(colors(), own, "the chip sets each of the four colour squares");
    assert_eq!(installs(&s), before + 1, "pressing the member palette installs once");
    let slider_values = (value(cx, ids!(tb_saturation)), value(cx, ids!(tb_lightness)));
    assert!((slider_values.0 - sat * 100.).abs() < 0.51 && (slider_values.1 - light * 100.).abs() < 0.51,
        "the chip moves both surface sliders, got {slider_values:?}, expected {:?}", (sat * 100., light * 100.));
    let adjust = root.widget(cx, ids!(tb_adjust));
    let prior_book_chip = carousel.borrow::<FabPaletteCarousel>().unwrap().chip_colors(1);
    let adjust = cx.capture_actions(|cx| cx.widget_action(adjust.widget_uid(), CheckBoxAction::Change(false)));
    panel.actions(cx, &root, &adjust, &mut s.ana.session);
    tick(cx, &mut panel, &mut s, 0.04);
    assert!(installs(&s) == before + 1 && carousel.borrow::<FabPaletteCarousel>().unwrap().chip_colors(1) != prior_book_chip,
        "Adjust regrows book chips without installing");

    let spacing = root.widget(cx, ids!(tb_spacing)).widget_uid();
    let slide = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, v| {
        let actions = cx.capture_actions(|cx| cx.widget_action(spacing, FabSliderAction::Changed(v)));
        panel.actions(cx, &root, &actions, &mut s.ana.session);
    };
    let before = installs(&s);
    for (i, v) in [9., 10., 11., 12.].into_iter().enumerate() {
        slide(cx, &mut panel, &mut s, v);
        tick(cx, &mut panel, &mut s, 1. + 0.02 * i as f64);
        assert_eq!(installs(&s), before, "each move resets the quiet interval even after the settle interval expired");
    }
    tick(cx, &mut panel, &mut s, 1.13);
    draw(cx);
    let history = root.portal_list(cx, ids!(tweak_history_list));
    assert!(installs(&s) == before + 1 && s.app.token_value("space_factor").unwrap().parse::<f64>().unwrap() == 12.
        && history.borrow().unwrap().range_end() == 1 && history.get_item(0).unwrap().1.label(cx, ids!(tweak_entry)).text().contains("theme.install"),
        "one quiet tick re-derives the final spacing and consecutive installs share one history run");

    let lock = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, which: BuildLock| {
        button(cx, panel, s, which.lock_in(&body).child(TB_LOCK_FACES[0]));
    };
    lock(cx, &mut panel, &mut s, BuildLock::Palette);
    lock(cx, &mut panel, &mut s, BuildLock::Row(BuildRow::Lightness));
    let was = (colors(), value(cx, ids!(tb_lightness)), installs(&s));
    let roll = root.widget(cx, ids!(tb_random));
    button(cx, &mut panel, &mut s, roll);
    tick(cx, &mut panel, &mut s, 2.);
    assert!(colors() == was.0 && value(cx, ids!(tb_lightness)) == was.1 && value(cx, ids!(tb_spacing)) != 12.
        && installs(&s) == was.2 + 1, "Roll keeps the locked palette and lightness and rolls the unlocked dimensions");

    let pick = root.widget(cx, ids!(theme_pick)).widget_uid();
    let preview = cx.capture_actions(|cx| cx.widget_action(pick, DropDownAction::Select(1)));
    panel.actions(cx, &root, &preview, &mut s.ana.session);
    pump(cx, &mut panel, &mut s);
    assert!(root.label(cx, ids!(theme_msg)).text().contains("drops the built theme") && installs(&s) == was.2 + 1,
        "the first preset selection asks before discarding a standing builder");
    button(cx, &mut panel, &mut s, mix.clone());
    tick(cx, &mut panel, &mut s, 2.1);
    assert!(fold.text() == "+ build" && mix.text() == "- mix" && !body.visible()
        && s.app.token_value("space_factor").unwrap().parse::<f64>().unwrap() == 10.,
        "opening the mixer restores the builder entry and closes the builder");
    button(cx, &mut panel, &mut s, fold.clone());
    slide(cx, &mut panel, &mut s, 12.);
    tick(cx, &mut panel, &mut s, 3.);
    panel.tick_theme_at(cx, &root, &mut s.ana.session, 3.07);
    pump(cx, &mut panel, &mut s); // Applied, before the refreshed token values.
    let name = root.text_input(cx, ids!(theme_name));
    name.set_text(cx, "built spacing");
    let save = root.widget(cx, ids!(theme_save));
    button(cx, &mut panel, &mut s, save);
    assert!(!dir.join("built_spacing.theme").exists(), "a save following Applied waits for the shared app’s refreshed values");
    pump(cx, &mut panel, &mut s);
    let saved = store::load_in(&dir, "built_spacing").unwrap();
    assert!(saved.to_install().globals.contains(&("space_factor".into(), ThemeVal::Num(12.))),
        "save promotes a standing builder with its moved spacing in globals");
    let before = installs(&s);
    button(cx, &mut panel, &mut s, fold.clone());
    tick(cx, &mut panel, &mut s, 3.2);
    assert_eq!(installs(&s), before, "closing a promoted builder keeps the saved theme live");

    button(cx, &mut panel, &mut s, mix.clone());
    let light = root.widget(cx, ids!(eq_light));
    button(cx, &mut panel, &mut s, light);
    tick(cx, &mut panel, &mut s, 4.);
    assert_eq!(installs(&s), before + 1, "the mixer changes the clean worn saved theme");
    button(cx, &mut panel, &mut s, mix);
    tick(cx, &mut panel, &mut s, 4.1);
    assert!(installs(&s) == before + 2 && root.drop_down(cx, ids!(theme_pick)).selected_label() == "built_spacing"
        && root.widget(cx, ids!(theme_delete)).visible(), "restoring a clean saved theme with overrides keeps worn and delete");

    button(cx, &mut panel, &mut s, fold.clone());
    slide(cx, &mut panel, &mut s, 15.);
    tick(cx, &mut panel, &mut s, 5.);
    tick(cx, &mut panel, &mut s, 5.07);
    assert_eq!(installs(&s), before + 3, "the builder stands again before the peer changes the theme");
    slide(cx, &mut panel, &mut s, 16.);
    s.ben.send(Request::Theme(ThemeOp::Install(ThemeInstall { label: "Light".into(), base: "light".into(),
        sheet: None, globals: vec![], overrides: vec![] })));
    pump(cx, &mut panel, &mut s);
    let before = installs(&s);
    button(cx, &mut panel, &mut s, fold.clone());
    tick(cx, &mut panel, &mut s, 6.);
    assert!(installs(&s) == before && fold.text() == "+ build" && root.drop_down(cx, ids!(theme_pick)).selected_label() == "Light",
        "a peer install disarms both the pending builder settle and a close before the refreshed snapshot");
    std::fs::remove_dir_all(dir).unwrap();
}

type ThemeResult = makepad_ai_services::wire::ToolResult;
type ThemeEffect = crate::tools::ThemeToolEffect;

struct ThemeAgent {
    model: Model,
    tools: AppTools,
    waits: crate::tools::AgentWaits,
    cache: theme::ThemeMixerCache,
    dir: std::path::PathBuf,
    binding: String,
    agent: [u8; 32],
    line: [u8; 32],
}

impl ThemeAgent {
    fn new(mut model: Model) -> Self {
        let code = Credential::parse(&model.agent_code(None).unwrap()).unwrap();
        let (binding, agent) = model.agent_join_with_id(code, true).unwrap();
        let line = model.mention_agent(agent, "work on the shared theme".into(), None).unwrap();
        let mut waits = crate::tools::AgentWaits::default();
        waits.offered(&model, line);
        let token = Credential::parse(&binding).unwrap();
        waits.wait(&mut model, "turn", token, None);
        assert_eq!(model.owner_turn(token), Ok((agent, line)), "the fixture opens the person's own turn");
        Self { dir: model.home.dir.join("themes"), model, tools: AppTools::default(), waits, cache: Default::default(), binding, agent, line }
    }

    fn call(&self, op: &str, item: [u8; 32], fields: &[(&str, json::Value)]) -> makepad_ai_services::wire::ServiceCall {
        let mut args = vec![("binding", json::s(&self.binding))];
        if !matches!(op, "delete" | "palettes") { args.push(("item", json::s(hex(&item)))); }
        args.extend_from_slice(fields);
        makepad_ai_services::wire::ServiceCall { tool: format!("app_theme_{op}"), call_id: op.into(), args: json::obj(args).to_json(),
            caller: None, origin: makepad_ai_services::wire::CallOrigin::Conversation }
    }

    fn start(&mut self, s: &mut TownApp, op: &str, fields: &[(&str, json::Value)]) -> (Option<ThemeResult>, Option<ThemeEffect>) {
        let call = self.call(op, s.session.item, fields);
        let ctx = crate::tools::ThemeToolContext { themes_dir: &self.dir, mixer_cache: &mut self.cache, active_worn: None };
        let (result, _, effect) = self.tools.start(op, app_tool(&call.tool, &call.args).unwrap(), &mut self.model,
            std::slice::from_mut(&mut s.session), &mut self.waits, ctx);
        (result, effect)
    }

    fn notices(&mut self, s: &mut TownApp, mut deliver: impl FnMut(&mut Session<u64,u64>, Seat, RequestId, Option<[u8;32]>, &Notice, Option<ThemeEffect>))
        -> Option<ThemeResult> {
        let mut result = None;
        for (seat, req, agent, notice) in std::mem::take(&mut s.session.view.updates) {
            let ctx = crate::tools::ThemeToolContext { themes_dir: &self.dir, mixer_cache: &mut self.cache, active_worn: None };
            let effect = if let Some((reply, _, effect)) = self.tools.answer(&mut s.session, seat, req, agent, &notice, &mut self.waits, ctx) {
                assert!(result.replace(reply).is_none(), "one call has exactly one final reply");
                effect
            } else { None };
            deliver(&mut s.session, seat, req, agent, &notice, effect);
        }
        result
    }

    fn finish(&mut self, s: &mut Copies) -> ThemeResult {
        for _ in 0..4 {
            s.one();
            if let Some(reply) = self.notices(&mut s.ana, |_,_,_,_,_,_| {}) { return reply }
        }
        panic!("the two phases and any previous post-install refresh must settle the call")
    }

    fn run(&mut self, s: &mut Copies, op: &str, fields: &[(&str,json::Value)]) -> ThemeResult {
        self.start(&mut s.ana, op, fields).0.unwrap_or_else(|| self.finish(s))
    }
}

fn theme_agents(tag: &str) -> (crate::model::tests::ShareHomes, ThemeAgent, ThemeAgent, Copies) {
    makepad_widgets_sheets::register();
    let (homes, mut owner, guest) = crate::model::tests::shared_town(tag);
    owner.set_person_name("Walk A").unwrap();
    let town = owner.open_id();
    let item = owner.act(Action::AddApp { town, board: None, geometry: [0,0,6400,6400], session: SHARE, name: "Scene".into() }, Origin::Ui).unwrap();
    let mut owner = ThemeAgent::new(owner);
    let mut ana = ThemeAgent::new(guest);
    crate::model::tests::exchange(&mut owner.model, &mut ana.model);
    let mut s = Copies::in_town(town, owner.model.me(), ana.model.me(), item);
    s.host.session.set_agents(&owner.model.state);
    use makepad_widgets::theme_tokens::BlendTheme;
    let sheets = BlendTheme::all().into_iter().filter_map(|t| match t {
        BlendTheme::Sheet(id) => Some(json::obj(vec![("id",json::s(id)), ("label",json::s(t.label())), ("base",json::s(t.base().theme_name())),
            ("appearance",json::s(t.appearance().label().to_ascii_lowercase()))])), _ => None,
    }).collect::<Vec<_>>();
    assert!(sheets.len() >= 51, "the shared catalogue includes all shipped sheets");
    let mut tokens = parse_json(THEME).unwrap().get("values").unwrap().as_arr().unwrap().to_vec();
    for i in tokens.len()..620 {
        let (name, value) = if i % 10 == 0 { (format!("font_size_{i}"),"12") } else { (format!("color_surface_{i}"),"#223344ff") };
        tokens.push(json::obj(vec![("name",json::s(name)),("value",json::s(value)),("site",json::s("theme.rs:12"))]));
    }
    s.app.theme = json::obj(vec![("base",json::s("dark")),("sheet",json::Value::Null),("has_mix",json::Value::Bool(false)),
        ("has_pins",json::Value::Bool(false)),("edits",json::Value::Arr(vec![])),("values",json::Value::Arr(tokens)),("sheets",json::Value::Arr(sheets))]).to_json();
    (homes, owner, ana, s)
}

fn theme_ok(result: ThemeResult) -> json::Value {
    assert_eq!(result.outcome, makepad_ai_services::wire::ToolOutcome::Ok, "{} should succeed: {}", result.call_id, result.text);
    assert!(result.text.len() <= makepad_ai_services::wire::MAX_RESULT_BYTES, "{} fits the router", result.call_id);
    parse_json(&result.text).expect("a successful theme tool returns a complete JSON object")
}

#[test]
fn app_theme_tools_read_edit_install_store_mix_and_build_under_control() {
    use makepad_ai_services::wire::ToolOutcome;
    use json::Value::{Bool, Int};
    let (_homes, owner, mut ana, mut s) = theme_agents("theme-ai");
    let guest = ana.model.me();
    let host = owner.model.me();
    let preset = [("preset",json::s("light"))];
    let name = [("name",json::s("my_sunset"))];
    let token = |v| [("name",json::s("color_bg_app")),("value",json::s(v))];
    let set = token("#223344ff");
    let random = [("surprise",Bool(true)),("seed",Int(42))];
    let live: [(&str, &[(&str,json::Value)]);6] = [("get",&[]), ("set",&set), ("install",&preset), ("save",&name), ("mix",&random), ("build",&[])];
    for (op, args) in live {
        let result = ana.run(&mut s, op, args);
        assert_eq!((result.outcome, result.text.as_str()), (ToolOutcome::Refused,"Ask for control to edit this share."), "{op} requires Allow");
        assert_eq!(ana.waits.traces.rows(&ana.line)[0].output.last().unwrap().ink, crate::ai_trace::Ink::Warning, "{op} records a warning");
    }
    assert!(s.app.sent.is_empty(), "denied tools never reach the app");
    s.allow(guest,true);
    s.host.session.stop_agent(guest, ana.agent,true).unwrap();
    for (op,args) in live {
        let result = ana.run(&mut s,op,args);
        assert!(result.outcome == ToolOutcome::Refused && result.text.contains("Walk A stopped"), "Stop overrides Allow for {op}: {}",result.text);
    }
    s.host.session.stop_agent(guest,ana.agent,false).unwrap();
    s.exchange();
    s.exchange(); // Deliver Stop acknowledgements before measuring the next calls.
    ana.cache = Default::default();
    s.ana.session.set_peer_level(host,Some(THEME_LEVEL-1));
    for (op,args) in live {
        let (result,_) = ana.start(&mut s.ana,op,args);
        assert_eq!(result.unwrap().text,OLDER_SHARER_THEME,"{op} refuses the older sharer synchronously");
    }
    assert!(!ana.cache.is_preparing() && !ana.tools.needs_mixer_cache(),
        "an older sharer's synchronous mix refusal neither prepares the resolver nor requests frames");
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).is_empty() && s.app.sent.is_empty(), "compatibility refusals send zero packets");
    s.ana.session.set_peer_level(host,Some(THEME_LEVEL));
    let mut page = theme_ok(ana.run(&mut s,"get",&[]));
    assert_eq!(page.get("total_tokens"),Some(&Int(620)), "the tool reads the full shared catalogue");
    let sheets = page.get("sheets").unwrap().as_arr().unwrap();
    assert!(sheets.iter().any(|s| s.get("id") == Some(&json::s("omarchy")) && s.get("mixable") == Some(&Bool(true))),
        "the shared catalogue identifies the extra mixable sheets");
    let mut names = vec![];
    loop {
        names.extend(page.get("tokens").unwrap().as_arr().unwrap().iter().map(|t| t.get("name").unwrap().as_str().unwrap().to_owned()));
        let Some(next) = page.get("next").unwrap().as_str() else { break };
        page = theme_ok(ana.run(&mut s,"get",&[("after",json::s(next))]));
    }
    assert!(names.len() == 620 && names.windows(2).all(|w| w[0] < w[1]),"token pages cover every name in sorted order");
    let fonts = theme_ok(ana.run(&mut s,"get",&[("filter",json::s("size")),("group",json::s("font"))]));
    assert!(fonts.get("next") == Some(&json::Value::Null) && fonts.get("tokens").unwrap().as_arr().unwrap()
        .iter().all(|t| t.get("name").unwrap().as_str().unwrap().starts_with("font_size")), "font groups and name filters apply together");
    let body = theme_ok(ana.run(&mut s,"set",&set));
    assert_eq!((body.get("name"),body.get("old"),body.get("new")), (Some(&set[0].1),Some(&json::s("#1e1e1eff")),Some(&set[1].1)), "a scalar reply carries the actual edit");
    assert_eq!(body.get("seq"),Some(&json::s("1")), "the first edit has its signed sequence");
    theme_ok(ana.run(&mut s,"set",&token("theme.color_primary")));
    assert!(s.records.iter().all(|e| e.agent == Some(ana.agent) && e.seat == guest), "signed edits credit the guest and its AI");
    for copy in [&s.ana,&s.host,&s.ben] {
        let done = &copy.session.view.finished[&(guest,ana.agent)].0;
        assert!(done.done_text("Scene").unwrap().starts_with("set color_bg_app "), "each seat retains the AI's completed theme edit");
    }
    let expr = "mix(#f00, #00f, 0.5)";
    assert_eq!(ana.run(&mut s,"set",&token(expr)).outcome, ToolOutcome::Refused, "a nonliteral edit is refused");
    assert_eq!(s.records.last().unwrap().change, Change::Note(expr.into()), "the refusal saves an implementer note");
    let before = s.app.sent.len();
    theme_ok(ana.run(&mut s,"install",&preset));
    assert_eq!(s.app.sent.len(),before+1,"a base installs in one round trip");
    s.one();
    let before = s.app.sent.len();
    let mac = theme_ok(ana.run(&mut s,"install",&[("preset",json::s("macos-dark"))]));
    assert!(s.app.sent.len() == before+2 && mac.get("installed") == Some(&json::s("macOS dark")), "a sheet uses get then install");
    s.one();
    let before = s.app.sent.len();
    assert_eq!(ana.run(&mut s,"install",&[("preset",json::s("absent"))]).outcome,ToolOutcome::Refused,"unknown sheets are refused");
    assert_eq!(s.app.sent.len(),before+1,"unknown sheets send only the initial Get");
    theme_ok(ana.run(&mut s,"save",&name));
    let file = ana.dir.join("my_sunset.theme");
    assert!(file.is_file() && !owner.dir.join("my_sunset.theme").exists(), "snapshots live in the requesting person's store");
    assert_eq!(ana.run(&mut s,"save",&name).outcome,ToolOutcome::Refused,"replacement needs explicit consent");
    let replacement = theme_ok(ana.run(&mut s,"save",&[name[0].clone(),("replace",Bool(true))]));
    assert_eq!(replacement.get("replaced"),Some(&Bool(true)),"explicit replacement is reported");
    for builtin in ["dark","macos-dark"] {
        assert!(ana.run(&mut s,"save",&[("name",json::s(builtin))]).text.contains("library ships"),"built-in {builtin} stays protected");
    }
    let worn = theme_ok(ana.run(&mut s,"install",&[("saved",json::s("my_sunset"))]));
    assert_eq!(worn.get("saved"),Some(&name[0].1),"saved installs load from the guest's store");
    let before = s.app.sent.len();
    theme_ok(ana.run(&mut s,"delete",&name));
    for protected in ["dark","missing"] { assert_eq!(ana.run(&mut s,"delete",&[("name",json::s(protected))]).outcome,ToolOutcome::Refused,"cannot delete {protected}"); }
    assert!(s.app.sent.len() == before && !file.exists(),"deletion is local and never installs");
    s.one();
    let weights = [("weights",json::obj(vec![("dark",Int(50)),("macos-dark",Int(50))]))];
    assert!(ana.cache.ready.is_none() && ana.start(&mut s.ana,"mix",&weights).0.is_none(), "a cold mix starts with Get");
    assert!(ana.cache.is_preparing() && ana.tools.needs_mixer_cache(), "an accepted mix starts warming while its Get is in flight");
    s.one();
    assert!(ana.notices(&mut s.ana,|_,_,_,_,_,_| {}).is_none() && ana.tools.needs_mixer_cache(), "the snapshot parks until the cache is ready");
    let before = s.app.sent.len();
    while ana.cache.ready.is_none() { ana.cache.step(); }
    ana.tools.flush_mixes(std::slice::from_mut(&mut s.ana.session), &ana.cache);
    let mixed = theme_ok(ana.finish(&mut s));
    assert_eq!(s.app.sent.len(),before+1,"the cold rendezvous sends exactly one install");
    assert!(mixed.get("readability").unwrap().get("measured").unwrap().as_u64().unwrap() > 0, "mix returns the installed contrast");
    assert_eq!(mixed.get("weights").unwrap().as_arr().unwrap().iter().map(|v| v.get("share").unwrap().to_json().parse::<f64>().unwrap()).sum::<f64>(),
        100., "default relative shares total one hundred");
    let first = theme_ok(ana.run(&mut s,"mix",&random));
    let second = theme_ok(ana.run(&mut s,"mix",&random));
    assert_eq!(first.get("weights"), second.get("weights"), "seeded surprise repeats through the tool");
    std::fs::write(ana.dir.join("palettes.txt"), "#ff5c39 #42a880 #bbc7dd #282c30\n").unwrap();
    let palettes = theme_ok(ana.run(&mut s,"palettes",&[("color",json::s("#ff5c39")),("adjust",Bool(false))]));
    let own = &palettes.get("palettes").unwrap().as_arr().unwrap()[0];
    assert_eq!(own.get("label"),Some(&json::s("From your palettes")), "the person's matching palette appears first");
    let total = palettes.get("total").unwrap().as_u64().unwrap();
    let end = theme_ok(ana.run(&mut s,"palettes",&[("color",json::s("#ff5c39")),("adjust",Bool(false)),("offset",Int(total as i64 - 1))]));
    let tail = &end.get("palettes").unwrap().as_arr().unwrap()[0];
    assert!(palettes.get("palettes").unwrap().as_arr().unwrap()[1].get("label").and_then(json::Value::as_str).is_some_and(|s| s.starts_with("Combination "))
        && tail.get("harmony").and_then(json::Value::as_str).is_some() && end.get("next_offset") == Some(&json::Value::Null),
        "the book follows the member's palettes, with generated harmonies last");
    for bad in [vec![("favourite",json::s("#f00")),("palette",json::Value::Arr(vec![json::s("#abc")]))],
        vec![("harmony",json::s("single"))], vec![("palette",json::Value::Arr(vec![json::s("blue")]))]] {
        assert_eq!(ana.start(&mut s.ana,"build",&bad).0.unwrap().outcome, ToolOutcome::Refused, "invalid builder arguments fail before touching the share");
    }
    let built = theme_ok(ana.run(&mut s,"build",&[("palette",own.get("shown").unwrap().clone()),("spacing",Int(12))]));
    assert!(built.get("readability").is_some() && built.get("crowding").is_some() && s.app.token_value("space_factor").unwrap().parse::<f64>() == Ok(12.),
        "shown colours feed the builder and spacing reaches the shared app");
    s.one();
    ana.cache = Default::default();
    ana.start(&mut s.ana,"mix",&weights);
    s.one();
    assert!(ana.notices(&mut s.ana, |_,_,_,_,_,_| {}).is_none(), "the last mix parks before the share ends");
    s.ana.session.end("closed");
    let mut replies = vec![];
    ana.tools.take_ready(std::slice::from_ref(&s.ana.session), &mut ana.waits, &mut |r| replies.push(r));
    assert!(matches!(replies.as_slice(), [r] if r.call_id == "mix" && r.outcome == ToolOutcome::Refused && r.text == "share ended")
        && !ana.tools.needs_mixer_cache(), "ending a share settles its parked call and releases the cache wakeup");
}

#[test]
fn ai_theme_install_and_store_sync_stand_down_open_panel_tools() {
    use makepad_widgets::{fab_controls::{FabColorPick, FabSliderAction}, *};
    let (_homes, mut owner, mut ana, mut s) = theme_agents("theme-ai-panel");
    let guest = ana.model.me();
    s.allow(guest, true);
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let ui = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* View { tweak_panel := AppTweakPanel { width: 420 } } });
        WidgetRef::script_from_value(vm, value)
    });
    let root = ui.widget(cx,ids!(tweak_panel));
    let mut draw = draw_panel(cx, ui.clone());
    let mut hub = Tweaks::<u64,u64>::default();
    hub.set_themes_dir(ana.dir.clone());
    hub.sessions.push(Session::peer(s.ana.session.town, SHARE, s.ana.session.item, guest, owner.model.me()).unwrap());
    let call = ana.call("get", s.ana.session.item, &[]);
    let (pending, _) = hub.tool(cx,&ui,&mut ana.model,&call,&mut ana.waits).unwrap();
    assert!(pending.is_none(),"a theme read starts with the panel closed");
    hub.update(cx,&ui,&ana.model,&Default::default(),&mut ana.waits, |_| {});
    assert!(!root.visible() && root.widget(cx,ids!(tweak_props_wrap)).visible() && !root.widget(cx,ids!(tweak_theme_wrap)).visible(),
        "an AI theme call leaves the panel closed and the selected tab on Props");

    hub.open_panel(SHARE);
    hub.sessions[0].view.updates.push_back((guest, 0, None, Notice::Theme { theme: s.app.theme.clone() }));
    hub.update(cx, &ui, &ana.model, &Default::default(), &mut ana.waits, |_| {});
    let press = |cx: &mut Cx, hub: &mut Tweaks<u64,u64>, path: &[LiveId]| {
        let uid = root.widget(cx, path).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(Default::default())));
        hub.actions(cx, &ui, &actions);
    };
    let tick = |cx: &mut Cx, hub: &mut Tweaks<u64,u64>, frame: u64| {
        hub.tick_theme(cx, &ui, &Event::NextFrame(NextFrameEvent { frame, time: frame as f64 / 60., ..Default::default() }));
    };
    press(cx, &mut hub, ids!(tab_theme));
    press(cx, &mut hub, ids!(eq_fold));
    // One frame initializes the resolver, then one resolves each theme. Leave exactly one theme pending.
    let themes = makepad_widgets::theme_tokens::BlendTheme::all().len() as u64;
    for frame in 0..themes { tick(cx, &mut hub, frame); }
    let read = root.label(cx, ids!(eq_read));
    assert_eq!(read.text(), "Preparing theme mixer…", "the cold mixer still needs its final theme");
    press(cx, &mut hub, ids!(tb_fold));
    assert!(!hub.needs_theme_tick(), "an idle builder has no reason to finish the paused mixer");
    let slider = root.widget(cx, ids!(tb_spacing)).widget_uid();
    let drag = cx.capture_actions(|cx| cx.widget_action(slider, FabSliderAction::Changed(12.)));
    hub.actions(cx, &ui, &drag);
    assert!(hub.needs_theme_tick(), "the builder drag schedules an actual frame");
    tick(cx, &mut hub, themes);
    press(cx, &mut hub, ids!(eq_fold));
    assert_eq!(read.text(), "Preparing theme mixer…", "the builder drag left the final mixer step pending");
    tick(cx, &mut hub, themes + 1);
    assert!(read.text().contains("pairs read") && !hub.needs_theme_tick(),
        "the person's mixer becomes ready on its first resumed frame");

    root.set_visible(cx,true);
    let mut panel = panel::Panel::default();
    panel.set_themes_dir(ana.dir.clone());
    let cache = std::sync::Arc::new(theme::lab::tests::bench());
    panel.mixer_cache.ready = Some(cache.clone());
    owner.cache.ready = Some(cache);
    let state = ana.model.state.clone();
    panel.refresh(cx,&root,&s.ana.session,&state,&Default::default());
    let deliver = |cx: &mut Cx, panel: &mut panel::Panel, ana: &mut ThemeAgent, s: &mut Copies| {
        ana.notices(&mut s.ana, |session,seat,req,agent,n,effect| {
            panel.notice(cx,&root,session,seat,req,agent,&state,n);
            if let Some(effect) = effect { panel.apply_theme_tool_effect(cx,&root,effect); }
        })
    };
    let pump = |cx: &mut Cx, panel: &mut panel::Panel, ana: &mut ThemeAgent, s: &mut Copies| {
        s.one();
        deliver(cx,panel,ana,s)
    };
    let button = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, path: &[LiveId]| {
        let uid = root.widget(cx,path).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid,ButtonAction::Clicked(Default::default())));
        panel.actions(cx,&root,&actions,&mut s.ana.session);
    };
    let spacing = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, value, time| {
        let uid = root.widget(cx,ids!(tb_spacing)).widget_uid();
        let actions = cx.capture_actions(|cx| cx.widget_action(uid,FabSliderAction::Ended(value)));
        panel.actions(cx,&root,&actions,&mut s.ana.session);
        panel.tick_theme_at(cx,&root,&mut s.ana.session,time);
    };
    let installs = |s: &Copies| s.records.iter().filter(|e| e.path == "theme" && e.prop == "install").count();
    button(cx,&mut panel,&mut s,ids!(tab_theme));
    pump(cx,&mut panel,&mut ana,&mut s);
    button(cx,&mut panel,&mut s,ids!(tb_fold));
    draw(cx);

    assert!(ana.start(&mut s.ana,"get",&[]).0.is_none(),"the AI requests its own snapshot");
    s.one();
    let updates = &mut s.ana.session.view.updates;
    let i = updates.iter().position(|(_,_,ai,n)| ai == &Some(ana.agent) && matches!(n,Notice::Theme { .. })).unwrap();
    let private = updates.remove(i).unwrap();
    spacing(cx,&mut panel,&mut s,12.,0.);
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(panel.theme_state().tb_stands && panel.theme_state().awaiting_snapshot,"the builder stands at Applied while its broadcast is outstanding");
    root.text_input(cx,ids!(theme_name)).set_text(cx,"after reload");
    button(cx,&mut panel,&mut s,ids!(theme_save));
    s.ana.session.view.updates.push_front(private);
    theme_ok(deliver(cx,&mut panel,&mut ana,&mut s).unwrap());
    assert!(panel.theme_state().awaiting_snapshot && !ana.dir.join("after_reload.theme").exists(),
        "a private AI snapshot cannot release the person's deferred save");
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(ana.dir.join("after_reload.theme").is_file(), "the broadcast snapshot releases the deferred save");

    spacing(cx,&mut panel,&mut s,13.,1.);
    pump(cx,&mut panel,&mut ana,&mut s);
    pump(cx,&mut panel,&mut ana,&mut s);
    let square = root.widget(cx,ids!(tb_seed_row)).child(live_id!(tb_color_0)).child(live_id!(tb_color));
    square.borrow_mut::<FabColorPick>().unwrap().open_popover(cx);
    assert!(square.borrow::<FabColorPick>().unwrap().is_open() && panel.theme_state().tb_stands, "the standing builder has an open palette square");
    let changed = root.widget(cx,ids!(tb_spacing)).widget_uid();
    let movement = cx.capture_actions(|cx| cx.widget_action(changed,FabSliderAction::Changed(15.)));
    panel.actions(cx,&root,&movement,&mut s.ana.session);
    ana.start(&mut s.ana,"build",&[("spacing",json::Value::Int(14))]);
    theme_ok(pump(cx,&mut panel,&mut ana,&mut s).unwrap());
    assert!(!square.borrow::<FabColorPick>().unwrap().is_open() && !panel.theme_state().tb_stands
        && panel.theme_state().tb_reanchor_pending && !panel.theme_state().tb_moving,
        "an own-AI install closes the popover and cancels the standing builder and pending drag at Applied");
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(!panel.theme_state().tb_reanchor_pending && panel.theme_state().snapshot.as_ref().unwrap().tokens.iter()
        .any(|t| t.name == "space_factor" && t.value.parse::<f64>() == Ok(14.)), "the refreshed snapshot re-anchors the builder");
    let before = installs(&s);
    button(cx,&mut panel,&mut s,ids!(tb_fold));
    pump(cx,&mut panel,&mut ana,&mut s);
    assert_eq!(installs(&s),before,"closing the builder preserves the AI's live theme");

    button(cx,&mut panel,&mut s,ids!(eq_fold));
    button(cx,&mut panel,&mut s,ids!(eq_random));
    panel.tick_theme_at(cx,&root,&mut s.ana.session,2.);
    pump(cx,&mut panel,&mut ana,&mut s);
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(panel.theme_state().eq_stands,"the person's mix is live before the peer AI installs");
    owner.start(&mut s.host,"mix",&[("surprise",json::Value::Bool(true)),("seed",json::Value::Int(7))]);
    s.one(); owner.notices(&mut s.host, |_,_,_,_,_,_| {});
    s.one(); theme_ok(owner.notices(&mut s.host, |_,_,_,_,_,_| {}).unwrap());
    deliver(cx,&mut panel,&mut ana,&mut s);
    assert!(!panel.theme_state().eq_stands && panel.theme_state().eq_reanchor_pending,"a peer AI's mix stands down the person's mix");
    let before = installs(&s);
    button(cx,&mut panel,&mut s,ids!(eq_fold));
    pump(cx,&mut panel,&mut ana,&mut s);
    assert_eq!(installs(&s),before,"closing the mixer preserves the peer AI's theme");

    button(cx,&mut panel,&mut s,ids!(tb_fold));
    spacing(cx,&mut panel,&mut s,12.,3.);
    pump(cx,&mut panel,&mut ana,&mut s);
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(panel.theme_state().tb_stands,"the builder is standing when the AI saves");
    let name = [("name",json::s("ai_saved"))];
    ana.start(&mut s.ana,"save",&name);
    theme_ok(pump(cx,&mut panel,&mut ana,&mut s).unwrap());
    let pick = root.drop_down(cx,ids!(theme_pick));
    let delete = root.widget(cx,ids!(theme_delete));
    assert!(pick.selected_label() == "ai_saved" && delete.visible() && !panel.theme_state().tb_stands,
        "saving from the AI selects the local name, reveals delete and promotes the builder");
    let before = installs(&s);
    button(cx,&mut panel,&mut s,ids!(tb_fold));
    pump(cx,&mut panel,&mut ana,&mut s);
    assert_eq!(installs(&s),before,"closing the promoted builder leaves the saved theme live");
    ana.start(&mut s.ana,"install",&[("preset",json::s("light"))]);
    theme_ok(pump(cx,&mut panel,&mut ana,&mut s).unwrap());
    pump(cx,&mut panel,&mut ana,&mut s);
    assert_eq!(pick.selected_label(),"Light","a base install clears the local saved selection");
    ana.start(&mut s.ana,"install",&[("saved",json::s("ai_saved"))]);
    pump(cx,&mut panel,&mut ana,&mut s);
    theme_ok(pump(cx,&mut panel,&mut ana,&mut s).unwrap());
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(pick.selected_label() == "ai_saved" && delete.visible(),"the saved install effect runs after stand-down and survives the broadcast");
    let before = s.app.sent.len();
    let (result,effect) = ana.start(&mut s.ana,"delete",&name);
    theme_ok(result.unwrap());
    panel.apply_theme_tool_effect(cx,&root,effect.unwrap());
    pump(cx,&mut panel,&mut ana,&mut s);
    assert!(panel.theme_state().saved_names.iter().all(|n| n != "ai_saved") && !delete.visible() && before == s.app.sent.len(),
        "AI deletion removes its picker entry and sends no hidden install");

    let mut previous = Session::<u64,u64>::peer(s.ana.session.town, [18; 16], s.ana.session.item, guest, owner.model.me()).unwrap();
    panel.refresh(cx,&root,&previous,&state,&Default::default());
    let tab = root.widget(cx,ids!(tab_theme)).widget_uid();
    let actions = cx.capture_actions(|cx| cx.widget_action(tab,ButtonAction::Clicked(Default::default())));
    panel.actions(cx,&root,&actions,&mut previous);
    let theme_wrap = root.widget(cx,ids!(tweak_theme_wrap));
    let props_wrap = root.widget(cx,ids!(tweak_props_wrap));
    assert!(root.visible() && theme_wrap.visible() && !props_wrap.visible(), "the previous share is open on Theme");
    assert!(ana.start(&mut s.ana,"get",&[]).0.is_none(), "the AI requests the newly active share's theme");
    s.one();
    let updates = &mut s.ana.session.view.updates;
    let i = updates.iter().position(|(_,_,ai,n)| ai == &Some(ana.agent) && matches!(n, Notice::Theme { .. })).unwrap();
    let (seat,req,agent,n) = updates.remove(i).unwrap();
    panel.notice(cx,&root,&mut s.ana.session,seat,req,agent,&state,&n);
    assert!(theme_wrap.visible() && !props_wrap.visible(),
        "a private reply received before the active share refresh preserves the tab: Theme={}, Props={}, panel={}",
        theme_wrap.visible(), props_wrap.visible(), root.visible());

    let (share, item) = (s.ana.session.share, s.ana.session.item);
    ana.start(&mut s.ana, "save", &[("name", json::s("unbound_saved"))]);
    theme_ok(pump(cx, &mut panel, &mut ana, &mut s).unwrap());
    assert!(root.visible() && theme_wrap.visible() && !props_wrap.visible() && panel.theme_state().worn.is_none(),
        "saving on an unbound share preserves the previous share's tab and worn theme");
    assert_eq!(panel.worn_for(share, item), Some((share, item, "unbound_saved".into())),
        "the private save records the worn name for its own share before binding");
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    assert!(panel.theme_state().worn.as_ref().is_some_and(|s| s.name == "unbound_saved")
        && panel.theme_state().saved_names.iter().any(|n| n == "unbound_saved"),
        "binding the saved share restores its worn theme and member-local picker entry");
    button(cx, &mut panel, &mut s, ids!(tab_theme));
    pump(cx, &mut panel, &mut ana, &mut s);
    assert!(pick.selected_label() == "unbound_saved" && delete.visible(),
        "opening Theme selects the restored saved theme and shows delete");
    let saved = panel.theme_state().worn.clone().unwrap();
    button(cx, &mut panel, &mut s, ids!(theme_delete));
    let message = root.label(cx, ids!(theme_msg));
    assert!(message.visible() && message.text().contains("unbound_saved"), "delete awaits confirmation for the worn theme");
    panel.clear_worn(cx, &root, share, item);
    assert!(panel.theme_state().worn.is_none() && pick.selected_label() != "unbound_saved" && !delete.visible(),
        "clearing the still-bound share updates the Theme header immediately");
    assert!(!message.visible() && root.text_input(cx, ids!(theme_name)).text() == "unbound_saved",
        "clearing the worn theme dismisses the stale confirmation and preserves the name field");
    // Wear it again for the unbound-install check below.
    panel.apply_theme_tool_effect(cx, &root, ThemeEffect::InstalledSaved { share, item, saved });

    let closed_ui = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* View { tweak_panel := AppTweakPanel { visible: false } } });
        WidgetRef::script_from_value(vm, value)
    });
    let closed_root = closed_ui.widget(cx, ids!(tweak_panel));
    let mut closed = Tweaks::<u64,u64>::default();
    closed.set_themes_dir(owner.dir.clone());
    closed.sessions.push(Session::owner(s.host.session.town, share, item, owner.model.me(), LINK).unwrap());
    let closed_call = |cx: &mut Cx, closed: &mut Tweaks<u64,u64>, owner: &mut ThemeAgent, app: &mut FakeApp,
                       op: &str, args: &[(&str, json::Value)]| {
        let call = owner.call(op, item, args);
        assert!(closed.tool(cx, &closed_ui, &mut owner.model, &call, &mut owner.waits).unwrap().0.is_none(),
            "the closed panel's AI {op} waits for the shared app");
        loop {
            let session = &mut closed.sessions[0];
            let before = app.sent.len();
            session.drive_app(app);
            assert!(app.sent.len() > before, "the shared app has a request to settle for {op}");
            app.answer(session);
            closed.record(&mut owner.model);
            if op == "save" {
                assert!(matches!(closed.sessions[0].view.updates.make_contiguous(), [(_, _, Some(ai), Notice::Theme { .. })] if *ai == owner.agent),
                    "only the private AI snapshot is queued before the first panel update");
            }
            let mut answer = None;
            // On the first update, the closed panel's bind reads the store because the share has a recorded worn theme.
            closed.update(cx, &closed_ui, &owner.model, &Default::default(), &mut owner.waits, |r| answer = Some(theme_ok(r)));
            if let Some(answer) = answer { return answer }
        }
    };
    let closed_name = json::s("closed_saved");
    assert_eq!(closed_call(cx, &mut closed, &mut owner, &mut s.app, "save", &[("name", closed_name.clone())]).get("name"), Some(&closed_name),
        "the AI save settles through the previously unbound hub");
    assert!(!closed_root.visible() && closed_root.widget(cx, ids!(tweak_props_wrap)).visible()
        && !closed_root.widget(cx, ids!(tweak_theme_wrap)).visible(), "the AI save leaves the panel closed on Props");
    assert_eq!(closed_call(cx, &mut closed, &mut owner, &mut s.app, "get", &[]).get("worn"), Some(&closed_name),
        "the AI reads its saved theme as worn while the panel is closed");

    panel.refresh(cx, &root, &previous, &state, &Default::default());
    let actions = cx.capture_actions(|cx| cx.widget_action(tab, ButtonAction::Clicked(Default::default())));
    panel.actions(cx, &root, &actions, &mut previous);
    assert!(theme_wrap.visible() && panel.worn_for(share, item).is_some(),
        "the previously saved share has a worn theme to invalidate while the person views another share");
    ana.start(&mut s.ana, "install", &[("preset", json::s("light"))]);
    theme_ok(pump(cx, &mut panel, &mut ana, &mut s).unwrap());
    assert!(root.visible() && theme_wrap.visible() && !props_wrap.visible() && panel.worn_for(share, item).is_none(),
        "an unbound AI install clears that share's worn name and leaves the person on Theme");
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    assert!(panel.theme_state().worn.is_none(), "binding after the AI preset install cannot restore the stale saved theme");

    let other_share = [19; 16];
    let other_item = [20; 32];
    let mut other = Session::owner(s.host.session.town, other_share, other_item, owner.model.me(), LINK).unwrap();
    other.ready = true;
    closed.sessions.push(other);
    assert!(closed.open_panel(other_share));
    closed_call(cx, &mut closed, &mut owner, &mut s.app, "install", &[("preset", json::s("light"))]);
    let after_switch = closed_call(cx, &mut closed, &mut owner, &mut s.app, "get", &[]);
    assert_eq!(after_switch.get("base"), Some(&json::s("light")), "the AI installed Light on the outgoing share");
    assert_eq!(after_switch.get("worn"), Some(&json::Value::Null), "binding the next share cannot write back the cleared name");

    assert!(closed.open_panel(share));
    closed.update(cx, &closed_ui, &owner.model, &Default::default(), &mut owner.waits, |_| {});
    assert_eq!(closed_call(cx, &mut closed, &mut owner, &mut s.app, "get", &[]).get("worn"), Some(&json::Value::Null),
        "returning to the share keeps its cleared worn name");
    let tab = closed_root.widget(cx, ids!(tab_theme)).widget_uid();
    let actions = cx.capture_actions(|cx| cx.widget_action(tab, ButtonAction::Clicked(Default::default())));
    closed.actions(cx, &closed_ui, &actions);
    assert!(closed_root.widget(cx, ids!(tweak_theme_wrap)).visible()
        && closed_root.drop_down(cx, ids!(theme_pick)).selected_label() != "closed_saved"
        && !closed_root.widget(cx, ids!(theme_delete)).visible(), "the reopened Theme tab reflects the preset install");
}

#[test]
fn app_theme_get_pages_large_store_edits_and_full_snapshot_within_result_limit() {
    use makepad_ai_services::wire::{ToolOutcome, MAX_RESULT_BYTES};
    use json::Value::{Arr, Bool, Int, Null};
    let (_homes, _owner, mut ana, mut s) = theme_agents("theme-pages");
    s.allow(ana.model.me(), true);
    let mut raw = parse_json(&s.app.theme).unwrap();
    let tokens = raw.get("values").unwrap().as_arr().unwrap().to_vec();
    let mut expected_tokens = tokens.clone();
    expected_tokens.sort_by(|a,b| a.get("name").unwrap().as_str().cmp(&b.get("name").unwrap().as_str()));
    let edits: Vec<_> = expected_tokens.iter().map(|t| {
        let lit = super::theme::literal(super::theme::scalar(t.get("value").unwrap().as_str().unwrap()).unwrap());
        Arr(vec![t.get("name").unwrap().clone(),json::s(lit)])
    }).collect();
    let replace = |v: &mut json::Value, key, new| {
        let json::Value::Obj(fields) = v else { unreachable!() };
        fields.iter_mut().find(|(k,_)| k == key).unwrap().1 = new;
    };
    replace(&mut raw,"edits",Arr(edits[..3].to_vec()));
    s.app.theme = raw.to_json();
    std::fs::create_dir_all(&ana.dir).unwrap();
    let saved: Vec<_> = (0..1024).map(|i| format!("member_theme_{i:04}")).collect();
    let write_saved = |dir: &std::path::Path, name: &str| {
        let theme = theme::store::SavedTheme::new(name,makepad_widgets::theme_tokens::Scheme::Dark);
        std::fs::write(dir.join(format!("{name}.theme")), theme.to_text()).unwrap();
    };
    for name in &saved[..3] { write_saved(&ana.dir,name); }
    let font = theme_ok(ana.run(&mut s,"get",&[("group",json::s("font")),("filter",json::s("SIZE"))]));
    let len = |v: &json::Value, key| v.get(key).unwrap().as_arr().unwrap().len();
    let fonts: Vec<_> = expected_tokens.iter().filter(|t| t.get("name").unwrap().as_str().unwrap().starts_with("font_size")).cloned().collect();
    assert_eq!(font.get("tokens"),Some(&Arr(fonts)),"only filtered font tokens appear");
    assert_eq!((len(&font,"sheets"),len(&font,"saved"),len(&font,"edits"),font.get("next")), (51,3,3,Some(&Null)),
        "the filtered common case includes the whole catalogue, saved names, edits and tokens in one call");
    let first = theme_ok(ana.run(&mut s,"get",&[]));
    assert!(len(&first,"sheets") == 51 && len(&first,"saved") == 3 && len(&first,"edits") == 3 && len(&first,"tokens") > 0
        && first.get("next").unwrap().as_str().unwrap().starts_with("tokens:"),"common unfiltered metadata fits alongside the first token rows");

    for name in &saved[3..] { write_saved(&ana.dir,name); }
    replace(&mut raw,"edits",Arr(edits.clone()));
    s.app.theme = raw.to_json();
    let keys = ["sheets","saved","edits","tokens"];
    let mut lists: [Vec<json::Value>;4] = Default::default();
    let mut cursor = None;
    let mut last_section = 0;
    let mut largest = 0;
    loop {
        let args: Vec<_> = cursor.iter().cloned().map(|v| ("after",v)).collect();
        let reply = ana.run(&mut s,"get",&args);
        largest = largest.max(reply.text.len());
        let page = theme_ok(reply);
        for (key,total) in [("total_sheets",51),("total_saved",1024),("total_edits",620),("total_tokens",620)] {
            assert_eq!(page.get(key),Some(&Int(total)),"every page carries the complete {key}");
        }
        assert_eq!((page.get("base"),page.get("sheet"),page.get("has_mix"),page.get("has_pins"),page.get("worn")),
            (Some(&json::s("dark")),Some(&Null),Some(&Bool(false)),Some(&Bool(false)),Some(&Null)), "header scalars are repeated on every page");
        if cursor.is_none() {
            assert!(len(&page,"sheets") == 51 && len(&page,"saved") > 0 && page.get("next").unwrap().as_str().unwrap().starts_with("saved:"),
                "the oversized store starts after the complete sheet catalogue on page one");
        }
        let mut emitted = 0;
        for (i,key) in keys.iter().enumerate() {
            let entries = page.get(key).unwrap().as_arr().unwrap();
            if !entries.is_empty() {
                assert!(i >= last_section,"page sections never go backward");
                last_section = i;
                lists[i].extend_from_slice(entries);
                emitted += entries.len();
            }
        }
        assert!(emitted > 0, "each page advances through at least one entry");
        let next = page.get("next").unwrap();
        if next == &Null { break }
        let sizes = [51,1024,620,620];
        let section = (last_section..4).find(|&i| lists[i].len() < sizes[i]).expect("a continuation has rows remaining");
        assert_eq!(next.as_str(), Some(format!("{}:{}", keys[section], lists[section].len()).as_str()),
            "the cursor names the exact next row even at section boundaries");
        cursor = Some(next.clone());
    }
    let expected_sheets: Vec<_> = raw.get("sheets").unwrap().as_arr().unwrap().iter().cloned().map(|mut sheet| {
        if let json::Value::Obj(fields) = &mut sheet { fields.push(("mixable".into(), Bool(true))); }
        sheet
    }).collect();
    assert_eq!(lists[0],expected_sheets,"every shipped sheet is present, mixable and in picker order");
    assert_eq!(lists[1],saved.iter().map(json::s).collect::<Vec<_>>(),"all 1024 saved names arrive in order without duplicates");
    assert_eq!(lists[2],edits,"all edits arrive exactly once");
    assert_eq!(lists[3],expected_tokens,"all 620 sorted token rows arrive exactly once");
    assert!(largest <= MAX_RESULT_BYTES, "the largest complete page fits the result limit");
    let n = s.app.sent.len();
    for bad in ["","color_bg_app","foo:0","saved:-1","saved:01","tokens:"] {
        let refused = ana.start(&mut s.ana,"get",&[("after",json::s(bad))]).0.unwrap();
        assert!(refused.outcome == ToolOutcome::Refused && refused.text.contains("invalid app_theme_get cursor"), "malformed cursor {bad:?} is refused locally");
    }
    s.ana.poll();
    assert!(s.app.sent.len() == n && s.host.relay.take(TWEAK).iter().all(|(from,b)| !matches!(wire::decode(*from,b).unwrap().unwrap().body,Body::Request(_))),
        "invalid cursors generate no wire requests");
    let end = theme_ok(ana.run(&mut s,"get",&[("after",json::s("tokens:99999"))]));
    assert!(keys.iter().all(|key| len(&end,key) == 0) && end.get("next") == Some(&Null),"past-end cursors terminate with empty arrays");
    let huge = json::obj(vec![("name",json::s("color_big")),("value",json::s("#112233ff")),("site",json::s("s".repeat(20*1024)))]);
    replace(&mut raw,"values",Arr(vec![huge]));
    s.app.theme = raw.to_json();
    let failed = ana.run(&mut s,"get",&[("after",json::s("tokens:0"))]);
    assert!(failed.outcome == ToolOutcome::Failed && failed.text.contains("tokens:0") && failed.text.len() <= MAX_RESULT_BYTES,
        "an indivisible oversized token fails explicitly without dropping data or repeating a cursor");
}

#[test]
fn peer_literal_class_violation_refused_before_preview() {
    use design::session::validate_design_inputs;
    let insert = |ty: &str, name: &str, props: &[(&str, &str)]| DesignRequest::Insert {
        path: PATH.into(), place: DesignPlace::Inside, ty: ty.into(), name: Some(name.into()),
        props: props.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
    };
    assert!(validate_design_inputs(&insert("mod.View", "card", &[("draw_bg.color", "#1a2b3cff")])).is_ok());
    for (key, value) in [("pixel", "fn() { return vec4(1) }"), ("res", "crate_resource(\"x\")"),
        ("text", "\"#(0)\""), ("text", "\"@(0)\"")] {
        let error = validate_design_inputs(&insert("View", "card", &[(key, value)])).unwrap_err();
        assert!(error.contains(key) && error.contains("literal class"), "{error}");
    }
    for props in [&[("scope.x", "1")][..], &[("draw_bg.fn", "1")], &[("a..b", "1")], &[("x", "1"), ("x", "2")]] {
        assert!(validate_design_inputs(&insert("View", "card", props)).is_err());
    }
    for request in [
        DesignRequest::GhostInsert { target: PATH.into(), place: DesignPlace::Inside, ty: "scope.View".into() },
        DesignRequest::Wrap { path: PATH.into(), ty: "View#(0)".into(), name: None },
        DesignRequest::Duplicate { path: PATH.into(), name: Some("let".into()) },
        DesignRequest::Rename { path: PATH.into(), name: "a.b".into(), force_rust_refs: false },
        insert("mod..View", "card", &[]), insert("View", "return", &[]),
    ] {
        assert!(validate_design_inputs(&request).is_err(), "{request:?}");
    }
}

struct DesignTestDir(std::path::PathBuf);

impl DesignTestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("town-design-{name}-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for DesignTestDir {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); }
}

#[test]
fn workspace_root_confinement_blocks_outside_files() {
    use design::session::DesignSession;
    use std::fs;
    let fixture = DesignTestDir::new("confinement");
    let root = fixture.0.join("app");
    let nested = root.join("src");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join("Cargo.toml"), "[workspace] # the shared app\n").unwrap();
    let source = root.join("view.splash");
    fs::write(&source, "View{}").unwrap();
    let outside = fixture.0.join("outside.rs");
    fs::write(&outside, "outside").unwrap();

    let session = DesignSession::from_run_dir(nested.to_str());
    assert_eq!(session.check_confined(source.to_str().unwrap()).unwrap(), source.canonicalize().unwrap());
    for path in [&outside, &root.join("../outside.rs"), &nested, &root.join("Cargo.toml")] {
        assert!(session.read_slot(path.to_str().unwrap()).is_err(), "accepted {}", path.display());
    }
    assert!(session.check_confined("").is_err());
    assert!(session.check_confined("invalid\0.rs").is_err());
    assert!(DesignSession::from_run_dir(None).check_confined(source.to_str().unwrap()).is_err());
    assert!(DesignSession::from_run_dir(root.join("missing").to_str()).check_confined(source.to_str().unwrap()).is_err());

    #[cfg(unix)] {
        let link = nested.join("outside.rs");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(session.read_slot(link.to_str().unwrap()).unwrap_err().contains("outside the workspace"));
    }
    fs::write(nested.join(".git"), "gitdir: elsewhere\n").unwrap();
    let closer = DesignSession::from_run_dir(nested.to_str());
    assert!(closer.check_confined(source.to_str().unwrap()).is_err(), "nearest git worktree must win");
    let disk = "script_mod! { mod.UI = View{ card := View{} } }";
    let (mut share, _cx) = structural_share(&root, disk);
    share.allow(ANA, true);
    let source = share.app.design_source.as_mut().unwrap();
    let original = std::mem::replace(&mut source.file, outside.canonicalize().unwrap().to_str().unwrap().into());
    let outside_edit = guest_design(&mut share, drag_into("/card"));
    assert!(has(&share.ana, outside_edit, |n| matches!(n, Notice::Refused(err) if err.contains("outside the workspace"))));
    assert!(share.app.design_source.as_ref().unwrap().live.is_none());
    share.app.design_source.as_mut().unwrap().file = original;
    let json::Value::Obj(mut fields) = parse_json(THEME).unwrap() else { unreachable!() };
    fields.push(("define_file".into(), json::s(outside.to_str().unwrap())));
    share.app.theme = json::Value::Obj(fields).to_json();
    let bake = guest_design(&mut share, DesignRequest::Bake { scope: BakeScope::Theme, path: None, prop: None });
    assert!(has(&share.ana, bake, |n| matches!(n, Notice::Design(DesignNotice::Mutated { path, .. }) if path == "/")),
        "{:?}", share.ana.session.view.updates.back());
    assert!(share.app.sent.iter().any(|(_,_,op)| matches!(op, DesignOp::Design {verb: DesignVerb::Tree,..})));
    assert!(share.app.design_source.as_ref().unwrap().text().contains("mod.theme +:"));
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside");
    assert_eq!(fs::read_to_string(root.join("ui.rs")).unwrap(), disk);
}

#[test]
fn export_new_patch_direct_fallback_and_flat_staging_preserve_existing() {
    use design::session::{export_new_patch, write_new_patch_direct};
    use std::{collections::BTreeSet, fs, sync::atomic::AtomicBool};

    let fixture = DesignTestDir::new("export-new-patch");
    let root = fixture.0.canonicalize().unwrap();
    let dir = root.join("local/design");
    let entries = || fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path()).collect::<BTreeSet<_>>();
    let primary = dir.join("ui.patch");
    assert_eq!(export_new_patch(&root, "ui", "first diff", &AtomicBool::new(false)).unwrap(), primary);
    assert_eq!(fs::read_to_string(&primary).unwrap(), "first diff");
    assert_eq!(entries(), BTreeSet::from([primary.clone()]), "first export left staging entries");

    fs::write(&primary, "owner notes").unwrap();
    let second = export_new_patch(&root, "ui", "second diff", &AtomicBool::new(false)).unwrap();
    let nonce = second.file_stem().unwrap().to_str().unwrap().strip_prefix("ui-").unwrap();
    assert_eq!(nonce.len(), 32);
    assert!(nonce.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(second, dir.join(format!("ui-{nonce}.patch")));
    assert_eq!(fs::read_to_string(&primary).unwrap(), "owner notes");
    assert_eq!(fs::read_to_string(&second).unwrap(), "second diff");
    let published = BTreeSet::from([primary, second]);
    assert_eq!(entries(), published, "collision left staging entries");
    assert_eq!(export_new_patch(&root, "cancelled", "cancelled diff", &AtomicBool::new(true)).unwrap_err(),
        "patch export cancelled");
    assert_eq!(entries(), published, "cancellation left a staged or published file");

    let nonce = "0123456789abcdef0123456789abcdef";
    let write = |stem| write_new_patch_direct(&dir, stem, nonce, "direct diff");
    let primary = dir.join("___a__.patch");
    assert_eq!(write("../a/é").unwrap(), primary);
    assert_eq!(fs::read_to_string(&primary).unwrap(), "direct diff");
    fs::write(&primary, "owner").unwrap();
    let fallback = dir.join(format!("___a__-{nonce}.patch"));
    assert_eq!(write("../a/é").unwrap(), fallback);
    assert_eq!(fs::read_to_string(&primary).unwrap(), "owner");
    assert_eq!(fs::read_to_string(fallback).unwrap(), "direct diff");

    #[cfg(unix)]
    {
        let sentinel = root.join("sentinel");
        fs::write(&sentinel, "sentinel").unwrap();
        let missing = root.join("missing");
        for (stem, target) in [("link", &sentinel), ("dangling", &missing)] {
            let link = dir.join(format!("{stem}.patch"));
            std::os::unix::fs::symlink(target, &link).unwrap();
            let fallback = dir.join(format!("{stem}-{nonce}.patch"));
            assert_eq!(write(stem).unwrap(), fallback);
            assert_eq!(fs::read_to_string(fallback).unwrap(), "direct diff");
            assert_eq!(fs::read_link(&link).unwrap(), *target, "direct export changed the symlink");
        }
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "sentinel");
        assert!(!missing.exists(), "direct export followed the dangling link");
    }
}

#[test]
fn disk_commit_refuses_when_disk_changed_externally() {
    use design::{patch::unified_patch, session::DesignSession};
    use makepad_widgets::makepad_platform::script_mod_doc::{DocKind, ScriptModDoc};
    use std::fs;
    let fixture = DesignTestDir::new("commit");
    let root = &fixture.0;
    let file = root.join("view.rs");
    let first = root.join("first.rs");
    let original = "script_mod! { mod.Card = View{} }\n";
    let edited = "script_mod! { mod.Card = View{width: 12} }\n";
    let mut design = DesignSession::with_root(root.clone());
    for path in [&first, &file] {
        fs::write(path, original).unwrap();
        let mut slot = design.read_slot(path.to_str().unwrap()).unwrap();
        slot.working = ScriptModDoc::parse(&slot.path, edited.to_owned(), DocKind::RustScriptMod).unwrap();
        std::sync::Arc::make_mut(&mut design.files).insert(slot.path.clone(), slot);
    }
    let key = file.canonicalize().unwrap().to_str().unwrap().to_owned();

    fs::write(&file, "// external change\n").unwrap();
    let err = design.commit_to_disk().unwrap_err();
    assert_eq!(err, "file changed on disk; reset or export patch first");
    assert_eq!(fs::read_to_string(&first).unwrap(), original, "another dirty file was committed before the refusal");
    assert!(!first.with_added_extension("town-tmp").exists());
    assert_eq!(fs::read_to_string(&file).unwrap(), "// external change\n");
    let slot = &design.files[&key];
    assert_eq!(slot.disk_text, original);
    assert_eq!(slot.working.text, edited);
    assert!(unified_patch(root, &file, &slot.disk_text, &slot.working.text).unwrap().contains("+script_mod!"));
    let temp = file.with_added_extension("town-tmp");
    assert!(!temp.exists(), "failed commit left a temp file");

    fs::write(&file, original).unwrap();
    fs::write(&temp, "owned by another writer").unwrap();
    assert!(design.commit_to_disk().unwrap_err().contains(temp.canonicalize().unwrap().to_str().unwrap()));
    assert_eq!(fs::read_to_string(&temp).unwrap(), "owned by another writer");
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
    fs::remove_file(&temp).unwrap();
    design.commit_to_disk().unwrap();
    assert_eq!(fs::read_to_string(&first).unwrap(), edited);
    assert_eq!(fs::read_to_string(&file).unwrap(), edited);
    assert_eq!(design.state_view().dirty_files, 0);
    assert_eq!(design.files[&key].disk_hash, design.files[&key].working.hash());
    assert!(!temp.exists());
}

#[test]
fn disk_commit_quarantines_only_provably_dead_town_temps() {
    use design::session::DesignSession;
    use makepad_widgets::makepad_platform::script_mod_doc::ScriptModDoc;
    use std::{fs, path::Path, sync::Arc};

    let fixture = DesignTestDir::new("crashed-temp");
    let root = fixture.0.canonicalize().unwrap();
    let file = root.join("view.rs");
    let original = "script_mod! { mod.Card = View{} }\n";
    let edited = "script_mod! { mod.Card = View{width: 12} }\n";
    let mut design = DesignSession::with_root(root.clone());
    let edit = |design: &mut DesignSession, path: &Path, text: &str| {
        let mut slot = design.read_slot(path.to_str().unwrap()).unwrap();
        slot.working = ScriptModDoc::parse(&slot.path, text.into(), slot.working.kind).unwrap();
        Arc::make_mut(&mut design.files).insert(slot.path.clone(), slot);
    };
    let no_crashes = || assert!(fs::read_dir(&root).unwrap().all(|entry|
        entry.unwrap().path().extension().is_none_or(|ext| ext != "crashed")), "refusal created a .crashed file");
    let ambiguous = |design: &mut DesignSession, path: &Path| {
        assert_eq!(design.commit_to_disk().unwrap_err(), format!("temporary source already exists: {}", path.display()),
            "refusal must identify the ambiguous entry");
        assert_eq!(fs::read_to_string(&file).unwrap(), original, "refusal changed the source");
        no_crashes();
    };
    fs::write(&file, original).unwrap();
    edit(&mut design, &file, edited);
    for suffix in ["town-tmp", "town-tmp.bad", "town-tmp.0.123", "town-tmp.123.0", "town-tmp.01.123",
        "town-tmp.1.+2", "town-tmp.1.02", "town-tmp.1.2.3"] {
        let path = file.with_added_extension(suffix);
        fs::write(&path, "foreign").unwrap();
        ambiguous(&mut design, &path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "foreign", "refusal changed {suffix}");
        fs::remove_file(path).unwrap();
    }
    // Apple file systems refuse a non-UTF-8 name (EILSEQ), so such a temp cannot exist there.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = root.join(OsStr::from_bytes(b"view.rs.town-tmp.42.\xff"));
        fs::write(&path, "foreign bytes").unwrap();
        ambiguous(&mut design, &path);
        assert_eq!(fs::read(&path).unwrap(), b"foreign bytes", "a non-UTF-8 name stays untouched");
        fs::remove_file(path).unwrap();
    }

    let pid = std::process::id();
    let absent = file.with_added_extension("town-tmp.4294967295.12345");
    let Some(start) = makepad_remote::group::start_time(pid).filter(|&t| t > 0) else {
        fs::write(&absent, "unknown writer").unwrap();
        ambiguous(&mut design, &absent);
        assert_eq!(fs::read_to_string(&absent).unwrap(), "unknown writer", "a blind probe preserves the temp");
        fs::remove_file(absent).unwrap();
        design.commit_to_disk().unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), edited, "legacy staging still commits");
        return;
    };
    let live = file.with_added_extension(format!("town-tmp.{pid}.{start}"));
    fs::write(&live, "in progress").unwrap();
    assert_eq!(design.commit_to_disk().unwrap_err(), format!("temporary source is in use by pid {pid}: {}", live.display()),
        "refusal identifies the live process and its temp");
    assert_eq!(fs::read_to_string(&live).unwrap(), "in progress", "refusal changed a live writer's temp");
    assert_eq!(fs::read_to_string(&file).unwrap(), original, "a live writer blocks the commit");
    no_crashes();
    fs::remove_file(live).unwrap();

    let wrong_start = if start == 1 { 2 } else { start ^ 1 };
    let dead_suffix = format!("town-tmp.{pid}.{wrong_start}");
    let dead = file.with_added_extension(&dead_suffix);
    let crashed = file.with_added_extension(format!("town.{pid}.{wrong_start}.crashed"));
    #[cfg(unix)]
    {
        let sentinel = root.join("sentinel");
        fs::write(&sentinel, "sentinel").unwrap();
        std::os::unix::fs::symlink(&sentinel, &dead).unwrap();
        ambiguous(&mut design, &dead);
        assert_eq!(fs::read_link(&dead).unwrap(), sentinel, "refusal preserved the symlink");
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "sentinel", "refusal preserved the symlink target");
        fs::remove_file(&dead).unwrap();
    }
    #[cfg(any(unix, windows))]
    {
        fs::write(&dead, "locked").unwrap();
        let locked = fs::File::open(&dead).unwrap();
        locked.try_lock().unwrap();
        assert_eq!(design.commit_to_disk().unwrap_err(), format!("temporary source is in use by pid {pid}: {}", dead.display()),
            "a held lock protects the temp even when the process start differs");
        no_crashes();
        drop(locked);
        assert_eq!(fs::read_to_string(&dead).unwrap(), "locked", "refusal preserved the locked temp");
        assert_eq!(fs::read_to_string(&file).unwrap(), original, "a locked temp blocks the commit");
    }

    let first = root.join("first.rs");
    fs::write(&first, original).unwrap();
    edit(&mut design, &first, edited);
    let first_dead = first.with_added_extension(&dead_suffix);
    fs::write(&first_dead, "first crash").unwrap();
    let blocker = file.with_added_extension("town-tmp");
    fs::write(&blocker, "foreign").unwrap();
    ambiguous(&mut design, &blocker);
    assert_eq!(fs::read_to_string(&first_dead).unwrap(), "first crash", "a later refusal prevented earlier quarantine");
    assert_eq!(fs::read_to_string(&first).unwrap(), original, "a later refusal prevented earlier replacement");
    fs::remove_file(blocker).unwrap();

    fs::write(&absent, "absent writer").unwrap();
    fs::write(&dead, "crashed partial write").unwrap();
    design.commit_to_disk().unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), edited, "recovery installed the edit");
    assert!(!dead.exists() && !absent.exists(), "recovery cleared both stale names");
    assert_eq!(fs::read_to_string(&crashed).unwrap(), "crashed partial write", "preserved the previous process's temp");
    assert_eq!(fs::read_to_string(file.with_added_extension("town.4294967295.12345.crashed")).unwrap(), "absent writer",
        "preserved the absent writer's temp");

    fs::write(&crashed, "older crash").unwrap();
    fs::write(&dead, "newer crash").unwrap();
    edit(&mut design, &file, original);
    design.commit_to_disk().unwrap();
    let numbered = file.with_added_extension(format!("town.{pid}.{wrong_start}.1.crashed"));
    let both_crashes = || {
        assert_eq!(fs::read_to_string(&crashed).unwrap(), "older crash", "preserved the older crash");
        assert_eq!(fs::read_to_string(&numbered).unwrap(), "newer crash", "preserved the newer crash under its own name");
    };
    assert!(!dead.exists(), "the second crash also cleared the stale name");
    assert_eq!(fs::read_to_string(&file).unwrap(), original, "an existing crash allowed another commit");
    both_crashes();
    edit(&mut design, &file, edited);
    design.commit_to_disk().unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), edited, "later commits ignore the quarantined files");
    both_crashes();
}

#[test]
#[cfg(any(unix, windows))]
fn quarantine_refuses_when_stale_temp_is_replaced_after_preflight() {
    use design::session::{collect_stale_temps, quarantine_crashed_temp, StaleTemp};
    use std::fs;
    let fixture = DesignTestDir::new("quarantine-replaced");
    let root = fixture.0.canonicalize().unwrap();
    let file = root.join("view.rs");
    let me = std::process::id();
    let (pid, start) = makepad_remote::group::start_time(me).filter(|&t| t > 0)
        .map_or((u32::MAX, 1), |start| (me, start));
    let wrong_start = if start == 1 { 2 } else { start ^ 1 };
    let dead = file.with_added_extension(format!("town-tmp.{pid}.{wrong_start}"));
    fs::write(&dead, "old crash").unwrap();
    let preflight = || {
        let mut stale = Vec::new();
        collect_stale_temps(&file, Some(start), &mut stale).unwrap();
        assert_eq!(stale.len(), 1);
        stale
    };
    let refused = |stale: &StaleTemp| {
        assert_eq!(quarantine_crashed_temp(stale).unwrap_err(),
            format!("temporary source already exists: {}", dead.display()));
        assert!(fs::read_dir(&root).unwrap().all(|e| e.unwrap().path().extension().is_none_or(|ext| ext != "crashed")));
    };
    let stale = preflight();
    let replacement = root.join("replacement");
    fs::write(&replacement, "fresh writer").unwrap();
    fs::rename(&replacement, &dead).unwrap();
    refused(&stale[0]);
    assert_eq!(fs::read_to_string(&dead).unwrap(), "fresh writer");

    #[cfg(unix)]
    {
        let second = preflight();
        let sentinel = root.join("sentinel");
        fs::write(&sentinel, "sentinel").unwrap();
        std::os::unix::fs::symlink(&sentinel, &replacement).unwrap();
        fs::rename(&replacement, &dead).unwrap();
        refused(&second[0]);
        assert_eq!(fs::read_link(&dead).unwrap(), sentinel);
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "sentinel");
    }
}

#[test]
fn map_live_to_pre_ghost_across_edits() {
    use design::session::map_live_to_pre_ghost;
    use makepad_widgets::makepad_script::syntax::edit::Replace;
    // "abCDxy12!" -> "abGHOSTxy!" (replacement followed by deletion).
    let edits = [Replace::new(2..4, "GHOST"), Replace::new(6..8, "")];
    for (live, pre) in [(0, 0), (1, 1), (7, 4), (8, 5), (9, 8), (10, 9)] {
        assert_eq!(map_live_to_pre_ghost(live, &edits), Ok(pre), "live offset {live}");
    }
    for live in 2..7 {
        assert_eq!(map_live_to_pre_ghost(live, &edits).unwrap_err(), "cannot target the active ghost");
    }
}

enum VerifyFailure { ReloadError, PendingReload, MalformedOkPendingReload, RolledBack }

#[derive(Default)]
struct FakeDesignSource {
    palette: Option<String>,
    file: String,
    disk: String,
    live: Option<String>,
    previous_live: Option<String>,
    chunks: Vec<u8>,
    mismatch: bool,
    verify_calls: usize,
    verify_failure: Option<VerifyFailure>,
}

impl FakeDesignSource {
    fn text(&self) -> &str { self.live.as_deref().unwrap_or(&self.disk) }

    fn answer(&mut self, verb: DesignVerb, args: &[(String, String)]) -> Result<String, String> {
        use makepad_widgets::design_data::locate::source_hash;
        let arg = |k| args.iter().find(|(key,_)| key == k).map(|(_,v)| v.as_str());
        let decoded = |s: &str| makepad_ai_hub::makepad_base64::base64_decode(s.as_bytes()).unwrap();
        match verb {
            DesignVerb::Palette => Ok(self.palette.clone().unwrap_or_else(|| r#"{"palette":[{"name":"View","group":"layout"},{"name":"Grid","group":"layout"},{"name":"Label","group":"text"}]}"#.into())),
            DesignVerb::Drop => Ok("null".into()),
            DesignVerb::Locate => {
                let path = arg("path").unwrap();
                let name = widget_name(path);
                let text = self.text();
                let start = text.find(&format!("{name} :=")).ok_or("widget missing")?;
                let open = start + text[start..].find('{').unwrap();
                Ok(format!(r#"{{"file":{},"file_hash":"{:016x}","block":0,"open":{open},"preview":{}}}"#,
                    json::s(&self.file).to_json(), source_hash(text), self.live.is_some()))
            }
            DesignVerb::Verify => {
                self.verify_calls += 1;
                let reply = match self.verify_failure.take() {
                    Some(VerifyFailure::ReloadError) => r#"{"ok":false,"rolled_back":false,"errors":["reload failed"]}"#,
                    Some(VerifyFailure::PendingReload) => r#"{"ok":false,"pending_reload":true,"errors":[]}"#,
                    Some(VerifyFailure::MalformedOkPendingReload) => r#"{"ok":true,"pending_reload":true,"errors":[]}"#,
                    Some(VerifyFailure::RolledBack) => {
                        self.live = self.previous_live.take();
                        r#"{"ok":false,"rolled_back":true,"errors":["script error"]}"#
                    }
                    None => r#"{"ok":true,"errors":[]}"#,
                };
                Ok(reply.into())
            }
            DesignVerb::PreviewChunk => {
                assert_eq!(arg("file"), Some(self.file.as_str()));
                let seq: usize = arg("seq").unwrap().parse().unwrap();
                if seq == 0 { self.chunks.clear(); }
                self.chunks.extend(decoded(arg("data").unwrap()));
                Ok(format!(r#"{{"ok":true,"next_seq":{}}}"#, seq+1))
            }
            DesignVerb::Preview | DesignVerb::Revert => {
                assert_eq!(arg("file"), Some(self.file.as_str()));
                let next = if matches!(verb, DesignVerb::Revert) { None }
                else if let Some(edits) = arg("edits") {
                    if std::mem::take(&mut self.mismatch) { return Err(r#"{"err":"hash_mismatch"}"#.into()); }
                    assert_eq!(arg("base_hash").unwrap(), format!("{:016x}", source_hash(self.text())));
                    let bytes = decoded(edits);
                    let edits = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
                    let mut next = self.text().to_owned();
                    for edit in edits.as_arr().unwrap().iter().rev() {
                        let [a,b,text] = edit.as_arr().unwrap() else { panic!("invalid edit triple") };
                        next.replace_range(a.as_u64().unwrap() as usize..b.as_u64().unwrap() as usize, text.as_str().unwrap());
                    }
                    Some(next)
                } else if arg("upload").is_some() {
                    Some(String::from_utf8(std::mem::take(&mut self.chunks)).unwrap())
                } else { Some(arg("text").unwrap().into()) };
                self.previous_live = std::mem::replace(&mut self.live, next);
                Ok(format!(r#"{{"ok":true,"file_hash":"{:016x}","pending_reload":true,"errors":[]}}"#, source_hash(self.text())))
            }
            _ => unreachable!(),
        }
    }
}

fn structural_share(root: &std::path::Path, disk: &str) -> (Copies, Cx) {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_agents(&ana_agent_state(OWNER));
    let cx = configure_design_share(&mut s, root, disk);
    (s, cx)
}

fn structural_copies(root: &std::path::Path, disk: &str, pool: &makepad_widgets::makepad_platform::thread::TaskPool) -> Copies {
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_agents(&ana_agent_state(OWNER));
    configure_design_with_pool(&mut s, root, disk, pool);
    s
}

fn configure_design_share(s: &mut Copies, root: &std::path::Path, disk: &str) -> Cx {
    let cx = Cx::new(Box::new(|_,_| {}));
    configure_design_with_pool(s, root, disk, &cx.task_pool());
    cx
}

fn configure_design_with_pool(s: &mut Copies, root: &std::path::Path, disk: &str, pool: &makepad_widgets::makepad_platform::thread::TaskPool) {
    let path = root.join("ui.rs");
    std::fs::write(&path, disk).unwrap();
    s.host.session.set_workspace_root(root.to_owned());
    s.host.session.configure_design_worker(pool);
    s.app.design_source = Some(FakeDesignSource { file: path.canonicalize().unwrap().to_str().unwrap().into(),
        disk: disk.into(), ..Default::default() });
    let owner = s.host.session.me;
    for peer in [s.ana.session.me, BEN] { s.host.session.set_peer_level(peer, Some(DESIGN_LEVEL)); }
    s.ana.session.set_peer_level(owner, Some(DESIGN_LEVEL));
    s.ben.session.set_peer_level(owner, Some(DESIGN_LEVEL));
}

fn settle_design(s: &mut Copies) {
        s.one();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while s.host.session.has_unsettled_edits() {
            assert!(std::time::Instant::now() < deadline, "structural pipeline failed to settle");
            s.one();
            std::thread::yield_now();
        }
}

#[test]
fn guest_structural_edit_requires_allow_consent_and_records_attribution() {
    let fixture = DesignTestDir::new("peer-insert");
    let path = fixture.0.join("ui.rs");
    let disk = "fn selected() { id!(card); }\nscript_mod! { mod.UI = View{ root := View{} } }\n";
    let (mut s, _cx) = structural_share(&fixture.0, disk);
    let send = |s: &mut Copies, r| s.ana.session.request(Some(ANA_AGENT), Request::Design(r)).unwrap();
    let insert = |name: &str, value: &str| DesignRequest::Insert { path: "/root".into(), place: DesignPlace::Inside,
        ty: "View".into(), name: Some(name.into()), props: vec![("width".into(),"12".into()), ("draw_bg.color".into(),value.into())] };
    let state = || DesignRequest::State { path: None };
    let finish = settle_design;

    let query = send(&mut s, state());
    finish(&mut s);
    assert!(has(&s.ana, query, |n| matches!(n, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, .. }))));
    assert!(!s.ben.session.view.agents.contains_key(&(ANA, ANA_AGENT)), "private read announced its AI");
    let denied = send(&mut s, insert("card", "#abc"));
    finish(&mut s);
    assert!(has(&s.ana, denied, |n| matches!(n, Notice::Refused(v) if v.contains("Ask for control"))));
    assert!(!has(&s.ben, denied, |_| true));
    s.allow(ANA, true);
    let invalid = send(&mut s, insert("bad", "crate_resource(\"x\")"));
    finish(&mut s);
    assert!(has(&s.ana, invalid, |n| matches!(n, Notice::Refused(v) if v.contains("literal class"))));
    assert!(s.app.sent.is_empty() && s.records.is_empty());

    s.allow(ANA, false);
    s.host.session.bind_window(WindowId(0));
    let wrong_window = send(&mut s, DesignRequest::DropTarget { window: 1, x: 0., y: 0., exclude: None });
    finish(&mut s);
    assert!(has(&s.ana, wrong_window, |n| matches!(n, Notice::Refused(v) if v.contains("outside this share"))));
    assert!(s.app.sent.is_empty());
    let palette = send(&mut s, DesignRequest::Palette { query: Some("LAYOUT".into()), offset: 1, limit: 1 });
    finish(&mut s);
    assert!(has(&s.ana, palette, |n| matches!(n, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::Palette, json })
        if parse_json(json).is_ok_and(|v| v.get("total") == Some(&json::Value::Int(2))
            && v.get("palette").and_then(json::Value::as_arr).is_some_and(|p| p.len()==1 && p[0].get("name")==Some(&json::s("Grid")))))));
    let drop = send(&mut s, DesignRequest::DropTarget { window: 0, x: 1., y: 2., exclude: None });
    finish(&mut s);
    assert!(has(&s.ana, drop, |n| matches!(n, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::DropTarget, json }) if json == "null")));
    let verify = send(&mut s, DesignRequest::Verify { path: None });
    finish(&mut s);
    assert!(has(&s.ana, verify, |n| matches!(n, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::Verify, .. }))));
    s.allow(ANA, true);

    let verify_calls = |s: &Copies| s.app.design_source.as_ref().unwrap().verify_calls;
    let before = verify_calls(&s);
    let id = send(&mut s, insert("card", "#1a2b3cff"));
    let queued = send(&mut s, state());
    s.one(); // locate answered; preparation and preview still own the FIFO
    assert!(!has(&s.ana, queued, |_| true));
    finish(&mut s);
    s.one();
    assert_eq!(verify_calls(&s) - before, 1, "an accepted edit verifies its preview once");
    assert!(has(&s.ana, queued, |n| matches!(n, Notice::Design(DesignNotice::Reply { .. }))));
    assert!(matches!(&s.records[..], [AppEdit { seat: ANA, agent: Some(ANA_AGENT), path, prop,
        change: Change::Applied { seq: 1, .. } }] if path == "/root" && prop == "design:insert"));
    assert!(has(&s.ben, id, |n| matches!(n, Notice::Design(DesignNotice::Running { .. }))));
    assert!(has(&s.ben, id, |n| matches!(n, Notice::Design(DesignNotice::Mutated { path, focus_path, seq: 1, .. })
        if path == "/root" && focus_path.as_deref() == Some("/root/card"))));
    assert_eq!(s.ben.session.view.design.undo, 1);
    let inserted = s.app.design_source.as_ref().unwrap().text().to_owned();
    assert!(inserted.contains("card :=") && inserted.contains("#x1a2b3cff") && !inserted.contains("\"draw_bg.color\""), "{inserted}");

    // The app rejected verify after accepting the preview. Restore the previous override
    // before releasing the FIFO, including when the compact preview required a full sync.
    let app = s.app.design_source.as_mut().unwrap();
    app.mismatch = true;
    app.verify_failure = Some(VerifyFailure::ReloadError);
    let bad = send(&mut s, insert("second", "#fff"));
    finish(&mut s);
    assert!(has(&s.ana, bad, |n| matches!(n, Notice::Refused(v) if v == "reload failed")));
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), inserted);
    assert_eq!(s.records.len(), 1);
    assert_eq!(s.ben.session.view.design.undo, 1);

    let preview_and_verify = |s: &Copies, from: usize| s.app.sent[from..].iter().filter_map(|(_, _, op)| match op {
        DesignOp::Design { verb: verb @ (DesignVerb::Preview | DesignVerb::Revert | DesignVerb::Verify), .. } => Some(verb.wire_name()),
        _ => None,
    }).collect::<Vec<_>>();
    for failure in [VerifyFailure::PendingReload, VerifyFailure::MalformedOkPendingReload] {
        let before = verify_calls(&s);
        let from = s.app.sent.len();
        let records = s.records.len();
        s.app.design_source.as_mut().unwrap().verify_failure = Some(failure);
        let pending = send(&mut s, insert("second", "#fff"));
        finish(&mut s);
        assert_eq!(verify_calls(&s) - before, 2, "the candidate and its restore each verify once");
        assert_eq!(preview_and_verify(&s, from), ["preview", "verify", "preview", "verify"],
            "a pending reply goes directly to restoring the previous preview");
        assert!(has(&s.ana, pending, |n| matches!(n, Notice::Refused(v) if v == "target app did not finish reload before verify")),
            "the guest sees why the pending candidate was refused");
        assert_eq!(s.app.design_source.as_ref().unwrap().text(), inserted, "Town restores the previous preview text");
        assert_eq!(s.records.len(), records, "pending reload published an edit record");
    }

    let before = verify_calls(&s);
    let from = s.app.sent.len();
    s.app.design_source.as_mut().unwrap().verify_failure = Some(VerifyFailure::RolledBack);
    let rolled_back = send(&mut s, insert("second", "#fff"));
    finish(&mut s);
    assert_eq!(verify_calls(&s) - before, 1, "an app-side rollback verifies only the candidate");
    assert_eq!(preview_and_verify(&s, from), ["preview", "verify"],
        "Town sends no restore preview or revert after an app-side rollback");
    assert!(has(&s.ana, rolled_back, |n| matches!(n, Notice::Refused(v) if v == "script error")),
        "the guest sees the app's script error");
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), inserted, "the app restored its prior override");

    for (r, op, undo, redo) in [(DesignRequest::Undo,"undo",0,1), (DesignRequest::Redo,"redo",1,0)] {
        let id = send(&mut s,r);
        finish(&mut s);
        assert!(has(&s.ben, id, |n| matches!(n, Notice::Design(DesignNotice::Mutated { op: actual, path, .. }) if actual == op && path == "/")));
        assert_eq!((s.ana.session.view.design.undo,s.ana.session.view.design.redo), (undo,redo));
    }
    let rename = |force| DesignRequest::Rename { path: "/root/card".into(), name: "renamed".into(), force_rust_refs: force };
    let refused = send(&mut s, rename(false));
    finish(&mut s);
    assert!(has(&s.ana, refused, |n| matches!(n, Notice::Refused(v) if parse_json(v).is_ok_and(|w|
        w.get("rust_refs").and_then(json::Value::as_arr).is_some_and(|r| r[0].get("line") == Some(&json::Value::Int(1)))))));
    for (r, op) in [
        (rename(true), "rename"),
        (DesignRequest::Duplicate { path: "/root/renamed".into(), name: Some("other".into()) }, "duplicate"),
        (DesignRequest::Step { path: "/root/other".into(), dir: StepDir::Up }, "up"),
        (DesignRequest::Step { path: "/root/other".into(), dir: StepDir::Down }, "down"),
        (DesignRequest::Wrap { path: "/root/other".into(), ty: "View".into(), name: Some("wrapper".into()) }, "wrap"),
        (DesignRequest::Move { path: "/root/renamed".into(), target: "/root/wrapper".into(), place: DesignPlace::Inside }, "move"),
        (DesignRequest::Step { path: "/root/wrapper/renamed".into(), dir: StepDir::Out }, "out"),
        (DesignRequest::Delete { path: "/root/renamed".into(), force_rust_refs: false }, "delete"),
        (DesignRequest::Reset, "reset"),
    ] {
        let id = send(&mut s,r);
        finish(&mut s);
        assert!(has(&s.ana, id, |n| matches!(n, Notice::Design(DesignNotice::Mutated { op: actual, .. }) if actual == op)),
            "{op}: {:?}", s.ana.session.view.updates.back());
    }
    assert_eq!(s.ben.session.view.design.dirty_files, 0);
    let records = s.records.len();
    let interrupted = send(&mut s, insert("pending", "#abc"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.app.design_source.as_ref().unwrap().live.is_none() {
        s.one();
        assert!(std::time::Instant::now() < deadline, "preview was never installed");
        std::thread::yield_now();
    }
    s.allow(ANA, false); // after Preview and before Verify: revert the previously unopened file
    finish(&mut s);
    assert!(has(&s.ana, interrupted, |n| matches!(n, Notice::Refused(v) if v.contains("Control ended"))));
    assert_eq!(s.records.len(), records);
    assert!(s.app.design_source.as_ref().unwrap().live.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(),disk);
    s.host.session.stop_agent(ANA, ANA_AGENT, true).unwrap();
    let stopped = send(&mut s, state());
    finish(&mut s);
    assert!(has(&s.ana, stopped, |n| matches!(n, Notice::Refused(v) if v.contains("stopped"))));

    // Inject an append failure at the real signed-record boundary after successful Verify.
    let unsaved = s.host.session.request(None, Request::Design(insert("unsaved", "#fff"))).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.host.session.next_record().is_none() {
        s.host.session.drive_app(&mut s.app);
        s.app.answer(&mut s.host.session);
        assert!(std::time::Instant::now() < deadline, "verified edit never reached the recorder");
        std::thread::yield_now();
    }
    s.host.session.record_failed("signed append failed");
    finish(&mut s);
    assert!(has(&s.host, unsaved, |n| matches!(n, Notice::Refused(v) if v == "signed append failed")));
    assert!(!has(&s.ben, unsaved, |n| matches!(n, Notice::Design(DesignNotice::Mutated { .. }))));
    assert!(s.app.design_source.as_ref().unwrap().live.is_none());

}

fn drag_into(target: &str) -> DesignRequest {
    DesignRequest::GhostInsert { target: target.into(), place: DesignPlace::Inside, ty: "Label".into() }
}

fn guest_design(s: &mut Copies, request: DesignRequest) -> RequestId {
    let id = s.ana.session.request(Some(ANA_AGENT), Request::Design(request)).unwrap();
    settle_design(s);
    id
}

fn design_reply_value(town: &TownApp, id: RequestId, wanted: DesignReplyKind) -> json::Value {
    town.session.view.updates.iter().rev().find_map(|(_,req,_,notice)| match notice {
        Notice::Design(DesignNotice::Reply { kind, json }) if *req == id && *kind == wanted => parse_json(json).ok(),
        _ => None,
    }).unwrap_or_else(|| panic!("missing design reply {wanted:?}: {:?}", town.session.view.updates.back()))
}

#[test]
fn ghost_move_squashes_and_commits_without_extra_reload() {
    let fixture = DesignTestDir::new("ghost-move");
    let disk = "script_mod! { mod.UI = View{ a := View{label := View{}} b := View{} } }";
    let (mut s, _cx) = structural_share(&fixture.0, disk);
    s.allow(ANA, true);
    let pick = r#"{"window":0,"rect":[1,2,3,4],"ref":"/a/label_2","type":"Label"}"#;
    s.app.picks.push_back(Ok(pick.into()));
    guest_design(&mut s, drag_into("/a"));
    let ghost = s.ben.session.view.design.ghost.as_ref().expect("verified dwell");
    assert_eq!(ghost.seat, ANA);
    assert_eq!(ghost.rect.unwrap().w, 3., "pick used the fresh child name");
    let held = s.ben.session.view.design.json();
    let calls = s.app.sent.len();
    let cancel = s.host.send(Request::Design(DesignRequest::GhostCancel));
    settle_design(&mut s);
    let state = design_reply_value(&s.host, cancel, DesignReplyKind::State);
    assert_eq!(state.get("ghost"), held.get("ghost"), "a non-holder leaves Ana's preview intact");
    assert_eq!(s.app.sent.len(), calls, "cancelling another seat's ghost makes no app calls");
    guest_design(&mut s, drag_into("/a"));
    let contested = s.host.send(Request::Design(DesignRequest::GhostCommit));
    settle_design(&mut s);
    assert!(has(&s.host, contested, |n| matches!(n, Notice::Refused(e) if e.contains("dragging"))));
    assert_eq!(s.app.sent.len(), calls, "same-target and other-holder requests must not reload");
    let read = guest_design(&mut s, DesignRequest::Diff { file: None, offset: 0, limit: 2048 });
    assert_eq!(design_reply_value(&s.ana, read, DesignReplyKind::Diff).get("patch"), Some(&json::s("")));
    let write = s.host.send(Request::Design(DesignRequest::WritePatch { file: None }));
    settle_design(&mut s);
    let saved = design_reply_value(&s.host, write, DesignReplyKind::WritePatch);
    assert_eq!(std::fs::read_to_string(saved.get("path").unwrap().as_str().unwrap()).unwrap(), "");
    let owner_patch = saved.get("path").unwrap().as_str().unwrap();
    std::fs::write(owner_patch, "owner review notes").unwrap();
    let export = guest_design(&mut s, DesignRequest::WritePatch { file: None });
    let exported = design_reply_value(&s.ana, export, DesignReplyKind::WritePatch);
    let peer_patch = exported.get("path").unwrap().as_str().unwrap();
    assert_ne!(peer_patch, owner_patch);
    assert_eq!(std::fs::read_to_string(owner_patch).unwrap(), "owner review notes");
    assert_eq!(std::fs::read_to_string(peer_patch).unwrap(), "", "export omits the active ghost");
    s.app.picks.push_back(Err("widget moved".into()));
    guest_design(&mut s, drag_into("/b"));
    assert!(s.ben.session.view.design.ghost.as_ref().unwrap().rect.is_none());
    let before_commit = s.app.sent.len();
    assert_eq!(s.ben.session.view.design.undo, 0);
    assert!(s.records.is_empty());
    let id = guest_design(&mut s, DesignRequest::GhostCommit);
    assert!(has(&s.ana, id, |n| matches!(n, Notice::Design(DesignNotice::Mutated { focus_path, .. })
        if focus_path.as_deref() == Some("/b/label"))));
    assert_eq!(s.app.sent.len(), before_commit);
    assert_eq!(s.app.sent.iter().filter(|(_,_,op)| matches!(op, DesignOp::Design {verb: DesignVerb::Preview,..})).count(), 2);
    assert_eq!(s.ben.session.view.design.undo, 1);
    assert!(s.ben.session.view.design.ghost.is_none());
    assert!(matches!(&s.records[..], [AppEdit { seat: ANA, agent: Some(ANA_AGENT), prop, .. }] if prop=="design:insert"));
    let live = s.app.design_source.as_ref().unwrap().text();
    assert!(!live.contains("label_2") && live.find("b :=").unwrap() < live.find("label := Label").unwrap(), "{live}");
    guest_design(&mut s, DesignRequest::Undo);
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk);

    let move_to = |target: &str| DesignRequest::GhostMove { source: "/a/label".into(),
        target: target.into(), place: DesignPlace::Inside };
    guest_design(&mut s, move_to("/b"));
    let start = s.app.sent.len();
    let moved = guest_design(&mut s, move_to("/a"));
    assert!(has(&s.ana, moved, |n| matches!(n, Notice::Design(DesignNotice::Reply {..}))),
        "{:?}", s.ana.session.view.updates.back());
    assert_eq!(s.app.sent[start..].iter().filter(|(_,_,op)| matches!(op, DesignOp::Design {verb: DesignVerb::Locate,..})).count(), 1);
    guest_design(&mut s, DesignRequest::GhostCancel);
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk);
    assert_eq!(s.ana.session.view.design.redo, 1, "provisional edits preserve redo history");
    let cancel = guest_design(&mut s, DesignRequest::GhostCancel);
    assert_eq!(design_reply_value(&s.ana, cancel, DesignReplyKind::State).get("ghost"), Some(&json::Value::Null),
        "cancelling an already retracted ghost returns the settled state");
    let commit = guest_design(&mut s, DesignRequest::GhostCommit);
    assert!(has(&s.ana, commit, |n| matches!(n, Notice::Refused(e) if e == "no active ghost to settle")));

    let other = fixture.0.join("other.splash");
    let other_disk = "View{ target := View{} }";
    std::fs::write(&other, other_disk).unwrap();
    s.app.other_source = Some(FakeDesignSource { file: other.canonicalize().unwrap().to_str().unwrap().into(),
        disk: other_disk.into(), ..Default::default() });
    guest_design(&mut s, drag_into("/b"));
    guest_design(&mut s, drag_into("/other/target"));
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk, "moving files restored the old preview");
    assert!(s.app.other_source.as_ref().unwrap().text().contains("label := Label"));
    let insert = s.host.send(Request::Design(DesignRequest::Insert { path: "/a".into(), place: DesignPlace::Inside,
        ty: "View".into(), name: Some("saved".into()), props: vec![] }));
    settle_design(&mut s);
    assert!(has(&s.host, insert, |n| matches!(n, Notice::Design(DesignNotice::Mutated { .. }))));
    assert_eq!(s.app.other_source.as_ref().unwrap().text(), other_disk, "committed mutation settled the other seat's drag");
    assert!(s.ben.session.view.design.ghost.is_none());
    let saved = s.app.design_source.as_ref().unwrap().text().to_owned();
    guest_design(&mut s, drag_into("/b"));
    let read = guest_design(&mut s, DesignRequest::Diff { file: None, offset: 0, limit: 32768 });
    let diff = design_reply_value(&s.ana, read, DesignReplyKind::Diff);
    let diff = diff.get("patch").unwrap().as_str().unwrap();
    assert!(diff.contains("saved := View") && !diff.contains("label := Label"), "{diff}");
    s.host.session.peer_unlinked(ANA);
    settle_design(&mut s);
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), saved, "disconnect restored the existing dirty override");
}

#[test]
fn ghost_auto_retracts_on_peer_disconnect_or_stop() {
    let fixture = DesignTestDir::new("ghost-cleanup");
    let disk = "script_mod! { mod.UI = View{ root := View{} } }";
    let (mut s, _cx) = structural_share(&fixture.0, disk);
    for reason in ["Stop", "Revoke", "Disconnect", "Departure", "End"] {
        s.allow(ANA, true);
        guest_design(&mut s, drag_into("/root"));
        assert!(s.ben.session.view.design.ghost.is_some(), "{reason}: ghost never installed");
        match reason {
            "Stop" => s.host.session.stop_agent(ANA, ANA_AGENT, true).unwrap(),
            "Revoke" => s.allow(ANA, false),
            "Disconnect" => s.host.session.peer_unlinked(ANA),
            "Departure" => s.host.session.set_members([OWNER, BEN]),
            _ => s.host.session.end("closed"),
        }
        settle_design(&mut s);
        assert!(s.ben.session.view.design.ghost.is_none(), "{reason} left a lease");
        assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk, "{reason} left provisional source");
        assert!(s.app.design_source.as_ref().unwrap().live.is_none(), "{reason} left a new override");
        assert_eq!(s.ben.session.view.design.undo, 0);
        if reason == "Stop" { s.host.session.stop_agent(ANA, ANA_AGENT, false).unwrap(); }
        if reason == "Departure" {
            s.host.session.set_members([OWNER, ANA, BEN]);
            s.host.session.set_peer_level(ANA, Some(DESIGN_LEVEL));
        }
    }
    assert!(s.records.is_empty());
    assert!(s.host.session.view.design.ghost.is_none(), "owner view still shows the ended drag");

    let (mut disconnected, _cx2) = structural_share(&fixture.0, disk);
    disconnected.allow(ANA, true);
    guest_design(&mut disconnected, drag_into("/root"));
    let before_end = disconnected.app.sent.len();
    disconnected.host.session.link_ended(LINK, "app closed");
    settle_design(&mut disconnected);
    assert!(disconnected.ben.session.view.design.ghost.is_none());
    assert_eq!(disconnected.app.sent.len(), before_end, "closed link received a restore command");
}

#[test]
fn bake_props_and_theme_writes_valid_macro_safe_ast_patch() {
    use makepad_widgets::makepad_platform::script_mod_doc::{DocKind, ScriptModDoc};
    let fixture = DesignTestDir::new("bake");
    let disk = "script_mod! { mod.UI = View{ panel := View{} } }\n";
    let (mut s, _cx) = structural_share(&fixture.0, disk);
    s.allow(ANA, true);
    s.app.theme = THEME.replace("color_primary", "color_accent");
    s.app.props = ROWS.replace("padding", "draw_bg.color");
    s.list();
    s.host.send(Request::Set { path: PATH.into(), prop: "draw_bg.color".into(), value: "#1a2b3cff".into() });
    s.one();
    s.host.send(Request::Theme(ThemeOp::Install(ThemeInstall { base: "dark".into(), sheet: None, label: "Custom".into(),
        globals: vec![("space_2".into(), ThemeVal::Num(12.))], overrides: vec![("color_accent".into(), ThemeVal::Color(0x112233ff))] })));
    s.one();
    s.one();
    s.host.send(Request::Theme(ThemeOp::Set { name: "color_accent".into(), value: "#336699ff".into() }));
    s.one();
    s.app.props = ROWS.into();
    s.list();
    s.host.send(set("unrepresentable value"));
    s.one();
    let bake = guest_design(&mut s, DesignRequest::Bake { scope: BakeScope::All, path: Some(PATH.into()), prop: None });
    assert!(has(&s.ana, bake, |n| matches!(n, Notice::Design(DesignNotice::Mutated {op,..}) if op=="bake")),
        "{:?}", s.ana.session.view.updates.back());
    let report = design_reply_value(&s.ana, bake, DesignReplyKind::State);
    assert_eq!(report.get("baked"), Some(&json::Value::Bool(true)));
    assert_eq!(report.get("skipped"), Some(&json::Value::Arr(vec![json::Value::Arr(vec![json::s(PATH), json::s("padding")])])));
    let read = guest_design(&mut s, DesignRequest::Diff { file: Some("ui.rs".into()), offset: 0, limit: 32768 });
    let page = design_reply_value(&s.ana, read, DesignReplyKind::Diff);
    let diff = page.get("patch").unwrap().as_str().unwrap();
    assert!(diff.starts_with("--- a/ui.rs\n+++ b/ui.rs") && diff.contains("#x1a2b3cff") && diff.contains("#x336699FF")
        && diff.contains("mod.theme +:") && diff.contains("space_2") && !diff.contains("112233FF"), "{diff}");
    let live = s.app.design_source.as_ref().unwrap().text();
    ScriptModDoc::parse("ui.rs", live.into(), DocKind::RustScriptMod).unwrap().block_syntax(0).unwrap();
    assert_eq!(std::fs::read_to_string(fixture.0.join("ui.rs")).unwrap(), disk);
    let start = s.app.sent.len();
    let retry = guest_design(&mut s, DesignRequest::Bake { scope: BakeScope::Props, path: None, prop: None });
    let retry_report = design_reply_value(&s.ana, retry, DesignReplyKind::State);
    assert_eq!(retry_report.get("baked"), Some(&json::Value::Bool(false)));
    assert_eq!(retry_report.get("skipped"), report.get("skipped"), "skipped values remain unbaked");
    assert_eq!(s.app.sent.len(), start, "already-baked properties should not be located or previewed");
}

#[test]
fn build_tab_hides_commit_for_guest_and_disables_edits_until_allowed() {
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 220} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut s = Copies::new(OWNER, ANA, ITEM);
    let mut panel = panel::Panel::default();
    let state = Default::default();
    let refresh = |cx: &mut Cx, panel: &mut panel::Panel, town: &TownApp| {
        panel.refresh(cx, &root, &town.session, &state, &Default::default());
    };
    let bake = root.button(cx, ids!(design_bake));
    let commit = root.widget(cx, ids!(design_commit));
    s.ana.session.view.design.dirty_files = 1;
    refresh(cx, &mut panel, &s.ana);
    let mut draw = draw_panel(cx, root.clone());
    draw(cx);
    let button = if root.widget(cx, ids!(tab_build)).visible() { ids!(tab_build) } else { ids!(tab_build_i) };
    let (_, _, click) = panel_click_at(cx, &root, root.widget(cx, button).area().rect(cx).center(), 1.);
    panel.actions(cx, &root, &click, &mut s.ana.session);
    draw(cx);
    assert!(root.widget(cx, ids!(tweak_build_wrap)).visible());
    assert_eq!(root.label(cx, ids!(build_status)).text(), OLDER_SHARER_DESIGN);
    s.ana.session.set_peer_level(OWNER, Some(makepad_town_core::share::hello::CONTROL_TWEAK_LEVEL));
    refresh(cx, &mut panel, &s.ana);
    assert!(root.widget(cx, ids!(tweak_ask)).visible() && !bake.borrow().unwrap().enabled());
    assert!(!commit.visible(), "a guest cannot commit dirty files");
    s.allow(ANA, true);
    refresh(cx, &mut panel, &s.ana);
    assert!(bake.borrow().unwrap().enabled() && !commit.visible());
    s.ana.session.set_peer_level(OWNER, Some(THEME_LEVEL));
    refresh(cx, &mut panel, &s.ana);
    assert!(!bake.borrow().unwrap().enabled(), "the older sharer cannot perform a structural edit");
    refresh(cx, &mut panel, &s.host);
    assert!(!commit.visible(), "a clean owner session has nothing to commit");
    s.host.session.view.design.dirty_files = 1;
    refresh(cx, &mut panel, &s.host);
    assert!(commit.visible());
}

#[test]
fn build_status_distinguishes_selected_type_from_insert_type() {
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = script_eval!(vm, { mod.widgets.AppTweakPanel{width: 360} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut panel = test_panel();
    let mut s = Copies::new(OWNER, ANA, ITEM);
    let session = &mut s.ana.session;
    session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    let state = Default::default();
    let path = "/increment_button";
    let picked = |ty: &str| Notice::Picked { path: path.into(), ty: ty.into(),
        rect: super::Rect { window: 0, x: 0., y: 0., w: 40., h: 20. } };
    panel.notice(cx, &root, session, ANA, 1, None, &state, &picked("Button"));
    let tab = root.button(cx, ids!(tab_build));
    let actions = cx.capture_actions(|cx| cx.widget_action(tab.widget_uid(), ButtonAction::Clicked(Default::default())));
    panel.actions(cx, &root, &actions, session);
    let status = root.label(cx, ids!(build_status));
    assert_eq!(status.text(), "/increment_button (Button) · Insert: View · 0 dirty files");

    let palette = parse_json(r#"{"palette":[{"name":"Label","group":"Text"}]}"#).unwrap();
    root.widget(cx, ids!(build_palette)).borrow_mut::<super::design::view::PaletteList>().unwrap().palette.page(&palette, true).unwrap();
    draw_panel(cx, root.clone())(cx);
    let entry = root.portal_list(cx, ids!(palette_rows)).get_item(1).expect("mounted Label entry").1;
    assert_eq!(entry.text(), "Label");
    let (_, _, click) = panel_click_at(cx, &root, entry.area().rect(cx).center(), 1.);
    panel.actions(cx, &root, &click, session);
    assert_eq!(status.text(), "/increment_button (Button) · Insert: Label · 0 dirty files");

    session.view.design.dirty_files = 1;
    panel.refresh(cx, &root, session, &state, &Default::default());
    assert_eq!(status.text(), "/increment_button (Button) · Insert: Label · 1 dirty file");
    panel.notice(cx, &root, session, ANA, 2, None, &state, &picked(""));
    panel.refresh(cx, &root, session, &state, &Default::default());
    assert_eq!(status.text(), "/increment_button · Insert: Label · 1 dirty file");
    panel.notice(cx, &root, session, ANA, 3, None, &state, &Notice::Selected { path: None, rect: None });
    panel.refresh(cx, &root, session, &state, &Default::default());
    assert_eq!(status.text(), "Select a widget · Insert: Label · 1 dirty file");
}

#[test]
fn props_box_row_and_tree_filter_fit_without_clipping_at_narrow_widths() {
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = script_eval!(vm, { mod.widgets.AppTweakPanel{} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut panel = test_panel();
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.ana.session.view.updates.clear();
    s.app.props = r#"{"props":[{"name":"margin","value":"4"},{"name":"padding","value":"8"}]}"#.into();
    let state = Default::default();
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    panel.notice(cx, &root, &mut s.ana.session, ANA, 1, None, &state, &Notice::Picked { path: PATH.into(),
        ty: "View".into(), rect: super::Rect { window: 0, x: 0., y: 0., w: 40., h: 20. } });
    s.one();
    while let Some((seat, req, agent, notice)) = s.ana.session.view.updates.pop_front() {
        panel.notice(cx, &root, &mut s.ana.session, seat, req, agent, &state, &notice);
    }
    let mut draw = draw_panel(cx, root.clone());
    let mut draw_at = |cx: &mut Cx, width| {
        let walk = root.walk(cx);
        root.as_view().set_walk(cx, Walk { width: Size::Fixed(width), ..walk });
        draw(cx);
        let bounds = root.area().rect(cx);
        assert_eq!(bounds.size.x, width, "panel width under test");
        bounds
    };
    let list = root.portal_list(cx, ids!(tweak_properties_list));
    for width in [220., 240., 360.] {
        let bounds = draw_at(cx, width);
        for prop in ["margin", "padding"] {
            let row = (0..list.borrow().unwrap().range_end()).filter_map(|i| list.get_item(i))
                .find(|(id, row)| *id == id!(BoxRow) && row.label(cx, ids!(name)).text() == prop)
                .expect("mounted inset property").1;
            for id in [id!(leg_top), id!(leg_left), id!(frame), id!(leg_right), id!(leg_bottom)] {
                let rect = row.widget(cx, &[id]).area().rect(cx);
                assert!(rect.size.x > 0. && rect.pos.x >= bounds.pos.x &&
                    rect.pos.x + rect.size.x <= bounds.pos.x + bounds.size.x, "{prop} {id} at {width}: {rect:?}");
                if width == 360. {
                    if id == id!(leg_left) || id == id!(leg_right) { assert_eq!(rect.size.x, 64., "{prop} {id}"); }
                    if id == id!(frame) { assert!(rect.size.x > 64., "{prop} frame receives the remaining space"); }
                }
            }
        }
    }
    let tab = root.button(cx, ids!(tab_tree));
    let actions = cx.capture_actions(|cx| cx.widget_action(tab.widget_uid(), ButtonAction::Clicked(Default::default())));
    panel.actions(cx, &root, &actions, &mut s.ana.session);
    for width in [240., 360.] {
        let bounds = draw_at(cx, width);
        let filter = root.widget(cx, ids!(tree_filter)).area().rect(cx);
        let refresh = root.widget(cx, ids!(tree_refresh)).area().rect(cx);
        assert!(filter.size.x > 0. && refresh.size.x > 0. && (filter.pos.y - refresh.pos.y).abs() < 6.,
            "tree controls share a line at {width}: {filter:?}, {refresh:?}");
        assert!(refresh.pos.x + refresh.size.x <= bounds.pos.x + bounds.size.x, "refresh fits at {width}");
    }
}

#[test]
fn palette_filter_narrows_rows_and_preserves_category_headers() {
    use design::view::{Palette, PaletteRow::{Entry, Group}};
    let mut palette = Palette::default();
    palette.page(&parse_json(r#"{"palette":[{"name":"View","group":"Layout"},{"name":"Button","group":"Controls"},{"name":"IconButton","group":"Controls"},{"name":"Label","group":"Text"}]}"#).unwrap(), true).unwrap();
    palette.filter("BtN".into());
    assert_eq!(palette.rows, [Group("Controls".into()), Entry("Button".into()), Entry("IconButton".into())]);
    palette.filter("text".into());
    assert_eq!(palette.rows, [Group("Text".into()), Entry("Label".into())]);
    palette.filter("unknown".into());
    assert!(palette.rows.is_empty());
}

#[test]
fn identity_row_rename_validates_identifier_and_emits_design_rename() {
    use makepad_widgets::*;
    let fixture = DesignTestDir::new("panel-rename");
    let disk = "fn selected() { id!(panel); }\nscript_mod! { mod.UI = View{ panel := View{} } }";
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let mut s = structural_copies(&fixture.0, disk, &cx.task_pool());
    s.allow(ANA, true);
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 360} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut panel = panel::Panel::default();
    let state = Default::default();
    let dispatch = |cx: &mut Cx, panel: &mut panel::Panel, town: &mut TownApp| {
        while let Some((seat, req, agent, notice)) = town.session.view.updates.pop_front() {
            panel.notice(cx, &root, &mut town.session, seat, req, agent, &state, &notice);
        }
    };
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    dispatch(cx, &mut panel, &mut s.ana);
    s.one(); // Service the theme read issued when this guest received Allow.
    s.ana.send(Request::Pick(At::Path(PATH.into())));
    s.one();
    dispatch(cx, &mut panel, &mut s.ana);
    settle_design(&mut s); // Locate the picked literal.
    s.one(); // Supply its reflected property rows.
    dispatch(cx, &mut panel, &mut s.ana);
    let mut draw = draw_panel(cx, root.clone());
    draw(cx);
    let list = root.portal_list(cx, ids!(tweak_properties_list));
    let row = (0..list.borrow().unwrap().range_end()).filter_map(|i| list.get_item(i))
        .find(|(id, _)| *id == id!(IdentityRow)).unwrap().1;
    let name = row.text_input(cx, ids!(name_field));
    assert!(!name.is_read_only(), "located guest selection should be editable: active={}, calls={:?}", s.ana.session.view.design.active, s.app.sent);
    let _ = panel_click_at(cx, &root, name.area().rect(cx).center(), 1.);
    let submit = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies, text: &str| {
        name.set_text(cx, text);
        let enter = cx.capture_actions(|cx| root.handle_event(cx,
            &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }), &mut Scope::empty()));
        panel.actions(cx, &root, &enter, &mut s.ana.session);
    };
    let before = s.app.sent.len();
    submit(cx, &mut panel, &mut s, "123 bad");
    draw(cx);
    assert!(row.label(cx, ids!(refusal)).text().contains("identifier"));
    s.one();
    assert_eq!(s.app.sent.len(), before, "an invalid identifier sends nothing");
    let _ = panel_click_at(cx, &root, name.area().rect(cx).center(), 2.);
    submit(cx, &mut panel, &mut s, "header_bar");
    settle_design(&mut s);
    dispatch(cx, &mut panel, &mut s.ana);
    draw(cx);
    assert!(row.label(cx, ids!(refusal)).text().contains("Referenced in Rust at lines 1"));
    assert!(row.widget(cx, ids!(rename_confirm)).visible());
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk, "refusal preserves the source");
    let (_, _, click) = panel_click_at(cx, &root, row.button(cx, ids!(rename_anyway)).area().rect(cx).center(), 3.);
    panel.actions(cx, &root, &click, &mut s.ana.session);
    settle_design(&mut s);
    assert!(s.app.design_source.as_ref().unwrap().text().contains("header_bar :="), "confirmed Rename edits the widget");
}

#[test]
fn theme_tab_routes_bake_to_app_or_d1_design() {
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let ui = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* View{tweak_panel := AppTweakPanel{width: 360}} });
        WidgetRef::script_from_value(vm, value)
    });
    let root = ui.widget(cx, ids!(tweak_panel));
    let mut panel = test_panel();
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.allow(ANA, true);
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    let homes = crate::model::tests::ShareHomes::new("panel-board-bake");
    let mut model = Model::open(&homes.0).unwrap().0;
    panel.refresh(cx, &root, &s.ana.session, &model.state, &Default::default());
    let click = |cx: &mut Cx, path| {
        let uid = root.button(cx, path).widget_uid();
        cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(Default::default())))
    };
    for path in [ids!(tab_theme), ids!(theme_bake)] {
        let action = click(cx, path);
        panel.actions(cx, &root, &action, &mut s.ana.session);
    }
    s.ana.poll();
    assert!(s.host.relay.net.borrow().inbox[&OWNER].iter().any(|(_, _, body)| matches!(
        wire::decode(ANA, body), Ok(Some(Packet { body: Body::Request(Request::Design(DesignRequest::Bake { scope: BakeScope::Theme, path: None, prop: None })), .. }))
    )), "the shared app receives a theme-only bake");

    let board = model.act(crate::model::Action::AddBoard { project: model.project().id, title: "Board".into() }, Origin::Ui).unwrap();
    let item = model.act(crate::model::Action::AddDesign { board, x: 0, y: 0, preset: "phone".into(),
        title: "Card".into(), src: "View { title := Label{} }".into() }, Origin::Ui).unwrap();
    let callback = cx.tweak_callback;
    cx.tweak_callback = Some(|_, op, _| match op {
        "design_theme" => Ok(THEME.into()), "design_palette" => Ok(r#"{"palette":[]}"#.into()), _ => unreachable!(),
    });
    let mut hub = Tweaks::<u64,u64>::default();
    hub.set_themes_dir(panel.themes_dir());
    hub.select_board_design(Some(item));
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(root.visible() && root.widget(cx, ids!(tweak_build_wrap)).visible(), "the board opens Build without a live share");
    cx.tweak_callback = callback;
    let insert = root.button(cx, ids!(insert_inside));
    board_frames_until(cx, &mut hub, &ui, &model, || insert.borrow().unwrap().enabled());
    let action = click(cx, ids!(tab_theme));
    hub.board_design_actions(cx, &ui, &action, &mut model);
    let action = click(cx, ids!(theme_bake));
    let id = hub.board_design_actions(cx, &ui, &action, &mut model).expect("signed design write");
    let snapshot = design::d1::read_design(&model.state, item).unwrap();
    assert_eq!(snapshot.revision, id);
    assert!(snapshot.src.contains("mod.theme +: {") && snapshot.src.contains("color_primary: #e6a294ff"), "baked source: {}", snapshot.src);
    assert!(snapshot.src.contains("title := Label{}"), "baking preserves widget source");
    board_frames_until(cx, &mut hub, &ui, &model, || insert.borrow().unwrap().enabled());
    assert!(hub.board_design_actions(cx, &ui, &action, &mut model).is_none(), "rebaking the same theme emits no write");
    let action = click(cx, ids!(tab_build));
    hub.board_design_actions(cx, &ui, &action, &mut model);
    let action = click(cx, ids!(insert_inside));
    let write = hub.board_design_actions(cx, &ui, &action, &mut model).expect("structural insertion on the board");
    let snapshot = design::d1::read_design(&model.state, item).unwrap();
    assert_eq!(snapshot.revision, write);
    assert_eq!(design::d1::list_widgets(&snapshot.src).unwrap().len(), 3);
    hub.select_board_design(None);
    hub.update(cx, &ui, &model, &Default::default(), &mut Default::default(), |_| {});
    assert!(!root.visible(), "deselecting the design closes its panel");
}

// Town delivers the frame to tick_theme, then paints model changes through update.
fn board_frames_until(cx: &mut Cx, hub: &mut Tweaks<u64, u64>, ui: &WidgetRef, model: &Model,
    mut installed: impl FnMut() -> bool) -> std::time::Duration {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let frame = cx.next_frame_id();
        let event = Event::NextFrame(makepad_widgets::NextFrameEvent { frame, time: frame as f64 / 60., ..Default::default() });
        let start = std::time::Instant::now();
        hub.tick_theme(cx, ui, &event);
        hub.update(cx, ui, model, &Default::default(), &mut Default::default(), |_| {});
        let elapsed = start.elapsed();
        if installed() { return elapsed }
        assert!(cx.next_frame_id() > frame, "a pending widget index must request the next frame");
        assert!(start < deadline, "the widget index did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn selecting_a_design_lists_its_widgets_off_the_ui_thread() {
    use crate::model::Action;
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let ui = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* View{tweak_panel := AppTweakPanel{width: 360}} });
        WidgetRef::script_from_value(vm, value)
    });
    let root = ui.widget(cx, ids!(tweak_panel));
    let picker = root.drop_down(cx, ids!(board_widget));
    let edit = root.button(cx, ids!(insert_inside));
    let result = root.label(cx, ids!(tweak_result));
    let homes = crate::model::tests::ShareHomes::new("board-widget-index");
    let mut model = Model::open(&homes.0).unwrap().0;
    let board = model.act(Action::AddBoard { project: model.project().id, title: "Board".into() }, Origin::Ui).unwrap();
    let src = "View { title := Label{} }";
    let item = model.act(Action::AddDesign { board, x: 0, y: 0, preset: "phone".into(),
        title: "Card".into(), src: src.into() }, Origin::Ui).unwrap();
    let write = |model: &mut Model, revision, src: String| model.act(Action::WriteDesign { item, revision, src }, Origin::Ui).unwrap();
    let paths = |src| design::d1::list_widgets(src).unwrap().into_iter().map(|node| node.path).collect::<Vec<_>>();
    let mut hub = Tweaks::<u64, u64>::default();
    hub.set_themes_dir(homes.0.join("themes"));
    let update = |cx: &mut Cx, hub: &mut Tweaks<u64, u64>, model: &Model| {
        hub.update(cx, &ui, model, &Default::default(), &mut Default::default(), |_| {});
    };
    let before = panel::BOARD_INDEX_RUNS.with(|count| count.get());
    hub.select_board_design(Some(item));
    update(cx, &mut hub, &model);
    assert_eq!(panel::BOARD_INDEX_RUNS.with(|count| count.get()), before, "selection parsed on the UI thread");
    assert!(picker.has_labels(&[]) && !edit.borrow().unwrap().enabled(), "the initial picker is empty and editing is disabled");
    let initial = paths(src);
    board_frames_until(cx, &mut hub, &ui, &model, || edit.borrow().unwrap().enabled());
    assert!(picker.has_labels(&initial), "the installed paths match the source index");

    let stale = write(&mut model, item, "View { obsolete := Button{} }".into());
    let frame = cx.next_frame_id();
    update(cx, &mut hub, &model);
    assert!(cx.next_frame_id() > frame, "submitting an index requests a frame without a later model event");
    assert!(picker.has_labels(&initial) && !edit.borrow().unwrap().enabled(), "previous paths remain visible during indexing");
    let current = "View { current := Button{} }";
    let revision = write(&mut model, stale, current.into());
    board_frames_until(cx, &mut hub, &ui, &model, || {
        let enabled = edit.borrow().unwrap().enabled();
        if !enabled { assert!(picker.has_labels(&initial), "a stale writer replaced the displayed paths"); }
        enabled
    });
    assert!(picker.has_labels(&paths(current)), "only the current writer can enable editing");

    let bad = "View {";
    let error = design::d1::list_widgets(bad).unwrap_err().to_string();
    let failed = write(&mut model, revision, bad.into());
    update(cx, &mut hub, &model);
    board_frames_until(cx, &mut hub, &ui, &model, || result.text() == error);
    assert!(picker.has_labels(&[]) && !edit.borrow().unwrap().enabled(), "invalid source clears the picker");
    hub.select_board_design(None);
    update(cx, &mut hub, &model);
    let frame = cx.next_frame_id();
    hub.select_board_design(Some(item));
    update(cx, &mut hub, &model);
    assert_eq!(cx.next_frame_id(), frame, "reopening an unchanged parse failure must not schedule another index");
    assert_eq!(result.text(), error);
    hub.tick_theme(cx, &ui, &Event::NextFrame(NextFrameEvent::default()));
    assert_eq!(cx.next_frame_id(), frame, "an idle board index stops requesting frames");

    let block = format!("Label {{ text: \"{}\" }}\n", "x".repeat(1008));
    let copies = 1_000_000_usize.div_ceil(block.len());
    let large = format!("View {{\n{} }}", block.repeat(copies));
    let bytes = large.len();
    write(&mut model, failed, large);
    let start = std::time::Instant::now();
    update(cx, &mut hub, &model);
    let submit = start.elapsed();
    let install = board_frames_until(cx, &mut hub, &ui, &model, || edit.borrow().unwrap().enabled());
    eprintln!("Board widget index ({bytes} bytes, {copies} labels) UI submit {submit:?}, install {install:?}");
    assert!(result.text().is_empty(), "a valid new revision clears the parse error");
    assert_eq!(panel::BOARD_INDEX_RUNS.with(|count| count.get()), before, "no board index ran on the UI thread");
}

impl ThemeAgent {
    fn design_call(&self, item: [u8; 32], op: &str, fields: &str) -> makepad_ai_services::wire::ServiceCall {
        let json::Value::Obj(mut fields) = parse_json(fields).unwrap() else { panic!("test call needs arguments") };
        fields.extend([("item".into(), json::s(hex(&item))), ("binding".into(), json::s(&self.binding))]);
        makepad_ai_services::wire::ServiceCall { tool: format!("app_design_{op}"), call_id: op.into(),
            args: json::Value::Obj(fields).to_json(), caller: None, origin: makepad_ai_services::wire::CallOrigin::Conversation }
    }

    fn start_design(&mut self, seat: &mut TownApp, op: &str, fields: &str) -> Option<ThemeResult> {
        let call = self.design_call(seat.session.item, op, fields);
        let ctx = crate::tools::ThemeToolContext { themes_dir: &self.dir, mixer_cache: &mut self.cache, active_worn: None };
        let (reply, _, effect) = self.tools.start(op, app_tool(&call.tool, &call.args).unwrap(), &mut self.model,
            std::slice::from_mut(&mut seat.session), &mut self.waits, ctx);
        assert!(effect.is_none(), "{op} never sends a panel effect");
        reply
    }

    fn run_design(&mut self, s: &mut Copies, op: &str, fields: &str) -> ThemeResult {
        if let Some(reply) = self.start_design(&mut s.ana, op, fields) { return reply }
        settle_design(s);
        self.notices(&mut s.ana, |_, _, _, _, _, effect| assert!(effect.is_none()))
            .unwrap_or_else(|| panic!("missing final reply for {op}"))
    }
}

#[test]
fn ai_design_tools_roundtrip_all_structural_ops() {
    let (homes, _owner, mut ai, mut s) = theme_agents("design-tools-roundtrip");
    let disk = "fn uses_card() { id!(card); }\nscript_mod! { mod.UI = View{ panel := View{ a := View{} b := View{} } } }\n";
    let _cx = configure_design_share(&mut s, &homes.0, disk);
    let state = theme_ok(ai.run_design(&mut s, "state", r#"{"path":"/panel"}"#));
    assert_eq!(state.get("dirty_files"), Some(&json::Value::Int(0)));
    assert_eq!(state.get("located").unwrap().get("rust_refs"), Some(&json::Value::Arr(vec![])));
    let palette = theme_ok(ai.run_design(&mut s, "palette", r#"{"query":"layout","offset":1,"limit":1}"#));
    assert_eq!(palette.get("items").unwrap().as_arr().unwrap()[0].get("name"), Some(&json::s("Grid")));
    theme_ok(ai.run_design(&mut s, "patch", r#"{"op":"verify","path":"/panel"}"#));
    let empty = theme_ok(ai.run_design(&mut s, "patch", r#"{"op":"diff"}"#));
    assert_eq!(empty.get("patch"), Some(&json::s("")));
    let denied = ai.run_design(&mut s, "patch", r#"{"op":"write_patch"}"#);
    assert_eq!(denied.outcome, makepad_ai_services::wire::ToolOutcome::Refused);
    s.allow(ai.model.me(), true);
    let insert = theme_ok(ai.run_design(&mut s, "insert", r#"{"path":"/panel","place":"inside","ty":"View","name":"card","props":{"width":"12"}}"#));
    assert_eq!(insert.get("focus_path"), Some(&json::s("/panel/card")));
    assert_eq!(insert.get("name"), Some(&json::s("card")));
    for (op, fields, expected) in [
        ("move", r#"{"path":"/panel/card","target":"/panel/b","place":"before"}"#, "move"),
        ("move", r#"{"path":"/panel/card","direction":"up"}"#, "up"),
        ("move", r#"{"path":"/panel/card","direction":"down"}"#, "down"),
        ("move", r#"{"path":"/panel/card","direction":"out"}"#, "out"),
        ("duplicate", r#"{"path":"/card","name":"copy"}"#, "duplicate"),
        ("wrap", r#"{"path":"/copy","ty":"View","name":"frame"}"#, "wrap"),
    ] {
        let result = theme_ok(ai.run_design(&mut s, op, fields));
        assert_eq!(result.get("op"), Some(&json::s(expected)));
        assert!(result.get("seq").is_some(), "{op} identifies the signed edit");
    }
    for (op, args) in [("rename", r#"{"path":"/card","name":"tile"}"#), ("delete", r#"{"path":"/card"}"#)] {
        let refusal = ai.run_design(&mut s, op, args);
        assert_eq!(refusal.outcome, makepad_ai_services::wire::ToolOutcome::Refused);
        assert!(!parse_json(&refusal.text).unwrap().get("rust_refs").unwrap().as_arr().unwrap().is_empty());
    }
    let rename = theme_ok(ai.run_design(&mut s, "rename", r#"{"path":"/card","name":"tile","force_rust_refs":true}"#));
    assert_eq!(rename.get("old_path"), Some(&json::s("/card")));
    assert_eq!(rename.get("name"), Some(&json::s("tile")));
    let delete = theme_ok(ai.run_design(&mut s, "delete", r#"{"path":"/tile"}"#));
    assert_eq!(delete.get("path"), Some(&json::s("/tile")));
    s.list();
    s.host.send(set("12"));
    s.one();
    let baked = theme_ok(ai.run_design(&mut s, "bake", r#"{"scope":"props","path":"ui.panel","prop":"padding"}"#));
    assert_eq!(baked.get("baked"), Some(&json::Value::Bool(true)));
    let noop = theme_ok(ai.run_design(&mut s, "bake", r#"{"scope":"props"}"#));
    assert_eq!(noop.get("baked"), Some(&json::Value::Bool(false)));
    s.host.send(Request::Theme(ThemeOp::Set { name: "space_2".into(), value: "16".into() }));
    s.one();
    let theme = theme_ok(ai.run_design(&mut s, "bake", r#"{"scope":"theme","path":"ui.panel"}"#));
    assert_eq!(theme.get("baked"), Some(&json::Value::Bool(true)));
    let diff = theme_ok(ai.run_design(&mut s, "patch", r#"{"op":"diff","file":"ui.rs"}"#));
    let patch = diff.get("patch").unwrap().as_str().unwrap();
    assert!(patch.contains("frame :=") && patch.contains("space_2"), "{patch}");
    let export = theme_ok(ai.run_design(&mut s, "patch", r#"{"op":"write_patch","file":"ui.rs"}"#));
    let path = std::path::Path::new(export.get("path").unwrap().as_str().unwrap());
    assert!(path.starts_with(homes.0.canonicalize().unwrap().join("local/design")));
    assert_eq!(std::fs::read_to_string(path).unwrap(), patch);
    for (op, redo) in [("undo", 1), ("redo", 0), ("reset", 0)] {
        let result = theme_ok(ai.run_design(&mut s, "history", &json::obj(vec![("op", json::s(op))]).to_json()));
        assert_eq!(result.get("op"), Some(&json::s(op)));
        assert_eq!(result.get("redo"), Some(&json::Value::Int(redo)));
        assert_eq!(result.get("dirty_files"), Some(&json::Value::Int(if op == "reset" { 0 } else { 1 })));
    }
    assert_eq!(s.app.design_source.as_ref().unwrap().text(), disk);
    assert_eq!(std::fs::read_to_string(homes.0.join("ui.rs")).unwrap(), disk);
}

#[test]
fn ai_design_rejects_non_literal_class_props_and_invalid_args() {
    let (_homes, _owner, mut ai, mut s) = theme_agents("design-args");
    s.ana.session.set_peer_level(s.host.session.me, Some(DESIGN_LEVEL));
    for (op, args) in [
        ("insert", r#"{"path":"/root","place":"inside","ty":"View","allow":true}"#),
        ("insert", r#"{"path":"/root","place":"inside","ty":"View","name":"bad-name"}"#),
        ("insert", r#"{"path":"/root","place":"inside","ty":"View","name":"fn"}"#),
        ("insert", r#"{"path":"/root","place":"inside","ty":"scope.View"}"#),
        ("insert", r#"{"path":"/root","place":"inside","ty":"View","props":{"draw":"fn(){}"}}"#),
        ("insert", r#"{"path":"/root","place":"inside","ty":"View","props":{"src":"crate_resource(1)"}}"#),
        ("insert", r##"{"path":"/root","place":"inside","ty":"View","props":{"value":"#(1)"}}"##),
        ("insert", r#"{"path":"/root","place":"inside","ty":"View","props":{"value":"@(1)"}}"#),
        ("move", r#"{"path":"/root","target":"/x","place":"before","direction":"up"}"#),
        ("move", r#"{"path":"/root"}"#), ("move", r#"{"path":"/root","target":"/x"}"#),
        ("move", r#"{"path":"/root","place":"before"}"#),
        ("patch", r#"{"op":"diff","path":"/x"}"#), ("patch", r#"{"op":"write_patch","offset":0}"#),
        ("patch", r#"{"op":"verify","file":"ui.rs"}"#),
    ] {
        let result = ai.start_design(&mut s.ana, op, args).expect("invalid arguments must settle before dispatch");
        assert_eq!(result.outcome, makepad_ai_services::wire::ToolOutcome::Refused, "{op}: {args}");
        assert!(!result.text.is_empty(), "{op} gives a reason");
    }
    s.one();
    assert!(s.records.is_empty() && s.app.sent.is_empty(), "argument errors never reach the shared app");
}

#[test]
fn ai_design_patch_and_palette_paginate_within_max_result_bytes() {
    let (homes, _owner, mut ai, mut s) = theme_agents("design-pages");
    let disk = format!("script_mod! {{ mod.UI = View{{ panel := Label{{text: \"{}\"}} }} }}\n", "雪\\n".repeat(10_300));
    let _cx = configure_design_share(&mut s, &homes.0, &disk);
    let items: Vec<_> = (0..500).map(|i| json::obj(vec![("name", json::s(format!("Widget{i}"))),
        ("group", json::s("Layout")), ("body", json::s("{width: Fill}"))])).collect();
    s.app.design_source.as_mut().unwrap().palette = Some(json::obj(vec![("palette", json::Value::Arr(items.clone()))]).to_json());
    let mut collected = Vec::new();
    loop {
        let args = format!(r#"{{"offset":{},"limit":500}}"#, collected.len());
        let page = theme_ok(ai.run_design(&mut s, "palette", &args));
        assert_eq!(page.get("total"), Some(&json::Value::Int(500)));
        assert_eq!(page.get("offset"), Some(&json::Value::Int(collected.len() as i64)));
        let rows = page.get("items").unwrap().as_arr().unwrap();
        assert!(!rows.is_empty());
        collected.extend_from_slice(rows);
        if page.get("next_offset") == Some(&json::Value::Null) { break }
        assert_eq!(page.get("next_offset"), Some(&json::Value::Int(collected.len() as i64)));
    }
    assert_eq!(collected, items);
    let empty = theme_ok(ai.run_design(&mut s, "palette", r#"{"offset":501}"#));
    assert!(empty.get("items").unwrap().as_arr().unwrap().is_empty());
    assert_eq!(empty.get("next_offset"), Some(&json::Value::Null));

    s.allow(ai.model.me(), true);
    theme_ok(ai.run_design(&mut s, "rename", r#"{"path":"/panel","name":"message"}"#));
    let exported = theme_ok(ai.run_design(&mut s, "patch", r#"{"op":"write_patch"}"#));
    let full = std::fs::read_to_string(exported.get("path").unwrap().as_str().unwrap()).unwrap();
    assert!(full.len() >= 100 * 1024);
    let inside = full.find('雪').unwrap() + 1;
    let args = format!(r#"{{"op":"diff","offset":{inside},"limit":1}}"#);
    let part = theme_ok(ai.run_design(&mut s, "patch", &args));
    let start = full.ceil_char_boundary(inside);
    assert_eq!(part.get("offset"), Some(&json::Value::Int(start as i64)));
    assert_eq!(part.get("patch"), Some(&json::s(&full[start..start + 1])));
    assert_eq!(part.get("next_offset"), Some(&json::Value::Int(start as i64 + 1)));

    let mut collected = String::new();
    loop {
        let args = format!(r#"{{"op":"diff","offset":{},"limit":32768}}"#, collected.len());
        let page = theme_ok(ai.run_design(&mut s, "patch", &args));
        assert_eq!(page.get("offset"), Some(&json::Value::Int(collected.len() as i64)));
        assert_eq!(page.get("total_bytes"), Some(&json::Value::Int(full.len() as i64)));
        let part = page.get("patch").unwrap().as_str().unwrap();
        assert!(!part.is_empty());
        collected.push_str(part);
        if page.get("next_offset") == Some(&json::Value::Null) { break }
        assert_eq!(page.get("next_offset"), Some(&json::Value::Int(collected.len() as i64)));
    }
    assert_eq!(collected, full);
}

#[test]
fn ai_design_partial_bake_and_failed_ghost_promotion_publish_the_settled_state() {
    let (homes, _owner, mut ai, mut s) = theme_agents("design-settlement");
    let _cx = configure_design_share(&mut s, &homes.0, "script_mod! { mod.UI = View{ panel := View{} } }");
    s.allow(ai.model.me(), true);
    s.list();
    s.host.send(set("unrepresentable value"));
    s.one();
    s.app.props = ROWS.replace("padding", "width");
    s.list();
    s.host.send(Request::Set { path: PATH.into(), prop: "width".into(), value: "24".into() });
    s.one();
    let result = theme_ok(ai.run_design(&mut s, "bake", r#"{"scope":"props","path":"ui.panel"}"#));
    let skipped = json::Value::Arr(vec![json::Value::Arr(vec![json::s(PATH), json::s("padding")])]);
    assert_eq!(result.get("baked"), Some(&json::Value::Bool(true)));
    assert_eq!(result.get("skipped"), Some(&skipped), "the AI keeps the partial-bake report until the signed mutation");
    assert_eq!(result.get("dirty_files"), Some(&json::Value::Int(1)));
    assert!(result.get("seq").is_some());
    let retry = theme_ok(ai.run_design(&mut s, "bake", r#"{"scope":"props"}"#));
    assert_eq!(retry.get("baked"), Some(&json::Value::Bool(false)));
    assert_eq!(retry.get("skipped"), Some(&skipped));

    s.ana.send(Request::Design(drag_into("/panel")));
    settle_design(&mut s);
    assert!(s.ana.session.view.design.ghost.is_some());
    let req = s.ana.send(Request::Design(DesignRequest::GhostCommit));
    s.exchange();
    s.host.session.drive_app(&mut s.app);
    assert!(s.host.session.next_record().is_some(), "ghost promotion is awaiting its signed append");
    s.host.session.link_ended(LINK, "app closed");
    s.exchange();
    assert!(s.ana.session.view.design.ghost.is_some(), "link loss retains the preview awaiting its append");
    s.ana.session.view.updates.clear();
    s.host.session.record_failed("append refused");
    settle_design(&mut s);
    assert!(has(&s.ana, req, |n| matches!(n, Notice::Refused(why) if why == "append refused")));
    assert!(has(&s.ana, 0, |n| matches!(n, Notice::Design(DesignNotice::Status(raw))
        if design::DesignStateView::read(raw).is_some_and(|v| v.ghost.is_none()))));
    assert!(s.ana.session.view.design.ghost.is_none(), "the owner publishes the cleared ghost to the guest");
}

#[test]
fn ai_design_tools_never_open_panel_or_switch_tab() {
    use makepad_widgets::*;
    let (homes, mut owner, _guest, mut s) = theme_agents("design-panel");
    let _cx = configure_design_share(&mut s, &homes.0, "script_mod! { mod.UI = View{ panel := View{ row := View{} } } }");
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let ui = cx.with_vm(|vm| {
        let v = vm.eval(script! { use mod.widgets.* View { tweak_panel := AppTweakPanel { visible: false } } });
        WidgetRef::script_from_value(vm, v)
    });
    let root = ui.widget(cx, ids!(tweak_panel));
    let item = s.host.session.item;
    let mut hub = Tweaks::default();
    hub.set_themes_dir(owner.dir.clone());
    hub.sessions.push(s.host.session);
    hub.update(cx, &ui, &owner.model, &Default::default(), &mut owner.waits, |_| {});
    for (tab, wrap) in [(None, ids!(tweak_props_wrap)), (Some(ids!(tab_theme)), ids!(tweak_theme_wrap)),
        (Some(ids!(tab_shader)), ids!(tweak_shader_wrap))] {
        if let Some(tab) = tab {
            assert!(hub.open_panel(SHARE));
            hub.update(cx, &ui, &owner.model, &Default::default(), &mut owner.waits, |_| {});
            let button = root.widget(cx, tab).widget_uid();
            let actions = cx.capture_actions(|cx| cx.widget_action(button, ButtonAction::Clicked(Default::default())));
            hub.actions(cx, &ui, &actions);
        }
        let current = root.widget(cx, wrap);
        for (op, args) in [
            ("state", "{}"), ("palette", "{}"),
            ("insert", r#"{"path":"/panel","place":"inside","ty":"View","name":"card"}"#),
            ("move", r#"{"path":"/panel/card","direction":"up"}"#),
            ("duplicate", r#"{"path":"/panel/card","name":"copy"}"#),
            ("wrap", r#"{"path":"/panel/copy","name":"group_box","ty":"View"}"#),
            ("rename", r#"{"path":"/panel/card","name":"tile"}"#),
            ("delete", r#"{"path":"/panel/tile"}"#),
            ("bake", r#"{"scope":"props"}"#), ("patch", r#"{"op":"diff"}"#), ("history", r#"{"op":"reset"}"#),
        ] {
            let call = owner.design_call(item, op, args);
            assert!(hub.tool(cx, &ui, &mut owner.model, &call, &mut owner.waits).unwrap().0.is_none());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut result = None;
            while result.is_none() {
                assert!(std::time::Instant::now() < deadline, "{op} did not settle");
                let session = &mut hub.sessions[0];
                session.drive_app(&mut s.app);
                s.app.answer(session);
                hub.record(&mut owner.model);
                hub.update(cx, &ui, &owner.model, &Default::default(), &mut owner.waits, |r| result = Some(r));
                assert_eq!(root.visible(), tab.is_some(), "{op} changed panel visibility");
                assert!(current.visible(), "{op} changed the person's tab");
                std::thread::yield_now();
            }
            theme_ok(result.unwrap());
            assert!(hub.sessions[0].view.selections.is_empty(), "{op} changed the person's selection");
        }
    }
}

#[test]
fn ai_design_mutating_tools_respect_allow_and_stop() {
    use makepad_ai_services::wire::ToolOutcome;
    use makepad_town_core::{entity::field, event::Value};
    let (homes, mut owner, mut ai, mut s) = theme_agents("design-consent");
    let _cx = configure_design_share(&mut s, &homes.0, "script_mod! { mod.UI = View{ panel := View{} } }");
    let insert = r#"{"path":"/panel","place":"inside","ty":"View","name":"card"}"#;
    let denied = ai.run_design(&mut s, "insert", insert);
    assert_eq!(denied.outcome, ToolOutcome::Refused);
    assert!(denied.text.contains("Ask for control") && s.app.sent.is_empty() && s.records.is_empty());
    let seat = ai.model.me();
    s.allow(seat, true);
    theme_ok(ai.run_design(&mut s, "insert", insert));
    let [edit] = s.records.as_slice() else { panic!("one signed structural edit expected") };
    assert_eq!((edit.seat, edit.agent), (seat, Some(ai.agent)));
    let town = s.host.session.town;
    let saved = owner.model.act(Action::RecordAppEdit { town, item: s.host.session.item, edit: edit.clone() }, Origin::Ui).unwrap();
    crate::model::tests::exchange(&mut owner.model, &mut ai.model);
    assert_eq!(ai.model.tweak_board_state(town).unwrap().0.entities[&saved].value(field::EDIT_AGENT), Some(&Value::Id(ai.agent)));

    let before = s.app.design_source.as_ref().unwrap().text().to_owned();
    for stop in [true, false] {
        let pending = r#"{"path":"/panel","place":"inside","ty":"View","name":"pending"}"#;
        assert!(ai.start_design(&mut s.ana, "insert", pending).is_none());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.app.design_source.as_ref().unwrap().text().contains("pending :=") {
            assert!(std::time::Instant::now() < deadline, "the accepted insert must reach preview");
            s.one();
            std::thread::yield_now();
        }
        assert!(ai.notices(&mut s.ana, |_, _, _, _, _, _| {}).is_none(), "Running cannot complete the pending edit");
        if stop { s.host.session.stop_agent(seat, ai.agent, true).unwrap(); } else { s.host.session.end("share ended"); }
        settle_design(&mut s);
        let refused = ai.notices(&mut s.ana, |_, _, _, _, _, _| {}).expect("cancellation settles the held AI call");
        assert_eq!(refused.outcome, ToolOutcome::Refused);
        assert!(refused.text.contains(if stop { "stopped" } else { "share ended" }), "{}", refused.text);
        assert_eq!(s.app.design_source.as_ref().unwrap().text(), before, "cancellation restores the preceding preview");
        assert_eq!(s.records.len(), 1, "a cancelled insert leaves no signed edit");
        if stop { s.host.session.stop_agent(seat, ai.agent, false).unwrap(); s.exchange(); }
    }
}

#[test]
fn tree_drop_zone_computes_before_inside_after_and_blocks_descendant() {
    use tree::{drop_zone, DropIndicator::*};
    for ty in ["View", "Grid", "Masonry", "KeyboardView"] {
        for (y, expected) in [(0., Before), (24., Before), (25., Inside), (74., Inside), (75., After), (100., After)] {
            assert_eq!(drop_zone("a", "b", ty, y, 100.), Some(expected), "{ty} at {y}%");
        }
    }
    for ty in ["Button", "Window", "Dock", "Modal", "Splitter", "PageFlip"] {
        assert_eq!(drop_zone("a", "b", ty, 49., 100.), Some(Before), "{ty} upper half");
        assert_eq!(drop_zone("a", "b", ty, 50., 100.), Some(After), "{ty} lower half");
    }
    for (source, target) in [("a.b", "a.b.c"), ("/dock", "/dock/card"), ("/dock", "/dock"), ("/", "/dock")] {
        assert_eq!(drop_zone(source, target, "View", 5., 10.), None, "{source} onto {target}");
    }
    assert_eq!(drop_zone("/dock/View", "/dock/View.2", "View", 5., 10.), Some(Inside));
    for (source, target) in [("", "/dock"), ("", ""), ("/dock", ""), ("/", "dock")] {
        assert!(!tree::is_self_or_descendant(source, target), "{source:?} is not an ancestor of {target:?}");
    }
    for (y, height, expected) in [(-0.001, 100., Some(Before)), (100.001, 100., Some(After)),
        (f64::NAN, 100., None), (1., 0., None), (1., f64::INFINITY, None)] {
        assert_eq!(drop_zone("a", "b", "View", y, height), expected, "row geometry {y}/{height}");
    }
}

fn tree_drop_uniform(cx: &Cx, face: &makepad_widgets::View) -> f32 {
    use makepad_widgets::*;
    let vars = &face.draw_bg.draw_vars;
    let inputs = &cx.draw_shaders[vars.draw_shader_id.unwrap().index].mapping.dyn_instances;
    let drop = inputs.inputs.iter().find(|i| i.id == id!(drop_mode)).expect("Tree row has drop_mode");
    vars.dyn_instances[vars.dyn_instances.len() - inputs.total_slots + drop.offset]
}

#[test]
fn design_drag_overlay_and_tree_read_theme_tokens() {
    use crate::{board::canvas::BoardCanvas, call::SharePanel};
    use makepad_widgets::*;

    let mut cx = Cx::new(Box::new(|_, _| {}));
    cx.init_cx_os();
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::script_mod(vm);
    });

    let check = |cx: &mut Cx, theme_id: LiveId| {
        let (share, board, portal, colors) = cx.with_vm(|vm| {
            let value = script_eval!(vm, { mod.widgets.SharePanel{} });
            let share = SharePanel::script_from_value(vm, value);
            let value = script_eval!(vm, { mod.widgets.BoardCanvas{} });
            let board = BoardCanvas::script_from_value(vm, value);
            let row = View::script_proto(vm);
            let value = super::tree::view::tree_list_value(vm, row);
            let portal = WidgetRef::script_from_value(vm, value);
            let themes = vm.module(id!(themes));
            let theme = vm.value(themes, theme_id).as_object().unwrap();
            let colors = [id!(color_primary), id!(color_on_primary), id!(color_drag_target_preview)].map(|role| {
                let value = vm.value(theme, role);
                Vec4f::script_from_value(vm, value)
            });
            (share, board, portal, colors)
        });
        for ghost in [share.ghost_ink(), board.design_ghost()] {
            assert_eq!([ghost.color, ghost.on_color, ghost.veil_color], colors, "{theme_id} ghost inks");
            assert!(crate::theme::wcag_contrast(ghost.color, ghost.on_color) >= 4.5, "{theme_id} ghost label contrast");
        }
        let row = portal.as_portal_list().borrow_mut().unwrap().item(cx, 0, id!(TreeRow));
        let face = row.view(cx, ids!(tree_face));
        let face = face.borrow().unwrap();
        let vars = &face.draw_bg.draw_vars;
        for (role, color) in [(id!(drop_line), colors[0]), (id!(drop_veil), colors[2])] {
            assert_eq!(vars.uniform_range(cx, role).map(|(_, slots)| slots), Some(4), "{role} uniform");
            let mut rgba = [0.; 4];
            vars.get_uniform(cx, role, &mut rgba);
            assert_eq!(rgba, [color.x, color.y, color.z, color.w], "{theme_id} tree {role}");
        }
        colors
    };
    let dark = check(&mut cx, id!(dark));
    cx.with_vm(|vm| vm.with_reload(|vm| {
        script_eval!(vm, {
            mod.theme = mod.themes.light
            mod.prelude.workspace.theme = mod.theme
            mod.theme.color_drag_target_preview = #x30609080
        });
        crate::board::canvas::script_mod(vm);
        crate::call::script_mod(vm);
    }));
    let light = check(&mut cx, id!(light));
    for (dark, light) in dark.into_iter().zip(light) {
        assert_ne!(dark, light, "each tested role must change on reload");
    }
}

#[test]
fn tree_row_bind_resets_drop_uniform_on_reuse() {
    use makepad_widgets::*;
    use super::tree::{DropIndicator, TreeLine};
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let portal = cx.with_vm(|vm| {
        let row = View::script_proto(vm);
        let value = super::tree::view::tree_list_value(vm, row);
        WidgetRef::script_from_value(vm, value)
    });
    let row = portal.as_portal_list().borrow_mut().unwrap().item(cx, 0, id!(TreeRow));
    let line = |drop| TreeLine::Node { path: "node".into(), depth: 0, label: "Node".into(), ty: "View".into(),
        dim: false, has_children: false, expanded: false, selected: false, even: false, seats: vec![], drop };
    let face = row.view(cx, ids!(tree_face));
    for (drop, expected) in [(Some(DropIndicator::Before), 1.), (None, 0.)] {
        line(drop).draw_tree(cx, &row.borrow::<View>().unwrap(), true);
        assert_eq!(tree_drop_uniform(cx, &face.borrow().unwrap()), expected, "same row rebound with {drop:?}");
    }
}

#[test]
fn tree_drop_disabled_while_filter_active() {
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 285} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut draw = draw_panel(cx, root.clone());
    let mut s = Copies::new(OWNER, ANA, ITEM);
    let mut panel = test_panel();
    s.app.tree = TREE.replace(r#""path":"/dock","#, r#""path":"","#);
    let state = Default::default();
    panel.refresh(cx, &root, &s.host.session, &state, &Default::default());
    draw(cx);
    let tab = root.button(cx, ids!(tab_tree));
    let (_, press, release) = panel_click_at(cx, &root, tab.area().rect(cx).center(), 1.);
    for batch in [&press, &release] { panel.actions(cx, &root, batch, &mut s.host.session); }
    s.one();
    while let Some((seat, req, agent, notice)) = s.host.session.view.updates.pop_front() {
        panel.notice(cx, &root, &mut s.host.session, seat, req, agent, &state, &notice);
    }
    draw(cx);
    let widget = root.widget(cx, ids!(tweak_tree));
    let portal = root.portal_list(cx, ids!(tweak_tree_list));
    let face = portal.get_item(0).unwrap().1.view(cx, ids!(tree_face));
    let rect = face.area().rect(cx);
    let abs = rect.center();
    let window_id = makepad_widgets::WindowId(1, 1);
    widget.handle_event(cx, &Event::Drag(DragEvent {
        abs, window_id, modifiers: Default::default(), handled: Default::default(), response: std::sync::Arc::new(std::sync::Mutex::new(DragResponse::None)),
        items: std::sync::Arc::new(vec![DragItem::String { value: "/other".into(), internal_id: Some(live_id!(town_design_node)) }]),
    }), &mut Scope::empty());
    draw(cx);
    assert_eq!(tree_drop_uniform(cx, &face.borrow().unwrap()), 3., "unfiltered container shows Inside");
    widget.handle_event(cx, &Event::MouseDown(MouseDownEvent { abs, window_id, button: MouseButton::PRIMARY,
        time: 2., modifiers: Default::default(), handled: Default::default() }), &mut Scope::empty());
    let mut tree = super::tree::TreeState::default();
    tree.offer_search("card".into());
    widget.borrow_mut::<panel::TweakList>().unwrap().configure_tree_drag(cx, true, &tree);
    draw(cx);
    assert_eq!(tree_drop_uniform(cx, &face.borrow().unwrap()), 0., "filter clears the pending indicator");
    widget.handle_event(cx, &Event::MouseMove(MouseMoveEvent { abs: abs + dvec2(8., 0.), window_id,
        time: 2.1, modifiers: Default::default(), handled: Default::default(), lock_delta: DVec2::default() }), &mut Scope::empty());
    assert!(cx.drag_visual().is_none(), "filter cancels a press before crossing the drag threshold");
}

fn palette_picture(drop: bool, x: f64) -> design::drag::DesignPictureEvent {
    use makepad_widgets::*;
    use super::design::drag::DesignPictureEvent;
    let items = std::sync::Arc::new(vec![DragItem::String { value: "Button".into(), internal_id: Some(live_id!(town_design_palette)) }]);
    let point = dvec2(x, 12.);
    if drop { DesignPictureEvent::Drop { item: ITEM, window: 0, point, items } }
    else { DesignPictureEvent::Hover { item: ITEM, window: 0, point, items } }
}

fn sent_drag_packets(s: &mut Copies) -> Vec<(RequestId, DesignRequest)> {
    s.ana.poll();
    s.host.relay.take(TWEAK).into_iter().filter_map(|(sender, b)| {
        let p = wire::decode(sender, &b).unwrap().unwrap();
        if let Body::Request(Request::Design(r)) = p.body { Some((p.req, r)) } else { None }
    }).collect()
}

fn drop_reply() -> Notice {
    Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::DropTarget, json:
        r#"{"target_path":"/host","container_path":"/host","place":"inside","flow":"col","target_rect":[0,0,40,30],"container_rect":[0,0,40,30]}"#.into() })
}

#[test]
fn canvas_drag_fast_drop_before_dwell_commits_directly() {
    use makepad_widgets::{TimerEvent, Timer};
    let cx = &mut Cx::new(Box::new(|_,_| {}));
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    let mut drag = design::drag::DragController::default();
    for pending in [false, true] {
        drag.picture(cx, &mut s.ana.session, &palette_picture(false, 10.));
        let packets = sent_drag_packets(&mut s);
        let [(req, DesignRequest::DropTarget { exclude: None, .. })] = packets.as_slice() else { panic!("expected one palette hit query: {packets:?}") };
        let mut timer = Timer::empty();
        if !pending {
            assert!(drag.notice(cx, &mut s.ana.session, *req, &drop_reply()));
            timer = drag.timer(SHARE);
            assert_ne!(timer.0, 0);
        }
        drag.picture(cx, &mut s.ana.session, &palette_picture(true, 10.));
        drag.event(cx, &mut s.ana.session, &Event::DragEnd);
        if pending {
            assert!(sent_drag_packets(&mut s).is_empty(), "drop waits for its unresolved hit");
            assert!(drag.notice(cx, &mut s.ana.session, *req, &drop_reply()));
        }
        drag.event(cx, &mut s.ana.session, &Event::Timer(TimerEvent { timer_id: timer.0, time: None }));
        assert!(matches!(sent_drag_packets(&mut s).as_slice(), [(_, DesignRequest::Insert { path, place: DesignPlace::Inside, ty, .. })]
            if path == "/host" && ty == "Button"), "drop commits once without a late ghost, pending={pending}");
    }
    // Two releases can both precede the first peer reply. Resolve them in order.
    drag.picture(cx, &mut s.ana.session, &palette_picture(true, 10.));
    drag.event(cx, &mut s.ana.session, &Event::DragEnd);
    drag.picture(cx, &mut s.ana.session, &palette_picture(true, 20.));
    drag.event(cx, &mut s.ana.session, &Event::DragEnd);
    let queries = sent_drag_packets(&mut s);
    let [(first, DesignRequest::DropTarget { x: 10., .. })] = queries.as_slice() else { panic!("first drop owns the query slot") };
    drag.notice(cx, &mut s.ana.session, *first, &drop_reply());
    let packets = sent_drag_packets(&mut s);
    let [(_, DesignRequest::Insert { .. }), (second, DesignRequest::DropTarget { x: 20., .. })] = packets.as_slice()
        else { panic!("release the first edit before querying the second drop: {packets:?}") };
    drag.notice(cx, &mut s.ana.session, *second, &drop_reply());
    assert!(matches!(sent_drag_packets(&mut s).as_slice(), [(_, DesignRequest::Insert { .. })]));
}

#[test]
fn canvas_drag_dwell_then_cancel_retracts_ghost() {
    use makepad_widgets::{TimerEvent, KeyEvent, KeyCode};
    let cx = &mut Cx::new(Box::new(|_,_| {}));
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    let mut drag = design::drag::DragController::default();
    drag.picture(cx, &mut s.ana.session, &palette_picture(false, 10.));
    drag.picture(cx, &mut s.ana.session, &palette_picture(false, 13.));
    let packets = sent_drag_packets(&mut s);
    let [(req, DesignRequest::DropTarget { x: 10., .. })] = packets.as_slice() else { panic!("one in-flight pointer query: {packets:?}") };
    drag.notice(cx, &mut s.ana.session, *req, &drop_reply());
    let timer = drag.timer(SHARE);
    let packets = sent_drag_packets(&mut s);
    let [(req, DesignRequest::DropTarget { x: 13., .. })] = packets.as_slice() else { panic!("coalesced latest pointer: {packets:?}") };
    drag.notice(cx, &mut s.ana.session, *req, &drop_reply());
    assert_eq!(drag.timer(SHARE).0, timer.0, "motion over the same placement preserves dwell");
    let tick = Event::Timer(TimerEvent { timer_id: timer.0, time: None });
    drag.event(cx, &mut s.ana.session, &tick);
    let packets = sent_drag_packets(&mut s);
    let [(req, DesignRequest::GhostInsert { target, .. })] = packets.as_slice() else { panic!("dwell starts exactly one preview: {packets:?}") };
    assert_eq!(target, "/host");
    drag.event(cx, &mut s.ana.session, &Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
    assert!(drag.caret(SHARE).is_none() && sent_drag_packets(&mut s).is_empty(), "local hint clears while provisional operation settles");
    drag.notice(cx, &mut s.ana.session, *req, &Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, json: "{}".into() }));
    drag.event(cx, &mut s.ana.session, &tick);
    let packets = sent_drag_packets(&mut s);
    let [(cancel, DesignRequest::GhostCancel)] = packets.as_slice() else { panic!("late preview must be retracted exactly once: {packets:?}") };
    let state = Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, json: "{}".into() });
    drag.notice(cx, &mut s.ana.session, *cancel, &state);

    for retracted in [true, false] {
        drag.picture(cx, &mut s.ana.session, &palette_picture(false, 10.));
        let query = sent_drag_packets(&mut s)[0].0;
        drag.notice(cx, &mut s.ana.session, query, &drop_reply());
        let tick = Event::Timer(TimerEvent { timer_id: drag.timer(SHARE).0, time: None });
        drag.event(cx, &mut s.ana.session, &tick);
        let packets = sent_drag_packets(&mut s);
        let [(preview, DesignRequest::GhostInsert { .. })] = packets.as_slice() else { panic!("expected a preview") };
        drag.notice(cx, &mut s.ana.session, *preview, &state);
        if retracted { drag.sync_status(&s.ana.session); }
        drag.picture(cx, &mut s.ana.session, &palette_picture(true, 10.));
        let packets = sent_drag_packets(&mut s);
        if retracted {
            assert!(matches!(packets.as_slice(), [(_, DesignRequest::Insert { .. })]), "owner already retracted the preview");
        } else {
            assert!(matches!(packets.as_slice(), [(_, DesignRequest::GhostCommit)]), "an acknowledged preview commits in place");
        }
    }
}

#[test]
fn live_resize_refreshes_selection_ring_and_handles_before_release() {
    use super::{design::drag::DesignPictureEvent, overlay::{ResizePhase, ResizePress, Selections, SizeHandle, size_handle_rects}};
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 285} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_peer_level(ANA, Some(DESIGN_LEVEL));
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    s.host.session.bind_window(super::WindowId(0));
    s.host.session.window(super::WindowId(0), [420., 220.]);
    s.allow(ANA, true);
    s.app.props = r##"{"props":[{"name":"width","value":"66","set":true},{"name":"draw_bg.color","value":"#fff","set":true}],"cascade":[]}"##.into();
    s.app.prop_values = [("width".into(), "66".into()), ("draw_bg.color".into(), "#fff".into())].into();
    s.list();
    let original = super::Rect { window: 0, x: 354., y: 0., w: 66., h: 28. };
    let grown = super::Rect { x: 324., w: 96., ..original };
    s.app.picks.push_back(Ok(format!(r#"{{"window":0,"rect":[354,0,66,28],"path":"{PATH}","type":"Button"}}"#)));
    s.host.send(Request::Pick(At::Path(PATH.into())));
    s.one();
    s.ana.send(Request::Select(Some(PATH.into())));
    s.exchange();
    let state = Default::default();
    let mut panel = test_panel();
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    s.ana.session.view.updates.clear();
    panel.restore_selection(cx, &root, &mut s.ana.session);
    assert_eq!(panel.sidebar_rect(), Some(original));
    let tab = root.button(cx, ids!(tab_build));
    let actions = cx.capture_actions(|cx| cx.widget_action(tab.widget_uid(), ButtonAction::Clicked(Default::default())));
    panel.actions(cx, &root, &actions, &mut s.ana.session);
    assert!(panel.is_build_mode(SHARE, ITEM));
    sent_drag_packets(&mut s); // Drain the queries from restoring the panel and opening Build.

    let selections = |view: &SeatView| Selections::new(0, [420., 220.], view, &state)
        .with_design(SHARE, view, &ANA, true, true, None);
    let rings = selections(&s.ana.session.view);
    let tf = rings.transform(makepad_widgets::Rect { pos: dvec2(0., 0.), size: dvec2(420., 220.) });
    let [(right, handle), (_, bottom), _] = size_handle_rects(&tf, original);
    assert_eq!(right, SizeHandle::Right);
    let start = handle.center();
    let mut press = ResizePress::begin(ITEM, tf, &rings, event::DigitId(live_id!(test_mouse)), start).unwrap();
    let drag = |cx: &mut Cx, press: &mut ResizePress, dx| {
        let actions = cx.capture_actions(|cx| press.update(cx, tab.widget_uid(), start + dvec2(dx, 0.), ResizePhase::Drag));
        let action = actions[0].as_widget_action().unwrap();
        let Some(DesignPictureEvent::Resize { resize, .. }) = action.action.downcast_ref() else { panic!("expected resize") };
        resize.clone()
    };
    let revisions = [s.host.session.view.selection_revision, s.ana.session.view.selection_revision];
    s.app.typed_rects.push_back(grown);
    let resize = drag(cx, &mut press, 30.);
    panel.resize_widget(cx, &root, &mut s.ana.session, &resize);
    s.one();
    for (view, revision) in [&s.host.session.view, &s.ana.session.view].into_iter().zip(revisions) {
        for seat in [OWNER, ANA] { assert_eq!(view.selections[&seat].rect, Some(grown)); }
        assert!(view.selection_revision > revision, "the edit invalidates both seats' overlays before release");
    }
    let rings = selections(&s.ana.session.view);
    assert_eq!(rings.local_selection, Some((PATH.into(), grown)));
    let tf = rings.transform(makepad_widgets::Rect { pos: dvec2(0., 0.), size: dvec2(420., 220.) });
    let [(_, new_right), (_, new_bottom), (_, corner)] = size_handle_rects(&tf, rings.local_selection.unwrap().1);
    assert_eq!(new_right, handle, "the right edge is anchored");
    assert_eq!(new_bottom.center(), bottom.center() - dvec2(15., 0.));
    assert_eq!(corner.center(), dvec2(handle.center().x, new_bottom.center().y), "the corner follows both edges");
    while let Some((seat, req, agent, notice)) = s.ana.session.view.updates.pop_front() {
        panel.notice(cx, &root, &mut s.ana.session, seat, req, agent, &state, &notice);
    }
    assert_eq!(panel.sidebar_rect(), Some(grown));
    assert!(root.label(cx, ids!(tweak_result)).text().ends_with(&format!(" set {PATH}.width to 96")));
    s.ana.poll();
    assert!(s.host.relay.take(TWEAK).into_iter().all(|(sender, body)| {
        !matches!(wire::decode(sender, &body).unwrap().unwrap().body, Body::Request(Request::Props { .. } | Request::Pick(_)))
    }), "geometry refresh after restoring a selection must not read props or re-pick");
    assert_eq!(drag(cx, &mut press, 35.).width, Some(101.), "the second step uses the width at finger down");

    let revisions = [s.host.session.view.selection_revision, s.ana.session.view.selection_revision];
    s.host.session.view.updates.clear();
    s.app.typed_rects.push_back(grown);
    s.ana.send(Request::Set { path: PATH.into(), prop: "draw_bg.color".into(), value: "#f00".into() });
    s.one();
    assert_eq!(s.app.prop_values["draw_bg.color"], "#f00");
    for (view, revision) in [&s.host.session.view, &s.ana.session.view].into_iter().zip(revisions) {
        assert_eq!(view.selection_revision, revision);
        assert!(!view.updates.iter().any(|(_, _, _, n)| matches!(n, Notice::Selected { .. })));
    }
}

#[test]
fn size_handle_drag_updates_live_walk_under_zoom() {
    use super::{design::drag::DesignPictureEvent, focus::FocusState, overlay::{ResizePhase, ResizePress, Selections, SizeHandle}};
    use makepad_widgets::*;
    let mut pool = panel_test_cx();
    let cx = &mut *pool;
    let root = cx.with_vm(|vm| {
        let value = vm.eval(script! { use mod.widgets.* AppTweakPanel{width: 285} });
        WidgetRef::script_from_value(vm, value)
    });
    let mut s = Copies::new(OWNER, ANA, ITEM);
    s.host.session.set_peer_level(ANA, Some(DESIGN_LEVEL));
    s.ana.session.set_peer_level(OWNER, Some(DESIGN_LEVEL));
    s.app.props = r#"{"props":[{"name":"width","value":"40","set":true},{"name":"height","value":"20","set":true}],"cascade":[]}"#.into();
    s.app.prop_values = [("width".into(),"40".into()), ("height".into(),"20".into())].into();
    s.allow(ANA, true);
    s.list();
    let mut panel = test_panel();
    let state = Default::default();
    panel.refresh(cx, &root, &s.ana.session, &state, &Default::default());
    let rect = super::Rect { window: 0, x: 50., y: 30., w: 40., h: 20. };
    panel.notice(cx, &root, &mut s.ana.session, ANA, 1, None, &state, &Notice::Picked { path: PATH.into(), ty: "View".into(), rect });
    let tab = root.button(cx, ids!(tab_build));
    let actions = cx.capture_actions(|cx| cx.widget_action(tab.widget_uid(), ButtonAction::Clicked(Default::default())));
    panel.actions(cx, &root, &actions, &mut s.ana.session);
    assert!(panel.is_build_mode(SHARE, ITEM), "the local seat opened Build");
    sent_drag_packets(&mut s); // Discard the tab's read-only palette and property queries.

    let mut focus = FocusState::default();
    focus.toggle_isolate(Some((PATH, Some(rect))));
    focus.step_zoom(1.);
    let mut view = SeatView::default();
    view.selections.insert(ANA, Selection { path: PATH.into(), rect: Some(rect), agent: None });
    let rings = Selections::new(0, [200., 100.], &view, &state).with_focus(focus)
        .with_design(SHARE, &view, &ANA, true, true, None);
    let tf = rings.transform(makepad_widgets::Rect { pos: dvec2(15., 8.), size: dvec2(200., 100.) });
    let start = tf.app_to_screen(dvec2(90., 40.));
    assert_eq!(super::overlay::hit_size_handle(&tf, &rings, start), Some(SizeHandle::Right));
    let corner = |delta| super::overlay::resize_from_drag(&tf, rect, SizeHandle::Corner, start, start + delta);
    assert_eq!(corner(dvec2(21.4, -3.4)), (Some(51.), Some(18.)), "round each app-space dimension");
    assert_eq!(corner(dvec2(-100., -60.)), (Some(1.), Some(1.)), "both dimensions retain one point");
    let uid = tab.widget_uid();
    let mut press = ResizePress::begin(ITEM, tf, &rings, event::DigitId(live_id!(test_mouse)), start).unwrap();
    for (phase, dx, count) in [(ResizePhase::Drag, 21.4, 1), (ResizePhase::Drag, 21.2, 0), (ResizePhase::Release, 100.4, 1)] {
        let actions = cx.capture_actions(|cx| press.update(cx, uid, start + dvec2(dx, 0.), phase));
        assert_eq!(actions.len(), count, "sub-point jitter within the same width emits no action: {dx}");
        if actions.is_empty() { continue }
        let action = actions[0].as_widget_action().unwrap();
        let Some(DesignPictureEvent::Resize { resize, .. }) = action.action.downcast_ref() else { panic!("expected resize") };
        panel.resize_widget(cx, &root, &mut s.ana.session, resize);
    }
    let dispatch = |cx: &mut Cx, panel: &mut panel::Panel, s: &mut Copies| {
        while let Some((seat, req, agent, notice)) = s.ana.session.view.updates.pop_front() {
            if matches!(notice, Notice::Applied { .. } | Notice::Refused(_)) {
                panel.notice(cx, &root, &mut s.ana.session, seat, req, agent, &state, &notice);
            }
        }
    };
    s.one();
    assert_eq!(s.app.prop_values["width"], "51", "21.4 screen points at 2× add 11 whole app points");
    dispatch(cx, &mut panel, &mut s);
    s.one();
    assert_eq!(s.app.prop_values["width"], "90", "release outside the picture uses the original rect and inverse transform");
    assert_eq!(s.app.prop_values["height"], "20", "Right touches only width");
    assert!(sent_drag_packets(&mut s).is_empty(), "bake waits for the last width acknowledgement");
    dispatch(cx, &mut panel, &mut s);
    s.ana.poll();
    let requests: Vec<_> = s.host.relay.take(TWEAK).into_iter().filter_map(|(sender, body)| {
        match wire::decode(sender, &body).unwrap().unwrap().body { Body::Request(r) => Some(r), _ => None }
    }).collect();
    assert_eq!(requests.iter().filter(|r| matches!(r, Request::Design(DesignRequest::Bake { scope: BakeScope::Props, path: Some(p), prop: None }) if p == PATH)).count(), 1);
    assert!(requests.contains(&Request::Pick(At::Path(PATH.into()))), "settled geometry is picked again");
}
