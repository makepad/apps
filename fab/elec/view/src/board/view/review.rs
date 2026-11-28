//! A proposal borrows the pane's layer targets and camera. The live drawing and selection stay
//! with the pane until Back, so a comparison never becomes a source of edit geometry.
use super::*;
use super::super::shared::BoardSource;
use crate::{canvas::pane::review_chip, snapshot::ProposalView};
use fab_elec_board::ZoneFill;

#[derive(Default)]
pub(super) struct Comparison {
    bound: Weak<ProposalView>,
    pane_unit: Option<BoardId>,
    unit: Option<BoardId>,
    board: Weak<Board>,
    fills: Weak<Vec<ZoneFill>>,
    created: Weak<Vec<Sel>>,
    drawn: Option<Arc<BoardDraw>>,
    preparing: bool,
    pub(super) emphasis: Option<ItemStates>,
    pub(super) caption: String,
}

impl Comparison {
    fn bind(&mut self, cx: &mut Cx, view: &Arc<ProposalView>, pane_unit: Option<BoardId>) {
        let design = view.design.as_ref().expect("only a Ready design is previewed");
        let unit = pane_unit.or_else(|| design.units.iter().find(|u| u.board.is_some()).map(|u| u.id));
        let board = unit.and_then(|id| design.unit(id)?.board.as_ref());
        let results = unit.and_then(|id| view.boards.iter().find(|b| b.unit == id));
        if let Some((board, results)) = board.zip(results) {
            BoardDraws::set_source(cx, BoardSource::Proposal(view.id), board.clone(), results.fills.clone());
            if self.board.as_ptr() != Arc::as_ptr(board) || self.created.as_ptr() != Arc::as_ptr(&view.created) {
                let mut rows = Vec::new();
                if view.created.iter().any(|sel| sel.unit() == Some(board.unit)) {
                    let parts = crate::part_rows(board);
                    for sel in view.created.iter() { crate::board_rows(board.unit, board, &parts, sel, &mut rows); }
                    rows.sort_unstable();
                    rows.dedup();
                }
                if !rows.is_empty() {
                    self.emphasis.get_or_insert_with(ItemStates::new).write(board.next_item as usize,
                        rows.into_iter().map(|id| (id, [0., 1., 0., 0.])).collect());
                } else if let Some(mut old) = self.emphasis.take() { old.release(cx); }
            }
            self.board = Arc::downgrade(board);
            self.created = Arc::downgrade(&view.created);
            self.fills = Arc::downgrade(&results.fills);
        } else {
            if let Some(mut old) = self.emphasis.take() { old.release(cx); }
            self.board = Weak::new();
            self.fills = Weak::new();
        }
        self.unit = unit;
        self.pane_unit = pane_unit;
        self.bound = Arc::downgrade(view);
    }

    /// Only a finished drawing for both proposed Arcs can use the proposal's item rows.
    fn picture(&mut self, cx: &mut Cx, view: &Arc<ProposalView>, unit: Option<BoardId>,
        live: Option<&Arc<BoardDraw>>, chip: &WidgetRef) -> Option<Arc<BoardDraw>> {
        let rebound = self.bound.as_ptr() != Arc::as_ptr(view) || self.pane_unit != unit;
        if rebound { self.bind(cx, view, unit); }
        let current = self.current(cx, view);
        let present = self.board.strong_count() != 0;
        let preparing = present && current.is_none();
        if rebound || self.preparing != preparing {
            self.preparing = preparing;
            self.caption = review_chip::caption(view, !present && unit.is_some(), preparing.then_some("drawing the proposal"));
            chip.label(cx, ids!(preview_caption)).set_text(cx, &self.caption);
        }
        if preparing { live.or(self.drawn.as_ref()).cloned() } else { current }
    }

    pub(super) fn shown_emphasis(&mut self) -> Option<&mut ItemStates> {
        if self.preparing { return None; }
        self.emphasis.as_mut()
    }

    fn current(&self, cx: &mut Cx, view: &ProposalView) -> Option<Arc<BoardDraw>> {
        BoardDraws::current_source(cx, BoardSource::Proposal(view.id), self.unit?).filter(|d|
            Arc::as_ptr(&d.board) == self.board.as_ptr() && Arc::as_ptr(&d.fills) == self.fills.as_ptr())
    }
}

impl BoardPane {
    pub(super) fn comparison_paused(&self, cx: &Cx) -> bool {
        self.st.review.is_some() || cx.get_global_ref::<ElecLive>().is_some_and(|l| l.previewed().is_some())
    }

