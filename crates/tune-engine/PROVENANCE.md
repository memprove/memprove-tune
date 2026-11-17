# Where the engine came from

`tune-engine` began as a copy of the `memprove-eval` crate of
<https://github.com/memprove/memprove> (Apache-2.0, the same license as this
repository), taken on 2026-11-17:

- Source: `crates/memprove-eval` at commit
  `c1196fbf1f23dbce972f8a535f0386b599e667a6`, plus
  `crates/memprove-protocol/src/strict_json.rs` and its tests
  (`crates/memprove-protocol/tests/strict_json_regressions.rs`) at the same
  commit.
- Plus one change that was not yet in that repository: "eval: pass the agent
  a listed environment only" (an agent starts from an empty environment and
  inherits only the variables named in `agent::PASSED_ENV`,
  `agent::PASSED_ENV_PREFIXES` and `--pass-env`).

## What was copied

`agent.rs`, `agent/{claude,codex,command}.rs`, `error.rs`, `git.rs`,
`json.rs`, `model.rs`, `process.rs`, `runner.rs`, `task.rs`, `time.rs`,
`workspace.rs`, `strict_json.rs`, and the tests of the runner, the agent
adapters, the hidden answer and the JSON parser with their fixtures.

Taken in a second step, from the same commit, for `tune init`:
`mine.rs`, `mine/{history,narrow,verify}.rs` and their test
(`tests/u23_w4_mine.rs`, here `tests/mine.rs`); `init/detect.rs` as
`detect.rs`; and from `init.rs` only what `mine` needs, as `taskfile.rs`:
`render_task`, `setup_commands`, `slashed`, `detect_check` and the
timeouts of a written task.

Not copied: the `memprove eval` command line, baselines, reports, the rest
of `init` (the smoke task, the workflow file, the templates), and the
memory probe. The experiment layer (`crates/memprove-tune`) has its own command
line and reports.

## What was changed in the copy

Names only, and the removal of the memory feature:

| In memprove-eval | Here |
|---|---|
| crate `memprove-eval`, `memprove_protocol::strict_json` | crate `tune-engine`, module `strict_json` |
| task schema `memprove.eval.task.v1` | `tune.task.v1`; the old name is still read |
| run schema `memprove.eval.run.v1` | `tune.run.v1` |
| variables `MEMPROVE_EVAL_*` | `TUNE_*` |
| run files in `<git common dir>/memprove-eval/runs` | `<git common dir>/tune/runs` |
| working copies in `$TMPDIR/memprove-eval-<run>` | `$TMPDIR/tune-<run>` |
| trial commit author `memprove eval <eval@memprove.invalid>` | `tune <trial@tune.invalid>` |
| message prefixes `memprove eval:`, `memprove-eval:` | `tune:` in a trial's `agent.err`; `memprove-tune:`, the command's name, on the terminal |
| mined tasks go to `evals/` (`init::EVALS_DIR`) | `.tune/tasks/` (`taskfile::TASKS_DIR`) |
| `--verify` worktrees and logs in `<git common dir>/memprove-eval/mine` | `<git common dir>/tune/mine` |

Removed: `MemoryProbe`, `--memory pack`, the `memory*` members of `run.json`,
the baseline schema, and `libtest_summary`. Every other line of logic is as
it was, so a fix to isolation or to an agent adapter in one repository
should be made in the other as well.

In the mining code, besides the names: the message for a repository
without a test command no longer points at `evals/README.md`;
`history::MAX_FILES` and `history::MAX_LINES` are public, so that the
command can say them; and `mine/verify.rs` passes `clear_env: false`, the
field the environment change above added to `process::Invocation` (setup
and check commands keep the caller's environment, as in the runner).

Kept on purpose: `runner::AGENT_CONFIG_ENV` still points `MEMPROVE_CONFIG_DIR`
at an empty directory for every agent, and `agent::REMOVED_ENV` still lists
memprove's session variables, so that a repository under test that uses
memprove gives a trial no memory and no session of the caller.
