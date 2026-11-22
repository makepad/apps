//! The networking API's shared vocabulary: what a program's `net_*` ports mean, in one place that the
//! reference interpreter, the device runtimes, the tether's host end and Fab's text all use.
//!
//! - A program declares connections, `net(kind, slots, send_bytes, fn)`; connection `c` is the
//!   declaration's ordinal. It never holds a name, a key or a token: the host's project settings, or
//!   the board's settings record, say what `c` reaches.
//! - The link (Wi-Fi, or the tether to a host) and each connection have a state and the code of their
//!   last failure ([`code`]); programs branch on the code only, and a `detail` number (a Wi-Fi reason,
//!   an HTTP status, a WebSocket close code) is kept for display.
//! - Between the device's ports layer and whatever carries the connections, the same seven messages
//!   ([`NetMsg`]) travel: on the tether as FC1 frames of kinds `watch::NET_OPEN..=watch::NET_LINK`, in
//!   the reference straight from [`crate::sched::Device`] to a [`Host`].
//! - Flow control never drops: the host sends a connection's data only against receive slots the
//!   device freed, and the device sends only within bytes the host granted.
//!
//! [`World`] is the played network behind the host end (the reference model and Fab's twin); [`Host`]
//! is the tether's host end over it; [`upstream`] takes a world's real endpoints to the network
//! through an input log, so a run with a real server still replays exactly.

mod host;
pub mod upstream;
mod world;

pub use host::{Host, HostError};
pub use upstream::{deliver, exchange, Arrival, Asked, Cursor, Network, Request, UpstreamLog};
pub use world::{Condition, Endpoint, Field, Outcome, Payload, Responder, Rule, Rules, Served, Service, Then, When, World, WorldEvent};

use alloc::vec::Vec;

/// The largest part of a message in one NET_DATA: an ETH-sized body less `c` and `flags`.
pub const NET_PART: usize = crate::watch::ETH_MAX - 2;

/// Connection kinds (`net`'s first argument).
pub const KIND_WEBSOCKET: i32 = 0;
pub const KIND_TCP: i32 = 1;

/// `net_send` flags: the message is binary (else text); more of it follows in the next send.
pub const SEND_BINARY: i32 = 1;
pub const SEND_MORE: i32 = 2;

/// NET_DATA flags: binary (else text), the message's first part, its last part.
pub const PART_BINARY: u8 = 1;
pub const PART_FIRST: u8 = 2;
pub const PART_LAST: u8 = 4;

/// Connection states (`net_state`).
pub const OFFLINE: u8 = 0;
pub const CONNECTING: u8 = 1;
pub const READY: u8 = 2;

/// Link states (`net_link`).
pub const LINK_NONE: u8 = 0;
pub const LINK_JOINING: u8 = 1;
pub const LINK_UP: u8 = 2;

/// The host grants this many send bytes when a connection goes ready, then tops the grant up in
/// steps of at least [`GRANT_STEP`] as its upstream takes them.
pub const FIRST_GRANT: u32 = 64 * 1024;
pub const GRANT_STEP: u32 = 16 * 1024;

/// The ports layer reopens a connection the program opened after this long, doubling each failed
/// attempt up to [`RETRY_MAX_NS`]; a ready state resets it. The host and the network try once per
/// open.
pub const RETRY_FIRST_NS: u64 = 1_000_000_000;
pub const RETRY_MAX_NS: u64 = 10_000_000_000;

/// Why a link or a connection is not up: `net_link_error` and `net_error`. Each code belongs to a
/// stage of the link or of an open; the `detail` beside it is the raw number its source gave.
pub mod code {
    pub const NONE: u8 = 0;
    /// No settings record, or no endpoint for the connection.
    pub const NOT_CONFIGURED: u8 = 1;
    /// No access point found (detail: the Wi-Fi reason); no host on the tether.
    pub const NO_NETWORK: u8 = 2;
    /// Wrong password or a security mismatch (detail: the Wi-Fi reason).
    pub const SIGN_IN_FAILED: u8 = 3;
    /// The link went down (detail: the Wi-Fi reason).
    pub const LINK_LOST: u8 = 4;
    /// DHCP gave no address.
    pub const NO_ADDRESS: u8 = 5;
    /// The endpoint's name did not resolve (detail: the DNS rcode).
    pub const NAME_NOT_FOUND: u8 = 6;
    pub const REFUSED: u8 = 7;
    pub const TIMED_OUT: u8 = 8;
    /// The server refused the upgrade or the token (detail: the HTTP status).
    pub const REJECTED: u8 = 9;
    pub const RESET: u8 = 10;
    /// The peer closed the connection (detail: the WebSocket close code).
    pub const CLOSED: u8 = 11;
    pub const PROTOCOL: u8 = 12;
    pub const LAST: u8 = PROTOCOL;
}

