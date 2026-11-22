//! A firmware part's connections, as a bench declares them (`b.net(comp, c, opts)`): what the twin
//! plays for connection `c` of the part's program, and the real server it may reach.
//!
//! - The twin plays a fake server (echo, sink, the pictures of a project folder, or a table of
//!   rules) under the bench's conditions (latency, jitter, rate) unless the person turned the real
//!   network on for the project; then a connection that names a real end ([`Real`]) reaches it
//!   (`fab_shell::firmware_net` keeps that choice and the tokens).
//! - A real end is a server by its address (`url:`), or a service of this machine's own AI hub by
//!   its name (`hub: "talk"`), which Fab finds through the hub's client and reaches on loopback
//!   only: the bench holds no address and no token for it. `model:` names the chat model the
//!   service's turns ask for (the hub's own choice without it).
//! - A connection that may send a token names its endpoint: the key its token is saved under in the
//!   person's own settings. A token never crosses the network in plain text: a named endpoint
//!   reaches its server over TLS (`wss://`, `tls://`) unless the server is on this machine
//!   ([`Server::carries_tokens`]). A server without a name (a plain `ws://` device on the LAN) is
//!   reached without a token. The address carries no credentials: a URL with `user@` is refused.
//! - The program never holds a name, an address or a token: `c` is all it knows.

pub use fab_elec_mcu_rt::net::{Field, When};

/// What the twin plays for a connection while it does not reach a real server.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Fake {
    /// Sends every whole message straight back.
    #[default]
    Echo,
    /// Takes everything and sends nothing.
    Sink,
    /// Serves the pictures in the project's `firmware/<folder>/` (`/`-separated below it), fitted
    /// to the size the device asks: `fab_elec_fw::pictures`. A folder of its own, not a firmware's
    /// (`firmware/<name>/` for `<name>.fw.splash`), whose pictures go into its image.
    Files(String),
    /// Answers by a table (`fake: {rules: [..]}`): `fab_elec_mcu_rt::net::Rules`, with the files its
    /// sends carry still named; the twin reads them when it builds the run.
    ///
    /// It plays a server's answers, not its checks, so it is no checker of a protocol: a `wav`
    /// plays at the rate the rules name (a server that follows its client's rate, as the spoken
    /// turn's does from `hello`, is not followed), and nothing it is sent is refused (a gap in a
    /// client's blocks gets no `error`).
    Rules(Vec<RuleSpec>),
}

impl Fake {
    /// The folders under the project's `firmware/` this fake reads (a rules fake's, by its files).
    pub fn folders(&self) -> Vec<&str> {
        match self {
            Fake::Files(folder) => vec![folder.as_str()],
            Fake::Rules(rules) => rules.iter().flat_map(|rule| &rule.send).filter_map(|send| send.body.file())
                .map(|file| file.rsplit_once('/').map_or("", |(folder, _)| folder)).collect(),
            Fake::Echo | Fake::Sink => Vec::new(),
        }
    }
}

/// A rule of a rules fake: `fab_elec_mcu_rt::net::Rule` with its bodies named.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleSpec {
    pub when: When,
    pub stop: bool,
    pub send: Vec<SendSpec>,
}

/// One send of a rule: `fab_elec_mcu_rt::net::Then` with its body named.
#[derive(Clone, Debug, PartialEq)]
pub struct SendSpec {
    /// Seconds after the rule matched or its previous send ended.
    pub after: f64,
    pub binary: bool,
    pub head: Vec<Field>,
    pub body: Body,
    /// Units of body per message (0: all of it in one), `every` seconds apart.
    pub block: usize,
    pub every: f64,
}

/// What a send carries after its head.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Body {
    None,
    /// A file under the project's `firmware/` (`<folder>/<name>`) as it is; a unit is a byte.
    File(String),
    /// A WAV file under the project's `firmware/`, as 16-bit little-endian samples at `rate` Hz; a
    /// unit is a sample.
    Wav { file: String, rate: u32 },
}

impl Body {
    /// The file it names, under the project's `firmware/`.
    pub fn file(&self) -> Option<&str> {
        match self {
            Body::None => None,
            Body::File(file) | Body::Wav { file, .. } => Some(file),
        }
    }

    /// The bytes of one unit.
    pub fn unit(&self) -> usize { if matches!(self, Body::Wav { .. }) { 2 } else { 1 } }
}

