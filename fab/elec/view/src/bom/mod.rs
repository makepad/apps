//! The BOM tab. Prepared sheets live on the pool. Each list item binds only its visible cells;
//! the edit box holds the exact member paths of the cell that opened it across regrouping.
use crate::{ElecLive, ViewAction, ViewState};
mod display;
pub use display::{cost_text, PriceCell};
use fab_elec_bom::{BomSheet, import::Draft, prices::{Library, PriceKey, Settings, Sourcing}};
use fab_elec_core::{BoardId, CompRef, Sel};
use fab_elec_design::{Focus, FocusKind, FocusOrigin};
use makepad_widgets_core::*;
use std::sync::Arc;

pub struct UnitShown {
    pub unit: BoardId,
    pub name: String,
    pub sheet: Arc<BomSheet>,
    pub grouped_sheet: Arc<BomSheet>,
    pub facts: Arc<fab_elec_bom::BoardFacts>,
    pub prepared: Option<Arc<fab_elec_bom::cost::Prepared>>,
    pub rates: Option<Arc<fab_elec_bom::Rates>>,
    pub placements: Arc<fab_elec_bom::cost::Placements>,
    pub cost: String,
    pub details: String,
    pub prices: Arc<Vec<PriceCell>>,
    pub book: Option<Arc<Library>>,
    pub settings: Settings,
}
pub struct Shown {
    pub design: Arc<fab_elec_design::Design>,
    pub units: Vec<UnitShown>,
    pub grouped: bool,
    pub quantity: i64,
}

#[derive(Default)]
pub struct PricePanels {
    pub review: Option<(u64, Arc<String>)>,
    pub draft: Option<Arc<Draft>>,
}

