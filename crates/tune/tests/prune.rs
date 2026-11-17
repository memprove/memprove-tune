//! `tune prune` from the outside, with an agent that is a shell script
//! whose behaviour depends on the configuration it finds: units that
//! matter are kept, the others end up in a patch that applies, the
//! verdicts are written down, and the same command twice gives the same.

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
/// it fail the tasks whose prompt says "hard", every trial costs half a
/// dollar, and it notes how many task files it can see.
const AGENT: &str = r#"
grep -qs 'write ok' CLAUDE.md && echo ok > answer.txt
grep -qs 'HARM' CLAUDE.md && grep -qs 'hard' "$TUNE_PROMPT_FILE" && rm -f answer.txt
[ -n "$PROBE" ] && ls evals 2>/dev/null | wc -l | tr -d ' ' >> "$PROBE"
printf '{"cost_microusd": 500000, "input_tokens": 100, "output_tokens": 10, "turns": 2}' > "$TUNE_USAGE_FILE"
exit 0
"#;

/// A repository with `claude` as its CLAUDE.md and `tasks` committed task
/// files; every second task is a hard one.
fn repo(claude: &str, tasks: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "check.sh", "test \"$(cat answer.txt)\" = ok\n");
    write(root, "agent.sh", AGENT);
    write(root, "CLAUDE.md", claude);
    for index in 1..=tasks {
        let kind = if index % 2 == 0 { "hard" } else { "easy" };
        write(
            root,
            &format!("evals/t{index}.json"),
            &format!(
                r#"{{"schema": "tune.task.v1", "prompt": "make the check pass ({kind})",
                "check": {{"argv": ["sh", "check.sh"], "timeout_secs": 20}}}}"#
            ),
        );
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "start"]);
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

fn list<'a>(value: &'a JsonValue, member: &str) -> &'a [JsonValue] {
    value
        .get(member)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no list {member}"))
}

fn is_null(value: &JsonValue, member: &str) -> bool {
    value.get(member) == Some(&JsonValue::Null)
}

/// The advice for `unit` in a prune report or in the ledger.
fn advice<'a>(units: &'a [JsonValue], unit: &str) -> &'a str {
    units
        .iter()
        .find(|entry| text(entry, "unit") == unit)
        .map_or("(not there)", |entry| text(entry, "advice"))
}

fn ledger(root: &Path) -> JsonValue {
    let written = fs::read_to_string(root.join(".tune/verdicts.json")).expect("verdicts.json");
    parse_strict(&written).expect("strict JSON")
}

const THREE_KINDS: &str = "## Useful\nAlways write ok to answer.txt.\n\n## Useless\nBe kind to the reader.\n\n## Harmful\nHARM\n";
const TWO: &str =
    "## Useful\nAlways write ok to answer.txt.\n\n## Useless\nBe kind to the reader.\n";
const AGENT_ARGS: [&str; 2] = ["--agent-cmd", "sh agent.sh"];

