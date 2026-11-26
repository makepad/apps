//! The headless generator contract. The harness does not link the
//! generator: it runs a command (`--generator`) once per case, hands it a
//! request file and reads back a result file. That keeps the harness
//! independent of the Stage app's build and lets any generator (the in-app
//! agent loop run headless, a Claude Code skill driver, a person's script)
//! be measured the same way.
//!
//! ```text
//! <generator ...> {request}      # placeholders in --generator:
//!                                # {request} request.json path
//!                                # {out}     the case's output directory
//!                                # {result}  where result.json must go
//!                                # {model}   the model spec, e.g. claude-cli:opus
//! ```
//! The generator writes `result.json` (below) into `{out}` and exits; a
//! missing or unparsable result is a failed run, whatever the exit code.

use crate::bench::Case;
use makepad_micro_serde::*;

/// What the generator is asked to make. `out_dir` is where it writes the
/// edit, its motion documents, the transcript and `result.json`.
#[derive(Clone, Debug, SerJson, DeJson)]
pub struct GenRequest {
    pub case: String,
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub length_s: f64,
    pub voice: bool,
    pub subtitles: bool,
    /// `backend:model`, e.g. `claude-cli:opus` or `lan:qwen3.6-27b`.
    pub model: String,
    /// The agent loop's bound on write → inspect → fix rounds.
    pub max_iterations: u32,
    pub out_dir: String,
    /// Where `result.json` goes (inside `out_dir`).
    pub result_path: String,
    /// Label of the guide/prompt version under test, passed through.
    pub label: String,
    /// The song the brief is made from (an audio file), and its lyrics
    /// (a text file, one sung line per line, sections in `[...]`). The
    /// generator imports them before it starts.
    pub song: Option<String>,
    pub lyrics: Option<String>,
}

impl GenRequest {
    pub fn new(case: &Case, model: &str, max_iterations: u32, out_dir: &str, label: &str) -> GenRequest {
        GenRequest {
            case: case.id.clone(),
            prompt: case.prompt.clone(),
            width: case.width,
            height: case.height,
            fps: case.fps,
            length_s: case.length_s,
            voice: case.voice,
            subtitles: case.subtitles,
            model: model.to_string(),
            max_iterations,
            out_dir: out_dir.to_string(),
            result_path: format!("{out_dir}/result.json"),
            label: label.to_string(),
            song: None,
            lyrics: None,
        }
    }
}

/// One motion document the generator wrote (and published, when it did).
#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct GenMotion {
    /// Path of the `motion.splash` source on disk.
    pub path: String,
    /// The library asset id and revision, when published.
    pub asset: Option<String>,
    pub rev: Option<String>,
}

/// What the generator reports. Paths are absolute or relative to `out_dir`.
#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct GenResult {
    pub success: bool,
    pub error: Option<String>,
    /// Agent loop rounds (write → inspect → look → fix).
    pub iterations: Option<u32>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// The model that actually answered (a fleet node's model, the CLI's).
    pub model_used: Option<String>,
    /// The cut's Splash source.
    pub cut: Option<String>,
    pub motions: Option<Vec<GenMotion>>,
    /// The agent transcript (text or JSON lines).
    pub transcript: Option<String>,
}

impl GenResult {
    pub fn parse(text: &str) -> Result<GenResult, String> {
        GenResult::deserialize_json_lenient(text).map_err(|e| format!("result.json: {e:?}"))
    }
}

/// Expand the `{placeholders}` of a generator command line and split it
/// into argv. Words split on whitespace; single or double quotes group a
/// word (no escapes: keep generator paths simple).
pub fn expand_command(template: &str, vars: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in template.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            None => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err(format!("unbalanced quote in generator command `{template}`"));
    }
    if in_word {
        words.push(word);
    }
    if words.is_empty() {
        return Err("empty generator command".into());
    }
    Ok(words
        .into_iter()
        .map(|w| {
            let mut w = w;
            for (k, v) in vars {
                w = w.replace(&format!("{{{k}}}"), v);
            }
            w
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_expands_and_quotes() {
        let argv = expand_command(
            "gen --request {request} --model '{model}' --note \"a b\"",
            &[("request", "/o/request.json"), ("model", "claude-cli:opus")],
        )
        .unwrap();
        assert_eq!(argv, ["gen", "--request", "/o/request.json", "--model", "claude-cli:opus", "--note", "a b"]);
        assert!(expand_command("gen 'x", &[]).is_err());
        assert!(expand_command("   ", &[]).is_err());
    }

    #[test]
    fn result_is_lenient_about_extra_and_missing_fields() {
        let r = GenResult::parse(r#"{"success":true,"iterations":3,"cut":"cut.splash","extra":{"a":1},"motions":[{"path":"m1.splash"}]}"#).unwrap();
        assert!(r.success);
        assert_eq!(r.iterations, Some(3));
        assert_eq!(r.cut.as_deref(), Some("cut.splash"));
        assert_eq!(r.motions.unwrap()[0].path, "m1.splash");
        assert!(r.tokens_in.is_none());
    }

    #[test]
    fn request_round_trips() {
        let case = crate::bench::cases().remove(0);
        let req = GenRequest::new(&case, "claude-cli:opus", 6, "/tmp/x", "guide-v1");
        let back = GenRequest::deserialize_json(&req.serialize_json()).unwrap();
        assert_eq!(back.result_path, "/tmp/x/result.json");
        assert_eq!(back.width, 1080);
    }
}
