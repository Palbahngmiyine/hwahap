#!/usr/bin/env bash
# The static simplicity gates.
#
# Hwahap v4's design is a set of numbers: three tools, three profiles, one session, no daemon, no
# database. Numbers drift silently unless something counts them, so this script counts them and
# fails the build when one moves. Every gate here corresponds to a line in the current design.
#
# Run from the repository root: tests/gates.sh
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
skill_dir="$root/plugins/hwahap"
src="$skill_dir/runtime/src"
failures=0

fail() {
  printf 'GATE FAILED: %s\n' "$1" >&2
  failures=$((failures + 1))
}

pass() {
  printf 'ok  %s\n' "$1"
}

expect_count() {
  local label=$1 expected=$2 actual=$3
  if [ "$actual" = "$expected" ]; then
    pass "$label = $expected"
  else
    fail "$label is $actual, expected $expected"
  fi
}

# Counts matches of an extended regex in production code only.
#
# "Production" means everything above the file's `#[cfg(test)]` marker, with comment lines removed.
# Grepping the whole file would count the tests that deliberately mention a rejected shape — the
# test asserting that a v2 directory is refused has to name `hwahap/v2` in order to assert it.
production_matches() {
  local pattern=$1 total=0 file count
  while IFS= read -r file; do
    count=$(
      awk '/^#\[cfg\(test\)\]/ { exit } { print }' "$file" \
        | grep -vE '^[[:space:]]*(//|///|//!)' \
        | grep -cE "$pattern" || true
    )
    total=$((total + count))
  done < <(find "$src" -type f -name '*.rs')
  printf '%s' "$total"
}

# ---------------------------------------------------------------- surface size

skill="$skill_dir/skills/hwahap/SKILL.md"
if [ ! -f "$skill" ]; then
  fail "the thin skill is missing at $skill"
else
  lines=$(wc -l <"$skill" | tr -d ' ')
  if [ "$lines" -le 40 ]; then
    pass "SKILL.md is $lines physical lines (<= 40)"
  else
    fail "SKILL.md is $lines physical lines, and the gate is 40"
  fi
fi

tools=$(grep -cE '^\s*name = "hwahap_' "$src/mcp.rs" || true)
expect_count "MCP tool count" 3 "$tools"

read_only=$(grep -cE '^\s*read_only_hint = true' "$src/mcp.rs" || true)
expect_count "read-only tools" 1 "$read_only"

for forbidden in hwahap_plan hwahap_cycle hwahap_adjust hwahap_retry hwahap_create_unit hwahap_spawn_worker hwahap_integrate; do
  if grep -q "\"$forbidden\"" "$src"/*.rs 2>/dev/null; then
    fail "the tool $forbidden exists; the three-tool surface is the contract"
  fi
done
pass "no scheduling or approval tool was added"

# ------------------------------------------------------------- removed v2 shape

expect_count "production codex exec references" 0 "$(production_matches 'codex[[:space:]]+exec')"
expect_count "lifecycle hook definitions" 0 "$(find "$skill_dir" -name 'hooks.json' -o -name 'pretool.sh' -o -name 'posttool.sh' -o -name 'prompt.sh' -o -name 'gate.sh' | wc -l | tr -d ' ')"
expect_count "jq programs" 0 "$(find "$skill_dir" -name '*.jq' | wc -l | tr -d ' ')"

expect_count "production hwahap/v2 references" 0 "$(production_matches '"hwahap/v2"')"

# ------------------------------------------------------------------ dependencies

manifest="$skill_dir/runtime/Cargo.toml"
lock="$skill_dir/runtime/Cargo.lock"
for banned in rusqlite libsqlite3-sys sqlx diesel sea-orm; do
  if grep -qE "^name = \"$banned\"" "$lock" 2>/dev/null; then
    fail "the dependency graph contains $banned; Hwahap v4 has no database"
  fi
done
pass "SQLite dependencies = 0"

for banned in axum warp actix-web hyper-util tiny_http rocket; do
  if grep -qE "^name = \"$banned\"" "$lock" 2>/dev/null; then
    fail "the dependency graph contains the HTTP server crate $banned"
  fi
done
pass "HTTP server dependencies = 0"

if grep -qE 'transport-streamable-http|server-side-http' "$manifest"; then
  fail "an HTTP MCP transport feature is enabled; Hwahap v4 is local STDIO only"
fi
pass "MCP transport is local STDIO only"

if grep -qE 'unstable' "$manifest"; then
  fail "an unstable ACP feature is enabled in Cargo.toml"
fi
expect_count "ACP unstable feature references" 0 "$(production_matches 'unstable_')"

expect_count "daemon or service definitions" 0 "$(find "$skill_dir" -name '*.service' -o -name '*.plist' -o -name 'launchd*' | wc -l | tr -d ' ')"

# ------------------------------------------------------------- effort policy

# The whole policy is what `Profiles::defaults()` returns, so that is what gets counted.
defaults_block=$(awk '/pub fn defaults\(\)/,/^    }$/' "$src/profile.rs")
expect_count "model-effort profiles" 3 "$(printf '%s\n' "$defaults_block" | grep -cE '^\s+(economy|critic|deep): ProfileSpec' || true)"

check_pair() {
  local profile=$1 model=$2 effort=$3
  local block
  block=$(printf '%s\n' "$defaults_block" | grep -A3 -E "^\s+$profile: ProfileSpec")
  if printf '%s\n' "$block" | grep -q "\"$model\"" \
    && printf '%s\n' "$block" | grep -q "Effort::$effort"; then
    pass "$profile = $model / $effort"
  else
    fail "$profile is not pinned to $model / Effort::$effort"
  fi
}
check_pair economy gpt-5.6-luna Medium
check_pair critic gpt-6-astra High
check_pair deep gpt-6-astra High

for banned in None Low Max Ultra; do
  if grep -qE "^\s+$banned,\s*$" "$src/profile.rs"; then
    fail "Effort::$banned exists; the policy forbids it and the type must make it unrepresentable"
  fi
done
pass "none, low, max and ultra are unrepresentable efforts"

if [ -f "$src/acp.rs" ] || grep -q 'agent-client-protocol' "$manifest"; then
  fail "the removed ACP runtime or dependency is present"
fi
if ! grep -q 'receipt.verify_for' "$src/engine.rs"; then
  fail "native receipts are not checked against the requested role and profile"
fi
pass "native request evidence is validated without claiming an applied-model echo"

# Comparing them is only half the promise: the run has to *stop*. An earlier version of this gate
# checked only that the words appeared in acp.rs, and passed while `is_terminal_for_run` had no
# production caller at all and the error escaped as a protocol failure.
if ! grep -q 'is_terminal_for_run' "$src/engine.rs"; then
  fail "engine.rs never asks whether an error should stop the run, so nothing becomes blocked"
fi
if ! grep -q 'RunState::Blocked' "$src/engine.rs"; then
  fail "engine.rs never persists a blocked run"
fi
pass "an unrecoverable error is written down as a blocked run"

# -------------------------------------------------------- one session at a time

if ! grep -q 'RepoLock::acquire' "$src/native/host.rs"; then
  fail "native host does not lock execution across MCP processes"
fi
pass "concurrent step and ship calls are serialized"

echo
if [ "$failures" -gt 0 ]; then
  printf '%d gate(s) failed\n' "$failures" >&2
  exit 1
fi
echo "all gates passed"
