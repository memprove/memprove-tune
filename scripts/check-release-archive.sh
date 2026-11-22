#!/bin/sh
# Installs one release archive with scripts/install.sh on this machine, as
# a user would, before the archive is on any Release.
#
#   scripts/check-release-archive.sh <VERSION> <ARCHIVE.tar.gz>
#
# Only the download is replaced: a stand-in for curl, first on PATH, hands
# the installer that archive and the .sha256 beside it and nothing else,
# and only for the URLs of that version's Release. The installer does the
# rest itself: it picks the archive for this machine, checks the SHA-256,
# runs the binary and installs it, here under a temporary prefix and a
# temporary HOME that are removed at the end.
# .github/workflows/release.yml runs this on each platform before it
# attaches the archive.
#
# Exit status: 0 the installer installed that version from the archive,
# 1 it did not, 2 usage error. POSIX sh.
set -eu

usage() {
  echo "usage: check-release-archive.sh <VERSION> <ARCHIVE.tar.gz> (the archive's .sha256 is beside it)" >&2
  exit 2
}

[ $# -eq 2 ] || usage
version=$1
archive=$2
[ -f "$archive" ] && [ -f "$archive.sha256" ] || usage

here=$(cd "$(dirname "$0")" && pwd)
from=$(cd "$(dirname "$archive")" && pwd)
name=$(basename "$archive")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin" "$work/home"

cat >"$work/bin/curl" <<'STANDIN'
#!/bin/sh
# Stand-in for curl: -o names the file to write, the other argument is the URL.
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    --proto | --retry | -w) shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
case "$url" in
  "https://github.com/memprove/memprove-tune/releases/download/v$CHECK_VERSION/"*) ;;
  *)
    echo "check-release-archive: the installer asked for $url, which is no file of release v$CHECK_VERSION" >&2
    exit 22
    ;;
esac
asked=${url##*/}
case "$asked" in
  "$CHECK_NAME" | "$CHECK_NAME.sha256") ;;
  *)
    echo "check-release-archive: on this machine the installer asked for $asked, not for $CHECK_NAME" >&2
    exit 22
    ;;
esac
cp "$CHECK_FROM/$asked" "$out"
STANDIN
chmod +x "$work/bin/curl"

CHECK_VERSION="$version" CHECK_FROM="$from" CHECK_NAME="$name" HOME="$work/home" PATH="$work/bin:$PATH" \
  MEMPROVE_TUNE_VERSION="$version" sh "$here/install.sh" --prefix "$work/prefix" || exit 1

said=$("$work/prefix/bin/memprove-tune" --version) || exit 1
[ "$said" = "memprove-tune $version" ] || {
  echo "check-release-archive: the installed binary says '$said', not 'memprove-tune $version'" >&2
  exit 1
}
[ -z "$(ls -A "$work/home")" ] || {
  echo "check-release-archive: the installer wrote to the home directory: $(ls -A "$work/home")" >&2
  exit 1
}
echo "check-release-archive: scripts/install.sh installed memprove-tune $version from $name"
