//! The share view and board use the same letterboxed picture in screen layout points.
use super::{At, Seat, SeatView};
use crate::model::clean_name;
use makepad_town_core::{event::Id, fold::{Agent, Profile, State}, hex};
use makepad_widgets::{dvec2, vec4, Align, Cx2d, DVec2, DrawColor, DrawText, Rect, Vec4f, Walk};
use std::{collections::{BTreeMap, HashMap}, sync::Arc};

/// A profile colour of 0 is unset (a new profile writes 0), so the seat's own hue stands in, never black.
pub(crate) fn seat_colour(state: &State, seat: &Seat) -> Vec4f {
    state.profiles.get(seat).filter(|p| p.colour != 0).map_or_else(|| {
        let hue = seat.iter().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(*b as u32)) % 360;
        let channel = |shift: f32| 0.30 + 0.65 * (((hue as f32 / 60. + shift) % 6. - 3.).abs() - 1.).clamp(0., 1.);
        vec4(channel(0.), channel(4.), channel(2.), 1.)
    }, |p| Vec4f::from_u32((p.colour << 8) | 255))
}

pub(crate) fn who(state: &State, seat: &Seat, agent: Option<Id>) -> String {
    actor_label(&state.profiles, &state.agents, seat, agent)
}

pub(super) fn actor_label(profiles: &BTreeMap<Seat, Profile>, agents: &BTreeMap<Id, Agent>, seat: &Seat, agent: Option<Id>) -> String {
    let person = profiles.get(seat).map(|p| clean_name(&p.name)).filter(|s| !s.is_empty())
        .unwrap_or_else(|| hex(&seat[1..5]));
    let Some(id) = agent else { return person };
    if let Some(agent) = agents.get(&id).filter(|agent| &agent.owner == seat) {
        if agents.iter().any(|(other, a)| *other != id && &a.owner == seat) {
            let name = clean_name(&agent.name);
            if !name.is_empty() { return format!("{person}'s {name}") }
        }
    }
    format!("{person}'s AI")
}

#[derive(PartialEq)]
struct Ring { rect: super::Rect, label: String, colour: Vec4f, rank: usize, count: usize }

/// Rebuild on a selection, profile or window change. Drawing borrows these prepared labels.
#[derive(Clone, PartialEq)]
pub struct Selections {
    pub window: usize,
    pub window_size: [f64; 2],
    rings: Arc<[Ring]>,
    pub(super) revision: Option<(usize, u64)>,
}

impl Selections {
    pub fn new(window: usize, window_size: [f64; 2], view: &SeatView, state: &State) -> Self {
        let mut counts = HashMap::new();
        let bounds = |r: super::Rect| [r.x, r.y, r.w, r.h].map(f64::to_bits);
        let mut rings = Vec::new();
        for (seat, selection) in &view.selections {
            let Some(rect) = selection.rect.filter(|r| r.window == window && r.valid()) else { continue };
            let label = who(state, seat, selection.agent);
            let colour = seat_colour(state, seat);
            let rank = counts.entry(bounds(rect)).or_insert(0);
            rings.push(Ring { rect, label, colour, rank: *rank, count: 0 });
            *rank += 1;
        }
        for r in &mut rings { r.count = counts[&bounds(r.rect)]; }
        Self { window, window_size, rings: rings.into(), revision: None }
    }
}

fn picture(view: Rect, [w, h]: [f64; 2]) -> Option<(Rect, f64)> {
    if ![view.pos.x, view.pos.y, view.size.x, view.size.y, w, h].iter().all(|n| n.is_finite())
        || w <= 0. || h <= 0. { return None }
    let scale = (view.size.x / w).min(view.size.y / h);
    if scale <= 0. || !scale.is_finite() { return None }
    let size = dvec2(w, h) * scale;
    Some((Rect { pos: view.pos + (view.size - size) * 0.5, size }, scale))
}

/// The board draws its picture letterboxed inside the App item.
pub(crate) fn board_window_point(view: Rect, size: [f64; 2], screen: DVec2) -> Option<DVec2> {
    let (rect, scale) = picture(view, size)?;
    Some((screen - rect.pos) / scale)
}

/// Both picture views supply a point already mapped into the app window.
pub fn pick_at(window: usize, point: DVec2, [width, height]: [f64; 2]) -> Option<At> {
    (width.is_finite() && height.is_finite()
        && point.x >= 0. && point.x < width && point.y >= 0. && point.y < height)
        .then_some(At::Point { window, x: point.x, y: point.y })
}

