//! `tune ab`: the arms of an experiment, running them in turns through the
//! trial engine, continuing an experiment that was started before, and
//! stopping at the budget.

use std::{
    collections::BTreeMap,
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tune_engine::{
    AgentKind, AgentSpec, CommandSpec, Outcome, RunRecord, Task, TaskRef, agent_for,
    runner::{self, RunOptions, dollars},
    task,
};

use crate::{
    cli::print,
    error::Error,
    files::{self, FileMap},
    report, units,
    variant::{self, Change, Variant},
};

/// What `tune ab` was asked to do.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one switch of the command line"
)]
pub(crate) struct Options {
    /// The variants, in the order given.
    pub(crate) variants: Vec<Variant>,
    /// Task files or directories; empty for the default places.
    pub(crate) tasks: Vec<PathBuf>,
    /// Times each task runs under each arm.
    pub(crate) rounds: u32,
    /// The agent.
    pub(crate) agent: AgentSpec,
    /// What the agent is called in the plan and the report.
    pub(crate) agent_label: String,
    /// The most to spend, in millionths of a dollar.
    pub(crate) max_cost: Option<u64>,
    /// The caller's estimate of one trial's cost.
    pub(crate) cost_per_trial: Option<u64>,
    /// Trials at the same time.
    pub(crate) jobs: u32,
    /// Run the baseline's configuration a second time, to measure noise.
    pub(crate) control: bool,
    /// Run without asking.
    pub(crate) yes: bool,
    /// Print the plan and stop.
    pub(crate) dry_run: bool,
    /// Print the report as JSON.
    pub(crate) json: bool,
    /// Keep each trial's working copy.
    pub(crate) keep_worktrees: bool,
}

/// What an arm is in the experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Today's configuration.
    Baseline,
    /// Today's configuration a second time.
    Control,
    /// A changed configuration.
    Variant,
}

impl Role {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Control => "control",
            Self::Variant => "variant",
        }
    }
}

/// One configuration the agent runs under.
pub(crate) struct Arm {
    /// Its directory name.
    pub(crate) key: String,
    /// What the report calls it.
    pub(crate) label: String,
    pub(crate) role: Role,
    /// Its configuration files.
    pub(crate) files: FileMap,
    /// Whether it is the baseline with units dropped and nothing else.
    pub(crate) drops_only: bool,
    /// The tokens of its configuration that load in every session.
    pub(crate) config_tokens: u64,
}

/// One trial as the comparison needs it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cell {
    pub(crate) passed: bool,
    /// The trial could not be judged: not the agent's doing.
    pub(crate) error: bool,
    pub(crate) cost: Option<u64>,
    pub(crate) tokens: u64,
}

/// An arm's trials by task and round.
pub(crate) type Cells = BTreeMap<(String, u32), Cell>;

/// Everything the report is written from.
pub(crate) struct Finished<'a> {
    pub(crate) id: &'a str,
    pub(crate) dir: &'a Path,
    pub(crate) arms: &'a [Arm],
    /// The trials of each arm, in the order of `arms`.
    pub(crate) cells: &'a [Cells],
    pub(crate) tasks: usize,
    pub(crate) rounds: u32,
    /// Trials this invocation ran.
    pub(crate) trials_run: u64,
    /// Why it stopped before the end, when it did.
    pub(crate) stopped: Option<&'static str>,
    pub(crate) max_cost: Option<u64>,
    pub(crate) agent_label: &'a str,
    pub(crate) agent_version: Option<String>,
}

/// How long `tune apply` may take in a trial.
const APPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// A directory name for the arm `label` that no arm in `taken` has.
fn key_of(label: &str, taken: &[Arm]) -> String {
    let mut key = String::new();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            key.push(c.to_ascii_lowercase());
        } else if !key.ends_with('-') && !key.is_empty() {
            key.push('-');
        }
    }
    let mut key: String = key.trim_end_matches('-').chars().take(48).collect();
    if key.is_empty() {
        key.push_str("arm");
    }
    let base = key.clone();
    let mut repeat = 2;
    while taken.iter().any(|arm| arm.key == key) {
        key = format!("{base}-{repeat}");
        repeat += 1;
    }
    key
}

