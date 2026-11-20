//! What an experiment found, as text for a person and as JSON.

#![allow(
    clippy::cast_precision_loss,
    reason = "counts of trials and costs are far below 2^52"
)]

use std::fmt::Write as _;

use tune_engine::{json::Out, runner::dollars};

use crate::{
    experiment::{Arm, Cells, Finished, Role},
    scan::{counted, grouped},
    stats::{self, Paired, Verdict},
};

/// The schema of `memprove-tune ab --json` and of an experiment's `report.json`.
pub(crate) const AB_SCHEMA: &str = "tune.ab.v1";

/// One arm's totals.
pub(crate) struct Totals {
    pub(crate) trials: u64,
    pub(crate) passed: u64,
    /// Trials that could not be judged.
    pub(crate) errors: u64,
    pub(crate) cost: Option<u64>,
    pub(crate) tokens: u64,
}

pub(crate) fn totals(cells: &Cells) -> Totals {
    let mut totals = Totals {
        trials: 0,
        passed: 0,
        errors: 0,
        cost: None,
        tokens: 0,
    };
    for cell in cells.values() {
        totals.trials += 1;
        totals.passed += u64::from(cell.passed);
        totals.errors += u64::from(cell.error);
        totals.tokens += cell.tokens;
        if let Some(cost) = cell.cost {
            totals.cost = Some(totals.cost.unwrap_or(0) + cost);
        }
    }
    totals
}

/// An arm against the baseline.
pub(crate) struct Comparison<'a> {
    pub(crate) arm: &'a Arm,
    pub(crate) paired: Paired,
    /// The chance of a split this one-sided if the arm made no difference.
    p_value: Option<f64>,
    /// The most its pass rate can truly be below the baseline's.
    worst_loss: Option<f64>,
    verdict: Verdict,
    /// Its cost per trial less the baseline's, as a share of the
    /// baseline's, over the pairs that report both.
    cost_change: Option<f64>,
    /// Configuration tokens the baseline loads in every session and this
    /// arm does not.
    tokens_saved: i64,
}

impl Comparison<'_> {
    /// What to do with the units a drop-only arm leaves out.
    pub(crate) const fn advice(&self) -> Option<&'static str> {
        if !self.arm.drops_only {
            return None;
        }
        Some(match self.verdict {
            Verdict::Worse => "keep",
            Verdict::Better | Verdict::NoWorse => "removable",
            Verdict::Inconclusive => "inconclusive",
        })
    }
}

/// The trials of `cells` that could not be judged, by their tasks, and
/// by their rounds when there are several.
fn unjudged(cells: &Cells, rounds: u32) -> Vec<String> {
    cells
        .iter()
        .filter(|(_, cell)| cell.error)
        .map(|((task, round), _)| {
            if rounds > 1 {
                format!("{task} in round {round}")
            } else {
                task.clone()
            }
        })
        .collect()
}

