---
name: memprove-tune-setup
description: Install memprove-tune and get the current Git repository ready for it - list the units of the agent configuration (CLAUDE.md, AGENTS.md, rule files, skills) and what each costs in tokens, write the first tasks from the repository's history, and show the plan and cost of an experiment without running it. Use when the user asks to install, set up or try memprove-tune in this repository, or asks which of their agent instructions earn their place.
---

# Get this repository ready for memprove-tune

memprove-tune tests whether the instructions a coding agent is given earn
their place. It runs the agent on tasks from the repository's own history
with a part of the configuration and without it, and only the repository's
tests decide. It is a tool of its own and does not need memprove. Docs:
https://github.com/memprove/memprove-tune

The user asked you to set it up. Do the steps below in order. Run each
command as a command of its own, read its output, then go on. Every step is
safe to run again. When you are done, report as step 7 says.

## Rules

- Never run `memprove-tune ab` or `memprove-tune prune` without `--dry-run`,
  and never with `--yes`, unless the user asks for that run and names the
  spending cap. A real run starts Claude Code once for every trial, often a
  few hundred times, and spends the user's credits or usage. `scan`,
  `init`, `init --verify` and anything with `--dry-run` start no agent and
  call no model.
- Do not edit or delete `CLAUDE.md`, `AGENTS.md`, rule files or skills, and
  do not apply `.tune/prune.patch`, unless the user asks. What comes out of
  this setup is a list and a plan, not a change to their configuration.
- Do not commit, push, change the shell profile, use `sudo` or build from
  source unless the user asks.
- If your sandbox blocks a command (no network, a write outside the
  repository), ask the user to approve running it outside the sandbox, or
  give them the command to run themselves. Do not work around it.
- If every command runs in a new shell for you, `export` does not carry
  over: where these steps say `memprove-tune`, call
  `~/.memprove-tune/bin/memprove-tune` by its path whenever `memprove-tune`
  is not on `PATH`.

## 1. Check the repository

```bash
git rev-parse --show-toplevel
git log -1 --oneline
```

Run every later step from the directory the first command prints. If either
command fails, stop and tell the user: memprove-tune needs a Git repository
with at least one commit.

## 2. Find memprove-tune

```bash
memprove-tune --version
```

- It prints `memprove-tune` and a version: go to step 4.
- Not found, but `~/.memprove-tune/bin/memprove-tune --version` works: use
  that path (see Rules) and go to step 4.
- Neither: step 3 installs it.

## 3. Install

Release binaries exist for macOS on Apple silicon (`uname -sm` prints
`Darwin arm64`) and Linux x86_64 with glibc 2.35 or later (Ubuntu 22.04,
Debian 12). On anything else (Intel macOS, Linux arm64, musl, Windows) stop
and tell the user that it builds from source with Rust 1.89 or later:
`cargo install --locked --git https://github.com/memprove/memprove-tune memprove-tune`.

Make a new temporary directory outside the repository:

```bash
mktemp -d
```

Then, with `<dir>` standing for the directory it printed, download the
installer and run it as a command of its own:

```bash
curl -fsSLo <dir>/install.sh https://memprove.dev/tune/install.sh
sh <dir>/install.sh
~/.memprove-tune/bin/memprove-tune --version
```

The installer takes the latest release from
https://github.com/memprove/memprove-tune/releases, stops unless the
archive's SHA-256 matches the release's `.sha256` file and the binary says
that version, and puts one file at `~/.memprove-tune/bin/memprove-tune`.
There is no daemon and no configuration, and the shell profile is left
alone; ask the user before running it again with `--write-profile`, which
adds the `PATH` line so later sessions find `memprove-tune`.

If it exits with status 1, read its `memprove-tune-install: error:` lines.
On a checksum mismatch nothing was installed: tell the user and do not
install memprove-tune another way. If memprove.dev cannot be reached but
github.com can, install the same release by hand: download
`memprove-tune-<version>-<target>.tar.gz` and its `.sha256` from the latest
release (`<target>` is `aarch64-apple-darwin` or
`x86_64-unknown-linux-gnu`), check it with `shasum -a 256 -c` (Linux:
`sha256sum -c`), unpack it, and copy
`memprove-tune-<version>-<target>/bin/memprove-tune` to
`~/.memprove-tune/bin/memprove-tune`. Stop if the checksum does not match.

