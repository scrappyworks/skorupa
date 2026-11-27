#!/usr/bin/env python3
"""Check that tests and the model are tied to requirements.

1. Every `#[test]` function names a requirement: `req_proto_001_...` in its
   name, or `REQ-PROTO-001` in its doc comment.
2. Every fuzz target names a requirement somewhere in its file.
3. Every requirement named this way, and every `REQ-...` in the Quint model
   (spec/*.qnt), is listed in docs/requirements.md.

Step 3 is skipped, with a loud notice, only while the catalog does not exist
at all. A branch that lacks the catalog although main has it fails.

Exits with 1 when a check fails.

    req-lint.py [--root DIR]
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

from sources import repository_files

REQUIREMENTS = Path("docs/requirements.md")
MAIN = "origin/main"
# Anything that claims to be an id: REQ-PROTO-001, and also REQ-SOME-NAME, so
# that an id in another style is reported instead of overlooked.
ID_IN_TEXT = re.compile(r"\bREQ-[A-Z0-9]+(?:-[A-Z0-9]+)+\b")
ID_IN_NAME = re.compile(r"(?:^|_)req_([a-z][a-z0-9]*)_([0-9]+)(?=_|$)")
TEST_ATTRIBUTE = re.compile(r"\s*#\[test\]")
FUNCTION = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
DOC_OR_ATTRIBUTE = re.compile(r"\s*(///|#\[)")


def catalog_on_main(root: Path) -> bool:
    """Whether main has the catalog. False when git cannot tell."""
    try:
        found = subprocess.run(
            ["git", "cat-file", "-e", f"{MAIN}:{REQUIREMENTS}"],
            cwd=root,
            capture_output=True,
        )
    except FileNotFoundError:
        return False
    return found.returncode == 0


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
    for shown in repository_files(root, ".rs"):
        text = (root / shown).read_text(encoding="utf-8")
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
    models = [shown for shown in repository_files(root, ".qnt") if shown.parts[0] == "spec"]
    for shown in models:
        lines = (root / shown).read_text(encoding="utf-8").splitlines()
        for number, line in enumerate(lines, start=1):
            for requirement in ID_IN_TEXT.findall(line):
                used.setdefault(requirement, f"{shown}:{number}")

    requirements = root / REQUIREMENTS
    notice = None
    if requirements.is_file():
        known = set(ID_IN_TEXT.findall(requirements.read_text(encoding="utf-8")))
        for requirement, where in sorted(used.items()):
            if requirement not in known:
                failures.append(f"{where}: {requirement} is not in {REQUIREMENTS}")
        checked = f"checked against {REQUIREMENTS}"
    elif catalog_on_main(root):
        failures.append(f"{REQUIREMENTS} exists on {MAIN} but not here: rebase onto main")
        checked = "NOT checked"
    else:
        checked = "NOT checked"
        notice = (
            f"WARNING: {REQUIREMENTS} DOES NOT EXIST. The {len(used)} requirement ids in use "
            "were NOT checked against the catalog. This is acceptable only until the catalog lands."
        )

    print(
        f"req-lint: {count} tests and fuzz targets, {len(models)} model files, "
        f"{len(used)} requirement ids: {checked}"
    )
    if notice:
        bar = "!" * 78
        print(f"{bar}\nreq-lint: {notice}\n{bar}", file=sys.stderr)
    for failure in failures:
        print(f"req-lint: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
