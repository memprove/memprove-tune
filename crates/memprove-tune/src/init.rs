//! `memprove-tune init`: the first task files, from the repository's own history.
//! A commit that changed a test and code becomes a task: start from its
//! parent, ask for what its message (or its pull request) says, judge by
//! its tests.

use std::{
    fmt::Write as _,
    io::Write,
    path::{Path, PathBuf},
};

use tune_engine::{
    json::Out,
    mine::{self, MineSpec, MineStatus, MinedTask, PromptFrom, PromptSource, history, pull},
};

use crate::{cli::print, error::Error, experiment::count, scan::counted};

/// The schema of `memprove-tune init --json`.
pub(crate) const INIT_SCHEMA: &str = "tune.init.v1";

/// What `memprove-tune init` was asked to do.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one switch of the command line"
)]
pub(crate) struct Options {
    /// At most this many tasks.
    pub(crate) limit: usize,
    /// The commits to look at, in `git log` syntax; `None` for the
    /// history of HEAD.
    pub(crate) range: Option<String>,
    /// Where the task files go, from the repository root; `None` for the
    /// default place.
    pub(crate) out: Option<PathBuf>,
    /// Keep a task only when its tests fail before its commit and pass
    /// after it.
    pub(crate) verify: bool,
    /// The commands a fresh checkout needs before its tests can run,
    /// instead of the guess; none keeps the guess.
    pub(crate) setup: Vec<Vec<String>>,
    /// The test command, instead of the guess.
    pub(crate) test: Option<Vec<String>>,
    /// Pass over a commit whose check cannot be narrowed to the tests it
    /// changed.
    pub(crate) narrowed_only: bool,
    /// What the prompts are taken from.
    pub(crate) prompt_from: PromptFrom,
    /// Pass over a commit whose tests use names its change brings in and
    /// its prompt does not give.
    pub(crate) named_only: bool,
    /// Say what would be written, and write nothing.
    pub(crate) dry_run: bool,
    /// Print the report as JSON.
    pub(crate) json: bool,
}

/// A task's status as the JSON report names it.
const fn status_name(status: &MineStatus) -> &'static str {
    match status {
        MineStatus::Written => "written",
        MineStatus::DryRun => "dry_run",
        MineStatus::Exists => "exists",
        MineStatus::Rejected(_) => "rejected",
        MineStatus::Unnamed(_) => "unnamed",
    }
}

/// Why a task was not written, when it was not.
fn reason(status: &MineStatus) -> Option<String> {
    match status {
        MineStatus::Rejected(reason) => Some(reason.clone()),
        MineStatus::Unnamed(names) => Some(format!(
            "its tests use names that the change brings in and the prompt does not give: {}",
            names.join(", ")
        )),
        MineStatus::Written | MineStatus::DryRun | MineStatus::Exists => None,
    }
}

/// What the report says about prompts taken from pull requests, for the
/// tasks `new`.
fn prompts_said(new: &[&MinedTask]) -> String {
    let from_pulls = new
        .iter()
        .filter(|task| matches!(task.prompt, Some(PromptSource::PullRequest(_))))
        .count();
    let mut out = format!(
        "Prompts taken from pull requests: {from_pulls} of {}{}\n",
        counted(new.len(), "task"),
        if from_pulls == new.len() {
            "."
        } else {
            "; the rest are commit messages."
        },
    );
    let unread: Vec<(u64, &str)> = new
        .iter()
        .filter_map(|task| match &task.prompt {
            Some(PromptSource::Unread { number, error }) => Some((*number, error.as_str())),
            _ => None,
        })
        .collect();
    if let Some((number, error)) = unread.first() {
        let _ = writeln!(
            out,
            "{} could not be read with gh; the first, #{number}: {error}",
            counted(unread.len(), "pull request"),
        );
    }
    out
}

/// `count` of them "was" or "were".
const fn was(count: usize) -> &'static str {
    if count == 1 { "was" } else { "were" }
}

/// What is said when no commit can become a task.
fn none_found(options: &Options) -> String {
    let looked = options.range.as_deref().map_or_else(
        || "the history of HEAD".to_owned(),
        |range| format!("the commits of {range}"),
    );
    let narrowed = if options.narrowed_only {
        " With --narrowed-only, a commit whose check cannot be narrowed to the tests it changed is passed over too; without it, such a task runs the whole test suite."
    } else {
        ""
    };
    format!(
        "No commit in {looked} can become a task. A task is made from a commit that is not a merge and changed a test file and other code, in at most {} files and {} lines.{narrowed}\nWrite the task files by hand instead: each is a prompt and a check command (`memprove-tune init --help` shows one).\n",
        history::MAX_FILES,
        history::MAX_LINES,
    )
}

