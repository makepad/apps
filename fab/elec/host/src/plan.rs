//! Plans resolved by tool prefixes and route jobs handed to the host.

use crate::inbox::Ticket;
use crate::Docs;
use fab_elec_core::{BoardId, Diag, NetId};
use fab_elec_design::{By, Design, Resolved};
use fab_elec_route::{Base, Effort, Engine, JobId, Method, RouteJob, RouteScope};
use std::collections::HashMap;

/// Resolve against strand-owned state; cost per call is the registered plan function's work.
pub type Resolver = fn(&Design, Option<&Docs>, &str, &[(String, String)]) -> Result<Plan, Diag>;

/// Design edits or a route job.
#[derive(Clone, Debug)]
pub enum Plan {
    /// Design edits to commit or preview together.
    Edit(Resolved),
    /// A job to hand to the route worker.
    Route(RouteJob),
}

/// A route job the strand resolved, with the exact committed base it runs on (no zone of
/// the unit pending). The base travels with the start because a fill merge makes a base exact
/// without moving the board generation, so a generation alone would not name it.
/// Sent once the unit's committed fills are exact; a newer route replaces a parked one.
#[derive(Clone, Debug)]
pub struct RouteStart {
    /// The caller's identity for the job's outcome.
    pub ticket: Ticket,
    /// The author supplied by the calling code path.
    pub by: By,
    /// The routing work to start.
    pub job: RouteJob,
    /// The exact committed board state the job runs on.
    pub base: Base,
}

/// Pure: O(scope names + nets or parts of the unit). The strand numbers the job and builds its base.
pub fn route_plan(design: &Design, _docs: Option<&Docs>, tool: &str, args: &[(String, String)]) -> Result<Plan, Diag> {
    if tool != "elec.route" {
        return Err(Diag::error(format!("{tool} is not available yet"), vec![]));
    }
    let args: HashMap<_, _> = args.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
    let unit = if let Some(value) = args.get("unit") {
        // The unit's name (the AI's and the reads' word for it), else its id (the pane's).
        match design.unit_by_name(value) {
            Some(unit) => Some(unit).filter(|unit| unit.board.is_some())
                .ok_or_else(|| Diag::error(format!("unit {} has no board", unit.name), vec![]))?,
            None => {
                let id = BoardId(value.parse::<u16>().map_err(|_| Diag::error(format!("no board unit named `{value}`"), vec![]))?);
                design.unit(id).filter(|unit| unit.board.is_some())
                    .ok_or_else(|| Diag::error(format!("unit {} has no board", id.0), vec![]))?
            }
        }
    } else {
        let mut boards = design.units.iter().filter(|unit| unit.board.is_some());
        let unit = boards.next();
        if unit.is_none() || boards.next().is_some() {
            return Err(Diag::error("say which board unit to route (`unit`)", vec![]));
        }
        unit.expect("there is exactly one board unit")
    };
    let board = unit.board.as_ref().expect("the selected unit has a board");
    let scope = match (args.get("nets"), args.get("parts")) {
        (Some(_), Some(_)) => return Err(Diag::error("route either `nets` or `parts`, not both", vec![])),
        (Some(names), None) => {
            let ids: HashMap<_, _> = board.nets.iter().enumerate()
                .map(|(i, net)| (net.name.as_str(), NetId(i as u32))).collect();
            RouteScope::Nets(scope_names(names, &ids, "net")?)
        }
        (None, Some(names)) => {
            let ids: HashMap<_, _> = unit.circuit.comps.iter().map(|part| (part.refdes.as_str(), part.id)).collect();
            RouteScope::Parts(scope_names(names, &ids, "part")?)
        }
        (None, None) => RouteScope::All,
    };
    let method = match args.get("method").map(|m| m.strip_prefix('@').unwrap_or(m)).unwrap_or("interactive") {
        "interactive" => Method::Interactive,
        "organic" => Method::Organic,
        value => return Err(Diag::error(format!("unknown route method `{value}`"), vec![])),
    };
    let effort = match args.get("effort") {
        Some(value) => Effort(value.replace('_', "").parse::<u32>()
            .map_err(|_| Diag::error(format!("invalid effort `{value}`: expected a u32"), vec![]))?),
        None => Effort::default(),
    };
    let reroute = match args.get("reroute").copied() {
        None | Some("false") => false,
        Some("true") => true,
        Some(value) => return Err(Diag::error(format!("invalid reroute `{value}`: expected true or false"), vec![])),
    };
    Ok(Plan::Route(RouteJob {
        job: JobId(0), unit: unit.id, scope, method, engine: Engine::Auto, style: None, effort, layers: None, reroute,
    }))
}

