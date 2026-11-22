use fab_elec_bench::bench::Supply;
use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::input::{LowerInput, LowerNet, LowerPart, LowerPin, PartKind};
use fab_elec_bench::tolerance::{draw, member_circuit, member_noise, member_param, member_values, noise_seed};
use fab_elec_bench::{lower, lower_input, ModelLibs};
use fab_elec_core::ids::{BoardId, CompId, CompRef, NetId, NetRef};
use fab_elec_sim::analysis::op::{operating_point, operating_point_from};
use fab_elec_sim::livesim::{LiveSpec, FAST_RELTOL};
use fab_elec_sim::{OpAid, OpSeed, SimCircuit, Solver, Wave};
use std::collections::HashSet;

fn net(id: u32) -> NetRef {
    NetRef { board: BoardId(0), net: NetId(id) }
}

fn resistor(id: u32, path: &str) -> LowerPart {
    LowerPart {
        id: CompRef { board: BoardId(0), comp: CompId(id) },
        path: path.into(),
        kind: PartKind::Resistor { ohms: 1e3 },
        tol: Some((990.0, 1010.0)),
        pins: vec![
            LowerPin { name: "1".into(), net: Some(net(id)), nc: false },
            LowerPin { name: "2".into(), net: Some(net(0)), nc: false },
        ],
        noise: false,
    }
}

