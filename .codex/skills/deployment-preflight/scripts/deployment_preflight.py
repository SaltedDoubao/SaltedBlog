#!/usr/bin/env python3
"""Deterministic deployment checks for SaltedBlog."""

from __future__ import annotations

import argparse
import base64
import os
import secrets
import shutil
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path


ROOT = Path(__file__).resolve().parents[4]
DEPLOY = ROOT / "deploy"
IMAGE_TAGS = {
    "api": "ghcr.io/salteddoubao/saltedblog-api:preflight",
    "web": "ghcr.io/salteddoubao/saltedblog-web:preflight",
    "caddy": "ghcr.io/salteddoubao/saltedblog-caddy:preflight",
}


def run(command: list[str], *, env: dict[str, str] | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(command), flush=True)
    executable = shutil.which(command[0])
    resolved = [executable, *command[1:]] if executable else command
    return subprocess.run(resolved, cwd=ROOT, env=env, text=True, check=check)


def require_docker() -> None:
    if shutil.which("docker") is None:
        raise RuntimeError("Docker is required for deployment validation")
    run(["docker", "version"])


def compose_base(project: str | None = None) -> list[str]:
    command = [
        "docker", "compose", "--project-directory", str(DEPLOY),
        "--env-file", str(DEPLOY / ".env.example"),
        "-f", str(DEPLOY / "docker-compose.yml"),
    ]
    if project:
        command[2:2] = ["--project-name", project]
    return command


def validate_compose() -> None:
    run([*compose_base(), "config", "--quiet"])
    run([
        *compose_base(), "-f", str(DEPLOY / "docker-compose.cloudflare.yml"),
        "config", "--quiet",
    ])


def validate_caddyfiles() -> None:
    common = [
        "docker", "run", "--rm",
        "-e", "SITE_DOMAIN=blog.example.com",
        "-e", "ADMIN_DOMAIN=admin.example.com",
        "-e", "ADMIN_ALLOWED_CIDRS=10.8.0.0/24",
        "-e", "UPSTREAM_PROXY_CIDRS=127.0.0.1/32",
    ]
    for name in ("Caddyfile", "Caddyfile.cloudflare"):
        mount = f"{(DEPLOY / name).resolve()}:/etc/caddy/Caddyfile:ro"
        run([*common, "-v", mount, "caddy:2.10-alpine", "caddy", "validate", "--config", "/etc/caddy/Caddyfile"])


def image_exists(image: str) -> bool:
    return subprocess.run(
        ["docker", "image", "inspect", image], cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ).returncode == 0


def build_images(images: list[str]) -> None:
    for image in images:
        run([
            "docker", "build", "--pull", "--file", f"deploy/Dockerfile.{image}",
            "--tag", IMAGE_TAGS[image], ".",
        ])
    if "api" in images:
        run([
            "docker", "run", "--rm", "--entrypoint", "/bin/sh", IMAGE_TAGS["api"],
            "-c", "! ldd /usr/local/bin/salted-api | grep -q 'not found'",
        ])


def remove_images(images: list[str]) -> None:
    for image in images:
        run(["docker", "image", "rm", "--force", IMAGE_TAGS[image]], check=False)


def write_secret(directory: Path, name: str, value: str) -> Path:
    path = directory / name
    path.write_bytes(value.encode("utf-8"))
    return path


