//! Makepad Create's core, without UI: what two people (and their AIs) share, and the rules that
//! decide what each AI may see.
//!
//! - [`event`]: the signed event format (v1). Ids hash the signed header only, so a second valid
//!   signature never makes a "new" event; the body is separate and salted, so it can be erased
//!   (redaction) while the chain still verifies; bytes are kept as received, never re-encoded.
//! - [`key`]: this device's own P-256 key (Create's, never the fleet key) and the one function
//!   every signature goes through, with a domain per purpose.
//! - [`log`]: the append-only log of raw events on disk.
//! - [`doc`]: shared Unicode text, stable character and block ids, and Markdown diffs.
//! - [`entity`]: shared objects, their numbered and named fields, and queries for a project or its links.
//! - [`order_key`]: fractional byte keys for ordered fields.
//! - [`fold`]: one deterministic replay of the log in a canonical order (height, author, id) into
//!   the state the app shows, and `ai_visible`, the chat a person's own AI may read.
//! - [`queue`]: the durable local queue of what waits for my AI (not shared).
//! - [`peers`]: this device's private record of linked devices and rooms.
//! - [`home`]: the directory one Create instance owns (its lock, key, log and endpoint file).

pub mod blob;
pub mod connect;
pub mod doc;
pub mod entity;
pub mod event;
pub mod fold;
pub mod home;
pub mod key;
pub mod link;
pub mod literal;
pub mod log;
pub mod order_key;
pub mod peers;
pub mod queue;
pub mod share;
pub mod sync;

/// Lower-case hex of `bytes` (event ids in tool input and output).
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An event id from 64 hex digits.
pub fn parse_id(text: &str) -> Option<event::Id> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut id = [0u8; 32];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(id)
}
