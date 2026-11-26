//! A production as data: the ask, the steps in order, each step's state and
//! time, and what each made. Saved as `production.json` after every change,
//! so a production resumes at its first unfinished step, and any step can be
//! made again (it and the steps after it go back to waiting; what came
//! before stays).

use makepad_strict_json::{self as json, Value};

/// The steps of a production, in order: each takes the outputs before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// The lyric sheet, the music brief and the title.
    Lyrics,
    /// The song, sung from the sheet on the AI Hub's music model.
    Music,
    /// Beats, sections and every sung word's time: the song timing module.
    Timing,
    /// The storyboard over the song's sections and the style guide.
    Storyboard,
    /// One Motion scene per section, each with its editor panel.
    Scenes,
    /// The cut: the scenes over the song, with transitions.
    Cut,
}

impl Step {
    pub const ALL: [Step; 6] = [Step::Lyrics, Step::Music, Step::Timing, Step::Storyboard, Step::Scenes, Step::Cut];

    pub fn id(self) -> &'static str {
        match self {
            Step::Lyrics => "lyrics",
            Step::Music => "music",
            Step::Timing => "timing",
            Step::Storyboard => "storyboard",
            Step::Scenes => "scenes",
            Step::Cut => "cut",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Step::Lyrics => "Lyrics",
            Step::Music => "Music",
            Step::Timing => "Timing",
            Step::Storyboard => "Storyboard",
            Step::Scenes => "Scenes",
            Step::Cut => "Edit",
        }
    }

    pub fn from_id(id: &str) -> Option<Step> {
        Step::ALL.into_iter().find(|step| step.id() == id)
    }

    pub fn index(self) -> usize {
        Step::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Waiting,
    Running,
    Done,
    Failed(String),
}

impl Status {
    pub fn id(&self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed(_) => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepRecord {
    pub step: Step,
    pub status: Status,
    /// Seconds the step took (or has taken so far, while running).
    pub secs: f64,
    /// What it is doing or what it made, in a line.
    pub note: String,
}

/// A library revision.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetRef {
    pub asset_id: String,
    pub revision: String,
}

impl AssetRef {
    pub fn new(asset_id: impl Into<String>, revision: impl Into<String>) -> AssetRef {
        AssetRef { asset_id: asset_id.into(), revision: revision.into() }
    }

    pub fn is_set(&self) -> bool {
        !self.asset_id.is_empty() && !self.revision.is_empty()
    }

    pub fn to_value(&self) -> Value {
        json::obj(vec![("asset_id", json::s(self.asset_id.clone())), ("revision", json::s(self.revision.clone()))])
    }

    pub fn from_value(value: Option<&Value>) -> AssetRef {
        let Some(value) = value else { return AssetRef::default() };
        AssetRef { asset_id: text(value, "asset_id"), revision: text(value, "revision") }
    }
}

/// A sung line: its text and when it is sung.
#[derive(Clone, Debug, PartialEq)]
pub struct LineSpan {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// A section of the song as the timing found it.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub name: String,
    pub start: f64,
    pub end: f64,
    /// Its sung lines, in order.
    pub lines: Vec<LineSpan>,
}

impl Section {
    pub fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.text.clone()).collect()
    }
}

/// The storyboard's plan for one section's scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScenePlan {
    pub section: String,
    pub title: String,
    /// The one idea it carries.
    pub idea: String,
    /// What is on screen and how it moves.
    pub look: String,
}

