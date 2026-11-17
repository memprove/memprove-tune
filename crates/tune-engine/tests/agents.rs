//! The agent adapters against fake agents (shell scripts in
//! `tests/fixtures/agents/`) that replay real or documented output. No
//! test starts the real `claude` or `codex`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use tune_engine::{
    Agent, AgentEnd, AgentKind, AgentOutcome, AgentRequest, AgentSpec, Usage,
    agent::{
        CLAUDE_PROGRAM_ENV, CODEX_PROGRAM_ENV, PROMPT_FILE, Programs, REMOVED_ENV, STDERR_FILE,
        STDOUT_FILE, USAGE_FILE, agent_with,
    },
    agent_for,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agents")
        .join(name)
}

fn fake_programs() -> Programs {
    Programs {
        claude: fixture("claude").to_string_lossy().into_owned(),
        codex: fixture("codex").to_string_lossy().into_owned(),
    }
}

fn spec(kind: AgentKind, model: Option<&str>, command: &[&str], extra: &[&str]) -> AgentSpec {
    let strings = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
    AgentSpec {
        kind,
        model: model.map(str::to_owned),
        command: strings(command),
        extra_args: strings(extra),
        pass_env: Vec::new(),
    }
}

/// A trial's directories: `work` (the worktree) and `log`.
struct Trial {
    _dir: tempfile::TempDir,
    work: PathBuf,
    log: PathBuf,
}

impl Trial {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let work = dir.path().join("work");
        fs::create_dir(&work).expect("work");
        let log = dir.path().join("log");
        Self {
            work,
            log,
            _dir: dir,
        }
    }

    fn run(&self, agent: &dyn Agent, prompt: &str, timeout: Duration) -> AgentOutcome {
        self.run_passing(agent, prompt, timeout, &[])
    }

    /// As [`Trial::run`], the agent also inheriting `pass_env`.
    fn run_passing(
        &self,
        agent: &dyn Agent,
        prompt: &str,
        timeout: Duration,
        pass_env: &[String],
    ) -> AgentOutcome {
        agent.run(&AgentRequest {
            workdir: &self.work,
            prompt,
            timeout,
            log_dir: &self.log,
            env: &[],
            pass_env,
        })
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.log.join(name)).unwrap_or_default()
    }

    /// The lines of `agent.err` that start with `prefix`, without it.
    fn err_lines(&self, prefix: &str) -> Vec<String> {
        self.log(STDERR_FILE)
            .lines()
            .filter_map(|line| line.strip_prefix(prefix).map(str::to_owned))
            .collect()
    }
}

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn claude_replays_a_real_result_and_reports_its_usage() {
    let agent = agent_with(
        &spec(AgentKind::ClaudeCode, None, &[], &[]),
        &fake_programs(),
    )
    .expect("adapter");
    assert_eq!(agent.version().as_deref(), Some("2.1.325 (Claude Code)"));
    let trial = Trial::new();
    let prompt = "Create a file named ok.txt containing ok";
    let outcome = trial.run(agent.as_ref(), prompt, MINUTE);
    assert_eq!(outcome.end, AgentEnd::Exited(0));
    assert_eq!(
        outcome.usage,
        Usage {
            input_tokens: Some(4),
            output_tokens: Some(187),
            cache_read_tokens: Some(69_492),
            cache_write_tokens: Some(20_173),
            cost_microusd: Some(179_038),
            turns: Some(2),
        }
    );
    assert_eq!(
        trial.err_lines("arg: "),
        [
            "-p",
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
            "--no-session-persistence",
            "--",
            prompt
        ]
    );
    assert_eq!(trial.log(PROMPT_FILE), prompt);
    assert_eq!(
        trial.log(STDOUT_FILE),
        fs::read_to_string(fixture("claude-result.json")).expect("fixture")
    );
    assert_eq!(
        fs::read_to_string(trial.work.join("ok.txt")).expect("the agent ran in the worktree"),
        "ok\n"
    );
    assert!(trial.err_lines("tune: ").is_empty());
}

