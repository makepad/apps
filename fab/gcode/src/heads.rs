//! Which nozzle each filament prints with: the choice whose changes take least time.
//!
//! A filament's changes depend on the others on its nozzle: two that print in the same layers
//! swap on each of them when they share one, and not at all apart. So the choice is weighed over
//! the print's own layer sets by the rule the writer prints by ([`plan_changes`]): the changes'
//! time first (each purge at its flush speed, the profile's switch, load and unload times), then
//! the purge. Every choice is weighed when there are few; past [`EVERY`] the search starts from
//! the nozzles given and moves one filament or swaps two while that saves.

use crate::writer::{plan_changes, ChangeCount};
use makepad_fab_settings::change::ChangeModel;

/// The most choices weighed one by one.
pub const EVERY: usize = 256;

/// A print's changes as the writer plans them: each layer's filaments ([`crate::writer::layer_sets`])
/// and the change model of its job (the nozzles the filaments are on).
#[derive(Clone, Debug)]
pub struct Changes {
    pub sets: Vec<Vec<usize>>,
    pub model: ChangeModel,
}

/// The nozzles chosen, and what the print's changes come to on them and on the nozzles given.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    /// Per filament, its nozzle.
    pub heads: Vec<usize>,
    pub count: ChangeCount,
    pub given: ChangeCount,
}

impl Choice {
    /// The time and purge the choice saves over the nozzles given (s, g).
    pub fn saves(&self) -> (f64, f64) {
        (self.given.seconds - self.count.seconds, self.given.purge_g - self.count.purge_g)
    }
}

/// Better: less time, or as long and less purge.
fn better(a: &ChangeCount, b: &ChangeCount) -> bool {
    const SAME: f64 = 1e-6;
    a.seconds < b.seconds - SAME || (a.seconds <= b.seconds + SAME && a.purge_mm3 < b.purge_mm3 - SAME)
}