/// One section's Motion scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub section: String,
    /// The Motion document's name (what its author writes).
    pub motion: String,
    /// Its latest revision in the library, once written.
    pub asset: AssetRef,
    /// It carries an editor panel.
    pub panel: bool,
    /// Done, or why not.
    pub state: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Production {
    pub id: String,
    /// The one-line ask.
    pub ask: String,
    /// The song's length.
    pub seconds: f64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The person gave the lyrics; the Lyrics step only writes the brief.
    pub given_lyrics: bool,
    pub title: String,
    /// The music model's brief: genre, tempo, key, voice, arrangement.
    pub caption: String,
    /// The lyric sheet, section-tagged.
    pub sheet: String,
    /// The music model's seed; a new song takes a new one.
    pub seed: u64,
    pub song: AssetRef,
    /// The song's measured length.
    pub song_seconds: f64,
    pub timing: AssetRef,
    pub sections: Vec<Section>,
    /// The timing's numbers in a line (words forced, agreement, unsung).
    pub timing_report: String,
    /// The storyboard's id in the storyboard store.
    pub board: String,
    pub style: String,
    pub plans: Vec<ScenePlan>,
    pub scenes: Vec<Scene>,
    pub edit: AssetRef,
    pub steps: Vec<StepRecord>,
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn number(value: &Value, key: &str) -> f64 {
    match value.get(key) {
        Some(Value::Int(i)) => *i as f64,
        Some(Value::F64(f)) => *f,
        _ => 0.0,
    }
}

fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_arr).unwrap_or(&[])
}

impl Production {
    /// A new production of `ask`: a song of about `seconds`, the person's
    /// own `lyrics` when given.
    pub fn new(id: impl Into<String>, ask: impl Into<String>, seconds: f64, lyrics: Option<String>, seed: u64) -> Production {
        let lyrics = lyrics.filter(|text| !text.trim().is_empty());
        Production {
            id: id.into(),
            ask: ask.into(),
            seconds: seconds.clamp(15.0, 300.0),
            width: 1920,
            height: 1080,
            fps: 60,
            given_lyrics: lyrics.is_some(),
            title: String::new(),
            caption: String::new(),
            sheet: lyrics.unwrap_or_default(),
            seed,
            song: AssetRef::default(),
            song_seconds: 0.0,
            timing: AssetRef::default(),
            sections: Vec::new(),
            timing_report: String::new(),
            board: String::new(),
            style: String::new(),
            plans: Vec::new(),
            scenes: Vec::new(),
            edit: AssetRef::default(),
            steps: Step::ALL.iter().map(|step| StepRecord { step: *step, status: Status::Waiting, secs: 0.0, note: String::new() }).collect(),
        }
    }

    pub fn record(&self, step: Step) -> &StepRecord {
        &self.steps[step.index()]
    }

    pub fn record_mut(&mut self, step: Step) -> &mut StepRecord {
        &mut self.steps[step.index()]
    }

    /// The first step not done: what runs next (or is running, or failed).
    pub fn next(&self) -> Option<Step> {
        self.steps.iter().find(|record| record.status != Status::Done).map(|record| record.step)
    }

    pub fn done(&self) -> bool {
        self.next().is_none()
    }

    /// Make `step` again: it and every step after it go back to waiting and
    /// lose what they made; what came before stays. A new song gets a new
    /// seed.
    pub fn redo(&mut self, step: Step) {
        for record in self.steps.iter_mut().filter(|record| record.step.index() >= step.index()) {
            record.status = Status::Waiting;
            record.secs = 0.0;
            record.note.clear();
        }
        for later in Step::ALL.into_iter().filter(|later| later.index() >= step.index()) {
            match later {
                Step::Lyrics => {
                    self.title.clear();
                    self.caption.clear();
                    if !self.given_lyrics {
                        self.sheet.clear();
                    }
                }
                Step::Music => {
                    self.song = AssetRef::default();
                    self.song_seconds = 0.0;
                    self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 1;
                }
                Step::Timing => {
                    self.timing = AssetRef::default();
                    self.sections.clear();
                    self.timing_report.clear();
                }
                Step::Storyboard => {
                    self.style.clear();
                    self.plans.clear();
                }
                Step::Scenes => self.scenes.clear(),
                Step::Cut => self.edit = AssetRef::default(),
            }
        }
    }

