# Credential store failure vs. credential absence — design

> **Status:** DRAFT — keep "the keyring could not be read" distinct from "no key is stored", all the way to the user-facing sentence.

> **Implements:** https://github.com/savvagent/light-factory/issues/51
> **Follows:** https://github.com/savvagent/light-factory/issues/47 (the error-class rule this extends), https://github.com/savvagent/light-factory/issues/49 (the injected-env seam this builds on)

## 1. Brief

`CredentialStore::get` (`crates/tui/src/credentials.rs:11-13`) documents a three-way contract:

```rust
/// The stored key for `provider`, or `None` when absent. `Err` signals a store failure
/// (e.g. the OS keyring is unavailable) rather than "not found".
fn get(&self, provider: &str) -> anyhow::Result<Option<String>>;
```

`KeyringStore::get` honours it: `keyring::Error::NoEntry` becomes `Ok(None)` and every other
backend error becomes `Err` (`credentials.rs:32-39`). Three consumers then throw the distinction
away:

1. `sources_with` (`crates/tui/src/selection.rs:50`) — `store.get(provider).ok().flatten()`,
   feeding both `key_status` and `resolve_key`.
2. `apply_preferences` (`selection.rs:110-113`) — `if let Ok(Some(key))`, whose `else` is silence.

Consequences, all of them the #47 failure shape (the user is told to fix something that is not
broken, so the real fault stays invisible):

- **`/models` claims there is no key.** `fetch_model_list_inner` (`modal.rs:696`) calls
  `resolve_key`; a locked KWallet or a dead D-Bus session yields `None`, so `fetch_with_key`
  returns `FetchFailure::MissingKey` with `connect.no_key` — "No API key for openai" — for a user
  who stored one. The modal's credentials step then offers `/connect`, `/key openai`, or
  `/model <id>`: `/key` writes to the same broken store and fails, and `/model` sets an id that
  the next `/ask` cannot use.
- **`/key` lists the provider as `none`.** `key_status_label` (`app.rs:1006-1011`) maps the
  swallowed error to `KeyStatus::None` → "none".
- **`/connect` offers to take a key the user already gave.** `build_provider_rows`
  (`app.rs:647-661`) derives `connected` from `key_status(...) != KeyStatus::None`, so an
  unreadable store renders every remote provider as unconnected and `connect_step_next`
  (`modal.rs:454-470`) routes Enter to the key-entry step.
- **Startup drops to the offline provider with no explanation.** `apply_preferences` inserts
  nothing, `build_provider` sees an empty `keys` map, and the engine pane shows
  `provider.offline.nothing` — "No provider configured — set ANTHROPIC_API_KEY (or another
  provider's key) or LIGHT_OLLAMA=1" — which is false and points at the wrong remedy.

It is also untestable: `MemStore` (`credentials.rs:66-84`) always returns `Ok`, so no test double
can reach the `Err` branch of any of these call sites.

### Premise corrections

The issue names `resolve_key`, `key_status`, and `apply_preferences` as the swallowing call sites.
That is accurate, but two of them swallow through a shared helper rather than individually:
`resolve_key_with` and `key_status_with` both go through `sources_with`, so one fix covers both.
The issue does not name `build_provider_rows` (`app.rs:647`), which is a fourth consumer of the
same distinction via `key_status` and produces the most actively misleading behaviour of the set —
it is in scope here.

## 2. Scope

**In:**

- A `CredentialStore` test double whose operations fail (`credentials.rs`).
- Three-way key resolution in `crates/tui/src/selection.rs`: `KeyStatus::Unavailable`, a
  `KeyResolution` result for `resolve_key`, and store failures surfaced out of `build_selection`
  instead of dropped.
- A `FetchFailure::StoreUnavailable` class and its `/models` message (`crates/tui/src/modal.rs`).
- The `/key` listing label, the `/connect` provider-row state, and the engine-pane offline notice
  (`app.rs`, `modal.rs`, `provider.rs`).
- A private `crates/tui/src/text.rs` holding the one-line/truncate helpers both the resolution seam
  and the modal need (§4.1).
- EN + ES catalog entries for every new string.

**Out:**

- Retrying or repairing the keyring (unlocking a wallet, starting a D-Bus session). The TUI
  reports; it does not manage the OS credential service.
