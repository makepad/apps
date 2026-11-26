//! A director and parallel scene authors: the way a long,
//! chaptered video (a 2–3 minute music video with a recurring character)
//! is made by one model directing several.
//!
//! The director is an ordinary [`Agent`] with two more tools:
//!
//! - `style_guide` — the rules every scene author reads first: palette,
//!   type, characters and rigs (the shared modules), timing helpers, the
//!   performance budget, which shared documents are off-limits;
//! - `delegate` — hand scenes to scene authors, each writing only its own
//!   Motion document; they run in parallel, and the call answers when they
//!   have all finished, with each one's summary and last critique. Calling
//!   it again with an existing author's id sends that author a fix, in its
//!   own conversation.
//!
//! A scene author is an [`Agent`] whose tools are the Motion and feedback
//! tools, held to its own document: writes to another document are
//! refused, and the looking tools default to it. Everything every agent
//! does is its own transcript, which the panel shows side by side.
//!
//! The team is passive like the agent: [`Team::poll`] hands the host the
//! calls to run, each tagged with the agent that made it, and
//! [`Team::result`] takes the answers back.

use crate::agent::{Action, Agent, Brief, Doc, Limits, Outcome, State};
use crate::brain::Backend;
#[cfg(test)]
use crate::brain::{Brain, BrainEvent, BrainImage, CallResult};
use crate::tools::ToolDef;
use makepad_strict_json::{self as json, Value};

/// The director's id in [`TeamAction::agent`] and the panel.
pub const DIRECTOR: &str = "director";

/// The host tools a scene author gets: Motion writing, looking and
/// reading. Everything that changes the cut stays the director's.
pub const AUTHOR_TOOLS: &[&str] = &[
    "motion_catalog",
    "motion_fields",
    "read_motion",
    "validate_motion",
    "write_motion",
    "patch_motion",
    "compile_shader",
    "motion_templates",
    "render_kit_effects",
    "list_effects",
    "search_library",
    "asset_info",
    "beat_grid",
    "analyze_music",
    "voice_lines",
    "inspect",
    "frame",
    "strip",
    "onion",
];

/// The director's own team tools.
pub fn team_tools() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "style_guide",
            "Write (or replace) the style guide every scene author reads before it writes: the look (palette as hex, typefaces and sizes, paper/brush texture), the characters and props and the shared modules that draw them (and that they are off-limits: authors import them, never change them), the timing helpers and how scenes sync to the beat and the lyrics, transitions between chapters, the performance budget (layers, shader cost), and anything an author must never do.",
            r#"{"type": "object", "properties": {"text": {"type": "string", "description": "the whole guide, in Markdown"}}, "required": ["text"]}"#,
        ),
        ToolDef::new(
            "delegate",
            "Give scenes to scene authors who work in parallel, each writing only its own Motion document (by name) against the style guide, checking it with inspect and strips until it is clean. Answers when all of them have finished, with each author's summary and last critique. Use an existing author's id to send it a targeted fix in its own conversation. Place the finished documents on the timeline yourself (add_graphic).",
            r#"{"type": "object", "properties": {"tasks": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string", "description": "the author's id, e.g. ch3"}, "motion": {"type": "string", "description": "the one Motion document name it owns"}, "brief": {"type": "string", "description": "the scene: its span on the cut's timeline, what happens beat by beat (with times or beat/word references), the lyrics or voice lines it covers, which shared modules to use; or, for an existing author, the fix"}}, "required": ["id", "motion", "brief"]}}}, "required": ["tasks"]}"#,
        ),
    ]
}

pub struct Member {
    pub id: String,
    /// The Motion document it owns.
    pub motion: String,
    pub agent: Agent,
}

/// A call for the host, and which agent made it.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamAction {
    pub agent: String,
    pub action: Action,
}

/// Makes a scene author's brain from its system text and tools.
pub type Spawn = Box<dyn FnMut(&str, &[ToolDef]) -> Result<Box<dyn crate::brain::Brain>, String>>;

