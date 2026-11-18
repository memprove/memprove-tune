//! `mine`: task files from the repository's own history. A
//! commit that changed tests and code becomes a task: start from its
//! parent, ask for what its message says, judge by its tests (applied as
//! the check patch, so the agent never sees them).
//!
//! Commits are looked at newest first; merges, root commits and commits
//! that [`history::is_test_path`] / [`history::is_doc_path`] and the size
//! limits rule out are passed over without a word. A task file that
//! already exists is never overwritten.

pub mod history;
mod narrow;
mod verify;

use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    CommandSpec, EvalError, Task,
    detect::{self, ProjectKind},
    git::git,
    taskfile::{self, AGENT_TIMEOUT, CHECK_TIMEOUT},
};

/// What `mine` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MineSpec {
    /// The worktree root.
    pub repo_root: PathBuf,
    /// At most this many tasks.
    pub limit: usize,
    /// The commits to look at (`git log` syntax); `None` is the history of
    /// HEAD.
    pub range: Option<String>,
    /// Where the task files and patches go, relative to the root.
    pub out_dir: PathBuf,
    /// List the files that would be written; write nothing (and verify nothing).
    pub dry_run: bool,
    /// Keep a task only when its check fails on the parent commit with the
    /// check patch applied and passes on the commit: each candidate is
    /// tried in two worktrees, setup and check run in each.
    pub verify: bool,
    /// The commands a fresh worktree needs before the tests can run,
    /// instead of the guess; `None` keeps the guess.
    pub setup: Option<Vec<Vec<String>>>,
    /// The test command, instead of the guess; `None` keeps the guess. A
    /// command with the word `pytest` still gets the test files a commit
    /// changed appended; any other is every task's check as it is.
    pub test: Option<Vec<String>>,
}

impl MineSpec {
    /// The defaults of the command line for `repo_root`: at most 10 tasks
    /// from the history of HEAD into `.tune/tasks/`, written, not verified.
    #[must_use]
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            limit: 10,
            range: None,
            out_dir: PathBuf::from(taskfile::TASKS_DIR),
            dry_run: false,
            verify: false,
            setup: None,
            test: None,
        }
    }
}

/// What became of one candidate commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MineStatus {
    /// The task file and its check patch were written.
    Written,
    /// A dry run: they would be written.
    DryRun,
    /// A task file of this id exists; it was left as it is.
    Exists,
    /// `--verify` refused it, for this reason (which names its logs).
    Rejected(String),
}

/// One task `mine` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinedTask {
    /// The task's id, `mined-<first 7 hex digits of the commit>`.
    pub id: String,
    /// The commit it was made from.
    pub commit: String,
    /// The task file, relative to the root.
    pub path: PathBuf,
    /// The commit's subject, the task's title.
    pub title: String,
    /// The check command written into the task.
    pub check: Vec<String>,
    /// What became of it. Only [`MineStatus::Rejected`] tasks do not count
    /// towards [`MineSpec::limit`].
    pub status: MineStatus,
}

