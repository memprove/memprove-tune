# Task files

A task is one piece of work the agent is asked to do in a clean copy of
your repository, and a command that says whether it did it. Good tasks are
things the repository really needed: a bug that was fixed, a small feature
that was added, each with the test that proves it.

`memprove-tune init` writes the first task files from your history (a commit that
changed a test and code becomes a task); `memprove-tune init --verify` keeps only
those whose check fails before the commit and passes after it. When it
guesses your test command wrong, give it: `--test "uv run pytest"`, and
`--setup "uv sync"` for what a fresh checkout needs first. Where commits
are squash merges with one-line messages, `--prompt-from pr` takes each
prompt from the pull request instead (it needs the GitHub CLI, signed in),
and `--named-only` passes over a commit whose tests call something the
prompt never names. Read what it wrote, and add tasks by hand for work the
history does not show.

Put task files under `.tune/tasks/` (or pass `--tasks PATH`). One JSON file
per task:

```json
{
  "schema": "tune.task.v1",
  "title": "Reject parcels heavier than 30 kg",
  "base": "3f2a91c",
  "prompt": "Parcels heavier than 30 kg must be rejected with the error OVERWEIGHT. Implement it.",
  "check": { "argv": ["npm", "test", "--", "weight"], "timeout_secs": 300 },
  "check_patch": "weight.test.patch",
  "setup": [{ "argv": ["npm", "ci"], "timeout_secs": 600 }],
  "agent_timeout_secs": 900
}
```

| Member | Required | Meaning |
| --- | --- | --- |
| `schema` | yes | `tune.task.v1` |
| `prompt` | yes | What the agent is told. Describe the behaviour wanted, not the test. |
| `check` | yes | `argv` (the command, not a shell line) and `timeout_secs`. Exit 0 means the trial passed. |
| `id` | no | 1 to 64 of `a-z`, `0-9`, `-`, `_`. The file name without `.json` when absent. |
| `title` | no | A name for reports. |
| `base` | no | The commit the trial starts from. `HEAD` when absent. Use the commit just before the real fix. |
| `check_patch` | no | A patch file, beside the task file, applied after the agent is done and before the check: the hidden tests. The agent never sees it. |
| `setup` | no | Commands run in the copy before the agent, for example installing dependencies. |
| `agent_timeout_secs` | no | The agent's time limit; 1800 when absent. |
| `tags` | no | Free labels. |

Unknown members are refused, so a typo is found before any trial runs.

## What a trial is

1. The files of the base commit are written to a new directory and made a
   repository of their own with one commit. Task files and check patches
   are left out.
2. The arm's configuration replaces the one the base commit had.
3. The setup commands run.
4. The agent runs with the prompt, in that directory, with an environment
   that holds only what it needs.
5. The check patch is applied, then the check command runs.

A trial whose setup, patch or start failed is reported as an error and is
left out of the comparison: it says nothing about the configuration.

## How many tasks

A unit that matters shows with few pairs: five tasks that all fail without
it are enough. Showing that a unit can go takes about 30 pairs that agree,
for example 10 tasks in 3 rounds. `memprove-tune ab --dry-run` shows the number of
trials and the estimated cost before anything runs.

`memprove-tune prune` works out the rounds from the tasks it has, and with four or
more tasks it keeps a quarter of them out of the search to check the result
on. Tasks the agent fails with today's configuration tell it nothing: use
tasks the baseline passes most of the time.
