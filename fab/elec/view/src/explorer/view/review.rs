//! A held proposal borrows the camera. Its layout has a separate pool slot and retained
//! ink; the live sheet, its selection and its owners stay installed throughout comparison.
use super::*;
use super::follow::SheetSource;
use crate::canvas::pane::review_chip;
use crate::snapshot::ProposalView;

#[derive(Default)]
pub(super) struct SheetLayers {
    pub(super) boxes: CanvasLayer,
    pub(super) lines: CanvasLayer,
    pub(super) arcs: CanvasLayer,
    pub(super) bridges: CanvasLayer,
    pub(super) leaders: CanvasLayer,
}

impl SheetLayers {
    pub(super) fn publish(&mut self, scene: &Scene, t: &Templates) {
        macro_rules! publish {
            ($layer:ident, $template:ident) => {{
                let mut rows = Vec::with_capacity(scene.$layer.len() * t.$template.slots());
                for row in &scene.$layer { t.$template.append(&mut rows, row); }
                self.$layer.set(t.$template.slots(), rows);
            }};
        }
        publish!(boxes, boxes);
        publish!(lines, line);
        publish!(arcs, arc);
        publish!(bridges, bridge);
        publish!(leaders, line);
    }
}

/// The finished draw streams and the layout whose labels they use.
pub(super) struct StaticSheet {
    sheet: Arc<SheetLayout>,
    grounds: CanvasLayer,
    reveal: Option<Reveal>,
    scene: Scene,
    ink: SheetLayers,
    legible: f64,
    dark: bool,
}

/// The same drawing path can borrow the live pane's installed streams or a static sheet's.
struct Drawing<'a> {
    sheet: &'a SheetLayout,
    scene: &'a mut Scene,
    grounds: &'a mut CanvasLayer,
    reveal: Option<&'a mut Reveal>,
    ink: &'a mut SheetLayers,
    legible: &'a mut f64,
}

impl StaticSheet {
    fn drawing(&mut self) -> Drawing<'_> {
        Drawing { sheet: &self.sheet, scene: &mut self.scene, grounds: &mut self.grounds,
            ink: &mut self.ink, legible: &mut self.legible, reveal: self.reveal.as_mut() }
    }
}

enum Layout {
    Missing,
    Shared,
    Preparing(LayoutJobs),
    Failed,
    Ready(Laid),
}

pub(super) struct Comparison {
    mode: SheetMode,
    pub view: Arc<ProposalView>,
    source: Option<SheetSource>,
    layout: Layout,
    picture: Option<StaticSheet>,
    emphasis: Option<crate::ItemStates>,
    pub(super) off: CanvasLive,
    pub caption: String,
    removed: bool,
    pub deferred: Vec<LayoutResult>,
}

impl Layout {
    /// Use the live sheet until the proposed sheet is available. A removed unit is empty.
    fn displayed<'a>(&'a self, live: Option<&'a Arc<SheetLayout>>) -> Option<&'a Arc<SheetLayout>> {
        match self {
            Layout::Missing => None,
            Layout::Ready(laid) => Some(&laid.sheet),
            _ => live,
        }
    }

    fn candidate_ready(&self) -> bool { matches!(self, Layout::Shared | Layout::Ready(_)) }
}

impl Comparison {
    pub(super) fn release(&mut self, cx: &mut Cx) {
        if let Some(mut rows) = self.emphasis.take() { rows.release(cx); }
    }

    fn poll(&mut self, cx: &mut Cx) -> bool {
        let Layout::Preparing(jobs) = &mut self.layout else { return false };
        if let Some(LayoutResult::Layout(laid)) = jobs.poll(cx) {
            self.layout = Layout::Ready(laid);
            return true;
        }
        if !jobs.busy() {
            self.layout = Layout::Failed;
            return true;
        }
        false
    }

    fn caption(&mut self, cx: &mut Cx, chip: &WidgetRef) {
        let progress = match self.layout {
            Layout::Preparing(_) => Some("laying out the proposal"),
            Layout::Failed => Some("could not lay out the proposal"),
            _ => None,
        };
        self.caption = review_chip::caption(&self.view, self.removed, progress);
        chip.label(cx, ids!(preview_caption)).set_text(cx, &self.caption);
    }

