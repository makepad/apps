//! Open a two-colour plate without hardware, map it to two tools, slice it in Fab and print it on
//! the virtual printer: Monitor follows the job to its end and the saved job says Finished.
mod common;
#[path = "common/simulated_profiles.rs"]
mod simulated_profiles;
#[path = "common/workspace_row.rs"]
mod workspace_row;

use makepad_strict_json::Value;
use makepad_test::{run_with_config, Locator, Role, Selector, TestApp, TestConfig};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Scroll the Printers editor until the named control or message is on screen.
fn scroll_to_printer_item(app: &TestApp, selector: Selector, down: bool) -> Locator {
    let panel = app.locator(Selector::role(Role::TabPanel).name("Printers")).snapshot().bounds;
    let query = selector.as_query();
    let item = app.locator(selector);
    let distance = f64::from(panel.h) * if down { 0.5 } else { -0.5 };
    app.wait_until(|| {
        if item.try_snapshot().is_ok() { return None; }
        app.scroll_at(f64::from(panel.x + panel.w * 0.5), f64::from(panel.y + panel.h * 0.5), 0.0, distance, None);
        Some(format!("scrolling the Printers editor to {query}"))
    });
    item
}

/// Read the same mapping that the printer panel's colour list shows.
fn printer(app: &TestApp) -> Value {
    let lines = app.call_tool(Duration::from_secs(30), "fab.printer", "{}");
    let reply = lines.iter().filter_map(|l| makepad_strict_json::parse_depth(l.as_bytes(), 64).ok())
        .find_map(|r| r.get("Result").cloned()).unwrap_or_else(|| panic!("fab.printer: {lines:?}"));
    let result = reply.as_arr().and_then(|a| a.first()).unwrap_or(&reply);
    makepad_strict_json::parse_depth(result.get("text").and_then(Value::as_str).unwrap().as_bytes(), 64).unwrap()
}

const SMALL_BLOCK: &str = "let block = m.box({size: [4mm 1.2mm 4mm] at: [0mm 0.6mm 0mm]})\nobject(block)";

