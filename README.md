# tune

> `tune` is a working name. The product will be renamed before it is public.

Every line in `CLAUDE.md`, every rule file and every skill is read by your
coding agent in every session, and nobody knows which of them help. `tune`
finds out by experiment: it runs the agent on tasks from your own
repository with a part of the configuration and without it, and lets your
own tests decide.

- **Only tests judge.** A trial passes when the task's check command exits
  0. No model grades another model.
- **Three verdicts.** Keep, removable, or not enough evidence. It does not
  guess when the trials cannot tell.
- **The cost is stated first.** The plan and its estimated cost are shown
  before anything runs, and a run stops at the cap you set.
- **Nothing leaves your machine** except what your agent itself sends.

Status: early. `scan` and `ab` work and are tested with scripted agents;
the agent supported for real runs is Claude Code.

## Build

```sh
cargo build --release      # Rust 1.96.0, see rust-toolchain.toml
./target/release/tune --help
```

## Look before you run anything

```console
$ tune scan
1 configuration file, 3 units. About 43 tokens load in every Claude Code session; 43 in all.

CLAUDE.md (9 lines, loaded in every session)
  CLAUDE.md#project-notes  lines 1-2              5 tokens
  CLAUDE.md#answers        lines 3-6             18 tokens
  CLAUDE.md#style          lines 7-9             20 tokens

Findings: 1
  CLAUDE.md:7 [formatter-rules] states formatting rules (2 spaces, indent, semicolon) that .prettierrc can enforce
```

`scan` runs nothing and calls no model. A unit is a section of `CLAUDE.md`
or `AGENTS.md`, a file under `.claude/rules`, or a skill under
`.claude/skills`. `tune scan --json` gives the same as one JSON document.

## Test whether a unit earns its place

Write a few task files (see [docs/tasks.md](docs/tasks.md)): each is a
prompt and a check command, taken from work your repository really needed.
Then:

```console
$ tune ab --drop 'CLAUDE.md#style' --rounds 5 --max-cost 15 --cost-per-trial 0.11
Experiment 09edc248eed4: 3 arms x 6 tasks x 5 rounds = 90 trials (0 done, 90 to run)
  arms: baseline; control; without CLAUDE.md#style
  agent: claude-code
  estimated cost of what is left: $9.90 at $0.11 a trial; cap $15.00
Run it? [y/N]
```

and, when it is done (this report is from a scripted agent):

```
  ARM                           PASSED    RATE  COST/TRIAL  TOKENS/TRIAL  CONFIG TOKENS
  baseline                       30/30  100.0%       $0.11         9,566             43
  control                        30/30  100.0%       $0.11         9,566             43
  without CLAUDE.md#style        30/30  100.0%       $0.11         9,486             22

without CLAUDE.md#style vs baseline: 30 pairs (both passed 30, neither 0, only the baseline 0, only this arm 0)
  REMOVABLE: the agent passes as often without it (at most 9.5 points lower, 95% bound); each session carries about 21 fewer configuration tokens.
control vs baseline (the same configuration twice): 30 pairs, 0 differ. A difference has to be larger than that to count.
```

Besides `--drop UNIT` a variant can be `--replace PATH=FILE`,
`--add PATH=FILE`, `--bare` (no configuration at all) or several changes
under one name with `--variant`. `tune ab --help` lists every option.

## How a verdict is reached

- Every trial starts from a clean copy of the repository at the task's
  base commit, with the arm's configuration in place of the one that commit
  had. The copy has one commit and nothing in it holds the hidden tests or
  the configuration that was replaced.
- The arms take turns, round by round, so that none always runs first.
- The same task in the same round under two arms is a pair. A variant is
  **worse** or **better** when the pairs that changed sides are too
  one-sided to be chance (exact sign test, 5 %) and more than the control's.
- It is **no worse** when its pass rate is at most 10 points below the
  baseline's with 95 % confidence (exact binomial bounds). That takes about
  30 pairs that agree.
- Anything else is **not enough evidence**. Running the same command again
  with a larger `--rounds` continues the experiment.

For a variant that only drops units, the report says KEEP, REMOVABLE or NOT
ENOUGH EVIDENCE.

## Limits you should know

- Real runs support Claude Code only. `--agent-cmd` runs any command as the
  agent, which is how the tests work.
- A verdict holds for the tasks, the agent version and the model it was
  measured with. It does not carry over to others.
- Token counts are estimates (about four characters to a token).
- "Removable" means "no measurable loss on these tasks", not "useless
  everywhere": tasks that never touch what a unit is about cannot show its
  worth.
- The spending cap is checked before each arm's round, so a run can pass
  it by at most the trials of one arm in one round.

## Where it comes from

The trial engine (`crates/tune-engine`) was copied from
[memprove](https://github.com/memprove/memprove)'s `memprove-eval` crate
and renamed; `crates/tune-engine/PROVENANCE.md` says exactly what was
taken. `tune` does not need memprove to build or to run.

## License

Apache-2.0.
