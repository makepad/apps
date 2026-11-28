//! A Requirements press follows its tracked chat line, then the proposal from that turn.
use fab_elec_design::By;
use fab_elec_view::req_list::{DraftView, NO_AI_MODEL};
use fab_elec_view::snapshot::{ProposalAuthor, ProposalId, ProposalState, ProposalView};
use makepad_ai_services::state::{EngineState, Status};
use makepad_app_shell::chat::ChatSaid;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

pub const DRAFT_PROMPT: &str = r#"Draft requirements from my brief. Call elec.req.list first, then call elec.doc.read with the filename returned in brief.doc so you keep brief(..) and any existing lines verbatim. Then propose replacing that document with elec.step.propose (slot "requirements", label "Requirements from the brief"). Pass edits as a JSON array of {"doc", "op", "text"} objects (op "create", "replace", or "remove" without "text"), never as a quoted string. For op "replace", "text" must be the entire file, never a partial snippet or single changed line. Write positional Splash statements only (no argument labels like measure: or max:), e.g. require("fits enclosure", board.size, {max: [80mm 50mm]}), require("4-layer board", board.layers, {is: 4}), require("ERC clean", checks(@erc).errors, {max: 0}), wish("battery powered"). Measures: board.size|layers|parts|unplaced|unrouted, checks(@erc|@drc|@system|@route|@case|@custom|@all).errors|warnings, case.fits, bench("b"), bench("b").expect("e"), print.time, print.filament (use board("unit") only when there are multiple boards). Account for every constraint and goal in the brief: write a require(..) for each one Fab can measure (such as size, layer count, checks, case fit, print time or filament) using {max: ..}, {min: ..}, or {is: ..} with the brief's own numbers, and a wish(..) in my words for every other goal Fab cannot measure (such as power source, features, interfaces, or appearance). Never invent a bound. Keep brief(..) exactly as it is."#;

pub struct StageSpec {
    pub slot: &'static str,
    pub label: &'static str,
    pub prompt: &'static str,
}

pub const ARCH_PROMPT: &str = r#"Draft the block architecture from my requirements and brief. Call elec.req.list and elec.doc.list first; only if elec.doc.list returns a circuit or blocks file, call elec.doc.read with the filename returned in its name field (or elec.design.read). Then propose <stem>.blocks.splash and <stem>.ckt.splash together with elec.step.propose (slot "architecture", label "Block architecture"), reusing an existing circuit stem or picking one short lowercase stem. Pass edits as a JSON array of {"doc", "op", "text"} objects (op "create", "replace", or "remove" without "text"), never as a quoted string. For op "replace", "text" must be the entire file, never a partial snippet or single changed line. Use op "create" for files not in elec.doc.list and "replace" for files already listed. Write planned blocks only (planned: true, role, no build, no parts). In <stem>.blocks.splash start with `let e = mod.elec` and define top-level blocks, e.g. `let reg = e.block({name: "3.3V regulator" planned: true role: "3.3V LDO" ports: {vin: e.port(@power_in, {volts: [4.5V 5.5V] current: 120mA}) vout: e.port(@power_out, {volts: 3.3V current: 300mA}) gnd: e.port(@ground)}})` and `let sensor = e.block({name: "Temp sensor" planned: true role: "SPI sensor" ports: {vdd: e.port(@power_in, {volts: [2.7V 3.6V] current: 15mA}) gnd: e.port(@ground) spi: e.port(@bidi, {width: 4}) irq: e.port(@out)}})` (port kinds: @power_in, @power_out, @ground, @in, @out, @bidi, @passive, @pair; units V, mV, A, mA, uA). In <stem>.ckt.splash put one statement per line starting with `use mod.elec.*`, `use mod.project.*`, `let e = mod.elec`, declare rails, nets and buses (`let gnd = e.net({name: "GND" kind: @ground})`, `let v5 = e.net({name: "5V" kind: @power volts: 5V})`, `let v33 = e.net({name: "3V3" kind: @power volts: 3.3V})`, `let irq = e.net({name: "IRQ"})`, `let spi = e.bus("SPI", 4)`), and instantiate each block with `let` (`let ldo = reg({vin: v5 vout: v33 gnd: gnd})`, `let sens = sensor({vdd: v33 gnd: gnd spi: spi irq: irq})`). Drive every power rail from a @power_out port whose volts fit every @power_in and whose current covers the sum of @power_in loads, and connect every signal or bus between at least two blocks. If the proposal reply reports ERC errors in checks or requirements, fix them and re-propose in slot "architecture"."#;