/// The report as text for a person.
fn text(options: &Options, out_dir: &str, tasks: &[MinedTask]) -> String {
    if tasks.is_empty() {
        return none_found(options);
    }
    let of = |wanted: fn(&MineStatus) -> bool| -> Vec<&MinedTask> {
        tasks.iter().filter(|task| wanted(&task.status)).collect()
    };
    let new = of(|status| matches!(status, MineStatus::Written | MineStatus::DryRun));
    let there = of(|status| matches!(status, MineStatus::Exists));
    let refused = of(|status| matches!(status, MineStatus::Rejected(_)));
    let unnamed = of(|status| matches!(status, MineStatus::Unnamed(_)));

    let mut out = format!(
        "{} {} to {out_dir}{}\n",
        counted(new.len(), "task"),
        if options.dry_run {
            "would be written"
        } else {
            "written"
        },
        if new.is_empty() { "." } else { ":" },
    );
    for task in &new {
        let _ = writeln!(
            out,
            "  {}  {}\n      check: {}",
            task.id,
            task.title,
            task.check.join(" ")
        );
    }
    if options.prompt_from == PromptFrom::PullRequest && !new.is_empty() {
        out.push_str(&prompts_said(&new));
    }
    if !there.is_empty() {
        let ids: Vec<&str> = there.iter().map(|task| task.id.as_str()).collect();
        let _ = writeln!(
            out,
            "{} {} already there and left as {}: {}",
            there.len(),
            was(there.len()),
            if there.len() == 1 {
                "it is"
            } else {
                "they are"
            },
            ids.join(", ")
        );
    }
    if !refused.is_empty() {
        let _ = writeln!(
            out,
            "{} {} refused by --verify:",
            counted(refused.len(), "commit"),
            was(refused.len()),
        );
        for task in &refused {
            if let MineStatus::Rejected(reason) = &task.status {
                let _ = writeln!(out, "  {}  {}\n      {reason}", task.id, task.title);
            }
        }
    }
    if !unnamed.is_empty() {
        let _ = writeln!(
            out,
            "{} {} passed over by --named-only: the tests use names that the change brings in and the prompt does not give.",
            counted(unnamed.len(), "commit"),
            was(unnamed.len()),
        );
        for task in &unnamed {
            if let MineStatus::Unnamed(names) = &task.status {
                let _ = writeln!(
                    out,
                    "  {}  {}\n      names: {}",
                    task.id,
                    task.title,
                    names.join(", ")
                );
            }
        }
    }
    if options.dry_run {
        out.push_str("Nothing was written (--dry-run).\n");
        return out;
    }
    if !options.verify && !new.is_empty() {
        out.push_str(
            "The tasks were not run. A task whose tests pass before the change, or fail after it, judges nothing: `memprove-tune init --verify` runs each task's tests before and after its commit and keeps the ones that behave.\n",
        );
    }
    out.push_str(if options.prompt_from == PromptFrom::PullRequest {
        "Read the prompts before you trust them: each is what a pull request or a commit message says, and it may say too little to do the work from.\n"
    } else {
        "Read the prompts before you trust them: each is a commit message, and it may say too little to do the work from.\n"
    });
    out.push_str(
        "Next: `memprove-tune scan` lists the units of the configuration, and `memprove-tune prune --dry-run` shows the plan and its cost.\n",
    );
    out
}

