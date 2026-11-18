//! `memprove-tune init` from the outside: task files from the repository's own
//! history, which the experiment commands then run as they are.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tune_engine::strict_json::{JsonValue, parse_strict};

/// Runs `git` in `dir` with a date that grows with `tick`, so that the
/// history is in the order it was made, and returns what it printed.
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

/// The agent: when the configuration tells it to work, it adds the word
/// the prompt asks for.
const AGENT: &str = r#"
word=$(sed -n 's/^lib: add \([a-z]*\).*/\1/p' "$TUNE_PROMPT_FILE" | head -1)
grep -qs 'do the work' CLAUDE.md && echo "$word" >> src/lib.txt
exit 0
"#;

/// What the tests mine.
struct History {
    dir: tempfile::TempDir,
    /// A commit with a test that its code passes.
    two: String,
    /// A commit whose code does not do what its test asks.
    four: String,
    /// The newest commit with a test that its code passes.
    five: String,
}

/// A repository whose tests run with `make test`, with three commits that
/// changed code and a test, two that changed only code, and a
/// configuration added last.
fn history() -> History {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, 0, &["init", "--quiet", "-b", "main"]);
    write(root, "Makefile", "test:\n\tsh tests/run.sh\n");
    write(root, "agent.sh", AGENT);
    write(root, "src/lib.txt", "one\n");
    write(root, "tests/run.sh", "grep -qx one src/lib.txt\n");
    commit(root, 1, "lib: one");

    append(root, "src/lib.txt", "two\n");
    append(root, "tests/run.sh", "grep -qx two src/lib.txt\n");
    let two = commit(root, 2, "lib: add two\n\nThe library also says two.");

    append(root, "src/lib.txt", "three\n");
    commit(root, 3, "lib: add three without a test");

    append(root, "src/lib.txt", "fuor\n");
    append(root, "tests/run.sh", "grep -qx four src/lib.txt\n");
    let four = commit(root, 4, "lib: add four");

    append(root, "src/lib.txt", "four\n");
    commit(root, 5, "lib: spell four right");

    append(root, "src/lib.txt", "five\n");
    append(root, "tests/run.sh", "grep -qx five src/lib.txt\n");
    let five = commit(root, 6, "lib: add five");

    write(
        root,
        "CLAUDE.md",
        "## Work\nAlways do the work.\n\n## Manners\nBe kind to the reader.\n",
    );
    commit(root, 7, "docs: tell the agent what to do");
    History {
        dir,
        two,
        four,
        five,
    }
}

fn tune(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memprove-tune"))
        .args(args)
        .current_dir(dir)
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

fn number(value: &JsonValue, member: &str) -> i64 {
    value.get(member).and_then(JsonValue::as_i64).unwrap_or(-1)
}

fn list<'a>(value: &'a JsonValue, member: &str) -> &'a [JsonValue] {
    value
        .get(member)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no list {member}"))
}