// Interim placement and footprint guidance until imported definitions can join a project.
pub const PARTS_PROMPT: &str = r#"Choose parts for each planned block from my local libraries. Call elec.parts.shortlist once with no arguments first (it returns a ranked shortlist for every planned block; call it again with find or role only for a block whose list came back empty) and call elec.doc.list (call elec.doc.read only if a .part.splash file already exists). Prefer top-ranked local candidates (project, then pinned, used, imported, std; approved and active first; priced over unpriced). Prefer parts with in_project true; a part with in_project false can't be placed yet. A part defined without a footprint can't be placed (ERC reports MissingFootprint), so prefer library parts that have footprints and tell me which blocks need footprints imported. Then propose <stem>.part.splash with elec.step.propose (slot "parts", label "Parts shortlist"), reusing the circuit stem or picking one short lowercase stem. Pass edits as a JSON array of {"doc", "op", "text"} objects (op "create", "replace", or "remove" without "text"), never as a quoted string. For op "replace", "text" must be the entire file, never a partial snippet or single changed line. Use op "create" for a file not in elec.doc.list and "replace" for one already listed. In <stem>.part.splash record the pick and alternates for each planned block in `//` comments, and add `e.define(...)` definitions (starting with `let e = mod.elec` and `let f = e.f`) only for project parts not already in the local library, e.g. `e.define("ldo_3v3", {kind: @regulator prefix: "U" lifecycle: @active fields: {mpn: "AP2112K-3.3" supplier: "LCSC" supplier_part: "C51118" "alt.1.mpn": "TLV75533PDBVR"} symbol: f.ic_symbol("ldo_3v3", {left: [["VIN" @power_in] ["EN" @in]] right: [["VOUT" @power_out]] bottom: [["GND" @power_in]]})})`. Never set `approved: true` (only the person approves parts). If no local part fits a planned block, do not fabricate a footprint or fetch from the network: write `// import: LCSC <part>` in <stem>.part.splash and tell me which supplier part number to import."#;

pub const REQ_STAGE: StageSpec = StageSpec { slot: "requirements", label: "Requirements", prompt: DRAFT_PROMPT };
// Interim power-flag guidance until imported parts and footprints can join a project.
pub const SCHEMATIC_PROMPT: &str = r#"Draft the schematic from my architecture and chosen parts. Call elec.req.list and elec.doc.list first, then call elec.doc.read for the <stem>.blocks.splash, <stem>.ckt.splash and <stem>.part.splash files elec.doc.list returns (call elec.parts.shortlist only for a planned block <stem>.part.splash gives no pick). Then propose the filled <stem>.blocks.splash and <stem>.ckt.splash, and <stem>.part.splash only if it needs a project part definition, together with elec.step.propose (slot "schematic", label "Schematic"). Pass edits as a JSON array of {"doc", "op", "text"} objects (op "create", "replace", or "remove" without "text"), never as a quoted string. For op "replace", "text" must be the entire file, never a partial snippet or single changed line. Use op "create" for a file not in elec.doc.list and "replace" for one already listed. Replace every planned block (planned: true, no build) with the same block, keeping its name, role and ports, without planned: true, and with a build function that places the picked parts and wires their pins to the block's ports (p.<port>) and params (o.<param>), separating statements with `;`, e.g. `let reg = e.block({name: "3.3V regulator" role: "3.3V LDO" ports: {vin: e.port(@power_in, {volts: [4.5V 5.5V] current: 120mA}) vout: e.port(@power_out, {volts: 3.3V current: 300mA}) gnd: e.port(@ground)} build: fn(p, o) { let u = e.part("ldo_3v3", {pins: {VIN: p.vin EN: p.vin GND: p.gnd VOUT: p.vout}}); let c_in = e.c(1uF, p.vin, p.gnd, {package: "0603"}); let c_out = e.c(1uF, p.vout, p.gnd, {package: "0603"}); e.near(c_in, u.VIN, 2mm); e.near(c_out, u.VOUT, 2mm) }})` and `let lamp = e.block({name: "Status LED" role: "status LED" ports: {ctl: e.port(@in) vdd: e.port(@power_in, {volts: 3.3V current: 5mA})} build: fn(p, o) { let d = e.led({color: @green package: "0603" current: 5mA}); let r = e.r(330ohm, p.vdd, d.A, {package: "0402"}); e.connect(d.K, p.ctl) }})` (parts: e.part("<part name>", {pins: {PIN: net} nc: [@PIN]}) for a picked or project part; e.r, e.c and e.led for passives and LEDs). Keep the nets and block instances in <stem>.ckt.splash unless a port changed. Give every part pin a net or list it in nc, decouple every IC power pin, and never set approved: true. If ERC reports an externally supplied ground or rail as undriven, wire the standard power flag to that net in the build, e.g. `let g = e.part(mod.std.power_flag(), {pins: [p.gnd]})`. If the proposal reply lists errors in checks.findings or requirements, fix them and re-propose in slot "schematic" (when checks.next is set, read the rest with elec.step.list {findings_from: checks.next}) until ERC clean passes."#;

