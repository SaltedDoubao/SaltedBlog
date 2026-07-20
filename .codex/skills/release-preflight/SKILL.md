---
name: release-preflight
description: Validate SaltedBlog releases before pushing a v* tag. Use for any version bump, image release, Dockerfile/base-image change, release tag, or request to publish a GitHub release.
---

# Release Preflight

Run this workflow before creating or pushing any `v*` release tag.

1. Determine the target version and ensure every project version and deployment image-tag example matches it.
2. Run the deterministic gate from the repository root:

   ```powershell
   python .codex/skills/release-preflight/scripts/release_preflight.py --version 0.1.4
   ```

   The gate checks a clean worktree, version consistency, absence of the local and remote tag,
   formatting, Rust tests, the web build, all production Docker builds, and API dynamic-library
   resolution inside the runtime image.
3. Push `main`, wait for the `release-preflight` GitHub Actions run for that exact commit to pass,
   then create and push the annotated `v<version>` tag.
4. Inspect the triggered `release-images` run. Do not move, delete, or force-update a failed
   release tag; fix forward and issue the next patch version instead.

If Docker is unavailable locally, stop before tagging. Push the candidate commit and use the
`release-preflight` GitHub Actions workflow to obtain an equivalent clean build before release.
