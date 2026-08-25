# Env-supplied API keys follow the interactive trim rule — design

> **Status:** DRAFT — an env-supplied API key is trimmed before use, and a whitespace-only value is treated as absent, matching the interactive `/key` and connect-modal paths.

> **Implements:** https://github.com/savvagent/light-factory/issues/52
> **Follows:** #49 (whose test-coverage review surfaced the defect), #68 (which consolidated the TUI's presence rule into a single seam this change then extends)

## 1. Brief

A key inherited from the environment bypasses the trim rule the interactive paths already apply:

- `selection_from_env` (`crates/providers/src/selection.rs:549`) filters an env-supplied key on
  `!key.is_empty()` only before inserting it into `Selection.keys`.
- `env_key` (`crates/tui/src/selection.rs:36`) filters on `!k.is_empty()` only. Since #68 this is
  the **single** TUI seam for "does the environment supply a usable key" — both `key_status_with`
  and `resolve_key_with` route through it.

So `export OPENAI_API_KEY=" "` — or, more realistically, a value picked up with a trailing newline
from `$(cat keyfile)` — yields a whitespace-padded or whitespace-only key. `key_status` reports
`Env`, and `fetch_model_list` sends it verbatim, producing an opaque 401 instead of the honest
"no key configured" state.

The interactive paths already enforce the correct rule: `submit_key_entry`
(`crates/tui/src/app.rs:465`) stores `self.key_input.trim().to_string()` and refuses an
empty-after-trim value; the connect modal's key entry gates on `!input.trim().is_empty()`
(`crates/tui/src/modal.rs:539`). A typed key therefore cannot be whitespace-only; an inherited one
can.

### Premise corrections

Recorded explicitly, because the first draft of this spec got one half wrong:

1. **The original read of `crates/tui/src/selection.rs` was taken from a pre-#68 checkout**
   (local `master` at 017b8ff, behind `origin/master` at f2e87be). That draft cited `classify`
   (`:25`) and `resolve_key_from` (`:83`) as two separate fix points. Neither function exists on
   the branch base: #68 replaced them with `KeyStatus::Unavailable` / `KeyResolution` machinery
   and funneled all env-presence decisions through `env_key`. Corrected against f2e87be: the TUI
   fix is **one expression in `env_key`**, which is strictly better — one source of truth, and
   #68's store-failure-vs-miss distinction is preserved untouched because a whitespace-only env
   value simply makes the environment answer "nothing usable", letting the existing fall-through
   to the store proceed.
2. **All other premises survive verification on f2e87be**: the three untrimmed filter sites are
   exactly as the issue states (`env_key` covers two of the issue's three named locations;
   `selection_from_env` the third), and the interactive trim rule is live at the lines above.
3. One refinement to the issue's wording: since #68, `resolve_key_from`'s old doc comment ("an
   empty env value is treated as absent") moved onto `env_key` — this change generalizes that
   documented rule from "empty" to "empty after trimming", which is what the doc always meant.

## 2. Scope

**In:**

- One shared notion of "a usable env-supplied key": surrounding Unicode whitespace stripped,
  empty-after-trim treated as absent — applied at the two crates' single entry points for
  env-supplied keys: `crates/providers/src/selection.rs`'s `selection_from_env` (via a pure
  helper) and `crates/tui/src/selection.rs`'s `env_key`.
- The resolved key handed downstream (`fetch_model_list`, provider construction) is the
  **trimmed** value, not merely filtered.
- Pure, injectable-input helpers so both crates' rules stay testable without mutating the
  process environment (each crate already states this principle in its module docs).
- Wiring-level tests covering the whitespace-only and trailing-newline cases in **both**
  crates.
- Doc-comment updates where the contract changes wording: `Selection.keys`
  ("non-empty" → "trimmed, non-empty") and `env_key`'s rule comment.

**Out:**

- The other untrimmed env reads in `selection_from_env`: `LIGHT_OLLAMA_MODEL`,
  `LIGHT_<P>_MODEL`, and the `*_BASE_URL` overrides share the same raw-value pattern for
  non-key inputs. No defect is reported against them; widening scope here would touch provider
  construction and base-url validation for no reported failure. Filed as a follow-up issue
  instead (Stop & Escalate rule 11).