/// Writes tasks mined from history.
///
/// # Errors
/// [`EvalError::Usage`] for a `range` that starts with `-`, an absolute
/// `out_dir` outside the root or a given command without words;
/// [`EvalError::Invalid`] when no test command is given and none can be
/// guessed (see [`detect::detect`]); [`EvalError::Git`] or
/// [`EvalError::Io`] when `git` fails or a file cannot be written.
pub fn mine(spec: &MineSpec) -> Result<Vec<MinedTask>, EvalError> {
    let root = &spec.repo_root;
    let out_dir = relative_out_dir(root, &spec.out_dir)?;
    let commands = commands(spec)?;
    let commits = history::commits(root, spec.range.as_deref())?;
    let verifier = spec
        .verify
        .then(|| scratch_dir(root))
        .transpose()?
        .map(|scratch| verify::Verifier {
            repo_root: root,
            scratch,
        });

    let mut found = Vec::new();
    let mut counted = 0;
    for (commit, parent) in commits {
        if counted >= spec.limit {
            break;
        }
        let Some(candidate) = history::candidate(root, &commit, &parent)? else {
            continue;
        };
        let id = format!("mined-{}", &commit[..7.min(commit.len())]);
        let task_path = out_dir.join(format!("{id}.json"));
        let patch_path = out_dir.join(format!("{id}.check.patch"));
        let check = match commands.narrow_as {
            Some(kind) => narrow::narrow(root, &commit, kind, &commands.check, &candidate.tests),
            None => commands.check.clone(),
        };
        let mut mined = MinedTask {
            id: id.clone(),
            commit: commit.clone(),
            path: task_path.clone(),
            title: candidate.subject.clone(),
            check: check.clone(),
            status: MineStatus::Written,
        };
        if root.join(&task_path).exists() {
            mined.status = MineStatus::Exists;
        } else if spec.dry_run {
            mined.status = MineStatus::DryRun;
        } else {
            let patch = test_patch(root, &candidate)?;
            let task = Task {
                id,
                title: Some(candidate.subject.clone()),
                base: Some(parent.clone()),
                prompt: candidate.prompt.clone(),
                setup: taskfile::setup_commands(&commands.setup),
                check: CommandSpec {
                    argv: check,
                    timeout: CHECK_TIMEOUT,
                },
                check_patch: Some(patch_path.clone()),
                agent_timeout: AGENT_TIMEOUT,
                tags: vec!["mined".to_owned()],
                source: task_path.clone(),
            };
            if let Some(verifier) = &verifier {
                let staged = verifier.scratch.join(format!("{}.check.patch", task.id));
                write(&staged, &patch)?;
                if let Some(reason) = verifier.verify(
                    &task.id,
                    &parent,
                    &commit,
                    Some(&staged),
                    &task.setup,
                    &task.check,
                )? {
                    mined.status = MineStatus::Rejected(reason);
                }
            }
            if mined.status == MineStatus::Written {
                write(&root.join(&patch_path), &patch)?;
                write(&root.join(&task_path), &taskfile::render_task(&task))?;
            }
        }
        if !matches!(mined.status, MineStatus::Rejected(_)) {
            counted += 1;
        }
        found.push(mined);
    }
    if let Some(verifier) = &verifier {
        let _ = git(root, &["worktree", "prune"]);
        let _ = fs::remove_dir_all(verifier.scratch.join("worktrees"));
    }
    Ok(found)
}

/// How the tasks install and test: what was given, else the guess.
struct Commands {
    /// What a fresh worktree needs before the tests can run.
    setup: Vec<Vec<String>>,
    /// The repository's test command.
    check: Vec<String>,
    /// The kind of project `check` is narrowed as; `None` leaves it as it
    /// is.
    narrow_as: Option<ProjectKind>,
}

fn commands(spec: &MineSpec) -> Result<Commands, EvalError> {
    let no_words = |what: &str| EvalError::Usage(format!("the {what} command has no words"));
    if spec.test.as_ref().is_some_and(Vec::is_empty) {
        return Err(no_words("test"));
    }
    if spec
        .setup
        .as_ref()
        .is_some_and(|setup| setup.iter().any(Vec::is_empty))
    {
        return Err(no_words("setup"));
    }
    let root = &spec.repo_root;
    let detected = detect::detect(root);
    let (check, narrow_as) = match (&spec.test, &detected) {
        (Some(test), _) => (
            test.clone(),
            test.iter()
                .any(|word| word == "pytest")
                .then_some(ProjectKind::Python),
        ),
        (None, Some(found)) => (found.check.clone(), Some(found.kind)),
        (None, None) => {
            return Err(EvalError::invalid(
                None,
                format!(
                    "no test command was found in {} to judge tasks with ({}); give it with `--test`, or write the task files by hand: a prompt and a check command each",
                    root.display(),
                    detect::ORDER
                ),
            ));
        }
    };
    let setup = match &spec.setup {
        Some(setup) => setup.clone(),
        None => detected.map(|found| found.setup).unwrap_or_default(),
    };
    Ok(Commands {
        setup,
        check,
        narrow_as,
    })
}

