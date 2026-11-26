use crate::{rates::Money, BoardFacts, Rates};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Figure {
    pub micros: i128,
    pub currency: String,
    pub source: String,
    pub date: String,
}

impl Figure {
    pub fn display(&self) -> String {
        let fraction = format!("{:06}", self.micros % 1_000_000);
        let digits = fraction.trim_end_matches('0').len().max(2);
        format!("{} {}.{}", self.currency, self.micros / 1_000_000, &fraction[..digits])
    }
}

#[derive(Clone, Debug)]
pub struct Run {
    pub boards: i64,
    pub setup: Figure,
    pub each: Figure,
    pub surcharges: Vec<(String, Figure)>,
    pub totals: Vec<Figure>,
    /// Subtotals divided by the board count, rounded to the nearest millionth.
    pub per_board: Vec<Figure>,
}

#[derive(Clone, Debug)]
pub struct Estimate {
    pub header: &'static str,
    pub excludes: &'static str,
    pub selected: Result<Run, String>,
    pub comparisons: Vec<(i64, Result<Run, String>)>,
}

pub fn estimate_board(facts: &BoardFacts, rates: &Rates, boards: i64) -> Estimate {
    let run = |boards: i64| {
        if boards <= 0 { return Err("the board quantity must be positive".into()); }
        if facts.area2_nm2 <= 0 { return Err("the board has no outline area".into()); }
        let bracket = rates.prices.iter().filter(|p| p.layers == i64::from(facts.layers)
            && facts.area2_nm2 <= i128::from(p.max_mm2) * 2_000_000_000_000)
            .map(|p| p.max_mm2).min();
        let price = rates.prices.iter().filter(|p| p.layers == i64::from(facts.layers)
            && Some(p.max_mm2) == bracket && p.min_boards <= boards).max_by_key(|p| p.min_boards)
            .ok_or_else(|| format!("{} has no rate for this board at quantity {boards}", rates.name))?;
        let figure = |currency: &str, micros| Figure { micros, currency: currency.into(), source: rates.name.clone(), date: rates.date.clone() };
        let amount = |m: &Money| figure(&m.currency, i128::from(m.micros));
        let setup = amount(&rates.setup);
        let each = amount(&price.each);
        let mut sums = BTreeMap::<String, i128>::new();
        sums.insert(each.currency.clone(), each.micros * i128::from(boards));
        *sums.entry(setup.currency.clone()).or_default() += setup.micros;
        let mut surcharges = Vec::new();
        for rule in rates.surcharges.iter().filter(|r| r.when.matches(facts)) {
            let fee = amount(&rule.fee);
            *sums.entry(fee.currency.clone()).or_default() += fee.micros;
            surcharges.push((rule.name.clone(), fee));
        }
        Ok(Run { boards, setup, each, surcharges,
            totals: sums.iter().map(|(c, n)| figure(c, *n)).collect(),
            per_board: sums.iter().map(|(c, n)| figure(c, (n + i128::from(boards / 2)) / i128::from(boards))).collect() })
    };
    Estimate {
        header: "Estimate, not a quote",
        excludes: "Board fabrication only. Parts and assembly are not estimated. Shipping, duty and tax are excluded.",
        selected: run(boards),
        comparisons: [1, 10, 50].into_iter().filter(|q| *q != boards).map(|q| (q, run(q))).collect(),
    }
}

use crate::prices::{Entry, Prices, Source, Sourcing};
use crate::rates::{AssemblyRates, Attrition, Billing};
use crate::sheet::{BomSheet, Member};
use fab_elec_core::Side;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Keyed by the component path, counted once from the placed footprints.
pub type Placements = BTreeMap<String, (Side, usize)>;

#[derive(Clone, Debug)]
pub struct Exchange {
    pub from: String,
    pub to: String,
    /// Target currency millionths per one unit of the source currency.
    pub rate: i64,
    pub source: Source,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub hand: bool,
    pub hand_attrition: Option<(Attrition, Source)>,
    pub extra: Option<(Money, Source)>,
    pub exchange: Option<Exchange>,
}

