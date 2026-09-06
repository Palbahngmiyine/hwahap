#!/usr/bin/env bash
# Package the checked-out release, then run its launcher from an extracted copy.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:?usage: tests/package.sh RUST_TARGET}
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-apple-darwin) ;;
  *) echo "unsupported release target: $target" >&2; exit 1 ;;
esac
version=$(cat version.txt)
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
binary="runtime/target/$target/release/hwahap"
test -x "$binary"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/hwahap/runtime/target/release" dist
# An explicit list excludes Git credentials, run state, and build intermediates.
git archive HEAD SKILL.md README.md RELEASING.md OPERATIONS.md PLATFORM.md USAGE.md version.txt bin runtime tests | tar -x -C "$work/hwahap"
cp "$binary" "$work/hwahap/runtime/target/release/hwahap"
archive="hwahap-v$version-$target.tar.gz"
tar -czf "dist/$archive" -C "$work" hwahap
mkdir "$work/extracted"
tar -xzf "dist/$archive" -C "$work/extracted"
test "$("$work/extracted/hwahap/bin/hwahap" --version)" = "hwahap $version"
(cd dist && shasum -a 256 "$archive" > "$archive.sha256")
echo "Verified dist/$archive"
