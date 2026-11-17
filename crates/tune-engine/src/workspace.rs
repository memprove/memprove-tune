//! One trial's working copy at a task's base commit: diffed after the
//! agent, patched for the check, removed at the end. The runner gives the
//! agent a repository of its own ([`Worktree::isolated`]); `mine --verify`,
//! where no agent runs, uses a detached worktree ([`Worktree::create`]).

use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    DiffStat, EvalError,
    git::{git, git_with_env},
};

/// The subject of an isolated working copy's one commit, ahead of the
/// base commit's name: `git log` there says whose files it holds.
pub const START_SUBJECT: &str = "the files of commit";

/// A working copy of a repository at a commit, removed by
/// [`Worktree::remove`].
#[derive(Debug)]
pub struct Worktree {
    path: PathBuf,
    /// The commit, in the working copy's own repository, that it began at:
    /// what [`Worktree::diff`] and [`Worktree::restore_and_apply`] compare
    /// with.
    start: String,
    /// The repository a detached worktree belongs to; `None` for an
    /// isolated working copy.
    linked_to: Option<PathBuf>,
}

impl Worktree {
    /// Adds a detached worktree of `repo_root` at `commit`, at
    /// `parent_dir/name` (which must not exist; `parent_dir` is created).
    ///
    /// # Errors
    /// When `git worktree add` fails.
    pub fn create(
        repo_root: &Path,
        parent_dir: &Path,
        name: &str,
        commit: &str,
    ) -> Result<Self, EvalError> {
        fs::create_dir_all(parent_dir).map_err(|error| EvalError::io(parent_dir, error))?;
        let path = parent_dir.join(name);
        let target = path.to_string_lossy().into_owned();
        git(
            repo_root,
            &["worktree", "add", "--detach", "--quiet", &target, commit],
        )?;
        Ok(Self {
            path,
            start: commit.to_owned(),
            linked_to: Some(repo_root.to_path_buf()),
        })
    }