pub const SCHEMATIC_STAGE: StageSpec = StageSpec { slot: "schematic", label: "Schematic", prompt: SCHEMATIC_PROMPT };

pub const ARCH_STAGE: StageSpec = StageSpec { slot: "architecture", label: "Architecture", prompt: ARCH_PROMPT };
pub const PARTS_STAGE: StageSpec = StageSpec { slot: "parts", label: "Parts", prompt: PARTS_PROMPT };

pub fn admit(state: Option<&EngineState>) -> Result<(), &'static str> {
    if state.is_some_and(EngineState::has_model) { Ok(()) } else { Err(NO_AI_MODEL) }
}

pub struct Draft {
    spec: &'static StageSpec,
    receipt: u64,
    turn: Option<u64>,
    ended: bool,
    status: Status,
    model_secs: u64,
    calls: HashMap<u64, bool>, // outstanding call id -> elec.step.propose
    cancelled: bool,
    failed: bool,
    refusal: Option<String>,
    matched: Option<ProposalId>,
    seen: Weak<Vec<Arc<ProposalView>>>,
    view: Option<DraftView>,
}

impl Draft {
    pub fn new(spec: &'static StageSpec, receipt: u64) -> Self {
        Self {
            spec, receipt, turn: None, ended: false, status: Status::Thinking, model_secs: 0,
            calls: HashMap::new(), cancelled: false, failed: false, refusal: None, matched: None, seen: Weak::new(),
            view: Some(DraftView { text: "Sent to the AI".into(), review: None, done: false }),
        }
    }

    pub fn spec(&self) -> &'static StageSpec { self.spec }
    pub fn receipt(&self) -> u64 { self.receipt }
    pub fn view(&self) -> Option<&DraftView> { self.view.as_ref() }

    fn show(&mut self, text: String, done: bool) {
        self.view = Some(DraftView { text, review: None, done });
    }

    pub fn said(&mut self, said: ChatSaid) {
        if self.ended { return }
        match said {
            ChatSaid::Queued if self.turn.is_none() => self.show("Waiting for the AI to finish the current request".into(), false),
            ChatSaid::Started(turn) if self.turn.is_none() => {
                self.turn = Some(turn);
                self.show("Thinking".into(), false);
            }
            ChatSaid::Refused(reason) if self.turn.is_none() => {
                self.ended = true;
                self.show(format!("Not sent: {reason}"), true);
            }
            ChatSaid::Cancelled if self.turn.is_none() => {
                self.ended = true;
                self.show("Cancelled".into(), true);
            }
            ChatSaid::Stopped if self.turn.is_some() => {
                self.cancelled = true;
                self.end();
            }
            // The status that follows carries the error text; `end` falls back to this flag.
            ChatSaid::Failed if self.turn.is_some() => self.failed = true,
            _ => {}
        }
    }

