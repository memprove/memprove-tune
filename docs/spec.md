# tune: one-page spec (stages 1 and 2)

`tune` is a working name. Scope of this page: what stages 1 (`scan`, `ab`)
and 2 (`prune`, the verdict file, `init` for the first tasks) built, the
decisions in them, and what is left out.

## What it is for

A person who maintains the configuration of a coding agent (`CLAUDE.md`,
`AGENTS.md`, rule files, skills) wants to know which parts help. `tune`
answers by experiment on the repository's own tasks, judged only by the
repository's own checks.

## Rules

1. Only tests judge. No model grades a trial.
2. Three verdicts: keep, removable, not enough evidence.
3. A verdict belongs to a configuration, a task set, an agent version and
   a model. `prune` writes them to `.tune/verdicts.json`, and `scan` says
   which no longer hold.
4. The plan and its cost are shown before a run, and a run stops at the
   cap.
5. Hidden tests are never where the agent can read them.

## Commands

| Command | Does |
| --- | --- |
| `tune scan [--json]` | Units, their token cost, findings, and the verdicts on file with whether each still holds. Runs nothing. |
| `tune ab ...` | Baseline, control and variants on the task files; report as text or JSON. |
| `tune prune ...` | Every unit dropped alone, the removable ones dropped together and checked, a patch and the verdict file; report as text or JSON. |
| `tune init ...` | Task files from the repository's history: a commit that changed a test and code becomes a task. Calls no model. |
| `tune apply --files DIR [--hide LIST]` | Internal: the setup command every trial starts with. |

Later: `check`, and the rest of `init` (a starting configuration, the
onboarding skill) for the public release; `optimize` (v2).

## Decisions

- **Independent of memprove.** Own repository and binary. The trial engine
  is a renamed copy (`crates/tune-engine/PROVENANCE.md`); nothing here
  builds against, runs or prints memprove.
- **The engine is used as it is.** The experiment calls `run_suite` once
  per arm per round with one trial per task. The arm's configuration gets
  into the trial through a setup command, `tune apply`, which amends the
  trial's single commit and points the old commit's name at the new one
  with `git replace`, then expires the reflog and prunes. The engine's
  diff and hidden-test handling then work unchanged, the agent sees one
  commit and a clean tree, and the replaced configuration is gone from the
  object store. `crates/tune/tests/apply.rs` proves each of these.
- **Today's configuration, the task's code.** The baseline is the
  configuration in the working tree now, laid over each task's base commit.
- **Units.** Sections of an instruction file, split at the shallowest
  heading level that occurs twice; a rule file; a skill directory.
- **Statistics.** Pairs are the same task in the same round. Different:
  exact one-sided sign test at 5 %, and a net difference larger than the
  control's. No worse: exact (Clopper-Pearson) 95 % upper bound of "only
  the baseline passed" minus the lower bound of "only the variant passed"
  is at most 10 points. Both are written out in `stats.rs`; no dependency.
- **Continuing.** The experiment's name is a hash of the arms'
  configuration, the task files and the agent. The same command finds the
  same experiment, skips finished arm-rounds and adds what is missing.
- **Budget.** `--max-cost` is required for a real agent. Before each
  arm-round the spending so far plus the expected cost of that arm-round
  (observed mean, else `--cost-per-trial`) must fit under the cap.
- **Isolation of Claude Code.** Trials pass `--setting-sources
  project,local --strict-mcp-config`, so the person's own settings, memory
  and MCP servers stay out.
- **The margin is a choice.** `--margin POINTS` (1 to 50, default 10) sets
  how much lower the pass rate may truly be and still count as no worse.
  With nothing differing, 10 points needs 29 pairs, 20 needs 14, 5 needs
  59. The plan of `prune` states the pairs a run gives and the pairs a
  verdict needs.

### `prune` (stage 2)

- **One experiment for the search.** Baseline, control, and one arm per
  unit that leaves that unit out, on the search tasks. A unit's advice is
  the verdict of its arm. Units the agent does not read are listed as not
  tested; `--unit` names the units instead, `--top N` keeps the N largest.
- **Rounds.** Unless `--rounds` says otherwise, as many as give each unit
  the pairs a REMOVABLE verdict needs at the margin.
- **Held-out tasks.** A quarter of the tasks when there are four or more
  (`--holdout N` otherwise), chosen by the hash of their names so that it
  is the same every time. They take no part in the search.
