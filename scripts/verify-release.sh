#!/usr/bin/env bash
set -euo pipefail

# verify-release.sh — single gate command for deployment readiness.
#
# Usage: scripts/verify-release.sh <dev|staging|production> [OPTIONS]
#
# Gates (run in order, fail-fast):
#   1. Environment validation (explicit selection, approval for production)
#   2. Config schema validation (company.yaml + env YAML against schema)
#   3. Required assets check (scripts/check-assets.sh <env>)
#   4. Budget margin check (scripts/check-budget.py <env>)
#   5. Critical tests (cargo fmt, clippy, test; non-critical skippable via --skip-tests)
#   6. SAM validate
#   7. SAM build (if --require-sam-build)
#   8. Environment isolation check
#   9. Approval gate (production only, unless --dry-run)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

usage() {
  cat <<'EOF'
Usage: scripts/verify-release.sh <dev|staging|production> [OPTIONS]

Gates in order (fail-fast):
  1. Environment validation
  2. Config schema validation
  3. Required assets check
  4. Budget margin check
  5. Critical tests (Rust fmt, clippy, test; can skip non-critical via --skip-tests)
  6. SAM validate
  7. SAM build (if --require-sam-build)
  8. Environment isolation check
  9. Approval gate (production only, unless --dry-run)

Options:
  --skip-tests         Skip non-critical test suites only (agent-harness unit tests)
  --require-sam-build  Also require successful sam build + build artifacts
  --approve            Required to proceed for production deployment
  --approval-file PATH Override approval file (default: docs/verification/staging-signoff.md)
  --usage-snapshot PATH JSON file with authoritative usage for budget gate
  --dry-run            Run all gates but don't require approval file to exist
  --root DIR           Operate on a copy at DIR instead of the real repo root
EOF
}

