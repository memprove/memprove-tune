//! `tune prune`: every unit dropped alone against today's configuration,
//! then the units that are not needed dropped together and checked once
//! more, a patch that removes them, and the verdicts written down.

#![allow(
    clippy::cast_precision_loss,
    reason = "counts of trials are far below 2^52"
)]

use std::{
    fmt::Write as _,
    fs,
    io::Write,
    iter::once,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tune_engine::{agent_for, json::Out, runner::dollars};

use crate::{
    cli::print,
    error::Error,
    experiment::{self, Experiment, Suite, count},
    files, ledger, patch, report,
    scan::{counted, grouped},
    stats::{self, Paired},
    units::{self, Inventory, Load, Unit},
    variant::{self, Change, Variant},
};

/// The schema of `tune prune --json`.
pub(crate) const PRUNE_SCHEMA: &str = "tune.prune.v1";

/// Where the patch goes, from the repository root.
pub(crate) const PATCH_PATH: &str = ".tune/prune.patch";

/// What `tune prune` was asked to do.
pub(crate) struct Options {
    /// What it shares with `tune ab`; `rounds` is set here.
    pub(crate) run: experiment::Options,
    /// The units to test; empty for every unit the agent reads.
    pub(crate) units: Vec<String>,
    /// Test only this many, the largest.
    pub(crate) top: Option<u32>,
    /// Tasks to keep out of the search, for the final check.
    pub(crate) holdout: Option<u32>,
    /// Times each task runs under each arm; `None` for as many as a
    /// verdict needs.
    pub(crate) rounds: Option<u32>,
}

/// A unit and what dropping it alone showed.
struct Tested<'a> {
    unit: &'a Unit,
    paired: Paired,
    /// `keep`, `removable` or `inconclusive`.
    advice: &'static str,
}

/// The pruned configuration against the baseline, once more.
struct Check {
    /// Whether the tasks were kept out of the search.
    held_out: bool,
    tasks: usize,
    rounds: u32,
    paired: Paired,
    /// The experiment that holds the trials.
    experiment: String,
    /// Whether the search's own trials are the check.
    by_search: bool,
}

impl Check {
    const fn pruned_passed(&self) -> u64 {
        self.paired.both_pass + self.paired.only_variant
    }

    const fn baseline_passed(&self) -> u64 {
        self.paired.both_pass + self.paired.only_baseline
    }

    /// Whether the pruned configuration's pass rate, as seen, is within
    /// `margin` of the baseline's. A check without a pair that could be
    /// judged shows nothing, and does not hold.
    fn holds(&self, margin: f64) -> bool {
        let lost = self
            .paired
            .only_baseline
            .saturating_sub(self.paired.only_variant);
        self.paired.pairs() > 0 && lost as f64 <= margin * self.paired.pairs() as f64
    }

    /// Why there is no patch after a check that does not hold, for
    /// `units` units dropped together.
    fn failure(&self, units: usize) -> String {
        if self.paired.pairs() == 0 {
            "no trial of the final check could be judged; its report says what went wrong"
                .to_owned()
        } else if units > 1 {
            "removed together, the units lose the final check; each is removable alone, and `tune ab --variant` tests a combination".to_owned()
        } else {
            "on the held-out tasks the agent does worse without the unit".to_owned()
        }
    }
}

/// What came of it.
#[derive(Default)]
struct Pruned {
    /// The units to remove, by id.
    units: Vec<String>,
    /// Tokens every session carries less without them.
    tokens_saved: u64,
    check: Option<Check>,
    /// Whether the patch was written.
    patch: bool,
    /// Why there is no patch, when there is none.
    reason: Option<String>,
    /// Whether the verdicts were written.
    verdicts: bool,
}

/// Units that are not tested, each with the reason.
type NotTested<'a> = Vec<(&'a str, &'a str)>;

