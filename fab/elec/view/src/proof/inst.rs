//! A drawable instance as plain data: the painters build these (pure, testable without a GPU),
//! and the pane appends them to its rows through the captured templates.

use crate::canvas::draw::{ArcInst, BoxInst, LineInst};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Inst {
    Line(LineInst),
    Box(BoxInst),
    Arc(ArcInst),
}

impl Inst {
    /// The common block (identity, look, ink) of the instance.
    pub fn common(&self) -> &crate::canvas::draw::Common {
        match self {
            Inst::Line(i) => &i.c,
            Inst::Box(i) => &i.c,
            Inst::Arc(i) => &i.c,
        }
    }
}
