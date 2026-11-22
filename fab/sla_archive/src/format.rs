//! The file format's required bytes.
//!
//! Everything in this module is data the printer itself reads: the archive's entry names and
//! extensions, the scene table's and the layer images' magic and markers, the private ZIP field
//! that carries each entry's hash, and the JSON documents' keys. They are spelled exactly as the
//! printer expects them, like the words of a G-code dialect, so they may name the printer's maker.
//! Fab's own code names them by what they are, through these constants, and nowhere spells them
//! itself.

/// The archive's extension (the file name's, without the dot).
pub const EXTENSION: &str = "pwsz";

/// The settings document (JSON): the screen, the machine, the resin and the job.
pub const SETTINGS: &str = "anycubic_photon_resins.pwsp";
/// The print's totals (JSON): resin, cost and time.
pub const PRINT_INFO: &str = "print_info.json";
/// Who wrote the file (JSON).
pub const SOFTWARE: &str = "software_info.conf";
/// The small preview, 224 × 168 RGBA.
pub const PREVIEW_SMALL: &str = "preview_images/preview_0.png";
/// The large preview, 336 × 252 RGBA.
pub const PREVIEW_LARGE: &str = "preview_images/preview_1.png";
/// The per-layer table (JSON).
pub const LAYER_TABLE: &str = "layers_controller.conf";
/// The scene table (binary), the archive's last entry.
pub const SCENE: &str = "scene.slice";
/// Entries newer writers add, which a reader allows and Fab doesn't write.
pub const OPTIONAL: [&str; 2] = ["lcd_function.json", "calc_layer_volumes.data"];

/// A layer image's name before its number (from 0, no padding) and its extension.
pub const LAYER_PREFIX: &str = "layer_images/layer_";
/// A raster layer's extension: 4-bit grey, run-length coded.
pub const RASTER_EXTENSION: &str = ".pw0Img";
/// A vector layer's extension: the outlines as an edge table.
pub const VECTOR_EXTENSION: &str = ".pwszImg";

/// The entries every archive holds, in the order they are written, before the layers and the
/// scene table.
pub const LEADING: [&str; 6] = [SETTINGS, PRINT_INFO, SOFTWARE, PREVIEW_SMALL, PREVIEW_LARGE, LAYER_TABLE];

/// Layer `k`'s entry name, with `extension` ([`RASTER_EXTENSION`] or [`VECTOR_EXTENSION`]).
pub fn layer_entry(k: u32, extension: &str) -> String {
    format!("{LAYER_PREFIX}{k}{extension}")
}

/// The scene table's magic at offset 0, NUL-padded to 16 bytes.
pub const SCENE_MAGIC: &[u8] = b"ANYCUBIC-PWSZ";
/// The scene table's markers around its per-layer records.
pub const SCENE_TABLE_START: [u8; 4] = *b"<---";
pub const SCENE_TABLE_END: [u8; 4] = *b"--->";
/// A vector layer's markers: around the layer, and around its segments.
pub const VECTOR_START: [u8; 4] = *b"{==\0";
pub const VECTOR_SEGMENTS_START: [u8; 4] = *b"[--\0";
pub const VECTOR_SEGMENTS_END: [u8; 4] = *b"--]\0";
pub const VECTOR_END: [u8; 4] = *b"==}\0";

/// The central directory's private extra field that carries an entry's SHA-256: its header id,
/// then in its 36 data bytes a u16 tag, a u16 length (32) and the hash of the entry's
/// uncompressed contents.
pub const HASH_FIELD_ID: u16 = 0x1a51;
pub const HASH_FIELD_TAG: u16 = 0x0017;
pub const HASH_FIELD_LEN: u16 = 36;

/// The settings document's keys.
pub mod settings {
    pub const VERSION: &str = "version";
    pub const MACHINE: &str = "machine_type";
    pub const EXTERN: &str = "machine_extern";

    // the machine: its screen and its previews
    pub const NAME: &str = "name";
    pub const KEY_SUFFIX: &str = "key_suffix";
    pub const KEY_IMAGE_FORMAT: &str = "key_image_format";
    pub const RES_X: &str = "res_x";
    pub const RES_Y: &str = "res_y";
    pub const PITCH_X: &str = "xy_pixel";
    pub const PITCH_Y: &str = "xy_pixel_y";
    pub const MAX_SAMPLES: &str = "max_samples";
    pub const PROPERTY: &str = "property";
    pub const SIZE_X: &str = "print_xsize";
    pub const SIZE_Y: &str = "print_ysize";
    pub const SIZE_Z: &str = "print_zsize";
    pub const MAX_FILE_VERSION: &str = "max_file_version";
    pub const PREV_BACK_COLOR: &str = "prev_back_color";
    pub const PREV_MODEL_COLOR: &str = "prev_model_color";
    pub const PREV_SUPPORTS_COLOR: &str = "prev_supports_color";
    pub const PREV_IMAGE_SIZE: &str = "prev_image_size";
    pub const CHILD_SCREEN: &str = "child_screen";
    pub const PANEL_X: &str = "x";
    pub const PANEL_Y: &str = "y";
    pub const PANEL_WIDTH: &str = "width";
    pub const PANEL_HEIGHT: &str = "height";
    pub const PREV2_BACK_COLOR: &str = "prev2_back_color";
    pub const PREV2_IMAGE_SIZE: &str = "prev2_image_size";
    pub const SEGMENTS_CAPACITY: &str = "raster_segments_capacity";
    pub const RASTER_ANTIALIASING: &str = "raster_antialiasing";
    pub const CLOUDPREV_BACK_COLOR: &str = "cloudprev_back_color";
    pub const CLOUDPREV_IMAGE_SIZE: &str = "cloudprev_imag_size";