/// The units to test, largest first, and the units left out because the
/// agent does not read them.
fn candidates<'a>(
    inventory: &'a Inventory,
    options: &Options,
) -> Result<(Vec<&'a Unit>, NotTested<'a>), Error> {
    let mut not_tested = Vec::new();
    let mut units: Vec<&Unit> = Vec::new();
    if options.units.is_empty() {
        for unit in &inventory.units {
            match &unit.load {
                Load::NotRead(why) => not_tested.push((unit.id.as_str(), why.as_str())),
                Load::Always | Load::OnDemand(_) => units.push(unit),
            }
        }
    } else {
        for id in &options.units {
            let unit = inventory
                .units
                .iter()
                .find(|unit| unit.id == *id)
                .ok_or_else(|| {
                    Error::Usage(format!(
                        "the configuration has no unit `{id}`; `tune scan` lists the units"
                    ))
                })?;
            if !units.iter().any(|listed| listed.id == unit.id) {
                units.push(unit);
            }
        }
    }
    units.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.id.cmp(&b.id)));
    if let Some(top) = options.top {
        units.truncate(top as usize);
    }
    Ok((units, not_tested))
}

/// `tasks` as those to search on and those held out. Which are held out
/// depends on their names alone, so that it is the same every time.
fn split(tasks: Suite, holdout: Option<u32>) -> Result<(Suite, Suite), Error> {
    let all = tasks.len();
    let held = holdout.map_or(if all >= 4 { all / 4 } else { 0 }, |held| held as usize);
    if held >= all {
        return Err(Error::Usage(format!(
            "`--holdout {held}` leaves no task to search on: there are {all}"
        )));
    }
    let mut by_name: Vec<(String, &str)> = tasks
        .iter()
        .map(|(task, _)| {
            (
                tune_engine::hex(&Sha256::digest(task.id.as_bytes())),
                task.id.as_str(),
            )
        })
        .collect();
    by_name.sort_unstable();
    let chosen: Vec<String> = by_name
        .into_iter()
        .take(held)
        .map(|(_, id)| id.to_owned())
        .collect();
    Ok(tasks
        .into_iter()
        .partition(|(task, _)| !chosen.contains(&task.id)))
}

/// What is said when there is no unit to test.
fn nothing(inventory: &Inventory, not_tested: &[(&str, &str)]) -> String {
    if inventory.units.is_empty() {
        return "Nothing to prune: no CLAUDE.md or AGENTS.md, and nothing under .claude/rules or .claude/skills.\n".to_owned();
    }
    let names: Vec<&str> = not_tested.iter().map(|(id, _)| *id).collect();
    format!(
        "Nothing to prune: Claude Code reads none of the units ({}); `tune scan` says why.\n",
        names.join(", ")
    )
}

/// The first lines of the plan: what is tested, on what, and whether that
/// can give a verdict.
fn plan_head(
    units: &[&Unit],
    not_tested: &[(&str, &str)],
    held: &[String],
    search_tasks: usize,
    rounds: u32,
    margin: f64,
) -> String {
    let pairs = count(search_tasks) * u64::from(rounds);
    let needed = stats::pairs_needed(margin);
    let mut out = format!(
        "Prune: {}, each dropped alone, on {} x {} = {pairs} pairs a unit. A REMOVABLE verdict at a margin of {} points needs {needed} pairs that agree.",
        counted(units.len(), "unit"),
        counted(search_tasks, "task"),
        counted(rounds as usize, "round"),
        points(margin),
    );
    if pairs < needed {
        out.push_str(
            " This run cannot give one: it takes more rounds, more tasks or a wider --margin.",
        );
    }
    let listed: Vec<String> = units
        .iter()
        .map(|unit| format!("{} ({} tokens)", unit.id, grouped(unit.tokens)))
        .collect();
    let _ = write!(out, "\n  units, largest first: {}\n", listed.join("; "));
    if held.is_empty() {
        out.push_str("  no task is held out: the final check runs on the same tasks\n");
    } else {
        let _ = writeln!(out, "  held out for the final check: {}", held.join(", "));
    }
    if !not_tested.is_empty() {
        let names: Vec<&str> = not_tested.iter().map(|(id, _)| *id).collect();
        let _ = writeln!(
            out,
            "  not tested, because Claude Code does not read them: {}",
            names.join(", ")
        );
    }
    out
}

