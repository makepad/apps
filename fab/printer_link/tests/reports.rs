use fab_printer_link::model::*;
use fab_printer_link::{ConnectionStatus, Snapshot};
use std::sync::Arc;

fn report() -> PrinterState { Reports::default().apply(include_bytes!("fictional-report.json")).unwrap().unwrap() }

#[test]
fn fictional_report_and_delta_preserve_objects_replace_arrays() {
    let mut reports = Reports::default();
    let full = reports.apply(include_bytes!("fictional-report.json")).unwrap().unwrap();
    assert_eq!(full.progress, Some(37.0));
    assert_eq!((full.layer, full.total_layers), (Some(12), Some(80)));
    assert_eq!(full.bed, Temperature { current: Some(55.5), target: Some(60.0) });
    assert_eq!(full.chamber, Temperature { current: Some(30.5), target: None });
    assert_eq!(full.extruders[0].temperature, Temperature { current: Some(200.0), target: Some(220.0) });
    assert_eq!(full.extruders[1].temperature, Temperature { current: Some(190.0), target: Some(210.0) });
    assert_eq!(full.extruders[1].loaded, Some(LoadedSlot::Tray(TrayId { ams: 1, slot: 0 })));
    assert_eq!(full.extruders[1].target, Some(LoadedSlot::NoneOrExternal));
    assert_eq!(full.ams[0].extruder.and_then(CloudNozzleId::side), Some(NozzleSide::Right));
    assert_eq!(full.ams[1].extruder.and_then(CloudNozzleId::side), Some(NozzleSide::Left));
    assert_eq!(full.ams[0].kind, Some(3));
    assert_eq!(full.ams[0].trays[2].present, Some(false));
    assert_eq!(full.ams[0].trays[0].nozzle_temp_min, Some(190.0));
    assert_eq!(full.ams[1].trays[0].colour.unwrap().0, [0.0, 1.0, 0.0, 128.0 / 255.0]);
    assert_eq!(full.external[0].id, Some(TrayId { ams: 254, slot: 254 }));
    assert_eq!(full.external[0].remain, None);
    assert_eq!(full.nozzles[0].diameter, Some(0.4));
    assert_eq!(full.hms[0].code(), "HMS_0102_0304_0003_0007");
    assert_eq!(full.print_error, Some(1234));
    let delta = reports.apply(br#"{"print":{"msg":1,"mc_percent":38,"device":{"nozzle":{"info":[{"id":1,"diameter":"0.8"}]}},"hms":[]}}"#).unwrap().unwrap();
    assert_eq!(delta.progress, Some(38.0));
    assert_eq!(delta.extruders, full.extruders);
    assert_eq!(delta.ams, full.ams);
    assert_eq!(delta.nozzles.len(), 1);
    assert_eq!(delta.nozzles[0].diameter, Some(0.8));
    assert!(delta.hms.is_empty());
    assert!(reports.apply(br#"{"info":{"command":"get_version"}}"#).unwrap().is_none());
    let full = reports.apply(br#"{"print":{"command":"push_status","gcode_state":"IDLE"}}"#).unwrap().unwrap();
    assert!(full.ams.is_empty());
    assert_eq!(full.progress, None);
    assert!(Reports::default().apply(br#"{"print":{"msg":1}}"#).is_err());
}

#[test]
fn stages_have_names_and_an_unknown_one_says_its_number() {
    use fab_printer_link::model::stage_name;
    assert_eq!(stage_name(29).as_deref(), Some("Cooling the chamber"));
    assert_eq!(stage_name(39).as_deref(), Some("Nozzle offset calibration"));
    assert_eq!(stage_name(99).as_deref(), Some("Stage 99"));
}

#[test]
fn numbering_colour_conversion_and_lenient_missing_fields() {
    assert_eq!(CloudNozzleId(0).side(), SlicerNozzleId(2).side());
    assert_eq!(PreviewNozzleId(0).side(), SlicerNozzleId(1).side());
    assert_eq!(SlicerNozzleId(0).side(), None);
    let c = Colour::parse("80808080").unwrap();
    assert!((c.linear()[0] - 0.21586).abs() < 0.0001);
    assert_eq!(c.linear()[3], 128.0 / 255.0);
    assert!(Colour::parse("bad").is_none());
    let state = Reports::default().apply(br#"{"print":{"mc_percent":"unknown","device":{"extruder":{"info":[{}]}}}}"#).unwrap().unwrap();
    assert_eq!(state.progress, None);
    assert_eq!(state.extruders[0], Extruder::default());
}

#[test]
fn inspection_has_only_typed_telemetry_and_a_serial_suffix() {
    let snapshot = Snapshot { name: "Test printer".into(), model: "Test model".into(), serial_suffix: Some("0001".into()), status: ConnectionStatus::Connected, state: Arc::new(report()), ..Default::default() };
    let text = snapshot.inspect(makepad_strict_json::Value::Arr(Vec::new())).to_json();
    assert!(text.contains("0001"));
    for private in ["TESTSERIAL0001", "access_code", "host", "unrecognised", "raw"] { assert!(!text.contains(private)); }
    assert!(text.contains("Left") && text.contains("Right"));
}

#[test]
fn every_stat_the_report_carries_is_published_and_nothing_private() {
    let report = r#"{"print":{"msg":0,"gcode_state":"RUNNING",
        "cooling_fan_speed":"10","heatbreak_fan_speed":"15","big_fan1_speed":"0","big_fan2_speed":"5",
        "spd_lvl":2,"spd_mag":100,"wifi_signal":"-52dBm","fail_reason":"0","mc_print_error_code":"0","err":"0300-8010",
        "upgrade_state":{"status":"IDLE","sn":"FICTIONALSERIAL","new_version_state":2},
        "device":{"extruder":{"state":17,"info":[]},"fire_ext":{"state":1}},
        "lights_report":[{"node":"chamber_light","mode":"on"}],
        "xcam":{"first_layer_inspector":true},
        "net":{"info":[{"ip":4076863498,"mask":16777215}]},
        "ipcam":{"rtsp_url":"rtsps://192.0.2.7:322/streaming/live/1","resolution":"1080p"},
        "ams":{"ams":[{"id":"0","tray":[{"id":"0","tag_uid":"FICTIONALTAG","tray_uuid":"FICTIONALUUID","tray_weight":"1000"}]}]},
        "dev_id":"FICTIONALDEV","access_code":"FICTIONALCODE","user_id":"FICTIONALUSER","project_id":"FICTIONALPROJECT",
        "odd":"rtsps://192.0.2.7/x","odder":"192.0.2.7","mail":"someone@example.com"}}"#;
    let s = Reports::default().apply(report.as_bytes()).unwrap().unwrap();
    assert_eq!((s.fans.part, s.fans.heatbreak, s.fans.aux, s.fans.chamber), (Some(10), Some(15), Some(0), Some(5)));
    assert_eq!((s.speed_level, s.speed_percent), (Some(2), Some(100.0)));
    assert_eq!(s.wifi.as_deref(), Some("-52dBm"));
    assert_eq!(s.firmware.as_deref(), Some("IDLE"));
    assert_eq!(s.active_extruder.and_then(CloudNozzleId::side), Some(NozzleSide::Left), "bits 4-7 of 17 are 1");
    assert_eq!(s.errors, vec![("err".to_string(), "0300-8010".to_string())], "zeros are no errors");
    let detail = |path: &str| s.details.iter().find(|(k, _)| k == path).map(|(_, v)| v.as_str());
    // What Fab has no name for stays visible.
    assert_eq!(detail("device.fire_ext.state"), Some("1"));
    assert_eq!(detail("lights_report.0.mode"), Some("on"));
    assert_eq!(detail("xcam.first_layer_inspector"), Some("true"));
    assert_eq!(detail("ams.ams.0.tray.0.tray_weight"), Some("1000"));
    assert_eq!(detail("upgrade_state.new_version_state"), Some("2"));
    // Who and where the printer is does not.
    let all = format!("{:?}", s.details);
    for private in ["FICTIONAL", "192.0.2.7", "4076863498", "rtsps", "someone", "resolution"] {
        assert!(!all.contains(private), "{private} in {all}");
    }
}

/// What an answer says, whatever shape it comes in. No answer of a real printer is recorded here,
/// so none of these shapes may read as success (or as an error) by accident.
#[test]
fn an_answer_counts_for_what_it_says() {
    let ok = |fields: &str| {
        let json = format!(r#"{{"print":{{"command":"stop","sequence_id":"70002"{fields}}}}}"#);
        let a = ack(json.as_bytes()).expect("an answer");
        assert_eq!((a.command.as_str(), a.sequence), ("stop", 70002));
        (a.ok, a.reason)
    };
    let said = |s: &str| Some(s.to_string());
    assert_eq!(ok(r#","result":"success""#), (Some(true), None));
    assert_eq!(ok(r#","result":"SUCCESS","reason":"SUCCESS","err_code":0"#), (Some(true), None));
    assert_eq!(ok(r#","result":"fail""#), (Some(false), None));
    assert_eq!(ok(r#","result":"FAILED","reason":"SUCCESS""#), (Some(false), None));
    assert_eq!(ok(r#","result":"failed","reason":"verify failed","err_code":84033543"#), (Some(false), said("verify failed")));
    assert_eq!(ok(r#","result":"failed","err_code":84033543"#), (Some(false), said("84033543")));
    assert_eq!(ok(r#","result":"success","err_code":42"#), (Some(false), said("42")));
    // No result at all.
    assert_eq!(ok(r#","err_code":42"#), (Some(false), said("42")));
    assert_eq!(ok(r#","errno":"7""#), (Some(false), said("7")));
    assert_eq!(ok(r#","fail_reason":"no plate""#), (Some(false), said("no plate")));
    assert_eq!(ok(r#","reason":"no plate""#), (Some(false), said("no plate")));
    assert_eq!(ok(r#","reason":"success""#), (Some(true), None));
    assert_eq!(ok(r#","reason":"bad\u0000 thing""#).1, said("bad thing"), "no control characters reach logs or the UI");
    // An echo says neither; zero in any form is no error.
    assert_eq!(ok(""), (None, None));
    assert_eq!(ok(r#","param":"","err_code":0,"errno":"0","fail_reason":"0","reason":"""#), (None, None));
}

#[test]
fn an_answer_shows_its_shape_and_keeps_its_contents() {
    use fab_printer_link::model::answer_shape;
    let shape = |json: &str| answer_shape(json.as_bytes(), |s| s.replace("TESTCODE", "***").replace("TESTSERIAL0001", "***"));
    // Status reports are no answers, in whichever form they come.
    assert_eq!(shape(r#"{"print":{"command":"push_status","msg":0,"sequence_id":"7","gcode_state":"IDLE"}}"#), None);
    assert_eq!(shape(r#"{"print":{"gcode_state":"IDLE"}}"#), None);
    assert_eq!(shape("not json"), None);
    assert_eq!(shape("[1]"), None);
    // The verdict's fields show their values and how they are written; the rest name and kind.
    assert_eq!(
        shape(r#"{"print":{"command":"stop","sequence_id":"812345678","result":"fail","reason":"busy","err_code":42,"param":"","flag":true,"nothing":null,"ratio":0.5,"list":[1,2]}}"#).unwrap(),
        r#"print{command="stop" sequence_id="812345678" result="fail" reason="busy" err_code=42 param:str flag:bool nothing:null ratio:float list:arr[2]}"#,
    );
    assert_eq!(shape(r#"{"print":{"command":"stop","sequence_id":812345678}}"#).unwrap(), r#"print{command="stop" sequence_id=812345678}"#);
    // One without a command, one under another module, one nested.
    assert_eq!(shape(r#"{"print":{"sequence_id":"1","result":"success"}}"#).unwrap(), r#"print{sequence_id="1" result="success"}"#);
    assert_eq!(
        shape(r#"{"info":{"command":"get_version","sequence_id":"2","module":[{"sn":"TESTSERIAL0001"}]}}"#).unwrap(),
        r#"info{command="get_version" sequence_id="2" module:arr[1]}"#,
    );
    assert_eq!(shape(r#"{"print":{"command":"stop","data":{"result":"fail","url":"x"}}}"#).unwrap(), r#"print{command="stop" data{result="fail" url:str}}"#);
    // Nothing else a message carries reaches the log, and neither does a secret in what does.
    let line = shape(r#"{"system":{"command":"get_access_code","access_code":"TESTCODE","url":"ftp://TESTCODE@printer","file":"my secret plan.3mf"}}"#).unwrap();
    assert_eq!(line, r#"system{command="get_access_code" access_code:str url:str file:str}"#);
    assert_eq!(shape(r#"{"print":{"command":"stop","reason":"bad \u0054ESTCODE on TESTSERIAL0001"}}"#).unwrap(), r#"print{command="stop" reason="bad *** on ***"}"#);
    assert_eq!(shape(r#"{"print":{"command":"stop","reason":"bad TEST\nCODE"}}"#).unwrap(), r#"print{command="stop" reason="bad ***"}"#, "a control character does not hide it");
    assert_eq!(shape(r#"{"print":{"command":"stop","TESTCODE":"x"}}"#).unwrap(), r#"print{command="stop" ***:str}"#);
    assert_eq!(shape(r#"{"print":{"command":"stop","err_code":12345678}}"#).map(|l| l.contains("12345678")), Some(true), "a number that is no secret stays");
    // One line, however long.
    let long = format!(r#"{{"print":{{"command":"stop","reason":"{}"}}}}"#, "x".repeat(1000));
    assert!(shape(&long).unwrap().len() < 200);
}
