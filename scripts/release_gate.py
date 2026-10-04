#!/usr/bin/env python3
"""Fail-closed release validation; Python 3.11+, standard library only."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request


TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "armv7-unknown-linux-gnueabihf", "riscv64gc-unknown-linux-gnu",
    "i686-unknown-linux-gnu", "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc", "i686-pc-windows-msvc",
    "x86_64-apple-darwin", "aarch64-apple-darwin",
)
REQUIRED_JOBS = {"fmt", "clippy", "doc", "test macos-latest", "test windows-latest", "release-policy"}
TAG_PATTERN = r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"


class GateError(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise GateError(message)


def git(*arguments):
    return subprocess.check_output(["git", *arguments], text=True).strip()


def validate_tag(tag, manifest):
    require(re.fullmatch(TAG_PATTERN, tag), "Tag must be a canonical vMAJOR.MINOR.PATCH")
    require(manifest["package"]["version"] == tag[1:], "Tag and Cargo.toml version differ")


def extract_notes(text, tag):
    # Same-level headings delimit sections; never include a previous release's draft.
    headings = list(re.finditer(r"^### programmer (v[^\s]+)([^\n]*)$", text, re.MULTILINE))
    matches = [index for index, heading in enumerate(headings) if heading[1] == tag]
    require(len(matches) == 1, f"Expected exactly one release notes section for {tag}")
    index = matches[0]
    heading = headings[index]
    require(not heading[2].strip(), "Release notes heading is marked as a draft")
    end = headings[index + 1].start() if index + 1 < len(headings) else len(text)
    body = text[heading.end():end].strip()
    body = re.sub(r"\n---\s*$", "", body).strip()
    require(not re.search(r"\b(draft|withheld|TODO|TBD|FIXME|placeholder|unreleased)\b|待补|待完善|草稿", body, re.I),
            "Release notes contain a draft reference or unfinished placeholder")
    require(not re.search(r"(?:listed|described|mentioned)\s+(?:above|below)|(?:see|includes?)\s+(?:the\s+)?(?:previous|prior)|见上|见下", body, re.I),
            "Release notes must be self-contained, not refer to another section")
    bullets = re.findall(r"^[-*] (.+)$", body, re.MULTILINE)
    require(any(len(bullet.strip()) >= 20 and not re.search(r"changelog|https?://", bullet, re.I)
                for bullet in bullets), "Release notes need substantive change descriptions, not only a changelog")
    require(re.search(r"\*\*Full changelog:\*\* https://github\.com/[^/\s]+/[^/\s]+/compare/"
                      + TAG_PATTERN + r"\.\.\." + re.escape(tag) + r"(?:\s|$)", body),
            "Release notes require a full changelog ending at the exact release tag")
    return body + "\n"


class GitHub:
    def __init__(self, repository, token):
        require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository), "Invalid GitHub repository")
        require(token, "GH_TOKEN is required")
        self.base = f"https://api.github.com/repos/{repository}"
        self.token = token

    def request(self, path, method="GET", data=None, missing_ok=False):
        request = urllib.request.Request(
            self.base + path,
            data=json.dumps(data).encode() if data is not None else None,
            method=method,
            headers={"Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
                     "X-GitHub-Api-Version": "2022-11-28", "Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if missing_ok and error.code == 404:
                return None
            raise GateError(f"GitHub {method} {path} failed: HTTP {error.code}") from error

    def pages(self, path, key=None):
        values = []
        separator = "&" if "?" in path else "?"
        for page in range(1, 101):
            response = self.request(f"{path}{separator}per_page=100&page={page}")
            batch = response[key] if key else response
            values.extend(batch)
            if len(batch) < 100:
                return values
        raise GateError("GitHub pagination exceeded safety limit")


def verify_ci(api, sha):
    # Do not filter by success: that would silently accept an older green run.
    runs = api.pages(f"/actions/workflows/ci.yml/runs?head_sha={sha}", "workflow_runs")
    runs = [run for run in runs if run["head_sha"] == sha]
    require(runs, f"No ci.yml run for release SHA {sha}")
    require(any(run["event"] == "push" for run in runs), "Release SHA needs a push CI run, not only PR merge checks")
    latest = {}
    for run in runs:
        group = (run["event"], run["head_branch"])
        if group not in latest or (run["run_number"], run["run_attempt"]) > (
                latest[group]["run_number"], latest[group]["run_attempt"]):
            latest[group] = run
    for run in latest.values():
        require(run["status"] == "completed" and run["conclusion"] == "success",
                f"Latest CI run {run['id']} is not completed/success; retry after CI succeeds")
        jobs = api.pages(f"/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs", "jobs")
        require(REQUIRED_JOBS <= {job["name"] for job in jobs}, f"CI run {run['id']} is missing required jobs")
        require(all(job["status"] == "completed" and job["conclusion"] == "success" for job in jobs),
                f"CI run {run['id']} has a non-successful job")


def remote_tag_sha(api, tag):
    reference = api.request(f"/git/ref/tags/{tag}")["object"]
    for _ in range(10):
        if reference["type"] == "commit":
            return reference["sha"]
        require(reference["type"] == "tag", "Tag does not resolve to a commit")
        reference = api.request(f"/git/tags/{reference['sha']}")["object"]
    raise GateError("Annotated tag nesting exceeds safety limit")


def verify_source(api, tag, expected_sha=None):
    validate_tag(tag, tomllib.loads(Path("Cargo.toml").read_text()))
    sha = git("rev-parse", "HEAD")
    require(re.fullmatch(r"[0-9a-f]{40}", sha), "Invalid checkout SHA")
    require(not expected_sha or sha == expected_sha, "Checkout differs from gate SHA")
    require(remote_tag_sha(api, tag) == sha, "Remote tag differs from checkout SHA")
    verify_ci(api, sha)
    return sha, extract_notes(Path("RELEASE_NOTES.md").read_text(), tag)


def verify_assets(directory, assets=None):
    expected = {f"programmer-{target}." + ("zip" if "windows" in target else "tar.gz") for target in TARGETS}
    files = {path.name: path for path in directory.iterdir()}
    require(set(files) == expected, "Expected exactly the ten release archives in dist")
    require(all(path.is_file() and path.stat().st_size > 0 for path in files.values()), "Release archive is empty or not a file")
    if assets is None:
        return
    require(len(assets) == len(expected) and {asset["name"] for asset in assets} == expected,
            "Draft release does not have exactly the ten expected assets")
    for asset in assets:
        path = files[asset["name"]]
        require(asset["state"] == "uploaded" and asset["size"] == path.stat().st_size,
                f"Asset upload is incomplete: {asset['name']}")
        if asset.get("digest"):
            digest = "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()
            require(asset["digest"] == digest, f"Asset digest mismatch: {asset['name']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("check", "prepare", "publish"))
    parser.add_argument("--tag", required=True)
    parser.add_argument("--sha")
    parser.add_argument("--release-id", type=int)
    parser.add_argument("--notes", default="release-body.md")
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    arguments = parser.parse_args()
    # Validate before the tag can reach an API path or a git argument.
    validate_tag(arguments.tag, tomllib.loads(Path("Cargo.toml").read_text()))
    api = GitHub(os.environ["GITHUB_REPOSITORY"], os.environ.get("GH_TOKEN"))
    sha, notes = verify_source(api, arguments.tag, arguments.sha)
    releases = api.pages("/releases")
    matching = [release for release in releases if release["tag_name"] == arguments.tag]
    require(not any(not release["draft"] for release in matching), "A public release already exists; refusing to overwrite")
    if arguments.mode != "publish":
        require(not matching, "A draft already exists; inspect/remove it explicitly before retrying")
        if arguments.mode == "prepare":
            require(arguments.sha, "prepare requires the gate SHA")
            verify_assets(arguments.dist)
        Path(arguments.notes).write_text(notes)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(f"sha={sha}\n")
        print(f"Release gate passed: {arguments.tag} at {sha}")
        return
    require(arguments.sha and arguments.release_id, "publish requires the gate SHA and draft release ID")
    require(len(matching) == 1 and matching[0]["id"] == arguments.release_id, "Draft release identity changed")
    release = api.request(f"/releases/{arguments.release_id}")
    require(release["draft"] and release["tag_name"] == arguments.tag, "Release is not the expected draft")
    require(release["body"].strip() == notes.strip(), "Draft body differs from validated release notes")
    require(release["target_commitish"] == sha, "Draft target differs from gate SHA")
    verify_assets(arguments.dist, api.pages(f"/releases/{arguments.release_id}/assets"))
    # Recheck immediately before the only operation that makes anything public.
    require(remote_tag_sha(api, arguments.tag) == sha, "Tag moved before publication")
    verify_ci(api, sha)
    api.request(f"/releases/{arguments.release_id}", "PATCH", {"draft": False})
    print(f"Published {arguments.tag} at {sha} with ten verified assets")


if __name__ == "__main__":
    try:
        main()
    except (GateError, OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Release gate failed: {error}", file=sys.stderr)
        sys.exit(1)
