//! `FabAreaNotches`: each area's nub (design V10.2, FABWS Q2), the chip with its front editor's
//! glyph, its name as the tooltip: the area's editor selector. A panel editor (a list, a form)
//! starts with a toolbar (`widgets::FabEditorBar`) and the nub is its first button, the chip
//! alone, so the editor's own tools follow it and the tab bar above holds only the area's tabs
//! (user, 11-25: a selector in the tab bar read as the whole area's editor). An editor that draws
//! over its whole area (a view, the code, the camera) keeps the nub as a small notch in its
//! top-left corner in the chrome ground, its free corner rounded and a concave fillet where it
//! meets the area's top and left edges (both from the sheet's corner radius: square on a square
//! sheet); those editors keep the notch's corner clear. A press opens the area's menu under it (the shell's
//! `open_area_menu`). A drag carries the whole area. The other three body corners have
//! invisible split/join handles above phone width.
//!
//! The shell hands it the dock and its areas (the front tab of each) when the layout changes; it
//! draws over the dock, each nub at the top left of the tab's body as drawn this frame (the dock
//! makes a body on its first draw, so it is looked up then, not when the layout loads).
//! An outside edit lights the areas that show what it changed with a thin outline that fades
//! ([`FabAreaNotches::light`]), the AI accent's for an AI edit.
//!
//! It also says the board's, the schematic's, the 3D view's and the Scope's notices (`Notices`):
//! a chip at the foot of each area that shows that view, centred, above the editor's own bar; the
//! plate view says its own (`tools::overlay`). A press in an area clears its view's passing notice
//! said before the press (the layer sees the press before the dock and its editors do).
//!
//! Cost on the UI thread: per draw O(areas) (at most a handful), per pointer event O(areas) hit
//! checks for a nub's drag and a press's area; a chip's words set only when the notices change; a
//! frame a draw while an outline fades.

