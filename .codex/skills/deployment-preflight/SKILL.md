---
name: deployment-preflight
description: Validate SaltedBlog production deployment changes. Use for deploy Dockerfiles, Compose files, Caddyfiles, init-db.sh, production environment examples, base images, container users, health checks, networks, secrets, or image runtime changes.
---

# Deployment Preflight

Run from the repository root:

```powershell
python .codex/skills/deployment-preflight/scripts/deployment_preflight.py --mode full
```

- `config`: validate both Compose variants and both Caddyfiles.
- `images --images api web caddy`: build selected production images and verify API runtime libraries.
- `smoke`: start PostgreSQL, initialization, migration, API, and Web with generated temporary secrets in an isolated Compose project.
- `full`: run config, all image builds, and the smoke test.

Docker is mandatory. The script owns only `saltedblog-preflight-*` resources and must remove its temporary project, volumes, and secret files in `finally` cleanup.
