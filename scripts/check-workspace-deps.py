#!/usr/bin/env python3
"""Enforce workspace membership and dependency centralization.

Every first-party Rust package must be a root workspace member. Every member
dependency must be declared in root `[workspace.dependencies]` and referenced
with `dep.workspace = true` (optionally with per-package features).

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


GOVERNED_DIRS = ("crates", "functions", "tests")


def _dep_sections(table: dict) -> list[tuple[str, dict]]:
    sections: list[tuple[str, dict]] = []
    dependency_tables = ("dependencies", "dev-dependencies", "build-dependencies")
    for name in dependency_tables:
        sec = table.get(name)
        if isinstance(sec, dict):
            sections.append((name, sec))
    for target_name, target in (table.get("target") or {}).items():
        if not isinstance(target, dict):
            continue
        for name in dependency_tables:
            sec = target.get(name)
            if isinstance(sec, dict):
                sections.append((f"target.{target_name}.{name}", sec))
    return sections


def _first_party_manifests(root: Path) -> set[Path]:
    return {
        manifest.resolve()
        for directory in GOVERNED_DIRS
        if (root / directory).is_dir()
        for manifest in (root / directory).rglob("Cargo.toml")
    }


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.cwd()
    root_toml = root / "Cargo.toml"
    if not root_toml.exists():
        print(f"error: {root_toml} not found", file=sys.stderr)
        return 2

    workspace = _load(root_toml).get("workspace", {})
    managed: set[str] = set((workspace.get("dependencies") or {}).keys())
    members = workspace.get("members") or []

    if not managed:
        print("error: no [workspace.dependencies] found in root Cargo.toml", file=sys.stderr)
        return 2

    violations: list[str] = []
    member_manifests: set[Path] = set()
    for member in members:
        member_toml = (root / member / "Cargo.toml").resolve()
        if not member_toml.exists():
            violations.append(f"Cargo.toml: workspace member `{member}` has no manifest")
            continue
        member_manifests.add(member_toml)

    first_party = _first_party_manifests(root)
    for manifest in sorted(first_party - member_manifests):
        violations.append(
            f"{manifest.relative_to(root)}: first-party package must be a root workspace member"
        )

    for member_toml in sorted(member_manifests):
        data = _load(member_toml)
        rel = member_toml.relative_to(root)
        if "workspace" in data:
            violations.append(f"{rel}: nested [workspace] tables are forbidden")
        for section, deps in _dep_sections(data):
            for dep_name, spec in deps.items():
                if dep_name not in managed:
                    violations.append(
                        f"{rel}: [{section}] {dep_name} must be declared in "
                        "root [workspace.dependencies]"
                    )
                    continue
                if not isinstance(spec, dict) or not spec.get("workspace", False):
                    violations.append(
                        f"{rel}: [{section}] {dep_name} must use `"
                        f"{dep_name}.workspace = true`"
                    )

    if violations:
        print("workspace dependency violations found:\n")
        for v in violations:
            print(f"  - {v}")
        print(
            "\nAdd every first-party package to the root workspace, declare every "
            "dependency version once in root [workspace.dependencies], and reference "
            "it from members with `dep.workspace = true`."
        )
        return 1

    print(f"ok: {len(members)} member(s), {len(managed)} workspace dep(s) centralized")
    return 0


if __name__ == "__main__":
    sys.exit(main())
