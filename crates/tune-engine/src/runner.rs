//! Running a suite: every trial of every task, in isolated worktrees, the
//! agent then the check, and `run.json` in the run directory.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use sha2::{Digest, Sha256};

use crate::{
    Agent, AgentEnd, AgentRequest, AgentSpec, CommandSpec, DiffStat, EvalError, Outcome,
    RepositoryInfo, RunRecord, Task, TaskRef, TrialResult, git,
    process::{self, Exit, Invocation},
    time,
    workspace::Worktree,
};

/// Variables removed from the environment of setup and check commands, the
/// same the agent adapters remove: they would tie a project's test run to
/// the session that started the evaluation.
pub const SCRUBBED_ENV: &[&str] = crate::agent::REMOVED_ENV;

/// How a suite is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    /// The worktree root the run starts from.
    pub repo_root: PathBuf,
    /// The agent.
    pub agent: AgentSpec,
    /// Trials of each task, at least 1.
    pub trials: u32,
    /// Trials run at the same time, at least 1.
    pub jobs: u32,
    /// Where run directories go; each run gets `<out_root>/<run_id>/`.
    pub out_root: PathBuf,
    /// Keep each trial's worktree instead of removing it.
    pub keep_worktrees: bool,
    /// Print one line per finished trial on standard error.
    pub progress: bool,
}

/// The variable memprove (a memory tool a repository under test may have
/// installed as a hook) reads its configuration from; every run points it
/// at an empty directory, so that a memprove an agent's hook starts finds
/// no installation and gives the trial no memory.
pub const AGENT_CONFIG_ENV: &str = "MEMPROVE_CONFIG_DIR";

/// The empty configuration directory of a run's agents, in the run
/// directory.
pub const AGENT_CONFIG_DIR: &str = "agent-config";

/// The default `out_root` of `repo_root`: `tune/runs` in its Git
/// common directory, so that run files are never untracked files of any
/// worktree.
///
/// # Errors
/// When `repo_root` is not in a Git worktree.
pub fn default_out_root(repo_root: &Path) -> Result<PathBuf, EvalError> {
    let common = git::git(repo_root, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() {
        common
    } else {
        repo_root.join(common)
    };
    Ok(common.join("tune").join("runs"))
}

/// The run directory `name` names under `out_root`: `latest` (or `None`)
/// is the one with the greatest name that holds a `run.json`; anything
/// else is a path.
///
/// # Errors
/// [`EvalError::Invalid`] when there is no run yet.
pub fn resolve_run_dir(out_root: &Path, name: Option<&str>) -> Result<PathBuf, EvalError> {
    match name {
        Some(name) if name != "latest" => Ok(PathBuf::from(name)),
        _ => {
            let mut runs: Vec<PathBuf> = fs::read_dir(out_root)
                .map_err(|_| no_runs(out_root))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|dir| dir.join("run.json").is_file())
                .collect();
            runs.sort();
            runs.pop().ok_or_else(|| no_runs(out_root))
        }
    }
}

fn no_runs(out_root: &Path) -> EvalError {
    EvalError::invalid(None, format!("no run under {}", out_root.display()))
}

/// Reads `<run_dir>/run.json`.
///
/// # Errors
/// When it cannot be read or is not a valid run.
pub fn read_run(run_dir: &Path) -> Result<RunRecord, EvalError> {
    let path = run_dir.join("run.json");
    let text = fs::read_to_string(&path).map_err(|error| EvalError::io(&path, error))?;
    RunRecord::from_json(&text, Some(&path))
}

/// One trial to run.
struct Job<'a> {
    task: &'a Task,
    base: Result<String, String>,
    trial: u32,
}

/// What every trial of a run shares.
struct Context<'a> {
    options: &'a RunOptions,
    agent: &'a dyn Agent,
    run_dir: &'a Path,
    worktrees: &'a Path,
    /// The run's task files and check patches, relative to the repository
    /// root: left out of every trial's working copy.
    hidden: Vec<PathBuf>,
    /// The agent's added variables.
    agent_env: Vec<(String, String)>,
}

