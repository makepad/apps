//! The expression dialect: constant values against hand-computed ones, compiled tapes' values and
//! exact partials, and where a step changes its choice between two evaluations.

use fab_elec_sim::expr::{Env, Expr, Leaf, Ref, Scratch, Tape};
use fab_elec_sim::spice::parse;
use std::f64::consts::PI;

fn constant(text: &str) -> Result<f64, String> {
    Expr::parse(text)?.value(&|name| (name == "rload").then_some(1e3))
}

/// Every operator, precedence rule and function of the dialect against its hand-computed value,
/// then the errors: a text that is not one expression, an unknown name, a value that reads the
/// circuit, a result that is not finite, a NaN hidden by the operand it chooses, and nesting past
/// the bound.
#[test]
fn the_dialect_evaluates_as_documented() {
    let cases: &[(&str, f64, &str)] = &[
        ("1+2*3", 7.0, "* binds tighter than +"),
        ("(1+2)*3", 9.0, "parentheses"),
        ("2^3^2", 512.0, "^ is right-associative"),
        ("2**3", 8.0, "** is power"),
        ("-2^2", -4.0, "power binds tighter than unary minus"),
        ("2^-1", 0.5, "a signed exponent"),
        ("1k*2 + .5meg", 2e3 + 5e5, "SPICE scale suffixes"),
        ("2*rload", 2e3, "a parameter"),
        ("1 < 2", 1.0, "a true comparison is 1"),
        ("2 <= 1", 0.0, "a false comparison is 0"),
        ("3 > 2 && 1 >= 2", 0.0, "&&"),
        ("0 || 2 != 2", 0.0, "|| binds looser than !="),
        ("!0 + !3", 1.0, "! of zero and non-zero"),
        ("1 == 1.0", 1.0, "=="),
        ("1 < 2 ? 10 : 20", 10.0, "a comparison binds tighter than the choice"),
        ("0 ? 2 : 0 ? 4 : 5", 5.0, "the choice is right-associative"),
        ("1 ? 2 : sqrt(-1)", 2.0, "the branch not taken is never read"),
        ("log(exp(2))", 2.0, "log is the natural log"),
        ("ln(1)", 0.0, "ln"),
        ("log10(1000)", 3.0, "log10"),
        ("sqrt(16) + abs(-3)", 7.0, "sqrt and abs"),
        ("sgn(-2) + 10*sgn(0) + 100*sgn(5)", 99.0, "sgn"),
        ("u(0) + 10*u(1n) + 100*u(-1)", 10.0, "u is 1 only above 0"),
        ("uramp(-2) + uramp(3)", 3.0, "uramp"),
        ("floor(-1.5) + 10*ceil(-1.5)", -12.0, "floor and ceil"),
        ("min(3, 2) + 10*max(3, 2)", 32.0, "min and max"),
        ("pow(2, 10)", 1024.0, "pow"),
        ("pwr(-8, 1/3)", -2.0, "pwr keeps the sign of its base"),
        ("limit(5, 0, 3) + 10*limit(-1, 0, 3) + 100*limit(2, 0, 3)", 203.0, "limit"),
        ("sin(pi/2) + cos(0) + 4*atan(1)", 2.0 + PI, "trigonometry"),
        ("tan(pi/4) + asin(1) + acos(1)", 1.0 + PI / 2.0, "tan, asin, acos"),
        ("sinh(0) + cosh(0) + tanh(0)", 1.0, "hyperbolic functions"),
        ("SIN(PI/6)*2 + LOG10(10)", 2.0, "function and built-in names ignore case"),
    ];
    for &(text, want, law) in cases {
        let got = constant(text).unwrap_or_else(|e| panic!("{law}: {text} evaluates: {e}"));
        assert!((got - want).abs() <= 1e-12 * want.abs().max(1.0), "{law}: {text} = {got}, want {want}");
    }
    let nested = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    let errors: &[(&str, &str)] = &[
        ("1 +", "expected expression value"),
        ("1 = 2", "unexpected text"),
        ("foo(1)", "unknown function"),
        ("min(1)", "wrong argument count"),
        ("rx*2", "unknown parameter rx"),
        ("v(a)*2", "is not a constant"),
        ("time", "is not a constant"),
        ("1/0", "non-finite"),
        ("min(sqrt(-1), 1)", "non-finite"),
        ("sqrt(-1) > 0 ? 1 : 2", "non-finite"),
        (&nested, "nesting limit"),
    ];
    for &(text, want) in errors {
        let err = constant(text).expect_err(text);
        assert!(err.contains(want), "{text}: the error names {want:?}: {err}");
    }

    // the reader's values are the same expressions, wrapped or not
    let deck = parse("params\n.param a={u(1)*limit(5, 0, 3)} b={a > 2 ? 10 : 20} c=log(100)*min(a, 4)\nR1 x 0 {b*a}\nR2 x 0 {c}\nV1 x 0 1\n.op\n.end\n");
    assert!(deck.diags.is_empty(), "the deck reads cleanly: {:?}", deck.diags);
    let r = |name: &str| deck.circuit.devices.r.iter().find(|r| r.name == name).map(|r| r.r).expect("resistor");
    assert_eq!(r("r1"), 30.0, "a brace value of choices and steps");
    assert!((r("r2") - 3.0 * 100f64.ln()).abs() < 1e-12, "an unwrapped .param value keeps its function's commas");
}

