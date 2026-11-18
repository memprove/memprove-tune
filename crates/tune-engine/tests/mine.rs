//! `mine` on temporary Git repositories whose
//! commits are made to be picked or passed over.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use tune_engine::{
    CommandSpec, EvalError, Task,
    json::{Reader, parse},
    mine::{MineSpec, MineStatus, MinedTask, mine, verify_task},
    workspace::Worktree,
};

/// A Git repository in a temporary directory; commits get increasing
/// dates so that `git log` orders them as made.
struct Repo {
    dir: tempfile::TempDir,
    commits: u32,
}

impl Repo {
    fn new() -> Self {
        let repo = Self {
            dir: tempfile::tempdir().expect("tempdir"),
            commits: 0,
        };
        repo.git(&["init", "--quiet", "-b", "main"]);
        repo
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> String {
        let date = format!("2026-01-01T00:{:02}:00Z", self.commits);
        let output = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args([
                "-c",
                "user.name=eval",
                "-c",
                "user.email=eval@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.root().join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    fn append(&self, name: &str, text: &str) {
        let current = fs::read_to_string(self.root().join(name)).unwrap_or_default();
        self.write(name, &format!("{current}{text}"));
    }

    /// Commits everything with `message` and returns the commit's name.
    fn commit(&mut self, message: &str) -> String {
        self.commits += 1;
        self.git(&["add", "--all"]);
        self.git(&["commit", "--quiet", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

/// The history the tests mine, newest first:
///
/// - a merge of `five` (passed over: a merge) and `five` itself (picked);
/// - 21 new files with a test, and `four` spelt right (too big);
/// - `four`, whose code does not do what its test asks (verify: fails after);
/// - a weaker test with other code (verify: passes before);
/// - `three` without a test (no test file);
/// - documentation only;
/// - `two`, with its test and a trailer in its message (picked);
/// - the root commit (no parent).
struct History {
    repo: Repo,
    two: String,
    weaker: String,
    four: String,
    five: String,
    merge: String,
    three: String,
}

fn history() -> History {
    let mut repo = Repo::new();
    repo.write("Makefile", "test:\n\tsh tests/run.sh\n");
    repo.write("src/lib.txt", "one\n");
    repo.write("tests/run.sh", "grep -qx one src/lib.txt\n");
    repo.commit("lib: one");

    repo.append("src/lib.txt", "two\n");
    repo.append("tests/run.sh", "grep -qx two src/lib.txt\n");
    let two = repo.commit(
        "lib: add two\n\nThe library also says two.\n\nCo-Authored-By: Someone <someone@example.invalid>\nSigned-off-by: Someone <someone@example.invalid>",
    );

    repo.write("README.md", "# lib\n");
    repo.write("docs/usage.md", "Use it.\n");
    repo.commit("docs: explain the library");

    repo.append("src/lib.txt", "three\n");
    let three = repo.commit("lib: add three without a test");

    repo.append("tests/run.sh", "true\n");
    repo.write("src/other.txt", "other\n");
    let weaker = repo.commit("lib: other, with a test that tests nothing new");

    repo.append("src/lib.txt", "fuor\n");
    repo.append("tests/run.sh", "grep -qx four src/lib.txt\n");
    let four = repo.commit("lib: add four, misspelt");

    for index in 0..21 {
        repo.write(&format!("src/many/f{index}.txt"), "x\n");
    }
    repo.write("tests/many_test.sh", "true\n");
    repo.append("src/lib.txt", "four\n");
    repo.commit("lib: many files, and four spelt right");

    repo.git(&["checkout", "--quiet", "-b", "five"]);
    repo.append("src/lib.txt", "five\n");
    repo.append("tests/run.sh", "grep -qx five src/lib.txt\n");
    let five = repo.commit("lib: add five");
    repo.git(&["checkout", "--quiet", "main"]);
    repo.commits += 1;
    repo.git(&["merge", "--quiet", "--no-ff", "-m", "Merge five", "five"]);
    let merge = repo.git(&["rev-parse", "HEAD"]);
    History {
        repo,
        two,
        weaker,
        four,
        five,
        merge,
        three,
    }
}

fn ids(tasks: &[MinedTask]) -> Vec<String> {
    tasks.iter().map(|task| task.id.clone()).collect()
}

fn id(commit: &str) -> String {
    format!("mined-{}", &commit[..7])
}

#[test]
fn mine_picks_commits_that_change_tests_and_code_newest_first() {
    let h = history();
    let tasks = mine(&MineSpec::new(h.repo.root())).expect("mine");
    assert_eq!(
        ids(&tasks),
        [id(&h.five), id(&h.four), id(&h.weaker), id(&h.two)]
    );
    assert!(tasks.iter().all(|task| task.status == MineStatus::Written));
    assert!(!ids(&tasks).contains(&id(&h.merge)));
    assert!(!ids(&tasks).contains(&id(&h.three)));

    let two = tasks.iter().find(|task| task.commit == h.two).expect("two");
    assert_eq!(two.title, "lib: add two");
    assert_eq!(two.check, ["make", "test"]);
    assert_eq!(
        two.path,
        Path::new(".tune/tasks").join(format!("{}.json", id(&h.two)))
    );
    let text = fs::read_to_string(h.repo.root().join(&two.path)).expect("task file");
    let value = parse(&text, None).expect("json");
    let task = Reader::new(&value, None, "").expect("object");
    task.deny_unknown(&[
        "schema",
        "id",
        "title",
        "base",
        "prompt",
        "check",
        "check_patch",
        "agent_timeout_secs",
        "tags",
    ])
    .expect("only task members");
    let parent = h.repo.git(&["rev-parse", &format!("{}^", h.two)]);
    assert_eq!(task.str("base").expect("base"), parent);
    assert_eq!(
        task.str("prompt").expect("prompt"),
        "lib: add two\n\nThe library also says two."
    );
    assert_eq!(task.strs("tags").expect("tags"), ["mined"]);
    let patch = task.str("check_patch").expect("patch");
    assert_eq!(patch, format!(".tune/tasks/{}.check.patch", id(&h.two)));

    // The check patch holds the test change only and applies to the parent.
    let patch_text = fs::read_to_string(h.repo.root().join(patch)).expect("patch");
    assert!(
        patch_text.contains("+grep -qx two src/lib.txt"),
        "{patch_text}"
    );
    assert!(
        !patch_text.contains("src/lib.txt b/src/lib.txt"),
        "{patch_text}"
    );
    let scratch = h.repo.root().join(".git").join("u23-w4-test");
    let tree = Worktree::create(h.repo.root(), &scratch, "parent", &parent).expect("worktree");
    tree.apply(&h.repo.root().join(patch))
        .expect("applies to the parent");
    let run = fs::read_to_string(tree.path().join("tests/run.sh")).expect("run.sh");
    assert!(run.ends_with("grep -qx two src/lib.txt\n"), "{run}");
    tree.remove().expect("removed");
}

#[test]
fn a_second_run_overwrites_nothing_and_the_limit_counts_existing_tasks() {
    let h = history();
    let mut spec = MineSpec::new(h.repo.root());
    spec.limit = 2;
    let first = mine(&spec).expect("first");
    assert_eq!(ids(&first), [id(&h.five), id(&h.four)]);
    let path = h.repo.root().join(&first[0].path);
    fs::write(&path, "edited by hand\n").expect("edit");

    let second = mine(&spec).expect("second");
    assert_eq!(ids(&second), ids(&first));
    assert!(second.iter().all(|task| task.status == MineStatus::Exists));
    assert_eq!(fs::read_to_string(&path).expect("read"), "edited by hand\n");
}

#[test]
fn a_dry_run_writes_nothing() {
    let h = history();
    let mut spec = MineSpec::new(h.repo.root());
    spec.dry_run = true;
    let tasks = mine(&spec).expect("dry run");
    assert_eq!(tasks.len(), 4);
    assert!(tasks.iter().all(|task| task.status == MineStatus::DryRun));
    assert!(!h.repo.root().join(".tune").exists());
}

#[test]
fn verify_keeps_only_tasks_that_fail_before_and_pass_after() {
    let h = history();
    let mut spec = MineSpec::new(h.repo.root());
    spec.verify = true;
    spec.out_dir = PathBuf::from("evals/mined");
    let tasks = mine(&spec).expect("mine --verify");
    assert_eq!(
        ids(&tasks),
        [id(&h.five), id(&h.four), id(&h.weaker), id(&h.two)]
    );
    let status = |commit: &str| {
        tasks
            .iter()
            .find(|task| task.commit == commit)
            .map(|task| task.status.clone())
            .expect("found")
    };
    assert_eq!(status(&h.five), MineStatus::Written);
    assert_eq!(status(&h.two), MineStatus::Written);
    match status(&h.four) {
        MineStatus::Rejected(reason) => {
            assert!(reason.contains("on the commit with the change"), "{reason}");
            assert!(reason.contains("(logs: "), "{reason}");
        }
        other => panic!("four: {other:?}"),
    }
    match status(&h.weaker) {
        MineStatus::Rejected(reason) => assert!(
            reason.contains("the check passed on the base commit"),
            "{reason}"
        ),
        other => panic!("weaker: {other:?}"),
    }
    let written: Vec<String> = fs::read_dir(h.repo.root().join("evals/mined"))
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(written.len(), 4, "{written:?}");
    assert!(
        written
            .iter()
            .all(|name| name.contains(&id(&h.five)[6..]) || name.contains(&id(&h.two)[6..]))
    );
    // No worktree is left behind, and the work tree is as it was.
    let worktrees = h.repo.git(&["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 1, "{worktrees}");
    assert_eq!(
        h.repo
            .git(&["status", "--porcelain", "--untracked-files=no"]),
        ""
    );
}

#[test]
fn a_hand_written_task_is_verified_against_a_reference_commit() {
    let mut repo = Repo::new();
    repo.write("Makefile", "test:\n\tsh tests/run.sh\n");
    repo.write("src/lib.txt", "one\n");
    repo.write("tests/run.sh", "grep -qx one src/lib.txt\n");
    let base = repo.commit("lib: one");
    repo.write("tests/hidden.sh", "grep -qx two src/lib.txt\n");
    repo.git(&["add", "--all"]);
    let patch = repo.git(&["diff", "--cached", "--binary", "HEAD"]);
    repo.git(&["reset", "--quiet", "--hard"]);
    repo.git(&["clean", "-fdq"]);
    repo.git(&["checkout", "--quiet", "-b", "answer"]);
    repo.append("src/lib.txt", "two\n");
    repo.write("tests/hidden.sh", "grep -qx two src/lib.txt\n");
    let answer = repo.commit("lib: two (reference answer)");
    repo.git(&["checkout", "--quiet", "main"]);
    repo.write("evals/two.check.patch", &format!("{patch}\n"));
    let task = |reference_check: &str| Task {
        id: "two".to_owned(),
        title: None,
        base: Some(base.clone()),
        prompt: "Make the library say two.".to_owned(),
        setup: Vec::new(),
        check: CommandSpec {
            argv: vec!["sh".to_owned(), reference_check.to_owned()],
            timeout: std::time::Duration::from_secs(60),
        },
        check_patch: Some(PathBuf::from("evals/two.check.patch")),
        agent_timeout: std::time::Duration::from_secs(60),
        tags: Vec::new(),
        source: PathBuf::from("evals/two.json"),
    };
    assert_eq!(
        verify_task(repo.root(), &task("tests/hidden.sh"), &answer).expect("verify"),
        None
    );
    let reason = verify_task(repo.root(), &task("tests/run.sh"), &answer)
        .expect("verify")
        .expect("refused");
    assert!(
        reason.contains("the check passed on the base commit"),
        "{reason}"
    );
}

#[test]
fn the_range_limits_the_history_and_an_option_is_refused() {
    let h = history();
    let mut spec = MineSpec::new(h.repo.root());
    spec.range = Some(h.three.clone());
    spec.dry_run = true;
    assert_eq!(ids(&mine(&spec).expect("range")), [id(&h.two)]);
    spec.range = Some("--all".to_owned());
    assert!(matches!(mine(&spec), Err(EvalError::Usage(_))));
}

#[test]
fn mine_needs_a_test_command() {
    let mut repo = Repo::new();
    repo.write("src/a.txt", "a\n");
    repo.commit("a");
    let error = mine(&MineSpec::new(repo.root())).expect_err("no test command");
    assert!(error.to_string().contains("no test command"), "{error}");
    // It says how to give one.
    assert!(error.to_string().contains("--test"), "{error}");
}

/// The members `setup` and `check` of the task file `path` as argv lists.
fn commands(root: &Path, path: &Path) -> (Vec<Vec<String>>, Vec<String>) {
    let text = fs::read_to_string(root.join(path)).expect("task file");
    let value = parse(&text, None).expect("json");
    let argv = |value: &tune_engine::json::JsonValue| -> Vec<String> {
        Reader::new(value, None, "")
            .expect("object")
            .strs("argv")
            .expect("argv")
    };
    let setup = value
        .get("setup")
        .and_then(|setup| setup.as_array())
        .map(|commands| commands.iter().map(argv).collect())
        .unwrap_or_default();
    (setup, argv(value.get("check").expect("check")))
}

#[test]
fn a_uv_project_installs_and_tests_through_uv() {
    let mut repo = Repo::new();
    repo.write("pyproject.toml", "[project]\nname = \"p\"\n");
    repo.write("uv.lock", "version = 1\n");
    repo.write("src/p.py", "A = 1\n");
    repo.commit("init");
    repo.append("src/p.py", "B = 2\n");
    repo.write("tests/test_p.py", "def test_b():\n    pass\n");
    repo.commit("p: B");
    let tasks = mine(&MineSpec::new(repo.root())).expect("uv");
    let expected = ["uv", "run", "--frozen", "pytest", "tests/test_p.py"];
    assert_eq!(tasks[0].check, expected);
    let (setup, check) = commands(repo.root(), &tasks[0].path);
    assert_eq!(setup, [["uv", "sync", "--frozen"]]);
    assert_eq!(check, expected);
}

fn words(command: &[&str]) -> Vec<String> {
    command.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn the_setup_and_test_commands_that_are_given_replace_the_guess() {
    let h = history();
    let tasks = mine(&MineSpec {
        setup: Some(vec![words(&["touch", ".prepared"]), words(&["true"])]),
        test: Some(words(&["sh", "tests/run.sh"])),
        ..MineSpec::new(h.repo.root())
    })
    .expect("mine");
    let two = tasks.iter().find(|task| task.commit == h.two).expect("two");
    assert_eq!(two.check, ["sh", "tests/run.sh"]);
    let (setup, check) = commands(h.repo.root(), &two.path);
    assert_eq!(setup, [vec!["touch", ".prepared"], vec!["true"]]);
    assert_eq!(check, ["sh", "tests/run.sh"]);
}

#[test]
fn a_given_test_command_keeps_the_setup_that_was_guessed() {
    let mut repo = Repo::new();
    repo.write(
        "package.json",
        "{\"scripts\": {\"test\": \"node test.js\"}}\n",
    );
    repo.write("package-lock.json", "{}\n");
    repo.write("src/a.js", "exports.a = 1;\n");
    repo.commit("init");
    repo.append("src/a.js", "exports.b = 2;\n");
    repo.write("test/a.test.js", "require('../src/a.js');\n");
    repo.commit("a: b");
    let tasks = mine(&MineSpec {
        test: Some(words(&["npx", "vitest", "run"])),
        ..MineSpec::new(repo.root())
    })
    .expect("mine");
    let (setup, check) = commands(repo.root(), &tasks[0].path);
    assert_eq!(setup, [["npm", "ci"]]);
    assert_eq!(check, ["npx", "vitest", "run"]);
}

#[test]
fn a_given_pytest_command_narrows_to_the_changed_tests_without_a_project_file() {
    let mut repo = Repo::new();
    repo.write("src/p.py", "A = 1\n");
    repo.commit("init");
    repo.append("src/p.py", "B = 2\n");
    repo.write("tests/test_p.py", "def test_b():\n    pass\n");
    repo.commit("p: B");
    let tasks = mine(&MineSpec {
        dry_run: true,
        test: Some(words(&["uv", "run", "pytest", "-q"])),
        ..MineSpec::new(repo.root())
    })
    .expect("no project file is needed");
    assert_eq!(
        tasks[0].check,
        ["uv", "run", "pytest", "-q", "tests/test_p.py"]
    );

    // A command that does not run pytest is taken as it is.
    let tasks = mine(&MineSpec {
        dry_run: true,
        test: Some(words(&["sh", "run-tests.sh"])),
        ..MineSpec::new(repo.root())
    })
    .expect("as given");
    assert_eq!(tasks[0].check, ["sh", "run-tests.sh"]);
}

#[test]
fn a_commit_whose_only_test_file_is_documentation_is_passed_over() {
    let mut repo = Repo::new();
    repo.write("Makefile", "test:\n\tsh tests/run.sh\n");
    repo.write("src/lib.txt", "one\n");
    repo.write("tests/run.sh", "grep -qx one src/lib.txt\n");
    repo.commit("lib: one");

    // Continuous integration and a note beside the tests: nothing here
    // judges an agent's work.
    repo.write("ci/smoke.yml", "run: make test\n");
    repo.write("tests/README.md", "The smoke job runs these.\n");
    repo.commit("ci: run the tests in the smoke job");

    // A test and a note beside it: a task, and the note stays among its
    // hidden files.
    repo.append("src/lib.txt", "two\n");
    repo.append("tests/run.sh", "grep -qx two src/lib.txt\n");
    repo.append("tests/README.md", "They look for two as well.\n");
    let two = repo.commit("lib: add two");

    let tasks = mine(&MineSpec::new(repo.root())).expect("mine");
    assert_eq!(ids(&tasks), [id(&two)]);
    let patch = fs::read_to_string(
        repo.root()
            .join(format!(".tune/tasks/{}.check.patch", id(&two))),
    )
    .expect("patch");
    assert!(patch.contains("b/tests/run.sh"), "{patch}");
    assert!(patch.contains("b/tests/README.md"), "{patch}");
}

#[test]
fn rust_checks_narrow_to_the_package_and_its_integration_tests() {
    let mut repo = Repo::new();
    repo.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n",
    );
    repo.write(
        "crates/a/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n",
    );
    repo.write(
        "crates/b/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
    );
    repo.write("crates/a/src/lib.rs", "pub fn a() {}\n");
    repo.write("crates/b/src/lib.rs", "pub fn b() {}\n");
    repo.commit("init");

    repo.append("crates/a/src/lib.rs", "pub fn a2() {}\n");
    repo.write(
        "crates/a/tests/e2e.rs",
        "#[test]\nfn a2() { alpha::a2(); }\n",
    );
    let one = repo.commit("alpha: a2");

    repo.append("crates/a/src/lib.rs", "pub fn a3() {}\n");
    repo.write("crates/a/tests/common/mod.rs", "pub fn helper() {}\n");
    let common = repo.commit("alpha: a3 with a test helper");

    repo.append("crates/b/src/lib.rs", "pub fn b2() {}\n");
    repo.write("crates/b/tests/b.rs", "#[test]\nfn b2() { beta::b2(); }\n");
    repo.append(
        "crates/a/tests/e2e.rs",
        "#[test]\nfn a3() { alpha::a3(); }\n",
    );
    let both = repo.commit("both: b2 and a3 tests");

    let mut spec = MineSpec::new(repo.root());
    spec.dry_run = true;
    let tasks = mine(&spec).expect("mine");
    let check = |commit: &str| {
        tasks
            .iter()
            .find(|task| task.commit == commit)
            .map(|task| task.check.join(" "))
            .expect("found")
    };
    assert_eq!(check(&one), "cargo test -p alpha --test e2e");
    assert_eq!(check(&common), "cargo test -p alpha");
    assert_eq!(check(&both), "cargo test -p alpha -p beta");
}

#[test]
fn python_and_go_checks_narrow_to_the_changed_tests() {
    let mut python = Repo::new();
    python.write("pyproject.toml", "[project]\nname = \"p\"\n");
    python.write("src/p.py", "A = 1\n");
    python.commit("init");
    python.append("src/p.py", "B = 2\n");
    python.write("tests/test_p.py", "def test_b():\n    pass\n");
    let commit = python.commit("p: B");
    let tasks = mine(&MineSpec {
        dry_run: true,
        ..MineSpec::new(python.root())
    })
    .expect("python");
    assert_eq!(tasks[0].commit, commit);
    assert_eq!(
        tasks[0].check,
        ["python3", "-m", "pytest", "tests/test_p.py"]
    );

    let mut go = Repo::new();
    go.write("go.mod", "module example.com/g\n");
    go.write("pkg/g/g.go", "package g\n");
    go.commit("init");
    go.append("pkg/g/g.go", "func G() {}\n");
    go.write("pkg/g/g_test.go", "package g\n");
    go.commit("g: G");
    let tasks = mine(&MineSpec {
        dry_run: true,
        ..MineSpec::new(go.root())
    })
    .expect("go");
    assert_eq!(tasks[0].check, ["go", "test", "./pkg/g"]);
}
