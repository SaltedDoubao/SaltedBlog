---
name: change-validation
description: Validate SaltedBlog changes locally with path-aware checks. Use after modifying source, configuration, documentation, workflows, AGENTS.md, or repository skills, and before reporting or committing completed work.
---

# Change Validation

Run from the repository root:

```powershell
python .codex/skills/change-validation/scripts/validate_change.py --scope auto
```

- Use `--base <ref>` when validating commits rather than only working-tree changes.
- Use `--scope api`, `web`, or `deploy` to force a subsystem gate.
- Use `--scope full` before a release or after a cross-cutting/high-risk change.
- Never treat missing Docker as success for deployment or full validation.

The script always checks whitespace, tracked secrets/runtime artifacts, local Markdown links, and skill structure. It then runs the union of checks required by the changed paths. Report the selected paths, commands, and failures; do not weaken a gate to make it pass.
