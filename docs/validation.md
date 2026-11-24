# What has been measured

This page lists every run of `memprove-tune` with a real agent, what each
one found, and what it does not show. Last changed 2026-11-24.

All runs used Claude Code 2.1.310 with the model `claude-opus-5-5` on one
laptop (macOS, arm64). Dollar figures are usage as Claude Code reports it,
at API prices. Runs 2 and 3 ran three trials at a time, and each trial was
stopped by Claude Code at $1.50 and by the tool at 30 minutes. Run 4 ran
three at a time with a stop at $0.50.

## In short

- A fact that was planted in a configuration file was found: without the
  file the agent fails the one task that needs it, every time.
- On three public repositories, taking away the whole agent configuration
  did not lower the pass rate by an amount the runs could see. With 88 to
  90 tasks a repository, two of the three came out **no worse** within 10
  points and the third missed that by 0.9 points. In none of them did the
  configuration show a benefit.
- Sessions without the configuration cost 8 to 12 % less a trial.
- A verdict on a whole configuration took $84 to $93 and three to eight
  hours a repository.
- `prune`, run unit by unit on the planted-fact repository, kept the one
  section a task needs, marked the other three removable and wrote a patch
  that leaves only that section: 200 trials, $21.99. It has not been run
  on a real repository yet.

## Run 1: a planted fact (2026-11-17 and 11-18)

A small made-up repository, `parcelpost`, with four tasks. Its `CLAUDE.md`
holds one fact that one task needs and the code does not say (a port
number). Three arms, five rounds, 60 trials, $6.36.

| Arm | Passed | Cost a trial |
|---|---|---|
| baseline | 20/20 | $0.10 |
| control (the same configuration again) | 20/20 | $0.10 |
| without `CLAUDE.md` | 15/20 | $0.11 |

Verdict: **KEEP** (5 of 20 pairs changed sides, all against the arm
without the file; exact sign test, one-sided, p = 0.031). The control
differed from the baseline in 0 pairs.

What it shows: when a unit holds something a task needs, the tool finds
it. What it does not: the five pairs are one task in five rounds, and the
effect was put there on purpose.

## Run 2: three public repositories, commit titles as prompts (2026-11-18 and 11-19)

| Repository | At commit | Configuration tokens |
|---|---|---|
| `modelcontextprotocol/python-sdk` | `77001c9` | 2,252 |
| `PrefectHQ/fastmcp` | `922d78d` | 5,110 |
| `openai/openai-agents-python` | `e7ce15a` | 4,162 |

Tasks were mined from each repository's history with `memprove-tune init
--verify`: the agent starts at a commit's parent, is asked for what the
commit did, and is judged by the tests that commit brought. Three arms
(baseline, control, without any configuration), 30 tasks, one round: 270
trials, $155.95.

| Repository | Baseline | Control | Without configuration | Verdict |
|---|---|---|---|---|
| python-sdk | 15/30 | 15/30 | 14/30 | not enough evidence |
| fastmcp | 16/30 | 16/30 | 17/30 | not enough evidence |
| openai-agents-python | 12/30 | 12/30 | 13/30 | not enough evidence |

The same configuration run twice differed from itself in 0, 6 and 2 of 30
tasks. The task set was blunt: the prompt was the commit's title (a median
of ten words). Of the 90 tasks, 36 passed in every arm and 39 failed in
every arm; the changes of those 39 were about three times as large, and in
12 of them the tests called a function or class the prompt never named.
Only 15 tasks could tell the arms apart, too few for a verdict in either
direction.

This led to `init --prompt-from pr` (the pull request's title and
description as the prompt) and `--named-only` (pass over a commit whose
tests need a name that neither the prompt nor the code has).

## Run 3: the same repositories, better tasks (2026-11-19 and 11-20)

The same repositories at the same commits. Tasks had to pass
`--narrowed-only`, `--named-only` and `--verify`, and every prompt came
from a pull request. Two arms (baseline, without any configuration), one
round, 536 trials, $260.96. The rules were written down before the run
and not changed after it: the tool's own verdict, a margin of 10 points,
5 %.

| Repository | Tasks | Baseline | Without configuration | Only the baseline passed / only without | At most this much lower (95 %) | Verdict | Cost |
|---|---|---|---|---|---|---|---|
| python-sdk | 88 | 62 (70.5 %) | 66 (75.0 %) | 3 / 7 | 4.8 points | no worse | $83.54 |
| fastmcp | 90 | 62 (68.9 %) | 59 (65.6 %) | 5 / 2 | 10.9 points | not enough evidence | $84.75 |
| openai-agents-python | 90 | 57 (63.3 %) | 59 (65.6 %) | 7 / 9 | 8.8 points | no worse | $92.67 |

- No repository shows the configuration helping: the pairs that changed
  sides are within chance in all three (p = 0.17, 0.23 and 0.40).
- Without the configuration a trial cost 11.7 %, 8.2 % and 11.1 % less.
- The tasks were sharper than in run 2: the baseline passed 67.5 % of
  them, against 48 %, and the share that failed in every arm fell from
  43 % to 26 %.
- The agent's runs took 3 h 12 min, 2 h 47 min and 8 h 8 min; mining and
  verifying the tasks took another 1 to 1.8 hours a repository and no
  model call.

Every figure in the table was recomputed from the task-by-task results
with separate code and exact arithmetic, and agrees with the tool's report.

### Trials that were cut short

In three of the 536 trials the agent ended with exit code 143 and no
output: something outside the trial sent it `SIGTERM`. Two of them ended
within a second of each other, part-way through their work. What sent the
signal was not found; the machine did not sleep. All three were baseline
trials, and the version that ran (`fbe40bf`) counted them as failed.

| Reading | fastmcp | openai-agents-python |
|---|---|---|
| As the run counted them (failed) | 10.9 points, not enough evidence | 8.8 points, no worse |
| Left out, as the tool does now | 11.4 points, not enough evidence | 9.8 points, no worse |
| Had each of them passed | 11.3 points, not enough evidence | 11.0 points, not enough evidence |

So the verdict for openai-agents-python holds under the rules of the run
and under the rule the tool has now, and turns only if both cut-short
trials are assumed to have passed. It is the weaker of the two "no worse"
verdicts; python-sdk had no such trial and is 5 points inside the margin.

One more trial, in the arm without configuration, reached the 30-minute
limit with no output. It counts as failed in every reading.

## Run 4: `prune` on the planted fact (2026-11-24)

The `parcelpost` repository of run 1, with its `CLAUDE.md` written as four
sections. Only `## Ports` holds something a task needs and the code does
not say; the other three repeat what the code and its tests already show.

