---
name: migration-safety
description: Validate SaltedBlog database migrations and persistence compatibility. Use when changing api/migration, SeaORM entities, database initialization, schema-dependent backup/restore behavior, or SQLite/PostgreSQL persistence semantics.
---

# Migration Safety

1. Add a new ordered migration; never edit or delete a migration contained in a published `v*` tag.
2. Keep SQLite and PostgreSQL SQL semantics compatible and register every migration exactly once.
3. Preserve existing data during `up`; make destructive transforms explicit and test representative old rows.
4. Run from the repository root:

   ```powershell
   python .codex/skills/migration-safety/scripts/validate_migrations.py --database-checks full
   ```

`full` runs a fresh SQLite migration and the disposable PostgreSQL deployment smoke test. Docker is mandatory. Use `--database-checks sqlite` only for intermediate development, never as final validation of a migration change.
