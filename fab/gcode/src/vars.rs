//! The variables the printer templates read (R2 §4.3): whole-print globals
//! for the start template and the per-call variables of the layer,
//! timelapse, filament-change and end templates.

use makepad_fab_settings::machine::Job;
use makepad_fab_settings::template::{Env, TVar};

/// Facts about the sliced plate the start template needs.
#[derive(Clone, Debug, Default)]
pub struct PlateFacts {
    pub total_layers: usize,
    pub max_print_z: f64,
    pub first_layer_min: [f64; 2],
    pub first_layer_max: [f64; 2],
    pub first_layer_area: f64,
    /// Used filaments (0-based) in first-use order.
    pub used: Vec<usize>,
    pub has_wipe_tower: bool,
    pub wipe_tower_center: [f64; 2],
    pub total_toolchanges: usize,
}

fn vecf(job: &Job, key: &str) -> Vec<f64> {
    (0..job.filaments()).map(|f| job.fil(key, f)).collect()
}

/// Per-filament values where 0 means "use another key's value".
fn vec_or(job: &Job, key: &str, fallback: &str) -> Vec<f64> {
    (0..job.filaments())
        .map(|f| {
            let v = job.fil(key, f);
            if v == 0.0 {
                job.fil(fallback, f)
            } else {
                v
            }
        })
        .collect()
}

