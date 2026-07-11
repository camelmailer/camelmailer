# Installing on Debian / Ubuntu (.deb)

Native packages for amd64 and arm64 are attached to every
[GitHub release](https://github.com/camelmailer/camelmailer/releases).
They install the `camelmailer` binary, three systemd units and
`/etc/camelmailer/camelmailer.yml`, and create a `camelmailer` system user.

## 1. Install the package

```bash
# pick the .deb matching your architecture (amd64 or arm64)
curl -fsSLO https://github.com/camelmailer/camelmailer/releases/latest/download/camelmailer_VERSION_amd64.deb
sudo dpkg -i camelmailer_*.deb
```

(If `dpkg -i` reports missing dependencies, `sudo apt-get -f install`
finishes the job.)

## 2. Provide PostgreSQL

PostgreSQL is intentionally not a hard dependency — it may run on the same
machine or anywhere else. For a local database:

```bash
sudo apt-get install postgresql
sudo -u postgres psql -c "CREATE ROLE camelmailer LOGIN PASSWORD 'change-me' NOSUPERUSER;"
sudo -u postgres psql -c "CREATE DATABASE camelmailer OWNER camelmailer;"
```

The role must **not** be a superuser: superusers bypass PostgreSQL
row-level security, which would silently disable tenant isolation.

## 3. Configure

Either edit the config file (kept across upgrades — it is a dpkg conffile):

```bash
sudo editor /etc/camelmailer/camelmailer.yml   # postgres: host/username/password/database
```

…or set `DATABASE_URL` (takes precedence over the `postgres:` group) in the
environment file read by all three units:

```bash
echo 'DATABASE_URL=postgres://camelmailer:change-me@localhost:5432/camelmailer' \
  | sudo tee /etc/camelmailer/env
sudo chown root:camelmailer /etc/camelmailer/env && sudo chmod 640 /etc/camelmailer/env
```

## 4. Migrate and start

```bash
sudo -u camelmailer CAMELMAILER_CONFIG_FILE_PATH=/etc/camelmailer/camelmailer.yml \
  camelmailer initialize    # creates/upgrades the schema

sudo systemctl enable --now camelmailer-web camelmailer-smtp camelmailer-worker
curl -s http://localhost:5000/health
```

The SMTP unit binds port 25 as the unprivileged `camelmailer` user via
`AmbientCapabilities=CAP_NET_BIND_SERVICE` — no extra setup needed.

## 5. Bootstrap the first account

```bash
sudo -u camelmailer CAMELMAILER_CONFIG_FILE_PATH=/etc/camelmailer/camelmailer.yml \
  camelmailer make-user you@example.com Ada Ops --admin
```

Then follow **[quickstart.md](quickstart.md)** from step 2 (create an admin
API key, organization, server, domain, send mail) — every command works the
same, just run `camelmailer …` directly instead of
`docker compose exec web camelmailer …`.

## Operations

```bash
systemctl status camelmailer-web camelmailer-smtp camelmailer-worker
journalctl -u camelmailer-worker -f      # delivery logs
sudo dpkg -i camelmailer_NEWVERSION_amd64.deb   # upgrade (config preserved)
sudo apt-get purge camelmailer           # full removal incl. config + user
```

For DKIM keys, DNS records, TLS and the production checklist see
**[configuration.md](configuration.md)**.