pub fn draw_selections(cx: &mut Cx2d, view_rect: Rect, selections: &Selections, ink: &mut DrawColor, label: &mut DrawText) {
    let Some((picture, scale)) = picture(view_rect, selections.window_size) else { return };
    let previous_colours = (ink.color, label.color);
    cx.push_clip_rect(picture);
    for r in selections.rings.iter() {
        let size = dvec2(r.rect.w, r.rect.h) * scale;
        let corner = picture.pos + dvec2(r.rect.x, r.rect.y) * scale;
        if size.x <= 0. || size.y <= 0. || !picture.intersects(Rect { pos: corner, size }) { continue }
        let band = (size.x.min(size.y) / (2. * r.count as f64)).min(2.);
        let inset = r.rank as f64 * band;
        let pos = corner + dvec2(inset, inset);
        let size = size - dvec2(2. * inset, 2. * inset);
        ink.color = r.colour;
        for (offset, edge) in [
            (DVec2::default(), dvec2(size.x, band)), (dvec2(0., size.y - band), dvec2(size.x, band)),
            (DVec2::default(), dvec2(band, size.y)), (dvec2(size.x - band, 0.), dvec2(band, size.y)),
        ] { ink.draw_abs(cx, Rect { pos: pos + offset, size: edge }); }
        let text = label.layout(cx, 0., 0., None, false, Align::default(), &r.label);
        let extent = dvec2(text.size_in_lpxs.width as f64, text.size_in_lpxs.height as f64) * label.font_scale as f64 + dvec2(6., 2.);
        let x = pos.x.min(picture.pos.x + picture.size.x - extent.x).max(picture.pos.x);
        let bottom = picture.pos.y + picture.size.y;
        let y = (picture.pos.y + r.rect.y * scale - extent.y).max(picture.pos.y)
            .min(bottom - extent.y * r.count as f64).max(picture.pos.y) + r.rank as f64 * extent.y;
        let at = dvec2(x, y);
        ink.draw_abs(cx, Rect { pos: at, size: extent });
        label.color = if 0.2126*r.colour.x + 0.7152*r.colour.y + 0.0722*r.colour.z > 0.52 {
            vec4(0., 0., 0., 1.)
        } else { vec4(1., 1., 1., 1.) };
        label.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: at + dvec2(3., 1.), size: extent }), &text);
    }
    cx.pop_clip_rect();
    (ink.color, label.color) = previous_colours;
}

#[test]
fn overlay_maps_only_the_letterboxed_picture() {
    use makepad_town_core::fold::{Agent, Profile};
    use super::{Rect as AppRect, Selection};
    let view = Rect { pos: dvec2(40., 70.), size: dvec2(300., 300.) };
    let size = [200., 100.];
    assert_eq!(pick_at(7, dvec2(50., 25.), size), Some(At::Point { window: 7, x: 50., y: 25. }),
        "the share panel supplies window points directly");
    for (point, size) in [(dvec2(200., 3.), size), (dvec2(4., 100.), size), (dvec2(f64::NAN, 0.), size),
        (dvec2(0., 0.), [f64::INFINITY, 100.]), (dvec2(0., 0.), [200., 0.])] {
        assert!(pick_at(7, point, size).is_none(), "invalid window geometry or an outside point cannot pick");
    }
    let pick = |x, y| pick_at(7, board_window_point(view, size, dvec2(x, y))?, size);
    assert_eq!(pick(115., 182.5), Some(At::Point { window: 7, x: 50., y: 25. }), "offset, 1.5 scale and letterbox use app layout points");
    assert_eq!(pick(40., 145.), Some(At::Point { window: 7, x: 0., y: 0. }), "the picture starts below the upper bar");
    for (x,y) in [(40., 144.), (340., 200.), (50., 295.), (39., 180.)] {
        assert!(pick(x,y).is_none(), "press ({x}, {y}) outside the picture produces no pick");
    }
    let mut seats = SeatView::default();
    let mut state = State::default();
    let rect = AppRect { window: 7, x: 5., y: 5., w: 50., h: 20. };
    for (n, name, colour, window, agent) in [(1, "Owner", 0x446688, 7, None),
        (2, "Peer", 0x88aa44, 7, Some([4; 32])), (3, "Guest", 0x668844, 8, None)] {
        let seat = [n; 65];
        state.profiles.insert(seat, Profile { name: name.into(), colour });
        seats.selections.insert(seat, Selection { path: "ui.panel".into(), rect: Some(AppRect { window, ..rect }), agent });
    }
    let snapshot = Selections::new(7, [200., 100.], &seats, &state);
    let labels: Vec<_> = snapshot.rings.iter().map(|r| (r.label.as_str(), r.rank, r.count)).collect();
    assert_eq!(labels, [("Owner", 0, 2), ("Peer's AI", 1, 2)], "coincident selections each get a ring and label in their own window");
    assert_eq!(snapshot.rings[1].colour, Vec4f::from_u32(0x88aa44ff), "an AI uses its person's profile colour");
    state.profiles.get_mut(&[2; 65]).unwrap().colour = 0;
    let unset = seat_colour(&state, &[2; 65]);
    assert!(unset.x.max(unset.y).max(unset.z) >= 0.9 && unset.w == 1.,
        "an unset profile colour (0) takes the seat's hue, never black ink on the dark panel: {unset:?}");
    state.profiles.get_mut(&[2; 65]).unwrap().colour = 0x88aa44;

    let ana = [2; 65];
    let first = [4; 32];
    let second = [5; 32];
    state.profiles.get_mut(&ana).unwrap().name = "Ana".into();
    state.agents.insert(first, Agent { owner: ana, name: "  Aster\n  ".into(), kind: "session".into() });
    assert_eq!(who(&state, &ana, Some(first)), "Ana's AI", "a person's only AI uses the generic label");
    state.agents.insert(second, Agent { owner: ana, name: "Fe\nrn".into(), kind: "session".into() });
    let snapshot = Selections::new(7, size, &seats, &state);
    assert_eq!(snapshot.rings[1].label, "Ana's Aster", "the selection ring names the first of two AIs");
    assert_eq!(who(&state, &ana, Some(second)), "Ana's Fern", "a second AI has its own cleaned name");
    assert_eq!(who(&state, &ana, Some([6; 32])), "Ana's AI", "an unreplicated AI has a generic label");
    assert_eq!(who(&state, &ana, None), "Ana", "a person keeps their own label when they have several AIs");
}
