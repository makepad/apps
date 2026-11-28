//! The call between the linked devices (WORKCALL).

pub mod ai;
mod flow;
mod session;
mod media_key;
mod media_session;
mod devices;
mod signal;
pub mod transcribe;
mod widgets;
mod level;
mod share_codec;
mod share_panel;
pub mod control;
pub use session::{ShareChoice, ShareEvent, ShareOffer, ShareRole, ShareRow, ShareShown};
pub use share_panel::{opening_uids, ShareControl, SharePanel, SharePanelSize, ShareSurface};

pub use session::{NotLinked, PeerEvent};
pub use signal::topic;
pub use session::{AudioHost, CallController, CallEnv, MicPort};
pub use flow::{CallFlow, CallScope, CallTarget, CallView, ParticipantShown, Effect, Fact, Ignored, Now, Phase, Press, RING_WAIT};
pub use devices::{DeviceChanges, DeviceKind, DeviceNotice, DeviceRow, Devices, PickerChange, Remember};
pub use signal::{decode, AnswerExt, CallId, Message, Refused, RingExt, Secret, ShareEndReason, ShareOfferExt, ShareWatchExt, Why};
pub(crate) use widgets::length;
pub use widgets::{CallAction, Talking, CallButtonRef, CallButtonWidgetRefExt, CallEntryRowRef,
    CallEntryRowWidgetRefExt, CallRowRef, CallRowWidgetRefExt, LiveDotRef, LiveDotWidgetRefExt};

/// A device picker as it was at the last change to the call row.
#[derive(Clone)]
pub struct PickerShown {
    pub rows: Vec<DeviceRow>,
    pub label: String,
    pub notice: Option<DeviceNotice>,
}

impl PickerShown {
    fn read(devices: &Devices, kind: DeviceKind) -> Self {
        let rows = match kind { DeviceKind::Input => devices.inputs(), DeviceKind::Output => devices.outputs() };
        Self { rows: rows.to_vec(), label: devices.label(kind).into(), notice: devices.notice(kind).cloned() }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TranscribeShown {
    #[default]
    Off,
    Starting,
    On(String),
    Unavailable(String),
}

/// Shared by the chat rail and page. The view also carries the peer's name and live controls.
#[derive(Clone)]
pub struct CallShown {
    pub view: std::sync::Arc<CallView>,
    pub input: PickerShown,
    pub output: PickerShown,
    pub linked: bool,
    pub can_call: bool,
    pub can_share: bool,
    pub mic_open: bool,
    pub echo_cancel_notice: Option<String>,
    pub talking: Talking,
    pub transcribe: TranscribeShown,
    pub transcript_town: String,
    peer_level: std::rc::Rc<std::cell::Cell<u8>>,
    pub share: ShareShown,
}

impl Default for CallShown {
    fn default() -> Self {
        let devices = Devices::default();
        Self {
            view: Default::default(), input: PickerShown::read(&devices, DeviceKind::Input),
            output: PickerShown::read(&devices, DeviceKind::Output), linked: false, can_call: false, can_share: false,
            echo_cancel_notice: None,
            peer_level: Default::default(),
            mic_open: false, talking: Talking::default(), transcribe: TranscribeShown::Off, transcript_town: String::new(),
            share: ShareShown::default(),
        }
    }
}

impl CallShown {
    /// The other person's audible level in 16 steps. Zero hides the ring.
    pub fn peer_level_step(&self) -> u8 {
        if self.view.phase == Phase::Live && !self.view.peer_muted { self.peer_level.get() } else { 0 }
    }
}

/// The call's widgets, registered before the chat that places them.
pub fn script_mod(vm: &mut makepad_widgets::ScriptVm) {
    share_panel::script_mod(vm);
    widgets::script_mod(vm);
}
