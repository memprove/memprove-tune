//! The agent configuration of a repository as a set of files: which paths
//! belong to it, reading it from a working tree, and keeping a copy of it
//! in a directory.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
};

use sha2::{Digest, Sha256};
use tune_engine::git::git;

use crate::error::Error;

/// One configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    /// A regular file.
    File {
        /// Its content.
        bytes: Vec<u8>,
        /// Whether its owner may execute it.
        executable: bool,
    },
    /// A symbolic link and its target, as written.
    Link(String),
}

impl Entry {
    /// A regular, non-executable file holding `text`.
    #[cfg(test)]
    pub(crate) fn text(text: &str) -> Self {
        Self::File {
            bytes: text.as_bytes().to_vec(),
            executable: false,
        }
    }

    /// The file's content as text; `None` for a link or bytes that are not
    /// UTF-8.
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Self::File { bytes, .. } => std::str::from_utf8(bytes).ok(),
            Self::Link(_) => None,
        }
    }
}

/// Configuration files by their path relative to the repository root,
/// with `/` between components.
pub(crate) type FileMap = BTreeMap<String, Entry>;

/// The names of instruction files.
pub(crate) const INSTRUCTION_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// Whether `path` (relative to the repository root, `/` between
/// components) is part of the agent configuration under test: an
/// instruction file at any depth, or a file under a `.claude/rules/` or
/// `.claude/skills/` directory at any depth.
pub(crate) fn is_config_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let Some((name, dirs)) = parts.split_last() else {
        return false;
    };
    if INSTRUCTION_FILES.contains(name) {
        return true;
    }
    dirs.windows(2)
        .any(|pair| pair[0] == ".claude" && (pair[1] == "rules" || pair[1] == "skills"))
}

/// The agent configuration in the working tree of `repo_root`: the files
/// of [`is_config_path`] that Git tracks or would add (ignored files are
/// left out), as they are on disk now.
pub(crate) fn read_config(repo_root: &Path) -> Result<FileMap, Error> {
    let listing = git(
        repo_root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut files = FileMap::new();
    for path in listing.split('\0').filter(|path| is_config_path(path)) {
        if let Some(entry) = read_entry(&repo_root.join(path))? {
            files.insert(path.to_owned(), entry);
        }
    }
    Ok(files)
}

/// The file or link at `path`; `None` when there is neither.
fn read_entry(path: &Path) -> Result<Option<Entry>, Error> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if meta.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|error| Error::io(path, error))?;
        return Ok(Some(Entry::Link(target.to_string_lossy().into_owned())));
    }
    if !meta.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|error| Error::io(path, error))?;
    Ok(Some(Entry::File {
        bytes,
        executable: is_executable(&meta),
    }))
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

/// A name for the content of `files`: the SHA-256, in hex, of every path
/// with its kind and content, in path order.
pub(crate) fn digest(files: &FileMap) -> String {
    let mut hash = Sha256::new();
    for (path, entry) in files {
        let (kind, content): (&[u8], &[u8]) = match entry {
            Entry::File {
                bytes,
                executable: false,
            } => (b"file", bytes),
            Entry::File {
                bytes,
                executable: true,
            } => (b"exec", bytes),
            Entry::Link(target) => (b"link", target.as_bytes()),
        };
        // Lengths make the encoding unambiguous.
        for part in [path.as_bytes(), kind, content] {
            hash.update((part.len() as u64).to_be_bytes());
            hash.update(part);
        }
    }
    tune_engine::hex(&hash.finalize())
}

/// Writes `files` under `dir` (created; expected to be empty).
pub(crate) fn write_tree(files: &FileMap, dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(dir).map_err(|error| Error::io(dir, error))?;
    for (path, entry) in files {
        write_entry(dir, path, entry)?;
    }
    Ok(())
}

/// Writes `entry` at `root/path`, replacing what is there.
pub(crate) fn write_entry(root: &Path, path: &str, entry: &Entry) -> Result<(), Error> {
    if !is_plain_relative(path) {
        return Err(Error::Invalid(format!(
            "{path} is not a path inside the repository"
        )));
    }
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    if fs::symlink_metadata(&full).is_ok() {
        fs::remove_file(&full).map_err(|error| Error::io(&full, error))?;
    }
    match entry {
        Entry::File { bytes, executable } => {
            fs::write(&full, bytes).map_err(|error| Error::io(&full, error))?;
            set_mode(&full, *executable)
        }
        Entry::Link(target) => make_link(target, &full),
    }
}

/// Whether `path` has only ordinary components: no root, no `..`.
fn is_plain_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| Error::io(path, error))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) -> Result<(), Error> {
    Ok(())
}

#[cfg(unix)]
fn make_link(target: &str, path: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, path).map_err(|error| Error::io(path, error))
}

#[cfg(not(unix))]
fn make_link(target: &str, path: &Path) -> Result<(), Error> {
    fs::write(path, target).map_err(|error| Error::io(path, error))
}

/// Reads back what [`write_tree`] wrote under `dir`.
pub(crate) fn read_tree(dir: &Path) -> Result<FileMap, Error> {
    let mut files = FileMap::new();
    if dir.is_dir() {
        read_dir_into(dir, "", &mut files)?;
    }
    Ok(files)
}

