//! How a board's case goes together, in order: what the Device editor's Kit to Assembled
//! animation plays, and what a person reads as the build order. Derived from the case's parts
//! ([`case_poses`](crate::case_poses)) until a case script states its own order; the animation
//! reads only an [`AssemblyPlan`], so a stated order replaces this derive without touching it.

use crate::bridge::{board_base, CasePose, CaseRole};

/// What one step of the assembly puts on.
#[derive(Clone, Debug, PartialEq)]
pub enum StepKind {
    /// The shell the board is fixed to ([`board_base`]): it stays where it lies, and everything
    /// else comes to it.
    Base,
    /// Another shell (neither base nor lid), with the bodies printed in it.
    Shell(String),
    /// The board, into its base. Its host moves it by its own part, so the step names no case part.
    Board,
    /// The key pins, onto their switches: before the lid, which holds them.
    Keys,
    /// The lid, with the bodies printed in it (light pipes).
    Lid,
    /// The knobs, onto their shafts through the lid.
    Knobs,
}

/// One step: the parts it puts on and how they arrive.
#[derive(Clone, Debug, PartialEq)]
pub struct AssemblyStep {
    pub kind: StepKind,
    /// The case parts it puts on, by placed index ([`CasePose::part`]), in placed order.
    pub parts: Vec<usize>,
    /// The side its parts come from, in the board's frame (a unit vector): they wait clear of
    /// their place along it, then move straight onto it. Above the board (`[0, 0, 1]`) for every
    /// part the case library builds today, which all seat from the top; the base, which the rest
    /// come to, would come from below (`[0, 0, -1]`).
    pub from: [f64; 3],
}

/// A case's build order, base first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssemblyPlan {
    pub steps: Vec<AssemblyStep>,
}

impl AssemblyPlan {
    /// The step that puts `part` (a placed index) on, if any.
    pub fn step_of(&self, part: usize) -> Option<usize> {
        self.steps.iter().position(|s| s.parts.contains(&part))
    }
}

/// The build order of a board's case from its parts: the base, other shells, the board, the key
/// pins, the lid with what is printed in it, the knobs. A body printed in a shell goes on with its
/// shell. Empty when the case has no shell (a bare board needs no assembly).
/// Cost: O(parts) per call; a host derives it once per case resolve, never per frame.
pub fn assembly_plan(poses: &[CasePose]) -> AssemblyPlan {
    let Some(base) = board_base(poses) else { return AssemblyPlan::default() };
    let base_role = match &base.role {
        CaseRole::Shell(role) => role.clone(),
        _ => return AssemblyPlan::default(),
    };
    const ABOVE: [f64; 3] = [0.0, 0.0, 1.0];
    let step = |kind, parts: Vec<usize>, from| AssemblyStep { kind, parts, from };
    // a shell's own part and the bodies printed in it
    let with_bodies = |role: &str| -> Vec<usize> {
        poses.iter().filter(|p| match &p.role {
            CaseRole::Shell(r) => r == role,
            CaseRole::Body { host, .. } => host == role,
            _ => false,
        }).map(|p| p.part).collect()
    };
    let mut steps = vec![step(StepKind::Base, with_bodies(&base_role), [0.0, 0.0, -1.0])];
    let mut others: Vec<&str> = Vec::new();
    for pose in poses {
        if let CaseRole::Shell(role) = &pose.role {
            if role != &base_role && role != "lid" && !others.contains(&role.as_str()) {
                others.push(role);
            }
        }
    }
    for role in others {
        steps.push(step(StepKind::Shell(role.to_string()), with_bodies(role), ABOVE));
    }
    steps.push(step(StepKind::Board, Vec::new(), ABOVE));
    let of = |keep: fn(&CaseRole) -> bool| -> Vec<usize> { poses.iter().filter(|p| keep(&p.role)).map(|p| p.part).collect() };
    let keys = of(|r| matches!(r, CaseRole::Key(_)));
    if !keys.is_empty() {
        steps.push(step(StepKind::Keys, keys, ABOVE));
    }
    if base_role != "lid" {
        let lid = with_bodies("lid");
        if !lid.is_empty() {
            steps.push(step(StepKind::Lid, lid, ABOVE));
        }
    }
    let knobs = of(|r| matches!(r, CaseRole::Knob(_)));
    if !knobs.is_empty() {
        steps.push(step(StepKind::Knobs, knobs, ABOVE));
    }
    AssemblyPlan { steps }
}
