//! View identities for Stage's performance UI.
//!
//! These name things on screen. They do not load, play, or route audio.

/// Which deck a library command names.
///
/// `A` and `B` only: the identity [`crate::library_nav`] binds its load
/// keys to. The donor deck type's playback and routing stay in the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckId {
    A,
    B,
}

impl DeckId {
    /// The other deck. Identity only.
    pub fn other(self) -> Self {
        match self {
            DeckId::A => DeckId::B,
            DeckId::B => DeckId::A,
        }
    }

    /// `A` is 0, `B` is 1.
    pub fn index(self) -> usize {
        match self {
            DeckId::A => 0,
            DeckId::B => 1,
        }
    }
}
