# tune: one-page spec (stage 1)

`tune` is a working name. Scope of this page: what stage 1 built, the
decisions in it, and what is left out.

## What it is for

A person who maintains the configuration of a coding agent (`CLAUDE.md`,
`AGENTS.md`, rule files, skills) wants to know which parts help. `tune`
answers by experiment on the repository's own tasks, judged only by the
repository's own checks.

## Rules

1. Only tests judge. No model grades a trial.
2. Three verdicts: keep, removable, not enough evidence.
3. A verdict belongs to a configuration, a task set, an agent version and
   a model. (Recording and expiring verdicts is stage 2.)
4. The plan and its cost are shown before a run, and a run stops at the
   cap.
5. Hidden tests are never where the agent can read them.

## Commands in stage 1

| Command | Does |
| --- | --- |
| `tune scan [--json]` | Units, their token cost, findings. Runs nothing. |
| `tune ab ...` | Baseline, control and variants on the task files; report as text or JSON. |
| `tune apply --files DIR` | Internal: the setup command `ab` adds to every task. |

Later: `prune` (stage 2), `check` and `init` (public release), `optimize`
(v2).

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
- Token counts are estimates.
- `scan` findings are heuristics. They prefer saying nothing to saying
  something wrong, so they miss things.

## Verification

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` on Rust 1.96.0.
