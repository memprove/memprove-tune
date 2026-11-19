//! A task's prompt from the pull request its commit was merged from, read
//! with the GitHub CLI: a squash merge leaves a commit whose message is one
//! line, and what was asked for is in the pull request.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    EvalError,
    process::{self, Exit, Invocation},
    strict_json::{JsonValue, parse_strict},
};

/// The variable that names the `gh` program; unset or empty is `gh` on
/// `PATH`.
pub const GH_PROGRAM_ENV: &str = "TUNE_GH";

/// More characters than this and a description is cut, at the end of a
/// line where it has one.
pub const MAX_DESCRIPTION_CHARS: usize = 8000;

/// What a description that was cut ends with.
const CUT_NOTE: &str = "[The description goes on; it was cut here.]";

/// How long `gh` may take over one question.
const GH_TIMEOUT: Duration = Duration::from_secs(60);

/// More characters than this and what `gh` said is cut.
const MAX_ERROR_CHARS: usize = 200;

/// The number of the pull request a squash-merged commit's subject ends
/// with: 123 for `Fix the parser (#123)`.
#[must_use]
pub fn number(subject: &str) -> Option<u64> {
    let rest = subject.trim_end().strip_suffix(')')?;
    let (_, digits) = rest.rsplit_once("(#")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A pull request's description as part of a prompt: with plain line
/// ends, without its HTML comments (a template's notes to the author),
/// runs of empty lines as one, at most [`MAX_DESCRIPTION_CHARS`]
/// characters, trimmed.
#[must_use]
pub fn clean_description(body: &str) -> String {
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut kept = String::with_capacity(body.len());
    let mut rest = body.as_str();
    while let Some(start) = rest.find("<!--") {
        kept.push_str(&rest[..start]);
        let comment = &rest[start..];
        rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
    }
    kept.push_str(rest);

    let mut out = String::new();
    let mut chars = 0;
    let mut gap = false;
    for line in kept.lines().map(str::trim_end) {
        if line.is_empty() {
            gap = true;
            continue;
        }
        let breaks = match (out.is_empty(), gap) {
            (true, _) => "",
            (false, false) => "\n",
            (false, true) => "\n\n",
        };
        let length = line.chars().count() + breaks.len();
        if chars + length > MAX_DESCRIPTION_CHARS {
            if out.is_empty() {
                out.extend(line.chars().take(MAX_DESCRIPTION_CHARS));
            }
            out.push_str("\n\n");
            out.push_str(CUT_NOTE);
            return out;
        }
        out.push_str(breaks);
        out.push_str(line);
        chars += length;
        gap = false;
    }
    out
}

/// The prompt of a pull request: its title, then its description.
fn prompt(title: &str, body: &str) -> String {
    let description = clean_description(body);
    if description.is_empty() {
        title.to_owned()
    } else {
        format!("{title}\n\n{description}")
    }
}

/// The GitHub CLI, asked from the repository root, which is how it knows
/// the repository.
pub(super) struct Gh<'a> {
    /// The program.
    pub program: &'a str,
    /// The worktree root.
    pub root: &'a Path,
    /// A directory for its answers while they are read.
    pub scratch: PathBuf,
}

