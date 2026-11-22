use std::{collections::{BTreeMap, BTreeSet}, fmt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportClass { Gate(u32), Glue, Rom(u32), WeakAbsent }

impl fmt::Display for ImportClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gate(n) => write!(f, "gate {n}"),
            Self::Glue => write!(f, "glue"),
            Self::Rom(address) => write!(f, "rom {address:#010x}"),
            Self::WeakAbsent => write!(f, "weak-absent"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ImportMap { pub entries: BTreeMap<String, ImportClass> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapError { pub line: usize, pub message: String }

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for MapError {}

impl ImportMap {
    pub fn parse(text: &str) -> Result<Self, MapError> {
        let mut map = Self::default();
        let mut gates = BTreeSet::new();
        for (n, line) in text.lines().enumerate() {
            let words: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
            let fail = |message: &str| MapError { line: n + 1, message: message.into() };
            let (symbol, class) = match words.as_slice() {
                [] => continue,
                ["gate", index, symbol] => {
                    if !index.bytes().all(|b| b.is_ascii_digit()) { return Err(fail("invalid gate index")); }
                    let index = index.parse::<u32>().map_err(|_| fail("invalid gate index"))?;
                    if !gates.insert(index) { return Err(fail("duplicate gate index")); }
                    (*symbol, ImportClass::Gate(index))
                }
                ["glue", symbol] => (*symbol, ImportClass::Glue),
                ["rom", symbol, address] => {
                    let hex = address.strip_prefix("0x").filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit()))
                        .ok_or_else(|| fail("ROM address must be 0x followed by hexadecimal digits"))?;
                    (*symbol, ImportClass::Rom(u32::from_str_radix(hex, 16).map_err(|_| fail("ROM address out of range"))?))
                }
                ["weak-absent", symbol] => (*symbol, ImportClass::WeakAbsent),
                _ => return Err(fail("expected gate <index> <symbol>, glue <symbol>, rom <symbol> 0x<address>, or weak-absent <symbol>")),
            };
            if map.entries.insert(symbol.into(), class).is_some() { return Err(fail("duplicate symbol")); }
        }
        Ok(map)
    }
}

pub fn parse_roots(text: &str) -> Result<Vec<String>, MapError> {
    let mut roots = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let words: Vec<_> = line.split('#').next().unwrap_or("").split_whitespace().collect();
        match words.as_slice() {
            [] => {},
            [symbol] => roots.push((*symbol).into()),
            _ => return Err(MapError { line: n + 1, message: "expected one root symbol".into() }),
        }
    }
    Ok(roots)
}
