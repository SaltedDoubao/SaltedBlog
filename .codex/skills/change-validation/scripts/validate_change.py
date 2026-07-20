#!/usr/bin/env python3
"""Path-aware local validation gate for SaltedBlog."""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[4]
SCOPES = {"auto", "docs", "api", "web", "deploy", "full"}


def run(command: list[str], cwd: Path = ROOT) -> None:
    print("+", " ".join(command), flush=True)
    executable = shutil.which(command[0])
    resolved = [executable, *command[1:]] if executable else command
    subprocess.run(resolved, cwd=cwd, check=True)


def output(command: list[str], cwd: Path = ROOT, *, check: bool = True) -> str:
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=check)
    return result.stdout


def require_tool(name: str) -> None:
    if shutil.which(name) is None:
        raise RuntimeError(f"required tool is unavailable: {name}")


def collect_changed_paths(base: str | None) -> set[str]:
    commands = [
        ["git", "diff", "--name-only", "--diff-filter=ACDMRTUXB"],
        ["git", "diff", "--cached", "--name-only", "--diff-filter=ACDMRTUXB"],
        ["git", "ls-files", "--others", "--exclude-standard"],
    ]
    if base:
        commands.append(["git", "diff", "--name-only", "--diff-filter=ACDMRTUXB", f"{base}...HEAD"])
    paths: set[str] = set()
    for command in commands:
        paths.update(line.strip().replace("\\", "/") for line in output(command).splitlines() if line.strip())
    return paths


def classify_paths(paths: set[str]) -> set[str]:
    scopes: set[str] = set()
    if any(path.startswith("api/") for path in paths):
        scopes.add("api")
    if any(path.startswith("web/") for path in paths):
        scopes.add("web")
    if any(path.startswith("deploy/") or path.startswith(".github/workflows/") for path in paths):
        scopes.add("deploy")
    if not scopes and paths:
        scopes.add("docs")
    return scopes


def check_git_whitespace() -> None:
    run(["git", "diff", "--check"])
    run(["git", "diff", "--cached", "--check"])


def forbidden_tracked_path(path: str) -> bool:
    normalized = path.replace("\\", "/")
    name = normalized.rsplit("/", 1)[-1]
    if normalized in {".env", "deploy/.env"}:
        return True
    if normalized.startswith("deploy/secrets/") and normalized != "deploy/secrets/README.md":
        return True
    if normalized.startswith(("data/", "backups/")):
        return True
    if "__pycache__" in normalized or name.endswith((".pyc", ".pyo")):
        return True
    return False


def check_tracked_artifacts() -> None:
    offenders = [path for path in output(["git", "ls-files"]).splitlines() if forbidden_tracked_path(path)]
    if offenders:
        raise RuntimeError("sensitive/runtime artifacts are tracked:\n" + "\n".join(offenders))


LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")


def markdown_link_targets(text: str) -> list[str]:
    targets: list[str] = []
    for raw in LINK_RE.findall(text):
        value = raw.strip()
        if value.startswith("<") and ">" in value:
            value = value[1 : value.index(">")]
        else:
            value = value.split(maxsplit=1)[0]
        if not value or value.startswith(("#", "/", "http://", "https://", "mailto:", "app://")):
            continue
        targets.append(value.split("#", 1)[0].replace("%20", " "))
    return targets


def check_markdown_links(extra_paths: set[str]) -> None:
    markdown = {line for line in output(["git", "ls-files", "*.md"]).splitlines() if line}
    markdown.update(path for path in extra_paths if path.lower().endswith(".md"))
    missing: list[str] = []
    for relative in sorted(markdown):
        source = ROOT / relative
        if not source.is_file():
            continue
        for target in markdown_link_targets(source.read_text(encoding="utf-8")):
            if not (source.parent / target).resolve().exists():
                missing.append(f"{relative}: {target}")
    if missing:
        raise RuntimeError("broken local Markdown links:\n" + "\n".join(missing))


