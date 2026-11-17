//! The command line: which command, its options, and the help texts.
//! Parsed by hand; there is no option a person has to guess.

use std::{io::Write, path::Path};

use tune_engine::{AgentKind, AgentSpec};

use crate::{
    apply,
    error::Error,
    experiment::{self, Options},
    scan,
    variant::{Change, Variant},
};

/// What `tune --help` prints.
pub(crate) const HELP: &str = "\
tune tests whether the instructions you give a coding agent earn their place.
It runs the agent on your repository's own tasks with and without a part of
the configuration, and only your tests decide.

Usage:
  tune scan [--json]
      List the units of the configuration (the sections of CLAUDE.md and
      AGENTS.md, the files under .claude/rules, the skills under
      .claude/skills), what each costs in tokens, and what looks wrong.
      Runs nothing and calls no model.

  tune ab (--drop UNIT | --replace PATH=FILE | --add PATH=FILE | --bare
           | --variant NAME:CHANGE[,CHANGE...])... [options]
      Run the agent on the task files with today's configuration and with
      each changed one, in turns, and say whether the difference is real.
      See `tune ab --help`.

  tune --help | --version
";

/// What `tune ab --help` prints.
pub(crate) const AB_HELP: &str = "\
tune ab runs the agent on your task files under today's configuration (the
baseline), under the same configuration a second time (the control, which
shows how much two runs differ by chance), and under each variant. The arms
take turns, every trial starts from a clean copy of the repository, and only
the task's own check decides pass or fail.

Variants (each option makes one):
  --drop UNIT              without this unit (`tune scan` lists the units)
  --replace PATH=FILE      with the configuration file PATH holding FILE's text
  --add PATH=FILE          with a new configuration file PATH
  --bare                   without any configuration
  --variant NAME:CHANGE[,CHANGE...]
                           several changes as one variant; a CHANGE is
                           drop=UNIT, replace=PATH=FILE, add=PATH=FILE or bare

Options:
  --tasks PATH             a task file or a directory of them; may repeat
                           (default: .tune/tasks, else evals)
  --rounds N               times each task runs under each arm (default 3)
  --agent claude-code      the agent (the default and, for now, the only one)
  --agent-cmd COMMAND      a shell command to run as the agent instead; it
                           finds the prompt in $TUNE_PROMPT_FILE
  --model MODEL            the model the agent is asked to use
  --agent-arg ARG          one more argument for the agent; may repeat
  --pass-env NAME          let the agent see this environment variable
  --max-cost USD           stop before the spending passes this; needed
                           unless the agent is a command
  --cost-per-trial USD     your estimate, for the plan shown before the run
  --jobs N                 trials at the same time (default 1)
  --no-control             leave the control out
  --yes                    run without asking
  --dry-run                show the plan and stop
  --json                   print the report as JSON
  --keep-worktrees         keep each trial's working copy

Running the same command again continues the experiment: finished trials
are not run twice, and a larger --rounds adds rounds.

The verdicts: a variant is WORSE or BETTER when the tasks that changed
sides are too one-sided to be chance (exact sign test, 5%) and more than
the control's; NO WORSE when its pass rate is at most 10 points below the
baseline's with 95% confidence, which takes about 30 pairs that agree;
otherwise NOT ENOUGH EVIDENCE. For a variant that only drops units these
read KEEP, REMOVABLE and NOT ENOUGH EVIDENCE.
";

/// The arguments Claude Code is given so that a trial reads the
/// repository's configuration and nothing of the person running it.
const CLAUDE_ISOLATION: [&str; 3] = ["--setting-sources", "project,local", "--strict-mcp-config"];

/// Dollars as written (`4`, `0.5`, `$12.34`) in millionths of a dollar.
pub(crate) fn microusd(text: &str) -> Option<u64> {
    let text = text.strip_prefix('$').unwrap_or(text);
    let (whole, part) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && part.is_empty()
        || part.len() > 6
        || !whole
            .chars()
            .chain(part.chars())
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let part: u64 = format!("{part:0<6}").parse().ok()?;
    whole.checked_mul(1_000_000)?.checked_add(part)
}

/// What `tune ab` does when no option says otherwise.
fn ab_defaults() -> Options {
    Options {
        variants: Vec::new(),
        tasks: Vec::new(),
        rounds: 3,
        agent: AgentSpec {
            kind: AgentKind::ClaudeCode,
            model: None,
            command: Vec::new(),
            extra_args: Vec::new(),
            pass_env: Vec::new(),
        },
        agent_label: String::new(),
        max_cost: None,
        cost_per_trial: None,
        jobs: 1,
        control: true,
        yes: false,
        dry_run: false,
        json: false,
        keep_worktrees: false,
    }
}