    pub(super) fn leave_comparison(&mut self, cx: &mut Cx) {
        if let Some(mut review) = self.st.review.take() {
            if let Some(draw) = review.drawn.take() { BoardDraws::retire(cx, draw); }
            if let Some(mut rows) = review.emphasis.take() { rows.release(cx); }
        }
    }

    pub(super) fn install_empty(&mut self, cx: &mut Cx) {
        for mut layer in self.st.layers.drain(..) {
            if let Some(target) = layer.target.as_mut() { target.release(cx); }
        }
        self.st.substrate = None;
        self.st.glows = None;
        self.st.holes = None;
    }

    pub(super) fn draw_comparison(&mut self, cx: &mut Cx2d, scope: &mut Scope, rect: Rect, view: &Arc<ProposalView>) {
        if self.st.review.is_none() {
            self.end_drag(Some(cx));
            self.cancel_placing(cx);
            self.cancel_shape();
            self.st.glide = None;
            self.st.pending = None;
            self.st.press_at = None;
            self.st.tune_press = None;
            self.st.pair_press = None;
            self.st.marquee = None;
            self.st.hover = None;
            self.st.hover_pour = false;
        }
        let review = self.st.review.get_or_insert_with(Comparison::default);
        if let Some(draw) = review.picture(cx, view, self.st.unit, self.st.drawn.as_ref(), &self.review_chip) {
            if !review.drawn.as_ref().is_some_and(|old| Arc::ptr_eq(old, &draw)) {
                let new_picture = review.drawn.as_ref().is_none_or(|old|
                    !Arc::ptr_eq(&old.batch, &draw.batch));
                if let Some(old) = review.drawn.replace(draw.clone()) { BoardDraws::retire(cx, old); }
                self.install_picture(cx, &draw, new_picture);
            }
            let b = draw.geom.scene.bbox;
            let bounds = [to_mm(b.min.x), to_mm(b.min.y), to_mm(b.max.x), to_mm(b.max.y)];
            // Fit only a never-placed camera. A live camera compares in the same coordinates.
            let fit = (!self.pane.placed).then_some(bounds);
            self.pane.place(cx, rect, fit);
            self.draw_board(cx, rect);
        } else if let Some(old) = review.drawn.take() {
            BoardDraws::retire(cx, old);
            self.install_empty(cx);
        }
        if self.status.visible() {
            if let Size::Fixed(height) = self.status.walk(cx).height {
                self.review_bar.color = self.ground;
                self.review_bar.draw_abs(cx, Rect { pos: rect.pos + dvec2(0., rect.size.y - height), size: dvec2(rect.size.x, height) });
            }
        }
        review_chip::draw(cx, scope, self.uid, &self.review_chip, rect);
    }

