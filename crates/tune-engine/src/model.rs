//! The records every part of the crate shares: a task, an agent's run, one
//! trial's result, and a run of a suite. `run.json` is the
//! [`RunRecord`] rendered by [`RunRecord::to_json`]; whatever compares
//! runs reads it back with [`RunRecord::from_json`].

use std::{path::PathBuf, time::Duration};

use crate::{
    EvalError,
    json::{JsonValue, Out, Reader},
};

/// The `schema` of a task file.
pub const TASK_SCHEMA: &str = "tune.task.v1";
/// The `schema` of `run.json`.
pub const RUN_SCHEMA: &str = "tune.run.v1";
/// The `schema` of a task file written for `memprove eval`, read as a
/// task file of this schema's members: a repository that already has such
/// tasks needs no second copy of them.
pub const LEGACY_TASK_SCHEMA: &str = "memprove.eval.task.v1";

/// A command of a task: setup or check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program and arguments; never empty. Run without a shell; write
    /// `["sh", "-c", "..."]` for one.
    pub argv: Vec<String>,
    /// Time limit.
    pub timeout: Duration,
}

/// One task: a starting commit, what to ask the agent, and how to judge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Unique in its suite: 1 to 64 of `a-z`, `0-9`, `-`, `_`.
    pub id: String,
    /// One line for reports.
    pub title: Option<String>,
    /// Commit to start from; `None` is the repository's HEAD when the run
    /// starts.
    pub base: Option<String>,
    /// What the agent is asked to do.
    pub prompt: String,
    /// Run in the task's worktree before the agent, in order; any failure
    /// makes the trial `setup_failed`.
    pub setup: Vec<CommandSpec>,
    /// The acceptance command: exit code 0 is `passed`, anything else
    /// `failed`. The only judge.
    pub check: CommandSpec,
    /// A patch (relative to the repository root) applied after the agent
    /// and before the check, for tests the agent must not see or edit.
    pub check_patch: Option<PathBuf>,
    /// Time limit for the agent.
    pub agent_timeout: Duration,
    /// Free labels for filtering.
    pub tags: Vec<String>,
    /// The file the task was read from, relative to the repository root.
    pub source: PathBuf,
}

/// Which agent runs the tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Claude Code, `claude -p`.
    ClaudeCode,
    /// Codex, `codex exec`.
    Codex,
    /// Any program, given by `AgentSpec::command`.
    Command,
}

impl AgentKind {
    /// The name on the command line and in `run.json`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Command => "command",
        }
    }

    /// The kind named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [Self::ClaudeCode, Self::Codex, Self::Command]
            .into_iter()
            .find(|kind| kind.name() == name)
    }
}

/// The agent and how it is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    /// Which agent.
    pub kind: AgentKind,
    /// Model to ask for; `None` is the agent's default.
    pub model: Option<String>,
    /// For [`AgentKind::Command`]: program and arguments, where `{prompt_file}`
    /// and `{workdir}` are replaced. Empty for the other kinds.
    pub command: Vec<String>,
    /// More arguments passed to the agent as they are.
    pub extra_args: Vec<String>,
    /// Variables the agent inherits besides the ones
    /// [`crate::agent::passes`] keeps by default (`--pass-env`).
    pub pass_env: Vec<String>,
}

/// Token use and cost one agent run reported. Every member is `None` when
/// the agent did not report it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens, not counting cache reads.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Input tokens read from a prompt cache.
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to a prompt cache.
    pub cache_write_tokens: Option<u64>,
    /// Cost in millionths of a US dollar, as the agent reported it.
    pub cost_microusd: Option<u64>,
    /// Model turns.
    pub turns: Option<u64>,
}

/// How an agent run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEnd {
    /// It exited with this code.
    Exited(i32),
    /// A signal ended it.
    Signaled(i32),
    /// It ran past the task's limit and was killed.
    TimedOut,
    /// It could not be started.
    FailedToStart(String),
}