- Caching a successful read so a mid-session failure can be answered from memory. That trades one
  wrong answer for a staler one, and `#47`'s note that `provider_info.offline` is already a stale
  snapshot is an argument against adding a second cache, not for it.
- Any change to `KeyringStore`'s mapping of `keyring::Error` — it is already correct.
- `store.set` / `store.delete` failures. Both are already surfaced (`app.rs:479`, `app.rs:918`,
  `clear_key` at `app.rs:988-1004`); only `get` is swallowed.
- The providers crate. `OfflineReason` stays as-is; the credential store is a TUI concept and the
  inward dependency flow keeps it out of `providers`.

## 3. Goal & success criteria

A store failure must be distinguishable from a store miss at every point where the TUI acts on the
answer, and the sentence the user reads must name the store failure rather than assert that no key
is configured.

- `cargo test -p light-factory-tui` exercises the `Err` branch of `key_status`, `resolve_key`,
  `build_selection`, and the `/models` fetch, via a double that returns `Err` from `get`.
- With an unreadable store and no env key, `/models` reports the store failure — not
  "No API key for {provider}".
- With an unreadable store, `/key` lists the provider as `unavailable`, not `none`.
- With an unreadable store, `/connect` does not render a provider as unconnected, and Enter does
  not route to key entry as though no key existed.
- With an unreadable store and no env keys, the engine pane's offline notice names the store
  failure instead of `provider.offline.nothing`.
- An env-supplied key still works with a completely broken store: no call site reports
  `Unavailable` when the environment answered.

## 4. Resolution seam (`crates/tui/src/selection.rs`)

### 4.1 Env-first, store-second

`sources_with` currently resolves both sources unconditionally and hands the pair to `classify` /
`resolve_key_from`. Replace it with two shared primitives, so the empty-string rule and the error
stringification each live in exactly one place:

```rust
/// The env-supplied key for `provider`, if the environment supplies a usable one. An empty value
/// is treated as absent — the rule the old `classify`/`resolve_key_from` pair stated twice.
fn env_key(provider: &str, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    env_key_var(provider).and_then(env).filter(|k| !k.is_empty())
}

/// The store's answer for `provider`, with a failure reduced to one display-ready line.
fn read_store(provider: &str, store: &dyn CredentialStore) -> Result<Option<String>, String> {
    store.get(provider).map_err(|e| one_line(&format!("{e:#}")))
}
```

`{:#}` keeps anyhow's source chain on one line, matching `fetch_error` (`modal.rs:626-631`); a
backend error's outermost message alone ("failed") would hide the cause. `one_line` strips control
characters and keeps the first line: a keyring backend's error text reaches a terminal cell, and a
raw `ESC` in a cell is an escape-sequence injection. Length is bounded separately, at the modal
boundary (§6), which already owns that cap.

`one_line` does not exist yet, and the "one place" justification above only holds if it is not
written twice. It lands in a new private module, `crates/tui/src/text.rs` (`mod text;` in
`main.rs`), holding the two text-hygiene rules the TUI needs:

```rust
/// The first line of `s`, with control characters removed and the ends trimmed.
pub(crate) fn one_line(s: &str) -> String;
/// `s` truncated to `max` characters, with an ellipsis when it was truncated.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String;
```

`summarize_provider_error` (`modal.rs:608-622`) is reduced to
`truncate_chars(&one_line(message), PROVIDER_ERROR_MAX_CHARS)` — same behaviour, same tests, and
the strip rule now has exactly one implementation for `read_store` to share. `text.rs` owns the
unit tests for both helpers; `summarize_provider_error`'s existing tests stay where they are and
keep guarding the composition.

The store is consulted **only when the environment did not answer**:

```rust
fn key_status_with(provider, store, env) -> KeyStatus {
    if env_key(provider, &env).is_some() { return KeyStatus::Env; }
    match read_store(provider, store) {
        Ok(Some(_)) => KeyStatus::Keyring,
        Ok(None)    => KeyStatus::None,
        Err(_)      => KeyStatus::Unavailable,
    }
}

fn resolve_key_with(provider, store, env) -> KeyResolution {
    if let Some(k) = env_key(provider, &env) { return KeyResolution::Found(k); }
    match read_store(provider, store) {
        Ok(Some(k)) => KeyResolution::Found(k),
        Ok(None)    => KeyResolution::Missing,
        Err(e)      => KeyResolution::Unavailable(e),
    }
}
```