    fn set_emphasis(&mut self, cx: &mut Cx) {
        let mut lit = Vec::new();
        if let Some(s) = &self.source {
            let u = &s.design.units[s.unit];
            for sel in self.view.created.iter() {
                crate::sheet_rows(u.id, &u.circuit, u.board.as_deref(), &s.rows.rows, sel, &mut lit);
            }
            if !lit.is_empty() {
                lit.sort_unstable();
                lit.dedup();
                self.emphasis.get_or_insert_with(crate::ItemStates::new).write(s.rows.count as usize,
                    lit.into_iter().map(|row| (row, [0., 0., 0., 1.])).collect());
                return;
            }
        }
        if let Some(mut old) = self.emphasis.take() { old.release(cx); }
    }
}

impl ExplorerView {
    pub(super) fn comparison_paused(&self, cx: &Cx) -> bool {
        self.review.is_some() || self.return_picture.is_some() || (!self.pinned && cx.get_global_ref::<ElecLive>().is_some_and(|h| h.previewed().is_some()))
    }

    pub(super) fn follow_comparison(&mut self, cx: &mut Cx, view: Arc<ProposalView>, source: Option<SheetSource>, assigned: bool) {
        let same = self.review.as_ref().is_some_and(|r| r.mode == self.sheet_mode && match (&r.source, &source) {
            (Some(a), Some(b)) => a.same_sheet(b),
            (None, None) => true,
            _ => false,
        });
        let created_changed = self.review.as_ref().is_none_or(|r| !Arc::ptr_eq(&r.view.created, &view.created)
            || r.source.as_ref().zip(source.as_ref()).is_some_and(|(a,b)| !Arc::ptr_eq(&a.design, &b.design)));
        let mut review = self.review.take().unwrap_or_else(|| {
            // Finish transient input without applying its pending layouts or any field text.
            let mut deferred = Vec::new();
            deferred.extend(self.pending_sheet.take().map(LayoutResult::Layout));
            deferred.extend(self.pending_reveal.take().map(LayoutResult::Reveal));
            self.close_value_edit(cx);
            self.end_marquee(cx);
            self.end_scrub(cx, false);
            if self.gesture.as_ref().is_some_and(|g| !g.phase().landing()) { self.escape_gesture(cx); }
            if self.drag.held() { self.drag.cancel(cx, self.uid, cx.seconds_since_app_start()); }
            self.end_carry(cx);
            if let Some(cmd) = self.live_press.up() { cx.action(SimAction::Cmd(cmd)); }
            self.card_press = None;
            self.wire_press = None;
            self.wire_click = None;
            self.flag_ghost = None;
            self.pane.nav.stop();
            let (picture, off) = match self.return_picture.take() {
                Some((picture, off)) => (Some(picture), off),
                None => (None, CanvasLive::off(cx)),
            };
            Comparison { mode: self.sheet_mode, view: view.clone(), source: None, layout: Layout::Missing, picture,
                emphasis: None, off, caption: String::new(), removed: false, deferred }
        });
        review.removed = assigned && source.is_none();
        if !same {
            review.layout = match &source {
                None => Layout::Missing,
                Some(s) if self.laid_mode == self.sheet_mode && self.applied_gen == self.gen && self.sheet_design.is_some() &&
                    self.followed.as_ref().is_some_and(|f| f.matches(s)) => Layout::Shared,
                Some(s) => {
                    super::super::font_metrics::measure_once(cx, &mut self.draw_text);
                    let mut jobs = LayoutJobs::default();
                    jobs.request(cx, s.design.clone(), s.unit, SheetHints::default(), None, None,
                        Keep { shift: self.shift, block: None }, 1, self.layout_opts());
                    if jobs.busy() { Layout::Preparing(jobs) } else { Layout::Failed }
                }
            };
        }
        review.mode = self.sheet_mode;
        review.view = view;
        review.source = source;
        if !same || created_changed { review.set_emphasis(cx); }
        review.caption(cx, &self.review_chip);
        self.review = Some(review);
        self.draw_bg.redraw(cx);
    }

