//! `b.net`: a firmware part's connections reach the lowering with what the twin plays for each and
//! the real server it may reach; invalid declarations are refused at the offending argument, a
//! named endpoint (one that may send a token) on a plain scheme to another machine among them.

use fab_elec_bench::net::{Body, Fake, Field, NetEndpoint, Real, RuleSpec, Scheme, SendSpec, Server, When};
use fab_elec_bench::{ModelLibs, lower};
use fab_elec_core::{Sel, Severity};
use fab_elec_lang::{Sources, UnitState, evaluate, std_libs};

const CIRCUIT: &str = r#"
use mod.elec.*
use mod.std.*
let e = mod.elec
e.define("S3_T", {
    kind: @mcu
    fields: {model: "ESP32-S3"}
    symbol: ic_symbol("S3_T", {left: [["VDD" @power_in] ["GND" @power_in] ["IO1" @bidi]]})
})
let gnd = e.net({name: "GND" kind: @ground})
let vdd = e.net({name: "VDD" kind: @power volts: 3.3V})
let u = e.part("S3_T", {pins: {VDD: vdd GND: gnd}})
let pull = e.r(10kohm, vdd, u.IO1)
"#;

#[test]
fn net_declares_each_connection_and_refuses_what_it_cannot_play() {
    let mut sources = Sources::new();
    sources.add("radio.ckt.splash", CIRCUIT).expect("the circuit is accepted");
    sources.add("radio.fw.splash", "firmware text is not evaluated by the circuit language").expect("the firmware is accepted");
    sources.add("good.bench.splash", r#"
bench("chat", {circuit: "radio"}, fn(b, c) {
    b.firmware(c.u, "radio.fw.splash", {})
    b.net(c.u, 0, {endpoint: "chat" url: "wss://chat.example.org/ws" latency: 20ms jitter: 5ms rate: 200000})
    b.net(c.u, 1, {fake: "sink"})
    b.net(c.u, 2)
    b.net(c.u, 3, {url: "ws://192.0.2.7:8080/"})
    b.net(c.u, 4, {endpoint: "dev" url: "ws://localhost:8080/"})
    b.net(c.u, 5, {fake: {files: "photos/"}})
    b.net(c.u, 6, {fake: {rules: [
        {ready: true send: ["count 0"]}
        {on: "end " send: [
            {after: 300ms text: "heard {{{1}}}"}
            {binary: ["A" 0 "{1:u16}{offset:u32}"] wav: "voice/reply.wav" rate: 16000 block: 320 every: 20ms}
            "done {1} {sent}"
        ]}
        {on: "cancel " stop: true send: ["done {1} {sent}"]}
        {binary: "M" whole: false}
    ]}})
    b.net(c.u, 7, {hub: "talk" fake: "sink"})
    b.net(c.u, 8, {model: "voice/β+1&2" hub: "talk"})
})
"#).expect("the bench is accepted");
    let fw = "b.firmware(c.u, \"radio.fw.splash\", {})\n";
    let cases = [
        ("before_firmware", "b.net(c.u, 0, {})".to_string(), "net: u runs no firmware: b.firmware(..) first", "b.net(c.u, 0, {})"),
        ("twice", format!("{fw}b.net(c.u, 0, {{}})\nb.net(c.u, 0, {{}})"), "net: u's connection 0 is already declared", "0"),
        ("not_whole", format!("{fw}b.net(c.u, 1.5, {{}})"), "net: the connection is a whole number from 0 to 255 (the program's net declarations in order)", "1.5"),
        ("plaintext", format!("{fw}b.net(c.u, 0, {{endpoint: \"chat\" url: \"ws://chat.example.org/\"}})"),
            "net: url: plaintext: a named endpoint's token goes only over wss:// or tls:// (or to this machine), not ws://", "\"ws://chat.example.org/\""),
        ("plaintext_stream", format!("{fw}b.net(c.u, 0, {{url: \"tcp://192.0.2.7:7\" endpoint: \"x\"}})"),
            "net: url: plaintext: a named endpoint's token goes only over wss:// or tls:// (or to this machine), not tcp://", "\"tcp://192.0.2.7:7\""),
        ("credentials", format!("{fw}b.net(c.u, 0, {{endpoint: \"x\" url: \"wss://me:pw@example.org/\"}})"),
            "net: url: a server address holds no user name or password: the token is kept in your Fab settings", "\"wss://me:pw@example.org/\""),
        ("fake", format!("{fw}b.net(c.u, 0, {{fake: \"loop\"}})"), "net: fake: \"echo\", \"sink\", {files: \"<folder>\"} or {rules: [..]}", "\"loop\""),
        ("files_outside", format!("{fw}b.net(c.u, 0, {{fake: {{files: \"../photos\"}}}})"),
            "net: fake: files: a folder under the project's firmware/, like \"photos\"", "\"../photos\""),
        ("rule_two_answers", format!("{fw}b.net(c.u, 0, {{fake: {{rules: [{{on: \"a\" ready: true}}]}}}})"),
            "net: rules: a rule answers one thing: on: \"<text>\", ready: true or binary: \"<bytes>\"", "{on: \"a\" ready: true}"),
        ("rule_field", format!("{fw}b.net(c.u, 0, {{fake: {{rules: [{{on: \"a\" send: [{{text: \"x {{turn}}\"}}]}}]}}}})"),
            "net: send: {turn}: a word is numbered from 0 to 255", "\"x {turn}\""),
        ("rule_joined", format!("{fw}b.net(c.u, 0, {{fake: {{rules: [{{on: \"a\" send: [\"x\" {{text: \"y\"}}]}}]}}}})"),
            "net: send: this item joined the one before it (a value followed by {..} in a list is one item): separate the items with commas", "{text: \"y\"}"),
        ("rule_block", format!("{fw}b.net(c.u, 0, {{fake: {{rules: [{{on: \"a\" send: [{{text: \"x\" block: 4}}]}}]}}}})"),
            "net: send: block: cuts a file or wav into messages; this message has none", "4"),
        ("hub_and_url", format!("{fw}b.net(c.u, 0, {{url: \"wss://chat.example.org/\" hub: \"talk\"}})"),
            "net: url or hub: a connection reaches one real end", "\"talk\""),
        ("hub_name", format!("{fw}b.net(c.u, 0, {{hub: \"/talk\"}})"),
            "net: hub: the name of a service of this computer's AI hub, like \"talk\"", "\"/talk\""),
        ("hub_token", format!("{fw}b.net(c.u, 0, {{hub: \"talk\" endpoint: \"chat\"}})"),
            "net: endpoint: a hub service takes no token, so no endpoint name: Fab reaches this computer's hub as this computer", "\"chat\""),
        ("model_spaces", format!("{fw}b.net(c.u, 0, {{hub: \"talk\" model: \"a b\"}})"),
            "net: model: the id of a chat model, without spaces", "\"a b\""),
        ("model_without_hub", format!("{fw}b.net(c.u, 0, {{url: \"wss://chat.example.org/\" model: \"m\"}})"),
            "net: model: only a hub service's turns ask a chat model: add hub: \"talk\"", "\"m\""),
        ("latency", format!("{fw}b.net(c.u, 0, {{latency: -1ms}})"), "net: latency: must be nonnegative", "-1ms"),
        ("unknown", format!("{fw}b.net(c.u, 0, {{token: \"abc\"}})"),
            "unknown option token (the options are endpoint, url, hub, model, fake, latency, jitter, rate)", "token: \"abc\""),
    ];
    let mut errors = String::new();
    let mut starts = Vec::new();
    for (name, body, _, selection) in &cases {
        errors.push_str(&format!("bench(\"{name}\", {{circuit: \"radio\"}}, fn(b, c) {{\n"));
        starts.push(errors.len() + body.rfind(*selection).expect("the selection is in its body"));
        errors.push_str(body);
        errors.push_str("\n})\n");
    }
    let errors_doc = sources.add("errors.bench.splash", errors).expect("the refusals are accepted");
    let ev = evaluate(&sources, &std_libs()).expect("the circuit evaluates");
    let bench = |name: &str| ev.benches.iter().find(|b| b.name == name).expect("the named bench has a result");

    let good = bench("chat");
    assert_eq!(good.state, UnitState::Built, "{:?}", good.state.diags());
    let chat = Server { scheme: Scheme::Wss, host: "chat.example.org".into(), port: 443, path: "/ws".into() };
    let played = |c, fake| NetEndpoint { path: "u".into(), c, name: None, real: None, fake, latency: 0.0, jitter: 0.0, rate: 0.0 };
    let server = |server| Some(Real::Server(server));
    let want = [
        NetEndpoint { name: Some("chat".into()), real: server(chat), latency: 0.02, jitter: 0.005, rate: 200_000.0, ..played(0, Fake::Echo) },
        played(1, Fake::Sink),
        played(2, Fake::Echo),
        NetEndpoint { real: server(Server::parse("ws://192.0.2.7:8080/").unwrap()), ..played(3, Fake::Echo) },
        NetEndpoint { name: Some("dev".into()), real: server(Server::parse("ws://localhost:8080/").unwrap()), ..played(4, Fake::Echo) },
        played(5, Fake::Files("photos".into())),
        played(6, Fake::Rules(vec![
            RuleSpec { when: When::Ready, stop: false, send: vec![
                SendSpec { after: 0.0, binary: false, head: vec![Field::Bytes(b"count 0".to_vec())], body: Body::None, block: 0, every: 0.0 },
            ] },
            RuleSpec { when: When::Message { binary: false, bytes: b"end ".to_vec(), whole: false }, stop: false, send: vec![
                SendSpec { after: 0.3, binary: false, head: vec![Field::Bytes(b"heard {".to_vec()), Field::Word(1), Field::Bytes(b"}".to_vec())],
                    body: Body::None, block: 0, every: 0.0 },
                SendSpec { after: 0.0, binary: true, head: vec![Field::Bytes(b"A".to_vec()), Field::Bytes(vec![0]), Field::Word16(1), Field::Offset32],
                    body: Body::Wav { file: "voice/reply.wav".into(), rate: 16000 }, block: 320, every: 0.02 },
                SendSpec { after: 0.0, binary: false, head: vec![Field::Bytes(b"done ".to_vec()), Field::Word(1), Field::Bytes(b" ".to_vec()), Field::Sent],
                    body: Body::None, block: 0, every: 0.0 },
            ] },
            RuleSpec { when: When::Message { binary: false, bytes: b"cancel ".to_vec(), whole: false }, stop: true, send: vec![
                SendSpec { after: 0.0, binary: false, head: vec![Field::Bytes(b"done ".to_vec()), Field::Word(1), Field::Bytes(b" ".to_vec()), Field::Sent],
                    body: Body::None, block: 0, every: 0.0 },
            ] },
            RuleSpec { when: When::Message { binary: true, bytes: b"M".to_vec(), whole: false }, stop: false, send: Vec::new() },
        ])),
        NetEndpoint { real: Some(Real::Hub { service: "talk".into(), model: None }), ..played(7, Fake::Sink) },
        NetEndpoint { real: Some(Real::Hub { service: "talk".into(), model: Some("voice/β+1&2".into()) }), ..played(8, Fake::Echo) },
    ];
    assert_eq!(good.spec.nets, want,
        "each connection keeps its endpoint, server, fake and conditions; echo is the default; a plain server goes unnamed (no token), or named on this machine; a files fake names its folder, a rules fake its table with templates parsed and files named; a hub service is named, with no address, and with the chat model its turns ask when the bench names one, written before or after hub");
    let lowered = lower(&ev.design, good.scope_id(), &good.spec, &ModelLibs::generic());
    assert_eq!(lowered.firmware[0].net, want, "the firmware binding carries its part's connections");

    for ((name, _, message, selection), expected_start) in cases.iter().zip(starts) {
        let failed = bench(name);
        let expected = format!("{name}: {message}");
        let diag = failed.state.diags().iter().find(|d| d.message == expected)
            .unwrap_or_else(|| panic!("{name}: expected {expected:?}, got {:?}", failed.state.diags()));
        assert_eq!(diag.severity, Severity::Error, "{name}: the refusal is an error");
        let Some(Sel::Source { doc, start, end }) = diag.sel.first() else { panic!("{name}: no source selection: {diag:?}") };
        assert_eq!((*doc, *start as usize), (errors_doc, expected_start), "{name}: the refusal selects the offending occurrence");
        let text = &sources.doc(*doc).expect("the document exists").text;
        assert_eq!(&text[*start as usize..*end as usize], *selection, "{name}: the refusal selects exactly");
    }
}
