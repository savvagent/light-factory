# light-factory

An agentic coding platform in Rust, delivered as a TUI that humans run on their own machines. The
engine runs in-process in the TUI (`crates/engine`, `crates/tools`, `crates/providers`); the
backend (`crates/server`, `crates/auth`, `crates/persistence`, axum + sqlx Postgres) handles
passwordless identity only; `web/` is a Svelte 5 SPA for sign-up, sign-in, and device approval.

Key docs: `ARCHITECTURE.md` (thesis, crate layout, auth and engine design, deployment, local
environment, conventions; read it first), `README.md` (setup, run, env vars), and
`docs/superpowers/archive/README.md` (what shipped and why; the localization specs there still
bind every new user-facing string).

## Development Workflow

### Repo
- slug: savvagent/light-factory
- trunk: master
- tracker: github
- ticketless work: open-issue
- status labels: status:in-progress, status:in-review

### Tickets
- default assignee: none
- priority labels: none
- estimate labels: none
- types: bug=bug feature=enhancement docs=documentation

### Branch, commit, PR
- branch: <scope>-<slug>, flat kebab-case with no `/` and no type prefix (e.g. `tui-whitespace-api-key`, `providers-error-bodies`)
- commit: `<scope>: <imperative, lower-case subject>`; scope is a crate (`protocol`, `auth`, `persistence`, `server`, `engine-core`, `providers`, `tools`, `engine`, `tui`) or `web`, `docs`, `ci`, `chore`; body explains why
- PR body must contain: Closes #N
- merge: gh pr merge <N> --repo savvagent/light-factory --squash --delete-branch
- after merge: git branch -D <branch>

The PR title becomes the squash subject, so it follows the commit format. `master` has no branch
protection or ruleset: nothing but this workflow stops a direct push, and the merge gate below is
on the agent. Development runs commit on their own branch in a worktree; see the commit rule in
`ARCHITECTURE.md` § Conventions.

### Worktree
- path: .worktrees/<branch>
- bootstrap:
  - If you will run the database gate below: the dev cluster lives in the main checkout. `pg_ctl -D "$(git worktree list --porcelain | sed -n '1s/^worktree //p')/.pgdata" status`; if no server is running, start it with the `pg_ctl … start` command in `ARCHITECTURE.md` § Environment. Then `createdb -h 127.0.0.1 -p 5432 -U light lf_<branch, "-" → "_">`
  - If the change touches `web/**`: `(cd web && npm ci)`
- teardown: dropdb -h 127.0.0.1 -p 5432 -U light --if-exists lf_<branch, "-" → "_">

`.worktrees/` is gitignored; never `git add .` from the main checkout. Do not run
`scripts/dev-server.sh` from a worktree: it initializes a second cluster under the worktree's own
`.pgdata` on the same port. Each worktree gets its own scratch database because the persistence
test applies the branch's migrations: a branch migration applied to the shared `light` database
makes every other checkout's `run_migrations` fail, and the test then skips instead of failing.

