//! `memprove-tune init` and what a task asks for: the prompt can come from
//! the pull request a commit was merged from, and a task whose hidden tests
//! use names the prompt does not give can be passed over.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tune_engine::strict_json::{JsonValue, parse_strict};

fn git(dir: &Path, tick: u32, args: &[&str]) -> String {
    let date = format!("2026-01-01T00:{tick:02}:00Z");
    let done = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("git runs");
    assert!(done.status.success(), "git {args:?}: {done:?}");
    String::from_utf8_lossy(&done.stdout).trim().to_owned()
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

fn append(dir: &Path, name: &str, text: &str) {
    let current = fs::read_to_string(dir.join(name)).unwrap_or_default();
    write(dir, name, &format!("{current}{text}"));
}

/// Commits everything and returns the first seven digits of the commit.
fn commit(dir: &Path, tick: u32, message: &str) -> String {
    git(dir, tick, &["add", "--all"]);
    git(dir, tick, &["commit", "--quiet", "-m", message]);
    git(dir, tick, &["rev-parse", "--short=7", "HEAD"])
}

/// A repository whose tests run with `make test` and whose first commit
/// holds one word.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, 0, &["init", "--quiet", "-b", "main"]);
    write(root, "Makefile", "test:\n\tsh tests/run.sh\n");
    write(root, "src/lib.txt", "one\n");
    write(root, "tests/run.sh", "grep -qx one src/lib.txt\n");
    commit(root, 1, "lib: one");
    dir
}

/// Adds `word` to the library with a test that asks for it, as one commit.
fn add_word(root: &Path, tick: u32, word: &str, message: &str) -> String {
    append(root, "src/lib.txt", &format!("{word}\n"));
    append(
        root,
        "tests/run.sh",
        &format!("grep -qx {word} src/lib.txt\n"),
    );
    commit(root, tick, message)
}

/// Runs the tool in `dir` with `gh` as the program it asks about pull
/// requests.
fn tune(dir: &Path, gh: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memprove-tune"))
        .args(args)
        .current_dir(dir)
        .env("TUNE_GH", gh)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("tune runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8")
}

fn text<'a>(value: &'a JsonValue, member: &str) -> &'a str {
    value
        .get(member)
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
}

/// The task of `id` in the JSON report.
fn reported<'a>(report: &'a JsonValue, id: &str) -> &'a JsonValue {
    report
        .get("tasks")
        .and_then(JsonValue::as_array)
        .expect("tasks")
        .iter()
        .find(|task| text(task, "id") == id)
        .unwrap_or_else(|| panic!("no task {id} in the report"))
}

/// The prompt in the task file of `id`.
fn prompt(root: &Path, id: &str) -> String {
    let file = fs::read_to_string(root.join(format!(".tune/tasks/{id}.json"))).expect("task file");
    let task = parse_strict(&file).expect("strict JSON");
    text(&task, "prompt").to_owned()
}

/// A stand-in for `gh`: it says its version, knows pull request 12, has
/// never heard of 15, and writes down what it was asked.
#[cfg(unix)]
fn stand_in_gh(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/asked.log"
case "$*" in
  "--version")
    echo "gh version 0.0.0 (stand-in)"
    ;;
  "pr view 12 --json title,body")
    printf '%s\n' '{"title":"Add two to the library","body":"<!-- say why -->\r\nThe library must also say `two`,\r\non a line of its own.\r\n\r\n<!--\nchecklist\n-->\r\n"}'
    ;;
  *)
    echo "GraphQL: Could not resolve to a PullRequest with the number of 15." >&2
    exit 1
    ;;
