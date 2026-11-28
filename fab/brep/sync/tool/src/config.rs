//! Reader for the deliberately small rules.toml language.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Inline(BTreeMap<String, Value>),
    Text(String),
    Strings(Vec<String>),
    Integer(i64),
    Bool(bool),
}

pub(crate) struct Row {
    pub name: String,
    pub array: bool,
    pub location: String,
    pub fields: BTreeMap<String, (String, Value)>,
}

impl Row {
    fn take(&mut self, key: &str) -> Result<(String, Value), String> {
        self.fields.remove(key).ok_or_else(|| format!("{}: missing {key}", self.location))
    }

    pub fn string(&mut self, key: &str) -> Result<String, String> {
        match self.take(key)? {
            (_, Value::Text(s)) => Ok(s),
            (at, _) => Err(format!("{at}: {key} requires a string")),
        }
    }

    pub fn strings(&mut self, key: &str) -> Result<Vec<String>, String> {
        match self.take(key)? {
            (_, Value::Strings(v)) => Ok(v),
            (at, _) => Err(format!("{at}: {key} requires an array of strings")),
        }
    }

    pub fn boolean(&mut self, key: &str) -> Result<bool, String> {
        match self.take(key)? {
            (_, Value::Bool(b)) => Ok(b),
            (at, _) => Err(format!("{at}: {key} requires a boolean")),
        }
    }

    pub fn count(&mut self, key: &str) -> Result<usize, String> {
        match self.take(key)? {
            (_, Value::Integer(n)) if usize::try_from(n).is_ok() => Ok(n as usize),
            (at, _) => Err(format!("{at}: {key} requires a nonnegative count")),
        }
    }

    pub fn finish(self) -> Result<(), String> {
        match self.fields.into_iter().next() {
            Some((key, (at, _))) => Err(format!("{at}: unknown key {key} in {}", self.name)),
            None => Ok(()),
        }
    }
}

struct Reader<'a> {
    cargo: bool,
    path: &'a str,
    rest: &'a str,
    line: usize,
}

impl Reader<'_> {
    fn error(&self, message: &str) -> String { format!("{}:{}: {message}", self.path, self.line) }

    fn eat(&mut self, prefix: &str) -> bool {
        if let Some(rest) = self.rest.strip_prefix(prefix) {
            self.rest = rest;
            true
        } else {
            false
        }
    }

    fn spaces(&mut self) { self.rest = self.rest.trim_start_matches([' ', '\t']); }

    fn eol(&mut self) -> Result<(), String> {
        self.spaces();
        if self.rest.starts_with('#') {
            self.rest = self.rest.trim_start_matches(|c| c != '\n' && c != '\r');
        }
        if self.eat("\r\n") || self.eat("\n") { self.line += 1; }
        else if !self.rest.is_empty() { return Err(self.error("expected end of line")); }
        Ok(())
    }

    fn layout(&mut self) -> Result<(), String> {
        self.spaces();
        while self.rest.starts_with(['#', '\n', '\r']) { self.eol()?; self.spaces(); }
        Ok(())
    }

    fn key(&mut self) -> Result<String, String> {
        let end = self.rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-').count();
        if end == 0 { return Err(self.error("expected a bare key")); }
        let (key, rest) = self.rest.split_at(end);
        self.rest = rest;
        Ok(key.into())
    }

    fn dotted_key(&mut self) -> Result<String, String> {
        let mut key = self.key()?;
        while self.cargo && self.eat(".") {
            key.push('.');
            key.push_str(&self.key()?);
        }
        Ok(key)
    }

    fn string(&mut self) -> Result<String, String> {
        if !self.eat("\"") { return Err(self.error("expected a double-quoted string")); }
        let mut result = String::new();
        while let Some(c) = self.rest.chars().next() {
            self.rest = &self.rest[c.len_utf8()..];
            match c {
                '"' => return Ok(result),
                '\\' if self.eat("\"") => result.push('"'),
                '\\' if self.eat("\\") => result.push('\\'),
                '\\' if self.eat("t") => result.push('\t'),
                '\\' if self.eat("r") => result.push('\r'),
                '\\' if self.eat("n") => result.push('\n'),
                '\\' => return Err(self.error("unsupported string escape")),
                c if c.is_control() => return Err(self.error("control character in string")),
                _ => result.push(c),
            }
        }
        Err(self.error("unterminated string"))
    }

    fn value(&mut self) -> Result<Value, String> {
        if self.cargo && self.eat("{") {
            let mut fields = BTreeMap::new();
            loop {
                self.spaces();
                let key = self.key()?;
                self.spaces();
                if !self.eat("=") { return Err(self.error("expected = in inline table")); }
                self.spaces();
                let value = self.value()?;
                if fields.insert(key.clone(), value).is_some() { return Err(self.error(&format!("duplicate key {key}"))); }
                self.spaces();
                if self.eat("}") { return Ok(Value::Inline(fields)); }
                if !self.eat(",") { return Err(self.error("expected comma or } in inline table")); }
            }
        }
        if self.rest.starts_with('"') { return self.string().map(Value::Text); }
        if self.eat("[") {
            let mut strings = Vec::new();
            self.layout()?;
            while !self.eat("]") {
                strings.push(self.string()?);
                self.layout()?;
                if self.eat("]") { break; }
                if !self.eat(",") { return Err(self.error("expected comma or ]")); }
                self.layout()?;
            }
            return Ok(Value::Strings(strings));
        }
        if self.eat("true") { return Ok(Value::Bool(true)); }
        if self.eat("false") { return Ok(Value::Bool(false)); }
        let end = self.rest.find([' ', '\t', '\r', '\n', '#', ',', '}']).unwrap_or(self.rest.len());
        let (word, rest) = self.rest.split_at(end);
        let digits = word.trim_start_matches(['-', '+']);
        if digits.len() > 1 && digits.starts_with('0') { return Err(self.error("leading zero in integer")); }
        let n = word.parse().map_err(|_| self.error("expected a string, integer, boolean or string array"))?;
        self.rest = rest;
        Ok(Value::Integer(n))
    }
}