impl Gh<'_> {
    /// Asks `gh` for its version, to find out before any work that it can
    /// be started.
    ///
    /// # Errors
    /// [`EvalError::Usage`] when it cannot.
    pub(super) fn available(&self) -> Result<(), EvalError> {
        match self.ask(&["--version"]) {
            Err(EvalError::Spawn { program, source }) => Err(EvalError::Usage(format!(
                "--prompt-from pr reads pull requests with the GitHub CLI, and `{program}` cannot be started ({source}). Install it and sign in with `gh auth login`, name the program with {GH_PROGRAM_ENV}, or leave the option out to use commit messages."
            ))),
            other => other.map(|_| ()),
        }
    }

    /// The prompt of pull request `number`: its title and description.
    /// The inner error says in one line why it could not be read.
    ///
    /// # Errors
    /// When `gh` cannot be started or its answer cannot be kept.
    pub(super) fn prompt(&self, number: u64) -> Result<Result<String, String>, EvalError> {
        let number = number.to_string();
        let (exit, out, err) = self.ask(&["pr", "view", &number, "--json", "title,body"])?;
        if !exit.success() {
            let said = err
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.chars().take(MAX_ERROR_CHARS).collect::<String>());
            return Ok(Err(said.unwrap_or_else(|| match exit {
                Exit::TimedOut => format!("gh ran past {} seconds", GH_TIMEOUT.as_secs()),
                Exit::Code(code) => format!("gh exited with code {code} and said nothing"),
                Exit::Signal(signal) => format!("gh was ended by signal {signal}"),
            })));
        }
        let Ok(answer) = parse_strict(&out) else {
            return Ok(Err("gh did not answer in JSON".to_owned()));
        };
        let member = |name: &str| answer.get(name).and_then(JsonValue::as_str);
        Ok(match member("title").map(str::trim) {
            Some(title) if !title.is_empty() => Ok(prompt(title, member("body").unwrap_or(""))),
            _ => Err("the pull request has no title".to_owned()),
        })
    }

    /// Runs `gh` with `args` and returns how it ended and what it wrote to
    /// standard output and standard error.
    fn ask(&self, args: &[&str]) -> Result<(Exit, String, String), EvalError> {
        fs::create_dir_all(&self.scratch).map_err(|error| EvalError::io(&self.scratch, error))?;
        let stdout = self.scratch.join("gh.out");
        let stderr = self.scratch.join("gh.err");
        let mut command = vec![self.program.to_owned()];
        command.extend(args.iter().map(|arg| (*arg).to_owned()));
        let finished = process::run(&Invocation {
            argv: &command,
            cwd: self.root,
            env: &[
                ("GH_PROMPT_DISABLED".to_owned(), "1".to_owned()),
                ("NO_COLOR".to_owned(), "1".to_owned()),
            ],
            env_remove: &[],
            clear_env: false,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            timeout: GH_TIMEOUT,
        });
        let read = |path: &Path| {
            let text = fs::read(path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            let _ = fs::remove_file(path);
            text
        };
        let (out, err) = (read(&stdout), read(&stderr));
        Ok((finished?.exit, out, err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_number_is_what_the_subject_ends_with() {
        assert_eq!(number("Fix the parser (#123)"), Some(123));
        assert_eq!(number("Revert \"Add it (#7)\" (#19) \n"), Some(19));
        for subject in [
            "Fix the parser",
            "Fix #123",
            "Fix the parser (#123) again",
            "Fix the parser (#)",
            "Fix the parser (#12a)",
            "Fix the parser (123)",
            "",
        ] {
            assert_eq!(number(subject), None, "{subject}");
        }
    }

    #[test]
    fn a_description_loses_its_comments_and_keeps_its_words() {
        assert_eq!(
            clean_description(
                "<!-- why? -->\r\nIt broke.\r\n\r\n\r\n- one  \r\n- two\r\n<!--\nlist\n-->"
            ),
            "It broke.\n\n- one\n- two"
        );
        assert_eq!(
            clean_description("Before <!-- inside --> after."),
            "Before  after."
        );
        assert_eq!(clean_description("Kept.\n<!-- never closed"), "Kept.");
        assert_eq!(clean_description("  \n<!-- only this -->\n"), "");
    }

    #[test]
    fn a_long_description_is_cut_at_the_end_of_a_line() {
        let line = "x".repeat(99);
        let long = vec![line.as_str(); 200].join("\n");
        let cut = clean_description(&long);
        let (kept, note) = cut.rsplit_once("\n\n").expect("a note");
        assert_eq!(note, CUT_NOTE);
        assert_eq!(kept.lines().count(), 80);
        assert!(kept.lines().all(|kept| kept == line));

        let one = clean_description(&"y".repeat(MAX_DESCRIPTION_CHARS + 5));
        assert_eq!(
            one,
            format!("{}\n\n{CUT_NOTE}", "y".repeat(MAX_DESCRIPTION_CHARS))
        );
        let fits = "z".repeat(MAX_DESCRIPTION_CHARS);
        assert_eq!(clean_description(&fits), fits);
    }

    #[test]
    fn the_prompt_is_the_title_then_the_description() {
        assert_eq!(
            prompt("Add two", "It says two.\n"),
            "Add two\n\nIt says two."
        );
        assert_eq!(prompt("Add two", "<!-- nothing -->"), "Add two");
    }
}