/// A margin in points, without a fraction when it has none.
fn points(margin: f64) -> String {
    let points = margin * 100.0;
    if (points - points.round()).abs() < 1e-9 {
        format!("{points:.0}")
    } else {
        format!("{points:.1}")
    }
}

/// What the cap can pay for, when the whole search costs more.
fn cap_note(
    units: usize,
    arms: usize,
    search_tasks: usize,
    rounds: u32,
    each: Option<u64>,
    cap: Option<u64>,
) -> String {
    let (Some(each), Some(cap)) = (each, cap) else {
        return String::new();
    };
    let an_arm = each * count(search_tasks) * u64::from(rounds);
    let all = an_arm * count(arms);
    if an_arm == 0 || all <= cap {
        return String::new();
    }
    let others = count(arms - units);
    let fits = (cap / an_arm).saturating_sub(others);
    if fits == 0 {
        format!(
            "  The cap fits none of the {units} units at this many rounds: one unit, with the baseline and the control, needs about {}.\n",
            dollars(an_arm * (others + 1))
        )
    } else {
        format!(
            "  The cap fits {fits} of the {units} units at this many rounds (--top {fits}); all of them need about {}.\n",
            dollars(all)
        )
    }
}

/// `names` as a person lists them: `a`, `a and b`, `a, b and c`.
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// Removes a patch an earlier run left.
fn remove_patch(root: &Path) -> Result<(), Error> {
    let file = root.join(PATCH_PATH);
    if file.exists() {
        fs::remove_file(&file).map_err(|error| Error::io(&file, error))?;
    }
    Ok(())
}

/// The sentence on the final check.
fn check_sentence(check: &Check) -> String {
    let on = if check.by_search {
        format!(
            "Checked by the search itself ({} x {})",
            counted(check.tasks, "task"),
            counted(check.rounds as usize, "round")
        )
    } else if check.held_out {
        format!(
            "Checked on {} held-out {} x {}",
            check.tasks,
            if check.tasks == 1 { "task" } else { "tasks" },
            counted(check.rounds as usize, "round")
        )
    } else {
        format!(
            "Checked on the same {} x {}",
            counted(check.tasks, "task"),
            counted(check.rounds as usize, "round")
        )
    };
    let held = if check.by_search {
        String::new()
    } else {
        format!(" (experiment {})", check.experiment)
    };
    format!(
        "{on}: the pruned configuration passed {} of {}, the baseline {} of {}{held}.",
        check.pruned_passed(),
        check.paired.pairs(),
        check.baseline_passed(),
        check.paired.pairs(),
    )
}

/// What follows the search's report, as text for a person.
fn text(
    tested: &[Tested<'_>],
    pruned: &Pruned,
    baseline: &report::Totals,
    spent_in_all: Option<(u64, Option<u64>)>,
) -> String {
    let mut out = String::new();
    let judged = baseline.trials - baseline.errors;
    if judged > 0 && baseline.passed * 2 < judged {
        let _ = writeln!(
            out,
            "Caution: the baseline passed only {} of {judged} trials. A unit cannot show its worth on a task the agent fails with or without it, so REMOVABLE says less here than it seems.",
            baseline.passed
        );
    }
    if !pruned.units.is_empty() {
        let _ = writeln!(
            out,
            "Pruned configuration: without {} ({} of {}; about {} fewer tokens in every session).",
            listed(&pruned.units),
            pruned.units.len(),
            counted(tested.len(), "unit"),
            grouped(pruned.tokens_saved),
        );
    }
    if let Some(check) = &pruned.check {
        let _ = writeln!(out, "{}", check_sentence(check));
    }
    if pruned.patch {
        let _ = writeln!(
            out,
            "Patch: {PATCH_PATH} (apply it with `git apply {PATCH_PATH}`)"
        );
    } else if let Some(reason) = &pruned.reason {
        let _ = writeln!(out, "No patch: {reason}.");
    }
    if pruned.verdicts {
        let _ = writeln!(
            out,
            "Verdicts: {} ({})",
            ledger::LEDGER_PATH,
            counted(tested.len(), "unit")
        );
    }
    if let Some((spent, cap)) = spent_in_all {
        let cap = cap.map_or_else(String::new, |cap| format!(" of the {} cap", dollars(cap)));
        let _ = writeln!(out, "Spent {}{cap} in all.", dollars(spent));
    }
    out
}

/// Everything the JSON report is written from.
struct Document<'a> {
    tested: &'a [Tested<'a>],
    not_tested: &'a [(&'a str, &'a str)],
    held: &'a [String],
    pruned: &'a Pruned,
    stopped: Option<&'static str>,
    margin: f64,
    rounds: u32,
    trials_run: u64,
    spent: u64,
    max_cost: Option<u64>,
    /// The search's own report, as JSON.
    search: &'a str,
}