### Gates
- format: cargo fmt --all -- --check
- lint: cargo clippy --workspace --all-targets --all-features -- -D warnings
- test: DATABASE_URL=postgres://light@127.0.0.1:5432/lf_<branch, "-" → "_"> cargo test --workspace
- extra: test -z "$(git diff --name-only --diff-filter=MDR origin/master...HEAD -- crates/persistence/migrations/)"
- when crates/persistence/**, crates/auth/src/store.rs: DATABASE_URL=postgres://light@127.0.0.1:5432/lf_<branch, "-" → "_"> cargo test -p light-factory-persistence --test pg_store -- --nocapture
- when web/**: cd web && npm run build
- when web/src/lib/i18n.js, web/src/lib/i18n.test.mjs: node web/src/lib/i18n.test.mjs
- when Dockerfile, .dockerignore, rust-toolchain.toml: podman build -t light-factory:check .
- when .github/workflows/**: ! grep -rhE '^\s*(- )?uses:' .github/workflows | grep -vE '@[0-9a-f]{40}(\s|$)'
- evidence: report the pass count per test binary. The database gate must report its tests passed (2 today) and must not print `skipping: Postgres unavailable`; a skip is a failed gate. CI (`Test and lint`) has no Postgres, so the persistence test always skips there and CI is no evidence for it.

CI runs `lint` and `test` only; `format` is local. Podman, not docker, is installed on the dev
host. No actionlint is installed; the workflow gate checks only the SHA-pin rule, so review
workflow changes by hand. `scripts/*.sh` and `.github/scripts/bump_version.py` have no gate:
run them by hand (`python3 .github/scripts/bump_version.py get`) and say so in the PR body.

### Design docs
- mode: committed
- spec path: docs/superpowers/specs/YYYY-MM-DD-<slug>-design.md
- plan path: docs/superpowers/plans/YYYY-MM-DD-<slug>.md
- shipped: a final commit in the same PR before merge: the spec's `> **Status:**` becomes `IMPLEMENTED (#<PR>)`, the plan's boxes are ticked, both are `git mv`ed to `docs/superpowers/archive/{specs,plans}/`, a row is added to `docs/superpowers/archive/README.md`, and the "Active" list in `ARCHITECTURE.md` § Design record is updated if it names them

Specs open with a `> **Status:**` line and `> **Implements:** <issue URL>`; plans with a
`**Spec:**` line and `**Source:**` (see the active or archived ones for shape). A fast-path run
still commits its single-task plan, and its archive row says "plan only".

### Reviews
- language reviewers: crates/** -> rust-pro, web/** -> frontend-developer, .github/scripts/** -> python-pro, scripts/** -> devops
- extra reviewers: database-optimizer when `crates/persistence/**` changes; deployment-engineer when `.github/workflows/**`, `Dockerfile`, or `fly.toml` changes
- security focus: no user enumeration (unknown email and bad TOTP are both `InvalidCredentials`); TOTP as the only credential; AES-256-GCM seeds under `LIGHT_SECRET_KEY`, fail-closed at startup; bearer and device tokens stored only as SHA-256 hashes; single-use challenges and device grants (`DELETE … RETURNING`); secrets, codes, and keys never logged, echoed in errors, or returned after setup; provider API keys (keyring storage, env reads, redaction in error text) and the provider `base_url` trust boundary; the `PlanGate` scope check and the sensitive-path floor on paths and every bash argument; no shell interpretation in the `bash` tool; untrusted provider and tool output reaching the terminal
- automated reviewer: copilot-pull-request-reviewer

### Ship
- deploy: ci-auto
- merge gate: on the PR's head SHA, the `Test and lint` check (workflow `CI and Deploy`) succeeded; `Web -> Cloudflare Pages` and `Backend -> Fly.io` report SKIPPED on pull requests, which is expected; plus every other check that ran on the head SHA (no branch protection requires any), plus the local `format` gate and any `when` gate CI does not run
- verify: find the run for the merge SHA (`gh run list -R savvagent/light-factory --workflow deploy.yml --commit <merge SHA> --json databaseId,status,conclusion`, then `gh run view <id> -R savvagent/light-factory --json jobs`); its `Web -> Cloudflare Pages` and `Backend -> Fly.io` jobs are the SHA proxy, since `/health` reports no SHA. Web: `curl -s https://light-factory.pages.dev/` references the same `assets/index-*.js` name that `npm run build` produces at the merge SHA (Vite names are content hashes). Backend: `fly releases -a light-factory` shows a new release, `fly status -a light-factory` shows the machine on it, `curl -s https://light-factory.fly.dev/health` returns `{"status":"ok"}` (the first request may start a stopped machine), and `fly logs -a light-factory --no-tail` shows `light-factory listening on`; then exercise the changed `/auth/...` route read-only. Until #76 is resolved the Fly app does not exist and the `Backend -> Fly.io` job fails: a change to the server surface is merged, not live; a change with no server surface reports that job's failure as #76 and verifies the Pages job and the Pages page only.
- release: none for a change PR. The server and SPA deploy on every push to `master`. The TUI ships only in installers: `bump.yml` (manual dispatch) bumps the shared version, pushes the bump commit to `master` itself, and tags `v*`; `release.yml` builds the installers and the GitHub Release. Cutting one is Rob's decision. A breaking change to `protocol` wire types or a crate's public API bumps the shared version in the same PR with `python3 .github/scripts/bump_version.py minor` (0.x minor position); additive changes bump nothing.
- version files: Cargo.toml (`[workspace.package] version`, inherited by every crate), Cargo.lock (the `light-factory-*` entries), web/package.json

The deploy pipeline ships only `crates/server` and what it links (`auth`, `persistence`,
`protocol`) plus `web/`. A change confined to `crates/tui`, `engine`, `engine-core`, `providers`,
or `tools`, or to docs, redeploys unchanged server and SPA code: verify health only and state
that the changed surface is not deployed (it reaches users in the next installer release). A push
whose changes are all in `Cargo.toml`, `Cargo.lock`, or `web/package.json` triggers no run at all
(`paths-ignore`, so `bump.yml` commits do not deploy): that merge is not deployed.

### Out-of-band
- Migrations (agent): applied by `run_migrations` at server boot; confirm in `fly logs`. A failing migration stops the server from starting.
- New server config key (agent, in the PR): `crates/server/src/config.rs`, `.env.example`, the env table in `README.md`, and `fly.toml` `[env]` when production needs a non-default value.
- New server secret (human: Rob, before merge): `fly secrets set -a light-factory NAME=…`.
- New SPA build-time variable (agent, in the PR): `web/.env.production` (committed, non-secret only).
- New GitHub Actions secret (human: Rob): `gh secret set`; the workflow's header comment lists the secrets it needs.
- Fly app and Postgres attachment (human: Rob): see #76.
- TUI installer release (human: Rob): see Ship > release.

### Human-only
Hand these to Rob with the exact command; never run them:
- Any write, DDL, or role change on the production database (`savvagent-pg` on Fly Managed
  Postgres, shared with `nels-api`). Production diagnostics are read-only.
- `fly secrets set`, `gh secret set`, and Cloudflare account or Pages project settings.
- A hand deploy: `fly deploy`, or `npm run deploy` (wrangler) from `web/`.
- Anything that adds recurring cost: creating or scaling a Fly app or machine, a volume, a
  database, or a paid service.
- Running `bump.yml`, pushing a `v*` tag, or editing or deleting a GitHub Release.

### Invariants
Reviewers check every one. Detail lives in `ARCHITECTURE.md` (§ Auth architecture, § Engine
architecture, § Crate layout, § Conventions) and the archived specs.
- **Auth spine** (`ARCHITECTURE.md` § Auth architecture): passwordless, TOTP only; no user
  enumeration; seeds encrypted under `LIGHT_SECRET_KEY`, read only in `crates/server/src/config.rs`
  and fail-closed at startup; tokens stored hashed; challenges and device grants single-use in SQL;
  no secret logged, echoed, or returned after setup.
- **Dependency flow is inward.** `protocol` depends only on serde. `auth` defines the `Store`
  trait and never depends on `persistence` or `server`. `persistence` → `auth`; `server` →
  `auth`, `persistence`, `protocol`; `engine-core` → `protocol`; `providers` → `engine-core`;
  `tools` → `engine-core`, `protocol`; `engine` → `protocol`, `engine-core`, `providers`, `tools`;
  `tui` → `protocol`, `providers`, `engine`. A new edge is a design decision for the spec.
- **Human-in-the-loop gating** (`ARCHITECTURE.md` § Engine architecture): every gate fails closed;
  writes and commands stay inside the approved plan's scope; the sensitive-path floor always asks
  and plan approval never unlocks it; the `bash` tool takes an argument vector, never a shell string.
- **Append-only migrations.** A schema change is a new `crates/persistence/migrations/NNNN_<name>.sql`;
  never edit, rename, or delete an applied one (sqlx checksums them and the server refuses to
  boot). Migrations must be compatible with the server still running during rollout.
- **Semver.** `protocol` wire types and crate public APIs change additively (new optional fields,
  new variants, new DTOs). Removing, renaming, or reordering a field or variant, or changing a
  signature, is breaking: named in the spec and bumped per Ship > release. An externally tagged
  enum without `#[serde(other)]` still rejects a new variant on an old receiver.
- **Localization.** Every user-facing string goes through the `EN`/`ES` catalogs
  (`crates/tui/src/i18n.rs`, `web/src/lib/i18n.js`), and both must keep the same key set.
- **Tests stay offline-deterministic** where possible: `MemStore` for auth, `ScriptedProvider` for
  engine turns; no test reads the developer's real keyring or ambient provider env.
- **GitHub Actions are pinned to full 40-character commit SHAs** (`ARCHITECTURE.md` § Conventions).
- **Scope discipline.** Deepen the TUI and the protocol; do not widen the product surface
  (`ARCHITECTURE.md` § Thesis).
