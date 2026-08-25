# Env-supplied API Keys Follow the Interactive Trim Rule — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close #52. An env-supplied API key is trimmed before use and a whitespace-only value is
treated as absent — the same rule the interactive `/key` path already enforces — so selection never
sends a whitespace-padded credential and never reports a whitespace-only one as configured.

**Architecture:** One private normalization helper per crate, applied at each crate's single
env-entry point for keys. In `crates/providers/src/selection.rs`, `keys_from(read)` collects the
four `*_API_KEY` variables through an injectable reader and routes every raw value through
`normalize_env_key`; `selection_from_env` delegates its key loop to it. In `crates/tui/src/selection.rs`,
`classify` and `resolve_key_from` switch their presence filter from `!k.is_empty()` to
`!k.trim().is_empty()` and `resolve_key_from` returns the trimmed value. Downstream surfaces
(`fetch_model_list`, provider construction, the `/key` listing) change behavior without code edits.

**Tech Stack:** Rust (edition 2024, toolchain pinned by `rust-toolchain.toml`); std only — no new
dependency, no new crate, no manifest change. Tests reuse each module's existing injectable-env
stub pattern; nothing calls `std::env::set_var`.

**Spec:** `docs/superpowers/specs/2026-08-24-tui-env-key-trim-design.md` — read it first. This plan
implements it exactly, including §3 Assumptions (trim = `str::trim`; trimmed value replaces the raw
value downstream; internal whitespace preserved; classify keeps classifying by source) and §2 Scope
Out (model/base-url env vars are a filed follow-up, not this PR).

## Global Constraints

- **No AI attribution of any kind** — no `Co-Authored-By`, no "Generated with", no `🤖`, no AI credit
  in commit messages, code comments, docs, or the PR body. Comments added to these two modules must
  match the surrounding modules' density and "why, not what" style.
- **Semver: no bump.** Both helpers are private; no public signature, type, wire, or manifest change.
  The public functions' behavior moves toward what their doc comments already claim. Do not touch any
  `Cargo.toml` `version` (Non-Negotiable Rule 6 does not trigger).
- **Inward dependency flow untouched.** No crate gains or loses a dependency edge;
  `protocol`/`auth`/`persistence`/`server`/`web/` are not touched at all.
- **Secrets hygiene in tests:** keys are secrets. Test fixtures use obviously-fake values
  (`"sk-…"`, `"   "`); never log a key, never print a resolved value beyond a test assertion.
- **Tests stay offline-deterministic and process-env-free**: injectable readers/stubs only — the
  established convention in both modules (edition-2024 `set_var` is unsafe and racy).
- **No user-facing string changes**: no i18n keys, no rendering, no warnings text. If the implementer
  finds themselves editing `i18n.rs`, they have left scope.
- Run `cargo fmt --all` before every Rust commit (rustfmt is pinned in `rust-toolchain.toml`). Lint
  gate: `cargo clippy --workspace --all-targets -D warnings`. Test gate: `cargo test --workspace`.
- Tests live next to the code they cover (`#[cfg(test)] mod tests` in the same file).

## File Structure

| File | Responsibility |
|---|---|
| Modify. `crates/providers/src/selection.rs` | Private `normalize_env_key` + `keys_from(read)`; `selection_from_env`'s key loop delegates to them; `Selection.keys` field doc updated to "trimmed, non-empty"; new unit + wiring tests |
| Modify. `crates/tui/src/selection.rs` | `classify` and `resolve_key_from` apply the trim rule; rule comments updated; extended pure + wiring-level tests |
| Create. `docs/superpowers/specs/2026-08-24-tui-env-key-trim-design.md` | Committed design spec (already on this branch) |
| Modify. `docs/superpowers/plans/2026-08-24-tui-env-key-trim.md` | This file — checkboxes marked at close-out |

## Task Order & Rationale

Two tasks, providers-first.