use crate::api::{AppState, NoticeView, Notices, ShellAction};
use crate::ui::editors::Editor;
use makepad_widgets_core::event::{DigitId, TouchState};
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use makepad_widgets_data::dock::DockRef;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    // The notch: 38 x 28, a tab bar's left end (its margin by its height), plus room for its
    // two fillets (8 each way); its chip where the bar's nub has its own. On a square sheet the
    // corner is 2 and there are no fillets.
    let Notch = View{
        new_batch: true
        width: 46 height: 36
        padding: Inset{left: 8 top: 4}
        show_bg: true
        draw_bg +: {
            color: fab.color_editor
            sheet: uniform(fab.radius_lg)
            // at a tab bar's left end: the bar's own ground and rule
            bar: instance(0.0)
            pixel: fn() {
                if self.bar > 0.5 {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                    sdf.fill(fab.color_header)
                    sdf.rect(0.0, self.rect_size.y - 1.0, self.rect_size.x, 1.0)
                    sdf.fill(fab.color_border)
                    return sdf.result
                }
                let p = self.pos * self.rect_size
                // the box: the bar's left end, 38 x 28
                let w = 38.0
                let h = 28.0
                let k = clamp(self.sheet / 3.5, 0.0, 1.0)
                let r = max(2.0, 11.0 * k)
                let f = 8.0 * k
                let q = p - vec2(w - r, h - r)
                let d = length(max(q, vec2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r
                let body = clamp(0.5 - d, 0.0, 1.0)
                let on = step(0.01, f)
                let top = clamp(length(p - vec2(w + f, f)) - f + 0.5, 0.0, 1.0) * step(p.x, w + f) * step(p.y, f) * on
                let left = clamp(length(p - vec2(f, h + f)) - f + 0.5, 0.0, 1.0) * step(p.x, f) * step(p.y, h + f) * on
                // (a toolbar's nub has no ground: its colour's alpha is 0)
                let a = max(body, max(top, left)) * self.color.w
                return vec4(self.color.xyz * a, a)
            }
        }
        tip := mod.widgets.FabTip{
            text: ""
            chip := mod.widgets.FabIconButton{
                width: 22 height: 20
                padding: Inset{left: 3 right: 3 top: 2 bottom: 2}
                draw_bg +: {color_hover: theme.prop_choice_on color_down: theme.prop_choice_on border_radius: fab.radius_lg}
                draw_icon +: {color: fab.color_text_dim}
            }
        }
    }

    mod.widgets.FabAreaNotchesBase = #(FabAreaNotches::register_widget(vm))
    mod.widgets.FabAreaNotches = set_type_default() do mod.widgets.FabAreaNotchesBase{
        // no room of its own and no clip: each notch draws at its area's corner
        width: 0 height: 0 clip_x: false clip_y: false
        draw_corner +: { color: #0000 }
        // an area an outside edit changed: its outline, fading
        draw_lit +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, fab.radius_lg)
                sdf.stroke(self.color, 1.5)
                return sdf.result
            }
        }
        color_outside: fab.color_text_active
        color_notice: fab.color_text_dim
        color_notice_warn: fab.color_warning
        Notice := View{
            new_batch: true
            width: Fit height: Fit
            flow: Right{wrap: true}
            padding: Inset{left: 8 right: 8 top: 4 bottom: 4}
            a11y: {role: "status"}
            show_bg: true
            draw_bg +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, fab.radius_lg)
                    sdf.fill(fab.color_float)
                    return sdf.result
                }
            }
            text := mod.widgets.FabLabelSmall{}
        }
        // a toolbar's close button, at its right end past the search: closes the area's tab
        CloseTab := mod.widgets.FabTip{
            text: "Close this tab"
            button := mod.widgets.FabIconButton{
                a11y: {label: "Close tab"}
                width: 22 height: 20
                // its glyph square and centred, on the toolbar's middle as its other icons
                align: Align{x: 0.5 y: 0.5}
                padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
                icon_walk: Walk{width: 16 height: 16}
                draw_icon +: {svg: crate_resource("self://resources/icons/close.svg") color: fab.color_text_dim}
            }
        }
        // one nub per editor kind (`Editor::kind`), its glyph and its menu name
        Viewport := Notch{tip +: {text: "Plate" chip +: {a11y: {label: "Plate: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/place_plate.svg")}}}}
        Device := Notch{tip +: {text: "3D view" chip +: {a11y: {label: "3D view: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_3d.svg")}}}}
        Plates := Notch{tip +: {text: "Plates" chip +: {a11y: {label: "Plates: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/grid.svg")}}}}
        Parts := Notch{tip +: {text: "Parts" chip +: {a11y: {label: "Parts: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/object.svg")}}}}
        Part := Notch{tip +: {text: "Part" chip +: {a11y: {label: "Part: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/cube.svg")}}}}
        Slicer := Notch{tip +: {text: "Slicer" chip +: {a11y: {label: "Slicer: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/layers.svg")}}}}
        Settings := Notch{tip +: {text: "Job" chip +: {a11y: {label: "Job: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/gear.svg")}}}}
        Printer := Notch{tip +: {text: "Printer" chip +: {a11y: {label: "Printer: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/printer.svg")}}}}
        Calibrate := Notch{tip +: {text: "Calibrate" chip +: {a11y: {label: "Calibrate: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/ruler.svg")}}}}
        Camera := Notch{tip +: {text: "Camera" chip +: {a11y: {label: "Camera: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/camera.svg")}}}}
        Features := Notch{tip +: {text: "Features" chip +: {a11y: {label: "Features: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/shape.svg")}}}}
        Code := Notch{tip +: {text: "Program" chip +: {a11y: {label: "Program: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/code.svg")}}}}
        Mechanics := Notch{tip +: {text: "Mechanics" chip +: {a11y: {label: "Mechanics: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/grab.svg")}}}}
        Slice := Notch{tip +: {text: "Slice" chip +: {a11y: {label: "Slice: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/section.svg")}}}}
        Arm := Notch{tip +: {text: "Arm" chip +: {a11y: {label: "Arm: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/link.svg")}}}}
        ArmCell := Notch{tip +: {text: "Arm cell" chip +: {a11y: {label: "Arm cell: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/nozzle.svg")}}}}
        ArmCompare := Notch{tip +: {text: "Compare" chip +: {a11y: {label: "Compare: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/swap_sides.svg")}}}}
        Force := Notch{tip +: {text: "Force" chip +: {a11y: {label: "Force: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/force.svg")}}}}
        Scan := Notch{tip +: {text: "Scan" chip +: {a11y: {label: "Scan: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/eye.svg")}}}}
        ScanView := Notch{tip +: {text: "Scan view" chip +: {a11y: {label: "Scan view: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/iso_view.svg")}}}}
        Board := Notch{tip +: {text: "Board" chip +: {a11y: {label: "Board: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_board.svg")}}}}
        Circuit := Notch{tip +: {text: "Schematic" chip +: {a11y: {label: "Schematic: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_schematic.svg")}}}}
        Bench := Notch{tip +: {text: "Bench" chip +: {a11y: {label: "Bench: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_bench.svg")}}}}
        Scope := Notch{tip +: {text: "Scope" chip +: {a11y: {label: "Scope: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_scope.svg")}}}}
        Transport := Notch{tip +: {text: "Transport" chip +: {a11y: {label: "Transport: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_transport.svg")}}}}
        Firmware := Notch{tip +: {text: "Firmware" chip +: {a11y: {label: "Firmware: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/area_firmware.svg")}}}}
        Overrides := Notch{tip +: {text: "Overrides" chip +: {a11y: {label: "Overrides: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/settings_here.svg")}}}}
        TestPrints := Notch{tip +: {text: "Test prints" chip +: {a11y: {label: "Test prints: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/closed_box.svg")}}}}
        Strength := Notch{tip +: {text: "Strength" chip +: {a11y: {label: "Strength: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/warning.svg")}}}}
        Library := Notch{tip +: {text: "Library" chip +: {a11y: {label: "Library: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/folder_open.svg")}}}}
        Checks := Notch{tip +: {text: "Checks" chip +: {a11y: {label: "Checks: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/check.svg")}}}}
        BoardControls := Notch{tip +: {text: "Board controls" chip +: {a11y: {label: "Board controls: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/gear.svg")}}}}
        Paint := Notch{tip +: {text: "Paint" chip +: {a11y: {label: "Paint: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/paint.svg")}}}}
        Jobs := Notch{tip +: {text: "Jobs" chip +: {a11y: {label: "Jobs: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/layers.svg")}}}}
        Performance := Notch{tip +: {text: "Performance" chip +: {a11y: {label: "Performance: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/speed.svg")}}}}
        ModelSources := Notch{tip +: {text: "Model sources" chip +: {a11y: {label: "Model sources: area menu"} draw_icon +: {svg: crate_resource("self://resources/icons/import.svg")}}}}
    }
}

