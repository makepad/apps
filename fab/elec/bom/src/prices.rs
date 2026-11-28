//! The offline price index. The host supplies the text and publishes immutable snapshots.
use crate::rates::{array, number, text, Money};
use makepad_strict_json::{obj, s, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq)]
pub enum PriceKey {
    Mpn(String),
    Supplier { supplier: String, part: String },
}

// Keep the spelling on the entry. Only the supplier name folds for purchasing identity,
// exactly as CAM's ${SUPPLIER_KEY} does. Neither MPNs nor supplier part numbers fold.
impl Ord for PriceKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Mpn(a), Self::Mpn(b)) => a.cmp(b),
            (Self::Mpn(_), Self::Supplier { .. }) => Ordering::Less,
            (Self::Supplier { .. }, Self::Mpn(_)) => Ordering::Greater,
            (Self::Supplier { supplier: a, part: ap }, Self::Supplier { supplier: b, part: bp }) => {
                let folded = |byte: u8| byte.to_ascii_lowercase();
                a.trim().bytes().map(folded).cmp(b.trim().bytes().map(folded)).then_with(|| ap.cmp(bp))
            }
        }
    }
}

impl PartialOrd for PriceKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl PartialEq for PriceKey {
    fn eq(&self, other: &Self) -> bool { self.cmp(other).is_eq() }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sourcing {
    pub mpn: String,
    pub supplier: String,
    pub supplier_part: String,
}

impl Sourcing {
    pub fn supplier_key(&self) -> Option<PriceKey> {
        let (supplier, part) = (self.supplier.trim(), self.supplier_part.trim());
        (!supplier.is_empty() && !part.is_empty()).then(|| PriceKey::Supplier {
            supplier: supplier.into(), part: part.into(),
        })
    }

