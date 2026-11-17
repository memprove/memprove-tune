//! The `tune` binary from the outside: `scan`, help and errors.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use tune_engine::strict_json::parse_strict;

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

fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    for (name, text) in files {
        write(dir.path(), name, text);
    }
    git(dir.path(), &["add", "-A"]);
    git(
        dir.path(),
        &["commit", "-q", "--allow-empty", "-m", "start"],
    );
    dir
}

fn tune(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tune"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("tune runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8")
}

const CLAUDE: &str = "## Setup\nRun `scripts/setup.sh` first.\n\n## Style\nKeep functions small.\n";
const RULE: &str = "---\npaths: src/api/**\n---\n# API\nReturn errors as JSON.\n";
const SKILL: &str = "---\nname: deploy\ndescription: Deploys the service to staging.\n---\nRun the deploy script.\n";

fn sample() -> tempfile::TempDir {
    repo(&[
        ("CLAUDE.md", CLAUDE),
        (".claude/rules/api.md", RULE),
        (".claude/skills/deploy/SKILL.md", SKILL),
        ("scripts/build.sh", "#!/bin/sh\n"),
        ("AGENTS.md", "Use tabs.\n"),
    ])
}

#[test]
fn scan_lists_units_with_their_cost_and_the_findings() {
    let dir = sample();
    let output = tune(dir.path(), &["scan"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for expected in [
        "4 configuration files, 5 units",
        "CLAUDE.md (5 lines, loaded in every session)",
        "CLAUDE.md#setup",
        "CLAUDE.md#style",
        ".claude/rules/api.md (5 lines, loaded when a file matching src/api/** is read)",
        ".claude/skills/deploy/SKILL.md (5 lines, loaded when the skill is invoked",
        "AGENTS.md (1 line, not read",
        "Findings: 3",
        "CLAUDE.md:2 [missing-path] names `scripts/setup.sh`, which does not exist",
        "CLAUDE.md [diverged-files]",
        "AGENTS.md [not-read]",
        "Nothing was run and no model was called.",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
}

#[test]
fn scan_reads_the_working_tree_from_any_directory_of_the_repository() {
    let dir = sample();
    // Not committed yet: the scan is about what an agent would read now.
    write(
        dir.path(),
        "CLAUDE.md",
        "## Only\nOne unit now, and `scripts/build.sh` exists.\n",
    );
    fs::remove_file(dir.path().join("AGENTS.md")).expect("remove");
    let output = tune(&dir.path().join("scripts"), &["scan"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("3 configuration files, 3 units"), "{text}");
    assert!(text.contains("Findings: 0"), "{text}");
}

#[test]
fn scan_json_is_one_document_with_units_and_findings() {
    let dir = sample();
    let output = tune(dir.path(), &["scan", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value = parse_strict(&stdout(&output)).expect("strict JSON");
    assert_eq!(
        value.get("schema").and_then(|v| v.as_str()),
        Some("tune.scan.v1")
    );
    let units = value
        .get("units")
        .and_then(|v| v.as_array())
        .expect("units");
    let ids: Vec<&str> = units
        .iter()
        .filter_map(|unit| unit.get("id").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(
        ids,
        [
            "CLAUDE.md#setup",
            "CLAUDE.md#style",
            "AGENTS.md",
            ".claude/rules/api.md",
            ".claude/skills/deploy"
        ]
    );
    let first = &units[0];
    assert_eq!(first.get("kind").and_then(|v| v.as_str()), Some("section"));
    assert_eq!(first.get("load").and_then(|v| v.as_str()), Some("always"));
    assert_eq!(
        first
            .get("first_line")
            .and_then(tune_engine::strict_json::JsonValue::as_i64),
        Some(1)
    );
    assert_eq!(
        first
            .get("last_line")
            .and_then(tune_engine::strict_json::JsonValue::as_i64),
        Some(3)
    );
    assert!(
        first
            .get("tokens")
            .and_then(tune_engine::strict_json::JsonValue::as_i64)
            .expect("tokens")
            > 0
    );
    let rule = &units[3];
    assert_eq!(rule.get("load").and_then(|v| v.as_str()), Some("on_demand"));
    assert_eq!(
        rule.get("when").and_then(|v| v.as_str()),
        Some("when a file matching src/api/** is read")
    );
    let findings = value
        .get("findings")
        .and_then(|v| v.as_array())
        .expect("findings");
    assert_eq!(findings.len(), 3);
    let path = findings
        .iter()
        .find(|finding| finding.get("code").and_then(|v| v.as_str()) == Some("missing-path"))
        .expect("missing-path");
    assert_eq!(path.get("file").and_then(|v| v.as_str()), Some("CLAUDE.md"));
    assert_eq!(
        path.get("line")
            .and_then(tune_engine::strict_json::JsonValue::as_i64),
        Some(2)
    );
    assert_eq!(
        path.get("unit").and_then(|v| v.as_str()),
        Some("CLAUDE.md#setup")
    );
    let totals = value.get("totals").expect("totals");
    assert_eq!(
        totals
            .get("units")
            .and_then(tune_engine::strict_json::JsonValue::as_i64),
        Some(5)
    );
}

#[test]
fn scan_says_so_when_there_is_no_configuration() {
    let dir = repo(&[("README.md", "hello\n")]);
    let output = tune(dir.path(), &["scan"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("No agent configuration found"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn scan_outside_a_repository_fails_with_a_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = tune(dir.path(), &["scan"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).starts_with("tune: "), "{}", stderr(&output));
}

#[test]
fn help_version_and_wrong_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let version = tune(dir.path(), &["--version"]);
    assert_eq!(
        stdout(&version),
        format!("tune {}\n", env!("CARGO_PKG_VERSION"))
    );
    let help = tune(dir.path(), &["--help"]);
    assert!(help.status.success());
    for command in ["tune scan", "tune ab"] {
        assert!(stdout(&help).contains(command), "{}", stdout(&help));
    }
    let none = tune(dir.path(), &[]);
    assert_eq!(none.status.code(), Some(2));
    let unknown = tune(dir.path(), &["frobnicate"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        stderr(&unknown).contains("frobnicate"),
        "{}",
        stderr(&unknown)
    );
    let flag = tune(dir.path(), &["scan", "--nope"]);
    assert_eq!(flag.status.code(), Some(2));
}

#[test]
fn nothing_it_prints_names_the_project_the_engine_came_from() {
    let dir = sample();
    for args in [
        &["scan"][..],
        &["scan", "--json"],
        &["--help"],
        &["ab", "--help"],
    ] {
        let output = tune(dir.path(), args);
        let all = format!("{}{}", stdout(&output), stderr(&output)).to_lowercase();
        assert!(!all.contains("memprove"), "{args:?}: {all}");
    }
}
