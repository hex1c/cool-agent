#!/usr/bin/env python3
"""Parse an lcov coverage report and emit a GitHub Step Summary table
restricted to the files that changed in the triggering event.

Usage: coverage-summary.py <lcov-path> <changed-file> [<changed-file> ...]
"""
from __future__ import annotations

import os
import sys
from pathlib import Path


def parse_lcov(path: str) -> dict[str, dict[str, int]]:
    """Return {source_file: {total, covered}} from an lcov report."""
    coverage: dict[str, dict[str, int]] = {}
    current: str | None = None
    total = covered = 0

    def flush() -> None:
        if current is not None:
            coverage[current] = {"total": total, "covered": covered}

    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if line.startswith("SF:"):
                current = line[3:]
                total = covered = 0
            elif line.startswith("DA:"):
                # DA:<line>,<hits>[,<checksum>]
                parts = line[3:].split(",")
                hits = int(parts[1]) if len(parts) > 1 else 0
                total += 1
                if hits > 0:
                    covered += 1
            elif line == "end_of_record":
                flush()
                current = None
    flush()
    return coverage


def normalize(p: str) -> str:
    """Best-effort path normalization for matching lcov SF entries to changed files."""
    return os.path.normpath(p)


def main() -> int:
    if len(sys.argv) < 3:
        print("usage: coverage-summary.py <lcov-path> <changed-file> [...]", file=sys.stderr)
        return 2

    lcov_path = sys.argv[1]
    changed = [normalize(p) for p in sys.argv[2:]]

    coverage = parse_lcov(lcov_path)

    # Index lcov source files by their normalized and basename forms so we can
    # match against the changed paths regardless of absolute-prefix differences.
    by_norm: dict[str, dict[str, int]] = {}
    by_basename: dict[str, list[dict[str, int]]] = {}
    for sf, stats in coverage.items():
        norm = normalize(sf)
        by_norm[norm] = stats
        by_basename.setdefault(Path(norm).name, []).append(stats)

    print("### Coverage for changed files")
    print()
    print("| File | Covered | Total | % |")
    print("| --- | ---: | ---: | ---: |")

    grand_total = grand_covered = 0
    seen: set[str] = set()

    for rel in changed:
        stats = by_norm.get(rel)
        if stats is None:
            candidates = by_basename.get(Path(rel).name, [])
            if len(candidates) == 1:
                stats = candidates[0]
        if stats is None:
            continue
        if rel in seen:
            continue
        seen.add(rel)

        t, c = stats["total"], stats["covered"]
        if t == 0:
            continue
        grand_total += t
        grand_covered += c
        pct = (c / t) * 100.0
        print(f"| `{rel}` | {c} | {t} | {pct:.1f}% |")

    if grand_total == 0:
        print()
        print("_No coverage data for the changed files._")
        return 0

    overall = (grand_covered / grand_total) * 100.0
    print(f"| **Total (changed files)** | **{grand_covered}** | **{grand_total}** | **{overall:.1f}%** |")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
