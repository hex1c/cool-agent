#!/usr/bin/env python3
"""Enforce workspace dependency centralization.

Every dependency declared in the root `[workspace.dependencies]` table must be
referenced from member crates via `dep.workspace = true` (optionally with extra
`features`). Member crates must NOT re-declare an inline `version = "..."` for a
workspace-managed dependency.

Usage:
    python3 scripts/check-workspace-deps.py [repo-root]

Exit code 0 = compliant, 1 = violations found.
"""

from __future__ import annotations

import sys
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    print("error: requires Python 3.11+ (tomllib)", file=sys.stderr)
    sys.exit(2)


def _load(path: Path) -> dict:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def _dep_sections(table: dict) -> list[tuple[str, dict]]:
    sections: list[tuple[str, dict]] = []
    for name in ("dependencies", "dev-dependencies", "build-dependencies"):
        sec = table.get(name)
        if isinstance(sec, dict):
            sections.append((name, sec))
    return sections


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.cwd()
    root_toml = root / "Cargo.toml"
    if not root_toml.exists():
        print(f"error: {root_toml} not found", file=sys.stderr)
        return 2

    workspace = _load(root_toml).get("workspace", {})
    managed: set[str] = set((workspace.get("dependencies") or {}).keys())
    members = workspace.get("members") or []
    exclude = set(workspace.get("exclude") or [])

    if not managed:
        print("error: no [workspace.dependencies] found in root Cargo.toml", file=sys.stderr)
        return 2

    violations: list[str] = []

    for member in members:
        member_toml = (root / member / "Cargo.toml").resolve()
        if not member_toml.exists():
            continue
        data = _load(member_toml)
        rel = member_toml.relative_to(root)
        for section, deps in _dep_sections(data):
            for dep_name, spec in deps.items():
                if dep_name not in managed:
                    continue
                uses_workspace = isinstance(spec, dict) and spec.get("workspace") is True
                if uses_workspace:
                    continue
                violations.append(
                    f"{rel}: [{section}] {dep_name} must use `"
                    f"{dep_name}.workspace = true` (declared in [workspace.dependencies])"
                )

    if violations:
        print("workspace dependency violations found:\n")
        for v in violations:
            print(f"  - {v}")
        print(
            "\nDeclare dependency versions once in the root Cargo.toml "
            "[workspace.dependencies] and reference them in member crates with "
            "`dep.workspace = true`."
        )
        return 1

    print(f"ok: {len(members)} member(s), {len(managed)} workspace dep(s) centralized")
    return 0


if __name__ == "__main__":
    sys.exit(main())