/// Nodes a and b are unknowns 1 and 2, the branch of `vs` unknown 3; anything else is ground or
/// unknown.
fn compile(text: &str) -> Tape {
    Expr::parse(text)
        .and_then(|e| {
            e.compile(&mut |r| match r {
                Ref::Node("a") => Ok(Leaf::Unknown(1)),
                Ref::Node("b") => Ok(Leaf::Unknown(2)),
                Ref::Node("0") => Ok(Leaf::Value(0.0)),
                Ref::Branch("vs") => Ok(Leaf::Unknown(3)),
                Ref::Param("gain") => Ok(Leaf::Value(4.0)),
                r => Err(format!("unresolved {r:?}")),
            })
        })
        .unwrap_or_else(|e| panic!("{text} compiles: {e}"))
}

/// Value and partials by unknown (1, 2, 3) of `text` at a, b and the branch current.
fn eval(text: &str, a: f64, b: f64, i: f64, env: Env) -> (f64, [f64; 3]) {
    let tape = compile(text);
    let mut s = Scratch::default();
    s.fit(&tape);
    let mut grad = [0.0; 3];
    let x = [0.0, a, b, i];
    let v = tape.eval(&x, env, &mut s, &mut grad);
    let mut by_unknown = [0.0; 3];
    for (k, &u) in tape.inputs().iter().enumerate() {
        by_unknown[u as usize - 1] = grad[k];
    }
    let alone = tape.value(&x, env, &mut s);
    assert_eq!(alone.to_bits(), v.to_bits(), "{text}: the value alone equals the value with partials");
    (v, by_unknown)
}

fn close(got: f64, want: f64, rel: f64) -> bool {
    (got - want).abs() <= rel * want.abs().max(1e-300) || got == want
}

/// Compiled tapes against hand-derived values and partials, and every slope rule against a
/// central difference: constants folded, the branch not taken dropped, a fixed exponent of a
/// negative base differentiated without its log.
#[test]
fn a_compiled_tape_gives_exact_partials() {
    let env = Env::default();
    let x = 0.6f64;
    let cases: &[(&str, [f64; 3], f64, [f64; 3], &str)] = &[
        ("v(a)*v(b)", [3.0, -2.0, 0.0], -6.0, [-2.0, 3.0, 0.0], "a multiplier: d(ab) = b da + a db"),
        ("v(a, b)^2", [3.0, 1.0, 0.0], 4.0, [4.0, -4.0, 0.0], "a differential square"),
        ("1e-14*exp(v(a)/0.025)", [x, 0.0, 0.0], 1e-14 * (x / 0.025).exp(), [1e-14 * (x / 0.025).exp() / 0.025, 0.0, 0.0], "an exponential diode"),
        ("v(a) > v(b) ? 2*v(a) : v(b)^3", [3.0, 1.0, 0.0], 6.0, [2.0, 0.0, 0.0], "the choice taken"),
        ("v(a) > v(b) ? 2*v(a) : v(b)^3", [1.0, 3.0, 0.0], 27.0, [0.0, 27.0, 0.0], "the other choice"),
        ("pow(v(a), 3)", [-2.0, 0.0, 0.0], -8.0, [12.0, 0.0, 0.0], "a fixed exponent of a negative base"),
        ("pow(2, v(a))", [3.0, 0.0, 0.0], 8.0, [8.0 * std::f64::consts::LN_2, 0.0, 0.0], "a varying exponent"),
        ("limit(v(a), 0, 1) + limit(v(b), 0, 1)", [0.5, 2.0, 0.0], 1.5, [1.0, 0.0, 0.0], "limit inside and outside its range"),
        ("gain*i(vs) + v(a, 0)", [1.0, 0.0, 0.25], 2.0, [1.0, 0.0, 4.0], "a branch current, a parameter and ground"),
        ("abs(v(a)) + uramp(v(b))", [-2.0, 3.0, 0.0], 5.0, [-1.0, 1.0, 0.0], "abs and uramp"),
    ];
    for &(text, [a, b, i], want, grad, law) in cases {
        let (v, g) = eval(text, a, b, i, env);
        assert!(close(v, want, 1e-14), "{law}: {text} = {v}, want {want}");
        for k in 0..3 {
            assert!(close(g[k], grad[k], 1e-14), "{law}: {text}: the partial by unknown {} is {}, want {}", k + 1, g[k], grad[k]);
        }
    }
    let time = Env { time: 0.25e-3, temper: 27.0 };
    let (v, _) = eval("sin(2*pi*1k*time) + temper", 0.0, 0.0, 0.0, time);
    assert!(close(v, 28.0, 1e-14), "time and temper are read from the environment: {v}");
    assert!(compile("sin(2*pi*1k*time)").reads_time(), "a tape knows it reads the time");

    // folding: the constant part becomes one number, the branch not taken and its unknown vanish
    assert_eq!(compile("2*3 + v(a)").len(), 3, "2*3 + v(a) is a number, an input and a sum");
    assert_eq!(compile("1 ? v(a) : v(b)").inputs(), &[1], "a fixed choice keeps only its branch");
    assert_eq!(compile("v(a)*v(a) + v(a)").inputs(), &[1], "an unknown read twice is one input");

    // each slope rule against a central difference
    let unary = [
        "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "ln", "log", "log10", "sqrt", "abs", "uramp",
    ];
    let fd = |text: &str, a: f64, b: f64| {
        let h = 1e-6;
        let at = |a, b| eval(text, a, b, 0.0, env).0;
        [(at(a + h, b) - at(a - h, b)) / (2.0 * h), (at(a, b + h) - at(a, b - h)) / (2.0 * h)]
    };
    let mut texts: Vec<(String, f64, f64)> = unary.iter().map(|f| (format!("{f}(v(a))"), 0.3, 0.0)).collect();
    texts.push(("abs(v(a))".into(), -0.7, 0.0));
    texts.push(("v(a)/v(b)".into(), 1.5, -2.5));
    texts.push(("pow(v(a), v(b))".into(), 1.5, 2.5));
    texts.push(("pwr(v(a), v(b))".into(), -1.5, 2.5));
    texts.push(("min(v(a), v(b)) + 3*max(v(a), v(b))".into(), 1.5, 2.5));
    texts.push(("v(a) - v(b)*v(a)^2 + -v(b)".into(), 1.5, 2.5));
    for (text, a, b) in texts {
        let (_, g) = eval(&text, a, b, 0.0, env);
        let want = fd(&text, a, b);
        for k in 0..2 {
            assert!((g[k] - want[k]).abs() <= 1e-7 * want[k].abs().max(1.0), "{text}: the partial by unknown {} is {}, a central difference {}", k + 1, g[k], want[k]);
        }
    }
}

