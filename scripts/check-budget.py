#!/usr/bin/env python3
"""Budget margin gate for release verification.

Reads the company budget configuration and the AWS monthly cost forecast,
then asserts that projected monthly run-rate leaves enough headroom below
the configured monthly cap.

Usage:
    scripts/check-budget.py <environment> [--usage-snapshot PATH] [--config-dir DIR]
                            [--margin-floor-micro-inr MICRO_INR]

Exit 0: budget margin is sufficient.
Exit 1: insufficient margin.
Exit 2: configuration or input error.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MICROINR_PER_INR = 1_000_000


def _load_yaml(path: Path) -> dict[str, Any]:
    """Load a YAML file without external dependencies using a minimal inline parser.

    This handles the flat and nested scalar dictionaries used by the company
    and environment configuration files. It does NOT resolve tags, anchors,
    or complex YAML features that the config files don't use.
    """
    import re

    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    root: dict[str, Any] = {}
    stack: list[tuple[dict[str, Any], int]] = [(root, -1)]

    for line in lines:
        stripped = line.rstrip()
        if not stripped or stripped.lstrip().startswith("#"):
            continue

        indent = len(line) - len(line.lstrip())
        key_val = stripped.lstrip()

        # pop stack until we find a parent with lower indent
        while len(stack) > 1 and stack[-1][1] >= indent:
            stack.pop()

        parent, _ = stack[-1]

        if ":" not in key_val:
            continue

        key, _, val = key_val.partition(":")
        key = key.strip()
        val = val.strip()

        # remove optional quotes
        if len(val) >= 2 and val[0] == val[-1] and val[0] in {"'", '"'}:
            val = val[1:-1]

        if val == "":
            # nested mapping
            child: dict[str, Any] = {}
            parent[key] = child
            stack.append((child, indent))
        elif val == "null" or val == "~":
            parent[key] = None
        elif val == "true":
            parent[key] = True
        elif val == "false":
            parent[key] = False
        elif re.fullmatch(r"-?[0-9]+", val):
            parent[key] = int(val)
        elif re.fullmatch(r"-?[0-9]+\.[0-9]+", val):
            parent[key] = float(val)
        else:
            parent[key] = val

    return root


def _read_budget(config_dir: Path) -> dict[str, int]:
    company_yaml = config_dir / "company.yaml"
    if not company_yaml.exists():
        print(f"error: {company_yaml} not found", file=sys.stderr)
        sys.exit(2)

    config = _load_yaml(company_yaml)
    budget = config.get("budget")
    if not isinstance(budget, dict):
        print("error: budget section missing or invalid in company.yaml", file=sys.stderr)
        sys.exit(2)

    required = [
        "monthlyCapMicroInr",
        "warningThresholdMicroInr",
        "suspensionThresholdMicroInr",
        "operationalReserveMicroInr",
        "safetyMarginBps",
    ]
    for field in required:
        if field not in budget or not isinstance(budget[field], (int, float)):
            print(f"error: budget.{field} missing or invalid", file=sys.stderr)
            sys.exit(2)

    return {k: int(budget[k]) for k in required}


def _read_forecast_run_rate(root_dir: Path, scenario: str) -> float:
    """Return the month-12 total service cost (INR, aggregate) for a scenario.

    The forecast CSV is account-aggregate across all three environments
    (see docs/cost/aws-monthly-forecast.md). Callers divide by the environment
    count to obtain a per-environment baseline; that split is a conservative
    placeholder pending the approved per-environment allocation.
    """
    forecast_csv = root_dir / "docs" / "cost" / "aws-monthly-forecast.csv"
    if not forecast_csv.exists():
        print(f"error: {forecast_csv} not found", file=sys.stderr)
        sys.exit(2)

    target_month = "12"
    target_service = "Total service cost with safety margin"

    with forecast_csv.open("r", encoding="utf-8", newline="") as fh:
        reader = csv.DictReader(fh)
        for row in reader:
            if (
                row.get("scenario", "").strip() == scenario
                and row.get("month", "").strip() == target_month
                and row.get("service", "").strip() == target_service
            ):
                cost_inr_str = row.get("service_cost_inr_at_100_excluding_gst", "0").strip()
                try:
                    return float(cost_inr_str)
                except ValueError:
                    print(
                        f"error: invalid cost value in forecast CSV row: {cost_inr_str}",
                        file=sys.stderr,
                    )
                    sys.exit(2)

    print(f"error: forecast row not found: {scenario}/month {target_month}/{target_service}", file=sys.stderr)
    sys.exit(2)


def _validate_invoice_month(invoice_month: str) -> None:
    """Fail if invoice_month is not current or previous month."""
    try:
        datetime.strptime(invoice_month, "%Y-%m").replace(tzinfo=timezone.utc)
    except ValueError:
        print(f"error: invoice_month '{invoice_month}' is not valid YYYY-MM", file=sys.stderr)
        sys.exit(1)

    now = datetime.now(timezone.utc)
    current_month = now.strftime("%Y-%m")
    # Allow current or previous month
    valid_months = [current_month]
    if now.month == 1:
        prev_month = f"{now.year - 1}-12"
    else:
        prev_month = f"{now.year}-{now.month - 1:02d}"
    valid_months.append(prev_month)

    if invoice_month not in valid_months:
        print(
            f"error: stale usage snapshot (invoice_month={invoice_month}, "
            f"expected {current_month} or {prev_month})",
            file=sys.stderr,
        )
        sys.exit(1)


def _read_usage_snapshot(path: Path) -> dict[str, Any]:
    """Read and validate a usage snapshot JSON file."""
    if not path.exists():
        print(f"error: usage snapshot not found: {path}", file=sys.stderr)
        sys.exit(2)

    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"error: invalid usage snapshot JSON: {exc}", file=sys.stderr)
        sys.exit(2)

    if not isinstance(data, dict):
        print("error: usage snapshot must be a JSON object", file=sys.stderr)
        sys.exit(2)

    required = ["invoice_month", "settled_micro_inr", "reserved_micro_inr", "reconciled_micro_inr"]
    for field in required:
        if field not in data or not isinstance(data[field], (int, float)):
            print(f"error: usage snapshot missing/invalid field: {field}", file=sys.stderr)
            sys.exit(2)

    _validate_invoice_month(str(data["invoice_month"]))

    # Coerce numeric fields to int once at the read boundary so callers never
    # repeat int() conversions on raw JSON values.
    try:
        coerced: dict[str, Any] = {
            "invoice_month": str(data["invoice_month"]),
            "settled_micro_inr": int(data["settled_micro_inr"]),
            "reserved_micro_inr": int(data["reserved_micro_inr"]),
            "reconciled_micro_inr": int(data["reconciled_micro_inr"]),
        }
    except (TypeError, ValueError, OverflowError) as exc:
        print(f"error: usage snapshot numeric field is not coercible to int: {exc}", file=sys.stderr)
        sys.exit(2)
    return coerced


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    config_dir = args.config_dir  # already resolved in _parse_args

    budget = _read_budget(config_dir)

    # The forecast CSV is account-aggregate. The per-environment baseline is the
    # aggregate month-12 run-rate divided by the environment count. This equal
    # split is a conservative placeholder pending the approved per-environment
    # allocation (see docs/cost/aws-monthly-forecast.md). The `expected` scenario
    # is the default baseline; `--forecast-scenario worst-case-attachment` is a
    # stricter stress check.
    forecast_aggregate_inr = _read_forecast_run_rate(args.root_dir, args.forecast_scenario)
    if args.env_count <= 0:
        print("error: --env-count must be a positive integer", file=sys.stderr)
        sys.exit(2)
    forecast_per_env_inr = forecast_aggregate_inr / args.env_count
    try:
        forecast_micro_inr = int(forecast_per_env_inr * MICROINR_PER_INR)
    except (ValueError, OverflowError, TypeError) as exc:
        print(f"error: forecast cost is not coercible to microINR: {exc}", file=sys.stderr)
        sys.exit(2)

    monthly_cap = budget["monthlyCapMicroInr"]
    warning_threshold = budget["warningThresholdMicroInr"]
    suspension_threshold = budget["suspensionThresholdMicroInr"]
    margin_floor = (
        args.margin_floor_micro_inr
        if args.margin_floor_micro_inr is not None
        else budget["operationalReserveMicroInr"]
    )

    snapshot_provided = False
    snap_total = 0
    if args.usage_snapshot:
        snapshot_provided = True
        snap = _read_usage_snapshot(args.usage_snapshot.resolve())
        snap_total = (
            snap["settled_micro_inr"]
            + snap["reserved_micro_inr"]
            + snap["reconciled_micro_inr"]
        )

    # Authoritative observed usage (when provided) wins over the forecast
    # baseline; take the conservative max so a low forecast never masks an
    # over-budget snapshot.
    projected_micro_inr = max(forecast_micro_inr, snap_total)
    available = monthly_cap - projected_micro_inr

    # --- Human-readable output to stderr ---
    mode = "snapshot+forecast" if snapshot_provided else "forecast-only"
    print(f"environment:     {args.environment}", file=sys.stderr)
    print(f"mode:            {mode}", file=sys.stderr)
    print(f"monthly cap:     {monthly_cap:,} µINR (₹{monthly_cap / MICROINR_PER_INR:,.2f})", file=sys.stderr)
    print(
        f"forecast (mo12): {forecast_micro_inr:,} µINR "
        f"(₹{forecast_per_env_inr:,.2f}/env from {args.forecast_scenario} aggregate ₹{forecast_aggregate_inr:,.2f} ÷ {args.env_count})",
        file=sys.stderr,
    )
    if snapshot_provided:
        print(f"snapshot total:  {snap_total:,} µINR", file=sys.stderr)
    print(f"projected:       {projected_micro_inr:,} µINR", file=sys.stderr)
    print(f"available:       {available:,} µINR", file=sys.stderr)
    print(f"margin floor:    {margin_floor:,} µINR", file=sys.stderr)

    if not snapshot_provided:
        print(
            "WARNING: no authoritative usage snapshot provided — using forecast "
            "baseline only. Pass --usage-snapshot for a real margin check.",
            file=sys.stderr,
        )

    # --- Threshold warnings ---
    if projected_micro_inr >= warning_threshold:
        print(
            f"WARNING: projected usage crosses warning threshold "
            f"({warning_threshold:,} µINR)",
            file=sys.stderr,
        )
    if projected_micro_inr >= suspension_threshold:
        print(
            f"WARNING: projected usage crosses suspension threshold "
            f"({suspension_threshold:,} µINR)",
            file=sys.stderr,
        )

    # --- JSON summary to stdout ---
    summary = {
        "environment": args.environment,
        "mode": mode,
        "forecast_scenario": args.forecast_scenario,
        "monthly_cap_micro_inr": monthly_cap,
        "forecast_micro_inr": forecast_micro_inr,
        "projected_micro_inr": projected_micro_inr,
        "available_micro_inr": available,
        "margin_floor_micro_inr": margin_floor,
        "margin_sufficient": available >= margin_floor,
    }
    if snapshot_provided:
        summary["snapshot_total_micro_inr"] = snap_total
    print(json.dumps(summary))

    # --- Decision ---
    if available < margin_floor:
        shortfall = margin_floor - available
        print(
            f"FAIL: budget margin insufficient — need {margin_floor:,} µINR, "
            f"only {available:,} µINR available (shortfall: {shortfall:,} µINR)",
            file=sys.stderr,
        )
        return 1

    print("PASS: budget margin sufficient", file=sys.stderr)
    return 0


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Budget margin gate for release verification",
    )
    parser.add_argument(
        "environment",
        choices=("dev", "staging", "production"),
        help="target environment",
    )
    parser.add_argument(
        "--usage-snapshot",
        type=Path,
        default=None,
        help="JSON file with authoritative usage (invoice_month, settled_micro_inr, "
        "reserved_micro_inr, reconciled_micro_inr)",
    )
    parser.add_argument(
        "--config-dir",
        type=Path,
        default=Path("config"),
        help="config directory (default: config)",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path("."),
        dest="root_dir",
        help="repo root directory (default: .)",
    )
    parser.add_argument(
        "--margin-floor-micro-inr",
        type=int,
        default=None,
        help="minimum required margin in microINR (default: operationalReserveMicroInr from config)",
    )
    parser.add_argument(
        "--forecast-scenario",
        choices=("expected", "worst-case-attachment"),
        default="expected",
        help="forecast CSV scenario used for the per-env baseline (default: expected)",
    )
    parser.add_argument(
        "--env-count",
        type=int,
        default=3,
        help="number of environments to divide the aggregate forecast by (default: 3)",
    )
    parsed = parser.parse_args(argv)

    if not parsed.config_dir.is_dir():
        parser.error(f"config directory not found: {parsed.config_dir}")

    parsed.config_dir = parsed.config_dir.resolve()
    parsed.root_dir = parsed.root_dir.resolve()

    return parsed


if __name__ == "__main__":
    raise SystemExit(main())