**Task 1 (providers) first** because it is the inner crate and owns the env→`Selection.keys`
contract that Task 2's downstream narrative depends on; after it alone lands, provider construction
already treats a whitespace-only env key as absent, so a bisect between the commits holds a correct
(if incompletely classified) system.

**Task 2 (tui) second** because `classify`/`resolve_key_from` are presentation-and-resolution-layer
concerns (`/key` listing truthfulness, which key `fetch_model_list` sends) layered on top of the
same rule; landing them separately keeps each commit's diff single-crate and independently
revertable.

### Task 1: Trim env-supplied keys in `crates/providers`

**Files:** `crates/providers/src/selection.rs`

**Interfaces:**
- *Consumes:* `env_key_var(id)` (existing), `str::trim`. Nothing new.
- *Produces:* private `fn normalize_env_key(raw: String) -> Option<String>` and
  `fn keys_from(read: impl Fn(&str) -> Option<String>) -> HashMap<String, String>`.
  `pub fn selection_from_env()` keeps its exact signature; `Selection`'s shape is unchanged.
  **No public API change.**

- [ ] Write the failing tests in `crates/providers/src/selection.rs`'s `#[cfg(test)] mod tests`:
  - `normalize_env_key_trims_surrounding_whitespace`: `"  sk-1\n".to_string()` →
    `Some("sk-1".to_string())`.
  - `normalize_env_key_keeps_a_clean_value_verbatim`: `"sk-1"` in → identical `Some` out.
  - `normalize_env_key_treats_a_whitespace_only_value_as_absent`: `" \t\r\n"` → `None`.
  - `normalize_env_key_treats_an_empty_value_as_absent`: `String::new()` → `None`.
  - `normalize_env_key_preserves_internal_whitespace`: `"abc def "` → `Some("abc def")`.
  - `keys_from_reads_only_the_declared_vars`: stub returns `Some(format!("k-{var}"))` for every
    var; assert all four ids map to `k-<VAR>` (proves `env_key_var` naming flows through) and,
    via a counting stub over an id like `"ollama"`, that undeclared ids are never consulted —
    mirror the spirit of the tui's
    `resolve_key_never_reads_the_env_for_a_provider_with_no_declared_var`.
  - `keys_from_treats_a_whitespace_only_env_value_as_absent`: stub answers
    `"OPENAI_API_KEY" → Some("   \n".to_string())`, everything else `None`; assert the map is empty.
  - `keys_from_trims_a_trailing_newline_before_inserting`: stub answers
    `"ANTHROPIC_API_KEY" → Some("sk-a\n".to_string())`; assert
    `keys.get("anthropic") == Some(&"sk-a".to_string())` — the AC's trailing-newline case at the
    wiring level.
- [ ] Run `cargo test -p light-factory-providers` — expect **compile failure**
      (`normalize_env_key`/`keys_from` do not exist).
- [ ] Implement in `crates/providers/src/selection.rs` exactly as the spec's §1 shows both helpers
      (doc comments included), replace `selection_from_env`'s key loop body with
      `let mut keys = keys_from(|var| std::env::var(var).ok());`, and update the `Selection.keys`
      field doc from "Resolved, non-empty API keys by provider id" to
      "Resolved, trimmed non-empty API keys by provider id (env wins over keyring, decided by the
      caller)". Change nothing else in the function — models and base_urls loops stay as they are.
