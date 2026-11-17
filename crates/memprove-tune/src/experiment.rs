//! An experiment: its arms, running them in turns through the trial
//! engine, continuing one that was started before, and stopping at the
//! budget. `memprove-tune ab` is one experiment; `memprove-tune prune` runs two.

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

/// What `memprove-tune ab` was asked to do; `memprove-tune prune` shares all but the
/// variants.
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
    /// How much lower a pass rate may truly be and still count as no
    /// worse, as a share of 1.
    pub(crate) margin: f64,
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
    /// The margin of "no worse".
    pub(crate) margin: f64,
    pub(crate) agent_label: &'a str,
    pub(crate) agent_version: Option<String>,
}

/// How long `memprove-tune apply` may take in a trial.
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

/// The tokens of the configuration `files` that load in every session.
pub(crate) fn always_tokens(root: &Path, files: &FileMap) -> u64 {
    let read = |path: &str| fs::read_to_string(root.join(path)).ok();
    units::inventory(files, &read)
        .units
        .iter()
        .map(|unit| unit.always_tokens)
        .sum()
}

/// The arms: the baseline, the control, and one per variant.
fn arms(root: &Path, variants: &[Variant], control: bool) -> Result<Vec<Arm>, Error> {
    let config = files::read_config(root)?;
    let read = |path: &str| fs::read_to_string(root.join(path)).ok();
    let inventory = units::inventory(&config, &read);
    let mut arms = vec![Arm {
        key: "baseline".to_owned(),
        label: "baseline".to_owned(),
        role: Role::Baseline,
        config_tokens: always_tokens(root, &config),
        files: config.clone(),
        drops_only: false,
    }];
    if control {
        arms.push(Arm {
            key: "control".to_owned(),
            label: "control".to_owned(),
            role: Role::Control,
            config_tokens: always_tokens(root, &config),
            files: config.clone(),
            drops_only: false,
        });
    }
    let today = files::digest(&config);
    for variant in variants {
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
            config_tokens: always_tokens(root, &files),
            files,
            drops_only: variant
                .changes
                .iter()
                .all(|change| matches!(change, Change::Drop(_))),
        });
    }
    Ok(arms)
}

/// Tasks as the engine takes them: each with where it came from.
pub(crate) type Suite = Vec<(Task, TaskRef)>;

/// The task files: those asked for, or the default places. With them,
/// the paths they were read from, as given.
pub(crate) fn load_tasks(root: &Path, options: &Options) -> Result<(Vec<String>, Suite), Error> {
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
    let named = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    Ok((named, task::load_suite(root, &paths)?))
}

