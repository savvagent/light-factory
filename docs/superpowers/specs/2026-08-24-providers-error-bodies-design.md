# Provider error bodies on the model-list path — design

> **Status:** DRAFT — capture a bounded, sanitized, key-redacted snippet of a provider's own error
> body on a non-2xx model-list response and carry it into the `anyhow` error, while keeping the
> underlying `reqwest::Error` (and therefore its status) as the error's source.

> **Implements:** https://github.com/savvagent/light-factory/issues/59
> **Follows:** https://github.com/savvagent/light-factory/issues/47 (the error classes this feeds),
> https://github.com/savvagent/light-factory/issues/44 (the fetch bounds this extends)

## 1. Brief

Quoted from issue #59 (`providers: parse provider error bodies so Gemini's invalid-key 400
classifies as auth`):

> Raised by three reviews of #55 (PR for #47).
>
> #55 treats only 401/403 as credential failures. Google's Generative Language API answers an
> **invalid API key with HTTP 400 `INVALID_ARGUMENT` / `API_KEY_INVALID`** ("API key not valid.
> Please pass a valid API key."), reserving 403 for permission problems. So for one of the four
> keyed providers, the most common credential failure lands in the retryable class — the exact path
> #47 exists to eliminate.
>
> The conservative threshold is correct as a global rule and should stay: the credentials step is
> terminal, so misclassifying *into* it costs the user both remedies, while misclassifying into the
> retryable class costs nothing beyond today's behaviour. Widening to a bare 400 would misroute
> genuine bad-request bugs.
>
> The real fix is that **the diagnostic detail is discarded before classification can ever see
> it.** Every provider fetch ends in `raw.error_for_status()?`, which builds the error from the
> status line and drops the response body. Gemini's 400 body carries the one string that would tell
> the user what is wrong, and it is thrown away in `crates/providers/src/models.rs`.
>
> Today a Gemini user with a bad key sees a bare `400 Bad Request` and an invitation to type a
> model id. The "unverified" label tells them the *id* is unverified; nothing tells them the *key*
> is rejected.
>
> Suggested: capture the body on a non-2xx (`let status = raw.status(); let body = raw.text().await`)
> and carry it into the error. Surfacing the body is the cheap, low-risk half and makes the manual
> path non-silent even without reclassifying. Reclassifying a 400 whose body matches an auth
> signature is a further option on top.
>
> Note `class_for_status(Some(400)) == Fetch` is currently pinned by a test, so widening the class
> forces the decision to be made deliberately — that is the right treatment for a known gap.
>
> Depends on the seam in the sibling issue if classification moves to `providers`.

## 2. Scope

**In:**

- `crates/providers/src/models.rs`: on a non-2xx model-list response, read a *bounded* prefix of
  the response body, extract the provider's own diagnostic message, sanitize and key-redact it, and
  attach it as `anyhow` context on top of the `reqwest::Error` produced by `error_for_status_ref`.
- A new `ListBounds` field (`max_error_bytes`) so the error-body read is bounded by the same
  mechanism as every other bound on this path, and is testable without a runtime knob.
- Preserving, as a pinned test, the property `crates/tui` already depends on: the `reqwest::Error`
  carrying the HTTP status stays reachable via `anyhow::Error::chain()`.
- Tests: one per provider shape (OpenAI/DeepSeek, Anthropic, Gemini, Ollama), plus the
  non-JSON-body, oversized-body, empty-body, key-echo, and control-character cases.

**Out:**

- **Any change under `crates/tui`.** PR #68 (`tui-keyring-failure-vs-miss`) is in flight and
  rewrites `crates/tui/src/{selection,modal,app,provider,i18n,text}.rs`. Nothing here touches that
  crate, including the `class_for_status(Some(400)) == Fetch` test the issue names.
- **Reclassifying a 400 whose body matches an auth signature.** See §4 A1 — deferred by design, not
  by omission.
- **The completion paths** (`anthropic.rs:102`, `gemini.rs:127`, `ollama.rs:69`,
  `openai_compatible.rs:70`), which have the identical `error_for_status()` gap. See §10.
- Any new public API on `crates/providers`, any new env var, any new dependency.

## 3. Premise corrections

**P1. The issue's literal suggestion (`raw.text().await`) is unsafe on this path and must not be
taken verbatim.** `Response::text()` buffers the body to completion, which is precisely the hazard
#44 removed from this module: `read_capped` exists because "a hostile endpoint's body length becomes
this process's allocation — and the Ollama path needs no credential at all, so anything listening on
`127.0.0.1:11434` could reach it" (`crates/providers/src/models.rs:84-99`). The error path is
*strictly more* exposed than the success path, because it is reached without a valid credential. The
capture therefore reuses `read_capped` under its own, tighter cap.

**P2. Replacing `error_for_status()` with a bare `anyhow::bail!` would silently regress #47.**
`crates/tui/src/modal.rs:573-580` recovers the HTTP status by walking `err.chain()` for a
`reqwest::Error` and calling `reqwest::Error::status()`. If the non-2xx arm stopped producing a
`reqwest::Error` in the chain, every 401/403 would degrade to `FetchFailure::Fetch` and the
credentials step #47 built would become unreachable — a total regression of the feature this issue
exists to strengthen, with no failing test in `crates/providers` to catch it. The design therefore
keeps `reqwest::Error` as the error's **source** and adds the body detail as `anyhow` **context**,
and pins that property with a test in `crates/providers` so it can never be "simplified" away.

**P3. `crates/providers` version is `version.workspace = true`.** A semver bump here would move
every crate in the workspace, which is a further reason this change is deliberately additive-only
(§8).

**P4. `crates/tui/src/text.rs` does not exist on `master`.** The cap-and-strip boundary the task
brief refers to is `summarize_provider_error` in `crates/tui/src/modal.rs:596-621` today; PR #68
moves it to `text.rs`. This spec cites the `master` location and does not depend on either.

## 4. Assumptions

**A1. Classification stays where it is; this PR ships only the capture half.** The issue explicitly
frames body-surfacing as "the cheap, low-risk half" and reclassification as "a further option on
top", and pins `class_for_status(Some(400)) == Fetch` with a test in `crates/tui/src/modal.rs` that
this PR is forbidden to edit. Rationale: the whole classification apparatus lives in `crates/tui`;
there is no consumer in `crates/providers` for a classification produced there, and adding an
unused public `FetchFailure`-shaped type to `providers` would be a new public interface built for
nobody — YAGNI, and semver surface that outlives the decision. Signature-matching a body is also a
*policy* choice with a false-positive cost (a 400 misrouted into a terminal step costs the user both
remedies), and the issue is explicit that such a widening must be made deliberately. Once this PR
lands, the body text is present in the error string that `crates/tui` already reads, so a follow-up
can match on it inside `class_for_status` with a one-function change and its own test. Filed as a
follow-up (§10), not smuggled in here.

**A2. `providers` sanitizes and bounds the captured text itself, rather than relying on the TUI's
boundary.** The TUI does cap at 120 chars, take the first line, and strip control characters
(`summarize_provider_error`). That is not sufficient reason for `providers` to hand out unbounded
remote text: (a) `providers` is a workspace library with `pub` entry points (`list_models`,
`list_ollama_models`) and no knowledge of its consumers — the server crate or a future log line is
just as reachable as the modal; (b) `read_capped`'s own doc already establishes that this module
owns its memory bound rather than delegating it; (c) two independent bounds is the pattern the repo
already uses on this path (byte cap *and* list cap *and* deadline). The cost is ~20 lines. The
`providers` cap is deliberately set *above* the TUI's display cap so the TUI remains the display
authority and this cap is a safety floor, not a second, competing truncation policy.

**A3. The error-body read gets its own, much tighter cap (`max_error_bytes`, 16 KiB) rather than
reusing `max_body_bytes` (2 MiB).** An honest provider error body is a JSON envelope of a few
hundred bytes; nothing above 16 KiB is a diagnostic. Reusing the 2 MiB success cap would let a
non-2xx response — reachable with **no** credential on the Ollama path — buffer 2 MiB we then throw
away down to 200 characters. Because `read_capped` *refuses* rather than truncates, an over-cap
error body yields no detail and the error degrades to the status line alone, which is exactly
today's behaviour. That is the correct failure mode: no detail is strictly better than an
attacker-sized allocation.

**A4. A body that is not the expected JSON envelope falls back to the raw (bounded, sanitized)
text.** Some failures come from a proxy or gateway rather than the provider (`502 Bad Gateway`,
`text/plain` bodies), and the first line of those is often the only signal there is. This is not a
security downgrade: the `message` field of a JSON envelope is exactly as attacker-chosen as a raw
body, so both paths carry the same trust level and both go through the same sanitize/redact/cap.
Envelope extraction is a *noise-reduction* choice, not a trust boundary.

**A5. The API key is redacted out of the captured detail.** Load-Bearing Invariant 6 says secrets
never reach logs or error messages. An endpoint that echoes the submitted key back in its error body
— a misconfigured gateway, a compromised or attacker-chosen `*_BASE_URL` override — would otherwise
put the user's live API key into a terminal cell and into scrollback. Redaction is a substring
replace over the normalized text, performed **before** the length cap so a truncation cannot leave a
usable key prefix, and **after** control-character stripping so a key with interleaved control
characters cannot evade the match. Empty keys are skipped (replacing the empty string would be a
runaway). The match is an exact substring, so it catches a verbatim echo (including inside a
`Bearer <key>` fragment) but not a percent-encoded, case-folded, or partially-masked one — a masked
echo is not a usable credential, so that residue is accepted rather than chased with fuzzy matching
that would mangle honest text.

**A6. The captured detail is framed as the provider's, not as this tool's.** The context string is
`the provider returned HTTP {status}: {detail}`, or — when no detail could be captured — exactly
`the provider returned HTTP {status} with no error detail`. Those two are the canonical strings;
§5.2 is their definition and everything else quotes it.
The status is repeated in the context — even though `reqwest::Error`'s own Display carries it —
because the TUI shows only the first 120 characters of `format!("{err:#}")`, and the context comes
first; without the status in the context, a long provider message would push the status off the
rendered line.

**A7. Extraction covers `{"error": {"message": ...}}` and `{"error": "..."}` only.** Those two
shapes cover all five providers on this path: OpenAI and DeepSeek (`error.message`), Anthropic
(`error.message` inside a `type: "error"` envelope), Gemini (`error.message` alongside
`error.code`/`error.status`), and Ollama (`error` as a bare string). Anything else falls through to
A4's raw-text path, so an unrecognized shape degrades rather than erroring.

**A8. Ollama passes no secret to redact.** `list_ollama_models_at` takes no key, so its capture is
invoked with `None`. This is threaded as `Option<&str>` rather than an empty string so "no secret"
is distinguishable from "empty secret" at the call site.

## 5. Design

### 5.1 `ListBounds` gains `max_error_bytes`

```rust
pub(crate) struct ListBounds {
    pub(crate) timeout: Duration,
    pub(crate) max_body_bytes: usize,
    pub(crate) max_error_bytes: usize,   // new
    pub(crate) max_models: usize,
}
```

`ListBounds::DEFAULT.max_error_bytes = 16 * 1024` (A3). `debug_check` gains
`debug_assert!(self.max_error_bytes >= MIN_BODY_BYTES)` — a cap below the smallest well-formed
envelope could never admit an honest error body, so it is a caller bug, exactly as the existing
`max_body_bytes` assertion.

`ListBounds` is `pub(crate)`; the field is not public API (§8).

### 5.2 The status check moves out of `parse_capped`

`parse_capped` today opens with `let resp = raw.error_for_status()?;`. That becomes a call to a new
`check_status`:

```rust
async fn check_status(
    raw: reqwest::Response,
    bounds: ListBounds,
    secret: Option<&str>,
) -> anyhow::Result<reqwest::Response> {
    let Err(status_err) = raw.error_for_status_ref() else {
        return Ok(raw);
    };
    let status = status_err.status().map(|s| s.as_u16());
    let detail = error_detail(raw, bounds, secret).await;
    Err(anyhow::Error::new(status_err).context(match (status, detail) {
        (Some(code), Some(d)) => format!("the provider returned HTTP {code}: {d}"),
        (Some(code), None) => format!("the provider returned HTTP {code} with no error detail"),
        (None, Some(d)) => format!("the provider returned an error status: {d}"),
        (None, None) => "the provider returned an error status".to_string(),
    }))
}
```

`error_for_status_ref` rather than `error_for_status` is load-bearing: it yields the `reqwest::Error`
*without* consuming the response, so the body is still readable. The `reqwest::Error` becomes the
`anyhow` **source**, so `err.chain().find_map(downcast_ref::<reqwest::Error>())` — the exact walk
`crates/tui/src/modal.rs:574-578` performs — still finds it and still recovers the status (P2).

Ordering within `parse_capped` is otherwise unchanged and still load-bearing: `reject_redirect` has
already refused a 3xx before this runs, and no success-path byte is read until the status is clear.

### 5.3 Capturing the detail

```rust
async fn error_detail(
    resp: reqwest::Response,
    bounds: ListBounds,
    secret: Option<&str>,
) -> Option<String> {
    let body = read_capped(resp, bounds.max_error_bytes).await.ok()?;
    let text = String::from_utf8_lossy(&body);
    let message = extract_error_message(&text).unwrap_or_else(|| text.to_string());
    let normalized = sanitize_detail(&message);      // first line, control-free, trimmed
    let redacted = redact_secret(normalized, secret); // before the cap (A5)
    let capped = cap_chars(redacted, DETAIL_MAX_CHARS);
    (!capped.is_empty()).then_some(capped)
}
```

- `read_capped` failing (over-cap, transport error mid-read) yields `None` → the status-only
  context. No part of the body reaches that error, which `read_capped`'s own doc already guarantees.
- `from_utf8_lossy` rather than `from_utf8().ok()?`: a body with one invalid byte should not throw
  away an otherwise-readable diagnostic, and U+FFFD is not a control character.
- **Order is deliberate:** strip control characters first (so an interleaved-control-character key
  is matched), redact second, cap last (so truncation cannot bisect a key and leave a usable
  prefix).
- `DETAIL_MAX_CHARS = 200`, above the TUI's 120-char display cap by design (A2). **Contract:** at
  most `DETAIL_MAX_CHARS` characters *of provider text*, plus a single U+2026 marker when
  truncation happened — so the rendered detail is at most 201 characters and a truncated message
  never looks like a complete one. This mirrors `summarize_provider_error`'s existing 120-then-append
  behaviour rather than inventing a second convention.

`extract_error_message` deserializes with `serde_json` into:

```rust
#[derive(Deserialize)]
struct ErrorEnvelope { error: ErrorPayload }