/// The report as one JSON document.
fn json(document: &Document<'_>) -> String {
    let pruned = document.pruned;
    let units = document
        .tested
        .iter()
        .map(|tested| {
            let mut members = vec![
                ("unit", Out::str(&tested.unit.id)),
                ("tokens", Out::uint(tested.unit.tokens)),
                ("advice", Out::str(tested.advice)),
            ];
            members.extend(report::paired_members(&tested.paired));
            Out::Object(members)
        })
        .collect();
    let not_tested = document
        .not_tested
        .iter()
        .map(|(unit, why)| Out::Object(vec![("unit", Out::str(unit)), ("why", Out::str(why))]))
        .collect();
    let validation = Out::opt(pruned.check.as_ref(), |check| {
        Out::Object(vec![
            ("held_out", Out::bool(check.held_out)),
            ("tasks", Out::uint(count(check.tasks))),
            ("rounds", Out::uint(u64::from(check.rounds))),
            ("pairs", Out::uint(check.paired.pairs())),
            ("pruned_passed", Out::uint(check.pruned_passed())),
            ("baseline_passed", Out::uint(check.baseline_passed())),
            ("experiment", Out::str(&check.experiment)),
        ])
    });
    let needs_check = !pruned.units.is_empty();
    Out::Object(vec![
        ("schema", Out::str(PRUNE_SCHEMA)),
        (
            "complete",
            Out::bool(document.stopped.is_none() && (!needs_check || pruned.check.is_some())),
        ),
        ("stopped", Out::opt(document.stopped, Out::str)),
        ("margin", report::fraction(document.margin)),
        (
            "pairs_needed",
            Out::uint(stats::pairs_needed(document.margin)),
        ),
        ("rounds", Out::uint(u64::from(document.rounds))),
        ("units", Out::Array(units)),
        ("not_tested", Out::Array(not_tested)),
        ("held_out", Out::strs(document.held)),
        (
            "pruned",
            Out::Object(vec![
                ("units", Out::strs(&pruned.units)),
                ("tokens_saved", Out::uint(pruned.tokens_saved)),
                ("validation", validation),
                (
                    "patch",
                    Out::opt(pruned.patch.then_some(PATCH_PATH), Out::str),
                ),
                ("reason", Out::opt(pruned.reason.as_deref(), Out::str)),
            ]),
        ),
        (
            "verdicts_file",
            Out::opt(pruned.verdicts.then_some(ledger::LEDGER_PATH), Out::str),
        ),
        ("trials_run", Out::uint(document.trials_run)),
        ("spent_microusd", Out::uint(document.spent)),
        ("max_cost_microusd", Out::opt(document.max_cost, Out::uint)),
        (
            "search",
            Out::Raw(document.search.trim_end().replace('\n', "\n  ")),
        ),
    ])
    .render()
}