/// The nozzles the filaments of a print of `sets` (the writer's [`crate::writer::layer_sets`])
/// print with fastest. `changes` gives the printer, the filaments and the nozzles they are on now;
/// `choices[f]` the nozzles filament `f` may take (one or none: it stays where it is), `room[h]`
/// how many filaments nozzle `h` can take (at least those on it now). A filament no layer prints
/// stays where it is.
pub fn cheapest_heads(sets: &[Vec<usize>], changes: &ChangeModel, choices: &[Vec<usize>], room: &[usize]) -> Choice {
    let nf = choices.len();
    let given: Vec<usize> = (0..nf).map(|f| changes.ext_of(f)).collect();
    let on = |heads: &[usize], h: usize| heads.iter().filter(|&&x| x == h).count();
    let fits = |heads: &[usize]| room.iter().enumerate().all(|(h, &n)| on(heads, h) <= n.max(on(&given, h)));
    let weigh = |heads: &[usize]| plan_changes(sets, &changes.on_nozzles(heads));
    let now = weigh(&given);
    // The filaments that print and have somewhere else to go.
    let free: Vec<usize> = (0..nf).filter(|&f| sets.iter().any(|s| s.contains(&f)) && choices[f].iter().any(|&h| h != given[f])).collect();
    let mut best = (given.clone(), now);
    let every = free.iter().try_fold(1usize, |n, &f| n.checked_mul(choices[f].len()).filter(|&n| n <= EVERY));
    if let Some(total) = every {
        for code in 1..total {
            let mut heads = given.clone();
            let mut c = code;
            for &f in &free {
                heads[f] = choices[f][c % choices[f].len()];
                c /= choices[f].len();
            }
            if !fits(&heads) {
                continue;
            }
            let count = weigh(&heads);
            if better(&count, &best.1) {
                best = (heads, count);
            }
        }
    } else {
        loop {
            let mut step: Option<(Vec<usize>, ChangeCount)> = None;
            let consider = |heads: Vec<usize>, step: &mut Option<(Vec<usize>, ChangeCount)>| {
                if !fits(&heads) {
                    return;
                }
                let count = weigh(&heads);
                let to_beat = step.as_ref().map_or(&best.1, |s| &s.1);
                if better(&count, to_beat) {
                    *step = Some((heads, count));
                }
            };
            for &f in &free {
                for &h in choices[f].iter().filter(|&&h| h != best.0[f]) {
                    let mut heads = best.0.clone();
                    heads[f] = h;
                    consider(heads, &mut step);
                }
            }
            for (i, &a) in free.iter().enumerate() {
                for &b in &free[i + 1..] {
                    let (ha, hb) = (best.0[a], best.0[b]);
                    if ha != hb && choices[a].contains(&hb) && choices[b].contains(&ha) {
                        let mut heads = best.0.clone();
                        heads.swap(a, b);
                        consider(heads, &mut step);
                    }
                }
            }
            match step {
                Some(s) => best = s,
                None => break,
            }
        }
    }
    Choice { heads: best.0, count: best.1, given: now }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_settings::config::Config;
    use makepad_fab_settings::machine::Job;

    fn changes(nozzles: usize, map: &[usize]) -> ChangeModel {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); nozzles]);
        cfg.set_vec("filament_map", map.iter().map(|e| (e + 1).to_string()).collect());
        let nf = map.len();
        cfg.set_vec("flush_volumes_matrix", (0..nozzles * nf * nf).map(|i| if i % nf == (i / nf) % nf { "0" } else { "100" }.to_string()).collect());
        cfg.set_vec("filament_max_volumetric_speed", vec!["10".into(); nf]);
        cfg.set_vec("filament_density", vec!["1.24".into(); nf]);
        cfg.set_str("machine_switch_extruder_time", "5");
        ChangeModel::new(&Job::new(cfg))
    }

    /// Two filaments that print together take a nozzle each; the third goes with the one it
    /// never meets: no swaps where the given nozzles swap every layer.
    #[test]
    fn filaments_that_print_together_take_a_nozzle_each() {
        // 0 and 1 print in every layer, 2 only over the top, where 1 has ended
        let sets: Vec<Vec<usize>> = (0..20).map(|i| if i < 10 { vec![0, 1] } else { vec![0, 2] }).collect();
        let given = changes(2, &[0, 0, 1]);
        let c = cheapest_heads(&sets, &given, &[vec![0, 1], vec![0, 1], vec![0, 1]], &[4, 4]);
        assert!(c.given.swaps >= 10, "the given nozzles swap on every layer of the first half: {:?}", c.given);
        assert_eq!(c.count.swaps, 1, "{:?}", c);
        assert!(c.heads[0] != c.heads[1] && c.heads[1] == c.heads[2], "{:?}", c.heads);
        let (s, g) = c.saves();
        assert!(s > 0.0 && g > 0.0, "{s} s, {g} g");
        assert_eq!(c.count, plan_changes(&sets, &given.on_nozzles(&c.heads)), "weighed by the writer's plan");
    }

    /// A nozzle takes no more filaments than it has room for: a nozzle each when there is room,
    /// else the two that never meet share the one with room for two.
    #[test]
    fn a_nozzle_takes_no_more_than_its_room() {
        // 0 prints throughout, 1 in the lower half, 2 in the upper
        let sets: Vec<Vec<usize>> = (0..10).map(|i| if i < 5 { vec![0, 1] } else { vec![0, 2] }).collect();
        let given = changes(3, &[0, 0, 2]);
        let c = cheapest_heads(&sets, &given, &vec![vec![0, 1, 2]; 3], &[1, 1, 1]);
        assert_eq!(c.count.swaps, 0, "{c:?}");
        let c = cheapest_heads(&sets, &given, &vec![vec![0, 1, 2]; 3], &[2, 0, 1]);
        assert_eq!((c.count.swaps, c.heads.clone()), (1, vec![2, 0, 0]), "{c:?}");
    }

    /// A filament held to one nozzle stays there, and the rest are chosen round it.
    #[test]
    fn a_filament_with_one_choice_stays() {
        let sets: Vec<Vec<usize>> = (0..10).map(|_| vec![0, 1, 2]).collect();
        let given = changes(2, &[0, 1, 0]);
        let c = cheapest_heads(&sets, &given, &[vec![0], vec![0, 1], vec![0, 1]], &[4, 4]);
        assert_eq!(c.heads[0], 0);
        assert!(!better(&c.given, &c.count), "never worse than the given: {c:?}");
    }

    /// Past [`EVERY`] choices the search moves and swaps from the given nozzles: four nozzles and
    /// six filaments, three pairs that print together, end with no swaps.
    #[test]
    fn many_choices_are_searched_from_the_given_nozzles() {
        let sets: Vec<Vec<usize>> = (0..30).map(|i| match i / 10 { 0 => vec![0, 1], 1 => vec![2, 3], _ => vec![4, 5] }).collect();
        let given = changes(4, &[0, 0, 1, 1, 2, 2]);
        let all = vec![0, 1, 2, 3];
        let c = cheapest_heads(&sets, &given, &vec![all; 6], &[2; 4]);
        assert!(4usize.pow(6) > EVERY);
        assert!(c.given.swaps >= 30 && c.count.swaps <= 2, "{:?}", c);
    }
}