- **The final check.** Every unit that came out removable is dropped at
  once, and that configuration runs against the baseline on the held-out
  tasks, or on the search tasks again when none is held out. The patch is
  written only if the pass rate seen is within the margin of the
  baseline's and at least one pair could be judged. With one removable
  unit and no held-out task the search's own pairs are the check.
- **The check is a guard, not a second verdict.** A few held-out pairs
  cannot prove "no worse". The check catches units that cover for each
  other and a result that does not carry to other tasks; the verdicts come
  from the search.
- **Nothing is changed.** The result is `.tune/prune.patch`, a `git diff`
  of the configuration files that `git apply` takes. A run that finds
  nothing removable, or loses the check, removes a patch left by an
  earlier run.
- **Stopped at the budget, nothing is concluded.** No verdict is written
  and no patch; the trials are kept and the same command with a larger
  cap goes on. `--max-cost` covers the search and the check together.
- **No trial sees a task file.** The engine hides the files of the tasks
  it runs; `tune apply --hide` removes the files and hidden tests of every
  other task of the suite, so that a search trial cannot read a held-out
  task and the other way round.
- **The verdict file.** `.tune/verdicts.json` (`tune.verdicts.v1`): by
  unit, the advice, the pairs, the margin, the hash of the unit's text,
  of the whole configuration and of the task files, the agent and its
  version, and the experiment. No dates: the same trials give the same
  bytes. It is meant to be committed.
- **When a verdict stops holding.** `scan` reads the file and compares:
  the unit's text changed, the unit is gone, or the task files changed or
  cannot be read make it stale; a change elsewhere in the configuration is
  a note. A unit that only got a new name (a file left with one section
  is one unit named by its path) keeps its verdict, because the verdict is
  about the text.

### `init` (the first tasks)

- **Copied, not written again.** Mining is memprove-eval's `mine`, taken
  into `tune-engine` with names changed (`PROVENANCE.md`); `tune init` is
  its command line. Tasks go to `.tune/tasks`, where `ab` and `prune` look
  first.
- **What becomes a task.** A commit that is not a merge and changed a test
  file and other code, in at most 20 files and 600 lines. Base = its
  parent, prompt = its message without trailers, check = the detected test
  command narrowed to the changed tests, hidden tests = the commit's diff
  of its test files.
- **`--verify` is optional and free of model cost.** It runs setup and
  check in two worktrees per task and keeps a task only when the check
  fails before the commit and passes after it. Without it the report says
  that the tasks were not run.
- **Old commits, today's configuration.** A mined task starts from a
  commit that may be older than the configuration; the experiment lays
  today's configuration over it, as for any task.

## Known limits

- Claude Code only for real runs; Codex and MCP servers as units are v2.
- No real agent has been run yet. Everything is tested with agents that
  are shell scripts.
- Units of an `AGENTS.md` that no `CLAUDE.md` imports can be dropped, but
  Claude Code never read them, so the experiment will show no difference.
  `scan` says so (`not-read`).
- A task whose base commit is old may not have the paths today's
  configuration names. Both arms see the same, but a unit about code that
  did not exist then cannot show its worth.
- The cap can be passed by one arm-round.
- `prune` tests each unit alone against today's configuration. Two units
  that cover for each other both read removable; the final check catches
  that and writes no patch, but it does not say which of them to keep
  (`tune ab --variant` tests a combination).
- A unit cannot show its worth on a task the agent fails with or without
  it. `prune` warns when the baseline passes fewer than half its trials.
- `prune` does not fit the units to the cap: the plan says how many the
  cap pays for (`--top K`), and the run stops at the cap otherwise.
- Changing the units tested (`--unit`, `--top`) or the held-out tasks
  makes a new experiment; trials of the earlier one are not reused.
- When nothing is held out and more than one unit is removable, the
  baseline runs again for the check.
- There is no early stop: a unit that is plainly needed still gets all its
  rounds.
- Token counts are estimates.
- A mined prompt is a commit message. It can say too little to do the
  work from, and then no arm passes the task. `init` says so; nothing
  checks it.
- `init` guesses one test command for the repository (Cargo, npm, pytest,
  go, make). A repository with several, or with none of these, needs task
  files written by hand.
- `scan` findings are heuristics. They prefer saying nothing to saying
  something wrong, so they miss things.

## Verification

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` on Rust 1.96.0.
