//! The command line: which command, its options, and the help texts.
//! Parsed by hand; there is no option a person has to guess.

use std::{io::Write, path::Path};

use tune_engine::{AgentKind, AgentSpec};

use crate::{
    apply,
    error::Error,
    experiment::{self, Options},
    prune, scan, stats,
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

  tune prune [--unit UNIT]... [--top N] [options]
      Drop every unit in turn, find the ones the agent does as well
      without, check them together, and write a patch that removes them
      (.tune/prune.patch) and what was found (.tune/verdicts.json).
      See `tune prune --help`.

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
  --margin POINTS          how many points lower the pass rate may truly be
                           and still count as no worse (default 10)
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
baseline's with 95% confidence, which takes about 30 pairs that agree (14
at --margin 20); otherwise NOT ENOUGH EVIDENCE. For a variant that only
drops units these read KEEP, REMOVABLE and NOT ENOUGH EVIDENCE.
";

/// What `tune prune --help` prints.
pub(crate) const PRUNE_HELP: &str = "\
tune prune finds the units of the configuration that the agent does as well
without. It runs one experiment with today's configuration (the baseline),
the same a second time (the control), and one arm per unit that leaves that
unit out. Then it drops every unit that came out REMOVABLE at once and
checks that configuration against the baseline once more, on tasks the
search did not use when there are enough of them. If the check holds it
writes .tune/prune.patch; `git apply .tune/prune.patch` makes the change,
and nothing is changed before you do that. What was found for each unit is
written to .tune/verdicts.json, and `tune scan` shows whether it still holds.

What to test:
  --unit UNIT              test this unit; may repeat (default: every unit
                           the agent reads; `tune scan` lists them)
  --top N                  test only the N largest units
  --holdout N              tasks kept out of the search for the final check
                           (default: a quarter of them when there are 4 or
                           more, else none; which ones depends only on
                           their names)
  --rounds N               times each task runs under each arm (default: as
                           many as a REMOVABLE verdict needs)

Options:
  --tasks PATH             a task file or a directory of them; may repeat
                           (default: .tune/tasks, else evals)
  --agent claude-code      the agent (the default and, for now, the only one)
  --agent-cmd COMMAND      a shell command to run as the agent instead; it
                           finds the prompt in $TUNE_PROMPT_FILE
  --model MODEL            the model the agent is asked to use
  --agent-arg ARG          one more argument for the agent; may repeat
  --pass-env NAME          let the agent see this environment variable
  --max-cost USD           stop before the spending passes this, the final
                           check included; needed unless the agent is a
                           command
  --cost-per-trial USD     your estimate, for the plan shown before the run
  --jobs N                 trials at the same time (default 1)
  --margin POINTS          how many points lower the pass rate may truly be
                           and still count as no worse (default 10)
  --yes                    run without asking
  --dry-run                show the plan and stop
  --json                   print the report as JSON
  --keep-worktrees         keep each trial's working copy

Running the same command again continues: finished trials are not run
twice. Stopped at the budget, it writes no patch and no verdict; a larger
--max-cost goes on from there.

What a verdict needs: REMOVABLE at a margin of 10 points takes 29 pairs
(the same task in the same round, with and without the unit) that agree;
14 at --margin 20, 59 at --margin 5. The plan says how many pairs this run
gives a unit, and what the cap pays for.

What it cannot tell: each unit is tested alone against today's
configuration, so two units that cover for each other both read REMOVABLE;
the final check is what catches that, and then there is no patch. A unit
cannot show its worth on a task the agent fails with or without it.
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

/// A margin as written, in points of pass rate (`10`, `12.5`), as a share
/// of 1. From 1 to 50 points.
pub(crate) fn margin(text: &str) -> Option<f64> {
    let plain = !text.is_empty() && text.chars().all(|c| c.is_ascii_digit() || c == '.');
    let points: f64 = text.parse().ok().filter(|_| plain)?;
    (1.0..=50.0).contains(&points).then_some(points / 100.0)
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
        margin: stats::MARGIN,
        control: true,
        yes: false,
        dry_run: false,
        json: false,
        keep_worktrees: false,
    }
}

/// Refuses an agent that cannot be run and fills in what it needs.
fn agent_checked(options: &mut Options, agent_args: Vec<String>) -> Result<(), Error> {
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

/// The arguments of a command that are still to read.
type Rest<'a> = std::slice::Iter<'a, String>;

/// The value that follows the option `arg`.
fn value(arg: &str, rest: &mut Rest<'_>) -> Result<String, Error> {
    rest.next()
        .cloned()
        .ok_or_else(|| Error::Usage(format!("`{arg}` needs a value")))
}

/// The whole number above 0 that follows the option `arg`.
fn number(arg: &str, rest: &mut Rest<'_>) -> Result<u32, Error> {
    value(arg, rest)?
        .parse::<u32>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| Error::Usage(format!("`{arg}` needs a whole number above 0")))
}

