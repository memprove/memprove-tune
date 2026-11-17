//! `tune ab` from the outside, with an agent that is a shell script whose
//! behaviour depends on the configuration it finds: a unit that matters is
//! kept, one that does not is removable, the same configuration twice
//! shows no difference, a second run continues the first, and a budget
//! stops it.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tune_engine::strict_json::{JsonValue, parse_strict};

fn git(dir: &Path, args: &[&str]) {
    let done = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(done.status.success(), "git {args:?}: {done:?}");
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

/// The agent: it writes the answer only when told to, a `HARM` line makes
/// it undo that, and every trial costs half a dollar.
const AGENT: &str = r#"
grep -qs 'write ok' CLAUDE.md && echo ok > answer.txt
grep -qs 'HARM' CLAUDE.md && rm -f answer.txt
printf '{"cost_microusd": 500000, "input_tokens": %s, "output_tokens": 10, "turns": 2}' \
  "$(cat CLAUDE.md 2>/dev/null | wc -c | tr -d ' ')" > "$TUNE_USAGE_FILE"
exit 0
"#;

fn repo(claude: &str, tasks: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "check.sh", "test \"$(cat answer.txt)\" = ok\n");
    write(root, "agent.sh", AGENT);
    write(root, "CLAUDE.md", claude);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "start"]);
    for index in 1..=tasks {
        write(
            root,
            &format!("evals/t{index}.json"),
            r#"{"schema": "tune.task.v1", "prompt": "make the check pass",
                "check": {"argv": ["sh", "check.sh"], "timeout_secs": 20}}"#,
        );
    }
    dir
}

fn tune(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tune"))
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

fn comparison<'a>(report: &'a JsonValue, label: &str) -> &'a JsonValue {
    report
        .get("comparisons")
        .and_then(JsonValue::as_array)
        .expect("comparisons")
        .iter()
        .find(|comparison| text(comparison, "label") == label)
        .unwrap_or_else(|| panic!("no comparison {label}"))
}

const CLAUDE: &str =
    "## Useful\nAlways write ok to answer.txt.\n\n## Useless\nBe kind to the reader.\n";
const AGENT_ARGS: [&str; 2] = ["--agent-cmd", "sh agent.sh"];