    /// Writes the files of `commit` in `repo_root`, but those at `hidden`
    /// (paths relative to its root), to `parent_dir/name` (which must not
    /// exist; `parent_dir` is created) and makes that a repository of its
    /// own whose one commit holds them: no history, no other branch or tag,
    /// no link back to `repo_root`, so nothing `git` shows there holds a
    /// hidden file or a later commit. `repo_root`'s index, worktrees and
    /// refs are not touched.
    ///
    /// # Errors
    /// When a directory or file cannot be made or removed, or `git` fails.
    pub fn isolated(
        repo_root: &Path,
        parent_dir: &Path,
        name: &str,
        commit: &str,
        hidden: &[PathBuf],
    ) -> Result<Self, EvalError> {
        fs::create_dir_all(parent_dir).map_err(|error| EvalError::io(parent_dir, error))?;
        let path = std::path::absolute(parent_dir.join(name))
            .map_err(|error| EvalError::io(parent_dir, error))?;
        fs::create_dir(&path).map_err(|error| EvalError::io(&path, error))?;
        let index = path.with_extension("index");
        let written = check_out(repo_root, &path, &index, commit);
        let _ = fs::remove_file(&index);
        written?;
        for hidden in hidden {
            let file = path.join(hidden);
            if fs::symlink_metadata(&file).is_ok_and(|meta| !meta.is_dir()) {
                fs::remove_file(&file).map_err(|error| EvalError::io(&file, error))?;
                // A directory it leaves empty goes too.
                let mut dir = file.parent();
                while let Some(parent) = dir.filter(|parent| *parent != path.as_path()) {
                    if fs::remove_dir(parent).is_err() {
                        break;
                    }
                    dir = parent.parent();
                }
            }
        }
        git(
            &path,
            &["init", "--quiet", "--template=", "--initial-branch=main"],
        )?;
        git(&path, &["add", "--all", "--force"])?;
        git(
            &path,
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
                "-m",
                &format!("{START_SUBJECT} {commit}"),
            ],
        )?;
        let start = crate::git::commit(&path, "HEAD")?;
        Ok(Self {
            path,
            start,
            linked_to: None,
        })
    }

    /// The worktree's root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The commit it began at, in its own repository.
    #[must_use]
    pub fn start(&self) -> &str {
        &self.start
    }

    /// Writes everything the working copy holds that differs from the
    /// commit it began at (commits the agent made, staged and unstaged
    /// changes, new files) as a binary diff to `out`, and counts it. The
    /// index is left with every change staged.
    ///
    /// # Errors
    /// When `git` fails or `out` cannot be written.
    pub fn diff(&self, out: &Path) -> Result<DiffStat, EvalError> {
        let from = self.start.as_str();
        git(&self.path, &["add", "--all"])?;
        let patch = git(&self.path, &["diff", "--cached", "--binary", from])?;
        fs::write(out, &patch).map_err(|error| EvalError::io(out, error))?;
        let numstat = git(&self.path, &["diff", "--cached", "--numstat", from])?;
        let mut stat = DiffStat {
            sha256: crate::hex(&Sha256::digest(patch.as_bytes())),
            ..DiffStat::default()
        };
        for line in numstat.lines() {
            let mut fields = line.split('\t');
            let insertions = fields.next().and_then(|field| field.parse::<u64>().ok());
            let deletions = fields.next().and_then(|field| field.parse::<u64>().ok());
            stat.files += 1;
            stat.insertions += insertions.unwrap_or(0);
            stat.deletions += deletions.unwrap_or(0);
        }
        Ok(stat)
    }

    /// Applies `patch` (a file outside the worktree) to the worktree's
    /// files; a patch that does not apply cleanly changes nothing.
    ///
    /// # Errors
    /// [`EvalError::Git`] when it does not apply.
    pub fn apply(&self, patch: &Path) -> Result<(), EvalError> {
        let patch = patch.to_string_lossy().into_owned();
        git(
            &self.path,
            &["apply", "--binary", "--whitespace=nowarn", &patch],
        )
        .map(|_| ())
    }

    /// Puts every path `patch` touches back as it is at the commit the
    /// working copy began at (a path that commit does not have is deleted),
    /// then applies `patch`: the check patch's files replace the agent's
    /// versions of them, so an agent that added its own tests to the same
    /// file does not make the patch conflict. The agent's other changes
    /// stay.
    ///
    /// # Errors
    /// [`EvalError::Git`] when `git` fails or the patch does not apply.
    pub fn restore_and_apply(&self, patch: &Path) -> Result<(), EvalError> {
        let start = self.start.as_str();
        let file = patch.to_string_lossy().into_owned();
        let listing = git(&self.path, &["apply", "--numstat", "-z", &file])?;
        for path in touched(&listing) {
            let at_start = format!("{start}:{path}");
            if git(&self.path, &["cat-file", "-e", &at_start]).is_ok() {
                git(&self.path, &["checkout", start, "--", path])?;
            } else {
                let full = self.path.join(path);
                if fs::symlink_metadata(&full).is_ok() {
                    fs::remove_file(&full).map_err(|error| EvalError::io(&full, error))?;
                }
            }
        }
        self.apply(patch)
    }

    /// Removes the working copy (and a worktree's administrative files).
    ///
    /// # Errors
    /// When `git worktree remove` fails or the directory cannot be removed.
    pub fn remove(self) -> Result<(), EvalError> {
        match &self.linked_to {
            Some(repo_root) => {
                let target = self.path.to_string_lossy().into_owned();
                git(repo_root, &["worktree", "remove", "--force", &target]).map(|_| ())
            }
            None => {
                fs::remove_dir_all(&self.path).map_err(|error| EvalError::io(&self.path, error))
            }
        }
    }
}