## 4. The configuration: `memprove-tune scan`

```bash
memprove-tune scan
```

It lists the units of the configuration (a section of `CLAUDE.md` or
`AGENTS.md`, a file under `.claude/rules`, a skill under `.claude/skills`),
what each costs in tokens in every session, and its findings, such as
formatting rules a formatter could enforce. It runs nothing and calls no
model. Keep the output for the report.

If it says `No agent configuration found`, stop here: there is nothing to
test in this repository. Tell the user so.

## 5. Tasks: `memprove-tune init`

A task is a prompt and a check command. `init` writes the first ones from
the history: a commit that changed a test and code becomes a task that
starts from its parent, asks for what its message says, and is judged by
its tests, which the agent never sees.

```bash
memprove-tune init --dry-run
```

- It lists the tasks it would write: go on.
- `no test command was found`: find the repository's test command (the
  README, the CI workflow, a Makefile) and give it, with the command a
  fresh checkout needs first when there is one:
  `memprove-tune init --dry-run --setup "npm ci" --test "npm test"`. Each
  is split into words at spaces and is not a shell line. Use the same
  options in the next command. If you cannot tell the test command, ask the
  user.
- `No commit in the history of HEAD can become a task`: tasks have to be
  written by hand (`memprove-tune init --help` shows one). Tell the user
  and go to step 7.

Then write the tasks and keep only those that can be passed:

```bash
memprove-tune init --verify
```

`--verify` runs the repository's tests twice for each task, before and
after its commit, and keeps a task only when the check fails before and
passes after. It calls no model and costs nothing but time; with a slow
test suite, say so before you start it, or use `--limit 5`. It prints the
tasks written to `.tune/tasks` and the commits it refused, each with the
reason. A task file that is already there is never written again.

Read the prompts it prints. Each is a commit message, and one that says too
little to do the work from tells nothing about the configuration. Where the
commits are squash merges that end with a pull request number, as in
`Fix it (#123)`, and `gh` is signed in, offer the user
`memprove-tune init --verify --prompt-from pr --named-only` instead; it
reads the pull requests with `gh`.

## 6. The plan: `memprove-tune prune --dry-run`

```bash
memprove-tune prune --dry-run
```

It prints how many units would be tested, on how many tasks and rounds, how
many trials that is, and which tasks are held out for the final check.
Nothing runs. Keep the output for the report. With fewer than four tasks
there is no held-out check, and with very few tasks a verdict takes many
rounds; say so if the plan shows it.

The tool does not know what a trial costs. If the user gives an estimate,
`--cost-per-trial USD` adds the cost to the plan; do not make one up.
`docs/validation.md` in memprove-tune's repository has what real runs
cost.

## 7. Report

```bash
git status --short
```

Then tell the user, briefly:

- The memprove-tune version and where it is. If `~/.memprove-tune/bin` is
  not on their `PATH`, the line to add
  (`export PATH="$HOME/.memprove-tune/bin:$PATH"`), or offer to rerun the
  installer with `--write-profile`.
- What `scan` found: the units, largest first, the tokens they cost in
  every session, and the findings.
- The tasks in `.tune/tasks`, the commits `--verify` refused and why, and
  any prompt that says too little.
- The plan of `prune --dry-run`, and that nothing was run.
- What they decide themselves: a real run. `memprove-tune prune --max-cost
  <USD>` tests every unit and writes `.tune/prune.patch`;
  `memprove-tune ab --drop <UNIT> --max-cost <USD>` tests one. Both show
  the plan and ask before they start, run Claude Code in clean copies of
  the repository, and stop at the cap. Run one only when they ask and name
  the cap.
- The files `git status --short` lists: `.tune/tasks/` is repository
  content to review and commit, and so is `.tune/verdicts.json` after a
  run. The trials themselves are kept under `.git/tune`, outside the
  working tree.

Offer to commit `.tune/tasks/`; commit only if they say yes.
