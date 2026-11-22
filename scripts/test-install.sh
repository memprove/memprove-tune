#!/bin/bash
# Tests scripts/install.sh offline: a fake curl serves fake releases from a
# temporary directory, fake uname, getconf, ldd and sysctl play the
# platform, and the "binary" in each archive is a shell script that says
# its version. The installer runs under /bin/sh (dash on Ubuntu) with a
# PATH that holds only the tools it is allowed to need.
#
#   bash scripts/test-install.sh
#   INSTALLER_SH=bash bash scripts/test-install.sh   # under another shell
#
# The last cases test scripts/check-release-archive.sh the same way. Runs
# shellcheck on both scripts first when shellcheck is on PATH. Needs Linux
# (GNU coreutils), as on the CI runner that runs it.
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo "test-install.sh: runs on Linux only" >&2; exit 2; }

root="$(cd "$(dirname "$0")/.." && pwd -P)"
installer="$root/scripts/install.sh"
sh_bin="$(command -v "${INSTALLER_SH:-sh}")"
T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s sh "$installer" "$root/scripts/check-release-archive.sh"
  echo "shellcheck -s sh scripts/install.sh scripts/check-release-archive.sh: OK"
else
  echo "shellcheck not on PATH: skipped"
fi

# ---- fakes
fakes="$T/fakes"
mkdir -p "$fakes" "$T/log" "$T/rel" "$T/tmp"
cat >"$fakes/curl" <<'EOF'
#!/bin/bash
out="" fmt="" url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -w) fmt=$2; shift 2 ;;
    --proto | --retry) shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
printf '%s\n' "$url" >>"$FAKE_LOG/curl.log"
repo=https://github.com/memprove/memprove-tune
case "$url" in
  "$repo/releases/latest")
    [ -n "${FAKE_LATEST:-}" ] || exit 22
    [ "$fmt" = '%{redirect_url}' ] || exit 99
    printf '%s/releases/tag/%s' "$repo" "$FAKE_LATEST" ;;
  "$repo/releases/download/"*)
    src="$FAKE_RELEASES/${url#"$repo/releases/download/"}"
    [ -f "$src" ] || exit 22
    cp "$src" "$out" ;;
  *) exit 6 ;;