/// The names of the files in `dir`, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn init_writes_tasks_that_the_experiment_runs_as_they_are() {
    let h = history();
    let root = h.dir.path();

    let first = tune(root, &["init"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let said = stdout(&first);
    for expected in [
        "3 tasks written to .tune/tasks",
        &format!("mined-{}", h.two),
        &format!("mined-{}", h.five),
        "lib: add two",
        "make test",
        // Nothing was run to see whether the tasks judge anything.
        "--verify",
        "memprove-tune prune --dry-run",
    ] {
        assert!(said.contains(expected), "missing {expected:?} in:\n{said}");
    }
    // It is a tool of its own: nothing of memprove's is read or written.
    assert!(!root.join(".memprove").exists());
    let written = files(&root.join(".tune/tasks"));
    assert_eq!(written.len(), 6, "{written:?}");
    assert!(written.contains(&format!("mined-{}.json", h.two)));
    assert!(written.contains(&format!("mined-{}.check.patch", h.two)));
    let task = fs::read_to_string(root.join(format!(".tune/tasks/mined-{}.json", h.two)))
        .expect("task file");
    assert!(task.contains("\"tune.task.v1\""), "{task}");
    assert!(task.contains("The library also says two."), "{task}");

    // A second time, every task is there already and none is written again.
    let second = tune(root, &["init"]);
    assert!(second.status.success(), "{}", stderr(&second));
    let said = stdout(&second);
    assert!(said.contains("0 tasks written"), "{said}");
    assert!(said.contains("3 were already there"), "{said}");
    assert_eq!(
        fs::read_to_string(root.join(format!(".tune/tasks/mined-{}.json", h.two))).expect("task"),
        task
    );

    // The experiment finds the tasks where init put them. Each starts from
    // a commit older than the configuration, and today's configuration is
    // laid over it: the agent does the work, and the commit whose own code
    // was wrong is a task like the others.
    let ab = tune(
        root,
        &[
            "ab",
            "--drop",
            "CLAUDE.md#work",
            "--rounds",
            "1",
            "--yes",
            "--json",
            "--agent-cmd",
            "sh agent.sh",
        ],
    );
    assert!(ab.status.success(), "{}", stderr(&ab));
    let report = parse_strict(&stdout(&ab)).expect("strict JSON");
    let arms = list(&report, "arms");
    let passed = |label: &str| {
        arms.iter()
            .find(|arm| text(arm, "label") == label)
            .map_or(-1, |arm| number(arm, "passed"))
    };
    assert_eq!(passed("baseline"), 3);
    assert_eq!(passed("without CLAUDE.md#work"), 0);
}

#[test]
fn verify_keeps_only_tasks_that_fail_before_and_pass_after() {
    let h = history();
    let root = h.dir.path();
    let output = tune(
        root,
        &["init", "--verify", "--out", "evals/mined", "--json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    assert_eq!(text(&value, "schema"), "tune.init.v1");
    assert_eq!(text(&value, "out_dir"), "evals/mined");
    assert_eq!(
        value.get("verified").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(number(&value, "written"), 2);
    let tasks = list(&value, "tasks");
    let of = |commit: &str| {
        tasks
            .iter()
            .find(|task| text(task, "id") == format!("mined-{commit}"))
            .unwrap_or_else(|| panic!("no task for {commit}"))
    };
    assert_eq!(text(of(&h.five), "status"), "written");
    assert_eq!(text(of(&h.two), "status"), "written");
    assert_eq!(
        text(of(&h.two), "path"),
        format!("evals/mined/mined-{}.json", h.two)
    );
    let check: Vec<&str> = list(of(&h.two), "check")
        .iter()
        .filter_map(JsonValue::as_str)
        .collect();
    assert_eq!(check, ["make", "test"]);
    // Its own code never passed its test: nothing an agent does is judged
    // fairly by it.
    assert_eq!(text(of(&h.four), "status"), "rejected");
    assert!(
        text(of(&h.four), "reason").contains("on the commit with the change"),
        "{}",
        text(of(&h.four), "reason")
    );
    assert_eq!(files(&root.join("evals/mined")).len(), 4);

    // As text, the refused commit is named with the reason.
    let again = tune(root, &["init", "--verify", "--out", "evals/again"]);
    let said = stdout(&again);
    assert!(said.contains("2 tasks written to evals/again"), "{said}");
    assert!(said.contains("1 commit was refused"), "{said}");
    assert!(said.contains(&format!("mined-{}", h.four)), "{said}");
}

/// The words of each command of the member `member` of a task file (an
/// object for `check`, a list of objects for `setup`).
fn argvs(task: &JsonValue, member: &str) -> Vec<Vec<String>> {
    let words = |command: &JsonValue| -> Vec<String> {
        list(command, "argv")
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::to_owned)
            .collect()
    };
    match task.get(member) {
        Some(JsonValue::Array(commands)) => commands.iter().map(words).collect(),
        Some(command) => vec![words(command)],
        None => Vec::new(),
    }
}

#[test]
fn the_setup_and_test_commands_can_be_given() {
    let h = history();
    let root = h.dir.path();
    // The commands that are given are the ones --verify runs: `make test`
    // would find no Makefile rule for this, and the tasks are kept.
    let output = tune(
        root,
        &[
            "init",
            "--verify",
            "--setup",
            "cp src/lib.txt prepared.txt",
            "--setup",
            "true",
            "--test",
            "sh  tests/run.sh",
            "--json",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    assert_eq!(number(&value, "written"), 2);
    let check: Vec<&str> = list(&list(&value, "tasks")[0], "check")
        .iter()
        .filter_map(JsonValue::as_str)
        .collect();
    assert_eq!(check, ["sh", "tests/run.sh"]);

    let text = fs::read_to_string(root.join(format!(".tune/tasks/mined-{}.json", h.two)))
        .expect("task file");
    let task = parse_strict(&text).expect("strict JSON");
    assert_eq!(
        argvs(&task, "setup"),
        [vec!["cp", "src/lib.txt", "prepared.txt"], vec!["true"]]
    );
    assert_eq!(argvs(&task, "check"), [["sh", "tests/run.sh"]]);

    // A setup command that fails is why a task is refused, so it was run.
    let failing = tune(
        root,
        &[
            "init",
            "--verify",
            "--out",
            "evals/failing",
            "--setup",
            "false",
            "--json",
        ],
    );
    assert!(failing.status.success(), "{}", stderr(&failing));
    let value = parse_strict(&stdout(&failing)).expect("strict JSON");
    assert_eq!(number(&value, "written"), 0);

    let help = tune(root, &["init", "--help"]);
    let said = stdout(&help);
    assert!(said.contains("--setup"), "{said}");
    assert!(said.contains("--test"), "{said}");

    // A command needs words.
    for option in ["--setup", "--test"] {
        let output = tune(root, &["init", option, " "]);
        assert_eq!(output.status.code(), Some(2));
        assert!(
            stderr(&output).contains(&format!("`{option}` needs a command")),
            "{}",
            stderr(&output)
        );
    }
}

#[test]
fn a_dry_run_and_a_limit() {
    let h = history();
    let root = h.dir.path();
    let dry = tune(root, &["init", "--dry-run"]);
    assert!(dry.status.success(), "{}", stderr(&dry));
    let said = stdout(&dry);
    assert!(said.contains("3 tasks would be written"), "{said}");
    assert!(said.contains("Nothing was written"), "{said}");
    assert!(!root.join(".tune").exists());

    // The newest first.
    let one = tune(root, &["init", "--limit", "1", "--json"]);
    let value = parse_strict(&stdout(&one)).expect("strict JSON");
    let tasks = list(&value, "tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(text(&tasks[0], "id"), format!("mined-{}", h.five));

    // Only the commits of a range.
    let ranged = tune(
        root,
        &["init", "--range", "HEAD~4..HEAD~2", "--dry-run", "--json"],
    );
    assert!(ranged.status.success(), "{}", stderr(&ranged));
    let value = parse_strict(&stdout(&ranged)).expect("strict JSON");
    let ids: Vec<&str> = list(&value, "tasks")
        .iter()
        .map(|task| text(task, "id"))
        .collect();
    assert_eq!(ids, [format!("mined-{}", h.four)]);
    assert_eq!(text(&list(&value, "tasks")[0], "status"), "dry_run");
}

#[test]
fn what_cannot_be_done_is_said() {
    let h = history();
    let root = h.dir.path();
    for args in [
        &["init", "--limit", "0"][..],
        &["init", "--limit"],
        &["init", "--range", "--all"],
        &["init", "--tasks", "evals"],
        &["init", "--out", "/elsewhere"],
        &["init", "--test"],
    ] {
        let output = tune(root, args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    let help = tune(root, &["init", "--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("--verify"));
    assert!(stdout(&tune(root, &["--help"])).contains("memprove-tune init"));

    // No way to run the tests: no task can be judged.
    let bare = tempfile::tempdir().expect("tempdir");
    git(bare.path(), 0, &["init", "--quiet", "-b", "main"]);
    write(bare.path(), "notes.txt", "x\n");
    commit(bare.path(), 1, "start");
    let output = tune(bare.path(), &["init"]);
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("no test command"),
        "{}",
        stderr(&output)
    );

    // Tests, but no commit that changed a test and code: say what a task
    // is made from, and that is not an error.
    let young = tempfile::tempdir().expect("tempdir");
    git(young.path(), 0, &["init", "--quiet", "-b", "main"]);
    write(young.path(), "Makefile", "test:\n\ttrue\n");
    commit(young.path(), 1, "start");
    append(young.path(), "notes.txt", "x\n");
    commit(young.path(), 2, "notes");
    let output = tune(young.path(), &["init"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let said = stdout(&output);
    assert!(said.contains("No commit"), "{said}");
    assert!(said.contains("a test file"), "{said}");
    assert!(!young.path().join(".tune").exists());
}
