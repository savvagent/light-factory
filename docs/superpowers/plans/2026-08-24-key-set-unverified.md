# `/key` Stores Without Verifying — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `/key <provider>` stops reporting a keyring write as a working credential. The status set
the instant the key is stored says *stored, not yet verified*; an off-loop probe of the key just
written then replaces it with one of three sharper sentences — the provider accepted it, the
provider rejected it (and it is still stored), or the provider could not be reached to check. No
probe outcome ever removes the key from the store, and no status ever carries the key or the
provider's own error text.

**Architecture:** Two `App` fields (`key_probe_nonce: u64`, `key_probe: Option<JoinHandle<()>>`)
mirroring `ModalHost`'s nonce+handle pair without reaching into it — `/key` is `Mode::Key`, not a
modal, so `ModalHost`'s nonce (which exists to discard results that outlive their *modal*) cannot be
borrowed for it. `submit_key_entry`'s `Ok` arm sets the floor status and calls a new
`begin_key_probe`, which claims a generation via `next_key_probe_nonce` (bump + abort-previous, one
inseparable operation) and spawns `crate::modal::fetch_model_list` — **called, never modified** —
with the typed key as `key_override`. The result returns as a new `UiEvent::KeyProbed` variant and is
mapped to a status by `handle_key_probed` using the existing `FetchFailure::needs_credentials()`
predicate. Everything is private to `crates/tui`'s binary modules.

**Tech Stack:** Rust (edition 2024, toolchain pinned in `rust-toolchain.toml`); existing `tokio`
(1.52, workspace `features = ["full"]`) for the spawn; existing `crate::modal::fetch_model_list` and
`FetchError`/`FetchFailure` from #55. **No new dependency, runtime or dev.**

**Spec:** `docs/superpowers/specs/2026-08-24-key-set-unverified-design.md` — read it first. This plan
implements it exactly.

## Global Constraints

