//! A suite of eight tasks run with an agent that is a shell
//! script, covering every outcome, the agent's time limit, hidden tests,
//! `run.json`, the logs, and the worktrees' removal.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tune_engine::{
    Agent, AgentEnd, AgentKind, AgentOutcome, AgentRequest, AgentSpec, Outcome, Usage,
    git::git,
    process::{self, Exit, Invocation},
    runner::{self, RunOptions},
    task,
};

/// Runs the task's prompt as a shell script in the worktree; the prompt
/// `cannot start` stands for an agent that cannot be started.
struct ScriptAgent;

impl Agent for ScriptAgent {
    fn version(&self) -> Option<String> {
        Some("script 1".to_owned())
    }

    fn run(&self, request: &AgentRequest<'_>) -> AgentOutcome {
        if request.prompt == "cannot start" {
            return AgentOutcome {
                end: AgentEnd::FailedToStart("no such agent".to_owned()),
                duration_ms: 0,
                usage: Usage::default(),
            };
        }
        let argv = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            request.prompt.to_owned(),
        ];
        let finished = process::run(&Invocation {
            argv: &argv,
            cwd: request.workdir,
            env: &[],
            env_remove: &[],
            clear_env: false,
            stdout: request.log_dir.join("agent.out"),
            stderr: request.log_dir.join("agent.err"),
            timeout: request.timeout,
        })
        .expect("sh starts");
        AgentOutcome {
            end: match finished.exit {
                Exit::Code(code) => AgentEnd::Exited(code),
                Exit::Signal(signal) => AgentEnd::Signaled(signal),
                Exit::TimedOut => AgentEnd::TimedOut,
            },
            duration_ms: u64::try_from(finished.duration.as_millis()).unwrap_or(u64::MAX),
            usage: Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cost_microusd: Some(1_000),
                ..Usage::default()
            },
        }
    }
}

fn write(root: &Path, name: &str, text: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(path, text).expect("write");
}

