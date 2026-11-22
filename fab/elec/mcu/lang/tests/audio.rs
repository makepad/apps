use fab_elec_mcu::{compile, encode, Compiled, Profile, FORMS, PORTS};
use fab_elec_mcu_rt::{self as rt, module, ports, AudioProfile};
use makepad_script_compute::{ir::{Elem, PortBuf}, lower::event::DeclArg};

const AUDIO: AudioProfile = AudioProfile { rates: &[16_000, 48_000], mic: true };
const P: Profile = Profile {
    id: 0, pins: 0, max_entry_ops: 50_000, call_ops: None, ram_words: 1 << 16, buffer_bytes: 4096,
    eeprom: 0, eeprom_write_ns: 0, f64: false, op_ns: 5, clock_hz: 160_000_000,
    pwm: &rt::pwm::NONE, analog: &rt::analog::NONE, serial: &rt::serial::NONE,
    display: None, motion: None, touch: false, audio: Some(AUDIO),
};
const DUPLEX: &str = "let tx = speaker(16000, 20, fn() {})\nlet rx = mic(16000, 20, fn() {})";

fn compiled(source: &str) -> Compiled { compile(source, &P, &[], &[]).expect("valid audio source") }

fn refuses(source: &str, profile: &Profile, why: &str) {
    let errors = compile(source, profile, &[], &[]).err().expect("invalid audio source");
    assert!(errors[0].message.contains(why), "{source}: expected {why}, got {}", errors[0].message);
}

#[test]
fn audio_numbers_and_buffer_signatures() {
    assert_eq!((ports::FORM_SPEAKER, ports::FORM_MIC, ports::FORMS, ports::PORT_TABLE_VERSION), (17, 18, 19, 15));
    assert_eq!([FORMS[17].name, FORMS[18].name], ["speaker", "mic"]);
    let numbers = [ports::PLAY, ports::SPEAKER_FREE, ports::SPEAKER_QUEUED, ports::SPEAKER_ON,
        ports::RECORD, ports::MIC_READY, ports::MIC_REWIND];
    assert_eq!(numbers, [40, 41, 42, 43, 44, 45, 46]);
    assert_eq!(numbers.map(|p| PORTS[p as usize].name),
        ["play", "speaker_free", "speaker_queued", "speaker_on", "record", "mic_ready", "mic_rewind"]);
    for (port, writes) in [(ports::PLAY, false), (ports::RECORD, true)] {
        assert_eq!(PORTS[port as usize].bufs, &[PortBuf { param: 0, elem: Some(Elem::I16), writes }]);
    }
}

