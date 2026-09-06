#!/usr/bin/env bash
# Exercise failed download recovery and offline reuse without network access.
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/plugin/bin" "$work/tools" "$work/fixture"
cp plugins/hwahap/bin/* "$work/plugin/bin/"
cp plugins/hwahap/version.txt "$work/plugin/"
version=$(cat "$work/plugin/version.txt")
printf '#!/bin/sh\nprintf "hwahap %s\\n"\n' "$version" > "$work/fixture/runtime"
gzip -c "$work/fixture/runtime" > "$work/fixture/runtime.gz"
shasum -a 256 "$work/fixture/runtime.gz" > "$work/fixture/checksum"
cat > "$work/tools/curl" <<'STUB'
#!/usr/bin/env bash
set -eu
source=runtime.gz
for arg in "$@"; do case "$arg" in *.sha256) source=checksum ;; esac; done
while [ "$1" != -o ]; do shift; done
cp "$FIXTURE/$source" "$2"
STUB
chmod +x "$work/tools/curl"
export PATH="$work/tools:$PATH" FIXTURE="$work/fixture" PLUGIN_DATA="$work/cache"
printf '%064d  runtime.gz\n' 0 > "$work/fixture/checksum"
if "$work/plugin/bin/hwahap" --version > "$work/stdout" 2> "$work/stderr"; then exit 1; fi
test ! -s "$work/stdout"
grep -q 'checksum mismatch' "$work/stderr"
test -z "$(find "$work/cache" -name hwahap -type f)"
shasum -a 256 "$work/fixture/runtime.gz" > "$work/fixture/checksum"
test "$("$work/plugin/bin/hwahap" --version)" = "hwahap $version"
rm "$work/tools/curl"
test "$(HWAHAP_OFFLINE=1 "$work/plugin/bin/hwahap" --version)" = "hwahap $version"
echo 'download rejection, recovery, and offline reuse passed'
