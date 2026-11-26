//! The results of a run: `results.json` (the data), `results.md` (the
//! table people read and paste) and `gallery.html` (every case's contact
//! sheet, frames and verdict, opened from disk), plus comparisons of two
//! runs.

use crate::eval::CaseResult;
use crate::judge::CRITERIA;
use makepad_micro_serde::*;
use std::fmt::Write;

#[derive(Clone, Debug, Default, SerJson, DeJson)]
pub struct RunResults {
    /// The guide/prompt version under test.
    pub label: String,
    /// `backend:model` the generator was asked to use.
    pub model: String,
    /// `backend:model` of the judge, or empty when unjudged.
    pub judge: String,
    pub rubric_hash: String,
    /// Hash of the rubric's separate richness section, when judged.
    pub richness_hash: Option<String>,
    /// Unix seconds.
    pub started: u64,
    pub generator: String,
    pub results: Vec<CaseResult>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub runs: usize,
    pub succeeded: usize,
    pub judged: usize,
    pub mean_overall: f64,
    pub mean_visual: Option<f64>,
    pub mean_iterations: Option<f64>,
    pub mean_wall_s: Option<f64>,
    pub inspect_errors: u32,
    pub zone_violations: u32,
    pub failed_checks: usize,
    pub mean_density: Option<f64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub mean_richness: Option<f64>,
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (mut sum, mut n) = (0.0, 0usize);
    for v in values {
        sum += v;
        n += 1;
    }
    (n > 0).then(|| sum / n as f64)
}

pub fn summarize(results: &[CaseResult]) -> Summary {
    let ok: Vec<&CaseResult> = results.iter().filter(|r| r.success).collect();
    Summary {
        runs: results.len(),
        succeeded: ok.len(),
        judged: results.iter().filter(|r| r.judged()).count(),
        mean_overall: mean(results.iter().map(|r| r.overall)).unwrap_or(0.0),
        // Judged pictures of a failed generation are not the model's work:
        // only successful rows count toward the visual and richness means.
        mean_visual: mean(ok.iter().filter_map(|r| r.verdict.as_ref().map(|v| v.mean))),
        mean_iterations: mean(results.iter().filter_map(|r| r.iterations.map(|i| i as f64))),
        mean_wall_s: mean(results.iter().filter_map(|r| r.wall_s)),
        inspect_errors: ok.iter().filter_map(|r| r.inspect.as_ref()).map(|i| i.errors).sum(),
        zone_violations: ok.iter().filter_map(|r| r.inspect.as_ref()).map(|i| i.safe_zone + i.overflow).sum(),
        failed_checks: ok.iter().map(|r| r.failed_checks()).sum(),
        mean_density: mean(ok.iter().map(|r| r.motion.density)),
        tokens_in: results.iter().filter_map(|r| r.tokens_in).sum(),
        tokens_out: results.iter().filter_map(|r| r.tokens_out).sum(),
        mean_richness: mean(ok.iter().filter_map(|r| r.richness)),
    }
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("–".to_string(), |v| v.to_string())
}