pub fn set_start_vars(env: &mut Env, job: &Job, family: &makepad_fab_settings::family::Family, p: &PlateFacts) {
    let f0 = p.used.first().copied().unwrap_or(0);
    let e0 = job.ext_of(f0);
    let ne = job.extruders();
    let i = |v: i64| TVar::int(v);
    for k in ["initial_tool", "initial_extruder", "current_extruder", "initial_filament_id", "initial_no_support_tool", "initial_no_support_extruder", "initial_no_support_filament_id"] {
        env.set(k, i(f0 as i64));
    }
    for k in ["initial_extruder_id", "initial_nozzle_id", "initial_no_support_extruder_id", "initial_no_support_nozzle_id"] {
        env.set(k, i(e0 as i64));
    }
    env.set("current_hotend", i(-1));
    env.set("initial_no_support_hotend", i(-1));
    // Per physical extruder: the first filament it prints.
    let mut first_phys = vec![-1i64; ne.max(2)];
    for &f in &p.used {
        let ph = job.physical_index(job.ext_of(f)).min(first_phys.len() - 1);
        if first_phys[ph] < 0 {
            first_phys[ph] = f as i64;
        }
    }
    for k in ["first_tools", "first_filaments", "first_non_support_tools", "first_non_support_filaments"] {
        env.set(k, TVar::ints(&first_phys));
    }
    env.set("first_non_support_hotend", TVar::ints(&vec![-1; first_phys.len()]));
    let nd: Vec<f64> = (0..ne).map(|e| job.nozzle_diameter(e)).collect();
    env.set("nozzle_diameter_at_nozzle_id", TVar::f64s(&nd));
    let nvt: Vec<String> = (0..ne).map(|e| job.cfg.raw("nozzle_volume_type", e).to_string()).collect();
    env.set("nozzle_volume_types", TVar::strings(&nvt));
    env.set("retraction_distance_when_cut", TVar::f64(job.fil("retraction_distances_when_cut", f0)));
    env.set("long_retraction_when_cut", TVar::boolean(job.cfg.bool_at("long_retractions_when_cut", e0)));
    env.set("retraction_distance_when_ec", TVar::f64(job.fil("retraction_distances_when_ec", f0)));
    env.set("long_retraction_when_ec", TVar::boolean(job.cfg.bool_at("long_retractions_when_ec", f0)));
    let max_aux = p.used.iter().map(|&f| job.fil("additional_cooling_fan_speed", f)).fold(0.0, f64::max);
    env.set("max_additional_fan", TVar::f64(max_aux));
    env.set("total_layer_count", i(p.total_layers as i64));
    env.set("current_object_idx", i(0));
    env.set("has_wipe_tower", TVar::boolean(p.has_wipe_tower));
    env.set("has_wipe_tower_this_layer", TVar::boolean(p.has_wipe_tower));
    env.set("total_toolchanges", i(p.total_toolchanges as i64));
    let bed = job.cfg.points("printable_area");
    let (mut bmin, mut bmax) = ([f64::MAX; 2], [f64::MIN; 2]);
    for (x, y) in &bed {
        bmin[0] = bmin[0].min(*x);
        bmin[1] = bmin[1].min(*y);
        bmax[0] = bmax[0].max(*x);
        bmax[1] = bmax[1].max(*y);
    }
    env.set("print_bed_min", TVar::f64s(&bmin));
    env.set("print_bed_max", TVar::f64s(&bmax));
    env.set("print_bed_size", TVar::f64s(&[bmax[0] - bmin[0], bmax[1] - bmin[1]]));
    env.set("first_layer_print_min", TVar::f64s(&p.first_layer_min));
    env.set("first_layer_print_max", TVar::f64s(&p.first_layer_max));
    env.set("first_layer_print_size", TVar::f64s(&[p.first_layer_max[0] - p.first_layer_min[0], p.first_layer_max[1] - p.first_layer_min[1]]));
    env.set("first_layer_center_no_wipe_tower", TVar::f64s(&[(p.first_layer_min[0] + p.first_layer_max[0]) / 2.0, (p.first_layer_min[1] + p.first_layer_max[1]) / 2.0]));
    set_mesh_vars(env, job, p, (bmin, bmax));
    env.set("in_head_wrap_detect_zone", TVar::boolean(false));
    env.set("max_print_z", i(p.max_print_z.ceil() as i64));
    env.set("bbl_bed_temperature_gcode", TVar::boolean(false));
    let bt_init: Vec<f64> = (0..job.filaments()).map(|f| job.bed_temp(f, true)).collect();
    let bt: Vec<f64> = (0..job.filaments()).map(|f| job.bed_temp(f, false)).collect();
    env.set("bed_temperature_initial_layer", TVar::f64s(&bt_init));
    env.set("bed_temperature", TVar::f64s(&bt));
    let single = if job.s("bed_temperature_formula") == "by_highest_temp" { p.used.iter().map(|&f| bt_init[f]).fold(0.0, f64::max) } else {
        p.used.iter().copied().chain(p.used.is_empty().then_some(f0))
            .find(|&f| !job.cfg.bool_at("fab_bed_unheated", f))
            .and_then(|f| bt_init.get(f)).copied().unwrap_or(0.0)
    };
    env.set("bed_temperature_initial_layer_single", i(single.round() as i64));
    env.set("bed_temperature_initial_layer_vector", TVar::string(""));
    let chamber = p.used.iter().map(|&f| job.fil("chamber_temperatures", f)).fold(0.0, f64::max);
    env.set("chamber_temperature", TVar::f64s(&[chamber]));
    env.set("overall_chamber_temperature", i(chamber.round() as i64));
    env.set("enable_high_low_temp_mix", TVar::boolean(false));
    let mut vit = f64::MAX;
    let mut positive_vit = f64::MAX;
    let mut high_temperature = false;
    for &f in &p.used {
        let t = job.fil("temperature_vitrification", f);
        vit = vit.min(t);
        if t > 0.0 { positive_vit = positive_vit.min(t); }
        high_temperature |= job.fil("filament_is_high_temperature", f) == 1.0;
    }
    env.set("min_vitrification_temperature", i(if vit == f64::MAX { 0 } else { vit.round() as i64 }));
    let cooling = if high_temperature || positive_vit == f64::MAX { 0 } else if vit <= 50.0 { 2 } else if vit <= 70.0 { 1 } else { 0 };
    if !job.cfg.has("chamber_cooling_mode") {
        env.set("chamber_cooling_mode", i(cooling));
    }
    env.set("first_layer_bed_temperature", TVar::f64s(&bt_init));
    env.set("first_layer_temperature", TVar::f64s(&vecf(job, "nozzle_temperature_initial_layer")));
    if !job.cfg.has("temperature") {
        env.set("temperature", TVar::f64s(&vecf(job, "nozzle_temperature")));
    }
    env.set("max_print_height", TVar::f64(job.f("printable_height")));
    env.set("z_offset", TVar::f64(0.0));
    env.set("plate_name", TVar::string(""));
    let own = family.own_filament_vendor;
    let all_own = p.used.iter().all(|&f| job.fil_str("filament_vendor", f) == own);
    env.set("is_all_bbl_filament", TVar::boolean(all_own));
    env.set("wipe_tower_center_pos_x", TVar::f64(p.wipe_tower_center[0]));
    env.set("wipe_tower_center_pos_y", TVar::f64(p.wipe_tower_center[1]));
    env.set("wipe_tower_center_pos_valid", TVar::boolean(p.has_wipe_tower));
    let exhaust: Vec<i64> = (0..job.filaments()).map(|f| (job.fil("during_print_exhaust_fan_speed", f) * 2.55).round() as i64).collect();
    env.set("during_print_exhaust_fan_speed_num", TVar::ints(&exhaust));
    let outer_w = job.f("outer_wall_line_width");
    let lh = job.f("layer_height");
    let ov = (job.ext("outer_wall_speed", e0)) * makepad_fab_toolpath::flow::cross_section(outer_w, lh);
    env.set("outer_wall_volumetric_speed", TVar::f64(ov.min(job.fil("filament_max_volumetric_speed", f0))));
    let tpu_first = p.used.iter().any(|&f| job.fil_str("filament_type", f).contains("TPU"));
    env.set("has_tpu_in_first_layer", TVar::boolean(tpu_first));
    env.set("scan_first_layer", TVar::boolean(false));
    env.set("hold_chamber_temp_for_flat_print", TVar::boolean(false));
    env.set("first_layer_area_sum", TVar::f64(p.first_layer_area));
    set_variant_vectors(env, job);
}

