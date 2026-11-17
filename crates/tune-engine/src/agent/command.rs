//! The `command` adapter: any program, given after `--`.
//!
//! In each argument, `{prompt_file}` becomes the absolute path of the
//! trial's `prompt.md` and `{workdir}` the absolute path of the worktree;
//! the extra arguments follow as they are. The program also gets
//! [`PROMPT_FILE_ENV`], [`WORKDIR_ENV`], [`USAGE_FILE_ENV`] and, when the
//! run asks for a model, [`MODEL_ENV`]. When it writes the usage file (one
//! JSON object with the members of `usage` in `run.json`, every one
//! optional), the adapter reads it; a file that is not such an object is
//! noted in `agent.err` and reports nothing.

use std::{fs, path::Path};

use crate::{
    AgentOutcome, AgentSpec, EvalError, Usage,
    json::{self, Reader},
};

use super::{
    Agent, AgentRequest, Launch, MODEL_ENV, PROMPT_FILE_ENV, USAGE_FILE, USAGE_FILE_ENV,
    WORKDIR_ENV, absolute, launch, not_started, note, write_prompt,
};

/// The members of a usage file.
const USAGE_MEMBERS: &[&str] = &[
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "cost_microusd",
    "turns",
];

/// Runs the program of `AgentSpec::command`.
#[derive(Debug, Clone)]
pub(super) struct CommandAgent {
    command: Vec<String>,
    model: Option<String>,
    extra_args: Vec<String>,
}

impl CommandAgent {
    pub(super) fn new(spec: &AgentSpec) -> Result<Self, EvalError> {
        if spec.command.is_empty() {
            return Err(EvalError::Usage(
                "`--agent command` needs the program after `--`, as in \
                 `tune ab --agent command -- ./my-agent {prompt_file}`"
                    .to_owned(),
            ));
        }
        Ok(Self {
            command: spec.command.clone(),
            model: spec.model.clone(),
            extra_args: spec.extra_args.clone(),
        })
    }

    fn argv(&self, prompt_file: &str, workdir: &str) -> Vec<String> {
        self.command
            .iter()
            .map(|arg| {
                arg.replace("{prompt_file}", prompt_file)
                    .replace("{workdir}", workdir)
            })
            .chain(self.extra_args.iter().cloned())
            .collect()
    }
}

impl Agent for CommandAgent {
    fn version(&self) -> Option<String> {
        None
    }

    fn run(&self, request: &AgentRequest<'_>) -> AgentOutcome {
        let prompt_file = match write_prompt(request) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(detail) => return not_started(detail, 0),
        };
        let workdir = absolute(request.workdir).to_string_lossy().into_owned();
        let usage_file = absolute(&request.log_dir.join(USAGE_FILE));
        let _ = fs::remove_file(&usage_file);
        let mut env = vec![
            (PROMPT_FILE_ENV.to_owned(), prompt_file.clone()),
            (WORKDIR_ENV.to_owned(), workdir.clone()),
            (
                USAGE_FILE_ENV.to_owned(),
                usage_file.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(model) = &self.model {
            env.push((MODEL_ENV.to_owned(), model.clone()));
        }
        let (end, duration_ms) = launch(
            request,
            &Launch {
                name: "the command",
                program_env: None,
                argv: self.argv(&prompt_file, &workdir),
                env,
            },
        );
        let usage = match read_usage(&usage_file) {
            Ok(usage) => usage.unwrap_or_default(),
            Err(error) => {
                note(request, &format!("ignored the usage file: {error}"));
                Usage::default()
            }
        };
        AgentOutcome {
            end,
            duration_ms,
            usage,
        }
    }
}

/// The usage file at `path`; `None` when the program wrote none.
fn read_usage(path: &Path) -> Result<Option<Usage>, EvalError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(EvalError::io(path, error)),
    };
    let value = json::parse(&text, Some(path))?;
    let reader = Reader::new(&value, Some(path), "")?;
    reader.deny_unknown(USAGE_MEMBERS)?;
    Ok(Some(Usage {
        input_tokens: reader.opt_u64("input_tokens")?,
        output_tokens: reader.opt_u64("output_tokens")?,
        cache_read_tokens: reader.opt_u64("cache_read_tokens")?,
        cache_write_tokens: reader.opt_u64("cache_write_tokens")?,
        cost_microusd: reader.opt_u64("cost_microusd")?,
        turns: reader.opt_u64("turns")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &[&str], extra_args: &[&str]) -> AgentSpec {
        AgentSpec {
            kind: crate::AgentKind::Command,
            model: None,
            command: command.iter().map(|arg| (*arg).to_owned()).collect(),
            extra_args: extra_args.iter().map(|arg| (*arg).to_owned()).collect(),
            pass_env: Vec::new(),
        }
    }

    #[test]
    fn placeholders_are_replaced_inside_arguments_of_the_command_only() {
        let agent = CommandAgent::new(&spec(
            &["./agent", "--prompt={prompt_file}", "{workdir}", "{other}"],
            &["{workdir}"],
        ))
        .expect("command");
        assert_eq!(
            agent.argv("/l/prompt.md", "/w"),
            [
                "./agent",
                "--prompt=/l/prompt.md",
                "/w",
                "{other}",
                "{workdir}"
            ]
        );
    }

    #[test]
    fn an_empty_command_is_a_usage_error() {
        let error = CommandAgent::new(&spec(&[], &[])).expect_err("empty");
        assert!(matches!(error, EvalError::Usage(_)));
        assert!(error.to_string().contains("after `--`"), "{error}");
    }

    #[test]
    fn the_usage_file_is_optional_and_strict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("usage.json");
        assert_eq!(read_usage(&path).expect("absent"), None);
        fs::write(
            &path,
            r#"{"input_tokens": 5, "cost_microusd": 1200, "turns": null}"#,
        )
        .expect("write");
        assert_eq!(
            read_usage(&path).expect("valid"),
            Some(Usage {
                input_tokens: Some(5),
                cost_microusd: Some(1200),
                ..Usage::default()
            })
        );
        fs::write(&path, r#"{"cost_usd": 0.1}"#).expect("write");
        let error = read_usage(&path).expect_err("unknown member").to_string();
        assert!(
            error.ends_with("`cost_usd` is not a known member"),
            "{error}"
        );
        fs::write(&path, r#"{"output_tokens": -1}"#).expect("write");
        assert!(read_usage(&path).is_err());
        fs::write(&path, "[]").expect("write");
        assert!(read_usage(&path).is_err());
    }
}