#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorPayload {
    Detailed { message: String },   // tried first: OpenAI/DeepSeek/Anthropic/Gemini
    Text(String),                   // Ollama
}
```

Untagged variant order is load-bearing — serde tries variants top to bottom, so the struct must
precede the bare string. A parse failure returns `None` and A4's raw-text fallback applies.

### 5.4 Threading the secret

`parse_capped` gains a `secret: Option<&str>` parameter, forwarded to `check_status`. Its **four**
call sites — serving five providers, since `list_openai_compatible` backs both OpenAI and DeepSeek —
pass:

| Call site | `secret` |
|---|---|
| `list_anthropic` | `Some(key)` |
| `list_openai_compatible` (openai, deepseek) | `Some(key)` |
| `list_gemini` | `Some(key)` |
| `list_ollama_models_at` | `None` (A8) |

Threading the key explicitly, rather than reaching for it from a wider scope, keeps the redaction
site adjacent to the only thing that knows what the secret is.

## 6. Error handling & edge cases

| Case | Behaviour |
|---|---|
| 2xx | Untouched. `check_status` returns the response; the success path is byte-for-byte as before. |
| 3xx | Untouched — `reject_redirect` fires before `parse_capped` is reached. |
| 4xx/5xx with a recognized envelope | `the provider returned HTTP 400: API key not valid. Please pass a valid API key.` with the `reqwest::Error` as source. |
| 4xx/5xx with an empty body | Detail is empty after trim → `None` → `... HTTP 401 with no error detail`. |
| 4xx/5xx with a non-JSON body | First sanitized line of the raw text, capped (A4). |
| 4xx/5xx with a body over `max_error_bytes` | `read_capped` refuses → `None` → status-only context. Today's behaviour exactly. |
| 4xx/5xx with a body echoing the API key | Key replaced with `<redacted>` before the cap (A5). |
| 4xx/5xx with a body containing `ESC`/`CR`/`NUL` | Control characters stripped; only the first line survives. |
| 4xx/5xx with invalid UTF-8 | Lossy-decoded; U+FFFD survives sanitization. |
| Body read stalls after the status line | The per-request `timeout` from `model_list_request` covers the whole request including the body read, so the deadline still binds. |
| Ollama non-2xx | Same handling, `secret: None`. Still classified `Fetch` by the TUI (`class_for_provider`), unchanged. |

## 7. Testing

All offline, against `wiremock`, next to the code in `crates/providers/src/models.rs`'s existing
`#[cfg(test)] mod tests`. New tests:

1. `a_gemini_invalid_key_400_surfaces_the_providers_own_message` — the motivating case: a 400 with
   Gemini's real envelope; the error chain contains `API key not valid` and `400`.
2. `the_status_error_survives_in_the_chain_for_classification` — **the regression guard for P2**: a
   401 response; `err.chain().find_map(downcast_ref::<reqwest::Error>()).and_then(status)` is
   `Some(401)`. Mirrors the walk `crates/tui/src/modal.rs` performs, so removing the `reqwest::Error`
   source fails here rather than silently in the TUI.
3. `an_openai_error_envelope_is_extracted` — `{"error":{"message":...,"type":...}}` on a 401.
4. `an_ollama_string_error_envelope_is_extracted` — `{"error":"..."}` on a 404 via
   `list_ollama_models_at`.
5. `a_non_json_error_body_falls_back_to_its_first_line` — an HTML/`text/plain` 502.
6. `an_empty_error_body_yields_a_status_only_message` — a 401 with no body; the error names the
   status and says there is no detail.
7. `an_oversized_error_body_is_refused_rather_than_captured` — a body far over a tight
   `max_error_bytes`; no body content in `format!("{err:#}")`, and the status still present.
8. `an_error_body_echoing_the_api_key_is_redacted` — the key appears in the body; `format!("{err:#}")`
   must not contain it. Uses a key long enough that an accidental prefix leak would be visible.