/// The name of the experiment: the same arms, tasks and agent give the
/// same name, so that running the command again continues it.
fn experiment_id(
    root: &Path,
    arms: &[Arm],
    tasks: &[(Task, TaskRef)],
    agent: &AgentSpec,
) -> String {
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
        field(&fs::read(root.join(&task.source)).unwrap_or_default());
        if let Some(patch) = &task.check_patch {
            field(&fs::read(root.join(patch)).unwrap_or_default());
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

/// What the trials in `cells` cost, as far as they say.
pub(crate) fn spent(cells: &[Cells]) -> u64 {
    cells
        .iter()
        .flat_map(BTreeMap::values)
        .filter_map(|cell| cell.cost)
        .sum()
}

/// The mean cost of the trials that reported one.
pub(crate) fn observed_cost(cells: &[Cells]) -> Option<u64> {
    let costs: Vec<u64> = cells
        .iter()
        .flat_map(BTreeMap::values)
        .filter_map(|cell| cell.cost)
        .collect();
    let count = u64::try_from(costs.len()).ok().filter(|count| *count > 0)?;
    Some(costs.iter().sum::<u64>() / count)
}

pub(crate) fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Asks before spending, unless told not to.
pub(crate) fn confirm(options: &Options) -> Result<bool, Error> {
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
pub(crate) struct Experiment<'a> {
    root: PathBuf,
    /// Its directory under the repository's Git directory.
    pub(crate) dir: PathBuf,
    pub(crate) id: String,
    pub(crate) arms: Vec<Arm>,
    pub(crate) tasks: Vec<(Task, TaskRef)>,
    /// Files of the repository, beyond those of `tasks`, that no trial may
    /// see: task files and hidden tests that are not part of this
    /// experiment.
    hide: Vec<PathBuf>,
    options: &'a Options,
}

impl<'a> Experiment<'a> {
    /// Sets up the experiment of the baseline, the control when `control`
    /// says so, and `variants`, on `tasks`, in the repository at `root`.
    pub(crate) fn open(
        root: &Path,
        options: &'a Options,
        variants: &[Variant],
        control: bool,
        tasks: Vec<(Task, TaskRef)>,
        hide: Vec<PathBuf>,
    ) -> Result<Self, Error> {
        let arms = arms(root, variants, control)?;
        let id = experiment_id(root, &arms, &tasks, &options.agent);
        let dir = runner::default_out_root(root)?
            .parent()
            .map_or_else(|| root.join(".git/tune"), Path::to_path_buf)
            .join("experiments")
            .join(&id);
        Ok(Self {
            root: root.to_path_buf(),
            dir,
            id,
            arms,
            tasks,
            hide,
            options,
        })
    }

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

    /// The list of files no trial may see.
    fn hide_list(&self) -> PathBuf {
        self.dir.join("hidden.txt")
    }

    /// The trials an earlier invocation finished.
    pub(crate) fn finished_cells(&self) -> Vec<Cells> {
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

    /// The trials of the experiment, and how many of them `cells` holds.
    fn progress(&self, cells: &[Cells]) -> (u64, u64) {
        let total =
            count(self.arms.len()) * count(self.tasks.len()) * u64::from(self.options.rounds);
        let done: u64 = cells
            .iter()
            .map(|cells| {
                count(
                    cells
                        .keys()
                        .filter(|(_, round)| *round <= self.options.rounds)
                        .count(),
                )
            })
            .sum();
        (total, done)
    }

    /// Whether trials are still to run.
    pub(crate) fn left(&self, cells: &[Cells]) -> bool {
        let (total, done) = self.progress(cells);
        done < total
    }

    /// The plan a person reads before anything is spent. `cap` is the
    /// most this experiment may spend and `prior` the expected cost of a
    /// trial while none has reported one.
    pub(crate) fn plan(&self, cells: &[Cells], cap: Option<u64>, prior: Option<u64>) -> String {
        let options = self.options;
        let (total, done) = self.progress(cells);
        let left = total.saturating_sub(done);
        let labels: Vec<&str> = self.arms.iter().map(|arm| arm.label.as_str()).collect();
        let estimate = match observed_cost(cells).or(prior) {
            Some(each) => format!("{} at {} a trial", dollars(each * left), dollars(each)),
            None if options.agent.kind == AgentKind::Command => {
                "unknown (the agent is a command; it may report its cost)".to_owned()
            }
            None => "unknown (pass --cost-per-trial for an estimate)".to_owned(),
        };
        let cap = cap.map_or_else(
            || "no cap".to_owned(),
            |cap| format!("cap {}", dollars(cap)),
        );
        format!(
            "Experiment {}: {} arms x {} tasks x {} rounds = {total} trials ({done} done, {left} to run)\n  arms: {}\n  agent: {}\n  estimated cost of what is left: {estimate}; {cap}\n",
            self.id,
            self.arms.len(),
            self.tasks.len(),
            options.rounds,
            labels.join("; "),
            options.agent_label,
        )
    }

    /// Writes each arm's configuration, and the list of files to hide,
    /// where the trials read them.
    fn write_arms(&self) -> Result<(), Error> {
        for arm in &self.arms {
            let files = self.arm_files(arm);
            let _ = fs::remove_dir_all(&files);
            fs::create_dir_all(&files).map_err(|error| Error::io(&files, error))?;
            files::write_tree(&arm.files, &files)?;
        }
        let mut list = String::new();
        for path in &self.hide {
            list.push_str(&path.to_string_lossy());
            list.push('\n');
        }
        let file = self.hide_list();
        fs::write(&file, list).map_err(|error| Error::io(&file, error))
    }

    /// Runs every task once under `arm`: the trials of one round.
    fn run_unit(
        &self,
        arm: &Arm,
        round: u32,
        exe: &Path,
        agent: &dyn tune_engine::Agent,
    ) -> Result<RunRecord, Error> {
        let mut argv = vec![
            exe.to_string_lossy().into_owned(),
            "apply".to_owned(),
            "--files".to_owned(),
            self.arm_files(arm).to_string_lossy().into_owned(),
        ];
        if !self.hide.is_empty() {
            argv.push("--hide".to_owned());
            argv.push(self.hide_list().to_string_lossy().into_owned());
        }
        let apply = CommandSpec {
            argv,
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
    /// `cap` is the most this experiment may spend, `prior` the expected
    /// cost of a trial while none has reported one. Returns how many
    /// trials ran and why it stopped early, if it did.
    pub(crate) fn run_rounds(
        &self,
        agent: &dyn tune_engine::Agent,
        cells: &mut [Cells],
        cap: Option<u64>,
        prior: Option<u64>,
    ) -> Result<(u64, Option<&'static str>), Error> {
        let options = self.options;
        let exe = std::env::current_exe()
            .map_err(|error| Error::io(Path::new("memprove-tune"), error))?;
        self.write_arms()?;
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
                let each = observed_cost(cells).or(prior).unwrap_or(0);
                if let Some(cap) = cap
                    && spent(cells) + each * count(self.tasks.len()) > cap
                {
                    eprintln!(
                        "memprove-tune: stopped at the budget: {} spent, the next {} trials would pass the cap of {}",
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
                    "memprove-tune: round {round}/{}, {}: {passed}/{} passed ({} spent so far)",
                    options.rounds,
                    arm.label,
                    record.results.len(),
                    dollars(spent(cells))
                );
            }
        }
        Ok((trials_run, None))
    }

    /// What the report of this experiment is written from.
    pub(crate) fn finished<'b>(
        &'b self,
        cells: &'b [Cells],
        trials_run: u64,
        stopped: Option<&'static str>,
        cap: Option<u64>,
        agent_version: Option<String>,
    ) -> Finished<'b> {
        Finished {
            id: &self.id,
            dir: &self.dir,
            arms: &self.arms,
            cells,
            tasks: self.tasks.len(),
            rounds: self.options.rounds,
            trials_run,
            stopped,
            max_cost: cap,
            margin: self.options.margin,
            agent_label: &self.options.agent_label,
            agent_version,
        }
    }

    /// Writes the report of `finished` into the experiment's directory
    /// and returns it as JSON.
    pub(crate) fn save_report(&self, finished: &Finished<'_>) -> Result<String, Error> {
        let json = report::json(finished);
        let file = self.dir.join("report.json");
        fs::create_dir_all(&self.dir).map_err(|error| Error::io(&self.dir, error))?;
        fs::write(&file, &json).map_err(|error| Error::io(&file, error))?;
        Ok(json)
    }
}

/// Runs the experiment `options` describes in the repository that holds
/// `cwd` and writes the plan and the report to `out`.
pub(crate) fn run(options: &Options, cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let root = tune_engine::git::toplevel(cwd)?;
    let (_, tasks) = load_tasks(&root, options)?;
    let experiment = Experiment::open(
        &root,
        options,
        &options.variants,
        options.control,
        tasks,
        Vec::new(),
    )?;
    let mut cells = experiment.finished_cells();
    let plan = experiment.plan(&cells, options.max_cost, options.cost_per_trial);
    if options.json {
        eprint!("{plan}");
    } else {
        print(out, &plan)?;
    }
    if options.dry_run {
        return print(out, "Nothing was run (--dry-run).\n");
    }
    let agent = agent_for(&options.agent)?;
    if experiment.left(&cells) && !confirm(options)? {
        return print(out, "Nothing was run.\n");
    }
    let (trials_run, stopped) = experiment.run_rounds(
        agent.as_ref(),
        &mut cells,
        options.max_cost,
        options.cost_per_trial,
    )?;
    let finished = experiment.finished(
        &cells,
        trials_run,
        stopped,
        options.max_cost,
        agent.version(),
    );
    let json = experiment.save_report(&finished)?;
    print(
        out,
        &if options.json {
            json
        } else {
            report::text(&finished)
        },
    )
}
