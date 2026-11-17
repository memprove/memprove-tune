//! The trial engine: one task, one agent, one verdict. A task names a
//! starting commit, a prompt and an acceptance command; a run gives each
//! task to a coding agent (Claude Code, Codex, or any program) in a Git
//! repository of its own that holds the files of that commit and nothing
//! of the check, and the acceptance command's exit code alone says whether
//! it passed. The engine never judges the work in any other way.
//!
//! What the tasks' setup commands do to the working copy before the agent
//! starts (overlaying the agent configuration under test) and how runs are
//! compared belong to the `tune` crate; this crate only runs trials and
//! writes `run.json`.
//!
//! The engine began as a copy of the `memprove-eval` crate of
//! <https://github.com/memprove/memprove> (see `PROVENANCE.md`); it depends
//! on no memprove crate and on nothing but `sha2`.

#![forbid(unsafe_code)]

pub mod agent;
mod error;
pub mod git;
pub mod json;
mod model;
pub mod process;
pub mod runner;
pub mod strict_json;
pub mod task;
pub mod time;
pub mod workspace;

/// `bytes` as lowercase hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            write!(text, "{byte:02x}").expect("writing to a String cannot fail");
            text
        })
}

pub use agent::{Agent, AgentRequest, agent_for};
pub use error::EvalError;
pub use model::{
    AgentEnd, AgentKind, AgentOutcome, AgentSpec, CommandSpec, DiffStat, LEGACY_TASK_SCHEMA,
    Outcome, RUN_SCHEMA, RepositoryInfo, RunRecord, Summary, TASK_SCHEMA, Task, TaskRef,
    TrialResult, Usage,
};