- Persisted settings and stored keys — they are written by the interactive paths, which
  already trim at entry (`app.rs:465`).
- Any server/auth-surface, wire-type, or i18n change. No user-facing string changes: the
  offline reason and `/key` listing strings already say the right thing once the classification
  is honest.
- #68's machinery: `KeyResolution`, `KeyStatus::Unavailable`, `read_store`, `apply_preferences`'
  failure reporting — all untouched; they compose with this change as described in §4.3.

## 3. Assumptions

1. **Trim means `str::trim` (Unicode whitespace).** *Rationale:* it is exactly what the
   interactive path uses (`app.rs:465`), so "same rule as the interactive paths" is literal:
   one definition of whitespace across all entry modes. It covers the realistic failures
   (`\n` from `$(cat keyfile)`, `\r\n` from Windows-edited dotenv files, spaces).
2. **The trimmed value replaces the raw value downstream**, not just the presence decision.
   *Rationale:* AC 1 says "trimmed before use". A trailing newline inside a Bearer header is
   the exact reported failure mode; filtering-only would leave it reachable.
3. **Only surrounding whitespace is stripped.** A key containing internal whitespace
   (`"abc def"`) survives verbatim. *Rationale:* trimming is a transport-hygiene fix, not a
   validation policy; internal content is the operator's business.
4. **No semver impact, no version bump.** New helpers are private; no public signature,
   type, or wire change (Non-Negotiable Rule 6 applies only to public/wire surfaces).
   Behavior of public functions is refined toward what their doc comments already claim.
5. **Env-var reads remain injectable in tests.** `crates/providers` gains a pure
   `keys_from(read)` seam mirroring the pattern `crates/tui`'s `env_key(provider, env)` already
   establishes, so neither crate's new tests call `std::env::set_var` (unsafe in edition 2024
   and racy across the binary's tests — precedent: the archived models-fetch-bounds spec,
   Risk 2).
6. **Classification keeps classifying by source, for free.** A padded-but-real env key still
   reports `KeyStatus::Env` (it *is* from the env, and `env_key` answers usable); a
   whitespace-only one falls through to the store, so `Keyring`/`None`/`Unavailable` are all
   still reachable and none of #68's distinctions are disturbed. *Rationale:* the enum describes
   provenance; the fix only stops lying about usability.

## 4. Design

### §1 `crates/providers/src/selection.rs`

Two private helpers, used by `selection_from_env`:

```rust
/// An env-supplied key is usable only after stripping surrounding whitespace (a trailing
/// newline from `$(cat keyfile)` must not reach a Bearer header); a whitespace-only value is
/// treated as absent.
fn normalize_env_key(raw: String) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Collect the env-supplied keys by provider id, applying [`normalize_env_key`] uniformly.
/// Pure: `read` supplies variable values so the mapping (`env_key_var` naming → normalization →
/// map insertion) is testable without the process env.
fn keys_from(read: impl Fn(&str) -> Option<String>) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    for id in ["anthropic", "openai", "gemini", "deepseek"] {
        if let Some(var) = env_key_var(id)
            && let Some(key) = read(var).and_then(normalize_env_key)
        {
            keys.insert(id.to_string(), key);
        }
    }
    keys
}
```

`selection_from_env`'s key loop collapses to `let mut keys = keys_from(|var| std::env::var(var).ok());`.
The `Selection.keys` field doc becomes "Resolved, trimmed non-empty API keys by provider id".

### §2 `crates/tui/src/selection.rs`

One expression at the seam #68 created:

```rust
fn env_key(provider: &str, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    env_key_var(provider)
        .and_then(env)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}
```

`env_key`'s doc updates from "An empty value is treated as absent…" to the whitespace rule. Both
consumers inherit the rule without further edit:

- `key_status_with`: whitespace-only env → `env_key` answers `None` → store consulted →
  `Keyring`/`None`/`Unavailable` exactly as today for an unset variable.
- `resolve_key_with`: whitespace-only env → falls through to the store; a padded-real env value
  resolves to `KeyResolution::Found(<trimmed>)`.

### §3 Downstream effects (no code beyond §1–§2)

