#!/usr/bin/env python3
"""Synchronize all SaltedBlog version surfaces without committing or tagging."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

from versioning import highest_tag_version, parse_version, rendered_version_files, tag_exists, validate_consistency


ROOT = Path(__file__).resolve().parents[4]


def ensure_clean() -> None:
    result = subprocess.run(
        ["git", "status", "--porcelain"], cwd=ROOT, text=True, capture_output=True, check=True,
    )
    if result.stdout.strip():
        raise RuntimeError("worktree must be clean before changing the version")


def main() -> int:
    parser = argparse.ArgumentParser(description="Bump every SaltedBlog version surface.")
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    requested = args.version.strip()
    requested_key = parse_version(requested)
    ensure_clean()
    current = validate_consistency(ROOT)
    if requested_key <= parse_version(current):
        raise RuntimeError(f"new version {requested} must be greater than current version {current}")
    highest = highest_tag_version(ROOT)
    if highest and requested_key <= parse_version(highest):
        raise RuntimeError(f"new version {requested} must be greater than latest tag v{highest}")
    if tag_exists(ROOT, requested):
        raise RuntimeError(f"release tag already exists: v{requested}")

    rendered = rendered_version_files(ROOT, requested)
    for path, content in rendered.items():
        path.write_text(content, encoding="utf-8")
    validate_consistency(ROOT, requested)
    print(f"updated SaltedBlog version from {current} to {requested}")
    print("review the diff, then commit it as: chore(release): v" + requested)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"version bump failed: {error}", file=sys.stderr)
        raise SystemExit(1)
