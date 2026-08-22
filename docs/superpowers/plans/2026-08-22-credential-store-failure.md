# Credential Store Failure vs. Absence — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep "the credential store could not be read" distinct from "no key is stored" at every
TUI call site, so a locked keyring stops being reported as "No API key for openai".

**Architecture:** `CredentialStore::get` already returns `anyhow::Result<Option<String>>` — three
states. `crates/tui/src/selection.rs` collapses them to two with `.ok().flatten()`. Replace the
`sources_with`/`classify`/`resolve_key_from` trio with two shared primitives (`env_key`,
`read_store`), widen `KeyStatus` with `Unavailable`, and give `resolve_key` a three-state
`KeyResolution` return. The new state then propagates to the four consumers: the `/models` fetch
(a new `FetchFailure::StoreUnavailable` class with its own remedy), the `/key` listing label, the
`/connect` provider rows (`ProviderRow.connected: bool` becomes a tri-state `RowKey`), and the
engine-pane offline notice (`ProviderInfo` gains `store_failures`).

**Tech Stack:** Rust edition 2024, toolchain pinned by `rust-toolchain.toml`; `anyhow`, `ratatui`,
`crossterm`, the existing `CredentialStore`/`MemStore` seam and the `light_factory_providers`
selection layer.

**Spec:** `docs/superpowers/specs/2026-08-22-credential-store-failure-design.md` — read it first.
This plan implements it exactly.

**Source:** GitHub issue savvagent/light-factory#51.

## Global Constraints

- **No AI/self-attribution anywhere** — no `Co-Authored-By`, no "Generated with", no `🤖`, in
  commits, PR bodies, code comments, or docs.
- **`cargo fmt --all` before every Rust commit.** rustfmt is pinned in `rust-toolchain.toml`.
- **Every task ends green:** `cargo test -p light-factory-tui` and
  `cargo clippy --workspace --all-targets -- -D warnings` both clean before the commit.
- **Tests live next to the code** in `#[cfg(test)] mod tests` at the bottom of the file. There is
  no `tests/` directory in `crates/tui`.
- **Every new user-facing string is added to BOTH catalogs** in `crates/tui/src/i18n.rs` (`EN` and
  `ES`). `i18n::tests::es_mirrors_en_exactly` fails otherwise. Add the EN entry and the ES entry in
  the same step.
- **Secrets never reach logs or `Debug` output.** `KeyResolution::Found` holds a live API key; it
  gets a hand-written redacting `Debug` (Task 3).
- **Dependency flow is inward.** Nothing in this change touches `crates/providers`; the credential
  store is a TUI concept, so `OfflineReason` gains no variant.
- **No `Cargo.toml` version bump.** `crates/tui/src/lib.rs` exposes only `credentials`,
  `engine_view`, and `i18n`; `selection.rs`, `modal.rs`, `app.rs`, `provider.rs`, and the new
  `text.rs` are binary-crate-internal. The library-surface changes are the additive `FailingStore`
  and the new `i18n` catalog entries — both additive, both semver-minor.
- **No out-of-band surfaces are touched.** No `Dockerfile`/`fly.toml`, no `web/`, no
  `crates/persistence/migrations/`.

## File Structure

| File | Responsibility |
|---|---|
| Create. `crates/tui/src/text.rs` | `one_line` / `truncate_chars` text-hygiene helpers shared by the resolution seam and the modal |
| Modify. `crates/tui/src/main.rs` | `mod text;` declaration |
| Modify. `crates/tui/src/credentials.rs` | `FailingStore` test double whose every operation returns `Err` |
| Modify. `crates/tui/src/selection.rs` | `env_key` / `read_store` primitives, `KeyStatus::Unavailable`, `KeyResolution`, store failures out of `build_selection` |
| Modify. `crates/tui/src/modal.rs` | `FetchFailure::StoreUnavailable`, `ModelsStep::Credentials { remedy }`, `ProviderRow`/`RowKey`, row rendering and Enter routing, `fetch_model_list_inner` |
| Modify. `crates/tui/src/app.rs` | `fetch_error_message` arm, `credentials_remedy`, `key_status_label` arm, `build_provider_rows`, `enter_engine` notice assembly |
| Modify. `crates/tui/src/provider.rs` | `StoreFailure`, `ProviderInfo.store_failures`, `ProviderInfo::notices` |
| Modify. `crates/tui/src/i18n.rs` | Six new EN + ES strings and a popup-width test for the new row suffix |
| Create. `docs/superpowers/plans/2026-08-22-credential-store-failure.md` | This plan |

## Task Order & Rationale

1. **Text helpers first** (Task 1) because both the resolution seam and the modal need them, and
   extracting them from `summarize_provider_error` is a behaviour-preserving refactor that is
   easiest to verify while nothing else has moved.
2. **The test double next** (Task 2) because every later task's failing test needs a store that
   returns `Err`, and it cannot be written before the double exists.
3. **The `/models` failure class** (Task 3) before anything produces it, so the class, its message,
   and its remedy can be reviewed on their own against a hand-built `FetchError`.
4. **The resolution seam** (Task 4) then makes a real unreadable store produce that class, and
   fixes the `/key` label at the same time — both are `key_status`/`resolve_key` consumers.
5. **The `/connect` rows** (Task 5) is the last `key_status` consumer and the only one that needs a
   type change (`bool` → `RowKey`), so it lands after the seam it reads from is settled.
6. **The offline notice** (Task 6) is the startup path (`build_selection`/`rebuild`), independent
   of the modal work, and ends the change with the third acceptance criterion.

---

### Task 1: Extract the text-hygiene helpers into `crates/tui/src/text.rs`

**Files:**
- Create: `crates/tui/src/text.rs`
- Modify: `crates/tui/src/main.rs` (add `mod text;` to the module list at lines 3-12)
- Modify: `crates/tui/src/modal.rs` (`summarize_provider_error`, lines 608-622)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub(crate) fn one_line(s: &str) -> String` and
  `pub(crate) fn truncate_chars(s: &str, max: usize) -> String` in `crate::text`.

- [ ] **Step 1: Write the failing tests**

Create `crates/tui/src/text.rs` containing only the test module for now:

```rust
//! Text-hygiene helpers for the code paths that render foreign error text.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_keeps_only_the_first_line() {
        assert_eq!(one_line("first\nsecond\nthird"), "first");
        assert_eq!(one_line("only"), "only");
        assert_eq!(one_line(""), "");
    }

    /// Error text is written into a terminal cell verbatim; a raw `ESC` in a cell is an
    /// escape-sequence injection, and a tab or a carriage return corrupts the row.
    #[test]
    fn one_line_strips_control_characters() {
        assert_eq!(one_line("a\u{1b}[31mb\tc"), "a[31mbc");
        assert_eq!(one_line("a\rb"), "ab");
    }

    /// `str::lines` treats `\r\n` as one terminator, so a message that opens with a blank line
    /// yields an empty first line. That is the pre-existing `summarize_provider_error` behaviour
    /// and this refactor must preserve it — changing it would be a behaviour change wearing a
    /// refactor's clothes.
    #[test]
    fn one_line_does_not_skip_a_leading_blank_line() {
        assert_eq!(one_line("\r\nafter"), "");
        assert_eq!(one_line("\nafter"), "");
    }

    #[test]
    fn one_line_trims_the_ends() {
        assert_eq!(one_line("   padded   \nnext"), "padded");
    }

    #[test]
    fn truncate_chars_leaves_a_short_string_alone() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("exactly10!", 10), "exactly10!");
    }

    #[test]
    fn truncate_chars_appends_an_ellipsis_when_it_cuts() {
        assert_eq!(truncate_chars("abcdef", 3), "abc\u{2026}");
    }

    /// Counting characters rather than bytes is what keeps a multi-byte message from panicking
    /// on a split boundary — `&s[..max]` would.
    #[test]
    fn truncate_chars_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("ñññññ", 2), "ññ\u{2026}");
        assert_eq!(truncate_chars("ñññ", 3), "ñññ");
    }
}
```

Add `mod text;` to `crates/tui/src/main.rs`, keeping the list alphabetical (between `mod
settings;` and `mod ws;`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-tui text::`
Expected: FAIL to compile — `cannot find function 'one_line' in this scope` (and the same for
`truncate_chars`).

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/tui/src/text.rs`:

```rust
/// The first line of `s`, with control characters removed and the ends trimmed.
///
/// Error text from a provider or from the OS credential store reaches a terminal cell verbatim.
/// A raw `ESC` in a cell is an escape-sequence injection, and a newline turns a one-row field
/// into an unbounded block that pushes the modal's own trusted rows off the screen.
pub(crate) fn one_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// `s` capped at `max` characters, with a trailing ellipsis when it was cut.
///
/// Characters, not bytes: slicing a multi-byte message at a byte offset panics.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}\u{2026}")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p light-factory-tui text::`
Expected: PASS, 7 tests.