def check_skill_structure() -> None:
    errors: list[str] = []
    skills_root = ROOT / ".codex" / "skills"
    for skill_dir in sorted(path for path in skills_root.iterdir() if path.is_dir()):
        skill_file = skill_dir / "SKILL.md"
        metadata_file = skill_dir / "agents" / "openai.yaml"
        if not skill_file.is_file():
            errors.append(f"{skill_dir.name}: missing SKILL.md")
            continue
        text = skill_file.read_text(encoding="utf-8")
        match = re.match(r"^---\n(.*?)\n---\n", text, re.DOTALL)
        if not match:
            errors.append(f"{skill_dir.name}: invalid frontmatter")
            continue
        fields = {
            key.strip(): value.strip()
            for key, value in (line.split(":", 1) for line in match.group(1).splitlines() if ":" in line)
        }
        if fields.get("name") != skill_dir.name or not fields.get("description"):
            errors.append(f"{skill_dir.name}: name/description mismatch")
        if not metadata_file.is_file():
            errors.append(f"{skill_dir.name}: missing agents/openai.yaml")
        elif f"${skill_dir.name}" not in metadata_file.read_text(encoding="utf-8"):
            errors.append(f"{skill_dir.name}: default_prompt must mention ${skill_dir.name}")
    if errors:
        raise RuntimeError("invalid skill structure:\n" + "\n".join(errors))


def run_api_checks() -> None:
    require_tool("cargo")
    run(["cargo", "fmt", "--check"], ROOT / "api")
    run(["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], ROOT / "api")
    run(["cargo", "test", "--workspace", "--locked"], ROOT / "api")


def run_web_checks() -> None:
    require_tool("npm")
    run(["npm", "ci"], ROOT / "web")
    run(["npm", "run", "check"], ROOT / "web")
    run(["npm", "run", "build"], ROOT / "web")


def deployment_images(paths: set[str]) -> list[str]:
    mapping = {
        "deploy/Dockerfile.api": "api",
        "deploy/Dockerfile.web": "web",
        "deploy/Dockerfile.caddy": "caddy",
    }
    return [image for path, image in mapping.items() if path in paths]


def run_deploy_checks(paths: set[str], *, full: bool, skip_images: bool) -> None:
    script = ROOT / ".codex" / "skills" / "deployment-preflight" / "scripts" / "deployment_preflight.py"
    if full and not skip_images:
        run([sys.executable, str(script), "--mode", "full"])
        return
    run([sys.executable, str(script), "--mode", "config"])
    images = deployment_images(paths)
    if images and not skip_images:
        run([sys.executable, str(script), "--mode", "images", "--images", *images])


def run_migration_checks(*, full_deployment: bool) -> None:
    script = ROOT / ".codex" / "skills" / "migration-safety" / "scripts" / "validate_migrations.py"
    database_checks = "sqlite" if full_deployment else "full"
    run([sys.executable, str(script), "--database-checks", database_checks])


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate SaltedBlog changes locally.")
    parser.add_argument("--scope", choices=sorted(SCOPES), default="auto")
    parser.add_argument("--base", help="Also validate changes between this ref and HEAD.")
    parser.add_argument("--skip-images", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()

    require_tool("git")
    paths = collect_changed_paths(args.base)
    selected = classify_paths(paths) if args.scope == "auto" else {args.scope}
    if args.scope == "full":
        selected = {"api", "web", "deploy"}

    print("Changed paths:", *(sorted(paths) or ["(none)"]), sep="\n  ")
    print("Selected scopes:", ", ".join(sorted(selected)) or "baseline only")

    check_git_whitespace()
    check_tracked_artifacts()
    check_markdown_links(paths)
    check_skill_structure()

    if "api" in selected:
        run_api_checks()
    if "web" in selected:
        run_web_checks()

    migration_changed = any(
        path.startswith("api/migration/") or path.startswith("api/src/entities/") for path in paths
    )
    if migration_changed or args.scope == "full":
        run_migration_checks(full_deployment=args.scope == "full")
    if "deploy" in selected:
        run_deploy_checks(paths, full=args.scope == "full", skip_images=args.skip_images)

    print("change validation passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"change validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