/// The report as one JSON document.
fn json(options: &Options, out_dir: &str, tasks: &[MinedTask]) -> String {
    let written = tasks
        .iter()
        .filter(|task| task.status == MineStatus::Written)
        .count();
    let listed = tasks
        .iter()
        .map(|task| {
            let reason = reason(&task.status);
            let mut members = vec![
                ("id", Out::str(&task.id)),
                ("commit", Out::str(&task.commit)),
                ("path", Out::str(&slashed(&task.path))),
                ("title", Out::str(&task.title)),
                ("check", Out::strs(&task.check)),
                ("status", Out::str(status_name(&task.status))),
                ("reason", Out::opt(reason.as_deref(), Out::str)),
            ];
            // Where the prompt came from; not known of a task file that
            // was already there.
            match &task.prompt {
                None => {}
                Some(PromptSource::Commit) => members.push(("prompt_from", Out::str("commit"))),
                Some(PromptSource::PullRequest(number)) => {
                    members.push(("prompt_from", Out::str("pull_request")));
                    members.push(("pull_request", Out::uint(*number)));
                }
                Some(PromptSource::Unread { number, error }) => {
                    members.push(("prompt_from", Out::str("commit")));
                    members.push(("pull_request", Out::uint(*number)));
                    members.push(("pull_request_error", Out::str(error)));
                }
            }
            Out::Object(members)
        })
        .collect();
    Out::Object(vec![
        ("schema", Out::str(INIT_SCHEMA)),
        ("out_dir", Out::str(out_dir)),
        ("range", Out::opt(options.range.as_deref(), Out::str)),
        ("verified", Out::bool(options.verify)),
        ("dry_run", Out::bool(options.dry_run)),
        ("written", Out::uint(count(written))),
        ("tasks", Out::Array(listed)),
    ])
    .render()
}

/// `path` with `/` between its parts.
fn slashed(path: &Path) -> String {
    path.iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Writes task files for the repository that holds `cwd` and reports
/// them to `out`.
pub(crate) fn run(options: &Options, cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let root = tune_engine::git::toplevel(cwd)?;
    let mut spec = MineSpec::new(&root);
    spec.limit = options.limit;
    spec.range.clone_from(&options.range);
    if let Some(dir) = &options.out {
        spec.out_dir.clone_from(dir);
    }
    spec.dry_run = options.dry_run;
    spec.verify = options.verify;
    if !options.setup.is_empty() {
        spec.setup = Some(options.setup.clone());
    }
    spec.test.clone_from(&options.test);
    spec.narrowed_only = options.narrowed_only;
    spec.prompt_from = options.prompt_from;
    spec.named_only = options.named_only;
    if let Some(gh) = std::env::var(pull::GH_PROGRAM_ENV)
        .ok()
        .filter(|gh| !gh.is_empty())
    {
        spec.gh = gh;
    }
    if options.verify && !options.dry_run {
        eprintln!(
            "memprove-tune: running each task's tests before and after its commit; this can take a while"
        );
    }
    let tasks = mine::mine(&spec)?;
    // As the task files name it: from the root, whatever was given.
    let out_dir = spec
        .out_dir
        .strip_prefix(&root)
        .map_or_else(|_| slashed(&spec.out_dir), slashed);
    print(
        out,
        &if options.json {
            json(options, &out_dir, &tasks)
        } else {
            text(options, &out_dir, &tasks)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: MineStatus) -> MinedTask {
        MinedTask {
            id: id.to_owned(),
            commit: "0123456789abcdef".to_owned(),
            path: Path::new(".tune/tasks").join(format!("{id}.json")),
            title: "lib: add two".to_owned(),
            check: vec!["make".to_owned(), "test".to_owned()],
            prompt: None,
            status,
        }
    }

    fn options() -> Options {
        Options {
            limit: 10,
            range: None,
            out: None,
            verify: false,
            setup: Vec::new(),
            test: None,
            narrowed_only: false,
            prompt_from: PromptFrom::Commit,
            named_only: false,
            dry_run: false,
            json: false,
        }
    }

    #[test]
    fn one_of_each_is_said_in_the_singular() {
        let tasks = [
            task("mined-a", MineStatus::Written),
            task("mined-b", MineStatus::Exists),
            task(
                "mined-c",
                MineStatus::Rejected("it passed before".to_owned()),
            ),
        ];
        let mut options = options();
        options.verify = true;
        let said = text(&options, ".tune/tasks", &tasks);
        assert!(said.contains("1 task written to .tune/tasks:"), "{said}");
        assert!(
            said.contains("1 was already there and left as it is: mined-b"),
            "{said}"
        );
        assert!(said.contains("1 commit was refused by --verify:"), "{said}");
        assert!(said.contains("it passed before"), "{said}");
        // Verified tasks need no warning that they were not run.
        assert!(!said.contains("were not run"), "{said}");
    }

    #[test]
    fn nothing_found_says_what_a_task_is_made_from() {
        let said = text(&options(), ".tune/tasks", &[]);
        assert!(said.contains("the history of HEAD"), "{said}");
        assert!(said.contains("20 files and 600 lines"), "{said}");
        let mut ranged = options();
        ranged.range = Some("v1..v2".to_owned());
        assert!(text(&ranged, "evals", &[]).contains("the commits of v1..v2"));
    }
}
