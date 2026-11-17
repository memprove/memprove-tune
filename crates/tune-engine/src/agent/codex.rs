//! The `codex` adapter: `codex exec` with its events as JSON Lines.
//!
//! The command line is `codex exec --json [--model M] --sandbox
//! workspace-write --ephemeral -C <workdir> <extra args> -- <prompt>`
//! (flags as `codex-rs/exec/src/cli.rs` and
//! `codex-rs/utils/cli/src/shared_options.rs` of openai/codex state them
//! at `81b995a`, release `rust-v0.169.0`): `exec` never asks for approval,
//! the sandbox lets the agent write its worktree, and the session is not
//! saved. Pass `--agent-arg --dangerously-bypass-approvals-and-sandbox` for
//! a run as unconfined as Claude Code's `bypassPermissions`.
//!
//! Every `turn.completed` event carries the turn's `usage`; the adapter
//! adds them up. Codex counts cached input inside `input_tokens`, so the
//! cached part is taken out of it. Codex reports no cost and no model
//! turns.

use crate::{
    AgentOutcome, AgentSpec, Usage,
    json::{self, JsonValue},
};

use super::{
    Agent, AgentRequest, CODEX_PROGRAM_ENV, Launch, absolute, count, first_line, launch,
    not_started, note, read_stdout, version_line, write_prompt,
};

/// Runs `codex exec`.
#[derive(Debug, Clone)]
pub(super) struct Codex {
    program: String,
    model: Option<String>,
    extra_args: Vec<String>,
}

impl Codex {
    pub(super) fn new(spec: &AgentSpec, program: &str) -> Self {
        Self {
            program: program.to_owned(),
            model: spec.model.clone(),
            extra_args: spec.extra_args.clone(),
        }
    }

    fn argv(&self, workdir: &str, prompt: &str) -> Vec<String> {
        let mut argv = vec![self.program.clone(), "exec".to_owned(), "--json".to_owned()];
        if let Some(model) = &self.model {
            argv.extend(["--model".to_owned(), model.clone()]);
        }
        argv.extend(
            ["--sandbox", "workspace-write", "--ephemeral", "-C", workdir].map(str::to_owned),
        );
        argv.extend(self.extra_args.iter().cloned());
        argv.extend(["--".to_owned(), prompt.to_owned()]);
        argv
    }
}

impl Agent for Codex {
    fn version(&self) -> Option<String> {
        version_line(&self.program, &["--version"])
    }

    fn run(&self, request: &AgentRequest<'_>) -> AgentOutcome {
        if let Err(detail) = write_prompt(request) {
            return not_started(detail, 0);
        }
        let workdir = absolute(request.workdir);
        let (end, duration_ms) = launch(
            request,
            &Launch {
                name: "codex",
                program_env: Some(CODEX_PROGRAM_ENV),
                argv: self.argv(&workdir.to_string_lossy(), request.prompt),
                env: Vec::new(),
            },
        );
        let report = parse(&read_stdout(request));
        for error in &report.errors {
            note(request, error);
        }
        AgentOutcome {
            end,
            duration_ms,
            usage: report.usage,
        }
    }
}

/// What Codex's events said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Report {
    /// The summed usage of every completed turn; all `None` without one.
    pub(super) usage: Usage,
    /// A sentence for each `turn.failed` and `error` event.
    pub(super) errors: Vec<String>,
}

/// Reads the JSON Lines events in `stdout`; lines that are not JSON
/// objects are skipped.
pub(super) fn parse(stdout: &str) -> Report {
    let mut input = None;
    let mut cached = None;
    let mut cache_write = None;
    let mut output = None;
    let mut errors = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(event) = json::parse(line, None) else {
            continue;
        };
        match event.get("type").and_then(JsonValue::as_str) {
            Some("turn.completed") => {
                let usage = event.get("usage");
                add(&mut input, count(usage, "input_tokens"));
                add(&mut cached, count(usage, "cached_input_tokens"));
                add(&mut cache_write, count(usage, "cache_write_input_tokens"));
                add(&mut output, count(usage, "output_tokens"));
            }
            Some("turn.failed") => errors.push(failure(
                "turn.failed",
                event.get("error").and_then(|error| error.get("message")),
            )),
            Some("error") => errors.push(failure("error", event.get("message"))),
            _ => {}
        }
    }
    Report {
        usage: Usage {
            input_tokens: input.map(|input: u64| input.saturating_sub(cached.unwrap_or(0))),
            output_tokens: output,
            cache_read_tokens: cached,
            cache_write_tokens: cache_write,
            cost_microusd: None,
            turns: None,
        },
        errors,
    }
}

fn failure(event: &str, message: Option<&JsonValue>) -> String {
    match message.and_then(JsonValue::as_str) {
        Some(message) if !message.trim().is_empty() => {
            format!("codex reported {event}: {}", first_line(message, 300))
        }
        _ => format!("codex reported {event}"),
    }
}

fn add(sum: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *sum = Some(sum.unwrap_or(0).saturating_add(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_names_the_worktree_and_ends_options() {
        let spec = AgentSpec {
            kind: crate::AgentKind::Codex,
            model: Some("gpt-5.5".to_owned()),
            command: Vec::new(),
            extra_args: vec!["-c".to_owned(), "model_reasoning_effort=high".to_owned()],
            pass_env: Vec::new(),
        };
        let agent = Codex::new(&spec, "codex");
        assert_eq!(
            agent.argv("/w/t1", "review the parser"),
            [
                "codex",
                "exec",
                "--json",
                "--model",
                "gpt-5.5",
                "--sandbox",
                "workspace-write",
                "--ephemeral",
                "-C",
                "/w/t1",
                "-c",
                "model_reasoning_effort=high",
                "--",
                "review the parser"
            ]
        );
    }

    #[test]
    fn sums_completed_turns_and_takes_cache_reads_out_of_input() {
        let stdout = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"turn.started"}"#,
            "\n",
            "Reading additional input from stdin...\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122,"reasoning_output_tokens":0}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":0,"cache_write_input_tokens":7,"output_tokens":8,"reasoning_output_tokens":3}}"#,
            "\n",
            "{broken\n",
        );
        let report = parse(stdout);
        assert_eq!(
            report.usage,
            Usage {
                input_tokens: Some(1315),
                output_tokens: Some(130),
                cache_read_tokens: Some(24_448),
                cache_write_tokens: Some(7),
                cost_microusd: None,
                turns: None,
            }
        );
        assert!(report.errors.is_empty());
    }

    #[test]
    fn no_completed_turn_reports_no_usage_and_failures_are_sentences() {
        let stdout = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"error","message":"stream disconnected"}"#,
            "\n",
            r#"{"type":"turn.failed","error":{"message":"usage limit reached\nsee"}}"#,
            "\n",
            r#"{"type":"turn.failed"}"#,
            "\n",
        );
        let report = parse(stdout);
        assert_eq!(report.usage, Usage::default());
        assert_eq!(
            report.errors,
            [
                "codex reported error: stream disconnected",
                "codex reported turn.failed: usage limit reached",
                "codex reported turn.failed"
            ]
        );
        assert_eq!(parse("not json at all"), Report::default());
    }
}
