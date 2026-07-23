#!/usr/bin/env bash
set -euo pipefail

# verify-release-selftest.sh — proves that verify-release.sh blocks correctly.
#
# Forces each gate to fail and asserts verify-release.sh returns non-zero.
# Does NOT mutate the real working tree — operates in a temp copy.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY_RELEASE="$SCRIPT_DIR/verify-release.sh"

TEMP_ROOT=""
PASSED=0
FAILED=0

cleanup() {
  if [[ -n "$TEMP_ROOT" ]] && [[ -d "$TEMP_ROOT" ]]; then
    rm -rf "$TEMP_ROOT"
  fi
}
trap cleanup EXIT

pass_case() {
  echo "  [PASS] $1"
  PASSED=$((PASSED + 1))
}

fail_case() {
  echo "  [FAIL] $1" >&2
  FAILED=$((FAILED + 1))
}

run_gate() {
  # Run verify-release.sh and capture exit code.
  # Returns the exit code.
  set +e
  "$VERIFY_RELEASE" "$@" >/dev/null 2>&1
  local rc=$?
  set -e
  return $rc
}

echo "=== verify-release self-test ==="
echo ""

# ── Setup: copy repo to temp dir ──────────────────────────────────────────────
# Only the directories the exercised gates (1-4) touch are copied: config/,
# docs/, infrastructure/, and scripts/. Build artifacts (target/, node_modules/,
# .aws-sam/build/) are excluded so the selftest stays fast and never exhausts a
# small tmpfs. Gate 5 (cargo) is never reached because every case fails earlier.
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# Prefer the large root filesystem for the temp copy so a small /tmp tmpfs
# cannot cause ENOSPC; fall back to the system default.
if mkdir -p "$REPO_ROOT/.selftest-tmp" 2>/dev/null; then
  TEMP_ROOT="$(mktemp -d "$REPO_ROOT/.selftest-tmp/tmp.XXXXXX")"
else
  TEMP_ROOT="$(mktemp -d)"
fi
echo "  temp root: $TEMP_ROOT"

mkdir -p "$TEMP_ROOT"
for d in config docs infrastructure scripts; do
  if [[ -d "$REPO_ROOT/$d" ]]; then
    cp -a "$REPO_ROOT/$d" "$TEMP_ROOT/$d"
  fi
done

# Ensure .aws-sam/build/template.yaml exists for any --require-sam-build path.
mkdir -p "$TEMP_ROOT/.aws-sam/build"
touch "$TEMP_ROOT/.aws-sam/build/template.yaml"

# ── Test 1: Unknown environment ───────────────────────────────────────────────
echo ""
echo "Test 1: Unknown environment 'foo'"
if run_gate foo --root "$TEMP_ROOT" --approve --dry-run; then
  fail_case "Test 1: unknown environment should have been rejected"
else
  pass_case "Test 1: unknown environment correctly rejected"
fi

# ── Test 2: Production without --approve ──────────────────────────────────────
echo ""
echo "Test 2: Production without --approve"
if run_gate production --root "$TEMP_ROOT"; then
  fail_case "Test 2: production without --approve should have been rejected"
else
  pass_case "Test 2: production without --approve correctly rejected"
fi

# ── Test 3: --skip-critical refused ───────────────────────────────────────────
echo ""
echo "Test 3: --skip-critical refusal"
if "$VERIFY_RELEASE" dev --root "$TEMP_ROOT" --skip-critical --dry-run >/dev/null 2>&1; then
  fail_case "Test 3: --skip-critical should have been refused"
else
  pass_case "Test 3: --skip-critical correctly refused"
fi

# ── Test 4: Missing asset ─────────────────────────────────────────────────────
echo ""
echo "Test 4: Missing asset"
# Remove a required asset from the temp copy
rm -f "$TEMP_ROOT/docs/sample-quotation.pdf"
if run_gate dev --root "$TEMP_ROOT" --skip-tests --dry-run; then
  fail_case "Test 4: missing asset should have been rejected"
else
  pass_case "Test 4: missing asset correctly rejected"
fi
# Restore for subsequent tests
cp -a "$REPO_ROOT/docs/sample-quotation.pdf" "$TEMP_ROOT/docs/sample-quotation.pdf"

# ── Test 5: Insufficient budget margin ────────────────────────────────────────
echo ""
echo "Test 5: Insufficient budget margin (via large usage snapshot)"
# Create a usage snapshot with settled costs exceeding the cap
USAGE_FILE="$TEMP_ROOT/test-usage-over-cap.json"
cat > "$USAGE_FILE" <<'JSONEOF'
{
  "invoice_month": "2026-07",
  "settled_micro_inr": 500000000,
  "reserved_micro_inr": 0,
  "reconciled_micro_inr": 0
}
JSONEOF
if run_gate dev --root "$TEMP_ROOT" --skip-tests --dry-run --usage-snapshot "$USAGE_FILE"; then
  fail_case "Test 5: over-cap usage snapshot should have been rejected"
else
  pass_case "Test 5: over-cap usage snapshot correctly rejected"
fi
rm -f "$USAGE_FILE"

# ── Test 6: Stale invoice month ───────────────────────────────────────────────
echo ""
echo "Test 6: Stale invoice month"
STALE_FILE="$TEMP_ROOT/test-stale-usage.json"
cat > "$STALE_FILE" <<'JSONEOF'
{
  "invoice_month": "2020-01",
  "settled_micro_inr": 0,
  "reserved_micro_inr": 0,
  "reconciled_micro_inr": 0
}
JSONEOF
if run_gate dev --root "$TEMP_ROOT" --skip-tests --dry-run --usage-snapshot "$STALE_FILE"; then
  fail_case "Test 6: stale invoice month should have been rejected"
else
  pass_case "Test 6: stale invoice month correctly rejected"
fi
rm -f "$STALE_FILE"

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "=== Self-test summary ==="
echo "Passed: $PASSED"
echo "Failed: $FAILED"

if [[ $FAILED -gt 0 ]]; then
  echo "SELFTEST FAILED"
  exit 1
fi

echo "SELFTEST PASSED"
exit 0