    // the resin and the job
    pub const ALIAS: &str = "alias";
    pub const PICTURE: &str = "picture";
    pub const CLOUD_PROPERTY: &str = "cloud_property";
    pub const DEVICE_CN_CODE: &str = "device_cn_code";
    pub const FACTORY_RESINS: &str = "factory_resins";
    pub const ACTIVE_RESINS: &str = "active_resins";
    pub const USER_RESINS: &str = "user_resins";

    // a resin's property block
    pub const CODE: &str = "code";
    pub const CURRENCY: &str = "currency";
    pub const PRICE: &str = "price";
    pub const VOLUME: &str = "volume";
    pub const DENSITY: &str = "density";
    pub const TYPE: &str = "type";
    pub const SUBFUNC_CODE: &str = "subfunc_code";
    pub const TARGET_TEMPERATURE: &str = "target_temperature";

    // a resin's penetration curve
    pub const PENETRATION_CURVE: &str = "depth_penetration_curve";
    pub const ZTHICK_MIN: &str = "zthick_min";
    pub const ZTHICK_MAX: &str = "zthick_max";
    pub const LIGHT_INTENSITY: &str = "light_intensity";
    pub const SAFETY_COEFFICIENT: &str = "safety_coefficient";
    pub const TEMPCURVE_SELECTOR: &str = "current_tempcurve_selector";
    pub const TEMPERATURE_COEFFICIENTS: &str = "temperature_coefficients";
    pub const TEMPERATURE: &str = "temperature";
    pub const X_COEFFICIENT: &str = "x_coefficient";
    pub const Y_COMPENSATION: &str = "y_compensation";

    // a resin's extra slicing parameters
    pub const SLICE_EXTPARA: &str = "slice_extpara";
    pub const MULTI_STATE_USED: &str = "multi_state_used";
    pub const MULTI_STATE_PARAS: &str = "multi_state_paras";
    pub const BOTTOM_0: &str = "bott_0";
    pub const BOTTOM_1: &str = "bott_1";
    pub const NORMAL_0: &str = "normal_0";
    pub const NORMAL_1: &str = "normal_1";
    pub const HEIGHT: &str = "height";
    pub const UP_SPEED: &str = "up_speed";
    pub const DOWN_SPEED: &str = "down_speed";
    pub const TRANSITION_LAYERCOUNT: &str = "transition_layercount";
    pub const TRANSITION_TYPE: &str = "transition_type";
    pub const EXPOSURE_COMPENSATE: &str = "exposure_compensate";
    pub const INTELLI_MODE: &str = "intelli_mode";
    pub const MAX_ACCELERATION: &str = "max_acceleration";
    pub const SEPARATE_SUPPORT_EXPOSURE_DELAYED: &str = "separate_support_exposure_delayed";

    // the job: a resin's slicing parameters
    pub const SLICEPARA: &str = "slicepara";
    pub const ZTHICK: &str = "zthick";
    pub const EXPOSURE_TIME: &str = "exposure_time";
    pub const BOTT_TIME: &str = "bott_time";
    pub const BOTT_LAYERS: &str = "bott_layers";
    pub const OFF_TIME: &str = "off_time";
    pub const ZUP_HEIGHT: &str = "zup_height";
    pub const ZUP_SPEED: &str = "zup_speed";
    pub const ZDOWN_SPEED: &str = "zdown_speed";
    pub const ANTI_COUNT: &str = "anti_count";
    pub const BLUR_LEVEL: &str = "blur_level";
    pub const GRAY_LEVEL: &str = "gray_level";
    pub const USE_INDIVI_LAYERPARA: &str = "use_indivi_layerpara";
    pub const USE_RANDOM_ERODE: &str = "use_random_erode";