    /// Remake one section's scene: the Scenes step and the Edit after it
    /// run again for that scene only (the others keep theirs).
    pub fn redo_scene(&mut self, section: &str) -> bool {
        let Some(scene) = self.scenes.iter_mut().find(|scene| scene.section == section) else { return false };
        scene.asset = AssetRef::default();
        scene.panel = false;
        scene.state.clear();
        for step in [Step::Scenes, Step::Cut] {
            let record = self.record_mut(step);
            record.status = Status::Waiting;
            record.secs = 0.0;
            record.note.clear();
        }
        self.edit = AssetRef::default();
        true
    }

    pub fn to_value(&self) -> Value {
        let sections = self.sections.iter().map(|section| json::obj(vec![
            ("name", json::s(section.name.clone())),
            ("start", Value::F64(section.start)),
            ("end", Value::F64(section.end)),
            ("lines", Value::Arr(section.lines.iter().map(|line| json::obj(vec![("text", json::s(line.text.clone())), ("start", Value::F64(line.start)), ("end", Value::F64(line.end))])).collect())),
        ])).collect();
        let plans = self.plans.iter().map(|plan| json::obj(vec![
            ("section", json::s(plan.section.clone())),
            ("title", json::s(plan.title.clone())),
            ("idea", json::s(plan.idea.clone())),
            ("look", json::s(plan.look.clone())),
        ])).collect();
        let scenes = self.scenes.iter().map(|scene| json::obj(vec![
            ("section", json::s(scene.section.clone())),
            ("motion", json::s(scene.motion.clone())),
            ("asset", scene.asset.to_value()),
            ("panel", Value::Bool(scene.panel)),
            ("state", json::s(scene.state.clone())),
        ])).collect();
        let steps = self.steps.iter().map(|record| {
            let mut pairs = vec![
                ("step", json::s(record.step.id())),
                ("status", json::s(record.status.id())),
                ("secs", Value::F64((record.secs * 10.0).round() / 10.0)),
                ("note", json::s(record.note.clone())),
            ];
            if let Status::Failed(why) = &record.status {
                pairs.push(("error", json::s(why.clone())));
            }
            json::obj(pairs)
        }).collect();
        json::obj(vec![
            ("format", json::s("stage-production")),
            ("version", Value::Int(1)),
            ("id", json::s(self.id.clone())),
            ("ask", json::s(self.ask.clone())),
            ("seconds", Value::F64(self.seconds)),
            ("width", Value::Int(self.width as i64)),
            ("height", Value::Int(self.height as i64)),
            ("fps", Value::Int(self.fps as i64)),
            ("given_lyrics", Value::Bool(self.given_lyrics)),
            ("title", json::s(self.title.clone())),
            ("caption", json::s(self.caption.clone())),
            ("sheet", json::s(self.sheet.clone())),
            ("seed", Value::Int((self.seed & 0x7fff_ffff_ffff_ffff) as i64)),
            ("song", self.song.to_value()),
            ("song_seconds", Value::F64(self.song_seconds)),
            ("timing", self.timing.to_value()),
            ("sections", Value::Arr(sections)),
            ("timing_report", json::s(self.timing_report.clone())),
            ("board", json::s(self.board.clone())),
            ("style", json::s(self.style.clone())),
            ("plans", Value::Arr(plans)),
            ("scenes", Value::Arr(scenes)),
            ("cut", self.edit.to_value()),
            ("steps", Value::Arr(steps)),
        ])
    }

    pub fn to_json(&self) -> String {
        self.to_value().to_json()
    }