/// A code as Fab's text says it.
pub fn code_text(code: u8) -> &'static str {
    match code {
        code::NONE => "none",
        code::NOT_CONFIGURED => "not configured",
        code::NO_NETWORK => "no network",
        code::SIGN_IN_FAILED => "sign-in failed",
        code::LINK_LOST => "link lost",
        code::NO_ADDRESS => "no address",
        code::NAME_NOT_FOUND => "name not found",
        code::REFUSED => "refused",
        code::TIMED_OUT => "timed out",
        code::REJECTED => "rejected by the server",
        code::RESET => "reset",
        code::CLOSED => "closed by the peer",
        code::PROTOCOL => "protocol error",
        _ => "unknown",
    }
}

/// Every value of the pinned Wi-Fi driver's `wifi_err_reason_t` (`esp_wifi_types_generic.h`, ESP-IDF
/// v6.1.1), with the code it reports, in value order. A reason missing here reads as
/// [`code::LINK_LOST`], its number kept as the detail.
pub const WIFI_REASONS: &[(u16, &str, u8)] = {
    use code::{LINK_LOST as L, NO_NETWORK as N, SIGN_IN_FAILED as S};
    &[
        (1, "UNSPECIFIED", L), (2, "AUTH_EXPIRE", L), (3, "AUTH_LEAVE", L),
        (4, "DISASSOC_DUE_TO_INACTIVITY", L), (5, "ASSOC_TOOMANY", N),
        (6, "CLASS2_FRAME_FROM_NONAUTH_STA", L), (7, "CLASS3_FRAME_FROM_NONASSOC_STA", L),
        (8, "ASSOC_LEAVE", L), (9, "ASSOC_NOT_AUTHED", L), (10, "DISASSOC_PWRCAP_BAD", L),
        (11, "DISASSOC_SUPCHAN_BAD", L), (12, "BSS_TRANSITION_DISASSOC", L), (13, "IE_INVALID", L),
        (14, "MIC_FAILURE", S), (15, "4WAY_HANDSHAKE_TIMEOUT", S), (16, "GROUP_KEY_UPDATE_TIMEOUT", L),
        (17, "IE_IN_4WAY_DIFFERS", S), (18, "GROUP_CIPHER_INVALID", S), (19, "PAIRWISE_CIPHER_INVALID", S),
        (20, "AKMP_INVALID", S), (21, "UNSUPP_RSN_IE_VERSION", S), (22, "INVALID_RSN_IE_CAP", S),
        (23, "802_1X_AUTH_FAILED", S), (24, "CIPHER_SUITE_REJECTED", S), (25, "TDLS_PEER_UNREACHABLE", L),
        (26, "TDLS_UNSPECIFIED", L), (27, "SSP_REQUESTED_DISASSOC", L), (28, "NO_SSP_ROAMING_AGREEMENT", L),
        (29, "BAD_CIPHER_OR_AKM", S), (30, "NOT_AUTHORIZED_THIS_LOCATION", N),
        (31, "SERVICE_CHANGE_PERCLUDES_TS", L), (32, "UNSPECIFIED_QOS", L), (33, "NOT_ENOUGH_BANDWIDTH", L),
        (34, "MISSING_ACKS", L), (35, "EXCEEDED_TXOP", L), (36, "STA_LEAVING", L), (37, "END_BA", L),
        (38, "UNKNOWN_BA", L), (39, "TIMEOUT", L), (46, "PEER_INITIATED", L), (47, "AP_INITIATED", L),
        (48, "INVALID_FT_ACTION_FRAME_COUNT", L), (49, "INVALID_PMKID", S), (50, "INVALID_MDE", L),
        (51, "INVALID_FTE", L), (67, "TRANSMISSION_LINK_ESTABLISH_FAILED", L),
        (68, "ALTERATIVE_CHANNEL_OCCUPIED", L), (200, "BEACON_TIMEOUT", L), (201, "NO_AP_FOUND", N),
        (202, "AUTH_FAIL", S), (203, "ASSOC_FAIL", N), (204, "HANDSHAKE_TIMEOUT", S), (205, "CONNECTION_FAIL", N),
        (206, "AP_TSF_RESET", L), (207, "ROAMING", L), (208, "ASSOC_COMEBACK_TIME_TOO_LONG", N),
        (209, "SA_QUERY_TIMEOUT", L), (210, "NO_AP_FOUND_W_COMPATIBLE_SECURITY", N),
        (211, "NO_AP_FOUND_IN_AUTHMODE_THRESHOLD", N), (212, "NO_AP_FOUND_IN_RSSI_THRESHOLD", N),
    ]
};

/// The row of a Wi-Fi reason, when the pinned driver names it.
pub fn wifi_reason(reason: u16) -> Option<(&'static str, u8)> {
    WIFI_REASONS.binary_search_by_key(&reason, |row| row.0).ok().map(|k| (WIFI_REASONS[k].1, WIFI_REASONS[k].2))
}

/// The code a Wi-Fi reason reports; one the table lacks is a link loss.
pub fn wifi_code(reason: u16) -> u8 {
    wifi_reason(reason).map_or(code::LINK_LOST, |row| row.1)
}

