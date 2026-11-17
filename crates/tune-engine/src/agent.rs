//! Agent adapters: how `claude -p`, `codex exec` and any other program are
//! run on one task in its worktree, and what they reported.
//!
//! Every adapter starts its program through [`process::run`] (its own
//! process group, the whole group killed past the time limit), with the
//! worktree as the working directory, standard input closed, standard
//! output in `agent.out` and standard error in `agent.err` of the trial's
//! log directory, the prompt in `prompt.md` there, and an environment of
//! the variables [`passes`] keeps and nothing else. An adapter
//! reports how the program ended and what it said it used; it never judges
//! the work.

use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    AgentEnd, AgentKind, AgentOutcome, AgentSpec, EvalError,
    process::{self, Exit, Invocation},
};

mod claude;
mod codex;
mod command;

/// One agent run.
#[derive(Debug, Clone, Copy)]
pub struct AgentRequest<'a> {
    /// The task's worktree; the agent's working directory.
    pub workdir: &'a Path,
    /// The task's prompt.
    pub prompt: &'a str,
    /// The task's time limit for the agent.
    pub timeout: Duration,
    /// The trial's directory: the adapter writes `agent.out` and
    /// `agent.err` (and may write more files) here.
    pub log_dir: &'a Path,
    /// Variables set in the agent's environment, after the inherited ones
    /// [`passes`] keeps and before the adapter's own.
    pub env: &'a [(String, String)],
    /// Names the run passes on besides [`PASSED_ENV`] (`--pass-env`).
    pub pass_env: &'a [String],
}

/// An agent that can run tasks; one value runs every trial of a run,
/// possibly from several threads.
pub trait Agent: Sync {
    /// What the agent says its version is, for `run.json`.
    fn version(&self) -> Option<String>;

