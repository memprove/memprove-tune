# Changelog

## Unreleased

- The product is called `memprove-tune`, and so are the package and the
  command. It stays a tool of its own: no memprove crate, no memprove
  install, nothing of memprove's read or written. What it keeps on disk
  is still named `tune` (`.tune/`, `tune.*.v1`, `TUNE_*`).
- `memprove-tune scan`: units of the agent configuration, their token cost, and
  findings; text and JSON (`tune.scan.v1`).
- `memprove-tune ab`: baseline, control and variants on the repository's task
  files, with a plan and a spending cap; text and JSON (`tune.ab.v1`).
- The trial engine, copied from memprove-eval and renamed.
- `memprove-tune prune`: every unit dropped alone, the removable ones dropped
  together and checked on held-out tasks, `.tune/prune.patch` for
  `git apply`, and the verdicts in `.tune/verdicts.json`
  (`tune.verdicts.v1`); text and JSON (`tune.prune.v1`).
- `memprove-tune init`: task files from the repository's history (the mining code
  of memprove-eval, copied into the engine and renamed), with `--verify`
  to keep only tasks that fail before their commit and pass after it;
  text and JSON (`tune.init.v1`).
- `memprove-tune scan` lists the verdicts on file and whether each still holds
  (`verdicts` in `tune.scan.v1`).
- `--margin POINTS` for `ab` and `prune`: how much lower the pass rate may
  truly be and still count as no worse.
- Changed: a trial whose agent a signal ended before its time limit (seen
  as the signal, or as exit code 128 plus the signal) has no verdict. Its
  check does not run, it is left out of the pairs, and the report of `ab`
  names it. It used to count as failed when its check failed.
- `docs/validation.md`: every run with a real agent, what it found and
  what it does not show.
- Fixed: run from a subdirectory, `memprove-tune ab` named the experiment without
  the content of the task files, so a changed task continued the old
  experiment.