/// A send's head written as text: `{n}` is word n of the matched text message (word 0 its first),
/// `{n:u16}` and `{n:u32}` that word as a little-endian number, `{offset:u32}` where the message's
/// part of the body starts, `{sent}` the units of binary body sent so far (with those of the sends
/// a stop ended); `{{` and `}}` are braces.
pub fn template(text: &str) -> Result<Vec<Field>, String> {
    let word = |n: &str| n.parse::<u8>().map_err(|_| format!("{{{n}}}: a word is numbered from 0 to 255"));
    let (mut fields, mut bytes, mut rest) = (Vec::new(), Vec::new(), text);
    while let Some(k) = rest.find(['{', '}']) {
        bytes.extend_from_slice(rest[..k].as_bytes());
        let brace = rest.as_bytes()[k];
        rest = &rest[k + 1..];
        if rest.as_bytes().first() == Some(&brace) {
            bytes.push(brace);
            rest = &rest[1..];
            continue;
        }
        if brace == b'}' { return Err("a } with no { before it: write }} for a brace".into()); }
        let end = rest.find('}').ok_or("a { with no } after it: write {{ for a brace")?;
        let field = match rest[..end].split_once(':') {
            None if &rest[..end] == "sent" => Field::Sent,
            None => Field::Word(word(&rest[..end])?),
            Some(("offset", "u32")) => Field::Offset32,
            Some((n, "u16")) => Field::Word16(word(n)?),
            Some((n, "u32")) => Field::Word32(word(n)?),
            _ => return Err(format!("{{{}}} is none of {{n}}, {{n:u16}}, {{n:u32}}, {{offset:u32}}, {{sent}}", &rest[..end])),
        };
        rest = &rest[end + 1..];
        if !bytes.is_empty() { fields.push(Field::Bytes(std::mem::take(&mut bytes))); }
        fields.push(field);
    }
    bytes.extend_from_slice(rest.as_bytes());
    if !bytes.is_empty() { fields.push(Field::Bytes(bytes)); }
    Ok(fields)
}

/// A folder or file path inside the project's electronics files: `/`-separated names, none empty,
/// `.` or `..`.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty() && !path.contains('\\') && path.split('/').all(|name| !name.is_empty() && name != "." && name != "..")
}

/// How a server is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// WebSocket (`ws://`).
    Ws,
    /// WebSocket over TLS (`wss://`).
    Wss,
    /// A TCP stream (`tcp://`).
    Tcp,
    /// A TCP stream over TLS (`tls://`).
    Tls,
}

impl Scheme {
    pub fn prefix(self) -> &'static str {
        match self {
            Scheme::Ws => "ws",
            Scheme::Wss => "wss",
            Scheme::Tcp => "tcp",
            Scheme::Tls => "tls",
        }
    }

    pub fn tls(self) -> bool { matches!(self, Scheme::Wss | Scheme::Tls) }

    /// The `net` declaration kind it carries (0 WebSocket, 1 TCP stream).
    pub fn kind(self) -> u8 { if matches!(self, Scheme::Ws | Scheme::Wss) { 0 } else { 1 } }
}

/// A real server's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub scheme: Scheme,
    /// A DNS name, a dotted IPv4 address or a bracketed IPv6 address, lowercased.
    pub host: String,
    pub port: u16,
    /// The WebSocket request path (`/` at least); empty for a TCP stream.
    pub path: String,
}

impl Server {
    /// `ws://host[:port]/path`, `wss://...`, `tcp://host:port` or `tls://host:port`. WebSocket ports
    /// default to 80 and 443; a stream names its port.
    pub fn parse(url: &str) -> Result<Server, String> {
        if url.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || !b.is_ascii()) {
            return Err("a server address has no spaces or special characters".into());
        }
        let (scheme, rest) = url.split_once("://").ok_or("a server address starts with ws://, wss://, tcp:// or tls://")?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "ws" => Scheme::Ws,
            "wss" => Scheme::Wss,
            "tcp" => Scheme::Tcp,
            "tls" => Scheme::Tls,
            other => return Err(format!("{other}:// is not a server address: use ws://, wss://, tcp:// or tls://")),
        };
        let (authority, path) = match rest.find('/') {
            Some(k) => (&rest[..k], &rest[k..]),
            None => (rest, ""),
        };
        if authority.contains('@') {
            return Err("a server address holds no user name or password: the token is kept in your Fab settings".into());
        }
        if path.contains('#') {
            return Err("a server address has no #fragment".into());
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (inside, after) = v6.split_once(']').ok_or("an IPv6 address closes with ]")?;
            if inside.is_empty() || !inside.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') {
                return Err(format!("[{inside}] is not an IPv6 address"));
            }
            (format!("[{}]", inside.to_ascii_lowercase()), after.strip_prefix(':'))
        } else {
            let (host, port) = match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            let valid = !host.is_empty() && host.len() <= 253 && host.split('.').all(|label| {
                !label.is_empty() && label.len() <= 63 && !label.starts_with('-') && !label.ends_with('-')
                    && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
            if !valid {
                return Err(format!("{host:?} is not a host name"));
            }
            (host.to_ascii_lowercase(), port)
        };
        let port = match (port, scheme) {
            (Some(port), _) => port.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| format!("{port:?} is not a port"))?,
            (None, Scheme::Ws) => 80,
            (None, Scheme::Wss) => 443,
            (None, _) => return Err(format!("a {}:// address names its port", scheme.prefix())),
        };
        let path = match scheme {
            Scheme::Ws | Scheme::Wss => if path.is_empty() { "/".to_string() } else { path.to_string() },
            Scheme::Tcp | Scheme::Tls if path.is_empty() || path == "/" => String::new(),
            _ => return Err(format!("a {}:// stream has no path", scheme.prefix())),
        };
        Ok(Server { scheme, host, port, path })
    }

    /// The address as written back: what a log line and the settings show.
    pub fn url(&self) -> String { format!("{}://{}:{}{}", self.scheme.prefix(), self.host, self.port, self.path) }

    /// Whether a token may go to this server: over TLS, or to this machine.
    pub fn carries_tokens(&self) -> bool { self.scheme.tls() || loopback(&self.host) }
}