/// A calculated amount retains every input's provenance, including optional conversion.
#[derive(Clone, Debug, Default)]
pub struct Amount {
    pub micros: i128,
    pub currency: String,
    pub sources: BTreeSet<Source>,
}

impl Amount {
    fn new(n: i128, currency: &str, source: &Source) -> Self {
        Self { micros: n, currency: currency.into(), sources: [source.clone()].into() }
    }
    pub fn display(&self) -> String {
        format!("{} {}", self.currency, decimal(self.micros))
    }
    fn times(&self, n: i128) -> Result<Self,String> {
        Ok(Self { micros: mul(self.micros,n)?, ..self.clone() })
    }
}

fn decimal(micros: i128) -> String {
    let fraction = format!("{:06}", micros % 1_000_000);
    format!("{}.{}", micros / 1_000_000, &fraction[..fraction.trim_end_matches('0').len().max(2)])
}

fn mul(a:i128,b:i128) -> Result<i128,String> {
    a.checked_mul(b).ok_or_else(|| "the estimate exceeds integer millionths".into())
}
fn add(a:i128,b:i128) -> Result<i128,String> {
    a.checked_add(b).ok_or_else(|| "the estimate exceeds integer millionths".into())
}
fn ceil_div(n:i128,d:i128) -> i128 { n/d + i128::from(n%d != 0) }
fn nearest(n:i128,d:i128) -> i128 { n/d + i128::from(n%d >= ceil_div(d,2)) }

#[derive(Clone, Debug)]
pub struct Purchase {
    pub needed: i128,
    pub spares: Option<i128>,
    pub ordered: Option<i128>,
    pub unit: Option<Amount>,
    pub bought: Option<Amount>,
    pub used_per_board: Option<Amount>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LineCost {
    pub key: Arc<str>,
    pub refs: Arc<Vec<String>>,
    pub fitted: Option<Purchase>,
    /// Alternate membership key and slot, exactly as on the sheet.
    pub alternates: Vec<(String, i64, Purchase)>,
}

#[derive(Clone, Debug)]
pub struct Assembly {
    pub charges: Vec<(String, Amount)>,
    pub total: Amount,
    pub assumptions: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct CostRun {
    pub boards: i64,
    pub lines: Vec<LineCost>,
    pub dnp: Arc<Vec<String>>,
    pub priced: usize,
    pub fitted: usize,
    pub parts: Vec<Amount>,
    pub fabrication: Result<Run,String>,
    pub assembly: Result<Assembly,String>,
    pub extra: Option<Amount>,
    pub exchange: Option<Exchange>,
    pub totals: Vec<Amount>,
    pub per_board: Vec<Amount>,
    pub lower_bound: bool,
}

#[derive(Clone, Debug)]
pub struct WholeEstimate {
    pub header: &'static str,
    pub excludes: &'static str,
    pub selected: Result<CostRun,String>,
    pub comparisons: Vec<(i64,Result<CostRun,String>)>,
}

/// Member identity, fitted offers, package classes and footprint counts do not change with quantity.
pub struct Prepared {
    lines: Vec<PreparedLine>,
    dnp: Arc<Vec<String>>,
    work: AssemblyWork,
}
struct PreparedLine {
    key: Arc<str>,
    refs: Arc<Vec<String>>,
    fitted: Option<OrderLine>,
    alternates: Vec<(String,i64,OrderLine)>,
}
struct OrderLine {
    count: i128,
    entry: Option<Entry>,
    different: bool,
    attrition: Option<Attrition>,
}
impl OrderLine {
    fn new(members: &[&Member], entry: Option<&Entry>, different: bool, rates: &Rates, options: &Options) -> Self {
        let rule = |member: &Member| if options.hand { options.hand_attrition.as_ref().map(|(r,_)| r) }
            else { rates.assembly.as_ref()?.attrition(&member.footprint) };
        let attrition = rule(members[0]).filter(|&r| members.iter().all(|m| rule(m) == Some(r))).cloned();
        Self { count: members.len() as i128, entry: entry.cloned(), different, attrition }
    }
}
impl Prepared {
    pub fn new(sheet: &BomSheet, placements: &Placements, prices: &Prices, rates: &Rates, options: &Options) -> Self {
        let mut work = AssemblyWork::default();
        let mut dnp = Vec::new();
        let mut lines = Vec::with_capacity(sheet.rows.len());
        for row in &sheet.rows {
            let members: Vec<_> = row.members.iter().filter(|m| !m.dnp).collect();
            dnp.extend(row.members.iter().filter(|m| m.dnp).map(|m| m.reference.clone()));
            let mut prepared = PreparedLine { key: row.key.as_str().into(), refs: Arc::new(row.refs.clone()), fitted: None, alternates: vec![] };
            if let Some(first) = members.first() {
                work.count(&members,placements,prices);
                let first = prices.resolve(&first.part);
                let different = members.iter().any(|m| prices.resolve(&m.part).map(|e| &e.key) != first.map(|e| &e.key));
                prepared.fitted = Some(OrderLine::new(&members,first,different,rates,options));
                let by_path: BTreeMap<_,_> = members.iter().map(|&m| (m.path.as_str(),m)).collect();
                for alt in &row.alternates {
                    let (_, paths) = crate::sheet::line_members(&alt.key).expect("a sheet alternate carries a line key");
                    let applicable: Vec<_> = paths.iter().filter_map(|p| by_path.get(p.as_str()).copied()).collect();
                    if applicable.is_empty() { continue; }
                    let part = Sourcing { mpn:alt.mpn.clone(),supplier:alt.supplier.clone(),supplier_part:alt.supplier_part.clone() };
                    let order = OrderLine::new(&applicable,prices.resolve(&part),false,rates,options);
                    prepared.alternates.push((alt.key.clone(),alt.number,order));
                }
            }
            lines.push(prepared);
        }
        // A footprint deliberately left out of the factory BOM can still be fitted by the assembler.
        let skipped: BTreeSet<_> = sheet.rows.iter().flat_map(|r| &r.members).filter(|m| m.dnp).map(|m| m.path.as_str()).collect();
        for (path, &(side, joints)) in placements {
            if skipped.contains(path.as_str()) { continue; }
            work.joints += joints as i128;
            work.placements += 1;
            work.top |= side == Side::Top;
            work.bottom |= side == Side::Bottom;
        }
        Self { lines, work, dnp:Arc::new(dnp) }
    }