/// The arms: the baseline, the control, and one per variant.
fn arms(root: &Path, options: &Options) -> Result<Vec<Arm>, Error> {
    let config = files::read_config(root)?;
    let read = |path: &str| fs::read_to_string(root.join(path)).ok();
    let inventory = units::inventory(&config, &read);
    let tokens = |files: &FileMap| {
        units::inventory(files, &read)
            .units
            .iter()
            .map(|unit| unit.always_tokens)
            .sum()
    };
    let mut arms = vec![Arm {
        key: "baseline".to_owned(),
        label: "baseline".to_owned(),
        role: Role::Baseline,
        config_tokens: tokens(&config),
        files: config.clone(),
        drops_only: false,
    }];
    if options.control {
        arms.push(Arm {
            key: "control".to_owned(),
            label: "control".to_owned(),
            role: Role::Control,
            config_tokens: tokens(&config),
            files: config.clone(),
            drops_only: false,
        });
    }
    let today = files::digest(&config);
    for variant in &options.variants {
        let files = variant::changed(&config, &inventory, &variant.changes)?;
        if files::digest(&files) == today {
            return Err(Error::Usage(format!(
                "the variant `{}` changes nothing in the configuration",
                variant.label
            )));
        }
        if arms.iter().any(|arm| arm.label == variant.label) {
            return Err(Error::Usage(format!(
                "two arms are called `{}`",
                variant.label
            )));
        }
        arms.push(Arm {
            key: key_of(&variant.label, &arms),
            label: variant.label.clone(),
            role: Role::Variant,
            config_tokens: tokens(&files),
            files,
            drops_only: variant
                .changes
                .iter()
                .all(|change| matches!(change, Change::Drop(_))),
        });
    }
    Ok(arms)
}

/// The task files: those asked for, or the default places.
fn tasks(root: &Path, options: &Options) -> Result<Vec<(Task, TaskRef)>, Error> {
    let paths = if options.tasks.is_empty() {
        let found = [".tune/tasks", "evals"]
            .iter()
            .map(PathBuf::from)
            .find(|path| root.join(path).is_dir());
        vec![found.ok_or_else(|| {
            Error::Usage(
                "no task files: pass --tasks PATH, or put task files (docs/tasks.md says what one holds) under .tune/tasks"
                    .to_owned(),
            )
        })?]
    } else {
        options.tasks.clone()
    };
    for path in &paths {
        if !root.join(path).exists() {
            return Err(Error::Usage(format!(
                "there is no task file or directory `{}`",
                path.display()
            )));
        }
    }
    Ok(task::load_suite(root, &paths)?)
}

/// The name of the experiment: the same arms, tasks and agent give the
/// same name, so that running the command again continues it.
fn experiment_id(arms: &[Arm], tasks: &[(Task, TaskRef)], agent: &AgentSpec) -> String {
    let mut hasher = Sha256::new();
    let mut field = |text: &[u8]| {
        hasher.update(u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(text);
    };
    field(b"tune.experiment.v1");
    for arm in arms {
        field(arm.label.as_bytes());
        field(files::digest(&arm.files).as_bytes());
    }
    for (task, _) in tasks {
        field(task.id.as_bytes());
        field(&fs::read(&task.source).unwrap_or_default());
        if let Some(patch) = &task.check_patch {
            field(&fs::read(patch).unwrap_or_default());
        }
    }
    field(agent.kind.name().as_bytes());
    field(agent.model.as_deref().unwrap_or_default().as_bytes());
    for word in agent.command.iter().chain(&agent.extra_args) {
        field(word.as_bytes());
    }
    tune_engine::hex(&hasher.finalize())[..12].to_owned()
}

/// The finished run of one arm in one round, when there is one with a
/// result for every task.
fn finished_unit(unit: &Path, tasks: usize) -> Option<RunRecord> {
    let mut runs: Vec<PathBuf> = fs::read_dir(unit)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("run.json").is_file())
        .collect();
    runs.sort();
    let record = runner::read_run(&runs.pop()?).ok()?;
    (record.results.len() == tasks).then_some(record)
}

fn record_cells(record: &RunRecord, round: u32, cells: &mut Cells) {
    for result in &record.results {
        let usage = result.agent.as_ref().map(|agent| &agent.usage);
        cells.insert(
            (result.task.clone(), round),
            Cell {
                passed: result.outcome == Outcome::Passed,
                error: matches!(
                    result.outcome,
                    Outcome::Error | Outcome::SetupFailed | Outcome::PatchFailed
                ),
                cost: usage.and_then(|usage| usage.cost_microusd),
                tokens: usage.map_or(0, |usage| {
                    usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0)
                }),
            },
        );
    }
}

fn spent(cells: &[Cells]) -> u64 {
    cells
        .iter()
        .flat_map(BTreeMap::values)
        .filter_map(|cell| cell.cost)
        .sum()
}