/// Refuses what cannot be run and fills in what the agent needs.
fn ab_checked(options: &mut Options, agent_args: Vec<String>) -> Result<(), Error> {
    if options.variants.is_empty() {
        return Err(Error::Usage(
            "nothing to compare: name a variant with --drop, --replace, --add, --bare or --variant"
                .to_owned(),
        ));
    }
    if options.agent.kind == AgentKind::ClaudeCode {
        if options.max_cost.is_none() && !options.dry_run {
            return Err(Error::Usage(
                "set --max-cost USD: the most this run may spend on the agent".to_owned(),
            ));
        }
        options.agent.extra_args = CLAUDE_ISOLATION
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect();
        options.agent_label = match &options.agent.model {
            Some(model) => format!("claude-code, model {model}"),
            None => "claude-code".to_owned(),
        };
    }
    options.agent.extra_args.extend(agent_args);
    tune_engine::agent::check_pass_env(&options.agent.pass_env)?;
    Ok(())
}

fn ab_command(args: &[String], cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let mut options = ab_defaults();
    let mut agent_args: Vec<String> = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .ok_or_else(|| Error::Usage(format!("`{arg}` needs a value")))
        };
        let number = |text: String| {
            text.parse::<u32>()
                .ok()
                .filter(|number| *number > 0)
                .ok_or_else(|| Error::Usage(format!("`{arg}` needs a whole number above 0")))
        };
        let money = |text: String| {
            microusd(&text).ok_or_else(|| {
                Error::Usage(format!(
                    "`{arg}` needs an amount in dollars, like 4 or 0.50"
                ))
            })
        };
        match arg.as_str() {
            "--help" | "-h" => return print(out, AB_HELP),
            "--drop" => options.variants.push(Variant::of(Change::Drop(value()?))),
            "--replace" => {
                let change = Change::parse(&format!("replace={}", value()?), cwd)?;
                options.variants.push(Variant::of(change));
            }
            "--add" => {
                let change = Change::parse(&format!("add={}", value()?), cwd)?;
                options.variants.push(Variant::of(change));
            }
            "--bare" => options.variants.push(Variant::of(Change::Bare)),
            "--variant" => options.variants.push(Variant::parse(&value()?, cwd)?),
            "--tasks" => options.tasks.push(value()?.into()),
            "--rounds" => options.rounds = number(value()?)?,
            "--jobs" => options.jobs = number(value()?)?,
            "--agent" => {
                let name = value()?;
                if name != AgentKind::ClaudeCode.name() {
                    return Err(Error::Usage(format!(
                        "`--agent {name}`: claude-code is the only agent for now; --agent-cmd runs any command as one"
                    )));
                }
            }
            "--agent-cmd" => {
                let command = value()?;
                options.agent.kind = AgentKind::Command;
                options.agent.command =
                    vec!["/bin/sh".to_owned(), "-c".to_owned(), command.clone()];
                options.agent_label = command;
            }
            "--model" => options.agent.model = Some(value()?),
            "--agent-arg" => agent_args.push(value()?),
            "--pass-env" => options.agent.pass_env.push(value()?),
            "--max-cost" => options.max_cost = Some(money(value()?)?),
            "--cost-per-trial" => options.cost_per_trial = Some(money(value()?)?),
            "--no-control" => options.control = false,
            "--yes" | "-y" => options.yes = true,
            "--dry-run" => options.dry_run = true,
            "--json" => options.json = true,
            "--keep-worktrees" => options.keep_worktrees = true,
            other => {
                return Err(Error::Usage(format!(
                    "`tune ab` has no option `{other}`; `tune ab --help` lists them"
                )));
            }
        }
    }
    ab_checked(&mut options, agent_args)?;
    experiment::run(&options, cwd, out)
}

/// Runs the command `args` (without the program name) in `cwd`, writing
/// what it reports to `out`.
pub(crate) fn run(args: &[String], cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let Some(command) = args.first() else {
        return Err(Error::Usage("no command given".to_owned()));
    };
    let rest = &args[1..];
    match command.as_str() {
        "--help" | "-h" | "help" => print(out, HELP),
        "--version" | "-V" => print(out, &format!("tune {}\n", env!("CARGO_PKG_VERSION"))),
        "scan" => scan_command(rest, cwd, out),
        "ab" => ab_command(rest, cwd, out),
        "apply" => match rest {
            [flag, arm] if flag == "--files" => apply::apply(cwd, Path::new(arm)),
            _ => Err(Error::Usage(
                "`tune apply --files DIR` is the setup command of a trial".to_owned(),
            )),
        },
        other => Err(Error::Usage(format!("unknown command `{other}`"))),
    }
}

/// Writes `text` to `out`. A reader that went away is not an error.
#[allow(
    clippy::unnecessary_wraps,
    reason = "printing is how every command ends, so it returns what they return"
)]
pub(crate) fn print(out: &mut dyn Write, text: &str) -> Result<(), Error> {
    let _ = out.write_all(text.as_bytes());
    Ok(())
}

fn scan_command(args: &[String], cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => return print(out, HELP),
            other => {
                return Err(Error::Usage(format!("`tune scan` has no option `{other}`")));
            }
        }
    }
    let root = tune_engine::git::toplevel(cwd)?;
    let found = scan::scan(&root)?;
    print(
        out,
        &if json {
            scan::json(&found)
        } else {
            scan::text(&found)
        },
    )
}