    pub fn turn_started(&mut self, turn: u64) {
        if self.turn.is_some_and(|own| turn > own) { self.end(); }
    }

    pub fn call(&mut self, id: u64, turn: u64, is_propose: bool) {
        if self.turn == Some(turn) && !self.ended { self.calls.insert(id, is_propose); }
    }

    pub fn answered(&mut self, id: u64, ok: bool, error_text: &str) {
        if self.calls.remove(&id) == Some(true) {
            self.refusal = (!ok).then(|| error_text.to_owned());
        }
    }

    pub fn cancelled(&mut self, id: u64) {
        if self.calls.remove(&id).is_some() { self.cancelled = true; }
    }

    fn end(&mut self) {
        if self.ended { return }
        self.ended = true;
        self.calls.clear();
        if self.matched.is_some() { return }
        let text = if self.cancelled { "Cancelled".into() }
            else if let Status::Error(e) = &self.status { format!("The AI stopped: {e}") }
            else if self.failed { "The AI stopped with an error".into() }
            else if let Some(e) = &self.refusal { format!("Refused: {e}") }
            else { "Finished without a proposal".into() };
        self.show(text, true);
    }

    pub fn status(&mut self, status: &Status, model_secs: u64) {
        if self.turn.is_none() || self.ended { return }
        if self.failed {
            self.status.clone_from(status);
            self.end();
            return;
        }
        if &self.status == status && self.model_secs == model_secs { return }
        self.status.clone_from(status);
        self.model_secs = model_secs;
        if !status.is_busy() { self.end(); return }
        if self.matched.is_some() { return }
        let text = match status {
            Status::Loading { .. } => "Loading the model".into(),
            Status::Thinking if model_secs > 0 => format!("Thinking · {model_secs} s"),
            Status::Thinking => "Thinking".into(),
            Status::Streaming => "Writing".into(),
            Status::WaitingForTool => "Waiting for a tool".into(),
            Status::Idle | Status::Error(_) => unreachable!("handled by end"),
        };
        self.show(text, false);
    }

    pub fn snapshot(&mut self, rows: &Arc<Vec<Arc<ProposalView>>>) {
        if self.turn.is_some() && (self.seen.strong_count() == 0 || self.seen.as_ptr() != Arc::as_ptr(rows)) {
            self.seen = Arc::downgrade(rows);
            self.proposals(rows);
        }
    }