Short-circuiting is observably identical for every case the old code could reach — env wins in
`classify` and in `resolve_key_from` alike — and it is what makes the new `Unavailable` state
*correct* rather than noisy: a working `OPENAI_API_KEY` must not be reported as unavailable
because the keyring is down. It also removes a keyring round-trip per call.

`classify`, `resolve_key_from`, and `sources_with` are deleted; their tests are rewritten against
`env_key` and the two `_with` functions, which is where the wiring actually lives. The
`process_env` reader and the `_with` seam from #49 are kept exactly as they are.

### 4.2 The two result types

```rust
/// Where a provider's key comes from, for the `/key` listing and the `/connect` rows.
pub enum KeyStatus { Env, Keyring, None, Unavailable }

/// A resolved API key, or why there is none. Distinguishing `Missing` from `Unavailable` is the
/// point of the type: the first has a remedy the user can act on, the second does not.
pub enum KeyResolution {
    Found(String),
    Missing,
    /// The store could not be read; the payload is one display-ready line naming the cause.
    Unavailable(String),
}
```

`KeyResolution` rather than `anyhow::Result<Option<String>>`: the modal must branch three ways and
the failure text must be `Clone`/`PartialEq` to be asserted in a test. `KeyStatus` deliberately
keeps no detail — it renders as a single word in a list.

`KeyResolution::Found` holds a secret. It gets a manual `Debug` that redacts the value, following
`Selection`'s precedent (`providers/src/selection.rs:71-84`), so a `dbg!`/`{:?}` in a future call
site cannot print a key.

### 4.3 Store failures out of `build_selection`

`apply_preferences` gains an out-parameter shape rather than silent skipping:

```rust
pub fn apply_preferences(base, settings, store) -> (Selection, Vec<StoreFailure>)
pub fn build_selection(settings, store) -> (Selection, Vec<StoreFailure>)
```

with `StoreFailure { provider: String, error: String }` defined in `crate::provider` next to
`ProviderInfo` (it is a display record; `selection.rs` already depends on `provider.rs`). A
provider whose `get` fails contributes one entry and no key — the fallback behaviour is unchanged,
only now it is reported. `rebuild` folds the vector into `ProviderInfo.store_failures`.

These are `pub` items of a binary-crate module (`crates/tui/src/selection.rs` is reachable only
from `main.rs`), not of the `light_factory_tui` library, so the signature change is not a
semver event. The library surface (`lib.rs`: `credentials`, `engine_view`, `i18n`) gains only the
additive `FailingStore`.

## 5. The test double (`crates/tui/src/credentials.rs`)

```rust
/// A store whose every operation fails, for exercising the store-failure branch that `MemStore`
/// cannot reach. Models a keyring that is present but unusable (a locked wallet, no D-Bus
/// session), which fails uniformly rather than per-operation.
#[doc(hidden)]
pub struct FailingStore { message: String }
```

`#[doc(hidden)] pub` follows `MemStore` exactly, and for the same reason: `selection.rs` and
`modal.rs` live in the binary crate and can only reach a double that the library exports.
`FailingStore::new(message)` lets a test assert that the store's own words reach the user;
`Default` supplies `"credential store unavailable"`. All three trait methods return
`Err(anyhow!(...))` — `set` and `delete` too, so the double stays honest about what a down keyring
does even though only `get` is under test here.

## 6. `/models` (`crates/tui/src/modal.rs`)

`FetchFailure` gains a fourth class:

```rust
/// The credential store could not be read, so whether a key exists is unknown.
StoreUnavailable,
```

`needs_credentials()` returns `true` for it: no model id repairs a credential store, so the
credentials step — which takes no input and shows a remedy — is the right destination, and it
already carries the Ctrl+R retry that a transient D-Bus failure needs.

`fetch_model_list_inner` matches the three `KeyResolution` arms:

| Resolution | Result |
|---|---|
| `Found(k)` | fetch with `k`, exactly as today |
| `Missing` | `FetchFailure::MissingKey`, `connect.no_key` — unchanged |
| `Unavailable(e)` | `FetchFailure::StoreUnavailable`, `connect.store_unavailable` interpolating `provider` and the summarized `e` |