pub struct Team {
    vision: bool,
    spawn: Spawn,
    host_tools: Vec<ToolDef>,
    director: Agent,
    members: Vec<Member>,
    style: String,
    /// Delegations waiting for their authors: (the director's call id,
    /// author ids).
    waiting: Vec<(String, Vec<String>)>,
    docs: Vec<Doc>,
    limits: Limits,
    generation: u64,
}

fn text_arg(args: &Value, key: &str) -> String {
    args.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

impl Team {
    pub fn start(backend: &Backend, host_tools: Vec<ToolDef>, brief: &Brief, docs: Vec<Doc>, limits: Limits) -> Result<Team, String> {
        let mut tools = host_tools.clone();
        tools.extend(team_tools());
        let director = Agent::start_team(backend, tools, brief, docs.clone(), limits)?;
        let author = backend.clone();
        let spawn: Spawn = Box::new(move |system, tools| author.start(system, tools));
        Ok(Team::with_director(director, backend.vision(), spawn, host_tools, docs, limits))
    }

    /// A team over a director and a brain factory already made (tests, a
    /// headless evaluator).
    pub fn with_director(director: Agent, vision: bool, spawn: Spawn, host_tools: Vec<ToolDef>, docs: Vec<Doc>, limits: Limits) -> Team {
        Team { vision, spawn, host_tools, director, members: Vec::new(), style: String::new(), waiting: Vec::new(), docs, limits, generation: 0 }
    }

    pub fn director(&self) -> &Agent {
        &self.director
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// Every agent, the director first: `(id, agent)`.
    pub fn agents(&self) -> Vec<(&str, &Agent)> {
        let mut out = vec![(DIRECTOR, &self.director)];
        out.extend(self.members.iter().map(|member| (member.id.as_str(), &member.agent)));
        out
    }

    pub fn style_guide(&self) -> &str {
        &self.style
    }

    pub fn busy(&self) -> bool {
        self.director.state().busy() || self.members.iter().any(|member| member.agent.state().busy())
    }

    /// Bumped whenever any agent's transcript changed.
    pub fn generation(&self) -> u64 {
        self.generation + self.director.generation() + self.members.iter().map(|member| member.agent.generation()).sum::<u64>()
    }

    pub fn observe(&mut self, docs: Vec<Doc>) {
        self.docs = docs.clone();
        self.director.observe(docs.clone());
        for member in &mut self.members {
            member.agent.observe(docs.clone());
        }
    }

    /// The person's line goes to the director.
    pub fn say(&mut self, text: String) {
        self.director.say(text);
    }

    pub fn cancel(&mut self) {
        for member in &mut self.members {
            member.agent.cancel();
        }
        self.director.cancel();
        self.waiting.clear();
    }

    fn member_tools(&self) -> Vec<ToolDef> {
        self.host_tools.iter().filter(|tool| AUTHOR_TOOLS.contains(&tool.name.as_str())).cloned().collect()
    }

    fn delegate(&mut self, call_id: String, args: &Value) {
        let Some(tasks) = args.get("tasks").and_then(Value::as_arr).filter(|tasks| !tasks.is_empty()) else {
            self.director.result(&call_id, Outcome::error("`tasks` is empty: give at least one {id, motion, brief}"));
            return;
        };
        let mut ids = Vec::new();
        let mut refused = Vec::new();
        for task in tasks {
            let (id, motion, brief) = (text_arg(task, "id"), text_arg(task, "motion"), text_arg(task, "brief"));
            if id.is_empty() || motion.is_empty() || brief.trim().is_empty() || id == DIRECTOR {
                refused.push(format!("a task needs an id (not `{DIRECTOR}`), a motion name and a brief: {}", task.to_json()));
                continue;
            }
            match self.members.iter_mut().find(|member| member.id == id) {
                Some(member) if member.motion != motion => {
                    refused.push(format!("`{id}` owns `{}`, not `{motion}`; use another id for another document", member.motion));
                    continue;
                }
                Some(member) => {
                    if member.agent.state().busy() {
                        member.agent.say(format!("[the director says] {brief}"));
                    } else {
                        member.agent.say(format!("The director asks for a change:\n{brief}"));
                    }
                }
                None => {
                    if let Some(owner) = self.members.iter().find(|member| member.motion == motion) {
                        refused.push(format!("`{motion}` already belongs to `{}`", owner.id));
                        continue;
                    }
                    let system = crate::guide::author_system(self.vision);
                    let first = crate::guide::author_message(&self.style, &id, &motion, &brief);
                    let mut tools = crate::tools::own_tools();
                    tools.extend(self.member_tools());
                    match (self.spawn)(&system, &tools) {
                        Ok(brain) => {
                            let agent = Agent::start_on(brain, tools, first, brief.clone(), &motion, self.docs.clone(), self.limits);
                            self.members.push(Member { id: id.clone(), motion, agent });
                        }
                        Err(error) => {
                            refused.push(format!("`{id}` could not start: {error}"));
                            continue;
                        }
                    }
                }
            }
            ids.push(id);
        }
        if ids.is_empty() {
            self.director.result(&call_id, Outcome::error(refused.join("\n")));
            return;
        }
        if !refused.is_empty() {
            self.director.note(format!("Some tasks were refused: {}", refused.join("; ")));
        }
        self.generation += 1;
        self.waiting.push((call_id, ids));
    }

    /// A finished delegation's answer: each author's state, summary and
    /// last critique.
    fn report(&self, ids: &[String]) -> Outcome {
        let mut items = Vec::new();
        for id in ids {
            let Some(member) = self.members.iter().find(|member| &member.id == id) else { continue };
            let state = match member.agent.state() {
                State::Done => "done".to_string(),
                State::Stopped(why) => format!("stopped: {why}"),
                State::Failed(why) => format!("failed: {why}"),
                _ => "working".into(),
            };
            let critique = member.agent.entries().iter().rev().find_map(|entry| match entry {
                crate::agent::Entry::Critique(c) => Some(json::obj(vec![("verdict", json::s(c.verdict.clone())), ("issues", Value::Arr(c.issues.iter().map(|i| json::s(i.clone())).collect()))])),
                _ => None,
            });
            items.push(json::obj(vec![
                ("id", json::s(id.clone())),
                ("motion", json::s(member.motion.clone())),
                ("state", json::s(state)),
                ("summary", json::s(member.agent.summary().unwrap_or(""))),
                ("changes", Value::Int(member.agent.revisions().len() as i64)),
                ("last_critique", critique.unwrap_or(Value::Null)),
            ]));
        }
        Outcome::ok(json::obj(vec![("authors", Value::Arr(items))]).to_json())
    }

    /// Advance every agent; the host runs what comes back.
    pub fn poll(&mut self) -> Vec<TeamAction> {
        let mut out = Vec::new();
        for action in self.director.poll() {
            match action.name.as_str() {
                "style_guide" => {
                    let text = text_arg(&action.args, "text");
                    let outcome = if text.trim().is_empty() {
                        Outcome::error("`text` is empty")
                    } else {
                        self.style = text;
                        self.generation += 1;
                        Outcome::ok(format!("Style guide recorded ({} lines); every scene author reads it first.", self.style.lines().count()))
                    };
                    self.director.result(&action.call_id, outcome);
                }
                "delegate" => self.delegate(action.call_id, &action.args),
                _ => out.push(TeamAction { agent: DIRECTOR.into(), action }),
            }
        }
        for member in &mut self.members {
            for mut action in member.agent.poll() {
                if let Some(refusal) = hold_to_own(&member.motion, &mut action) {
                    member.agent.result(&action.call_id, Outcome::error(refusal));
                    continue;
                }
                out.push(TeamAction { agent: member.id.clone(), action });
            }
        }
        // Delegations whose authors have all finished answer the director.
        let mut answered = Vec::new();
        for (index, (_, ids)) in self.waiting.iter().enumerate() {
            let done = ids.iter().all(|id| self.members.iter().find(|member| &member.id == id).is_none_or(|member| !member.agent.state().busy()));
            if done {
                answered.push(index);
            }
        }
        for index in answered.into_iter().rev() {
            let (call_id, ids) = self.waiting.remove(index);
            let outcome = self.report(&ids);
            self.director.result(&call_id, outcome);
            self.generation += 1;
        }
        out
    }

    pub fn result(&mut self, agent: &str, call_id: &str, outcome: Outcome) {
        if agent == DIRECTOR {
            self.director.result(call_id, outcome);
        } else if let Some(member) = self.members.iter_mut().find(|member| member.id == agent) {
            member.agent.result(call_id, outcome);
        }
    }
}

/// Scene authors without a director: a production that planned its
/// scenes itself (the style guide and each scene's brief written before)
/// starts one author per scene at once. Each holds to its own document as
/// under a director.
pub struct Authors {
    vision: bool,
    members: Vec<Member>,
    generation: u64,
}

impl Authors {
    /// One author per `(id, motion, brief)`, all reading `style` first.
    pub fn start(backend: &Backend, host_tools: &[ToolDef], style: &str, tasks: &[(String, String, String)], docs: Vec<Doc>, limits: Limits) -> Result<Authors, String> {
        if tasks.is_empty() {
            return Err("no scenes to write".into());
        }
        let mut tools = crate::tools::own_tools();
        tools.extend(host_tools.iter().filter(|tool| AUTHOR_TOOLS.contains(&tool.name.as_str())).cloned());
        let system = crate::guide::author_system(backend.vision());
        let mut members = Vec::with_capacity(tasks.len());
        for (id, motion, brief) in tasks {
            let brain = backend.start(&system, &tools)?;
            let first = crate::guide::author_message(style, id, motion, brief);
            let agent = Agent::start_on(brain, tools.clone(), first, brief.clone(), motion, docs.clone(), limits);
            members.push(Member { id: id.clone(), motion: motion.clone(), agent });
        }
        Ok(Authors { vision: backend.vision(), members, generation: 0 })
    }

    pub fn vision(&self) -> bool {
        self.vision
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn busy(&self) -> bool {
        self.members.iter().any(|member| member.agent.state().busy())
    }

    pub fn generation(&self) -> u64 {
        self.generation + self.members.iter().map(|member| member.agent.generation()).sum::<u64>()
    }

    pub fn poll(&mut self) -> Vec<TeamAction> {
        let mut out = Vec::new();
        for member in &mut self.members {
            for mut action in member.agent.poll() {
                if let Some(refusal) = hold_to_own(&member.motion, &mut action) {
                    member.agent.result(&action.call_id, Outcome::error(refusal));
                    continue;
                }
                out.push(TeamAction { agent: member.id.clone(), action });
            }
        }
        out
    }

    pub fn result(&mut self, agent: &str, call_id: &str, outcome: Outcome) {
        if let Some(member) = self.members.iter_mut().find(|member| member.id == agent) {
            member.agent.result(call_id, outcome);
        }
    }

    /// The person's line goes to every author still working.
    pub fn say(&mut self, text: String) {
        for member in self.members.iter_mut().filter(|member| member.agent.state().busy()) {
            member.agent.say(text.clone());
        }
        self.generation += 1;
    }
}

/// Hold a scene author to its own document: writes elsewhere are refused,
/// and a looking tool without a target looks at its document.
fn hold_to_own(motion: &str, action: &mut Action) -> Option<String> {
    let Value::Obj(fields) = &mut action.args else { return None };
    match action.name.as_str() {
        "write_motion" | "patch_motion" => {
            let name = fields.iter().find(|(key, _)| key == "name").and_then(|(_, v)| v.as_str()).unwrap_or("");
            (name != motion).then(|| format!("you own only `{motion}`; `{name}` belongs to someone else (the shared modules are off-limits)"))
        }
        "inspect" | "frame" | "strip" | "onion" => {
            let target = fields.iter().find(|(key, _)| key == "target").and_then(|(_, v)| v.as_str()).map(str::to_string);
            if target.as_deref().is_none_or(|target| target.is_empty() || target == "cut") {
                fields.retain(|(key, _)| key != "target");
                fields.push(("target".into(), json::s(motion)));
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authors_write_only_their_own_document_and_look_at_it_by_default() {
        let mut write = Action { call_id: "1".into(), name: "write_motion".into(), args: json::obj(vec![("name", json::s("ch2")), ("source", json::s("use mod.motion.*; Motion{}"))]) };
        assert!(hold_to_own("ch3", &mut write).unwrap().contains("you own only `ch3`"));
        write.args = json::obj(vec![("name", json::s("ch3")), ("source", json::s("use mod.motion.*; Motion{}"))]);
        assert!(hold_to_own("ch3", &mut write).is_none());
        let mut strip = Action { call_id: "2".into(), name: "strip".into(), args: json::obj(vec![("count", Value::Int(8))]) };
        assert!(hold_to_own("ch3", &mut strip).is_none());
        assert_eq!(strip.args.get("target").and_then(Value::as_str), Some("ch3"));
    }

    /// A brain that plays scripted replies; every reply is taken once.
    struct Script(Vec<Vec<BrainEvent>>, Option<Vec<BrainEvent>>, std::rc::Rc<std::cell::RefCell<Vec<String>>>);
    impl Brain for Script {
        fn label(&self) -> String {
            "script".into()
        }
        fn vision(&self) -> bool {
            false
        }
        fn send_user(&mut self, text: String, _: Vec<BrainImage>) {
            self.2.borrow_mut().push(text);
            self.1 = Some(if self.0.is_empty() { vec![BrainEvent::TurnDone] } else { self.0.remove(0) });
        }
        fn send_results(&mut self, results: Vec<CallResult>, _: Vec<BrainImage>) {
            self.2.borrow_mut().push(results.iter().map(|r| format!("{}:{}", r.name, r.text)).collect::<Vec<_>>().join("|"));
            self.1 = Some(if self.0.is_empty() { vec![BrainEvent::TurnDone] } else { self.0.remove(0) });
        }
        fn poll(&mut self) -> Vec<BrainEvent> {
            self.1.take().unwrap_or_default()
        }
        fn cancel(&mut self) {}
    }

    fn call(id: &str, name: &str, args: &[(&str, &str)]) -> BrainEvent {
        BrainEvent::Call { call_id: id.into(), name: name.into(), args: Ok(crate::tools::Args(args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())) }
    }

    #[test]
    fn a_director_delegates_and_hears_back_when_its_authors_finish() {
        let host = vec![
            ToolDef::new("write_motion", "w", r#"{"type":"object","properties":{"name":{"type":"string"},"source":{"type":"string"}}}"#).mutating(),
            ToolDef::new("add_graphic", "a", r#"{"type":"object","properties":{}}"#).mutating(),
        ];
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let director_script = Script(
            vec![
                vec![call("1", "style_guide", &[("text", "Palette: #111 and #fc3.")]), call("2", "delegate", &[("tasks", r#"[{"id":"ch1","motion":"ch1","brief":"0-8 s: the intro."}]"#)]), BrainEvent::TurnDone],
                vec![BrainEvent::Delta("placing".into()), BrainEvent::TurnDone],
            ],
            None,
            said.clone(),
        );
        let mut tools = crate::tools::own_tools();
        tools.extend(host.clone());
        tools.extend(team_tools());
        let mut director = Agent::with_brain(Box::new(director_script), tools, Limits::default());
        director.start_turn("make a music video".into());
        let author_said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let heard = author_said.clone();
        let spawn: Spawn = Box::new(move |_, tools| {
            assert!(tools.iter().any(|t| t.name == "write_motion") && !tools.iter().any(|t| t.name == "add_graphic"));
            Ok(Box::new(Script(
                vec![vec![call("a", "write_motion", &[("name", "ch2"), ("source", "use mod.motion.*; Motion{}")]), call("b", "write_motion", &[("name", "ch1"), ("source", "use mod.motion.*; Motion{}")]), BrainEvent::TurnDone]],
                None,
                heard.clone(),
            )) as Box<dyn Brain>)
        });
        let mut team = Team::with_director(director, false, spawn, host, Vec::new(), Limits::default());
        let actions = team.poll();
        assert_eq!(team.style_guide(), "Palette: #111 and #fc3.");
        assert_eq!(team.members().len(), 1);
        assert!(author_said.borrow()[0].starts_with("# Style guide\nPalette: #111 and #fc3."));
        // The author starts at once; its write to another document never
        // reaches the host. to another document never reaches the host.
        assert_eq!(actions.len(), 1);
        assert_eq!((actions[0].agent.as_str(), actions[0].action.args.get("name").and_then(Value::as_str)), ("ch1", Some("ch1")));
        team.result("ch1", "b", Outcome::ok("published"));
        for _ in 0..6 {
            team.poll();
        }
        assert!(said.borrow().iter().any(|s| s.contains("delegate:{\"authors\":[{\"id\":\"ch1\"")), "{:?}", said.borrow());
        assert!(author_said.borrow().iter().any(|s| s.contains("you own only `ch1`")));
    }

    #[test]
    fn team_tool_schemas_are_json_objects() {
        for tool in team_tools() {
            let schema = json::parse_depth(tool.parameters.as_bytes(), 16).unwrap();
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
        }
    }
}

/// One author, or a director with scene authors: what the Generate panel
/// runs, driven the same way.
pub enum Crew {
    Solo(Agent),
    Team(Team),
    /// Scene authors a production started without a director.
    Authors(Authors),
}

impl Crew {
    /// The agent the person talks to: the only one, or the director.
    /// Authors without a director: the first author.
    pub fn lead(&self) -> &Agent {
        match self {
            Crew::Solo(agent) => agent,
            Crew::Team(team) => team.director(),
            Crew::Authors(authors) => &authors.members[0].agent,
        }
    }

    pub fn agents(&self) -> Vec<(&str, &Agent)> {
        match self {
            Crew::Solo(agent) => vec![(DIRECTOR, agent)],
            Crew::Team(team) => team.agents(),
            Crew::Authors(authors) => authors.members.iter().map(|member| (member.id.as_str(), &member.agent)).collect(),
        }
    }

    pub fn busy(&self) -> bool {
        match self {
            Crew::Solo(agent) => agent.state().busy(),
            Crew::Team(team) => team.busy(),
            Crew::Authors(authors) => authors.busy(),
        }
    }

    pub fn generation(&self) -> u64 {
        match self {
            Crew::Solo(agent) => agent.generation(),
            Crew::Team(team) => team.generation(),
            Crew::Authors(authors) => authors.generation(),
        }
    }

    pub fn observe(&mut self, docs: Vec<Doc>) {
        match self {
            Crew::Solo(agent) => agent.observe(docs),
            Crew::Team(team) => team.observe(docs),
            Crew::Authors(authors) => {
                for member in &mut authors.members {
                    member.agent.observe(docs.clone());
                }
            }
        }
    }

    /// A line with pictures (a solo agent takes them; a team's director gets the text).
    pub fn say_seeing(&mut self, text: String, images: Vec<crate::brain::BrainImage>) {
        match self {
            Crew::Solo(agent) => agent.say_seeing(text, images),
            other => other.say(text),
        }
    }

    pub fn say(&mut self, text: String) {
        match self {
            Crew::Solo(agent) => agent.say(text),
            Crew::Team(team) => team.say(text),
            Crew::Authors(authors) => authors.say(text),
        }
    }

    pub fn cancel(&mut self) {
        match self {
            Crew::Solo(agent) => agent.cancel(),
            Crew::Team(team) => team.cancel(),
            Crew::Authors(authors) => {
                for member in &mut authors.members {
                    member.agent.cancel();
                }
            }
        }
    }

    pub fn poll(&mut self) -> Vec<TeamAction> {
        match self {
            Crew::Solo(agent) => agent.poll().into_iter().map(|action| TeamAction { agent: DIRECTOR.into(), action }).collect(),
            Crew::Team(team) => team.poll(),
            Crew::Authors(authors) => authors.poll(),
        }
    }

    pub fn result(&mut self, agent: &str, call_id: &str, outcome: Outcome) {
        match self {
            Crew::Solo(solo) => solo.result(call_id, outcome),
            Crew::Team(team) => team.result(agent, call_id, outcome),
            Crew::Authors(authors) => authors.result(agent, call_id, outcome),
        }
    }

    /// Every document change by any agent, in order of the agents.
    pub fn revisions(&self) -> Vec<&crate::agent::Revision> {
        self.agents().into_iter().flat_map(|(_, agent)| agent.revisions()).collect()
    }
}
