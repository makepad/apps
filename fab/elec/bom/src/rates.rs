use crate::BoardFacts;
use fab_elec_board::Finish;
use makepad_strict_json::{obj, s, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Money {
    pub micros: i64,
    pub currency: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Price {
    pub layers: i64,
    pub max_mm2: i64,
    pub min_boards: i64,
    pub each: Money,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    Finish(Finish), Mask(u32), ThicknessAbove(i64), DrillBelow(i64), TrackBelow(i64), GapBelow(i64),
    HolesAbove(i64),
}

impl Condition {
    pub fn matches(&self, facts: &BoardFacts) -> bool {
        match *self {
            Self::Finish(v) => facts.finish == v,
            Self::Mask(v) => facts.mask_color == v,
            Self::ThicknessAbove(v) => i64::from(facts.thickness) > v,
            Self::DrillBelow(v) => facts.smallest_drill.is_some_and(|d| i64::from(d) < v),
            Self::TrackBelow(v) => facts.smallest_track.is_some_and(|w| i64::from(w) < v),
            Self::GapBelow(v) => facts.smallest_gap.is_some_and(|g| i64::from(g) < v),
            Self::HolesAbove(v) => facts.hole_count as u64 > v as u64,
        }
    }

    fn json(&self) -> Value {
        let pair = match *self {
            Self::Finish(v) => ("finish", s(format!("{v:?}"))),
            Self::Mask(v) => ("mask_rgba", Value::Int(i64::from(v))),
            Self::ThicknessAbove(v) => ("thickness_above_nm", Value::Int(v)),
            Self::DrillBelow(v) => ("drill_below_nm", Value::Int(v)),
            Self::TrackBelow(v) => ("track_below_nm", Value::Int(v)),
            Self::GapBelow(v) => ("gap_below_nm", Value::Int(v)),
            Self::HolesAbove(v) => ("holes_above", Value::Int(v)),
        };
        obj(vec![pair])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surcharge {
    pub name: String,
    pub when: Condition,
    /// Once per run.
    pub fee: Money,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rates {
    pub name: String,
    pub date: String,
    pub setup: Money,
    pub prices: Vec<Price>,
    pub surcharges: Vec<Surcharge>,
    pub assembly: Option<AssemblyRates>,
}

pub(crate) fn member<'a>(v: &'a Value, key: &str) -> Result<&'a Value, String> {
    v.get(key).ok_or_else(|| format!("missing {key}"))
}
pub(crate) fn number(v: &Value, key: &str) -> Result<i64, String> {
    member(v, key)?.as_i64().filter(|n| *n >= 0).ok_or_else(|| format!("{key} needs a nonnegative integer"))
}
pub(crate) fn text(v: &Value, key: &str) -> Result<String, String> {
    member(v, key)?.as_str().filter(|s| !s.trim().is_empty()).map(str::to_owned).ok_or_else(|| format!("{key} needs text"))
}
pub(crate) fn array<'a>(v: &'a Value, key: &str) -> Result<&'a [Value], String> {
    member(v, key)?.as_arr().ok_or_else(|| format!("{key} needs a list"))
}
impl Money {
    pub(crate) fn currency(v: &Value) -> Result<String, String> {
        let currency = text(v, "currency")?;
        if currency.len() != 3 || !currency.bytes().all(|c| c.is_ascii_uppercase()) {
            return Err("currency needs a three-letter uppercase code".into());
        }
        Ok(currency)
    }
    fn read(v: &Value) -> Result<Self, String> {
        Ok(Self { currency: Self::currency(v)?, micros: number(v, "micros")? })
    }
    fn json(&self) -> Value {
        obj(vec![("micros", Value::Int(self.micros)), ("currency", s(&self.currency))])
    }
}

fn condition(v: &Value) -> Result<Condition, String> {
    let Value::Obj(entries) = v else { return Err("when needs a condition".into()); };
    let [(key, _)] = entries.as_slice() else { return Err("when needs exactly one condition".into()); };
    Ok(match key.as_str() {
        "finish" => {
            let name = text(v, key)?;
            let known = [Finish::None, Finish::Hasl, Finish::LeadFreeHasl, Finish::Enig, Finish::Osp,
                Finish::ImmersionSilver, Finish::ImmersionTin];
            Condition::Finish(known.into_iter().find(|f| format!("{f:?}") == name)
                .ok_or_else(|| format!("unknown finish {name}"))?)
        }
        "mask_rgba" => Condition::Mask(number(v, key)?.try_into().map_err(|_| "mask_rgba exceeds 32 bits")?),
        "thickness_above_nm" => Condition::ThicknessAbove(number(v, key)?),
        "drill_below_nm" => Condition::DrillBelow(number(v, key)?),
        "track_below_nm" => Condition::TrackBelow(number(v, key)?),
        "gap_below_nm" => Condition::GapBelow(number(v, key)?),
        "holes_above" => Condition::HolesAbove(number(v, key)?),
        _ => return Err(format!("unknown surcharge condition {key}")),
    })
}

impl Rates {
    pub fn example() -> Self {
        Self::from_json(include_str!("example.json")).expect("the bundled example rate profile")
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let v = makepad_strict_json::parse(input.as_bytes())?;
        let name = text(&v, "name")?;
        let date = text(&v, "date")?;
        let setup = Money::read(member(&v, "setup")?)?;
        let mut breaks = BTreeSet::new();
        let mut prices = Vec::new();
        for row in array(&v, "prices")? {
            let layers = number(row, "layers")?;
            let max_mm2 = number(row, "max_mm2")?;
            let min_boards = number(row, "min_boards")?;
            if layers == 0 || max_mm2 == 0 || min_boards == 0 || !breaks.insert((layers, max_mm2, min_boards)) {
                return Err("price brackets need distinct, positive layer, area and quantity keys".into());
            }
            prices.push(Price { layers, max_mm2, min_boards, each: Money::read(member(row, "each")?)? });
        }
        let mut surcharges = Vec::new();
        for rule in array(&v, "surcharges")? {
            surcharges.push(Surcharge { name: text(rule, "name")?, when: condition(member(rule, "when")?)?,
                fee: Money::read(member(rule, "fee")?)? });
        }
        let assembly = v.get("assembly").map(|v| AssemblyRates::read(v, &setup.currency)).transpose()?;
        Ok(Self { name, date, setup, prices, surcharges, assembly })
    }

    pub fn to_json(&self) -> String {
        let prices = self.prices.iter().map(|p| obj(vec![("layers", Value::Int(p.layers)),
            ("max_mm2", Value::Int(p.max_mm2)), ("min_boards", Value::Int(p.min_boards)), ("each", p.each.json())])).collect();
        let surcharges = self.surcharges.iter().map(|p| obj(vec![("name", s(&p.name)), ("when", p.when.json()), ("fee", p.fee.json())])).collect();
        let mut fields = vec![("name", s(&self.name)), ("date", s(&self.date)), ("setup", self.setup.json()),
            ("prices", Value::Arr(prices)), ("surcharges", Value::Arr(surcharges))];
        if let Some(assembly) = &self.assembly { fields.push(("assembly", assembly.json())); }
        obj(fields).to_json()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attrition {
    /// Parts per million of the needed quantity, rounded up to a whole spare.
    pub ppm: i64,
    pub minimum: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Billing { Joint, Placement }

/// All fees in millionths of the profile setup currency. The minimum applies once per run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssemblyRates {
    pub currency: String,
    pub setup: i64,
    pub stencil: i64,
    pub billing: Billing,
    pub rate: i64,
    pub per_line: i64,
    pub outside_basic: i64,
    pub second_side: i64,
    pub minimum: i64,
    pub classes: BTreeMap<String, Attrition>,
    pub prefixes: BTreeMap<String, String>,
    pub default_class: Option<String>,
}

impl AssemblyRates {
    pub fn attrition(&self, footprint: &str) -> Option<&Attrition> {
        let class = self.prefixes.iter().filter(|(p, _)| footprint.starts_with(p.as_str()))
            .max_by_key(|(p,_)| p.len()).map(|(_, class)| class).or(self.default_class.as_ref())?;
        self.classes.get(class)
    }
    fn read(v: &Value, currency: &str) -> Result<Self, String> {
        let billing = match text(v, "billing")?.as_str() {
            "joint" => Billing::Joint, "placement" => Billing::Placement, _ => return Err("billing must be joint or placement".into()),
        };
        let mut classes = BTreeMap::new();
        for class in array(v, "classes")? {
            let name = text(class, "name")?;
            let rule = Attrition { ppm: number(class, "ppm")?, minimum: number(class, "minimum")? };
            if classes.insert(name, rule).is_some() { return Err("duplicate attrition class".into()); }
        }
        let mut prefixes = BTreeMap::new();
        for row in array(v, "prefixes")? {
            let class = text(row, "class")?;
            if !classes.contains_key(&class) { return Err(format!("unknown attrition class {class}")); }
            if prefixes.insert(text(row, "prefix")?, class).is_some() { return Err("duplicate footprint prefix".into()); }
        }
        let default_class = v.get("default_class").map(|_| text(v, "default_class")).transpose()?;
        if default_class.as_ref().is_some_and(|c| !classes.contains_key(c)) { return Err("unknown default attrition class".into()); }
        Ok(Self { currency: currency.into(), setup: number(v,"setup")?, stencil: number(v,"stencil")?, billing,
            rate: number(v,"rate")?, per_line: number(v,"per_line")?, outside_basic: number(v,"outside_basic")?,
            second_side: number(v,"second_side")?, minimum: number(v,"minimum")?, classes, prefixes, default_class })
    }
    fn json(&self) -> Value {
        let mut fields = vec![("billing", s(match self.billing { Billing::Joint => "joint", Billing::Placement => "placement" })),
            ("classes", Value::Arr(self.classes.iter().map(|(name,r)| obj(vec![("name",s(name)), ("ppm",Value::Int(r.ppm)),
                ("minimum",Value::Int(r.minimum))])).collect())),
            ("prefixes", Value::Arr(self.prefixes.iter().map(|(p,c)| obj(vec![("prefix",s(p)),("class",s(c))])).collect()))];
        for (key,n) in [("setup",self.setup),("stencil",self.stencil),("rate",self.rate),("per_line",self.per_line),
            ("outside_basic",self.outside_basic),("second_side",self.second_side),("minimum",self.minimum)] { fields.push((key,Value::Int(n))); }
        if let Some(c) = &self.default_class { fields.push(("default_class",s(c))); }
        obj(fields)
    }
}
