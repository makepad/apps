//! A part's mass and balance for the person: the Part panel's Mass and balance row and the view's
//! centre-of-mass marker. Both read `fab.measure {what: "mass", outline: true}` on the model
//! worker, the read the AI makes. The marker shows while the row is open on the selected part,
//! or once an AI read points at a part, until the person's next line, a model change or the row
//! closing; never otherwise.
//! On demand only: one O(triangles) read per opening, per model change while open, per AI read.

use super::*;
use fab::params::{MassFigures, MassShown};
use makepad_model::json::{self, Value};
use makepad_script::syntax::Node;

impl App {
    /// The Mass and balance row opened on `object` (weigh it) or closed.
    pub(super) fn weigh(&mut self, cx: &mut Cx, object: &str, open: bool) {
        if open {
            let same = self.state.params.mass.take().filter(|m| m.object == object);
            let figures = same.as_ref().and_then(|m| m.figures.clone());
            let ai = same.is_some_and(|m| m.ai);
            self.state.params.mass = Some(MassShown { object: object.into(), panel: true, ai, figures });
            self.ask_mass(object, true);
        } else {
            // closing the row puts the marker away, the AI's pointing too
            self.state.params.mass = None;
        }
        fab::ui::redraw(cx, &[ids!(tool_overlay)]);
    }

    fn ask_mass(&mut self, object: &str, person: bool) {
        if let Some(host) = &mut self.host {
            let args = json::obj(vec![("what", json::s("mass")), ("object", json::s(object)), ("outline", Value::Bool(true))]);
            self.mass_call = Some(if person { host.call_as_person("measure", args) } else { host.call("measure", args) });
        }
    }

    /// The person sent their next line: the AI's pointing ends, with its reads still in flight
    /// (the open row's own marker stays).
    pub(super) fn mass_turn_started(&mut self, cx: &mut Cx) {
        self.mass_ai_calls.clear();
        if let Some(m) = self.state.params.mass.as_mut().filter(|m| m.ai) {
            m.ai = false;
            if !m.panel {
                self.state.params.mass = None;
            }
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        }
    }