/// This machine: `localhost`, 127.0.0.0/8 or `[::1]`, the only place a token may go unencrypted.
pub fn loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
        || host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).and_then(|h| h.parse::<std::net::Ipv6Addr>().ok()).is_some_and(|ip| ip.is_loopback())
}

/// Where a connection goes when the person turns the project's real network on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Real {
    /// A server by its address (`url:`).
    Server(Server),
    /// A service of this machine's own AI hub (`hub:`), by its route's name (`talk`: `/talk`), and
    /// the chat model its turns ask for (`model:`; without it, the hub's own choice).
    Hub { service: String, model: Option<String> },
}

impl Real {
    /// What the person approves for a project: the server's host, or `hub:<service>` (no host name
    /// has a `:`, so the two never meet).
    pub fn approval(&self) -> String {
        match self {
            Real::Server(server) => server.host.clone(),
            Real::Hub { service, .. } => format!("hub:{service}"),
        }
    }

    /// What the pane and the log show: the address without its query, or the hub's service.
    pub fn shown(&self) -> String {
        match self {
            Real::Server(server) => server.url().split('?').next().unwrap_or_default().to_string(),
            Real::Hub { service, model: None } => format!("this computer's hub: {service}"),
            Real::Hub { service, model: Some(model) } => format!("this computer's hub: {service} with {model}"),
        }
    }
}

/// Connection `c` of the program running on the part at `path`.
#[derive(Clone, Debug, PartialEq)]
pub struct NetEndpoint {
    /// Component path of the firmware part (`mcu.u`).
    pub path: String,
    pub c: u8,
    /// The endpoint's name, the key of its token in the person's settings; without one, no token.
    /// Only a server's: a hub service takes none.
    pub name: Option<String>,
    pub real: Option<Real>,
    pub fake: Fake,
    /// Each way, a message's delay in seconds, plus up to `jitter` seconds (seeded).
    pub latency: f64,
    pub jitter: f64,
    /// Bytes a second each way (0: no limit).
    pub rate: f64,
}

/// A chat model's id as the bench takes it: not empty, no spaces or control characters.
pub fn valid_model(model: &str) -> bool {
    !model.is_empty() && !model.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// An endpoint name: letters, digits, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_parse_to_scheme_host_port_and_path() {
        let s = Server::parse("wss://Chat.Example.org/ws?room=1").unwrap();
        assert_eq!((s.scheme, s.host.as_str(), s.port, s.path.as_str(), s.scheme.kind()), (Scheme::Wss, "chat.example.org", 443, "/ws?room=1", 0));
        assert_eq!(s.url(), "wss://chat.example.org:443/ws?room=1");
        let s = Server::parse("ws://192.0.2.7:8080").unwrap();
        assert_eq!((s.port, s.path.as_str()), (8080, "/"), "a WebSocket's path is / at least");
        let s = Server::parse("tls://[2001:DB8::1]:9000/").unwrap();
        assert_eq!((s.host.as_str(), s.port, s.path.as_str(), s.scheme.tls(), s.scheme.kind()), ("[2001:db8::1]", 9000, "", true, 1));
        for (url, carries) in [
            ("wss://chat.example.org/", true), ("tls://192.0.2.7:9000", true),
            ("ws://chat.example.org/", false), ("tcp://192.0.2.7:7", false), ("ws://127.0.0.2:8080/", true),
            ("ws://localhost:8080/", true), ("ws://[::1]:8080/", true), ("ws://[::2]:8080/", false),
            ("ws://localhost.example.org/", false),
        ] {
            assert_eq!(Server::parse(url).unwrap().carries_tokens(), carries, "{url}: a token goes only over TLS or to this machine");
        }
        for (bad, says) in [
            ("http://example.org/", "use ws://"),
            ("ws://user:pw@example.org/", "no user name or password"),
            ("tcp://example.org", "names its port"),
            ("tcp://example.org:7/x", "has no path"),
            ("ws://exa mple.org/", "no spaces"),
            ("ws://-bad.example/", "not a host name"),
            ("ws://example.org:0/", "not a port"),
            ("ws://example.org/#frag", "no #fragment"),
            ("example.org", "starts with ws://"),
        ] {
            let error = Server::parse(bad).unwrap_err();
            assert!(error.contains(says), "{bad}: {error}");
        }
    }
}