/// The mean cost of the trials that reported one.
fn observed_cost(cells: &[Cells]) -> Option<u64> {
    let costs: Vec<u64> = cells
        .iter()
        .flat_map(BTreeMap::values)
        .filter_map(|cell| cell.cost)
        .collect();
    let count = u64::try_from(costs.len()).ok().filter(|count| *count > 0)?;
    Some(costs.iter().sum::<u64>() / count)
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// The plan a person reads before anything is spent.
fn plan(id: &str, arms: &[Arm], tasks: usize, options: &Options, cells: &[Cells]) -> String {
    let total = count(arms.len()) * count(tasks) * u64::from(options.rounds);
    let done: u64 = cells
        .iter()
        .map(|cells| {
            count(
                cells
                    .keys()
                    .filter(|(_, round)| *round <= options.rounds)
                    .count(),
            )
        })
        .sum();
    let left = total.saturating_sub(done);
    let labels: Vec<&str> = arms.iter().map(|arm| arm.label.as_str()).collect();
    let estimate = match observed_cost(cells).or(options.cost_per_trial) {
        Some(each) => format!("{} at {} a trial", dollars(each * left), dollars(each)),
        None if options.agent.kind == AgentKind::Command => {
            "unknown (the agent is a command; it may report its cost)".to_owned()
        }
        None => "unknown (pass --cost-per-trial for an estimate)".to_owned(),
    };
    let cap = options.max_cost.map_or_else(
        || "no cap".to_owned(),
        |cap| format!("cap {}", dollars(cap)),
    );
    format!(
        "Experiment {id}: {} arms x {tasks} tasks x {} rounds = {total} trials ({done} done, {left} to run)\n  arms: {}\n  agent: {}\n  estimated cost of what is left: {estimate}; {cap}\n",
        arms.len(),
        options.rounds,
        labels.join("; "),
        options.agent_label,
    )
}

/// Asks before spending, unless told not to.
fn confirm(options: &Options) -> Result<bool, Error> {
    if options.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Usage(
            "this would run the agent; pass --yes to run it without being asked, or --dry-run to see the plan"
                .to_owned(),
        ));
    }
    eprint!("Run it? [y/N] ");
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

/// An experiment that is set up: where it is and what it runs.
struct Experiment<'a> {
    root: PathBuf,
    /// Its directory under the repository's Git directory.
    dir: PathBuf,
    id: String,
    arms: Vec<Arm>,
    tasks: Vec<(Task, TaskRef)>,
    options: &'a Options,
}

