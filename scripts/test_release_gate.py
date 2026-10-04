"""Offline release policy regressions: no credentials, network or publication."""

from contextlib import redirect_stdout
import copy
import io
import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import release_gate as gate


SHA = "a" * 40
TAG = "v1.2.3"
NOTES = """### programmer v1.2.3

- Validate release commits and all CI jobs before publishing binaries.

**Full changelog:** https://github.com/example/project/compare/v1.2.2...v1.2.3

---

### programmer v1.2.2 (withheld draft)

Old draft content must never be included.
"""


def run(identifier=10, **changes):
    value = dict(id=identifier, head_sha=SHA, event="push", head_branch="main",
                 run_number=identifier, run_attempt=1, status="completed", conclusion="success")
    value.update(changes)
    return value


def jobs():
    return [dict(name=name, status="completed", conclusion="success") for name in gate.REQUIRED_JOBS]


def ci_api(runs, run_jobs=None):
    api = Mock()
    api.pages.side_effect = lambda path, key: runs if key == "workflow_runs" else (jobs() if run_jobs is None else run_jobs)
    return api


class TagAndNotesTests(unittest.TestCase):
    def test_canonical_tag_matches_cargo(self):
        gate.validate_tag(TAG, {"package": {"version": "1.2.3"}})

    def test_invalid_tags_and_mismatched_version(self):
        for tag in ("1.2.3", "v01.2.3", "v1.2.3-rc1", "v1.2.3\n", "v1.2.3; echo bad", "v1.2.4"):
            with self.subTest(tag=tag), self.assertRaises(gate.GateError):
                gate.validate_tag(tag, {"package": {"version": "1.2.3"}})

    def test_exact_section_excludes_previous_draft(self):
        body = gate.extract_notes(NOTES, TAG)
        self.assertIn("Validate release commits", body)
        self.assertNotIn("Old draft", body)
        self.assertNotIn("---", body)

    def test_incomplete_notes_fail(self):
        examples = [
            NOTES.replace(TAG, "v1.2.30"),
            NOTES + "\n### programmer v1.2.3\nDuplicate",
            NOTES.replace("### programmer v1.2.3", "### programmer v1.2.3 (draft)"),
            NOTES.replace("- Validate release commits and all CI jobs before publishing binaries.", ""),
            NOTES.replace("Validate release commits", "Includes the withheld v1.2.2 draft; validate release commits"),
            NOTES.replace("Validate release commits", "Changes listed below validate release commits"),
            NOTES.replace("Validate release commits", "TODO validate release commits"),
            NOTES.replace("...v1.2.3", "...v1.2.4"),
        ]
        for text in examples:
            with self.subTest(text=text), self.assertRaises(gate.GateError):
                gate.extract_notes(text, TAG)

    def test_annotated_tag_is_peeled(self):
        api = Mock()
        api.request.side_effect = [{"object": {"type": "tag", "sha": "b" * 40}},
                                   {"object": {"type": "commit", "sha": SHA}}]
        self.assertEqual(gate.remote_tag_sha(api, TAG), SHA)

    def test_tag_movement_fails(self):
        with patch.object(gate.Path, "read_text", return_value='[package]\nversion="1.2.3"'), \
                patch.object(gate, "git", return_value=SHA), \
                patch.object(gate, "remote_tag_sha", return_value="b" * 40), \
                self.assertRaisesRegex(gate.GateError, "Remote tag"):
            gate.verify_source(Mock(), TAG, SHA)


class CITests(unittest.TestCase):
    def test_all_jobs_pass(self):
        gate.verify_ci(ci_api([run()]), SHA)

    def test_missing_and_other_sha_fail(self):
        for runs in ([], [run(head_sha="b" * 40)], [run(event="pull_request")]):
            with self.subTest(runs=runs), self.assertRaises(gate.GateError):
                gate.verify_ci(ci_api(runs), SHA)

    def test_latest_run_overrides_older_success(self):
        for status, conclusion in (("queued", None), ("in_progress", None), ("completed", "failure"),
                                   ("completed", "cancelled"), ("completed", "skipped"), ("completed", "timed_out")):
            with self.subTest(status=status, conclusion=conclusion), self.assertRaises(gate.GateError):
                gate.verify_ci(ci_api([run(), run(11, status=status, conclusion=conclusion)]), SHA)

    def test_new_success_supersedes_old_failure(self):
        gate.verify_ci(ci_api([run(conclusion="failure"), run(11)]), SHA)

    def test_latest_attempt_is_checked(self):
        api = ci_api([run(), run(run_attempt=2)])
        gate.verify_ci(api, SHA)
        self.assertIn("/attempts/2/jobs", api.pages.call_args[0][0])
        with self.assertRaises(gate.GateError):
            gate.verify_ci(ci_api([run(), run(run_attempt=2, status="queued", conclusion=None)]), SHA)

    def test_pending_other_branch_or_event_blocks(self):
        for change in ({"head_branch": "develop"}, {"event": "pull_request"}):
            with self.subTest(change=change), self.assertRaises(gate.GateError):
                gate.verify_ci(ci_api([run(), run(11, status="queued", conclusion=None, **change)]), SHA)

    def test_missing_skipped_failed_and_extra_failed_jobs(self):
        examples = [jobs()[:-1]]
        for conclusion in ("skipped", "failure", "cancelled", None):
            changed = jobs()
            changed[0]["conclusion"] = conclusion
            examples.append(changed)
        examples.append(jobs() + [dict(name="future job", status="completed", conclusion="failure")])
        for run_jobs in examples:
            with self.subTest(jobs=run_jobs), self.assertRaises(gate.GateError):
                gate.verify_ci(ci_api([run()], run_jobs), SHA)


class AssetTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.assets = []
        for target in gate.TARGETS:
            name = f"programmer-{target}." + ("zip" if "windows" in target else "tar.gz")
            (self.directory / name).write_bytes(b"archive fixture")
            self.assets.append(dict(name=name, state="uploaded", size=15,
                                    digest="sha256:" + hashlib.sha256(b"archive fixture").hexdigest()))

    def test_complete_assets(self):
        gate.verify_assets(self.directory, self.assets)

    def test_missing_extra_duplicate_incomplete_or_corrupt_upload(self):
        examples = [self.assets[:-1], self.assets + [self.assets[0]], self.assets[:-1] + [self.assets[0]]]
        for field, value in (("size", 0), ("state", "new"), ("digest", "sha256:bad")):
            changed = copy.deepcopy(self.assets)
            changed[0][field] = value
            examples.append(changed)
        for assets in examples:
            with self.subTest(assets=assets), self.assertRaises(gate.GateError):
                gate.verify_assets(self.directory, assets)

    def test_unexpected_and_empty_local_files(self):
        (self.directory / self.assets[0]["name"]).write_bytes(b"")
        with self.assertRaises(gate.GateError):
            gate.verify_assets(self.directory)
        (self.directory / "local-draft.md").write_text("must not upload")
        with self.assertRaises(gate.GateError):
            gate.verify_assets(self.directory)


class PublicationTests(unittest.TestCase):
    def invoke(self, mode, releases, release=None):
        api = Mock()
        api.pages.side_effect = lambda path, *arguments: releases if path == "/releases" else []
        api.request.return_value = release
        with patch.dict(gate.os.environ, {"GITHUB_REPOSITORY": "example/project", "GH_TOKEN": "offline-fixture"}), \
                patch.object(gate.sys, "argv", ["release_gate.py", mode, "--tag", TAG, "--sha", SHA, "--release-id", "1"]), \
                patch.object(gate.Path, "read_text", return_value='[package]\nversion="1.2.3"'), \
                patch.object(gate, "GitHub", return_value=api), \
                patch.object(gate, "verify_source", return_value=(SHA, "complete notes\n")), \
                patch.object(gate, "verify_assets"), \
                patch.object(gate, "remote_tag_sha", return_value=SHA), \
                patch.object(gate, "verify_ci"):
            try:
                with redirect_stdout(io.StringIO()):
                    gate.main()
            except gate.GateError:
                self.assertFalse(any(call.args[1:2] == ("PATCH",) for call in api.request.call_args_list))
                raise
        return api

    def test_public_release_never_overwritten(self):
        for mode in ("check", "prepare", "publish"):
            with self.subTest(mode=mode), self.assertRaisesRegex(gate.GateError, "public release"):
                self.invoke(mode, [dict(id=1, tag_name=TAG, draft=False)])

    def test_existing_draft_requires_explicit_cleanup(self):
        with self.assertRaisesRegex(gate.GateError, "draft already exists"):
            self.invoke("prepare", [dict(id=1, tag_name=TAG, draft=True)])

    def test_only_validated_draft_is_published(self):
        release = dict(id=1, tag_name=TAG, draft=True, body="complete notes", target_commitish=SHA)
        api = self.invoke("publish", [release], release)
        api.request.assert_called_with("/releases/1", "PATCH", {"draft": False})
        for field, value in (("body", "wrong notes"), ("target_commitish", "main"), ("id", 2), ("tag_name", "v1.2.4")):
            changed = dict(release, **{field: value})
            with self.subTest(field=field), self.assertRaises(gate.GateError):
                self.invoke("publish", [changed], changed)


class PaginationTests(unittest.TestCase):
    def test_ci_pagination_does_not_drop_newer_runs(self):
        api = gate.GitHub("example/project", "offline-fixture")
        with patch.object(api, "request", side_effect=[{"workflow_runs": [run()] * 100},
                                                       {"workflow_runs": [run(11)]}]):
            values = api.pages("/actions/workflows/ci.yml/runs?head_sha=" + SHA, "workflow_runs")
        self.assertEqual(len(values), 101)
        self.assertEqual(values[-1]["id"], 11)


if __name__ == "__main__":
    unittest.main()