/// Checks a task against a reference answer the way `--verify` does: in a
/// worktree at the task's base (HEAD when it has none) with its check patch
/// applied, setup then check must fail; in one at `reference` (a commit
/// that holds the answer and the tests, so no patch is applied) they must
/// pass. For a task written by hand.
///
/// `Ok(None)` when it behaves, `Ok(Some(reason))` when not; the reason
/// names the logs, under `<git common dir>/tune/mine/`.
///
/// # Errors
/// When `git` fails or a worktree or log cannot be written.
pub fn verify_task(
    repo_root: &Path,
    task: &Task,
    reference: &str,
) -> Result<Option<String>, EvalError> {
    let base = crate::git::commit(repo_root, task.base.as_deref().unwrap_or("HEAD"))?;
    let reference = crate::git::commit(repo_root, reference)?;
    let verifier = verify::Verifier {
        repo_root,
        scratch: scratch_dir(repo_root)?,
    };
    let patch = task.check_patch.as_ref().map(|patch| repo_root.join(patch));
    let reason = verifier.verify(
        &task.id,
        &base,
        &reference,
        patch.as_deref(),
        &task.setup,
        &task.check,
    );
    let _ = git(repo_root, &["worktree", "prune"]);
    let _ = fs::remove_dir_all(verifier.scratch.join("worktrees"));
    reason
}

/// The binary diff of the commit's test files against its parent.
fn test_patch(root: &Path, candidate: &history::Candidate) -> Result<String, EvalError> {
    let mut args = vec![
        "diff".to_owned(),
        "--binary".to_owned(),
        "--no-renames".to_owned(),
        candidate.parent.clone(),
        candidate.commit.clone(),
        "--".to_owned(),
    ];
    args.extend(
        candidate
            .tests
            .iter()
            .map(|path| format!(":(literal){path}")),
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git(root, &args)
}

/// `out_dir` relative to `root`: as it is when relative, with the root
/// stripped when absolute and inside it.
fn relative_out_dir(root: &Path, out_dir: &Path) -> Result<PathBuf, EvalError> {
    if out_dir.is_relative() {
        return Ok(out_dir.to_path_buf());
    }
    out_dir
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            EvalError::Usage(format!(
                "--out must be inside the repository ({}), since task files name their patch relative to its root: {}",
                root.display(),
                out_dir.display()
            ))
        })
}

/// `<git common dir>/tune/mine/<UTC time>-<process id>`: inside
/// `.git`, so that it is never an untracked file.
fn scratch_dir(root: &Path) -> Result<PathBuf, EvalError> {
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let dir = Path::new(common.trim())
        .join("tune")
        .join("mine")
        .join(format!(
            "{}-{}",
            crate::time::compact(SystemTime::now()),
            std::process::id()
        ));
    fs::create_dir_all(&dir).map_err(|error| EvalError::io(&dir, error))?;
    Ok(dir)
}

fn write(path: &Path, text: &str) -> Result<(), EvalError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| EvalError::io(parent, error))?;
    }
    fs::write(path, text).map_err(|error| EvalError::io(path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_out_dir_must_be_inside_the_root() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_out_dir(root, Path::new("evals")).expect("relative"),
            Path::new("evals")
        );
        assert_eq!(
            relative_out_dir(root, Path::new("/repo/evals/mined")).expect("inside"),
            Path::new("evals/mined")
        );
        assert!(matches!(
            relative_out_dir(root, Path::new("/elsewhere")),
            Err(EvalError::Usage(_))
        ));
    }
}