fn scope_names<T: Copy>(names: &str, ids: &HashMap<&str, T>, kind: &str) -> Result<Vec<T>, Diag> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for name in names.split(',').map(str::trim) {
        match ids.get(name) {
            Some(id) => found.push(*id),
            None => missing.push(name),
        }
    }
    if missing.is_empty() {
        Ok(found)
    } else {
        Err(Diag::error(format!("no {kind} named {} on this board", missing.join(", ")), vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_fixtures::ledctl;

    fn route(design: &Design, args: Vec<(String, String)>) -> RouteJob {
        let Plan::Route(job) = route_plan(design, None, "elec.route", &args).expect("valid route arguments resolve") else {
            panic!("a route tool resolves to a route job");
        };
        job
    }

    /// Why: `elec.route`'s arguments reach the worker as the typed job the person or AI asked
    /// for, and a wrong argument is refused by name instead of routing something else.
    #[test]
    fn route_plan_resolves_and_refuses_by_name() {
        let design = ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_ref().expect("fixture setup: ledctl unit 0 has a board");

        let job = route(&design, vec![]);
        assert_eq!(job.unit, unit.id, "no unit: the design's only board");
        assert_eq!(job.job, JobId(0), "the pure resolver leaves numbering to the strand");
        assert_eq!(job.scope, RouteScope::All, "no scope: all nets");
        assert_eq!((job.method, job.effort, job.engine), (Method::Interactive, Effort::default(), Engine::Auto),
            "defaults: interactive, the default effort, the worker's engine");
        assert!(job.style.is_none() && job.layers.is_none(), "no style or layers: each net's own");
        assert!(!job.reroute, "no reroute: the board's copper is kept");

        let nets = format!("{}, {}", board.nets[1].name, board.nets[0].name);
        assert_eq!(route(&design, vec![("nets".into(), nets)]).scope, RouteScope::Nets(vec![NetId(1), NetId(0)]),
            "net names become the board's net ids, in the order asked");
        let part = &unit.circuit.comps[0];
        assert_eq!(route(&design, vec![("parts".into(), part.refdes.clone())]).scope, RouteScope::Parts(vec![part.id]),
            "a refdes becomes its circuit part");
        let job = route(&design, vec![("method".into(), "organic".into()), ("effort".into(), "3".into())]);
        assert_eq!((job.method, job.effort), (Method::Organic, Effort(3)), "method and effort are typed");
        assert!(route(&design, vec![("reroute".into(), "true".into())]).reroute, "a re-route is asked by name");

        let refused = |tool: &str, args: &[(&str, &str)]| {
            let args: Vec<_> = args.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
            route_plan(&design, None, tool, &args).expect_err("the plan is refused").message
        };
        assert_eq!(refused("elec.route", &[("nets", "missing_A, missing_B")]), "no net named missing_A, missing_B on this board",
            "every unknown net is named");
        assert_eq!(refused("elec.route", &[("nets", "A"), ("parts", "R1")]), "route either `nets` or `parts`, not both",
            "two scopes are refused");
        assert_eq!(refused("elec.route", &[("method", "fast")]), "unknown route method `fast`", "an unknown method is named");
        assert_eq!(refused("elec.route", &[("reroute", "yes")]), "invalid reroute `yes`: expected true or false", "reroute is a bool");
        assert_eq!(refused("elec.route", &[("unit", "9")]), "unit 9 has no board", "a unit without a board is named");
        assert_eq!(refused("elec.route.explain", &[]), "elec.route.explain is not available yet",
            "another elec.route tool is not a route job");
    }
}
