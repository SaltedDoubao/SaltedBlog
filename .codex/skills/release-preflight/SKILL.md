---
name: release-preflight
description: Prepare and validate SaltedBlog releases. Use for VERSION changes, semantic version bumps, v* tags, GHCR image publication, GitHub Releases, or checking an exact release candidate commit; do not use for an ordinary Dockerfile change without a version bump.
---

# Release Preflight

1. Start from a clean `main` and update every version surface:

   ```powershell
   python .codex/skills/release-preflight/scripts/bump_version.py --version 0.1.5
   ```

2. Review the version-only diff, commit it as `chore(release): v<version>`, then run on the clean candidate commit:

   ```powershell
   python .codex/skills/release-preflight/scripts/release_preflight.py
   ```

3. Push `main` and require the `release-preflight` Actions run for that exact SHA to succeed.
4. Create and push an annotated `v<version>` tag on the same SHA. Inspect `release-images` until it succeeds.

`--bootstrap` is reserved for the one commit that first adds `VERSION` at the already-published version. `--skip-images` is reserved for CI, whose image matrix performs the omitted builds. Docker is mandatory for the normal local gate. Never reuse or move a failed release tag; fix forward with the next patch version.
