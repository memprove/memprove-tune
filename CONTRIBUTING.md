# Contributing to memprove-tune

memprove-tune is a Rust workspace of two crates: the trial engine
(`crates/tune-engine`) and the command (`crates/memprove-tune`). It has
two dependencies, `sha2` and `tempfile`, and nothing of memprove's.

## Building and testing

```sh
cargo build --locked
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
bash scripts/test-install.sh                      # the installer; Linux only
INSTALLER_SH=bash bash scripts/test-install.sh    # the same under bash
```

`rust-toolchain.toml` names the Rust these run with (1.96.0). The tests
use scripted agents: they call no model, need no network and write only
to temporary directories. The installer's tests run it, and
`scripts/check-release-archive.sh`, against stand-ins for `curl` and
`uname` and download nothing. CI (`.github/workflows/ci.yml`) runs all of
this on Ubuntu, and the Rust commands on macOS as well.

## What a change must keep

- The five rules of [docs/spec.md](docs/spec.md): only tests judge, three
  verdicts, a verdict belongs to what it was measured with, the plan and
  its cost come before a run, and hidden tests are never where the agent
  can read them.
- How a verdict is reached (the pairs, the sign test, the bounds, the
  margin, and which trials have no verdict) is pinned by tests. A change
  to it changes those tests in the same commit, and the README and
  `docs/spec.md` say the same as the code does.
- No test and no command but `ab` and `prune` starts an agent, and those
  two show the plan and its cost first.
- Dependencies are pinned (`=x.y.z`) and `Cargo.lock` is committed. Adding
  or upgrading one is a change of its own, with the reason.
- `memprove-tune` builds and runs without memprove.
  `crates/tune-engine/PROVENANCE.md` says what was copied from it; code
  taken later is added there.
- [docs/validation.md](docs/validation.md) lists every run with a real
  agent, what it found and what it does not show. A new run is added
  there, with its cost, whatever it found.

## Commits and pull requests

- One change per commit, with a title `<area>: <imperative summary>` under
  72 characters (`init: add --prompt-from pr and --named-only`) and a body
  that says what changed and why.
- Run the narrow tests of what you touched, then the commands above,
  before opening a pull request, and say in it which commands you ran and
  what they printed.
- A change a user can see gets a line in `CHANGELOG.md`.

## Releasing

1. On `main`: the version in `Cargo.toml` (and `Cargo.lock`), and a dated
   section in `CHANGELOG.md`. CI passes on that commit.
2. Create the tag `vX.Y.Z` on that commit and, straight after, a draft
   GitHub Release for it with the notes. The tag starts
   `.github/workflows/release.yml`, which builds the archive for each
   platform, runs the binary from it, installs it on that runner with
   `scripts/install.sh` (`scripts/check-release-archive.sh`) and attaches
   it with its `.sha256` to that Release. The workflow fails when the tag
   and `Cargo.toml` disagree or when the Release does not exist.
3. Check the four files on the Release, publish it, and run the one-line
   installer once against the published Release.
4. `scripts/install.sh` is served at `https://memprove.dev/tune/install.sh`
   from the site's repository. A change to the script is copied there in
   the same release.

## Licence

By contributing you agree that your contribution is licensed under the
Apache License, Version 2.0 (`LICENSE`).
