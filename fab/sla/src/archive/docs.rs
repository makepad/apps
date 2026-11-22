//! The archive's four JSON documents: the settings document, the per-layer table, the print's
//! totals and the writer's note.
//!
//! The settings document carries what Fab sets (the screen, the job, the resin's name, price,
//! bottle and density) inside the maker's own values for the profile's settings group, which
//! the printer may read and Fab doesn't model (the resin's codes, its penetration curve, the
//! two-stage lift kept off, the firmware's estimate blocks). Every document is written as the
//! maker's are: UTF-8, LF, four-space indents, members in the maker's order, whole numbers as
//! integers where the maker writes integers and every other number with a decimal point.

use crate::printer::{Group, ResinProfile};
use fab_sla_archive::format::settings as key;
use fab_sla_archive::format::{layer_table, maker, print_info, software, EXTENSION};
use makepad_strict_json::Value;

/// The image the archive's layers hold: its pixels and their pitch (mm) along its width and
/// height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Image {
    pub pixels: [u32; 2],
    pub pitch: [f64; 2],
}

impl Image {
    /// Its size (mm), each side to the µm as the maker's files give it.
    pub fn size(&self) -> [f64; 2] {
        [0, 1].map(|i| (self.pixels[i] as f64 * self.pitch[i] * 1000.0).round() / 1000.0)
    }
}

/// A float as the maker writes one: the shortest decimal of its single-precision value, with a
/// decimal point.
fn float(v: f64) -> Value {
    Value::F64(format!("{:?}", v as f32).parse().unwrap_or(v))
}

fn int(v: i64) -> Value {
    Value::Int(v)
}

fn text(v: &str) -> Value {
    Value::Str(v.into())
}

fn floats(v: &[f64]) -> Value {
    Value::Arr(v.iter().map(|v| float(*v)).collect())
}

fn ints(v: &[i64]) -> Value {
    Value::Arr(v.iter().map(|v| int(*v)).collect())
}

fn object(pairs: Vec<(&str, Value)>) -> Value {
    makepad_strict_json::obj(pairs)
}

/// The maker's own values for a settings group, which Fab copies: the resin's name, codes and
/// class, the penetration curve's table, the two-stage lift's normal stages and the firmware's
/// motion settings.
struct GroupValues {
    resin: &'static str,
    code: &'static str,
    class: &'static str,
    subfunc_code: i64,
    intelli_mode: i64,
    max_acceleration: f64,
    transition_layers: i64,
    /// The curve's temperature rows (temperature, x coefficient, y compensation); none selects
    /// no curve.
    curve: &'static [[f64; 3]],
    /// The normal stages of the two-stage lift (down speed, height, up speed), which stays off.
    normal_stages: [[f64; 3]; 2],
}

/// The penetration curve's table in the maker's normal group.
const NORMAL_CURVE: [[f64; 3]; 5] = [[25.0, 197.78, 1803.2], [10.0, 184.27, 1675.8], [55.0, 166.76, 1474.1], [35.0, 161.19, 1417.3], [45.0, 167.31, 1480.9]];

fn group_values(group: Group) -> GroupValues {
    match group {
        Group::Normal => GroupValues {
            resin: maker::RESIN_NAMES[0],
            code: "10",
            class: maker::STANDARD_RESIN_TYPE,
            subfunc_code: 0,
            intelli_mode: 1,
            max_acceleration: 2.0,
            transition_layers: 10,
            curve: &NORMAL_CURVE,
            normal_stages: [[2.0, 3.0, 2.0], [6.0, 5.0, 6.0]],
        },
        Group::Fast | Group::HighSpeed => GroupValues {
            resin: if group == Group::Fast { maker::RESIN_NAMES[1] } else { maker::RESIN_NAMES[2] },
            code: if group == Group::Fast { "10" } else { "22" },
            class: if group == Group::Fast { maker::STANDARD_RESIN_TYPE } else { maker::HIGH_SPEED_RESIN_TYPE },
            subfunc_code: 1,
            intelli_mode: 2,
            max_acceleration: 20.0,
            transition_layers: 15,
            curve: &[],
            normal_stages: [[6.0, 2.0, 6.0], [15.0, 4.0, 15.0]],
        },
    }
}

/// The resin's name in the archive: the name its settings document selects.
pub fn resin_name(job: &ResinProfile) -> &'static str {
    group_values(job.output.group).resin
}