    pub(super) fn comparison_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let proposal = cx.get_global_ref::<ElecLive>().and_then(ElecLive::previewed).cloned();
        if matches!(event, Event::Signal) {
            BoardDraws::poll(cx);
            let changed = match (&self.st.review, &proposal) {
                (Some(r), Some(p)) => r.bound.as_ptr() != Arc::as_ptr(p)
                    || r.current(cx, p).is_some_and(|d| r.preparing || r.drawn.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &d))),
                _ => true,
            };
            if changed { self.redraw(cx); }
        }
        let mut back = false;
        if let Some(view) = &proposal {
            if self.st.review.as_ref().and_then(|r| r.bound.upgrade()).is_some_and(|shown| shown.id == view.id) {
                back = review_chip::back(cx, event, scope, &self.review_chip);
            }
        }
        for input in self.pane.navigation(cx, event, self.draw_bg.area()) {
            match input {
                CanvasInput::Escape => back = true,
                CanvasInput::Camera => self.redraw(cx),
                _ => {}
            }
        }
        if back && proposal.is_some() {
            ElecLive::select_preview(cx, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ProposalAuthor, ProposalBoard, ProposalId, ProposalState};
    use fab_elec_design::By;
    use fab_elec_core::CompRef;

    #[test]
    fn comparison_uses_the_candidate_board_rows_and_reports_owed_zones() {
        let design = Arc::new(fab_elec_fixtures::ledctl());
        let unit = &design.units[0];
        let board = unit.board.as_ref().unwrap();
        let part = &board.placed[0];
        let mut view = Arc::new(ProposalView { id: ProposalId(2), shown: 1, slot: "layout".into(), label: "Copper".into(),
            by: ProposalAuthor::Local(By::Person), state: ProposalState::Ready, design: Some(design.clone()), run: Arc::default(),
            docs: Arc::default(), boards: Arc::new(vec![ProposalBoard { unit: unit.id, fills: Arc::default(),
                pending_zones: Arc::from([board.zones[0].id]) }]), sheets: Arc::default(),
            created: Arc::new(vec![Sel::Comp(CompRef { board: unit.id, comp: part.comp })]), checks: None, requirements: None });
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let pane = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
            crate::board::script_mod(vm);
            BoardPane::script_new_with_default(vm)
        });
        BoardDraws::set_templates(&mut cx, Templates { stroke: StrokeTemplate::packed(), pad: PadTemplate::packed(), tri: TriTemplate::packed() });
        BoardDraws::set_board(&mut cx, board.clone(), Arc::default());
        let live = loop {
            BoardDraws::poll(&mut cx);
            if let Some(draw) = BoardDraws::current(&mut cx, unit.id) { break draw; }
            assert!(cx.get_global::<BoardDraws>().drawing(), "the live fallback has a drawing in progress");
            std::thread::yield_now();
        };
        let mut review = Comparison::default();
        let fallback = review.picture(&mut cx, &view, None, Some(&live), &pane.review_chip);
        assert!(fallback.is_some_and(|p| Arc::ptr_eq(&p, &live)) && review.shown_emphasis().is_none(),
            "the installed live board stays visible without candidate emphasis while the pool draws");
        assert!(review.unit == Some(unit.id) && review.board.as_ptr() == Arc::as_ptr(board), "an unassigned pane follows the first candidate board");
        let caption = &review.caption;
        assert!(caption.contains("Copper") && caption.contains("1 zone not refilled yet") && caption.ends_with("drawing the proposal"), "the chip names the proposal and its real refill debt");
        let emphasis = review.emphasis.as_mut().unwrap();
        assert_eq!(emphasis.row(part.id.ix())[1], 1.0, "the created component emphasizes its placement's candidate row");
        emphasis.take_dirty();
        Arc::make_mut(&mut Arc::make_mut(&mut view).boards)[0].pending_zones = Arc::default();
        while review.current(&mut cx, &view).is_none() {
            assert!(cx.get_global::<BoardDraws>().drawing(), "the proposed board has a drawing in progress");
            BoardDraws::poll(&mut cx);
            std::thread::yield_now();
        }
        let finished = review.picture(&mut cx, &view, Some(unit.id), Some(&live), &pane.review_chip).unwrap();
        assert!(!Arc::ptr_eq(&finished, &live) && review.shown_emphasis().is_some() && !review.caption.contains("drawing the proposal"),
            "only the finished candidate shows its own emphasis and clears the drawing notice");
        assert!(!review.caption.contains("refilled") && !review.emphasis.as_ref().unwrap().is_dirty(),
            "settling fills updates the chip and keeps the same emphasis rows");
        let absent = review.picture(&mut cx, &view, Some(BoardId(unit.id.0 + 1)), Some(&live), &pane.review_chip);
        assert!(absent.is_none() && review.emphasis.is_none() && review.caption.contains("removes this board"),
            "a removed pane unit stays empty even when another candidate board exists");

        // Exercise the chip's actual layout, including the button's themed text and padding.
        let pass = DrawPass::new(&mut cx);
        let mut list = DrawList2d::new(&mut cx);
        let layout = |cx: &mut Cx, list: &mut DrawList2d, text: &str, width: f64| {
            let size = dvec2(width, 128.0);
            pass.set_size(cx, size);
            let caption = pane.review_chip.child(id!(preview_caption));
            let back = pane.review_chip.child(id!(preview_back_slot)).child(id!(preview_back));
            caption.set_text(cx, text);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, Some(1.0));
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            review_chip::draw(&mut frame, &mut Scope::empty(), pane.uid, &pane.review_chip, Rect { pos: dvec2(0., 0.), size });
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
            let chip = pane.review_chip.area().rect(&frame);
            let button = back.area().rect(&frame);
            assert!(button.size.x > 0. && button.pos.x + button.size.x <= chip.pos.x + chip.size.x
                && chip.pos.x + chip.size.x <= width - STRIP_INSET, "the full Back button stays within the inset chip");
            assert_eq!(caption.text(), text, "layout preserves the full caption for the Board's semantic value");
            (chip.size.x, button.size)
        };
        let long = "Previewing a proposal with a long label · 1 zone not refilled yet".repeat(3);
        let (narrow, button) = layout(&mut cx, &mut list, &long, 250.);
        let (_, wrapped) = layout(&mut cx, &mut list, &long, 50.);
        let (short, same_button) = layout(&mut cx, &mut list, "Previewing X", 800.);
        assert!(wrapped.x < button.x && wrapped.y > button.y, "at the sash floor the full button wraps inside the chip");
        assert!(short < narrow && button == same_button, "a short chip fits its content and only the caption gives way on a narrow pane");
    }
}
