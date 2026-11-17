//! `tune apply` inside a trial of the engine: the agent sees the arm's
//! configuration, the trial's diff does not hold it, and nothing in the
//! trial's repository still holds the configuration it replaced.

use std::{fs, path::Path, time::Duration};

use tune_engine::{
    AgentKind, AgentSpec, CommandSpec, Outcome, agent_for,
    git::git,
    runner::{self, RunOptions},
    task,
};

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(path, text).expect("write");
}

/// What the scripted agent does: record what it can see, then solve the
/// task only when the arm's instruction is there.
const AGENT: &str = r#"
seen="$TUNE_TEST_SEEN"
cat CLAUDE.md > "$seen/claude" 2>/dev/null
ls .claude/rules > "$seen/rules" 2>/dev/null
ls held > "$seen/held" 2>/dev/null
git log --all --format=%s | wc -l | tr -d ' ' > "$seen/commits"
git status --porcelain > "$seen/status"
for object in $(git cat-file --batch-all-objects --batch-check | cut -d' ' -f1); do
  GIT_NO_REPLACE_OBJECTS=1 git cat-file -p "$object" 2>/dev/null
done > "$seen/objects"
git reflog > "$seen/reflog" 2>/dev/null
grep -q 'write ok' CLAUDE.md && echo ok > answer.txt
exit 0
"#;

/// A repository with old configuration, two files no trial may see, and
/// one task.
fn repo_with_secrets() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for args in [
        &["init", "--quiet", "-b", "main"][..],
        &["config", "user.email", "t@example.invalid"],
        &["config", "user.name", "t"],
    ] {
        git(root, args).expect("git");
    }
    write(root, "check.sh", "test \"$(cat answer.txt)\" = ok\n");
    write(root, "CLAUDE.md", "SECRET-OLD instructions\n");
    write(root, ".claude/rules/old.md", "SECRET-OLD rule\n");
    write(root, "agent.sh", AGENT);
    // Files of the repository that no trial may see: another task and its
    // hidden test.
    write(root, "held/secret.json", "SECRET-HELD task\n");
    write(root, "held/secret.patch", "SECRET-HELD test\n");
    git(root, &["add", "."]).expect("add");
    git(root, &["commit", "--quiet", "-m", "start"]).expect("commit");
    write(
        root,
        "evals/a.json",
        r#"{"schema": "tune.task.v1", "prompt": "make the check pass",
            "check": {"argv": ["sh", "check.sh"], "timeout_secs": 20}}"#,
    );
    dir
}

#[test]
fn the_agent_sees_the_arm_and_nothing_of_what_it_replaced() {
    let dir = repo_with_secrets();
    let root = dir.path();

    let arm_parent = tempfile::tempdir().expect("tempdir");
    let arm = arm_parent.path().join("files");
    write(&arm, "CLAUDE.md", "Always write ok to answer.txt.\n");
    write(&arm, ".claude/rules/new.md", "A new rule.\n");
    let seen = tempfile::tempdir().expect("tempdir");
    let hide = arm_parent.path().join("hide.txt");
    fs::write(
        &hide,
        "held/secret.json\nheld/secret.patch\nheld/not-there\n",
    )
    .expect("hide list");

    let mut tasks = task::load_suite(root, &[root.join("evals")]).expect("tasks");
    for (task, _) in &mut tasks {
        task.setup.insert(
            0,
            CommandSpec {
                argv: vec![
                    env!("CARGO_BIN_EXE_tune").to_owned(),
                    "apply".to_owned(),
                    "--files".to_owned(),
                    arm.to_string_lossy().into_owned(),
                    "--hide".to_owned(),
                    hide.to_string_lossy().into_owned(),
                ],
                timeout: Duration::from_secs(60),
            },
        );
    }
    let spec = AgentSpec {
        kind: AgentKind::Command,
        model: None,
        command: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!(
                "TUNE_TEST_SEEN='{}' sh agent.sh",
                seen.path().to_string_lossy()
            ),
        ],
        extra_args: Vec::new(),
        pass_env: Vec::new(),
    };
    let out = tempfile::tempdir().expect("tempdir");
    let options = RunOptions {
        repo_root: root.to_path_buf(),
        agent: spec.clone(),
        trials: 1,
        jobs: 1,
        out_root: out.path().to_path_buf(),
        keep_worktrees: false,
        progress: false,
    };
    let agent = agent_for(&spec).expect("agent");
    let (record, run_dir) = runner::run_suite(&options, &tasks, agent.as_ref()).expect("run");
    let result = &record.results[0];
    assert_eq!(result.outcome, Outcome::Passed, "{result:?}");

    let read = |name: &str| fs::read_to_string(seen.path().join(name)).unwrap_or_default();
    assert_eq!(read("claude"), "Always write ok to answer.txt.\n");
    assert_eq!(read("rules"), "new.md\n");
    assert_eq!(read("commits"), "1\n");
    assert_eq!(read("status"), "", "the working copy starts clean");
    assert_eq!(read("reflog"), "");
    assert_eq!(read("held"), "", "a hidden file is in the working copy");
    assert!(
        !read("objects").contains("SECRET-HELD"),
        "a hidden file is still in the repository's objects"
    );
    assert!(read("objects").contains("A new rule."));
    assert!(
        !read("objects").contains("SECRET-OLD"),
        "{}",
        read("objects")
    );

    // The trial's diff is the agent's work alone.
    let logs = run_dir.join(&result.logs);
    let diff = fs::read_to_string(logs.join("agent.diff")).expect("agent.diff");
    assert!(diff.contains("answer.txt"), "{diff}");
    assert!(
        !diff.contains("CLAUDE.md") && !diff.contains("rules"),
        "{diff}"
    );
    assert_eq!(result.diff.files, 1);
}

#[test]
fn apply_refuses_to_run_anywhere_but_a_trial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "--quiet", "-b", "main"]).expect("init");
    write(root, "CLAUDE.md", "mine\n");
    let arm = tempfile::tempdir().expect("tempdir");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tune"))
        .args(["apply", "--files"])
        .arg(arm.path())
        .env("TUNE_TASK", "a")
        .current_dir(root)
        .output()
        .expect("tune runs");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(root.join("CLAUDE.md")).expect("still there"),
        "mine\n"
    );
}
