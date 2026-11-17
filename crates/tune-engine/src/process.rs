//! Running one command with a time limit, its output going to files. Used
//! for setup and check commands and by the agent adapters.

use std::{
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::EvalError;

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It exited with this code.
    Code(i32),
    /// A signal ended it.
    Signal(i32),
    /// It ran past its time limit and was killed, with its process group.
    TimedOut,
}

impl Exit {
    /// Whether it exited with code 0.
    #[must_use]
    pub const fn success(self) -> bool {
        matches!(self, Self::Code(0))
    }

    /// The exit code, when it exited.
    #[must_use]
    pub const fn code(self) -> Option<i32> {
        match self {
            Self::Code(code) => Some(code),
            Self::Signal(_) | Self::TimedOut => None,
        }
    }
}

/// One command to run.
#[derive(Debug, Clone)]
pub struct Invocation<'a> {
    /// Program and arguments; never empty.
    pub argv: &'a [String],
    /// Working directory.
    pub cwd: &'a Path,
    /// Variables added to the inherited environment.
    pub env: &'a [(String, String)],
    /// Variables removed from the inherited environment.
    pub env_remove: &'a [String],
    /// Start from an empty environment instead of the inherited
    /// one, so that `env` is the whole environment.
    pub clear_env: bool,
    /// Where standard output goes.
    pub stdout: PathBuf,
    /// Where standard error goes.
    pub stderr: PathBuf,
    /// The time limit.
    pub timeout: Duration,
}

/// What [`run`] observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// How it ended.
    pub exit: Exit,
    /// Wall-clock time from start to end.
    pub duration: Duration,
}

/// Runs `invocation` in its own process group, standard input closed, and
/// waits for it at most its time limit; past it, the whole group is killed.
///
/// # Errors
/// [`EvalError::Spawn`] when the program cannot be started,
/// [`EvalError::Io`] when an output file cannot be created.
pub fn run(invocation: &Invocation<'_>) -> Result<Finished, EvalError> {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};

    let (program, args) = invocation
        .argv
        .split_first()
        .ok_or_else(|| EvalError::Usage("a command needs a program".to_owned()))?;
    let stdout = File::create(&invocation.stdout)
        .map_err(|error| EvalError::io(&invocation.stdout, error))?;
    let stderr = File::create(&invocation.stderr)
        .map_err(|error| EvalError::io(&invocation.stderr, error))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(invocation.cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0);
    if invocation.clear_env {
        command.env_clear();
    }
    for name in invocation.env_remove {
        command.env_remove(name);
    }
    command.envs(invocation.env.iter().map(|(name, value)| (name, value)));
    let started = Instant::now();
    let mut child = command.spawn().map_err(|source| EvalError::Spawn {
        program: program.clone(),
        source,
    })?;
    let mut pause = Duration::from_millis(10);
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| EvalError::io(invocation.cwd, error))?
        {
            let exit = status
                .code()
                .map_or_else(|| Exit::Signal(status.signal().unwrap_or(0)), Exit::Code);
            return Ok(Finished {
                exit,
                duration: started.elapsed(),
            });
        }
        if started.elapsed() >= invocation.timeout {
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Finished {
                exit: Exit::TimedOut,
                duration: started.elapsed(),
            });
        }
        thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(200));
    }
}

/// Sends `SIGKILL` to the process group `pgid` through `kill(1)`, since the
/// crate forbids `unsafe` and so calls no `libc` directly.
fn kill_group(pgid: u32) {
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg("--")
        .arg(format!("-{pgid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, dir: &Path, timeout: Duration) -> Finished {
        let argv = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()];
        run(&Invocation {
            argv: &argv,
            cwd: dir,
            env: &[("EVAL_TEST_VAR".to_owned(), "seen".to_owned())],
            env_remove: &[],
            clear_env: false,
            stdout: dir.join("out"),
            stderr: dir.join("err"),
            timeout,
        })
        .expect("runs")
    }

    #[test]
    fn records_exit_code_and_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let finished = sh(
            "echo $EVAL_TEST_VAR; echo oops >&2; exit 3",
            dir.path(),
            Duration::from_secs(10),
        );
        assert_eq!(finished.exit, Exit::Code(3));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out")).expect("out"),
            "seen\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("err")).expect("err"),
            "oops\n"
        );
    }

    #[test]
    fn kills_the_whole_group_past_the_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("late");
        let script = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let finished = sh(&script, dir.path(), Duration::from_millis(300));
        assert_eq!(finished.exit, Exit::TimedOut);
        assert!(finished.duration < Duration::from_secs(5));
        thread::sleep(Duration::from_secs(3));
        assert!(
            !marker.exists(),
            "the background child was killed with its group"
        );
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let argv = vec!["tune-no-such-program".to_owned()];
        let error = run(&Invocation {
            argv: &argv,
            cwd: dir.path(),
            env: &[],
            env_remove: &[],
            clear_env: false,
            stdout: dir.path().join("out"),
            stderr: dir.path().join("err"),
            timeout: Duration::from_secs(1),
        })
        .expect_err("cannot start");
        assert!(matches!(error, EvalError::Spawn { .. }));
    }
}
