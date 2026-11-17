//! The `claude-code` adapter: `claude -p` with its result as JSON.
//!
//! The command line is
//! `claude -p --output-format json [--model M] --permission-mode
//! bypassPermissions --no-session-persistence <extra args> -- <prompt>`
//! (flags as `claude --help` of Claude Code 2.1.325 states them): no
//! permission prompt can stop an unattended trial, and the trial's session
//! is not saved where `claude --resume` would list it. The usage comes from
//! the result object on standard output: `total_cost_usd`, `num_turns` and
//! `usage.{input,output,cache_read_input,cache_creation_input}_tokens`.

use crate::{
    AgentOutcome, AgentSpec, Usage,
    json::{self, JsonValue},
};

use super::{
    Agent, AgentRequest, CLAUDE_PROGRAM_ENV, Launch, count, first_line, launch, not_started, note,
    read_stdout, version_line, write_prompt,
};

/// Runs Claude Code's `claude -p`.
#[derive(Debug, Clone)]
pub(super) struct ClaudeCode {
    program: String,
    model: Option<String>,
    extra_args: Vec<String>,
}

impl ClaudeCode {
    pub(super) fn new(spec: &AgentSpec, program: &str) -> Self {
        Self {
            program: program.to_owned(),
            model: spec.model.clone(),
            extra_args: spec.extra_args.clone(),
        }
    }

    fn argv(&self, prompt: &str) -> Vec<String> {
        let mut argv = vec![
            self.program.clone(),
            "-p".to_owned(),
            "--output-format".to_owned(),
            "json".to_owned(),
        ];
        if let Some(model) = &self.model {
            argv.extend(["--model".to_owned(), model.clone()]);
        }
        argv.extend(
            [
                "--permission-mode",
                "bypassPermissions",
                "--no-session-persistence",
            ]
            .map(str::to_owned),
        );
        argv.extend(self.extra_args.iter().cloned());
        argv.extend(["--".to_owned(), prompt.to_owned()]);
        argv
    }
}

impl Agent for ClaudeCode {
    fn version(&self) -> Option<String> {
        version_line(&self.program, &["--version"])
    }

    fn run(&self, request: &AgentRequest<'_>) -> AgentOutcome {
        if let Err(detail) = write_prompt(request) {
            return not_started(detail, 0);
        }
        let (end, duration_ms) = launch(
            request,
            &Launch {
                name: "claude",
                program_env: Some(CLAUDE_PROGRAM_ENV),
                argv: self.argv(request.prompt),
                env: Vec::new(),
            },
        );
        let report = parse(&read_stdout(request));
        if let Some(error) = &report.error {
            note(request, error);
        }
        AgentOutcome {
            end,
            duration_ms,
            usage: report.usage,
        }
    }
}

/// What Claude Code's result said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Report {
    /// Its usage; all `None` without a result object.
    pub(super) usage: Usage,
    /// A sentence when the result has `is_error: true`.
    pub(super) error: Option<String>,
}

/// Reads the result object from `stdout`: one object (`--output-format
/// json`), an array of messages (`json` with `--verbose`), or one message
/// per line (`stream-json`). Anything else reports nothing.
pub(super) fn parse(stdout: &str) -> Report {
    let Some(result) = result_object(stdout) else {
        return Report::default();
    };
    let usage = result.get("usage");
    let usage = Usage {
        input_tokens: count(usage, "input_tokens"),
        output_tokens: count(usage, "output_tokens"),
        cache_read_tokens: count(usage, "cache_read_input_tokens"),
        cache_write_tokens: count(usage, "cache_creation_input_tokens"),
        cost_microusd: match result.get("total_cost_usd") {
            Some(JsonValue::Number(text)) => microusd(text),
            _ => None,
        },
        turns: count(Some(&result), "num_turns"),
    };
    let error = (result.get("is_error").and_then(JsonValue::as_bool) == Some(true)).then(|| {
        let subtype = result
            .get("subtype")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown");
        match result.get("result").and_then(JsonValue::as_str) {
            Some(text) if !text.trim().is_empty() => format!(
                "claude reported an error result (subtype {subtype}): {}",
                first_line(text, 300)
            ),
            _ => format!("claude reported an error result (subtype {subtype})"),
        }
    });
    Report { usage, error }
}

fn is_result(value: &JsonValue) -> bool {
    value.get("type").and_then(JsonValue::as_str) == Some("result")
}

fn result_object(stdout: &str) -> Option<JsonValue> {
    let text = stdout.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(value) = json::parse(text, None) {
        return match value {
            JsonValue::Array(items) => items.into_iter().rev().find(is_result),
            other => Some(other).filter(is_result),
        };
    }
    text.lines()
        .rev()
        .filter_map(|line| json::parse(line.trim(), None).ok())
        .find(is_result)
}

