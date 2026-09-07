#!/usr/bin/env bash
# Package a Git snapshot as an installable marketplace and a first-run binary.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${1:?usage: tests/package.sh RUST_TARGET [SNAPSHOT_REF]}
snapshot=$(git rev-parse --verify --end-of-options "${2:-HEAD}^{commit}")
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "unsupported release target: $target" >&2; exit 1 ;;
esac
version=$(git show "$snapshot:plugins/hwahap/version.txt")
binary="plugins/hwahap/runtime/target/$target/release/hwahap"
test "$("$binary" --version)" = "hwahap $version"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/hwahap" dist
git archive "$snapshot" .agents/plugins plugins/hwahap | tar -x -C "$work/hwahap"
plugin="$work/hwahap/plugins/hwahap"
mkdir -p "$plugin/runtime/target/release"
cp "$binary" "$plugin/runtime/target/release/hwahap"
archive="hwahap-v$version-$target.tar.gz"
tar -czf "dist/$archive" -C "$work" hwahap
mkdir "$work/extracted"
tar -xzf "dist/$archive" -C "$work/extracted"
test "$(HWAHAP_OFFLINE=1 "$work/extracted/hwahap/plugins/hwahap/bin/hwahap" --version)" = "hwahap $version"
gzip -n -c "$binary" > "dist/hwahap-v$version-$target.gz"
(cd dist && shasum -a 256 "$archive" > "$archive.sha256" && shasum -a 256 "hwahap-v$version-$target.gz" > "hwahap-v$version-$target.gz.sha256")
echo "Verified $archive and standalone runtime"