#[derive(Clone, Debug)]
pub enum Request {
    Run(&'static str, Vec<(String, String)>),
    Reload,
    Group(bool),
    Quantity(i64),
    PreviewCsv(String),
    Import(Arc<Draft>, Vec<String>),
    Settings(Settings),
    Review(u64,bool),
}

#[derive(Clone, Debug)]
pub struct EditResult(pub Result<String, String>);

const EDITABLE: [(&str, usize); 4] = [("mpn", 4), ("supplier", 5), ("supplier_part", 6), ("description", 7)];

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let Cell = Label{
        width: Fill height: Fill padding: 0 align: Align{y: 0.5}
        max_lines: 1 text_overflow: Ellipsis
        draw_text +: {text_style: theme.font_regular{font_size: 9}}
    }
    let Title = Cell{draw_text +: {text_style: theme.font_bold{font_size: 9}}}
    let Detail = Label{
        width: Fill height: Fit padding: 0 flow: Right{wrap: true}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}
    }
    let Heading = Detail{draw_text +: {text_style: theme.font_bold{font_size: 9}}}
    let DataColumns = View{width: Fill height: 26 flow: Right spacing: 4}
    let RowBand = View{width: Fill height: Fit flow: Right spacing: 4 align: Align{y: 0.5}}
    let ActionColumn = View{width: 110 height: 26 flow: Right align: Align{y: 0.5}}
    let EditCell = ButtonFlatter{
        width: Fill height: Fill margin: 0 padding: 0 align: Align{x: 0 y: 0.5}
        label_walk: Walk{width: Fill height: Fill} label_align: Align{x: 0 y: 0.5}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}
    }
    let DnpBox = CheckBoxFlat{
        width: 32 height: Fill margin: 0 padding: 0 text: "" a11y: {label: "Do not populate"}
        draw_bg +: {
            size: 17.0
            border_color: theme.color_label_outer
            border_color_disabled: theme.color_bevel_inset_2
            // A disabled box still reports its fitted state: dim both the face and the tick.
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                let top = (self.rect_size.y - self.size) * 0.5 + self.mark_box_offset_y
                let outline = self.border_color.mix(self.mark_color_focus, self.focus)
                    .mix(self.mark_color_active_hover, self.hover).mix(self.border_color_disabled, self.disabled)
                let ink = self.mark_color_active.mix(self.mark_color_focus, self.focus)
                    .mix(self.mark_color_active_hover, self.hover).mix(self.border_color_disabled, self.disabled)
                let paper = self.color.mix(self.color_down, self.down)
                sdf.box(1.0, top + 1.0, self.size - 2.0, self.size - 2.0, self.border_radius * 0.5)
                sdf.fill_keep(paper.mix(ink, self.active))
                sdf.stroke(outline, 1.5)
                sdf.move_to(self.size * 0.20, top + self.size * 0.50)
                sdf.line_to(self.size * 0.44, top + self.size * 0.73)
                sdf.line_to(self.size * 0.80, top + self.size * 0.26)
                sdf.stroke(vec4(paper.rgb, paper.a * self.active), 2.0)
                return sdf.result
            }
        }
    }
    let Item = #(BomItem::register_widget(vm))
    mod.widgets.BomPaneBase = #(BomPane::register_widget(vm))
    mod.widgets.BomPane = set_type_default() do mod.widgets.BomPaneBase{
        width: Fill height: Fill flow: Down padding: 8 spacing: 4
        scroll_bars: ScrollBars{show_scroll_x: false show_scroll_y: true}
        a11y: {role: "group" label: "Bill of materials"}
        tools := View{
            width: Fill height: Fit flow: Right{wrap: true} spacing: 5 align: Align{y: 0.5}
            title := Title{width: Fit height: 22 text: "Bill of materials"}
            unit := ButtonFlatter{text: "Next board" visible: false}
            group := ButtonFlatter{text: "Show individual parts"}
            import := ButtonFlatter{text: "Import prices"}
            settings := ButtonFlatter{text: "Estimate settings"}
        }
        approval := View{visible: false width: Fill height: Fit flow: Down
            proposal := Detail{}
            View{width: Fill height: Fit flow: Right
                accept := ButtonFlatter{text: "Save this AI price"} decline := ButtonFlatter{text: "Decline price"}
            }
        }
        csv := View{visible: false width: Fill height: Fit flow: Down spacing: 2
            file := TextInput{width: Fill height: 24 empty_text: "CSV file path"}
            preview := ButtonFlatter{text: "Preview columns"}
            mapping := View{visible: false width: Fill height: Fit flow: Down
                headers := Detail{}
                columns := TextInput{width: Fill height: 24}
                Detail{text: "Map each column in order: mpn, supplier, supplier_part, price, currency, min, moq, multiple, assembly_class, url, or - to skip."}
                confirm := ButtonFlatter{text: "Import mapped prices"}
            }
        }
        options := View{visible: false width: Fill height: Fit flow: Down spacing: 2
            hand := CheckBoxFlat{text: "Hand assembly"}
            spares := TextInput{width: Fill height: 24 empty_text: "Hand spares, e.g. 2% 1"}
            flat := TextInput{width: Fill height: 24 empty_text: "Optional flat amount, e.g. USD 5.00"}
            fx := TextInput{width: Fill height: 24 empty_text: "Optional exchange rate, e.g. EUR 1.10 USD"}
            apply := ButtonFlatter{text: "Apply estimate settings"}
        }
        pricing := View{visible: false width: Fill height: Fit flow: Down spacing: 2
            target := Heading{}
            breaks := TextInput{width: Fill height: 24 empty_text: "Unit price, or breaks such as 1=0.80, 10=0.65"}
            View{width: Fill height: 24 flow: Right spacing: 4
                currency := TextInput{width: Fill height: Fill empty_text: "Currency"}
                moq := TextInput{width: Fill height: Fill empty_text: "Minimum order"}
                multiple := TextInput{width: Fill height: Fill empty_text: "Order multiple"}
            }
            source := TextInput{width: Fill height: 24 empty_text: "Source (default: User entry)"}
            url := TextInput{width: Fill height: 24 empty_text: "Source URL (optional)"}
            View{width: Fill height: Fit flow: Right
                apply := ButtonFlatter{text: "Save price"} cancel := ButtonFlatter{text: "Cancel price"}
            }
        }
        edit := View{
            visible: false width: Fill height: Fit flow: Down spacing: 3
            target := Detail{draw_text +: {text_style: theme.font_bold{font_size: 9}}}
            inputs := View{width: Fill height: 27 flow: Right spacing: 4
                value := TextInput{width: Fill height: Fill empty_text: "MPN or field value"}
                supplier := TextInput{visible: false width: Fill height: Fill empty_text: "Supplier"}
                supplier_part := TextInput{visible: false width: Fill height: Fill empty_text: "Supplier part"}
                alt_note := TextInput{visible: false width: Fill height: Fill empty_text: "Note"}
            }
            buttons := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 4
                save := ButtonFlatter{text: "Apply"}
                cancel := ButtonFlatter{text: "Cancel"}
            }
        }
        note := Cell{width: Fill height: Fit visible: false max_lines: 3}
        header := RowBand{
            padding: Inset{left: 5 right: 17}
            DataColumns{height: Fit min_height: 26 align: Align{y: 0.5}
                Heading{text: "Refs"} Heading{width: 27 text: "Qty"} Heading{text: "Value"} Heading{text: "Footprint"}
                Heading{text: "MPN"} Heading{text: "Supplier"} Heading{text: "Supplier part"} Heading{text: "Description"} Heading{width: 32 text: "DNP"}
            }
            actions := ActionColumn{Heading{text: "Actions"}}
        }
        rows := PortalList{
            width: Fill height: Fill min_height: "50%" flow: Down padding: Inset{right: 12}
            drag_scrolling: false grab_key_focus: false selectable: false
            Row := Item{
                width: Fill height: Fit flow: Down padding: Inset{bottom: 5}
                cursor: MouseCursor.Hand capture_overload: true grab_key_focus: false
                body := View{
                    width: Fill height: Fit flow: Down padding: Inset{left: 5 right: 5 bottom: 4}
                    show_bg: true draw_bg +: {color: theme.color_inset}
                    fitted := RowBand{
                        cells := DataColumns{
                            refs := Cell{} qty := Cell{width: 27} value := Cell{} footprint := Cell{}
                            mpn := EditCell{} supplier := EditCell{} supplier_part := EditCell{} description := EditCell{}
                            dnp := DnpBox{}
                        }
                        actions := ActionColumn{add := ButtonFlatter{height: Fill text: "Add alternate"}}
                    }
                    alternate := RowBand{
                        option := Detail{width: Fill padding: Inset{left: 8} draw_text +: {text_style: theme.font_bold{font_size: 9}}}
                        decision := View{
                            width: Fit height: Fit flow: Right spacing: 4 align: Align{y: 0.5}
                            use_alt := ButtonFlatter{text: "Use"}
                            note := Detail{width: Fit text: "Electrical equivalence is your call."}
                        }
                    }
                    price := View{width: Fill height: Fit flow: Right spacing: 5 align: Align{y: 0.5}
                        change := ButtonFlatter{text: "Price"}
                        quote := Detail{}
                        old_quote := Detail{visible: false draw_text +: {color: theme.color_label_inner_inactive}}
                    }
                    detail := Detail{draw_text +: {color: theme.color_label_inner_inactive}}
                }
                View{width: Fill height: 1 show_bg: true draw_bg +: {color: theme.color_bevel_inset_2}}
            }
        }
        cost := View{
            width: Fill height: Fit flow: Down padding: 6 spacing: 4
            show_bg: true draw_bg +: {color: theme.color_inset}
            bar := View{
                width: Fill height: Fit flow: Right{wrap: true} spacing: 5 align: Align{y: 0.5}
                heading := Title{width: Fit height: 22 text: "Estimate, not a quote"}
                boards := Cell{width: Fit height: 22 text: "Boards"}
                qty := TextInput{width: 55 height: 23 text: "10"}
                compare := ButtonFlatter{text: "Quantities and charges"}
                reload := ButtonFlatter{text: "Reload prices and rates"}
            }
            breakdown := View{visible: false width: Fill height: Fit flow: Down
                data := Detail{}
            }
            figures := Detail{text: "Open a board to estimate a run."
                draw_text +: {text_style: theme.font_regular{font_size: 9}}}
        }
    }
}