impl AgentEnd {
    /// The signal that ended the agent, when one did and it was not the
    /// task's time limit: seen as it is, or as the exit code a shell and
    /// many programs give for it, 128 + the signal.
    #[must_use]
    pub const fn signal(&self) -> Option<i32> {
        match self {
            Self::Signaled(signal) => Some(*signal),
            Self::Exited(code) if *code > 128 && *code < 160 => Some(*code - 128),
            _ => None,
        }
    }
}

/// What one agent run did, as the adapter observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutcome {
    /// How it ended.
    pub end: AgentEnd,
    /// Wall-clock milliseconds.
    pub duration_ms: u64,
    /// What it reported.
    pub usage: Usage,
}

/// The verdict of one trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The check exited 0.
    Passed,
    /// The check exited non-zero or was ended by a signal.
    Failed,
    /// The check ran past its limit.
    CheckTimeout,
    /// A setup command failed; the agent did not run.
    SetupFailed,
    /// The check patch did not apply to what the agent left.
    PatchFailed,
    /// The trial has no verdict: the worktree, the agent's start, or the
    /// check's start failed, or a signal ended the agent before its time
    /// limit. It says nothing about the agent; counted as not passed.
    Error,
}

impl Outcome {
    /// The name in `run.json` and reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::CheckTimeout => "check_timeout",
            Self::SetupFailed => "setup_failed",
            Self::PatchFailed => "patch_failed",
            Self::Error => "error",
        }
    }

    /// The outcome named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Passed,
            Self::Failed,
            Self::CheckTimeout,
            Self::SetupFailed,
            Self::PatchFailed,
            Self::Error,
        ]
        .into_iter()
        .find(|outcome| outcome.name() == name)
    }
}

/// What the agent changed, relative to the task's base commit, before the
/// check patch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffStat {
    /// Files changed.
    pub files: u64,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
    /// SHA-256 of the binary diff (`agent.diff`), hex; of the empty string
    /// when nothing changed.
    pub sha256: String,
}

/// One trial of one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrialResult {
    /// The task's id.
    pub task: String,
    /// 1-based trial number.
    pub trial: u32,
    /// The commit the trial started from.
    pub base: String,
    /// The verdict.
    pub outcome: Outcome,
    /// One sentence on why, for every outcome but `passed`.
    pub reason: Option<String>,
    /// The agent's run; `None` when it did not run (setup failed, error
    /// before it).
    pub agent: Option<AgentOutcome>,
    /// The check's exit code, when it exited.
    pub check_exit: Option<i32>,
    /// The check's wall-clock milliseconds; 0 when it did not run.
    pub check_ms: u64,
    /// What the agent changed.
    pub diff: DiffStat,
    /// The trial's directory under the run directory, holding `agent.out`,
    /// `agent.err`, `agent.diff`, `check.out`, `check.err`.
    pub logs: String,
}

/// The repository a run ran in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryInfo {
    /// The worktree root the run was started from.
    pub root: String,
    /// Its HEAD when the run started.
    pub head: String,
}

/// One task of a run, as it was when the run started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    /// The task's id.
    pub id: String,
    /// Its title.
    pub title: Option<String>,
    /// The file it came from.
    pub source: String,
    /// SHA-256 of that file's bytes, hex.
    pub sha256: String,
}

/// One run of a suite: `run.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    /// `YYYYMMDDTHHMMSSZ-<6 hex>`; also the run directory's name.
    pub run_id: String,
    /// RFC 3339 UTC.
    pub started_at: String,
    /// RFC 3339 UTC.
    pub finished_at: String,
    /// Where it ran.
    pub repository: RepositoryInfo,
    /// The agent.
    pub agent: AgentSpec,
    /// What the agent reported as its version (`--version`), when it did.
    pub agent_version: Option<String>,
    /// Trials of each task.
    pub trials_per_task: u32,
    /// The tasks, in run order.
    pub tasks: Vec<TaskRef>,
    /// Every trial, in task order, then trial order.
    pub results: Vec<TrialResult>,
}

