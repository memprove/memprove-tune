//! `memprove-tune apply`: the setup command an experiment adds to every task. In
//! the trial's working copy it replaces the configuration the task's base
//! commit holds with an arm's, and leaves the copy as if it had begun that
//! way: one commit, a clean tree, and nothing of the old configuration in
//! the repository's objects.

use std::{fs, path::Path};

use tune_engine::{git, workspace::START_SUBJECT};

use crate::{
    error::Error,
    files::{self, is_config_path, is_plain_relative},
};

/// The variable the engine gives a task's setup commands.
const TASK_ENV: &str = "TUNE_TASK";

/// Removes `file` from the working copy at `top`, and the directories that
/// leaves empty.
fn remove(top: &Path, file: &Path) -> Result<(), Error> {
    fs::remove_file(file).map_err(|error| Error::io(file, error))?;
    let mut dir = file.parent();
    while let Some(parent) = dir.filter(|parent| *parent != top) {
        if fs::remove_dir(parent).is_err() {
            break;
        }
        dir = parent.parent();
    }
    Ok(())
}

/// Puts the files under `arm` in place of the configuration in the trial
/// working copy that holds `cwd`, and takes out the files the lines of
/// `hide` name: task files and hidden tests of the repository that the
/// engine does not know of.
pub(crate) fn apply(cwd: &Path, arm: &Path, hide: Option<&Path>) -> Result<(), Error> {
    let not_a_trial = || {
        Error::Invalid(
            "`memprove-tune apply` only runs as a setup command inside a trial's working copy; `memprove-tune ab` adds it by itself"
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
    let hidden = match hide {
        Some(list) => fs::read_to_string(list).map_err(|error| Error::io(list, error))?,
        None => String::new(),
    };
    let old = git::commit(&top, "HEAD")?;

    let listing = git::git(&top, &["ls-files", "-z"])?;
    for path in listing
        .split('\0')
        .filter(|path| !path.is_empty() && is_config_path(path))
    {
        remove(&top, &top.join(path))?;
    }
    for path in hidden.lines().filter(|path| is_plain_relative(path)) {
        let file = top.join(path);
        if fs::symlink_metadata(&file).is_ok_and(|meta| !meta.is_dir()) {
            remove(&top, &file)?;
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
