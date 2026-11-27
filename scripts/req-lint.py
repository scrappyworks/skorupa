#!/usr/bin/env python3
"""Check that tests are tied to requirements.

1. Every `#[test]` function names a requirement: `req_proto_001_...` in its
   name, or `REQ-PROTO-001` in its doc comment.
2. Every fuzz target names a requirement somewhere in its file.
3. Every requirement named this way is listed in docs/requirements.md. This
   step is skipped, with a notice, while that file does not exist.

Exits with 1 when a check fails.

    req-lint.py [--root DIR]
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

REQUIREMENTS = Path("docs/requirements.md")
SKIPPED_DIRS = {".git", "target", "result"}
ID_IN_TEXT = re.compile(r"\bREQ-[A-Z][A-Z0-9]*-[0-9]+\b")
ID_IN_NAME = re.compile(r"(?:^|_)req_([a-z][a-z0-9]*)_([0-9]+)(?=_|$)")
TEST_ATTRIBUTE = re.compile(r"\s*#\[test\]")
FUNCTION = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
DOC_OR_ATTRIBUTE = re.compile(r"\s*(///|#\[)")


def rust_files(root: Path) -> list[Path]:
    """Tracked and new Rust files, or every Rust file outside a git checkout."""
    try:
        listed = subprocess.run(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "*.rs"],
            cwd=root,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        listed = None
    if listed is not None and listed.returncode == 0:
        return sorted(root / line for line in listed.stdout.splitlines() if (root / line).is_file())
    return sorted(
        path
        for path in root.rglob("*.rs")
        if not SKIPPED_DIRS.intersection(path.relative_to(root).parts)
    )


def tests(lines: list[str]):
    """Yield (line number, function name, requirement ids) for each test."""
    for index, line in enumerate(lines):
        if not TEST_ATTRIBUTE.match(line):
            continue
        # The doc comment and other attributes sit directly around `#[test]`.
        start = index
        while start > 0 and DOC_OR_ATTRIBUTE.match(lines[start - 1]):
            start -= 1
        end = index + 1
        while end < len(lines) and DOC_OR_ATTRIBUTE.match(lines[end]):
            end += 1
        function = FUNCTION.match(lines[end]) if end < len(lines) else None
        if not function:
            yield index + 1, None, set()
            continue
        name = function.group(1)
        ids = {f"REQ-{part.upper()}-{number}" for part, number in ID_IN_NAME.findall(name)}
        for doc in lines[start:end]:
            if doc.lstrip().startswith("///"):
                ids.update(ID_IN_TEXT.findall(doc))
        yield end + 1, name, ids


def main() -> int:
    here = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=here)
    root = parser.parse_args().root.resolve()

    failures = []
    used: dict[str, str] = {}
    count = 0
    for path in rust_files(root):
        shown = path.relative_to(root)
        text = path.read_text(encoding="utf-8")
        for number, name, ids in tests(text.splitlines()):
            count += 1
            if name is None:
                failures.append(f"{shown}:{number}: no function found after #[test]")
            elif not ids:
                failures.append(f"{shown}:{number}: test {name} names no requirement (REQ-...)")
            for requirement in ids:
                used.setdefault(requirement, f"{shown}:{number}")
        if shown.parts[:2] == ("fuzz", "fuzz_targets"):
            count += 1
            ids = set(ID_IN_TEXT.findall(text))
            if not ids:
                failures.append(f"{shown}: fuzz target names no requirement (REQ-...)")
            for requirement in ids:
                used.setdefault(requirement, str(shown))

    requirements = root / REQUIREMENTS
    if requirements.is_file():
        known = set(ID_IN_TEXT.findall(requirements.read_text(encoding="utf-8")))
        for requirement, where in sorted(used.items()):
            if requirement not in known:
                failures.append(f"{where}: {requirement} is not in {REQUIREMENTS}")
        checked = f"checked against {REQUIREMENTS}"
    else:
        checked = f"{REQUIREMENTS} does not exist yet, so their presence there was NOT checked"

    for failure in failures:
        print(f"req-lint: {failure}", file=sys.stderr)
    print(f"req-lint: {count} tests and fuzz targets, {len(used)} requirement ids: {checked}")
    if used:
        print("req-lint: ids in use: " + " ".join(sorted(used)))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