/// Totals over a run's trials.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Tasks.
    pub tasks: u64,
    /// Trials.
    pub trials: u64,
    /// Trials whose outcome is `passed`.
    pub passed: u64,
    /// Trials whose outcome is `error`.
    pub errors: u64,
    /// Sum of the cost the agent reported, over the trials that reported
    /// one.
    pub cost_microusd: Option<u64>,
    /// Sum of input tokens (cache reads not counted).
    pub input_tokens: u64,
    /// Sum of output tokens.
    pub output_tokens: u64,
    /// Sum of the agent's wall-clock milliseconds.
    pub agent_ms: u64,
    /// Sum of the check's wall-clock milliseconds.
    pub check_ms: u64,
}

impl Summary {
    /// Passed trials in tenths of a percent of all trials (`875` is
    /// 87.5 %); `None` for no trials.
    #[must_use]
    pub const fn pass_permille(&self) -> Option<u64> {
        (self.passed * 1000).checked_div(self.trials)
    }
}

impl RunRecord {
    /// Totals over [`RunRecord::results`].
    #[must_use]
    pub fn summary(&self) -> Summary {
        let mut summary = Summary {
            tasks: self.tasks.len() as u64,
            trials: self.results.len() as u64,
            ..Summary::default()
        };
        for result in &self.results {
            match result.outcome {
                Outcome::Passed => summary.passed += 1,
                Outcome::Error => summary.errors += 1,
                _ => {}
            }
            summary.check_ms += result.check_ms;
            if let Some(agent) = &result.agent {
                summary.agent_ms += agent.duration_ms;
                summary.input_tokens += agent.usage.input_tokens.unwrap_or(0);
                summary.output_tokens += agent.usage.output_tokens.unwrap_or(0);
                if let Some(cost) = agent.usage.cost_microusd {
                    summary.cost_microusd = Some(summary.cost_microusd.unwrap_or(0) + cost);
                }
            }
        }
        summary
    }

    /// `run.json`'s text.
    #[must_use]
    pub fn to_json(&self) -> String {
        let summary = self.summary();
        Out::Object(vec![
            ("schema", Out::str(RUN_SCHEMA)),
            ("run_id", Out::str(&self.run_id)),
            ("started_at", Out::str(&self.started_at)),
            ("finished_at", Out::str(&self.finished_at)),
            (
                "repository",
                Out::Object(vec![
                    ("root", Out::str(&self.repository.root)),
                    ("head", Out::str(&self.repository.head)),
                ]),
            ),
            (
                "agent",
                Out::Object(vec![
                    ("kind", Out::str(self.agent.kind.name())),
                    ("model", Out::opt(self.agent.model.as_deref(), Out::str)),
                    ("version", Out::opt(self.agent_version.as_deref(), Out::str)),
                    ("command", Out::strs(&self.agent.command)),
                    ("extra_args", Out::strs(&self.agent.extra_args)),
                    ("pass_env", Out::strs(&self.agent.pass_env)),
                ]),
            ),
            (
                "trials_per_task",
                Out::uint(u64::from(self.trials_per_task)),
            ),
            (
                "tasks",
                Out::Array(self.tasks.iter().map(task_ref_out).collect()),
            ),
            (
                "results",
                Out::Array(self.results.iter().map(result_out).collect()),
            ),
            ("summary", summary_out(&summary)),
        ])
        .render()
    }

