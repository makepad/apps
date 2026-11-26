//! The text tool protocol for backends without native tools (Claude Code
//! run chat-only, and any model reached as plain text): the model ends a
//! reply with one or more calls written the way Claude writes function
//! calls,
//!
//! ```text
//! <invoke name="write_motion">
//! <parameter name="name">hook_title</parameter>
//! <parameter name="source">Motion{ … }</parameter>
//! </invoke>
//! ```
//!
//! Parameter values are raw text up to their closing tag, so a Splash
//! document goes in as it is, never escaped. A `<tool_call>{"name": …,
//! "arguments": {…}}</tool_call>` block (the JSON form other chat engines
//! use) is read too.

use crate::tools::{Args, ToolDef};
use makepad_strict_json as json;

/// The protocol paragraph and the tool table, appended to the system text
/// of a text backend.
pub fn protocol(tools: &[ToolDef]) -> String {
    let mut out = String::from(
        "\n# Calling tools\n\
         You act only through the tools below; you have no file, shell or web access. To call tools, end your reply \
         with one or more blocks, each exactly:\n\n\
         <invoke name=\"TOOL\">\n<parameter name=\"PARAM\">VALUE</parameter>\n</invoke>\n\n\
         VALUE is raw text up to `</parameter>`: write Splash sources as they are, with no escaping and no code fences. \
         Arrays and numbers are written as JSON (`[{\"id\": \"hook\", …}]`, `2.5`). Write nothing after the last block and never \
         write a result yourself: the results (and any pictures) come in the next message, in call order. Several calls in \
         one reply run in order; batch independent calls into one reply.\n\n\
         Tools (name, what it does, JSON-schema parameters):\n",
    );
    for tool in tools {
        out.push_str(&format!("\n## {}\n{}\nParameters: {}\n", tool.name, tool.description, tool.parameters));
    }
    out
}

/// One call as written: its name and arguments, or why it could not be read.
pub type Written = Result<(String, Args), String>;

/// A reply split into its visible text and the calls at its end.
#[derive(Debug, Default, PartialEq)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<Written>,
}

const MARKERS: [&str; 3] = ["<invoke name=", "<tool_call>", "<function_calls>"];

fn next_marker(text: &str) -> Option<usize> {
    MARKERS.iter().filter_map(|marker| text.find(marker)).min()
}

/// Read a whole reply. Text after an invented result is dropped: nothing
/// the model writes as a result is real.
pub fn parse_reply(raw: &str) -> Reply {
    let mut reply = Reply::default();
    let mut rest = raw;
    if let Some(at) = ["<function_results>", "[tool result]", "<tool_result>"].iter().filter_map(|marker| rest.find(marker)).min() {
        rest = &rest[..at];
    }
    while let Some(at) = next_marker(rest) {
        reply.text.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(after) = rest.strip_prefix("<function_calls>") {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("<tool_call>") {
            let (body, next) = match after.find("</tool_call>") {
                Some(end) => (&after[..end], &after[end + "</tool_call>".len()..]),
                None => (after, ""),
            };
            reply.calls.push(parse_json_call(body));
            rest = next;
            continue;
        }
        let (body, next) = match rest.find("</invoke>") {
            Some(end) => (&rest[..end], &rest[end + "</invoke>".len()..]),
            None => (rest, ""),
        };
        reply.calls.push(parse_invoke(body));
        rest = next.trim_start();
        if let Some(after) = rest.strip_prefix("</function_calls>") {
            rest = after;
        }
    }
    reply.text.push_str(rest);
    reply.text = reply.text.trim().to_string();
    reply
}

/// `<invoke name="x"><parameter name="k">v</parameter>…` (without
/// `</invoke>`).
fn parse_invoke(body: &str) -> Written {
    let open = body.find("name=\"").ok_or("an <invoke> without a name")? + "name=\"".len();
    let close = body[open..].find('"').ok_or("an <invoke> without a name")? + open;
    let name = body[open..close].trim().to_string();
    let mut args = Vec::new();
    let mut rest = &body[close..];
    while let Some(at) = rest.find("<parameter name=\"") {
        let after = &rest[at + "<parameter name=\"".len()..];
        let key_end = after.find('"').ok_or_else(|| format!("`{name}`: a <parameter> without a name"))?;
        let key = after[..key_end].to_string();
        let value_start = after[key_end..].find('>').ok_or_else(|| format!("`{name}`: parameter `{key}` has no value"))? + key_end + 1;
        let (value, next) = match after[value_start..].find("</parameter>") {
            Some(end) => (&after[value_start..value_start + end], &after[value_start + end + "</parameter>".len()..]),
            None => return Err(format!("`{name}`: parameter `{key}` is not closed with </parameter> (was the reply cut off?)")),
        };
        args.push((key, strip_newline(value).to_string()));
        rest = next;
    }
    Ok((name, Args(args)))
}

/// A value's one leading and one trailing newline belong to the layout.
fn strip_newline(value: &str) -> &str {
    let value = value.strip_prefix("\r\n").or_else(|| value.strip_prefix('\n')).unwrap_or(value);
    value.strip_suffix("\r\n").or_else(|| value.strip_suffix('\n')).unwrap_or(value)
}

fn parse_json_call(body: &str) -> Written {
    let value = json::parse_depth(body.trim().as_bytes(), 32).map_err(|error| format!("a <tool_call> block is not JSON ({error}); use the <invoke> form"))?;
    let name = value.get("name").and_then(json::Value::as_str).ok_or("a <tool_call> block without a `name`")?.to_string();
    let args = match value.get("arguments") {
        Some(arguments @ json::Value::Obj(_)) => Args::from_json(&arguments.to_json())?,
        Some(json::Value::Str(text)) => Args::from_json(text)?,
        _ => Args::default(),
    };
    Ok((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_blocks_keep_splash_raw() {
        let raw = "Writing the title.\n\n<invoke name=\"write_motion\">\n<parameter name=\"name\">hook</parameter>\n<parameter name=\"source\">\nMotion{\n    inputs: {headline: \"Hi \\\"there\\\"\"}\n}\n</parameter>\n</invoke>\n<invoke name=\"inspect\">\n<parameter name=\"target\">hook</parameter>\n</invoke>";
        let reply = parse_reply(raw);
        assert_eq!(reply.text, "Writing the title.");
        assert_eq!(reply.calls.len(), 2);
        let (name, args) = reply.calls[0].as_ref().unwrap();
        assert_eq!(name, "write_motion");
        assert_eq!(args.0[1].1, "Motion{\n    inputs: {headline: \"Hi \\\"there\\\"\"}\n}");
        assert_eq!(reply.calls[1].as_ref().unwrap().0, "inspect");
    }

    #[test]
    fn function_calls_wrappers_json_blocks_and_invented_results() {
        let raw = "<function_calls>\n<invoke name=\"timeline\">\n</invoke>\n</function_calls>\n<tool_call>{\"name\": \"frame\", \"arguments\": {\"at\": 1.5}}</tool_call>\n<function_results>made up</function_results><invoke name=\"finish\"></invoke>";
        let reply = parse_reply(raw);
        assert_eq!(reply.calls.len(), 2);
        let (name, args) = reply.calls[1].as_ref().unwrap();
        assert_eq!((name.as_str(), args.0[0].1.as_str()), ("frame", "1.5"));
        assert!(reply.text.is_empty());
    }

    #[test]
    fn a_cut_off_parameter_is_reported() {
        let reply = parse_reply("<invoke name=\"write_doc\"><parameter name=\"source\">Flow{");
        assert!(reply.calls[0].as_ref().unwrap_err().contains("cut off"));
    }
}
