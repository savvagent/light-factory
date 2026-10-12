# light-factory — Architecture

## Thesis

An agentic coding platform, in Rust, delivered as a **blazing fast TUI that humans run on
distributed machines**. Not a desktop app, not a web app, not a UI product.

The bet: catchy user interfaces are not necessary. Agents will increasingly communicate with
each other over channels and accomplish most work autonomously; human input becomes the
exception rather than the interface. So the product surface is the **protocol** — Command/Event
— and the TUI is simply the client a human attaches with. Another agent must be able to be a
first-class peer on that same seam.

The human stays in the loop through **plan-first approval**: the agent proposes a structured
plan, the human approves it once, and that approval authorizes the whole plan. Guardrails catch
deviation from what was approved, not each individual step.

Two predecessors shaped this:

- **otto** (https://github.com/robhicks/otto) — Rust engine; built a Dioxus desktop app, a web
  app, *and* a CLI at once, with a 19,794-line engine crate. Too much surface area for one
  developer. Also too aggressive: it applied edits autonomously.
- **dark-agent** (/home/robhicks/dev/dark-agent) — ratatui TUI hosting Claude Code; the
  "dark factory" idea that agents run the line and humans supervise.

**Scope discipline.** This is a solo project. Surface-area growth, not technical difficulty,
killed the last attempt. Deepen the TUI and the protocol; do not widen the product. otto's
design was sound — its seams are being ported. Its size was the problem.

## Execution model

The engine runs **locally, on the machine holding the repository**. Tool calls hit the real
filesystem with no network hop; source never leaves the machine. The fly.io server stays small
and stateless.

```
developer's machine                         fly.io                    Cloudflare Pages
┌────────────────────────────┐             ┌──────────────┐          ┌────────────────┐
│  light-factory (TUI)       │             │  Axum server │          │  Svelte SPA    │
│  ┌──────────────────────┐  │  HTTPS      │              │          │                │
│  │ engine (in-process)  │  │◄───────────►│  identity    │◄────────►│ sign-up/-in    │
│  │  session actor       │  │  auth only  │  (Postgres)  │          │ device approve │
│  │  plan gate           │  │             │              │          │                │
│  │  tools ── fs / bash  │  │             │  agent bus   │          └────────────────┘
│  └──────────┬───────────┘  │             │  (later)     │
│             │ Command/Event│             └──────┬───────┘
│      local repo (workspace)│                    │
└────────────────────────────┘             ┌──────▼───────┐
                                           │ LLM providers│
                                           └──────────────┘
```

The server's roles are identity today and the agent-to-agent bus later. It never holds a
workspace and never runs the loop.

**Why the engine is reached only through Command/Event**, even in-process: the protocol is the
product. The TUI talks to the engine over an mpsc channel using exactly the messages a remote
client or an agent peer would send. Making the engine a detachable daemon later is then a
transport change, not a redesign.

## Crate layout

Dependency flow is strictly inward.

```
crates/protocol      wire types — serde only, no I/O
                     auth DTOs · Command/Event · Plan/Scope · sensitive-path floor
crates/auth          domain: TOTP, AES-GCM secrets-at-rest, session/device tokens,
                     Store trait, AuthService — no I/O, no web framework
crates/persistence   sqlx Postgres PgStore + migrations
crates/server        axum HTTP + WebSocket
crates/engine-core   ported seams: Provider, Tool, Workspace/WorkspaceRead, PermissionGate,
                     PauseController, Decision
crates/providers     all seven otto providers + the base_url trust boundary + env selection
crates/tools         fs.read / fs.list / fs.write / bash
crates/engine        Engine, Session actor, turn state machine, PlanGate
crates/tui           ratatui client, binary `light-factory` (localized: en + es)
web/                 Svelte 5 SPA — auth and device approval only (localized: en + es)
```

**Localization.** Both clients are localized (English and Spanish). The TUI keeps its strings in
`crates/tui/src/i18n.rs` as `EN`/`ES` catalogs of dotted keys with `{param}` interpolation,
reached through `i18n::t` / `i18n::t_with`; a test asserts the two catalogs define exactly the
same key set. The web client mirrors this in `web/src/lib/i18n.js`. Any new user-facing string
goes through these catalogs — literals in client code are a convention violation, not a shortcut.

`protocol` is the dependency-free leaf. The sensitive-path floor lives there, rather than in
`engine-core`, so a tool that cannot take an engine-core dependency can still enforce it.

## Auth architecture

**Passwordless.** TOTP is the sole credential; no passwords exist anywhere in the system.

- **Registration** — `POST /auth/register` creates a pending account and returns an
  `otpauth://` URL plus a single-use setup token. `POST /auth/register/confirm` verifies the
  code, enables the account, and issues a session.
- **Login** — `POST /auth/login` takes email + TOTP code in one shot.
- **Distributed machines** sign in with the OAuth 2.0 Device Authorization Grant (RFC 8628).
  The TUI calls `POST /auth/device`, shows a short `user_code`, and polls
  `POST /auth/device/token`. The human approves once in the browser
  (`POST /auth/device/approve`), and the machine holds a 30-day bearer session at
  `$XDG_CONFIG_HOME/light-factory/session.json`. No password is ever typed into a box you
  don't fully trust.
- Corporate SSO is a planned seam, not built.

Secrets are stored encrypted (AES-256-GCM) under `LIGHT_SECRET_KEY`; tokens are stored only as
SHA-256 hashes. Single-use semantics for challenges and device grants are enforced in SQL with
`DELETE ... RETURNING`, so a concurrent poll cannot double-issue.

**No revenue model in the code.** An earlier token-balance metering scheme was removed
(migration `0003`). Token usage is reported for display only.

## Engine architecture

Built as a thin vertical slice — plan → approve → execute — running in-process in the TUI.
Full detail in `docs/superpowers/specs/2026-08-20-engine-core-design.md`.

- **Own agent loop** across pluggable providers, ported from otto — not a hosted subprocess
  agent.
- **Session actor** — owns a workspace, conversation history, the approved plan, and a
  monotonic `seq` counter. Events go out over a broadcast channel so a human client and an
  agent peer can observe the same session. `seq` exists so a reattaching client can replay;
  it is cheap now and expensive to retrofit.
- **Turn state machine** — `SendPrompt` → propose plan → await approval → execute → complete.
  Every gate fails closed; a client that disconnects with an approval parked is a denial.
- **The gate** is deterministic and involves no LLM. Reads are allowed anywhere in the
  workspace (an agent must be able to explore a repo). Writes and commands must fall inside the
  approved plan's declared scope. The **sensitive-path floor** always asks and cannot be
  silently unlocked by plan approval — for both file paths and any element of a command's
  argument vector.
- **No shell strings.** The `bash` tool takes a program and an args vector with no shell
  interpretation, because a gate cannot meaningfully evaluate `cargo test; rm -rf ~`. This is
  accepted as occasionally inconvenient in exchange for a guardrail that holds.

Scope and the floor do all the guarding. A separate per-tool risk-tier ladder was considered
and rejected: once approving a plan authorizes everything inside it, tiers change no outcome
and become a second thing to keep in sync.

## Deployment and CI

- Backend → fly.io (`fly.toml`, multi-stage Dockerfile), `https://light-factory.fly.dev`.
- SPA → Cloudflare Pages, `https://light-factory.pages.dev`.
- Postgres → the existing fly Managed Postgres cluster.
- A serverless triangle (Workers/GCR/Neon) was considered and rejected: a stateful WebSocket
  service is a poor fit for scale-to-zero, and Rust-on-Workers cannot run sqlx/tokio.
- `.github/workflows/deploy.yml` — clippy + tests gate, then Pages and `flyctl deploy` on
  pushes to master.
- `.github/workflows/bump.yml` — manual semver bump, commits and tags via a PAT.
- `.github/workflows/release.yml` — on `v*` tags, builds Linux (.deb/.rpm/.tar.gz), macOS
  aarch64 (.tar.gz/.dmg), Windows x86_64 (.zip) + checksums, publishes a GitHub Release.

**Note:** `auto_stop_machines = "stop"` is correct for today's stateless auth API. If the server
ever takes on a stateful role (the agent bus), that setting has to be revisited.

## Current state

| Area | State |
|---|---|
| protocol (auth DTOs, Ping/Pong) | Built |
| auth domain | Built, 10/10 tests green |
| persistence (PgStore, 4 migrations) | Built, integration test green |
| server (auth routes, `/ws`) | Built, deployed |
| web SPA (auth + device approve) | Built, deployed |
| TUI (sign-in, device login, WS) | Built, released as installers |
| Localization (en + es, both clients) | Built |
| CI/CD | Built |
| LLM providers | Built — all seven providers, the `base_url` trust boundary, env selection, TUI `/ask` |
| **engine core** | **Built — thin vertical slice (plan → approve → execute)** |
| Command/Event protocol | Built in `crates/protocol/src/session.rs`; `wire.rs` Ping/Pong still serves the fly server |
| Agent-to-agent bus | Not started |

## Environment

Operational facts for a fresh machine or session.

- Rust 1.97.0 stable, pinned in `rust-toolchain.toml`. Edition 2024.
- Local PostgreSQL 16.14, no sudo. Data dir `/home/robhicks/dev/light/.pgdata`
  (`initdb -U light --auth=trust`, UTF8, C.UTF-8). Start it with:
  `pg_ctl -D /home/robhicks/dev/light/.pgdata -o "-p 5432 -k /tmp/opencode -c listen_addresses=127.0.0.1" -l /tmp/opencode/pg.log start`
  `DATABASE_URL=postgres://light@127.0.0.1:5432/light`. May need restarting each session.
- flyctl authed as rob@hixfamily.org. App `light-factory`, region iad.
- fly Managed Postgres `savvagent-pg` (ID `kyzl60xmdjxopj9g`, org `savvagent`, iad, basic),
  shared with `nels-api`. light-factory uses database `light_factory` + user `light-factory`
  (MPG rejects `_` in usernames). Attach:
  `flyctl mpg attach kyzl60xmdjxopj9g --app light-factory --database light_factory --username light-factory`
- No docker; podman available.
- otto is checked out at `/home/robhicks/dev/otto` — same author, same MIT/Apache-2.0 dual
  license, so ported crates are a copy-and-rename rather than a dependency.

## Conventions

- No comments unless asked (doc comments on pub items are encouraged).
- Tests inline or in `tests/`; offline-deterministic where possible — the `MemStore` pattern in
  `crates/auth`, and `ScriptedProvider` for engine turns.
- Fail-closed auth and fail-closed gates; no user enumeration; secrets never returned to
  clients; TOTP seeds and codes never logged.
- `cargo fmt`, `cargo clippy -D warnings`, `cargo test` before calling a slice done.
- Commits: an interactive session does not commit unless asked. A development-workflow run
  (`CLAUDE.md` § Development Workflow) commits on its own branch in its own worktree and
  reaches `master` only through a merged PR. Nothing commits or pushes directly to `master`
  except the version commit `bump.yml` makes when Rob cuts a release.
  No attribution/Co-Authored-By anywhere.
- On close-out, mark the spec `IMPLEMENTED`, tick the plan's steps, and `git mv` both into
  `docs/superpowers/archive/`. The active spec/plan directories hold only work in flight.
- GitHub Actions pinned to full 40-char commit SHAs (never mutable tags/branches). To bump an
  action, resolve its tag with
  `git ls-remote https://github.com/<owner>/<repo>.git "refs/tags/<tag>^{}"` (the `^{}`
  dereferences annotated tags to the commit they point to). `dtolnay/rust-toolchain` is pinned
  to a `stable` branch-tip commit and must be bumped explicitly on toolchain changes.

## Design record

Design specs live in `docs/superpowers/specs/`, implementation plans in
`docs/superpowers/plans/`. **On close-out both move to `docs/superpowers/archive/`**, so the
active directories list only work in flight. Archiving is not deletion — the archive holds the
reasoning behind decisions the code cannot express, and some of it still binds (the localization
specs define the i18n contract every new user-facing string must follow). See
`docs/superpowers/archive/README.md`.

Active:

_None — the engine core slice and the providers port have shipped; their specs and plans are in
`docs/superpowers/archive/`._