esac
EOF
cat >"$fakes/uname" <<'EOF'
#!/bin/bash
case "${1:-}" in -m) echo "$FAKE_UNAME_M" ;; *) echo "$FAKE_UNAME_S" ;; esac
EOF
cat >"$fakes/getconf" <<'EOF'
#!/bin/bash
[ "${1:-}" = GNU_LIBC_VERSION ] && [ -n "${FAKE_GLIBC:-}" ] || exit 1
echo "glibc $FAKE_GLIBC"
EOF
cat >"$fakes/ldd" <<'EOF'
#!/bin/bash
if [ -n "${FAKE_MUSL:-}" ]; then echo "musl libc (x86_64)" >&2; exit 1; fi
echo "ldd (GNU libc) ${FAKE_GLIBC:-}"
EOF
cat >"$fakes/sysctl" <<'EOF'
#!/bin/bash
[ "${2:-}" = sysctl.proc_translated ] || exit 1
echo "${FAKE_TRANSLATED:-0}"
EOF
chmod +x "$fakes"/*

# The PATH the installer gets: the fakes plus these tools, and one hasher.
# Where a wrapper is first on PATH for a tool (some hosts put one before rm)
# and runs the next program of that name, the next one goes into $T/next,
# which comes after on the installer's PATH.
minbin() { # <dir> <hasher or "none"> [tool to leave out]
  local tool found
  mkdir -p "$1" "$T/next"
  for tool in tar gzip mktemp rm tr cat grep cp mv mkdir chmod awk tail dirname basename; do
    mapfile -t found < <(type -aP "$tool")
    ln -sf "${found[0]}" "$1/$tool"
    if [ "${#found[@]}" -gt 1 ]; then ln -sf "${found[1]}" "$T/next/$tool"; fi
  done
  [ "$2" = none ] || ln -sf "$(command -v "$2")" "$1/$2"
  if [ "$2" = shasum ]; then ln -sf "$(command -v perl)" "$1/perl"; fi
  for f in "$fakes"/*; do ln -sf "$f" "$1/"; done
  if [ -n "${3:-}" ]; then rm -f "$1/$3"; fi
}
for h in sha256sum shasum openssl none; do minbin "$T/bin-$h" "$h"; done
minbin "$T/bin-nocurl" sha256sum curl

# ---- fake releases: the "binary" is a script that says its version
release() { # <version> <target> [says]
  local version=$1 target=$2 says=${3:-$1} name d
  name="memprove-tune-$version-$target"
  d="$T/build/$name"
  rm -rf "$d"
  mkdir -p "$d/bin" "$T/rel/v$version"
  cat >"$d/bin/memprove-tune" <<EOF
#!/bin/sh
[ -z "\${FAKE_BROKEN_BINARY:-}" ] || exit 1
echo "memprove-tune $says"
EOF
  chmod +x "$d/bin/memprove-tune"
  printf 'readme\n' >"$d/README.md"
  tar -czf "$T/rel/v$version/$name.tar.gz" -C "$T/build" "$name"
  (cd "$T/rel/v$version" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
}
for version in 0.1.0 0.2.0; do
  for target in aarch64-apple-darwin x86_64-unknown-linux-gnu; do release "$version" "$target"; done
done
linux=x86_64-unknown-linux-gnu
mac=aarch64-apple-darwin

# ---- the runner
passed=0
failed=0
out="$T/log/out"
current=""
current_ok=1
home=""

note() {
  current_ok=0
  echo "    $*"
}

# case_ <name> <expected status> [VAR=value ...] -- [installer arguments ...]
# Runs the installer from $T/cwd with a HOME of its own (kept from the case
# before when KEEP_HOME=1); HASHER (default sha256sum) picks the PATH, and
# EXTRA_PATH is put after it.
case_() {
  finish
  current=$1
  local want=$2 got vars=()
  shift 2
  while [ "$1" != -- ]; do vars+=("$1"); shift; done
  shift
  rm -f "$T"/log/*.log "$out"
  if [ "${KEEP_HOME:-0}" != 1 ]; then
    home="$T/home-$((passed + failed))"
    rm -rf "$home"
    mkdir -p "$home"
  fi
  mkdir -p "$T/cwd"
  current_ok=1
  set +e
  (cd "$T/cwd" && env -i PATH="$T/bin-${HASHER:-sha256sum}:$T/next${EXTRA_PATH:+:$EXTRA_PATH}" HOME="$home" TMPDIR="$T/tmp" FAKE_LOG="$T/log" \
    FAKE_RELEASES="$T/rel" FAKE_UNAME_S=Linux FAKE_UNAME_M=x86_64 FAKE_GLIBC=2.35 FAKE_LATEST=v0.2.0 SHELL=/bin/bash \
    "${vars[@]}" "$sh_bin" "$installer" "$@") >"$out" 2>&1
  got=$?
  set -e
  [ "$got" = "$want" ] || note "exit status $got, expected $want"
  if [ -n "$(ls -A "$T/tmp")" ]; then
    note "the temporary directory was left behind: $(ls -A "$T/tmp")"
    rm -rf "${T:?}"/tmp/*
  fi
}

finish() {
  [ -n "$current" ] || return 0
  if [ "$current_ok" = 1 ]; then
    passed=$((passed + 1))
    echo "ok   $current"
  else
    failed=$((failed + 1))
    echo "FAIL $current"
    sed 's/^/    | /' "$out" 2>/dev/null || true
  fi
  current=""
}

says() { grep -qF -- "$1" "$out" || note "the output lacks: $1"; }
says_not() { ! grep -qF -- "$1" "$out" || note "the output has: $1"; }
asked() { grep -qxF -- "https://github.com/memprove/memprove-tune/$1" "$T/log/curl.log" 2>/dev/null || note "curl was not asked for $1"; }
asked_nothing() { [ ! -s "$T/log/curl.log" ] || note "curl was asked for: $(tr '\n' ' ' <"$T/log/curl.log")"; }
not_asked() { ! grep -qF -- "$1" "$T/log/curl.log" 2>/dev/null || note "curl was asked for $1"; }
installed() { # <version> [prefix]
  local bin="${2:-$home/.memprove-tune}/bin/memprove-tune"
  [ -x "$bin" ] || { note "no executable $bin"; return 0; }
  [ "$("$bin" --version)" = "memprove-tune $1" ] || note "$bin says $("$bin" --version), expected memprove-tune $1"
}
nothing_installed() { [ ! -e "${1:-$home/.memprove-tune}" ] || note "${1:-$home/.memprove-tune} exists"; }
no_profile() {
  local f
  for f in .bashrc .bash_profile .zshrc .profile .config/fish/config.fish; do
    [ ! -e "$home/$f" ] || note "$home/$f was written"
  done
}

# ---- platforms
case_ "Linux x86_64 installs the latest release" 0 --
installed 0.2.0
asked releases/latest
asked "releases/download/v0.2.0/memprove-tune-0.2.0-$linux.tar.gz.sha256"
asked "releases/download/v0.2.0/memprove-tune-0.2.0-$linux.tar.gz"
says "SHA-256 matches"
says "installed memprove-tune 0.2.0"
says "$home/.memprove-tune/bin/memprove-tune"
no_profile

case_ "macOS on Apple silicon gets the darwin archive" 0 FAKE_UNAME_S=Darwin FAKE_UNAME_M=arm64 FAKE_GLIBC= --
installed 0.2.0
asked "releases/download/v0.2.0/memprove-tune-0.2.0-$mac.tar.gz"

case_ "a shell under Rosetta gets the Apple silicon archive" 0 FAKE_UNAME_S=Darwin FAKE_UNAME_M=x86_64 FAKE_TRANSLATED=1 FAKE_GLIBC= --
asked "releases/download/v0.2.0/memprove-tune-0.2.0-$mac.tar.gz"

case_ "an Intel Mac is told to build from source" 1 FAKE_UNAME_S=Darwin FAKE_UNAME_M=x86_64 FAKE_GLIBC= --
says "no release archive for macOS on x86_64"
says "cargo install --locked --git https://github.com/memprove/memprove-tune memprove-tune"
asked_nothing
nothing_installed

case_ "Linux on arm64 is told to build from source" 1 FAKE_UNAME_M=aarch64 --
says "no release archive for Linux on aarch64"
asked_nothing

case_ "musl is refused" 1 FAKE_GLIBC= FAKE_MUSL=1 --
says "musl"
asked_nothing

case_ "a glibc older than 2.35 is refused" 1 FAKE_GLIBC=2.31 --
says "glibc 2.31"
asked_nothing

case_ "a glibc version that cannot be read is a warning" 0 FAKE_GLIBC= --
says "warning: could not read the glibc version"
installed 0.2.0

case_ "Windows is refused" 1 FAKE_UNAME_S=MINGW64_NT-10.0 --
says "no release archive for Windows"
asked_nothing

# ---- versions
case_ "MEMPROVE_TUNE_VERSION picks the release and asks for no latest" 0 MEMPROVE_TUNE_VERSION=0.1.0 --
installed 0.1.0
not_asked releases/latest

case_ "MEMPROVE_TUNE_VERSION may carry the v" 0 MEMPROVE_TUNE_VERSION=v0.1.0 --
installed 0.1.0

case_ "a version that is no version is a usage error" 2 MEMPROVE_TUNE_VERSION=latest-ish --
says "MEMPROVE_TUNE_VERSION must be a release version"
asked_nothing
nothing_installed

case_ "a version with a path in it is a usage error" 2 MEMPROVE_TUNE_VERSION=0.1.0/../x --
asked_nothing

case_ "a release that does not exist says so" 1 MEMPROVE_TUNE_VERSION=9.9.9 --
says "is there a release v9.9.9?"
nothing_installed

case_ "no answer about the latest release says how to name one" 1 FAKE_LATEST= --
says "set MEMPROVE_TUNE_VERSION"
nothing_installed

# ---- the checks before anything is installed
release 0.3.0 "$linux"
printf '%064d  memprove-tune-0.3.0-%s.tar.gz\n' 0 "$linux" >"$T/rel/v0.3.0/memprove-tune-0.3.0-$linux.tar.gz.sha256"
case_ "a SHA-256 that does not match installs nothing" 1 MEMPROVE_TUNE_VERSION=0.3.0 --
says "the download is damaged or was changed; nothing was installed"
nothing_installed

release 0.3.1 "$linux"
sed -i 's/memprove-tune-0.3.1/memprove-tune-0.3.0/' "$T/rel/v0.3.1/memprove-tune-0.3.1-$linux.tar.gz.sha256"
case_ "a .sha256 that names another file installs nothing" 1 MEMPROVE_TUNE_VERSION=0.3.1 --
says "names 'memprove-tune-0.3.0-$linux.tar.gz'"
nothing_installed

release 0.3.2 "$linux"
echo "not a hash" >"$T/rel/v0.3.2/memprove-tune-0.3.2-$linux.tar.gz.sha256"
case_ "a .sha256 without a SHA-256 installs nothing" 1 MEMPROVE_TUNE_VERSION=0.3.2 --
says "does not hold a SHA-256"
nothing_installed

mkdir -p "$T/build/memprove-tune-0.3.3-$linux" "$T/rel/v0.3.3"
printf 'readme\n' >"$T/build/memprove-tune-0.3.3-$linux/README.md"
tar -czf "$T/rel/v0.3.3/memprove-tune-0.3.3-$linux.tar.gz" -C "$T/build" "memprove-tune-0.3.3-$linux"
(cd "$T/rel/v0.3.3" && sha256sum "memprove-tune-0.3.3-$linux.tar.gz" >"memprove-tune-0.3.3-$linux.tar.gz.sha256")
case_ "an archive without the binary installs nothing" 1 MEMPROVE_TUNE_VERSION=0.3.3 --
says "has no memprove-tune-0.3.3-$linux/bin/memprove-tune"
nothing_installed

case_ "a binary that does not run here installs nothing" 1 FAKE_BROKEN_BINARY=1 --
says "does not run on this machine; nothing was installed"
nothing_installed

release 0.3.4 "$linux" 0.3.3
case_ "a binary that says another version installs nothing" 1 MEMPROVE_TUNE_VERSION=0.3.4 --
says "says 'memprove-tune 0.3.3', not 'memprove-tune 0.3.4'"
nothing_installed

for h in shasum openssl; do
  HASHER=$h case_ "the SHA-256 is checked with $h" 0 --
  installed 0.2.0
  says "SHA-256 matches"
done

HASHER=none case_ "without a tool for SHA-256 nothing is installed" 1 --
says "found none of sha256sum, shasum or openssl"
nothing_installed

HASHER=nocurl case_ "a missing curl is named" 1 --
says "needs curl"

# ---- where it goes
case_ "--prefix DIR installs there" 0 -- --prefix "$T/p1"
installed 0.2.0 "$T/p1"
nothing_installed
rm -rf "$T/p1"

case_ "--prefix=DIR installs there" 0 -- "--prefix=$T/p 2"
installed 0.2.0 "$T/p 2"
rm -rf "$T/p 2"

case_ "--prefix without a value is a usage error" 2 -- --prefix
says "--prefix requires a value"
asked_nothing

case_ "a relative --prefix is a usage error" 2 -- --prefix some/dir
says "--prefix must be an absolute path"
asked_nothing

case_ "an unknown argument is a usage error" 2 -- --frobnicate
says "unknown argument: --frobnicate"
asked_nothing

case_ "--help prints the usage and asks for nothing" 0 -- --help
says "Usage:"
says "MEMPROVE_TUNE_VERSION"
says "cargo install --locked --git https://github.com/memprove/memprove-tune memprove-tune"
asked_nothing
nothing_installed

# ---- upgrade
case_ "an older installation: first 0.1.0" 0 MEMPROVE_TUNE_VERSION=0.1.0 --
installed 0.1.0
KEEP_HOME=1 case_ "an older installation is replaced and named" 0 --
installed 0.2.0
says "replaced memprove-tune 0.1.0"
[ "$(ls -A "$home/.memprove-tune/bin")" = memprove-tune ] || note "bin holds more than the binary: $(ls -A "$home/.memprove-tune/bin" | tr '\n' ' ')"

KEEP_HOME=1 case_ "a failed upgrade leaves the old binary" 1 MEMPROVE_TUNE_VERSION=0.3.0 --
installed 0.2.0

# ---- the shell profile
case_ "without --write-profile the PATH line is printed, not written" 0 --
says "export PATH=\"$home/.memprove-tune/bin:\$PATH\""
says "--write-profile"
no_profile

EXTRA_PATH="$T/onpath/bin" case_ "with the directory on PATH already no line is offered" 0 -- --prefix "$T/onpath"
installed 0.2.0 "$T/onpath"
says_not "export PATH="
rm -rf "$T/onpath"

case_ "--write-profile writes the line for bash on Linux" 0 -- --write-profile
grep -qxF "export PATH=\"$home/.memprove-tune/bin:\$PATH\"" "$home/.bashrc" 2>/dev/null || note ".bashrc lacks the PATH line"
says "$home/.bashrc"

KEEP_HOME=1 case_ "--write-profile a second time keeps one block" 0 -- --write-profile
[ "$(grep -c 'memprove-tune/bin' "$home/.bashrc")" = 1 ] || note ".bashrc has $(grep -c 'memprove-tune/bin' "$home/.bashrc") PATH lines"

case_ "--write-profile for zsh writes .zshrc" 0 FAKE_UNAME_S=Darwin FAKE_UNAME_M=arm64 FAKE_GLIBC= SHELL=/bin/zsh -- --write-profile
grep -qxF "export PATH=\"$home/.memprove-tune/bin:\$PATH\"" "$home/.zshrc" 2>/dev/null || note ".zshrc lacks the PATH line"

finish
home="$T/home-keep"
rm -rf "$home"
mkdir -p "$home"
printf 'alias ll="ls -l"' >"$home/.zshrc"
KEEP_HOME=1 case_ "--write-profile keeps what the profile had, also a last line without a newline" 0 SHELL=/bin/zsh -- --write-profile
[ "$(head -n 1 "$home/.zshrc")" = 'alias ll="ls -l"' ] || note "the first line of .zshrc changed: $(head -n 1 "$home/.zshrc")"
grep -qxF "export PATH=\"$home/.memprove-tune/bin:\$PATH\"" "$home/.zshrc" || note ".zshrc lacks the PATH line"

case_ "--write-profile on macOS bash writes .bash_profile" 0 FAKE_UNAME_S=Darwin FAKE_UNAME_M=arm64 FAKE_GLIBC= -- --write-profile
[ -f "$home/.bash_profile" ] || note "no .bash_profile"
[ ! -e "$home/.bashrc" ] || note ".bashrc was written"

case_ "--write-profile for fish writes its form" 0 SHELL=/usr/bin/fish -- --write-profile
grep -qxF "set -gx PATH $home/.memprove-tune/bin \$PATH" "$home/.config/fish/config.fish" 2>/dev/null || note "config.fish lacks the PATH line"

case_ "--write-profile for another shell prints the line and writes nothing" 0 SHELL=/bin/tcsh -- --write-profile
says "is not zsh, bash or fish"
says "export PATH="
no_profile

case_ "--no-write-profile is accepted and writes nothing" 0 -- --no-write-profile
installed 0.2.0
no_profile

case_ "--write-profile and --no-write-profile exclude each other" 2 -- --write-profile --no-write-profile
says "exclude each other"
asked_nothing

# ---- a download that was cut short runs nothing
lines=$(wc -l <"$installer")
head -n $((lines - 1)) "$installer" >"$T/cut.sh"
finish
current="an installer without its last line does nothing"
current_ok=1
rm -f "$T"/log/*.log
set +e
(cd "$T/cwd" && env -i PATH="$T/bin-sha256sum:$T/next" HOME="$T/home-cut" TMPDIR="$T/tmp" FAKE_LOG="$T/log" FAKE_RELEASES="$T/rel" \
  FAKE_UNAME_S=Linux FAKE_UNAME_M=x86_64 FAKE_GLIBC=2.35 FAKE_LATEST=v0.2.0 "$sh_bin" "$T/cut.sh") >"$out" 2>&1
set -e
asked_nothing
[ ! -e "$T/home-cut" ] || note "something was written to the home directory"
finish

# ---- scripts/check-release-archive.sh: the installer on an archive that is
# on no Release yet. It gets the host's PATH with the fake platform first;
# the fake curl of this file is on it too and must never be asked.
checker="$root/scripts/check-release-archive.sh"
check_() { # <name> <expected status> [VAR=value ...] -- [arguments ...]
  finish
  current=$1
  local want=$2 got vars=()
  shift 2
  while [ "$1" != -- ]; do vars+=("$1"); shift; done
  shift
  rm -f "$T"/log/*.log "$out"
  home="$T/home-$((passed + failed))"
  rm -rf "$home"
  mkdir -p "$home"
  current_ok=1
  set +e
  (cd "$T/cwd" && env PATH="$fakes:$PATH" HOME="$home" TMPDIR="$T/tmp" FAKE_LOG="$T/log" FAKE_RELEASES="$T/rel" \
    FAKE_UNAME_S=Linux FAKE_UNAME_M=x86_64 FAKE_GLIBC=2.35 "${vars[@]}" "$sh_bin" "$checker" "$@") >"$out" 2>&1
  got=$?
  set -e
  [ "$got" = "$want" ] || note "exit status $got, expected $want"
  if [ -n "$(ls -A "$T/tmp")" ]; then
    note "the temporary directory was left behind: $(ls -A "$T/tmp")"
    rm -rf "${T:?}"/tmp/*
  fi
  [ -z "$(ls -A "$home")" ] || note "the home directory was written to: $(ls -A "$home")"
}

check_ "the archive check installs a good archive with the installer" 0 -- 0.2.0 "$T/rel/v0.2.0/memprove-tune-0.2.0-$linux.tar.gz"
says "SHA-256 matches"
says "scripts/install.sh installed memprove-tune 0.2.0 from memprove-tune-0.2.0-$linux.tar.gz"
asked_nothing

check_ "the archive check takes the macOS archive on a Mac" 0 FAKE_UNAME_S=Darwin FAKE_UNAME_M=arm64 FAKE_GLIBC= -- 0.2.0 "$T/rel/v0.2.0/memprove-tune-0.2.0-$mac.tar.gz"
says "installed memprove-tune 0.2.0 from memprove-tune-0.2.0-$mac.tar.gz"

check_ "the archive check fails for an archive of another platform" 1 -- 0.2.0 "$T/rel/v0.2.0/memprove-tune-0.2.0-$mac.tar.gz"
says "memprove-tune-0.2.0-$linux.tar.gz"
asked_nothing

check_ "the archive check fails when the SHA-256 does not match" 1 -- 0.3.0 "$T/rel/v0.3.0/memprove-tune-0.3.0-$linux.tar.gz"
says "the download is damaged or was changed; nothing was installed"

check_ "the archive check fails when the binary says another version" 1 -- 0.3.4 "$T/rel/v0.3.4/memprove-tune-0.3.4-$linux.tar.gz"
says "says 'memprove-tune 0.3.3', not 'memprove-tune 0.3.4'"

check_ "the archive check fails when the version is not the archive's" 1 -- 0.2.1 "$T/rel/v0.2.0/memprove-tune-0.2.0-$linux.tar.gz"
asked_nothing

check_ "the archive check needs the .sha256 beside the archive" 2 -- 0.3.3 "$T/build/no-such.tar.gz"
says "usage:"

check_ "the archive check without arguments prints its usage" 2 --
says "usage:"
finish

echo
echo "$passed passed, $failed failed"
[ "$failed" = 0 ]
