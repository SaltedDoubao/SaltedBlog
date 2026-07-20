#!/usr/bin/env python3
"""Release gate for SaltedBlog. Run from the repository root."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path


def run(command: list[str], cwd: Path, *, check: bool = True) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(command), flush=True)
    return subprocess.run(command, cwd=cwd, text=True, check=check)


def require_tool(name: str) -> None:
    if shutil.which(name) is None:
        raise RuntimeError(f"required tool is unavailable: {name}")


def read_version(path: Path, pattern: str) -> str:
    match = re.search(pattern, path.read_text(encoding="utf-8"), re.MULTILINE)
    if match is None:
        raise RuntimeError(f"cannot find version in {path}")
    return match.group(1)


def check_versions(root: Path, version: str) -> None:
    expected_tag = f"v{version}"
    values = {
        "api/Cargo.toml": read_version(root / "api/Cargo.toml", r'^version = "([^"]+)"'),
        "api/migration/Cargo.toml": read_version(
            root / "api/migration/Cargo.toml", r'^version = "([^"]+)"'
        ),
        "web/package.json": json.loads((root / "web/package.json").read_text(encoding="utf-8"))["version"],
        "deploy/.env.example": read_version(
            root / "deploy/.env.example", r"^SALTEDBLOG_IMAGE_TAG=(.+)$"
        ),
        "README.md": read_version(root / "README.md", r"^# SALTEDBLOG_IMAGE_TAG=(.+)$"),
    }
    expected = {
        "api/Cargo.toml": version,
        "api/migration/Cargo.toml": version,
        "web/package.json": version,
        "deploy/.env.example": expected_tag,
        "README.md": expected_tag,
    }
    mismatches = [f"{path}: {actual} (expected {expected[path]})" for path, actual in values.items() if actual != expected[path]]
    if mismatches:
        raise RuntimeError("version mismatch:\n" + "\n".join(mismatches))


def ensure_tag_is_available(root: Path, tag: str) -> None:
    local = subprocess.run(["git", "tag", "--list", tag], cwd=root, text=True, capture_output=True, check=True)
    remote = subprocess.run(
        ["git", "ls-remote", "--tags", "origin", f"refs/tags/{tag}"],
        cwd=root,
        text=True,
        capture_output=True,
        check=True,
    )
    if local.stdout.strip() or remote.stdout.strip():
        raise RuntimeError(f"release tag already exists: {tag}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Run SaltedBlog's release preflight gate.")
    parser.add_argument("--version", required=True, help="Release version without the v prefix")
    args = parser.parse_args()
    if not re.fullmatch(r"\d+\.\d+\.\d+", args.version):
        raise SystemExit("--version must use MAJOR.MINOR.PATCH")

    root = Path(__file__).resolve().parents[4]
    for tool in ("git", "cargo", "npm", "docker"):
        require_tool(tool)

    run(["git", "diff", "--check"], root)
    status = subprocess.run(["git", "status", "--porcelain"], cwd=root, text=True, capture_output=True, check=True)
    if status.stdout.strip():
        raise RuntimeError("worktree must be clean before release preflight")
    check_versions(root, args.version)
    ensure_tag_is_available(root, f"v{args.version}")

    run(["cargo", "fmt", "--check"], root / "api")
    run(["cargo", "test", "--workspace", "--locked"], root / "api")
    run(["npm", "ci"], root / "web")
    run(["npm", "run", "build"], root / "web")

    for image, dockerfile in (("api", "deploy/Dockerfile.api"), ("web", "deploy/Dockerfile.web"), ("caddy", "deploy/Dockerfile.caddy")):
        run(["docker", "build", "--pull", "--file", dockerfile, "--tag", f"saltedblog-preflight:{image}", "."], root)
    run(
        [
            "docker", "run", "--rm", "--entrypoint", "/bin/sh", "saltedblog-preflight:api",
            "-c", "! ldd /usr/local/bin/salted-api | grep -q 'not found'",
        ],
        root,
    )
    print(f"release preflight passed for v{args.version}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(f"release preflight failed: {error}", file=sys.stderr)
        raise SystemExit(1)
