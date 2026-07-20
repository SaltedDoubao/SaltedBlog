#!/usr/bin/env python3
"""Validate migration history, registration, and database execution."""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[4]
MIGRATION_ROOT = ROOT / "api" / "migration" / "src"
MIGRATION_NAME = re.compile(r"m\d{8}_\d{6}_[a-z0-9_]+")


def run(command: list[str], cwd: Path = ROOT, *, env: dict[str, str] | None = None) -> None:
    print("+", " ".join(command), flush=True)
    executable = shutil.which(command[0])
    resolved = [executable, *command[1:]] if executable else command
    subprocess.run(resolved, cwd=cwd, env=env, check=True)


def output(command: list[str], *, check: bool = True) -> str:
    return subprocess.run(command, cwd=ROOT, text=True, capture_output=True, check=check).stdout


def semver_key(tag: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", tag)
    return tuple(map(int, match.groups())) if match else (-1, -1, -1)


def latest_release_tag() -> str | None:
    tags = [tag for tag in output(["git", "tag", "--list", "v*"], check=False).splitlines() if semver_key(tag) >= (0, 0, 0)]
    return max(tags, key=semver_key) if tags else None


def migration_files(migration_root: Path = MIGRATION_ROOT) -> dict[str, Path]:
    return {
        path.stem: path
        for path in migration_root.glob("m*.rs")
        if MIGRATION_NAME.fullmatch(path.stem)
    }


def validate_registration(migration_root: Path = MIGRATION_ROOT) -> None:
    files = migration_files(migration_root)
    lib = (migration_root / "lib.rs").read_text(encoding="utf-8")
    modules = re.findall(r"^mod\s+(m\d{8}_\d{6}_[a-z0-9_]+);", lib, re.MULTILINE)
    registrations = re.findall(r"Box::new\((m\d{8}_\d{6}_[a-z0-9_]+)::Migration\)", lib)
    errors: list[str] = []
    if modules != sorted(modules) or registrations != sorted(registrations):
        errors.append("migration modules and registrations must be ordered by name")
    for name in sorted(files):
        if modules.count(name) != 1 or registrations.count(name) != 1:
            errors.append(f"{name} must be declared and registered exactly once")
    extras = (set(modules) | set(registrations)) - set(files)
    if extras:
        errors.append("registered migrations without files: " + ", ".join(sorted(extras)))
    if errors:
        raise RuntimeError("migration registration failed:\n" + "\n".join(errors))


def validate_published_history() -> None:
    tag = latest_release_tag()
    if not tag:
        return
    published = {
        path for path in output(["git", "ls-tree", "-r", "--name-only", tag, "api/migration/src"]).splitlines()
        if MIGRATION_NAME.fullmatch(Path(path).stem)
    }
    changes = output(["git", "diff", "--name-status", tag, "--", "api/migration/src"]).splitlines()
    offenders: list[str] = []
    for line in changes:
        parts = line.split("\t")
        status = parts[0][0] if parts else ""
        paths = parts[1:]
        if status in {"M", "D", "R"} and any(path in published for path in paths):
            offenders.append(line)
    if offenders:
        raise RuntimeError(f"published migrations from {tag} are immutable:\n" + "\n".join(offenders))


def sqlite_smoke() -> None:
    local_root = ROOT / ".local"
    local_root.mkdir(exist_ok=True)
    temp_dir = Path(tempfile.mkdtemp(prefix="migration-sqlite-", dir=local_root))
    relative = temp_dir.relative_to(ROOT).as_posix()
    database_url = f"sqlite://{relative}/blog.db?mode=rwc"
    env = os.environ.copy()
    env.update({
        "APP_ENV": "development",
        "DATABASE_URL": database_url,
        "DATABASE_MAINTENANCE_URL": database_url,
        "UPLOAD_DIR": f"{relative}/uploads",
        "BACKUP_DIR": f"{relative}/backups",
        "ADMIN_USERNAME": "preflight-admin",
        "ADMIN_PASSWORD": f"preflight-{uuid.uuid4().hex}",
    })
    try:
        run(["cargo", "run", "--locked", "--quiet", "--", "migrate"], ROOT / "api", env=env)
    finally:
        shutil.rmtree(temp_dir, ignore_errors=True)


def postgres_smoke() -> None:
    script = ROOT / ".codex" / "skills" / "deployment-preflight" / "scripts" / "deployment_preflight.py"
    run([sys.executable, str(script), "--mode", "smoke"])


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate SaltedBlog migrations.")
    parser.add_argument("--database-checks", choices=("none", "sqlite", "full"), default="full")
    args = parser.parse_args()
    if shutil.which("git") is None or shutil.which("cargo") is None:
        raise RuntimeError("git and cargo are required")
    validate_registration()
    validate_published_history()
    if args.database_checks in {"sqlite", "full"}:
        sqlite_smoke()
    if args.database_checks == "full":
        postgres_smoke()
    print("migration validation passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"migration validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
