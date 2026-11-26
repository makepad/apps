//! The app's remote commands (docs/agents/app-remote.md): makepad-remote over the app's Unix
//! socket, ownership checked on every answer as tools/ci does.

use makepad_remote::Client;
use makepad_strict_json::{self as json, Value};
use std::time::Duration;

pub struct Remote {
    /// Connected once the app's startup line names its socket.
    client: Option<Client>,
    /// The `activity` counter pinned at launch; mutations carry it, and an answer whose
    /// end line reports another counter means a person touched the window.
    pub user_seq: Option<u64>,
}

/// The `user_seq` of an answer's end line.
fn end_seq(end: &str) -> Option<u64> {
    end.split("\"user_seq\":").nth(1)?.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

impl Remote {
    pub fn new() -> Remote {
        Remote { client: None, user_seq: None }
    }

    /// Connects to the socket of the startup line.
    pub fn connect(&mut self, socket: &str) -> Result<(), String> {
        self.client = Some(Client::connect(socket).map_err(|e| format!("the app's remote at {socket}: {e}"))?);
        Ok(())
    }

    /// Runs the command `argv` (its words and `k=v` arguments); `mutate` adds the pinned
    /// `if_user_seq`. `timeout` bounds the exchange (a long `step` answers after its last
    /// frame): past it the request is closed and this is an error, so a hung child cannot block
    /// the caller.
    pub fn get(&self, argv: &[String], mutate: bool, timeout: Duration) -> Result<String, String> {
        let shown = argv.join(" ");
        let client = self.client.as_ref().ok_or("the app's remote is not connected")?;
        let mut argv = argv.to_vec();
        match (mutate, self.user_seq) {
            (true, Some(seq)) => argv.push(format!("if_user_seq={seq}")),
            (true, None) => return Err("refusing an input before activity".into()),
            (false, _) => {}
        }
        client.set_timeout(Some(timeout));
        let mut lines = Vec::new();
        let (_, end) = makepad_remote::app::run_end(client, &argv, &mut |l| {
            lines.push(l.to_string());
            true
        })
        .map_err(|e| format!("{shown}: {e}"))?;
        let text = lines.join("\n");
        if text.starts_with("{\"err\":\"user_interacting\"") || text.starts_with("{\"err\":\"user_intervened\"") {
            return Err(format!("{shown}: a person used the window; the run is void"));
        }
        if let Some(seq) = self.user_seq {
            match end.as_deref().and_then(end_seq) {
                Some(now) if now != seq => return Err(format!("{shown}: a person used the window (user_seq {now}); the run is void")),
                Some(_) => {}
                // the app is gone after a quit
                None if matches!(argv.first().map(String::as_str), Some("quit" | "gq")) => {}
                None => return Err(format!("{shown}: the answer carries no ownership reading")),
            }
        }
        Ok(text)
    }

    /// A JSON command; an `err` field is an error.
    pub fn json(&self, argv: &[String], mutate: bool, timeout: Duration) -> Result<Value, String> {
        let shown = argv.join(" ");
        let text = self.get(argv, mutate, timeout)?;
        let value = json::parse_depth(text.as_bytes(), 32).map_err(|e| format!("{shown}: {e}: {}", text.trim()))?;
        if let Some(e) = value.get("err") {
            return Err(format!("{shown}: {}", e.as_str().map(str::to_string).unwrap_or_else(|| e.to_json())));
        }
        Ok(value)
    }
}

/// The socket from our child's startup line.
pub fn parse_startup(line: &str, pid: u32) -> Option<String> {
    let (actual, _, socket) = makepad_remote::parse_startup_line(line)?;
    if actual != pid { return None; }
    socket.into_os_string().into_string().ok()
}

/// A number field of a JSON object as f64 (ints and floats).
pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_line_names_our_socket() {
        let line = "[makepad-remote] pid=25107 app=makepad-example-counter socket=/tmp/a b/makepad-remote-25107.sock";
        assert_eq!(parse_startup(line, 25107).as_deref(), Some("/tmp/a b/makepad-remote-25107.sock"));
        assert_eq!(parse_startup(line, 1), None);
        assert_eq!(end_seq("{\"end\":\"snap\",\"ms\":1,\"user_seq\":4,\"user_seq_start\":4}"), Some(4));
    }
}
