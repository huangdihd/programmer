#!/usr/bin/env python3
"""Reject tracked local drafts, including files staged with `git add -f`.

Read the Git index rather than the working tree; untracked drafts are untouched.
Run with `python3 scripts/check_release_scope.py --repo PATH` in release/CI jobs.
"""

import argparse
import os
from pathlib import Path
import subprocess
import sys
from typing import Iterable


ALLOWED_PATHS = frozenset({
    "design-demos/light-themes.html",
    "design-demos/peer-delegation-lifecycle.html",
    "design-demos/peer-delegation-model.html",
    "design-demos/peer-delegation-model.test.cjs",
    "design-demos/peer-delegation-v2.html",
    "design-demos/peer-delegation.html",
    "design-demos/usage-variants.html",
    ".programmer/diagnostics.toml",
})
RESTRICTED_ROOTS = ("design-demos", ".programmer")


def forbidden_paths(paths: Iterable[str]) -> list[str]:
    return sorted({
        path for path in paths
        if path not in ALLOWED_PATHS
        and any(path == root or path.startswith(root + "/") for root in RESTRICTED_ROOTS)
    })


def tracked_paths(repository: Path) -> list[str]:
    # Resolve the root so invocation from a subdirectory still checks the whole index.
    root = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", "--show-toplevel"],
        check=True, capture_output=True,
    ).stdout.removesuffix(b"\n")
    result = subprocess.run(
        ["git", "-C", os.fsdecode(root), "ls-files", "--cached", "--full-name", "-z"],
        check=True, capture_output=True,
    )
    # NUL separation preserves spaces, newlines, and non-UTF-8 filenames.
    return [os.fsdecode(path) for path in result.stdout.split(b"\0") if path]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path.cwd(), help="repository directory (default: cwd)")
    arguments = parser.parse_args()
    try:
        violations = forbidden_paths(tracked_paths(arguments.repo))
    except (OSError, subprocess.CalledProcessError) as error:
        if isinstance(error, subprocess.CalledProcessError):
            detail = error.stderr.decode(errors="replace").strip()
        else:
            detail = str(error)
        print(f"Release scope check could not read the Git index: {detail}", file=sys.stderr)
        return 2
    if violations:
        print("Release scope check FAILED: forbidden tracked local drafts:", file=sys.stderr)
        for path in violations:
            print(f"  {path!r}", file=sys.stderr)
        print("Only the exact legacy allowlist is permitted under design-demos/ and .programmer/. No files were changed.", file=sys.stderr)
        return 1
    print("Release scope check passed: no forbidden tracked local drafts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