    /// Runs the agent once and reports how it ended and what it used. It
    /// never judges the work.
    fn run(&self, request: &AgentRequest<'_>) -> AgentOutcome;
}

/// The variable that names the `claude` program; unset or empty is
/// `claude` on `PATH`.
pub const CLAUDE_PROGRAM_ENV: &str = "TUNE_CLAUDE";
/// The variable that names the `codex` program; unset or empty is `codex`
/// on `PATH`.
pub const CODEX_PROGRAM_ENV: &str = "TUNE_CODEX";
/// Given to a `command` agent: the absolute path of the file that holds the
/// prompt.
pub const PROMPT_FILE_ENV: &str = "TUNE_PROMPT_FILE";
/// Given to a `command` agent: the absolute path of the task's worktree.
pub const WORKDIR_ENV: &str = "TUNE_WORKDIR";
/// Given to a `command` agent: where it may write what it used, as one
/// JSON object with the members of `usage` in `run.json`.
pub const USAGE_FILE_ENV: &str = "TUNE_USAGE_FILE";
/// Given to a `command` agent when the run asks for a model.
pub const MODEL_ENV: &str = "TUNE_MODEL";

/// The agent's standard output, in the trial's log directory.
pub const STDOUT_FILE: &str = "agent.out";
/// The agent's standard error, in the trial's log directory; the adapter
/// appends lines that start with `tune:` after the agent ends.
pub const STDERR_FILE: &str = "agent.err";
/// The prompt as the agent was given it, in the trial's log directory.
pub const PROMPT_FILE: &str = "prompt.md";
/// What a `command` agent may write about its use, in the trial's log
/// directory.
pub const USAGE_FILE: &str = "usage.json";

/// Variables taken out of the environment of setup and check commands,
/// and never passed to an agent, because they would tie the command to the
/// session that started the run instead of letting it start as a
/// session of its own:
///
/// - memprove's client Session and agent declaration, under the current
///   and the earlier (`CONTINUITY_`) names: with them, the agent's hook
///   would continue the caller's Session or begin one as the caller's
///   agent;
/// - what Claude Code sets for the commands a session runs: with
///   `CLAUDECODE` and the session's id a new `claude` runs as that
///   session's child; in a Claude Code cloud container,
///   `CLAUDE_CODE_REMOTE_SESSION_ID` makes it take the caller's session id
///   once `CLAUDE_CODE_CHILD_SESSION` is gone; and the effort, invoked
///   skills, task list, IDE port, entrypoint and trace parent are the
///   caller's, not the trial's;
/// - Codex's thread id, set for the commands a Codex thread runs;
/// - this module's own per-trial variables, in case a run is started
///   inside a `command` agent of another run.
///
/// An agent gets less still: only what [`passes`] keeps.
pub const REMOVED_ENV: &[&str] = &[
    "MEMPROVE_CLIENT_SESSION",
    "MEMPROVE_AGENT_NAME",
    "MEMPROVE_AGENT_ROLE",
    "MEMPROVE_PARENT_SESSION",
    "CONTINUITY_CLIENT_SESSION",
    "CONTINUITY_AGENT_NAME",
    "CONTINUITY_AGENT_ROLE",
    "CONTINUITY_PARENT_SESSION",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_REMOTE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_INVOKED_SKILLS",
    "CLAUDE_CODE_TASK_LIST_ID",
    "CLAUDE_CODE_AGENT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
    "TRACEPARENT",
    "CODEX_THREAD_ID",
    PROMPT_FILE_ENV,
    WORKDIR_ENV,
    USAGE_FILE_ENV,
    MODEL_ENV,
];

/// The variables an agent inherits from the run's own
/// environment, besides those named by a prefix of [`PASSED_ENV_PREFIXES`]
/// and the run's `--pass-env` names; nothing else reaches it. Whatever
/// else the caller has (its client session's sockets and tokens, its
/// identity, `SSH_AUTH_SOCK`, `GITHUB_TOKEN`, memprove's data directory
/// and token) stays with the caller. Kept: what a shell and a toolchain
/// need (the user, home, locale, temporary directory, proxies and
/// certificates, the usual compiler and package manager directories), and
/// what the agent logs in with (`CLAUDE_CONFIG_DIR`,
/// `CLAUDE_CODE_OAUTH_TOKEN`, `CODEX_HOME`, and the `ANTHROPIC_`,
/// `OPENAI_`, `AWS_` and Vertex variables).
pub const PASSED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "COLORTERM",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CODEX_HOME",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "CLOUD_ML_REGION",
    "IS_SANDBOX",
    "LIBCLANG_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "LD_LIBRARY_PATH",
    "PKG_CONFIG_PATH",
    "CC",
    "CXX",
    "JAVA_HOME",
    "GOPATH",
    "GOROOT",
    "GOCACHE",
    "GOMODCACHE",
    "GOFLAGS",
    "GOPROXY",
    "NVM_DIR",
    "PNPM_HOME",
    "BUN_INSTALL",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "PYENV_ROOT",
    "HOMEBREW_PREFIX",
    "HOMEBREW_CELLAR",
    "HOMEBREW_REPOSITORY",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
];

/// The prefixes of the variables an agent inherits besides
/// [`PASSED_ENV`]: locale and XDG directories, the model providers' own
/// variables, and Rust's and npm's settings.
pub const PASSED_ENV_PREFIXES: &[&str] = &[
    "LC_",
    "XDG_",
    "ANTHROPIC_",
    "OPENAI_",
    "AWS_",
    "VERTEX_REGION_",
    "CARGO_",
    "RUSTUP_",
    "npm_config_",
];

/// Whether an agent inherits the variable `name`: one of
/// [`PASSED_ENV`], one with a prefix of [`PASSED_ENV_PREFIXES`], or one of
/// `pass_env` (the run's `--pass-env` names); never one of
/// [`REMOVED_ENV`].
#[must_use]
pub fn passes(name: &str, pass_env: &[String]) -> bool {
    !REMOVED_ENV.contains(&name)
        && (PASSED_ENV.contains(&name)
            || PASSED_ENV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
            || pass_env.iter().any(|passed| passed == name))
}