- **Scope boundary (PR #68 is in flight over `crates/tui/src/app.rs`).** Confine every edit to the
  `/key` entry path plus the i18n catalogs. Do **not** touch `enter_models`,
  `handle_models_fetched`, `handle_connect_models`, `begin_model_fetch`, `fetch_error_message`,
  `credentials_remedy`, `build_provider_rows`, `key_status_label`, `enter_engine`, the `/connect`
  modal's own `KeyEntry` write path in `handle_modal_key`, or anything in `selection.rs` /
  `provider.rs`. Do **not** change `FetchFailure`, `FetchError`, `ModelsStep`, `ConnectStep`,
  `ProviderRow`, `ProviderInfo`, or anything else in `modal.rs` — `fetch_model_list` is *called*,
  not edited.
- **Secrets never reach a status, an error, a log, or `Debug` output.** The submitted key moves from
  `key_input` into `store.set` and into the probe task and nowhere else. Every `/key` status
  interpolates `{provider}` and nothing else — in particular the probe's `FetchError::message` is
  **not** rendered by this path.
- **A probe failure never un-stores the key.** No code added by this plan calls `store.delete`.
- Inward dependency flow: all changes stay in `crates/tui` (a client leaf). `protocol`, `auth`,
  `persistence`, `server`, `providers` and `web/` are untouched; `cargo build/test --workspace` must
  never require node.
- **Both i18n catalogs in the same commit.** Every new string goes in the `EN` *and* `ES` tables in
  `crates/tui/src/i18n.rs`; `es_mirrors_en_exactly` enforces parity. None of the new keys is a
  `*.footer`, so `every_footer_fits_the_popup_in_both_locales`' 58-column cap does not apply, but it
  must stay green.
- **No test may reach the network and no test may depend on the ambient environment.** Verify with
  both `cargo test -p light-factory-tui` and `OPENAI_API_KEY=sk-test cargo test -p light-factory-tui`
  — identical results required.
- Semver: `UiEvent` is `pub` but lives in `crates/tui/src/app.rs`, a **binary**-crate module;
  `crates/tui/src/lib.rs` exposes only `credentials`, `engine_view`, `i18n`. The new variant reaches
  no public API and is additive regardless ⇒ **no `Cargo.toml` version bump** (Non-Negotiable
  Rule 6).
- No comments unless the surrounding code already carries them in that style — `app.rs` documents
  *why*, at length, on every non-obvious branch; match that. **No AI / Co-Authored-By / "Generated
  with" attribution** in commits, PR bodies, code comments, or docs.
- Run `cargo fmt --all` before every Rust commit. Lint with
  `cargo clippy --workspace --all-targets -- -D warnings`.
- Tests live next to the code in `crates/tui/src/app.rs`'s `#[cfg(test)] mod tests`, using the
  existing `test_app()` / `test_app_with_store()` helpers, the `MemStore` double, and the existing
  `FailingStore` double (`app.rs:2037`).

## File Structure

| File | Responsibility |
|---|---|
| **Modify.** `crates/tui/src/i18n.rs` | Remove `status.key_set` from both catalogs; add `status.key_stored_unverified`, `status.key_verified`, `status.key_rejected`, `status.key_unverified` to both. |
| **Modify.** `crates/tui/src/app.rs` | `submit_key_entry`'s `Ok` arm; new `key_probe_nonce` / `key_probe` fields (+ their `App::new` initialisers); new `UiEvent::KeyProbed` variant and its event-loop arm; new `next_key_probe_nonce`, `begin_key_probe`, `handle_key_probed`; new tests. |
| **Untouched.** `crates/tui/src/modal.rs` | `fetch_model_list`, `FetchError`, `FetchFailure` are consumed as-is. |
| **Untouched.** `crates/tui/src/selection.rs`, `provider.rs` | `takes_key` already gates `begin_key_entry`; nothing changes. |
| **Untouched.** `Cargo.toml` (all) | No dependency and no version change. |

## Task Order & Rationale

Task 1 lands the acceptance floor on its own: the status stops reading as verification the moment
the key is stored, with no async surface at all. That is deliberate and mirrors the spec's central
argument (§5) — the floor must not be contingent on a network round-trip, so it is committed as a
standing, independently-correct change before the probe exists. If Task 2 were reverted tomorrow the
bug from #61 would stay fixed.

Task 2 adds the probe on top, and only ever *replaces* the floor status with a sharper one. It comes
second because its tests assert against the floor status as the starting state.

---

### Task 1: The status stops claiming verification

**Files:** `crates/tui/src/i18n.rs`, `crates/tui/src/app.rs`

**Interfaces:** consumes nothing new; produces the `status.key_stored_unverified` catalog key and
retires `status.key_set`.

- [ ] Confirm the premise before editing: `grep -rn "status.key_set" crates/` must show exactly
      three hits — the EN catalog, the ES catalog, and `submit_key_entry`. If it shows more, stop
      and report; the removal below is only safe because `submit_key_entry` is the sole consumer.
- [ ] Write the failing test in `crates/tui/src/app.rs`'s `mod tests`:
      `submitting_a_key_reports_it_as_unverified`. A `#[tokio::test]` (Task 2 will make
      `submit_key_entry` spawn; writing the test as `#[tokio::test]` now means Task 2 does not have
      to rewrite it). Build `test_app()`, set `app.key_target = Some("openai".to_string())` and
      `app.key_input = "sk-test-key".to_string()`, call `app.submit_key_entry()`, then assert
      `app.status == "API key stored for openai \u{2014} not yet verified"` and
      `app.store.get("openai").unwrap().as_deref() == Some("sk-test-key")`.
      Add a doc comment on the test recording *why* it is env-independent: nothing on this path calls
      `resolve_key`, so an exported `OPENAI_API_KEY` cannot change the assertion.
- [ ] Run `cargo test -p light-factory-tui submitting_a_key_reports_it_as_unverified` — **expect
      failure**: the status is still `"API key saved for openai"`.
- [ ] In `crates/tui/src/i18n.rs`, delete the `("status.key_set", …)` entry from the **EN** table
      (currently `i18n.rs:207`) and from the **ES** table (currently `i18n.rs:516`).
- [ ] In the same two tables, in the same positions, add
      `("status.key_stored_unverified", "API key stored for {provider} \u{2014} not yet verified")`
      (EN) and
      `("status.key_stored_unverified", "Clave de API guardada para {provider} \u{2014} a\u{fa}n sin verificar")`
      (ES). Write the em dash as `\u{2014}`, matching the existing `status.model_set_unverified`
      entries; write the ES text with a literal `ú` if the file's other ES strings use literal
      accented characters (check `status.key_enter`'s ES entry and match it).
- [ ] In `crates/tui/src/app.rs`'s `submit_key_entry`, change the `Ok(())` arm's status line from
      `self.t_with("status.key_set", …)` to `self.t_with("status.key_stored_unverified", …)`.
      Leave `rebuild_provider()`, the `Err` arm, the empty-input arm, and everything above the
      `match` untouched.
- [ ] Run `cargo test -p light-factory-tui submitting_a_key_reports_it_as_unverified` — **expect
      pass**.
- [ ] Run `cargo test -p light-factory-tui` — **expect pass**, including `es_mirrors_en_exactly` and
      `every_footer_fits_the_popup_in_both_locales`.
- [ ] Run `OPENAI_API_KEY=sk-test cargo test -p light-factory-tui` — **expect the same pass**.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "tui: report a stored key as unverified rather than saved"`.

---

### Task 2: Probe the key off the UI loop and report the outcome

**Files:** `crates/tui/src/i18n.rs`, `crates/tui/src/app.rs`

**Interfaces:** consumes `crate::modal::fetch_model_list`, `FetchError`, and
`FetchFailure::needs_credentials()` (all unmodified); produces `UiEvent::KeyProbed`, the `App` fields
`key_probe_nonce` / `key_probe`, and three catalog keys.

- [ ] Write the failing tests in `crates/tui/src/app.rs`'s `mod tests`. All of them must be
      network-free; add a module-level doc comment on the group recording the two mechanisms:
      (a) `submit_key_entry` is exercised inside a `#[tokio::test]` whose body **never awaits**, so
      the current-thread runtime never polls the spawned probe and no request is issued — the
      runtime drops the task unpolled at the end of the test; (b) `handle_key_probed` is called
      directly with synthetic `FetchError` values and touches no runtime at all. Neither path calls
      `resolve_key`, so an exported `OPENAI_API_KEY` cannot change any assertion.
  - `submitting_a_key_starts_a_probe` — `#[tokio::test]`, no await. After
    `app.submit_key_entry()` with `key_target = Some("openai")`: `app.key_probe_nonce != 0` and
    `app.key_probe.is_some()`.
  - `a_failed_keyring_write_starts_no_probe` — `#[tokio::test]`, no await,
    `test_app_with_store(Arc::new(FailingStore))`. After submitting: `app.key_probe.is_none()`,
    `app.key_probe_nonce == 0`, and `app.error.is_some()`.
  - `an_accepted_key_reports_verification` — plain `#[test]`. Set `app.key_probe_nonce = 7`, call
    `app.handle_key_probed(7, "openai".to_string(), Ok(()))`, assert
    `app.status == "openai accepted the API key"`.
  - `a_rejected_key_reports_rejection_and_stays_stored` — plain `#[test]`. Store a key in the
    `MemStore` first, then `handle_key_probed(7, "openai", Err(FetchError { class:
    FetchFailure::Auth, message: "…".to_string() }))`; assert the status is
    `"openai rejected the API key \u{2014} it is still stored"` **and**
    `app.store.get("openai").unwrap().is_some()`.
  - `a_missing_key_class_reports_rejection` — same shape with `FetchFailure::MissingKey`, pinning
    that both credential classes share the arm via `needs_credentials()`.
  - `an_unreachable_provider_does_not_accuse_the_key` — `FetchFailure::Fetch` ⇒
    `"Couldn't reach openai to verify the API key \u{2014} it is stored"`, distinct from both other
    error statuses, and the key still stored.
  - `a_stale_probe_result_is_discarded` — set `app.key_probe_nonce = 7` and `app.status` to a
    sentinel, call `handle_key_probed(6, …, Ok(()))`, assert the status is unchanged.
  - `claiming_a_probe_nonce_aborts_the_previous_probe` — `#[tokio::test]`. Reuse the existing
    `pending_task()` helper (`app.rs:2675`) and `settle()` (`app.rs:2681`): park the pending handle
    in `app.key_probe`, call `app.next_key_probe_nonce()` **directly** (not `submit_key_entry` — that
    would spawn a real network probe that `settle()`'s yields would then poll), `settle(&probe).await`,
    and assert `probe.is_finished()` and that the returned nonce is greater than the one before.
- [ ] Run `cargo test -p light-factory-tui key_prob probe` — **expect compile failure** (the fields,
      the variant, and the three methods do not exist). Compile failure is the failing state for
      this task.
- [ ] Add the three EN/ES catalog entries to `crates/tui/src/i18n.rs`, both tables, adjacent to
      `status.key_stored_unverified`:
      `("status.key_verified", "{provider} accepted the API key")` /
      `("status.key_verified", "{provider} acept\u{f3} la clave de API")`;
      `("status.key_rejected", "{provider} rejected the API key \u{2014} it is still stored")` /
      `("status.key_rejected", "{provider} rechaz\u{f3} la clave de API \u{2014} sigue guardada")`;
      `("status.key_unverified", "Couldn't reach {provider} to verify the API key \u{2014} it is stored")` /
      `("status.key_unverified", "No se pudo contactar con {provider} para verificar la clave de API \u{2014} est\u{e1} guardada")`.
      Match the file's existing convention for accented ES characters (literal vs escape) — check the
      neighbouring ES entries and follow them.
- [ ] In `crates/tui/src/app.rs`, add the `KeyProbed` variant to `pub enum UiEvent`, after
      `ModelsFetched`:
      ```rust
      KeyProbed {
          nonce: u64,
          provider: String,
          result: Result<(), FetchError>,
      },
      ```
- [ ] Add the two fields to `pub struct App`, next to the existing `key_target` / `key_input` /
      `key_return` block, each with a doc comment giving the reason (stale-result discard; one key in
      flight at a time), and initialise them in `App::new` (`key_probe_nonce: 0`,
      `key_probe: None`).
- [ ] Add `next_key_probe_nonce`, `begin_key_probe`, and `handle_key_probed` to the `impl App` block
      immediately after `submit_key_entry`, exactly as specified in the spec's §6.4 and §6.5. Key
      points the reviewer will check: `next_key_probe_nonce` bumps **and** aborts the previous handle
      as one operation, so no call site can forget the cancellation; `begin_key_probe` passes
      `Some(key)` (never `None` — `resolve_key` would prefer an exported env var and verify a
      different credential); the result is `.map(|_| ())` so the model list is discarded at the
      boundary; `handle_key_probed` returns early on a stale nonce, clears `key_probe`, and selects
      the status key by `Ok` / `needs_credentials()` / else, interpolating `{provider}` only.
- [ ] Call `self.begin_key_probe(provider, key)` as the last statement of `submit_key_entry`'s
      `Ok(())` arm, after the floor status is set. `key` must still be in scope — do not move it into
      `store.set` (it is passed by reference there).
- [ ] Add the event-loop arm in `run` next to `UiEvent::ModelsFetched`:
      ```rust
      UiEvent::KeyProbed {
          nonce,
          provider,
          result,
      } => app.handle_key_probed(nonce, provider, result),
      ```
- [ ] Run `cargo test -p light-factory-tui` — **expect pass**, all new tests included, plus
      `es_mirrors_en_exactly`.
- [ ] Run `OPENAI_API_KEY=sk-test cargo test -p light-factory-tui` — **expect the same pass**. This
      is the env-independence gate; if any new test's result differs, the test is reading the ambient
      environment and must be fixed, not accommodated.
- [ ] Run `cargo test --workspace` — **expect pass** (the `crates/persistence` PostgreSQL
      integration test skips without `DATABASE_URL`; that is the known, pre-existing behaviour).
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings` — **expect clean**. Watch for
      `private_interfaces` on `FetchError` inside the new `pub` `UiEvent` variant: `FetchError` is
      already `pub(crate)` and already appears in `UiEvent::ModelsFetched` for exactly this reason,
      so no visibility change is needed — if clippy complains, the fix is in the new code, not in
      `modal.rs`.
- [ ] Format and commit: `cargo fmt --all` then
      `git commit -m "tui: probe a newly stored key and report whether the provider accepted it"`.

---

## Out-of-band surfaces (Phase 5)

None. This change touches no `Dockerfile`, no `fly.toml`, no `web/`, no
`crates/persistence/migrations/`, and no `.github/`. State that explicitly at verification rather
than skipping the step. Target verification is `cargo test --workspace` + clippy + `cargo fmt --all
--check` on the merge commit, plus one manual `cargo run -p light-factory-tui` exercising
`/key openai` to see the two statuses land in sequence.