main() {
  local env=""
  local skip_tests=false
  local require_sam_build=false
  local approve=false
  local approval_file="docs/verification/staging-signoff.md"
  local usage_snapshot=""
  local dry_run=false
  local root="$REPO_ROOT"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --skip-tests)
        skip_tests=true
        shift
        ;;
      --require-sam-build)
        require_sam_build=true
        shift
        ;;
      --approve)
        approve=true
        shift
        ;;
      --approval-file)
        approval_file="$2"
        shift 2
        ;;
      --usage-snapshot)
        usage_snapshot="$2"
        shift 2
        ;;
      --dry-run)
        dry_run=true
        shift
        ;;
      --root)
        root="$(cd "$2" && pwd)"
        shift 2
        ;;
      --skip-critical)
        echo "FAIL [gate 0]: --skip-critical is not allowed. Refusing." >&2
        exit 2
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      -*)
        echo "unknown option: $1" >&2
        usage >&2
        exit 2
        ;;
      *)
        if [[ -z "$env" ]]; then
          env="$1"
        else
          echo "unexpected argument: $1" >&2
          usage >&2
          exit 2
        fi
        shift
        ;;
    esac
  done

  if [[ -z "$env" ]]; then
    echo "error: environment required (dev, staging, or production)" >&2
    usage >&2
    exit 2
  fi

  # ── Gate 1: Environment validation ──────────────────────────────────────────
  echo "━━━ Gate 1: Environment validation ━━━"
  case "$env" in
    dev)
      echo "environment: development"
      ;;
    staging)
      echo "environment: staging"
      ;;
    production)
      echo "environment: production"
      if [[ "$approve" != "true" ]] && [[ "$dry_run" != "true" ]]; then
        echo "FAIL [gate 1]: production deployment requires --approve" >&2
        exit 1
      fi
      ;;
    *)
      echo "FAIL [gate 1]: unknown environment '$env' (must be dev, staging, or production)" >&2
      exit 1
      ;;
  esac
  echo "PASS [gate 1]: environment '$env' selected"

  # ── Gate 2: Config schema validation ────────────────────────────────────────
  echo ""
  echo "━━━ Gate 2: Config schema validation ━━━"
  _validate_config_schema "$env" "$root"
  echo "PASS [gate 2]: config schema validation"

  # ── Gate 3: Required assets ─────────────────────────────────────────────────
  echo ""
  echo "━━━ Gate 3: Required assets ━━━"
  local assets_args=("$env" "--root" "$root")
  if [[ "$require_sam_build" == "true" ]]; then
    assets_args+=("--require-sam-build")
  fi
  if ! "$SCRIPT_DIR/check-assets.sh" "${assets_args[@]}"; then
    echo "FAIL [gate 3]: missing or empty required assets" >&2
    exit 1
  fi
  echo "PASS [gate 3]: all required assets present"

  # ── Gate 4: Budget margin ───────────────────────────────────────────────────
  echo ""
  echo "━━━ Gate 4: Budget margin ━━━"
  local budget_args=("$env" "--config-dir" "$root/config" "--root" "$root")
  if [[ -n "$usage_snapshot" ]]; then
    budget_args+=("--usage-snapshot" "$usage_snapshot")
  fi
  if ! python3 "$SCRIPT_DIR/check-budget.py" "${budget_args[@]}"; then
    echo "FAIL [gate 4]: insufficient budget margin" >&2
    exit 1
  fi
  echo "PASS [gate 4]: budget margin sufficient"

  # ── Gate 5: Critical tests ──────────────────────────────────────────────────
  echo ""
  echo "━━━ Gate 5: Critical tests ━━━"
  cd "$root"

  # Detect if the compact rust wrappers are available (local pi environment);
  # CI uses raw cargo, which always works.
  local rust_compact_dir="$HOME/.pi/agent/skills/rust-compact/scripts"
  local rust_test_cmd="cargo test"
  local rust_clippy_cmd="cargo clippy"

  if [[ -x "$rust_compact_dir/rust_test" ]]; then
    rust_test_cmd="$rust_compact_dir/rust_test"
  fi
  if [[ -x "$rust_compact_dir/rust_clippy" ]]; then
    rust_clippy_cmd="$rust_compact_dir/rust_clippy"
  fi

  echo "  cargo fmt --all -- --check"
  cargo fmt --all -- --check || {
    echo "FAIL [gate 5]: cargo fmt check failed" >&2
    exit 1
  }

  echo "  cargo clippy --workspace --all-targets --all-features -- -D warnings"
  $rust_clippy_cmd --workspace --all-targets --all-features -- -D warnings 2>&1 || {
    echo "FAIL [gate 5]: clippy check failed" >&2
    exit 1
  }

  echo "  cargo test --workspace --all-features"
  $rust_test_cmd --workspace --all-features 2>&1 || {
    echo "FAIL [gate 5]: tests failed" >&2
    exit 1
  }

  # Non-critical tests (skippable)
  if [[ "$skip_tests" != "true" ]]; then
    echo "  pnpm --filter @novus/agent-harness test --run (non-critical)"
    if command -v pnpm &>/dev/null || command -v corepack &>/dev/null; then
      local pnpm_cmd="pnpm"
      if ! command -v pnpm &>/dev/null; then
        pnpm_cmd="corepack pnpm"
      fi
      $pnpm_cmd --filter @novus/agent-harness test --run 2>&1 || {
        echo "WARNING [gate 5]: agent-harness tests failed (non-critical, continuing)" >&2
      }
      echo "  pnpm --filter @novus/agent-harness build (non-critical)"
      $pnpm_cmd --filter @novus/agent-harness build 2>&1 || {
        echo "WARNING [gate 5]: agent-harness build failed (non-critical, continuing)" >&2
      }
    else
      echo "  (skipping non-critical TS tests: pnpm not available)"
    fi
  else
    echo "  (non-critical tests skipped via --skip-tests)"
  fi

  echo "PASS [gate 5]: critical tests passed"

  # ── Gate 6: SAM validate ────────────────────────────────────────────────────
  echo ""
  echo "━━━ Gate 6: SAM validate ━━━"
  if ! command -v sam &>/dev/null; then
    echo "FAIL [gate 6]: sam CLI not found" >&2
    exit 1
  fi
  sam validate --lint --template-file "$root/infrastructure/template.yaml" || {
    echo "FAIL [gate 6]: SAM template validation failed" >&2
    exit 1
  }
  echo "PASS [gate 6]: SAM template validates"

  # ── Gate 7: SAM build (optional) ────────────────────────────────────────────
  if [[ "$require_sam_build" == "true" ]]; then
    echo ""
    echo "━━━ Gate 7: SAM build ━━━"
    sam build --template-file "$root/infrastructure/template.yaml" || {
      echo "FAIL [gate 7]: SAM build failed" >&2
      exit 1
    }
    if [[ ! -f "$root/.aws-sam/build/template.yaml" ]]; then
      echo "FAIL [gate 7]: SAM build artifacts missing" >&2
      exit 1
    fi
    echo "PASS [gate 7]: SAM build succeeded"
  else
    echo ""
    echo "━━━ Gate 7: SAM build ━━━"
    echo "  (skipped: pass --require-sam-build to require)"
  fi

  # ── Gate 8: Environment isolation check ─────────────────────────────────────
  echo ""
  echo "━━━ Gate 8: Environment isolation ━━━"
  _check_isolation "$env" "$root"
  echo "PASS [gate 8]: environment isolation verified"

  # ── Gate 9: Approval gate (production only) ─────────────────────────────────
  echo ""
  echo "━━━ Gate 9: Approval ━━━"
  case "$env" in
    production)
      if [[ "$dry_run" == "true" ]]; then
        echo "  DRY-RUN: production approval would be required (skipped)"
      elif [[ -f "$root/$approval_file" ]] && [[ -s "$root/$approval_file" ]]; then
        echo "PASS [gate 9]: production approval file present ($approval_file)"
      else
        echo "FAIL [gate 9]: production requires approval file ($approval_file) to exist and be non-empty" >&2
        exit 1
      fi
      ;;
    staging)
      # Staging: approval is optional but nothing blocks without it
      if [[ -f "$root/$approval_file" ]] && [[ -s "$root/$approval_file" ]]; then
        echo "  staging approval file present ($approval_file)"
      else
        echo "  (staging: no approval file, continuing)"
      fi
      echo "PASS [gate 9]: staging approval not required"
      ;;
    dev)
      echo "PASS [gate 9]: dev does not require approval"
      ;;
  esac

  # ── Summary ─────────────────────────────────────────────────────────────────
  echo ""
  echo "╔══════════════════════════════════════╗"
  echo "║  ALL GATES PASSED                    ║"
  echo "║  Environment: $env"
  if [[ "$dry_run" == "true" ]]; then
    echo "║  Mode: dry-run"
  fi
  echo "╚══════════════════════════════════════╝"
  exit 0
}

