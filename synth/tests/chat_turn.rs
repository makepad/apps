//! The chat's tool rounds, driven over `--remote` by makepad_test, with the
//! calls a request for a fugue makes. A call always ends in a result the
//! model can act on, and the chat never waits with no named tool running.

use makepad_test::{run_with_config, TestApp, TestConfig};
use makepad_widgets::makepad_micro_serde::*;
use std::time::Duration;

// The budgets are the app main thread's CPU, a bound for an app stuck at work, not a timing
// target (waits on the JIT are progress and spend none of it). Under the JIT on a loaded box one
// frame of a new chat round's UI has taken tens of seconds, so each bound sits well past that.
// A transcript read or a message submission: one UI dispatch and the frame it brings.
const COMMAND_BUDGET: Duration = Duration::from_secs(60);
// One tool's result: it may load or search the score library, then the frame that shows it.
const CONSOLE_BUDGET: Duration = Duration::from_secs(60);
// Seven chat rounds: six tool calls (two score writes, playback's start), each round's frames.
#[cfg(unix)]
const FUGUE_BUDGET: Duration = Duration::from_secs(300);

#[allow(dead_code)]
#[derive(DeJson, Debug)]
struct Row {
    kind: String,
    text: String,
    title: String,
    status: String,
    note: String,
}

/// `ai transcript` (apps/aichat/src/overlay.rs, `transcript_json`).
#[allow(dead_code)]
#[derive(DeJson, Debug)]
struct Transcript {
    status: String,
    provider: String,
    apps: Vec<String>,
    entries: Vec<Row>,
    generation: u64,
}

/// One command on the app's makepad-remote endpoint; its answer lines.
fn remote(app: &TestApp, words: &[&str], params: &[(&str, &str)]) -> String {
    app.command(COMMAND_BUDGET, words, params).join("\n")
}

fn transcript(app: &TestApp) -> Transcript {
    let body = remote(app, &["ai", "transcript"], &[]);
    Transcript::deserialize_json_lenient(&body).unwrap_or_else(|e| panic!("the transcript does not parse: {e}\n{body}"))
}

fn finished(row: &Row) -> bool {
    !row.status.is_empty() && row.status != "running" && row.status != "not responding" && row.status != "confirm"
}