/// What `tune prune --json` says of the repository with the three kinds
/// of unit, once every trial is done.
fn assert_three_kinds_report(value: &JsonValue) {
    assert_eq!(text(value, "schema"), "tune.prune.v1");
    assert_eq!(number(value, "trials_run"), 0);
    assert_eq!(number(value, "rounds"), 6);
    assert_eq!(number(value, "pairs_needed"), 29);
    assert_eq!(
        value.get("complete").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(list(value, "held_out").len(), 1);
    let units = list(value, "units");
    assert_eq!(advice(units, "CLAUDE.md#useful"), "keep");
    assert_eq!(advice(units, "CLAUDE.md#harmful"), "removable");
    let pruned = value.get("pruned").expect("pruned");
    let dropped: Vec<&str> = list(pruned, "units")
        .iter()
        .filter_map(JsonValue::as_str)
        .collect();
    assert_eq!(dropped, ["CLAUDE.md#harmful", "CLAUDE.md#useless"]);
    assert_eq!(text(pruned, "patch"), ".tune/prune.patch");
    assert!(is_null(pruned, "reason"));
    assert!(number(pruned, "tokens_saved") > 0);
    let check = pruned.get("validation").expect("validation");
    assert_eq!(
        check.get("held_out").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(number(check, "pairs"), 6);
    assert_eq!(number(check, "pruned_passed"), 6);
    assert!(number(check, "baseline_passed") <= 6);
    assert_eq!(
        text(value.get("search").expect("search"), "schema"),
        "tune.ab.v1"
    );
}

/// After the patch, the file is one unit with another name; its text is
/// what was tested, so its verdict still holds. The others are gone.
fn assert_the_verdict_follows_the_text(root: &Path) {
    let scan = parse_strict(&stdout(&tune(root, &["scan", "--json"]))).expect("strict JSON");
    let verdicts = list(&scan, "verdicts");
    let of = |unit: &str| {
        verdicts
            .iter()
            .find(|entry| text(entry, "unit") == unit)
            .unwrap_or_else(|| panic!("no verdict for {unit}"))
    };
    assert_eq!(text(of("CLAUDE.md#useful"), "status"), "current");
    assert_eq!(text(of("CLAUDE.md#useful"), "unit_now"), "CLAUDE.md");
    assert_eq!(text(of("CLAUDE.md#useless"), "status"), "stale");
    assert!(is_null(of("CLAUDE.md#useless"), "unit_now"));
    let said = stdout(&tune(root, &["scan"]));
    assert!(
        said.lines().any(|line| line.contains("CLAUDE.md#useful")
            && line.contains("KEEP")
            && line.contains("now CLAUDE.md")),
        "{said}"
    );
}

#[test]
fn units_that_help_stay_and_the_rest_goes_into_a_patch_that_applies() {
    let dir = repo(THREE_KINDS, 6);
    let root = dir.path();
    let probe = dir.path().join("probe.txt");
    let mut args = vec!["prune", "--jobs", "4", "--yes", "--pass-env", "PROBE"];
    args.extend(AGENT_ARGS);
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tune"))
            .args(args)
            .current_dir(root)
            .env("PROBE", &probe)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("tune runs")
    };

    // 6 tasks: one is held out, 5 are searched, and 6 rounds of 5 give the
    // 29 pairs a verdict at ten points needs.
    let first = run(&args);
    assert!(first.status.success(), "{}", stderr(&first));
    let report = stdout(&first);
    for expected in [
        "3 units",
        "5 arms x 5 tasks x 6 rounds = 150 trials",
        "KEEP",
        "REMOVABLE",
        "1 held-out task x 6 rounds",
        "Patch: .tune/prune.patch",
        "git apply .tune/prune.patch",
        "Verdicts: .tune/verdicts.json",
    ] {
        assert!(
            report.contains(expected),
            "missing {expected:?} in:\n{report}"
        );
    }
    assert!(!report.to_lowercase().contains("memprove"), "{report}");

    let patch = fs::read(root.join(".tune/prune.patch")).expect("the patch");
    let written = String::from_utf8_lossy(&patch).into_owned();
    assert!(written.contains("-## Useless\n"), "{written}");
    assert!(written.contains("-HARM\n"), "{written}");
    assert!(!written.contains("-## Useful"), "{written}");

    let verdicts = fs::read(root.join(".tune/verdicts.json")).expect("the verdicts");
    let entries = ledger(root);
    assert_eq!(text(&entries, "schema"), "tune.verdicts.v1");
    let entries = list(&entries, "verdicts");
    assert_eq!(entries.len(), 3);
    assert_eq!(advice(entries, "CLAUDE.md#useful"), "keep");
    assert_eq!(advice(entries, "CLAUDE.md#useless"), "removable");
    assert_eq!(advice(entries, "CLAUDE.md#harmful"), "removable");
    assert_eq!(number(&entries[0], "pairs"), 30);
    assert_eq!(text(&entries[0], "agent"), "sh agent.sh");

    // No trial, searched or held out, could see a task file.
    let seen = fs::read_to_string(&probe).expect("probe");
    assert_eq!(seen.lines().count(), 150 + 12);
    assert!(seen.lines().all(|line| line == "0"), "{seen}");

    // The same command again runs nothing and gives the same, byte for byte.
    args.push("--json");
    let second = run(&args);
    assert!(second.status.success(), "{}", stderr(&second));
    assert_three_kinds_report(&parse_strict(&stdout(&second)).expect("strict JSON"));
    assert_eq!(
        fs::read(root.join(".tune/prune.patch")).expect("patch"),
        patch
    );
    assert_eq!(
        fs::read(root.join(".tune/verdicts.json")).expect("verdicts"),
        verdicts
    );

    // The patch applies, and leaves the unit that helps.
    git(root, &["apply", ".tune/prune.patch"]);
    assert_eq!(
        fs::read_to_string(root.join("CLAUDE.md")).expect("CLAUDE.md"),
        "## Useful\nAlways write ok to answer.txt.\n\n"
    );

    assert_the_verdict_follows_the_text(root);
}

#[test]
fn units_that_are_removable_alone_but_not_together_give_no_patch() {
    let dir = repo(
        "## First\nAlways write ok to answer.txt.\n\n## Second\nOnce more: write ok to answer.txt.\n",
        6,
    );
    let root = dir.path();
    // A patch an earlier run left behind does not outlive this one.
    write(root, ".tune/prune.patch", "stale\n");
    let mut args = vec![
        "prune",
        "--holdout",
        "0",
        "--rounds",
        "5",
        "--jobs",
        "4",
        "--yes",
        "--json",
    ];
    args.extend(AGENT_ARGS);
    let output = tune(root, &args);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    let units = list(&value, "units");
    assert_eq!(advice(units, "CLAUDE.md#first"), "removable");
    assert_eq!(advice(units, "CLAUDE.md#second"), "removable");
    let pruned = value.get("pruned").expect("pruned");
    assert!(is_null(pruned, "patch"));
    assert!(
        text(pruned, "reason").contains("together"),
        "{}",
        text(pruned, "reason")
    );
    let check = pruned.get("validation").expect("validation");
    assert_eq!(
        check.get("held_out").and_then(JsonValue::as_bool),
        Some(false)
    );
    assert_eq!(number(check, "pairs"), 30);
    assert_eq!(number(check, "pruned_passed"), 0);
    assert_eq!(number(check, "baseline_passed"), 30);
    assert!(!root.join(".tune/prune.patch").exists());
    // Each verdict is still what the trials showed.
    assert_eq!(
        advice(list(&ledger(root), "verdicts"), "CLAUDE.md#first"),
        "removable"
    );

    let as_text: Vec<&str> = args
        .iter()
        .copied()
        .filter(|arg| *arg != "--json")
        .collect();
    let said = stdout(&tune(root, &as_text));
    assert!(said.contains("No patch"), "{said}");
    assert!(said.contains("passed 0 of 30"), "{said}");
}

#[test]
fn one_removable_unit_without_held_out_tasks_needs_no_second_run() {
    let dir = repo(TWO, 3);
    let root = dir.path();
    let mut args = vec!["prune", "--jobs", "4", "--yes", "--json"];
    args.extend(AGENT_ARGS);
    let output = tune(root, &args);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    // 3 tasks: none is held out, and 10 rounds give the pairs.
    assert_eq!(number(&value, "rounds"), 10);
    assert!(list(&value, "held_out").is_empty());
    assert_eq!(number(&value, "trials_run"), 4 * 3 * 10);
    let search = value.get("search").expect("search");
    let pruned = value.get("pruned").expect("pruned");
    let check = pruned.get("validation").expect("validation");
    assert_eq!(text(check, "experiment"), text(search, "experiment"));
    assert_eq!(number(check, "pairs"), 30);
    assert_eq!(text(pruned, "patch"), ".tune/prune.patch");
    git(root, &["apply", "--check", ".tune/prune.patch"]);
}

#[test]
fn the_budget_stops_it_without_a_patch_and_a_larger_one_continues() {
    let dir = repo(TWO, 2);
    let root = dir.path();
    // 4 arms x 2 tasks x 3 rounds at $0.50 a trial would be $12.
    let mut args = vec!["prune", "--rounds", "3", "--yes", "--json"];
    args.extend(AGENT_ARGS);
    let mut capped = args.clone();
    capped.extend(["--max-cost", "5", "--cost-per-trial", "0.5"]);
    let first = tune(root, &capped);
    assert!(first.status.success(), "{}", stderr(&first));
    let value = parse_strict(&stdout(&first)).expect("strict JSON");
    assert_eq!(text(&value, "stopped"), "budget");
    assert_eq!(
        value.get("complete").and_then(JsonValue::as_bool),
        Some(false)
    );
    assert_eq!(number(&value, "trials_run"), 10);
    assert_eq!(number(&value, "spent_microusd"), 5_000_000);
    let pruned = value.get("pruned").expect("pruned");
    assert!(is_null(pruned, "patch"));
    assert!(text(pruned, "reason").contains("budget"));
    assert!(is_null(&value, "verdicts_file"));
    assert!(!root.join(".tune").join("verdicts.json").exists());
    assert!(!root.join(".tune").join("prune.patch").exists());

    let mut raised = args.clone();
    raised.extend(["--max-cost", "40"]);
    let second = tune(root, &raised);
    assert!(second.status.success(), "{}", stderr(&second));
    let value = parse_strict(&stdout(&second)).expect("strict JSON");
    assert_eq!(number(&value, "trials_run"), 14);
    assert_eq!(
        value.get("complete").and_then(JsonValue::as_bool),
        Some(true)
    );
    // Six pairs show that the useful unit matters and nothing about the
    // other.
    let units = list(&value, "units");
    assert_eq!(advice(units, "CLAUDE.md#useful"), "keep");
    assert_eq!(advice(units, "CLAUDE.md#useless"), "inconclusive");
    let pruned = value.get("pruned").expect("pruned");
    assert!(is_null(pruned, "patch"));
    assert!(text(pruned, "reason").contains("removable"));
    assert_eq!(text(&value, "verdicts_file"), ".tune/verdicts.json");
    assert_eq!(list(&ledger(root), "verdicts").len(), 2);
}

#[test]
fn the_plan_comes_first_and_says_what_the_cap_fits() {
    let dir = repo(TWO, 2);
    let root = dir.path();
    write(root, "AGENTS.md", "Nobody imports this file.\n");
    let mut args = vec!["prune", "--cost-per-trial", "0.5", "--max-cost", "20"];
    args.extend(AGENT_ARGS);
    let mut dry = args.clone();
    dry.push("--dry-run");
    let plan = tune(root, &dry);
    assert!(plan.status.success(), "{}", stderr(&plan));
    let shown = stdout(&plan);
    for expected in [
        "2 units",
        // 29 pairs on 2 tasks: 15 rounds.
        "4 arms x 2 tasks x 15 rounds = 120 trials",
        "CLAUDE.md#useful",
        "not tested",
        "AGENTS.md",
        "$60.00",
        "cap $20.00",
        // $20 pays for 40 trials: the baseline, the control and no unit.
        "fits none of the 2 units",
        // The final check is priced too: 2 arms x 2 tasks x 15 rounds.
        "up to 60 more trials, about $30.00",
        "Nothing was run",
    ] {
        assert!(
            shown.contains(expected),
            "missing {expected:?} in:\n{shown}"
        );
    }
    assert!(!root.join(".tune").exists(), "a dry run wrote something");

    let mut fewer = dry.clone();
    fewer.extend(["--rounds", "3", "--max-cost", "10"]);
    let shown = stdout(&tune(root, &fewer));
    // $10 pays for 20 trials of 6 an arm: three arms, so one unit.
    assert!(shown.contains("--top 1"), "{shown}");
    assert!(shown.contains("6 pairs"), "{shown}");

    let mut top = dry.clone();
    top.extend(["--top", "1"]);
    let shown = stdout(&tune(root, &top));
    assert!(shown.contains("1 unit"), "{shown}");
    assert!(shown.contains("3 arms x 2 tasks"), "{shown}");

    let mut named = dry.clone();
    named.extend(["--unit", "CLAUDE.md#useless", "--unit", "AGENTS.md"]);
    let shown = stdout(&tune(root, &named));
    assert!(
        shown.contains("without CLAUDE.md#useless; without AGENTS.md")
            || shown.contains("without AGENTS.md; without CLAUDE.md#useless"),
        "{shown}"
    );

    // Not a terminal and no --yes: refuse rather than spend.
    let refused = tune(root, &args);
    assert_eq!(refused.status.code(), Some(2), "{}", stdout(&refused));
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
}

#[test]
fn wrong_requests_are_usage_errors() {
    let dir = repo(TWO, 2);
    let cases: [&[&str]; 7] = [
        // A real agent needs a cap.
        &["prune", "--yes"],
        &[
            "prune",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
            "--holdout",
            "2",
        ],
        &[
            "prune",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
            "--unit",
            "CLAUDE.md#nope",
        ],
        &["prune", "--yes", "--agent-cmd", "sh agent.sh", "--top", "0"],
        &[
            "prune",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
            "--drop",
            "CLAUDE.md#useless",
        ],
        &[
            "prune",
            "--yes",
            "--agent-cmd",
            "sh agent.sh",
            "--tasks",
            "nowhere",
        ],
        &["prune", "--rounds"],
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
    let help = tune(dir.path(), &["prune", "--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("--holdout"));
    assert!(stdout(&tune(dir.path(), &["--help"])).contains("tune prune"));

    // Nothing the agent reads: nothing to do, and that is not an error.
    let empty = repo("", 1);
    fs::remove_file(empty.path().join("CLAUDE.md")).expect("remove");
    let output = tune(
        empty.path(),
        &["prune", "--yes", "--agent-cmd", "sh agent.sh"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Nothing to prune"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn scan_shows_each_verdict_and_whether_it_still_holds() {
    let dir = repo(TWO, 1);
    let root = dir.path();
    let mut args = vec!["prune", "--rounds", "1", "--yes"];
    args.extend(AGENT_ARGS);
    let output = tune(root, &args);
    assert!(output.status.success(), "{}", stderr(&output));

    let scan = stdout(&tune(root, &["scan"]));
    assert!(scan.contains("Verdicts (.tune/verdicts.json): 2"), "{scan}");
    assert!(
        scan.lines().any(|line| line.contains("CLAUDE.md#useless")
            && line.contains("NOT ENOUGH EVIDENCE")
            && line.contains("current")),
        "{scan}"
    );

    // The unit is reworded: its verdict no longer holds; the other's does,
    // with a note that its neighbour changed.
    write(
        root,
        "CLAUDE.md",
        "## Useful\nAlways write ok to answer.txt.\n\n## Useless\nBe very kind to the reader.\n",
    );
    let value = parse_strict(&stdout(&tune(root, &["scan", "--json"]))).expect("strict JSON");
    let verdicts = list(&value, "verdicts");
    let of = |unit: &str| {
        verdicts
            .iter()
            .find(|entry| text(entry, "unit") == unit)
            .unwrap_or_else(|| panic!("no verdict for {unit}"))
    };
    let useless = of("CLAUDE.md#useless");
    assert_eq!(text(useless, "status"), "stale");
    assert_eq!(
        list(useless, "reasons")[0].as_str(),
        Some("the unit changed")
    );
    let useful = of("CLAUDE.md#useful");
    assert_eq!(text(useful, "status"), "current");
    assert_eq!(
        useful.get("rest_changed").and_then(JsonValue::as_bool),
        Some(true)
    );
    assert_eq!(number(useful, "pairs"), 1);

    // A task is reworded: every verdict is stale.
    write(
        root,
        "evals/t1.json",
        r#"{"schema": "tune.task.v1", "prompt": "another task",
            "check": {"argv": ["sh", "check.sh"], "timeout_secs": 20}}"#,
    );
    let scan = stdout(&tune(root, &["scan"]));
    assert!(
        scan.lines().any(|line| line.contains("CLAUDE.md#useful")
            && line.contains("stale: the task files changed")),
        "{scan}"
    );
}