# ── Helper: Config schema validation ──────────────────────────────────────────

_validate_config_schema() {
  local env="$1"
  local root="$2"

  local schema="$root/config/schema/company.schema.json"
  local base="$root/config/company.yaml"
  local env_file="$root/config/environments/${env}.yaml"

  # Try python jsonschema + pyyaml first; fall back to a structural check.
  if python3 -c "import jsonschema, yaml" 2>/dev/null; then
    python3 <<PYEOF
import json
import sys
import yaml
import jsonschema

schema_path = "$schema"
base_path = "$base"
env_path = "$env_file"

with open(schema_path) as f:
    schema = json.load(f)

# Validate base (kind: company) and env (kind: environment) separately,
# matching the behaviour of crates/application config_loader.
for label, path in (("base", base_path), ("env", env_path)):
    with open(path) as f:
        cfg = yaml.safe_load(f)
    try:
        jsonschema.validate(cfg, schema)
    except jsonschema.ValidationError as e:
        print(f"FAIL [gate 2]: {label} config validation error: {e.message}", file=sys.stderr)
        sys.exit(1)
PYEOF
    local rc=$?
    if [[ $rc -ne 0 ]]; then
      exit $rc
    fi
    return 0
  fi

  # Fallback: structural check only (jsonschema/pyyaml unavailable).
  echo "  (jsonschema not available; performing structural check only)"
  if [[ ! -f "$schema" ]] || [[ ! -s "$schema" ]]; then
    echo "FAIL [gate 2]: schema file missing or empty: $schema" >&2
    exit 1
  fi
  if [[ ! -f "$base" ]] || [[ ! -s "$base" ]]; then
    echo "FAIL [gate 2]: base config missing or empty: $base" >&2
    exit 1
  fi
  if [[ ! -f "$env_file" ]] || [[ ! -s "$env_file" ]]; then
    echo "FAIL [gate 2]: env config missing or empty: $env_file" >&2
    exit 1
  fi
}

# ── Helper: Environment isolation check ───────────────────────────────────────

_check_isolation() {
  local env="$1"
  local root="$2"

  local env_file="$root/config/environments/${env}.yaml"

  # Map env arg to expected environment field value
  local expected_env
  case "$env" in
    dev) expected_env="development" ;;
    staging) expected_env="staging" ;;
    production) expected_env="production" ;;
  esac

  # Verify the environment field in the env config matches
  local actual_env
  actual_env=$(grep -E '^environment:' "$env_file" | head -1 | sed 's/^environment: *//' | tr -d '"'"'" | xargs)
  if [[ "$actual_env" != "$expected_env" ]]; then
    echo "FAIL [gate 8]: env config declares environment '$actual_env', expected '$expected_env'" >&2
    exit 1
  fi

  # Verify secret refs are scoped to /novus/<env>/...
  local prefix="/novus/${expected_env}/"
  local bad_refs
  bad_refs=$(grep -E 'secretRef:|tokenRef:|SecretRef:' "$env_file" | grep -v "$prefix" || true)
  if [[ -n "$bad_refs" ]]; then
    echo "FAIL [gate 8]: cross-environment secret refs detected:" >&2
    echo "$bad_refs" >&2
    exit 1
  fi

  echo "  environment field: $actual_env ✓"
  echo "  secret refs scoped to: $prefix ✓"
}

main "$@"
