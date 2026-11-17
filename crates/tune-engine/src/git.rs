//! `git` as a subprocess: the crate reads and changes repositories only
//! through it.

use std::{ffi::OsStr, path::Path, process::Command};

use crate::EvalError;

/// Runs `git -C <repo> <args>` and returns its standard output.
///
/// # Errors
/// [`EvalError::Spawn`] when `git` cannot be started, [`EvalError::Git`]
/// when it exits non-zero.
pub fn git(repo: &Path, args: &[&str]) -> Result<String, EvalError> {
    git_with_env(repo, &[], args)
}

/// [`git`] with `env` added to its environment.
///
/// # Errors
/// As [`git`].
pub fn git_with_env(
    repo: &Path,
    env: &[(&str, &OsStr)],
    args: &[&str],
) -> Result<String, EvalError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().copied())
        .output()
        .map_err(|source| EvalError::Spawn {
            program: "git".to_owned(),
            source,
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(EvalError::Git {
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// The root of the worktree that holds `dir`.
///
/// # Errors
/// When `dir` is not in a Git worktree.
pub fn toplevel(dir: &Path) -> Result<std::path::PathBuf, EvalError> {
    Ok(git(dir, &["rev-parse", "--show-toplevel"])?.trim().into())
}

/// The full commit name `rev` resolves to in `repo`.
///
/// # Errors
/// When `rev` names no commit.
pub fn commit(repo: &Path, rev: &str) -> Result<String, EvalError> {
    let spec = format!("{rev}^{{commit}}");
    Ok(git(repo, &["rev-parse", "--verify", "--quiet", &spec])?
        .trim()
        .to_owned())
}
