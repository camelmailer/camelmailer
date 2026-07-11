#!/bin/sh
# Creates the application role and database at first boot of the postgres
# container. The role is deliberately NOT a superuser: superusers bypass
# PostgreSQL row-level security entirely, which would silently disable
# the tenant isolation on messages and the activity tables.
set -e

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname postgres <<-EOSQL
	CREATE ROLE camelmailer LOGIN PASSWORD '${CAMELMAILER_DB_PASSWORD}' NOSUPERUSER;
	CREATE DATABASE camelmailer OWNER camelmailer;
EOSQL