/// The bed mesh a start template probes under the first layer: the first layer's bounds grown
/// by `adaptive_bed_mesh_margin` and held inside `bed_mesh_min`..`bed_mesh_max`, with a probe
/// point at least every `bed_mesh_probe_distance` across, ends included. A plate with nothing
/// on its first layer probes the whole bed.
fn set_mesh_vars(env: &mut Env, job: &Job, p: &PlateFacts, bed: ([f64; 2], [f64; 2])) {
    // A point setting is "x,y"; one number is both.
    let pair = |key: &str, default: f64| -> [f64; 2] {
        let text = job.cfg.get(key).map(|v| v.as_vec().join(",")).unwrap_or_default();
        let v: Vec<f64> = text.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        match v.as_slice() {
            [x, y, ..] => [*x, *y],
            [x] => [*x, *x],
            [] => [default; 2],
        }
    };
    let margin = job.f("adaptive_bed_mesh_margin");
    let (lo, hi) = (pair("bed_mesh_min", -99999.0), pair("bed_mesh_max", 99999.0));
    let spacing = pair("bed_mesh_probe_distance", 50.0).map(|s| s.max(1.0));
    let spans = |(a, b): ([f64; 2], [f64; 2])| a[0] <= b[0] && a[1] <= b[1];
    let (from, to) = [(p.first_layer_min, p.first_layer_max), bed].into_iter().find(|&r| spans(r)).unwrap_or(([0.0; 2], [0.0; 2]));
    let min = [0, 1].map(|a| lo[a].max(from[a] - margin));
    let max = [0, 1].map(|a| hi[a].min(to[a] + margin));
    let mut count = [0, 1].map(|a| (((max[a] - min[a]) / spacing[a]).ceil() as i64 + 1).max(3));
    // Few points interpolate as a polynomial; more as a surface, which needs four a side.
    let algo = if count[0] * count[1] <= 6 {
        "lagrange"
    } else {
        if job.s("gcode_flavor") == "klipper" {
            count = count.map(|c| c.max(4));
        }
        "bicubic"
    };
    env.set("adaptive_bed_mesh_min", TVar::f64s(&min));
    env.set("adaptive_bed_mesh_max", TVar::f64s(&max));
    env.set("bed_mesh_probe_count", TVar::ints(&count));
    env.set("bed_mesh_algo", TVar::string(algo));
}

/// Per-filament vectors with the "0 means use another value" fallbacks.
pub fn set_variant_vectors(env: &mut Env, job: &Job) {
    env.set("flush_volumetric_speeds", TVar::f64s(&vec_or(job, "filament_flush_volumetric_speed", "filament_max_volumetric_speed")));
    let fast = job.s("prime_volume_mode") == "Fast";
    let key = if fast { "filament_flush_temp_fast" } else { "filament_flush_temp" };
    env.set("flush_temperatures", TVar::f64s(&vec_or(job, key, "nozzle_temperature_range_high")));
}