`App::fetch_error_message` (`app.rs:864-872`) matches `FetchFailure` exhaustively and decides
whether a class's text is wrapped or passed through. `StoreUnavailable` is **passthrough**, like
`MissingKey`: `connect.store_unavailable` already names the provider and the cause, so wrapping it
in `connect.fetch_error` would read "Couldn't fetch models: the credential store for openai could
not be read: ...".

`fetch_with_key` keeps its `Option<String>` signature and its `MissingKey` arm; the new arm is
produced before it, so the "no key" sentence still has exactly one source. The store error passes
through `summarize_provider_error` so `FetchError::message`'s documented invariant — one bounded
line — holds for the new class too. That the text is locally produced rather than remote-supplied
does not exempt it: the cap is what keeps the modal's own remedy rows on screen.

The credentials step's remedy line is class-dependent. `models.credentials_remedy` ("Use /connect,
/key {provider}, or /model <id>") is wrong for a store failure — `/connect` and `/key` both write
to the store that just failed. `ModelsStep::Credentials` gains a `remedy: String` field, populated
at construction from the class, so the step stays a pure value and the render function stays a
pure function of it. For `StoreUnavailable` the remedy is `models.store_remedy`: set the
provider's `*_API_KEY` environment variable, or unlock the credential store and retry.

## 7. `/connect` rows and the `/key` listing (`crates/tui/src/app.rs`, `modal.rs`)

`ProviderRow.connected: bool` cannot express three states, and the false branch is the harmful
one. Replace it:

```rust
pub(crate) enum RowKey { Present, Absent, Unavailable }
pub(crate) struct ProviderRow { pub(crate) id: String, pub(crate) key: RowKey }
```

- `build_provider_rows` maps `KeyStatus::{Env,Keyring} → Present`, `None → Absent`,
  `Unavailable → Unavailable`. The `ollama` special case (`LIGHT_OLLAMA=1`) is unchanged and
  yields `Present`/`Absent`.
- Render suffix: `Present → " (connected)"` (unchanged text), `Absent → ""`,
  `Unavailable → " (key store unavailable)"` (`connect.store_unavailable_row`).
- `connect_step_next`'s Enter arm treats `Unavailable` like `Present` — proceed to the model list.
  The fetch then re-reads the store and reports the failure honestly through §6. Routing to key
  entry instead would ask the user for a key they already stored and then fail to write it.

`key_status_label` gains `KeyStatus::Unavailable → provider.key.unavailable` ("unavailable").

## 8. The offline notice (`crates/tui/src/provider.rs`, `app.rs`)

`ProviderInfo` gains `store_failures: Vec<StoreFailure>`, and the notice assembly moves out of
`enter_engine` (`app.rs:332-338`) into a pure, testable method:

```rust
impl ProviderInfo {
    /// Every line the engine pane shows about how this provider was chosen: selection warnings,
    /// one line per unreadable credential store, then the offline notice if it is offline.
    pub fn notices(&self, locale: Locale) -> Vec<String>
}
```

