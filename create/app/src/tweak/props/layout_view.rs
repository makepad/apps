//! Drawing and field bindings for the shared selection's composited Layout rows.
use super::*;
use makepad_widgets::{fab_controls::FabValueInput, *};

#[derive(Default)]
pub(in crate::tweak) struct LayoutControls {
    pub(super) fields: Vec<(u64, String)>,
    pub(super) clicks: Vec<(u64, LayoutClick)>,
    pub(super) abs_uid: u64,
}

impl LayoutControls {
    pub(in crate::tweak) fn event<'a>(&'a self, action: &'a WidgetAction) -> Option<LayoutEvent<'a>> {
        let uid = action.widget_uid.0;
        if let Some((_, prop)) = self.fields.iter().find(|(field, _)| *field == uid) {
            if let Some(number) = action.action.downcast_ref::<fab_controls::FabValueInputAction>() {
                return Some(LayoutEvent::Number(prop, number));
            }
            if let Some(TextInputAction::Returned(text, _)) = action.action.downcast_ref::<TextInputAction>() {
                return Some(LayoutEvent::Returned(prop, text));
            }
        } else if let Some((_, click)) = self.clicks.iter().find(|(button, _)| *button == uid) {
            if matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) {
                return Some(LayoutEvent::Click(*click));
            }
        } else if self.abs_uid != 0 && self.abs_uid == uid {
            if let Some(CheckBoxAction::Change(on)) = action.action.downcast_ref::<CheckBoxAction>() {
                return Some(LayoutEvent::Absolute(*on));
            }
        }
        None
    }
}

impl LayoutKind {
    pub(in crate::tweak) fn template(self) -> LiveId {
        match self {
            Self::Identity => live_id!(IdentityRow),
            Self::Size => live_id!(SizeRow),
            Self::BoxInset(_) => live_id!(BoxRow),
            Self::FlowSpacing => live_id!(FlowRow),
            Self::AlignGrid => live_id!(AlignRow),
            Self::GridTracks => live_id!(GridRow),
            Self::Absolute => live_id!(AbsRow),
            Self::Group(_) => live_id!(GroupRow),
        }
    }
}