fn read_dir_into(dir: &Path, prefix: &str, files: &mut FileMap) -> Result<(), Error> {
    let entries = fs::read_dir(dir).map_err(|error| Error::io(dir, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::io(dir, error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let full = entry.path();
        let is_dir = fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_dir());
        if is_dir {
            read_dir_into(&full, &path, files)?;
        } else if let Some(read) = read_entry(&full)? {
            files.insert(path, read);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A new repository in a temporary directory, with an identity set.
    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"],
            &["config", "user.name", "test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(dir.path(), args).expect("git");
        }
        dir
    }

    /// Writes `text` to `root/name`, creating its directories.
    pub(crate) fn write(root: &Path, name: &str, text: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, text).expect("write");
    }

    /// Commits everything in `root`.
    pub(crate) fn commit(root: &Path, message: &str) -> String {
        git(root, &["add", "--all"]).expect("add");
        git(root, &["commit", "--quiet", "--allow-empty", "-m", message]).expect("commit");
        git(root, &["rev-parse", "HEAD"])
            .expect("head")
            .trim()
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::*, *};

    #[test]
    fn config_paths_are_instruction_files_rules_and_skills() {
        for path in [
            "CLAUDE.md",
            "AGENTS.md",
            "web/CLAUDE.md",
            "a/b/AGENTS.md",
            ".claude/CLAUDE.md",
            ".claude/rules/style.md",
            ".claude/rules/frontend/react.md",
            ".claude/skills/deploy/SKILL.md",
            ".claude/skills/deploy/scripts/run.sh",
            "web/.claude/skills/x/SKILL.md",
        ] {
            assert!(is_config_path(path), "{path}");
        }
        for path in [
            "README.md",
            "CLAUDE.local.md",
            "docs/CLAUDE.md.bak",
            "claude.md",
            ".claude/settings.json",
            ".claude/commands/x.md",
            ".claude/agents/reviewer.md",
            ".claude/rules.md",
            "rules/style.md",
            "skills/deploy/SKILL.md",
            ".mcp.json",
        ] {
            assert!(!is_config_path(path), "{path}");
        }
    }

    #[test]
    fn reads_tracked_and_new_files_but_not_ignored_ones() {
        let repo = repo();
        let root = repo.path();
        write(root, "CLAUDE.md", "# Rules\n");
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, ".gitignore", "node_modules/\nvendor/\n");
        commit(root, "one");
        write(root, ".claude/rules/new.md", "new, not added yet\n");
        write(root, "node_modules/pkg/CLAUDE.md", "ignored\n");
        write(root, "vendor/AGENTS.md", "ignored\n");
        write(root, "CLAUDE.local.md", "local\n");
        write(root, "CLAUDE.md", "# Rules\nedited, not committed\n");
        let files = read_config(root).expect("reads");
        let paths: Vec<&str> = files.keys().map(String::as_str).collect();
        assert_eq!(paths, [".claude/rules/new.md", "CLAUDE.md"]);
        assert_eq!(
            files["CLAUDE.md"].as_text(),
            Some("# Rules\nedited, not committed\n")
        );
    }

    #[test]
    fn a_tracked_file_that_was_deleted_is_not_read() {
        let repo = repo();
        let root = repo.path();
        write(root, "CLAUDE.md", "x\n");
        write(root, "AGENTS.md", "y\n");
        commit(root, "one");
        fs::remove_file(root.join("AGENTS.md")).expect("remove");
        let files = read_config(root).expect("reads");
        assert_eq!(files.keys().collect::<Vec<_>>(), ["CLAUDE.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn links_and_executables_survive_a_round_trip() {
        use std::os::unix::fs::PermissionsExt as _;
        let repo = repo();
        let root = repo.path();
        write(root, "AGENTS.md", "shared\n");
        std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).expect("link");
        write(
            root,
            ".claude/skills/deploy/SKILL.md",
            "---\nname: deploy\n---\n",
        );
        write(root, ".claude/skills/deploy/run.sh", "#!/bin/sh\n");
        fs::set_permissions(
            root.join(".claude/skills/deploy/run.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
        let files = read_config(root).expect("reads");
        assert_eq!(files["CLAUDE.md"], Entry::Link("AGENTS.md".to_owned()));
        assert!(matches!(
            files[".claude/skills/deploy/run.sh"],
            Entry::File {
                executable: true,
                ..
            }
        ));

        let copy = tempfile::tempdir().expect("tempdir");
        let dir = copy.path().join("files");
        write_tree(&files, &dir).expect("writes");
        assert_eq!(read_tree(&dir).expect("reads back"), files);
        assert_eq!(
            read_tree(&copy.path().join("absent")).expect("empty"),
            FileMap::new()
        );
    }

    #[test]
    fn the_digest_names_paths_kinds_and_content() {
        let mut files = FileMap::new();
        files.insert("CLAUDE.md".to_owned(), Entry::text("a\n"));
        let one = digest(&files);
        assert_eq!(one.len(), 64);
        assert_eq!(one, digest(&files.clone()));
        let mut edited = files.clone();
        edited.insert("CLAUDE.md".to_owned(), Entry::text("b\n"));
        assert_ne!(digest(&edited), one);
        let mut renamed = FileMap::new();
        renamed.insert("AGENTS.md".to_owned(), Entry::text("a\n"));
        assert_ne!(digest(&renamed), one);
        let mut linked = FileMap::new();
        linked.insert("CLAUDE.md".to_owned(), Entry::Link("a\n".to_owned()));
        assert_ne!(digest(&linked), one);
        assert_ne!(digest(&FileMap::new()), one);
    }
}