/// The app with data of its own: what one run saves (the score, the chat) is
/// not what the next one finds, and the box's real files stay as they are. Its
/// home stays the box's: that is where a hub's address and the weights are.
fn config(name: &str) -> TestConfig {
    let mut config = TestConfig::current_package(env!("CARGO_MANIFEST_DIR"), env!("CARGO_PKG_NAME"), format!("chat_turn::{name}")).unwrap();
    let data = std::env::temp_dir().join(format!("synth-chat-data-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    config.env.insert("SYNTH_DATA_DIR".to_string(), data.display().to_string());
    config.env.insert("MAKEPAD_AICHAT_DIR".to_string(), data.display().to_string());
    config
}

/// Types one console line and returns its finished tool row.
fn console(app: &TestApp, line: &str) -> Row {
    let before = transcript(app).entries.iter().filter(|row| row.kind == "tool").count();
    println!("say {line} ({before} tool rows)");
    remote(app, &["ai"], &[("say", line)]);
    let mut result = None;
    app.wait_until_within(CONSOLE_BUDGET, || {
        let t = transcript(app);
        let last = t.entries.into_iter().filter(|row| row.kind == "tool").skip(before).last();
        match last {
            Some(row) if finished(&row) => {
                result = Some(row);
                None
            }
            pending => Some(format!("no answer to `{line}`: chat {}, last new tool row {pending:?}", t.status)),
        }
    });
    result.unwrap()
}

/// Every call of the fugue request ends, whether or not the song library is
/// installed on this machine.
#[test]
fn the_fugue_calls_each_end_in_a_result() {
    let mut config = config("the_fugue_calls_each_end_in_a_result");
    let data = config.env["SYNTH_DATA_DIR"].clone();
    config.env.insert("MAKEPAD_AI_PROVIDER".to_string(), "none".to_string());
    run_with_config(config, |app: TestApp| {
        println!("app {}", app.pid());
        app.wait_until_within(CONSOLE_BUDGET, || {
            let row = console(&app, "/synth.score_library");
            println!("{row:?}");
            (row.status != "ok").then(|| format!("waiting for the score library: {row:?}"))
        });
        for line in [
            "/synth.score_library",
            r#"/synth.score_new {"template":"blank","bpm":84,"label":"Fugue"}"#,
            r#"/synth.preset_search {"engine":"Piano","limit":8}"#,
            r#"/synth.midi_lookup {"query":"Bach fugue piano well-tempered","n":5}"#,
        ] {
            let row = console(&app, line);
            println!("{line}: {} {}", row.status, row.note);
            assert!(row.status == "ok" || line.contains("midi_lookup"), "{line}: {row:?}");
        }
    })
    .unwrap();
    std::fs::remove_dir_all(data).unwrap();
}

const SUBJECT: &str = "use mod.score.*\n\nlet clock = Clock{bpm: 72 sig: [4 4] seed: 1}\n\nlet piano = Instrument{kind: @piano preset: \"Concert Grand\"}\n\nlet alto = Loop{play: piano beats: 8 amp: 0.8\n    notes: [[0 @c4 0.5 0.72] [0.5 @eb4 0.5 0.7] [1 @g4 1 0.78] [2 @ab4 0.5 0.8] [3 @g4 0.5 0.72] [4 @f4 0.5 0.72] [5 @eb4 0.5 0.66] [6 @b3 0.5 0.72] [7 @c4 1 0.74]]\n}\n\nScore{label: \"Fugue\" clock piano alto}\n";

/// The answer joins the subject, a fifth up and a bar later: the second step.
fn with_answer() -> String {
    SUBJECT.replace(
        "Score{label: \"Fugue\" clock piano alto}",
        "let soprano = Loop{play: piano beats: 8 amp: 0.8\n    notes: [[4 @g4 0.5 0.72] [4.5 @bb4 0.5 0.7] [5 @d5 1 0.78] [6 @eb5 0.5 0.8] [7 @d5 0.5 0.72]]\n}\n\nScore{label: \"Fugue\" clock piano alto soprano}",
    )
}

/// One reply of the stand-in, in the CLI's stream.
fn reply(text: &str) -> String {
    let event = |text: &str| {
        let delta = makepad_widgets::makepad_micro_serde::SerJson::serialize_json(&text.to_string());
        format!(r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":{delta}}}}}}}"#)
    };
    let done = makepad_widgets::makepad_micro_serde::SerJson::serialize_json(&text.to_string());
    format!("{}\n{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":{done},\"session_id\":\"stand-in\"}}\n", event(text))
}

fn call(name: &str, args: &str) -> String {
    format!("<tool_call>{{\"name\":\"synth.{name}\",\"arguments\":{args}}}</tool_call>\n")
}

/// The person's request through the chat, with a stand-in for the Claude Code
/// CLI (`CLAUDE_CODE_PATH`): each turn reads the prompt and answers after a
/// second, the way the fugue goes in small steps, then it plays. At no moment
/// does the chat wait for a tool with none running or asking, and a line typed
/// during a turn does not end it.
#[cfg(unix)]
#[test]
fn the_fugue_request_goes_through_every_round_and_plays() {
    use std::os::unix::fs::PermissionsExt;
    let mut config = config("the_fugue_request_goes_through_every_round_and_plays");
    let data = config.env["SYNTH_DATA_DIR"].clone();
    // Apart from the artifacts, which the app's start clears.
    let dir = std::env::temp_dir().join(format!("synth-chat-stand-in-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let quote = |text: &str| makepad_widgets::makepad_micro_serde::SerJson::serialize_json(&text.to_string());
    let turns = [
        call("score_guide", "{}") + &call("preset_search", r#"{"query":"grand piano"}"#),
        call("score_write", &format!("{{\"source\":{}}}", quote(SUBJECT))),
        call("score_write", &format!("{{\"source\":{}}}", quote(&with_answer()))),
        call("score_play", r#"{"play":true}"#),
        call("score_status", "{}"),
        "The fugue is playing.".to_string(),
        "Still on piano.".to_string(),
    ];
    for (index, turn) in turns.iter().enumerate() {
        std::fs::write(dir.join(format!("turn{}.jsonl", index + 1)), reply(turn)).unwrap();
    }
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ndir='{}'\nn=$(cat \"$dir/n\" 2>/dev/null || echo 0)\nn=$((n+1))\necho $n > \"$dir/n\"\ncat > \"$dir/stdin$n\"\nsleep 1\ncat \"$dir/turn$n.jsonl\"\n",
            dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    config.env.insert("MAKEPAD_AI_PROVIDER".to_string(), "claude-cli".to_string());
    config.env.insert("CLAUDE_CODE_PATH".to_string(), script.display().to_string());
    run_with_config(config, |app: TestApp| {
        remote(&app, &["ai"], &[("say", "make me a Bach fugue on piano")]);
        let (mut last_status, mut typed) = (String::new(), false);
        let mut completed = None;
        app.wait_until_within(FUGUE_BUDGET, || {
            let t = transcript(&app);
            if t.status != last_status {
                println!("{:?} ({} rows)", t.status, t.entries.len());
                last_status = t.status.clone();
            }
            let open = t.entries.iter().any(|row| row.kind == "tool" && !finished(row));
            assert!(t.status != "waiting_for_tool" || open, "the chat waits for no tool: {t:#?}");
            assert!(!t.status.starts_with("error"), "{t:#?}");
            if !typed && dir.join("stdin2").exists() {
                remote(&app, &["ai"], &[("say", "and keep it on piano")]);
                typed = true;
            }
            let done = t.entries.iter().any(|row| row.kind == "assistant" && row.text.contains("Still on piano"));
            if t.status == "idle" && done {
                completed = Some(t);
                return None;
            }
            Some(format!("the turn did not go on: {t:#?}"))
        });
        let t = completed.unwrap();
        println!("{t:#?}");
        for row in t.entries.iter().filter(|row| row.kind == "tool") {
            assert_eq!(row.status, "ok", "{row:?}");
        }
        assert_eq!(std::fs::read_to_string(dir.join("n")).unwrap().trim(), "7", "each step one turn, then the typed line; none repeated");
        assert!(std::fs::read_to_string(dir.join("stdin7")).unwrap().contains("and keep it on piano"), "the typed line went out after the turn");
        let results = std::fs::read_to_string(dir.join("stdin2")).unwrap();
        assert!(results.contains("synth.score_guide") && results.contains("Work in small steps"), "the results reached the model");
        let status = console(&app, "/synth.score_status");
        println!("{status:?}");
        assert_eq!(status.status, "ok", "{status:?}");
        assert!(status.text.contains("\"playing\":true"), "{status:?}");
    })
    .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(data).unwrap();
}
