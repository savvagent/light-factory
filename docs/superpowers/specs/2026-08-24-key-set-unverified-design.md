# `/key` stores without verifying — design

> **Status:** DRAFT — `/key` stops reporting a keyring write as a working credential: the immediate status says *stored, not yet verified*, and an off-loop probe upgrades it to accepted, rejected, or unreachable.

> **Implements:** https://github.com/savvagent/light-factory/issues/61
> **Follows:** https://github.com/savvagent/light-factory/issues/47 (the verified/unverified convention this mirrors), https://github.com/savvagent/light-factory/issues/55 (`FetchFailure` classes reused here)

## 1. Brief

`submit_key_entry` (`crates/tui/src/app.rs:466`) reports success on the keyring write alone:

```rust
match self.store.set(&provider, &key) {
    Ok(()) => {
        self.rebuild_provider();
        self.status = self.t_with("status.key_set", &[("provider", &provider)]);
    }
```

`status.key_set` is `"API key saved for {provider}"` (`i18n.rs:207`). `store.set` returning `Ok` means
one thing: the OS keyring accepted a string under that entry name. It says nothing about whether the
string is a credential the provider will accept. A truncated paste, a key with a trailing newline,
an `sk-ant-…` key typed at the `openai` prompt, or a revoked key all take the `Ok` arm and all
produce the same unqualified sentence.

The failure shape is #47's, one command over. #55 taught the `/models` modal to route a rejected
credential to a step recommending `/key <provider>`, so the corrected journey now reads:

`/models` → "openai rejected the credential" → `/key openai` → **"API key saved for openai"** → `/ask`
→ the same auth failure.

The `/models` modal no longer lies; the command it delegates to does. The inconsistency is sharp
because #55 *already* introduced the honest form for the sibling case — `status.model_set_unverified`,
`"Model set to {model} — not verified against {provider}"` (`i18n.rs:195`) — and left the credential
path claiming unqualified success.

The machinery to do better already exists and is already used by the neighbouring path.
`handle_modal_key` (`app.rs:881`) writes the key typed into the `/connect` modal's `KeyEntry` step
and then transitions to a fetching `ModelList`, which spawns `fetch_model_list` with that key as
`key_override` (the write is at `app.rs:918`, the spawn at `app.rs:949`, `begin_model_fetch` at `app.rs:704`). So `/connect`'s key entry is verified by construction and
`/key`'s is not, even though both write the same keyring entry through the same `CredentialStore`.

## 2. Scope

**In:**
- `submit_key_entry` (`app.rs:466`) and the helpers it calls directly.
- A new immediate status that does not read as verification, replacing `status.key_set`.
- An off-loop probe of the key just stored, reusing `crate::modal::fetch_model_list` unmodified,
  delivered back through a new `UiEvent` variant and handled by one new `App` method.
- A generation counter + tracked `JoinHandle` for that probe, so a stale result cannot overwrite a
  newer status and two probes can never be in flight carrying two keys.
- EN + ES catalog entries for every new string; removal of the now-unused `status.key_set`.