    pub fn from_json(bytes: &[u8]) -> Result<Production, String> {
        let value = json::parse_depth(bytes, 8).map_err(|error| format!("production.json: {error}"))?;
        if text(&value, "format") != "stage-production" {
            return Err("not a production record".into());
        }
        let mut production = Production::new(text(&value, "id"), text(&value, "ask"), number(&value, "seconds"), None, value.get("seed").and_then(Value::as_u64).unwrap_or(1));
        production.width = number(&value, "width").max(16.0) as u32;
        production.height = number(&value, "height").max(16.0) as u32;
        production.fps = number(&value, "fps").max(1.0) as u32;
        production.given_lyrics = value.get("given_lyrics").and_then(Value::as_bool).unwrap_or(false);
        production.title = text(&value, "title");
        production.caption = text(&value, "caption");
        production.sheet = text(&value, "sheet");
        production.song = AssetRef::from_value(value.get("song"));
        production.song_seconds = number(&value, "song_seconds");
        production.timing = AssetRef::from_value(value.get("timing"));
        production.sections = items(&value, "sections").iter().map(|s| Section { name: text(s, "name"), start: number(s, "start"), end: number(s, "end"), lines: items(s, "lines").iter().map(|l| LineSpan { text: text(l, "text"), start: number(l, "start"), end: number(l, "end") }).collect() }).collect();
        production.timing_report = text(&value, "timing_report");
        production.board = text(&value, "board");
        production.style = text(&value, "style");
        production.plans = items(&value, "plans").iter().map(|p| ScenePlan { section: text(p, "section"), title: text(p, "title"), idea: text(p, "idea"), look: text(p, "look") }).collect();
        production.scenes = items(&value, "scenes").iter().map(|s| Scene {
            section: text(s, "section"),
            motion: text(s, "motion"),
            asset: AssetRef::from_value(s.get("asset")),
            panel: s.get("panel").and_then(Value::as_bool).unwrap_or(false),
            state: text(s, "state"),
        }).collect();
        production.edit = AssetRef::from_value(value.get("cut"));
        for record in items(&value, "steps") {
            let Some(step) = Step::from_id(&text(record, "step")) else { continue };
            let status = match text(record, "status").as_str() {
                "done" => Status::Done,
                "failed" => Status::Failed(text(record, "error")),
                // A step that was running when Stage stopped runs again.
                _ => Status::Waiting,
            };
            let slot = production.record_mut(step);
            slot.status = status;
            slot.secs = number(record, "secs");
            slot.note = text(record, "note");
        }
        Ok(production)
    }

    /// The production's state for a tool's answer: every step with its
    /// status, time and note, and what it made.
    pub fn status_value(&self) -> Value {
        let mut value = self.to_value();
        if let Value::Obj(pairs) = &mut value {
            pairs.retain(|(key, _)| key != "format" && key != "version");
            pairs.push(("next".into(), self.next().map_or(Value::Null, |step| json::s(step.id()))));
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_production_resumes_where_it_stopped_and_redoes_from_a_step() {
        let mut p = Production::new("p1", "a lyric video about tides, synthwave", 75.0, None, 7);
        assert_eq!(p.next(), Some(Step::Lyrics));
        p.title = "Tides".into();
        p.sheet = "[Verse]\nwe move with the moon".into();
        p.record_mut(Step::Lyrics).status = Status::Done;
        p.song = AssetRef::new("a1", "r1");
        p.record_mut(Step::Music).status = Status::Done;
        p.record_mut(Step::Timing).status = Status::Running;
        let back = Production::from_json(p.to_json().as_bytes()).expect("reads back");
        assert_eq!(back.next(), Some(Step::Timing), "a running step runs again");
        assert_eq!(back.song, AssetRef::new("a1", "r1"));
        assert_eq!(back.sheet, p.sheet);
        let mut again = back.clone();
        let seed = again.seed;
        again.redo(Step::Music);
        assert_eq!(again.next(), Some(Step::Music));
        assert!(!again.song.is_set());
        assert_ne!(again.seed, seed, "a new song takes a new seed");
        assert_eq!(again.title, "Tides", "what came before stays");
        again.redo(Step::Lyrics);
        assert!(again.sheet.is_empty(), "written lyrics are written again");
        let mut given = Production::new("p2", "x", 60.0, Some("[Verse]\nmine".into()), 1);
        given.redo(Step::Lyrics);
        assert_eq!(given.sheet, "[Verse]\nmine", "the person's lyrics stay theirs");
    }
}