    /// Reads `run.json`'s text (from `path`, for messages). `summary` is
    /// ignored: it is recomputed from `results`.
    ///
    /// # Errors
    /// [`EvalError::Invalid`] naming the first member that is wrong.
    pub fn from_json(text: &str, path: Option<&std::path::Path>) -> Result<Self, EvalError> {
        let value = crate::json::parse(text, path)?;
        let top = Reader::new(&value, path, "")?;
        let schema = top.str("schema")?;
        if schema != RUN_SCHEMA {
            return Err(top.error("schema", &format!("must be {RUN_SCHEMA}, not {schema}")));
        }
        let repository = Reader::new(
            top.get("repository")
                .ok_or_else(|| top.error("repository", "is required"))?,
            path,
            "repository",
        )?;
        let agent = Reader::new(
            top.get("agent")
                .ok_or_else(|| top.error("agent", "is required"))?,
            path,
            "agent",
        )?;
        let kind = agent.str("kind")?;
        let trials_per_task = u32::try_from(top.u64("trials_per_task")?)
            .map_err(|_| top.error("trials_per_task", "is too large"))?;
        Ok(Self {
            run_id: top.str("run_id")?.to_owned(),
            started_at: top.str("started_at")?.to_owned(),
            finished_at: top.str("finished_at")?.to_owned(),
            repository: RepositoryInfo {
                root: repository.str("root")?.to_owned(),
                head: repository.str("head")?.to_owned(),
            },
            agent: AgentSpec {
                kind: AgentKind::parse(kind)
                    .ok_or_else(|| agent.error("kind", &format!("names no agent: {kind}")))?,
                model: agent.opt_str("model")?.map(str::to_owned),
                command: agent.strs("command")?,
                extra_args: agent.strs("extra_args")?,
                // Absent in a run.json written before --pass-env.
                pass_env: if agent.get("pass_env").is_some() {
                    agent.strs("pass_env")?
                } else {
                    Vec::new()
                },
            },
            agent_version: agent.opt_str("version")?.map(str::to_owned),
            trials_per_task,
            tasks: indexed(&top, "tasks", task_ref_in)?,
            results: indexed(&top, "results", result_in)?,
        })
    }
}

fn indexed<T>(
    top: &Reader<'_>,
    member: &str,
    read: impl Fn(&Reader<'_>) -> Result<T, EvalError>,
) -> Result<Vec<T>, EvalError> {
    top.array(member)?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let at = format!("{member}[{index}]");
            read(&Reader::new(value, top.path(), &at)?)
        })
        .collect()
}

fn nested<'a>(
    reader: &Reader<'a>,
    member: &str,
    at: &'a str,
) -> Result<Option<Reader<'a>>, EvalError> {
    reader
        .get(member)
        .map(|value: &'a JsonValue| Reader::new(value, reader.path(), at))
        .transpose()
}

fn task_ref_out(task: &TaskRef) -> Out {
    Out::Object(vec![
        ("id", Out::str(&task.id)),
        ("title", Out::opt(task.title.as_deref(), Out::str)),
        ("source", Out::str(&task.source)),
        ("sha256", Out::str(&task.sha256)),
    ])
}

fn task_ref_in(reader: &Reader<'_>) -> Result<TaskRef, EvalError> {
    Ok(TaskRef {
        id: reader.str("id")?.to_owned(),
        title: reader.opt_str("title")?.map(str::to_owned),
        source: reader.str("source")?.to_owned(),
        sha256: reader.str("sha256")?.to_owned(),
    })
}

fn usage_out(usage: &Usage) -> Out {
    Out::Object(vec![
        ("input_tokens", Out::opt(usage.input_tokens, Out::uint)),
        ("output_tokens", Out::opt(usage.output_tokens, Out::uint)),
        (
            "cache_read_tokens",
            Out::opt(usage.cache_read_tokens, Out::uint),
        ),
        (
            "cache_write_tokens",
            Out::opt(usage.cache_write_tokens, Out::uint),
        ),
        ("cost_microusd", Out::opt(usage.cost_microusd, Out::uint)),
        ("turns", Out::opt(usage.turns, Out::uint)),
    ])
}

fn usage_in(reader: &Reader<'_>) -> Result<Usage, EvalError> {
    Ok(Usage {
        input_tokens: reader.opt_u64("input_tokens")?,
        output_tokens: reader.opt_u64("output_tokens")?,
        cache_read_tokens: reader.opt_u64("cache_read_tokens")?,
        cache_write_tokens: reader.opt_u64("cache_write_tokens")?,
        cost_microusd: reader.opt_u64("cost_microusd")?,
        turns: reader.opt_u64("turns")?,
    })
}