/// One area to give a nub: its front tab, the nub's template (the front editor's kind),
/// whether the area shows its tab bar (the nub sits at the bar's left end, else in the corner)
/// and whether its editor draws a header strip across its top (the nub, in the corner, is the
/// strip's left end and has the bar's look).
#[derive(Clone, Copy, PartialEq)]
pub struct NotchedArea {
    pub tab: LiveId,
    pub kind: LiveId,
    pub bar: bool,
    pub strip: bool,
    /// Whether its front tab may close (the area has another tab, or there is another area):
    /// then its toolbar shows a close button at its right end.
    pub closable: bool,
}

/// The view that speaks in an area with this front editor.
fn notice_view(kind: LiveId) -> Option<NoticeView> {
    Some(match Editor::of_kind(kind)? {
        Editor::Viewport => NoticeView::Plate,
        Editor::Board => NoticeView::Board,
        Editor::Circuit => NoticeView::Circuit,
        Editor::Device => NoticeView::Device,
        Editor::Scope => NoticeView::Scope,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug)]
pub enum Corner {
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug)]
pub enum CornerPhase {
    Move,
    End,
    Cancel,
}

/// What a nub or an area corner asks of the shell.
#[derive(Clone, Copy, Debug, Default)]
pub enum NubAction {
    /// The nub of the area showing this tab was dragged past the drag distance: the area moves.
    Drag(LiveId),
    Corner { tab: LiveId, corner: Corner, start: DVec2, at: DVec2, phase: CornerPhase },
    #[default]
    None,
}

/// How far a nub's press moves before it drags (the dock's tabs' distance).
pub(super) const DRAG_DISTANCE: f64 = 4.0;
const CORNER_SIZE: f64 = 10.0;

#[derive(Clone, Copy)]
struct CornerPress {
    tab: LiveId,
    corner: Corner,
    window: WindowId,
    digit: DigitId,
    start: DVec2,
    dragging: bool,
}

impl CornerPress {
    fn emit(self, cx: &mut Cx, uid: WidgetUid, at: DVec2, phase: CornerPhase) {
        cx.widget_action(uid, NubAction::Corner { tab: self.tab, corner: self.corner, start: self.start, at, phase });
    }
}

