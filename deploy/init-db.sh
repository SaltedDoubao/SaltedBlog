#!/bin/sh
set -eu

export PGPASSWORD="$(cat /run/secrets/postgres_superuser_password)"
OWNER_PASSWORD="$(cat /run/secrets/postgres_owner_password)"
APP_PASSWORD="$(cat /run/secrets/postgres_app_password)"

psql --host postgres --username "${POSTGRES_SUPERUSER:-salted_admin}" --dbname "${POSTGRES_DB:-saltedblog}" \
  --set ON_ERROR_STOP=1 --set owner_password="$OWNER_PASSWORD" --set app_password="$APP_PASSWORD" <<'SQL'
SELECT format('CREATE ROLE salted_owner LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE', :'owner_password')
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'salted_owner') \gexec
SELECT format('CREATE ROLE salted_app LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE CONNECTION LIMIT 30', :'app_password')
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'salted_app') \gexec
ALTER ROLE salted_owner PASSWORD :'owner_password';
ALTER ROLE salted_app PASSWORD :'app_password';
SELECT format('REVOKE CREATE, TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()) \gexec
ALTER SCHEMA public OWNER TO salted_owner;
SELECT format('ALTER TABLE %I.%I OWNER TO salted_owner', schemaname, tablename)
FROM pg_tables WHERE schemaname = 'public' \gexec
SELECT format('ALTER SEQUENCE %I.%I OWNER TO salted_owner', sequence_schema, sequence_name)
FROM information_schema.sequences WHERE sequence_schema = 'public' \gexec
SELECT format('GRANT CONNECT ON DATABASE %I TO salted_app', current_database()) \gexec
REVOKE ALL ON SCHEMA public FROM salted_app;
GRANT USAGE ON SCHEMA public TO salted_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO salted_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO salted_app;
SET ROLE salted_owner;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO salted_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO salted_app;
RESET ROLE;
SQL
