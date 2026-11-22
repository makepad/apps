use fab_elec_mcu_rt::{analog, exec::{self, Observe, PortCall, Ports}, format::*, module, ports, pwm, serial, Profile};

const PROFILE: Profile = Profile {
    id: 0, pins: 0, max_entry_ops: 1000, call_ops: None, ram_words: 1024, buffer_bytes: 0,
    eeprom: 0, eeprom_write_ns: 0, f64: true, op_ns: 1, clock_hz: 1,
    pwm: &pwm::NONE, analog: &analog::NONE, serial: &serial::NONE,
    display: None, motion: None, touch: false, audio: None,
};

fn program() -> Vec<u32> {
    let mut body = vec![OP_CONST, 0, 7, OP_SET, 0, 0, OP_GET, 1, 0,
        OP_STORE | REGION_STATE << SUB_SHIFT, 0, 1, NONE, 1];
    let inner = [OP_GET, 3, 0, OP_CONST, 4, 1, OP_BIN | BIN_ADD_I << SUB_SHIFT, 5, 3, 4,
        OP_SET, 0, 5, OP_STORE | REGION_STATE << SUB_SHIFT, 1, 1, NONE, 5,
        OP_STORE | REGION_FRAME << SUB_SHIFT, 0, 1, NONE, 5];
    body.extend([OP_LOOP, 3, inner.len() as u32]); body.extend(inner);
    body.extend([OP_PORT, u32::from(ports::CLOCK_US), 0, 0, 1, 6,
        OP_STORE | REGION_STATE << SUB_SHIFT, 2, 1, NONE, 6,
        OP_CONST_D, 7, 0x89abcdef, 0x12345678, OP_SET, 1, 7, OP_GET, 8, 1]);
    let values = [TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_F64, TY_F64];
    let vars = [TY_I32, TY_F64];
    let mut code = vec![values.len() as u32, vars.len() as u32, 1, 1];
    for types in [values.as_slice(), vars.as_slice()] {
        for chunk in types.chunks(4) {
            let mut bytes = [0; 4]; bytes[..chunk.len()].copy_from_slice(chunk);
            code.push(u32::from_le_bytes(bytes));
        }
    }
    code.push(body.len() as u32); code.extend(body);
    let sections = [
        vec![3, 0, 1, 1, 1, 0, 0, 0], vec![0; 3], vec![],
        vec![5, u32::from_le_bytes(*b"setu"), b'p' as u32], vec![], vec![],
        vec![0, 0, code.len() as u32, 1000, 0], code,
    ];
    let mut words = vec![0; HEADER_WORDS + SECTION_COUNT * DIRECTORY_WORDS];
    words[..HEADER_WORDS].copy_from_slice(&[MAGIC, FORMAT_VERSION, ports::PORT_TABLE_VERSION, 0, 0, SECTION_COUNT as u32, 0, 0]);
    for (k, section) in sections.into_iter().enumerate() {
        let at = HEADER_WORDS + k * DIRECTORY_WORDS;
        let start = words.len() as u32;
        words[at..at + 3].copy_from_slice(&[k as u32 + 1, start, section.len() as u32]);
        words.extend(section);
    }
    words[H_WORDS] = words.len() as u32;
    words
}

#[derive(Debug, PartialEq, Eq)]
enum Event { Value(usize, u32, u64), Set(usize, u32, u64), Store(u32, u32, u32) }
#[derive(Default)]
struct Seen(Vec<Event>);
impl Observe for Seen {
    fn value(&mut self, e: usize, v: u32, bits: u64) { self.0.push(Event::Value(e, v, bits)); }
    fn set(&mut self, e: usize, v: u32, bits: u64) { self.0.push(Event::Set(e, v, bits)); }
    fn store(&mut self, r: u32, w: u32, bits: u32) { self.0.push(Event::Store(r, w, bits)); }
}
struct Clock(bool);
impl Ports for Clock {
    fn call(&mut self, port: u16, _: u32, _: u64, _: &[u32], rets: &mut [u32]) -> PortCall {
        assert_eq!(port, ports::CLOCK_US);
        if self.0 { PortCall::Suspend } else { rets[0] = 42; PortCall::Done }
    }
}

#[test]
fn observed_run_and_resume_preserve_bits_ops_and_noop_execution() {
    let words = program();
    let m = module::load(&words, &PROFILE).unwrap();
    let mut state = m.state_init().to_vec();
    let mut scratch = vec![0; m.scratch_words(0)];
    let plain = exec::run(&m, 0, &mut state, &mut scratch, &mut Clock(false));
    let mut observed_state = m.state_init().to_vec();
    let mut observed_scratch = vec![0xa5a5a5a5; scratch.len()];
    let mut seen = Seen::default();
    let observed = exec::run_observed(&m, 0, &mut observed_state, &mut observed_scratch, &mut Clock(false), &mut seen);
    assert_eq!(plain, observed);
    assert_eq!(observed_state, state);
    assert_eq!(observed_scratch, scratch, "observer does not affect any scratch word");
    assert_eq!(state, [7, 10, 42]);
    let values = seen.0.iter().filter(|e| matches!(e, Event::Value(..))).count();
    let sets = seen.0.iter().filter(|e| matches!(e, Event::Set(..))).count();
    let stores = seen.0.iter().filter(|e| matches!(e, Event::Store(..))).count();
    assert_eq!((values, sets, stores), (14, 5, 8), "each definition/write, including every loop iteration");
    assert_eq!(plain, exec::Exit::Done { ops: (values + sets + stores + 3) as u64 }, "only the three LOOP controls have no hook");
    assert!(seen.0.contains(&Event::Value(0, 8, 0x1234567889abcdef)), "f64 bits survive the variable round trip");
    let noop = exec::run_observed(&m, 0, &mut observed_state, &mut observed_scratch, &mut Clock(false), &mut ());
    assert_eq!(noop, plain);
    assert_eq!((observed_state.clone(), observed_scratch.clone()), (state.clone(), scratch));

    let mut resumed = Seen::default();
    assert!(matches!(exec::run_observed(&m, 0, &mut observed_state, &mut observed_scratch, &mut Clock(true), &mut resumed), exec::Exit::Suspended { .. }));
    assert!(!resumed.0.iter().any(|e| matches!(e, Event::Value(_, 6, _))), "suspension has not defined the result");
    let exit = exec::resume_observed(&m, &mut observed_state, &mut observed_scratch, &mut Clock(false), &[42], &mut resumed);
    assert_eq!(exit, plain);
    assert_eq!(observed_state, state);
    assert_eq!(resumed.0, seen.0, "resumption reports the result exactly once and preserves event order");
    let events = resumed.0.len();
    assert_eq!(exec::resume_observed(&m, &mut observed_state, &mut observed_scratch, &mut Clock(false), &[], &mut resumed), plain);
    assert_eq!(resumed.0.len(), events, "a completed run emits no further events");
}