#[derive(Script, Widget)]
struct BomItem {
    #[deref] view: View,
    #[rust] accessible: String,
    #[rust] prices: Option<Arc<Vec<PriceCell>>>,
    #[rust] bound: Option<(Arc<BomSheet>, usize)>,
    #[rust] compact: Option<bool>,
}

impl ScriptHook for BomItem {
    fn on_after_apply(&mut self, _: &mut ScriptVm, apply: &Apply, _: &mut Scope, _: ScriptValue) {
        if apply.is_reload() {
            // PortalList reapplied the row template, including its cell labels and layout.
            self.bound = None;
            self.compact = None;
        }
    }
}

impl Widget for BomItem {
    fn semantics(&self, _: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::ListItem;
        makepad_platform::a11y::set_string(&mut node.name, &self.accessible);
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) { self.view.handle_event(cx, event, scope); }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }
}

#[derive(Script, Widget)]
pub struct BomPane {
    #[deref] view: View,
    #[rust] shown: Option<Arc<Shown>>,
    #[rust] unit: Option<BoardId>,
    #[rust] compact: Option<bool>,
    #[rust] draft: Option<Arc<Draft>>,
    #[rust] review: Option<u64>,
    #[rust] price_edit: Option<(Arc<fab_elec_design::Design>,PriceKey)>,
    #[rust] edit: Option<(Arc<fab_elec_design::Design>, String, Option<String>)>,
}
impl ScriptHook for BomPane {
    fn on_after_apply(&mut self, _: &mut ScriptVm, apply: &Apply, _: &mut Scope, _: ScriptValue) {
        if apply.is_reload() {
            self.shown = None;
            self.compact = None;
            self.draft = None;
            self.review = None;
        }
    }
}