/// Prunes the configuration of the repository that holds `cwd` and writes
/// the plan and the report to `out`.
#[allow(
    clippy::too_many_lines,
    reason = "one pass from the plan to the report, in the order a person reads it"
)]
pub(crate) fn run(mut options: Options, cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let root = tune_engine::git::toplevel(cwd)?;
    let config = files::read_config(&root)?;
    let read = |path: &str| fs::read_to_string(root.join(path)).ok();
    let inventory = units::inventory(&config, &read);
    let (units, not_tested) = candidates(&inventory, &options)?;
    if units.is_empty() {
        return print(out, &nothing(&inventory, &not_tested));
    }

    let (task_paths, tasks) = experiment::load_tasks(&root, &options.run)?;
    let tasks_digest = ledger::tasks_digest(&root, &tasks);
    // No trial may see a task file or a hidden test, whichever of the two
    // experiments it belongs to.
    let hide: Vec<PathBuf> = tasks
        .iter()
        .flat_map(|(task, _)| once(task.source.clone()).chain(task.check_patch.clone()))
        .collect();
    let (search_tasks, held_tasks) = split(tasks, options.holdout)?;
    let held: Vec<String> = held_tasks.iter().map(|(task, _)| task.id.clone()).collect();
    let margin = options.run.margin;
    let rounds = options.rounds.unwrap_or_else(|| {
        let needed = stats::pairs_needed(margin).div_ceil(count(search_tasks.len()));
        u32::try_from(needed).unwrap_or(u32::MAX).max(1)
    });
    options.run.rounds = rounds;
    let options = options;
    let run = &options.run;
    let cap = run.max_cost;

    let variants: Vec<Variant> = units
        .iter()
        .map(|unit| Variant::of(Change::Drop(unit.id.clone())))
        .collect();
    let search = Experiment::open(&root, run, &variants, true, search_tasks, hide.clone())?;
    let mut cells = search.finished_cells();
    let mut plan = plan_head(
        &units,
        &not_tested,
        &held,
        search.tasks.len(),
        rounds,
        margin,
    );
    plan.push_str(&search.plan(&cells, cap, run.cost_per_trial));
    if cells.iter().all(std::collections::BTreeMap::is_empty) {
        plan.push_str(&cap_note(
            units.len(),
            search.arms.len(),
            search.tasks.len(),
            rounds,
            run.cost_per_trial,
            cap,
        ));
    }
    let check_tasks = if held.is_empty() {
        search.tasks.len()
    } else {
        held.len()
    };
    let check_trials = 2 * count(check_tasks) * u64::from(rounds);
    let check_cost = experiment::observed_cost(&cells)
        .or(run.cost_per_trial)
        .map_or_else(String::new, |each| {
            format!(", about {}", dollars(each * check_trials))
        });
    let _ = writeln!(
        plan,
        "Then the units that are not needed are dropped together and checked against the baseline on {}: up to {check_trials} more trials{check_cost}.",
        if held.is_empty() {
            "the same tasks"
        } else {
            "the held-out tasks"
        },
    );
    if run.json {
        eprint!("{plan}");
    } else {
        print(out, &plan)?;
    }
    if run.dry_run {
        return print(out, "Nothing was run (--dry-run).\n");
    }

    let agent = agent_for(&run.agent)?;
    let mut asked = false;
    if search.left(&cells) {
        if !experiment::confirm(run)? {
            return print(out, "Nothing was run.\n");
        }
        asked = true;
    }
    let (mut trials_run, mut stopped) =
        search.run_rounds(agent.as_ref(), &mut cells, cap, run.cost_per_trial)?;
    let finished = search.finished(&cells, trials_run, stopped, cap, agent.version());
    let search_json = search.save_report(&finished)?;
    let (_, compared) = report::comparisons(&finished);
    let tested: Vec<Tested<'_>> = units
        .iter()
        .zip(&compared)
        .map(|(unit, comparison)| Tested {
            unit,
            paired: comparison.paired,
            advice: comparison.advice().unwrap_or("inconclusive"),
        })
        .collect();

    let mut pruned = Pruned::default();
    let mut spent = experiment::spent(&cells);
    let mut second_ran = false;
    if stopped.is_some() {
        pruned.reason = Some(
            "stopped at the budget before every unit had its rounds; the same command with a larger --max-cost continues"
                .to_owned(),
        );
    } else {
        let entries: Vec<ledger::Entry> = tested
            .iter()
            .map(|tested| ledger::Entry {
                unit: tested.unit.id.clone(),
                sha256: tested.unit.sha256.clone(),
                advice: tested.advice.to_owned(),
                both_pass: tested.paired.both_pass,
                both_fail: tested.paired.both_fail,
                only_baseline: tested.paired.only_baseline,
                only_variant: tested.paired.only_variant,
                margin,
                config: files::digest(&config),
                tasks: tasks_digest.clone(),
                task_paths: task_paths.clone(),
                agent: run.agent_label.clone(),
                agent_version: agent.version(),
                experiment: search.id.clone(),
            })
            .collect();
        ledger::write(
            &root,
            &ledger::merged(ledger::read(&root)?, &entries, &inventory),
        )?;
        pruned.verdicts = true;

        let removable: Vec<&Tested<'_>> = {
            let mut removable: Vec<&Tested<'_>> = tested
                .iter()
                .filter(|tested| tested.advice == "removable")
                .collect();
            removable.sort_by(|a, b| a.unit.id.cmp(&b.unit.id));
            removable
        };
        if removable.is_empty() {
            pruned.reason = Some("no unit is removable on this evidence".to_owned());
            remove_patch(&root)?;
        } else {
            pruned.units = removable
                .iter()
                .map(|tested| tested.unit.id.clone())
                .collect();
            let drops: Vec<Change> = pruned.units.iter().cloned().map(Change::Drop).collect();
            let without = variant::changed(&config, &inventory, &drops)?;
            pruned.tokens_saved = experiment::always_tokens(&root, &config)
                .saturating_sub(experiment::always_tokens(&root, &without));

            if held_tasks.is_empty() && removable.len() == 1 {
                // The one arm that drops it is the pruned configuration.
                pruned.check = Some(Check {
                    held_out: false,
                    tasks: search.tasks.len(),
                    rounds,
                    paired: removable[0].paired,
                    experiment: search.id.clone(),
                    by_search: true,
                });
            } else {
                let held_out = !held_tasks.is_empty();
                let on = if held_out {
                    held_tasks
                } else {
                    search.tasks.clone()
                };
                let variant = Variant {
                    label: "pruned".to_owned(),
                    changes: drops,
                };
                let second = Experiment::open(&root, run, &[variant], false, on, hide)?;
                let mut second_cells = second.finished_cells();
                if second.left(&second_cells) && !asked && !experiment::confirm(run)? {
                    pruned.reason = Some("the final check was not run".to_owned());
                } else {
                    let (ran, second_stopped) = second.run_rounds(
                        agent.as_ref(),
                        &mut second_cells,
                        cap.map(|cap| cap.saturating_sub(spent)),
                        experiment::observed_cost(&cells).or(run.cost_per_trial),
                    )?;
                    second_ran = true;
                    trials_run += ran;
                    spent += experiment::spent(&second_cells);
                    let done =
                        second.finished(&second_cells, ran, second_stopped, cap, agent.version());
                    second.save_report(&done)?;
                    if second_stopped.is_some() {
                        stopped = second_stopped;
                        pruned.reason = Some(
                            "stopped at the budget before the final check was done; the same command with a larger --max-cost continues"
                                .to_owned(),
                        );
                    } else {
                        pruned.check = Some(Check {
                            held_out,
                            tasks: second.tasks.len(),
                            rounds,
                            paired: report::pair(&second_cells[0], &second_cells[1]).0,
                            experiment: second.id.clone(),
                            by_search: false,
                        });
                    }
                }
            }

            match &pruned.check {
                Some(check) if check.holds(margin) => {
                    let bytes = patch::between(&config, &without, &search.dir.join("patch-work"))?;
                    let file = root.join(PATCH_PATH);
                    if let Some(dir) = file.parent() {
                        fs::create_dir_all(dir).map_err(|error| Error::io(dir, error))?;
                    }
                    fs::write(&file, bytes).map_err(|error| Error::io(&file, error))?;
                    pruned.patch = true;
                }
                Some(check) => {
                    pruned.reason = Some(check.failure(pruned.units.len()));
                    remove_patch(&root)?;
                }
                None => {}
            }
        }
    }

    let report = if run.json {
        json(&Document {
            tested: &tested,
            not_tested: &not_tested,
            held: &held,
            pruned: &pruned,
            stopped,
            margin,
            rounds,
            trials_run,
            spent,
            max_cost: cap,
            search: &search_json,
        })
    } else {
        let mut report = report::text(&finished);
        report.push('\n');
        report.push_str(&text(
            &tested,
            &pruned,
            &report::totals(&cells[0]),
            second_ran.then_some((spent, cap)),
        ));
        report
    };
    print(out, &report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_note_says_how_many_units_the_money_pays_for() {
        // 4 arms (two units) x 2 tasks x 15 rounds at $0.50: $15 an arm.
        let note = |cap: u64| cap_note(2, 4, 2, 15, Some(500_000), Some(cap * 1_000_000));
        assert!(
            note(20).contains("fits none of the 2 units"),
            "{}",
            note(20)
        );
        assert!(note(20).contains("$45.00"), "{}", note(20));
        assert!(note(50).contains("fits 1 of the 2 units"), "{}", note(50));
        assert!(note(50).contains("--top 1"), "{}", note(50));
        assert!(note(50).contains("$60.00"), "{}", note(50));
        assert_eq!(note(60), "");
        assert_eq!(cap_note(2, 4, 2, 15, None, Some(1)), "");
        assert_eq!(cap_note(2, 4, 2, 15, Some(500_000), None), "");
    }

    #[test]
    fn the_final_check_allows_a_loss_within_the_margin_as_seen() {
        let check = |only_baseline: u64, only_variant: u64, both_pass: u64| Check {
            held_out: true,
            tasks: 1,
            rounds: 1,
            paired: Paired {
                both_pass,
                both_fail: 0,
                only_baseline,
                only_variant,
            },
            experiment: String::new(),
            by_search: false,
        };
        assert!(check(0, 0, 6).holds(0.10));
        // One loss in six is more than ten points.
        assert!(!check(1, 0, 5).holds(0.10));
        assert!(check(1, 1, 4).holds(0.10));
        assert!(check(3, 0, 27).holds(0.10));
        assert!(!check(4, 0, 26).holds(0.10));
        assert!(check(1, 0, 5).holds(0.20));
        // No pair that could be judged is no evidence, not a pass.
        assert!(!check(0, 0, 0).holds(0.10));
        assert_eq!(check(2, 1, 3).pruned_passed(), 4);
        assert_eq!(check(2, 1, 3).baseline_passed(), 5);
    }

    #[test]
    fn a_check_that_fails_says_why() {
        let check = |only_baseline: u64, both_pass: u64| Check {
            held_out: true,
            tasks: 1,
            rounds: 1,
            paired: Paired {
                both_pass,
                both_fail: 0,
                only_baseline,
                only_variant: 0,
            },
            experiment: String::new(),
            by_search: false,
        };
        assert!(check(0, 0).failure(2).contains("could be judged"));
        assert!(check(3, 3).failure(2).contains("together"));
        assert!(check(3, 3).failure(1).contains("held-out"));
    }

    #[test]
    fn units_are_listed_as_a_person_would() {
        let names = |count: usize| -> Vec<String> {
            ["a", "b", "c"][..count]
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        };
        assert_eq!(listed(&names(1)), "a");
        assert_eq!(listed(&names(2)), "a and b");
        assert_eq!(listed(&names(3)), "a, b and c");
    }

    #[test]
    fn a_margin_is_named_in_points() {
        assert_eq!(points(0.10), "10");
        assert_eq!(points(0.125), "12.5");
    }
}