    // the firmware's own estimates
    pub const FIRMWARE_CALC_PRINT_TIME: &str = "firmware_calc_print_time";
    pub const FIRMWARE_CALC_PRINT_TIME_PARAS: &str = "firmware_calc_print_time_paras";
    pub const FIRMWARE_CALC_EXP_TIME_PARAS: &str = "firmware_calc_exp_time_paras";
    pub const PRECISION_RANGE_BRANCH: &str = "precision_range_branch";
    pub const PRECISION_PER_VOLUME: &str = "precision_per_volume";
    pub const PRECISION_COEFF_VALUE: &str = "precision_coeff_value";
    pub const ENERGY_COEFF: &str = "energy_coeff";
    pub const MACHINE_EXPOSURE_TON: &str = "machine_exposure_ton";
    /// The machine's motion constants in the firmware's print-time block, in file order, each
    /// with its JSON kind; every one is zero or empty in the maker's files.
    pub const MOTION_CONSTANTS: [(&str, Motion); 24] = [
        ("MACHINE_AXIS_STEPS_PER_UNIT", Motion::List),
        ("MACHINE_BLOCK_BUFFER_SIZE", Motion::Int),
        ("MACHINE_DEFAULT_ACCELERATION", Motion::Float),
        ("MACHINE_DEFAULT_MINSEGMENTTIME", Motion::Int),
        ("MACHINE_DEFAULT_XYJERK", Motion::Float),
        ("MACHINE_DEFAULT_ZJERK", Motion::Float),
        ("MACHINE_GENERATE_FRAME_TIME", Motion::Float),
        ("MACHINE_MAX_ACCELERATION", Motion::List),
        ("MACHINE_MAX_FEEDRATE", Motion::List),
        ("MACHINE_MAX_STEP_FREQUENCY", Motion::Int),
        ("MACHINE_MINIMUM_PLANNER_SPEED", Motion::Float),
        ("MACHINE_NOR_LAYER_DOWN_HEIGHT_DIV", Motion::Float),
        ("MACHINE_NOR_LAYER_DOWN_SPEED_DIV", Motion::Float),
        ("MACHINE_NOR_LAYER_UP_HEIGHT_DIV", Motion::Float),
        ("MACHINE_NOR_LAYER_UP_SPEED_DIV", Motion::Float),
        ("MACHINE_STEP_MUL", Motion::Int),
        ("MACHINE_TIME_COMPENSATE", Motion::Float),
        ("MACHINE_TIM_PRES", Motion::Float),
        ("MACHINE_TIM_RCC_CLK", Motion::Float),
        ("FUNCTION", Motion::Int),
        ("MACHINE_MODE_ACCELERATION", Motion::List),
        ("LAYER_COMPENSATE", Motion::List),
        ("HEIGHT_COMPENSATE", Motion::List),
        ("TIMES_COMPENSATE", Motion::List),
    ];

    /// The JSON kind of a motion constant: 0, 0.0 or an empty list.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Motion {
        Int,
        Float,
        List,
    }
}

/// The per-layer table's keys.
pub mod layer_table {
    pub const COUNT: &str = "count";
    pub const PARAS: &str = "paras";
    pub const EXPOSURE_TIME: &str = "exposure_time";
    pub const LAYER_INDEX: &str = "layer_index";
    pub const LAYER_MINHEIGHT: &str = "layer_minheight";
    pub const LAYER_THICKNESS: &str = "layer_thickness";
    pub const ZUP_HEIGHT: &str = "zup_height";
    pub const ZUP_SPEED: &str = "zup_speed";
}

/// The print totals' keys.
pub mod print_info {
    pub const COST: &str = "cost";
    pub const CURRENCY: &str = "currency";
    pub const PRINT_TIME: &str = "print_time";
    pub const VOLUME: &str = "volume";
}

/// The writer's note's keys.
pub mod software {
    pub const MARK: &str = "mark";
    pub const OPENGL: &str = "opengl";
    pub const OS: &str = "os";
    pub const VERSION: &str = "version";
}

/// The maker's own values for this printer, as its published files carry them: what a file
/// copies into every member Fab doesn't set.
pub mod maker {
    /// The machine's name in the settings document.
    pub const MACHINE_NAME: &str = "Anycubic Photon Mono M7 Pro";
    /// The layer images' format the settings document names, whatever the layers' encoding.
    pub const IMAGE_FORMAT: &str = "pwszImg";
    /// The currency the resin's price is in.
    pub const CURRENCY: &str = "$";
    /// A standard resin's class label (UTF-8).
    pub const STANDARD_RESIN_TYPE: &str = "\u{6807}\u{51c6}\u{6811}\u{8102}";
    /// The high-speed resin's class label (UTF-8).
    pub const HIGH_SPEED_RESIN_TYPE: &str = "\u{9ad8}\u{901f}\u{6811}\u{8102}";
    /// The resins' names in the maker's three groups: normal, fast, high-speed.
    pub const RESIN_NAMES: [&str; 3] = ["default_resin", "default_resin_fast", "high_speed_resin"];
}