/// Runs `tasks` with `agent` and writes `<out_root>/<run_id>/run.json`.
///
/// # Errors
/// When the run directory cannot be written or the repository cannot be
/// read; a failing trial is a result, not an error.
pub fn run_suite(
    options: &RunOptions,
    tasks: &[(Task, TaskRef)],
    agent: &dyn Agent,
) -> Result<(RunRecord, PathBuf), EvalError> {
    let started = SystemTime::now();
    let root = &options.repo_root;
    let head = git::commit(root, "HEAD")?;
    let run_id = format!("{}-{}", time::compact(started), nonce(started));
    let run_dir = options.out_root.join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|error| EvalError::io(&run_dir, error))?;
    let agent_config = run_dir.join(AGENT_CONFIG_DIR);
    create_private_dir(&agent_config)?;
    let worktrees = std::env::temp_dir().join(format!("tune-{run_id}"));
    let jobs: Vec<Job<'_>> = tasks
        .iter()
        .flat_map(|(task, _)| {
            let base = task.base.as_deref().map_or_else(
                || Ok(head.clone()),
                |rev| git::commit(root, rev).map_err(|error| error.to_string()),
            );
            (1..=options.trials.max(1)).map(move |trial| Job {
                task,
                base: base.clone(),
                trial,
            })
        })
        .collect();
    let context = Context {
        options,
        agent,
        run_dir: &run_dir,
        worktrees: &worktrees,
        hidden: hidden_paths(tasks),
        agent_env: vec![(
            AGENT_CONFIG_ENV.to_owned(),
            agent_config.to_string_lossy().into_owned(),
        )],
    };
    let mut results = run_jobs(&context, &jobs);
    results.sort_by_key(|(index, _)| *index);
    let _ = fs::remove_dir(&worktrees);
    let record = RunRecord {
        run_id,
        started_at: time::rfc3339(started),
        finished_at: time::rfc3339(SystemTime::now()),
        repository: RepositoryInfo {
            root: root.to_string_lossy().into_owned(),
            head,
        },
        agent: options.agent.clone(),
        agent_version: agent.version(),
        trials_per_task: options.trials.max(1),
        tasks: tasks
            .iter()
            .map(|(_, reference)| reference.clone())
            .collect(),
        results: results.into_iter().map(|(_, result)| result).collect(),
    };
    let path = run_dir.join("run.json");
    fs::write(&path, record.to_json()).map_err(|error| EvalError::io(&path, error))?;
    Ok((record, run_dir))
}

/// The run's task files and check patches, relative to the repository
/// root: kept out of every trial's working copy.
fn hidden_paths(tasks: &[(Task, TaskRef)]) -> Vec<PathBuf> {
    tasks
        .iter()
        .flat_map(|(task, _)| std::iter::once(task.source.clone()).chain(task.check_patch.clone()))
        .collect()
}

fn run_jobs(context: &Context<'_>, jobs: &[Job<'_>]) -> Vec<(usize, TrialResult)> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(jobs.len()));
    let workers = usize::try_from(context.options.jobs.max(1))
        .unwrap_or(1)
        .min(jobs.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(job) = jobs.get(index) else {
                        break;
                    };
                    let result = run_trial(context, job);
                    if context.options.progress {
                        eprintln!("{}", progress_line(&result));
                    }
                    results
                        .lock()
                        .expect("no trial panics while holding the lock")
                        .push((index, result));
                }
            });
        }
    });
    results.into_inner().expect("every worker has ended")
}

/// The line printed on standard error when a trial ends.
#[must_use]
pub fn progress_line(result: &TrialResult) -> String {
    let seconds = result.agent.as_ref().map_or(0, |agent| agent.duration_ms) / 1000;
    let mut line = format!(
        "memprove-tune: {} #{}: {} (agent {seconds} s",
        result.task,
        result.trial,
        result.outcome.name()
    );
    if let Some(cost) = result
        .agent
        .as_ref()
        .and_then(|agent| agent.usage.cost_microusd)
    {
        line.push_str(", ");
        line.push_str(&dollars(cost));
    }
    line.push(')');
    if let Some(reason) = &result.reason {
        line.push_str(": ");
        line.push_str(reason);
    }
    line
}