#[test]
fn duplex_ports_and_inclusive_duration_bounds() {
    for (queue, history) in [(20, 20), (25, 40), (200, 640)] {
        let source = format!("let buf = samples(8)
            let rx = mic(48000, {history}, fn() {{ record(buf, 1, 7) }})
            let tx = speaker(48000, {queue}, fn() {{ play(buf, 0, 8) }})
            fn setup() {{
                speaker_on(1)
                probe(1, speaker_free())
                probe(2, speaker_queued())
                probe(3, mic_ready())
                probe(4, mic_rewind(0))
                play(buf, 8, 0)
                speaker_on(0)
            }}");
        compiled(&source);
    }
}

#[test]
fn profile_must_supply_each_direction() {
    for name in ["speaker", "mic"] {
        let src = format!("let path = {name}(16000, 20, fn() {{}})");
        let bare = Profile { audio: None, ..P };
        let msg = format!("no {name} on this chip");
        refuses(&src, &bare, &msg);
        assert_eq!(module::load(&compiled(&src).module, &bare).unwrap_err().to_string(), msg);
    }
    let playback = Profile { audio: Some(AudioProfile { mic: false, ..AUDIO }), ..P };
    compile("let tx = speaker(16000, 20, fn() {})", &playback, &[], &[]).unwrap();
    refuses(DUPLEX, &playback, "no mic input");
}

#[test]
fn rates_must_be_reachable_and_equal_in_either_order() {
    for (first, second) in [("speaker", "mic"), ("mic", "speaker")] {
        refuses(&format!("let a = {first}(16000, 20, fn() {{}})\nlet b = {second}(48000, 20, fn() {{}})"),
            &P, "must use the same rate, got 16000 and 48000");
        refuses(&format!("let a = {first}(12345, 20, fn() {{}})"), &P, "12345 Hz on this board. Supported rates in Hz are [16000, 48000]");
    }
}

#[test]
fn durations_have_a_minimum_maximum_and_granularity() {
    for (form, values, reason) in [("speaker", [15, 22, 205], "queue_ms must be 20..=200 ms in multiples of 5"),
        ("mic", [0, 30, 660], "history_ms must be 20..=640 ms in multiples of 20")] {
        for ms in values { refuses(&format!("let a = {form}(16000, {ms}, fn() {{}})"), &P, reason); }
    }
}

#[test]
fn each_direction_has_one_declaration_with_two_integers_and_a_handler() {
    for form in ["speaker", "mic"] {
        let call = format!("{form}(16000, 20, fn() {{}})");
        refuses(&format!("let a = {call}\nlet b = {call}"), &P, &format!("at most one {form}"));
        for args in ["16000, 20", "16000, fn() {}", "16000, 20, 0", "16000, 20, 5, fn() {}"] {
            refuses(&format!("let a = {form}({args})\nfn setup() {{}}"), &P, "needs (integer rate_hz");
        }
    }
}

#[test]
fn each_port_needs_its_own_declaration() {
    for (call, needs, other) in [
        ("play(b, 0, 1)", "speaker", "mic"), ("speaker_free()", "speaker", "mic"),
        ("speaker_queued()", "speaker", "mic"), ("speaker_on(1)", "speaker", "mic"),
        ("record(b, 0, 1)", "mic", "speaker"), ("mic_ready()", "mic", "speaker"),
        ("mic_rewind(0)", "mic", "speaker"),
    ] {
        let source = format!("let b = samples(1)\nlet a = {other}(16000, 20, fn() {{}})\nfn setup() {{ {call} }}");
        refuses(&source, &P, &format!("needs a {needs} declaration"));
    }
}

#[test]
fn sample_ports_check_buffer_kind_handle_and_extent() {
    for port in ["play", "record"] {
        for (args, reason) in [("bytes_buf, 0, 1", "expected I16"), ("0, 0, 1", "must be a buffer name"),
            ("b, -1, 1", "sample range"), ("b, 0, -1", "sample range"), ("b, 1, 2", "sample range")] {
            refuses(&format!("{DUPLEX}\nlet bytes_buf = bytes(2)\nlet b = samples(2)\nfn setup() {{ {port}({args}) }}"), &P, reason);
        }
    }
    for (call, reason) in [("speaker_on(2)", "level 0 or 1"), ("mic_rewind(-1)", "nonnegative sample count"),
        ("mic_ready(0)", "takes 0 arguments")] {
        refuses(&format!("{DUPLEX}\nfn setup() {{ {call} }}"), &P, reason);
    }
}

#[test]
fn loader_checks_encoded_declarations_and_handler_bindings() {
    let mut c = compiled(DUPLEX);
    for (param, value, why) in [(0, 48000, "same rate"), (0, 1, "Supported rates"), (1, 21, "multiples of 20")] {
        let saved = c.lowered.decls[1].args[param];
        c.lowered.decls[1].args[param] = DeclArg::Int(value);
        let words = encode::encode(&c.lowered, P.id).unwrap();
        assert!(module::load(&words, &P).unwrap_err().to_string().contains(why));
        c.lowered.decls[1].args[param] = saved;
    }
    let tx_entry = c.lowered.decls[0].args[2];
    c.lowered.decls[1].args[2] = tx_entry;
    let words = encode::encode(&c.lowered, P.id).unwrap();
    assert!(matches!(module::load(&words, &P), Err(module::LoadError::DuplicateEntry { decl: 1, .. })));
}

#[test]
fn loader_checks_buffer_kind_in_encoded_audio_calls() {
    for port in ["play", "record"] {
        let mut c = compiled(&format!("let b = samples(1)\n{DUPLEX}\nfn setup() {{ {port}(b, 0, 1) }}"));
        c.lowered.decls[0].form = ports::FORM_BYTES as u16;
        c.lowered.buffers[0].elem = Elem::U8;
        let words = encode::encode(&c.lowered, P.id).unwrap();
        assert!(matches!(module::load(&words, &P), Err(module::LoadError::BufferArgument { param: 0, .. })));
    }
}