fn agent_out(agent: &AgentOutcome) -> Out {
    let (end, code, detail) = match &agent.end {
        AgentEnd::Exited(code) => ("exited", Some(i64::from(*code)), None),
        AgentEnd::Signaled(signal) => ("signaled", Some(i64::from(*signal)), None),
        AgentEnd::TimedOut => ("timed_out", None, None),
        AgentEnd::FailedToStart(detail) => ("failed_to_start", None, Some(detail.as_str())),
    };
    Out::Object(vec![
        ("end", Out::str(end)),
        ("code", Out::opt(code, Out::int)),
        ("detail", Out::opt(detail, Out::str)),
        ("duration_ms", Out::uint(agent.duration_ms)),
        ("usage", usage_out(&agent.usage)),
    ])
}

fn agent_in(reader: &Reader<'_>) -> Result<AgentOutcome, EvalError> {
    let code = || -> Result<i32, EvalError> {
        reader
            .opt_i64("code")?
            .and_then(|code| i32::try_from(code).ok())
            .ok_or_else(|| reader.error("code", "must be a 32-bit integer for this end"))
    };
    let end = match reader.str("end")? {
        "exited" => AgentEnd::Exited(code()?),
        "signaled" => AgentEnd::Signaled(code()?),
        "timed_out" => AgentEnd::TimedOut,
        "failed_to_start" => {
            AgentEnd::FailedToStart(reader.opt_str("detail")?.unwrap_or_default().to_owned())
        }
        other => return Err(reader.error("end", &format!("names no end: {other}"))),
    };
    let usage = match nested(reader, "usage", "usage")? {
        Some(usage) => usage_in(&usage)?,
        None => Usage::default(),
    };
    Ok(AgentOutcome {
        end,
        duration_ms: reader.u64("duration_ms")?,
        usage,
    })
}

fn result_out(result: &TrialResult) -> Out {
    Out::Object(vec![
        ("task", Out::str(&result.task)),
        ("trial", Out::uint(u64::from(result.trial))),
        ("base", Out::str(&result.base)),
        ("outcome", Out::str(result.outcome.name())),
        ("reason", Out::opt(result.reason.as_deref(), Out::str)),
        ("agent", Out::opt(result.agent.as_ref(), agent_out)),
        (
            "check_exit",
            Out::opt(result.check_exit.map(i64::from), Out::int),
        ),
        ("check_ms", Out::uint(result.check_ms)),
        (
            "diff",
            Out::Object(vec![
                ("files", Out::uint(result.diff.files)),
                ("insertions", Out::uint(result.diff.insertions)),
                ("deletions", Out::uint(result.diff.deletions)),
                ("sha256", Out::str(&result.diff.sha256)),
            ]),
        ),
        ("logs", Out::str(&result.logs)),
    ])
}

fn result_in(reader: &Reader<'_>) -> Result<TrialResult, EvalError> {
    let outcome = reader.str("outcome")?;
    let diff = match nested(reader, "diff", "diff")? {
        Some(diff) => DiffStat {
            files: diff.u64("files")?,
            insertions: diff.u64("insertions")?,
            deletions: diff.u64("deletions")?,
            sha256: diff.str("sha256")?.to_owned(),
        },
        None => DiffStat::default(),
    };
    Ok(TrialResult {
        task: reader.str("task")?.to_owned(),
        trial: u32::try_from(reader.u64("trial")?)
            .map_err(|_| reader.error("trial", "is too large"))?,
        base: reader.str("base")?.to_owned(),
        outcome: Outcome::parse(outcome)
            .ok_or_else(|| reader.error("outcome", &format!("names no outcome: {outcome}")))?,
        reason: reader.opt_str("reason")?.map(str::to_owned),
        agent: nested(reader, "agent", "agent")?
            .map(|agent| agent_in(&agent))
            .transpose()?,
        check_exit: reader
            .opt_i64("check_exit")?
            .map(|code| i32::try_from(code).map_err(|_| reader.error("check_exit", "is too large")))
            .transpose()?,
        check_ms: reader.opt_u64("check_ms")?.unwrap_or(0),
        diff,
        logs: reader.str("logs")?.to_owned(),
    })
}