9. `control_characters_and_extra_lines_are_stripped_from_the_error_detail` — a body whose message
   contains `\n`, `\r` and `\u{1b}`; only the first line survives and no control character does.
10. `an_over_long_error_detail_is_capped` — a 1 KiB single-line message; the rendered detail is
    capped at `DETAIL_MAX_CHARS` and ends in an ellipsis marker.
11. `default_bounds_are_the_production_values` — extended with `max_error_bytes == 16 * 1024`.

Existing tests that must stay green unchanged: `an_auth_error_is_surfaced_as_an_error` (the
outermost message still names 401), `a_stalled_endpoint_fails_at_the_deadline` (the timeout still
downcasts at the root, because it fires at `send()` before any status check), and every success-path
and bound test.

`crates/tui`'s `class_for_status_treats_only_401_and_403_as_credential_failures` is untouched and
stays green: no classification rule changes.

## 8. Semver

**Additive / semver-minor — no `Cargo.toml` version bump.**

- `ListBounds` is `pub(crate)`; the new field is invisible outside the crate.
- `parse_capped`, `read_capped`, `check_status`, `error_detail`, `extract_error_message`,
  `sanitize_detail`, `redact_secret`, `cap_chars`, `ErrorEnvelope`, `ErrorPayload` are all private.
