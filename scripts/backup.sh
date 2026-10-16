#!/bin/sh
set -eu
umask 077
if [ "$#" -ne 1 ]; then
  echo 'Usage: DATABASE_URL=... scripts/backup.sh /path/to/new-backup.dump' >&2
  exit 2
fi
if [ -e "$1" ]; then
  echo 'Backup destination already exists' >&2
  exit 2
fi
: "${DATABASE_URL:?Set the administrative PostgreSQL connection URL}"
backup_tmp=$(mktemp "$1.partial.XXXXXX")
trap 'rm -f "$backup_tmp"' 0
trap 'exit 1' HUP INT TERM
"${SUMI_PG_DUMP:-pg_dump}" --format=custom --file="$backup_tmp" --dbname="$DATABASE_URL"
# A link publishes a complete backup without replacing an existing destination.
ln "$backup_tmp" "$1"