impl Experiment<'_> {
    /// Where the run of `arm` in `round` goes.
    fn unit_dir(&self, arm: &Arm, round: u32) -> PathBuf {
        self.dir
            .join("runs")
            .join(&arm.key)
            .join(format!("r{round}"))
    }

    /// Where the configuration files of `arm` are kept for its trials.
    fn arm_files(&self, arm: &Arm) -> PathBuf {
        self.dir.join("arms").join(&arm.key).join("files")
    }

    /// The trials an earlier invocation finished.
    fn finished_cells(&self) -> Vec<Cells> {
        let mut cells: Vec<Cells> = vec![Cells::new(); self.arms.len()];
        for (index, arm) in self.arms.iter().enumerate() {
            for round in 1..=self.options.rounds {
                if let Some(record) = finished_unit(&self.unit_dir(arm, round), self.tasks.len()) {
                    record_cells(&record, round, &mut cells[index]);
                }
            }
        }
        cells
    }

    /// Writes each arm's configuration where its trials read it.
    fn write_arms(&self) -> Result<(), Error> {
        for arm in &self.arms {
            let files = self.arm_files(arm);
            let _ = fs::remove_dir_all(&files);
            fs::create_dir_all(&files).map_err(|error| Error::io(&files, error))?;
            files::write_tree(&arm.files, &files)?;
        }
        Ok(())
    }

    /// Runs every task once under `arm`: the trials of one round.
    fn run_unit(
        &self,
        arm: &Arm,
        round: u32,
        exe: &Path,
        agent: &dyn tune_engine::Agent,
    ) -> Result<RunRecord, Error> {
        let apply = CommandSpec {
            argv: vec![
                exe.to_string_lossy().into_owned(),
                "apply".to_owned(),
                "--files".to_owned(),
                self.arm_files(arm).to_string_lossy().into_owned(),
            ],
            timeout: APPLY_TIMEOUT,
        };
        let with_arm: Vec<(Task, TaskRef)> = self
            .tasks
            .iter()
            .cloned()
            .map(|(mut task, reference)| {
                task.setup.insert(0, apply.clone());
                (task, reference)
            })
            .collect();
        let run = RunOptions {
            repo_root: self.root.clone(),
            agent: self.options.agent.clone(),
            trials: 1,
            jobs: self.options.jobs,
            out_root: self.unit_dir(arm, round),
            keep_worktrees: self.options.keep_worktrees,
            progress: false,
        };
        Ok(runner::run_suite(&run, &with_arm, agent)?.0)
    }

    /// Runs what is left, round by round, and adds the trials to `cells`.
    /// Returns how many trials ran and why it stopped early, if it did.
    fn run_rounds(
        &self,
        agent: &dyn tune_engine::Agent,
        cells: &mut [Cells],
    ) -> Result<(u64, Option<&'static str>), Error> {
        let options = self.options;
        let exe = std::env::current_exe().map_err(|error| Error::io(Path::new("tune"), error))?;
        let mut trials_run = 0;
        for round in 1..=options.rounds {
            // The arms take turns going first, so that none always runs
            // when the machine or the service is at its best.
            for turn in 0..self.arms.len() {
                let index = (turn + round as usize - 1) % self.arms.len();
                let arm = &self.arms[index];
                if finished_unit(&self.unit_dir(arm, round), self.tasks.len()).is_some() {
                    continue;
                }
                let each = observed_cost(cells).or(options.cost_per_trial).unwrap_or(0);
                if let Some(cap) = options.max_cost
                    && spent(cells) + each * count(self.tasks.len()) > cap
                {
                    eprintln!(
                        "tune: stopped at the budget: {} spent, the next {} trials would pass the cap of {}",
                        dollars(spent(cells)),
                        self.tasks.len(),
                        dollars(cap)
                    );
                    return Ok((trials_run, Some("budget")));
                }
                let record = self.run_unit(arm, round, &exe, agent)?;
                record_cells(&record, round, &mut cells[index]);
                trials_run += count(record.results.len());
                let passed = record
                    .results
                    .iter()
                    .filter(|result| result.outcome == Outcome::Passed)
                    .count();
                eprintln!(
                    "tune: round {round}/{}, {}: {passed}/{} passed ({} spent so far)",
                    options.rounds,
                    arm.label,
                    record.results.len(),
                    dollars(spent(cells))
                );
            }
        }
        Ok((trials_run, None))
    }
}

/// Runs the experiment `options` describes in the repository that holds
/// `cwd` and writes the plan and the report to `out`.
pub(crate) fn run(options: &Options, cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let root = tune_engine::git::toplevel(cwd)?;
    let arms = arms(&root, options)?;
    let tasks = tasks(&root, options)?;
    let id = experiment_id(&arms, &tasks, &options.agent);
    let dir = runner::default_out_root(&root)?
        .parent()
        .map_or_else(|| root.join(".git/tune"), Path::to_path_buf)
        .join("experiments")
        .join(&id);
    let experiment = Experiment {
        root,
        dir,
        id,
        arms,
        tasks,
        options,
    };
    let mut cells = experiment.finished_cells();
    let plan = plan(
        &experiment.id,
        &experiment.arms,
        experiment.tasks.len(),
        options,
        &cells,
    );
    if options.json {
        eprint!("{plan}");
    } else {
        print(out, &plan)?;
    }
    if options.dry_run {
        return print(out, "Nothing was run (--dry-run).\n");
    }
    let agent = agent_for(&options.agent)?;
    let planned = experiment.arms.len() * experiment.tasks.len() * options.rounds as usize;
    let left = planned > cells.iter().map(BTreeMap::len).sum::<usize>();
    if left && !confirm(options)? {
        return print(out, "Nothing was run.\n");
    }
    experiment.write_arms()?;
    let (trials_run, stopped) = experiment.run_rounds(agent.as_ref(), &mut cells)?;

    let finished = Finished {
        id: &experiment.id,
        dir: &experiment.dir,
        arms: &experiment.arms,
        cells: &cells,
        tasks: experiment.tasks.len(),
        rounds: options.rounds,
        trials_run,
        stopped,
        max_cost: options.max_cost,
        agent_label: &options.agent_label,
        agent_version: agent.version(),
    };
    let json = report::json(&finished);
    let file = experiment.dir.join("report.json");
    fs::write(&file, &json).map_err(|error| Error::io(&file, error))?;
    print(
        out,
        &if options.json {
            json
        } else {
            report::text(&finished)
        },
    )
}
