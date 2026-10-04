# Release procedure

A release needs explicit approval for its current change set. Approval does not
carry over to later changes. Never use `git add -A` to prepare a release.

1. Review `git status` and diffs. Stage explicit source/documentation paths only.
   Run `python3 scripts/check_release_scope.py` and inspect
   `git diff --cached --name-status` before committing. Local prototypes in
   `design-demos/` and runtime files in `.programmer/` must not enter the index;
   the scope checker permits only the small pre-existing tracked allowlist.
2. Update Cargo.toml/Cargo.lock and write a self-contained version section in
   RELEASE_NOTES.md. Include all shipped changes, not a reference to a withheld
   draft or only a changelog URL. Review this text before pushing.
3. Run tests, formatting and Clippy. Commit with the required coauthor trailer,
   push, and wait for the exact commit's CI workflow to finish successfully.
   Missing, pending, skipped, cancelled or failed checks are not approval.
4. Only then create and push a new immutable version tag on that commit. Never
   move an already published tag or overwrite a public release's assets.
5. Dispatch `.github/workflows/release.yml` with that tag. Its gates validate the
   tag/version, release scope, CI and version notes; builds use the resolved SHA.
   Artifacts are uploaded to a draft first, and publication requires verification
   of the full expected asset set and release body plus a fresh CI check.
6. Inspect the public release body, version and all ten downloads. A successful
   build alone is not a successful release. Report failures without advertising
   publication as complete.

The safeguards are tested offline with Python standard-library unittest:

```sh
python3 -m unittest discover -s scripts -p 'test_release*.py' -v
python3 scripts/check_release_scope.py
```

Workflow permissions cannot prevent a repository administrator from publishing
manually. Manual bypasses are not part of this procedure. Run the release workflow
from the protected/default branch containing these safeguards; historic workflow
revisions do not retroactively acquire the new checks.
