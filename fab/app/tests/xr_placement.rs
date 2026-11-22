//! The bed is set down in front of the wearer in every tracking space. The headset's origin moves
//! between its sessions and when its wearer recenters, and a bed left at its old place in the
//! space can end up behind them or inside a wall. The bed lands 0.6 m before the head along its
//! level forward, turned to face it, its top 0.45 m below the eyes:
//! - when the headset links;
//! - when it links again from a moved origin;
//! - when its wearer recenters.
//!
//! The test drives a synthetic headset on the native GPU through those three tracking spaces.

#[path = "common/wait.rs"]
mod idle_wait;
use idle_wait::wait_poll;
use makepad_test::{run_with_config, Role, Selector, TestApp, TestConfig};

/// A set-down the log told: the tracking space, the bed's centre and the head (room, m).
#[derive(Debug)]
struct SetDown {
    space: u32,
    bed: [f32; 3],
    head: [f32; 3],
}

/// The set-downs logged since `seq`, appended to `all`.
fn read_set_downs(app: &TestApp, seq: &mut u64, all: &mut Vec<SetDown>) -> Result<(), String> {
    let t = app.log_since(*seq).map_err(|e| format!("{e:?}"))?;
    *seq = t.last_seq;
    for l in &t.lines {
        let Some(r) = l.split("fab xr: the bed set down for tracking space ").nth(1) else { continue };
        let n: Vec<f32> = r.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).filter_map(|w| w.parse().ok()).collect();
        // space, bed x y z, turn, scale, head x y z
        assert_eq!(n.len(), 9, "a set-down line: {l}");
        all.push(SetDown { space: n[0] as u32, bed: [n[1], n[2], n[3]], head: [n[6], n[7], n[8]] });
    }
    Ok(())
}

/// Waits for the set-down of a tracking space after `after` and checks it against the head the
/// headset was given (`at`, turned `yaw` about y).
fn next_set_down(app: &TestApp, seq: &mut u64, all: &mut Vec<SetDown>, after: Option<u32>, at: [f32; 3], yaw: f32, what: &str) -> u32 {
    let d = wait_poll(app, || {
        read_set_downs(app, seq, all)?;
        all.iter().find(|d| after.is_none_or(|a| d.space > a)).map(|d| (d.space, d.bed, d.head)).ok_or_else(|| format!("{what}: waiting for the bed to be set down"))
    });
    let (space, bed, head) = d;
    for k in 0..3 {
        assert!((head[k] - at[k]).abs() < 0.002, "{what}: set down from a pose in the new space, the head at {at:?}: {head:?}");
    }
    let forward = [-yaw.sin(), -yaw.cos()];
    let (dx, dz) = (bed[0] - head[0], bed[2] - head[2]);
    let ahead = dx * forward[0] + dz * forward[1];
    let aside = dx * forward[1] - dz * forward[0];
    assert!((ahead - 0.6).abs() < 0.01 && aside.abs() < 0.01, "{what}: the bed 0.6 m before the head along its forward: {ahead:.3} m ahead, {aside:.3} m aside (bed {bed:?}, head {head:?})");
    let below = head[1] - bed[1];
    assert!((below - 0.45).abs() < 0.01, "{what}: the bed's top 0.45 m below the eyes: {below:.3} m (bed {bed:?}, head {head:?})");
    all.retain(|d| d.space != space);
    space
}

/// Hands the headset its words for the tracking space (whole: written aside, then moved in).
fn tell_headset(dir: &std::path::Path, words: &str) {
    let aside = dir.join("space.next");
    std::fs::write(&aside, words).unwrap();
    std::fs::rename(&aside, dir.join("space")).unwrap();
}

#[test]
fn the_bed_is_set_down_in_front_of_the_head_in_every_tracking_space() {
    let name = "the_bed_is_set_down_in_front_of_the_head_in_every_tracking_space";
    let dir = std::env::temp_dir().join(format!("fab-xr-placement-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), name).unwrap();
    for (k, v) in [("MAKEPAD_XR_REMOTE_SELFTEST", "1"), ("MAKEPAD_XR_REMOTE_SELFTEST_STILL", "1"), ("MAKEPAD_XR_STREAM", "on"), ("MAKEPAD_LAN", "1")] {
        config.env.insert(k.into(), v.into());
    }
    config.env.insert("MAKEPAD_XR_REMOTE_SELFTEST_SPACE_FILE".into(), dir.join("space").display().to_string());
    let r = run_with_config(config, |app| {
        app.locator(Selector::role(Role::Window)).wait_visible();
        let (mut seq, mut all) = (0, Vec::new());
        // linked: the head 1.6 m over the origin, looking along -z
        let first = next_set_down(&app, &mut seq, &mut all, None, [0.0, 1.6, 0.0], 0.0, "the first session");
        // linked again from an origin elsewhere: the head low and off to the side, turned
        tell_headset(&dir, "head=0.8,0.75,0.45,1.2 session");
        let second = next_set_down(&app, &mut seq, &mut all, Some(first), [0.8, 0.75, 0.45], 1.2, "a session from a moved origin");
        // the wearer recenters: the space moves again
        tell_headset(&dir, "head=-0.3,1.2,0.1,-0.5 recenter");
        next_set_down(&app, &mut seq, &mut all, Some(second), [-0.3, 1.2, 0.1], -0.5, "a recenter");
    });
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        panic!("{}", e.message());
    }
}

/// The app main thread's CPU budget per wait.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);
