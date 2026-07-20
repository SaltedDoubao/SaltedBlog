from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]


def load_module(name: str, relative: str):
    path = ROOT / relative
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


validate_change = load_module(
    "validate_change", ".codex/skills/change-validation/scripts/validate_change.py"
)
validate_migrations = load_module(
    "validate_migrations", ".codex/skills/migration-safety/scripts/validate_migrations.py"
)
deployment_preflight = load_module(
    "deployment_preflight", ".codex/skills/deployment-preflight/scripts/deployment_preflight.py"
)
versioning = load_module(
    "versioning", ".codex/skills/release-preflight/scripts/versioning.py"
)
release_preflight = load_module(
    "release_preflight", ".codex/skills/release-preflight/scripts/release_preflight.py"
)


class ChangeValidationTests(unittest.TestCase):
    def test_path_classification_unions_subsystems(self):
        paths = {"api/src/main.rs", "web/src/lib/api.ts", "deploy/Caddyfile"}
        self.assertEqual(validate_change.classify_paths(paths), {"api", "web", "deploy"})

    def test_docs_only_and_sensitive_artifacts(self):
        self.assertEqual(validate_change.classify_paths({"README.md"}), {"docs"})
        self.assertTrue(validate_change.forbidden_tracked_path("deploy/secrets/database_url"))
        self.assertFalse(validate_change.forbidden_tracked_path("deploy/secrets/README.md"))

    def test_markdown_link_parser_ignores_external_links(self):
        text = "[local](deploy/HARDENING.md) [web](https://example.com) [anchor](#top)"
        self.assertEqual(validate_change.markdown_link_targets(text), ["deploy/HARDENING.md"])


class MigrationValidationTests(unittest.TestCase):
    def test_registration_requires_order_and_single_registration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = "m20260720_000001_first"
            second = "m20260720_000002_second"
            (root / f"{first}.rs").write_text("", encoding="utf-8")
            (root / f"{second}.rs").write_text("", encoding="utf-8")
            (root / "lib.rs").write_text(
                f"mod {first};\nmod {second};\nvec![Box::new({first}::Migration), Box::new({second}::Migration)]\n",
                encoding="utf-8",
            )
            validate_migrations.validate_registration(root)
            (root / "lib.rs").write_text(
                f"mod {second};\nmod {first};\nvec![Box::new({second}::Migration), Box::new({first}::Migration)]\n",
                encoding="utf-8",
            )
            with self.assertRaises(RuntimeError):
                validate_migrations.validate_registration(root)


class VersioningTests(unittest.TestCase):
    def test_semver_is_strict(self):
        self.assertEqual(versioning.parse_version("1.2.3"), (1, 2, 3))
        for invalid in ("v1.2.3", "1.2", "1.2.3-rc1"):
            with self.assertRaises(RuntimeError):
                versioning.parse_version(invalid)

    def test_cargo_lock_updates_only_named_package(self):
        lock = """[[package]]
name = "salted-api"
version = "0.1.4"

[[package]]
name = "other"
version = "0.1.4"
"""
        updated = versioning.update_cargo_lock(lock, "salted-api", "0.1.5")
        self.assertEqual(versioning.cargo_lock_version(updated, "salted-api"), "0.1.5")
        self.assertEqual(versioning.cargo_lock_version(updated, "other"), "0.1.4")

    def test_rendered_files_are_consistent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "api/migration").mkdir(parents=True)
            (root / "web").mkdir()
            (root / "deploy").mkdir()
            (root / "VERSION").write_text("0.1.4\n", encoding="utf-8")
            (root / "api/Cargo.toml").write_text('[package]\nname = "salted-api"\nversion = "0.1.4"\n', encoding="utf-8")
            (root / "api/migration/Cargo.toml").write_text('[package]\nname = "migration"\nversion = "0.1.4"\n', encoding="utf-8")
            (root / "api/Cargo.lock").write_text(
                '[[package]]\nname = "salted-api"\nversion = "0.1.4"\n\n[[package]]\nname = "migration"\nversion = "0.1.4"\n',
                encoding="utf-8",
            )
            package = {"name": "web", "version": "0.1.4"}
            package_lock = {"name": "web", "version": "0.1.4", "packages": {"": {"version": "0.1.4"}}}
            (root / "web/package.json").write_text(json.dumps(package), encoding="utf-8")
            (root / "web/package-lock.json").write_text(json.dumps(package_lock), encoding="utf-8")
            (root / "deploy/.env.example").write_text("SALTEDBLOG_IMAGE_TAG=v0.1.4\n", encoding="utf-8")
            (root / "README.md").write_text("# SALTEDBLOG_IMAGE_TAG=v0.1.4\n", encoding="utf-8")
            for path, content in versioning.rendered_version_files(root, "0.1.5").items():
                path.write_text(content, encoding="utf-8")
            self.assertEqual(versioning.validate_consistency(root), "0.1.5")


class ReleaseCandidateTests(unittest.TestCase):
    def test_bootstrap_requires_first_version_file_and_current_tag(self):
        with mock.patch.object(release_preflight, "highest_tag_version", return_value="0.1.4"), \
             mock.patch.object(release_preflight, "version_was_added_in_head", return_value=True):
            release_preflight.validate_candidate("0.1.4", bootstrap=True)
        with mock.patch.object(release_preflight, "highest_tag_version", return_value="0.1.4"), \
             mock.patch.object(release_preflight, "version_was_added_in_head", return_value=False):
            with self.assertRaises(RuntimeError):
                release_preflight.validate_candidate("0.1.4", bootstrap=True)

    def test_normal_candidate_must_advance_and_have_no_tag(self):
        with mock.patch.object(release_preflight, "highest_tag_version", return_value="0.1.4"), \
             mock.patch.object(release_preflight, "tag_exists", return_value=False):
            release_preflight.validate_candidate("0.1.5", bootstrap=False)
            with self.assertRaises(RuntimeError):
                release_preflight.validate_candidate("0.1.4", bootstrap=False)


class DeploymentCleanupTests(unittest.TestCase):
    def test_smoke_failure_removes_temporary_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            temporary_root = Path(directory)
            calls = 0

            def failing_run(command, **kwargs):
                nonlocal calls
                calls += 1
                if "up" in command:
                    raise subprocess.CalledProcessError(1, command)
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(deployment_preflight, "ROOT", temporary_root), \
                 mock.patch.object(deployment_preflight, "image_exists", return_value=True), \
                 mock.patch.object(deployment_preflight, "run", side_effect=failing_run):
                with self.assertRaises(subprocess.CalledProcessError):
                    deployment_preflight.smoke_stack()
            self.assertGreaterEqual(calls, 2)
            leftovers = list((temporary_root / ".local").glob("preflight-secrets-*"))
            self.assertEqual(leftovers, [])


if __name__ == "__main__":
    unittest.main()