#[test]
fn claude_passes_the_model_and_extra_arguments_before_the_prompt() {
    let agent = agent_with(
        &spec(
            AgentKind::ClaudeCode,
            Some("sonnet"),
            &[],
            &["--max-turns", "30"],
        ),
        &fake_programs(),
    )
    .expect("adapter");
    let trial = Trial::new();
    trial.run(agent.as_ref(), "-x looks like a flag", MINUTE);
    let args = trial.err_lines("arg: ");
    assert_eq!(args[3..5], ["--model", "sonnet"]);
    assert_eq!(
        args[args.len() - 4..],
        ["--max-turns", "30", "--", "-x looks like a flag"]
    );
}

#[test]
fn claude_output_that_is_not_json_reports_no_usage() {
    let agent = agent_with(
        &spec(AgentKind::ClaudeCode, None, &[], &[]),
        &fake_programs(),
    )
    .expect("adapter");
    let trial = Trial::new();
    let outcome = trial.run(agent.as_ref(), "garbage please", MINUTE);
    assert_eq!(outcome.end, AgentEnd::Exited(0));
    assert_eq!(outcome.usage, Usage::default());
    assert!(trial.err_lines("tune: ").is_empty());
}

#[test]
fn claude_error_result_is_noted_and_its_exit_code_kept() {
    let agent = agent_with(
        &spec(AgentKind::ClaudeCode, None, &[], &[]),
        &fake_programs(),
    )
    .expect("adapter");
    let trial = Trial::new();
    let outcome = trial.run(agent.as_ref(), "error out", MINUTE);
    assert_eq!(outcome.end, AgentEnd::Exited(1));
    assert_eq!(
        outcome.usage,
        Usage {
            input_tokens: Some(3),
            output_tokens: Some(9),
            cost_microusd: Some(10_000),
            turns: Some(2),
            ..Usage::default()
        }
    );
    assert_eq!(
        trial.err_lines("tune: "),
        ["claude reported an error result (subtype error_max_turns)"]
    );
    assert!(
        trial
            .log(STDERR_FILE)
            .ends_with("tune: claude reported an error result (subtype error_max_turns)\n"),
        "the note is the last line"
    );
}

#[test]
fn a_timed_out_agent_is_killed_with_its_whole_group() {
    let agent = agent_with(
        &spec(AgentKind::ClaudeCode, None, &[], &[]),
        &fake_programs(),
    )
    .expect("adapter");
    let trial = Trial::new();
    let marker = trial.work.join("late");
    let prompt = format!("hang {}", marker.display());
    let outcome = trial.run(agent.as_ref(), &prompt, Duration::from_millis(500));
    assert_eq!(outcome.end, AgentEnd::TimedOut);
    assert!(outcome.duration_ms < 5_000, "{}", outcome.duration_ms);
    assert_eq!(outcome.usage, Usage::default());
    thread::sleep(Duration::from_secs(3));
    assert!(!marker.exists(), "the agent's background child was killed");
}

#[test]
fn a_missing_program_failed_to_start() {
    let programs = Programs {
        claude: "/nonexistent/engine-test/claude".to_owned(),
        codex: "/nonexistent/engine-test/codex".to_owned(),
    };
    for (kind, variable) in [
        (AgentKind::ClaudeCode, CLAUDE_PROGRAM_ENV),
        (AgentKind::Codex, CODEX_PROGRAM_ENV),
    ] {
        let agent = agent_with(&spec(kind, None, &[], &[]), &programs).expect("adapter");
        assert_eq!(agent.version(), None);
        let trial = Trial::new();
        let outcome = trial.run(agent.as_ref(), "anything", MINUTE);
        let AgentEnd::FailedToStart(detail) = &outcome.end else {
            panic!("started: {outcome:?}");
        };
        assert!(
            detail.starts_with("cannot start /nonexistent/engine-test/")
                && detail.contains(variable),
            "{detail}"
        );
        assert_eq!(outcome.usage, Usage::default());
    }
    let agent = agent_with(
        &spec(
            AgentKind::Command,
            None,
            &["/nonexistent/engine-test/agent"],
            &[],
        ),
        &programs,
    )
    .expect("adapter");
    let outcome = Trial::new().run(agent.as_ref(), "anything", MINUTE);
    assert!(
        matches!(&outcome.end, AgentEnd::FailedToStart(detail)
            if detail.starts_with("cannot start /nonexistent/engine-test/agent: ")),
        "{outcome:?}"
    );
}

