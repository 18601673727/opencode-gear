#!/usr/bin/env bash
# CLI smoke tests for the `oc` entry point.
#
# Run with:  bash tests/test_cli.sh      (or: make test)
#
# These tests never launch OpenCode: `--dry-run` prints the merged config and
# OC_GEAR_OPENCODE_BIN is pointed at a harmless command.
set -uo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
OC="$ROOT/bin/oc"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

export OC_GEAR_OPENCODE_BIN=false
export OC_GEAR_USER_CONFIG="$TMP/no-user-config.json"
unset OC_GEAR_PROJECT_CONFIG 2>/dev/null || true
export PATH="$ROOT/bin:$PATH"

# Run from an empty directory so the repo's own config cannot leak in.
cd "$TMP"

PASS=0
FAIL=0

ok()   { PASS=$((PASS + 1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$1"; }

expect_eq() { # name expected actual
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$2', got '$3')"; fi
}

expect_contains() { # name needle haystack
  case "$3" in
    *"$2"*) ok "$1" ;;
    *) bad "$1 (missing '$2')" ;;
  esac
}

expect_fail() { # name command...
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then bad "$name (expected non-zero exit)"; else ok "$name"; fi
}

json_field() { # field  (reads JSON on stdin)
  python3 -c "import json,sys; print(json.load(sys.stdin).get('$1',''))"
}

echo "oc CLI tests"

# --- version / help --------------------------------------------------------
expect_contains "version" "OpenCode Gear" "$("$OC" version)"
expect_contains "help exits zero" "Usage:" "$("$OC" help)"

# --- dry run ---------------------------------------------------------------
out="$("$OC" --dry-run)"
expect_eq "dry-run default agent is lead-low" "lead-low" "$(printf '%s' "$out" | json_field default_agent)"
expect_eq "dry-run default model is sol" "openai/gpt-5.6-sol" "$(printf '%s' "$out" | json_field model)"

out="$("$OC" --throttle high --dry-run)"
expect_eq "--throttle high selects lead-high" "lead-high" "$(printf '%s' "$out" | json_field default_agent)"
expect_eq "--throttle high selects astra" "openai/gpt-6-astra" "$(printf '%s' "$out" | json_field model)"

out="$("$OC" --throttle=mid --dry-run)"
expect_eq "--throttle=mid selects lead-mid" "lead-mid" "$(printf '%s' "$out" | json_field default_agent)"

out="$(OC_GEAR_THROTTLE=high "$OC" --dry-run)"
expect_eq "OC_GEAR_THROTTLE selects lead-high" "lead-high" "$(printf '%s' "$out" | json_field default_agent)"

out="$(OC_GEAR_THROTTLE=high "$OC" --throttle low --dry-run)"
expect_eq "--throttle beats env" "lead-low" "$(printf '%s' "$out" | json_field default_agent)"

# consumer routing is independent of throttle
out="$("$OC" --throttle high --dry-run)"
expect_eq "explore stays on volcano at throttle high" "volcengine-coding/kimi-k2.7-code" \
  "$(printf '%s' "$out" | python3 -c "import json,sys; print(json.load(sys.stdin)['agent']['ocg-explore']['model'])")"
expect_eq "build stays on deepseek at throttle high" "opencode-go/deepseek-v4.1-flash" \
  "$(printf '%s' "$out" | python3 -c "import json,sys; print(json.load(sys.stdin)['agent']['ocg-build']['model'])")"

# --- subcommands -----------------------------------------------------------
expect_contains "status shows throttle" "Throttle (OpenAI Lead tier)" "$("$OC" status)"
expect_contains "routing shows kimi" "kimi-k2.7-code" "$("$OC" routing)"
expect_contains "validate passes" "configuration is valid" "$("$OC" validate)"
expect_contains "layers shows gear home" "gear home" "$("$OC" layers)"

# --- project override ------------------------------------------------------
proj="$TMP/project"
mkdir -p "$proj"
cat >"$proj/.opencode-gear.json" <<'JSON'
{
  "throttle": { "default": "mid" },
  "routing": { "roles": { "build": { "model": "glm-5.3", "variant": "high" } } }
}
JSON
out="$("$OC" --project "$proj" --dry-run)"
expect_eq "project override changes default throttle" "lead-mid" "$(printf '%s' "$out" | json_field default_agent)"
expect_eq "project override changes build model" "opencode-go/glm-5.3" \
  "$(printf '%s' "$out" | python3 -c "import json,sys; print(json.load(sys.stdin)['agent']['ocg-build']['model'])")"
expect_eq "project override does not leak into cwd" "lead-low" \
  "$("$OC" --dry-run | json_field default_agent)"

# --- failure modes ---------------------------------------------------------
cat >"$proj/bad.json" <<'JSON'
{ "routing": { "roles": { "build": { "model": "does-not-exist" } } } }
JSON
expect_fail "unknown model in override fails" env OC_GEAR_PROJECT_CONFIG="$proj/bad.json" "$OC" --dry-run

expect_fail "unknown command fails" "$OC" not-a-command
expect_fail "unknown option fails" "$OC" --nope
expect_fail "--throttle without value fails" "$OC" --throttle
expect_fail "--project without directory fails" "$OC" --project "$TMP/missing-dir" --dry-run

echo
echo "passed: $PASS   failed: $FAIL"
[ "$FAIL" -eq 0 ]