/// Refuses a `--pass-env` name that is empty, holds `=` or a NUL,
/// or is one of [`REMOVED_ENV`] (it would tie the agent to the session
/// running the evaluation).
///
/// # Errors
/// [`EvalError::Usage`] naming the first such name.
pub fn check_pass_env(names: &[String]) -> Result<(), EvalError> {
    for name in names {
        if name.is_empty() || name.contains(['=', '\0']) {
            return Err(EvalError::Usage(format!(
                "--pass-env {name:?} is not the name of a variable"
            )));
        }
        if REMOVED_ENV.contains(&name.as_str()) {
            return Err(EvalError::Usage(format!(
                "--pass-env {name}: tune never passes it to an agent, since it would \
                 tie the agent to the session running the evaluation"
            )));
        }
    }
    Ok(())
}

/// The variables of this process's environment an agent inherits
/// ([`passes`]); a name or value that is not UTF-8 is left out.
#[must_use]
pub fn inherited_env(pass_env: &[String]) -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| passes(name, pass_env))
        .collect()
}

/// How long `<program> --version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

/// The programs the `claude-code` and `codex` adapters start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programs {
    /// `claude`: [`CLAUDE_PROGRAM_ENV`], else `claude` on `PATH`.
    pub claude: String,
    /// `codex`: [`CODEX_PROGRAM_ENV`], else `codex` on `PATH`.
    pub codex: String,
}

impl Programs {
    /// The programs this process's environment names.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The programs `lookup` names, a variable at a time; an empty value
    /// is unset.
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let program = |variable: &str, default: &str| {
            lookup(variable)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        Self {
            claude: program(CLAUDE_PROGRAM_ENV, "claude"),
            codex: program(CODEX_PROGRAM_ENV, "codex"),
        }
    }
}

/// The adapter for `spec`, starting the programs this process's
/// environment names.
///
/// # Errors
/// [`EvalError::Usage`] when `spec` cannot be run (a `command` agent
/// without a command).
pub fn agent_for(spec: &AgentSpec) -> Result<Box<dyn Agent>, EvalError> {
    agent_with(spec, &Programs::from_env())
}

/// The adapter for `spec`, starting `programs`.
///
/// # Errors
/// [`EvalError::Usage`] when `spec` cannot be run: a `command` agent
/// without a command, or a command given to another kind.
pub fn agent_with(spec: &AgentSpec, programs: &Programs) -> Result<Box<dyn Agent>, EvalError> {
    if spec.kind != AgentKind::Command && !spec.command.is_empty() {
        return Err(EvalError::Usage(format!(
            "a program after `--` is only for `--agent command`, not `--agent {}`",
            spec.kind.name()
        )));
    }
    Ok(match spec.kind {
        AgentKind::ClaudeCode => Box::new(claude::ClaudeCode::new(spec, &programs.claude)),
        AgentKind::Codex => Box::new(codex::Codex::new(spec, &programs.codex)),
        AgentKind::Command => Box::new(command::CommandAgent::new(spec)?),
    })
}

