#!/usr/bin/env python3
"""Release gate for SaltedBlog. Run on a clean candidate commit."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

from versioning import highest_tag_version, parse_version, tag_exists, validate_consistency


ROOT = Path(__file__).resolve().parents[4]


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    executable = shutil.which(command[0])
    resolved = [executable, *command[1:]] if executable else command
    subprocess.run(resolved, cwd=ROOT, check=True)


def ensure_clean() -> None:
    run(["git", "diff", "--check"])
    result = subprocess.run(
        ["git", "status", "--porcelain"], cwd=ROOT, text=True, capture_output=True, check=True,
    )
    if result.stdout.strip():
        raise RuntimeError("worktree must be clean before release preflight")


def version_was_added_in_head() -> bool:
    base = os.environ.get("GITHUB_EVENT_BEFORE", "").strip()
    if not base or set(base) == {"0"}:
        base = "HEAD^"
    previous = subprocess.run(
        ["git", "cat-file", "-e", f"{base}:VERSION"], cwd=ROOT,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return previous.returncode != 0


def validate_candidate(version: str, bootstrap: bool) -> None:
    highest = highest_tag_version(ROOT)
    if bootstrap:
        if not version_was_added_in_head():
            raise RuntimeError("--bootstrap is only valid on the commit that first adds VERSION")
        if highest != version:
            raise RuntimeError(f"bootstrap VERSION {version} must equal latest tag {highest or '(none)'}")
        return
    if highest and parse_version(version) <= parse_version(highest):
        raise RuntimeError(f"candidate {version} must be newer than latest tag {highest}")
    if tag_exists(ROOT, version):
        raise RuntimeError(f"release tag already exists: v{version}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Run SaltedBlog's release preflight gate.")
    parser.add_argument("--skip-images", action="store_true", help="CI only: image matrix runs separately")
    parser.add_argument("--bootstrap", action="store_true", help="one-time VERSION initialization")
    args = parser.parse_args()

    for tool in ("git", "cargo", "npm"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"required tool is unavailable: {tool}")
    if not args.skip_images and shutil.which("docker") is None:
        raise RuntimeError("Docker is required for local release preflight")

    ensure_clean()
    version = validate_consistency(ROOT)
    validate_candidate(version, args.bootstrap)
    validation = ROOT / ".codex" / "skills" / "change-validation" / "scripts" / "validate_change.py"
    command = [sys.executable, str(validation), "--scope", "full"]
    if args.skip_images:
        command.append("--skip-images")
    run(command)
    print(f"release preflight passed for {'bootstrap ' if args.bootstrap else ''}v{version}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"release preflight failed: {error}", file=sys.stderr)
        raise SystemExit(1)