/// A JSON number of US dollars (`0.0482`, `1.2e-05`) in millionths,
/// rounded half up, without floating point. `None` when it is negative or
/// does not fit.
pub(super) fn microusd(text: &str) -> Option<u64> {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], text[at + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let is_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || !is_digits(whole) || !is_digits(fraction) {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    // The value is `digits * 10^shift` millionths.
    let fraction_len = i32::try_from(fraction.len()).ok()?;
    let shift = exponent.checked_add(6)?.checked_sub(fraction_len)?;
    if shift >= 0 {
        let zeros = usize::try_from(shift).ok()?;
        if digits.len() + zeros > 20 {
            return None;
        }
        return format!("{digits}{}", "0".repeat(zeros)).parse().ok();
    }
    let dropped = usize::try_from(shift.unsigned_abs()).ok()?;
    if dropped > digits.len() {
        return Some(0);
    }
    let (kept, rest) = digits.split_at(digits.len() - dropped);
    let kept: u64 = if kept.is_empty() {
        0
    } else {
        kept.parse().ok()?
    };
    let round_up = rest.as_bytes().first().is_some_and(|digit| *digit >= b'5');
    kept.checked_add(u64::from(round_up))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_ends_options_before_the_prompt() {
        let spec = AgentSpec {
            kind: crate::AgentKind::ClaudeCode,
            model: Some("sonnet".to_owned()),
            command: Vec::new(),
            extra_args: vec!["--max-turns".to_owned(), "30".to_owned()],
            pass_env: Vec::new(),
        };
        let agent = ClaudeCode::new(&spec, "/bin/claude");
        assert_eq!(
            agent.argv("--fix it"),
            [
                "/bin/claude",
                "-p",
                "--output-format",
                "json",
                "--model",
                "sonnet",
                "--permission-mode",
                "bypassPermissions",
                "--no-session-persistence",
                "--max-turns",
                "30",
                "--",
                "--fix it"
            ]
        );
    }

    #[test]
    fn dollars_become_millionths_without_floating_point() {
        for (text, expected) in [
            ("0", Some(0)),
            ("0.0", Some(0)),
            ("1", Some(1_000_000)),
            ("0.04821375", Some(48_214)),
            ("0.048213749", Some(48_214)),
            ("0.0482134", Some(48_213)),
            ("12.5", Some(12_500_000)),
            ("1.2e-05", Some(12)),
            ("5e-7", Some(1)),
            ("4.9e-7", Some(0)),
            ("1e-30", Some(0)),
            ("1E2", Some(100_000_000)),
            ("2.5e+1", Some(25_000_000)),
            ("0000.5", Some(500_000)),
            ("1e30", None),
            ("-0.1", None),
            (".5", None),
            ("1.", Some(1_000_000)),
            ("1e", None),
            ("abc", None),
            ("", None),
        ] {
            assert_eq!(microusd(text), expected, "{text}");
        }
    }

    const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":4,"result":"Done.","total_cost_usd":0.0731625,"usage":{"input_tokens":12,"cache_creation_input_tokens":11405,"cache_read_input_tokens":40211,"output_tokens":388}}"#;

    #[test]
    fn reads_the_usage_of_a_result_object() {
        let report = parse(RESULT);
        assert_eq!(
            report.usage,
            Usage {
                input_tokens: Some(12),
                output_tokens: Some(388),
                cache_read_tokens: Some(40_211),
                cache_write_tokens: Some(11_405),
                cost_microusd: Some(73_163),
                turns: Some(4),
            }
        );
        assert_eq!(report.error, None);
    }

    #[test]
    fn finds_the_result_in_an_array_or_a_stream() {
        let array = format!(r#"[{{"type":"system"}},{RESULT}]"#);
        assert_eq!(parse(&array).usage.turns, Some(4));
        let stream = format!("{{\"type\":\"system\"}}\nnot json\n{RESULT}\n");
        assert_eq!(parse(&stream).usage.turns, Some(4));
    }

    #[test]
    fn other_output_reports_nothing() {
        for stdout in [
            "",
            "Done, I created ok.txt.",
            r#"{"type":"assistant"}"#,
            "[1, 2]",
        ] {
            assert_eq!(parse(stdout), Report::default(), "{stdout}");
        }
        let partial = parse(r#"{"type":"result","num_turns":2,"total_cost_usd":"0.1"}"#);
        assert_eq!(
            partial.usage,
            Usage {
                turns: Some(2),
                ..Usage::default()
            }
        );
    }

    #[test]
    fn an_error_result_is_a_sentence() {
        let report =
            parse(r#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":2}"#);
        assert_eq!(
            report.error.as_deref(),
            Some("claude reported an error result (subtype error_max_turns)")
        );
        let report = parse(
            r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 529\nmore"}"#,
        );
        assert_eq!(
            report.error.as_deref(),
            Some("claude reported an error result (subtype success): API Error: 529")
        );
    }
}