impl LayoutValues {
    /// A composite has several properties. Its individual field labels reset the same
    /// named Set that editing the field would send. Box legs use their scrubber's Reset.
    pub(in crate::tweak) fn reset_label(&self, cx: &Cx, editor: &WidgetRef, kind: LayoutKind, abs: Vec2d) -> Option<&'static str> {
        let along_y = parse_flow_text(self.row_value("flow").unwrap_or("")).dir == FlowDir::Down;
        let labels: &[(LiveId, &str)] = match kind {
            LayoutKind::Size => &[
                (id!(w_axis), "width"), (id!(h_axis), "height"),
                (id!(w_min_label), "min_width"), (id!(w_max_label), "max_width"),
                (id!(h_min_label), "min_height"), (id!(h_max_label), "max_height"),
                (id!(w_basis_label), "width"), (id!(h_basis_label), "height"), (id!(aspect_label), "aspect"),
            ],
            LayoutKind::FlowSpacing => &[(id!(name), "flow"), (id!(gap_label), "spacing"), (id!(wrap_label), "wrap_spacing")],
            LayoutKind::AlignGrid if self.is_grid => &[(id!(just_label), "justify_items"), (id!(cross_label), "align_items")],
            LayoutKind::AlignGrid => &[
                (id!(just_label), if along_y { "align.y" } else { "align.x" }),
                (id!(cross_label), if along_y { "align.x" } else { "align.y" }), (id!(space_label), "distribute"),
            ],
            LayoutKind::GridTracks => &[(id!(cols_label), "columns"), (id!(rows_label), "rows"), (id!(areas_label), "areas"),
                (id!(gap_x), "column_gap"), (id!(gap_y), "row_gap"), (id!(fill_label), "auto_flow")],
            LayoutKind::Absolute => &[(id!(abs_label), "abs_pos"), (id!(name), "abs_pos")],
            _ => &[],
        };
        labels.iter().find_map(|(label, prop)| {
            let area = editor.widget(cx, &[*label]).area();
            (self.originals.contains_key(*prop) && area.is_valid(cx) && area.clipped_rect(cx).contains(abs)).then_some(*prop)
        })
    }

    pub(in crate::tweak) fn draw_sidebar(&self, cx: &mut Cx, item: &WidgetRef, kind: LayoutKind,
        path: &str, ty: &str, new_binding: bool, controls: &mut LayoutControls) {
        match kind {
            LayoutKind::Identity => {
                item.child(live_id!(name_field)).set_text(cx, crate::tweak::widget_name(path));
                item.child(live_id!(type_label)).set_text(cx, ty);
            }
            LayoutKind::Size => {
                item.child(live_id!(name)).set_text(cx, "size");
                let size_col = item.child(live_id!(size_col));
                for (axis, row_id, seg_id, input_id, segs, lines, fields) in [
                    (
                        "width",
                        live_id!(w_row),
                        live_id!(w_seg),
                        live_id!(w_input),
                        [live_id!(w_fill), live_id!(w_fit), live_id!(w_fix)],
                        [live_id!(w_clamp), live_id!(w_grow), live_id!(w_basis)],
                        [live_id!(w_min), live_id!(w_max), live_id!(w_weight), live_id!(w_shrink), live_id!(w_basis_in)],
                    ),
                    (
                        "height",
                        live_id!(h_row),
                        live_id!(h_seg),
                        live_id!(h_input),
                        [live_id!(h_fill), live_id!(h_fit), live_id!(h_fix)],
                        [live_id!(h_clamp), live_id!(h_grow), live_id!(h_basis)],
                        [live_id!(h_min), live_id!(h_max), live_id!(h_weight), live_id!(h_shrink), live_id!(h_basis_in)],
                    ),
                ] {
                    let row = size_col.child(row_id);
                    // The row is ONE value now -- `Size.Fill{..}`,
                    // `Size.Fixed(200)`, `"50%"` -- read as such.
                    let size = parse_size_text(self.row_value(axis).unwrap_or("Fit"));
                    // The autolayout convention: Fill spreads
                    // (arrows out), Fit hugs (arrows in), and the
                    // third is a size in the person's own words --
                    // a number of points, `50%`, `25vw`, or an
                    // expression -- which the field then shows.
                    let seg = row.child(seg_id);
                    let labels = ["\u{2194}", "\u{2192}\u{2190}", "#"];
                    let active = match size {
                        SizeText::Fill { .. } => 0,
                        SizeText::Fit => 1,
                        _ => 2,
                    };
                    for (i, seg_child) in segs.into_iter().enumerate() {
                        let btn = seg.child(seg_child);
                        btn.set_text(cx, labels[i]);
                        set_button_fill(cx, btn.clone(), i == active);
                        controls.clicks.push((btn.widget_uid().0, LayoutClick::Size(axis, i)));
                    }
                    let input = row.child(input_id);
                    if bind_text(cx, &input, new_binding)
                    {
                        input.set_text(cx, &size.field_text());
                    }
                    controls.fields
                        .push((input.widget_uid().0, axis.to_string()));
                    // The content-box clamps. A row only exists once
                    // a bound is set, so no row reads as none.
                    let clamp = size_col.child(lines[0]);
                    for (child, prop) in
                        [(fields[0], format!("min_{axis}")), (fields[1], format!("max_{axis}"))]
                    {
                        let field = clamp.child(child);
                        if bind_text(cx, &field, new_binding) {
                            let text = self.row_value(&prop).map(bound_text).unwrap_or_default();
                            field.set_text(cx, &text);
                        }
                        controls.fields.push((field.widget_uid().0, prop));
                    }
                    // A Fill's own fields, on their lines under the
                    // axis; the lines are not there for anything else.
                    let fill = match &size {
                        SizeText::Fill(fill) => Some(fill.clone()),
                        _ => None,
                    };
                    let grow = size_col.child(lines[1]);
                    let basis = size_col.child(lines[2]);
                    grow.set_visible(cx, fill.is_some());
                    basis.set_visible(cx, fill.is_some());
                    if let Some(fill) = fill {
                        for (child, key, value) in
                            [(fields[2], "weight", fill.weight), (fields[3], "shrink", fill.shrink)]
                        {
                            let field = grow.child(child);
                            if let Some(mut input) = field.borrow_mut::<FabValueInput>() {
                                if new_binding { input.set_enabled(cx, false); input.set_enabled(cx, true); }
                                input.set_value(cx, value);
                            }
                            controls.fields
                                .push((field.widget_uid().0, format!("{axis}#{key}")));
                        }
                        let field = basis.child(fields[4]);
                        if bind_text(cx, &field, new_binding) {
                            field.set_text(cx, &fill.basis);
                        }
                        controls.fields
                            .push((field.widget_uid().0, format!("{axis}#basis")));
                    }
                }
                // The aspect, width over height, under both axes.
                let field = size_col.child(live_id!(aspect_row)).child(live_id!(aspect_in));
                if bind_text(cx, &field, new_binding) {
                    let text = self
                        .row_value("aspect")
                        .and_then(|v| v.parse::<f64>().ok())
                        .map(fmt_f64)
                        .unwrap_or_default();
                    field.set_text(cx, &text);
                }
                controls.fields.push((field.widget_uid().0, "aspect".to_string()));
            }
            LayoutKind::BoxInset(kind) => {
                let base = kind.prop();
                item.child(live_id!(name)).set_text(cx, base);
                let all = self
                    .row_value(base)
                    .and_then(|v| v.parse::<f64>().ok());
                let box_col = item.child(live_id!(box_col));
                let legs = [
                    (live_id!(mid_row), live_id!(leg_left), "left"),
                    (live_id!(top_row), live_id!(leg_top), "top"),
                    (live_id!(mid_row), live_id!(leg_right), "right"),
                    (live_id!(bot_row), live_id!(leg_bottom), "bottom"),
                ];
                for (row, child, leg) in legs {
                    let prop = format!("{base}.{leg}");
                    let value = self
                        .row_value(&prop)
                        .and_then(|v| v.parse::<f64>().ok())
                        .or(all)
                        .unwrap_or(0.0);
                    let field = box_col.child(row).child(child);
                    if let Some(mut input) = field.borrow_mut::<FabValueInput>() {
                        if new_binding { input.set_enabled(cx, false); input.set_enabled(cx, true); }
                        input.set_value(cx, value);
                    }
                    controls.fields.push((field.widget_uid().0, prop));
                }
            }
            LayoutKind::FlowSpacing => {
                item.child(live_id!(name)).set_text(cx, "flow");
                // The row is one value -- `Flow.Right{wrap: true ..}`
                // -- and every button writes the whole of it back.
                let flow = parse_flow_text(self.row_value("flow").unwrap_or(""));
                let col = item.child(live_id!(flow_col));
                let dir_row = col.child(live_id!(dir_row));
                let seg = dir_row.child(live_id!(flow_seg));
                let dirs = [
                    (live_id!(f_right), "\u{2192}", FlowDir::Right),
                    (live_id!(f_down), "\u{2193}", FlowDir::Down),
                    (live_id!(f_over), "stack", FlowDir::Overlay),
                ];
                for (child, label, dir) in dirs {
                    let btn = seg.child(child);
                    btn.set_text(cx, label);
                    set_button_fill(cx, btn.clone(), flow.dir == dir);
                    controls.clicks
                        .push((btn.widget_uid().0, LayoutClick::Direction(dir)));
                }
                let wrap_btn = dir_row.child(live_id!(f_wrap));
                wrap_btn.set_text(cx, "wrap");
                set_button_fill(cx, wrap_btn.clone(), flow.wraps());
                controls.clicks
                    .push((wrap_btn.widget_uid().0, LayoutClick::Wrap));
                // How a row lines up only means anything left to
                // right; the segment is not shown otherwise.
                let ra_seg = dir_row.child(live_id!(ra_seg));
                ra_seg.set_visible(cx, flow.dir == FlowDir::Right);
                let aligns = [
                    (live_id!(ra_top), "\u{2191}", RowAlignText::Top),
                    (live_id!(ra_mid), "\u{2195}", RowAlignText::Center),
                    (live_id!(ra_bottom), "\u{2193}", RowAlignText::Bottom),
                ];
                for (child, label, row_align) in aligns {
                    let btn = ra_seg.child(child);
                    btn.set_text(cx, label);
                    set_button_fill(
                        cx,
                        btn.clone(),
                        flow.dir == FlowDir::Right && flow.row_align == row_align,
                    );
                    controls.clicks
                        .push((btn.widget_uid().0, LayoutClick::RowAlign(row_align)));
                }
                // The gaps: between children, and between wrapped
                // rows. A Label flows its text but has no spacing,
                // so the line only shows where there is one to set.
                let gap_row = col.child(live_id!(gap_row));
                gap_row.set_visible(cx, self.row_value("spacing").is_some());
                let field = gap_row.child(live_id!(spacing_input));
                if let Some(mut input) = field.borrow_mut::<FabValueInput>() {
                    if new_binding { input.set_enabled(cx, false); input.set_enabled(cx, true); }
                    let v = self
                        .row_value("spacing")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    input.set_value(cx, v);
                }
                controls.fields
                    .push((field.widget_uid().0, "spacing".into()));
                let wrap_box = gap_row.child(live_id!(wrap_box));
                wrap_box.set_visible(
                    cx,
                    flow.wraps() && self.row_value("wrap_spacing").is_some(),
                );
                let field = wrap_box.child(live_id!(wrap_input));
                if let Some(mut input) = field.borrow_mut::<FabValueInput>() {
                    if new_binding { input.set_enabled(cx, false); input.set_enabled(cx, true); }
                    let v = self
                        .row_value("wrap_spacing")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    input.set_value(cx, v);
                }
                controls.fields
                    .push((field.widget_uid().0, "wrap_spacing".into()));
            }
            LayoutKind::AlignGrid => if self.is_grid {
                // A Grid aligns each child inside its cell: along
                // the columns and along the rows, stretch included.
                item.child(live_id!(name)).set_text(cx, "cells");
                let col = item.child(live_id!(align_col));
                col.child(live_id!(space_row)).set_visible(cx, false);
                for (row_id, seg_id, axis_id, prop, arrow, ids) in [
                    (
                        live_id!(just_row),
                        live_id!(just_seg),
                        live_id!(just_axis),
                        "justify_items",
                        "",
                        [live_id!(j_stretch), live_id!(j_start), live_id!(j_mid), live_id!(j_end)],
                    ),
                    (
                        live_id!(cross_row),
                        live_id!(cross_seg),
                        live_id!(cross_axis),
                        "align_items",
                        "",
                        [live_id!(c_stretch), live_id!(c_start), live_id!(c_mid), live_id!(c_end)],
                    ),
                ] {
                    let row = col.child(row_id);
                    row.child(axis_id).set_text(cx, arrow);
                    let current = self
                        .row_value(prop)
                        .map(|v| v.rsplit('.').next().unwrap_or("").to_string())
                        .unwrap_or_else(|| "Stretch".to_string());
                    let seg = row.child(seg_id);
                    for (child, label, variant) in [
                        (ids[0], "stretch", "Stretch"),
                        (ids[1], "start", "Start"),
                        (ids[2], "centre", "Center"),
                        (ids[3], "end", "End"),
                    ] {
                        let btn = seg.child(child);
                        btn.set_visible(cx, true);
                        btn.set_text(cx, label);
                        set_button_fill(cx, btn.clone(), current == variant);
                        controls.clicks
                            .push((btn.widget_uid().0, LayoutClick::Variant(prop, "CellAlign", variant)));
                    }
                }
            } else {
                    item.child(live_id!(name)).set_text(cx, "align");
                    let ax = self
                        .row_value("align.x")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let ay = self
                        .row_value("align.y")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    // Which axis is along the flow and which across
                    // follows the direction; the labels carry the arrow.
                    let dir = parse_flow_text(self.row_value("flow").unwrap_or("")).dir;
                    let down = dir == FlowDir::Down;
                    let (main, cross) = if down { (ay, ax) } else { (ax, ay) };
                    let distribute = self
                        .row_value("distribute")
                        .map(|v| v.rsplit('.').next().unwrap_or("").to_string())
                        .unwrap_or_else(|| "Start".to_string());
                    let col = item.child(live_id!(align_col));
                    col.child(live_id!(space_row)).set_visible(cx, true);
                    let just_row = col.child(live_id!(just_row));
                    just_row
                        .child(live_id!(just_axis))
                        .set_text(cx, if down { "\u{2195}" } else { "\u{2194}" });
                    let seg = just_row.child(live_id!(just_seg));
                    seg.child(live_id!(j_stretch)).set_visible(cx, false);
                    for (child, label, value) in [
                        (live_id!(j_start), "start", 0.0),
                        (live_id!(j_mid), "centre", 0.5),
                        (live_id!(j_end), "end", 1.0),
                    ] {
                        let btn = seg.child(child);
                        btn.set_text(cx, label);
                        set_button_fill(
                            cx,
                            btn.clone(),
                            distribute == "Start" && (main - value).abs() < 0.25,
                        );
                        controls.clicks.push((btn.widget_uid().0, LayoutClick::Justify(value)));
                    }
                    let seg = col.child(live_id!(space_row)).child(live_id!(space_seg));
                    for (child, label, variant) in [
                        (live_id!(s_between), "between", "SpaceBetween"),
                        (live_id!(s_around), "around", "SpaceAround"),
                        (live_id!(s_evenly), "evenly", "SpaceEvenly"),
                    ] {
                        let btn = seg.child(child);
                        btn.set_text(cx, label);
                        set_button_fill(cx, btn.clone(), distribute == variant);
                        controls.clicks
                            .push((btn.widget_uid().0, LayoutClick::Variant("distribute", "Distribute", variant)));
                    }
                    let cross_row = col.child(live_id!(cross_row));
                    cross_row
                        .child(live_id!(cross_axis))
                        .set_text(cx, if down { "\u{2194}" } else { "\u{2195}" });
                    let seg = cross_row.child(live_id!(cross_seg));
                    seg.child(live_id!(c_stretch)).set_visible(cx, false);
                    for (child, label, value) in [
                        (live_id!(c_start), "start", 0.0),
                        (live_id!(c_mid), "centre", 0.5),
                        (live_id!(c_end), "end", 1.0),
                    ] {
                        let btn = seg.child(child);
                        btn.set_text(cx, label);
                        set_button_fill(cx, btn.clone(), (cross - value).abs() < 0.25);
                        controls.clicks.push((btn.widget_uid().0, LayoutClick::Cross(value)));
                    }
                }
            LayoutKind::GridTracks => {
                item.child(live_id!(name)).set_text(cx, "grid");
                let col = item.child(live_id!(grid_col));
                // The tracks, as the CSS they were written in.
                for (row_id, input_id, prop) in [
                    (live_id!(cols_row), live_id!(cols_in), "columns"),
                    (live_id!(rows_row), live_id!(rows_in), "rows"),
                ] {
                    let field = col.child(row_id).child(input_id);
                    if bind_text(cx, &field, new_binding) {
                        field.set_text(cx, &quoted_list_text(self.row_value(prop).unwrap_or(""), " "));
                    }
                    controls.fields.push((field.widget_uid().0, prop.to_string()));
                }
                let gaps = col.child(live_id!(gaps_row));
                for (child, prop) in
                    [(live_id!(gap_col), "column_gap"), (live_id!(gap_row_in), "row_gap")]
                {
                    let field = gaps.child(child);
                    if let Some(mut input) = field.borrow_mut::<FabValueInput>() {
                        if new_binding { input.set_enabled(cx, false); input.set_enabled(cx, true); }
                        let v = self
                            .row_value(prop)
                            .and_then(|v| v.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        input.set_value(cx, v);
                    }
                    controls.fields.push((field.widget_uid().0, prop.to_string()));
                }
                // Which way the cells without a place are filled in.
                let auto = self
                    .row_value("auto_flow")
                    .map(|v| v.rsplit('.').next().unwrap_or("").to_string())
                    .unwrap_or_else(|| "Row".to_string());
                let seg = col.child(live_id!(fill_row)).child(live_id!(fill_seg));
                for (child, label, variant) in [
                    (live_id!(f_rows), "by row", "Row"),
                    (live_id!(f_cols), "by column", "Column"),
                ] {
                    let btn = seg.child(child);
                    btn.set_text(cx, label);
                    set_button_fill(cx, btn.clone(), auto == variant);
                    controls.clicks
                        .push((btn.widget_uid().0, LayoutClick::Variant("auto_flow", "AutoFlow", variant)));
                }
                let field = col.child(live_id!(areas_row)).child(live_id!(areas_in));
                if bind_text(cx, &field, new_binding) {
                    field.set_text(cx, &quoted_list_text(self.row_value("areas").unwrap_or(""), " / "));
                }
                controls.fields.push((field.widget_uid().0, "areas".to_string()));
            }
            LayoutKind::Group(kind) => {
                item.child(live_id!(title)).set_text(
                    cx,
                    match kind {
                        GroupKind::Item => "in its parent",
                        GroupKind::Container => "its children",
                    },
                );
            }
            LayoutKind::Absolute => {
                item.child(live_id!(name)).set_text(cx, "position");
                let pos = self.row_value("abs_pos").and_then(parse_vec2_text);
                let col = item.child(live_id!(abs_col));
                let check = col.child(live_id!(abs_check));
                if let Some(mut check) = check.borrow_mut::<CheckBox>() {
                    check.set_active(cx, pos.is_some(), Animate::No);
                }
                controls.abs_uid = check.widget_uid().0;
                let xy = col.child(live_id!(abs_xy));
                xy.set_visible(cx, pos.is_some());
                if let Some((x, y)) = pos {
                    for (child, key, value) in
                        [(live_id!(abs_x), "x", x), (live_id!(abs_y), "y", y)]
                    {
                        let field = xy.child(child);
                        if let Some(mut input) = field.borrow_mut::<FabValueInput>() {
                            if new_binding { input.set_enabled(cx, false); input.set_enabled(cx, true); }
                            input.set_value(cx, value);
                        }
                        controls.fields
                            .push((field.widget_uid().0, format!("abs_pos#{key}")));
                    }
                }
            }
        }
    }
}

