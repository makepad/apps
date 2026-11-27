//! Catalog actions for producing and inspecting held electronics proposals.
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};

pub const PROPOSE: ActionDef = ActionDef {
    name: "elec.step.propose", label: "Propose a step", group: "Steps",
    help: "Propose a stage's documents together. It appears in the person's Review strip and is checked on the pool. The reply comes when the check finishes. A refusal gives its reason. Only the person can accept it.",
    returns: "id: int
shown: int  the verified revision shown for review
state: text  ready
docs: [{..}]
  doc: text
  added: int
  removed: int
checks: {..}
  counts: [{..}]
    kind: text
    severity: text
    count: int
  pending: bool
requirements: {..}  the person's requirements judged on the proposed design; absent when the project has none
  pass: int
  fail: int
  unknown: int
  waived: int
  failures: [{..}]  each requirement the proposal breaks
    name: text  the person's words
    why: text  what was measured and the bound it breaks
  unjudged: [{..}]  each requirement the proposal cannot settle yet
    name: text
    why: text",
    fields: &[
        FieldDef { name: "slot", kind: FieldKind::Text, required: true, default: "",
            help: "The stage and scope, such as requirements or schematic:power. A new proposal replaces the pending one in this slot." },
        FieldDef { name: "label", kind: FieldKind::Text, required: true, default: "",
            help: "What the person reads in the Review strip." },
        FieldDef { name: "edits", kind: FieldKind::Splash, required: true, default: "",
            help: r#"Pass as a JSON array of {"doc", "op", "text"} objects, never as a quoted string or Splash text: doc is the project document name, op is "create", "replace" or "remove" (include text, the whole new document, for create and replace; omit text for remove). In .req.splash use positional require(name, measure, bound) and wish(words), with board.* or case.fits for the only board."# },
    ],
    inputs: &[], risk: Risk::Preview, keys: "",
};

pub const LIST: ActionDef = ActionDef {
    name: "elec.step.list", label: "List proposals", group: "Steps",
    help: "Read the held proposals in the Review strip, their authors, document changes and latest checks.",
    returns: "proposals: [{..}]
  id: int
  shown: int
  slot: text
  label: text
  author: {..}
    kind: text  person, ai, agent, disk or peer
    turn: int  for a local AI
    call: int  for an AI or agent
    label: text  for a peer
  state: text  waiting, checking, ready, unchecked, out of date or not usable, with a reason for the last two
  docs: [{..}]
    doc: text
    added: int
    removed: int
  checks: {..}  nil until available
    counts: [{..}]
      kind: text
      severity: text
      count: int
    pending: bool
  requirements: {..}  the person's requirements judged on the proposed design; absent when the project has none
    pass: int
    fail: int
    unknown: int
    waived: int
    failures: [{..}]  each requirement the proposal breaks
      name: text  the person's words
      why: text  what was measured and the bound it breaks
    unjudged: [{..}]  each requirement the proposal cannot settle yet
      name: text
      why: text",
    fields: &[], inputs: &[], risk: Risk::Read, keys: "",
};

pub static ACTIONS: &[ActionDef] = &[PROPOSE, LIST];