#[test]
fn tolerance_sites_and_member_draws() -> Result<(), String> {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let bench = ledctl_bench(1e3, [0.3; 8]);
    let lowered = lower(&design, BoardId(0), &bench, &libs);
    assert!(lowered.diags.is_empty(), "ledctl tolerance lowering has diagnostics: {:?}", lowered.diags);
    assert!(!lowered.tol.is_empty(), "ledctl must have tolerance sites");
    let circuit = &design.unit(BoardId(0)).ok_or("ledctl is missing board 0")?.circuit;
    for site in &lowered.tol {
        assert_eq!((&site.device, site.key.as_str()), (&site.path, "value"),
            "{} tolerance site must use its path and value key", site.path);
        let comp = circuit.comps.get(site.part as usize)
            .ok_or_else(|| format!("{} tolerance part index is outside the design", site.path))?;
        assert_eq!(comp.path, site.path, "{} tolerance part index must preserve design order", site.path);
        let id = CompRef { board: BoardId(0), comp: comp.id };
        assert_eq!(lowered.index.comp(id), Some(site.part as usize),
            "{} tolerance part index must match the value index", site.path);
    }
    for i in 0..8 {
        let path = format!("lamps[{i}].r");
        let site = lowered.tol.iter().find(|site| site.path == path)
            .ok_or_else(|| format!("ledctl is missing tolerance site {path}"))?;
        assert_eq!(site.nominal, 82.0, "{path} nominal resistance must be 82 ohms for true green at 5 mA with a 25 ohm sink");
        assert!(((site.hi - site.lo) / site.nominal - 0.02).abs() < 1e-12,
            "{path} must use the 1% resistor class tolerance: {site:?}");
    }
    assert!(!lowered.circuit.devices.c.is_empty(), "ledctl must have capacitors to check their tolerance");
    // the part capacitors: a stub's pin capacitances (`mcu.u#PA0.cin`) are not parts
    for cap in lowered.circuit.devices.c.iter().filter(|cap| !cap.name.contains('#')) {
        let site = lowered.tol.iter().find(|site| site.device == cap.name)
            .ok_or_else(|| format!("{} capacitor is missing a tolerance site", cap.name))?;
        assert!(((site.hi - site.lo) / site.nominal - 0.2).abs() < 1e-12,
            "{} must use the 10% capacitor class tolerance: {site:?}", cap.name);
    }

    let site = lowered.tol.iter().find(|site| site.path == "lamps[0].r")
        .ok_or("ledctl is missing the 82 ohm sampling site")?;
    let sample = draw(17, &site.path);
    assert_eq!(sample.to_bits(), draw(17, &site.path).to_bits(), "same seed and path must reproduce the draw bits");
    assert_ne!(sample.to_bits(), draw(18, &site.path).to_bits(), "different seeds must change the draw");
    assert_ne!(sample.to_bits(), draw(17, "lamps[1].r").to_bits(), "different paths must change the draw");

    let sigma = (site.hi - site.lo) / 6.0;
    let n = 10_000_u64;
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for seed in 0..n {
        let values = member_values(std::slice::from_ref(site), seed);
        assert_eq!(values.len(), 1, "one tolerance site must produce one member value");
        let value = values[0];
        assert!((site.lo..=site.hi).contains(&value), "seed {seed} draws {value} outside the tolerance bounds");
        let delta = value - site.nominal;
        sum += delta;
        sum_sq += delta * delta;
    }
    let mean = site.nominal + sum / n as f64;
    let sd = ((sum_sq - sum * sum / n as f64) / (n - 1) as f64).sqrt();
    assert!((mean - site.nominal).abs() < 4.0 * sigma / (n as f64).sqrt(),
        "member mean {mean} must stay within four standard errors of {}", site.nominal);
    // A 3-sigma truncation shrinks the standard deviation by about 1.4%; clamping shrinks it less.
    assert!((sd / sigma - 1.0).abs() < 0.05,
        "member sample standard deviation {sd} must be within 5% of {sigma}");

    let mut input = LowerInput {
        nets: ["0", "a", "b"].into_iter().enumerate()
            .map(|(i, name)| LowerNet { id: net(i as u32), name: name.into(), supply: false }).collect(),
        ground: net(0),
        parts: vec![resistor(1, "r1"), resistor(2, "r2")],
        supplies: Vec::new(),
        sources: Vec::new(),
        firmware: Vec::new(),
        probes: Vec::new(),
    };
    let before = lower_input(&input, 0, &libs);
    input.nets.push(LowerNet { id: net(3), name: "c".into(), supply: false });
    input.parts.insert(0, resistor(3, "r0"));
    let after = lower_input(&input, 1, &libs);
    // each resistor's other end is a net of its own: only those open ends are noted, by net
    let notes = |l: &fab_elec_bench::Lowered| -> Vec<(String, String)> {
        l.diags.iter().map(|d| (d.path.clone(), if d.net.is_some() && d.part.is_none() { d.message.clone() } else { format!("{d:?}") })).collect()
    };
    let open = |net: &str, r: &str| (net.to_owned(), format!("{net}: one connection only ({r} 1): an open end"));
    assert_eq!(notes(&before), vec![open("a", "r1"), open("b", "r2")],
        "the resistor lowering has no part diagnostics, only its open ends noted by net");
    assert_eq!(notes(&after), vec![open("a", "r1"), open("b", "r2"), open("c", "r0")],
        "the resistor lowering with r0 has no part diagnostics, only its open ends noted by net");
    assert_eq!(before.tol.len(), 2, "both original resistors must have tolerance sites");
    assert_eq!(after.tol.len(), 3, "inserting r0 must add one tolerance site");
    for (i, site) in after.tol.iter().enumerate() {
        assert_eq!(site.part as usize, i, "{} part index must follow the reordered input", site.path);
        assert_eq!(site.path, input.parts[i].path, "tolerance sites must follow the reordered input");
    }
    for seed in 0..20 {
        let original = member_values(&before.tol, seed);
        let inserted = member_values(&after.tol, seed);
        for path in ["r1", "r2"] {
            let a = before.tol.iter().position(|site| site.path == path)
                .ok_or_else(|| format!("original lowering is missing {path}"))?;
            let b = after.tol.iter().position(|site| site.path == path)
                .ok_or_else(|| format!("inserted lowering is missing {path}"))?;
            assert_eq!(original[a].to_bits(), inserted[b].to_bits(),
                "{path} draw must survive insertion before it and relowering at seed {seed}");
        }
    }

    let values = member_values(&after.tol, 17);
    let member = member_circuit(&after, &values);
    assert_eq!(values.len(), after.tol.len(), "each resistor site must have a member value");
    for (site, &drawn) in after.tol.iter().zip(&values) {
        let device = member.devices.r.iter().find(|device| device.name == site.device)
            .ok_or_else(|| format!("member circuit is missing resistor {}", site.device))?;
        assert_eq!(device.r, drawn, "{} member resistance must equal its drawn value", site.path);
        let scaled = member_param(&after.tol, &values, &site.device, &site.key, 2.0 * site.nominal);
        assert!((scaled - 2.0 * drawn).abs() <= 1e-12 * drawn.abs(),
            "{} doubled nominal parameter must double the member's drawn value", site.path);
    }
    assert!(after.circuit.devices.r.iter().all(|device| device.r == 1e3),
        "building a member must preserve nominal resistor values");
    assert_eq!(member_param(&after.tol, &values, "unknown", "value", 123.0), 123.0,
        "unknown device parameters must remain unchanged");
    let resistances = |c: &fab_elec_sim::SimCircuit| c.devices.r.iter().map(|d| d.r).collect::<Vec<_>>();
    assert_eq!(resistances(&member_circuit(&after, &[])), resistances(&after.circuit),
        "unpaired sites must leave the circuit unchanged");
    let mut extra = values.clone();
    extra.push(123.0);
    assert_eq!(resistances(&member_circuit(&after, &extra)), resistances(&member), "extra member values must be ignored");

    let values = member_values(&lowered.tol, 3);
    let mut noisy = member_circuit(&lowered, &values);
    let mut repeated = member_circuit(&lowered, &values);
    let mut other = member_circuit(&lowered, &member_values(&lowered.tol, 4));
    let dt = 1e-6;
    let sources = noisy.devices.i.len();
    let resistors = noisy.devices.r.len();
    assert!(resistors > 1, "ledctl must have multiple resistors to check independent noise sequences");
    for (seed, circuit) in [(3, &mut noisy), (3, &mut repeated), (4, &mut other)] {
        assert_eq!(member_noise(circuit, seed, dt), resistors,
            "member {seed} must add one noise source per resistor");
        assert_eq!(circuit.devices.i.len(), sources + resistors,
            "member {seed} noise must add exactly one current source per resistor");
    }
    let mut seeds = HashSet::new();
    for resistor in &noisy.devices.r {
        let name = format!("{}#noise", resistor.name);
        assert_eq!(noisy.devices.i.iter().filter(|source| source.name == name).count(), 1,
            "{} must have exactly one named noise current source", resistor.name);
        let source = noisy.devices.i.iter().find(|source| source.name == name)
            .ok_or_else(|| format!("member 3 is missing noise source {name}"))?;
        assert_eq!((source.p, source.n), (resistor.a, resistor.b),
            "{name} must span its resistor's two nodes");
        let Wave::Noise { seed, sigma, dt: spacing, .. } = &source.src.wave else {
            return Err(format!("{name} must carry a noise waveform"));
        };
        assert_eq!(*spacing, dt, "{name} sample spacing must equal the member's largest step");
        let expected = (2.0 * 1.380649e-23 * (noisy.temp_c + 273.15) / (resistor.r * dt)).sqrt();
        assert!((sigma / expected - 1.0).abs() <= 1e-12,
            "{name} sigma {sigma} must match the member resistance's thermal noise {expected}");
        assert_eq!(*seed, noise_seed(3, &resistor.name), "{name} must use its resistor and member noise seed");
        assert!(seeds.insert(*seed), "{name} must have a seed distinct from every other resistor and member");
        let repeated_source = repeated.devices.i.iter().find(|source| source.name == name)
            .ok_or_else(|| format!("repeated member 3 is missing noise source {name}"))?;
        assert_eq!(repeated_source.src.wave, source.src.wave,
            "{name} must reproduce its noise seed, sigma and spacing on a fresh member 3 circuit");
        let other_source = other.devices.i.iter().find(|source| source.name == name)
            .ok_or_else(|| format!("member 4 is missing noise source {name}"))?;
        let Wave::Noise { seed: other_seed, .. } = &other_source.src.wave else {
            return Err(format!("member 4's {name} must carry a noise waveform"));
        };
        assert_eq!(*other_seed, noise_seed(4, &resistor.name), "member 4's {name} must use its own noise seed");
        assert_ne!(seed, other_seed, "{name} must have different noise seeds in members 3 and 4");
        assert!(seeds.insert(*other_seed), "member 4's {name} must have a seed distinct from every other resistor and member");
    }
    for dt in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut circuit = member_circuit(&lowered, &values);
        assert_eq!(member_noise(&mut circuit, 3, dt), 0, "invalid noise spacing {dt} must add no sources");
        assert_eq!(circuit.devices.i, lowered.circuit.devices.i,
            "invalid noise spacing {dt} must preserve the original current sources");
    }
    Ok(())
}