| Unit | Tokens |
|---|---|
| `CLAUDE.md#tests` | 24 |
| `CLAUDE.md#money` | 40 |
| `CLAUDE.md#limits` | 31 |
| `CLAUDE.md#ports` | 34 |

`memprove-tune prune` held out one of the four tasks (`free-shipping-faq`)
and ran the search on the other three: baseline, control and one arm
without each unit, ten rounds, 180 trials, $19.84.

| Arm | Passed | Cost a trial |
|---|---|---|
| baseline | 30/30 | $0.11 |
| control | 30/30 | $0.11 |
| without `#money` | 30/30 | $0.11 |
| without `#ports` | 20/30 | $0.11 |
| without `#limits` | 30/30 | $0.11 |
| without `#tests` | 30/30 | $0.12 |

- `#ports`: **KEEP**. Without it the `port` task failed in all ten rounds
  and nothing else changed (10 of 30 pairs against the arm, none for it;
  p = 0.001).
- `#money`, `#limits`, `#tests`: **REMOVABLE**. All 30 pairs passed on
  both sides, so the pass rate is at most 9.5 points lower with 95 %
  confidence.
- The control differed from the baseline in 0 of 30 pairs.

Then the three removable units were dropped together and checked against
the baseline on the held-out task, ten rounds, 20 trials, $2.15: both
passed 10 of 10. With ten pairs that comparison cannot show "no worse"
within 10 points (its bound is 25.9 points), so it is a check that the
pruned configuration still works, not a verdict. The patch the tool wrote
removes the three sections and keeps `## Ports`, about 95 fewer tokens in
every session.

In all 200 trials, $21.99, 21 minutes. No trial reached the $0.50 stop or
the time limit.

What it shows: unit by unit, the tool keeps the unit that matters and lets
go of the ones that do not, and the patch is what a person would write.
What it does not: the repository is made up and the effect was put there
on purpose, as in run 1.

## What these runs do not show

- **Only what tests measure.** A configuration's rules on style, commit
  messages and process cannot fail a test, so they cannot show here.
- **History replayed.** The tasks are past commits and the configuration
  is today's. A unit about code that did not exist then cannot help.
- **"No worse" is a bound, not "useless".** It says the pass rate is at
  most 10 points lower with 95 % confidence, on these tasks.
- **One trial for each task and arm.** Run 3 had no control arm; in run 2
  the same configuration differed from itself in up to 6 of 30 tasks.
- **One agent and one model.**
- **Three repositories that we picked**, all Python libraries with a
  `CLAUDE.md` or `AGENTS.md`. They are not a sample of anything.
- **Units only on a made-up repository.** `prune` has been run on the
  planted fact of run 4. Whether a single section or skill earns its place
  in a real repository has not been tested.

## Where the records are

The reports, the task-by-task results and the script that recomputes the
verdicts are kept by the maintainers and are not in this repository yet.
