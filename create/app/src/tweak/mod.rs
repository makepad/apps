//! Shared app tweaking. A session belongs to one timeline share. Only its owner holds an app link.
mod app;
pub(crate) mod design;
pub(crate) mod focus;
mod hub;
pub use hub::Tweaks;
pub mod overlay;
pub mod panel;
mod props;
mod relay;
mod session;
mod shader;
pub mod theme;
mod tree;
pub mod wire;

pub use app::AppLink;
pub use crate::app_link::{AppLinkEvent, DesignOp, DesignVerb, LinkId, Ticket, WindowId};
pub use crate::call::{NotLinked, topic::TWEAK};
pub use relay::{PeerUndelivered, Relay};
pub use session::{poll_relay, SeatView, Session, Selection, OLDER_SHARER_TREE, TREE_LEVEL, OLDER_SHARER_THEME, THEME_LEVEL,
    DESIGN_LEVEL, OLDER_SHARER_DESIGN, OLDER_SHARER_CONTROL};
pub use wire::{At, Notice, Rect, Request, TreeQuery, ThemeInstall, ThemeOp, ThemeVal,
    BakeScope, DesignNotice, DesignPlace, DesignReplyKind, DesignRequest, StepDir};

use makepad_town_core::{event::Id, key::PublicKey};
use std::collections::BTreeSet;

pub type Seat = PublicKey;
pub type SessionId = [u8; 16];
pub type RequestId = u128;

/// A share's control right as the sharer's call holds it. A seat with control can tweak the app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlRights {
    pub all: bool,
    pub seats: BTreeSet<Seat>,
}

impl ControlRights {
    pub fn allows(&self, seat: &Seat) -> bool { self.all || self.seats.contains(seat) }
}

pub(crate) const NEEDS_CONTROL: &str = "Ask for control to edit this share.";
pub(crate) const CONTROL_ENDED: &str = "Control ended. Ask for control to edit this share.";
pub(crate) const OWN_LOOK: &str = "This share is your own app, so app_look has no picture for it. See your app with your local tools, such as `makepad-remote app <app> grab`.";

// Exact widget refs use slashes (and can end in View.2). Older widget paths use dots.
pub(crate) fn widget_name(path: &str) -> &str {
    path.rsplit_once('/').or_else(|| path.rsplit_once('.')).map_or(path, |(_, name)| name)
}

impl Request {
    pub(super) fn done_text(&self, title: &str) -> Option<String> {
        Some(match self {
            Self::Pick(At::Path(path)) => format!("picked {}", widget_name(path)),
            Self::Props { path } => format!("read {}'s properties", widget_name(path)),
            Self::Set { prop, value, .. } => format!("set {prop} {value}"),
            Self::Tree(_) => "read the widget tree".into(),
            Self::Theme(ThemeOp::Get) => "read the theme".into(),
            Self::Theme(ThemeOp::Set { name, value }) => format!("set {name} {value}"),
            Self::Theme(ThemeOp::Install(spec)) => format!("installed {}", spec.label),
            Self::Look => format!("looked at {title}'s share"),
            _ => return None,
        })
    }

    pub(super) fn running_text(&self, title: &str) -> Option<String> {
        Some(match self {
            Self::Pick(At::Path(path)) => format!("picking {}", widget_name(path)),
            Self::Pick(At::Point { .. }) => format!("picking in {title}"),
            Self::Props { path } => format!("reading {}'s properties", widget_name(path)),
            Self::Set { prop, value, .. } => format!("proposing {prop} {value}"),
            Self::Tree(_) => "reading the widget tree".into(),
            Self::Theme(ThemeOp::Get) => "reading the theme".into(),
            Self::Theme(ThemeOp::Set { name, value }) => format!("setting {name} {value}"),
            Self::Theme(ThemeOp::Install(spec)) => format!("installing {}", spec.label),
            Self::Look => format!("looking at {title}'s share"),
            Self::Design(_) | Self::Select(_) | Self::LookDone { .. } => return None,
        })
    }
}

/// Stack guard for the recursive JSON parser, far above the roughly three levels used by rows and picks.
pub const TWEAK_JSON_DEPTH: u32 = 256;
const TOO_DEEP: &str = "nesting too deep";

pub(crate) fn parse_json(text: &str) -> Result<makepad_strict_json::Value, &'static str> {
    makepad_strict_json::parse_depth(text.as_bytes(), TWEAK_JSON_DEPTH)
}

fn log_depth_refusal(peer: Seat, share: SessionId) {
    use makepad_town_core::hex;
    makepad_widgets::log!("town: refused tweak JSON over depth {TWEAK_JSON_DEPTH} from peer {} in share {}",
        hex(&peer), hex(&share));
}

/// The session id is TL0's SESSION Open id. Its sharer is bound to the signed App creator.
pub fn share_of(app: &makepad_town_core::entity::Entity) -> Option<(SessionId, Seat)> {
    use makepad_town_core::{entity::{field, kind}, event::Value};
    if app.kind != kind::APP { return None }
    let session = app.fields.get(&field::SESSION)?;
    let sharer = app.fields.get(&field::SHARER)?;
    if session.writer != app.id || sharer.writer != app.id { return None }
    let Value::Key(id) = &session.value else { return None };
    let Value::Key(sharer) = &sharer.value else { return None };
    if sharer.as_slice() != app.author { return None }
    Some((id.as_slice().try_into().ok()?, app.author))
}

/// TL0 supplies the lookup when its fold lands. An unknown session has no live/ended claim.
pub fn app_caption(app: &makepad_town_core::entity::Entity, lookup: impl FnOnce(SessionId, Seat) -> Option<bool>) -> String {
    let name = app.text(makepad_town_core::entity::field::TITLE);
    match share_of(app).and_then(|(session, sharer)| lookup(session, sharer)) {
        Some(true) => format!("{name} · live"),
        Some(false) => format!("{name} · ended"),
        None => name.to_owned(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEdit {
    pub seat: Seat,
    pub agent: Option<Id>,
    pub path: String,
    pub prop: String,
    pub change: Change,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Applied { old: String, new: String, seq: u64 },
    Note(String),
    Sidebar(String),
}

#[cfg(test)]
mod tests;