impl BomPane {
    fn active(&self) -> Option<&UnitShown> {
        self.shown.as_ref()?.units.iter().find(|u| Some(u.unit) == self.unit)
    }
    fn note(&self, cx: &mut Cx, text: &str) {
        let label = self.view.label(cx, ids!(note));
        label.set_text(cx, text);
        label.set_visible(cx, !text.is_empty());
    }
    fn sync(&mut self, cx: &mut Cx) {
        if let Some(panel) = cx.get_global_ref::<PricePanels>() {
            let draft = panel.draft.clone();
            let review = panel.review.clone();
            if self.review != review.as_ref().map(|(id,_)| *id) {
                self.review = review.as_ref().map(|(id,_)| *id);
                self.view.view(cx, ids!(approval)).set_visible(cx,review.is_some());
                if let Some((_,text)) = review { self.view.label(cx,ids!(approval.proposal)).set_text(cx,&text); }
            }
            if self.draft.as_ref().map(Arc::as_ptr) != draft.as_ref().map(Arc::as_ptr) {
                self.draft = draft;
                self.view.view(cx,ids!(csv.mapping)).set_visible(cx,self.draft.is_some());
                if let Some(d) = &self.draft {
                    self.view.label(cx,ids!(csv.mapping.headers)).set_text(cx,&format!("{} · {} · {}",d.source,d.date,d.header.join("  |  ")));
                    self.view.text_input(cx,ids!(csv.mapping.columns)).set_text(cx,&d.columns.join(", "));
                }
            }
        }
        let live = cx.get_global_ref::<ElecLive>();
        let shown = live.and_then(|l| l.bom.clone());
        if self.shown.as_ref().map(Arc::as_ptr) == shown.as_ref().map(Arc::as_ptr) { return; }
        let preferred = self.unit.or_else(|| live.and_then(|l| l.shown_unit));
        self.shown = shown;
        self.unit = self.shown.as_ref().and_then(|s| s.units.iter().find(|u| preferred == Some(u.unit)).or(s.units.first())).map(|u| u.unit);
        self.update(cx);
    }
    fn update(&mut self, cx: &mut Cx) {
        let unit = self.active();
        let title = unit.map_or_else(|| "No board on this plate".into(), |u| format!("{} · {} BOM lines", u.name, u.sheet.rows.len()));
        self.view.label(cx, ids!(tools.title)).set_text(cx, &title);
        self.view.button(cx, ids!(tools.unit)).set_visible(cx, self.shown.as_ref().is_some_and(|s| s.units.len() > 1));
        if let Some(s) = &self.shown {
            self.view.button(cx, ids!(tools.group)).set_text(cx, if s.grouped { "Show individual parts" } else { "Group parts" });
            self.view.text_input(cx, ids!(cost.bar.qty)).set_text(cx, &s.quantity.to_string());
        }
        self.view.label(cx, ids!(cost.breakdown.data)).set_text(cx, unit.map_or("", |u| &u.details));
        self.view.label(cx, ids!(cost.figures)).set_text(cx, unit.map_or("Open a board to estimate a run.", |u| &u.cost));
        self.view.redraw(cx);
    }
    fn current(cx: &Cx, design: &Arc<fab_elec_design::Design>) -> bool {
        cx.get_global_ref::<ElecLive>().is_some_and(|live| Arc::ptr_eq(&live.snap.design, design) && live.snap.preview.is_none())
    }

    fn open_price(&mut self, cx: &mut Cx, row: &fab_elec_bom::BomRow, alternate: Option<usize>) {
        let key = |p: &Sourcing| p.supplier_key().or_else(|| p.mpn_key());
        let target = if let Some(i) = alternate {
            let a = &row.alternates[i];
            key(&Sourcing {mpn:a.mpn.clone(),supplier:a.supplier.clone(),supplier_part:a.supplier_part.clone()})
        } else {
            let mut members = row.members.iter().filter(|m| !m.dnp).map(|m| key(&m.part));
            members.next().flatten().filter(|first| members.all(|k| k.as_ref() == Some(first)))
        };
        let Some(target) = target else {
            self.note(cx, "A price needs one MPN or one supplier and supplier part. Edit the sourcing cells or select an individual part.");
            return;
        };
        let Some(book) = self.active().and_then(|u| u.book.as_ref()) else { self.note(cx,"Price library unavailable."); return };
        let entry = book.prices.0.get(&target);
        let panel = self.view.view(cx,ids!(pricing));
        panel.set_visible(cx,true);
        panel.label(cx,ids!(target)).set_text(cx,&format!("Price for {}",target.label()));
        let input = panel.text_input(cx,ids!(breaks));
        input.set_text(cx,&entry.map(|e| e.break_label()).unwrap_or_default());
        input.set_key_focus(cx);
        panel.text_input(cx,ids!(currency)).set_text(cx,entry.map_or("",|e| &e.currency));
        panel.text_input(cx,ids!(moq)).set_text(cx,&entry.map_or(1,|e| e.moq).to_string());
        panel.text_input(cx,ids!(multiple)).set_text(cx,&entry.map_or(1,|e| e.multiple).to_string());
        panel.text_input(cx,ids!(source)).set_text(cx,"");
        panel.text_input(cx,ids!(url)).set_text(cx,"");
        self.price_edit = Some((self.shown.as_ref().unwrap().design.clone(),target));
        self.note(cx,"");
    }