/// `cost` (millionths of a dollar) as `$0.41`.
#[must_use]
pub fn dollars(cost: u64) -> String {
    let cents = (cost + 5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn run_trial(context: &Context<'_>, job: &Job<'_>) -> TrialResult {
    let task = job.task;
    let logs = format!("trials/{}/{}", task.id, job.trial);
    let mut result = TrialResult {
        task: task.id.clone(),
        trial: job.trial,
        base: job.base.clone().unwrap_or_default(),
        outcome: Outcome::Error,
        reason: None,
        agent: None,
        check_exit: None,
        check_ms: 0,
        diff: DiffStat::default(),
        logs: logs.clone(),
    };
    let log_dir = context.run_dir.join(&logs);
    if let Err(error) = fs::create_dir_all(&log_dir) {
        result.reason = Some(format!("cannot create {}: {error}", log_dir.display()));
        return result;
    }
    let base = match &job.base {
        Ok(base) => base.clone(),
        Err(error) => {
            result.reason = Some(format!("the task's base names no commit: {error}"));
            return result;
        }
    };
    let name = format!("{}-{}", task.id, job.trial);
    let tree = match Worktree::isolated(
        &context.options.repo_root,
        context.worktrees,
        &name,
        &base,
        &context.hidden,
    ) {
        Ok(tree) => tree,
        Err(error) => {
            result.reason = Some(format!("cannot create the trial's worktree: {error}"));
            return result;
        }
    };
    in_worktree(context, task, &tree, &log_dir, &mut result);
    if !context.options.keep_worktrees
        && let Err(error) = tree.remove()
    {
        eprintln!("memprove-tune: {}: {error}", result.logs);
    }
    result
}

fn in_worktree(
    context: &Context<'_>,
    task: &Task,
    tree: &Worktree,
    log_dir: &Path,
    result: &mut TrialResult,
) {
    let env = vec![
        ("TUNE_TASK".to_owned(), task.id.clone()),
        ("TUNE_TRIAL".to_owned(), result.trial.to_string()),
    ];
    for (index, setup) in task.setup.iter().enumerate() {
        let stem = format!("setup-{}", index + 1);
        match command(setup, tree.path(), &env, log_dir, &stem) {
            Ok(finished) if finished.exit.success() => {}
            Ok(finished) => {
                result.outcome = Outcome::SetupFailed;
                result.reason = Some(format!(
                    "setup command {} ({}) {}",
                    index + 1,
                    setup.argv.join(" "),
                    ended(finished.exit, setup.timeout)
                ));
                return;
            }
            Err(error) => {
                result.outcome = Outcome::SetupFailed;
                result.reason = Some(format!("setup command {}: {error}", index + 1));
                return;
            }
        }
    }
    let outcome = context.agent.run(&AgentRequest {
        workdir: tree.path(),
        prompt: &task.prompt,
        timeout: task.agent_timeout,
        log_dir,
        env: &context.agent_env,
        pass_env: &context.options.agent.pass_env,
    });
    let failed_to_start = matches!(outcome.end, AgentEnd::FailedToStart(_));
    if let AgentEnd::FailedToStart(detail) = &outcome.end {
        result.reason = Some(format!("the agent could not be started: {detail}"));
    }
    let ended_by_signal = outcome.end.signal().map(|signal| match outcome.end {
        AgentEnd::Exited(code) => {
            format!("the agent exited with code {code}, as a program ended by signal {signal} does")
        }
        _ => format!("the agent was ended by signal {signal}"),
    });
    result.agent = Some(outcome);
    if failed_to_start {
        return;
    }
    match tree.diff(&log_dir.join("agent.diff")) {
        Ok(diff) => result.diff = diff,
        Err(error) => {
            result.reason = Some(format!("cannot diff the agent's changes: {error}"));
            return;
        }
    }
    // Whatever ended the agent, it was not the task: the check would judge
    // work that was cut short, so the trial is left without a verdict.
    if let Some(ended) = ended_by_signal {
        result.reason = Some(format!("{ended}; the trial has no verdict"));
        return;
    }
    if let Some(patch) = &task.check_patch
        && let Err(error) = tree.restore_and_apply(&context.options.repo_root.join(patch))
    {
        result.outcome = Outcome::PatchFailed;
        result.reason = Some(format!(
            "the check patch {} does not apply at the base: {}",
            patch.display(),
            first_line(&error.to_string())
        ));
        return;
    }
    match command(&task.check, tree.path(), &env, log_dir, "check") {
        Ok(finished) => {
            result.check_ms = time::millis(finished.duration);
            result.check_exit = finished.exit.code();
            result.outcome = match finished.exit {
                Exit::Code(0) => Outcome::Passed,
                Exit::TimedOut => Outcome::CheckTimeout,
                Exit::Code(_) | Exit::Signal(_) => Outcome::Failed,
            };
            if result.outcome != Outcome::Passed {
                result.reason = Some(format!(
                    "the check {}",
                    ended(finished.exit, task.check.timeout)
                ));
            }
        }
        Err(error) => {
            result.reason = Some(format!("the check could not be started: {error}"));
        }
    }
}

fn command(
    spec: &CommandSpec,
    cwd: &Path,
    env: &[(String, String)],
    log_dir: &Path,
    stem: &str,
) -> Result<process::Finished, EvalError> {
    let scrubbed: Vec<String> = SCRUBBED_ENV.iter().map(|name| (*name).to_owned()).collect();
    process::run(&Invocation {
        argv: &spec.argv,
        cwd,
        env,
        env_remove: &scrubbed,
        clear_env: false,
        stdout: log_dir.join(format!("{stem}.out")),
        stderr: log_dir.join(format!("{stem}.err")),
        timeout: spec.timeout,
    })
}

fn ended(exit: Exit, limit: Duration) -> String {
    match exit {
        Exit::Code(code) => format!("exited with code {code}"),
        Exit::Signal(signal) => format!("was ended by signal {signal}"),
        Exit::TimedOut => format!("ran past its limit of {} s", limit.as_secs()),
    }
}

/// Creates `path`, readable by its owner only.
fn create_private_dir(path: &Path) -> Result<(), EvalError> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .and_then(|()| fs::set_permissions(path, fs::Permissions::from_mode(0o700)))
        .map_err(|error| EvalError::io(path, error))
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// Six hex digits that make two runs started in the same second differ.
fn nonce(now: SystemTime) -> String {
    let nanos = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let digest = Sha256::digest(format!("{nanos}-{}", std::process::id()).as_bytes());
    crate::hex(&digest[..3])
}
