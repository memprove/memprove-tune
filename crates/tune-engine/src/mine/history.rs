//! Reading candidate commits from `git log`.

use std::path::Path;

use crate::{EvalError, git::git};

/// More files than this and a commit is too big for a task.
pub const MAX_FILES: usize = 20;
/// More changed lines (added plus removed) than this and a commit is too
/// big for a task.
pub const MAX_LINES: u64 = 600;

/// A commit that can become a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    /// Full commit name.
    pub commit: String,
    /// Its parent's full name.
    pub parent: String,
    /// The message's first line.
    pub subject: String,
    /// The subject, then the body without its trailers.
    pub prompt: String,
    /// The test files it changed, relative to the root, `/`-separated.
    pub tests: Vec<String>,
    /// The files it changed that are neither tests nor documentation.
    pub sources: Vec<String>,
}

/// The non-merge commits of `range` (the history of HEAD when `None`),
/// newest first, with their first parent; root commits are left out.
pub(super) fn commits(
    root: &Path,
    range: Option<&str>,
) -> Result<Vec<(String, String)>, EvalError> {
    let range = range.unwrap_or("HEAD");
    if range.starts_with('-') {
        return Err(EvalError::Usage(format!(
            "--range takes commits in `git log` syntax, not an option: {range}"
        )));
    }
    let log = git(root, &["log", "--no-merges", "--format=%H %P", range, "--"])?;
    Ok(log
        .lines()
        .filter_map(|line| {
            let mut names = line.split_whitespace();
            let commit = names.next()?.to_owned();
            let parent = names.next()?.to_owned();
            Some((commit, parent))
        })
        .collect())
}

/// `commit` as a candidate, or `None` when it is not one: it must change
/// at least one test file that is not a document (a note beside the tests
/// judges nothing) and at least one file that is neither a test nor
/// documentation, at most [`MAX_FILES`] files and [`MAX_LINES`] lines in
/// all, and have a subject.
pub(super) fn candidate(
    root: &Path,
    commit: &str,
    parent: &str,
) -> Result<Option<Candidate>, EvalError> {
    let numstat = git(
        root,
        &["diff", "--numstat", "-z", "--no-renames", parent, commit],
    )?;
    let mut files = 0_usize;
    let mut lines = 0_u64;
    let mut tests = Vec::new();
    let mut sources = Vec::new();
    for record in numstat.split('\0').filter(|record| !record.is_empty()) {
        let mut fields = record.splitn(3, '\t');
        let added = fields.next().and_then(|field| field.parse::<u64>().ok());
        let removed = fields.next().and_then(|field| field.parse::<u64>().ok());
        let Some(path) = fields.next() else {
            continue;
        };
        files += 1;
        lines += added.unwrap_or(0) + removed.unwrap_or(0);
        if is_test_path(path) {
            tests.push(path.to_owned());
        } else if !is_doc_path(path) {
            sources.push(path.to_owned());
        }
    }
    if tests.iter().all(|test| is_document(test))
        || sources.is_empty()
        || files > MAX_FILES
        || lines > MAX_LINES
    {
        return Ok(None);
    }
    let subject = git(root, &["log", "-1", "--format=%s", commit])?
        .trim()
        .to_owned();
    if subject.is_empty() {
        return Ok(None);
    }
    let body = git(root, &["log", "-1", "--format=%b", commit])?;
    let body = strip_trailers(&body);
    let prompt = if body.is_empty() {
        subject.clone()
    } else {
        format!("{subject}\n\n{body}")
    };
    Ok(Some(Candidate {
        commit: commit.to_owned(),
        parent: parent.to_owned(),
        subject,
        prompt,
        tests,
        sources,
    }))
}

/// Whether `path` is a test file: a directory named `test`, `tests`,
/// `spec`, `specs` or `__tests__` on its way, or a file name that starts
/// with `test_` or holds `_test.`, `.test.`, `_spec.` or `.spec.`.
#[must_use]
pub fn is_test_path(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    parts
        .iter()
        .any(|dir| matches!(*dir, "test" | "tests" | "spec" | "specs" | "__tests__"))
        || name.starts_with("test_")
        || ["_test.", ".test.", "_spec.", ".spec."]
            .iter()
            .any(|infix| name.contains(infix))
}

/// Whether `path` is documentation: under `docs/` or `doc/`, or a
/// Markdown, `reStructuredText` or `AsciiDoc` file. (Not `.txt`, which is
/// as often `CMakeLists.txt` or `requirements.txt`.)
#[must_use]
pub fn is_doc_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("docs/") || lower.starts_with("doc/") || is_document(path)
}

/// Whether `path` is a Markdown, `reStructuredText` or `AsciiDoc` file,
/// wherever it is.
fn is_document(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".rst", ".adoc"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// `body` without its trailer block (a last paragraph whose every line is
/// `Token: value`, such as `Co-Authored-By:` or `Signed-off-by:`), trimmed.
#[must_use]
pub fn strip_trailers(body: &str) -> String {
    let body = body.trim();
    let (rest, last) = body
        .rsplit_once("\n\n")
        .map_or(("", body), |(rest, last)| (rest, last));
    if !last.is_empty() && last.lines().all(is_trailer) {
        rest.trim().to_owned()
    } else {
        body.to_owned()
    }
}

fn is_trailer(line: &str) -> bool {
    let Some((token, value)) = line.split_once(": ") else {
        return false;
    };
    !value.trim().is_empty()
        && token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_files_are_told_from_others() {
        for path in [
            "tests/parse.rs",
            "crates/a/tests/e2e.rs",
            "src/test/java/FooTest.java",
            "pkg/parse_test.go",
            "web/app.test.ts",
            "web/app.spec.js",
            "spec/models/user_spec.rb",
            "test_parse.py",
            "src/__tests__/a.js",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        for path in [
            "src/latest.rs",
            "src/contest.py",
            "attest/main.go",
            "README.md",
        ] {
            assert!(!is_test_path(path), "{path}");
        }
    }

    #[test]
    fn documentation_is_recognised() {
        assert!(is_doc_path("docs/guide.html"));
        assert!(is_doc_path("CHANGELOG.md"));
        assert!(!is_doc_path("src/main.rs"));
        assert!(!is_doc_path("scripts/install.sh"));
        assert!(!is_doc_path("CMakeLists.txt"));
    }

    #[test]
    fn trailers_are_dropped_and_other_text_kept() {
        assert_eq!(
            strip_trailers(
                "Why it changed.\n\nCo-Authored-By: A <a@example.invalid>\nClaude-Session: https://x\n"
            ),
            "Why it changed."
        );
        assert_eq!(
            strip_trailers("Co-Authored-By: A <a@example.invalid>\n"),
            ""
        );
        assert_eq!(
            strip_trailers("First.\n\nSecond: not a trailer because of this line\nplain words"),
            "First.\n\nSecond: not a trailer because of this line\nplain words"
        );
        assert_eq!(strip_trailers(""), "");
    }
}
