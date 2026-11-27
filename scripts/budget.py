#!/usr/bin/env python3
"""Compare the amount of Rust code with the size budget in budget.toml.

Counts the "code" column of tokei for Rust, tests included, and gives every
file to the component whose path contains it. Exits with 1 when a component or
the total is above its cap, or when a Rust file belongs to no component.

    budget.py [--root DIR] [--budget FILE]
"""

import argparse
import json
import subprocess
import sys
import tomllib
from pathlib import Path


def rust_code_by_file(root: Path) -> dict[Path, int]:
    """Lines of Rust code per file, paths relative to root."""
    result = subprocess.run(
        ["tokei", "--output", "json", "--types", "Rust", "."],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    reports = json.loads(result.stdout).get("Rust", {}).get("reports", [])
    return {Path(report["name"]): report["stats"]["code"] for report in reports}


def main() -> int:
    here = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=here)
    parser.add_argument("--budget", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    with (args.budget or root / "budget.toml").open("rb") as file:
        budget = tomllib.load(file)

    components = budget["components"]
    used = dict.fromkeys(components, 0)
    stray = []
    for file, code in sorted(rust_code_by_file(root).items()):
        owners = [
            name
            for name, component in components.items()
            if any(file.is_relative_to(path) for path in component["paths"])
        ]
        if len(owners) == 1:
            used[owners[0]] += code
        else:
            stray.append((file, owners))

    failures = []
    width = max(map(len, components))
    for name, component in components.items():
        cap = component["cap"]
        over = used[name] > cap
        print(f"{name:<{width}}  {used[name]:>7} / {cap:>7}  {used[name] / cap:>6.1%}{'  OVER' if over else ''}")
        if over:
            failures.append(f"{name} has {used[name]} lines of Rust code, its cap is {cap}")
    total, cap = sum(used.values()), budget["total"]
    print(f"{'total':<{width}}  {total:>7} / {cap:>7}  {total / cap:>6.1%}")
    if total > cap:
        failures.append(f"the total is {total} lines of Rust code, the cap is {cap}")
    for file, owners in stray:
        reason = f"belongs to {' and '.join(owners)}" if owners else "belongs to no component"
        failures.append(f"{file} {reason} in budget.toml")

    for failure in failures:
        print(f"budget: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