    fn proposals(&mut self, rows: &[Arc<ProposalView>]) {
        let Some(own) = self.turn else { return };
        let newest = rows.iter()
            .filter(|p| matches!(p.by, ProposalAuthor::Local(By::Ai { turn, .. }) if turn == own))
            // Requirements has always offered the newest row from its turn. Stages that
            // automatically reveal a row must reveal only a proposal for their own slot.
            .filter(|p| self.spec.slot == REQ_STAGE.slot || p.slot.as_ref() == self.spec.slot
                || (self.spec.slot == SCHEMATIC_STAGE.slot && p.slot.strip_prefix("schematic:").is_some_and(|unit| !unit.is_empty())))
            .max_by_key(|p| p.id.0);
        // Removing a reviewed row must not take the person back to an older proposal.
        let Some(row) = newest.filter(|p| self.matched.is_none_or(|id| p.id.0 >= id.0)) else {
            if self.matched.is_some() { self.view = None; }
            return;
        };
        self.matched = Some(row.id);
        self.view = Some(match &row.state {
            ProposalState::Unchecked | ProposalState::Checking =>
                DraftView { text: "Checking the proposal".into(), review: None, done: false },
            ProposalState::Ready =>
                DraftView { text: "Ready: review it in the strip".into(), review: Some(row.id), done: true },
            ProposalState::Stale(r) =>
                DraftView { text: format!("The proposal went out of date: {r}"), review: None, done: true },
            ProposalState::Refused(r) =>
                DraftView { text: format!("Refused: {r}"), review: None, done: true },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shows(d: &Draft, text: &str, review: Option<ProposalId>, done: bool) {
        assert_eq!(d.view().map(|v| (v.text.as_str(), v.review, v.done)), Some((text, review, done)),
            "the draft should show {text:?} with its own review target and completion state");
    }

    fn row(id: u64, turn: u64, state: ProposalState) -> Arc<ProposalView> {
        Arc::new(ProposalView {
            id: ProposalId(id), by: ProposalAuthor::Local(By::Ai { turn, call: id }), state,
            shown: 1, slot: format!("slot-{id}").into(), label: "Proposed".into(), run: Arc::default(),
            docs: Arc::default(), design: None, boards: Arc::default(), sheets: Arc::default(),
            created: Arc::default(), checks: None, requirements: None,
        })
    }

    #[test]
    fn follows_its_receipt_turn_and_review_row() {
        let mut draft = Draft::new(&REQ_STAGE, 7);
        shows(&draft, "Sent to the AI", None, false);
        draft.said(ChatSaid::Queued);
        draft.turn_started(2);
        draft.status(&Status::Idle, 0);
        draft.turn_started(3);
        shows(&draft, "Waiting for the AI to finish the current request", None, false);
        draft.said(ChatSaid::Started(3));
        shows(&draft, "Thinking", None, false);
        draft.status(&Status::Loading { phase: "prefill".into(), fraction: 0.2 }, 0);
        shows(&draft, "Loading the model", None, false);
        draft.status(&Status::Thinking, 9);
        shows(&draft, "Thinking · 9 s", None, false);
        draft.status(&Status::Streaming, 0);
        shows(&draft, "Writing", None, false);
        draft.status(&Status::WaitingForTool, 0);
        draft.proposals(&[row(9, 2, ProposalState::Ready)]);
        shows(&draft, "Waiting for a tool", None, false);
        let old = row(4, 3, ProposalState::Ready);
        for state in [ProposalState::Unchecked, ProposalState::Checking] {
            draft.proposals(&[row(5, 3, state), old.clone(), row(9, 2, ProposalState::Ready)]);
            shows(&draft, "Checking the proposal", None, false);
        }
        draft.turn_started(4);
        let ready = Arc::new(vec![row(5, 3, ProposalState::Ready), old.clone()]);
        draft.snapshot(&ready);
        let shown = draft.view().unwrap().text.as_ptr();
        draft.snapshot(&ready);
        assert_eq!(draft.view().unwrap().text.as_ptr(), shown, "the same published snapshot must keep the cached view");
        drop(ready);
        assert_eq!(draft.seen.strong_count(), 0, "the draft must release the old snapshot");
        shows(&draft, "Ready: review it in the strip", Some(ProposalId(5)), true);
        draft.snapshot(&Arc::new(vec![row(5, 3, ProposalState::Stale("edited".into()))]));
        shows(&draft, "The proposal went out of date: edited", None, true);
        draft.proposals(&[row(5, 3, ProposalState::Refused("invalid".into()))]);
        shows(&draft, "Refused: invalid", None, true);
        draft.proposals(&[old]);
        assert!(draft.view().is_none(), "removing the matched row hides the line even when an older row remains");
    }

    #[test]
    fn architecture_reveals_only_its_own_stage() {
        let mut draft = Draft::new(&ARCH_STAGE, 1);
        draft.said(ChatSaid::Started(3));
        let mut architecture = row(5, 3, ProposalState::Ready);
        Arc::make_mut(&mut architecture).slot = "architecture".into();
        draft.proposals(&[architecture, row(6, 3, ProposalState::Ready)]);
        shows(&draft, "Ready: review it in the strip", Some(ProposalId(5)), true);
    }

    #[test]
    fn parts_reveals_only_its_own_stage() {
        let mut draft = Draft::new(&PARTS_STAGE, 1);
        draft.said(ChatSaid::Started(3));
        let mut parts = row(5, 3, ProposalState::Ready);
        Arc::make_mut(&mut parts).slot = "parts".into();
        draft.proposals(&[parts, row(6, 3, ProposalState::Ready)]);
        shows(&draft, "Ready: review it in the strip", Some(ProposalId(5)), true);
    }

    #[test]
    fn schematic_reveals_its_unit_scoped_proposal() {
        let mut draft = Draft::new(&SCHEMATIC_STAGE, 1);
        draft.said(ChatSaid::Started(3));
        let scoped = |id, turn, slot: &str| {
            let mut p = row(id, turn, ProposalState::Ready);
            Arc::make_mut(&mut p).slot = slot.into();
            p
        };
        draft.proposals(&[scoped(5, 3, "schematic:power"), scoped(6, 3, "parts:power"), scoped(7, 4, "schematic:power")]);
        shows(&draft, "Ready: review it in the strip", Some(ProposalId(5)), true);
    }

    #[test]
    fn only_its_own_calls_and_status_explain_how_it_ended() {
        let mut engine = EngineState { provider_label: makepad_ai_services::state::NO_MODEL_LABEL.into(), ..Default::default() };
        assert_eq!(admit(None), Err(NO_AI_MODEL), "a missing chat must explain why Draft is disabled");
        assert_eq!(admit(Some(&engine)), Err(NO_AI_MODEL), "a disconnected chat must explain why Draft is disabled");
        engine.provider_label = "Model".into();
        engine.status = Status::Streaming;
        assert_eq!(admit(Some(&engine)), Ok(()), "a busy connected model must accept a queued draft");

        for (said, text) in [(ChatSaid::Cancelled, "Cancelled"), (ChatSaid::Refused("offline"), "Not sent: offline")] {
            let mut draft = Draft::new(&REQ_STAGE, 1);
            draft.said(said);
            draft.status(&Status::Thinking, 20);
            shows(&draft, text, None, true);
        }
        let started = || { let mut d = Draft::new(&REQ_STAGE, 1); d.said(ChatSaid::Started(3)); d };
        let mut draft = started();
        draft.call(1, 2, true);
        draft.cancelled(1);
        draft.call(2, 3, false);
        draft.answered(2, false, "list failed");
        draft.turn_started(4);
        draft.status(&Status::Error("later turn".into()), 0);
        shows(&draft, "Finished without a proposal", None, true);

        for (cancel, error, retry_ok, text) in [
            (true, true, false, "Cancelled"), (false, true, false, "The AI stopped: offline"),
            (false, false, false, "Refused: invalid"), (false, false, true, "Finished without a proposal"),
        ] {
            let mut draft = started();
            draft.call(1, 3, true);
            draft.answered(1, false, "invalid");
            draft.call(2, 3, true);
            if cancel { draft.cancelled(2); }
            if retry_ok { draft.answered(2, true, ""); }
            draft.status(&if error { Status::Error("offline".into()) } else { Status::Idle }, 0);
            shows(&draft, text, None, true);
        }

        let mut draft = started();
        draft.said(ChatSaid::Stopped);
        draft.status(&Status::Idle, 0);
        shows(&draft, "Cancelled", None, true);

        let mut draft = started();
        draft.proposals(&[row(5, 3, ProposalState::Checking)]);
        draft.said(ChatSaid::Stopped);
        shows(&draft, "Checking the proposal", None, false);
        draft.proposals(&[row(5, 3, ProposalState::Ready)]);
        shows(&draft, "Ready: review it in the strip", Some(ProposalId(5)), true);

        let mut draft = started();
        draft.said(ChatSaid::Failed);
        draft.status(&Status::Error("context full".into()), 0);
        shows(&draft, "The AI stopped: context full", None, true);

        let mut draft = started();
        draft.said(ChatSaid::Failed);
        draft.turn_started(4);
        shows(&draft, "The AI stopped with an error", None, true);

        // A busy status after the failure (the next line started, or the model loading) must
        // not leave the failed draft open.
        for busy in [Status::Thinking, Status::Loading { phase: "prefill".into(), fraction: 0.2 }] {
            let mut draft = started();
            draft.status(&Status::Thinking, 0);
            draft.said(ChatSaid::Failed);
            draft.status(&busy, 0);
            shows(&draft, "The AI stopped with an error", None, true);
        }
    }
}
