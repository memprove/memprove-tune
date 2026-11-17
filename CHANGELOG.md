# Changelog

## Unreleased

- `tune scan`: units of the agent configuration, their token cost, and
  findings; text and JSON (`tune.scan.v1`).
- `tune ab`: baseline, control and variants on the repository's task
  files, with a plan and a spending cap; text and JSON (`tune.ab.v1`).
- The trial engine, copied from memprove-eval and renamed.
- `tune prune`: every unit dropped alone, the removable ones dropped
  together and checked on held-out tasks, `.tune/prune.patch` for
  `git apply`, and the verdicts in `.tune/verdicts.json`
  (`tune.verdicts.v1`); text and JSON (`tune.prune.v1`).
- `tune init`: task files from the repository's history (the mining code
  of memprove-eval, copied into the engine and renamed), with `--verify`
  to keep only tasks that fail before their commit and pass after it;
  text and JSON (`tune.init.v1`).
- `tune scan` lists the verdicts on file and whether each still holds
  (`verdicts` in `tune.scan.v1`).
- `--margin POINTS` for `ab` and `prune`: how much lower the pass rate may
  truly be and still count as no worse.
- Fixed: run from a subdirectory, `tune ab` named the experiment without
  the content of the task files, so a changed task continued the old
  experiment.