fn opt1(v: Option<f64>) -> String {
    v.map_or("–".to_string(), |v| format!("{v:.1}"))
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn row_id(r: &CaseResult) -> String {
    if r.repeat == 0 {
        r.case.clone()
    } else {
        format!("{} #{}", r.case, r.repeat + 1)
    }
}

pub fn markdown(run: &RunResults) -> String {
    let mut md = String::new();
    let s = summarize(&run.results);
    let _ = writeln!(md, "# Video eval — {}\n", if run.label.is_empty() { "(no label)" } else { &run.label });
    let _ = writeln!(
        md,
        "Model `{}` · judge `{}` · rubric `{}` · generator `{}`\n",
        run.model,
        if run.judge.is_empty() { "none" } else { &run.judge },
        run.rubric_hash,
        run.generator
    );
    let _ = writeln!(
        md,
        "**{}/{} succeeded** · overall {:.1} · visual {} · iterations {} · wall {} s · inspect errors {} · safe/overflow {} · failed checks {} · motion {}/s · tokens {} in / {} out\n",
        s.succeeded,
        s.runs,
        s.mean_overall,
        s.mean_visual.map_or("–".into(), |v| format!("{v:.2}")),
        opt1(s.mean_iterations),
        opt1(s.mean_wall_s),
        s.inspect_errors,
        s.zone_violations,
        s.failed_checks,
        s.mean_density.map_or("–".into(), |v| format!("{v:.2}")),
        s.tokens_in,
        s.tokens_out
    );
    if let Some(m) = s.mean_richness {
        let _ = writeln!(
            md,
            "Visual richness (separate review, rubric `{}`; not in visual or overall): **{m:.2}** over {} cases\n",
            run.richness_hash.as_deref().unwrap_or(""),
            run.results.iter().filter(|r| r.richness.is_some()).count()
        );
    }
    let crit: Vec<String> = CRITERIA.iter().map(|c| c[..3].to_string()).collect();
    let _ = writeln!(
        md,
        "| case | format | ok | iters | wall s | tokens in/out | E/W | render s | safe/ovf | motion/s | clip/black % | {} | visual | checks | overall | rich |",
        crit.join(" | ")
    );
    let _ = writeln!(md, "|{}", "---|".repeat(15 + CRITERIA.len()));
    for r in &run.results {
        let (e, w, zone) = r.inspect.as_ref().map_or(("–".into(), "–".into(), "–".into()), |i| {
            (i.errors.to_string(), i.warnings.to_string(), format!("{}/{}", i.safe_zone, i.overflow))
        });
        let scores: Vec<String> =
            CRITERIA.iter().map(|c| r.verdict.as_ref().and_then(|v| v.score(c)).map_or("–".into(), |s| format!("{s}"))).collect();
        let checks = if r.conformance.is_empty() {
            "–".to_string()
        } else {
            let failed: Vec<&str> = r.conformance.iter().filter(|c| !c.pass).map(|c| c.name.as_str()).collect();
            if failed.is_empty() {
                "all".into()
            } else {
                format!("✗ {}", failed.join(", "))
            }
        };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {}/{} | {}/{} | {} | {} | {} | {} | {} | {} | {} | {:.0} | {} |",
            row_id(r),
            r.format,
            if r.success { "yes" } else { "**no**" },
            opt(r.iterations),
            opt1(r.wall_s),
            opt(r.tokens_in),
            opt(r.tokens_out),
            e,
            w,
            opt1(r.render_s),
            zone,
            if r.facts.is_some() { format!("{:.2}", r.motion.density) } else { "–".into() },
            r.exposure.as_ref().map_or("–".into(), |e| format!("{:.0}/{:.0}", e.clipped * 100.0, e.near_black * 100.0)),
            scores.join(" | "),
            r.verdict.as_ref().map_or("–".into(), |v| format!("{:.2}", v.mean)),
            checks,
            r.overall,
            r.richness.map_or("–".into(), |v| format!("{v}"))
        );
    }
    let accepted: Vec<&CaseResult> = run.results.iter().filter(|r| r.acceptance.is_some() || r.acceptance_error.is_some()).collect();
    if !accepted.is_empty() {
        let finished = accepted.iter().filter(|r| r.success).count();
        let passed = accepted.iter().filter(|r| acceptance_pass(r)).count();
        let _ = writeln!(
            md,
            "\n## Acceptance (design/REF-SYNTHESIS.md §2)\n\n**{}** · {finished}/{} runs finished (3/3 required) · {passed}/{} runs pass every item\n",
            if finished == accepted.len() && accepted.len() >= 3 && passed == accepted.len() { "PASS" } else { "FAIL" },
            accepted.len(),
            accepted.len()
        );
        let mut names: Vec<String> = Vec::new();
        for r in &accepted {
            for c in r.acceptance.iter().flatten() {
                if !names.contains(&c.name) {
                    names.push(c.name.clone());
                }
            }
        }
        let _ = writeln!(md, "| item | {} |", accepted.iter().map(|r| row_id(r)).collect::<Vec<_>>().join(" | "));
        let _ = writeln!(md, "|---|{}", "---|".repeat(accepted.len()));
        for name in &names {
            let cells: Vec<String> = accepted
                .iter()
                .map(|r| match r.acceptance.iter().flatten().find(|c| &c.name == name) {
                    Some(c) => format!("{} {}", if c.pass { "✓" } else { "✗" }, cell(&c.detail)),
                    None => "–".into(),
                })
                .collect();
            let _ = writeln!(md, "| {name} | {} |", cells.join(" | "));
        }
        for r in &accepted {
            if let Some(e) = &r.acceptance_error {
                let _ = writeln!(md, "\n- **{}** acceptance unjudged: {}", row_id(r), cell(e));
            }
        }
    }
    let notes: Vec<&CaseResult> = run
        .results
        .iter()
        .filter(|r| r.error.is_some() || r.verdict.as_ref().is_some_and(|v| !v.top_fix.is_empty()) || r.judge_error.is_some())
        .collect();
    if !notes.is_empty() {
        let _ = writeln!(md, "\n## Notes\n");
        for r in notes {
            if let Some(e) = &r.error {
                let _ = writeln!(md, "- **{}** failed: {}", row_id(r), cell(e));
                for d in &r.load_errors {
                    let _ = writeln!(md, "  - {}", cell(d));
                }
            }
            if let Some(v) = &r.verdict {
                if !v.top_fix.is_empty() {
                    let _ = writeln!(md, "- **{}** (brief: {}) top fix: {}", row_id(r), v.fulfils_brief, cell(&v.top_fix));
                }
            }
            if let Some(e) = &r.judge_error {
                let _ = writeln!(md, "- **{}** unjudged: {}", row_id(r), cell(e));
            }
        }
    }
    md
}

