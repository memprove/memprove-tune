//! What the agent of a trial can find. Its working copy has no
//! task file and no check patch of the run, and its repository has one
//! commit: no history, no branch of the repository it came from, no later
//! commit. The check patch is still applied for the check, and the
//! repository the run reads is left as it was.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tune_engine::{
    AgentKind, AgentSpec, Outcome,
    agent::agent_for,
    git::git,
    runner::{self, RunOptions},
    task,
    workspace::START_SUBJECT,
};

/// The check patch: a test that wants 42 in answer.txt.
const CHECK_PATCH: &str = "diff --git a/test_answer.sh b/test_answer.sh\n\
                           new file mode 100644\n\
                           --- /dev/null\n\
                           +++ b/test_answer.sh\n\
                           @@ -0,0 +1 @@\n\
                           +test \"$(cat answer.txt)\" = 42\n";

/// A repository whose HEAD holds the task file and its check patch, and a
/// branch `fix` with a later commit that holds the answer.
fn repository() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for args in [
        &["init", "--quiet", "-b", "main"][..],
        &["config", "user.email", "eval@example.invalid"],
        &["config", "user.name", "eval"],
    ] {
        git(root, args).expect("git");
    }
    fs::write(root.join("answer.txt"), "unknown\n").expect("answer");
    fs::create_dir_all(root.join("evals")).expect("evals");
    fs::write(
        root.join("evals/answer.json"),
        r#"{"schema": "tune.task.v1", "title": "the task file says 42",
            "prompt": "Write the answer to answer.txt.",
            "check": {"argv": ["sh", "test_answer.sh"]},
            "check_patch": "evals/answer.check.patch"}"#,
    )
    .expect("task");
    fs::write(root.join("evals/answer.check.patch"), CHECK_PATCH).expect("patch");
    git(root, &["add", "."]).expect("add");
    git(root, &["commit", "--quiet", "-m", "start"]).expect("commit");
    git(root, &["checkout", "--quiet", "-b", "fix"]).expect("branch");
    fs::write(root.join("answer.txt"), "42\n").expect("fix");
    git(root, &["commit", "--quiet", "-am", "the later fix"]).expect("fix commit");
    let fix = git(root, &["rev-parse", "HEAD"])
        .expect("fix")
        .trim()
        .to_owned();
    git(root, &["checkout", "--quiet", "main"]).expect("main");
    (dir, fix)
}

/// A `command` agent that looks for the answer everywhere a look at its
/// working copy and its repository would find it, writes what it saw next
/// to its prompt, then writes the answer.
fn options(root: &Path, out: &Path, fix: &str) -> RunOptions {
    let script = format!(
        "seen=\"$(dirname \"$TUNE_PROMPT_FILE\")/seen\"; {{ \
         ls -a evals; cat evals/answer.json evals/answer.check.patch; \
         test -e evals/answer.json && echo FOUND-TASK; test -e evals/answer.check.patch && echo FOUND-PATCH; \
         git log --all --format=%s; git branch --all; git show fix:answer.txt; git show {fix}; \
         echo \"commits: $(git rev-list --all | wc -l | tr -d ' ')\"; \
         echo \"git dir: $(git rev-parse --git-common-dir)\"; \
         }} > \"$seen\" 2>&1; echo 42 > answer.txt"
    );
    RunOptions {
        repo_root: root.to_path_buf(),
        agent: AgentSpec {
            kind: AgentKind::Command,
            model: None,
            command: vec!["sh".to_owned(), "-c".to_owned(), script],
            extra_args: Vec::new(),
            pass_env: Vec::new(),
        },
        trials: 1,
        jobs: 1,
        out_root: out.to_path_buf(),
        keep_worktrees: false,
        progress: false,
    }
}

#[test]
fn the_agent_finds_no_task_file_check_patch_or_later_commit() {
    let (repo, fix) = repository();
    let root = repo.path();
    let out = tempfile::tempdir().expect("out");
    let head = git(root, &["rev-parse", "HEAD"]).expect("head");
    let index = fs::read(root.join(".git/index")).expect("index");

    let options = options(root, out.path(), &fix);
    let suite = task::load_suite(root, &[PathBuf::from("evals")]).expect("suite");
    let agent = agent_for(&options.agent).expect("agent");
    let (record, run_dir) = runner::run_suite(&options, &suite, agent.as_ref()).expect("runs");
    let result = &record.results[0];
    let logs = run_dir.join(&result.logs);

    // What the agent saw: no task file, no check patch, one commit, its
    // own Git directory, and nothing of the later fix.
    let seen = fs::read_to_string(logs.join("seen")).expect("seen");
    assert!(!seen.contains("the task file says 42"), "{seen}");
    assert!(!seen.contains("= 42"), "{seen}");
    assert!(!seen.contains("the later fix"), "{seen}");
    assert!(!seen.lines().any(|line| line.trim() == "fix"), "{seen}");
    assert!(!seen.contains("FOUND-"), "{seen}");
    // Its one commit names the commit whose files it holds.
    assert!(
        seen.contains(&format!("{START_SUBJECT} {}\n", head.trim())),
        "{seen}"
    );
    assert!(seen.contains("commits: 1\n"), "{seen}");
    assert!(seen.contains("git dir: .git\n"), "{seen}");

    // The check patch was applied for the check, which the answer passes;
    // the diff is the agent's change alone, and the trial's base is the
    // repository's commit.
    assert_eq!(result.outcome, Outcome::Passed, "{:?}", result.reason);
    assert_eq!(result.base, head.trim());
    assert_eq!(
        (
            result.diff.files,
            result.diff.insertions,
            result.diff.deletions
        ),
        (1, 1, 1)
    );
    let diff = fs::read_to_string(logs.join("agent.diff")).expect("agent.diff");
    assert!(diff.contains("+++ b/answer.txt"), "{diff}");
    assert!(!diff.contains("evals/"), "{diff}");

    // The repository the run read is as it was: same HEAD, same index,
    // clean, no worktree added, no ref added.
    assert_eq!(git(root, &["rev-parse", "HEAD"]).expect("head"), head);
    assert_eq!(fs::read(root.join(".git/index")).expect("index"), index);
    assert_eq!(git(root, &["status", "--porcelain"]).expect("status"), "");
    assert_eq!(
        git(root, &["worktree", "list"])
            .expect("list")
            .lines()
            .count(),
        1
    );
    assert_eq!(
        git(root, &["for-each-ref", "--format=%(refname)"])
            .expect("refs")
            .lines()
            .collect::<Vec<_>>(),
        ["refs/heads/fix", "refs/heads/main"]
    );
}