Order: `warnings` (from the providers crate, verbatim, as today), then one
`provider.store.unavailable` line per failure ("Could not read the stored key for {provider}:
{error} — stored keys were not used"), then the offline line.

The offline line is substituted only when the substitution is true:

- `OfflineReason::NothingConfigured` **and** at least one store failure → `provider.offline.store_unavailable`
  ("Falling back to the offline provider: the credential store could not be read, so stored keys
  were unavailable"). This is the case the issue names: the store is the reason `keys` is empty.
- Every other combination → `offline_notice(locale, reason)` unchanged. A `BaseUrlRejected` or a
  `NamedProviderMissingKey` has its own real cause; the store failure is already reported on its
  own line above and must not overwrite it.

`enter_engine` becomes `self.engine_log.extend(info.notices(self.config.lang))`.

## 9. Assumptions

| # | Assumption | Rationale |
|---|---|---|
| 1 | The store is consulted only when the env has no usable key. | Observably identical outcomes; prevents a broken keyring from reporting `Unavailable` for a provider that is configured and working. Fewer keyring round-trips is a secondary benefit. |
| 2 | `KeyResolution` (a bespoke enum) rather than `anyhow::Result<Option<String>>`. | The modal must branch three ways; a `Clone + PartialEq` failure payload is what lets a test assert the sentence. An `anyhow::Error` is neither. |
| 3 | Store errors are stringified at the seam, not carried as `anyhow::Error`. | Every consumer wants display text, and stringifying once means the control-character strip cannot be forgotten by a new call site. |
| 4 | `FailingStore` fails every operation, not just `get`. | It models "the keyring is unusable", which is how a locked wallet or a dead D-Bus session actually behaves. A get-only failure is not a real state. |
| 5 | `StoreUnavailable` is a credential-class failure (`needs_credentials() == true`). | Typing a model id cannot repair it, which is the exact predicate that method answers. The credentials step's Ctrl+R covers the transient case. |
| 6 | An `Unavailable` provider row proceeds to the model list on Enter, not to key entry. | The fetch reports the real failure; key entry would ask for a key the user already stored and then fail to write it to the same store. |
| 7 | The offline-notice substitution is limited to `NothingConfigured`. | The other reasons have their own established cause; overwriting them would repeat the very error this issue is about, in the other direction. |
| 8 | No new `OfflineReason` variant in the `providers` crate. | The credential store is a TUI concept. A variant there would make `providers` name a dependency it does not have (inward flow). |
| 9 | ES strings are translated in this change, not deferred. | `i18n::tests::es_mirrors_en_exactly` is a hard gate; a deferred translation is a failing test. |

## 10. Error handling & edge cases

- **Env key present, store broken.** No store read at all; `KeyStatus::Env`,
  `KeyResolution::Found`, `/models` fetches normally, no notice. §4.1, criterion 6.
- **Store broken, `/connect` → provider row → Enter.** Model list step → fetch →
  `StoreUnavailable` credentials step naming the store error. No key-entry prompt.
- **Store broken for one provider only.** `KeyringStore` is per-entry, so a partial failure is
  representable: `build_selection` reports one `StoreFailure` and still inserts the other
  providers' keys. `notices` prints one line per failed provider.
- **Store recovers mid-session.** Nothing is cached, so the next `/key`, `/connect`, or `/models`
  reads through and reports the true state. Ctrl+R on the credentials step re-runs the fetch.
- **A multi-line or control-character-bearing backend error.** `read_store`'s `one_line` strips
  control characters and keeps the first line; `summarize_provider_error` caps the length on the
  modal path.
- **A store error that embeds the key.** Not a state `KeyringStore` can produce (`keyring` errors
  do not carry the secret), but `KeyResolution`'s redacting `Debug` and the existing rule that
  secrets never reach logs both hold; the error text is rendered, never logged.
- **`ollama` / `local`.** `env_key_var` returns `None`, so `env_key` is `None` and the store is
  consulted for a provider that never has an entry: `Ok(None) → None`, unchanged. A broken store
  makes `key_status("ollama")` report `Unavailable`, but `build_provider_rows` decides `ollama`
  from `LIGHT_OLLAMA` and never calls `key_status` for it, and `/key`'s listing covers
  `REMOTE_IDS` only.

## 11. Risks & open questions

- **`ProviderRow`'s field change touches every construction site and its tests.** Contained to
  `app.rs` and `modal.rs`, both in the binary crate; the compiler finds all of them. The
  alternative — a second bool — makes an impossible state representable.
- **`ModelsStep::Credentials` gains a field**, so every construction and pattern match in
  `modal.rs` moves. Same containment; the compiler is exhaustive here.
- **`ProviderInfo` gains a field**, so every struct-literal construction moves: `selection.rs:139`,
  the `provider.rs` test helper (`74-82`), and several `app.rs` test helpers. Compiler-exhaustive,
  same as the two above.
- **The new `/connect` row suffix is not covered by a width test.**
  `every_footer_fits_the_popup_in_both_locales` (`i18n.rs`) gates `*.footer` keys only, so a
  too-long ES `connect.store_unavailable_row` would silently truncate inside the 58-column popup.
  Mitigated by keeping the string short and adding a test that the longest provider id plus the
  suffix fits `INNER_WIDTH` in both locales.
- **The credentials step's remedy becomes data rather than a constant.** A future class that
  forgets to set it would render an empty remedy line. Mitigated by building the step from the
  class in one place, with a test per class asserting a non-empty remedy.
- **Open:** whether `/key <provider>` should refuse to prompt when the store is unreadable rather
  than accepting a key and failing on write. Out of scope — `store.set`'s error is already
  surfaced, so the user is told the truth, just one keystroke later. Worth a follow-up issue if it
  proves annoying in practice.
