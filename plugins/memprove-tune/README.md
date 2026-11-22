# memprove-tune plugin for coding agents

One skill, `memprove-tune-setup`, so that a single sentence to your coding
agent gets the repository you are in ready for memprove-tune:

> Set up memprove-tune in this repository.

The agent then installs memprove-tune when it is missing (the installer at
https://memprove.dev/tune/install.sh, which checks the release's SHA-256),
and runs memprove-tune's own commands: `memprove-tune scan` for the units of
your agent configuration and what each costs, `memprove-tune init --verify`
for the first tasks from your history, and `memprove-tune prune --dry-run`
for the plan of an experiment. It reports what it found and what is left
for you to decide. The skill holds no code of its own, and each step is
safe to run again.

None of these steps starts an agent or calls a model. The agent does not
run an experiment (`memprove-tune ab` or `prune` without `--dry-run`, which
starts Claude Code for every trial and spends your credits or usage), does
not change your `CLAUDE.md` or apply a patch, and does not commit, push or
change your shell profile, unless you ask it to.

## Install

Claude Code:

```
/plugin install memprove-tune --marketplace memprove/memprove-tune
```

or, in two steps, `/plugin marketplace add memprove/memprove-tune` and
`/plugin install memprove-tune@memprove-tune`. From a shell:
`claude plugin marketplace add memprove/memprove-tune`, then
`claude plugin install memprove-tune@memprove-tune`. The skill is
`/memprove-tune:memprove-tune-setup`, and Claude Code also picks it on its
own when you ask for memprove-tune.

Codex:

```bash
codex plugin marketplace add memprove/memprove-tune
codex plugin add memprove-tune@memprove-tune
```

Codex reads the same `.claude-plugin/marketplace.json`; the skill is
`memprove-tune:memprove-tune-setup`.

Cursor reads `.cursor-plugin/marketplace.json`: import
`https://github.com/memprove/memprove-tune` as a plugin repository, or copy
`skills/memprove-tune-setup/` into `~/.cursor/skills/`.

Any other agent that reads `SKILL.md` files (the Agent Skills format) can
use the folder `skills/memprove-tune-setup/` as it is. An agent that can
fetch a web page needs no install at all:

> Set up memprove-tune in this repository by following
> https://raw.githubusercontent.com/memprove/memprove-tune/main/plugins/memprove-tune/skills/memprove-tune-setup/SKILL.md

Real runs of memprove-tune support Claude Code only; the skill itself, which
runs none, works from any of these agents.

## Updates

The plugin sets no `version`, so its version is the commit it was installed
from, and `claude plugin update memprove-tune@memprove-tune` (or
`codex plugin marketplace upgrade`) brings the skill up to date.

## Licence

Apache License, Version 2.0, as the rest of the repository.