/// The steps a tape watches: their states change exactly when a choice does, and the crossing
/// lies where each linearly interpolated margin meets its edge.
#[test]
fn a_step_s_crossing_lies_where_its_margin_meets_the_edge() {
    let env = Env::default();
    let at = |tape: &Tape, a: f64| {
        let mut s = Scratch::default();
        s.fit(tape);
        tape.value(&[0.0, a, 0.0, 0.0], env, &mut s);
        let (mut states, mut margins) = (vec![0.0; tape.steps()], vec![0.0; tape.steps()]);
        tape.branches(&s, &mut states);
        tape.margins(&s, &mut margins);
        (states, margins)
    };
    let cases: &[(&str, f64, f64, Option<f64>, &str)] = &[
        ("v(a) > 0.5 ? 5 : 0", 0.2, 0.8, Some(0.5), "a comparator rising through its threshold"),
        ("v(a) > 0.5 ? 5 : 0", 0.8, 0.6, None, "a comparator that stays high"),
        ("v(a) <= 1 ? v(a) : 1", 2.0, 0.0, Some(0.5), "a comparison falling through its edge"),
        ("floor(v(a))", 0.5, 2.5, Some(0.25), "floor rising through 1"),
        ("ceil(v(a))", 2.5, 0.5, Some(0.25), "ceil falling through 2"),
        ("u(v(a))", -1.0, 3.0, Some(0.25), "the unit step"),
        ("sgn(v(a) - 1)", 3.0, -1.0, Some(0.5), "sgn falling through 0"),
    ];
    for &(text, a0, a1, want, law) in cases {
        let tape = compile(text);
        assert!(tape.steps() > 0, "{law}: {text} has steps");
        let ((s0, m0), (s1, m1)) = (at(&tape, a0), at(&tape, a1));
        let got = tape.crossing(&m0, &m1);
        assert_eq!(got.is_some(), want.is_some(), "{law}: {text} from {a0} to {a1}: crossing {got:?}, want {want:?}");
        if let (Some(got), Some(want)) = (got, want) {
            assert!((got - want).abs() < 1e-12, "{law}: {text}: crossing at {got}, want {want}");
        }
        let same = s0.iter().zip(&s1).all(|(x, y)| x.to_bits() == y.to_bits());
        assert_eq!(same, want.is_none(), "{law}: {text}: the states change exactly when a choice does");
    }
    let (s0, _) = at(&compile("v(a) ? 1 : 0"), 0.3);
    let (s1, _) = at(&compile("v(a) ? 1 : 0"), 0.7);
    assert_eq!(s0, s1, "a choice's state is its condition's truth, not its value");
    assert_eq!(compile("v(a) * 2").steps(), 0, "a smooth expression has no steps");
}
