#[allow(dead_code)] // Board patch and diff helpers are used by later slices.
pub(crate) mod d1;
mod fs_ops;
pub(crate) mod drag;
pub(crate) mod patch;
pub(super) mod session;
pub(crate) mod tools;
pub(super) mod view;

use super::{Rect, Seat, SessionId};
use makepad_town_core::event::Id;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesignTarget {
    LiveApp(SessionId),
    BoardDesign(Id),
}

/// The part of the owner's session that can be shown to every seat.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DesignStateView {
    pub active: bool,
    pub dirty_files: usize,
    pub undo: usize,
    pub redo: usize,
    pub ghost: Option<GhostView>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GhostView {
    pub seat: Seat,
    pub label: String,
    pub rect: Option<Rect>,
}

impl DesignStateView {
    pub(super) fn json(&self) -> makepad_strict_json::Value {
        use makepad_strict_json::{self as j, Value as V};
        let ghost = self.ghost.as_ref().map_or(V::Null, |g| {
            let rect = g.rect.map_or(V::Null, |r| j::obj(vec![("window", V::Int(r.window as i64)),
                ("rect", V::Arr([r.x, r.y, r.w, r.h].into_iter().map(V::F64).collect()))]));
            j::obj(vec![("seat", j::s(makepad_town_core::hex(&g.seat))), ("label", j::s(&g.label)), ("rect", rect)])
        });
        j::obj(vec![("active", V::Bool(self.active)), ("dirty_files", V::Int(self.dirty_files as i64)),
            ("undo", V::Int(self.undo as i64)), ("redo", V::Int(self.redo as i64)), ("ghost", ghost)])
    }

    pub(super) fn read(text: &str) -> Option<Self> {
        use makepad_strict_json::Value;
        let v = super::parse_json(text).ok()?;
        let count = |key| usize::try_from(v.get(key)?.as_u64()?).ok();
        let ghost = match v.get("ghost")? {
            Value::Null => None,
            g => {
                let hex = g.get("seat")?.as_str()?;
                let mut seat = [0; 65];
                if hex.len() != seat.len()*2 { return None }
                for (i,b) in seat.iter_mut().enumerate() {
                    *b = u8::from_str_radix(hex.get(2*i..2*i+2)?, 16).ok()?;
                }
                let rect = match g.get("rect")? {
                    Value::Null => None,
                    r => Some(super::app::read_rect(r.get("window")?.as_u64()?.try_into().ok()?, r.get("rect")?.as_arr()?).ok()?),
                };
                Some(GhostView { seat, label: g.get("label")?.as_str()?.into(), rect })
            }
        };
        Some(Self { active: v.get("active")?.as_bool()?, dirty_files: count("dirty_files")?, undo: count("undo")?, redo: count("redo")?, ghost })
    }
}
