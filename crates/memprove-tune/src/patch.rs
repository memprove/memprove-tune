//! The patch that turns one configuration into another, as `git apply`
//! reads it.

use std::{fs, path::Path, process::Command};

use crate::{
    error::Error,
    files::{self, FileMap},
};

/// Settings that keep the bytes of the files as they are, whatever the
/// person's own Git configuration says.
const AS_WRITTEN: [&str; 10] = [
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.safecrlf=false",
    "-c",
    "core.quotePath=false",
    "-c",
    "core.fileMode=true",
    "-c",
    "core.symlinks=true",
];

/// Runs `git` in `work` and returns what it printed, byte for byte.
fn git(work: &Path, args: &[&str]) -> Result<Vec<u8>, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(work)
        .args(AS_WRITTEN)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| Error::io(Path::new("git"), error))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::Invalid(format!(
            "git {} failed while writing the patch: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// The name of the Git tree that holds exactly `files`, made in `work`.
fn tree(files: &FileMap, work: &Path) -> Result<String, Error> {
    files::write_tree(files, work)?;
    git(work, &["add", "--all", "--force"])?;
    let name = git(work, &["write-tree"])?;
    for path in files.keys() {
        let file = work.join(path);
        fs::remove_file(&file).map_err(|error| Error::io(&file, error))?;
    }
    Ok(String::from_utf8_lossy(&name).trim().to_owned())
}

/// The Git patch that turns the configuration `before` into `after`, with
/// paths from the repository root; empty when the two are the same. `work`
/// is a directory this makes, fills and removes; it must not hold anything.
pub(crate) fn between(before: &FileMap, after: &FileMap, work: &Path) -> Result<Vec<u8>, Error> {
    let in_use = fs::read_dir(work).is_ok_and(|mut entries| entries.next().is_some());
    if in_use || work.is_file() {
        return Err(Error::Invalid(format!(
            "{} is in the way of writing the patch; remove it and run the command again",
            work.display()
        )));
    }
    fs::create_dir_all(work).map_err(|error| Error::io(work, error))?;
    let made = (|| {
        git(work, &["init", "--quiet"])?;
        let old = tree(before, work)?;
        let new = tree(after, work)?;
        git(
            work,
            &[
                "diff",
                "--binary",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                &old,
                &new,
            ],
        )
    })();
    let _ = fs::remove_dir_all(work);
    made
}

#[cfg(test)]
mod tests {
    use tune_engine::git::git;

    use super::{between, *};
    use crate::files::{self, Entry, testing::*};

    fn text(path: &str, text: &str) -> (String, Entry) {
        (path.to_owned(), Entry::text(text))
    }

    fn before() -> FileMap {
        FileMap::from([
            text(
                "CLAUDE.md",
                "Intro.\n\n## Setup\nRun it.\n\n## Style\nTabs.\n\n## Tests\nRun them.\n",
            ),
            text(".claude/rules/old.md", "Old rule.\n"),
            text(".claude/rules/kept.md", "Kept rule, no newline at the end"),
            text(
                ".claude/skills/ship/SKILL.md",
                "---\nname: ship\n---\nShip it.\n",
            ),
            (
                ".claude/skills/ship/run.sh".to_owned(),
                Entry::File {
                    bytes: b"#!/bin/sh\necho ship\n".to_vec(),
                    executable: true,
                },
            ),
            (
                ".claude/skills/ship/logo.bin".to_owned(),
                Entry::File {
                    bytes: vec![0, 159, 146, 150, 0, 255, 10, 13, 0],
                    executable: false,
                },
            ),
            (
                "docs/CLAUDE.md".to_owned(),
                Entry::Link("../CLAUDE.md".to_owned()),
            ),
        ])
    }

    /// `before` without the Style section, the old rule, the skill and
    /// the link.
    fn after() -> FileMap {
        FileMap::from([
            text(
                "CLAUDE.md",
                "Intro.\n\n## Setup\nRun it.\n\n## Tests\nRun them.\n",
            ),
            text(".claude/rules/kept.md", "Kept rule, no newline at the end"),
        ])
    }

    #[test]
    fn applying_the_patch_to_the_first_configuration_gives_the_second() {
        let repo = repo();
        files::write_tree(&before(), repo.path()).expect("write");
        write(repo.path(), "src/main.rs", "fn main() {}\n");
        commit(repo.path(), "start");
        let work = repo.path().join(".git/patch-work");

        let patch = between(&before(), &after(), &work).expect("patch");
        let written = String::from_utf8_lossy(&patch).into_owned();
        assert!(
            written.contains("--- a/CLAUDE.md\n+++ b/CLAUDE.md\n"),
            "{written}"
        );
        assert!(written.contains("-## Style\n"), "{written}");
        assert!(!work.exists(), "the work directory is left behind");

        let file = repo.path().join("the.patch");
        fs::write(&file, &patch).expect("write the patch");
        git(repo.path(), &["apply", "the.patch"]).expect("the patch applies");
        fs::remove_file(&file).expect("remove");
        assert_eq!(files::read_config(repo.path()).expect("read"), after());
        // Nothing outside the configuration was touched.
        assert_eq!(
            fs::read_to_string(repo.path().join("src/main.rs")).expect("read"),
            "fn main() {}\n"
        );
    }

    #[test]
    fn the_same_configuration_twice_gives_no_patch_and_the_patch_is_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        assert!(
            between(&before(), &before(), &work)
                .expect("patch")
                .is_empty()
        );
        let first = between(&before(), &after(), &work).expect("patch");
        let second = between(&before(), &after(), &work).expect("patch");
        assert!(!first.is_empty());
        assert_eq!(first, second);
    }

    #[test]
    fn a_work_directory_that_holds_something_is_not_emptied() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        fs::create_dir_all(&work).expect("mkdir");
        fs::write(work.join("precious"), "x").expect("write");
        assert!(between(&before(), &after(), &work).is_err());
        assert!(work.join("precious").exists());
    }
}