/// The settings document of `job` on `image`.
pub fn settings(job: &ResinProfile, image: &Image) -> Value {
    let g = group_values(job.output.group);
    let size = image.size();
    let [w, h] = image.pixels.map(|p| p as i64);
    let um = |mm: f64| (mm * 1e6).round() / 1000.0;
    let back = [0.0078125, 0.28125, 0.390625];
    let machine = object(vec![
        (key::VERSION, text("3")),
        (key::NAME, text(maker::MACHINE_NAME)),
        (key::KEY_SUFFIX, text(EXTENSION)),
        (key::KEY_IMAGE_FORMAT, text(maker::IMAGE_FORMAT)),
        (key::RES_X, int(w)),
        (key::RES_Y, int(h)),
        (key::PITCH_X, float(um(image.pitch[0]))),
        (key::PITCH_Y, float(um(image.pitch[1]))),
        (key::MAX_SAMPLES, int(16)),
        (key::PROPERTY, int(55)),
        (key::SIZE_X, float(size[0])),
        (key::SIZE_Y, float(size[1])),
        (key::SIZE_Z, float(job.max_height)),
        (key::MAX_FILE_VERSION, int(518)),
        (key::PREV_BACK_COLOR, floats(&back)),
        (key::PREV_MODEL_COLOR, floats(&[0.8046875; 3])),
        (key::PREV_SUPPORTS_COLOR, floats(&[0.07421875, 0.92578125, 0.9296875])),
        (key::PREV_IMAGE_SIZE, ints(&[224, 168])),
        (key::CHILD_SCREEN, Value::Arr(vec![object(vec![(key::PANEL_X, int(0)), (key::PANEL_Y, int(0)), (key::PANEL_WIDTH, int(w)), (key::PANEL_HEIGHT, int(h))])])),
        (key::PREV2_BACK_COLOR, floats(&[0.07843, 0.10588, 0.16078])),
        (key::PREV2_IMAGE_SIZE, ints(&[336, 252])),
        (key::SEGMENTS_CAPACITY, int(100_000)),
        (key::RASTER_ANTIALIASING, int(4)),
        (key::CLOUDPREV_BACK_COLOR, floats(&back)),
        (key::CLOUDPREV_IMAGE_SIZE, ints(&[800, 600])),
    ]);
    let bottle = job.output.bottle;
    let property = object(vec![
        (key::VERSION, text("3")),
        (key::CODE, text(g.code)),
        (key::CURRENCY, text(maker::CURRENCY)),
        (key::NAME, text(g.resin)),
        (key::PRICE, float(bottle.price)),
        (key::TYPE, text(g.class)),
        (key::VOLUME, float(bottle.ml)),
        (key::SUBFUNC_CODE, int(g.subfunc_code)),
        (key::DENSITY, float(bottle.density)),
        (key::TARGET_TEMPERATURE, float(25.0)),
    ]);
    let curve = object(vec![
        (key::ZTHICK_MIN, float(0.01)),
        (key::ZTHICK_MAX, float(0.2)),
        (key::LIGHT_INTENSITY, float(9000.0)),
        (key::SAFETY_COEFFICIENT, float(1.6)),
        (key::TEMPCURVE_SELECTOR, int(if g.curve.is_empty() { -1 } else { 0 })),
        (
            key::TEMPERATURE_COEFFICIENTS,
            Value::Arr(g.curve.iter().map(|[t, x, y]| object(vec![(key::TEMPERATURE, float(*t)), (key::X_COEFFICIENT, float(*x)), (key::Y_COMPENSATION, float(*y))])).collect()),
        ),
    ]);
    let stage = |[down, height, up]: [f64; 3]| object(vec![(key::DOWN_SPEED, float(down)), (key::HEIGHT, float(height)), (key::UP_SPEED, float(up))]);
    let extra = object(vec![
        (key::VERSION, text("3")),
        (key::MULTI_STATE_USED, int(0)),
        (key::TRANSITION_LAYERCOUNT, int(g.transition_layers)),
        (key::TRANSITION_TYPE, int(0)),
        (
            key::MULTI_STATE_PARAS,
            object(vec![(key::BOTTOM_0, stage([3.0, 5.0, 2.0])), (key::BOTTOM_1, stage([4.0, 3.0, 3.0])), (key::NORMAL_0, stage(g.normal_stages[0])), (key::NORMAL_1, stage(g.normal_stages[1]))]),
        ),
        (key::EXPOSURE_COMPENSATE, float(0.0)),
        (key::INTELLI_MODE, int(g.intelli_mode)),
        (key::MAX_ACCELERATION, float(g.max_acceleration)),
        (key::SEPARATE_SUPPORT_EXPOSURE_DELAYED, float(10.0)),
    ]);
    let slicing = object(vec![
        (key::ANTI_COUNT, int(job.output.anti_alias as i64)),
        (key::BLUR_LEVEL, int(0)),
        (key::BOTT_LAYERS, int(job.bottom_layers as i64)),
        (key::BOTT_TIME, float(job.bottom_exposure)),
        (key::EXPOSURE_TIME, float(job.exposure)),
        (key::GRAY_LEVEL, int(0)),
        (key::OFF_TIME, float(job.light_off_delay)),
        (key::USE_INDIVI_LAYERPARA, int(0)),
        (key::USE_RANDOM_ERODE, int(0)),
        (key::ZDOWN_SPEED, float(job.retract_speed)),
        (key::ZTHICK, float(job.layer_height)),
        (key::ZUP_HEIGHT, float(job.lift.height)),
        (key::ZUP_SPEED, float(job.lift.speed)),
    ]);
    let resin = object(vec![(key::VERSION, text("2")), (key::PROPERTY, property), (key::PENETRATION_CURVE, curve), (key::SLICE_EXTPARA, extra), (key::SLICEPARA, slicing)]);
    let motion = std::iter::once((key::VERSION, text("2")))
        .chain(key::MOTION_CONSTANTS.iter().map(|(name, kind)| {
            let zero = match kind {
                key::Motion::Int => int(0),
                key::Motion::Float => float(0.0),
                key::Motion::List => Value::Arr(Vec::new()),
            };
            (*name, zero)
        }))
        .collect();
    let exposure = object(vec![
        (key::PRECISION_RANGE_BRANCH, floats(&[0.0, 5.0, 25.0])),
        (key::PRECISION_PER_VOLUME, float(5.0)),
        (key::PRECISION_COEFF_VALUE, floats(&[0.024, 0.01, -0.2])),
        (key::ENERGY_COEFF, float(0.0)),
        (key::MACHINE_EXPOSURE_TON, float(1.0)),
    ]);
    let resins = object(vec![
        (key::VERSION, text("3")),
        (key::ALIAS, text("")),
        (key::PICTURE, text("")),
        (key::CLOUD_PROPERTY, int(0)),
        // the printer's cloud id: Fab never writes one
        (key::DEVICE_CN_CODE, text("")),
        (key::FACTORY_RESINS, Value::Arr(Vec::new())),
        (key::USER_RESINS, Value::Arr(vec![resin])),
        (key::ACTIVE_RESINS, Value::Arr(vec![text(g.resin)])),
        (key::FIRMWARE_CALC_PRINT_TIME, int(0)),
        (key::FIRMWARE_CALC_PRINT_TIME_PARAS, object(motion)),
        (key::FIRMWARE_CALC_EXP_TIME_PARAS, exposure),
    ]);
    object(vec![(key::VERSION, text("3")), (key::MACHINE, machine), (key::EXTERN, resins)])
}