- [ ] **Step 5: Reduce `summarize_provider_error` to a composition of the two helpers**

In `crates/tui/src/modal.rs`, replace the body of `summarize_provider_error` (lines 608-622) with:

```rust
fn summarize_provider_error(message: &str) -> String {
    crate::text::truncate_chars(&crate::text::one_line(message), PROVIDER_ERROR_MAX_CHARS)
}
```

Leave the function's doc comment exactly as it is — it explains *why* the cap exists, which is
still true — and append one sentence: `The two rules it composes live in [`crate::text`] so the
credential-store path can share them.`

- [ ] **Step 6: Run the existing modal tests to verify the refactor changed nothing**

Run: `cargo test -p light-factory-tui modal::`
Expected: PASS — in particular `a_provider_error_is_reduced_to_one_bounded_line` and every other
pre-existing modal test, unchanged.

- [ ] **Step 7: Run the whole crate, clippy, and commit**

```bash
cargo test -p light-factory-tui
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/tui/src/text.rs crates/tui/src/main.rs crates/tui/src/modal.rs
git commit -m "tui: extract the one-line and truncate text helpers into their own module"
```

Expected: all green; clippy clean.

---

### Task 2: Add the `FailingStore` test double

**Files:**
- Modify: `crates/tui/src/credentials.rs` (add after `MemStore`'s impl, before `mod tests`)

**Interfaces:**
- Consumes: the existing `CredentialStore` trait.
- Produces: `light_factory_tui::credentials::FailingStore`, with
  `FailingStore::new(message: impl Into<String>) -> Self` and
  `impl Default for FailingStore` (message `"credential store unavailable"`). All three trait
  methods return `Err`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/tui/src/credentials.rs`'s `mod tests`:

```rust
    /// `MemStore` always answers `Ok`, so no test could reach the `Err` branch that
    /// `CredentialStore::get`'s contract makes load-bearing. This double is that branch.
    #[test]
    fn failing_store_fails_every_operation() {
        let store = FailingStore::default();
        assert!(store.get("openai").is_err());
        assert!(store.set("openai", "sk-test").is_err());
        assert!(store.delete("openai").is_err());
    }

    /// The store's own words are what the user ends up reading, so a test must be able to pin
    /// them.
    #[test]
    fn failing_store_reports_the_message_it_was_given() {
        let store = FailingStore::new("no D-Bus session");
        let err = store.get("openai").expect_err("get must fail");
        assert_eq!(format!("{err:#}"), "no D-Bus session");
    }

    #[test]
    fn failing_store_default_names_the_store() {
        let err = FailingStore::default()
            .get("openai")
            .expect_err("get must fail");
        assert_eq!(format!("{err:#}"), "credential store unavailable");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-tui credentials::`
Expected: FAIL to compile — `cannot find type 'FailingStore' in this scope`.

- [ ] **Step 3: Write the implementation**

Insert into `crates/tui/src/credentials.rs`, after `impl CredentialStore for MemStore` and before
`#[cfg(test)]`:

```rust
/// A store whose every operation fails, for the store-failure branch [`MemStore`] cannot reach.
///
/// It models a keyring that is present but unusable — a locked wallet, a dead D-Bus session —
/// which fails for every operation rather than for one, so `set` and `delete` fail too.
///
/// `#[doc(hidden)] pub` for the same reason as [`MemStore`]: `selection.rs`, `modal.rs`, and
/// `app.rs` live in the binary crate and can only reach a double this library exports.
#[doc(hidden)]
pub struct FailingStore {
    message: String,
}

impl FailingStore {
    /// A store that fails with `message`, so a test can assert that the store's own words reach
    /// the user.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Default for FailingStore {
    fn default() -> Self {
        Self::new("credential store unavailable")
    }
}

impl CredentialStore for FailingStore {
    fn get(&self, _provider: &str) -> anyhow::Result<Option<String>> {
        Err(anyhow::anyhow!("{}", self.message))
    }

    fn set(&self, _provider: &str, _key: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("{}", self.message))
    }

    fn delete(&self, _provider: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("{}", self.message))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p light-factory-tui credentials::`
Expected: PASS — the three new tests plus the three pre-existing `MemStore` tests.

- [ ] **Step 5: Run the whole crate, clippy, and commit**

```bash
cargo test -p light-factory-tui
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/tui/src/credentials.rs
git commit -m "tui: add a credential store double whose operations fail"
```

Expected: all green; clippy clean.

---

### Task 3: Add the `StoreUnavailable` fetch class and its own remedy

**Files:**
- Modify: `crates/tui/src/modal.rs` (`FetchFailure` at lines 143-149, `needs_credentials` at
  152-157, `ModelsStep::Credentials` at 130-133, the `Credentials` render arm at 1135-1157, and
  the `Credentials` constructions in `mod tests`)
- Modify: `crates/tui/src/app.rs` (`fetch_error_message` at lines 864-873, `handle_models_fetched`'s
  `Credentials` construction at 845-851)
- Modify: `crates/tui/src/i18n.rs` (`connect.store_unavailable`, `models.store_remedy`, EN + ES)

**Interfaces:**
- Consumes: `crate::text` (Task 1) indirectly, via `summarize_provider_error`.
- Produces: `FetchFailure::StoreUnavailable`; `ModelsStep::Credentials { provider, error, remedy }`;
  `App::credentials_remedy(&self, provider: &str, class: FetchFailure) -> String`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/tui/src/modal.rs`'s `mod tests`:

```rust
    /// A store failure is a credential-class failure: no model id repairs a credential store, so
    /// the modal must show the remedy step rather than the manual-entry step.
    #[test]
    fn a_store_failure_needs_credentials() {
        assert!(FetchFailure::StoreUnavailable.needs_credentials());
        assert!(FetchFailure::MissingKey.needs_credentials());
        assert!(FetchFailure::Auth.needs_credentials());
        assert!(!FetchFailure::Fetch.needs_credentials());
    }
```

Add to `crates/tui/src/app.rs`'s `mod tests`:

```rust
    /// `/connect` and `/key` both write to the credential store, so neither is a remedy for a
    /// store that cannot be read. The remedy line must differ from the one the other credential
    /// classes get.
    #[test]
    fn a_store_failure_gets_its_own_remedy() {
        let app = test_app();
        let store = app.credentials_remedy("openai", FetchFailure::StoreUnavailable);
        let missing = app.credentials_remedy("openai", FetchFailure::MissingKey);
        assert_ne!(store, missing);
        assert!(!store.is_empty(), "every class must produce a remedy");
        assert!(
            !store.contains("/key") && !store.contains("/connect"),
            "the store remedy must not point at commands that write to the broken store: {store}"
        );
        assert!(store.contains("openai"), "the remedy names the provider: {store}");
    }

    /// Every credential class must produce a non-empty remedy, so a future class cannot render an
    /// empty row.
    #[test]
    fn every_credential_class_has_a_remedy() {
        let app = test_app();
        for class in [
            FetchFailure::MissingKey,
            FetchFailure::Auth,
            FetchFailure::StoreUnavailable,
        ] {
            assert!(
                !app.credentials_remedy("openai", class).is_empty(),
                "{class:?} has no remedy"
            );
        }
    }

    /// `connect.store_unavailable` already names the provider and the cause, so wrapping it in
    /// `connect.fetch_error` would read "Couldn't fetch models: the credential store for openai
    /// could not be read: ...".
    #[test]
    fn a_store_failure_message_is_passed_through_unwrapped() {
        let app = test_app();
        let err = FetchError {
            class: FetchFailure::StoreUnavailable,
            message: "the credential store for openai could not be read: locked".to_string(),
        };
        assert_eq!(app.fetch_error_message("openai", &err), err.message);
    }

    /// The failure routes to the credentials step, carrying the remedy that step renders.
    #[test]
    fn handle_models_fetched_routes_a_store_failure_to_the_credentials_step() {
        let mut app = test_app();
        // `open` rather than `App::open_modal`: the latter spawns a fetch, and this is a sync
        // test with no tokio runtime. The helper exists at app.rs:2147 for exactly this.
        let nonce = open(&mut app, Modal::Models(models_list_step(vec![], true)));
        app.handle_models_fetched(
            nonce,
            "openai".to_string(),
            Err(fetch_err(
                FetchFailure::StoreUnavailable,
                "the credential store for openai could not be read: locked",
            )),
        );
        let Some(ModelsStep::Credentials { error, remedy, .. }) = models_step(&app) else {
            panic!(
                "a store failure must not offer a model-id box, got {:?}",
                models_step(&app)
            );
        };
        assert!(error.contains("could not be read"), "{error}");
        assert!(!remedy.contains("/key"), "{remedy}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-tui`
Expected: FAIL to compile — `no variant named 'StoreUnavailable' found for enum 'FetchFailure'`
and `no method named 'credentials_remedy'`. (Run the whole crate rather than a name filter: the
five new tests do not share a substring.)

- [ ] **Step 3: Add the variant and widen `needs_credentials`**

In `crates/tui/src/modal.rs`, add to `FetchFailure`:

```rust
    /// The credential store could not be read, so whether a key exists is unknown. Distinct from
    /// [`FetchFailure::MissingKey`]: the remedy for a missing key is to store one, which is not a
    /// remedy for a store that cannot be read.
    StoreUnavailable,
```

and widen `needs_credentials`:

```rust
    pub(crate) fn needs_credentials(self) -> bool {
        matches!(
            self,
            FetchFailure::MissingKey | FetchFailure::Auth | FetchFailure::StoreUnavailable
        )
    }
```

- [ ] **Step 4: Give `ModelsStep::Credentials` a `remedy` field**

In `crates/tui/src/modal.rs`, change the variant (lines 130-133) to:

```rust
    /// A credential-class fetch failure (no key resolved, the provider refused the one we sent,
    /// or the credential store could not be read). Typing a model id cannot repair a credential,
    /// so this step shows a remedy and takes no input.
    ///
    /// `remedy` is already localized and already class-specific: `/connect` and `/key` are the
    /// answer to a missing or rejected key, and are useless against a store that cannot be read,
    /// so the step carries the sentence rather than deriving it at render time. That keeps the
    /// render a pure function of the step, as every other arm is.
    Credentials {
        provider: String,
        error: String,
        remedy: String,
    },
```

Change the render arm (lines 1135-1157) so it uses the carried remedy in place of the
`models.credentials_remedy` lookup:

```rust
        ModelsStep::Credentials {
            error, remedy, ..
        } => {
            lines.push(Line::from(Span::styled(
                i18n::t(ctx.locale, "models.credentials_hint"),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                remedy.clone(),
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )));
            // A 401/403 is not always about the key — a corporate proxy, a WAF, or an IP
            // allowlist produces the same status — and a store failure can be a transient D-Bus
            // blip, so the step keeps a retry rather than dead-ending on a remedy.
            footer = i18n::t(ctx.locale, "models.footer_retry");
        }
```

The `provider` binding is no longer read in this arm; keep it out of the pattern with `..` as
shown. `models_step_next`'s `Credentials` arm (line 738) still binds `provider` and is unchanged.

Fix every `ModelsStep::Credentials` site the compiler now rejects. Do not change what any of these
tests assert:

- `modal.rs` `mod tests`, constructions at lines 1863, 1902, 2214, 2397, 2434 — add
  `remedy: "remedy".to_string(),`.
- `app.rs` `mod tests`, **destructuring patterns** at lines 3317 and 3339
  (`let Some(ModelsStep::Credentials { provider, error }) = models_step(&app)`) — add `..` so they
  read `ModelsStep::Credentials { provider, error, .. }`.
- `app.rs` `mod tests`, the **construction** at line 3360 inside
  `retry_re_triggers_the_fetch_from_the_credentials_step` — add
  `remedy: "remedy".to_string(),`.

- [ ] **Step 5: Add the EN and ES strings**

In `crates/tui/src/i18n.rs`, add to `EN` next to `connect.no_key` (line 282):

```rust
    (
        "connect.store_unavailable",
        "the credential store for {provider} could not be read: {error}",
    ),
```

and next to `models.credentials_remedy` (line 298):

```rust
    (
        "models.store_remedy",
        "Unlock the credential store and retry, or set {provider}'s API key in the environment",
    ),
```

Add to `ES` at the mirrored positions:

```rust
    (
        "connect.store_unavailable",
        "no se pudo leer el almac\u{e9}n de credenciales de {provider}: {error}",
    ),
```

```rust
    (
        "models.store_remedy",
        "Desbloquea el almac\u{e9}n de credenciales y reintenta, o define la clave de API de {provider} en el entorno",
    ),
```

Neither key contains `.footer`, so `every_footer_fits_the_popup_in_both_locales` does not gate
them; both are body rows, which `draw_popup` wraps.

- [ ] **Step 6: Add `credentials_remedy` and the `fetch_error_message` arm**

In `crates/tui/src/app.rs`, add next to `fetch_error_message`:

```rust
    /// The remedy line for a credential-class failure.
    ///
    /// Class-specific because `/connect` and `/key` both write to the credential store: they are
    /// the answer to a missing or rejected key and are useless against a store that cannot be
    /// read. Matching on every variant rather than on a wildcard is deliberate — a new class must
    /// make this decision rather than inherit it.
    fn credentials_remedy(&self, provider: &str, class: FetchFailure) -> String {
        match class {
            FetchFailure::StoreUnavailable => {
                self.t_with("models.store_remedy", &[("provider", provider)])
            }
            FetchFailure::MissingKey | FetchFailure::Auth | FetchFailure::Fetch => {
                self.t_with("models.credentials_remedy", &[("provider", provider)])
            }
        }
    }
```

Add the passthrough arm to `fetch_error_message`:

```rust
            // Already a complete sentence naming the provider and the cause, like `MissingKey`;
            // wrapping it would read "Couldn't fetch models: the credential store for openai
            // could not be read: ...".
            FetchFailure::StoreUnavailable => err.message.clone(),
```

and build the remedy in `handle_models_fetched`'s `Credentials` construction:

```rust
                    .replace_step(Modal::Models(if err.class.needs_credentials() {
                        let remedy = self.credentials_remedy(&provider, err.class);
                        ModelsStep::Credentials {
                            provider,
                            error: message,
                            remedy,
                        }
                    } else {
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p light-factory-tui`
Expected: PASS — the five new tests plus all pre-existing ones.

- [ ] **Step 8: Run clippy and commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/tui/src/modal.rs crates/tui/src/app.rs crates/tui/src/i18n.rs
git commit -m "tui: give an unreadable credential store its own fetch class and remedy"
```

Expected: all green; clippy clean.

---

### Task 4: Make the resolution seam three-state

**Files:**
- Modify: `crates/tui/src/selection.rs` (replace `classify` at 22-32, `sources_with` at 41-53,
  `key_status_with` at 63-71, `resolve_key_from` at 78-84, `resolve_key_with` at 88-97, and the
  public `key_status` / `resolve_key`; plus the module's `mod tests`)
- Modify: `crates/tui/src/modal.rs` (`fetch_model_list_inner` at lines 683-698)
- Modify: `crates/tui/src/app.rs` (`key_status_label` at 1006-1012, `build_provider_rows` at
  647-661)
- Modify: `crates/tui/src/i18n.rs` (`provider.key.unavailable`, EN + ES)

**Interfaces:**
- Consumes: `light_factory_tui::credentials::FailingStore` (Task 2), `crate::text::one_line`
  (Task 1), `FetchFailure::StoreUnavailable` (Task 3).
- Produces: `KeyStatus::Unavailable`; `pub enum KeyResolution { Found(String), Missing,
  Unavailable(String) }`; `pub fn resolve_key(provider: &str, store: &dyn CredentialStore) ->
  KeyResolution`; `pub fn key_status(provider: &str, store: &dyn CredentialStore) -> KeyStatus`
  (unchanged signature, new variant).

- [ ] **Step 1: Write the failing tests**

**Delete all nine pre-existing seam tests** from `crates/tui/src/selection.rs`'s `mod tests` — every
one of them names a function this task removes or a return type it changes, so leaving any behind is
either a duplicate definition or a type error:

| Line | Test to delete | Why |
|---|---|---|
| 172 | `classify_distinguishes_env_keyring_and_none` | `classify` is deleted |
| 190 | `resolve_key_from_prefers_env_over_keyring` | `resolve_key_from` is deleted |
| 203 | `resolve_key_from_treats_an_empty_env_value_as_absent` | `resolve_key_from` is deleted |
| 214 | `resolve_key_reads_a_stored_keyring_key` | compares `resolve_key_with` to `Option<String>` |
| 226 | `resolve_key_reads_the_env_var_the_provider_declares` | same |
| 239 | `resolve_key_never_reads_the_env_for_a_provider_with_no_declared_var` | same |
| 257 | `resolve_key_with_treats_an_empty_env_value_as_absent` | same; the new block redefines this name |
| 269 | `resolve_key_delegates_to_the_process_env_reader` | same; the new block redefines this name |
| 278 | `key_status_with_classifies_every_wiring_outcome` | the new block redefines this name |

**Keep** `settings` (the helper at 163), `non_remote_providers_have_no_key` (183), both
`apply_preferences_*` tests (293, 303), and all three `build_and_info_*` tests (313, 324, 336).

Add `use light_factory_tui::credentials::FailingStore;` to the test module's imports, then add the
following in place of the deleted block.

```rust
    #[test]
    fn env_key_treats_an_empty_value_as_absent() {
        assert_eq!(
            env_key("openai", |_| Some("sk-env".to_string())),
            Some("sk-env".to_string())
        );
        assert_eq!(env_key("openai", |_| Some(String::new())), None);
        assert_eq!(env_key("openai", |_| None), None);
    }

    /// `env_key_var` yields no name for a provider with no declared var, so the reader is never
    /// invoked at all — the counting stub proves it.
    #[test]
    fn env_key_never_reads_the_env_for_a_provider_with_no_declared_var() {
        let reads = std::cell::Cell::new(0u32);
        let counting = |_: &str| {
            reads.set(reads.get() + 1);
            Some("sk-env".to_string())
        };
        assert_eq!(env_key("ollama", counting), None);
        assert_eq!(reads.get(), 0);
    }

    /// The store's error text is what the user reads, so it must survive the seam, on one line.
    #[test]
    fn read_store_reduces_a_failure_to_one_line() {
        let store = FailingStore::new("locked wallet\nsecond line");
        assert_eq!(
            read_store("openai", &store),
            Err("locked wallet".to_string())
        );
    }

    /// All four wiring outcomes of `key_status`, including the one `MemStore` could never reach.
    #[test]
    fn key_status_with_classifies_every_wiring_outcome() {
        let empty = MemStore::new();
        let ring = MemStore::new();
        ring.set("openai", "sk-ring").unwrap();
        let broken = FailingStore::default();
        let set = |_: &str| Some("sk-env".to_string());
        let blank = |_: &str| Some(String::new());
        let unset = |_: &str| None;

        assert_eq!(key_status_with("openai", &empty, set), KeyStatus::Env);
        assert_eq!(key_status_with("openai", &ring, blank), KeyStatus::Keyring);
        assert_eq!(key_status_with("openai", &ring, unset), KeyStatus::Keyring);
        assert_eq!(key_status_with("openai", &empty, unset), KeyStatus::None);
        assert_eq!(
            key_status_with("openai", &broken, unset),
            KeyStatus::Unavailable,
            "a store that cannot answer is not the same as a store with no key"
        );
    }

    /// A working environment variable must not be reported as unavailable because the keyring is
    /// down — the store is not consulted at all once the env has answered.
    #[test]
    fn an_env_key_wins_over_a_broken_store() {
        let broken = FailingStore::default();
        let set = |_: &str| Some("sk-env".to_string());
        assert_eq!(key_status_with("openai", &broken, set), KeyStatus::Env);
        let KeyResolution::Found(key) = resolve_key_with("openai", &broken, set) else {
            panic!("an env key must resolve even when the store is broken");
        };
        assert_eq!(key, "sk-env");
    }

    #[test]
    fn resolve_key_with_prefers_env_over_the_keyring() {
        let store = MemStore::new();
        store.set("openai", "sk-ring").unwrap();
        let only_openai = |var: &str| (var == "OPENAI_API_KEY").then(|| "sk-env".to_string());
        let KeyResolution::Found(key) = resolve_key_with("openai", &store, only_openai) else {
            panic!("expected a resolved key");
        };
        assert_eq!(key, "sk-env");
    }

    /// The env stub is supplied explicitly so an ambient `OPENAI_API_KEY` cannot decide the
    /// outcome; the process env is not read.
    #[test]
    fn resolve_key_with_reads_a_stored_keyring_key() {
        let unset = |_: &str| None;
        let store = MemStore::new();
        store.set("openai", "sk-ring").unwrap();
        let KeyResolution::Found(key) = resolve_key_with("openai", &store, unset) else {
            panic!("expected a resolved key");
        };
        assert_eq!(key, "sk-ring");
        assert_eq!(
            resolve_key_with("openai", &MemStore::new(), unset),
            KeyResolution::Missing
        );
    }

    /// The empty-env-value rule holds through the wiring, not only inside `env_key`.
    #[test]
    fn resolve_key_with_treats_an_empty_env_value_as_absent() {
        let store = MemStore::new();
        store.set("openai", "sk-ring").unwrap();
        let KeyResolution::Found(key) = resolve_key_with("openai", &store, |_| Some(String::new()))
        else {
            panic!("expected the keyring value");
        };
        assert_eq!(key, "sk-ring");
    }

    /// The distinction the whole change exists for: a store that fails is not a store with no
    /// key, and the failure's text survives to the caller.
    #[test]
    fn resolve_key_with_reports_a_store_failure_rather_than_a_miss() {
        let broken = FailingStore::new("no D-Bus session");
        assert_eq!(
            resolve_key_with("openai", &broken, |_| None),
            KeyResolution::Unavailable("no D-Bus session".to_string())
        );
    }

    /// The public entry point, deterministic under any ambient environment: `ollama` declares no
    /// env var, so `process_env` is never consulted and only the store can answer.
    #[test]
    fn resolve_key_delegates_to_the_process_env_reader() {
        let store = MemStore::new();
        store.set("ollama", "sk-ring").unwrap();
        let KeyResolution::Found(key) = resolve_key("ollama", &store) else {
            panic!("expected the stored key");
        };
        assert_eq!(key, "sk-ring");
    }

    /// A live key must never reach a log, an assertion message, or a `dbg!`.
    #[test]
    fn a_resolved_key_is_redacted_in_debug_output() {
        let rendered = format!("{:?}", KeyResolution::Found("sk-secret".to_string()));
        assert!(!rendered.contains("sk-secret"), "{rendered}");
        assert_eq!(rendered, "Found(<redacted>)");
        assert_eq!(format!("{:?}", KeyResolution::Missing), "Missing");
        assert!(
            format!("{:?}", KeyResolution::Unavailable("locked".to_string())).contains("locked"),
            "the failure text is not a secret and must stay visible"
        );
    }
```

Add to `crates/tui/src/modal.rs`'s `mod tests`:

```rust
    /// End to end through the real seam: an unreadable store produces the store class, not
    /// `MissingKey`, and the sentence names the store rather than claiming there is no key.
    #[tokio::test]
    async fn an_unreadable_store_reports_the_store_failure_not_a_missing_key() {
        let store = light_factory_tui::credentials::FailingStore::new("no D-Bus session");
        let err = fetch_model_list("openai", None, &store, Locale::En)
            .await
            .expect_err("an unreadable store cannot produce a model list");
        assert_eq!(err.class, FetchFailure::StoreUnavailable);
        assert!(err.message.contains("openai"), "{}", err.message);
        assert!(err.message.contains("no D-Bus session"), "{}", err.message);
        assert!(
            !err.message.contains("No API key"),
            "the store failure must not be reported as a missing key: {}",
            err.message
        );
    }
```

Add to `crates/tui/src/app.rs`'s `mod tests`:

```rust
    /// `/key` must not list a provider as having no key when the store could not be asked.
    ///
    /// The assertion is the negative on purpose: `key_status` reads the *process* environment and
    /// `App` has no injection seam for it, so a developer with `OPENAI_API_KEY` exported gets
    /// `env` here and anyone else gets `unavailable`. Both are correct; `none` is the defect. The
    /// strict `KeyStatus::Unavailable` assertion lives in `selection.rs`, where the env is
    /// injected.
    #[test]
    fn the_key_listing_never_reports_an_unreadable_store_as_no_key() {
        let app = test_app_with_store(Arc::new(
            light_factory_tui::credentials::FailingStore::default(),
        ));
        assert_ne!(app.key_status_label("openai"), app.t("provider.key.none"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-tui`
Expected: FAIL to compile — `cannot find function 'env_key'`, `cannot find function 'read_store'`,
`no variant named 'Unavailable' found for enum 'KeyStatus'`, `cannot find type 'KeyResolution'`.

- [ ] **Step 3: Replace the resolution seam in `selection.rs`**

Delete `classify`, `sources_with`, and `resolve_key_from` entirely. Add
`use crate::text::one_line;` to the module's imports. Write, in their place:

```rust
/// The env-supplied key for `provider`, if the environment supplies a usable one.
///
/// An empty value is treated as absent, so the connect flow never fetches with an empty key. This
/// is the single statement of that rule — the deleted `classify`/`resolve_key_from` pair stated
/// it twice.
fn env_key(provider: &str, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    env_key_var(provider).and_then(env).filter(|k| !k.is_empty())
}

/// The store's answer for `provider`, with a failure reduced to one display-ready line.
///
/// `{:#}` keeps anyhow's source chain, so the cause (no D-Bus session, a locked wallet) survives
/// rather than only the outermost "failed". [`one_line`] strips control characters because this
/// text is written into a terminal cell, where a raw `ESC` is an escape-sequence injection. The
/// length cap belongs to the modal, which already owns it.
fn read_store(provider: &str, store: &dyn CredentialStore) -> Result<Option<String>, String> {
    store.get(provider).map_err(|e| one_line(&format!("{e:#}")))
}
```

Widen `KeyStatus`:

```rust
/// Where a provider's key comes from, for the `/key` listing and the `/connect` rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    Env,
    Keyring,
    /// No key is stored, and the environment supplies none.
    None,
    /// The credential store could not be read, so whether a key exists is unknown. Distinct from
    /// [`KeyStatus::None`] on purpose: the remedy for `None` is to store a key, which is not a
    /// remedy for a store that cannot be read.
    Unavailable,
}
```

Add `KeyResolution`:

```rust
/// A resolved API key, or why there is none.
///
/// The point of the type is the distinction between [`KeyResolution::Missing`] and
/// [`KeyResolution::Unavailable`]: the first has a remedy the user can act on (store a key), the
/// second does not, and reporting the second as the first is the defect this replaces.
#[derive(Clone, PartialEq, Eq)]
pub enum KeyResolution {
    Found(String),
    Missing,
    /// The store could not be read; the payload is one display-ready line naming the cause.
    Unavailable(String),
}

impl std::fmt::Debug for KeyResolution {
    /// Redacts the key. `Found` holds a live credential, and a `{:?}` at a future call site — a
    /// `dbg!`, a `tracing` field, an assertion message — would otherwise print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyResolution::Found(_) => f.write_str("Found(<redacted>)"),
            KeyResolution::Missing => f.write_str("Missing"),
            KeyResolution::Unavailable(error) => {
                f.debug_tuple("Unavailable").field(error).finish()
            }
        }
    }
}
```

Rewrite the two `_with` functions and their public wrappers:

```rust
/// A provider's key source against an explicit environment. Lets the wiring be tested without the
/// process env deciding the result.
///
/// The store is consulted only when the environment did not answer, which is what keeps
/// [`KeyStatus::Unavailable`] meaningful: a working `OPENAI_API_KEY` must not be reported as
/// unavailable because the keyring is down.
fn key_status_with(
    provider: &str,
    store: &dyn CredentialStore,
    env: impl Fn(&str) -> Option<String>,
) -> KeyStatus {
    if env_key(provider, env).is_some() {
        return KeyStatus::Env;
    }
    match read_store(provider, store) {
        Ok(Some(_)) => KeyStatus::Keyring,
        Ok(None) => KeyStatus::None,
        Err(_) => KeyStatus::Unavailable,
    }
}

/// Classify a provider's key source without revealing the value.
pub fn key_status(provider: &str, store: &dyn CredentialStore) -> KeyStatus {
    key_status_with(provider, store, process_env)
}

/// The resolved API key for a provider against an explicit environment: env wins over the store,
/// and the store is consulted only when the env has no usable key.
fn resolve_key_with(
    provider: &str,
    store: &dyn CredentialStore,
    env: impl Fn(&str) -> Option<String>,
) -> KeyResolution {
    if let Some(key) = env_key(provider, env) {
        return KeyResolution::Found(key);
    }
    match read_store(provider, store) {
        Ok(Some(key)) => KeyResolution::Found(key),
        Ok(None) => KeyResolution::Missing,
        Err(error) => KeyResolution::Unavailable(error),
    }
}

/// The resolved API key for a provider (env over store), or why there is none.
pub fn resolve_key(provider: &str, store: &dyn CredentialStore) -> KeyResolution {
    resolve_key_with(provider, store, process_env)
}
```

Leave `process_env` exactly as it is.

- [ ] **Step 4: Wire the `/models` fetch to the new resolution**

In `crates/tui/src/modal.rs`, replace `fetch_model_list_inner`'s key resolution:

```rust
    let key = match key_override {
        Some(k) => Some(k),
        None => match crate::selection::resolve_key(provider, store) {
            crate::selection::KeyResolution::Found(k) => Some(k),
            crate::selection::KeyResolution::Missing => None,
            crate::selection::KeyResolution::Unavailable(error) => {
                return Err(FetchError {
                    class: FetchFailure::StoreUnavailable,
                    // Our own sentence rather than a remote one, but still capped: the cap is
                    // what keeps the modal's own remedy rows on screen when the backend is
                    // verbose.
                    message: summarize_provider_error(&i18n::t_with(
                        locale,
                        "connect.store_unavailable",
                        &[("provider", provider), ("error", &error)],
                    )),
                });
            }
        },
    };
    fetch_with_key(provider, key, locale).await
```

`fetch_with_key` keeps its `Option<String>` signature and its `MissingKey` arm untouched, so the
"no key" sentence still has exactly one source.

- [ ] **Step 5: Add the `/key` label and its strings**

In `crates/tui/src/app.rs`, add the arm to `key_status_label`:

```rust
            crate::selection::KeyStatus::Unavailable => {
                self.t("provider.key.unavailable").to_string()
            }
```

In `crates/tui/src/i18n.rs`, add `("provider.key.unavailable", "unavailable"),` to `EN` after
`provider.key.none` (line 191), and
`("provider.key.unavailable", "no disponible"),` to `ES` after its `provider.key.none`
(line 500).

- [ ] **Step 6a: Remove Task 3's temporary dead-code expectation**

Task 3 added `#[cfg_attr(not(test), expect(dead_code, reason = "key resolution does not report an
unreadable store yet"))]` to `FetchFailure::StoreUnavailable`, because nothing in non-test code
constructed it until Step 4 above. Step 4 now does, so the `expect` is unfulfilled and the build
fails under `-D warnings`. Delete the whole `#[cfg_attr(...)]` block and the two paragraphs of the
variant's doc comment that explain it, leaving the first paragraph ("The credential store could not
be read…") intact. That the compiler forces this is why `expect` was used instead of `allow`.

- [ ] **Step 6b: Rename the colliding test-local store double in `app.rs`**

`crates/tui/src/app.rs`'s `mod tests` already contains an unrelated `struct FailingStore;` whose
`get`/`delete` return `Ok` and whose `set` fails, used by
`handle_connect_key_keyring_failure_sets_error_and_stays`. Two different doubles under one name in
one file is a trap. Rename the local one to `SetFailsStore`, update its doc comment to
`/// A store whose \`set\` always fails, for exercising the keyring write-failure branch.`, and
update its single use site. Do not change its behaviour or what that test asserts — the new tests
in this task use the fully-qualified `light_factory_tui::credentials::FailingStore`.

- [ ] **Step 6: Stop `/connect` reporting an unreadable store as unconnected**

In `crates/tui/src/app.rs`, `build_provider_rows` currently reads
`key_status(id, ...) != KeyStatus::None`, which now yields `true` for `Unavailable` — the correct
navigation, since asking for a key the user already stored is the defect. Make that explicit
rather than incidental:

```rust
                let connected = if *id == "ollama" {
                    std::env::var("LIGHT_OLLAMA").as_deref() == Ok("1")
                } else {
                    // `Unavailable` counts as connected here: the store could not be asked, so
                    // routing to key entry would demand a key the user may already have stored.
                    // Task 5 gives it its own row state; this keeps the navigation honest now.
                    !matches!(
                        crate::selection::key_status(id, self.store.as_ref()),
                        crate::selection::KeyStatus::None
                    )
                };
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p light-factory-tui`
Expected: PASS — every new test plus all pre-existing ones.

Also run, as the #49 acceptance criterion this change must not regress:
`OPENAI_API_KEY=sk-test cargo test -p light-factory-tui`
Expected: PASS.

- [ ] **Step 8: Run clippy and commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/tui/src/selection.rs crates/tui/src/modal.rs crates/tui/src/app.rs crates/tui/src/i18n.rs
git commit -m "tui: keep a credential store failure distinct from a missing key"
```

Expected: all green; clippy clean.

---

### Task 5: Give the `/connect` provider rows a third state

**Files:**
- Modify: `crates/tui/src/modal.rs` (`ProviderRow` at lines 27-33, the `ProviderList` Enter arm at
  454-470, the `ProviderList` render arm at 965-984, and the `row` test helper at 1384-1388)
- Modify: `crates/tui/src/app.rs` (`build_provider_rows` at 647-661, and the `ProviderRow`
  constructions in `mod tests`)
- Modify: `crates/tui/src/i18n.rs` (`connect.store_unavailable_row`, EN + ES, plus a width test)

**Interfaces:**
- Consumes: `crate::selection::KeyStatus` (Task 4).
- Produces: `pub(crate) enum RowKey { Present, Absent, Unavailable }` and
  `pub(crate) struct ProviderRow { pub(crate) id: String, pub(crate) key: RowKey }`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/tui/src/modal.rs`'s `mod tests`:

```rust
    /// The store could not be asked, so Enter must not route to key entry: that would demand a
    /// key the user may already have stored, and storing it would fail against the same store.
    #[test]
    fn an_unavailable_row_goes_to_the_model_list_not_key_entry() {
        let step = ConnectStep::ProviderList {
            rows: vec![ProviderRow {
                id: "openai".to_string(),
                key: RowKey::Unavailable,
            }],
            selected: 0,
        };
        let ModalTransition::Step(Modal::Connect(next)) =
            connect_step_next(&step, key(KeyCode::Enter))
        else {
            panic!("Enter must step");
        };
        assert!(
            matches!(next, ConnectStep::ModelList { .. }),
            "expected the model list, got {next:?}"
        );
    }

    #[test]
    fn an_absent_row_still_goes_to_key_entry() {
        let step = ConnectStep::ProviderList {
            rows: vec![ProviderRow {
                id: "openai".to_string(),
                key: RowKey::Absent,
            }],
            selected: 0,
        };
        let ModalTransition::Step(Modal::Connect(next)) =
            connect_step_next(&step, key(KeyCode::Enter))
        else {
            panic!("Enter must step");
        };
        assert!(
            matches!(next, ConnectStep::KeyEntry { .. }),
            "expected key entry, got {next:?}"
        );
    }
```

`key` is the existing plain-key helper (`fn key(code: KeyCode) -> KeyEvent` at `modal.rs:1376`,
next to `ctrl_key` at `:1380`); do not add a second.

Add to `crates/tui/src/app.rs`'s `mod tests`:

```rust
    /// An unreadable store must not render a provider as though no key were stored — that is the
    /// row state that routes Enter to key entry.
    ///
    /// Negative assertion for the same reason as `the_key_listing_never_reports_...`: with
    /// `OPENAI_API_KEY` exported this row is `Present`, without it `Unavailable`. `Absent` is the
    /// defect. `RowKey::Unavailable` itself is pinned in `modal.rs`'s transition tests.
    #[test]
    fn provider_rows_never_report_an_unreadable_store_as_having_no_key() {
        let app = test_app_with_store(Arc::new(
            light_factory_tui::credentials::FailingStore::default(),
        ));
        let rows = app.build_provider_rows();
        let openai = rows
            .iter()
            .find(|r| r.id == "openai")
            .expect("openai is a listed provider");
        assert_ne!(openai.key, RowKey::Absent);
    }

    #[test]
    fn provider_rows_report_a_stored_key_as_present() {
        let store = MemStore::new();
        store.set("openai", "sk-ring").unwrap();
        let app = test_app_with_store(Arc::new(store));
        let rows = app.build_provider_rows();
        let openai = rows.iter().find(|r| r.id == "openai").expect("listed");
        assert_eq!(openai.key, RowKey::Present);
    }
```

Add to `crates/tui/src/i18n.rs`'s `mod tests`:

```rust
    /// The connect modal's provider rows share the footers' 60-column popup (58 inner), but
    /// `every_footer_fits_the_popup_in_both_locales` gates `*.footer` keys only. `anthropic` is
    /// the longest id in `PROVIDER_NAMES`, and the row is drawn as `"> {id} ({suffix})"`.
    #[test]
    fn the_unavailable_row_suffix_fits_the_popup_in_both_locales() {
        const INNER_WIDTH: usize = 58;
        for (locale, name) in [(Locale::En, "EN"), (Locale::Es, "ES")] {
            let row = format!(
                "> anthropic ({})",
                t(locale, "connect.store_unavailable_row")
            );
            let columns = row.chars().count();
            assert!(columns <= INNER_WIDTH, "{name} row is {columns} columns: {row}");
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-tui`
Expected: FAIL to compile — `cannot find type 'RowKey' in this scope`, and
`struct 'ProviderRow' has no field named 'key'`.

- [ ] **Step 3: Replace the boolean with the tri-state**

In `crates/tui/src/modal.rs`, replace `ProviderRow` (lines 27-33) with:

```rust
/// Whether a provider row has a key behind it.
///
/// Three states rather than a boolean because the failure that motivated them is a store that
/// cannot answer: rendering that as "no key" tells the user to store a key they may already have
/// stored, and the false branch is the one that routes to key entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowKey {
    /// A key is available, from the environment or the store.
    Present,
    /// The store answered, and there is no key.
    Absent,
    /// The store could not be read, so whether a key exists is unknown.
    Unavailable,
}

/// One row of the connect modal's provider list. Self-contained (id + key state) so the pure
/// transition can decide navigation without touching the keyring.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ProviderRow {
    pub(crate) id: String,
    pub(crate) key: RowKey,
}
```

The derive list above (`Debug, Clone, PartialEq, Eq`) is exactly what `ProviderRow` already carries
at `modal.rs:29` — unchanged.

Update the Enter arm (lines 454-470):

```rust
            KeyCode::Enter => match rows.get(*selected) {
                // `Unavailable` proceeds like `Present`: the fetch re-reads the store and reports
                // the real failure, where key entry would ask for a key the user may already have
                // stored and then fail to write it to the same store.
                Some(row)
                    if matches!(row.key, RowKey::Present | RowKey::Unavailable)
                        || row.id == "ollama" =>
                {
                    ModalTransition::Step(Modal::Connect(ConnectStep::ModelList {
                        rows: rows.clone(),
                        provider: row.id.clone(),
                        models: Vec::new(),
                        selected: 0,
                        fetching: true,
                        error: None,
                        from_key: false,
                    }))
                }
```

Update the render arm's suffix (lines 974-978):

```rust
                let suffix = match row.key {
                    RowKey::Present => format!(" ({})", i18n::t(ctx.locale, "connect.connected")),
                    RowKey::Absent => String::new(),
                    RowKey::Unavailable => format!(
                        " ({})",
                        i18n::t(ctx.locale, "connect.store_unavailable_row")
                    ),
                };
```

Update the `row` test helper (lines 1384-1388) to build from a `RowKey`:

```rust
    fn row(id: &str, key: RowKey) -> ProviderRow {
        ProviderRow {
            id: id.to_string(),
            key,
        }
    }
```

and update its call sites in `modal.rs`'s tests: `row(id, true)` becomes `row(id, RowKey::Present)`
and `row(id, false)` becomes `row(id, RowKey::Absent)`. Do not change what those tests assert.

- [ ] **Step 4: Add the row-suffix strings**

In `crates/tui/src/i18n.rs`, add to `EN` after `connect.connected` (line 266):

```rust
    ("connect.store_unavailable_row", "key store unavailable"),
```

and to `ES` after its `connect.connected` (line 584):

```rust
    (
        "connect.store_unavailable_row",
        "almac\u{e9}n de claves no disponible",
    ),
```

- [ ] **Step 5: Map `KeyStatus` to `RowKey` in `build_provider_rows`**

In `crates/tui/src/app.rs`, replace the body written in Task 4 Step 6:

```rust
    fn build_provider_rows(&self) -> Vec<ProviderRow> {
        PROVIDER_NAMES
            .iter()
            .map(|id| {
                let key = if *id == "ollama" {
                    // Ollama takes no API key; `LIGHT_OLLAMA` is the whole of its configuration,
                    // so the credential store is never consulted for it.
                    if std::env::var("LIGHT_OLLAMA").as_deref() == Ok("1") {
                        RowKey::Present
                    } else {
                        RowKey::Absent
                    }
                } else {
                    match crate::selection::key_status(id, self.store.as_ref()) {
                        crate::selection::KeyStatus::Env
                        | crate::selection::KeyStatus::Keyring => RowKey::Present,
                        crate::selection::KeyStatus::None => RowKey::Absent,
                        crate::selection::KeyStatus::Unavailable => RowKey::Unavailable,
                    }
                };
                ProviderRow {
                    id: id.to_string(),
                    key,
                }
            })
            .collect()
    }
```

Add `RowKey` to `app.rs`'s `use crate::modal::{...}` import list (alongside `ProviderRow`) and to
the `use super::{...}` list in `app.rs`'s `mod tests`. Update every `ProviderRow { id, connected }`
construction in `app.rs`'s tests to `ProviderRow { id, key: RowKey::Present }` (or `Absent`,
matching what the test previously meant by `connected: false`).

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p light-factory-tui`
Expected: PASS — the five new tests plus all pre-existing ones.

- [ ] **Step 7: Run clippy and commit**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/tui/src/modal.rs crates/tui/src/app.rs crates/tui/src/i18n.rs
git commit -m "tui: give the connect provider rows an unreadable-store state"
```

Expected: all green; clippy clean.

---

### Task 6: Say so when the offline fallback was caused by the store

**Files:**
- Modify: `crates/tui/src/provider.rs` (`ProviderInfo` at lines 11-20, add `StoreFailure` and
  `ProviderInfo::notices`, and the `info` test helper at 74-82)
- Modify: `crates/tui/src/selection.rs` (`apply_preferences` at 108-126, `build_selection` at
  128-131, `rebuild` at 146-152)
- Modify: `crates/tui/src/app.rs` (`enter_engine`'s notice assembly at lines 333-339, and the
  `ProviderInfo` construction in `test_app_with_store`)
- Modify: `crates/tui/src/i18n.rs` (`provider.store.unavailable`,
  `provider.offline.store_unavailable`, EN + ES)

**Interfaces:**
- Consumes: `crate::selection::read_store` (Task 4), `FailingStore` (Task 2).
- Produces: `pub struct StoreFailure { pub provider: String, pub error: String }` in
  `crate::provider`; `ProviderInfo.store_failures: Vec<StoreFailure>`;
  `ProviderInfo::notices(&self, locale: Locale) -> Vec<String>`;
  `apply_preferences(..) -> (Selection, Vec<StoreFailure>)` and
  `build_selection(..) -> (Selection, Vec<StoreFailure>)`. `rebuild`'s signature is unchanged.

- [ ] **Step 1: Write the failing tests**

Add to `crates/tui/src/selection.rs`'s `mod tests`:

```rust
    /// The startup path must report a store it could not read instead of silently continuing
    /// with an empty key map — the silence is what makes the offline fallback inexplicable.
    #[test]
    fn apply_preferences_reports_a_store_failure_for_every_remote_provider() {
        let broken = FailingStore::new("no D-Bus session");
        let (selection, failures) =
            apply_preferences(Selection::default(), &settings(None), &broken);
        assert!(selection.keys.is_empty());
        assert_eq!(failures.len(), REMOTE_IDS.len());
        assert!(failures.iter().all(|f| f.error == "no D-Bus session"));
        assert!(failures.iter().any(|f| f.provider == "openai"));
    }

    /// A store failure is per-entry, so a partial failure must not discard the keys that did
    /// resolve. An env-supplied key is already in `base.keys` and is never re-read.
    #[test]
    fn apply_preferences_keeps_an_env_key_and_reports_nothing_for_it() {
        let broken = FailingStore::default();
        let mut base = Selection::default();
        base.keys.insert("openai".to_string(), "sk-env".to_string());
        let (selection, failures) = apply_preferences(base, &settings(None), &broken);
        assert_eq!(selection.keys.get("openai"), Some(&"sk-env".to_string()));
        assert!(
            failures.iter().all(|f| f.provider != "openai"),
            "a provider the env already answered for is never read from the store"
        );
    }

    #[test]
    fn apply_preferences_reports_no_failures_for_a_working_store() {
        let store = MemStore::new();
        store.set("openai", "sk-o").unwrap();
        let (selection, failures) =
            apply_preferences(Selection::default(), &settings(Some("openai")), &store);
        assert!(failures.is_empty());
        assert_eq!(selection.keys.get("openai"), Some(&"sk-o".to_string()));
    }

    /// `rebuild` is the startup entry point; the failures have to survive it or nothing can
    /// render them.
    /// `build_selection` starts from `selection_from_env()`, so a developer with all four of
    /// `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`/`GEMINI_API_KEY`/`DEEPSEEK_API_KEY` exported would see
    /// every provider skipped before the store is read and no failure recorded. That is the same
    /// ambient-env caveat the App-level tests carry; the injected-env assertions live in
    /// `apply_preferences_reports_a_store_failure_for_every_remote_provider` above.
    #[test]
    fn rebuild_carries_store_failures_into_the_provider_info() {
        let broken = FailingStore::default();
        let (_provider, info) = rebuild(&settings(None), &broken);
        assert!(!info.store_failures.is_empty());
    }
```

The two pre-existing `apply_preferences_*` tests destructure a single return value; update them to
`let (selection, _failures) = apply_preferences(...)`.

Add to `crates/tui/src/provider.rs`'s `mod tests`:

```rust
    fn failure(provider: &str) -> StoreFailure {
        StoreFailure {
            provider: provider.to_string(),
            error: "locked".to_string(),
        }
    }

    /// The issue's third acceptance criterion: an offline fallback caused by an unreadable store
    /// must say so, instead of telling the user to set a key they already set.
    #[test]
    fn a_store_failure_replaces_the_nothing_configured_notice() {
        let mut info = info(Some(OfflineReason::NothingConfigured), None);
        info.store_failures = vec![failure("openai")];
        let notices = info.notices(Locale::En);
        assert!(
            notices.iter().any(|n| n.contains("openai") && n.contains("locked")),
            "the failing provider and cause must be named: {notices:?}"
        );
        assert!(
            !notices.iter().any(|n| n.contains("No provider configured")),
            "the nothing-configured notice is false here: {notices:?}"
        );
        assert!(
            notices.iter().any(|n| n.contains("credential store")),
            "the offline line must name the store: {notices:?}"
        );
    }

    /// Only `NothingConfigured` is substituted: a rejected base URL has its own real cause, and
    /// overwriting it would repeat this very bug in the other direction.
    #[test]
    fn a_store_failure_does_not_overwrite_another_offline_reason() {
        let mut info = info(
            Some(OfflineReason::BaseUrlRejected {
                var: "LIGHT_OPENAI_BASE_URL".into(),
            }),
            None,
        );
        info.store_failures = vec![failure("openai")];
        let notices = info.notices(Locale::En);
        assert!(
            notices.iter().any(|n| n.contains("LIGHT_OPENAI_BASE_URL")),
            "{notices:?}"
        );
        assert!(
            notices.iter().any(|n| n.contains("openai") && n.contains("locked")),
            "the store failure is still reported on its own line: {notices:?}"
        );
    }

    #[test]
    fn notices_without_a_store_failure_are_unchanged() {
        let info = info(Some(OfflineReason::NothingConfigured), None);
        assert_eq!(
            info.notices(Locale::En),
            vec![offline_notice(Locale::En, &OfflineReason::NothingConfigured)]
        );
    }

    #[test]
    fn notices_keep_the_selection_warnings_first() {
        let mut info = info(None, Some(SelectedBy::KeyPrecedence));
        info.warnings = vec!["a warning".to_string()];
        info.store_failures = vec![failure("openai")];
        let notices = info.notices(Locale::En);
        assert_eq!(notices[0], "a warning");
        assert_eq!(notices.len(), 2, "a live provider adds no offline line");
    }

    #[test]
    fn a_live_provider_with_no_warnings_has_no_notices() {
        assert!(
            info(None, Some(SelectedBy::KeyPrecedence))
                .notices(Locale::En)
                .is_empty()
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p light-factory-tui`
Expected: FAIL to compile — `cannot find type 'StoreFailure'`, `no method named 'notices'`, and
`this expression has type 'Selection'` at the `let (selection, failures) = apply_preferences(...)`
destructurings.

- [ ] **Step 3: Add `StoreFailure`, the field, and `notices`**

In `crates/tui/src/provider.rs`, add above `ProviderInfo`:

```rust
/// One provider's credential-store read that failed, with the cause already reduced to one line.
///
/// It lives here rather than in `crates/providers` because the credential store is a TUI concept:
/// the providers crate has no notion of a keyring, and giving `OfflineReason` a variant for one
/// would make it name a dependency it does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFailure {
    pub provider: String,
    pub error: String,
}
```

Add the field to `ProviderInfo`:

```rust
    /// Providers whose stored key could not be read. Empty when the store answered for all of
    /// them — including when it answered "no key".
    pub store_failures: Vec<StoreFailure>,
```

Add the method:

```rust
impl ProviderInfo {
    /// Every line the engine pane shows about how this provider was chosen: the selection
    /// warnings, one line per unreadable credential store, then the offline notice if it is
    /// offline.
    ///
    /// The offline line is substituted only for [`OfflineReason::NothingConfigured`], and only
    /// when a store actually failed: that is the one case where the store is why `keys` is empty.
    /// Every other reason has its own real cause, and overwriting it would repeat the defect this
    /// exists to fix, in the other direction — the failure is already reported on its own line.
    pub fn notices(&self, locale: Locale) -> Vec<String> {
        let mut lines = self.warnings.clone();
        for failure in &self.store_failures {
            lines.push(i18n::t_with(
                locale,
                "provider.store.unavailable",
                &[("provider", &failure.provider), ("error", &failure.error)],
            ));
        }
        if let Some(reason) = &self.offline {
            let store_caused = !self.store_failures.is_empty()
                && matches!(reason, OfflineReason::NothingConfigured);
            lines.push(if store_caused {
                i18n::t(locale, "provider.offline.store_unavailable").to_string()
            } else {
                offline_notice(locale, reason)
            });
        }
        lines
    }
}
```

Place it inside the existing `impl ProviderInfo` block next to `display` and `reason` rather than
opening a second one. Update the `info` test helper (lines 74-82) to add
`store_failures: Vec::new(),`.

- [ ] **Step 4: Add the EN and ES strings**

In `crates/tui/src/i18n.rs`, add to `EN` next to the other `provider.offline.*` entries
(lines 120-130):

```rust
    (
        "provider.store.unavailable",
        "Could not read the stored key for {provider}: {error}",
    ),
    (
        "provider.offline.store_unavailable",
        "Falling back to the offline provider: the credential store could not be read, so stored keys were unavailable",
    ),
```

and to `ES` at the mirrored positions (lines 414-424):

```rust
    (
        "provider.store.unavailable",
        "No se pudo leer la clave guardada de {provider}: {error}",
    ),
    (
        "provider.offline.store_unavailable",
        "Usando el proveedor sin conexi\u{f3}n: no se pudo leer el almac\u{e9}n de credenciales, as\u{ed} que las claves guardadas no estaban disponibles",
    ),
```

- [ ] **Step 5: Carry the failures out of `build_selection`**

In `crates/tui/src/selection.rs`, add `use crate::provider::StoreFailure;` to the imports and
rewrite the three functions:

```rust
/// Layer the persisted preferences and stored keys over an env-derived [`Selection`], reporting
/// any provider whose stored key could not be read.
///
/// A failed read contributes no key and one [`StoreFailure`]: the fallback is unchanged, but it
/// is no longer silent.
pub fn apply_preferences(
    mut base: Selection,
    settings: &Settings,
    store: &dyn CredentialStore,
) -> (Selection, Vec<StoreFailure>) {
    let mut failures = Vec::new();
    for id in REMOTE_IDS {
        if base.keys.contains_key(id) {
            continue;
        }
        match read_store(id, store) {
            Ok(Some(key)) => {
                base.keys.insert(id.to_string(), key);
            }
            Ok(None) => {}
            Err(error) => failures.push(StoreFailure {
                provider: id.to_string(),
                error,
            }),
        }
    }
    base.preferred = settings.provider.clone();
    for (id, model) in &settings.models {
        base.models
            .entry(id.clone())
            .or_insert_with(|| model.clone());
    }
    (base, failures)
}

/// Assemble the effective [`Selection`]: environment (via the providers crate), then the stored
/// keys and persisted preferences layered on top, plus any store failure encountered.
pub fn build_selection(
    settings: &Settings,
    store: &dyn CredentialStore,
) -> (Selection, Vec<StoreFailure>) {
    apply_preferences(selection_from_env(), settings, store)
}
```

and fold the failures into the info in `rebuild`:

```rust
/// Build the active provider and its display record from the given settings and credential store.
pub fn rebuild(
    settings: &Settings,
    store: &dyn CredentialStore,
) -> (Arc<dyn Provider>, ProviderInfo) {
    let (selection, store_failures) = build_selection(settings, store);
    let (provider, mut info) = build_and_info(&selection);
    info.store_failures = store_failures;
    (provider, info)
}
```

Add `store_failures: Vec::new(),` to the `ProviderInfo` literal in `build_and_info` (line 139).

- [ ] **Step 6: Render the notices**

In `crates/tui/src/app.rs`, `enter_engine` currently reads:

```rust
        self.engine_log.clear();
        for warning in info.warnings {
            self.engine_log.push(warning);
        }
        if let Some(reason) = &info.offline {
            self.engine_log
                .push(crate::provider::offline_notice(self.config.lang, reason));
        }
```

Replace **only the loop and the `if let`** (lines 333-339) — `self.engine_log.clear()` on line 332
stays, or the engine log accumulates across re-entries. The result reads:

```rust
        self.engine_log.clear();                                  // line 332, unchanged
        self.engine_log.extend(info.notices(self.config.lang));   // replaces 333-339
```

Add `store_failures: Vec::new(),` to the `ProviderInfo` literal in `test_app_with_store`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p light-factory-tui`
Expected: PASS — the nine new tests plus all pre-existing ones.

- [ ] **Step 8: Run the whole workspace, clippy, and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/tui/src/provider.rs crates/tui/src/selection.rs crates/tui/src/app.rs crates/tui/src/i18n.rs
git commit -m "tui: name the credential store when it is why the provider fell back to offline"
```

Expected: all green; clippy clean. The `crates/persistence` integration test skips without
`DATABASE_URL` — that is the documented pre-existing behaviour, not a regression.

## Deviations from the plan as written

- **Task 3 added a temporary `expect(dead_code)` on `FetchFailure::StoreUnavailable`.** The task as
  written left the variant constructed only by tests until Task 4 wired the resolution seam, which
  fails the bin target under `-D warnings`. The implementer added
  `#[cfg_attr(not(test), expect(dead_code, reason = "…"))]` — `expect` rather than `allow`, so the
  suppression becomes a compile error the moment Task 4 constructs the variant. Task 4 Step 6a
  removes it. Accepted: the alternative was to merge Tasks 3 and 4, losing the independent review
  of the class and its remedy.
- **Task 4 renames `app.rs`'s test-local `FailingStore` to `SetFailsStore`** (Step 6b). Not in the
  original plan; added after the Task 2 spec review flagged that the library's new `FailingStore`
  (all operations fail) and a pre-existing test-local `FailingStore` (only `set` fails) would
  otherwise share a name in one file.
- **Task 4's `modal.rs` end-to-end test uses provider `"local"`, not `"openai"`.** As the plan wrote
  it the test passed a plain `cargo test` but failed under `OPENAI_API_KEY=sk-test`:
  `fetch_model_list` reads the *process* environment and has no injection seam, so an ambient key
  resolves via `KeyResolution::Found`, the store is never consulted, and the fetch escapes to a real
  network request that returns `Auth` rather than `StoreUnavailable`. `"local"` declares no env var
  (`env_key_var("local") == None`), so `env_key` cannot answer and only the store can — the same
  technique the plan already uses with `"ollama"` in
  `resolve_key_delegates_to_the_process_env_reader`. Every assertion survives verbatim except
  `contains("openai")` → `contains("local")`; the strict class assertion is kept rather than
  weakened. This also stops the suite reaching the network on any machine with a provider key
  exported.
- **Task 4 also fixed two comment inaccuracies in `modal.rs`** surfaced by the Task 3 quality
  review: the `ModelsStep::Credentials` doc's "pure function of the step" claim (`models_view`
  already takes a locale-bearing `ModalContext`), replaced with the real argument — the carried
  `remedy` follows the sibling `error` field that `app.rs` already precomputes; and the render arm's
  reference to "the input box", which belongs to `ModelsStep::Manual`, not `Credentials`.