/// The lines that name each arm's trials without a verdict, with an
/// empty line after them; nothing when every trial has one.
fn left_out(finished: &Finished<'_>) -> String {
    let mut out = String::new();
    for (arm, cells) in finished.arms.iter().zip(finished.cells) {
        let unjudged = unjudged(cells, finished.rounds);
        if !unjudged.is_empty() {
            let _ = writeln!(
                out,
                "  {}: {} without a verdict, left out of the pairs (run.json says why): {}",
                arm.label,
                counted(unjudged.len(), "trial"),
                unjudged.join(", ")
            );
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The pairs of `other` against `baseline`: the same task in the same
/// round, leaving out trials that could not be judged.
pub(crate) fn pair(baseline: &Cells, other: &Cells) -> (Paired, Option<f64>) {
    let mut paired = Paired::default();
    let (mut base_cost, mut other_cost) = (0_u64, 0_u64);
    for (key, base) in baseline {
        let Some(cell) = other.get(key) else {
            continue;
        };
        if base.error || cell.error {
            continue;
        }
        match (base.passed, cell.passed) {
            (true, true) => paired.both_pass += 1,
            (false, false) => paired.both_fail += 1,
            (true, false) => paired.only_baseline += 1,
            (false, true) => paired.only_variant += 1,
        }
        if let (Some(base), Some(cell)) = (base.cost, cell.cost) {
            base_cost += base;
            other_cost += cell;
        }
    }
    let change = (base_cost > 0).then(|| (other_cost as f64 - base_cost as f64) / base_cost as f64);
    (paired, change)
}

/// The control's pairs, when there is a control, and every variant
/// against the baseline, in the order of the arms.
pub(crate) fn comparisons<'a>(finished: &'a Finished<'_>) -> (Option<Paired>, Vec<Comparison<'a>>) {
    let baseline = &finished.cells[0];
    let control = finished
        .arms
        .iter()
        .position(|arm| arm.role == Role::Control)
        .map(|index| pair(baseline, &finished.cells[index]).0);
    let compared = finished
        .arms
        .iter()
        .zip(finished.cells)
        .filter(|(arm, _)| arm.role == Role::Variant)
        .map(|(arm, cells)| {
            let (paired, cost_change) = pair(baseline, cells);
            let (more, fewer) = if paired.only_baseline >= paired.only_variant {
                (paired.only_baseline, paired.only_variant)
            } else {
                (paired.only_variant, paired.only_baseline)
            };
            Comparison {
                arm,
                paired,
                p_value: (more + fewer > 0).then(|| stats::sign_test(more, fewer)),
                worst_loss: stats::worst_loss(&paired),
                verdict: stats::verdict(&paired, control.as_ref(), finished.margin),
                cost_change,
                tokens_saved: i64::try_from(finished.arms[0].config_tokens).unwrap_or(i64::MAX)
                    - i64::try_from(arm.config_tokens).unwrap_or(i64::MAX),
            }
        })
        .collect();
    (control, compared)
}

fn trials_done(finished: &Finished<'_>) -> u64 {
    finished.cells.iter().map(|cells| cells.len() as u64).sum()
}

fn trials_planned(finished: &Finished<'_>) -> u64 {
    finished.arms.len() as u64 * finished.tasks as u64 * u64::from(finished.rounds)
}

fn spent(finished: &Finished<'_>) -> u64 {
    finished
        .cells
        .iter()
        .flat_map(|cells| cells.values())
        .filter_map(|cell| cell.cost)
        .sum()
}

/// A p-value as the report prints it.
fn chance(p: f64) -> String {
    if p < 0.001 {
        "p < 0.001".to_owned()
    } else {
        format!("p = {p:.3}")
    }
}

fn percent(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

/// One sentence on what the pairs say about `comparison`'s arm.
fn sentence(comparison: &Comparison<'_>, rounds: u32) -> String {
    let drop = comparison.arm.drops_only;
    let p = comparison
        .p_value
        .map_or_else(String::new, |p| format!(" ({})", chance(p)));
    let bound = comparison.worst_loss.map_or_else(String::new, |loss| {
        format!(
            " (at most {:.1} points lower, 95% bound)",
            loss.max(0.0) * 100.0
        )
    });
    let saved = if comparison.tokens_saved > 0 {
        format!(
            "; each session carries about {} fewer configuration tokens",
            grouped(comparison.tokens_saved.unsigned_abs())
        )
    } else {
        String::new()
    };
    match (comparison.verdict, drop) {
        (Verdict::Worse, true) => format!("KEEP: the agent passes less often without it{p}."),
        (Verdict::Worse, false) => format!("WORSE: the agent passes less often with it{p}."),
        (Verdict::Better, true) => {
            format!("REMOVABLE: the agent passes more often without it{p}{saved}.")
        }
        (Verdict::Better, false) => format!("BETTER: the agent passes more often with it{p}."),
        (Verdict::NoWorse, true) => {
            format!("REMOVABLE: the agent passes as often without it{bound}{saved}.")
        }
        (Verdict::NoWorse, false) => format!("NO WORSE: the agent passes as often with it{bound}."),
        (Verdict::Inconclusive, _) => format!(
            "NOT ENOUGH EVIDENCE: {} pairs cannot tell a difference from none; more tasks or rounds can (--rounds {} continues this experiment).",
            comparison.paired.pairs(),
            rounds.saturating_mul(2)
        ),
    }
}

/// The report as text for a person.
pub(crate) fn text(finished: &Finished<'_>) -> String {
    let (control, compared) = comparisons(finished);
    let mut out = format!(
        "\nExperiment {}: {} arms x {} tasks x {} rounds = {} trials ({} done{})\nAgent: {}{}\n\n",
        finished.id,
        finished.arms.len(),
        finished.tasks,
        finished.rounds,
        trials_planned(finished),
        trials_done(finished),
        if finished.stopped.is_some() {
            ", stopped at the budget"
        } else {
            ""
        },
        finished.agent_label,
        finished
            .agent_version
            .as_ref()
            .map_or_else(String::new, |version| format!(" ({version})")),
    );
    let width = finished
        .arms
        .iter()
        .map(|arm| arm.label.chars().count())
        .max()
        .unwrap_or(0)
        .max(3);
    let _ = writeln!(
        out,
        "  {:<width$}  {:>9}  {:>6}  {:>10}  {:>12}  {:>13}",
        "ARM", "PASSED", "RATE", "COST/TRIAL", "TOKENS/TRIAL", "CONFIG TOKENS"
    );
    for (arm, cells) in finished.arms.iter().zip(finished.cells) {
        let totals = totals(cells);
        let judged = totals.trials - totals.errors;
        let rate = if judged == 0 {
            "-".to_owned()
        } else {
            percent(totals.passed as f64 / judged as f64)
        };
        let cost = match totals.cost {
            Some(cost) if totals.trials > 0 => dollars(cost / totals.trials),
            _ => "-".to_owned(),
        };
        let tokens = totals
            .tokens
            .checked_div(totals.trials)
            .map_or_else(|| "-".to_owned(), grouped);
        let _ = writeln!(
            out,
            "  {:<width$}  {:>9}  {rate:>6}  {cost:>10}  {tokens:>12}  {:>13}",
            arm.label,
            format!("{}/{judged}", totals.passed),
            grouped(arm.config_tokens),
        );
    }
    out.push('\n');
    out.push_str(&left_out(finished));
    for comparison in &compared {
        let paired = &comparison.paired;
        let _ = writeln!(
            out,
            "{} vs baseline: {} pairs (both passed {}, neither {}, only the baseline {}, only this arm {})",
            comparison.arm.label,
            paired.pairs(),
            paired.both_pass,
            paired.both_fail,
            paired.only_baseline,
            paired.only_variant,
        );
        let _ = writeln!(out, "  {}", sentence(comparison, finished.rounds));
        if let Some(change) = comparison.cost_change {
            let _ = writeln!(out, "  cost per trial: {:+.1}%", change * 100.0);
        }
    }
    if let Some(control) = control {
        let _ = writeln!(
            out,
            "control vs baseline (the same configuration twice): {} pairs, {} differ. A difference has to be larger than that to count.",
            control.pairs(),
            control.only_baseline + control.only_variant,
        );
    }
    let cap = finished
        .max_cost
        .map_or_else(String::new, |cap| format!(" of the {} cap", dollars(cap)));
    let _ = write!(
        out,
        "\nSpent {}{cap}. Runs, logs and report.json: {}\n",
        dollars(spent(finished)),
        finished.dir.display()
    );
    out
}

fn uint(value: u64) -> Out {
    Out::uint(value)
}

pub(crate) fn fraction(value: f64) -> Out {
    Out::Raw(format!("{value:.4}"))
}

pub(crate) fn paired_members(paired: &Paired) -> Vec<(&'static str, Out)> {
    vec![
        ("pairs", uint(paired.pairs())),
        ("both_pass", uint(paired.both_pass)),
        ("both_fail", uint(paired.both_fail)),
        ("only_baseline", uint(paired.only_baseline)),
        ("only_variant", uint(paired.only_variant)),
    ]
}

/// The report as one JSON document.
pub(crate) fn json(finished: &Finished<'_>) -> String {
    let (control, compared) = comparisons(finished);
    let arms = finished
        .arms
        .iter()
        .zip(finished.cells)
        .map(|(arm, cells)| {
            let totals = totals(cells);
            Out::Object(vec![
                ("key", Out::str(&arm.key)),
                ("label", Out::str(&arm.label)),
                ("role", Out::str(arm.role.name())),
                ("trials", uint(totals.trials)),
                ("passed", uint(totals.passed)),
                ("errors", uint(totals.errors)),
                ("cost_microusd", Out::opt(totals.cost, uint)),
                ("tokens", uint(totals.tokens)),
                ("config_tokens", uint(arm.config_tokens)),
            ])
        })
        .collect();
    let comparisons = compared
        .iter()
        .map(|comparison| {
            let mut members = vec![
                ("arm", Out::str(&comparison.arm.key)),
                ("label", Out::str(&comparison.arm.label)),
            ];
            members.extend(paired_members(&comparison.paired));
            members.extend([
                ("p_value", Out::opt(comparison.p_value, fraction)),
                ("worst_loss", Out::opt(comparison.worst_loss, fraction)),
                (
                    "verdict",
                    Out::str(match comparison.verdict {
                        Verdict::Worse => "worse",
                        Verdict::Better => "better",
                        Verdict::NoWorse => "no_worse",
                        Verdict::Inconclusive => "inconclusive",
                    }),
                ),
                ("advice", Out::opt(comparison.advice(), Out::str)),
                ("cost_change", Out::opt(comparison.cost_change, fraction)),
                ("config_tokens_saved", Out::int(comparison.tokens_saved)),
            ]);
            Out::Object(members)
        })
        .collect();
    let done = trials_done(finished);
    let mut document = Out::Object(vec![
        ("schema", Out::str(AB_SCHEMA)),
        ("experiment", Out::str(finished.id)),
        ("dir", Out::str(&finished.dir.to_string_lossy())),
        ("complete", Out::bool(done >= trials_planned(finished))),
        ("stopped", Out::opt(finished.stopped, Out::str)),
        ("tasks", uint(finished.tasks as u64)),
        ("rounds", uint(u64::from(finished.rounds))),
        ("trials_run", uint(finished.trials_run)),
        ("trials_done", uint(done)),
        ("spent_microusd", uint(spent(finished))),
        ("max_cost_microusd", Out::opt(finished.max_cost, uint)),
        (
            "agent",
            Out::Object(vec![
                ("name", Out::str(finished.agent_label)),
                (
                    "version",
                    Out::opt(finished.agent_version.as_deref(), Out::str),
                ),
            ]),
        ),
        ("significance", fraction(stats::ALPHA)),
        ("margin", fraction(finished.margin)),
        ("arms", Out::Array(arms)),
        ("comparisons", Out::Array(comparisons)),
        (
            "noise",
            Out::opt(control, |control| Out::Object(paired_members(&control))),
        ),
    ])
    .render();
    document.push('\n');
    document
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trial_without_a_verdict_is_named_with_its_round_when_there_are_several() {
        let cell = |error| crate::experiment::Cell {
            passed: false,
            error,
            cost: None,
            tokens: 0,
        };
        let cells: Cells = [
            (("t1".to_owned(), 1), cell(false)),
            (("t1".to_owned(), 2), cell(true)),
            (("t2".to_owned(), 1), cell(true)),
        ]
        .into_iter()
        .collect();
        assert_eq!(unjudged(&cells, 2), ["t1 in round 2", "t2 in round 1"]);
        assert_eq!(unjudged(&cells, 1), ["t1", "t2"]);
    }

    #[test]
    fn a_tiny_chance_is_not_printed_as_zero() {
        assert_eq!(chance(0.000_000_9), "p < 0.001");
        assert_eq!(chance(0.0156), "p = 0.016");
        assert_eq!(chance(1.0), "p = 1.000");
    }
}
