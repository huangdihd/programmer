#!/usr/bin/env python3
"""Integration tests using disposable repositories and real staged Git indexes.

Run: python3 -B -m unittest discover -s scripts -p 'test_release_scope.py' -v
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


CHECKER = Path(__file__).with_name("check_release_scope.py").resolve()
LEGACY_FILES = (
    "design-demos/light-themes.html",
    "design-demos/peer-delegation-lifecycle.html",
    "design-demos/peer-delegation-model.html",
    "design-demos/peer-delegation-model.test.cjs",
    "design-demos/peer-delegation-v2.html",
    "design-demos/peer-delegation.html",
    "design-demos/usage-variants.html",
    ".programmer/diagnostics.toml",
)


class ReleaseScopeTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="release-scope-")
        self.addCleanup(temporary.cleanup)
        self.repository = Path(temporary.name)
        # Do not inherit the caller's index/worktree overrides into disposable repos.
        self.environment = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}
        self.git("init", "--quiet")

    def git(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", "-C", str(self.repository), *arguments],
            env=self.environment, check=True, capture_output=True, text=True,
        )

    def write(self, relative_path: str, content: str = "fixture\n") -> Path:
        path = self.repository / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def check(self, directory: Path | None = None, explicit: bool = True) -> subprocess.CompletedProcess[str]:
        directory = directory or self.repository
        arguments = ["--repo", str(directory)] if explicit else []
        return subprocess.run(
            [sys.executable, "-B", str(CHECKER), *arguments],
            cwd=directory, env=self.environment, capture_output=True, text=True,
        )

    def test_legacy_allowlist_and_ordinary_sources_pass(self) -> None:
        for path in (*LEGACY_FILES, "src/main.rs", "docs/design-demos/guide.md", "design-demos-extra/demo.html"):
            self.write(path)
        self.git("add", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "commit", "--quiet", "--no-verify", "-m", "Existing files")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("passed", result.stdout)

    def test_forced_staged_drafts_fail_without_mutation(self) -> None:
        self.write(".gitignore", "/design-demos/\n/.programmer/\n")
        forbidden = ("design-demos/new draft.html", ".programmer/nested/debug.log")
        for path in forbidden:
            self.write(path)
            self.git("add", "-f", "--", path)
        index = self.repository / ".git/index"
        before = index.read_bytes()
        result = self.check()
        self.assertEqual(result.returncode, 1, result.stderr)
        for path in forbidden:
            self.assertIn(repr(path), result.stderr)
            self.assertEqual((self.repository / path).read_text(), "fixture\n")
        self.assertEqual(index.read_bytes(), before)

    def test_untracked_drafts_are_ignored_and_preserved(self) -> None:
        paths = ("design-demos/local.html", ".programmer/local.log")
        for path in paths:
            self.write(path)
        self.assertEqual(self.check().returncode, 0)
        for path in paths:
            self.assertTrue((self.repository / path).is_file())

    def test_allowlist_is_exact_including_nested_paths(self) -> None:
        paths = (
            "design-demos/nested/light-themes.html",
            "design-demos/light-themes.html.bak",
            ".programmer/nested/diagnostics.toml",
            ".programmer/diagnostics.toml.bak",
            "design-demos/draft\nwith-newline.html",
        )
        for path in paths:
            self.write(path)
        self.git("add", ".")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        for path in paths:
            self.assertIn(repr(path), result.stderr)

    def test_restricted_root_cannot_be_replaced_by_tracked_file(self) -> None:
        self.write("design-demos")
        self.write(".programmer")
        self.git("add", ".")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("'design-demos'", result.stderr)
        self.assertIn("'.programmer'", result.stderr)

    def test_default_cwd_and_explicit_subdirectory_check_whole_repository(self) -> None:
        self.write("design-demos/draft.html")
        self.git("add", ".")
        directory = self.repository / "src"
        directory.mkdir()
        for explicit in (True, False):
            with self.subTest(explicit=explicit):
                result = self.check(directory, explicit)
                self.assertEqual(result.returncode, 1)
                self.assertIn("design-demos/draft.html", result.stderr)

    def test_missing_repository_fails_closed(self) -> None:
        result = subprocess.run(
            [sys.executable, "-B", str(CHECKER), "--repo", str(self.repository / "missing")],
            env=self.environment, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("could not read the Git index", result.stderr)


if __name__ == "__main__":
    unittest.main()