fn summary_out(summary: &Summary) -> Out {
    Out::Object(vec![
        ("tasks", Out::uint(summary.tasks)),
        ("trials", Out::uint(summary.trials)),
        ("passed", Out::uint(summary.passed)),
        ("errors", Out::uint(summary.errors)),
        (
            "pass_permille",
            Out::opt(summary.pass_permille(), Out::uint),
        ),
        ("cost_microusd", Out::opt(summary.cost_microusd, Out::uint)),
        ("input_tokens", Out::uint(summary.input_tokens)),
        ("output_tokens", Out::uint(summary.output_tokens)),
        ("agent_ms", Out::uint(summary.agent_ms)),
        ("check_ms", Out::uint(summary.check_ms)),
    ])
}

/// Records for the crate's unit tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A run of two tasks (one passed, one failed after the agent timed
    /// out): every kind of member is present.
    #[must_use]
    pub(crate) fn sample_run() -> RunRecord {
        let agent = |end, cost| AgentOutcome {
            end,
            duration_ms: 61_000,
            usage: Usage {
                input_tokens: Some(1200),
                output_tokens: Some(340),
                cache_read_tokens: Some(9000),
                cache_write_tokens: None,
                cost_microusd: cost,
                turns: Some(7),
            },
        };
        RunRecord {
            run_id: "20261114T221323Z-a1b2c3".to_owned(),
            started_at: "2026-11-14T22:13:23Z".to_owned(),
            finished_at: "2026-11-14T22:20:01Z".to_owned(),
            repository: RepositoryInfo {
                root: "/work/repo".to_owned(),
                head: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            },
            agent: AgentSpec {
                kind: AgentKind::ClaudeCode,
                model: Some("claude-sonnet-5-5".to_owned()),
                command: Vec::new(),
                extra_args: vec!["--max-turns".to_owned(), "30".to_owned()],
                pass_env: Vec::new(),
            },
            agent_version: Some("2.3.1 (Claude Code)".to_owned()),
            trials_per_task: 1,
            tasks: vec![
                TaskRef {
                    id: "empty-input".to_owned(),
                    title: Some("parser accepts empty input".to_owned()),
                    source: "evals/empty-input.json".to_owned(),
                    sha256: "ab".repeat(32),
                },
                TaskRef {
                    id: "truncated-header".to_owned(),
                    title: None,
                    source: "evals/truncated-header.json".to_owned(),
                    sha256: "cd".repeat(32),
                },
            ],
            results: vec![
                TrialResult {
                    task: "empty-input".to_owned(),
                    trial: 1,
                    base: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    outcome: Outcome::Passed,
                    reason: None,
                    agent: Some(agent(AgentEnd::Exited(0), Some(412_000))),
                    check_exit: Some(0),
                    check_ms: 2_500,
                    diff: DiffStat {
                        files: 2,
                        insertions: 14,
                        deletions: 3,
                        sha256: "ef".repeat(32),
                    },
                    logs: "trials/empty-input/1".to_owned(),
                },
                TrialResult {
                    task: "truncated-header".to_owned(),
                    trial: 1,
                    base: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    outcome: Outcome::Failed,
                    reason: Some("the check exited with code 101".to_owned()),
                    agent: Some(agent(AgentEnd::TimedOut, None)),
                    check_exit: Some(101),
                    check_ms: 3_100,
                    diff: DiffStat::default(),
                    logs: "trials/truncated-header/1".to_owned(),
                },
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::sample_run, *};

    #[test]
    fn run_json_round_trips() {
        let run = sample_run();
        let text = run.to_json();
        let back = RunRecord::from_json(&text, None).expect("reads back");
        assert_eq!(back, run);
        assert!(text.contains("\"pass_permille\": 500"));
    }

    #[test]
    fn summary_adds_only_reported_costs() {
        let summary = sample_run().summary();
        assert_eq!(summary.trials, 2);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.cost_microusd, Some(412_000));
        assert_eq!(summary.input_tokens, 2400);
        assert_eq!(summary.pass_permille(), Some(500));
    }

    #[test]
    fn a_wrong_schema_is_named() {
        let text = sample_run().to_json().replace(RUN_SCHEMA, "tune.run.v0");
        let error = RunRecord::from_json(&text, None)
            .expect_err("refused")
            .to_string();
        assert_eq!(error, "`schema` must be tune.run.v1, not tune.run.v0");
    }
}