esac
"#;
    let path = dir.join("gh");
    fs::write(&path, script).expect("gh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[cfg(unix)]
#[test]
fn the_prompt_can_be_the_pull_request_a_commit_was_merged_from() {
    let dir = repository();
    let root = dir.path();
    let two = add_word(root, 2, "two", "lib: add two (#12)");
    let five = add_word(root, 3, "five", "lib: add five (#15)\n\nFive is said too.");
    let six = add_word(root, 4, "six", "lib: add six");
    let tools = tempfile::tempdir().expect("tempdir");
    let gh = stand_in_gh(tools.path());

    let done = tune(root, &gh, &["init", "--prompt-from", "pr", "--json"]);
    assert!(done.status.success(), "{}", stderr(&done));
    let report = parse_strict(&stdout(&done)).expect("strict JSON");

    // The pull request's title and description, without its comments and
    // with plain line ends.
    let id = format!("mined-{two}");
    assert_eq!(
        prompt(root, &id),
        "Add two to the library\n\nThe library must also say `two`,\non a line of its own."
    );
    let task = reported(&report, &id);
    assert_eq!(text(task, "prompt_from"), "pull_request");
    assert_eq!(
        task.get("pull_request").and_then(JsonValue::as_i64),
        Some(12)
    );
    // The task is still named after the commit.
    assert_eq!(text(task, "title"), "lib: add two (#12)");

    // A pull request that cannot be read: the commit message, and why.
    let id = format!("mined-{five}");
    assert_eq!(
        prompt(root, &id),
        "lib: add five (#15)\n\nFive is said too."
    );
    let task = reported(&report, &id);
    assert_eq!(text(task, "prompt_from"), "commit");
    assert!(
        text(task, "pull_request_error").contains("Could not resolve"),
        "{task:?}"
    );

    // A commit that names no pull request: nothing is asked.
    let id = format!("mined-{six}");
    assert_eq!(prompt(root, &id), "lib: add six");
    assert_eq!(text(reported(&report, &id), "prompt_from"), "commit");
    let asked = fs::read_to_string(tools.path().join("asked.log")).expect("asked.log");
    assert_eq!(
        asked.lines().collect::<Vec<_>>(),
        [
            "--version",
            "pr view 15 --json title,body",
            "pr view 12 --json title,body"
        ],
        "asked once that it is there, then newest first, once each"
    );
}

#[cfg(unix)]
#[test]
fn the_report_says_where_the_prompts_came_from() {
    let dir = repository();
    let root = dir.path();
    add_word(root, 2, "two", "lib: add two (#12)");
    add_word(root, 3, "five", "lib: add five (#15)");
    let tools = tempfile::tempdir().expect("tempdir");
    let gh = stand_in_gh(tools.path());

    let done = tune(root, &gh, &["init", "--prompt-from", "pr", "--dry-run"]);
    assert!(done.status.success(), "{}", stderr(&done));
    let said = stdout(&done);
    for expected in [
        "Prompts taken from pull requests: 1 of 2 tasks; the rest are commit messages.",
        "1 pull request could not be read with gh",
        "#15",
        "Could not resolve",
    ] {
        assert!(said.contains(expected), "missing {expected:?} in:\n{said}");
    }

    // Without the option nothing is asked and nothing is said about it.
    let plain = tune(root, &gh, &["init", "--dry-run"]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    assert!(
        !stdout(&plain).contains("pull request"),
        "{}",
        stdout(&plain)
    );
    let asked = fs::read_to_string(tools.path().join("asked.log")).expect("asked.log");
    assert_eq!(asked.lines().count(), 3, "{asked}");
}

#[test]
fn prompt_from_pr_needs_gh_and_a_known_source() {
    let dir = repository();
    let root = dir.path();
    add_word(root, 2, "two", "lib: add two (#12)");
    let missing = root.join("no-such-dir").join("gh");

    let done = tune(root, &missing, &["init", "--prompt-from", "pr"]);
    assert_eq!(done.status.code(), Some(2), "{}", stderr(&done));
    let said = stderr(&done);
    assert!(said.contains("--prompt-from pr"), "{said}");
    assert!(said.contains("GitHub CLI"), "{said}");
    assert!(!root.join(".tune").exists(), "nothing is written");

    let done = tune(root, &missing, &["init", "--prompt-from", "issue"]);
    assert_eq!(done.status.code(), Some(2), "{}", stderr(&done));
    assert!(
        stderr(&done).contains("`commit` or `pr`"),
        "{}",
        stderr(&done)
    );
}

#[test]
fn named_only_passes_over_a_task_whose_tests_use_names_the_prompt_lacks() {
    let dir = repository();
    let root = dir.path();
    // The message gives the word the test asks for.
    let given = add_word(root, 2, "frobnicate_gadget", "lib: add `Frobnicate_Gadget`");
    // The word was in the repository before the change.
    write(root, "docs/words.txt", "a sprocket_wheel is a word too\n");
    commit(root, 3, "docs: a word list");
    let known = add_word(root, 4, "sprocket_wheel", "lib: add the next word");
    // The test asks for a word that the change brings in and the message
    // does not give.
    let hidden = add_word(root, 5, "frobnicate_widget", "lib: add a widget word");
    let missing = root.join("no-gh");

    let done = tune(root, &missing, &["init", "--named-only", "--limit", "2"]);
    assert!(done.status.success(), "{}", stderr(&done));
    let said = stdout(&done);
    for expected in [
        "2 tasks written to .tune/tasks",
        "1 commit was passed over by --named-only",
        &format!("mined-{hidden}"),
        "names: frobnicate_widget",
    ] {
        assert!(said.contains(expected), "missing {expected:?} in:\n{said}");
    }
    // It does not count towards the limit, and no file is written for it.
    assert!(
        root.join(format!(".tune/tasks/mined-{known}.json"))
            .exists()
    );
    assert!(
        root.join(format!(".tune/tasks/mined-{given}.json"))
            .exists()
    );
    assert!(
        !root
            .join(format!(".tune/tasks/mined-{hidden}.json"))
            .exists()
    );
    assert!(
        !root
            .join(format!(".tune/tasks/mined-{hidden}.check.patch"))
            .exists()
    );

    // The JSON report names it too.
    let again = tune(root, &missing, &["init", "--named-only", "--json"]);
    assert!(again.status.success(), "{}", stderr(&again));
    let report = parse_strict(&stdout(&again)).expect("strict JSON");
    let task = reported(&report, &format!("mined-{hidden}"));
    assert_eq!(text(task, "status"), "unnamed");
    assert!(
        text(task, "reason").contains("frobnicate_widget"),
        "{task:?}"
    );

    // Without the option the task is written like the others.
    let plain = tune(root, &missing, &["init"]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    assert!(
        root.join(format!(".tune/tasks/mined-{hidden}.json"))
            .exists()
    );
}
