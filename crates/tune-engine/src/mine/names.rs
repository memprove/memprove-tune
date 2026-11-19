//! The names a task's hidden tests use that its change brings in. An agent
//! that is not told such a name cannot pass the tests, whatever it builds:
//! the tests call a function it had to guess the name of.

use std::{
    collections::BTreeSet,
    path::Path,
    process::{Command, Stdio},
};

use super::history::Candidate;
use crate::{EvalError, git::git};

/// A shorter word than this is not looked at.
const MIN_NAME: usize = 3;

/// At most this many names of one commit are looked up in its parent.
const MAX_LOOKED_UP: usize = 200;

/// The names in the lines a diff adds: runs of ASCII letters, digits and
/// `_` that do not start with a digit, at least [`MIN_NAME`] long.
fn added_names(diff: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    for line in diff.lines() {
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if added.starts_with("++ ") {
            // `+++ b/path`, the header of a file.
            continue;
        }
        for word in added.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if word.len() >= MIN_NAME && !word.starts_with(|c: char| c.is_ascii_digit()) {
                names.insert(word);
            }
        }
    }
    names
}

/// The names the hidden tests of `candidate` (its `test_patch`) use that
/// its change to other code brings in and `prompt` does not give: in a
/// line the tests add and in a line the code adds, nowhere in the parent
/// commit's files as a whole word, and not in the prompt in any case of
/// letters. In order, without repeats.
///
/// # Errors
/// When `git` fails.
pub(super) fn unnamed(
    root: &Path,
    candidate: &Candidate,
    test_patch: &str,
    prompt: &str,
) -> Result<Vec<String>, EvalError> {
    let used = added_names(test_patch);
    if used.is_empty() || candidate.sources.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "diff".to_owned(),
        "--no-renames".to_owned(),
        "--unified=0".to_owned(),
        candidate.parent.clone(),
        candidate.commit.clone(),
        "--".to_owned(),
    ];
    args.extend(
        candidate
            .sources
            .iter()
            .map(|path| format!(":(literal){path}")),
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let change = git(root, &args)?;
    let brought = added_names(&change);
    let prompt = prompt.to_lowercase();
    let mut names = Vec::new();
    for name in used
        .intersection(&brought)
        .filter(|name| !prompt.contains(&name.to_lowercase()))
        .take(MAX_LOOKED_UP)
    {
        if !in_tree(root, &candidate.parent, name)? {
            names.push((*name).to_owned());
        }
    }
    Ok(names)
}

/// Whether any text file of `commit` holds `name` as a whole word.
fn in_tree(root: &Path, commit: &str, name: &str) -> Result<bool, EvalError> {
    let args = ["grep", "-q", "-I", "-w", "-F", "-e", name, commit, "--"];
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| EvalError::Spawn {
            program: "git".to_owned(),
            source,
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(EvalError::Git {
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_from_added_lines_only() {
        let diff = "\
diff --git a/src/lib.py b/src/lib.py
--- a/src/lib.py
+++ b/src/new_file.py
@@ -1,2 +1,3 @@
 def kept_function():
-    return old_name(1)
+    return parse_header(x, retries=3) + 2nd + __all__
+# id is ok
";
        let names: Vec<&str> = added_names(diff).into_iter().collect();
        assert_eq!(names, ["__all__", "parse_header", "retries", "return"]);
    }
}
