# HANDOVER — split this directory into the camelmailer org

This file briefs a local agent that has **no prior context**. It explains
where the project stands and exactly how to extract `camelmailer/` from
the Postal fork into its own repository under the **camelmailer** GitHub
org, with a rewritten (incognito) history. Read `CLAUDE.md` (same
directory) first for the project itself; this file only covers the split.

## Where you are

- The repo you are standing in is a fork of Postal
  (`relationsoftware/postal`), branch
  `claude/camelmailer-rust-refactor-in5d3g`.
- Everything that matters lives in the `camelmailer/` subdirectory —
  a complete, self-contained product (Rust workspace + Next.js frontend +
  template library + docs + CI workflow). The Ruby code around it is the
  original Postal application and is **not** taken along.
- The subdirectory is already split-out-ready: own `Dockerfile`,
  `docker-compose.yml`, `LICENSE` (MIT with Postal attribution),
  `.github/workflows/ci.yml` (becomes active at repo root), `.gitignore`.
- State of the code: 282 Rust tests green, clippy/fmt clean, Docker image
  builds, `next build` green, Playwright e2e green. ~33 commits touch the
  directory.

## The goal

1. New repository in the **camelmailer** org (already created on GitHub),
   containing only the contents of `camelmailer/` at repo root.
2. **History rewritten**: every commit re-authored to the owner
   (author *and* committer), all AI trailers removed
   (`Co-Authored-By: Claude …`, `Claude-Session: …`). **Keep the original
   timestamps** (author + committer dates stay as they are).
3. Postal attribution **stays** (README "born as a Rust rewrite of
   Postal", LICENSE notice, open-source page) — but there is **no
   upstream remote**: this is an independent project, not a tracking fork.
4. Clean out the last references to the old fork location.

## Step 1 — extract + rewrite history

Use `git filter-repo` (install: `pip install git-filter-repo` or
`brew install git-filter-repo`). Work on a **fresh clone** — filter-repo
refuses dirty/existing clones for good reasons.

```bash
git clone --branch claude/camelmailer-rust-refactor-in5d3g \
  https://github.com/relationsoftware/postal camelmailer-split
cd camelmailer-split

git filter-repo \
  --subdirectory-filter camelmailer \
  --name-callback  'return b"OWNER NAME"' \
  --email-callback 'return b"owner@example.com"' \
  --message-callback '
lines = message.split(b"\n")
kept = [l for l in lines
        if not l.startswith(b"Co-Authored-By:")
        and not l.startswith(b"Claude-Session:")]
while kept and kept[-1] == b"":
    kept.pop()
return b"\n".join(kept) + b"\n"'
```

Notes:
- Replace `OWNER NAME` / `owner@example.com` with the real identity
  (passed to you in the prompt).
- `--subdirectory-filter` makes `camelmailer/` the new root and drops
  every commit that only touched the Ruby app.
- The name/email callbacks rewrite **author and committer**; filter-repo
  does not touch dates, so timestamps are preserved automatically.
- filter-repo removes the origin remote by itself — the upstream is gone.

Verify before pushing:

```bash
git log --format='%an %ae | %cn %ce' | sort -u   # exactly one identity
git log --format=%B | grep -iE "claude|co-authored" || echo "clean"
git log --format='%ad %h %s' --date=short | tail -5  # old dates preserved
ls   # Cargo.toml, crates/, web/, templates/, docs/, Dockerfile at root
```

## Step 2 — clean the remaining fork references

All known references to the old location (verified by grep; the
`postalserver/postal` links are the intentional Postal attribution and
stay):

1. **`Cargo.toml`** (workspace root):
   `repository = "https://github.com/relationsoftware/postal"` →
   the new repo URL.
2. **`web/app/src/app/(marketing)/content.ts`** — 3 occurrences of
   `github.com/relationsoftware/postal` (GitHub links on the landing,
   templates and self-hosting pages) → the new repo URL.
3. Sweep to be sure nothing else slipped in:
   `grep -rn relationsoftware --exclude-dir=node_modules --exclude-dir=.next --exclude-dir=target .`
   (`.next/` and `target/` are build output — ignore or delete them).
4. `HANDOVER.md` (this file) has served its purpose — delete it in the
   same commit.

Commit these cleanups as the first new commit in the new repo (authored
by the owner, no AI trailers — configure `git config user.name/email`
accordingly before committing).

## Step 3 — push

```bash
git branch -m main
git remote add origin git@github.com:camelmailer/<REPO>.git
git push -u origin main
```

Do **not** add any remote pointing at relationsoftware/postal or
postalserver/postal.

## Step 4 — verify the result end to end

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets && cargo test --workspace
# Postgres integration tests too, if a Postgres is available:
#   CAMELMAILER_TEST_DATABASE_URL=postgres://<role-with-CREATEDB>@localhost/postgres cargo test

docker compose up -d --build && curl -fsS localhost:5000/health
cd web/app && pnpm install && pnpm run build
# optional full e2e (needs the Docker stack + an admin user; see web/README.md):
#   docker compose exec -e CAMELMAILER_USER_PASSWORD=... web camelmailer make-user ...
#   pnpm run dev & node e2e/smoke.mjs
```

The CI workflow (`.github/workflows/ci.yml`) becomes active on the first
push — it runs fmt + clippy `-D warnings` + the full test suite against a
Postgres service + a Docker build. It should be green as-is.

## Post-split backlog (not part of the split — just so it isn't lost)

- Legal pages (`web/app/src/app/(marketing)/legal/…` via `content.ts`)
  are placeholder templates: fill `[COMPANY …]` fields, have counsel
  review, remove the notice banners.
- Cloud pricing numbers on `/pricing` are plausible defaults, not
  validated business decisions; `cloud@camelmailer.example` /
  `security@…` / `abuse@…` addresses are placeholders.
- Deliberate feature gaps are listed at the end of `CLAUDE.md`.
- Consider GitHub org hygiene: branch protection on `main`, the repo
  description/website, topics (`email`, `smtp`, `rust`, `transactional`).