    pub(super) fn leave_comparison(&mut self, cx: &mut Cx, next: Option<&SheetSource>) -> Option<Laid> {
        let mut review = self.review.take()?;
        review.release(cx);
        if self.pane.placed {
            self.refit = false;
            self.refit_after_layout = false;
            self.opening_view = None;
            if self.applied_gen < self.gen { self.keep_camera_gen = Some(self.gen); }
        }
        // Accept uses the sheet the person saw, including its worker-prepared Live wiring.
        if review.source.as_ref().zip(next).is_some_and(|(a,b)| a.same_sheet(b)) {
            review.poll(cx);
            if let Layout::Ready(laid) = review.layout { return Some(laid); }
        }
        // A pane first opened in comparison (or changing live units) can have no live sheet
        // to restore. Keep its last finished streams until that unit's first live layout.
        let installed = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit));
        if !matches!(review.layout, Layout::Missing) && next.map_or(installed.is_none(), |s| {
            let u = &s.design.units[s.unit];
            installed.is_none_or(|old| old.id != u.id || old.name != u.name || self.unit != s.unit)
        }) {
            let last = if review.layout.candidate_ready() || installed.is_none() { review.picture } else {
                Some(StaticSheet { sheet: self.sheet.clone(), scene: std::mem::take(&mut self.scene),
                    ink: std::mem::take(&mut self.ink), grounds: std::mem::take(&mut self.frame_fills),
                    legible: self.legible, dark: self.dark, reveal: self.reveal.take() })
            };
            self.return_picture = last.map(|p| (p, review.off));
        }
        None
    }

    fn toggle_comparison_blocks(&mut self, cx: &mut Cx, view: Arc<ProposalView>) {
        self.toggle_blocks(cx);
        // The same proposal Arc can now need a different layout.
        if let Some(host) = cx.get_global_ref::<ElecLive>() {
            let displayed = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit))
                .map(|u| (u.id, u.name.as_str()));
            let source = super::follow::source(host, Some(&view), displayed);
            self.follow_comparison(cx, view, source, displayed.is_some());
        }
    }

    pub(super) fn comparison_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.frame.is_event(event).is_some() {
            self.frame = NextFrame::default();
            if self.return_picture.is_some() {
                self.assemble_pending = false;
                self.draw_bg.redraw(cx);
            }
        }
        let chosen = cx.get_global_ref::<ElecLive>().and_then(ElecLive::previewed).cloned();
        if matches!(event, Event::Signal) {
            if chosen.is_some() || self.review.is_none() { self.follow(cx); }
            if let Some(review) = &mut self.review {
                if review.poll(cx) { review.caption(cx, &self.review_chip); self.draw_bg.redraw(cx); }
                if let Some(result) = self.layout_jobs.poll(cx) { review.deferred.push(result); }
            } else if let Some(result) = self.layout_jobs.poll(cx) {
                self.layout_result(cx, result);
            }
        }
        if self.review.is_some() {
            // Only the display mode is interactive on a proposal's read-only sheet.
            let toggle = self.tools.widget(cx, ids!(a_blocks_tip));
            let actions = cx.capture_actions(|cx| toggle.handle_event(cx, event, scope));
            if self.tools.radio_button(cx, ids!(a_blocks)).clicked(&actions) {
                if let Some(view) = chosen { self.toggle_comparison_blocks(cx, view); }
                return;
            }
            cx.extend_actions(actions);
            if over_strip(cx, &toggle, &[ids!(a_blocks)], event, false) { return; }
        }
        let shown = chosen.as_ref().zip(self.review.as_ref()).is_some_and(|(p,r)| p.id == r.view.id);
        let mut back = shown && review_chip::back(cx, event, scope, &self.review_chip);
        for input in self.pane.navigation(cx, event, self.draw_bg.area()) {
            match input {
                CanvasInput::Escape => back = true,
                CanvasInput::Camera => self.draw_bg.redraw(cx),
                _ => {}
            }
        }
        if back && chosen.is_some() { ElecLive::select_preview(cx, None); }
    }

    pub(super) fn draw_comparison(&mut self, cx: &mut Cx2d, scope: &mut Scope, rect: Rect, tokens: &Tokens) {
        let (drawing, live) = if let Some(review) = &mut self.review {
            let mut live = review.off.clone();
            let sheet = review.layout.displayed(self.sheet_design.as_ref().map(|_| &self.sheet));
            let drawing = if let (Some(sheet), Some(source), Some(t)) =
                (sheet, review.source.as_ref().filter(|_| review.layout.candidate_ready()), &self.templates) {
                if review.picture.as_ref().is_none_or(|p| p.dark != self.dark || !Arc::ptr_eq(&p.sheet, sheet)) {
                    let scene = Scene::build(&source.design, source.unit, sheet, &source.rows.rows, tokens,
                        &SceneState::default(), &SceneLive::default());
                    let mut ink = SheetLayers::default();
                    ink.publish(&scene, t);
                    review.picture = Some(StaticSheet { sheet: sheet.clone(), grounds: CanvasLayer::default(),
                        scene, ink, legible: 0., dark: self.dark, reveal: None });
                }
                if let Some(states) = &mut review.emphasis { live = live.with_items(states.texture(cx)); }
                review.picture.as_mut().map(StaticSheet::drawing)
            } else if let Some(sheet) = sheet {
                Some(Drawing { sheet, scene: &mut self.scene, grounds: &mut self.frame_fills,
                    ink: &mut self.ink, legible: &mut self.legible, reveal: self.reveal.as_mut() })
            } else if matches!(review.layout, Layout::Missing) {
                None
            } else {
                review.picture.as_mut().map(StaticSheet::drawing)
            };
            (drawing, live)
        } else {
            let (picture, off) = self.return_picture.as_mut().unwrap();
            (Some(picture.drawing()), off.clone())
        };
        if let Some(d) = &drawing {
            self.pane.place_comparison(cx, rect, d.sheet.bounds.map(f64::from));
        }
        self.draw_bg.draw_grid(cx, rect, self.pane.placed.then_some(&self.pane.cam), tokens);
        if let Some(Drawing { sheet, scene, ink, grounds, legible, reveal }) = drawing {
            let now = cx.seconds_since_app_start();
            let cam = &self.pane.cam;
            let scale = label_scale(cam);
            if scale != *legible {
                scene.apply_legible(&legible_labels(sheet, scale), scale);
                *legible = scale;
            }
            let uniforms = CanvasUniforms { cam: cam.uniforms(), motion_t: now as f32, hover_item: -1., hover_mix: 0.,
                drag_item: -1., drag_offset: [0.; 2], layer_alpha: 1., time: now as f32, live_time: 0., light: !self.dark,
                clip: [rect.pos.x as f32, rect.pos.y as f32, (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32] };
            let sheet_alpha = reveal.as_ref().map_or(1., |r| r.sheet_alpha(now));
            CanvasUniforms { layer_alpha: sheet_alpha, ..uniforms }.with(tokens, |u| {
                grounds.draw(cx, &mut self.draw_box.draw_vars, u, &live);
                ink.boxes.draw(cx, &mut self.draw_box.draw_vars, u, &live);
                ink.lines.draw(cx, &mut self.draw_line.draw_vars, u, &live);
                ink.arcs.draw(cx, &mut self.draw_arc.draw_vars, u, &live);
                ink.bridges.draw(cx, &mut self.draw_bridge.draw_vars, u, &live);
                if scale == 1. && text_points(sheet::TEXT_NET, cam.px_per_unit) >= TEXT_MIN_POINTS {
                    ink.leaders.draw(cx, &mut self.draw_line.draw_vars, u, &live);
                }
            });
            let mut labels = |cx: &mut Cx2d, scene: &Scene, alpha, list: &mut Option<DrawList2d>| {
                let turn = UpTurn::of(cam.view);
                for up in [None, Some(turn)] {
                    match up { None => cx.push_clip_rect(rect), Some(turn) => turn.begin(cx, list) }
                    draw_scene_text(cx, &mut self.draw_text, cam, scene, now, alpha, None, None, up);
                    match up { None => cx.pop_clip_rect(), Some(turn) => turn.end(cx, list) }
                }
            };
            labels(cx, scene, sheet_alpha, &mut self.up_lists[0]);
            // A live relation sheet can be the last picture under the camera, too.
            // Its finished streams use the same off textures as the underlying sheet.
            if let Some(reveal) = reveal {
                reveal.draw_layers(cx, &uniforms, cam, tokens, &live, &mut self.draw_box, &mut self.draw_line,
                    &mut self.draw_arc, &mut self.draw_bridge, &mut self.draw_mini, &mut self.draw_pin, -1.);
                labels(cx, &reveal.scene, 1., &mut self.up_lists[1]);
            }
        }
        if self.review.is_some() {
            review_chip::draw(cx, scope, self.uid, &self.review_chip, rect);
            let below = self.review_chip.area().rect(cx).size.y + 4.0;
            let toggle = self.tools.widget(cx, ids!(a_blocks_tip));
            draw_strip(cx, scope, self.uid, &toggle, Rect {
                pos: rect.pos + dvec2(0.0, below),
                size: dvec2(rect.size.x, (rect.size.y - below).max(0.0)),
            });
        }
        self.pane.animate(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::gesture::tests::snapshot;
    use crate::snapshot::{ProposalAuthor, ProposalId, ProposalState, StateOf, Tagged};
    use crate::SheetIndex;

    #[test]
    fn comparing_a_sheet_keeps_the_camera_selection_and_live_owners() {
        let design = Arc::new(fab_elec_fixtures::ledctl());
        let unit = &design.units[0];
        let mut proposed = (*design).clone();
        let circuit = Arc::make_mut(&mut proposed.units[0].circuit);
        let selected = vec![(0, circuit.comps[0].path.clone())];
        circuit.comps[0].path.push_str("_proposed");
        let index = |c: &Circuit| {
            let mut rows = SheetRows::new();
            Arc::new(SheetIndex { rows: rows.sync(c), count: rows.count() })
        };
        let rows = index(&unit.circuit);
        let changed_rows = index(circuit);
        let proposed = Arc::new(proposed);
        let view = Arc::new(ProposalView {
            id: ProposalId(1), shown: 1, slot: "sheet".into(), label: "Circuit".into(),
            by: ProposalAuthor::Local(fab_elec_design::By::Person), state: ProposalState::Ready,
            run: Arc::default(), docs: Arc::default(), checks: None, requirements: None, design: Some(proposed.clone()),
            boards: Arc::default(), sheets: Arc::new([(unit.id, changed_rows.clone())].into()),
            created: Arc::new(vec![Sel::Comp(CompRef { board: unit.id, comp: CompId(0) })]),
        });
        let mut snap = snapshot(design.clone());
        snap.sheets.push(Tagged { unit: Some(unit.id), of: StateOf::Committed, generation: 0, value: rows.clone() });
        snap.proposals = Arc::new(vec![view.clone()]);
        let live = ElecLive::new(Arc::new(snap));
        let chosen = follow::source(&live, None, None).unwrap();
        assert!(Arc::ptr_eq(&chosen.design, &design) && Arc::ptr_eq(&chosen.rows, &rows), "live following pairs its design with committed rows");
        let first = follow::source(&live, Some(&view), None).unwrap();
        assert!(first.unit == 0 && Arc::ptr_eq(&first.design, &proposed) && Arc::ptr_eq(&first.rows, &changed_rows),
            "an unassigned pane follows the first proposed sheet and its own rows");
        assert!(follow::source(&live, Some(&view), Some((BoardId(unit.id.0 + 1), "removed"))).is_none(),
            "a removed displayed unit never falls back to a different proposed sheet");

        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut pane = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
            crate::live::script_mod(vm);
            crate::board::script_mod(vm);
            crate::explorer::view::script_mod(vm);
            ExplorerView::script_new_with_default(vm)
        });
        cx.set_global(live);
        let pass = DrawPass::new(&mut cx);
        let mut list = DrawList2d::new(&mut cx);
        let mut draw = |cx: &mut Cx, pane: &mut ExplorerView| {
            let size = dvec2(480., 320.);
            pass.set_size(cx, size);
            let event = DrawEvent::default();
            let mut frame = CxDraw::new(cx, &event);
            let mut cx = Cx2d::new(&mut frame);
            cx.begin_pass(&pass, Some(1.));
            list.begin_always(&mut cx);
            cx.begin_root_turtle(size, makepad_widgets_core::Layout::flow_down());
            pane.draw_walk_all(&mut cx, &mut Scope::empty(), Walk::fill());
            cx.end_pass_sized_turtle();
            list.end(&mut cx);
            cx.end_pass(&pass);
        };
        let finish_candidate = |cx: &mut Cx, pane: &mut ExplorerView| {
            while matches!(pane.review.as_ref().unwrap().layout, Layout::Preparing(_)) {
                pane.comparison_event(cx, &Event::Signal, &mut Scope::empty());
                std::thread::yield_now();
            }
        };
        ElecLive::select_preview(&mut cx, Some(view.id));
        pane.follow(&mut cx);
        finish_candidate(&mut cx, &mut pane);
        draw(&mut cx, &mut pane);
        pane.toggle_comparison_blocks(&mut cx, view.clone());
        finish_candidate(&mut cx, &mut pane);
        draw(&mut cx, &mut pane);
        let blocks = &pane.review.as_ref().unwrap().picture.as_ref().expect("the same proposal must redraw in Blocks mode").sheet;
        assert!(blocks.parts.iter().all(|p| !p.placed) && blocks.frames.iter().skip(1).any(|f| f.placed),
            "a mode change must collapse the held candidate without changing proposal identity");
        pane.toggle_comparison_blocks(&mut cx, view.clone());
        finish_candidate(&mut cx, &mut pane);
        draw(&mut cx, &mut pane);
        let first_picture = pane.review.as_ref().unwrap().picture.as_ref().expect("a fresh preview draws the finished candidate").sheet.clone();
        assert!(pane.pane.placed && pane.sheet_design.is_none(), "a pane opened in comparison fits the candidate before its first live sheet");
        ElecLive::select_preview(&mut cx, None);
        draw(&mut cx, &mut pane);
        assert!(pane.review.is_none() && pane.return_picture.as_ref().is_some_and(|(p, _)| Arc::ptr_eq(&p.sheet, &first_picture))
            && pane.comparison_paused(&cx), "Back removes the chip and keeps the last drawing read-only until the first live layout arrives");
        let laid = loop {
            if let Some(result) = pane.layout_jobs.poll(&mut cx) { break result; }
            assert!(pane.layout_jobs.busy(), "the initial live sheet has a layout in flight");
            std::thread::yield_now();
        };
        pane.layout_result(&mut cx, laid);
        draw(&mut cx, &mut pane);
        assert!(!pane.comparison_paused(&cx) && !Arc::ptr_eq(&pane.sheet, &first_picture),
            "drawing the finished live sheet restores editing");

        pane.pane.keeps_fit = true;
        pane.pane.cam.center = [7., 9.];
        pane.pane.cam.px_per_unit = 11.;
        let camera = pane.pane.cam;
        pane.set_selection(&mut cx, selected.clone());
        let sheet = pane.sheet.clone();
        let owners = |cx: &Cx| {
            let sim = &cx.get_global_ref::<ElecLive>().unwrap().sim;
            (sim.trees.version(), sim.minis.generation())
        };
        let registered = owners(&cx);
        assert_eq!(pane.live_owner, Some(TreeOwner::Sheet(unit.id, 0)), "the starting sheet has a real live wiring owner");
        ElecLive::select_preview(&mut cx, Some(view.id));
        let actions = cx.capture_actions(|cx| pane.follow(cx));
        let pending = pane.review.as_ref().unwrap();
        assert!(!pending.layout.candidate_ready() && Arc::ptr_eq(pending.layout.displayed(Some(&sheet)).unwrap(), &sheet)
            && pending.caption.ends_with("laying out the proposal"), "the live picture stays visible and unaccented until the candidate layout arrives");
        assert!(actions.is_empty() && pane.selected == selected, "previewing a changed part path neither rebinds nor publishes the live selection");
        assert_eq!(pane.pane.cam, camera, "entering comparison keeps the fitted camera");

        ElecLive::select_preview(&mut cx, None);
        pane.follow(&mut cx);
        assert!(pane.review.is_none() && Arc::ptr_eq(&pane.sheet, &sheet) && !pane.layout_jobs.busy(),
            "Back during layout returns to the installed live sheet without another job");
        ElecLive::select_preview(&mut cx, Some(view.id));
        pane.follow(&mut cx);
        finish_candidate(&mut cx, &mut pane);
        draw(&mut cx, &mut pane);
        let ready = pane.review.as_ref().unwrap();
        let laid = ready.layout.displayed(Some(&sheet)).unwrap().clone();
        assert!(ready.layout.candidate_ready() && !Arc::ptr_eq(&laid, &sheet) && !ready.caption.contains("laying out"),
            "the finished proposal replaces the live fallback and clears the layout notice");
        assert_eq!(ready.emphasis.as_ref().unwrap().row(changed_rows.rows.comp(0).unwrap() as usize)[3], 1.,
            "the created component is emphasized through its proposed sheet row");
        assert_eq!(owners(&cx), registered, "laying out the candidate preserves live wiring and miniature subscriptions");
        assert!(Arc::ptr_eq(&pane.sheet, &sheet) && pane.selected == selected, "the live layout and selection stay installed under the finished proposal");

        pane.pane.cam.center = [-3., 4.];
        let moved = pane.pane.cam;
        ElecLive::select_preview(&mut cx, None);
        pane.follow(&mut cx);
        assert!(pane.pane.cam == moved && pane.selected == selected && owners(&cx) == registered && !pane.layout_jobs.busy(),
            "Back keeps navigation performed in preview and restores the selection and registrations without a layout");

        // A board-only change shares the installed schematic. An accepted circuit change
        // instead installs its worker-prepared layout and registers it as live exactly then.
        let host = cx.get_global::<ElecLive>();
        let snap = Arc::make_mut(&mut host.snap);
        let shared = Arc::new(ProposalView { design: Some(design.clone()),
            sheets: Arc::new([(unit.id, rows.clone())].into()), ..(*view).clone() });
        snap.proposals = Arc::new(vec![shared]);
        ElecLive::select_preview(&mut cx, Some(view.id));
        pane.follow(&mut cx);
        assert!(matches!(pane.review.as_ref().unwrap().layout, Layout::Shared),
            "matching circuit and sheet index Arcs reuse the installed layout");
        Arc::make_mut(&mut cx.get_global::<ElecLive>().snap).proposals = Arc::new(vec![view.clone()]);
        pane.follow(&mut cx);
        finish_candidate(&mut cx, &mut pane);
        let Layout::Ready(ref laid) = pane.review.as_ref().unwrap().layout else { panic!("the changed sheet finishes layout") };
        let accepted = laid.sheet.clone();
        let host = cx.get_global::<ElecLive>();
        let mut snap = (*host.snap).clone();
        snap.design = proposed;
        snap.sheets[0].value = changed_rows;
        snap.proposals = Arc::default();
        host.end_invalid_preview(&snap);
        host.snap = Arc::new(snap);
        draw(&mut cx, &mut pane);
        assert!(pane.review.is_none() && Arc::ptr_eq(&pane.sheet, &accepted) && !pane.layout_jobs.busy(),
            "Accept installs the candidate layout immediately as the live sheet");
        assert_eq!(pane.pane.cam, moved, "Accept keeps the camera at the last comparison position");
    }
}