- [ ] Run `cargo test -p light-factory-providers` — all green, including the pre-existing suite.
- [ ] Verify the no-public-API claim rather than remembering it:
      `grep -n "normalize_env_key\|keys_from" crates/providers/src/selection.rs` must show both
      declared **without** `pub`, and no other file in the workspace referencing either name.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "providers: trim env-supplied api keys"`. No attribution trailer of any kind.

### Task 2: Apply the same rule to `classify` / `resolve_key_from` in the TUI

**Files:** `crates/tui/src/selection.rs`

**Interfaces:**
- *Consumes:* `str::trim`. Nothing new.
- *Produces:* changed private fn bodies only (`classify`, `resolve_key_from`) plus updated rule
  comments. All `pub fn` signatures (`key_status`, `resolve_key`, `apply_preferences`,
  `build_selection`, `rebuild`) unchanged. **No public API change.**

- [ ] Write the failing tests in `crates/tui/src/selection.rs`'s `#[cfg(test)] mod tests`:
  - Extend `classify_distinguishes_env_keyring_and_none` with three cases:
    `classify(Some(" \t\n".to_string()), Some("k".to_string())) == KeyStatus::Keyring`;
    `classify(Some(" \t\n".to_string()), None) == KeyStatus::None`;
    `classify(Some("  k  ".to_string()), None) == KeyStatus::Env` (padded-real stays `Env`).
  - Rename `resolve_key_from_treats_an_empty_env_value_as_absent` to
    `resolve_key_from_treats_a_blank_env_value_as_absent` and extend it: empty → keyring fallback
    and → `None`; `" \n\t"` → keyring fallback and → `None`; and a new assertion pair proving the
    trimmed value wins: `resolve_key_from(Some("sk-env\n".to_string()), Some("ring".to_string()))
    == Some("sk-env".to_string())` (spec review round 1: the old name understated the rule it now
    pins).
  - Extend the wiring-level `resolve_key_with_treats_an_empty_env_value_as_absent` (or add a sibling
    `resolve_key_with_trims_the_env_value`) : stub `|_| Some("  sk-env\n".to_string())` resolves
    through `sources_with` to `Some("sk-env")`; stub `|_| Some("   ".to_string())` falls back to the
    keyring value.
  - Extend `key_status_with_classifies_every_wiring_outcome`: add a whitespace-only blank stub
    (`Some(" \t\n".to_string())`) asserting `Keyring` against the populated store, and a
    trailing-newline real-value stub (`Some("sk-env\n".to_string())`) asserting `Env` against the
    empty store — the AC's two named cases through the public-shape wiring.
- [ ] Run `cargo test -p light-factory-tui selection` — expect the new assertions to **fail**
      (both fns still filter on bare `is_empty()`); the untouched pre-existing tests stay green.
- [ ] Implement in `crates/tui/src/selection.rs` exactly as the spec's §2 shows:
  - `classify`'s guard becomes `env_key.as_ref().is_some_and(|k| !k.trim().is_empty())`.
  - `resolve_key_from` becomes the trim→filter→`or(keyring_key)` chain; update its doc comment from
    "an empty env value is treated as absent (mirrors `classify`'s empty-string rule)" to the
    whitespace rule, keeping the mirror statement accurate.
- [ ] Run `cargo test -p light-factory-tui` — all green, including `modal`/`app` suites (they must be
      unaffected: no signature changed).
- [ ] Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -D warnings` — both
      clean. (The persistence integration test skips without `DATABASE_URL`; documented
      pre-existing behavior, not a regression.)
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "tui: treat a whitespace-only env api key as absent"`. No attribution trailer.

## Out-of-Band Surfaces (Phase 5 verification)

**None touched.** Two Rust source files plus tests and two docs files. No `Dockerfile`, `fly.toml`,
`web/`, `crates/persistence/migrations/`, or `.github/` change. Phase 5 step 14 is vacuously
satisfied — state it explicitly rather than skipping it.

## Close-Out Obligations

- [ ] **File the follow-up issue** for the same untrimmed pattern on non-key env inputs
      (`LIGHT_<P>_MODEL`, `LIGHT_OLLAMA_MODEL`, `*_BASE_URL`) and reference its number from this
      plan's follow-ups note and the PR body (spec review round 1 asked for the auditable trail).
- [ ] Update the spec's `> **Status:**` to IMPLEMENTED and mark this plan's checkboxes, then
      `git mv` both into `docs/superpowers/archive/{specs,plans}/`, add a row to
      `docs/superpowers/archive/README.md`, and commit as
      `docs: record the env-key trim rule as shipped`.
