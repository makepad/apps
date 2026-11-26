//! The printer link: telemetry, storage (FTPS) and typed print control.
//! Prints go through [`Command::Print`] with a safety-checked file.
//! Parsing and TLS run on one worker. The UI checks an atomic revision per event;
//! the app maps its print colours onto the reported trays on publications and edits only.

pub mod camera;
pub mod ftps;
pub mod job;
mod link;
mod inspect;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod moonraker;
#[cfg(not(target_arch = "wasm32"))]
pub mod moonraker_sim;
#[cfg(not(target_arch = "wasm32"))]
mod moonraker_link;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod mqtt;
pub mod safety;
pub mod secrets;
#[cfg(not(target_arch = "wasm32"))]
pub mod sim;
#[cfg(not(target_arch = "wasm32"))]
mod tls;
pub mod virtual_link;
pub mod virtual_run;

pub use job::{JobStage, JobStatus, PrintJob, PrintOptions};
pub use virtual_link::{VirtualSetup, VirtualTray};
pub use virtual_run::{package_gcode, Speed};
pub use link::{published, Command, CommandState, ConnectionStatus, PrinterLink, Snapshot};

/// Why print actions are hidden on this platform.
pub const UNSUPPORTED_PRINTING: &str = "Printing to this printer isn't supported on Windows yet";

/// Upload and start/pause/stop exist on macOS and Linux; Windows stays read-only
/// (its TLS stack cannot resume the printer's storage sessions).
pub const fn printing_supported() -> bool { cfg!(not(any(windows, target_arch = "wasm32"))) }
pub use model::{NozzleSide, PrinterState};
pub use secrets::first_config_name;

mod entries {
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) struct Counter(AtomicUsize);

    impl Counter {
        pub(super) fn enter(&self) { self.0.fetch_add(1, Ordering::Relaxed); }
    }

    macro_rules! boundaries {
        ($($field:ident => $counter:ident),+ $(,)?) => {
            $(pub(super) static $counter: Counter = Counter(AtomicUsize::new(0));)+

            /// Used by the no-network tests to prove a virtual printer never enters these boundaries.
            /// Counts only, with no names, hosts, paths or secrets. Nothing logs them.
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct EntryCounts { $(pub $field: usize,)+ }

            /// Used by the no-network tests to prove a virtual printer never enters these boundaries.
            /// Counts only, with no names, hosts, paths or secrets. Nothing logs them.
            pub fn entry_counts() -> EntryCounts {
                EntryCounts { $($field: $counter.0.load(Ordering::Relaxed),)+ }
            }
        };
    }

    boundaries! {
        start => START,
        config_load => CONFIG_LOAD,
        config_load_file => CONFIG_LOAD_FILE,
        credential => CREDENTIAL,
        mqtt_connect => MQTT_CONNECT,
        mqtt_tls => MQTT_TLS,
        pinned_leaf => PINNED_LEAF,
        storage_session => STORAGE_SESSION,
        ftps_login => FTPS_LOGIN,
        ftps_control_tls => FTPS_CONTROL_TLS,
        ftps_data_tls => FTPS_DATA_TLS,
        moonraker_http => MOONRAKER_HTTP,
        moonraker_websocket => MOONRAKER_WEBSOCKET,
        camera_open => CAMERA_OPEN,
        rtsp_tls => RTSP_TLS,
    }
}

pub use entries::{entry_counts, EntryCounts};