def smoke_environment(secret_dir: Path) -> dict[str, str]:
    super_password = "preflight-super-password"
    owner_password = "preflight-owner-password"
    app_password = "preflight-app-password"
    values = {
        "postgres_superuser_password": super_password,
        "postgres_owner_password": owner_password,
        "postgres_app_password": app_password,
        "database_url": f"postgres://salted_app:{app_password}@postgres:5432/saltedblog",
        "database_maintenance_url": f"postgres://salted_owner:{owner_password}@postgres:5432/saltedblog",
        "admin_password": "preflight-admin-password",
        "mfa_encryption_key": base64.b64encode(secrets.token_bytes(32)).decode(),
        "backup_signing_key": base64.b64encode(secrets.token_bytes(32)).decode(),
        "news_llm_api_key": "",
    }
    variable_names = {
        "postgres_superuser_password": "POSTGRES_SUPERUSER_PASSWORD_FILE",
        "postgres_owner_password": "POSTGRES_OWNER_PASSWORD_FILE",
        "postgres_app_password": "POSTGRES_APP_PASSWORD_FILE",
        "database_url": "DATABASE_URL_SECRET_FILE",
        "database_maintenance_url": "DATABASE_MAINTENANCE_URL_SECRET_FILE",
        "admin_password": "ADMIN_PASSWORD_FILE",
        "mfa_encryption_key": "MFA_ENCRYPTION_KEY_FILE",
        "backup_signing_key": "BACKUP_SIGNING_KEY_FILE",
        "news_llm_api_key": "NEWS_LLM_API_KEY_FILE",
    }
    env = os.environ.copy()
    env.update({
        "SALTEDBLOG_IMAGE_TAG": "preflight",
        "SITE_DOMAIN": "blog.example.com",
        "ADMIN_DOMAIN": "admin.example.com",
        "ADMIN_ALLOWED_CIDRS": "10.8.0.0/24",
        "UPSTREAM_PROXY_CIDRS": "127.0.0.1/32",
    })
    for name, value in values.items():
        env[variable_names[name]] = str(write_secret(secret_dir, name, value).resolve())
    return env


def smoke_stack() -> None:
    missing = [image for image in ("api", "web") if not image_exists(IMAGE_TAGS[image])]
    secret_dir: Path | None = None
    command: list[str] | None = None
    env: dict[str, str] | None = None
    try:
        if missing:
            build_images(missing)
        local_root = ROOT / ".local"
        local_root.mkdir(exist_ok=True)
        secret_dir = Path(tempfile.mkdtemp(prefix="preflight-secrets-", dir=local_root))
        project = f"saltedblog-preflight-{uuid.uuid4().hex[:10]}"
        env = smoke_environment(secret_dir)
        command = compose_base(project)
        run([*command, "up", "--no-build", "--wait", "postgres", "db-init", "migrate", "api", "web"], env=env)
        result = subprocess.run(
            [*command, "ps", "--services", "--status", "running"],
            cwd=ROOT, env=env, text=True, capture_output=True, check=True,
        )
        running = set(result.stdout.splitlines())
        missing_services = {"postgres", "api", "web"} - running
        if missing_services:
            raise RuntimeError("smoke services are not healthy: " + ", ".join(sorted(missing_services)))
    except (RuntimeError, subprocess.CalledProcessError):
        if command is not None:
            run([*command, "logs", "--no-color", "postgres", "db-init", "migrate", "api", "web"], env=env, check=False)
        raise
    finally:
        if command is not None:
            run([*command, "down", "--volumes", "--remove-orphans"], env=env, check=False)
        if secret_dir is not None:
            shutil.rmtree(secret_dir, ignore_errors=True)
        remove_images(missing)


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate SaltedBlog deployment artifacts.")
    parser.add_argument("--mode", choices=("config", "images", "smoke", "full"), required=True)
    parser.add_argument("--images", nargs="+", choices=tuple(IMAGE_TAGS), default=[])
    args = parser.parse_args()
    if args.mode == "images" and not args.images:
        parser.error("--mode images requires --images")

    require_docker()
    if args.mode in {"config", "full"}:
        validate_compose()
        validate_caddyfiles()
    if args.mode == "images":
        images = list(dict.fromkeys(args.images))
        try:
            build_images(images)
        finally:
            remove_images(images)
    elif args.mode == "full":
        images = list(IMAGE_TAGS)
        try:
            build_images(images)
            smoke_stack()
        finally:
            remove_images(images)
    elif args.mode == "smoke":
        smoke_stack()
    print("deployment preflight passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"deployment preflight failed: {error}", file=sys.stderr)
        raise SystemExit(1)