/// The first non-empty line `<program> <args>` prints, when it exits with
/// 0 within [`VERSION_TIMEOUT`].
fn version_line(program: &str, args: &[&str]) -> Option<String> {
    let mut process = Process::new(program);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    process.env_clear().envs(inherited_env(&[]));
    let mut child = process.spawn().ok()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < VERSION_TIMEOUT => {
                thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    if !status.success() {
        return None;
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// One program to start for a trial.
struct Launch<'a> {
    /// What the adapter is called in messages, such as `claude`.
    name: &'a str,
    /// The variable that names the program, for the hint when it is
    /// missing.
    program_env: Option<&'a str>,
    /// Program and arguments.
    argv: Vec<String>,
    /// Variables added to the environment.
    env: Vec<(String, String)>,
}

/// Starts `what` for `request` and waits for it; returns how it ended and
/// how long it ran, in milliseconds.
fn launch(request: &AgentRequest<'_>, what: &Launch<'_>) -> (AgentEnd, u64) {
    let started = Instant::now();
    let env: Vec<(String, String)> = inherited_env(request.pass_env)
        .into_iter()
        .chain(request.env.iter().cloned())
        .chain(what.env.iter().cloned())
        .collect();
    let invocation = Invocation {
        argv: &what.argv,
        cwd: request.workdir,
        env: &env,
        env_remove: &[],
        clear_env: true,
        stdout: request.log_dir.join(STDOUT_FILE),
        stderr: request.log_dir.join(STDERR_FILE),
        timeout: request.timeout,
    };
    match process::run(&invocation) {
        Ok(finished) => (end_of(finished.exit), millis(finished.duration)),
        Err(error) => (
            AgentEnd::FailedToStart(start_failure(what, &error)),
            millis(started.elapsed()),
        ),
    }
}

/// The sentence for a program that could not be started.
fn start_failure(launch: &Launch<'_>, error: &EvalError) -> String {
    match (error, launch.program_env) {
        (EvalError::Spawn { source, .. }, Some(variable))
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            format!(
                "{error}; install {} or set {variable} to its path",
                launch.name
            )
        }
        _ => error.to_string(),
    }
}

/// The [`AgentEnd`] for how the program ended.
const fn end_of(exit: Exit) -> AgentEnd {
    match exit {
        Exit::Code(code) => AgentEnd::Exited(code),
        Exit::Signal(signal) => AgentEnd::Signaled(signal),
        Exit::TimedOut => AgentEnd::TimedOut,
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Creates the log directory and writes the prompt file; returns its
/// absolute path.
fn write_prompt(request: &AgentRequest<'_>) -> Result<PathBuf, String> {
    fs::create_dir_all(request.log_dir)
        .map_err(|error| EvalError::io(request.log_dir, error).to_string())?;
    let path = absolute(&request.log_dir.join(PROMPT_FILE));
    fs::write(&path, request.prompt).map_err(|error| EvalError::io(&path, error).to_string())?;
    Ok(path)
}

/// `path` made absolute against the current directory, since the agent
/// runs in another one.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The agent's standard output as text; empty when it wrote none.
fn read_stdout(request: &AgentRequest<'_>) -> String {
    fs::read(request.log_dir.join(STDOUT_FILE))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Appends `line` (prefixed `tune: `) to the trial's `agent.err`.
fn note(request: &AgentRequest<'_>, line: &str) {
    let path = request.log_dir.join(STDERR_FILE);
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "tune: {line}");
    }
}

/// A non-negative integer member of `object`.
fn count(object: Option<&crate::json::JsonValue>, member: &str) -> Option<u64> {
    object?
        .get(member)?
        .as_i64()
        .and_then(|number| u64::try_from(number).ok())
}

/// At most `limit` characters of the first line of `text`, for notes.
fn first_line(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= limit {
        line.to_owned()
    } else {
        let mut short: String = line.chars().take(limit).collect();
        short.push('…');
        short
    }
}

/// An [`AgentOutcome`] for a trial whose program was not started.
fn not_started(detail: String, duration_ms: u64) -> AgentOutcome {
    AgentOutcome {
        end: AgentEnd::FailedToStart(detail),
        duration_ms,
        usage: crate::Usage::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Usage;

    fn spec(kind: AgentKind, command: &[&str]) -> AgentSpec {
        AgentSpec {
            kind,
            model: None,
            command: command.iter().map(|arg| (*arg).to_owned()).collect(),
            extra_args: Vec::new(),
            pass_env: Vec::new(),
        }
    }

    /// What an agent inherits.
    #[test]
    fn an_agent_inherits_the_listed_variables_only() {
        let none: &[String] = &[];
        for name in [
            "PATH",
            "HOME",
            "LC_ALL",
            "ANTHROPIC_API_KEY",
            "CODEX_HOME",
            "CARGO_HOME",
        ] {
            assert!(passes(name, none), "{name}");
        }
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_SESSION_INGRESS_TOKEN_FILE",
            "CLAUDE_CODE_USER_EMAIL",
            "CLAUDE_CODE_ACCOUNT_UUID",
            "CLAUDE_CODE_REMOTE",
            "MEMPROVE_DATA_DIR",
            "MEMPROVE_TOKEN",
            "SSH_AUTH_SOCK",
            "GITHUB_TOKEN",
            "PWD",
        ] {
            assert!(!passes(name, none), "{name}");
        }
        let extra = vec!["PYTHONPATH".to_owned()];
        assert!(passes("PYTHONPATH", &extra));
        assert!(check_pass_env(&extra).is_ok());
        for bad in ["", "A=B", "CLAUDECODE", "MEMPROVE_CLIENT_SESSION"] {
            assert!(check_pass_env(&[bad.to_owned()]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn programs_come_from_the_variables_and_empty_is_unset() {
        let programs = Programs::from_lookup(|name| match name {
            CLAUDE_PROGRAM_ENV => Some("/opt/fake/claude".to_owned()),
            CODEX_PROGRAM_ENV => Some(String::new()),
            _ => None,
        });
        assert_eq!(programs.claude, "/opt/fake/claude");
        assert_eq!(programs.codex, "codex");
        assert_eq!(
            Programs::from_lookup(|_| None),
            Programs {
                claude: "claude".to_owned(),
                codex: "codex".to_owned()
            }
        );
    }

    #[test]
    fn a_command_is_required_for_command_and_refused_for_the_others() {
        let programs = Programs::from_lookup(|_| None);
        let error = agent_with(&spec(AgentKind::Command, &[]), &programs)
            .err()
            .expect("no command");
        assert!(matches!(error, EvalError::Usage(_)), "{error}");
        let error = agent_with(&spec(AgentKind::ClaudeCode, &["x"]), &programs)
            .err()
            .expect("command for claude-code");
        assert_eq!(
            error.to_string(),
            "a program after `--` is only for `--agent command`, not `--agent claude-code`"
        );
        assert!(agent_with(&spec(AgentKind::Codex, &[]), &programs).is_ok());
        assert!(agent_with(&spec(AgentKind::Command, &["sh"]), &programs).is_ok());
    }

    #[test]
    fn exits_map_to_agent_ends() {
        assert_eq!(end_of(Exit::Code(3)), AgentEnd::Exited(3));
        assert_eq!(end_of(Exit::Signal(9)), AgentEnd::Signaled(9));
        assert_eq!(end_of(Exit::TimedOut), AgentEnd::TimedOut);
    }

    #[test]
    fn a_missing_program_names_its_variable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let request = AgentRequest {
            workdir: dir.path(),
            prompt: "p",
            timeout: Duration::from_secs(5),
            log_dir: dir.path(),
            env: &[],
            pass_env: &[],
        };
        let (end, _) = launch(
            &request,
            &Launch {
                name: "claude",
                program_env: Some(CLAUDE_PROGRAM_ENV),
                argv: vec!["/nonexistent/tune-claude".to_owned()],
                env: Vec::new(),
            },
        );
        let AgentEnd::FailedToStart(detail) = end else {
            panic!("started: {end:?}");
        };
        assert!(
            detail.starts_with("cannot start /nonexistent/tune-claude: ")
                && detail.ends_with("; install claude or set TUNE_CLAUDE to its path"),
            "{detail}"
        );
    }

    #[test]
    fn version_is_the_first_line_of_a_successful_run() {
        assert_eq!(
            version_line("sh", &["-c", "printf '\\n  1.2.3 (x)  \\nmore\\n'"]),
            Some("1.2.3 (x)".to_owned())
        );
        assert_eq!(version_line("sh", &["-c", "echo 1.0; exit 1"]), None);
        assert_eq!(version_line("/nonexistent/tune-x", &[]), None);
    }

    #[test]
    fn first_line_is_cut_at_the_limit() {
        assert_eq!(first_line("abc\ndef", 10), "abc");
        assert_eq!(first_line("abcdef", 3), "abc…");
        assert_eq!(first_line("", 3), "");
    }

    #[test]
    fn not_started_reports_no_usage() {
        let outcome = not_started("x".to_owned(), 5);
        assert_eq!(outcome.usage, Usage::default());
        assert_eq!(outcome.end, AgentEnd::FailedToStart("x".to_owned()));
    }
}