fn task_file(root: &Path, id: &str, members: &str) {
    write(
        root,
        &format!("evals/{id}.json"),
        &format!(r#"{{"schema": "tune.task.v1", {members}}}"#),
    );
}

const CHECK: &str = r#""check": {"argv": ["sh", "check.sh"], "timeout_secs": 20}"#;
const NEW_FILE_PATCH: &str = "diff --git a/answer.txt b/answer.txt\nnew file mode 100644\n--- /dev/null\n+++ b/answer.txt\n@@ -0,0 +1 @@\n+ok\n";
const STALE_PATCH: &str = "diff --git a/README b/README\n--- a/README\n+++ b/README\n@@ -1 +1 @@\n-not what the base holds\n+changed\n";
const HIDDEN_PATCH: &str = "diff --git a/hidden.sh b/hidden.sh\nnew file mode 100644\n--- /dev/null\n+++ b/hidden.sh\n@@ -0,0 +1 @@\n+test \"$(cat answer.txt)\" = ok && test ! -e leaked\n";

fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for args in [
        &["init", "--quiet", "-b", "main"][..],
        &["config", "user.email", "eval@example.invalid"],
        &["config", "user.name", "eval"],
    ] {
        git(root, args).expect("git");
    }
    write(root, "check.sh", "test \"$(cat answer.txt)\" = ok\n");
    write(root, "README", "a repository under evaluation\n");
    git(root, &["add", "."]).expect("add");
    git(root, &["commit", "--quiet", "-m", "start"]).expect("commit");
    task_file(
        root,
        "a-passes",
        &format!(r#""prompt": "echo ok > answer.txt", {CHECK}"#),
    );
    task_file(
        root,
        "b-fails",
        &format!(r#""prompt": "echo no > answer.txt", {CHECK}"#),
    );
    task_file(
        root,
        "c-check-timeout",
        r#""prompt": "true", "check": {"argv": ["sleep", "30"], "timeout_secs": 1}"#,
    );
    task_file(
        root,
        "d-setup-fails",
        &format!(
            r#""prompt": "echo ok > answer.txt", "setup": [{{"argv": ["sh", "-c", "exit 4"]}}], {CHECK}"#
        ),
    );
    write(root, "evals/e.check.patch", STALE_PATCH);
    task_file(
        root,
        "e-patch-fails",
        &format!(
            r#""prompt": "echo ok > answer.txt", "check_patch": "evals/e.check.patch", {CHECK}"#
        ),
    );
    task_file(
        root,
        "f-cannot-start",
        &format!(r#""prompt": "cannot start", {CHECK}"#),
    );
    write(root, "evals/g.check.patch", HIDDEN_PATCH);
    task_file(
        root,
        "g-hidden-test",
        r#""prompt": "test ! -e hidden.sh || touch leaked; echo ok > answer.txt", "check_patch": "evals/g.check.patch", "check": {"argv": ["sh", "hidden.sh"]}"#,
    );
    task_file(
        root,
        "h-agent-timeout",
        &format!(r#""prompt": "echo ok > answer.txt; sleep 30", "agent_timeout_secs": 1, {CHECK}"#),
    );
    // The check patch's file replaces the agent's version of it.
    write(root, "evals/i.check.patch", NEW_FILE_PATCH);
    task_file(
        root,
        "i-patch-replaces",
        &format!(
            r#""prompt": "echo mine > answer.txt", "check_patch": "evals/i.check.patch", {CHECK}"#
        ),
    );
    dir
}

fn options(root: &Path, out: &Path, trials: u32, jobs: u32) -> RunOptions {
    RunOptions {
        repo_root: root.to_path_buf(),
        agent: AgentSpec {
            kind: AgentKind::Command,
            model: None,
            command: vec!["sh".to_owned()],
            extra_args: Vec::new(),
            pass_env: Vec::new(),
        },
        trials,
        jobs,
        out_root: out.to_path_buf(),
        keep_worktrees: false,
        progress: false,
    }
}

/// The logs are where the record says, `run.json` reads back, `latest`
/// finds it, and every trial's worktree is gone.
fn files_and_cleanup(root: &Path, out: &Path, run: &tune_engine::RunRecord, run_dir: &Path) {
    let passed = run
        .results
        .iter()
        .find(|result| result.task == "a-passes")
        .expect("result");
    // The logs are where the record says, and the agent's diff is a patch.
    let logs = run_dir.join(&passed.logs);
    for file in [
        "agent.out",
        "agent.err",
        "agent.diff",
        "check.out",
        "check.err",
    ] {
        assert!(logs.join(file).is_file(), "{file}");
    }
    assert!(
        fs::read_to_string(logs.join("agent.diff"))
            .expect("diff")
            .contains("+ok")
    );

    // run.json reads back as the record, and `latest` finds it.
    assert_eq!(&runner::read_run(run_dir).expect("reads"), run);
    assert_eq!(runner::resolve_run_dir(out, None).expect("latest"), run_dir);
    assert_eq!(run.agent_version.as_deref(), Some("script 1"));
    let summary = run.summary();
    assert_eq!((summary.passed, summary.trials, summary.errors), (4, 9, 1));
    assert_eq!(summary.cost_microusd, Some(7_000));

    // Every trial's worktree is gone, and the repository is as it was.
    let worktrees = git(root, &["worktree", "list", "--porcelain"]).expect("list");
    assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
    assert_eq!(
        git(root, &["status", "--porcelain", "--", "check.sh", "README"]).expect("status"),
        ""
    );
}

#[test]
fn every_outcome_is_judged_by_the_check_alone() {
    let repo = repository();
    let root = repo.path();
    let out = tempfile::tempdir().expect("out");
    let suite = task::load_suite(root, &[PathBuf::from("evals")]).expect("suite");
    assert_eq!(suite.len(), 9);
    let options = options(root, out.path(), 1, 4);
    let (run, run_dir) = runner::run_suite(&options, &suite, &ScriptAgent).expect("runs");

    let outcomes: Vec<(&str, Outcome)> = run
        .results
        .iter()
        .map(|result| (result.task.as_str(), result.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("a-passes", Outcome::Passed),
            ("b-fails", Outcome::Failed),
            ("c-check-timeout", Outcome::CheckTimeout),
            ("d-setup-fails", Outcome::SetupFailed),
            ("e-patch-fails", Outcome::PatchFailed),
            ("f-cannot-start", Outcome::Error),
            ("g-hidden-test", Outcome::Passed),
            ("h-agent-timeout", Outcome::Passed),
            ("i-patch-replaces", Outcome::Passed),
        ]
    );
    let by_task = |id: &str| {
        run.results
            .iter()
            .find(|result| result.task == id)
            .expect("result")
    };
    assert_eq!(by_task("b-fails").check_exit, Some(1));
    assert_eq!(
        by_task("b-fails").reason.as_deref(),
        Some("the check exited with code 1")
    );
    assert_eq!(
        by_task("c-check-timeout").reason.as_deref(),
        Some("the check ran past its limit of 1 s")
    );
    assert_eq!(
        by_task("d-setup-fails").reason.as_deref(),
        Some("setup command 1 (sh -c exit 4) exited with code 4")
    );
    assert!(by_task("d-setup-fails").agent.is_none());
    assert!(
        by_task("e-patch-fails").reason.as_deref().is_some_and(
            |reason| reason.starts_with("the check patch evals/e.check.patch does not apply")
        )
    );
    let replaced = by_task("i-patch-replaces");
    assert!(
        fs::read_to_string(run_dir.join(&replaced.logs).join("agent.diff"))
            .expect("diff")
            .contains("+mine")
    );
    assert_eq!(
        by_task("f-cannot-start").reason.as_deref(),
        Some("the agent could not be started: no such agent")
    );
    let timed_out = by_task("h-agent-timeout");
    assert_eq!(
        timed_out.agent.as_ref().map(|agent| &agent.end),
        Some(&AgentEnd::TimedOut)
    );
    let passed = by_task("a-passes");
    assert_eq!((passed.diff.files, passed.diff.insertions), (1, 1));
    assert_eq!(passed.base, run.repository.head);

    files_and_cleanup(root, out.path(), &run, &run_dir);
}

#[test]
fn trials_repeat_each_task_in_order() {
    let repo = repository();
    let root = repo.path();
    let out = tempfile::tempdir().expect("out");
    let suite = task::load_suite(
        root,
        &[
            PathBuf::from("evals/a-passes.json"),
            PathBuf::from("evals/b-fails.json"),
        ],
    )
    .expect("suite");
    let options = options(root, out.path(), 3, 3);
    let (run, _) = runner::run_suite(&options, &suite, &ScriptAgent).expect("runs");
    let order: Vec<(&str, u32)> = run
        .results
        .iter()
        .map(|result| (result.task.as_str(), result.trial))
        .collect();
    assert_eq!(
        order,
        [
            ("a-passes", 1),
            ("a-passes", 2),
            ("a-passes", 3),
            ("b-fails", 1),
            ("b-fails", 2),
            ("b-fails", 3)
        ]
    );
    assert_eq!(run.trials_per_task, 3);
    assert_eq!(run.summary().passed, 3);
}