/// The amount of money that follows the option `arg`.
fn money(arg: &str, rest: &mut Rest<'_>) -> Result<u64, Error> {
    microusd(&value(arg, rest)?).ok_or_else(|| {
        Error::Usage(format!(
            "`{arg}` needs an amount in dollars, like 4 or 0.50"
        ))
    })
}

/// Reads `arg` when it is an option that `tune ab` and `tune prune` share,
/// and says whether it was one.
fn shared_option(
    arg: &str,
    rest: &mut Rest<'_>,
    options: &mut Options,
    agent_args: &mut Vec<String>,
) -> Result<bool, Error> {
    match arg {
        "--tasks" => options.tasks.push(value(arg, rest)?.into()),
        "--jobs" => options.jobs = number(arg, rest)?,
        "--agent" => {
            let name = value(arg, rest)?;
            if name != AgentKind::ClaudeCode.name() {
                return Err(Error::Usage(format!(
                    "`--agent {name}`: claude-code is the only agent for now; --agent-cmd runs any command as one"
                )));
            }
        }
        "--agent-cmd" => {
            let command = value(arg, rest)?;
            options.agent.kind = AgentKind::Command;
            options.agent.command = vec!["/bin/sh".to_owned(), "-c".to_owned(), command.clone()];
            options.agent_label = command;
        }
        "--model" => options.agent.model = Some(value(arg, rest)?),
        "--agent-arg" => agent_args.push(value(arg, rest)?),
        "--pass-env" => options.agent.pass_env.push(value(arg, rest)?),
        "--max-cost" => options.max_cost = Some(money(arg, rest)?),
        "--cost-per-trial" => options.cost_per_trial = Some(money(arg, rest)?),
        "--margin" => {
            options.margin = margin(&value(arg, rest)?).ok_or_else(|| {
                Error::Usage(
                    "`--margin` needs a number of points from 1 to 50, like 10 or 20".to_owned(),
                )
            })?;
        }
        "--yes" | "-y" => options.yes = true,
        "--dry-run" => options.dry_run = true,
        "--json" => options.json = true,
        "--keep-worktrees" => options.keep_worktrees = true,
        _ => return Ok(false),
    }
    Ok(true)
}

fn ab_command(args: &[String], cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let mut options = ab_defaults();
    let mut agent_args: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if shared_option(arg, &mut rest, &mut options, &mut agent_args)? {
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => return print(out, AB_HELP),
            "--drop" => options
                .variants
                .push(Variant::of(Change::Drop(value(arg, &mut rest)?))),
            "--replace" => {
                let change = Change::parse(&format!("replace={}", value(arg, &mut rest)?), cwd)?;
                options.variants.push(Variant::of(change));
            }
            "--add" => {
                let change = Change::parse(&format!("add={}", value(arg, &mut rest)?), cwd)?;
                options.variants.push(Variant::of(change));
            }
            "--bare" => options.variants.push(Variant::of(Change::Bare)),
            "--variant" => options
                .variants
                .push(Variant::parse(&value(arg, &mut rest)?, cwd)?),
            "--rounds" => options.rounds = number(arg, &mut rest)?,
            "--no-control" => options.control = false,
            other => {
                return Err(Error::Usage(format!(
                    "`tune ab` has no option `{other}`; `tune ab --help` lists them"
                )));
            }
        }
    }
    if options.variants.is_empty() {
        return Err(Error::Usage(
            "nothing to compare: name a variant with --drop, --replace, --add, --bare or --variant"
                .to_owned(),
        ));
    }
    agent_checked(&mut options, agent_args)?;
    experiment::run(&options, cwd, out)
}

fn prune_command(args: &[String], cwd: &Path, out: &mut dyn Write) -> Result<(), Error> {
    let mut options = prune::Options {
        run: ab_defaults(),
        units: Vec::new(),
        top: None,
        holdout: None,
        rounds: None,
    };
    let mut agent_args: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if shared_option(arg, &mut rest, &mut options.run, &mut agent_args)? {
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => return print(out, PRUNE_HELP),
            "--unit" => options.units.push(value(arg, &mut rest)?),
            "--top" => options.top = Some(number(arg, &mut rest)?),
            "--holdout" => {
                options.holdout = Some(value(arg, &mut rest)?.parse::<u32>().map_err(|_| {
                    Error::Usage("`--holdout` needs a whole number, 0 for none".to_owned())
                })?);
            }
            "--rounds" => options.rounds = Some(number(arg, &mut rest)?),
            other => {
                return Err(Error::Usage(format!(
                    "`tune prune` has no option `{other}`; `tune prune --help` lists them"
                )));
            }
        }
    }
    agent_checked(&mut options.run, agent_args)?;
    prune::run(options, cwd, out)
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
        "prune" => prune_command(rest, cwd, out),
        "apply" => match rest {
            [flag, arm] if flag == "--files" => apply::apply(cwd, Path::new(arm), None),
            [flag, arm, hide, list] if flag == "--files" && hide == "--hide" => {
                apply::apply(cwd, Path::new(arm), Some(Path::new(list)))
            }
            _ => Err(Error::Usage(
                "`tune apply --files DIR [--hide LIST]` is the setup command of a trial".to_owned(),
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
