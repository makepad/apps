#![deny(dead_code)]

use makepad_solidsync_serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(from = "u32", into = "u32")]
pub struct Revision {
    #[serde(rename = "serial")]
    pub number: u32,
}

impl From<u32> for Revision {
    fn from(number: u32) -> Self {
        Self { number }
    }
}

impl From<Revision> for u32 {
    fn from(revision: Revision) -> Self {
        revision.number
    }
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Edit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<Result<Revision, ()>>,
        #[serde(skip_serializing_if = "is_zero")]
        offset: f64,
    },
}

/// The derive emits `const _: fn(&f64) -> bool = is_zero;` for `offset`.
/// Changing the helper to take `&u32` or return `()` makes that assignment a type error.
#[test]
fn serde_derives_accept_the_inventory_and_nothing_else() {
    let Change::Edit { revision, offset } = Change::Edit {
        revision: Some(Ok(Revision::from(7))),
        offset: 0.0,
    };
    assert_eq!(u32::from(revision.unwrap().unwrap()), 7);
    assert_eq!(offset, 0.0);
}
