#!/bin/sh
set -eu
: "${SUMI_DB_APP_PASSWORD:?Set the runtime database password}"
psql --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
  --set ON_ERROR_STOP=1 --set app_password="$SUMI_DB_APP_PASSWORD" <<'SQL'
CREATE ROLE sumi_app LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD :'app_password';
SQL