/// The per-layer table of `layers` layers of `job`: each layer's exposure (the bottom layers'
/// for the first ones), its bottom face's height, its thickness and its lift, as the job's own
/// values say.
pub fn layer_table(job: &ResinProfile, layers: u32) -> Value {
    let h = job.layer_height;
    let rows = (0..layers)
        .map(|k| {
            object(vec![
                (layer_table::EXPOSURE_TIME, float(job.exposure_of(k))),
                (layer_table::LAYER_INDEX, int(k as i64)),
                (layer_table::LAYER_MINHEIGHT, float(k as f64 * h)),
                (layer_table::LAYER_THICKNESS, float(h)),
                (layer_table::ZUP_HEIGHT, float(job.lift.height)),
                (layer_table::ZUP_SPEED, float(job.lift.speed)),
            ])
        })
        .collect();
    object(vec![(layer_table::COUNT, int(layers as i64)), (layer_table::PARAS, Value::Arr(rows))])
}

/// The print's totals: `ml` of resin at the job's bottle price, and `seconds` on the printer.
pub fn print_info(job: &ResinProfile, ml: f64, seconds: f64) -> Value {
    let bottle = job.output.bottle;
    object(vec![
        (print_info::COST, float(ml * bottle.price / bottle.ml)),
        (print_info::CURRENCY, text(maker::CURRENCY)),
        (print_info::PRINT_TIME, int(seconds.round() as i64)),
        (print_info::VOLUME, float(ml)),
    ])
}

/// Fab's own note of who wrote the file.
pub fn software_info() -> Value {
    object(vec![
        (software::MARK, text("Fab")),
        (software::OPENGL, text("")),
        (software::OS, text(&format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))),
        (software::VERSION, text(env!("CARGO_PKG_VERSION"))),
    ])
}

/// `doc` as the maker's files lay JSON out: four-space indents, LF line ends, `"key": value`.
pub fn text_of(doc: &Value) -> String {
    let mut out = String::new();
    lay_out(doc, 0, &mut out);
    out.push('\n');
    out
}

fn lay_out(v: &Value, depth: usize, out: &mut String) {
    let indent = |out: &mut String, depth: usize| out.extend(std::iter::repeat_n(' ', 4 * depth));
    match v {
        Value::Obj(pairs) if !pairs.is_empty() => {
            out.push_str("{\n");
            for (i, (k, v)) in pairs.iter().enumerate() {
                indent(out, depth + 1);
                Value::Str(k.clone()).write_into(out);
                out.push_str(": ");
                lay_out(v, depth + 1, out);
                out.push_str(if i + 1 < pairs.len() { ",\n" } else { "\n" });
            }
            indent(out, depth);
            out.push('}');
        }
        Value::Arr(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, v) in items.iter().enumerate() {
                indent(out, depth + 1);
                lay_out(v, depth + 1, out);
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(out, depth);
            out.push(']');
        }
        other => other.write_into(out),
    }
}