    pub fn mpn_key(&self) -> Option<PriceKey> {
        let mpn = self.mpn.trim();
        (!mpn.is_empty()).then(|| PriceKey::Mpn(mpn.into()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Source {
    pub name: String,
    pub date: String,
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: PriceKey,
    pub currency: String,
    /// Minimum orderable quantity at this unit price in millionths.
    pub breaks: BTreeMap<i64, i64>,
    pub moq: i64,
    pub multiple: i64,
    pub source: Source,
    pub assembly_class: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Prices(pub BTreeMap<PriceKey, Entry>);

impl Prices {
    pub fn resolve(&self, part: &Sourcing) -> Option<&Entry> {
        part.supplier_key().and_then(|k| self.0.get(&k))
            .or_else(|| part.mpn_key().and_then(|k| self.0.get(&k)))
    }
}

pub fn valid_date(date: &str) -> bool {
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' ||
        !b.iter().enumerate().all(|(i,c)| i == 4 || i == 7 || c.is_ascii_digit()) { return false; }
    let year: u32 = date[..4].parse().unwrap();
    let month: u32 = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    let last = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    };
    year > 0 && day > 0 && day <= last
}

/// Parse a decimal amount exactly; decimal input never passes through a float.
pub fn millionths(input: &str) -> Result<i64, String> {
    let (whole, fraction) = input.trim().split_once('.').unwrap_or((input.trim(), ""));
    if (whole.is_empty() && fraction.is_empty()) || fraction.len() > 6 ||
        !whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit()) {
        return Err("enter a nonnegative amount with at most six decimal places".into());
    }
    let micros = format!("{whole}{fraction:0<6}");
    micros.parse().map_err(|_| "amount exceeds integer millionths".into())
}

impl Entry {
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let optional = |key| v.get(key).map(|_| text(v, key)).transpose();
        let part = Sourcing {
            mpn: optional("mpn")?.unwrap_or_default(),
            supplier: optional("supplier")?.unwrap_or_default(),
            supplier_part: optional("supplier_part")?.unwrap_or_default(),
        };
        let key = match (part.mpn_key(), part.supplier_key()) {
            (None, Some(key)) => key,
            (Some(key), None) if part.supplier.is_empty() && part.supplier_part.is_empty() => key,
            _ => return Err("identify the price by MPN or by both supplier and supplier part".into()),
        };
        let currency = Money::currency(v)?;
        let mut breaks = BTreeMap::new();
        for row in array(v, "breaks")? {
            let min = number(row, "min")?;
            if min == 0 || breaks.insert(min, number(row, "micros")?).is_some() {
                return Err("price breaks need distinct positive minimum quantities".into());
            }
        }
        if breaks.is_empty() { return Err("a price needs at least one break".into()); }
        let positive = |key| match v.get(key) {
            None => Ok(1),
            Some(_) => number(v,key).and_then(|n| if n > 0 { Ok(n) } else { Err(format!("{key} must be positive")) }),
        };
        let date = text(v, "date")?;
        if !valid_date(&date) { return Err("date must be a calendar date in YYYY-MM-DD form".into()); }
        Ok(Self { key, currency, breaks, moq: positive("moq")?, multiple: positive("multiple")?,
            source: Source { name: text(v, "source")?, date, url: optional("url")? }, assembly_class: optional("assembly_class")? })
    }

    pub fn json(&self) -> Value {
        let mut fields = match &self.key {
            PriceKey::Mpn(mpn) => vec![("mpn", s(mpn))],
            PriceKey::Supplier { supplier, part } => vec![("supplier", s(supplier)), ("supplier_part", s(part))],
        };
        fields.extend([("currency", s(&self.currency)), ("moq", Value::Int(self.moq)), ("multiple", Value::Int(self.multiple)),
            ("source", s(&self.source.name)), ("date", s(&self.source.date)),
            ("breaks", Value::Arr(self.breaks.iter().map(|(&q, &u)| obj(vec![("min", Value::Int(q)), ("micros", Value::Int(u))])).collect()))]);
        if let Some(url) = &self.source.url { fields.push(("url", s(url))); }
        if let Some(class) = &self.assembly_class { fields.push(("assembly_class", s(class))); }
        obj(fields)
    }
}

impl PriceKey {
    pub fn label(&self) -> String {
        match self { Self::Mpn(mpn) => mpn.clone(), Self::Supplier { supplier, part } => format!("{supplier} · {part}") }
    }
}

impl Entry {
    pub fn break_label(&self) -> String {
        self.breaks.iter().map(|(q, micros)| format!("{q}={}.{:06}", micros / 1_000_000, micros % 1_000_000))
            .collect::<Vec<_>>().join(", ")
    }
}

/// Each optional user figure keeps the date on which that particular setting changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setting {
    pub value: String,
    pub date: String,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub hand: bool,
    pub spares: Setting,
    pub flat: Setting,
    pub exchange: Setting,
}
impl Settings {
    pub fn options(&self) -> Result<crate::cost::Options, String> {
        let mut out = crate::cost::Options { hand: self.hand, ..Default::default() };
        let source = |s: &Setting| {
            if !valid_date(&s.date) { return Err("enter a valid date for the user setting".to_owned()); }
            Ok(Source { name: "User entry".into(), date: s.date.clone(), url: None })
        };
        let code = |s: &str| {
            if s.len() == 3 && s.bytes().all(|c| c.is_ascii_uppercase()) { Ok(s.to_string()) }
            else { Err("use a three-letter uppercase currency code".to_string()) }
        };
        if !self.flat.value.trim().is_empty() {
            let words = self.flat.value.split_whitespace().collect::<Vec<_>>();
            let [currency, amount] = words.as_slice() else { return Err("enter the flat amount as USD 5.00".into()); };
            out.extra = Some((Money { currency: code(currency)?, micros: millionths(amount)? }, source(&self.flat)?));
        }
        if !self.exchange.value.trim().is_empty() {
            let words = self.exchange.value.split_whitespace().collect::<Vec<_>>();
            let [from, ratio, to] = words.as_slice() else { return Err("enter the exchange rate as EUR 1.10 USD".into()); };
            let rate = millionths(ratio)?;
            if rate == 0 || from == to { return Err("use a positive rate and two different currencies".into()); }
            out.exchange = Some(crate::cost::Exchange { from: code(from)?, to: code(to)?, rate, source: source(&self.exchange)? });
        }
        if !self.spares.value.trim().is_empty() {
            let words = self.spares.value.split_whitespace().collect::<Vec<_>>();
            let [percent, min] = words.as_slice() else { return Err("enter hand spares as percentage and minimum count, e.g. 2% 1".into()); };
            let fraction = millionths(percent.trim_end_matches('%'))?;
            if fraction % 100 != 0 { return Err("use up to four decimal places for the percentage".into()); }
            let minimum = min.parse::<i64>().ok().filter(|n| *n >= 0).ok_or("minimum spares must be a nonnegative whole number")?;
            out.hand_attrition = Some((crate::rates::Attrition { ppm: fraction / 100, minimum }, source(&self.spares)?));
        }
        Ok(out)
    }
    pub fn dated(mut self, previous: &Self, date: &str) -> Self {
        for (now, old) in [(&mut self.flat,&previous.flat),(&mut self.exchange,&previous.exchange),(&mut self.spares,&previous.spares)] {
            now.date = if now.value == old.value { old.date.clone() } else { date.to_owned() };
        }
        self
    }
    fn json(&self) -> Value {
        let item = |v:&Setting| obj(vec![("value",s(&v.value)),("date",s(&v.date))]);
        obj(vec![("hand", Value::Bool(self.hand)),("spares",item(&self.spares)),("flat",item(&self.flat)),("exchange",item(&self.exchange))])
    }
    fn parse(v:&Value) -> Result<Self,String> {
        let read = |key| {
            let item = v.get(key).ok_or_else(||format!("missing setting {key}"))?;
            let field = |k| item.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(||format!("invalid setting {key}.{k}"));
            Ok::<_,String>(Setting {value:field("value")?,date:field("date")?})
        };
        let hand = match v.get("hand") { Some(Value::Bool(b)) => *b, _ => return Err("invalid assembly setting".into()) };
        let settings = Self {hand, spares:read("spares")?,flat:read("flat")?,exchange:read("exchange")?};
        settings.options()?;
        Ok(settings)
    }
}

/// Entries and confirmed CSV mappings form one atomic library revision on disk.
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub prices: Prices,
    pub settings: Settings,
    pub mappings: BTreeMap<Vec<String>, Vec<String>>,
}

impl Library {
    pub fn from_text(input: &str) -> Result<Self,String> {
        let v = makepad_strict_json::parse(input.as_bytes())?;
        let mut library = Self::default();
        for item in array(&v,"entries")? {
            let entry = Entry::from_json(item)?;
            if library.prices.0.insert(entry.key.clone(),entry).is_some() { return Err("duplicate price key in library".into()); }
        }
        let strings = |v:&Value,k| array(v,k)?.iter().map(|s| s.as_str().map(str::to_string)
            .ok_or_else(|| format!("{k} must contain text"))).collect::<Result<Vec<_>,String>>();
        if v.get("mappings").is_some() {
            for row in array(&v,"mappings")? {
                library.mappings.insert(strings(row,"header")?,strings(row,"columns")?);
            }
        }
        if let Some(v) = v.get("settings") { library.settings = Settings::parse(v)?; }
        Ok(library)
    }
    pub fn to_text(&self) -> String {
        let strings = |items:&[String]| Value::Arr(items.iter().map(s).collect());
        obj(vec![("settings",self.settings.json()),("entries",Value::Arr(self.prices.0.values().map(Entry::json).collect())),
            ("mappings",Value::Arr(self.mappings.iter().map(|(h,c)| obj(vec![("header",strings(h)),("columns",strings(c))])).collect()))]).to_json()
    }
}

pub fn breaks_from_text(input: &str) -> Result<BTreeMap<i64,i64>,String> {
    let mut result = BTreeMap::new();
    let mut put = |min, price| {
        if min < 1 || price < 0 || result.insert(min,price).is_some() {
            Err("breaks need distinct positive quantities and nonnegative unit prices".to_string())
        } else { Ok(()) }
    };
    if input.trim_start().starts_with('[') {
        // A JSON list of breaks, each a flat object with integer min and micros fields. The
        // objects may be separated by commas, by spaces, or by both.
        let body = input.trim().strip_prefix('[').and_then(|s| s.strip_suffix(']')).ok_or("expected a list of breaks")?;
        let mut tail = body.trim();
        while !tail.is_empty() {
            let end = tail.find('}').ok_or("expected a break object with min and micros")? + 1;
            let row = makepad_strict_json::parse(tail[..end].as_bytes())?;
            put(number(&row,"min")?,number(&row,"micros")?)?;
            tail=tail[end..].trim_start();
            if let Some(rest) = tail.strip_prefix(',') { tail=rest.trim_start(); }
        }
    } else {
        for item in input.split(',') {
            let (q,p) = item.split_once('=').ok_or("enter breaks as quantity=unit price, for example 1=0.80, 10=0.70")?;
            put(q.trim().parse().map_err(|_| "invalid break quantity")?,millionths(p)?)?;
        }
    }
    if result.is_empty() { return Err("enter at least one price break".into()); }
    Ok(result)
}