    fn price_controls(&mut self, cx: &mut Cx, actions: &Actions) {
        let pricing = self.view.view(cx,ids!(pricing));
        if pricing.button(cx,ids!(cancel)).clicked(actions) {
            self.price_edit = None;
            pricing.set_visible(cx,false);
        }
        if pricing.button(cx,ids!(apply)).clicked(actions) {
            if let Some((design, key)) = &self.price_edit {
                if !Self::current(cx,design) { self.note(cx,"The board changed. Select the price cell again."); return; }
                let mut args = match key {
                    PriceKey::Mpn(mpn) => vec![("mpn".into(),mpn.clone())],
                    PriceKey::Supplier {supplier,part} => vec![("supplier".into(),supplier.clone()),("supplier_part".into(),part.clone())],
                };
                let text = pricing.text_input(cx,ids!(breaks)).text();
                args.push(("breaks".into(),if text.contains('=') { text } else { format!("1={text}") }));
                for field in ["currency","moq","multiple","source","url"] {
                    let value = pricing.text_input(cx,&[LiveId::from_str(field)]).text();
                    if !value.trim().is_empty() { args.push((field.into(),value)); }
                }
                if !args.iter().any(|(k,_)| k == "currency") { self.note(cx,"Enter the price currency."); return; }
                cx.action(Request::Run("elec.price.set",args));
                self.price_edit = None;
                pricing.set_visible(cx,false);
            }
        }
        for (button, panel) in [(&ids!(cost.bar.compare)[..],&ids!(cost.breakdown)[..]),(&ids!(tools.import)[..],&ids!(csv)[..])] {
            if self.view.button(cx,button).clicked(actions) {
                let pane = self.view.view(cx,panel);
                pane.set_visible(cx,!pane.visible());
            }
        }
        let csv = self.view.view(cx,ids!(csv));
        if csv.button(cx,ids!(preview)).clicked(actions) {
            let path = csv.text_input(cx,ids!(file)).text();
            cx.action(Request::PreviewCsv(path));
        }
        if csv.button(cx,ids!(mapping.confirm)).clicked(actions) {
            if let Some(draft) = &self.draft {
                let columns = csv.text_input(cx,ids!(mapping.columns)).text().split(',').map(|s|s.trim().to_owned()).collect();
                cx.action(Request::Import(draft.clone(),columns));
            }
        }
        let options = self.view.view(cx,ids!(options));
        if self.view.button(cx,ids!(tools.settings)).clicked(actions) {
            let opening = !options.visible();
            options.set_visible(cx,opening);
            if let Some(unit) = self.active().filter(|_| opening) {
                let s = &unit.settings;
                options.check_box(cx,ids!(hand)).set_active(cx,s.hand,Animate::No);
                for (field,value) in [(id!(spares),&s.spares),(id!(flat),&s.flat),(id!(fx),&s.exchange)] {
                    options.text_input(cx,&[field]).set_text(cx,&value.value);
                }
            }
        }
        if options.button(cx,ids!(apply)).clicked(actions) {
            let field = |cx: &mut Cx, id| fab_elec_bom::prices::Setting { value:options.text_input(cx,&[id]).text(),date:String::new() };
            let settings = Settings {hand:options.check_box(cx,ids!(hand)).active(cx),
                spares:field(cx,id!(spares)),flat:field(cx,id!(flat)),exchange:field(cx,id!(fx))};
            cx.action(Request::Settings(settings));
            options.set_visible(cx,false);
        }
        if let Some(call) = self.review {
            for (id, approve) in [(id!(accept),true),(id!(decline),false)] {
                if self.view.button(cx,&[id!(approval),id]).clicked(actions) { cx.action(Request::Review(call,approve)); }
            }
        }
    }

    fn open_edit(&mut self, cx: &mut Cx, line: &str, key: Option<&str>, title: String, value: &str) {
        let design = self.shown.as_ref().unwrap().design.clone();
        self.edit = Some((design, line.into(), key.map(str::to_owned)));
        let panel = self.view.view(cx, ids!(edit));
        panel.set_visible(cx, true);
        panel.label(cx, ids!(target)).set_text(cx, &title);
        for name in [id!(supplier), id!(supplier_part), id!(alt_note)] {
            let field = panel.text_input(cx, &[id!(inputs), name]);
            field.set_text(cx, "");
            field.set_visible(cx, key.is_none());
        }
        let input = panel.text_input(cx, ids!(inputs.value));
        input.set_text(cx, value);
        input.set_key_focus(cx);
        self.note(cx, "");
    }
    fn close_edit(&mut self, cx: &mut Cx) {
        self.edit = None;
        self.view.view(cx, ids!(edit)).set_visible(cx, false);
    }
}