fn bind_text(cx: &mut Cx, field: &WidgetRef, new_binding: bool) -> bool {
    let focused = field.area() != Area::Empty && cx.has_key_focus(field.area());
    if new_binding && focused { cx.set_key_focus(Area::Empty); }
    new_binding || !focused
}

/// Selected state by fill, never by brackets in the label.
///
/// ALL FIVE fills, not just the resting one. A Button's face is
/// `color`/`color_hover`/`color_down`/`color_focus` mixed by the animator,
/// and it takes key focus on click — so setting `color` alone left every
/// toggle you had just clicked painting the theme's focus grey instead of
/// its own state, indefinitely. The state was correct and invisible: the
/// last button touched always looked the same whichever way it was set,
/// which is what made the whole row unreadable. The gradient end stops go
/// flat (negative alpha) for the same reason — the theme gives the hover and
/// focus states a second stop, and a two-tone face reads as a third state
/// that does not exist.
fn set_button_fill(cx: &mut Cx, btn: WidgetRef, selected: bool) {
    // Literals, and deliberately: the panel's chrome stands still under
    // whatever theme the app is wearing, so the colour that says "you are in
    // this mode" must not be read from a table that can move underneath it.
    // Both pairs are chosen against `fab.color_text`, which is what the
    // button draws its word in.
    let (base, hover, down): (Vec4f, Vec4f, Vec4f) = if selected {
        (
            vec4(0.23, 0.45, 0.83, 1.0),
            vec4(0.31, 0.54, 0.92, 1.0),
            vec4(0.17, 0.35, 0.67, 1.0),
        )
    } else {
        (
            vec4(0.17, 0.17, 0.18, 1.0),
            vec4(0.27, 0.27, 0.29, 1.0),
            vec4(0.12, 0.12, 0.13, 1.0),
        )
    };
    let flat: Vec4f = vec4(-1.0, -1.0, -1.0, -1.0);
    if let Some(mut btn) = btn.borrow_mut::<Button>() {
        set_face(cx, &mut btn, &[
            (live_id!(color), base),
            (live_id!(color_hover), hover),
            (live_id!(color_down), down),
            (live_id!(color_focus), base),
            (live_id!(color_2), flat),
            (live_id!(color_2_hover), flat),
            (live_id!(color_2_down), flat),
            (live_id!(color_2_focus), flat),
        ]);
    }
}

/// Writes a button's face colours, which are shader uniforms, straight in.
/// The panel sets them from its draw, where an apply would redraw the button
/// and so ask for the next frame for as long as the panel is up.
pub(in crate::tweak) fn set_face(cx: &Cx, btn: &mut Button, colors: &[(LiveId, Vec4f)]) {
    for (id, color) in colors {
        btn.draw_bg.set_uniform(cx, *id, &rgba(*color));
    }
}

pub(in crate::tweak) fn rgba(color: Vec4f) -> [f32; 4] {
    [color.x, color.y, color.z, color.w]
}