- `fetch_model_list` resolves through `resolve_key`, so the header carries the trimmed value;
  a whitespace-only env var now yields the honest no-key outcome instead of an opaque 401 —
  including #68's `Unavailable` text when the store is also unreadable (the whitespace-only env
  correctly does not mask a broken store).
- Provider construction via `rebuild` inherits `keys_from`'s filtering, so a whitespace-only
  `OPENAI_API_KEY` no longer satisfies `LIGHT_REMOTE_PROVIDER=openai`; the existing
  `NamedProviderMissingKey` warning fires with its actionable text — the correct, already-built
  behavior for "named but unusable".
- The `/key` listing reports `Keyring`/`None` truthfully for whitespace-only env values.

### §4 Testing (all next to the code, offline-deterministic, no process-env mutation)

| Crate | Test | Pins |
|---|---|---|
| providers | `normalize_env_key_*` unit cases | padded value → trimmed; clean value unchanged; whitespace-only/empty → `None`; internal whitespace preserved |
| providers | `keys_from_*` wiring cases | unset var → absent; whitespace-only → absent; `"sk\n"`-style trailing newline → present **and trimmed** |
| tui | rename/extend `env_key_treats_an_empty_value_as_absent` → `…_treats_a_blank_value_as_absent` | whitespace-only → `None`; padded-real `"  sk-env  "` → `Some("sk-env")` (trimmed, not just accepted); trailing-newline → trimmed; empty → `None` (pre-existing subset) |
| tui | extend `key_status_with_classifies_every_wiring_outcome` | whitespace-only env + stored key → `Keyring`; whitespace-only env alone → `None`; padded-real env stays `Env` |
| tui | rename/extend `resolve_key_with_treats_an_empty_env_value_as_absent` → `…_blank…` | whitespace-only → keyring fallback; trailing-newline env resolves `Found("sk-env")` |

## 5. Goal & success criteria

Make an env-inherited credential obey the same hygiene as a typed one, so selection never
sends a whitespace-padded key and never claims a whitespace-only key is configured.

- A whitespace-only `*_API_KEY` behaves exactly like an unset one in status, resolution, and
  selection — asserted by tests at the wiring level.
- A padded real key is sent trimmed — asserted by test.
- The rule is applied consistently across the two crates — reviewed in this PR, enforced by
  symmetric tests.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -D warnings`,
  `cargo fmt --all --check` clean.

## 6. Error handling & edge cases

| Case | Behaviour |
|---|---|
| Whitespace-only env key, stored key present | `Keyring` status; stored key resolved and used |
| Whitespace-only env key, nothing else | `Missing`; offline local with `NothingConfigured` — honest, not an opaque 401 |
| Whitespace-only env key, store broken | `Unavailable` with the store's cause (#68's distinction intact — the bad env value neither masks nor mimics it) |
| Whitespace-only env key, `LIGHT_REMOTE_PROVIDER` names that provider | `NamedProviderMissingKey` warning (existing path), never misrouting to another provider's key |
| Padded real key (`" sk\n"`) | Used as `"sk"` everywhere (headers, provider construction) |
| Empty env value | Unchanged from today: treated as absent (a strict subset of the new rule) |
| Non-UTF-8 env value | Unchanged: `std::env::var` errors → absent |

## 7. Risks & open questions

1. **An operator who deliberately pads a meaningful key breaks.** Argued impossible in
   practice: bearer-type API keys do not carry significant surrounding whitespace, the
   interactive paths have refused to preserve it since they existed, and padding reaches
   providers today only by accident (the bug being fixed). Accepted deliberately.
2. **Behavioral change is observable**: a user whose session silently "worked" via a
   whitespace-only env key plus a real stored key will now use the stored key (arguably the
   first time their real credential is used), and one with *only* a whitespace-only env key
   moves from opaque-401-at-ask-time to offline-with-a-reason-at-startup. Both are the issue's
   requested outcomes.
3. **Same untrimmed pattern in model/base-url env vars** — follow-up issue, explicitly out of
   scope (see §2).

## 8. Follow-ups (filed, not in this PR)

- Trim `LIGHT_<P>_MODEL` / `LIGHT_OLLAMA_MODEL` / `*_BASE_URL` env values the same way (a
  trailing newline there produces a bogus model id or URL override today). Issue number to be
  referenced from the PR body when filed.
