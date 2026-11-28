use makepad_solidsync_thiserror::Error;
use std::{error::Error as StdError, fmt};

#[derive(Debug, Error)]
#[error("mark {0}")]
struct Mark(pub u32);

#[derive(Debug, Error)]
#[error("{axis} at {distance}")]
struct Offset {
    pub axis: char,
    pub distance: i32,
}

#[derive(Debug)]
struct Chained(Mark);

impl fmt::Display for Chained {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("parent")
    }
}

impl StdError for Chained {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.0)
    }
}

fn tag(value: u32) -> String {
    format!("#{value}")
}

#[derive(Debug, Error)]
enum Problem {
    #[error("closed")]
    Closed,
    #[error(r#"{{0}} [{0:?}] / {}"#, .1)]
    Slot(char, u8),
    #[error("{op} {note:?}")]
    Named { op: &'static str, note: Option<u8> },
    #[error("{} of {}", .used >> 20, .limit >> 20)]
    Quota { used: usize, limit: usize },
    #[error("at {}", tag(*.value))]
    Location { value: u32 },
    #[error("{} bytes", .label.len())]
    Label { label: String },
    #[error("fit {}", .fit.distance)]
    Fit { fit: Offset },
    #[error("{axis} {} {}", if *outer { "out" } else { "in" }, if *outer { "+" } else { "-" })]
    Direction { axis: char, outer: bool },
    #[error(transparent)]
    Nested(Box<Chained>),
}

#[test]
fn thiserror_display_matches_upstream_forms() {
    assert_eq!(Mark(6).to_string(), "mark 6");
    assert_eq!(Offset { axis: 'z', distance: -5 }.to_string(), "z at -5");
    assert_eq!(Problem::Closed.to_string(), "closed");
    assert!(Problem::Closed.source().is_none());
    assert_eq!(Problem::Slot('x', 9).to_string(), "{0} ['x'] / 9");
    assert_eq!(Problem::Named { op: "cut", note: Some(3) }.to_string(), "cut Some(3)");
    assert_eq!(Problem::Quota { used: 3 << 20, limit: 7 << 20 }.to_string(), "3 of 7");
    assert_eq!(Problem::Location { value: 13 }.to_string(), "at #13");
    assert_eq!(Problem::Label { label: "arc".into() }.to_string(), "3 bytes");
    assert_eq!(Problem::Fit { fit: Offset { axis: 'x', distance: 4 } }.to_string(), "fit 4");
    assert_eq!(Problem::Direction { axis: 'y', outer: true }.to_string(), "y out +");
    assert_eq!(Problem::Direction { axis: 'y', outer: false }.to_string(), "y in -");

    let wrapped = Problem::Nested(Box::new(Chained(Mark(2))));
    assert_eq!(wrapped.to_string(), "parent");
    let Problem::Nested(inner) = &wrapped else { unreachable!() };
    let cause = wrapped.source().unwrap().downcast_ref::<Mark>().unwrap();
    assert!(std::ptr::eq(cause, &inner.0), "transparent source must be the inner error's source");
}
