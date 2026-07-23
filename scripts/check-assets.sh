#!/usr/bin/env bash
set -euo pipefail

# check-assets.sh — required-asset gate for release verification.
#
# Usage: scripts/check-assets.sh <environment> [--require-sam-build] [--root DIR]
#
# Checks every required file exists and is non-empty. Exits 1 on any
# missing/empty asset.

main() {
  local env=""
  local require_sam_build=false
  local root="."

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --require-sam-build)
        require_sam_build=true
        shift
        ;;
      --root)
        root="$2"
        shift 2
        ;;
      -*)
        echo "unknown option: $1" >&2
        exit 2
        ;;
      *)
        if [[ -z "$env" ]]; then
          env="$1"
        else
          echo "unexpected argument: $1" >&2
          exit 2
        fi
        shift
        ;;
    esac
  done

  if [[ -z "$env" ]]; then
    echo "usage: scripts/check-assets.sh <dev|staging|production> [--require-sam-build] [--root DIR]" >&2
    exit 2
  fi

  case "$env" in
    dev|staging|production) ;;
    *)
      echo "error: environment must be dev, staging, or production" >&2
      exit 2
      ;;
  esac

  if [[ ! -d "$root" ]]; then
    echo "error: root directory not found: $root" >&2
    exit 2
  fi

  # Normalize root to absolute
  root="$(cd "$root" && pwd)"

  local -a assets=(
    "docs/sample-quotation.pdf"
    "docs/quotation-layout.md"
    "config/company.yaml"
    "config/schema/company.schema.json"
    "config/environments/${env}.yaml"
    "infrastructure/template.yaml"
    "infrastructure/monitoring.yaml"
    "infrastructure/security.yaml"
    "docs/verification/prd-traceability.md"
    "docs/cost/aws-monthly-forecast.csv"
    "docs/adr/0002-shared-budget-control.md"
  )

  if [[ "$require_sam_build" == "true" ]]; then
    assets+=(".aws-sam/build/template.yaml")
  fi

  local failed=0
  local passed=0

  for asset in "${assets[@]}"; do
    local full_path="$root/$asset"
    if [[ -f "$full_path" ]] && [[ -s "$full_path" ]]; then
      echo "[PASS] $asset"
      passed=$((passed + 1))
    elif [[ -f "$full_path" ]]; then
      echo "[FAIL] $asset (empty file)" >&2
      failed=$((failed + 1))
    else
      echo "[FAIL] $asset (missing)" >&2
      failed=$((failed + 1))
    fi
  done

  echo ""
  echo "Assets: $passed passed, $failed failed"

  if [[ $failed -gt 0 ]]; then
    exit 1
  fi

  echo "PASS: all required assets present"
  exit 0
}

main "$@"
