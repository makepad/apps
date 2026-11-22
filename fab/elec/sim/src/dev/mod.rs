//! Device models, one bank per type. `build` turns a `SimCircuit`'s instance lists into banks in
//! the order their setups depend on (L before K, sources before F, H and B).

pub mod behav;
pub mod bjt;
pub mod boundary;
pub mod diode;
pub mod cell;
pub mod gpio;
pub mod jfet;
pub mod junction;
pub mod linear;
pub mod mos;
pub mod reg;
pub mod source;
pub mod sw;
pub mod tline;

use crate::circuit::{BKind, ModelKind, SimCircuit};
use crate::stamp::Bank;

pub fn build(c: &SimCircuit) -> Result<Vec<Box<dyn Bank>>, String> {
    let d = &c.devices;
    let mut banks: Vec<Box<dyn Bank>> = Vec::new();
    if !d.r.is_empty() {
        banks.push(Box::new(linear::RBank::new(d.r.clone())));
    }
    if !d.c.is_empty() {
        banks.push(Box::new(linear::CBank::new(d.c.clone())));
    }
    if !d.l.is_empty() {
        banks.push(Box::new(linear::LBank::new(d.l.clone())));
    }
    if !d.k.is_empty() {
        let mut lv = Vec::new();
        for k in &d.k {
            let find = |n: &str| {
                if d.b.iter().any(|b| b.kind == BKind::L && b.name == n) {
                    return Err(format!("{}: {n} is a behavioural inductor, which K does not couple", k.name));
                }
                d.l.iter().find(|l| l.name == n).map(|l| l.l).ok_or_else(|| format!("{}: no inductor named {}", k.name, n))
            };
            lv.push((find(&k.l1)?, find(&k.l2)?));
        }
        banks.push(Box::new(linear::KBank::new(d.k.clone(), lv)));
    }
    if !d.v.is_empty() {
        banks.push(Box::new(source::VBank::new(d.v.clone())));
    }
    if !d.i.is_empty() {
        banks.push(Box::new(source::IBank::new(d.i.clone())));
    }
    if !d.boundary.is_empty() {
        banks.push(Box::new(boundary::BoundaryBank::new(d.boundary.clone())));
    }
    if !d.ctl.is_empty() {
        banks.push(Box::new(source::CtlBank::new(d.ctl.clone())));
    }
    if !d.b.is_empty() {
        let mut bank = behav::BBank::new(d.b.clone());
        bank.bypass = c.opts.bypass;
        banks.push(Box::new(bank));
    }
    if !d.sw.is_empty() {
        let mut models = Vec::new();
        for s in &d.sw {
            let kind = if s.ctrl.is_some() { ModelKind::Csw } else { ModelKind::Sw };
            let m = model_of(c, &s.name, &s.model, &[kind])?;
            models.push(sw::SwModel::from_card(m));
        }
        banks.push(Box::new(sw::SwBank::new(d.sw.clone(), models)));
    }
    if !d.gpio.is_empty() {
        banks.push(Box::new(gpio::GpioBank::new(d.gpio.clone())));
    }
    if !d.cell.is_empty() {
        let mut models = Vec::new();
        for x in &d.cell {
            let down = sw::SwModel::from_card(model_of(c, &x.name, &x.down.1, &[ModelKind::Sw])?);
            let up = sw::SwModel::from_card(model_of(c, &x.name, &x.up.1, &[ModelKind::Sw])?);
            if up.vt != -down.vt || up.vh != down.vh || up.w != down.w {
                return Err(format!("{}: its switches' thresholds do not mirror each other", x.name));
            }
            let g = |m: &sw::SwModel| (m.ln_gon.exp(), m.ln_goff.exp());
            let (rise, fall) = cell::commits(&down);
            models.push(cell::CellModel { rise, fall, down: g(&down), up: g(&up) });
        }
        banks.push(Box::new(cell::CellBank::new(d.cell.clone(), models)));
    }
    if !d.d.is_empty() {
        let mut models = Vec::new();
        for x in &d.d {
            models.push(diode::DiodeModel::from_card(model_of(c, &x.name, &x.model, &[ModelKind::Diode])?));
        }
        let mut bank = diode::DiodeBank::new(d.d.clone(), models);
        bank.bypass = c.opts.bypass;
        banks.push(Box::new(bank));
    }
    if !d.q.is_empty() {
        let mut models = Vec::new();
        for x in &d.q {
            models.push(bjt::BjtModel::from_card(model_of(c, &x.name, &x.model, &[ModelKind::Npn, ModelKind::Pnp])?));
        }
        let mut bank = bjt::BjtBank::new(d.q.clone(), models);
        bank.bypass = c.opts.bypass;
        banks.push(Box::new(bank));
    }
    if !d.j.is_empty() {
        let mut models = Vec::new();
        for x in &d.j {
            models.push(jfet::JfetModel::from_card(model_of(c, &x.name, &x.model, &[ModelKind::Njf, ModelKind::Pjf])?));
        }
        banks.push(Box::new(jfet::JfetBank::new(d.j.clone(), models)));
    }
    if !d.m.is_empty() {
        let mut models = Vec::new();
        for x in &d.m {
            models.push(mos::MosModel::from_card(model_of(c, &x.name, &x.model, &[ModelKind::Nmos, ModelKind::Pmos])?));
        }
        banks.push(Box::new(mos::MosBank::new(d.m.clone(), models)));
    }
    if !d.o.is_empty() {
        let mut lines = d.o.clone();
        for o in &mut lines {
            if let Some(name) = &o.model {
                let m = model_of(c, &o.name, name, &[ModelKind::Ltra])?;
                (o.r, o.l, o.g, o.c, o.len) = (m.or("r", 0.0), m.or("l", 0.0), m.or("g", 0.0), m.or("c", 0.0), m.or("len", 0.0));
            }
        }
        banks.push(Box::new(tline::TBank::new(lines)?));
    }
    if !d.reg.is_empty() {
        let mut models = Vec::new();
        for x in &d.reg {
            models.push(reg::RegModel::from_card(model_of(c, &x.name, &x.model, &[ModelKind::Reg])?));
        }
        banks.push(Box::new(reg::RegBank::new(d.reg.clone(), models)));
    }
    Ok(banks)
}

pub(crate) fn model_of<'a>(c: &'a SimCircuit, inst: &str, name: &str, kinds: &[ModelKind]) -> Result<&'a crate::circuit::ModelCard, String> {
    let m = c.find_model(name).ok_or_else(|| format!("{inst}: no model named {name}"))?;
    if !kinds.contains(&m.kind) {
        return Err(format!("{inst}: model {name} is a {:?}, not a {:?}", m.kind, kinds));
    }
    Ok(m)
}
