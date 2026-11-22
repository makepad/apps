//! A small pull reader for the XML in 3MF parts: elements, attributes and
//! text. Comments, processing instructions and doctype are skipped; entity
//! references in attribute values and text are decoded.

#[derive(Debug, Clone)]
pub enum XmlEvent<'a> {
    Start { name: &'a str, attrs: Vec<(&'a str, String)>, empty: bool },
    End { name: &'a str },
    Text(String),
}

pub struct XmlReader<'a> {
    s: &'a str,
    i: usize,
    pending_end: Option<&'a str>,
}

impl<'a> XmlReader<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { s, i: 0, pending_end: None }
    }

    pub fn next_event(&mut self) -> Option<Result<XmlEvent<'a>, String>> {
        if let Some(n) = self.pending_end.take() {
            return Some(Ok(XmlEvent::End { name: n }));
        }
        let b = self.s.as_bytes();
        loop {
            if self.i >= b.len() {
                return None;
            }
            if b[self.i] != b'<' {
                let start = self.i;
                while self.i < b.len() && b[self.i] != b'<' {
                    self.i += 1;
                }
                let t = &self.s[start..self.i];
                if t.trim().is_empty() {
                    continue;
                }
                return Some(Ok(XmlEvent::Text(decode(t))));
            }
            let rest = &self.s[self.i..];
            if rest.starts_with("<!--") {
                match rest.find("-->") {
                    Some(e) => self.i += e + 3,
                    None => return Some(Err("unterminated comment".into())),
                }
                continue;
            }
            if rest.starts_with("<?") {
                match rest.find("?>") {
                    Some(e) => self.i += e + 2,
                    None => return Some(Err("unterminated processing instruction".into())),
                }
                continue;
            }
            if rest.starts_with("<!") {
                match rest.find('>') {
                    Some(e) => self.i += e + 1,
                    None => return Some(Err("unterminated declaration".into())),
                }
                continue;
            }
            if rest.starts_with("</") {
                let e = match rest.find('>') {
                    Some(e) => e,
                    None => return Some(Err("unterminated end tag".into())),
                };
                let name = rest[2..e].trim();
                self.i += e + 1;
                return Some(Ok(XmlEvent::End { name }));
            }
            // Start tag.
            let mut j = self.i + 1;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let name = &self.s[self.i + 1..j];
            let mut attrs = Vec::new();
            loop {
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= b.len() {
                    return Some(Err("unterminated start tag".into()));
                }
                if b[j] == b'>' {
                    self.i = j + 1;
                    return Some(Ok(XmlEvent::Start { name, attrs, empty: false }));
                }
                if b[j] == b'/' {
                    if j + 1 < b.len() && b[j + 1] == b'>' {
                        self.i = j + 2;
                        self.pending_end = Some(name);
                        return Some(Ok(XmlEvent::Start { name, attrs, empty: true }));
                    }
                    return Some(Err("bad tag end".into()));
                }
                let k0 = j;
                while j < b.len() && b[j] != b'=' && !b[j].is_ascii_whitespace() {
                    j += 1;
                }
                let key = &self.s[k0..j];
                while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b'=') {
                    j += 1;
                }
                if j >= b.len() || (b[j] != b'"' && b[j] != b'\'') {
                    return Some(Err(format!("attribute {key} without quoted value")));
                }
                let q = b[j];
                j += 1;
                let v0 = j;
                while j < b.len() && b[j] != q {
                    j += 1;
                }
                if j >= b.len() {
                    return Some(Err("unterminated attribute".into()));
                }
                attrs.push((key, decode(&self.s[v0..j])));
                j += 1;
            }
        }
    }
}

pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let e = match rest.find(';') {
            Some(e) => e,
            None => break,
        };
        let ent = &rest[1..e];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with("#x") => {
                if let Some(c) = u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32) {
                    out.push(c)
                }
            }
            _ if ent.starts_with('#') => {
                if let Some(c) = ent[1..].parse::<u32>().ok().and_then(char::from_u32) {
                    out.push(c)
                }
            }
            _ => {
                out.push_str(&rest[..=e]);
            }
        }
        rest = &rest[e + 1..];
    }
    out.push_str(rest);
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn attr<'b>(attrs: &'b [(&str, String)], key: &str) -> Option<&'b str> {
    attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
}
