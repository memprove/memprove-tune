//! `mine --verify`: a task is kept only when its check fails on the parent
//! commit with the check patch applied and passes on the commit itself.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    CommandSpec, EvalError,
    process::{self, Exit, Invocation},
    workspace::Worktree,
};

/// Where one verification runs: worktrees and logs under a directory in
/// the repository's Git directory.
pub(super) struct Verifier<'a> {
    pub repo_root: &'a Path,
    /// `<git common dir>/tune/mine/<stamp>`.
    pub scratch: PathBuf,
}

/// Which side of the change a check ran on.
#[derive(Debug, Clone, Copy)]
enum Side {
    Before,
    After,
}

impl Side {
    const fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }

    const fn commit_words(self) -> &'static str {
        match self {
            Self::Before => "on the base commit with the check patch applied",
            Self::After => "on the commit with the change",
        }
    }
}

/// Why a task was refused.
pub(super) struct Refusal {
    /// What to tell the person; it names the logs.
    pub reason: String,
    /// A setup command failed, so the tests never ran: that says more
    /// about the setup than about the task.
    pub setup_failed: bool,
}

impl Verifier<'_> {
    /// Runs setup and check in a worktree at `parent` with `patch` applied,
    /// then, when that check failed, in one at `commit` without it.
    /// `Ok(None)` when the task behaves (fails before, passes after);
    /// `Ok(Some(refusal))` when it does not.
    ///
    /// # Errors
    /// When a worktree cannot be made or removed, or a log written.
    pub(super) fn verify(
        &self,
        id: &str,
        parent: &str,
        commit: &str,
        patch: Option<&Path>,
        setup: &[CommandSpec],
        check: &CommandSpec,
    ) -> Result<Option<Refusal>, EvalError> {
        let logs = self.scratch.join(id);
        fs::create_dir_all(&logs).map_err(|error| EvalError::io(&logs, error))?;
        let job = Job {
            id,
            setup,
            check,
            logs: &logs,
        };
        let before = self.side(&job, Side::Before, parent, patch)?;
        let outcome = match before {
            SideResult::Check(exit) if exit.success() => SideResult::Refused(format!(
                "the check passed {}, so it does not test the change",
                Side::Before.commit_words()
            )),
            SideResult::Check(Exit::TimedOut) => SideResult::Refused(format!(
                "the check timed out {}",
                Side::Before.commit_words()
            )),
            SideResult::Check(_) => match self.side(&job, Side::After, commit, None)? {
                SideResult::Check(exit) if !exit.success() => SideResult::Refused(format!(
                    "the check {} {}, so the task cannot be passed",
                    ended(exit),
                    Side::After.commit_words()
                )),
                after => after,
            },
            refused => refused,
        };
        let (reason, setup_failed) = match outcome {
            SideResult::Check(_) => return Ok(None),
            SideResult::Refused(reason) => (reason, false),
            SideResult::SetupFailed(reason) => (reason, true),
        };
        Ok(Some(Refusal {
            reason: format!("{reason} (logs: {})", logs.display()),
            setup_failed,
        }))
    }

    fn side(
        &self,
        job: &Job<'_>,
        side: Side,
        commit: &str,
        patch: Option<&Path>,
    ) -> Result<SideResult, EvalError> {
        let tree = Worktree::create(
            self.repo_root,
            &self.scratch.join("worktrees"),
            &format!("{}-{}", job.id, side.name()),
            commit,
        )?;
        let result = Self::in_tree(&tree, job, side, patch);
        tree.remove()?;
        Ok(result)
    }

    fn in_tree(tree: &Worktree, job: &Job<'_>, side: Side, patch: Option<&Path>) -> SideResult {
        let Job {
            setup, check, logs, ..
        } = *job;
        if let Some(patch) = patch
            && let Err(error) = tree.apply(patch)
        {
            return SideResult::Refused(format!(
                "the check patch does not apply to the base commit: {error}"
            ));
        }
        for (index, command) in setup.iter().enumerate() {
            let name = format!("{}-setup-{}", side.name(), index + 1);
            match run(tree.path(), command, logs, &name) {
                Ok(exit) if exit.success() => {}
                Ok(exit) => {
                    return SideResult::SetupFailed(format!(
                        "setup `{}` {} {}",
                        command.argv.join(" "),
                        ended(exit),
                        side.commit_words()
                    ));
                }
                Err(error) => return SideResult::SetupFailed(error.to_string()),
            }
        }
        match run(tree.path(), check, logs, &format!("{}-check", side.name())) {
            Ok(exit) => SideResult::Check(exit),
            Err(error) => SideResult::Refused(error.to_string()),
        }
    }
}

/// One task being verified.
struct Job<'a> {
    id: &'a str,
    setup: &'a [CommandSpec],
    check: &'a CommandSpec,
    logs: &'a Path,
}

enum SideResult {
    /// The check ran and ended so.
    Check(Exit),
    /// The task does not behave, or the check could not be reached; why.
    Refused(String),
    /// A setup command failed or could not be started; why.
    SetupFailed(String),
}

fn run(cwd: &Path, command: &CommandSpec, logs: &Path, name: &str) -> Result<Exit, EvalError> {
    process::run(&Invocation {
        argv: &command.argv,
        cwd,
        env: &[],
        env_remove: &[],
        clear_env: false,
        stdout: logs.join(format!("{name}.out")),
        stderr: logs.join(format!("{name}.err")),
        timeout: command.timeout,
    })
    .map(|finished| finished.exit)
}

fn ended(exit: Exit) -> String {
    match exit {
        Exit::Code(code) => format!("exited {code}"),
        Exit::Signal(signal) => format!("was ended by signal {signal}"),
        Exit::TimedOut => "timed out".to_owned(),
    }
}