/// The height of the tab bar a nub sits in (`FabPanelTabBar`).
pub(crate) const BAR_H: f64 = 28.0;

/// A panel editor's toolbar (`widgets::FabEditorBar`): its height, and the room at its left end
/// its area's nub takes (the chip, 22 wide, `TOOL_INSET` in, then a gap before the bar's rule).
pub(crate) const TOOLBAR_H: f64 = 40.0;
pub(crate) const TOOLBAR_SLOT: f64 = 34.0;
/// The room a toolbar keeps at its right end for its tab's close button (`CloseTab`), drawn by
/// this layer: the button (22) and its inset from the edge; the toolbar's last tool (its
/// search) sits right against it.
pub(crate) const TOOLBAR_END: f64 = 28.0;
const TOOL_INSET: f64 = 6.0;
/// Where a toolbar's nub draws from its editor's top-left: its chip (the template's 8, 4 in,
/// 20 high) `TOOL_INSET` in and centred in the toolbar's height.
const TOOL_AT: DVec2 = DVec2 { x: TOOL_INSET - 8.0, y: (TOOLBAR_H - 20.0) / 2.0 - 4.0 };

/// Where a nub sits in its area, and its look there.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Place {
    /// The corner of an editor that draws over its whole area (a view, the code, the camera):
    /// the notch, with its fillets.
    Corner,
    /// The left end of the 3D view's plate strip: the strip's look.
    Strip,
    /// The first button of a panel editor's toolbar (`FabEditorBar`): the chip alone, so the
    /// toolbar reads as the editor's own and the tab bar holds only the area's tabs.
    Tool,
}

/// Whether an area with this front editor shows its nub in the editor's toolbar: every panel
/// editor, the code and the plate view (each starts with a `FabEditorBar`; the plate view's is
/// its stage's top row); the other views and the camera keep the corner.
fn in_toolbar(kind: LiveId) -> bool {
    use Editor::*;
    matches!(
        Editor::of_kind(kind),
        Some(Plates | Parts | Part | Slicer | Settings | Printer | Calibrate | Mechanics | Features | Slice | Arm | Scan | Force
            | Overrides | TestPrints | Strength | Library | Checks | BoardControls | Paint | Jobs | Performance | Code | Viewport)
    )
}

#[derive(Script, Widget)]
pub struct FabAreaNotches {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[rust]
    templates: HashMap<LiveId, ScriptObjectRef>,
    /// The nub of each area's tab, the template it was made from, and where it sits.
    #[rust]
    items: ComponentMap<LiveId, (WidgetRef, LiveId, Place)>,
    /// Each toolbar's close button, by its area's front tab.
    #[rust]
    closes: ComponentMap<LiveId, WidgetRef>,
    /// A press on a nub's chip, held: its tab and where it went down. The editor menu opens on
    /// the press and takes the pointer, so the drag that moves the area is read off the raw
    /// moves (`handle_event`).
    #[rust]
    chip_press: Option<(LiveId, DVec2)>,
    /// One chip for each front tab with a notice drawn by this layer.
    #[rust]
    notice_items: HashMap<LiveId, WidgetRef>,
    #[rust]
    notice_revision: Option<u64>,
    #[live]
    color_notice: Vec4f,
    #[live]
    color_notice_warn: Vec4f,
    /// The tab of the area whose nub is being dragged, until the drag ends: its press opens no
    /// menu.
    #[rust]
    dragging: Option<LiveId>,
    #[live]
    draw_corner: DrawColor,
    #[rust]
    corners_enabled: bool,
    #[rust]
    corners: Vec<(LiveId, Corner, Area)>,
    #[rust]
    corner_press: Option<CornerPress>,
    #[rust]
    areas: Vec<NotchedArea>,
    #[rust]
    dock: Option<DockRef>,
    #[live]
    draw_lit: DrawColor,
    /// An outline's colour for an edit not known to be the AI's.
    #[live]
    color_outside: Vec4f,
    /// The editors (notch kinds) an outside edit changed, the outline's colour and when it began.
    #[rust]
    lit: Vec<(LiveId, Vec4f, f64)>,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabAreaNotches {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
            self.notice_revision = None;
            self.notice_items.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            for (_, (w, t, place)) in self.items.iter_mut() {
                if let Some(template) = self.templates.get(t) {
                    let v: ScriptValue = template.as_object().into();
                    w.script_apply(vm, apply, scope, v);
                    // the template's look is back (the corner's): the next draw places it again
                    *place = Place::Corner;
                }
            }
        }
    }
}

