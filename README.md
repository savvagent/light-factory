# light-factory

An agentic coding platform in Rust, delivered as a **blazing fast TUI that humans run on
distributed machines**. The product surface is the **Command/Event protocol**; the TUI is just
the client a human attaches with, and another agent must be able to be a first-class peer on
that same seam.

- **Engine** runs locally on the machine holding the repo — tool calls hit the real filesystem,
  source never leaves the machine.
- **Human-in-the-loop** via plan-first approval: the agent proposes a plan, you approve once, and
  guardrails enforce what was approved (not each step).
- **Passwordless auth** — TOTP is the sole credential. Distributed machines sign in with the
  OAuth 2.0 Device Authorization Grant (RFC 8628).

Deep design rationale lives in [`ARCHITECTURE.md`](ARCHITECTURE.md). Read it before making
changes.

## Repo layout

Dependency flow is strictly inward.

```
crates/protocol      wire types — serde only, no I/O (the leaf)
crates/auth          domain: TOTP, AES-GCM secrets-at-rest, session/device tokens
crates/persistence   sqlx Postgres PgStore + migrations
crates/server        axum HTTP + WebSocket (binary: light-factory-server)
crates/engine-core   ported seams: Provider, Tool, Workspace, PermissionGate, PauseController
crates/providers     seven LLM providers + the base_url trust boundary + env selection
crates/tools         fs.read / fs.list / fs.write / bash
crates/engine        Engine, Session actor, turn state machine, PlanGate
crates/tui           ratatui client (binary: light-factory), localized en + es
web/                 Svelte 5 SPA — auth and device approval only, localized en + es
docs/superpowers/    design specs and implementation plans (active → archive on close-out)
```

## Prerequisites

- **Rust 1.97.0** stable — pinned in `rust-toolchain.toml`; `rustup` will pick it up. Edition 2024.
- **PostgreSQL 16** (for the server and its integration test).
- **Node 24** + npm (web client only).
- `libdbus-1-dev` + `pkg-config` on Linux (the TUI uses `keyring`, which needs D-Bus).

## Setup

```sh
# 1. Toolchain
rustup show            # installs the pinned 1.97.0 + rustfmt + clippy

# 2. Environment (server only)
cp .env.example .env   # or create .env by hand — see below

# 3. Web deps (web client only)
cd web && npm ci
```

Postgres is initialized and started by the dev-server script (see below) — no manual
`initdb`/`pg_ctl` needed, and it never wipes an existing cluster.

## Run

```sh
# Server: starts PostgreSQL (idempotent, keeps existing data) and runs the server
./scripts/dev-server.sh

# TUI (the product), in a second terminal
./scripts/dev-tui.sh

# Web SPA (auth + device approval), needs the server running
cd web && npm run dev                       # http://localhost:5173
```

The scripts just wrap `cargo run`, so the underlying commands are:

```sh
cargo run -p light-factory-server          # runs migrations on startup
cargo run -p light-factory-tui             # binary: light-factory
```

## Test & lint

Run all three before calling a slice done (CI runs clippy and the tests; the format check is local):

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

The persistence integration test needs the local Postgres cluster up and `DATABASE_URL` set
(default `postgres://light@127.0.0.1:5432/light`).

## Environment variables

The server reads config from the environment (`.env` is loaded via `dotenvy`):

| Variable | Default | Notes |
|---|---|---|
| `DATABASE_URL` | `postgres://light@127.0.0.1:5432/light` | Postgres connection string |
| `LIGHT_SECRET_KEY` | _(required)_ | base64-encoded 32-byte key; `openssl rand -base64 32` |
| `ADDR` | `127.0.0.1:8080` | TCP bind address |
| `CORS_ORIGINS` | `http://localhost:5173` | comma-separated; set to Pages origin in prod |
| `DEVICE_VERIFICATION_URI` | `http://localhost:5173` | origin shown in device-auth codes |
| `RUST_LOG` | `light_factory_server=info,tower_http=info` | tracing filter |

Secrets are never committed; set them with `fly secrets set` in production.

## Conventions

- No comments unless asked (doc comments on pub items are encouraged).
- Fail-closed auth and fail-closed gates; no user enumeration; secrets never returned to
  clients; TOTP seeds and codes never logged.
- **Localization**: every user-facing string goes through the `EN`/`ES` catalogs
  (`crates/tui/src/i18n.rs`, `web/src/lib/i18n.js`); a test enforces key parity. Literals in
  client code are a convention violation.
- Tests inline or in `tests/`; offline-deterministic where possible (`MemStore` in `crates/auth`,
  `ScriptedProvider` for engine turns).
- `cargo fmt`, `cargo clippy -D warnings`, `cargo test` before calling a slice done.
- Design specs live in `docs/superpowers/specs/`, plans in `docs/superpowers/plans/`; on
  close-out mark the spec `IMPLEMENTED`, tick the plan, and `git mv` both into
  `docs/superpowers/archive/`.
- GitHub Actions refs pinned to full 40-char commit SHAs.
- Interactive sessions do not commit unless asked; development-workflow runs commit on their
  own branch in a worktree and merge through a PR (`ARCHITECTURE.md` § Conventions). Nothing
  commits directly to `master`. No attribution / Co-Authored-By anywhere.

## Documentation

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — thesis, execution model, auth/engine architecture,
  deployment, environment, and conventions. **Start here.**
- `docs/superpowers/specs/` + `docs/superpowers/plans/` — work in flight.
- `docs/superpowers/archive/` — the durable "why" behind shipped decisions (see its README).