#[test]
fn codex_replays_its_events_and_sums_the_usage() {
    let agent = agent_with(
        &spec(
            AgentKind::Codex,
            Some("gpt-5.5"),
            &[],
            &["--color", "never"],
        ),
        &fake_programs(),
    )
    .expect("adapter");
    assert_eq!(agent.version().as_deref(), Some("codex-cli 0.169.0"));
    let trial = Trial::new();
    let outcome = trial.run(
        agent.as_ref(),
        "Create a file named ok.txt containing ok",
        MINUTE,
    );
    assert_eq!(outcome.end, AgentEnd::Exited(0));
    assert_eq!(
        outcome.usage,
        Usage {
            input_tokens: Some(315),
            output_tokens: Some(122),
            cache_read_tokens: Some(24_448),
            cache_write_tokens: None,
            cost_microusd: None,
            turns: None,
        }
    );
    let workdir = std::path::absolute(&trial.work).expect("absolute");
    assert_eq!(
        trial.err_lines("arg: "),
        [
            "exec",
            "--json",
            "--model",
            "gpt-5.5",
            "--sandbox",
            "workspace-write",
            "--ephemeral",
            "-C",
            workdir.to_str().expect("utf-8"),
            "--color",
            "never",
            "--",
            "Create a file named ok.txt containing ok"
        ]
    );
    assert!(trial.work.join("ok.txt").exists());
}

#[test]
fn codex_failed_turn_is_noted() {
    let agent =
        agent_with(&spec(AgentKind::Codex, None, &[], &[]), &fake_programs()).expect("adapter");
    let trial = Trial::new();
    let outcome = trial.run(agent.as_ref(), "fail now", MINUTE);
    assert_eq!(outcome.end, AgentEnd::Exited(1));
    assert_eq!(outcome.usage, Usage::default());
    assert_eq!(
        trial.err_lines("tune: "),
        ["codex reported turn.failed: stream disconnected before completion"]
    );
}

#[test]
fn command_gets_placeholders_and_variables_and_reports_its_usage_file() {
    let agent_path = fixture("command-agent");
    let agent = agent_with(
        &spec(
            AgentKind::Command,
            Some("local-model"),
            &[
                agent_path.to_str().expect("utf-8"),
                "{prompt_file}",
                "{workdir}",
            ],
            &["4"],
        ),
        &fake_programs(),
    )
    .expect("adapter");
    assert_eq!(agent.version(), None);
    let trial = Trial::new();
    let prompt = "Fix the parser.\nKeep the API.";
    let outcome = trial.run(agent.as_ref(), prompt, MINUTE);
    assert_eq!(
        outcome.end,
        AgentEnd::Exited(4),
        "{}",
        trial.log(STDERR_FILE)
    );
    assert_eq!(
        outcome.usage,
        Usage {
            input_tokens: Some(120),
            output_tokens: Some(45),
            cost_microusd: Some(3100),
            turns: Some(3),
            ..Usage::default()
        }
    );
    let prompt_file = std::path::absolute(trial.log.join(PROMPT_FILE)).expect("absolute");
    let workdir = std::path::absolute(&trial.work).expect("absolute");
    assert_eq!(
        trial.err_lines("arg: "),
        [
            prompt_file.to_str().expect("utf-8"),
            workdir.to_str().expect("utf-8"),
            "4"
        ]
    );
    assert_eq!(
        fs::read_to_string(trial.work.join("seen-prompt.md")).expect("copied"),
        prompt
    );
    assert_eq!(trial.log(STDOUT_FILE), "model=local-model\n");
    assert!(trial.log.join(USAGE_FILE).exists());
}

#[test]
fn command_usage_file_that_is_not_valid_is_noted() {
    let agent_path = fixture("command-agent");
    let agent = agent_with(
        &spec(
            AgentKind::Command,
            None,
            &[
                agent_path.to_str().expect("utf-8"),
                "{prompt_file}",
                "{workdir}",
                "bad-usage",
            ],
            &[],
        ),
        &fake_programs(),
    )
    .expect("adapter");
    let trial = Trial::new();
    let outcome = trial.run(agent.as_ref(), "p", MINUTE);
    assert_eq!(
        outcome.end,
        AgentEnd::Exited(0),
        "{}",
        trial.log(STDERR_FILE)
    );
    assert_eq!(outcome.usage, Usage::default());
    assert_eq!(trial.log(STDOUT_FILE), "model=unset\n");
    let notes = trial.err_lines("tune: ");
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("ignored the usage file: ")
            && notes[0].ends_with("`cost_usd` is not a known member"),
        "{notes:?}"
    );
}