/// Which way a message travels: device to host (up) or host to device (down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
}

/// One message between a device's ports layer and the host end of its link. The payload layouts are
/// the FC1 net kinds' (all multibyte fields little-endian); `encode` and `decode` are the only
/// codec, shared by every host and the reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetMsg {
    /// Up: open connection `c`, one attempt.
    Open { c: u8 },
    /// Up: close `c`; the host answers with `State` offline, code 0.
    Close { c: u8 },
    /// Either way: one part of a message, 1..=NET_PART bytes; `flags` are `PART_*`.
    Data { c: u8, flags: u8, bytes: Vec<u8> },
    /// Up: the device freed `slots` receive slots of `c`.
    Freed { c: u8, slots: u16 },
    /// Down: the host takes `bytes` more of `c`'s data.
    Granted { c: u8, bytes: u32 },
    /// Down: `c`'s state and why.
    State { c: u8, state: u8, code: u8, detail: u32 },
    /// Down: the link's state and why.
    Link { state: u8, code: u8, detail: u32 },
}

impl NetMsg {
    /// The FC1 kind that carries it.
    pub fn kind(&self) -> u8 {
        use crate::watch::*;
        match self {
            NetMsg::Open { .. } => NET_OPEN,
            NetMsg::Close { .. } => NET_CLOSE,
            NetMsg::Data { .. } => NET_DATA,
            NetMsg::Freed { .. } | NetMsg::Granted { .. } => NET_CREDIT,
            NetMsg::State { .. } => NET_STATE,
            NetMsg::Link { .. } => NET_LINK,
        }
    }

    /// The connection it concerns (None: the link).
    pub fn conn(&self) -> Option<u8> {
        match *self {
            NetMsg::Open { c } | NetMsg::Close { c } | NetMsg::Data { c, .. } | NetMsg::Freed { c, .. }
            | NetMsg::Granted { c, .. } | NetMsg::State { c, .. } => Some(c),
            NetMsg::Link { .. } => None,
        }
    }

    /// Appends the payload.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            NetMsg::Open { c } | NetMsg::Close { c } => out.push(*c),
            NetMsg::Data { c, flags, bytes } => {
                out.extend_from_slice(&[*c, *flags]);
                out.extend_from_slice(bytes);
            }
            NetMsg::Freed { c, slots } => {
                out.push(*c);
                out.extend_from_slice(&slots.to_le_bytes());
            }
            NetMsg::Granted { c, bytes } => {
                out.push(*c);
                out.extend_from_slice(&bytes.to_le_bytes());
            }
            NetMsg::State { c, state, code, detail } => {
                out.extend_from_slice(&[*c, *state, *code]);
                out.extend_from_slice(&detail.to_le_bytes());
            }
            NetMsg::Link { state, code, detail } => {
                out.extend_from_slice(&[*state, *code]);
                out.extend_from_slice(&detail.to_le_bytes());
            }
        }
    }

    /// Decodes a payload of `kind` travelling `dir`. None: not a well-formed message of that kind and
    /// direction (a bad frame): wrong length, a state above ready, a code above [`code::LAST`], data
    /// flags outside `PART_*`, an empty or oversized part, zero slots or bytes.
    pub fn decode(kind: u8, dir: Dir, payload: &[u8]) -> Option<NetMsg> {
        use crate::watch::*;
        let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let msg = match (kind, dir, payload) {
            (NET_OPEN, Dir::Up, &[c]) => NetMsg::Open { c },
            (NET_CLOSE, Dir::Up, &[c]) => NetMsg::Close { c },
            (NET_DATA, _, &[c, flags, ref bytes @ ..]) => {
                if flags & !(PART_BINARY | PART_FIRST | PART_LAST) != 0 || !(1..=NET_PART).contains(&bytes.len()) {
                    return None;
                }
                NetMsg::Data { c, flags, bytes: bytes.to_vec() }
            }
            (NET_CREDIT, Dir::Up, &[c, s0, s1]) => NetMsg::Freed { c, slots: u16::from_le_bytes([s0, s1]) },
            (NET_CREDIT, Dir::Down, &[c, ref b @ ..]) if b.len() == 4 => NetMsg::Granted { c, bytes: word(b) },
            (NET_STATE, Dir::Down, &[c, state, code, ref d @ ..]) if d.len() == 4 => NetMsg::State { c, state, code, detail: word(d) },
            (NET_LINK, Dir::Down, &[state, code, ref d @ ..]) if d.len() == 4 => NetMsg::Link { state, code, detail: word(d) },
            _ => return None,
        };
        let valid = match msg {
            NetMsg::Freed { slots, .. } => slots > 0,
            NetMsg::Granted { bytes, .. } => bytes > 0,
            NetMsg::State { state, code, .. } | NetMsg::Link { state, code, .. } => state <= READY && code <= code::LAST,
            _ => true,
        };
        valid.then_some(msg)
    }
}