impl FabAreaNotches {
    pub fn set_corners_enabled(&mut self, cx: &mut Cx, enabled: bool) {
        if self.corners_enabled != enabled {
            self.corners_enabled = enabled;
            if !enabled {
                self.corners.clear();
                self.cancel_corner(cx);
            }
            self.area.redraw(cx);
        }
    }

    pub fn cancel_corner(&mut self, cx: &mut Cx) {
        if let Some(press) = self.corner_press.take() {
            press.emit(cx, self.uid, press.start, CornerPhase::Cancel);
            cx.set_cursor(MouseCursor::Default);
        }
    }

    /// The front tabs of the dock, each with its nub and its notice.
    pub fn set_areas(&mut self, cx: &mut Cx, dock: DockRef, areas: Vec<NotchedArea>) {
        self.corners.clear();
        self.items.retain(|tab, _| areas.iter().any(|a| a.tab == *tab));
        self.closes.retain(|tab, _| areas.iter().any(|a| a.tab == *tab && a.closable));
        self.notice_items.retain(|tab, _| areas.iter().any(|a| a.tab == *tab));
        self.notice_revision = None;
        self.areas = areas;
        self.dock = Some(dock);
        self.area.redraw(cx);
    }

    /// Update chip text and colour only when a notice or a front tab changes.
    fn sync_notices(&mut self, cx: &mut Cx, notices: &Notices) {
        if self.notice_revision == Some(notices.revision) {
            return;
        }
        self.notice_revision = Some(notices.revision);
        let Some(template) = self.templates.get(&live_id!(Notice)) else { return };
        for area in &self.areas {
            let notice = notice_view(area.kind).filter(|view| *view != NoticeView::Plate).and_then(|view| notices.get(view));
            let Some(notice) = notice else {
                self.notice_items.remove(&area.tab);
                continue;
            };
            let chip = self.notice_items.entry(area.tab).or_insert_with(|| {
                let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, template.as_object().into()));
                cx.widget_tree_insert_child(self.uid, LiveId::from_str_num("notice", area.tab.0), w.clone());
                w
            });
            let label = chip.label(cx, ids!(text));
            label.set_text(cx, &notice.text);
            label.set_text_color(cx, if notice.standing { self.color_notice_warn } else { self.color_notice });
        }
    }

    /// Observe the raw press before the dock receives it. The editor can raise a new notice with
    /// that same press, so only a notice older than this instant may be cleared.
    fn press(&self, cx: &mut Cx, pos: DVec2) {
        let Some(dock) = &self.dock else { return };
        for area in &self.areas {
            let mut rect = dock.item(area.tab).area().rect(cx);
            if rect.size.x <= 0.0 || rect.size.y <= 0.0 { continue }
            if area.bar {
                rect.pos.y -= BAR_H;
                rect.size.y += BAR_H;
            }
            if rect.contains(pos) {
                if let Some(view) = notice_view(area.kind) {
                    cx.action(ShellAction::ClearPassingNotice { view, before: Cx::time_now() });
                }
                break;
            }
        }
    }

    /// Outline the areas that show the editors `kinds` (notch kinds) for a few seconds, fading: an
    /// outside edit changed them, an AI's (`ai`) in the AI accent. Looked up by kind at each draw,
    /// so a layout that comes forward for the edit (Follow AI) is outlined too.
    pub fn light(&mut self, cx: &mut Cx, kinds: &[LiveId], ai: bool) {
        let color = if ai {
            let ai = fab_elec_view::canvas::looks::Tokens::for_theme(base_theme(cx) != BaseTheme::Light).ai;
            vec4(ai[0], ai[1], ai[2], ai[3])
        } else {
            self.color_outside
        };
        let now = Cx::monotonic_now();
        self.lit.retain(|(kind, ..)| !kinds.contains(kind));
        self.lit.extend(kinds.iter().map(|kind| (*kind, color, now)));
        self.area.redraw(cx);
    }

    /// The area whose chip was pressed, and the chip's rectangle (the menu opens under it). A
    /// press that dragged the area is none.
    /// The front tab whose close button (at its toolbar's right end) was clicked.
    pub fn closed(&self, cx: &Cx, actions: &Actions) -> Option<LiveId> {
        self.closes.iter().find_map(|(tab, w)| w.button(cx, ids!(button)).clicked(actions).then_some(*tab))
    }

    pub fn pressed(&self, cx: &Cx, actions: &Actions) -> Option<(LiveId, Rect)> {
        if self.dragging.is_some() {
            return None;
        }
        // the editor menu opens as the chip is pressed (not on its release)
        self.items.iter().find_map(|(tab, (w, ..))| {
            let chip = w.button(cx, ids!(chip));
            chip.pressed(actions).then(|| (*tab, chip.area().rect(cx)))
        })
    }

    /// The front tab of the area whose nub began a drag.
    pub fn dragged(&self, actions: &Actions) -> Option<LiveId> {
        actions.filter_widget_actions_cast::<NubAction>(self.uid).find_map(|action| match action {
            NubAction::Drag(tab) => Some(tab),
            _ => None,
        })
    }

    /// A corner takes the press through its quad, then hands capture to the layer. The layer's
    /// area survives a preview even when the pressed tab disappears. Captured moves use one hit
    /// test. Hover, the initial press and the cursor after release search the zones.
    fn corner_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if let Some(press) = self.corner_press.as_mut() {
            let cancel = match event {
                Event::KeyDown(e) => e.key_code == KeyCode::Escape,
                Event::WindowLostFocus(id) => *id == press.window,
                Event::WindowClosed(e) => e.window_id == press.window,
                Event::DragEnd | Event::Pause | Event::Background => true,
                _ => false,
            };
            if cancel {
                self.cancel_corner(cx);
                return true;
            }
            match event.hits(cx, self.area) {
                Hit::FingerMove(e) if e.digit_id == press.digit => {
                    if e.window_id == press.window && (press.dragging || (e.abs - press.start).length() > DRAG_DISTANCE) {
                        press.dragging = true;
                        cx.set_cursor(MouseCursor::Grabbing);
                        press.emit(cx, self.uid, e.abs, CornerPhase::Move);
                    }
                    return true;
                }
                Hit::FingerUp(e) if e.digit_id == press.digit => {
                    let over_window = e.window_id == press.window && cx.windows.is_valid(press.window) && {
                        let size = cx.windows[press.window].get_inner_size();
                        e.abs.x >= 0.0 && e.abs.x < size.x && e.abs.y >= 0.0 && e.abs.y < size.y
                    };
                    let phase = if over_window && !e.cancelled && !e.is_sweep { CornerPhase::End } else { CornerPhase::Cancel };
                    press.emit(cx, self.uid, e.abs, phase);
                    self.corner_press = None;
                    let over_corner = over_window && self.corners.iter().any(|(_, _, zone)| zone.clipped_rect(cx).contains(e.abs));
                    cx.set_cursor(if over_corner { MouseCursor::Hand } else { MouseCursor::Default });
                    return true;
                }
                _ if matches!(event, Event::MouseMove(_) | Event::TouchUpdate(_)) && !cx.fingers.is_area_captured(self.area) => {
                    self.cancel_corner(cx);
                    return true;
                }
                _ => return false,
            }
        }
        let check_zones = match event {
            Event::MouseDown(e) => e.button.is_primary(),
            Event::MouseMove(_) | Event::TouchUpdate(_) => true,
            _ => false,
        };
        if !self.corners_enabled || !check_zones {
            return false;
        }
        for &(tab, corner, zone) in &self.corners {
            match event.hits(cx, zone) {
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                    cx.set_cursor(MouseCursor::Hand);
                    return true;
                }
                Hit::FingerDown(e) if e.device.is_primary_hit() => {
                    if cx.switch_finger_capture(zone, self.area, Area::Empty) {
                        let press = CornerPress { tab, corner, window: e.window_id, digit: e.digit_id, start: e.abs, dragging: false };
                        self.corner_press = Some(press);
                        cx.claim_finger_drag(e.digit_id);
                        cx.set_cursor(MouseCursor::Hand);
                        // The shell snapshots the original geometry at the press.
                        press.emit(cx, self.uid, e.abs, CornerPhase::Move);
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    fn draw_corners(&mut self, cx: &mut Cx2d, tab: LiveId, rect: Rect) {
        let size = dvec2(CORNER_SIZE.min(rect.size.x), CORNER_SIZE.min(rect.size.y));
        let far = rect.pos + rect.size - size;
        for (corner, pos) in [
            (Corner::TopRight, dvec2(far.x, rect.pos.y)),
            (Corner::BottomLeft, dvec2(rect.pos.x, far.y)),
            (Corner::BottomRight, far),
        ] {
            // Each quad owns its own hit area. Capture lives on the layer across redraws.
            self.draw_corner.draw_vars.area = Area::Empty;
            self.draw_corner.draw_abs(cx, Rect { pos, size });
            self.corners.push((tab, corner, self.draw_corner.area()));
        }
    }

    /// The close button of `tab`'s toolbar.
    fn close(&mut self, cx: &mut Cx, tab: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.closes.get(&tab) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&live_id!(CloseTab))?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, LiveId::from_str(&format!("close:{}", tab.0)), w.clone());
        self.closes.insert(tab, w.clone());
        Some(w)
    }

    /// The nub of `tab`'s area as `kind`, at its `place`.
    fn notch(&mut self, cx: &mut Cx, tab: LiveId, kind: LiveId, place: Place) -> Option<WidgetRef> {
        if let Some((w, t, p)) = self.items.get_mut(&tab) {
            if *t == kind {
                if *p != place {
                    *p = place;
                    Self::place(cx, &mut w.clone(), place);
                }
                return Some(w.clone());
            }
        }
        let template = self.templates.get(&kind)?;
        let value: ScriptValue = template.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        if place != Place::Corner {
            Self::place(cx, &mut w.clone(), place);
        }
        // a node under the notches: the semantic tree and `ui act` find each chip
        cx.widget_tree_insert_child(self.uid, tab, w.clone());
        self.items.insert(tab, (w.clone(), kind, place));
        Some(w)
    }

    /// A nub's look for its place: on the strip, the strip's ground; in the corner, the notch and
    /// its fillets; in a toolbar, the chip alone, centred in the toolbar's height.
    fn place(cx: &mut Cx, w: &mut WidgetRef, place: Place) {
        match place {
            Place::Strip => script_apply_eval!(cx, w, {
                width: 38 height: #(BAR_H)
                draw_bg +: {bar: 1.0 color: mod.fab.color_editor}
            }),
            Place::Corner => script_apply_eval!(cx, w, {
                width: 46 height: 36
                draw_bg +: {bar: 0.0 color: mod.fab.color_editor}
            }),
            // (the chip keeps the template's inset; the layer draws the nub offset: `TOOL_AT`)
            Place::Tool => script_apply_eval!(cx, w, {
                width: #(TOOLBAR_SLOT) height: 36
                draw_bg +: {bar: 0.0 color: #x00000000}
            }),
        }
    }
}

impl Widget for FabAreaNotches {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // a chip pressed and the pointer moved on before the release: the area moves (its menu,
        // opened by the press, is closed by the shell as the drag starts)
        match event {
            Event::MouseDown(e) if e.button.is_primary() => {
                self.chip_press = self.items.iter().find_map(|(tab, (w, ..))| {
                    let chip = w.button(cx, ids!(chip)).area();
                    (chip.is_valid(cx) && chip.rect(cx).contains(e.abs)).then_some((*tab, e.abs))
                });
            }
            Event::MouseMove(e) => {
                if let Some((tab, at)) = self.chip_press {
                    if self.dragging.is_none() && (e.abs - at).length() > DRAG_DISTANCE {
                        self.chip_press = None;
                        self.dragging = Some(tab);
                        cx.widget_action(self.uid, NubAction::Drag(tab));
                    }
                }
            }
            Event::MouseUp(_) => self.chip_press = None,
            _ => {}
        }
        match event {
            Event::MouseDown(e) => self.press(cx, e.abs),
            Event::TouchUpdate(e) => {
                for touch in e.touches.iter().filter(|t| t.state == TouchState::Start) {
                    self.press(cx, touch.abs);
                }
            }
            _ => {}
        }
        if scope.data.get::<AppState>().is_some_and(|s| self.notice_revision != Some(s.ui.notices.revision)) {
            self.area.redraw(cx);
        }
        let captured = self.corner_press.is_some();
        if captured && self.corner_event(cx, event) {
            return;
        }
        for (_, (w, ..)) in self.items.iter_mut() {
            w.handle_event(cx, event, scope);
        }
        for (_, w) in self.closes.iter_mut() {
            w.handle_event(cx, event, scope);
        }
        if !captured && self.corner_event(cx, event) {
            return;
        }
        // a nub's press that moves drags its area (the chip keeps its click for the menu)
        if self.dragging.is_none() {
            let mut start = None;
            for (tab, (w, ..)) in self.items.iter() {
                let chip = w.button(cx, ids!(chip)).area();
                if let Hit::FingerMove(e) = event.hits(cx, chip) {
                    if e.device.is_primary_hit() && (e.abs - e.abs_start).length() > DRAG_DISTANCE {
                        start = Some(*tab);
                    }
                }
            }
            if let Some(tab) = start {
                self.dragging = Some(tab);
                cx.widget_action(self.uid, NubAction::Drag(tab));
            }
        }
        if matches!(event, Event::DragEnd) {
            self.dragging = None;
        }
        if matches!(event, Event::NextFrame(_)) && !self.lit.is_empty() {
            self.area.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        cx.begin_turtle(walk, self.layout);
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync_notices(cx, &state.ui.notices);
        }
        let dock = self.dock.clone();
        // the outlines first, so the notches' chips sit over them
        let now = Cx::monotonic_now();
        self.lit.retain(|(_, _, at)| now - at < fab_elec_view::firmware::FLASH_SECONDS);
        for (kind, color, at) in self.lit.clone() {
            let fade = (1.0 - (now - at) / fab_elec_view::firmware::FLASH_SECONDS).clamp(0.0, 1.0) as f32;
            for area in self.areas.iter().filter(|area| area.kind == kind) {
                let Some(body) = dock.as_ref().map(|dock| dock.item(area.tab)) else { break };
                let rect = body.area().rect(cx);
                if rect.size.x >= 1.0 && rect.size.y >= 1.0 {
                    self.draw_lit.color = vec4(color.x, color.y, color.z, color.w * fade);
                    self.draw_lit.draw_abs(cx, rect);
                }
            }
        }
        if !self.lit.is_empty() {
            cx.new_next_frame();
        }
        self.corners.clear();
        for area in self.areas.clone() {
            let Some(body) = dock.as_ref().map(|dock| dock.item(area.tab)) else { break };
            let rect = body.area().rect(cx);
            if rect.size.x < 1.0 || rect.size.y < 1.0 {
                continue;
            }
            if self.corners_enabled {
                self.draw_corners(cx, area.tab, rect);
            }
            // (the plate view in Mechanics: its header strip is its toolbar, the chip as any)
            let place = if in_toolbar(area.kind) { Place::Tool } else if area.strip { Place::Strip } else { Place::Corner };
            if let Some(w) = self.notch(cx, area.tab, area.kind, place) {
                let at = if place == Place::Tool { rect.pos + TOOL_AT } else { rect.pos };
                let walk = w.walk(cx).with_abs_pos(at);
                w.draw_walk_all(cx, scope, walk);
            }
            // its toolbar's right end: the button that closes the tab, where it may close
            if place == Place::Tool && area.closable {
                if let Some(close) = self.close(cx, area.tab) {
                    let at = dvec2(rect.pos.x + rect.size.x - TOOL_INSET - 22.0, rect.pos.y + (TOOLBAR_H - 20.0) / 2.0);
                    let walk = close.walk(cx).with_abs_pos(at);
                    close.draw_walk_all(cx, scope, walk);
                }
            }
            // The notice at the area's foot, centred, over the editor's own bar when it has one
            // (the board's and the schematic's): the top row is the editors' controls.
            if let Some(chip) = self.notice_items.get(&area.tab) {
                let foot = if matches!(notice_view(area.kind), Some(NoticeView::Board | NoticeView::Circuit)) { BAR_H } else { 0.0 };
                let width = rect.size.x - 16.0;
                if width > 0.0 && rect.size.y > foot + 40.0 {
                    let over = Walk::new(Size::Fixed(rect.size.x), Size::Fixed(rect.size.y - foot)).with_abs_pos(rect.pos);
                    let pad = Inset { left: 8.0, right: 8.0, top: 8.0, bottom: 8.0 };
                    cx.begin_turtle(over, Layout { align: Align { x: 0.5, y: 1.0 }, padding: pad, ..Layout::default() });
                    let mut walk = chip.walk(cx);
                    walk.width = Size::Fit { min: None, max: Some(FitBound::Abs(width)) };
                    chip.draw_walk_all(cx, scope, walk);
                    cx.end_turtle();
                }
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