/// A run of the acceptance set passes when it finished and every item,
/// measured and judged, passed.
pub fn acceptance_pass(r: &CaseResult) -> bool {
    r.success && r.acceptance_error.is_none() && r.acceptance.as_ref().is_some_and(|l| !l.is_empty() && l.iter().all(|c| c.pass))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A self-contained page over the run directory's pictures.
pub fn gallery(run: &RunResults) -> String {
    let mut h = String::new();
    h.push_str(
        "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>Video eval</title>\
<style>:root{--bg:#f6f6f4;--fg:#1b1b1f;--muted:#6b6b75;--card:#fff;--line:#e2e2e6;--bad:#c62828;--good:#2e7d32}\
@media (prefers-color-scheme:dark){:root{--bg:#121216;--fg:#ececf1;--muted:#9a9aa6;--card:#1c1c22;--line:#2c2c35;--bad:#ef5350;--good:#66bb6a}}\
body{margin:0;padding:16px;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,sans-serif}\
h1{font-size:20px;margin:0 0 4px}.meta{color:var(--muted);margin-bottom:16px}\
.case{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:12px;margin:0 0 16px}\
.case h2{font-size:16px;margin:0 0 4px}.prompt{color:var(--muted);margin:0 0 8px}\
.pics{display:flex;gap:8px;overflow-x:auto}.pics figure{margin:0;flex:0 0 auto}.pics img{max-height:260px;max-width:90vw;border:1px solid var(--line);border-radius:4px}\
figcaption{font-size:12px;color:var(--muted)}table{border-collapse:collapse;margin-top:8px}td{padding:2px 10px 2px 0;vertical-align:top}\
.bad{color:var(--bad)}.good{color:var(--good)}</style>\n",
    );
    let s = summarize(&run.results);
    let _ = write!(
        h,
        "<h1>Video eval — {}</h1><div class=meta>model {} · judge {} · rubric {} · {}/{} succeeded · overall {:.1}</div>\n",
        esc(&run.label),
        esc(&run.model),
        esc(if run.judge.is_empty() { "none" } else { &run.judge }),
        esc(&run.rubric_hash),
        s.succeeded,
        s.runs,
        s.mean_overall
    );
    let cases = crate::bench::all_cases();
    for r in &run.results {
        let prompt = cases.iter().find(|c| c.id == r.case).map_or("", |c| c.prompt.as_str());
        let _ = write!(
            h,
            "<section class=case id=\"{}\"><h2>{} <span class={}>{}</span> · overall {:.0}</h2><p class=prompt>{} · {}</p>",
            esc(&row_id(r)),
            esc(&row_id(r)),
            if r.success { "good" } else { "bad" },
            if r.success { "ok" } else { "failed" },
            r.overall,
            esc(&r.format),
            esc(prompt)
        );
        if !r.images.is_empty() {
            h.push_str("<div class=pics>");
            for p in &r.images {
                let src = format!("{}/{}", r.dir, p.file);
                let _ = write!(h, "<figure><a href=\"{0}\"><img src=\"{0}\" alt=\"{1}\"></a><figcaption>{1}</figcaption></figure>", esc(&src), esc(&p.label));
            }
            h.push_str("</div>");
        }
        h.push_str("<table>");
        let mut row = |k: &str, v: String| {
            let _ = write!(h, "<tr><td>{}</td><td>{}</td></tr>", esc(k), v);
        };
        if let Some(e) = &r.error {
            row("error", format!("<span class=bad>{}</span>", esc(e)));
        }
        row(
            "generation",
            esc(&format!(
                "{} iterations · {} s · tokens {}/{} · {}",
                opt(r.iterations),
                opt1(r.wall_s),
                opt(r.tokens_in),
                opt(r.tokens_out),
                r.model_used.as_deref().unwrap_or("")
            )),
        );
        if let Some(i) = &r.inspect {
            row(
                "inspect",
                esc(&format!("{} errors · {} warnings · {} safe-zone · {} overflow {}", i.errors, i.warnings, i.safe_zone, i.overflow, i.first.join(" · "))),
            );
        }
        if let Some(e) = &r.inspect_error {
            row("inspect", format!("<span class=bad>{}</span>", esc(e)));
        }
        row("render", esc(&r.render_s.map_or_else(|| r.render_error.clone().unwrap_or("–".into()), |s| format!("{s:.1} s draft"))));
        row("motion", esc(&format!("{} animated layers · {:.2}/s", r.motion.animated_layers, r.motion.density)));
        if let Some(e) = &r.exposure {
            row(
                "exposure",
                esc(&format!(
                    "worst {:.1}% clipped at {:.2} s · worst {:.1}% near-black at {:.2} s · mean luma {:.2}",
                    e.clipped * 100.0,
                    e.clipped_at,
                    e.near_black * 100.0,
                    e.near_black_at,
                    e.mean_luma
                )),
            );
        }
        if !r.conformance.is_empty() {
            let checks: Vec<String> = r
                .conformance
                .iter()
                .map(|c| format!("<span class={}>{} {}</span> ({})", if c.pass { "good" } else { "bad" }, if c.pass { "✓" } else { "✗" }, esc(&c.name), esc(&c.detail)))
                .collect();
            row("checks", checks.join(" · "));
        }
        if let Some(v) = &r.verdict {
            for c in &v.criteria {
                row(&c.name, esc(&format!("{} — {}", c.score, c.note)));
            }
            row("brief", esc(&format!("{} · top fix: {}", v.fulfils_brief, v.top_fix)));
        }
        if let Some(e) = &r.judge_error {
            row("judge", format!("<span class=bad>{}</span>", esc(e)));
        }
        for c in r.acceptance.iter().flatten() {
            row(
                &format!("{} {}", if c.pass { "✓" } else { "✗" }, c.name),
                format!("<span class={}>{}</span>", if c.pass { "good" } else { "bad" }, esc(&c.detail)),
            );
        }
        if let Some(v) = r.richness {
            row("richness", esc(&format!("{v} — {}", r.richness_note.as_deref().unwrap_or(""))));
        }
        if let Some(e) = &r.richness_error {
            row("richness", format!("<span class=bad>{}</span>", esc(e)));
        }
        for e in &r.picture_errors {
            row("picture", format!("<span class=bad>{}</span>", esc(e)));
        }
        h.push_str("</table></section>\n");
    }
    h
}

/// Per case and overall deltas of `b` against `a`. Runs judged under
/// different rubrics or judges are flagged, not silently compared.
pub fn compare(a: &RunResults, b: &RunResults) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Compare `{}` → `{}`\n", a.label, b.label);
    if a.rubric_hash != b.rubric_hash || a.judge != b.judge {
        let _ = writeln!(
            md,
            "**Warning:** judged differently (rubric {} vs {}, judge `{}` vs `{}`); visual deltas are not comparable.\n",
            a.rubric_hash, b.rubric_hash, a.judge, b.judge
        );
    }
    let (sa, sb) = (summarize(&a.results), summarize(&b.results));
    let delta = |x: Option<f64>, y: Option<f64>| match (x, y) {
        (Some(x), Some(y)) => format!("{x:.2} → {y:.2} ({:+.2})", y - x),
        _ => format!("{} → {}", opt1(x), opt1(y)),
    };
    let _ = writeln!(md, "| metric | {} → {} |\n|---|---|", a.label, b.label);
    let _ = writeln!(md, "| succeeded | {}/{} → {}/{} |", sa.succeeded, sa.runs, sb.succeeded, sb.runs);
    let _ = writeln!(md, "| overall | {} |", delta(Some(sa.mean_overall), Some(sb.mean_overall)));
    let _ = writeln!(md, "| visual | {} |", delta(sa.mean_visual, sb.mean_visual));
    let _ = writeln!(md, "| iterations | {} |", delta(sa.mean_iterations, sb.mean_iterations));
    let _ = writeln!(md, "| wall s | {} |", delta(sa.mean_wall_s, sb.mean_wall_s));
    let _ = writeln!(md, "| inspect errors | {} → {} |", sa.inspect_errors, sb.inspect_errors);
    let _ = writeln!(md, "| safe/overflow | {} → {} |", sa.zone_violations, sb.zone_violations);
    let _ = writeln!(md, "| failed checks | {} → {} |", sa.failed_checks, sb.failed_checks);
    let _ = writeln!(md, "| motion/s | {} |", delta(sa.mean_density, sb.mean_density));
    let _ = writeln!(md, "| richness (separate) | {} |", delta(sa.mean_richness, sb.mean_richness));
    let _ = writeln!(md, "| tokens in/out | {}/{} → {}/{} |", sa.tokens_in, sa.tokens_out, sb.tokens_in, sb.tokens_out);
    let _ = writeln!(md, "\n| case | overall | visual |\n|---|---|---|");
    let mut ids: Vec<&str> = a.results.iter().chain(&b.results).map(|r| r.case.as_str()).collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        let of = |run: &RunResults| -> (Option<f64>, Option<f64>) {
            let rs: Vec<&CaseResult> = run.results.iter().filter(|r| r.case == id).collect();
            (mean(rs.iter().map(|r| r.overall)), mean(rs.iter().filter_map(|r| r.verdict.as_ref().map(|v| v.mean))))
        };
        let ((oa, va), (ob, vb)) = (of(a), of(b));
        let _ = writeln!(md, "| {id} | {} | {} |", delta(oa, ob), delta(va, vb));
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::{CriterionScore, Verdict};

    fn result(case: &str, success: bool, visual: Option<f64>, overall: f64) -> CaseResult {
        CaseResult {
            case: case.into(),
            success,
            overall,
            iterations: Some(2),
            verdict: visual.map(|m| Verdict {
                criteria: CRITERIA.iter().map(|c| CriterionScore { name: c.to_string(), score: m, note: "n|x".into() }).collect(),
                mean: m,
                top_fix: "bigger type".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn summary_and_table() {
        let run = RunResults {
            label: "guide-v1".into(),
            results: vec![
                CaseResult {
                    exposure: Some(crate::metrics::Exposure { clipped: 0.13, clipped_at: 1.6, near_black: 0.02, near_black_at: 0.0, mean_luma: 0.41 }),
                    ..result("a", true, Some(4.0), 75.0)
                },
                result("b", false, None, 0.0),
            ],
            ..Default::default()
        };
        let s = summarize(&run.results);
        assert_eq!((s.runs, s.succeeded, s.judged), (2, 1, 1));
        assert_eq!(s.mean_overall, 37.5);
        let md = markdown(&run);
        assert!(md.contains("**1/2 succeeded**"));
        assert!(md.contains("| a |") && md.contains("| **no** |"));
        assert!(md.contains("top fix: bigger type"));
        assert!(md.contains("| clip/black % |") && md.contains("| 13/2 |"), "the exposure column: {md}");
        let html = gallery(&run);
        assert!(html.contains("id=\"a\"") && html.contains("n|x"));
        assert!(html.contains("worst 13.0% clipped at 1.60 s · worst 2.0% near-black at 0.00 s · mean luma 0.41"), "{html}");
    }

    #[test]
    fn acceptance_section_needs_three_finished_passing_runs() {
        let pass = |ok: bool| CaseResult {
            case: "showreel-short".into(),
            success: true,
            acceptance: Some(vec![crate::metrics::Check { name: "hud".into(), pass: ok, detail: "x".into() }]),
            ..Default::default()
        };
        let mut run = RunResults { results: vec![pass(true), pass(true)], ..Default::default() };
        assert!(markdown(&run).contains("**FAIL** · 2/2 runs finished"));
        run.results.push(pass(true));
        for (i, r) in run.results.iter_mut().enumerate() {
            r.repeat = i as u32;
        }
        assert!(markdown(&run).contains("**PASS** · 3/3 runs finished"));
        run.results[1].acceptance.as_mut().unwrap()[0].pass = false;
        assert!(markdown(&run).contains("**FAIL**"));
    }

    #[test]
    fn compare_flags_different_judges() {
        let a = RunResults { label: "v1".into(), judge: "claude-cli:opus".into(), results: vec![result("a", true, Some(3.0), 50.0)], ..Default::default() };
        let b = RunResults { label: "v2".into(), judge: "claude-cli:sonnet".into(), results: vec![result("a", true, Some(4.0), 75.0)], ..Default::default() };
        let md = compare(&a, &b);
        assert!(md.contains("Warning"));
        assert!(md.contains("| a | 50.00 → 75.00 (+25.00) | 3.00 → 4.00 (+1.00) |"));
    }
}