pub(crate) fn parse(path: &str, source: &str) -> Result<Vec<Row>, String> {
    document(path, source, false)
}

pub(crate) fn document(path: &str, source: &str, cargo: bool) -> Result<Vec<Row>, String> {
    let mut reader = Reader { path, rest: source, line: 1, cargo };
    let mut rows: Vec<Row> = Vec::new();
    let mut singles = BTreeSet::new();
    let mut arrays = BTreeSet::new();
    reader.layout()?;
    while !reader.rest.is_empty() {
        let location = format!("{path}:{}", reader.line);
        if reader.eat("[") {
            let array = reader.eat("[");
            reader.spaces();
            let name = reader.dotted_key()?;
            reader.spaces();
            if !reader.eat(if array { "]]" } else { "]" }) { return Err(reader.error("malformed table header")); }
            if singles.contains(&name) || (!array && arrays.contains(&name)) {
                return Err(reader.error(&format!("duplicate or conflicting table {name}")));
            }
            if array { arrays.insert(name.clone()); } else { singles.insert(name.clone()); }
            rows.push(Row { name, array, location, fields: BTreeMap::new() });
        } else {
            let row = rows.last_mut().ok_or_else(|| reader.error("expected a table before its keys"))?;
            let key = if reader.rest.starts_with('"') { reader.string()? } else { reader.dotted_key()? };
            reader.spaces();
            if !reader.eat("=") { return Err(reader.error("expected = after key")); }
            reader.spaces();
            let value = reader.value()?;
            if row.fields.insert(key.clone(), (location.clone(), value)).is_some() {
                return Err(format!("{location}: duplicate key {key}"));
            }
        }
        reader.eol()?;
        reader.layout()?;
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_reader_preserves_values_and_refuses_unsupported_syntax() {
        let source = "[one]\nn = 2\nflag = false\ns = \"a # b\" # comment\n[[row]]\nx = [\"a\", # first\n\"b\",]\n[[row]]\nx = []";
        let mut rows = parse("sample", source).unwrap().into_iter();
        let mut one = rows.next().unwrap();
        assert_eq!(one.count("n").unwrap(), 2);
        assert!(!one.boolean("flag").unwrap());
        assert_eq!(one.string("s").unwrap(), "a # b");
        assert_eq!(rows.next().unwrap().strings("x").unwrap(), ["a", "b"]);
        assert!(rows.next().unwrap().strings("x").unwrap().is_empty());
        for (bad, line) in [
            ("[t]\nn = 1.2", 2), ("[t]\nn = [\"ok\", \n 42]", 3),
            ("[t]\nk = {a = 1}", 2), ("[t]\n[t]", 2), ("[t]\na = true\na = false", 3),
            ("[[t]]\n[t]", 2), ("[t]\na = \"x\" tail", 2), ("[t]\na = 01", 2),
        ] {
            let error = parse("sample", bad).err().unwrap();
            assert!(error.starts_with(&format!("sample:{line}:")), "{error}");
        }
    }
}