    pub(super) fn mass_done(&mut self, cx: &mut Cx, done: &model_host::Done) {
        let parsed = || makepad_script::syntax::parse_value(&done.text).ok();
        if let Some(at) = self.mass_ai_calls.iter().position(|c| *c == done.id) {
            self.mass_ai_calls.remove(at);
            // the part it weighed: the selected one among them, else its first
            let weighed: Vec<String> = parsed().filter(|_| done.ok).and_then(|v| {
                Some(v.field("parts")?.value.items()?.iter().filter_map(|p| Some(p.field("solid")?.value.string()?.to_string())).collect())
            }).unwrap_or_default();
            let selected = fab::ui::parts::active_part(&self.state).and_then(|i| self.state.plate.parts.get(i)).map(|p| p.object.clone());
            if let Some(object) = selected.filter(|s| weighed.contains(s)).or_else(|| weighed.first().cloned()) {
                let panel = self.state.params.mass.as_ref().is_some_and(|m| m.panel && m.object == object);
                self.state.params.mass = Some(MassShown { object: object.clone(), panel, ai: true, figures: None });
                self.ask_mass(&object, false);
            }
        }
        if self.mass_call == Some(done.id) {
            self.mass_call = None;
            let figures = parsed().filter(|_| done.ok).and_then(|v| figures(&self.state, &v));
            if let (Some(m), Some((object, figures))) = (self.state.params.mass.as_mut(), figures) {
                if m.object == object {
                    m.figures = Some(figures);
                }
            } else if let Some(m) = self.state.params.mass.as_mut().filter(|m| m.figures.is_none()) {
                // a part that cannot be weighed says so on its row
                let why = parsed().and_then(|v| v.field("error").and_then(|f| f.value.string().map(str::to_string))).unwrap_or_else(|| done.text.clone());
                m.figures = Some(MassFigures { rows: vec![("Not weighed".into(), why)], ..Default::default() });
            }
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        } else if done.model_changed {
            // what was weighed is gone: the open row weighs the part again, an AI's pointing ends
            if let Some(m) = self.state.params.mass.as_mut() {
                m.ai = false;
                if m.panel {
                    let object = m.object.clone();
                    self.ask_mass(&object, false);
                } else {
                    self.state.params.mass = None;
                }
                fab::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
        }
    }
}

/// The first weighed part's figures, from model mm to the view's printer mm, and its rows.
fn figures(state: &fab::api::AppState, v: &Node) -> Option<(String, MassFigures)> {
    let part = v.field("parts")?.value.items()?.first()?;
    let object = part.field("solid")?.value.string()?.to_string();
    let get = |n: &Node, k: &str| n.field(k).map(|f| f.value.clone());
    let num = |n: &Node, k: &str| get(n, k).and_then(|v| v.number());
    // lengths parse to metres, angles to radians
    let mm = |n: &Node, k: &str| num(n, k).map(|x| x * 1e3);
    let point = |n: &Node| -> Option<Vec<f64>> { n.items()?.iter().map(|x| x.number().map(|x| x * 1e3)).collect() };
    let com = point(&get(part, "centre_of_mass")?)?;
    let base = get(part, "base")?;
    let y = mm(&base, "y")?;
    let stands = get(&base, "stands").and_then(|v| v.ident().map(|s| s == "true")).unwrap_or(false);
    let outline: Vec<Vec<f64>> = get(&base, "outline").and_then(|v| v.items().map(|ps| ps.iter().filter_map(point).collect())).unwrap_or_default();
    let on = state.plate.parts.iter().find(|p| p.object == object)?;
    let to_plate = fab::tools::annotate::invert(&on.model_from_printer);
    let plate = |p: [f64; 3]| fab::tools::annotate::apply(&to_plate, p);
    let fill = get(part, "fill");
    let text = |n: Option<&Node>, k: &str| n.and_then(|n| get(n, k)).and_then(|v| v.string().map(str::to_string)).unwrap_or_default();
    let grams = num(part, "grams")?;
    let mut weight = match grams < 10.0 {
        true => format!("{grams:.1} g of {}", text(Some(part), "material")),
        false => format!("{grams:.0} g of {}", text(Some(part), "material")),
    };
    if let Some(fill) = &fill {
        weight.push_str(&format!(" · {} infill, {} walls", text(Some(fill), "infill"), num(fill, "walls").unwrap_or(0.0)));
        if !text(Some(fill), "from").starts_with("the job") {
            weight.push_str(" (Fab's defaults)");
        }
    }
    let height = mm(&base, "centre_height").unwrap_or(com[1] - y);
    let footing = if stands {
        format!("Yes: it tips over past {:.0}°", num(&base, "tip_angle").unwrap_or(0.0).to_degrees())
    } else if num(&base, "area_mm2").unwrap_or(0.0) <= 0.0 {
        "No: it rests on a point or an edge".to_string()
    } else {
        format!("No: its centre is {:.1} mm past its base", -mm(&base, "margin").unwrap_or(0.0))
    };
    let inertia = get(part, "inertia_g_mm2").and_then(|i| get(&i, "principal")).and_then(|p| p.items().map(|xs| {
        xs.iter().filter_map(Node::number).map(|x| format!("{:.1}", x / 100.0)).collect::<Vec<_>>().join(" · ")
    })).map(|m| format!("{m} g·cm²")).unwrap_or_default();
    let rows = vec![
        ("Weight".to_string(), weight),
        ("Centre of mass".to_string(), format!("{height:.1} mm above its base")),
        ("Stands".to_string(), footing),
        ("Inertia".to_string(), inertia),
    ];
    Some((object, MassFigures {
        rows,
        com: plate([com[0], com[1], com[2]]),
        foot: plate([com[0], y, com[2]]),
        base: outline.iter().filter(|q| q.len() == 2).map(|q| plate([q[0], y, q[1]])).collect(),
        stands,
    }))
}