fn two_colour_plate(dir: &std::path::Path, center: usize, block: &str) -> std::path::PathBuf {
    let plate = dir.join("blocks.fab.splash");
    std::fs::write(dir.join("block.cad.splash"), format!("{}{block}\n", fab_model::program::EMPTY)).unwrap();
    std::fs::write(&plate, format!(r#"use mod.fab.*
script("block.cad.splash")
color("White", rgb: #xf4f4f0)
color("Black", rgb: #x161616)
place("block", object: "block", at: [{center}mm {center}mm], name: "Light block", color: "White")
place("block", object: "block", link: 1, at: [{}mm {center}mm], name: "Dark block", color: "Black")
"#, center + 12)).unwrap();
    plate
}

/// Keep the letterboxed picture from the app's drawable, including its colour badge.
/// Count the light block inside the picture, clear of the badge and dock controls.
fn camera_picture(app: &TestApp, artifacts: &std::path::Path, name: &str) -> usize {
    use makepad_zune_png::{PngDecoder, PngEncoder, makepad_zune_core::{bytestream::ZCursor, options::EncoderOptions}};
    let camera = app.locator(Selector::role(Role::Image).name("Printer camera")).wait_visible().snapshot();
    assert_eq!(camera.value, "3D view of the job", "the capture shows this printer's job");
    let window = app.locator(Selector::role(Role::Window)).snapshot();
    let bytes = std::fs::read(app.screenshot()).unwrap();
    let mut decoder = PngDecoder::new(ZCursor::new(&bytes));
    let pixels = decoder.decode_raw().unwrap();
    let (w, h) = decoder.dimensions().unwrap();
    let colour = decoder.colorspace().unwrap();
    let channels = colour.num_components();
    let (sx, sy) = (w as f32 / window.bounds.w, h as f32 / window.bounds.h);
    let rect = camera.bounds;
    let height = rect.h.min(rect.w * 9.0 / 16.0);
    let width = height * 16.0 / 9.0;
    let x = rect.x - window.bounds.x + (rect.w - width) * 0.5;
    let y = rect.y - window.bounds.y + (rect.h - height) * 0.5;
    let (x0, x1) = ((x * sx).ceil() as usize, ((x + width) * sx).floor() as usize);
    let (y0, y1) = ((y * sy).ceil() as usize, ((y + height) * sy).floor() as usize);
    assert!(x0 < x1 && x1 <= w && y0 < y1 && y1 <= h, "the camera picture fits the app grab");
    let mut crop = Vec::new();
    let mut printed = 0;
    for row in y0..y1 {
        let line = &pixels[(row * w + x0) * channels..(row * w + x1) * channels];
        crop.extend_from_slice(line);
        if row > y0 + (y1 - y0) / 5 && row < y1 - (y1 - y0) / 5 {
            for p in line.chunks_exact(channels) {
                // White filament is light and neutral. The bed and background are dark,
                // and the two nozzle heads have saturated colours.
                let (r, g, b) = (p[0], p[1], p[2]);
                let low = r.min(g).min(b);
                printed += usize::from(low > 100 && r.max(g).max(b) - low < 25);
            }
        }
    }
    let options = EncoderOptions::default().set_width(x1 - x0).set_height(y1 - y0)
        .set_colorspace(colour).set_depth(makepad_zune_png::makepad_zune_core::bit_depth::BitDepth::Eight);
    let mut png = Vec::new();
    PngEncoder::new(&crop, options).encode(&mut png).unwrap();
    std::fs::write(artifacts.join(name), &png).unwrap();
    if let Some(keep) = std::env::var_os("DEVHUB_KEEP") {
        std::fs::write(std::path::Path::new(&keep).join(name), png).unwrap();
    }
    printed
}

#[test]
fn virtual_toolchanger_maps_two_colours_and_slices() {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), "virtual_toolchanger_maps_two_colours_and_slices").unwrap();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-virtual-plate-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    // Run-to-end stops for the embedded insert. That cut is stable until Resume.
    let plate = two_colour_plate(&dir, 100, r#"let block = m.box({size: [9mm 24mm 14mm] at: [0mm 12mm 0mm]})
object(block)
insert(m.box({size: [2mm 1mm 2mm] at: [0mm 5mm 0mm]}), name: "Pin", into: "block", embed: true)"#);
    let artifacts = config.artifacts_dir.clone();
    config.app_args = vec!["--open".into(), plate.display().to_string()];
    config.env.insert("FAB_PRINTER_VIRTUAL".into(), "four-tools".into());
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    let result = run_with_config(config, |app| {
        workspace_row::press(&app, "Monitor");
        app.locator(Selector::role(Role::DisclosureButton).name("Four-tool toolchanger (virtual)")).wait_visible();
        app.locator(Selector::all().name("Ready")).wait_visible();
        app.wait_until(|| {
            let p = printer(&app);
            let rows = p.get("colours").and_then(Value::as_arr).unwrap();
            let loaded = ["White", "Black"].iter().all(|name| rows.iter().any(|r|
                r.get("name").and_then(Value::as_str) == Some(*name) && r.get("used").and_then(Value::as_bool) == Some(true)));
            (!loaded).then(|| format!("waiting for both blocks on the plate: {rows:?}"))
        });
        app.locator(Selector::id("print")).click();
        app.wait_until(|| {
            let status = common::slice_state(&app);
            assert_ne!(status, "Failed", "virtual toolchanger slice");
            (status != "Sliced").then(|| format!("waiting for the two-colour slice: {status}"))
        });
        let p = printer(&app);
        assert_eq!(p.get("status").and_then(Value::as_str), Some("Connected"));
        let rows = p.get("colours").and_then(Value::as_arr).expect("printer colour list");
        for (name, tool, tray) in [("White", "tool_1", "A1"), ("Black", "tool_2", "B1")] {
            let row = rows.iter().find(|r| r.get("name").and_then(Value::as_str) == Some(name)).unwrap();
            assert_eq!(row.get("nozzle").and_then(Value::as_str), Some(tool), "{row:?}");
            assert_eq!(row.get("tray").and_then(Value::as_str), Some(tray), "{row:?}");
        }
        // Print it. The card of a virtual printer offers the speeds: To the end runs the job
        // through all its stages at once. Start print shows once the safety check has passed,
        // which can be after the slice reads Sliced.
        app.locator(Selector::role(Role::Button).name("Start print")).wait_visible().press();
        workspace_row::press(&app, "Monitor");
        app.locator(Selector::all().name("A virtual printer: this print is simulated. Speed")).wait_visible();
        app.locator(Selector::all().name("Speed to the end")).click();
        let resume = app.locator(Selector::role(Role::Button).name("Resume")).wait_visible();
        let mid = camera_picture(&app, &artifacts, "camera-mid.png");
        assert!(mid > 0, "the paused print has white filament pixels against the bed");
        resume.press();
        let jobs_dir = dir.join(".makepad/fab/jobs");
        app.wait_until(|| {
            // Print writes a job's folder before it is whole: read it once job.txt is there.
            let done = std::fs::read_dir(&jobs_dir).into_iter().flatten().flatten()
                .any(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().join("job.txt").is_file());
            if !done { return Some("waiting for Print to commit the job".to_string()); }
            let jobs = fab::print::jobs::read_all(&jobs_dir);
            let state = jobs.first().map(|(_, saved)| saved.state.clone());
            (state != Some(fab::print::jobs::JobState::Finished)).then(|| format!("waiting for the saved job to read Finished: {:?}", state.map(|s| s.label())))
        });
        let end = camera_picture(&app, &artifacts, "camera-end.png");
        assert!(end > mid, "the finished print shows more filament pixels: {end} vs {mid} at the insert");
        println!("virtual camera filament pixels: {mid} at the insert, {end} at the end");
    });
    std::fs::remove_dir_all(dir).unwrap();
    result.unwrap();
}

/// The two loaded spools are the reverse of the twin's default stock. Every other slot
/// explicitly reports empty, including the two external spools.
const TWO_SPOOLS: &str = r#"{"print":{"command":"push_status","msg":0,"gcode_state":"IDLE","sdcard":true,"device":{"nozzle":{"info":[{"id":0,"type":"HS01","diameter":0.4},{"id":1,"type":"HS01","diameter":0.4}]}},"ams":{"tray_exist_bits":"11","ams":[{"id":0,"info":"1003","tray":[{"id":0,"tray_type":"PLA","tray_color":"F4F4F0FF"},{"id":1},{"id":2},{"id":3}]},{"id":1,"info":"1103","tray":[{"id":0,"tray_type":"PLA","tray_color":"161616FF"},{"id":1},{"id":2},{"id":3}]}]},"vir_slot":[{"id":254,"tray_type":""},{"id":255,"tray_type":""}]}}"#;

fn entries(app: &TestApp) -> Value {
    app.command(Duration::from_secs(30), &["printer-entry-counts"], &[]).into_iter()
        .find_map(|s| makepad_strict_json::parse(s.as_bytes()).ok().filter(|v| v.get("counts").is_some()))
        .expect("the simulator test reads the app process's entry counters")
}

fn prepare_two_spools(app: &TestApp) -> Locator {
    app.wait_until(|| {
        let p = printer(app);
        let mapped = p.get("colours").and_then(Value::as_arr).is_some_and(|rows| {
            [("White", "A1"), ("Black", "B1")].iter().all(|(name, tray)| rows.iter().any(|r|
                r.get("used").and_then(Value::as_bool) == Some(true)
                && r.get("name").and_then(Value::as_str) == Some(*name)
                && r.get("tray").and_then(Value::as_str) == Some(*tray)))
        });
        (!mapped).then(|| format!("waiting for the two loaded spools: {:?}", p.get("colours")))
    });
    app.locator(Selector::id("print")).click();
    app.wait_until(|| {
        let status = common::slice_state(app);
        assert_ne!(status, "Failed", "the two-colour fixture must slice");
        (status != "Sliced").then(|| format!("waiting for slice: {status}"))
    });
    app.locator(Selector::role(Role::Button).name("Start print")).wait_visible()
}

#[test]
fn a_filament_twin_sends_the_same_package_without_entering_a_real_link() {
    use fab::print::jobs::{self, JobState};
    let name = "a_filament_twin_sends_the_same_package_without_entering_a_real_link";
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-twin-{}-{nonce}", std::process::id()));
    simulated_profiles::install(&dir);
    let plate = two_colour_plate(&dir, 40, SMALL_BLOCK);
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, format!("@capture-upload\n@upload 0\n@read-wait 5\n{TWO_SPOOLS}\n")).unwrap();
    config.app_args = vec!["--open".into(), plate.display().to_string()];
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
    config.env.insert("FAB_PRINTER_SIM_MODEL".into(), simulated_profiles::model());
    let result = run_with_config(config, |app| {
        workspace_row::press(&app, "Monitor");
        prepare_two_spools(&app).press();
        app.wait_until(|| (!fab_printer_link::sim::started_path(&sim).is_file()).then(|| "waiting for the simulated printer to receive the start command".to_string()));
        let sent = std::fs::read(sim.with_extension("uploaded")).expect("the bytes handed to simulated storage");
        let dir_jobs = dir.join(".makepad/fab/jobs");
        let (real, _) = jobs::read_all(&dir_jobs).pop().expect("committed simulated real job");
        assert!(!real.virtual_printer);
        assert!(sent.starts_with(b"PK"));
        let before = entries(&app);
        assert!(before.get("mqtt").and_then(Value::as_u64).unwrap() > 0, "real interval reached MQTT CONNECT");

        app.locator(Selector::role(Role::Button).name("Add printer")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name("Virtual twin of…")).wait_visible().press();
        app.locator(Selector::role(Role::MenuItem).name(simulated_profiles::MACHINE)).wait_visible().press();
        let quiet = || {
            let after = entries(&app);
            assert_eq!(after.get("start"), before.get("start"), "the twin entered PrinterLink::start");
            assert_eq!(after, before, "the twin entered a transport or credential boundary");
        };
        let title = format!("{} (virtual of Simulated printer)", simulated_profiles::MACHINE);
        app.wait_until(|| {
            quiet();
            (printer(&app).get("name").and_then(Value::as_str) != Some(title.as_str())).then(|| "waiting for the twin report".to_string())
        });
        app.locator(Selector::role(Role::DisclosureButton).name(&title)).wait_visible();
        let snapshot = printer(&app);
        assert_eq!(snapshot.get("name").and_then(Value::as_str), Some(title.as_str()));
        assert_eq!(snapshot.get("model").and_then(Value::as_str), Some(simulated_profiles::model().as_str()));
        prepare_two_spools(&app).press();
        app.locator(Selector::all().name("Speed to the end")).wait_visible().click();
        app.wait_until(|| {
            if std::fs::read_dir(&dir_jobs).unwrap().flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().join("job.txt").is_file()).count() != 2 {
                return Some("waiting for the twin to commit its own job".to_string());
            }
            let (meta, saved) = jobs::read_all(&dir_jobs).into_iter().find(|(j, _)| j.virtual_printer).expect("a virtual job on disk");
            assert_eq!(meta.machine_preset, simulated_profiles::MACHINE);
            assert_eq!(meta.printer_model, real.printer_model);
            assert!(meta.printer.as_deref().is_some_and(|p| p.starts_with("virtual:")));
            assert_eq!((&meta.trays, meta.flow_calibration, meta.timelapse), (&real.trays, real.flow_calibration, real.timelapse));
            assert!(!matches!(saved.state, JobState::Failed(_)), "twin job: {:?}", saved.state);
            if saved.state != JobState::Finished { return Some(format!("waiting for Finished: {:?}", saved.state)); }
            let virtual_bytes = jobs::read_package(&dir_jobs, &meta.id).unwrap();
            assert!(virtual_bytes == sent, "complete twin package differs from the simulated upload at byte {:?} ({} and {} bytes)",
                virtual_bytes.iter().zip(&sent).position(|(a, b)| a != b), virtual_bytes.len(), sent.len());
            None
        });
        quiet();
        let records = makepad_fab_settings::virtual_printer::load_from(&dir.join(".makepad/fab/virtual_printers.txt")).unwrap();
        let twin = records.printers.iter().find(|p| p.title == title).expect("the twin is saved");
        assert_eq!(records.picked, Some(format!("virtual:{}", twin.name)));
        assert_eq!(twin.machine_preset_name(), simulated_profiles::MACHINE);
    });
    std::fs::remove_dir_all(dir).unwrap();
    result.unwrap();
}

#[test]
fn configured_printer_starts_disconnected_and_only_connects_on_request() {
    use fab::print::jobs::{self, JobState};
    use makepad_fab_settings::virtual_printer::{self, Saved, Source};

    const CONFIG: &str = "Simulated printer";
    let test = "configured_printer_starts_disconnected_and_only_connects_on_request";
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), test).unwrap();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("fab-disconnected-{}-{nonce}", std::process::id()));
    simulated_profiles::install(&dir);
    let model = simulated_profiles::model();
    let mut saved = Saved::default();
    let mut twin = saved.printers.iter().find(|p| p.name == "dual-nozzle").unwrap().clone();
    twin.name = "configured-twin".into();
    twin.title = format!("{} (virtual)", simulated_profiles::MACHINE);
    twin.source = Source::Twin { preset: simulated_profiles::MACHINE.into(), model: model.clone(), source: Some(format!("printer:{CONFIG}")) };
    twin.configured_for = Some(CONFIG.into());
    for slot in &mut twin.slots {
        slot.colour = match slot.tray.as_str() { "A1" => Some(0xF4F4F0), "B1" => Some(0x161616), _ => None };
        slot.material = slot.colour.map(|_| "PLA".into());
    }
    let destination = format!("printer:{CONFIG}");
    saved.picked = Some(destination.clone());
    saved.disconnected.insert(CONFIG.into());
    saved.printers.push(twin);
    let prefs = dir.join(".makepad/fab/virtual_printers.txt");
    virtual_printer::save_to(&prefs, &saved).unwrap();
    let plate = two_colour_plate(&dir, 40, SMALL_BLOCK);
    let sim = dir.join("reports.jsonl");
    std::fs::write(&sim, format!("{TWO_SPOOLS}\n")).unwrap();
    config.app_args = vec!["--open".into(), plate.display().to_string()];
    config.env.insert("MAKEPAD_TEST_HOME".into(), dir.display().to_string());
    config.env.insert("FAB_PRINTER_SIM".into(), sim.display().to_string());
    config.env.insert("FAB_PRINTER_SIM_MODEL".into(), model.clone());
    config.env.insert("FAB_TEST_HOLD_PACKAGE".into(), "1".into());
    let result = run_with_config(config, |app| {
        let quiet = || {
            let counts = entries(&app);
            assert_eq!(counts.get("start").and_then(Value::as_u64), Some(0), "disconnected printer entered PrinterLink::start");
            assert_eq!(counts.get("mqtt").and_then(Value::as_u64), Some(0), "disconnected printer entered MQTT CONNECT");
            let zero = fab_printer_link::EntryCounts {
                start: 0, config_load: 0, config_load_file: 0, credential: 0, mqtt_connect: 0, mqtt_tls: 0,
                pinned_leaf: 0, storage_session: 0, ftps_login: 0, ftps_control_tls: 0, ftps_data_tls: 0,
                moonraker_http: 0, moonraker_websocket: 0, camera_open: 0, rtsp_tls: 0,
            };
            assert_eq!(counts.get("counts").and_then(Value::as_str).unwrap(), format!("{zero:?}"));
        };
        let jobs_dir = dir.join(".makepad/fab/jobs");
        let wait_job = |wanted: JobState, count: usize| {
            app.wait_until(|| {
                // A startup read cleans up partial writes. Wait until this commit is whole.
                let committed = std::fs::read_dir(&jobs_dir).into_iter().flatten().flatten().filter(|e|
                    !e.file_name().to_string_lossy().starts_with('.') && e.path().join("job.txt").is_file()).count();
                if committed < count { return Some("waiting for the package commit".into()); }
                let jobs = jobs::read_all(&jobs_dir);
                let Some((meta, saved)) = jobs.iter().find(|(_, s)| s.state == wanted) else {
                    return Some(format!("waiting for the saved job to be {}", wanted.label()));
                };
                assert_eq!(meta.printer.as_deref(), Some(destination.as_str()));
                assert!(meta.virtual_printer);
                assert_eq!(meta.machine_preset, simulated_profiles::MACHINE);
                assert_eq!(saved.state, wanted);
                None
            });
            quiet();
        };
        workspace_row::press(&app, "Monitor");
        // Both workers publish Ready. By then a wrongly constructed simulator entered MQTT.
        app.locator(Selector::all().name("Ready")).wait_visible();
        quiet();
        let snapshot = printer(&app);
        assert_eq!(snapshot.get("name").and_then(Value::as_str), Some(CONFIG));
        assert_eq!(snapshot.get("model").and_then(Value::as_str), Some(model.as_str()));
        assert_eq!(snapshot.get("virtual_printer").and_then(Value::as_bool), Some(true));
        assert_eq!(snapshot.get("serial_suffix"), Some(&Value::Null));
        app.locator(Selector::role(Role::DisclosureButton).name(format!("{CONFIG} (Virtual)"))).wait_visible();
        scroll_to_printer_item(&app, Selector::all().name("Virtual mode"), true);
        scroll_to_printer_item(&app, Selector::role(Role::Button).name("Storage"), false).press();
        app.wait_until(|| {
            let status = printer(&app);
            let error = status.get("storage_files").and_then(|s| s.get("error")).and_then(Value::as_str);
            (error != Some("a virtual printer keeps no files")).then(|| "waiting for virtual Check Storage".into())
        });
        quiet();

        let start = prepare_two_spools(&app);
        app.locator(Selector::id("speed_row.speed_end").name("Speed to the end")).wait_visible().click();
        start.press();
        scroll_to_printer_item(&app, Selector::all().name("Connected mode"), true).press();
        scroll_to_printer_item(&app, Selector::all().name("A job is being prepared for this printer. Wait before switching modes."), false);
        quiet();
        app.command(Duration::from_secs(30), &["printer-release-packages"], &[]);
        wait_job(JobState::Finished, 1);
        app.locator(Selector::id("virtual_row.speed_end")).wait_checked(true);
        app.locator(Selector::all().name("Speed 1×")).click();

        app.locator(Selector::id("print")).click();
        app.locator(Selector::role(Role::CheckBox).name("The plate is clear: the last print is off the bed")).wait_visible().check(true);
        app.locator(Selector::role(Role::Button).name("Start print")).wait_visible().press();
        let gcode = || printer(&app).get("gcode_state").and_then(Value::as_str).unwrap_or("").to_string();
        app.wait_until(|| (gcode() != "RUNNING").then(|| "waiting for the second virtual print".into()));
        app.locator(Selector::role(Role::Button).name("Pause")).wait_visible().press();
        app.wait_until(|| (gcode() != "PAUSE").then(|| "waiting for Pause".into()));
        quiet();
        app.locator(Selector::role(Role::Button).name("Resume")).wait_visible().press();
        app.wait_until(|| (gcode() != "RUNNING").then(|| "waiting for Resume".into()));
        app.locator(Selector::role(Role::Button).name("Stop…")).wait_visible().press();
        app.locator(Selector::role(Role::Button).name("Stop print")).wait_visible().press();
        wait_job(JobState::Stopped, 2);

        scroll_to_printer_item(&app, Selector::all().name("Connected mode"), true).press();
        app.wait_until(|| (entries(&app).get("mqtt").and_then(Value::as_u64).unwrap() == 0)
            .then(|| "waiting for the Connected simulator's MQTT CONNECT".into()));
        app.wait_until(|| {
            let saved = virtual_printer::load_from(&prefs).unwrap();
            (saved.disconnected.contains(CONFIG) || saved.twin_for(CONFIG).is_none() || saved.picked.as_ref() != Some(&destination))
                .then(|| "waiting for Connected mode with its own twin retained".into())
        });
    });
    std::fs::remove_dir_all(dir).unwrap();
    result.unwrap();
}
