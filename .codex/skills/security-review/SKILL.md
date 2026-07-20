---
name: security-review
description: Review SaltedBlog security-sensitive changes. Use for authentication, sessions, MFA, CSRF, admin authorization, uploads, backup/restore, log export, outbound HTTP/SSRF, trusted proxies, Caddy boundaries, secrets, or error/log redaction.
---

# Security Review

1. Read [references/checklist.md](references/checklist.md) and select every applicable boundary.
2. Trace untrusted input from entrypoint through validation, storage, logging, and output.
3. Add regression tests for the rejected case as well as the allowed case.
4. Run `change-validation` for every touched subsystem; also run `deployment-preflight` when proxy, Caddy, secret, or container boundaries change.
5. Report residual risks explicitly. Never print or commit real credentials while testing.