**Out (deliberately, because PR #68 is in flight over the same file):**
- `enter_models`, `handle_models_fetched`, `handle_connect_models`, `begin_model_fetch`,
  `fetch_error_message`, `credentials_remedy`, `build_provider_rows`, `key_status_label`,
  `enter_engine`.
- Anything in `selection.rs`, `provider.rs`, `text.rs`.
- `FetchFailure`, `FetchError`, `ModelsStep`, `ConnectStep`, `ProviderRow`, `ProviderInfo` — used,
  never changed.
- `crates/modal.rs`'s `fetch_model_list` — **called**, not modified.
- The `/connect` modal's own `KeyEntry` write path (`app.rs:900`–`app.rs:927`). It already probes;
  changing it is #68's territory.
- The env-shadowing sibling issue (a keyring key stored under an exported `OPENAI_API_KEY` is
  written but never used). See Assumption 4 and Follow-ups.
- `clear_key` — deleting a key needs no verification.

## 3. Premise corrections

1. **"the same `fetch_model_list` probe `/connect` already runs (it exists and is async off-loop)"**
   is right about the function and wrong about the plumbing. `begin_model_fetch` is
   *modal*-scoped: it claims its nonce from `ModalHost::next_fetch_nonce()` and parks its handle in
   `ModalHost::track_fetch` (`app.rs:705`, `app.rs:727`), and its result is routed by a `FetchSink`
   that only names modal steps. `/key` is not a modal — it is `Mode::Key`, a full screen with
   `key_target`/`key_input`/`key_return` fields on `App` (`app.rs:116`). Reusing `begin_model_fetch`
   would mean either giving `ModalHost` a fetch it has no modal for (a lie about its invariant: its
   nonce is what makes "a result that outlives its modal" discardable) or adding a `FetchSink`
   variant — and `FetchSink` is modal machinery inside `modal.rs`, which is off-limits. So the probe
   gets its own two-field generation/handle pair on `App`. `fetch_model_list` itself is reused
   verbatim.

2. **"a probe failure must not un-store a key the user may legitimately want stored"** — correct, and
   it costs nothing: this design never calls `store.delete`. The floor is stronger than "don't
   un-store on transport failure": the key is never un-stored on *any* probe outcome, including a
   clean 401. A rejected key can be the right key against a provider having an outage on its auth
   edge, and the user can re-run `/key` or `/key <provider> clear` themselves. The status carries the
   news; the store carries the user's intent.

3. **The `/models` guard is a stale snapshot.** `provider_info.offline` is refreshed only by
   `rebuild_provider()`, while `resolve_key` re-reads live — recorded in #47's spec §1 and unchanged
   here. It is why `FetchFailure::MissingKey` is reachable from a probe fired one line after a
   successful `store.set`: `resolve_key` is not consulted (see Assumption 4), but the class exists
   and must be handled rather than assumed impossible.

## 4. Goal & success criteria

`/key <provider>` must never tell the user a credential works when nothing has tried to use it, and
should tell them within seconds when it does not.

1. Immediately after a successful `store.set`, the status names storage and explicitly disclaims
   verification. No code path produces an unqualified "saved/set" sentence for a key.
2. The probe runs off the UI loop: `submit_key_entry` returns before any network I/O, the loop keeps
   drawing, and the result arrives as a `UiEvent`.
3. A provider that refuses the credential (`FetchFailure::MissingKey` | `Auth`, i.e.
   `FetchFailure::needs_credentials()`) produces a status distinct from both the stored-unverified
   status and the accepted status.
4. A transport failure (`FetchFailure::Fetch` — DNS, TLS, timeout, 5xx, a panic in the fetch)
   produces a third, distinct status that does not accuse the key.
5. The key stays in the store after every probe outcome. Asserted by test against `MemStore`.
6. No test reaches the network, and every test yields identical results with `OPENAI_API_KEY`
   exported.
7. The key never appears in a status line, an error, a log, or `Debug` output — including the
   provider's own error text, which is not interpolated into any `/key` status.

## 5. The decision: honest wording, or a live probe?

The issue offers a floor ("at minimum the status must not read as verification") and a ceiling (run
the probe). **This design does both, in that order, and the ordering is the design.**

**Why not the floor alone.** The floor makes the message true; it does not make the journey work. The
user in the issue's transcript arrived at `/key openai` *because* `/models` told them the credential
was rejected. Answering "stored, not yet verified" hands them back the same open question they came
with, and the only way to close it is to run `/models` again — one more command, for information the
TUI could have volunteered. Worse, the floor alone preserves a real asymmetry: type the key into
`/connect` and it is verified by construction; type the same key into `/key` and it is not. Two
commands writing the same keyring entry should not disagree about whether they checked it.

**Why not the probe alone.** A probe is a promise about a network round-trip, and every failure mode
of that round-trip is a failure mode of the promise: the process can be killed, the task can be
aborted by the next `/key`, the fetch can hang until reqwest's 15s deadline, the runtime can be
starved. If the *only* honest status is the one the probe produces, then every one of those cases
leaves the user reading "API key saved for openai" again. The floor must not be contingent on the
probe.

**So the floor is set unconditionally, at store time, and the probe only ever refines it.** The
sequence in `submit_key_entry` is: write → `status = key_stored_unverified` → spawn probe. The
"not yet verified" status is what the user sees if the probe never lands, and *that is correct* — if
nothing came back, nothing was verified. The probe's three outcomes each replace it with a sharper
sentence. The acceptance floor (§4.1) is therefore an invariant of the store path, not a consequence
of the probe succeeding, and removing the probe later would degrade the feature without regressing
the bug.

This also picks the honest verb for each outcome. "Accepted the API key" is what a successful model
list actually proves; "openai is ready" is not (see Assumption 4). "Rejected" is what a 401/403
proves. "Couldn't reach openai to verify" is what a timeout proves. None of the three is a synonym
for the others, which is criterion §4.3–4.4.

## 6. Shape

### 6.1 `App` state (two new fields)

```rust
/// Generation of the most recent `/key` probe. Bumped before every spawn, so a result whose
/// nonce is stale — a second `/key` submitted while the first was in flight — is discarded
/// rather than overwriting a newer status. Starts at 0 and is incremented before use, so no
/// live probe ever carries nonce 0 and a zero-valued event can never match.
key_probe_nonce: u64,
/// The in-flight `/key` probe, if any. Held so the next probe can abort it: the request carries
/// an API key in its headers, and two probes in flight means two keys in flight.
key_probe: Option<tokio::task::JoinHandle<()>>,
```

Mirrors `ModalHost`'s nonce+handle pair and its rationale, without reaching into it.

### 6.2 `UiEvent::KeyProbed`

```rust
KeyProbed {
    nonce: u64,
    provider: String,
    result: Result<(), FetchError>,
},
```

The model list is discarded at the boundary (`.map(|_| ())`): `/key` has no use for it, and not
carrying it keeps a provider-supplied payload out of a path that has no renderer for it. `FetchError`
is carried whole because `FetchFailure::needs_credentials()` is the predicate that splits the two
error statuses — the classification line #55 already drew, reused rather than re-derived.

`UiEvent` is `pub` but lives in `crates/tui/src/app.rs`, a binary-crate module; `crates/tui/src/lib.rs`
exposes only `credentials`, `engine_view`, `i18n`. The variant is additive and reaches no public API,
so no `Cargo.toml` bump (Non-Negotiable Rule 6).

### 6.3 `submit_key_entry`

Only the `Ok` arm changes:

```rust
Ok(()) => {
    self.rebuild_provider();
    self.status = self.t_with("status.key_stored_unverified", &[("provider", &provider)]);
    self.begin_key_probe(provider, key);
}
```

`key` is moved into the probe rather than re-read, for the reason in Assumption 4. The `Err` arm,
the empty-input arm, and the `key_target`/`key_input`/`mode` resets above them are untouched.

### 6.4 `next_key_probe_nonce` + `begin_key_probe`

```rust
/// Claim the next probe generation, cancelling and invalidating whatever probe was in flight.
/// Bumping and aborting are one operation for the reason `ModalHost::next_fetch_nonce` gives:
/// they are the same event, so no call site can perform one and forget the other.
fn next_key_probe_nonce(&mut self) -> u64 {
    self.key_probe_nonce = self.key_probe_nonce.wrapping_add(1);
    if let Some(previous) = self.key_probe.take() {
        previous.abort();
    }
    self.key_probe_nonce
}

fn begin_key_probe(&mut self, provider: String, key: String) {
    let nonce = self.next_key_probe_nonce();
    let events = self.events.clone();
    let store = self.store.clone();
    let lang = self.config.lang;
    self.key_probe = Some(tokio::spawn(async move {
        let result = fetch_model_list(&provider, Some(key), store.as_ref(), lang)
            .await
            .map(|_| ());
        let _ = events.send(UiEvent::KeyProbed { nonce, provider, result });
    }));
}
```

`Some(key)` — never `None`. See Assumption 4. `wrapping_add` because a `u64` counter of `/key`
submissions cannot realistically wrap, and a panic there would be a worse outcome than a reused
nonce.

### 6.5 `handle_key_probed`

```rust
fn handle_key_probed(&mut self, nonce: u64, provider: String, result: Result<(), FetchError>) {
    if nonce != self.key_probe_nonce {
        return;
    }
    self.key_probe = None;
    let key = match &result {
        Ok(()) => "status.key_verified",
        Err(e) if e.class.needs_credentials() => "status.key_rejected",
        Err(_) => "status.key_unreachable",
    };
    self.status = self.t_with(key, &[("provider", &provider)]);
}
```

`e.message` is deliberately not interpolated. It is provider-supplied text; the `/key` statuses are
one bounded sentence each and carry only the provider id, which is not a secret. §4.7.

Dispatched from the event loop's `match` alongside `ModelsFetched`.

### 6.6 Strings (`crates/tui/src/i18n.rs`, both catalogs, same commit)

| Key | EN | ES |
|---|---|---|
| `status.key_stored_unverified` | `API key stored for {provider} — not yet verified` | `Clave de API guardada para {provider} — aún sin verificar` |
| `status.key_verified` | `{provider} accepted the API key` | `{provider} aceptó la clave de API` |
| `status.key_rejected` | `{provider} rejected the API key — it is still stored` | `{provider} rechazó la clave de API — sigue guardada` |
| `status.key_unreachable` | `Couldn't reach {provider} to verify the API key — it is stored` | `No se pudo contactar con {provider} — la clave sigue guardada` |

The ES `status.key_unreachable` is deliberately not a literal translation. The status renders as one
unwrapped `Paragraph` in the title row behind a 17-column `" light-factory · "` prefix
(`app.rs:1383`); a literal ES translation totals 95 columns with `{provider}` = `openai` and would
lose its trailing reassurance at 80 columns, while the EN string totals 75. Accented Spanish letters
are written literally and em dashes as `\u{2014}`, matching the catalogs' existing convention.

`status.key_set` is **removed** from both catalogs: `submit_key_entry:482` was its only reference
(verified by `grep -rn "status.key_set" crates/`). The em dashes match `status.model_set_unverified`'s
existing `\u{2014}` form. None of these is a `*.footer`, so the 58-column cap does not apply; the
`es_mirrors_en_exactly` parity test does, and both catalogs change in the same commit.

## 7. Error handling & edge cases

| Case | Behaviour |
|---|---|
| Empty input | Unchanged — `status.key_empty`, no write, no probe. |
| `store.set` fails | Unchanged — `status.key_failed` on `self.error`, **no probe** (nothing was stored). |
| `key_target` is `None` | Unchanged — returns to `key_return` with no status. |
| Provider does not take a key | Unreachable: `begin_key_entry` gates on `takes_key` (`selection.rs:19`), so `key_target` is only ever `openai`/`anthropic`/`gemini`/`deepseek`. |
| Second `/key` while a probe is in flight | Nonce bumped, previous handle aborted; the stale result (if it still sends) fails the nonce check and is dropped. |
| Probe outlives the `Mode::Key` screen | Expected — the status line is visible from every mode, so a late result lands somewhere the user can read it. |
| Fetch panics | `fetch_model_list`'s `guard_panic` already yields `FetchFailure::Fetch` ⇒ `status.key_unreachable`. No new panic surface. |
| Probe never completes | The 15s reqwest deadline inside `fetch_model_list` bounds it; until then the user reads the honest `status.key_stored_unverified`. |
| App exits mid-probe | Handle dropped with the runtime; the floor status is the last thing written. |
| `submit_key_entry` called outside a tokio runtime | Panics, as any `tokio::spawn` does. The only production caller is the async event loop, and the `/key` submit path has no existing test, so nothing breaks — but a future plain `#[test]` on it would panic. The repo already documents this hazard at `app.rs:3090`. |

## 8. Security properties

- **The key is never rendered, logged, or interpolated.** It moves from `key_input` into `store.set`
  and into the probe task, and nowhere else. Every `/key` status takes `{provider}` and nothing else.
- **The provider's error text does not reach the status line** (§6.5), so a provider that echoes part
  of a submitted credential in an error body cannot surface it through this path.
- **At most one probe in flight**, because each spawn aborts its predecessor (§6.4) — the same
  reasoning `handle_modal_key` records for the connect fetch ("the request — and the API key in its
  headers — would otherwise outlive the 'Esc: cancel' the footer promises").
- **`FetchError`'s `Debug` carries `class` + `message`, never a key** — unchanged from #55.
- No auth-spine surface is touched: no registration, login, device grant, `SecretCipher`, session, or
  token path appears in this change.

## 9. Testing

All in `crates/tui/src/app.rs`'s `#[cfg(test)] mod tests`, using `test_app_with_store(MemStore)`.

**No network, no ambient env.** The probe only ever targets a network provider (`takes_key` excludes
`ollama`/`local`), so the network is kept out by *never polling the spawned task*: the
`submit_key_entry` tests are `#[tokio::test]` on the default current-thread runtime and their bodies
never `.await`, so the runtime driver never runs between the spawn and the end of the test and the
task is dropped unpolled at shutdown. Any test that *does* await — test 9 — must therefore not go
through `submit_key_entry`; it exercises `next_key_probe_nonce` directly, which spawns nothing.
`handle_key_probed` is tested directly with synthetic `FetchError` values and touches no runtime at
all. Nothing on any of these paths calls `resolve_key`, so `OPENAI_API_KEY` in the developer's
environment is irrelevant to every assertion — `OPENAI_API_KEY=sk-test cargo test -p
light-factory-tui` must be identical.

1. `submitting_a_key_reports_it_as_unverified` — status is `status.key_stored_unverified`, and is
   *not* the old "saved" sentence.
2. `submitting_a_key_stores_it_and_starts_a_probe` — `store.get("openai")` returns the key;
   `key_probe_nonce != 0`; `key_probe.is_some()`.
3. `a_failed_keyring_write_starts_no_probe` — a failing store double leaves `key_probe.is_none()`
   and `key_probe_nonce == 0`.
4. `an_accepted_key_reports_verification` — `handle_key_probed(nonce, "openai", Ok(()))` ⇒
   `status.key_verified`.
5. `a_rejected_key_reports_rejection_and_stays_stored` — `Err(FetchError { class: Auth, .. })` ⇒
   `status.key_rejected`, and `store.get("openai")` still returns the key.
6. `a_missing_key_class_reports_rejection` — `MissingKey` takes the same arm
   (`needs_credentials()`), pinning the shared branch.
7. `an_unreachable_provider_does_not_accuse_the_key` — `Err(FetchError { class: Fetch, .. })` ⇒
   `status.key_unreachable`, distinct from both other error statuses, and the key stays stored.
8. `a_stale_probe_result_is_discarded` — a result carrying `nonce - 1` leaves the status untouched.
9. `claiming_a_probe_nonce_aborts_the_previous_probe` — `#[tokio::test]` using the existing
   `pending_task()` (`app.rs:2675`) and `settle()` (`app.rs:2681`) helpers: park the pending handle
   in `key_probe`, call `next_key_probe_nonce()` **directly**, `settle()`, assert
   `probe.is_finished()`. It must not go through `submit_key_entry`, because `settle()`'s yields
   would then poll a real probe task and it would reach the network — which is the whole reason
   §6.4 splits the nonce claim out as its own method. Mirrors
   `starting_a_models_fetch_aborts_the_previous_one`. **Belt and braces:** if a future revision ever
   does route this test through a spawn, it must name a provider `list_models` rejects without I/O
   (`"local"` — `providers/src/models.rs:152` bails with "unknown provider" before any request),
   which is exactly what the two existing precedent tests do and say. Note that `submit_key_entry`
   does *not* re-check `takes_key`, so a test may set `key_target` to `"local"` directly even though
   the product path cannot.
10. i18n: the existing `es_mirrors_en_exactly` covers the four new keys and the removed one.

Test 3 — and **only** test 3 — uses the `FailingStore` double that already exists at `app.rs:2037`
(a `CredentialStore` whose `set` returns `Err` and whose `get` always returns `None`). No new double
is needed. Tests 5 and 7 assert the key *survives* via `store.get`, so they must run against
`MemStore`; routing them through `FailingStore` would make them vacuous.

## 10. Assumptions

1. **The probe is a model-list fetch, not a dedicated auth endpoint.** `fetch_model_list` is what the
   codebase has, what `/connect` uses, and what #55 already classifies. A provider that lists models
   without a valid key would produce a false "accepted" — none of the four supported providers does.
2. **One probe per submission, no retry.** A transport failure reports "couldn't verify" and stops;
   the user's retry is re-running `/key` or `/models`. Adding `Ctrl+R` to a full-screen mode with no
   footer for it is out of proportion here.
3. **The probe is not cancelled by mode changes or session loss.** `dismiss_modals` tears down modal
   fetches; `/key`'s probe is deliberately allowed to complete so its answer reaches the status line
   from whatever screen the user moved to. It is bounded by reqwest's deadline and aborted by the
   next `/key`.
4. **The probe verifies the key the user just typed, not the key the app would resolve.** Passing
   `None` would let `fetch_model_list` call `resolve_key`, which prefers an exported
   `OPENAI_API_KEY` over the keyring (`resolve_key_from`, `selection.rs:82`, reached via `resolve_key`, `selection.rs:102`) — so a developer with the
   env var set would be told their newly typed keyring key was "accepted" on the strength of a
   different credential entirely. `Some(key)` makes the status a statement about the string the user
   submitted, which is what they asked about. The residual gap — an accepted keyring key that env
   shadowing means `/ask` will never use — is the sibling env-shadowing issue, and is why §6.6's
   accepted string says "accepted the API key" rather than "{provider} is ready".
5. **A `MissingKey` class from a probe means rejection, not absence.** The probe supplies the key
   explicitly, so `fetch_with_key`'s `None` arm is unreachable from this call site. Routing
   `MissingKey` through `needs_credentials()` alongside `Auth` costs nothing and keeps the branch
   from depending on that unreachability.
6. **`status.key_set` has no external consumer.** It is a TUI catalog key in a binary crate; removal
   is not a public-API change.

## 11. Risks & open questions

- **Rebase against PR #68.** #68 rewrites much of `app.rs`. The overlap is deliberately narrow: the
  `Ok` arm of `submit_key_entry`, two `App` fields, one `UiEvent` variant, one event-loop arm, two
  new methods, and the catalogs. The out-of-scope list in §2 is the mitigation; conflicts should be
  confined to the `UiEvent` enum body, the `App` field block, and the event-loop `match`.
- **A user who submits a key and immediately quits never learns the outcome.** Accepted: the floor
  status is honest, and the next `/models` re-checks.
- **Four statuses on one command is more surface than one.** They are mutually exclusive, class-
  derived, and each names a different fact; collapsing "rejected" into "unverified" would re-lose
  exactly the distinction #55 established.
- **Providers can rate-limit model listing.** A 429 classifies as `Fetch` ⇒ "couldn't reach … to
  verify", which is true and does not accuse the key.

## 12. Follow-ups (not this change)

- Env-shadowed keyring keys (the sibling issue): `/key` should say when the key it just stored will
  be shadowed by an exported variable.
- Unifying `/connect`'s `KeyEntry` write with `submit_key_entry` so one function owns "store a key
  and say what happened". Blocked on #68 landing.
