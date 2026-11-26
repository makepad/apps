//! Board inputs use design ids, board part indices and the panel's glass window.

use fab_elec_bench::bench::Supply;
use fab_elec_bench::input::{LowerInput, LowerNet, LowerPart, LowerPin, PartKind};
use fab_elec_bench::{lower_input, BoardInput, Firmware, ModelLibs, SimCmd, Touches, ValueIndex};
use fab_elec_core::{BoardId, CompId, CompRef, NetId, NetRef};
use fab_elec_mcu_parts::{PartKind as BusKind, SoundTrack, Stimulus, TrackRef};
use fab_elec_sim::live::LiveCmd;

fn comp(id: u32) -> CompRef { CompRef { board: BoardId(0), comp: CompId(id) } }
fn net(id: u32) -> NetRef { NetRef { board: BoardId(0), net: NetId(id) } }

fn index() -> ValueIndex {
    let libs = ModelLibs::generic();
    let pin = |name: &str, n| LowerPin { name: name.into(), net: Some(net(n)), nc: false };
    let mut parts = vec![LowerPart {
        id: comp(0), path: "mcu".into(), tol: None, noise: false,
        kind: PartKind::Firmware {
            chip: Some("ESP32-S3".into()), rails: vec![0, 1], crystal: Vec::new(),
            pads: libs.gpio_params(), c_pin: Some(5e-12), supply: None,
        },
        pins: vec![pin("VDD", 1), pin("GND", 0)],
    }];
    // Only the lowering's addresses are needed: model pins can be tied to their own rail.
    // Sparse component ids deliberately differ from the board's consecutive part indices.
    for (id, path, model, kind) in [
        (4, "panel", "CO5300", BusKind::Co5300 { glass: [16, 12, 8, 8] }),
        (9, "touch", "CST820", BusKind::Cst820 { addr: 0x15 }),
        (12, "imu", "QMI8658C", BusKind::Qmi8658c { addr: 0x6b }),
        (17, "codec", "ES8311", BusKind::Es8311 { addr: 0x18 }),
    ] {
        let mut pins: Vec<_> = kind.pins().iter().map(|name| pin(name, 1)).collect();
        let rails = vec![pins.len(), pins.len() + 1];
        pins.extend([pin("VDD", 1), pin("GND", 0)]);
        parts.push(LowerPart {
            id: comp(id), path: path.into(), tol: None, noise: false, pins,
            kind: PartKind::Bus { model: model.into(), kind, c_pin: Some(5e-12), rails },
        });
    }
    let input = LowerInput {
        nets: vec![
            LowerNet { id: net(0), name: "gnd".into(), supply: false, heard: false },
            LowerNet { id: net(1), name: "vdd".into(), supply: true, heard: false },
        ],
        ground: net(0), parts, sources: Vec::new(), probes: Vec::new(),
        supplies: vec![Supply { net: net(1), v: 3.3, limit: None, ramp: None }],
        firmware: vec![(Firmware { path: "mcu".into(), doc: "board.fw.splash".into(), params: Vec::new() }, Some(40e6))],
    };
    lower_input(&input, 7, &libs).index
}

fn check(cmd: Option<LiveCmd>, generation: u64, key: &str, expected: Stimulus, replaceable: bool, property: &str) {
    let Some(LiveCmd::Input { generation: actual, key: stream, input, replaceable: replaces }) = cmd else {
        panic!("{property}: a board input command must be produced");
    };
    assert_eq!(actual, generation, "{property}: the input belongs to this lowering");
    assert_eq!(stream, key, "{property}: the key names the board part's stream");
    assert_eq!(input.0.downcast_ref::<Stimulus>(), Some(&expected), "{property}: part and payload must match");
    assert_eq!(replaces, replaceable, "{property}: only a move or pose may replace a waiting input");
}

