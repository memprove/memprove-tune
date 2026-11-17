//! `tune apply`: the setup command an experiment adds to every task. In
//! the trial's working copy it replaces the configuration the task's base
//! commit holds with an arm's, and leaves the copy as if it had begun that
//! way: one commit, a clean tree, and nothing of the old configuration in
//! the repository's objects.

use std::{fs, path::Path};

use tune_engine::{git, workspace::START_SUBJECT};

use crate::{
    error::Error,
    files::{self, is_config_path},
};

/// The variable the engine gives a task's setup commands.
const TASK_ENV: &str = "TUNE_TASK";

/// Puts the files under `arm` in place of the configuration in the trial
/// working copy that holds `cwd`.
pub(crate) fn apply(cwd: &Path, arm: &Path) -> Result<(), Error> {
    let not_a_trial = || {
        Error::Invalid(
            "`tune apply` only runs as a setup command inside a trial's working copy; `tune ab` adds it by itself"
                .to_owned(),
        )
    };
    if std::env::var_os(TASK_ENV).is_none() {
        return Err(not_a_trial());
    }
    let top = git::toplevel(cwd).map_err(|_| not_a_trial())?;
    let subjects = git::git(&top, &["log", "--all", "--format=%s"]).map_err(|_| not_a_trial())?;
    let mut subjects = subjects.lines();
    if !subjects
        .next()
        .is_some_and(|subject| subject.starts_with(START_SUBJECT))
        || subjects.next().is_some()
    {
        return Err(not_a_trial());
    }
    let files = files::read_tree(arm)?;
    let old = git::commit(&top, "HEAD")?;

    let listing = git::git(&top, &["ls-files", "-z"])?;
    for path in listing
        .split('\0')
        .filter(|path| !path.is_empty() && is_config_path(path))
    {
        let file = top.join(path);
        fs::remove_file(&file).map_err(|error| Error::io(&file, error))?;
        // A directory it leaves empty goes too.
        let mut dir = file.parent();
        while let Some(parent) = dir.filter(|parent| *parent != top.as_path()) {
            if fs::remove_dir(parent).is_err() {
                break;
            }
            dir = parent.parent();
        }
    }
    files::write_tree(&files, &top)?;
    git::git(&top, &["add", "--all", "--force"])?;
    git::git(
        &top,
        &[
            "-c",
            "user.name=tune",
            "-c",
            "user.email=trial@tune.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--quiet",
            "--no-verify",
            "--allow-empty",
            "--amend",
            "--no-edit",
        ],
    )?;
    let new = git::commit(&top, "HEAD")?;
    if new != old {
        // The engine compares the agent's work with the commit the copy
        // began at; that name now stands for the commit with the arm.
        git::git(&top, &["replace", &old, &new])?;
    }
    git::git(&top, &["reflog", "expire", "--expire=now", "--all"])?;
    git::git(&top, &["prune", "--expire=now"])?;
    Ok(())
}