/// Variables of the layer-change and timelapse templates.
#[allow(clippy::too_many_arguments)]
pub fn set_layer_vars(env: &mut Env, job: &Job, layer_num: usize, layer_z: f64, max_layer_z: f64, filament: usize, most_used_phys: usize, inline_photo: bool) {
    env.set("layer_num", TVar::int(layer_num as i64));
    env.set("layer_z", TVar::f64(layer_z));
    env.set("max_layer_z", TVar::f64(max_layer_z));
    let e = job.ext_of(filament);
    env.set("current_filament_id", TVar::int(filament as i64));
    env.set("current_extruder_id", TVar::int(e as i64));
    env.set("current_nozzle_id", TVar::int(e as i64));
    env.set("most_used_physical_extruder_id", TVar::int(most_used_phys as i64));
    env.set("curr_physical_extruder_id", TVar::int(job.physical_index(e) as i64));
    env.set("timelapse_inline_photo", TVar::boolean(inline_photo));
    env.set("farthest_point_timelapse_enabled", TVar::boolean(job.b("farthest_point_timelapse")));
    env.set("has_timelapse_safe_pos", TVar::boolean(false));
    env.set("timelapse_pos_x", TVar::int(0));
    env.set("timelapse_pos_y", TVar::int(0));
    env.set("clear_to_x0", TVar::boolean(false));
    env.set("curr_y_acceleration_limit", TVar::f64(job.cfg.f64_at("machine_max_acceleration_y", 2 * e)));
    env.set("curr_accumulated_mass", TVar::f64(0.0));
    env.set("curr_layer_mass", TVar::f64(0.0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_settings::{Config, template::Dialect};

    #[test]
    fn start_bed_uses_the_first_used_filament_requesting_heat() {
        let render = |flags: &[&str], used: Vec<usize>, highest: bool| {
            let mut cfg = Config::new();
            cfg.set_vec("filament_map", vec!["1".into(); 3]);
            cfg.set_str("curr_bed_type", makepad_fab_settings::machine::PLATES[0].0);
            let key = makepad_fab_settings::machine::PLATES[0].1;
            cfg.set_vec(key, ["0", "60", "70"].map(String::from).to_vec());
            cfg.set_vec(&format!("{key}_initial_layer"), ["0", "65", "75"].map(String::from).to_vec());
            if !flags.is_empty() { cfg.set_vec("fab_bed_unheated", flags.iter().map(|s| s.to_string()).collect()); }
            if highest { cfg.set_str("bed_temperature_formula", "by_highest_temp"); }
            let job = Job::new(cfg);
            let mut env = Env::new(&job.cfg, Dialect::Compact);
            set_start_vars(&mut env, &job, makepad_fab_settings::family::DEFAULT, &PlateFacts { used, ..Default::default() });
            env.render("M140 S{bed_temperature_initial_layer_single}\n; {bed_temperature_initial_layer[0]},{bed_temperature_initial_layer[1]},{bed_temperature_initial_layer[2]}").unwrap()
        };
        for flags in [&[][..], &["0", "0", "0"][..]] {
            for (used, first, highest) in [(vec![0, 1, 2], 0, 75), (vec![1, 2], 65, 75), (vec![2, 1], 75, 75), (vec![], 0, 0)] {
                for (rule, temp) in [(false, first), (true, highest)] {
                    assert_eq!(render(flags, used.clone(), rule), format!("M140 S{temp}\n; 0,65,75"), "unchanged without unheated filaments");
                }
            }
        }
        assert_eq!(render(&["1", "0", "0"], vec![0, 1, 2], false), "M140 S65\n; 0,65,75");
        assert_eq!(render(&["1", "0", "0"], vec![0, 2, 1], false), "M140 S75\n; 0,65,75");
        assert_eq!(render(&["1", "0", "0"], vec![0, 1, 2], true), "M140 S75\n; 0,65,75");
        assert_eq!(render(&["1", "1", "1"], vec![0, 1, 2], false), "M140 S0\n; 0,0,0");
    }

    #[test]
    fn chamber_cooling_follows_the_softest_used_material() {
        for (temps, flags, used, expected) in [
            (vec![45, 80], vec![0, 0], vec![0, 1], "2"),
            (vec![80, 60], vec![0, 0], vec![0, 1], "1"),
            (vec![45, 80], vec![0, 0], vec![1], "0"),
            (vec![45, 80], vec![0, 1], vec![0, 1], "0"),
            (vec![45, 80], vec![0, 1], vec![0], "2"),
            (vec![50, 70], vec![0, 0], vec![0, 1], "2"),
            (vec![50, 70], vec![0, 0], vec![1], "1"),
            (vec![0, 0], vec![0, 0], vec![0, 1], "0"),
            (vec![], vec![], vec![0], "0"),
            (vec![45], vec![], vec![0], "2"),
            (vec![45], vec![0], vec![], "0"),
        ] {
            let mut cfg = Config::new();
            cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
            if !temps.is_empty() { cfg.set_vec("temperature_vitrification", temps.iter().map(ToString::to_string).collect()); }
            if !flags.is_empty() { cfg.set_vec("filament_is_high_temperature", flags.iter().map(ToString::to_string).collect()); }
            let job = Job::new(cfg);
            let mut env = Env::new(&job.cfg, Dialect::Compact);
            set_start_vars(&mut env, &job, makepad_fab_settings::family::DEFAULT, &PlateFacts { used: used.clone(), ..Default::default() });
            assert_eq!(env.render("{chamber_cooling_mode}").unwrap(), expected, "temperatures={temps:?}, flags={flags:?}, used={used:?}");
        }
    }

    /// The mesh covers the first layer and its margin inside the probing limits, a point at
    /// least every probe distance, ends included.
    #[test]
    fn the_bed_mesh_covers_the_first_layer_inside_the_probing_limits() {
        let facts = PlateFacts { first_layer_min: [100.0, 1.0], first_layer_max: [180.5, 150.0], ..Default::default() };
        let mesh = |set: &[(&str, &str)]| {
            let mut cfg = Config::new();
            for &(k, v) in set { cfg.set_str(k, v); }
            let job = Job::new(cfg);
            let mut env = Env::new(&job.cfg, Dialect::Compact);
            set_start_vars(&mut env, &job, makepad_fab_settings::family::DEFAULT, &facts);
            env.render("{adaptive_bed_mesh_min[0]},{adaptive_bed_mesh_min[1]} {adaptive_bed_mesh_max[0]},{adaptive_bed_mesh_max[1]} {bed_mesh_probe_count[0]},{bed_mesh_probe_count[1]} {bed_mesh_algo}").unwrap()
        };
        assert_eq!(mesh(&[]), "100,1 180.5,150 3,4 bicubic", "unbounded, a point every 50 mm");
        assert_eq!(mesh(&[("bed_mesh_min", "3,3"), ("bed_mesh_max", "267,267"), ("adaptive_bed_mesh_margin", "5"), ("bed_mesh_probe_distance", "20,100")]), "95,3 185.5,155 6,3 bicubic");
        assert_eq!(mesh(&[("gcode_flavor", "klipper")]), "100,1 180.5,150 4,4 bicubic", "a surface needs four points a side");
        // Nothing on the first layer: the writer's bounds are empty, so the whole bed.
        let mut cfg = Config::new();
        cfg.set_vec("printable_area", ["0x0", "270x0", "270x270", "0x270"].map(String::from).to_vec());
        let job = Job::new(cfg);
        let mut env = Env::new(&job.cfg, Dialect::Compact);
        set_start_vars(&mut env, &job, makepad_fab_settings::family::DEFAULT, &PlateFacts { first_layer_min: [9e12; 2], first_layer_max: [-9e12; 2], ..Default::default() });
        assert_eq!(env.render("{adaptive_bed_mesh_min[0]},{adaptive_bed_mesh_min[1]} {adaptive_bed_mesh_max[0]},{adaptive_bed_mesh_max[1]} {bed_mesh_probe_count[0]}").unwrap(), "0,0 270,270 7");
    }

    #[test]
    fn temperature_defaults_to_each_filaments_nozzle_temperature() {
        let mut cfg = Config::new();
        cfg.set_vec("filament_map", vec!["2".into(), "1".into()]);
        cfg.set_vec("nozzle_temperature", vec!["210".into(), "230".into()]);
        let mut job = Job::new(cfg);
        let mut env = Env::new(&job.cfg, Dialect::Compact);
        set_start_vars(&mut env, &job, makepad_fab_settings::family::DEFAULT, &PlateFacts::default());
        assert_eq!(env.render("{temperature[0]}|{temperature[1]}").unwrap(), "210|230");
        job.cfg.set_vec("temperature", vec!["190".into(), "200".into()]);
        let mut env = Env::new(&job.cfg, Dialect::Compact);
        set_start_vars(&mut env, &job, makepad_fab_settings::family::DEFAULT, &PlateFacts::default());
        assert!(!env.vars.contains_key("temperature"));
        assert_eq!(env.render("{temperature[0]}|{temperature[1]}").unwrap(), "190|200");
    }
}
