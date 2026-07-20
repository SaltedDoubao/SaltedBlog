"""Shared semantic-version helpers for SaltedBlog release scripts."""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path


SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def parse_version(value: str) -> tuple[int, int, int]:
    match = SEMVER.fullmatch(value.strip())
    if not match:
        raise RuntimeError("version must use MAJOR.MINOR.PATCH with numeric components")
    return tuple(map(int, match.groups()))


def replace_once(text: str, pattern: str, replacement: str, label: str) -> str:
    updated, count = re.subn(pattern, replacement, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise RuntimeError(f"cannot update version in {label}")
    return updated


def cargo_lock_version(text: str, package_name: str) -> str:
    for block in text.split("[[package]]"):
        if re.search(rf'^\s*name = "{re.escape(package_name)}"\s*$', block, re.MULTILINE):
            match = re.search(r'^\s*version = "([^"]+)"\s*$', block, re.MULTILINE)
            if match:
                return match.group(1)
    raise RuntimeError(f"cannot find {package_name} in api/Cargo.lock")


def update_cargo_lock(text: str, package_name: str, version: str) -> str:
    blocks = text.split("[[package]]")
    updated = 0
    for index, block in enumerate(blocks):
        if re.search(rf'^\s*name = "{re.escape(package_name)}"\s*$', block, re.MULTILINE):
            blocks[index], count = re.subn(
                r'(^\s*version = ")[^"]+("\s*$)', rf"\g<1>{version}\2", block,
                count=1, flags=re.MULTILINE,
            )
            updated += count
    if updated != 1:
        raise RuntimeError(f"cannot uniquely update {package_name} in api/Cargo.lock")
    return "[[package]]".join(blocks)


def read_versions(root: Path) -> dict[str, str]:
    cargo_lock = (root / "api/Cargo.lock").read_text(encoding="utf-8")
    package = json.loads((root / "web/package.json").read_text(encoding="utf-8"))
    package_lock = json.loads((root / "web/package-lock.json").read_text(encoding="utf-8"))

    def match(path: str, pattern: str) -> str:
        found = re.search(pattern, (root / path).read_text(encoding="utf-8"), re.MULTILINE)
        if not found:
            raise RuntimeError(f"cannot find version in {path}")
        return found.group(1)

    return {
        "VERSION": (root / "VERSION").read_text(encoding="utf-8").strip(),
        "api/Cargo.toml": match("api/Cargo.toml", r'^version = "([^"]+)"'),
        "api/migration/Cargo.toml": match("api/migration/Cargo.toml", r'^version = "([^"]+)"'),
        "api/Cargo.lock:salted-api": cargo_lock_version(cargo_lock, "salted-api"),
        "api/Cargo.lock:migration": cargo_lock_version(cargo_lock, "migration"),
        "web/package.json": package["version"],
        "web/package-lock.json": package_lock["version"],
        "web/package-lock.json:root": package_lock["packages"][""]["version"],
        "deploy/.env.example": match("deploy/.env.example", r"^SALTEDBLOG_IMAGE_TAG=v(.+)$"),
        "README.md": match("README.md", r"^# SALTEDBLOG_IMAGE_TAG=v(.+)$"),
    }


def validate_consistency(root: Path, expected: str | None = None) -> str:
    versions = read_versions(root)
    version = expected or versions["VERSION"]
    parse_version(version)
    mismatches = [f"{name}: {value} (expected {version})" for name, value in versions.items() if value != version]
    if mismatches:
        raise RuntimeError("version mismatch:\n" + "\n".join(mismatches))
    return version


def rendered_version_files(root: Path, version: str) -> dict[Path, str]:
    parse_version(version)
    files: dict[Path, str] = {root / "VERSION": version + "\n"}
    for relative in ("api/Cargo.toml", "api/migration/Cargo.toml"):
        path = root / relative
        files[path] = replace_once(
            path.read_text(encoding="utf-8"), r'^version = "[^"]+"', f'version = "{version}"', relative,
        )

    cargo_lock_path = root / "api/Cargo.lock"
    cargo_lock = cargo_lock_path.read_text(encoding="utf-8")
    cargo_lock = update_cargo_lock(cargo_lock, "salted-api", version)
    files[cargo_lock_path] = update_cargo_lock(cargo_lock, "migration", version)

    for relative in ("web/package.json", "web/package-lock.json"):
        path = root / relative
        data = json.loads(path.read_text(encoding="utf-8"))
        data["version"] = version
        if relative.endswith("package-lock.json"):
            data["packages"][""]["version"] = version
        files[path] = json.dumps(data, ensure_ascii=False, indent=2) + "\n"

    env_path = root / "deploy/.env.example"
    files[env_path] = replace_once(
        env_path.read_text(encoding="utf-8"), r"^SALTEDBLOG_IMAGE_TAG=v.+$",
        f"SALTEDBLOG_IMAGE_TAG=v{version}", "deploy/.env.example",
    )
    readme_path = root / "README.md"
    files[readme_path] = replace_once(
        readme_path.read_text(encoding="utf-8"), r"^# SALTEDBLOG_IMAGE_TAG=v.+$",
        f"# SALTEDBLOG_IMAGE_TAG=v{version}", "README.md",
    )
    return files


def local_semver_tags(root: Path) -> list[str]:
    result = subprocess.run(
        ["git", "tag", "--list", "v*"], cwd=root, text=True, capture_output=True, check=True,
    )
    return [tag for tag in result.stdout.splitlines() if SEMVER.fullmatch(tag.removeprefix("v"))]


def highest_tag_version(root: Path) -> str | None:
    tags = local_semver_tags(root)
    return max((tag[1:] for tag in tags), key=parse_version) if tags else None


def tag_exists(root: Path, version: str) -> bool:
    tag = f"v{version}"
    local = subprocess.run(
        ["git", "tag", "--list", tag], cwd=root, text=True, capture_output=True, check=True,
    )
    remote = subprocess.run(
        ["git", "ls-remote", "--tags", "origin", f"refs/tags/{tag}"],
        cwd=root, text=True, capture_output=True, check=True,
    )
    return bool(local.stdout.strip() or remote.stdout.strip())