    pub fn estimate(&self, facts: &BoardFacts, rates: &Rates, boards: i64, options: &Options) -> WholeEstimate {
        let board = estimate_board(facts,rates,boards);
        let source = Source { name: rates.name.clone(), date: rates.date.clone(), url: None };
        let run = |q,f,detailed| full_run(self,rates.assembly.as_ref(),&source,options,q,f,detailed);
        WholeEstimate { header:board.header, excludes:"Shipping, duty and tax are excluded apart from any entered flat amount.",
            selected:run(boards,board.selected,true), comparisons:board.comparisons.into_iter().map(|(q,f)| (q,run(q,f,false))).collect() }
    }
}

pub fn estimate(sheet: &BomSheet, facts: &BoardFacts, placements: &Placements, prices: &Prices,
    rates: &Rates, boards: i64, options: &Options) -> WholeEstimate {
    Prepared::new(sheet,placements,prices,rates,options).estimate(facts,rates,boards,options)
}

fn full_run(prepared: &Prepared, schedule: Option<&AssemblyRates>, source: &Source, options: &Options,
    boards: i64, fabrication: Result<Run,String>, detailed:bool) -> Result<CostRun,String> {
    if boards <= 0 { return Err("the board quantity must be positive".into()); }
    let mut lines = Vec::new();
    let mut parts = BTreeMap::new();
    let mut priced = 0;
    let mut fitted = 0;
    let mut lower_bound = fabrication.is_err();
    for row in &prepared.lines {
        let mut line = LineCost { key:row.key.clone(), refs:row.refs.clone(), fitted:None, alternates:vec![] };
        if let Some(selection) = &row.fitted {
            fitted += 1;
            let order = purchase(selection, boards, source, options)?;
            if let Some(bought) = &order.bought {
                priced += 1;
                accumulate(&mut parts,bought)?;
            } else { lower_bound = true; }
            if detailed {
                line.fitted = Some(order);
                for (key,slot,selection) in &row.alternates {
                    line.alternates.push((key.clone(),*slot,purchase(selection,boards,source,options)?));
                }
            }
        }
        if detailed { lines.push(line); }
    }
    let parts: Vec<_> = parts.into_values().collect();
    let assembly = if options.hand { Err("Hand assembly selected".into()) }
        else if let Some(s) = schedule { Ok(prepared.work.charge(s,source,boards)?) }
        else { lower_bound = true; Err("Assembly not estimated: this profile has no assembly schedule".into()) };
    let mut totals = BTreeMap::new();
    for p in &parts { accumulate(&mut totals,p)?; }
    if let Ok(fab) = &fabrication {
        for f in &fab.totals { accumulate(&mut totals,&Amount::new(f.micros,&f.currency,source))?; }
    }
    if let Ok(a) = &assembly { accumulate(&mut totals,&a.total)?; }
    let extra = options.extra.as_ref().map(|(m,s)| Amount::new(m.micros.into(),&m.currency,s));
    if let Some(a) = &extra { accumulate(&mut totals,a)?; }
    let mut used_exchange = None;
    if let Some(exchange) = &options.exchange {
        if exchange.rate <= 0 || exchange.from == exchange.to { return Err("enter a positive exchange rate between distinct currencies".into()); }
        if let Some(mut a) = totals.remove(&exchange.from) {
            used_exchange = Some(exchange.clone());
            a.micros = nearest(mul(a.micros,exchange.rate.into())?,1_000_000);
            a.currency.clone_from(&exchange.to);
            a.sources.insert(exchange.source.clone());
            accumulate(&mut totals,&a)?;
        }
    }
    let totals: Vec<_> = totals.into_values().collect();
    let per_board = totals.iter().map(|a| Amount { micros: nearest(a.micros,boards.into()), ..a.clone() }).collect();
    Ok(CostRun { boards,lines,dnp:prepared.dnp.clone(),exchange:used_exchange,priced,fitted,parts,fabrication,assembly,extra,totals,per_board,lower_bound })
}
fn accumulate(sums: &mut BTreeMap<String,Amount>, amount: &Amount) -> Result<(),String> {
    let sum = sums.entry(amount.currency.clone()).or_insert_with(|| Amount { currency: amount.currency.clone(), ..Default::default() });
    sum.micros = add(sum.micros,amount.micros)?;
    sum.sources.extend(amount.sources.iter().cloned());
    Ok(())
}

fn purchase(line:&OrderLine, boards:i64, source:&Source, options:&Options) -> Result<Purchase,String> {
    let quantity = line.count;
    let needed = mul(quantity,boards.into())?;
    let attrition = &line.attrition;
    let mut out = Purchase { needed,spares:None,ordered:None,unit:None,bought:None,used_per_board:None,notes:Vec::new() };
    if let Some(r) = attrition { out.spares = Some(ceil_div(mul(needed,r.ppm.into())?,1_000_000).max(r.minimum.into())); }
    else if options.hand { out.spares = Some(0); }
    else { out.notes.push("Attrition unknown for this line.".into()); }
    if line.different {
        out.notes.push("Unpriced: these members buy as different parts. Split the grouping.".into());
        return Ok(out);
    }
    let Some(e) = &line.entry else { out.notes.push("Unpriced: no library price.".into()); return Ok(out) };
    let Some(spares) = out.spares else {
        // Without an order quantity no break or purchase total is known.
        out.notes.push("Unpriced until attrition is known.".into());
        return Ok(out);
    };
    let pack = i128::from(e.multiple);
    let orderable = |n| mul(ceil_div(n,pack),pack);
    let mut order = orderable(add(needed,spares)?.max(e.moq.into()))?;
    let applicable = |q:i128| e.breaks.range(..=i64::try_from(q).unwrap_or(i64::MAX)).next_back().map(|(_,p)| *p);
    let Some(mut price) = applicable(order) else {
        out.ordered = Some(order); out.notes.push("Unpriced: no price break applies at this quantity.".into()); return Ok(out);
    };
    let mut total = mul(order,price.into())?;
    if let Ok(q) = i64::try_from(order) {
        use std::ops::Bound::{Excluded,Unbounded};
        if let Some((&next,_)) = e.breaks.range((Excluded(q),Unbounded)).next() {
            let larger = orderable(next.into())?;
            let new_price = applicable(larger).expect("the candidate reaches a published break");
            let cheaper = mul(larger,new_price.into())?;
            if cheaper < total {
                out.notes.push(format!("Buying {larger} at the next break costs less than buying {order}."));
                order=larger; price=new_price; total=cheaper;
            }
        }
    }
    let unit = Amount::new(price.into(), &e.currency, &e.source);
    let mut bought = Amount::new(total, &e.currency, &e.source);
    if attrition.is_some() {
        bought.sources.insert(if options.hand { options.hand_attrition.as_ref().unwrap().1.clone() } else { source.clone() });
    }
    out.ordered = Some(order); out.used_per_board = Some(unit.times(quantity)?); out.unit=Some(unit); out.bought=Some(bought);
    Ok(out)
}

#[derive(Default)]
struct AssemblyWork {
    joints:i128, placements:i128, lines:i128, outside:i128, top:bool, bottom:bool, assumptions:Vec<String>, class_sources:BTreeSet<Source>,
}
impl AssemblyWork {
    fn count(&mut self,members:&[&Member],placements:&Placements,prices:&Prices) {
        let mut placed = false;
        let mut outside = false;
        let mut unknown = Vec::new();
        for m in members {
            if !placements.contains_key(&m.path) { continue; }
            placed = true;
            let class = m.assembly_class.as_deref().or_else(|| {
                let entry = prices.resolve(&m.part)?;
                let class = entry.assembly_class.as_deref()?;
                self.class_sources.insert(entry.source.clone());
                Some(class)
            });
            if class.is_none() { unknown.push(m.reference.as_str()); }
            outside |= class != Some("basic");
        }
        self.lines += i128::from(placed); self.outside += i128::from(outside);
        if !unknown.is_empty() { self.assumptions.push(format!("{}: unknown assembly class, assumed outside the basic library.",unknown.join(", "))); }
    }
    fn charge(&self,s:&AssemblyRates,src:&Source,boards:i64) -> Result<Assembly,String> {
        let (name,count) = match s.billing { Billing::Joint => ("Joints",self.joints), Billing::Placement => ("Placements",self.placements) };
        let mut fees = vec![("Setup",s.setup.into()), ("Stencil",s.stencil.into()), (name,mul(mul(count,boards.into())?,s.rate.into())?),
            ("Unique lines",mul(self.lines,s.per_line.into())?), ("Outside basic library",mul(self.outside,s.outside_basic.into())?)];
        if self.top && self.bottom { fees.push(("Second side",s.second_side.into())); }
        let sum = fees.iter().try_fold(0,|n,(_,f)| add(n,*f))?;
        let total = sum.max(s.minimum.into());
        if total > sum { fees.push(("Minimum charge adjustment",total-sum)); }
        let charges = fees.into_iter().map(|(name,value)| {
            let mut amount = Amount::new(value,&s.currency,src);
            if name == "Outside basic library" || name == "Minimum charge adjustment" {
                amount.sources.extend(self.class_sources.iter().cloned());
            }
            (name.into(),amount)
        }).collect();
        let mut total = Amount::new(total,&s.currency,src);
        total.sources.extend(self.class_sources.iter().cloned());
        Ok(Assembly { charges,total,assumptions:self.assumptions.clone() })
    }
}