#[test]
fn command_ended_by_a_signal_is_signaled() {
    let agent = agent_with(
        &spec(
            AgentKind::Command,
            None,
            &["sh", "-c", "kill -TERM $$"],
            &[],
        ),
        &fake_programs(),
    )
    .expect("adapter");
    let outcome = Trial::new().run(agent.as_ref(), "p", MINUTE);
    assert_eq!(outcome.end, AgentEnd::Signaled(15));
}

/// Set in the copy of this test binary that
/// [`the_environment_names_the_programs_and_drops_the_callers_session`]
/// starts.
const CHILD: &str = "ENGINE_TEST_ENV_CHILD";

/// Variables of a calling Claude Code session (and other credentials) that
/// are not on the agent's list, so never reach it.
const CALLER_ENV: &[&str] = &[
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_SESSION_INGRESS_TOKEN_FILE",
    "CLAUDE_CODE_USER_EMAIL",
    "CLAUDE_CODE_ACCOUNT_UUID",
    "CLAUDE_CODE_ORGANIZATION_UUID",
    "CLAUDE_CODE_REMOTE",
    "CLAUDE_CODE_REMOTE_SDK_URL",
    "MEMPROVE_DATA_DIR",
    "SSH_AUTH_SOCK",
    "GITHUB_TOKEN",
];

/// The environment is the process's own, so the test runs itself again as
/// a child with the variables set (setting them here would need `unsafe`).
#[test]
fn the_environment_names_the_programs_and_drops_the_callers_session() {
    if std::env::var_os(CHILD).is_none() {
        let mut child = Command::new(std::env::current_exe().expect("test binary"));
        child
            .args([
                "--exact",
                "the_environment_names_the_programs_and_drops_the_callers_session",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env(CLAUDE_PROGRAM_ENV, fixture("claude"))
            .env(CODEX_PROGRAM_ENV, fixture("codex"))
            .env("ANTHROPIC_API_KEY", "the-provider's")
            .env("LC_ALL", "C")
            .env("ENGINE_TEST_KEEP", "kept");
        for name in REMOVED_ENV.iter().chain(CALLER_ENV) {
            child.env(name, "the-caller's");
        }
        let output = child.output().expect("runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let keep = ["ENGINE_TEST_KEEP".to_owned()];
    for kind in [AgentKind::ClaudeCode, AgentKind::Codex] {
        let agent = agent_for(&spec(kind, None, &[], &[])).expect("adapter");
        assert!(
            agent.version().is_some(),
            "{kind:?}: the fake from the variable"
        );
        for pass_env in [&[][..], &keep[..]] {
            let trial = Trial::new();
            let outcome = trial.run_passing(agent.as_ref(), "Create ok.txt", MINUTE, pass_env);
            assert_eq!(outcome.end, AgentEnd::Exited(0), "{kind:?}");
            let env = trial.err_lines("env: ");
            for name in REMOVED_ENV.iter().chain(CALLER_ENV) {
                assert!(
                    !env.iter().any(|line| line.starts_with(&format!("{name}="))),
                    "{kind:?}: {name} reached the agent: {env:?}"
                );
            }
            for kept in ["ANTHROPIC_API_KEY=the-provider's", "LC_ALL=C"] {
                assert!(env.iter().any(|line| line == kept), "{kind:?}: {env:?}");
            }
            assert_eq!(
                env.iter().any(|line| line == "ENGINE_TEST_KEEP=kept"),
                !pass_env.is_empty(),
                "{kind:?}, --pass-env {pass_env:?}: {env:?}"
            );
        }
    }
    let agent_path = fixture("command-agent");
    let agent = agent_for(&spec(
        AgentKind::Command,
        None,
        &[
            agent_path.to_str().expect("utf-8"),
            "{prompt_file}",
            "{workdir}",
        ],
        &[],
    ))
    .expect("adapter");
    let trial = Trial::new();
    let outcome = trial.run(agent.as_ref(), "p", MINUTE);
    assert_eq!(
        outcome.end,
        AgentEnd::Exited(0),
        "the trial's variables replace the caller's: {}",
        trial.log(STDERR_FILE)
    );
}