#[test]
fn a_unit_that_matters_is_kept_and_one_that_does_not_is_removable() {
    let dir = repo(CLAUDE, 6);
    let mut args = vec![
        "ab",
        "--drop",
        "CLAUDE.md#useful",
        "--drop",
        "CLAUDE.md#useless",
        "--rounds",
        "5",
        "--jobs",
        "4",
        "--yes",
    ];
    args.extend(AGENT_ARGS);
    let first = tune(dir.path(), &args);
    assert!(first.status.success(), "{}", stderr(&first));
    let report = stdout(&first);
    for expected in [
        "4 arms x 6 tasks x 5 rounds = 120 trials",
        "baseline",
        "control",
        "without CLAUDE.md#useful",
        "KEEP",
        "REMOVABLE",
        "30 pairs",
    ] {
        assert!(
            report.contains(expected),
            "missing {expected:?} in:\n{report}"
        );
    }
    assert!(!report.to_lowercase().contains("memprove"), "{report}");

    // The same command again runs nothing new and reports the same.
    args.push("--json");
    let second = tune(dir.path(), &args);
    assert!(second.status.success(), "{}", stderr(&second));
    let value = parse_strict(&stdout(&second)).expect("strict JSON");
    assert_eq!(text(&value, "schema"), "tune.ab.v1");
    assert_eq!(number(&value, "trials_run"), 0);
    assert_eq!(number(&value, "trials_done"), 120);
    assert_eq!(
        value.get("complete").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(number(&value, "spent_microusd"), 60_000_000);

    let useful = comparison(&value, "without CLAUDE.md#useful");
    assert_eq!(text(useful, "verdict"), "worse");
    assert_eq!(text(useful, "advice"), "keep");
    assert_eq!(number(useful, "pairs"), 30);
    assert_eq!(number(useful, "only_baseline"), 30);
    let useless = comparison(&value, "without CLAUDE.md#useless");
    assert_eq!(text(useless, "verdict"), "no_worse");
    assert_eq!(text(useless, "advice"), "removable");
    assert_eq!(number(useless, "both_pass"), 30);
    assert!(number(useless, "config_tokens_saved") > 0);

    // The same configuration twice: no pair differs.
    let noise = value.get("noise").expect("noise");
    assert_eq!(number(noise, "pairs"), 30);
    assert_eq!(number(noise, "only_baseline"), 0);
    assert_eq!(number(noise, "only_variant"), 0);

    // More rounds continue the same experiment.
    let position = args.iter().position(|arg| *arg == "5").expect("rounds");
    args[position] = "6";
    let third = tune(dir.path(), &args);
    assert!(third.status.success(), "{}", stderr(&third));
    let value = parse_strict(&stdout(&third)).expect("strict JSON");
    assert_eq!(number(&value, "trials_run"), 24);
    assert_eq!(number(&value, "trials_done"), 144);
}

#[test]
fn dropping_a_unit_that_hurts_is_reported_as_better() {
    let dir = repo(
        "## Harmful\nHARM\n\n## Useful\nAlways write ok to answer.txt.\n",
        6,
    );
    let mut args = vec![
        "ab",
        "--drop",
        "CLAUDE.md#harmful",
        "--rounds",
        "1",
        "--yes",
        "--json",
        "--no-control",
    ];
    args.extend(AGENT_ARGS);
    let output = tune(dir.path(), &args);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    let harmful = comparison(&value, "without CLAUDE.md#harmful");
    assert_eq!(text(harmful, "verdict"), "better");
    assert_eq!(text(harmful, "advice"), "removable");
    assert_eq!(number(harmful, "only_variant"), 6);
    assert!(
        value
            .get("noise")
            .is_some_and(|noise| *noise == JsonValue::Null)
    );
}

#[test]
fn few_pairs_are_not_enough_evidence_and_a_named_variant_is_compared() {
    let dir = repo(CLAUDE, 2);
    write(
        dir.path(),
        "better.md",
        "## Useful\nAlways write ok to answer.txt.\n",
    );
    let mut args = vec![
        "ab",
        "--drop",
        "CLAUDE.md#useless",
        "--variant",
        "short:replace=CLAUDE.md=better.md",
        "--bare",
        "--rounds",
        "1",
        "--yes",
        "--json",
    ];
    args.extend(AGENT_ARGS);
    let output = tune(dir.path(), &args);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    let useless = comparison(&value, "without CLAUDE.md#useless");
    assert_eq!(text(useless, "verdict"), "inconclusive");
    assert_eq!(text(useless, "advice"), "inconclusive");
    let short = comparison(&value, "short");
    assert_eq!(text(short, "verdict"), "inconclusive");
    assert!(
        short
            .get("advice")
            .is_some_and(|advice| *advice == JsonValue::Null)
    );
    let bare = comparison(&value, "without any configuration");
    assert_eq!(number(bare, "only_baseline"), 2);
}

#[test]
fn the_budget_stops_the_run_and_a_larger_one_continues_it() {
    let dir = repo(CLAUDE, 2);
    // 3 arms x 2 tasks x 3 rounds at $0.50 a trial would be $9.
    let mut args = vec![
        "ab",
        "--drop",
        "CLAUDE.md#useless",
        "--rounds",
        "3",
        "--yes",
        "--json",
    ];
    args.extend(AGENT_ARGS);
    let mut capped = args.clone();
    capped.extend(["--max-cost", "4", "--cost-per-trial", "0.5"]);
    let first = tune(dir.path(), &capped);
    assert!(first.status.success(), "{}", stderr(&first));
    let value = parse_strict(&stdout(&first)).expect("strict JSON");
    assert_eq!(
        value.get("complete").and_then(JsonValue::as_bool),
        Some(false)
    );
    assert_eq!(text(&value, "stopped"), "budget");
    // Four arm-rounds of two trials fit under $4; the fifth would not.
    assert_eq!(number(&value, "trials_run"), 8);
    assert_eq!(number(&value, "spent_microusd"), 4_000_000);
    assert!(stderr(&first).contains("budget"), "{}", stderr(&first));

    let mut raised = args.clone();
    raised.extend(["--max-cost", "20"]);
    let second = tune(dir.path(), &raised);
    assert!(second.status.success(), "{}", stderr(&second));
    let value = parse_strict(&stdout(&second)).expect("strict JSON");
    assert_eq!(
        value.get("complete").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(number(&value, "trials_run"), 10);
    assert_eq!(number(&value, "trials_done"), 18);
}

#[test]
fn the_plan_is_shown_first_and_nothing_runs_without_a_yes() {
    let dir = repo(CLAUDE, 2);
    let mut args = vec![
        "ab",
        "--drop",
        "CLAUDE.md#useless",
        "--rounds",
        "2",
        "--cost-per-trial",
        "0.25",
        "--max-cost",
        "5",
    ];
    args.extend(AGENT_ARGS);
    let mut dry = args.clone();
    dry.push("--dry-run");
    let plan = tune(dir.path(), &dry);
    assert!(plan.status.success(), "{}", stderr(&plan));
    let shown = stdout(&plan);
    for expected in [
        "3 arms x 2 tasks x 2 rounds = 12 trials",
        "$3.00",
        "cap $5.00",
        "Nothing was run",
    ] {
        assert!(
            shown.contains(expected),
            "missing {expected:?} in:\n{shown}"
        );
    }
    // Not a terminal and no --yes: refuse rather than spend.
    let refused = tune(dir.path(), &args);
    assert_eq!(refused.status.code(), Some(2), "{}", stdout(&refused));
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    let common =
        tune_engine::git::git(dir.path(), &["rev-parse", "--git-common-dir"]).expect("git");
    let runs = dir.path().join(common.trim()).join("tune/experiments");
    let ran = fs::read_dir(&runs).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.path().join("runs").exists())
    });
    assert!(!ran, "trials ran without a yes");
}

#[test]
fn wrong_requests_are_usage_errors() {
    let dir = repo(CLAUDE, 1);
    let cases: [&[&str]; 7] = [
        &["ab", "--yes", "--agent-cmd", "sh agent.sh"],
        &[
            "ab",
            "--drop",
            "CLAUDE.md#nope",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
        ],
        &["ab", "--drop", "CLAUDE.md#useless", "--yes"],
        &[
            "ab",
            "--drop",
            "CLAUDE.md#useless",
            "--yes",
            "--agent",
            "codex",
            "--max-cost",
            "1",
        ],
        &[
            "ab",
            "--drop",
            "CLAUDE.md#useless",
            "--rounds",
            "0",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
        ],
        &[
            "ab",
            "--drop",
            "CLAUDE.md#useless",
            "--tasks",
            "nowhere",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
        ],
        &["ab", "--drop"],
    ];
    for args in cases {
        let output = tune(dir.path(), args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    let help = tune(dir.path(), &["ab", "--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("--max-cost"));
}