impl Widget for BomPane {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) { self.sync(cx); }
        if let Event::Actions(actions) = event {
            for result in actions.iter().filter_map(|a| a.downcast_ref::<EditResult>()) {
                self.note(cx, result.0.as_ref().err().map_or("", String::as_str));
            }
        }
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        self.price_controls(cx, &actions);
        if self.view.button(cx, ids!(cost.bar.reload)).clicked(&actions) { cx.action(Request::Reload); }
        if let Some((qty, _)) = self.view.text_input(cx, ids!(cost.bar.qty)).returned(&actions) {
            match qty.trim().parse::<i64>() {
                Ok(n) if n > 0 => { self.note(cx, ""); cx.action(Request::Quantity(n)); }
                _ => self.note(cx, "Enter a positive whole number of boards."),
            }
        }
        if self.view.button(cx, ids!(tools.group)).clicked(&actions) {
            if let Some(s) = &self.shown { cx.action(Request::Group(!s.grouped)); }
            self.close_edit(cx);
        }
        if self.view.button(cx, ids!(tools.unit)).clicked(&actions) {
            if let Some(s) = &self.shown {
                if let Some(i) = s.units.iter().position(|u| Some(u.unit) == self.unit) {
                    self.unit = Some(s.units[(i + 1) % s.units.len()].unit);
                }
            }
            self.close_edit(cx);
            self.update(cx);
        }
        let input = self.view.text_input(cx, ids!(edit.inputs.value));
        if self.view.button(cx, ids!(edit.buttons.cancel)).clicked(&actions) || input.escaped(&actions) {
            self.close_edit(cx);
        } else if self.view.button(cx, ids!(edit.buttons.save)).clicked(&actions) || input.returned(&actions).is_some() {
            if let Some((design, line, key)) = &self.edit {
                if !Self::current(cx, design) {
                    self.close_edit(cx);
                    self.note(cx, "The board changed. Select the cell again.");
                    cx.extend_actions(actions);
                    return;
                }
                let text = input.text();
                let mut args = vec![("parts".into(), line.clone())];
                let action = if let Some(key) = key {
                    args.push(("key".into(), key.clone()));
                    if !text.is_empty() { args.push(("text".into(), text)); }
                    "elec.bom.field.set"
                } else {
                    if text.trim().is_empty() {
                        self.note(cx, "Enter an MPN for the alternate.");
                        cx.extend_actions(actions);
                        return;
                    }
                    args.push(("mpn".into(), text));
                    for (name, id) in [("supplier", id!(supplier)), ("supplier_part", id!(supplier_part)), ("note", id!(alt_note))] {
                        let value = self.view.text_input(cx, &[id!(edit), id!(inputs), id]).text();
                        if !value.is_empty() { args.push((name.into(), value)); }
                    }
                    "elec.bom.alt.add"
                };
                cx.action(Request::Run(action, args));
                self.close_edit(cx);
            }
        }
        let list = self.view.portal_list(cx, ids!(rows));
        let mut handled = std::collections::HashSet::new();
        for (id, item) in list.items_with_actions(&actions) {
            if !handled.insert(id) { continue; }
            let Some((sheet, i)) = item.borrow::<BomItem>().and_then(|r| r.bound.clone()) else { continue };
            let Some(unit) = self.active().filter(|u| Arc::ptr_eq(&u.sheet, &sheet)).map(|u| u.unit) else { continue };
            let (row_index, option) = sheet.lines[i];
            let row = &sheet.rows[row_index];
            let alternate = option.map(|a| &row.alternates[a]);
            let clicked = EDITABLE.iter().find(|(key, _)| item.button(cx, &[id!(body), id!(fitted), id!(cells), LiveId::from_str(key)]).clicked(&actions));
            let pressed = actions.iter().filter_map(|a| a.as_widget_action()).any(|a| a.widget_uid == item.widget_uid()
                && matches!(a.action.downcast_ref::<makepad_widgets_core::view::ViewAction>(), Some(makepad_widgets_core::view::ViewAction::FingerDown(_))));
            let add = item.button(cx, ids!(body.fitted.actions.add)).clicked(&actions);
            let use_alt = item.button(cx, ids!(body.alternate.decision.use_alt)).clicked(&actions);
            let dnp = item.check_box(cx, ids!(body.fitted.cells.dnp)).changed(&actions);
            if dnp.is_some() {
                item.check_box(cx, ids!(body.fitted.cells.dnp)).set_active(cx, row.dnp, Animate::No);
            }
            if !self.shown.as_ref().is_some_and(|s| Self::current(cx, &s.design)) { continue; }
            if pressed || clicked.is_some() || add || use_alt || dnp.is_some() {
                let selected: Vec<_> = alternate.map_or(&row.comps, |alt| &alt.comps).iter().map(|&comp| Sel::Comp(CompRef { board: unit, comp })).collect();
                if let Some(first) = selected.first() {
                    cx.widget_action(self.widget_uid(), ViewAction::Focus(Focus { kind: FocusKind::Part, origin: FocusOrigin::Checks, sel: first.clone() }));
                    cx.widget_action(self.widget_uid(), ViewState { selected, emphasis: vec![] });
                }
            }
            if item.button(cx, ids!(body.price.change)).clicked(&actions) {
                self.open_price(cx,row,option);
                continue;
            }
            if let Some(alt) = alternate {
                if use_alt && alt.read_only.is_none() {
                    cx.action(Request::Run("elec.bom.alt.use", vec![("parts".into(), alt.key.clone()), ("alt".into(), alt.number.to_string())]));
                }
            } else if row.read_only.is_none() {
                if let Some(value) = dnp {
                    cx.action(Request::Run("elec.bom.dnp.set", vec![("parts".into(), row.key.clone()), ("dnp".into(), value.to_string())]));
                } else if add {
                    self.open_edit(cx, &row.key, None, format!("{} · Alternate MPN", row.cells[0]), "");
                } else if let Some(&(key, col)) = clicked {
                    self.open_edit(cx, &row.key, Some(key), format!("{} · {}", row.cells[0], sheet.header[col]), &row.cells[col]);
                }
            }
        }
        cx.extend_actions(actions);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync(cx);
        // Keep space for the seven text columns, quantity, DNP and the action button.
        let compact = cx.peek_walk_turtle(walk).size.x < 820.0;
        if self.compact != Some(compact) {
            self.view.view(cx, ids!(header.actions)).set_visible(cx, !compact);
            self.compact = Some(compact);
        }
        let sheet = self.active().map(|u| u.sheet.clone());
        let prices = self.active().map(|u| &u.prices).cloned();
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, sheet.as_ref().map_or(0, |s| s.lines.len()));
            while let Some(i) = list.next_visible_item(cx) {
                let Some(sheet) = sheet.as_ref().filter(|s| i < s.lines.len()) else { continue };
                let item = list.item(cx, i, id!(Row));
                if let Some(mut widget) = item.borrow_mut::<BomItem>() {
                    if widget.compact != Some(compact) {
                        for name in [id!(fitted), id!(alternate)] {
                            if let Some(mut band) = widget.view.view(cx, &[id!(body), name]).borrow_mut() {
                                band.layout.flow = if compact { Flow::Down } else { Flow::right() };
                            }
                        }
                        if let Some(mut actions) = widget.view.view(cx, ids!(body.alternate.decision)).borrow_mut() {
                            actions.walk.width = if compact { Size::fill() } else { Size::fit() };
                        }
                        if let Some(mut note) = widget.view.label(cx, ids!(body.alternate.decision.note)).borrow_mut() {
                            note.walk.width = if compact { Size::fill() } else { Size::fit() };
                        }
                        widget.compact = Some(compact);
                    }
                    if widget.prices.as_ref().map(Arc::as_ptr) != prices.as_ref().map(Arc::as_ptr) {
                        widget.bound = None;
                        widget.prices.clone_from(&prices);
                    }
                    if !widget.bound.as_ref().is_some_and(|(s, j)| Arc::ptr_eq(s, sheet) && *j == i) {
                        let (ri, ai) = sheet.lines[i];
                        let row = &sheet.rows[ri];
                        let body = widget.view.view(cx, ids!(body));
                        let cells = body.view(cx, ids!(fitted.cells));
                        body.view(cx, ids!(fitted)).set_visible(cx, ai.is_none());
                        body.view(cx, ids!(alternate)).set_visible(cx, ai.is_some());
                        let detail = if let Some(index) = ai {
                            let alt = &row.alternates[index];
                            let mut title = format!("{} · Alternate {} · {}", alt.refs.join(", "), alt.number,
                                if alt.mpn.is_empty() { "No MPN" } else { &alt.mpn });
                            for value in [&alt.supplier, &alt.supplier_part] {
                                if !value.is_empty() { title.push_str(" · "); title.push_str(value); }
                            }
                            if alt.ai { title.push_str(" · Added by AI"); }
                            body.label(cx, ids!(alternate.option)).set_text(cx, &title);
                            body.button(cx, ids!(alternate.decision.use_alt)).set_enabled(cx, alt.read_only.is_none());
                            widget.accessible = title;
                            alt.read_only.as_deref().unwrap_or(&alt.note).to_string()
                        } else {
                            for (j, name) in ["refs", "qty", "value", "footprint", "mpn", "supplier", "supplier_part", "description"].iter().enumerate() {
                                let cell = cells.widget(cx, &[LiveId::from_str(name)]);
                                let editable = EDITABLE.iter().any(|(k, _)| k == name);
                                cell.set_text(cx, if editable && row.cells[j].is_empty() { "—" } else { &row.cells[j] });
                                if editable { cell.set_disabled(cx, row.read_only.is_some()); }
                            }
                            let checkbox = cells.check_box(cx, ids!(dnp));
                            checkbox.set_active(cx, row.dnp, Animate::No);
                            cells.widget(cx, ids!(dnp)).set_disabled(cx, row.read_only.is_some());
                            body.button(cx, ids!(fitted.actions.add)).set_enabled(cx, row.read_only.is_none());
                            let mut info = Vec::new();
                            if let Some(why) = &row.read_only { info.push(why.clone()); }
                            info.extend(row.checks.iter().map(|c| (*c).to_string()));
                            if row.approved { info.push("approved".into()); }
                            let inherited = row.inherited.iter().filter(|k| EDITABLE.iter().any(|(key, _)| k.eq_ignore_ascii_case(key))).cloned().collect::<Vec<_>>();
                            if !inherited.is_empty() { info.push(format!("Library: {}", inherited.join(", "))); }
                            let detail = info.join(" · ");
                            widget.accessible = format!("{} · {} · quantity {} · {detail}", row.cells[0], row.cells[2], row.qty);
                            detail
                        };
                        let price = prices.as_ref().and_then(|p| p.get(i));
                        let old = price.is_some_and(|p| p.old);
                        for (id, visible) in [(id!(quote), !old), (id!(old_quote), old)] {
                            let label = body.label(cx, &[id!(price),id]);
                            label.set_visible(cx,visible);
                            if visible { label.set_text(cx,price.map_or("Estimate unavailable",|p| &p.label)); }
                        }
                        body.label(cx, ids!(detail)).set_text(cx, &detail);
                        widget.bound = Some((sheet.clone(), i));
                    }
                }
                item.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DesignSnapshot, StageFlags, StateOf, Tagged};
    use makepad_widgets_core::makepad_draw::cx_draw::CxDraw;

    #[test]
    fn retained_sheet_rebinds_its_cells_after_a_failed_rebuild() {
        let design = Arc::new(fab_elec_fixtures::ledctl());
        let unit = &design.units[0];
        let mut sheet = fab_elec_bom::sheet(&design, unit.id, &Default::default()).unwrap();
        let (first, _) = sheet.lines[0];
        sheet.rows[first].cells[4..8].clone_from_slice(&["MPN-42".into(), String::new(), "P-7".into(), String::new()]);
        let sheet = Arc::new(sheet);
        let mut live = ElecLive::new(Arc::new(DesignSnapshot {
            proposals: Default::default(),
            profiles: Arc::default(), profile_diags: Arc::default(),
            design: design.clone(), preview: None, preview_tag: None, preview_by: None, created: vec![],
            last_commit: None, sheets: vec![], proposal: None, job: None, conn: vec![], rules: vec![], fills: vec![],
            sysnets: None, export: vec![], violations: Tagged { unit: None, of: StateOf::Committed, generation: 0, value: Arc::default() },
            awaiting: Arc::default(), cases: Arc::default(), checking: vec![], last_refusal: None, stale: StageFlags::NONE,
            head: String::new(), benches: Arc::default(), source_map: None, docs: None, catalog: None,
        }));
        live.bom = Some(Arc::new(Shown {
            design: design.clone(), quantity: 10, grouped: true, units: vec![UnitShown {
                unit: unit.id, name: unit.name.clone(), sheet: sheet.clone(), grouped_sheet: sheet.clone(),
                facts: Arc::new(fab_elec_bom::BoardFacts::from_board(unit.board.as_ref().unwrap())),
                prepared: None, rates: None, placements: Arc::default(), cost: String::new(), details: String::new(),
                prices: Arc::default(), book: None, settings: Default::default(),
            }],
        }));
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.set_global(live);
        let (mut pane, template) = cx.with_vm(|vm| {
            makepad_widgets_core::script_mod_with(vm, Default::default(), &[]);
            super::script_mod(vm);
            let value = vm.eval(script! { use mod.widgets.* BomPane{} });
            (WidgetRef::script_from_value(vm, value), vm.new_object_ref(value.as_object().unwrap()))
        });
        let pass = DrawPass::new(&mut cx);
        let mut list = DrawList2d::new(&mut cx);
        let size = dvec2(640.0, 720.0);
        pass.set_size(&mut cx, size);
        for failed in [false, true] {
            let failed_job = if failed {
                let job = jobs::JobSpec::new("Rebuilding the plate").start();
                let id = job.id();
                job.fail("method no_such_shape not found", false);
                // The failed edit keeps the previous sheet. A template reload restores its widget defaults.
                cx.with_vm(|vm| pane.script_apply(vm, &Apply::Reload, &mut Scope::empty(), template.as_object().into()));
                Some(id)
            } else { None };
            cx.redraw_all();
            let event = std::mem::take(&mut cx.new_draw_event);
            {
                let mut draw = CxDraw::new(&mut cx, &event);
                let mut frame = Cx2d::new(&mut draw);
                frame.begin_pass(&pass, Some(1.0));
                list.begin_always(&mut frame);
                frame.begin_root_turtle(size, Layout::flow_down());
                pane.draw_all_unscoped(&mut frame);
                frame.end_pass_sized_turtle();
                list.end(&mut frame);
                frame.end_pass(&pass);
            }
            let row = pane.portal_list(&cx, ids!(rows)).item(&mut cx, 0, id!(Row));
            let cells = row.widget(&cx, ids!(body.fitted.cells));
            for ((name, _), expected) in EDITABLE.iter().zip(["MPN-42", "—", "P-7", "—"]) {
                assert_eq!(cells.child(LiveId::from_str(name)).text(), expected, "failed rebuild {failed}, {name}");
            }
            assert_eq!(pane.widget(&cx, ids!(tools.title)).text(), format!("{} · {} BOM lines", unit.name, sheet.rows.len()));
            if let Some(id) = failed_job { jobs::board().dismiss(id); }
        }
    }
}