#[test]
fn sound_addresses_the_codec() {
    let index = index();
    let codec = comp(17);
    let track = SoundTrack::new(0, (16_000, 1));
    track.push(1234);
    track.close();
    let track = TrackRef(std::sync::Arc::new(track));
    assert_eq!(index.sound_target(codec), Some(3));
    let mut cmds = index.command(&SimCmd::Board(BoardInput::Sound { comp: codec, track: track.clone() }));
    assert_eq!(cmds.len(), 1);
    check(cmds.pop(), 7, "sound 3", Stimulus::Sound { part: 3, track: track.clone() }, false, "recording");
    assert!(index.command(&SimCmd::Board(BoardInput::Sound { comp: comp(12), track })).is_empty());
}

#[test]
fn fingers_end_at_the_glass_and_motion_addresses_the_imu() {
    let mut index = index();
    let panel = comp(4);
    let imu = comp(12);
    let mut touches = Touches::default();
    assert_eq!(index.touch_target(panel), Some((1, [16, 12, 8, 8])), "the panel targets the touch controller's board index");
    assert_eq!(index.motion_target(imu), Some(2), "the IMU targets its board index");
    let touch = |x, y, down| Stimulus::Touch { part: 1, x, y, down };
    for (x, y, down, replaceable, property) in [
        (16, 12, true, false, "first down"),
        (18, 15, true, true, "move"),
        (23, 19, false, false, "inside release"),
    ] {
        check(touches.command(&index, panel, x, y, down), 7, "touch 1",
            touch(x - 16, y - 12, down), replaceable, property);
    }
    assert!(touches.command(&index, panel, 18, 15, false).is_none(), "an up without a finger gives no input");
    assert!(touches.command(&index, panel, 15, 12, true).is_none(), "a touch starting left of the glass gives no input");
    assert!(touches.command(&index, panel, 24, 20, true).is_none(), "the right and bottom glass boundaries are exclusive");
    check(touches.command(&index, panel, 20, 16, true), 7, "touch 1", touch(4, 4, true), false, "drag begins");
    check(touches.command(&index, panel, u16::MAX, 0, true), 7, "touch 1", touch(7, 0, false), false, "drag leaves glass");
    assert!(touches.command(&index, panel, u16::MAX, 0, true).is_none(), "a drag off the glass ends only once");
    assert!(touches.command(&index, panel, 20, 16, false).is_none(), "returning with an up cannot revive the ended finger");
    check(touches.command(&index, panel, 16, 12, true), 7, "touch 1", touch(0, 0, true), false, "down before a value edit");
    index.generation = 8;
    check(touches.command(&index, panel, 18, 14, true), 8, "touch 1", touch(2, 2, true), true,
        "a finger held across a value edit moves under the new generation");
    check(touches.command(&index, panel, 17, 13, false), 8, "touch 1", touch(1, 1, false), false,
        "a finger held across a value edit releases under the new generation");
    check(touches.command(&index, panel, 17, 13, true), 8, "touch 1", touch(1, 1, true), false,
        "the down after that release starts a new finger");
    touches.clear();
    assert!(touches.command(&index, panel, 17, 13, false).is_none(), "clear forgets every finger");
    assert!(touches.command(&index, imu, 17, 13, true).is_none(), "a component without a touch target gives no input");
    assert!(index.command(&SimCmd::Board(BoardInput::Touch { comp: panel, x: 17, y: 13, down: true })).is_empty(),
        "stateless command mapping leaves touches to Touches");
    let accel_ug = [1_000_000, -500_000, 250_000];
    let gyro_mdps = [1000, -2000, 500];
    let mut cmds = index.command(&SimCmd::Board(BoardInput::Motion { comp: imu, accel_ug, gyro_mdps }));
    assert_eq!(cmds.len(), 1, "a pose produces exactly one command");
    check(cmds.pop(), 8, "motion 2", Stimulus::Motion { part: 2, accel_ug, gyro_mdps }, true, "pose");
    assert!(index.command(&SimCmd::Board(BoardInput::Motion { comp: panel, accel_ug, gyro_mdps })).is_empty(),
        "a component without an IMU target gives no motion input");
}