- `list_models`, `list_ollama_models`, `list_models_at`, `list_ollama_models_at` keep their exact
  signatures.
- The only observable change to a consumer is the **text** of an `anyhow::Error` on a path that
  already returned `Err`. `anyhow::Error` is opaque and carries no stability guarantee on its
  Display; the structured property consumers actually rely on — a `reqwest::Error` with a status in
  the chain — is preserved and newly pinned by a test.
- `crates/providers` uses `version.workspace = true`, so a bump would move every workspace crate
  (P3). Another reason to keep this additive.

## 9. Goal & success criteria

A user whose Gemini key is rejected sees the provider's own sentence instead of a bare
`400 Bad Request`.

1. `list_models("gemini", bad_key)` against an endpoint answering Google's real 400 envelope returns
   an error whose `{:#}` rendering contains both `400` and `API key not valid`.
2. The `reqwest::Error` with its HTTP status remains reachable via `anyhow::Error::chain()` for every
   non-2xx, pinned by a test in `crates/providers`.
3. No captured error text can carry more than `DETAIL_MAX_CHARS` characters of provider text (plus
   the single U+2026 truncation marker), span more than one line, or contain a control character.
4. No response body over `max_error_bytes` is buffered on any path, credentialed or not.
5. A key echoed back **verbatim** by an endpoint never appears in the error.
6. `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and
   `cargo fmt --all --check` are clean, with zero lines changed under `crates/tui`.

## 10. Risks & open questions

**R1. The motivating case is not fully closed by this PR.** Gemini's invalid-key 400 still lands in
`FetchFailure::Fetch`, so the `/models` modal still offers a retry and a manual box rather than the
credentials step. What changes is that the modal now *says* "API key not valid. Please pass a valid
API key.", so the path is no longer silent — which is the outcome the issue calls the real fix.
Closing it fully needs the `crates/tui` half (A1). **Follow-up to file:** "tui: reclassify a 400
whose error body matches an auth signature as `FetchFailure::Auth`".

**Issue hygiene, load-bearing:** because the issue's title AC ("...classifies as auth") is *not* met,
the PR must reference #59 **without** a closing keyword, and the follow-up must be filed before
merge. Otherwise the recorded gap evaporates with the issue.

**Verification gap:** criterion 1 pins the untruncated `{:#}` rendering inside `crates/providers`.
What a user actually sees is that string after `crates/tui`'s 120-character
`summarize_provider_error` cap, which no test in this crate can cover. The budget works today —
`the provider returned HTTP 400: ` is 31 characters and Gemini's message is 47, so 78 of 120 — but
that is reasoning, not a pin. A future lengthening of the context prefix could push the provider's
sentence off the rendered line with every test still green.

**R2. The completion paths still discard their error bodies.** `anthropic.rs:102`,
`gemini.rs:127`, `ollama.rs:69`, and `openai_compatible.rs:70` all end in `error_for_status()?`.
Deliberately out of scope: the issue names `models.rs`, those errors surface on a different UI path
with a different rendering boundary, and widening the diff would collide with concurrent work.
**Follow-up to file:** "providers: capture error bodies on the completion paths too".

**R3. `reqwest::Error`'s Display includes the request URL, unredacted.** If a provider ever moved
its key into a query parameter, the key would appear in the error via the source chain — with or
without this change. All four keyed providers on this path send the key in a header
(`x-api-key`, `authorization`, `x-goog-api-key`), so nothing leaks today. Pre-existing and unchanged
by this PR; noted so it is not mistaken for something this PR introduced.

**R4. Frame-boundary overshoot on the error read.** `read_capped` buffers up to `max_error_bytes`
plus the one frame that trips the check, whose size is chosen by the HTTP layer rather than the
sender — the same closed-rather-than-one-frame-open bound `read_capped`'s doc already establishes.
Unchanged property, tighter cap.

**R5. Attacker-chosen text is now routinely rendered.** A hostile endpoint can put 200 characters of
chosen text into the modal, framed as `the provider returned HTTP 400: <text>`. This capability
already existed via `summarize_provider_error` for non-status errors; what is new is that a
*status* error also carries text. Mitigations: the `the provider returned HTTP {code}:` prefix
attributes the text to the remote party rather than to this tool, the one-line + control-free +
200-char normalization prevents layout takeover and escape-sequence injection, and the TUI applies
its own 120-char cap on top. Accepted, and it is the deliberate trade the issue asks for.