/// The operating point as a member's `LiveSim::new_from` solves it in Fast, the host's mode: at
/// `FAST_RELTOL`, node initial conditions held, first from `seed` when one is given.
fn live_op(circuit: &SimCircuit, seed: Option<&OpSeed>) -> Result<(OpAid, u64, Solver), String> {
    let mut circuit = circuit.clone();
    circuit.opts.reltol = FAST_RELTOL;
    let mut s = Solver::new(&circuit).map_err(|diag| diag.to_string())?;
    let aid = match seed {
        Some(seed) => operating_point_from(&mut s, true, Some(seed)),
        None => operating_point(&mut s, true),
    }.map_err(|diag| diag.to_string())?;
    Ok((aid, s.stats.iterations, s))
}

/// The largest difference between two solutions of one layout, relative to the larger value.
fn rel_diff(a: &Solver, b: &Solver) -> f64 {
    a.x.iter().zip(&b.x).map(|(x, y)| (x - y).abs() / (1e-6 + x.abs().max(y.abs()))).fold(0.0, f64::max)
}

/// ESIM-Q3 step 1: an ensemble member starts from its nominal's operating point. With the LEDs on
/// and the USB supply limited from 3 to 30 mA (where a cold solve needs source stepping, up to
/// thousands of iterations) and unlimited, each of the fixture's members converges by plain Newton
/// from the nominal's solution in at most 6 iterations, to its own cold solution; the nominal
/// restarted from its own solution converges at once; and a seed far from the solution (the
/// unlimited bench's, into the 10 mA bench) still ends at the cold solution through the homotopy.
#[test]
fn members_start_from_the_nominal_operating_point() -> Result<(), String> {
    let design = fab_elec_fixtures::ledctl();
    let libs = ModelLibs::generic();
    let scope = BoardId(0);
    let vbus = design.unit(scope).and_then(|u| u.circuit.net("VBUS")).ok_or("ledctl has no VBUS")?.id;
    let max_step = LiveSpec::default().max_step;
    // duty 0.3: every lamp's PWM is on at t = 0, the heaviest load the supply limit meets
    let bench_at = |limit: Option<f64>| {
        let mut bench = ledctl_bench(1e3, [0.3; 8]);
        bench.supplies.push(Supply { net: NetRef { board: scope, net: vbus }, v: 5.0, limit, ramp: None });
        lower(&design, scope, &bench, &libs)
    };
    let mut ten = None;
    for limit in [Some(3e-3), Some(5e-3), Some(7e-3), Some(10e-3), Some(15e-3), Some(20e-3), Some(30e-3), None] {
        let lowered = bench_at(limit);
        assert!(lowered.diags.is_empty(), "{limit:?}: the limited bench lowers cleanly: {:?}", lowered.diags);
        let (_, _, nominal) = live_op(&lowered.circuit, None)?;
        let seed = OpSeed::of(&nominal);
        let (aid, iterations, restarted) = live_op(&lowered.circuit, Some(&seed))?;
        assert!(aid == OpAid::Newton && iterations <= 2 && rel_diff(&nominal, &restarted) < 1e-6,
            "{limit:?}: the nominal restarted from its own solution converges at once, where it was: {aid:?} in {iterations}, {:e}",
            rel_diff(&nominal, &restarted));
        for m in 1..=8 {
            let mut circuit = member_circuit(&lowered, &member_values(&lowered.tol, m));
            member_noise(&mut circuit, m, max_step);
            let (aid, iterations, warm) = live_op(&circuit, Some(&seed))?;
            assert!(aid == OpAid::Newton && iterations <= 6,
                "{limit:?}: member {m} converges by plain Newton from the nominal in at most 6 iterations: {aid:?} in {iterations}");
            if m == 1 && limit == Some(10e-3) {
                let (_, _, cold) = live_op(&circuit, None)?;
                assert!(rel_diff(&cold, &warm) < 1e-6,
                    "member 1 at 10 mA reaches its cold solution from the nominal's: {:e}", rel_diff(&cold, &warm));
            }
        }
        if limit == Some(10e-3) {
            ten = Some((lowered, nominal));
        } else if limit.is_none() {
            let (lowered, cold) = ten.take().ok_or("the 10 mA bench ran")?;
            let (_, _, far) = live_op(&lowered.circuit, Some(&seed))?;
            assert!(rel_diff(&cold, &far) < 1e-6,
                "the unlimited solution into the 10 mA bench still ends at the cold solution: {:e}", rel_diff(&cold, &far));
        }
    }
    Ok(())
}