/// Writes the files of `commit` in `repo_root` to the empty directory
/// `dir` through the index file `index`, so that `repo_root`'s own index
/// is not touched.
fn check_out(repo_root: &Path, dir: &Path, index: &Path, commit: &str) -> Result<(), EvalError> {
    let work_tree = dir.to_string_lossy().into_owned();
    let env = [("GIT_INDEX_FILE", index.as_os_str())];
    git_with_env(
        repo_root,
        &env,
        &["--work-tree", &work_tree, "read-tree", commit],
    )?;
    git_with_env(
        repo_root,
        &env,
        &["--work-tree", &work_tree, "checkout-index", "--all"],
    )?;
    Ok(())
}

/// The paths in `git apply --numstat -z` output: `added\tdeleted\tpath\0`,
/// or `added\tdeleted\t\0old\0new\0` for a rename or copy (both paths
/// listed).
fn touched(listing: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    let mut fields = listing.split('\0');
    while let Some(entry) = fields.next() {
        let Some(path) = entry.splitn(3, '\t').nth(2) else {
            continue;
        };
        if path.is_empty() {
            paths.extend(fields.next());
            paths.extend(fields.next());
        } else {
            paths.push(path);
        }
    }
    paths.retain(|path| !path.is_empty());
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.email", "eval@example.invalid"],
            &["config", "user.name", "eval"],
        ] {
            git(root, args).expect("git");
        }
        fs::write(root.join("a.txt"), "one\n").expect("write");
        git(root, &["add", "a.txt"]).expect("add");
        git(root, &["commit", "--quiet", "-m", "one"]).expect("commit");
        dir
    }

    #[test]
    fn diffs_commits_and_new_files_then_applies_and_removes() {
        let repo = repo();
        let base = crate::git::commit(repo.path(), "HEAD").expect("head");
        let parent = repo.path().join(".git").join("engine-test");
        let tree = Worktree::create(repo.path(), &parent, "t1", &base).expect("worktree");
        fs::write(tree.path().join("a.txt"), "one\ntwo\n").expect("edit");
        git(tree.path(), &["commit", "--quiet", "-am", "agent commit"]).expect("agent commits");
        fs::write(tree.path().join("b.txt"), "new\n").expect("new file");
        let out = repo.path().join("agent.diff");
        let stat = tree.diff(&out).expect("diff");
        assert_eq!((stat.files, stat.insertions, stat.deletions), (2, 2, 0));
        assert_eq!(stat.sha256.len(), 64);

        let check = repo.path().join("check.patch");
        fs::write(
            &check,
            "diff --git a/c.txt b/c.txt\nnew file mode 100644\n--- /dev/null\n+++ b/c.txt\n@@ -0,0 +1 @@\n+check\n",
        )
        .expect("patch");
        tree.apply(&check).expect("applies");
        assert!(tree.path().join("c.txt").exists());
        assert!(tree.apply(&check).is_err(), "a second apply conflicts");

        let path = tree.path().to_path_buf();
        tree.remove().expect("removed");
        assert!(!path.exists());
    }

    #[test]
    fn the_check_patch_replaces_the_agents_versions_of_its_files() {
        let repo = repo();
        let base = crate::git::commit(repo.path(), "HEAD").expect("head");
        let parent = repo.path().join(".git").join("engine-test");
        let tree = Worktree::create(repo.path(), &parent, "t2", &base).expect("worktree");
        fs::write(tree.path().join("a.txt"), "agent\n").expect("edit");
        fs::write(tree.path().join("c.txt"), "agent\n").expect("new file");
        fs::write(tree.path().join("d.txt"), "kept\n").expect("other file");
        tree.diff(&repo.path().join("agent.diff")).expect("diff");
        let check = repo.path().join("check.patch");
        fs::write(
            &check,
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-one\n+checked\n\
             diff --git a/c.txt b/c.txt\nnew file mode 100644\n--- /dev/null\n+++ b/c.txt\n@@ -0,0 +1 @@\n+check\n",
        )
        .expect("patch");
        assert!(tree.apply(&check).is_err(), "it conflicts with the agent");
        tree.restore_and_apply(&check).expect("applies");
        let read = |name: &str| fs::read_to_string(tree.path().join(name)).expect("read");
        assert_eq!(read("a.txt"), "checked\n");
        assert_eq!(read("c.txt"), "check\n");
        assert_eq!(read("d.txt"), "kept\n");
        tree.remove().expect("removed");
    }

    #[test]
    fn an_isolated_copy_has_one_commit_and_no_hidden_file() {
        let repo = repo();
        let root = repo.path();
        fs::create_dir_all(root.join("evals/deep")).expect("evals");
        fs::write(root.join("evals/deep/t.json"), "task\n").expect("task");
        fs::write(root.join("evals/t.check.patch"), "answer\n").expect("patch");
        fs::write(root.join("evals/README.md"), "kept\n").expect("readme");
        git(root, &["add", "."]).expect("add");
        git(root, &["commit", "--quiet", "-m", "evals"]).expect("commit");
        git(root, &["checkout", "--quiet", "-b", "later"]).expect("branch");
        fs::write(root.join("a.txt"), "later\n").expect("later");
        git(root, &["commit", "--quiet", "-am", "later"]).expect("later commit");
        git(root, &["checkout", "--quiet", "main"]).expect("main");
        let base = crate::git::commit(root, "HEAD").expect("head");
        let index = fs::read(root.join(".git/index")).expect("index");

        let parent = root.join(".git").join("engine-test");
        let hidden = [
            PathBuf::from("evals/deep/t.json"),
            PathBuf::from("evals/t.check.patch"),
            PathBuf::from("evals/not-at-base.json"),
        ];
        let tree = Worktree::isolated(root, &parent, "t3", &base, &hidden).expect("copy");
        let path = tree.path().to_path_buf();
        let read = |name: &str| fs::read_to_string(path.join(name)).expect("read");
        assert_eq!(read("a.txt"), "one\n");
        assert_eq!(read("evals/README.md"), "kept\n");
        assert!(!path.join("evals/t.check.patch").exists());
        assert!(
            !path.join("evals/deep").exists(),
            "an emptied directory goes too"
        );

        // Its own repository: one commit, which has no hidden file, and
        // nothing of the repository it came from.
        let own = |args: &[&str]| git(&path, args).expect("git");
        assert_eq!(own(&["rev-parse", "--git-common-dir"]).trim(), ".git");
        assert_eq!(own(&["rev-list", "--all"]).lines().count(), 1);
        assert_eq!(
            own(&["log", "--format=%s"]).trim(),
            format!("{START_SUBJECT} {base}")
        );
        assert_eq!(
            own(&["for-each-ref", "--format=%(refname)"]).trim(),
            "refs/heads/main"
        );
        assert!(git(&path, &["cat-file", "-e", &base]).is_err());
        assert!(git(&path, &["show", "HEAD:evals/t.check.patch"]).is_err());
        assert_eq!(own(&["status", "--porcelain"]), "");
        assert_eq!(tree.start(), own(&["rev-parse", "HEAD"]).trim());

        // The agent's change is the diff; the repository it came from is
        // as it was.
        fs::write(path.join("a.txt"), "two\n").expect("edit");
        let stat = tree.diff(&root.join("agent.diff")).expect("diff");
        assert_eq!((stat.files, stat.insertions, stat.deletions), (1, 1, 1));
        assert_eq!(fs::read(root.join(".git/index")).expect("index"), index);
        assert_eq!(
            git(root, &["worktree", "list"])
                .expect("list")
                .lines()
                .count(),
            1
        );
        assert!(!parent.join("t3.index").exists());
        tree.remove().expect("removed");
        assert!(!path.exists());
    }

    #[test]
    fn touched_lists_both_sides_of_a_rename() {
        assert_eq!(
            touched("1\t0\ta.txt\0-\t-\tbin.png\0"),
            ["a.txt", "bin.png"]
        );
        assert_eq!(
            touched(concat!("0\t0\t\0old.rs\0new.rs\0", "2\t1\tc.rs\0")),
            ["old.rs", "new.rs", "c.rs"]
        );
        assert!(touched("").is_empty());
    }
}
