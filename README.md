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

Status: early. `scan`, `init`, `ab` and `prune` work and are tested with
scripted agents; the agent supported for real runs is Claude Code, and no
real run has been made yet.

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

A task is a prompt and a check command, taken from work your repository
really needed. `tune init` writes the first ones from your history: a
commit that changed a test and code becomes a task that starts from its
parent, asks for what its message says, and is judged by its tests, which
the agent never sees.

```console
$ tune init --verify
2 tasks written to .tune/tasks:
  mined-9e5fd15  lib: add five
      check: make test
  mined-c0c1fa6  lib: add two
      check: make test
1 commit was refused by --verify:
  mined-285e1cb  lib: add four
      the check exited 2 on the commit with the change, so the task cannot be passed (logs: .git/tune/mine/.../mined-285e1cb)
Read the prompts before you trust them: each is a commit message, and it may say too little to do the work from.
Next: `tune scan` lists the units of the configuration, and `tune prune --dry-run` shows the plan and its cost.
```

(The path of the logs is shortened here.)

`--verify` runs each task's tests before and after its commit and keeps
the ones that fail before and pass after; it calls no model. You can also
write task files by hand (see [docs/tasks.md](docs/tasks.md)). Then:

```console
$ tune ab --drop 'CLAUDE.md#style' --rounds 5 --max-cost 15 --cost-per-trial 0.11
Experiment 82837b287474: 3 arms x 6 tasks x 5 rounds = 90 trials (0 done, 90 to run)
  arms: baseline; control; without CLAUDE.md#style
  agent: claude-code
  estimated cost of what is left: $9.90 at $0.11 a trial; cap $15.00
Run it? [y/N]
```

and, when it is done (this report is from a scripted agent):

```
  ARM                         PASSED    RATE  COST/TRIAL  TOKENS/TRIAL  CONFIG TOKENS
  baseline                     30/30  100.0%       $0.11         9,566             43
  control                      30/30  100.0%       $0.11         9,566             43
  without CLAUDE.md#style      30/30  100.0%       $0.11         9,486             22

without CLAUDE.md#style vs baseline: 30 pairs (both passed 30, neither 0, only the baseline 0, only this arm 0)
  REMOVABLE: the agent passes as often without it (at most 9.5 points lower, 95% bound); each session carries about 21 fewer configuration tokens.
  cost per trial: +0.0%
control vs baseline (the same configuration twice): 30 pairs, 0 differ. A difference has to be larger than that to count.
```

Besides `--drop UNIT` a variant can be `--replace PATH=FILE`,
`--add PATH=FILE`, `--bare` (no configuration at all) or several changes
under one name with `--variant`. `tune ab --help` lists every option.

## Prune the whole configuration

`tune prune` does the same for every unit at once and writes the change as
a patch. It drops each unit alone, then drops everything that came out
removable together and checks that against today's configuration, on tasks
the search did not use when there are four or more. This run is from a
scripted agent that reports $0.50 a trial:

```console
$ tune prune --agent-cmd 'sh agent.sh' --max-cost 100
Prune: 3 units, each dropped alone, on 5 tasks x 6 rounds = 30 pairs a unit. A REMOVABLE verdict at a margin of 10 points needs 29 pairs that agree.
  units, largest first: CLAUDE.md#useful (11 tokens); CLAUDE.md#useless (9 tokens); CLAUDE.md#harmful (4 tokens)
  held out for the final check: t1
Experiment 341378027086: 5 arms x 5 tasks x 6 rounds = 150 trials (0 done, 150 to run)
  arms: baseline; control; without CLAUDE.md#useful; without CLAUDE.md#useless; without CLAUDE.md#harmful
  ...
Then the units that are not needed are dropped together and checked against the baseline on the held-out tasks: up to 12 more trials.
Run it? [y/N]
```

and after the report of that experiment:

```
Pruned configuration: without CLAUDE.md#harmful and CLAUDE.md#useless (2 of 3 units; about 13 fewer tokens in every session).
Checked on 1 held-out task x 6 rounds: the pruned configuration passed 6 of 6, the baseline 6 of 6 (experiment feb3ec8143e4).
Patch: .tune/prune.patch (apply it with `git apply .tune/prune.patch`)
Verdicts: .tune/verdicts.json (3 units)
Spent $81.00 of the $100.00 cap in all.
```

Nothing in the repository is changed until you apply the patch. What was
found for each unit is kept in `.tune/verdicts.json`, which is meant to be
committed, and `tune scan` then shows every verdict and whether it still
holds: a verdict goes stale when its unit's text or the task files change.

The plan says how many pairs the run gives each unit and how many a
REMOVABLE verdict needs, and, when the estimate is above the cap, how many
units the cap pays for (`--top K` tests the K largest). Stopped at the
cap, `prune` concludes nothing: no patch and no verdict; the trials are
kept, and the same command with a larger cap goes on from there.
`tune prune --help` lists every option.

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
  30 pairs that agree. `--margin POINTS` changes the 10: at 20 points 14
  pairs are enough, at 5 it takes 59.
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
- `prune` tests each unit alone. Two units that say the same thing both
  read removable; the final check catches that and writes no patch, and
  `tune ab --variant` tests a combination by name.
- A unit cannot show its worth on a task the agent fails with or without
  it. Tasks the baseline mostly fails make everything look removable;
  `prune` warns when the baseline passes fewer than half its trials.

## Where it comes from

The trial engine (`crates/tune-engine`) was copied from
[memprove](https://github.com/memprove/memprove)'s `memprove-eval` crate
and renamed; `crates/tune-engine/PROVENANCE.md` says exactly what was
taken. `tune` does not need memprove to build or to run.

## License

Apache-2.0.
