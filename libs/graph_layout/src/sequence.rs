use crate::{extent_valid, index_boxes, index_links, ItemKind, Point, Polyline, Skipped};

/// All participant headers start at y = 0 and share this height in canvas units.
pub const PARTICIPANT_HEIGHT: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Participant {
    pub id: u64,
    pub width: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Message {
    pub id: u64,
    pub from: u64,
    pub to: u64,
    /// Vertical space for the label above the arrow. Zero means no label.
    pub height: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SequencePlaced {
    /// ID and top-left corner of each participant header.
    pub participants: Vec<(u64, f64, f64)>,
    /// Each polyline runs from sender to receiver.
    pub messages: Vec<(u64, Polyline)>,
    /// ID, start at the header's bottom centre, and end below all messages.
    pub lifelines: Vec<(u64, Point, Point)>,
    pub skipped: Vec<Skipped>,
}

/// Place headers from left to right and messages in chronological input order.
/// A zero gap still separates successive arrows by one canvas unit.
pub fn sequence(participants: &[Participant], messages: &[Message], gap: f64) -> SequencePlaced {
    let mut placed = SequencePlaced::default();
    let (headers, indices) = index_boxes(
        participants,
        ItemKind::Participant,
        |p| (p.id, extent_valid(p.width)),
        &mut placed.skipped,
    );
    let links = index_links(
        messages,
        ItemKind::Message,
        |m| (m.id, m.from, m.to, m.height.is_finite() && m.height >= 0.0),
        &indices,
        &mut placed.skipped,
    );
    let gap = crate::gap(gap);
    let mut left = 0.0;
    for p in &headers {
        placed.participants.push((p.id, left, 0.0));
        let start = (left + p.width * 0.5, PARTICIPANT_HEIGHT);
        placed.lifelines.push((p.id, start, start));
        left += p.width + gap;
    }

    let spacing = gap.max(1.0);
    let mut y = PARTICIPANT_HEIGHT;
    for link in links {
        let message = &messages[link.index];
        y += spacing + message.height;
        let (_, (from, _), _) = placed.lifelines[link.from];
        let (_, (to, _), _) = placed.lifelines[link.to];
        let path = if link.from == link.to {
            let turn = from + (headers[link.from].width * 0.5).min(16.0);
            let top = y;
            y += 12.0;
            vec![(from, top), (turn, top), (turn, y), (from, y)]
        } else {
            vec![(from, y), (to, y)]
        };
        placed.messages.push((message.id, path));
    }
    for (_, _, end) in &mut placed.lifelines {
        end.1 = y + spacing;
    }
    placed
}
