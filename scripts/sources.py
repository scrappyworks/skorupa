"""Which files of the repository the gate scripts read."""

import subprocess
from pathlib import Path

SKIPPED_DIRS = {".git", "target", "result"}


def repository_files(root: Path, suffix: str) -> list[Path]:
    """Files under root that end in suffix, as paths relative to root.

    In a git checkout these are the tracked files plus new files that are not
    ignored, so build output never counts. Without git (the Nix sandbox, whose
    source holds tracked files only) it is every such file in the tree.
    """
    try:
        listed = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=root,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError:
        listed = None
    if listed is not None and listed.returncode == 0:
        names = [Path(name) for name in listed.stdout.split("\0") if name.endswith(suffix)]
        # A file deleted in the work tree is still listed until it is staged.
        return sorted(name for name in names if (root / name).is_file())
    return sorted(
        path.relative_to(root)
        for path in root.rglob(f"*{suffix}")
        if path.is_file() and not SKIPPED_DIRS.intersection(path.relative_to(root).parts)
    )
